//! The daemon: `tern --server`.
//!
//! One per user (and per build flavor, see [`crate::paths::mux_dir`]). It
//! owns every terminal's program, the Claude Code hook server and their
//! settings files, so all of them outlive the UI. Each UI connection gets a
//! reader thread and a writer thread; the writer's queue is bounded so a UI
//! that stops reading never stalls a program. A session that falls behind is
//! detached alone (its tab reattaches from a snapshot); a connection that
//! falls behind as a whole is dropped (every tab reattaches).
//!
//! Observers (the menu bar agent) watch the session list and ask for a
//! session to be shown; they don't keep the daemon alive.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{self, Write};
#[cfg(unix)]
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use tern_bridge::{HookEvent, HookServer};
use tern_term::mirror::Marker;
use tern_term::snapshot::Snapshot;
use tern_term::status::ActivityStatus;
use tern_term::{SpawnConfig, TermSize};

use crate::host::{Host, Observer, Subscriber};
use crate::paths;
use crate::proto::{self, ClientMsg, FrameReader, MAJOR, MINOR, Role, ServerMsg, SessionInfo};
use crate::{UnixListener, UnixStream};

/// Queued output a client may fall behind by, over all its sessions, before
/// it is dropped.
pub const MAX_QUEUED: usize = 256 << 20;
/// Queued output one session may fall behind by before it is detached.
pub const MAX_QUEUED_SESSION: usize = 64 << 20;
/// Hook requests kept for a session while no client is attached.
const MAX_QUEUED_HOOKS: usize = 64;
const SETTINGS_PLACEHOLDER: &str = "{tern-settings}";
/// The PTY a client gets with `Attached`, for its foreground-process
/// queries. Windows passes none: the client asks by the shell's pid.
#[cfg(unix)]
type PassedPty = OwnedFd;
#[cfg(windows)]
type PassedPty = std::convert::Infallible;
/// How often a watcher's list is checked for changes (also coalesces bursts).
pub const WATCH_INTERVAL: Duration = Duration::from_millis(250);
/// A watcher's list asks the OS about processes every this many checks.
const WATCH_SCANS_EVERY: u32 = 8;

pub struct ServeOpts {
    /// Exit after this long with no sessions and no client.
    pub idle_exit: Duration,
    /// Post macOS notifications while no client is connected.
    pub notifications: bool,
    /// Parent of the per-daemon dir that holds Claude settings files.
    pub run_root: PathBuf,
    /// Start the hook server.
    pub hooks: bool,
    /// Output a client may fall behind by before it is dropped.
    pub max_queued: usize,
    /// Output one session may fall behind by before it is detached.
    pub max_queued_session: usize,
}

/// `tern --server`: serve until idle.
pub fn run() -> ! {
    // SAFETY: plain signal dispositions, set before any thread starts.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        libc::signal(libc::SIGINT, libc::SIG_IGN);
    }
    let opts = ServeOpts {
        idle_exit: Duration::from_secs(10),
        notifications: true,
        run_root: paths::run_root(),
        hooks: true,
        max_queued: MAX_QUEUED,
        max_queued_session: MAX_QUEUED_SESSION,
    };
    let code = match serve(&paths::mux_dir(), opts) {
        Ok(()) => 0,
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
            log::info!("another daemon is running");
            0
        },
        Err(e) => {
            log::error!("daemon failed: {e}");
            1
        },
    };
    std::process::exit(code)
}

struct Session {
    host: Arc<Host>,
    token: Option<String>,
    /// The connection attached to it.
    owner: Option<Weak<Conn>>,
    /// Hook requests that came while no one was attached.
    hooks: VecDeque<Vec<u8>>,
    /// `session_id` of its latest Claude hook event.
    claude_session: Option<String>,
}

struct State {
    sessions: HashMap<u64, Session>,
    /// UI connections (observers don't count).
    clients: usize,
    /// UI connections, oldest first (`Reveal` goes to the newest).
    uis: Vec<Weak<Conn>>,
    /// Observer connections, closed when the daemon exits (they don't hold
    /// it, so they may still be there).
    observers: Vec<Weak<Conn>>,
    /// When the daemon last had neither sessions nor clients.
    idle_since: Option<Instant>,
}

