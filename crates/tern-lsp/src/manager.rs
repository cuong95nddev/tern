//! The language servers of one workspace: started on first use, one per
//! (server, project root), kept in step with the files open in tabs, and
//! shut down when the workspace goes.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use futures::lock::Mutex as AsyncMutex;
use serde_json::{Value, json};

use crate::config::{ServerConfig, find_root, language_id, servers_for};
use crate::rpc::{Connection, Incoming};
use crate::text::Encoding;
use crate::uri::path_to_uri;
use crate::{LspError, LspEvent};

/// Larger files aren't read from disk (as the file tab won't show them).
pub(crate) const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;

/// The text of the source file at `path`: none for what isn't a regular
/// file (a device a server pointed at would never end) or is too large.
pub fn read_text(path: &Path) -> Option<String> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// `path` (a `PATH` value) without its empty and relative entries: a server
/// runs in its project's root, where those would find a program the
/// project's authors put there instead of the one installed.
pub(crate) fn absolute_path_entries(path: &std::ffi::OsStr) -> std::ffi::OsString {
    std::env::join_paths(std::env::split_paths(path).filter(|p| p.is_absolute())).unwrap_or_default()
}

/// How long to wait for a server, and how often to restart one.
#[derive(Clone, Debug)]
pub struct Timeouts {
    /// For `initialize` (indexing servers answer it before they index).
    pub initialize: Duration,
    /// For any other request.
    pub request: Duration,
    /// A server that crashed or never started isn't started again sooner.
    pub restart_backoff: Duration,
    /// For `shutdown`, then for the process to exit before it is killed.
    pub shutdown: Duration,
    pub exit: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            initialize: Duration::from_secs(60),
            request: Duration::from_secs(10),
            restart_backoff: Duration::from_secs(60),
            shutdown: Duration::from_secs(2),
            exit: Duration::from_secs(3),
        }
    }
}

/// A running, initialized server.
pub(crate) struct Server {
    pub(crate) config: ServerConfig,
    pub(crate) conn: Arc<Connection>,
    /// Its capabilities as it sent them (`lsp-types` lacks some, like type hierarchy).
    pub(crate) caps: Value,
    pub(crate) encoding: Encoding,
    /// Documents it has been sent `didOpen` for.
    opened: Mutex<HashSet<PathBuf>>,
    /// Of those, the ones no tab showed when opened (asked about for an
    /// outline, a target's line), oldest first: closed again past [`MAX_BORROWED`].
    borrowed: Mutex<std::collections::VecDeque<PathBuf>>,
}

/// Documents opened for requests and not by a tab that a server keeps open.
const MAX_BORROWED: usize = 32;

/// One (server, root) and what became of starting it.
#[derive(Default)]
struct Slot {
    server: Option<Arc<Server>>,
    /// When it was last started: a server that exits is started again a
    /// backoff after that, not after its exit is noticed.
    started_at: Option<Instant>,
    /// When it last failed to start or exited, for the backoff.
    failed_at: Option<Instant>,
    failure: String,
}

/// Gives the `PATH` servers are found on.
pub type PathProvider = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// A server by name, and its project root.
type SlotKey = (String, PathBuf);

/// A file open in a tab (or several).
struct Doc {
    text: String,
    version: i32,
    tabs: usize,
}

pub struct Manager {
    me: Weak<Manager>,
    workspace: PathBuf,
    /// The workspace's real path, when it is opened through a link.
    workspace_real: Option<PathBuf>,
    configs: Mutex<Vec<ServerConfig>>,
    events: flume::Sender<LspEvent>,
    timeouts: Timeouts,
    /// Tells the `PATH` to find servers on (the user's login shell's), else
    /// Tern's own is used. Called when a server starts, off the UI thread.
    path_env: Mutex<Option<PathProvider>>,
    slots: Mutex<HashMap<SlotKey, Arc<AsyncMutex<Slot>>>>,
    /// The servers started, by slot: what document sync talks to, without
    /// waiting on a slot that is starting or being looked up.
    servers: Mutex<HashMap<SlotKey, Arc<Server>>>,
    docs: Mutex<HashMap<PathBuf, Doc>>,
    /// Servers reported missing (once each).
    reported: Mutex<HashSet<String>>,
}

