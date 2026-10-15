//! The daemon: `tern --server`.
//!
//! One per user (and per build flavor, see [`crate::paths::mux_dir`]). It
//! owns every terminal's program, the Claude Code hook server and their
//! settings files, so all of them outlive the UI. Each UI connection gets a
//! reader thread and a writer thread; the writer's queue is bounded so a UI
//! that stops reading never stalls a program (the connection is dropped and
//! the UI reattaches).

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
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
use crate::proto::{self, ClientMsg, FrameReader, MAJOR, MINOR, ServerMsg};

/// Queued output a client may fall behind by before it is dropped.
pub const MAX_QUEUED: usize = 64 << 20;
/// Hook requests kept for a session while no client is attached.
const MAX_QUEUED_HOOKS: usize = 64;
const SETTINGS_PLACEHOLDER: &str = "{tern-settings}";

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
}

/// `tern --server`: serve until idle.
pub fn run() -> ! {
    // SAFETY: plain signal dispositions, set before any thread starts.
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
}

struct State {
    sessions: HashMap<u64, Session>,
    clients: usize,
    /// When the daemon last had neither sessions nor clients.
    idle_since: Option<Instant>,
}

pub(crate) struct Server {
    state: Mutex<State>,
    hooks: Option<HookServer>,
    run_dir: PathBuf,
    notifications: bool,
    max_queued: usize,
}

/// Serve on `dir` until idle for `opts.idle_exit`. Fails with `AddrInUse`
/// when another daemon holds the directory.
pub fn serve(dir: &Path, opts: ServeOpts) -> io::Result<()> {
    paths::ensure_private_dir(dir)?;
    let _lock = lock(&dir.join("server.lock"))?;
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
            state: Mutex::new(State { sessions: HashMap::new(), clients: 0, idle_since: Some(Instant::now()) }),
            hooks,
            run_dir,
            notifications: opts.notifications,
            max_queued: opts.max_queued,
        }
    });
    log::info!("daemon {} listening on {}", std::process::id(), sock.display());

    let done = Arc::new(AtomicBool::new(false));
    {
        let (server, done, sock) = (server.clone(), done.clone(), sock.clone());
        std::thread::Builder::new().name("idle".into()).spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(100).min(opts.idle_exit));
                let idle = server.state.lock().unwrap().idle_since;
                if idle.is_some_and(|t| t.elapsed() >= opts.idle_exit) {
                    done.store(true, Ordering::Release);
                    // Wake the accept loop.
                    let _ = UnixStream::connect(&sock);
                    return;
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
    log::info!("daemon idle, exiting");
    Ok(())
}

fn lock(path: &Path) -> io::Result<File> {
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(path)?;
    // SAFETY: flock on an fd we own.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let e = io::Error::last_os_error();
        return Err(if e.raw_os_error() == Some(libc::EWOULDBLOCK) { io::ErrorKind::AddrInUse.into() } else { e });
    }
    Ok(file)
}

/// Single-quote `s` for sh.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
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
        let conn = Conn::start(stream.try_clone()?, self.max_queued)?;
        conn.send(ServerMsg::Hello { major: MAJOR, minor: MINOR, app_version: env!("CARGO_PKG_VERSION").into(), pid: std::process::id() });
        let mut reader = FrameReader::new(stream, proto::MAX_CLIENT_FRAME);
        match reader.read::<ClientMsg>()? {
            Some(ClientMsg::Hello { major, .. }) if major == MAJOR => {},
            other => {
                conn.close();
                return Err(io::Error::new(io::ErrorKind::InvalidData, format!("bad hello: {other:?}")));
            },
        }
        {
            let mut st = self.state.lock().unwrap();
            st.clients += 1;
            Self::update_idle(&mut st);
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
        st.clients -= 1;
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
            ClientMsg::List { req } => {
                let list = self.state.lock().unwrap().sessions.values().map(|s| s.host.info()).collect();
                conn.send(ServerMsg::Sessions { req, list });
            },
        }
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
            st.sessions.insert(id, Session { host: host.clone(), token: token.clone(), owner: None, hooks: VecDeque::new() });
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
        let fd = host.master_fd();
        // SAFETY: dup of an fd the host keeps open.
        let fd = unsafe { libc::dup(fd) };
        let fd = (fd >= 0).then(|| unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) });
        let pid = host.pid();
        let sub: Arc<dyn Subscriber> = conn.clone();
        // The reply is queued under the terminal lock, before any output
        // that follows the snapshot.
        host.attach_with(sub, |snapshot| conn.push(Out::Attached { req, id, token: token.clone(), pid, fd, snapshot: Box::new(snapshot) }));
        for body in hooks {
            if let Some(token) = &token {
                conn.send(ServerMsg::Hook { token: token.clone(), body });
            }
        }
        if let Some(size) = size
            && host.exited().is_none()
        {
            host.jiggle(size);
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
    Frame(Vec<u8>),
    /// Packed off the reader's thread (it can take a while for a big
    /// scrollback), then sent with the PTY fd.
    Attached {
        req: u64,
        id: u64,
        token: Option<String>,
        pid: u32,
        fd: Option<OwnedFd>,
        snapshot: Box<Snapshot>,
    },
}

pub(crate) struct Conn {
    tx: Mutex<Option<Sender<Out>>>,
    max_queued: usize,
    queued: Arc<AtomicUsize>,
    dead: AtomicBool,
    stream: UnixStream,
}

impl Conn {
    fn start(stream: UnixStream, max_queued: usize) -> io::Result<Arc<Self>> {
        let (tx, rx) = mpsc::channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let conn = Arc::new(Self {
            tx: Mutex::new(Some(tx)),
            max_queued,
            queued: queued.clone(),
            dead: AtomicBool::new(false),
            stream: stream.try_clone()?,
        });
        let weak = Arc::downgrade(&conn);
        std::thread::Builder::new().name("client writer".into()).spawn(move || writer(stream, rx, queued, weak))?;
        Ok(conn)
    }

    fn push(&self, out: Out) {
        if self.dead.load(Ordering::Acquire) {
            return;
        }
        let len = match &out {
            Out::Frame(f) => f.len(),
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
            Ok(frame) => self.push(Out::Frame(frame)),
            Err(e) => log::error!("encode: {e}"),
        }
    }

    fn close(&self) {
        self.dead.store(true, Ordering::Release);
        self.tx.lock().unwrap().take();
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

impl Subscriber for Conn {
    fn output(&self, id: u64, bytes: &[u8]) {
        self.send(ServerMsg::Output { id, bytes: bytes.to_vec() });
    }

    fn marker(&self, id: u64, marker: &Marker) {
        self.send(ServerMsg::Marker { id, marker: marker.clone() });
    }
}

fn writer(mut stream: UnixStream, rx: Receiver<Out>, queued: Arc<AtomicUsize>, conn: Weak<Conn>) {
    let result = (|| -> io::Result<()> {
        for out in rx {
            match out {
                Out::Frame(frame) => {
                    stream.write_all(&frame)?;
                    queued.fetch_sub(frame.len(), Ordering::AcqRel);
                },
                Out::Attached { req, id, token, pid, fd, snapshot } => {
                    let msg = ServerMsg::Attached { req, id, token, pid, fd: fd.is_some(), snapshot: proto::pack_snapshot(&snapshot)? };
                    match &fd {
                        Some(fd) => proto::write_frame_with_fd(&stream, &msg, fd)?,
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