pub(crate) struct Server {
    state: Mutex<State>,
    hooks: Option<HookServer>,
    run_dir: PathBuf,
    notifications: bool,
    max_queued: usize,
    max_queued_session: usize,
}

/// Serve on `dir` until idle for `opts.idle_exit`. Fails with `AddrInUse`
/// when another daemon holds the directory.
pub fn serve(dir: &Path, opts: ServeOpts) -> io::Result<()> {
    paths::ensure_private_dir(dir)?;
    // Before an update can replace the executable.
    paths::build_id();
    let _lock = lock(&paths::lock_path(dir))?;
    let sock = paths::socket_path(dir);
    match UnixStream::connect(&sock) {
        Ok(_) => return Err(io::ErrorKind::AddrInUse.into()),
        Err(_) => {
            let _ = std::fs::remove_file(&sock);
        },
    }
    let listener = UnixListener::bind(&sock)?;
    tern_bridge::sweep_run_dirs(&opts.run_root);
    let run_dir = opts.run_root.join(std::process::id().to_string());

    let server = Arc::new_cyclic(|weak: &Weak<Server>| {
        let hooks = opts
            .hooks
            .then(|| {
                let weak = weak.clone();
                HookServer::start(Arc::new(move |ev: HookEvent| {
                    if let Some(s) = weak.upgrade() {
                        s.on_hook(ev);
                    }
                }))
                .map_err(|e| log::warn!("hook server unavailable: {e}"))
                .ok()
            })
            .flatten();
        Server {
            state: Mutex::new(State {
                sessions: HashMap::new(),
                clients: 0,
                uis: Vec::new(),
                observers: Vec::new(),
                idle_since: Some(Instant::now()),
            }),
            hooks,
            run_dir,
            notifications: opts.notifications,
            max_queued: opts.max_queued,
            max_queued_session: opts.max_queued_session,
        }
    });
    log::info!("daemon {} listening on {}", std::process::id(), sock.display());

    let done = Arc::new(AtomicBool::new(false));
    {
        let (server, done, sock) = (server.clone(), done.clone(), sock.clone());
        std::thread::Builder::new().name("idle".into()).spawn(move || {
            // Wakes when an idle daemon may be due to exit, and once a
            // second otherwise to see whether it has gone idle.
            let mut wait = Duration::from_millis(100).min(opts.idle_exit);
            loop {
                std::thread::sleep(wait);
                let idle = server.state.lock().unwrap().idle_since;
                match idle.map(|t| t.elapsed()) {
                    Some(for_) if for_ >= opts.idle_exit => {
                        done.store(true, Ordering::Release);
                        // Wake the accept loop.
                        let _ = UnixStream::connect(&sock);
                        return;
                    },
                    Some(for_) => wait = opts.idle_exit - for_,
                    None => wait = Duration::from_secs(1).min(opts.idle_exit),
                }
            }
        })?;
    }

    for stream in listener.incoming() {
        if done.load(Ordering::Acquire) {
            break;
        }
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                log::warn!("accept: {e}");
                continue;
            },
        };
        if !paths::peer_uid_ok(&stream) {
            log::warn!("refused a connection from another user");
            continue;
        }
        let server = server.clone();
        let _ = std::thread::Builder::new().name("client".into()).spawn(move || {
            if let Err(e) = server.handle(stream) {
                log::info!("client: {e}");
            }
        });
    }
    let _ = std::fs::remove_file(&sock);
    let _ = std::fs::remove_dir_all(&server.run_dir);
    // Observers learn at once that this daemon is gone.
    let observers = std::mem::take(&mut server.state.lock().unwrap().observers);
    for c in observers.iter().filter_map(Weak::upgrade) {
        c.close();
    }
    log::info!("daemon idle, exiting");
    Ok(())
}