impl Manager {
    pub fn new(workspace: PathBuf, configs: Vec<ServerConfig>, events: flume::Sender<LspEvent>) -> Arc<Manager> {
        Self::with_timeouts(workspace, configs, events, Timeouts::default())
    }

    pub fn with_timeouts(
        workspace: PathBuf,
        configs: Vec<ServerConfig>,
        events: flume::Sender<LspEvent>,
        timeouts: Timeouts,
    ) -> Arc<Manager> {
        let manager = Arc::new_cyclic(|me| Manager {
            me: me.clone(),
            workspace_real: workspace.canonicalize().ok().filter(|r| *r != workspace),
            workspace,
            configs: Mutex::new(configs),
            events,
            timeouts,
            path_env: Mutex::default(),
            slots: Mutex::default(),
            servers: Mutex::default(),
            docs: Mutex::default(),
            reported: Mutex::default(),
        });
        let mut all = MANAGERS.lock().unwrap();
        all.retain(|m| m.strong_count() > 0);
        all.push(Arc::downgrade(&manager));
        manager
    }

    pub(crate) fn timeouts(&self) -> &Timeouts {
        &self.timeouts
    }

    /// Find servers on the `PATH` `provider` gives from now on.
    pub fn set_path_provider(&self, provider: PathProvider) {
        *self.path_env.lock().unwrap() = Some(provider);
    }

    /// New server configs: servers whose config changed are shut down (the
    /// next use starts them again).
    pub fn set_configs(&self, configs: Vec<ServerConfig>) {
        let mut current = self.configs.lock().unwrap();
        if *current == configs {
            return;
        }
        let gone: Vec<String> = current.iter().filter(|c| !configs.contains(c)).map(|c| c.name.clone()).collect();
        *current = configs;
        drop(current);
        let mut slots = self.slots.lock().unwrap();
        let stale: Vec<_> = slots.keys().filter(|(name, _)| gone.contains(name)).cloned().collect();
        let servers: Vec<_> = stale.iter().filter_map(|k| slots.remove(k)).collect();
        drop(slots);
        // Missing servers may be installed now, or replaced.
        self.reported.lock().unwrap().clear();
        let timeouts = self.timeouts.clone();
        std::thread::spawn(move || {
            for slot in servers {
                if let Some(server) = futures::executor::block_on(slot.lock()).server.take() {
                    shut_down(&server, &timeouts);
                }
            }
        });
    }

    /// Whether `path` is in a language some server is configured for.
    pub fn supports(&self, path: &Path) -> bool {
        language_id(path).is_some_and(|l| !servers_for(&self.configs.lock().unwrap(), l).is_empty())
    }

    // ---- document sync ----

    /// A tab shows `path` with `text`: its server starts (if it isn't
    /// running) and gets the document.
    pub fn open(&self, path: &Path, text: String) {
        if !self.supports(path) {
            return;
        }
        {
            let mut docs = self.docs.lock().unwrap();
            let doc = docs.entry(path.to_path_buf()).or_insert(Doc { text: String::new(), version: 0, tabs: 0 });
            doc.tabs += 1;
            if doc.tabs > 1 {
                return;
            }
            doc.text = text;
            doc.version = 1;
        }
        self.start_soon(path);
    }

    /// Start the server for `path` off this thread, if it isn't running.
    pub(crate) fn start_soon(&self, path: &Path) {
        let (Some(me), path) = (self.me.upgrade(), path.to_path_buf()) else { return };
        std::thread::spawn(move || {
            let _ = futures::executor::block_on(me.server_for(&path));
        });
    }

    /// Whether a server for `path` is up, so asking it won't wait for one to start.
    pub(crate) fn is_running_for(&self, path: &Path) -> bool {
        let Some(language) = language_id(path) else { return false };
        let configs: Vec<ServerConfig> = servers_for(&self.configs.lock().unwrap(), language).into_iter().cloned().collect();
        let servers = self.servers.lock().unwrap();
        let alive = |s: &Arc<Server>| s.conn.is_alive();
        if !path.starts_with(&self.workspace) {
            return servers.values().any(|s| alive(s) && s.config.languages.iter().any(|l| l == language));
        }
        configs.iter().any(|c| self.root_for(c, path).is_some_and(|root| servers.get(&(c.name.clone(), root)).is_some_and(alive)))
    }

    /// Whether server `name` is up for `path`'s project.
    pub fn serves(&self, path: &Path, name: &str) -> bool {
        let configs = self.configs.lock().unwrap().clone();
        let servers = self.servers.lock().unwrap();
        configs
            .iter()
            .filter(|c| c.name == name)
            .any(|c| self.root_for(c, path).is_some_and(|root| servers.get(&(c.name.clone(), root)).is_some_and(|s| s.conn.is_alive())))
    }

    /// A path a server gave as Tern names it: servers resolve links, so one
    /// in the workspace's real folder is put back under the workspace.
    pub(crate) fn local_path(&self, path: PathBuf) -> PathBuf {
        match self.workspace_real.as_ref().and_then(|real| path.strip_prefix(real).ok()) {
            Some(rest) => self.workspace.join(rest),
            None => path,
        }
    }

    /// The project root `config`'s server for `path` runs in.
    fn root_for(&self, config: &ServerConfig, path: &Path) -> Option<PathBuf> {
        find_root(path, &config.root_markers, &self.workspace, config.require_root_marker, config.outermost_root)
    }

    /// The text of open `path` changed.
    pub fn change(&self, path: &Path, text: String) {
        let version = {
            let mut docs = self.docs.lock().unwrap();
            let Some(doc) = docs.get_mut(path) else { return };
            doc.version += 1;
            doc.text = text.clone();
            doc.version
        };
        for server in self.servers_with(path) {
            server.conn.notify(
                "textDocument/didChange",
                json!({"textDocument": {"uri": uri(path), "version": version}, "contentChanges": [{"text": text}]}),
            );
        }
    }

    /// Open `path` was saved.
    pub fn save(&self, path: &Path) {
        for server in self.servers_with(path) {
            server.conn.notify("textDocument/didSave", json!({"textDocument": {"uri": uri(path)}}));
        }
    }

    /// A tab no longer shows `path`.
    pub fn close(&self, path: &Path) {
        {
            let mut docs = self.docs.lock().unwrap();
            let Some(doc) = docs.get_mut(path) else { return };
            doc.tabs -= 1;
            if doc.tabs > 0 {
                return;
            }
            docs.remove(path);
        }
        for server in self.servers_with(path) {
            server.opened.lock().unwrap().remove(path);
            server.conn.notify("textDocument/didClose", json!({"textDocument": {"uri": uri(path)}}));
        }
    }

    /// Files changed on disk (by anything): every running server hears of them.
    pub fn files_changed(&self, paths: &[PathBuf]) {
        let changes: Vec<Value> = paths
            .iter()
            .take(1000)
            .filter_map(|p| Some(json!({"uri": path_to_uri(p)?.as_str(), "type": if p.exists() { 2 } else { 3 }})))
            .collect();
        if changes.is_empty() {
            return;
        }
        for server in self.running() {
            server.conn.notify("workspace/didChangeWatchedFiles", json!({"changes": changes}));
            // A file changed on disk that no tab shows is the server's to read again.
            let mut opened = server.opened.lock().unwrap();
            let docs = self.docs.lock().unwrap();
            let stale: Vec<&PathBuf> = paths.iter().filter(|p| opened.contains(*p) && !docs.contains_key(*p)).collect();
            for p in stale {
                server.conn.notify("textDocument/didClose", json!({"textDocument": {"uri": uri(p)}}));
                opened.remove(p);
            }
        }
    }

    fn register(&self, key: SlotKey, server: &Arc<Server>) {
        self.servers.lock().unwrap().insert(key, server.clone());
    }

    fn running(&self) -> Vec<Arc<Server>> {
        self.servers.lock().unwrap().values().filter(|s| s.conn.is_alive()).cloned().collect()
    }

    fn servers_with(&self, path: &Path) -> Vec<Arc<Server>> {
        self.running().into_iter().filter(|s| s.opened.lock().unwrap().contains(path)).collect()
    }