fn lock(path: &Path) -> io::Result<File> {
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(io::ErrorKind::AddrInUse.into()),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// Single-quote `s` for sh.
#[cfg(unix)]
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Single-quote `s` for PowerShell, which Claude tabs run in on Windows. It
/// takes the curly quotes U+2018–U+201B for `'` too: double them all.
#[cfg(windows)]
fn shell_quote(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        if matches!(c, '\'' | '\u{2018}'..='\u{201B}') {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

fn random_id() -> u64 {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).expect("getrandom");
    // Ids go through JSON-ish paths in the UI; keep them positive i64.
    u64::from_le_bytes(b) >> 1
}

impl Server {
    fn update_idle(state: &mut State) {
        let idle = state.sessions.is_empty() && state.clients == 0;
        match (idle, state.idle_since) {
            (true, None) => state.idle_since = Some(Instant::now()),
            (false, Some(_)) => state.idle_since = None,
            _ => {},
        }
    }

    fn handle(self: &Arc<Self>, stream: UnixStream) -> io::Result<()> {
        let conn = Conn::start(stream.try_clone()?, self.max_queued, self.max_queued_session)?;
        conn.send(ServerMsg::Hello {
            major: MAJOR,
            minor: MINOR,
            app_version: env!("CARGO_PKG_VERSION").into(),
            pid: std::process::id(),
            build: paths::build_id().into(),
        });
        let mut reader = FrameReader::new(stream, proto::MAX_CLIENT_FRAME);
        let role = match reader.read::<ClientMsg>()? {
            Some(ClientMsg::Hello { major, role, .. }) if major == MAJOR => role,
            other => {
                conn.close();
                return Err(io::Error::new(io::ErrorKind::InvalidData, format!("bad hello: {other:?}")));
            },
        };
        if role == Role::Ui {
            let mut st = self.state.lock().unwrap();
            st.clients += 1;
            st.uis.push(Arc::downgrade(&conn));
            Self::update_idle(&mut st);
        } else {
            self.state.lock().unwrap().observers.push(Arc::downgrade(&conn));
        }
        let result = (|| {
            while let Some(msg) = reader.read::<ClientMsg>()? {
                if conn.dead.load(Ordering::Acquire) {
                    break;
                }
                self.on_msg(&conn, msg);
            }
            Ok(())
        })();
        conn.close();
        let mut st = self.state.lock().unwrap();
        if role == Role::Ui {
            st.clients -= 1;
            st.uis.retain(|w| w.upgrade().is_some_and(|c| !Arc::ptr_eq(&c, &conn)));
        } else {
            st.observers.retain(|w| w.upgrade().is_some_and(|c| !Arc::ptr_eq(&c, &conn)));
        }
        for s in st.sessions.values_mut() {
            if s.owner.as_ref().is_some_and(|o| o.as_ptr() == Arc::as_ptr(&conn)) {
                s.owner = None;
                s.host.detach(&(conn.clone() as Arc<dyn Subscriber>));
            }
        }
        Self::update_idle(&mut st);
        result
    }

    fn on_msg(self: &Arc<Self>, conn: &Arc<Conn>, msg: ClientMsg) {
        match msg {
            ClientMsg::Hello { .. } => {},
            ClientMsg::Watch => self.watch(conn),
            ClientMsg::Reveal { req, id } => {
                let ui = self.state.lock().unwrap().uis.iter().rev().filter_map(Weak::upgrade).find(|c| !c.dead.load(Ordering::Acquire));
                if let Some(ui) = &ui {
                    ui.send(ServerMsg::Reveal { id });
                }
                conn.send(ServerMsg::Revealed { req, delivered: ui.is_some() });
            },
            ClientMsg::Spawn { req, cfg, hook } => self.spawn(conn, req, cfg, hook),
            ClientMsg::Attach { req, id, size } => self.attach(conn, req, id, size),
            ClientMsg::Detach { id } => {
                let mut st = self.state.lock().unwrap();
                if let Some(s) = st.sessions.get_mut(&id)
                    && s.owner.as_ref().is_some_and(|o| o.as_ptr() == Arc::as_ptr(conn))
                {
                    s.owner = None;
                    s.host.detach(&(conn.clone() as Arc<dyn Subscriber>));
                }
            },
            ClientMsg::Input { id, bytes } => {
                if let Some(h) = self.host(id) {
                    h.input(bytes);
                }
            },
            ClientMsg::Resize { id, size } => {
                if let Some(h) = self.host(id) {
                    h.resize(size);
                }
            },
            ClientMsg::Close { id } => self.close(id),
            ClientMsg::List { req } => conn.send(ServerMsg::Sessions { req, list: self.list() }),
        }
    }

    /// Every session, by id (a stable order: watchers compare lists).
    /// Built outside the state lock: `info` asks the OS about processes.
    fn list(&self) -> Vec<SessionInfo> {
        self.list_with(Host::info)
    }

    /// [`Self::list`], with each session's `cwd` and `program` taken from
    /// `last` instead of asked of the OS.
    fn list_keeping(&self, last: &[SessionInfo]) -> Vec<SessionInfo> {
        self.list_with(|h| {
            let info = h.info_without_processes();
            match last.iter().find(|l| l.id == info.id) {
                Some(l) => SessionInfo { cwd: l.cwd.clone(), program: l.program.clone(), ..info },
                None => h.info(),
            }
        })
    }

    fn list_with(&self, info: impl Fn(&Host) -> SessionInfo) -> Vec<SessionInfo> {
        let sessions: Vec<_> =
            self.state.lock().unwrap().sessions.values().map(|s| (s.host.clone(), s.token.is_some(), s.claude_session.clone())).collect();
        let mut list: Vec<_> =
            sessions.into_iter().map(|(h, claude, claude_session)| SessionInfo { claude, claude_session, ..info(&h) }).collect();
        list.sort_by_key(|i| i.id);
        list
    }

    /// Send `conn` the session list now and whenever it changes (checked
    /// every `WATCH_INTERVAL`, which also coalesces bursts).
    fn watch(self: &Arc<Self>, conn: &Arc<Conn>) {
        if conn.watching.swap(true, Ordering::AcqRel) {
            return;
        }
        let (server, conn) = (Arc::downgrade(self), Arc::downgrade(conn));
        let _ = std::thread::Builder::new().name("watch".into()).spawn(move || {
            let mut last: Option<Vec<SessionInfo>> = None;
            let mut ticks = 0u32;
            loop {
                let (Some(server), Some(conn)) = (server.upgrade(), conn.upgrade()) else { return };
                if conn.dead.load(Ordering::Acquire) {
                    return;
                }
                // Programs and folders change rarely and cost a process scan:
                // asked every few ticks, kept in between.
                let list = match &last {
                    Some(l) if ticks % WATCH_SCANS_EVERY != 0 => server.list_keeping(l),
                    _ => server.list(),
                };
                ticks = ticks.wrapping_add(1);
                if last.as_ref() != Some(&list) {
                    conn.send(ServerMsg::SessionsChanged { list: list.clone() });
                    last = Some(list);
                }
                drop((server, conn));
                std::thread::sleep(WATCH_INTERVAL);
            }
        });
    }

    fn host(&self, id: u64) -> Option<Arc<Host>> {
        self.state.lock().unwrap().sessions.get(&id).map(|s| s.host.clone())
    }

    fn spawn(self: &Arc<Self>, conn: &Arc<Conn>, req: u64, mut cfg: SpawnConfig, hook: bool) {
        let id = random_id();
        let token = match (hook, &self.hooks) {
            (true, Some(hooks)) => {
                let token = hooks.register();
                match tern_bridge::write_claude_settings(&self.run_dir, &token, &hooks.hook_url(&token)) {
                    Ok(path) => {
                        let quoted = shell_quote(&path.to_string_lossy());
                        for a in &mut cfg.args {
                            *a = a.replace(SETTINGS_PLACEHOLDER, &quoted);
                        }
                        Some(token)
                    },
                    Err(e) => {
                        log::warn!("can't write Claude settings: {e}");
                        hooks.unregister(&token);
                        None
                    },
                }
            },
            _ => None,
        };
        if token.is_none() {
            for a in &mut cfg.args {
                *a = a.replace(&format!(" --settings {SETTINGS_PLACEHOLDER}"), "").replace(SETTINGS_PLACEHOLDER, "");
            }
        }
        let observer: Arc<dyn Observer> = self.clone();
        let host = match Host::spawn(id, &cfg, observer) {
            Ok(h) => h,
            Err(e) => {
                if let Some(t) = &token {
                    self.drop_token(t);
                }
                return conn.send(ServerMsg::Error { req, message: e.to_string() });
            },
        };
        log::info!("session {id}: started pid {}", host.pid());
        {
            let mut st = self.state.lock().unwrap();
            st.sessions.insert(
                id,
                Session { host: host.clone(), token: token.clone(), owner: None, hooks: VecDeque::new(), claude_session: None },
            );
            Self::update_idle(&mut st);
        }
        self.attach_host(conn, req, id, None);
    }

    fn attach(self: &Arc<Self>, conn: &Arc<Conn>, req: u64, id: u64, size: TermSize) {
        if self.host(id).is_none() {
            return conn.send(ServerMsg::NotFound { req, id });
        }
        self.attach_host(conn, req, id, Some(size));
    }

    /// Attach `conn` to session `id`; with `size`, make the program repaint
    /// at that size.
    fn attach_host(self: &Arc<Self>, conn: &Arc<Conn>, req: u64, id: u64, size: Option<TermSize>) {
        let (host, token, previous, hooks) = {
            let mut st = self.state.lock().unwrap();
            let Some(s) = st.sessions.get_mut(&id) else { return conn.send(ServerMsg::NotFound { req, id }) };
            let previous = s.owner.replace(Arc::downgrade(conn)).and_then(|w| w.upgrade()).filter(|p| !Arc::ptr_eq(p, conn));
            (s.host.clone(), s.token.clone(), previous, std::mem::take(&mut s.hooks))
        };
        if let Some(p) = previous {
            p.send(ServerMsg::Detached { id });
        }
        // Closed on exec: programs spawned meanwhile mustn't inherit it.
        // SAFETY: dup of an fd the host keeps open.
        #[cfg(unix)]
        let fd = unsafe { libc::fcntl(host.master_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        #[cfg(unix)]
        let fd = (fd >= 0).then(|| unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) });
        #[cfg(windows)]
        let fd: Option<PassedPty> = None;
        let pid = host.pid();
        let sub: Arc<dyn Subscriber> = conn.clone();
        // The reply is queued under the terminal lock, before any output
        // that follows the snapshot; an exit is either before it (said
        // here) or after it (sent as the marker).
        let mut exited = None;
        host.attach_with(sub, |snapshot| {
            // The snapshot has whatever it skipped while it lagged.
            conn.caught_up(id);
            conn.push(Out::Attached { req, id, token: token.clone(), pid, fd, snapshot: Box::new(snapshot) });
            exited = host.exited();
            if let Some(code) = exited {
                conn.send(ServerMsg::Marker { id, marker: Marker::Exited(code) });
            }
        });
        for body in hooks {
            if let Some(token) = &token {
                conn.send(ServerMsg::Hook { token: token.clone(), body });
            }
        }
        if let (None, Some(size)) = (exited, size) {
            if host.shell_in_foreground() {
                // A shell at its prompt: the snapshot is all there is; a
                // jiggle would only make it draw the prompt twice.
                if size != host.size() {
                    host.resize(size);
                }
            } else {
                host.jiggle(size);
            }
        }
    }

    fn close(&self, id: u64) {
        let s = {
            let mut st = self.state.lock().unwrap();
            let s = st.sessions.remove(&id);
            Self::update_idle(&mut st);
            s
        };
        if let Some(s) = s {
            log::info!("session {id}: closed");
            s.host.kill();
            if let Some(t) = &s.token {
                self.drop_token(t);
            }
        }
    }

    fn drop_token(&self, token: &str) {
        if let Some(h) = &self.hooks {
            h.unregister(token);
        }
        tern_bridge::remove_claude_settings(&self.run_dir, token);
    }

    fn on_hook(&self, ev: HookEvent) {
        let body = serde_json::to_vec(&ev.raw).unwrap_or_default();
        let mut st = self.state.lock().unwrap();
        let Some(s) = st.sessions.values_mut().find(|s| s.token.as_deref() == Some(ev.token.as_str())) else { return };
        if let Some(id) = &ev.session_id {
            s.claude_session = Some(id.clone());
        }
        match s.owner.as_ref().and_then(Weak::upgrade) {
            Some(conn) => conn.send(ServerMsg::Hook { token: ev.token, body }),
            None => {
                if s.hooks.len() >= MAX_QUEUED_HOOKS {
                    s.hooks.pop_front();
                }
                s.hooks.push_back(body);
            },
        }
    }

    fn unattended(&self) -> bool {
        self.notifications && self.state.lock().unwrap().clients == 0
    }

    fn agent_name(&self, id: u64) -> String {
        let host = self.host(id);
        let programs = host.map(|h| h.foreground_programs()).unwrap_or_default();
        if programs.iter().any(|p| p.contains("codex")) {
            "Codex".into()
        } else if programs.iter().any(|p| p.contains("opencode")) {
            "OpenCode".into()
        } else {
            "Claude".into()
        }
    }
}

impl Observer for Server {
    fn status(&self, id: u64, _old: ActivityStatus, new: ActivityStatus) {
        if !self.unattended() {
            return;
        }
        let name = self.agent_name(id);
        match new {
            ActivityStatus::Blocked => crate::notify::post(&format!("{name} needs your input"), "Waiting for permission or an answer"),
            ActivityStatus::Done => crate::notify::post(&format!("{name} finished"), "The turn is complete"),
            _ => {},
        }
    }

    fn notify(&self, id: u64, title: Option<String>, body: String) {
        if self.unattended() {
            let title = title.filter(|t| !t.is_empty()).unwrap_or_else(|| self.agent_name(id));
            crate::notify::post(&title, &body);
        }
    }

    fn exited(&self, id: u64, code: Option<i32>) {
        log::info!("session {id}: exited ({code:?})");
    }
}

/// What a connection's writer thread sends.
enum Out {
    /// A frame, and the session whose output it is.
    Frame(Vec<u8>, Option<u64>),
    /// Packed off the reader's thread (it can take a while for a big
    /// scrollback), then sent with the PTY fd.
    Attached { req: u64, id: u64, token: Option<String>, pid: u32, fd: Option<PassedPty>, snapshot: Box<Snapshot> },
}

pub(crate) struct Conn {
    tx: Mutex<Option<Sender<Out>>>,
    max_queued: usize,
    max_queued_session: usize,
    queued: Arc<AtomicUsize>,
    /// Output queued per session.
    per_session: Arc<Mutex<HashMap<u64, usize>>>,
    /// Sessions that fell behind: told `Detached`, their output dropped
    /// until they attach again.
    lagging: Mutex<HashSet<u64>>,
    dead: AtomicBool,
    /// A `watch` thread serves it.
    watching: AtomicBool,
    stream: UnixStream,
}

impl Conn {
    fn start(stream: UnixStream, max_queued: usize, max_queued_session: usize) -> io::Result<Arc<Self>> {
        let (tx, rx) = mpsc::channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let per_session = Arc::new(Mutex::new(HashMap::new()));
        let conn = Arc::new(Self {
            tx: Mutex::new(Some(tx)),
            max_queued,
            max_queued_session,
            queued: queued.clone(),
            per_session: per_session.clone(),
            lagging: Mutex::new(HashSet::new()),
            dead: AtomicBool::new(false),
            watching: AtomicBool::new(false),
            stream: stream.try_clone()?,
        });
        let weak = Arc::downgrade(&conn);
        std::thread::Builder::new().name("client writer".into()).spawn(move || writer(stream, rx, queued, per_session, weak))?;
        Ok(conn)
    }

    fn push(&self, out: Out) {
        if self.dead.load(Ordering::Acquire) {
            return;
        }
        let len = match &out {
            Out::Frame(f, _) => f.len(),
            Out::Attached { .. } => 0,
        };
        if self.queued.fetch_add(len, Ordering::AcqRel) + len > self.max_queued {
            log::warn!("client fell behind; dropping it");
            return self.close();
        }
        if let Some(tx) = &*self.tx.lock().unwrap() {
            let _ = tx.send(out);
        }
    }

    fn send(&self, msg: ServerMsg) {
        match proto::encode(&msg) {
            Ok(frame) => self.push(Out::Frame(frame, None)),
            Err(e) => log::error!("encode: {e}"),
        }
    }

    /// Queue session `id`'s output, or drop it while the session lags.
    fn send_output(&self, id: u64, msg: ServerMsg) {
        if self.lagging.lock().unwrap().contains(&id) {
            return;
        }
        let frame = match proto::encode(&msg) {
            Ok(f) => f,
            Err(e) => return log::error!("encode: {e}"),
        };
        {
            let mut per = self.per_session.lock().unwrap();
            let queued = per.entry(id).or_default();
            if *queued + frame.len() > self.max_queued_session {
                drop(per);
                log::warn!("session {id} fell behind; detaching it");
                self.lagging.lock().unwrap().insert(id);
                return self.send(ServerMsg::Detached { id });
            }
            *queued += frame.len();
        }
        self.push(Out::Frame(frame, Some(id)));
    }

    /// Session `id` attaches again (from a snapshot): send its output again.
    fn caught_up(&self, id: u64) {
        self.lagging.lock().unwrap().remove(&id);
    }

    fn close(&self) {
        self.dead.store(true, Ordering::Release);
        self.tx.lock().unwrap().take();
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

impl Subscriber for Conn {
    fn output(&self, id: u64, bytes: &[u8]) {
        self.send_output(id, ServerMsg::Output { id, bytes: bytes.to_vec() });
    }

    fn marker(&self, id: u64, marker: &Marker) {
        self.send_output(id, ServerMsg::Marker { id, marker: marker.clone() });
    }
}

fn writer(
    mut stream: UnixStream,
    rx: Receiver<Out>,
    queued: Arc<AtomicUsize>,
    per_session: Arc<Mutex<HashMap<u64, usize>>>,
    conn: Weak<Conn>,
) {
    let result = (|| -> io::Result<()> {
        for out in rx {
            match out {
                Out::Frame(frame, id) => {
                    stream.write_all(&frame)?;
                    queued.fetch_sub(frame.len(), Ordering::AcqRel);
                    if let Some(id) = id {
                        let mut per = per_session.lock().unwrap();
                        if let Some(n) = per.get_mut(&id) {
                            *n -= frame.len();
                            if *n == 0 {
                                per.remove(&id);
                            }
                        }
                    }
                },
                Out::Attached { req, id, token, pid, fd, snapshot } => {
                    let msg = ServerMsg::Attached { req, id, token, pid, fd: fd.is_some(), snapshot: proto::pack_snapshot(&snapshot)? };
                    match &fd {
                        #[cfg(unix)]
                        Some(fd) => proto::write_frame_with_fd(&stream, &msg, fd)?,
                        #[cfg(windows)]
                        Some(never) => match *never {},
                        None => proto::write_frame(&mut stream, &msg)?,
                    }
                },
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        log::info!("client writer: {e}");
    }
    if let Some(c) = conn.upgrade() {
        c.close();
    }
}

#[cfg(test)]
mod tests {
    use super::shell_quote;

    #[test]
    #[cfg(unix)]
    fn quotes_for_sh() {
        assert_eq!(shell_quote("/a b/it's"), r"'/a b/it'\''s'");
    }

    #[test]
    #[cfg(windows)]
    fn quotes_for_powershell() {
        assert_eq!(shell_quote(r"C:\a b\it's"), r"'C:\a b\it''s'");
        // PowerShell ends a single-quoted string at a curly quote too.
        assert_eq!(shell_quote("x\u{2019}; calc"), "'x\u{2019}\u{2019}; calc'");
    }
}