    // ---- servers ----

    /// Ask `method` about `path` (its server started and the document opened
    /// first). `params` gets `textDocument` filled in.
    pub async fn request(&self, path: &Path, method: &str, params: Value) -> Result<Value, LspError> {
        let server = self.server_for(path).await?;
        self.request_on(&server, path, method, params).await
    }

    pub(crate) async fn request_on(&self, server: &Server, path: &Path, method: &str, mut params: Value) -> Result<Value, LspError> {
        if let Value::Object(map) = &mut params {
            map.entry("textDocument").or_insert_with(|| json!({"uri": uri(path)}));
        }
        self.ensure_open(server, path)?;
        server.conn.request(method, params, self.timeouts.request).await
    }

    /// The server for `path`, started (and the document opened) if need be.
    pub(crate) async fn server_for(&self, path: &Path) -> Result<Arc<Server>, LspError> {
        let language = language_id(path).ok_or(LspError::Unsupported)?;
        let configs: Vec<ServerConfig> = servers_for(&self.configs.lock().unwrap(), language).into_iter().cloned().collect();
        if configs.is_empty() {
            return Err(LspError::Unsupported);
        }
        if !path.starts_with(&self.workspace) {
            // Outside the workspace (a dependency a jump led to): a server
            // already running for the language, never a new one.
            let running = self.running().into_iter().find(|s| s.config.languages.iter().any(|l| l == language));
            let server = running.ok_or_else(|| LspError::Unavailable("no language server runs for files outside the workspace".into()))?;
            self.ensure_open(&server, path)?;
            return Ok(server);
        }
        let mut last = LspError::NotFound;
        for config in &configs {
            let Some(root) = self.root_for(config, path) else {
                last = LspError::Unsupported;
                continue;
            };
            match self.start(config, &root).await {
                Ok(server) => {
                    self.ensure_open(&server, path)?;
                    return Ok(server);
                },
                // Not installed: the next one (a fallback) may be.
                Err(LspError::NotFound) => continue,
                Err(e) => return Err(e),
            }
        }
        if matches!(last, LspError::NotFound) {
            let first = &configs[0];
            if self.reported.lock().unwrap().insert(first.name.clone()) {
                let _ = self.events.send(LspEvent::Missing { server: first.name.clone(), hint: first.install_hint.clone() });
            }
        }
        Err(last)
    }

    /// The running server of `config` at `root`, started if it isn't.
    async fn start(&self, config: &ServerConfig, root: &Path) -> Result<Arc<Server>, LspError> {
        let key: SlotKey = (config.name.clone(), root.to_path_buf());
        let slot = self.slots.lock().unwrap().entry(key.clone()).or_default().clone();
        let mut slot = slot.lock().await;
        if let Some(server) = &slot.server {
            if server.conn.is_alive() {
                return Ok(server.clone());
            }
            slot.server = None;
            self.servers.lock().unwrap().remove(&key);
            slot.failed_at = Some(slot.started_at.unwrap_or_else(Instant::now));
            slot.failure = format!("{} exited", config.name);
            let _ = self.events.send(LspEvent::Exited { server: config.name.clone() });
        }
        if let Some(at) = slot.failed_at
            && at.elapsed() < self.timeouts.restart_backoff
            && !self.timeouts.restart_backoff.is_zero()
        {
            return Err(LspError::Unavailable(slot.failure.clone()));
        }
        slot.started_at = Some(Instant::now());
        match self.launch(config, root).await {
            Ok(server) => {
                slot.server = Some(server.clone());
                self.register(key, &server);
                Ok(server)
            },
            Err(LspError::NotFound) => Err(LspError::NotFound),
            Err(e) => {
                slot.failed_at = Some(Instant::now());
                slot.failure = format!("{}: {e}", config.name);
                let _ = self.events.send(LspEvent::Failed {
                    server: config.name.clone(),
                    message: slot.failure.clone(),
                    hint: config.install_hint.clone(),
                });
                Err(LspError::Unavailable(slot.failure.clone()))
            },
        }
    }

    /// Spawn and initialize `config`'s server for `root`.
    async fn launch(&self, config: &ServerConfig, root: &Path) -> Result<Arc<Server>, LspError> {
        let mut env: Vec<(String, String)> = config.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        if !config.env.contains_key("PATH") {
            let provider = self.path_env.lock().unwrap().clone();
            let path = provider.and_then(|p| p()).map(std::ffi::OsString::from).or_else(|| std::env::var_os("PATH")).unwrap_or_default();
            env.push(("PATH".into(), absolute_path_entries(&path).to_string_lossy().into_owned()));
        }
        let root_uri = path_to_uri(root).map(|u| u.as_str().to_owned());
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let folders = json!([{"uri": root_uri, "name": name}]);
        let state = Arc::new(ServerState {
            name: config.name.clone(),
            settings: config.settings.clone(),
            folders: folders.clone(),
            progress: Mutex::default(),
            events: self.events.clone(),
        });
        let handler_state = state.clone();
        let conn = Connection::spawn(
            &config.command,
            &config.args,
            &env,
            root,
            &config.name,
            Box::new(move |conn, msg| handler_state.handle(conn, msg)),
        )
        .map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { LspError::NotFound } else { LspError::Unavailable(e.to_string()) })?;
        let params = json!({
            "processId": std::process::id(),
            "clientInfo": {"name": "Tern"},
            "rootUri": root_uri,
            "rootPath": root,
            "workspaceFolders": folders,
            "initializationOptions": config.initialization_options,
            "capabilities": client_capabilities(),
        });
        let result = match conn.request("initialize", params, self.timeouts.initialize).await {
            Ok(r) => r,
            Err(e) => {
                conn.kill();
                return Err(e);
            },
        };
        let Some(caps) = result.get("capabilities").filter(|c| c.is_object()).cloned() else {
            conn.kill();
            return Err(LspError::Unavailable("bad initialize answer".into()));
        };
        conn.notify("initialized", json!({}));
        if let Some(settings) = &config.settings {
            conn.notify("workspace/didChangeConfiguration", json!({"settings": settings}));
        }
        let encoding = Encoding::from_name(caps["positionEncoding"].as_str());
        Ok(Arc::new(Server { config: config.clone(), conn, caps, encoding, opened: Mutex::default(), borrowed: Mutex::default() }))
    }

    /// Send `path` to `server` if it hasn't got it: the tab's text, else the file's.
    fn ensure_open(&self, server: &Server, path: &Path) -> Result<(), LspError> {
        let mut opened = server.opened.lock().unwrap();
        if opened.contains(path) {
            return Ok(());
        }
        let docs = self.docs.lock().unwrap();
        let (text, version) = match docs.get(path) {
            Some(doc) => (doc.text.clone(), doc.version),
            None => {
                let text = read_text(path).ok_or_else(|| LspError::Unavailable(format!("cannot read {}", path.display())))?;
                let mut borrowed = server.borrowed.lock().unwrap();
                borrowed.push_back(path.to_path_buf());
                while borrowed.len() > MAX_BORROWED {
                    let Some(old) = borrowed.pop_front() else { break };
                    // A tab may show it since: then it stays.
                    if !docs.contains_key(&old) && opened.remove(&old) {
                        server.conn.notify("textDocument/didClose", json!({"textDocument": {"uri": uri(&old)}}));
                    }
                }
                (text, 1)
            },
        };
        drop(docs);
        let language = language_id(path).unwrap_or("plaintext");
        server.conn.notify(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri(path), "languageId": language, "version": version, "text": text}}),
        );
        opened.insert(path.to_path_buf());
        Ok(())
    }

    /// Line `line` of `path` as the servers have it, without copying the rest.
    pub(crate) fn line_of(&self, path: &Path, line: u32) -> String {
        if let Some(doc) = self.docs.lock().unwrap().get(path) {
            return doc.text.lines().nth(line as usize).unwrap_or_default().to_owned();
        }
        read_text(path).and_then(|t| t.lines().nth(line as usize).map(str::to_owned)).unwrap_or_default()
    }

    /// The text of `path` as the servers have it: the tab's, else the file's.
    pub(crate) fn text_of(&self, path: &Path) -> Option<String> {
        if let Some(doc) = self.docs.lock().unwrap().get(path) {
            return Some(doc.text.clone());
        }
        read_text(path)
    }
}

impl Manager {
    /// Shut every server down (`shutdown`, `exit`, then kill), returning once they are gone.
    pub fn shut_down_servers(&self) {
        let servers: Vec<_> = self.servers.lock().unwrap().drain().map(|(_, s)| s).collect();
        shut_down_each(servers, &self.timeouts);
    }
}

/// Every manager made, so quitting can shut their servers down.
static MANAGERS: Mutex<Vec<Weak<Manager>>> = Mutex::new(Vec::new());

/// Shut down the servers of every manager, all at once, returning once
/// they are gone (or killed): for quitting, which won't wait otherwise.
pub fn shut_down_all() {
    let managers: Vec<Arc<Manager>> = MANAGERS.lock().unwrap().iter().filter_map(Weak::upgrade).collect();
    let threads: Vec<_> = managers.into_iter().map(|m| std::thread::spawn(move || m.shut_down_servers())).collect();
    for t in threads {
        let _ = t.join();
    }
}

fn shut_down_each(servers: Vec<Arc<Server>>, t: &Timeouts) {
    let threads: Vec<_> = servers
        .into_iter()
        .map(|s| {
            let t = t.clone();
            std::thread::spawn(move || shut_down(&s, &t))
        })
        .collect();
    for t in threads {
        let _ = t.join();
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        let servers: Vec<_> = self.servers.get_mut().unwrap().drain().map(|(_, s)| s).collect();
        self.slots.get_mut().unwrap().clear();
        let timeouts = self.timeouts.clone();
        std::thread::spawn(move || shut_down_each(servers, &timeouts));
    }
}

/// `shutdown`, `exit`, and kill if it is still there after a while.
fn shut_down(server: &Server, t: &Timeouts) {
    if server.conn.is_alive() {
        let _ = futures::executor::block_on(server.conn.request("shutdown", Value::Null, t.shutdown));
        server.conn.notify("exit", Value::Null);
    }
    if !server.conn.wait_exit(t.exit) {
        server.conn.kill();
    }
}

fn uri(path: &Path) -> String {
    path_to_uri(path).map(|u| u.as_str().to_owned()).unwrap_or_default()
}

/// What a server's own requests and notifications need.
struct ServerState {
    name: String,
    settings: Option<Value>,
    /// Its workspace folders, as `initialize` gave them.
    folders: Value,
    /// Work-done progress tokens begun and not ended.
    progress: Mutex<HashSet<String>>,
    events: flume::Sender<LspEvent>,
}

impl ServerState {
    fn handle(&self, conn: &Connection, msg: Incoming) {
        match msg {
            Incoming::Request { id, method, params } => {
                let result = match method.as_str() {
                    "workspace/configuration" => Ok(Value::Array(
                        params["items"]
                            .as_array()
                            .map(|items| items.iter().map(|i| self.section(i["section"].as_str())).collect())
                            .unwrap_or_default(),
                    )),
                    "window/workDoneProgress/create"
                    | "client/registerCapability"
                    | "client/unregisterCapability"
                    | "window/showMessageRequest" => Ok(Value::Null),
                    "workspace/workspaceFolders" => Ok(self.folders.clone()),
                    _ => Err((-32601, format!("{method} is not supported"))),
                };
                conn.respond(id, result);
            },
            Incoming::Notification { method, params } if method == "$/progress" => {
                let token = params["token"].to_string();
                let mut progress = self.progress.lock().unwrap();
                match params["value"]["kind"].as_str() {
                    Some("begin") => {
                        progress.insert(token);
                    },
                    Some("end") if progress.remove(&token) && progress.is_empty() => {
                        let _ = self.events.send(LspEvent::Settled { server: self.name.clone() });
                    },
                    _ => {},
                }
            },
            Incoming::Notification { .. } => {},
        }
    }

    /// The settings at dotted `section` (all of them without one).
    fn section(&self, section: Option<&str>) -> Value {
        let Some(settings) = &self.settings else { return Value::Null };
        let Some(section) = section.filter(|s| !s.is_empty()) else { return settings.clone() };
        section.split('.').try_fold(settings, |v, key| v.get(key)).cloned().unwrap_or(Value::Null)
    }
}

/// What Tern tells servers it can do.
fn client_capabilities() -> Value {
    json!({
        "general": {"positionEncodings": ["utf-32", "utf-16"]},
        "textDocument": {
            "synchronization": {"didSave": true, "dynamicRegistration": false},
            "definition": {"linkSupport": true},
            "declaration": {"linkSupport": true},
            "implementation": {"linkSupport": true},
            "typeDefinition": {"linkSupport": true},
            "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
            "typeHierarchy": {"dynamicRegistration": false},
        },
        "window": {"workDoneProgress": true},
        "workspace": {
            "configuration": true,
            "workspaceFolders": true,
            "didChangeConfiguration": {"dynamicRegistration": false},
            "didChangeWatchedFiles": {"dynamicRegistration": false},
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sync notifications reach a running server even while its slot is
    /// locked (a request starting or looking up its server).
    #[cfg(unix)]
    #[test]
    fn edits_reach_servers_whose_slot_is_busy() {
        let (tx, _rx) = flume::unbounded();
        let m = Manager::new("/w".into(), vec![], tx);
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let heard = seen.clone();
        // `cat` sends back what it is sent: what it echoes is what went out.
        let conn = Connection::spawn(
            "cat",
            &[],
            &[],
            Path::new("/"),
            "cat",
            Box::new(move |_, msg| {
                if let Incoming::Notification { method, .. } = msg {
                    heard.lock().unwrap().push(method);
                }
            }),
        )
        .unwrap();
        let config =
            ServerConfig { name: "cat".into(), ..serde_json::from_value(json!({"languages": ["rust"], "command": "cat"})).unwrap() };
        let server = Arc::new(Server {
            config,
            conn,
            caps: json!({}),
            encoding: Encoding::Utf16,
            opened: Mutex::default(),
            borrowed: Mutex::default(),
        });
        let path = Path::new("/w/a.rs");
        server.opened.lock().unwrap().insert(path.to_path_buf());
        m.docs.lock().unwrap().insert(path.to_path_buf(), Doc { text: String::new(), version: 1, tabs: 1 });
        let key: SlotKey = ("cat".into(), "/w".into());
        let slot = Arc::new(AsyncMutex::new(Slot { server: Some(server.clone()), ..Default::default() }));
        m.slots.lock().unwrap().insert(key.clone(), slot.clone());
        m.register(key, &server);
        let _busy = futures::executor::block_on(slot.lock());
        m.change(path, "x".into());
        let start = Instant::now();
        while !seen.lock().unwrap().iter().any(|s| s == "textDocument/didChange") {
            assert!(start.elapsed() < Duration::from_secs(5), "no didChange: {:?}", seen.lock().unwrap());
            std::thread::sleep(Duration::from_millis(20));
        }
        server.conn.kill();
    }

    #[cfg(unix)]
    #[test]
    fn only_absolute_path_entries_are_searched() {
        // `.`, empty and relative entries would find a program in the project
        // root (the server's working folder), planted by its authors.
        let path = std::ffi::OsString::from("/opt/bin::.:node_modules/.bin:/usr/bin");
        assert_eq!(absolute_path_entries(&path), std::ffi::OsString::from("/opt/bin:/usr/bin"));
    }

    #[test]
    fn read_text_refuses_what_is_no_source_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_text(dir.path()), None, "a folder");
        let big = dir.path().join("big.rs");
        std::fs::File::create(&big).unwrap().set_len(MAX_FILE_BYTES + 1).unwrap();
        assert_eq!(read_text(&big), None, "too large");
        let small = dir.path().join("a.rs");
        std::fs::write(&small, "fn a() {}\n").unwrap();
        assert_eq!(read_text(&small).as_deref(), Some("fn a() {}\n"));
        #[cfg(unix)]
        assert_eq!(read_text(Path::new("/dev/zero")), None, "a device");
    }
}
