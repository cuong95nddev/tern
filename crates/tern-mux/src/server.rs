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
//! Observers (the menu bar agent, the web server) watch the session list,
//! ask for a session to be shown and may view sessions beside their owner;
//! they don't keep the daemon alive.
//!
//! On Unix a daemon can hand its sessions to a daemon of a newer build
//! (`tern --server --adopt`, see [`Server::hand_off`] and [`adopt`]): the
//! programs keep running and the UIs reattach to the new daemon.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{self, Write};
#[cfg(unix)]
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use tern_bridge::context::{Action, Guard, Policy};
use tern_bridge::{HookEvent, HookServer};
use tern_term::mirror::Marker;
use tern_term::snapshot::Snapshot;
use tern_term::status::ActivityStatus;
use tern_term::{SpawnConfig, TermSize};

#[cfg(unix)]
use crate::host::Paused;
use crate::host::{Host, Observer, Subscriber};
use crate::paths;
use crate::proto::{self, ClientMsg, Controller, FrameReader, MAJOR, MINOR, Role, ServerMsg, SessionInfo};
#[cfg(unix)]
use crate::proto::{HANDOFF_VERSION, HandedSession, HandoffMsg};
use crate::{UnixListener, UnixStream};

/// Queued output a client may fall behind by, over all its sessions, before
/// it is dropped.
pub const MAX_QUEUED: usize = 256 << 20;
/// Queued output one session may fall behind by before it is detached.
pub const MAX_QUEUED_SESSION: usize = 64 << 20;
/// How long a daemon whose programs have all ended waits for a UI to show
/// their ends before it exits.
const ENDED_GRACE: Duration = Duration::from_secs(600);
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
/// How long either daemon waits for the other's next step in a handoff.
#[cfg(unix)]
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(10);
/// The fd an adopting daemon gets its predecessor's socket on.
#[cfg(unix)]
const ADOPT_FD: std::os::fd::RawFd = 3;

/// Starts the daemon that adopts the sessions on a handoff, from the given
/// executable: the socket to it, and how to end it if the handoff fails.
#[cfg(unix)]
pub type Successor = Arc<dyn Fn(&Path) -> io::Result<(UnixStream, Box<dyn FnOnce() + Send>)> + Send + Sync>;

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
    /// How a handoff starts the new daemon (`None`: `<exe> --server --adopt`).
    #[cfg(unix)]
    pub successor: Option<Successor>,
}

impl ServeOpts {
    fn daemon() -> Self {
        Self {
            idle_exit: Duration::from_secs(10),
            notifications: true,
            run_root: paths::run_root(),
            hooks: true,
            max_queued: MAX_QUEUED,
            max_queued_session: MAX_QUEUED_SESSION,
            #[cfg(unix)]
            successor: None,
        }
    }
}

/// `tern --server`: serve until idle (`--adopt`: take over the sessions of
/// the daemon that started this one first).
pub fn run() -> ! {
    // SAFETY: plain signal dispositions, set before any thread starts.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        libc::signal(libc::SIGINT, libc::SIG_IGN);
    }
    let opts = ServeOpts::daemon();
    #[cfg(unix)]
    let result = if std::env::args().any(|a| a == "--adopt") {
        // SAFETY: the daemon that started us passed its end of a socketpair
        // as this fd, and nothing else owns it.
        let stream = unsafe { <UnixStream as std::os::fd::FromRawFd>::from_raw_fd(ADOPT_FD) };
        // SAFETY: plain fcntl; programs we start mustn't inherit it.
        unsafe { libc::fcntl(ADOPT_FD, libc::F_SETFD, libc::FD_CLOEXEC) };
        adopt(&paths::mux_dir(), opts, stream)
    } else {
        serve(&paths::mux_dir(), opts)
    };
    #[cfg(windows)]
    let result = serve(&paths::mux_dir(), opts);
    let code = match result {
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
    /// Connections viewing it.
    viewers: Vec<Weak<Conn>>,
    /// The size each connection wants it at, the one that asked last at the
    /// end.
    sizes: Vec<(Weak<Conn>, TermSize)>,
    /// The viewer that has it (a phone): the PTY takes its size and the
    /// owner's typing is dropped, until the owner takes it back (`Drive`)
    /// or the viewer leaves. `None`: the owner has it.
    driver: Option<Weak<Conn>>,
    /// The owner took it back: a viewer's size alone (it refits as it
    /// comes back to the front) no longer takes it, only its typing or a
    /// `Drive`.
    held: bool,
    /// Hook requests that came while no one was attached.
    hooks: VecDeque<Vec<u8>>,
    /// `session_id` of its latest Claude hook event.
    claude_session: Option<String>,
    /// How full its Claude conversation's context window is, and its
    /// handoff past the limit.
    guard: Guard,
    /// The limit its tab set (none until it does).
    policy: Policy,
    /// Its tab muted its notifications.
    muted: bool,
}

/// The program wasn't typed to: focus reports (DECSET 1004), which a
/// terminal sends as its window comes and goes.
fn untyped(bytes: &[u8]) -> bool {
    !bytes.is_empty() && bytes.chunks(3).all(|c| c == b"\x1b[I" || c == b"\x1b[O")
}

impl Session {
    fn is_viewer(&self, conn: *const Conn) -> bool {
        self.viewers.iter().any(|v| v.as_ptr() == conn)
    }

    /// The viewer that has it, while it is there.
    fn driver(&self) -> Option<*const Conn> {
        self.driver.as_ref().filter(|d| d.strong_count() > 0).map(Weak::as_ptr)
    }

    fn size_of(&self, conn: *const Conn) -> Option<TermSize> {
        self.sizes.iter().find(|(c, _)| c.as_ptr() == conn).map(|e| e.1)
    }

    /// The PTY's size: the driver's, else the owner's, else the one that
    /// asked last.
    fn target(&self) -> Option<TermSize> {
        let owner = self.owner.as_ref().map(Weak::as_ptr);
        self.driver()
            .and_then(|d| self.size_of(d))
            .or_else(|| owner.and_then(|o| self.size_of(o)))
            .or_else(|| self.sizes.iter().rev().find(|(c, _)| c.strong_count() > 0).map(|e| e.1))
    }

    /// Who has it, as told to `conn`.
    fn controller(&self, conn: *const Conn) -> Controller {
        match self.driver() {
            Some(d) if d == conn => Controller::You,
            Some(_) => Controller::Viewer,
            None if self.owner.as_ref().is_some_and(|o| o.as_ptr() == conn) => Controller::You,
            None => Controller::Desktop,
        }
    }

    /// Change it with `f`, then resize the PTY to the size that has it, and
    /// tell its owner and viewers when someone else has it. The host
    /// resizes on its own thread, so `host.size()` may not yet hold a
    /// resize just asked for: a change of target always resizes.
    fn update(&mut self, id: u64, f: impl FnOnce(&mut Self)) {
        let was = self.driver();
        let before = self.target();
        f(self);
        if let Some(size) = self.target()
            && (Some(size) != before || size != self.host.size())
        {
            self.host.resize(size);
        }
        if self.driver() != was {
            self.tell_control(id);
        }
    }

    fn tell_control(&self, id: u64) {
        for c in self.owner.iter().chain(&self.viewers).filter_map(Weak::upgrade) {
            c.send(ServerMsg::Control { id, by: self.controller(Arc::as_ptr(&c)) });
        }
    }

    /// `conn` wants the PTY at `size`. A viewer takes it then, unless the
    /// owner took it back.
    fn want(&mut self, conn: &Arc<Conn>, size: TermSize) {
        let p = Arc::as_ptr(conn);
        match self.sizes.iter_mut().find(|(c, _)| c.as_ptr() == p) {
            Some(e) => e.1 = size,
            None => self.sizes.push((Arc::downgrade(conn), size)),
        }
        if self.is_viewer(p) && (!self.held || self.owner.is_none()) {
            self.drive(conn);
        }
    }

    /// `conn` takes it: a viewer has it, or the owner takes it back.
    fn drive(&mut self, conn: &Arc<Conn>) {
        if self.is_viewer(Arc::as_ptr(conn)) {
            self.driver = Some(Arc::downgrade(conn));
            self.held = false;
        } else if self.owner.as_ref().is_some_and(|o| o.as_ptr() == Arc::as_ptr(conn)) {
            self.driver = None;
            self.held = true;
        }
    }

    /// `conn` typed `bytes`: whether they go to the program. A viewer
    /// typing takes it; the owner's typing is dropped while a viewer has it.
    fn typed(&mut self, conn: &Arc<Conn>, bytes: &[u8]) -> bool {
        if self.is_viewer(Arc::as_ptr(conn)) {
            if !untyped(bytes) {
                self.drive(conn);
            }
            return true;
        }
        self.driver().is_none()
    }

    /// `conn` let go of it: the owner has it again when it was the driver.
    fn forget(&mut self, conn: *const Conn) {
        self.sizes.retain(|(c, _)| c.as_ptr() != conn && c.strong_count() > 0);
        if self.driver.as_ref().is_some_and(|d| d.as_ptr() == conn || d.strong_count() == 0) {
            self.driver = None;
        }
    }

    /// `forget`, resizing the PTY to the size that takes over.
    fn let_go(&mut self, id: u64, conn: *const Conn) {
        self.update(id, |s| s.forget(conn));
    }
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
    /// Connections not yet past `Hello`: they hold the daemon like clients.
    connecting: usize,
    /// When the daemon last had neither running programs nor clients.
    idle_since: Option<Instant>,
    /// Decided to exit (or handing off): new connections are turned away.
    exiting: bool,
    /// Handing the sessions to another daemon: no new ones start.
    handing_off: bool,
}

/// Why a daemon stops serving.
enum End {
    Idle,
    /// Another daemon serves its sessions now (and owns its files).
    #[cfg(unix)]
    HandedOff,
}

/// How the hook server starts.
enum Hooks {
    Off,
    Start,
    /// On a listener handed over, with the tokens it gave out.
    #[cfg(unix)]
    Adopt(std::net::TcpListener, Vec<String>),
}

pub(crate) struct Server {
    state: Mutex<State>,
    hooks: Option<HookServer>,
    sock: PathBuf,
    pid_file: PathBuf,
    run_dir: PathBuf,
    notifications: bool,
    max_queued: usize,
    max_queued_session: usize,
    /// The daemon lock, handed over with the sessions.
    #[cfg_attr(windows, allow(dead_code))]
    lock: Mutex<Option<File>>,
    /// The accept loops stop.
    stopped: AtomicBool,
    end: Mutex<Option<Sender<End>>>,
    #[cfg(unix)]
    successor: Option<Successor>,
}

/// Serve on `dir` until idle for `opts.idle_exit`. Fails with `AddrInUse`
/// when another daemon holds the directory.
pub fn serve(dir: &Path, opts: ServeOpts) -> io::Result<()> {
    paths::ensure_private_dir(dir)?;
    // Before an update can replace the executable.
    paths::build_id();
    let lock = lock(&paths::lock_path(dir))?;
    let pid_file = paths::pid_path(dir);
    write_pid(&pid_file);
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
    let hooks = if opts.hooks { Hooks::Start } else { Hooks::Off };
    let server = Server::new(&opts, dir, run_dir, lock, hooks);
    server.claim_run_dir();
    log::info!("daemon {} listening on {}", std::process::id(), sock.display());
    server.run(listener, opts.idle_exit)
}

fn write_pid(path: &Path) {
    if let Err(e) = std::fs::write(path, std::process::id().to_string()) {
        log::warn!("can't write {}: {e}", path.display());
    }
}

fn lock(path: &Path) -> io::Result<File> {
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(io::ErrorKind::AddrInUse.into()),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// Bind `sock` without a moment when no socket is there: bound beside it,
/// then renamed over it (whatever was there, a running daemon's included).
#[cfg(unix)]
fn bind_over(sock: &Path) -> io::Result<UnixListener> {
    let name = sock.file_name().and_then(|n| n.to_str()).unwrap_or("mux.sock");
    let tmp = sock.with_file_name(format!("{name}.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let listener = UnixListener::bind(&tmp)?;
    if let Err(e) = std::fs::rename(&tmp, sock) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(listener)
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

/// Send Claude in `host` what `action` says, once the turn whose hook asked
/// for it has ended and its prompt takes input again. The Enter goes on its
/// own, so it isn't taken as part of a paste.
fn type_after_turn(host: &Arc<Host>, action: Action) {
    let text = match action {
        Action::None => return,
        Action::Clear => "/clear".to_owned(),
        Action::Prompt(p) => p,
    };
    let host = host.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        host.input(text.into_bytes());
        std::thread::sleep(Duration::from_millis(300));
        host.input(b"\r".to_vec());
    });
}

fn random_id() -> u64 {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).expect("getrandom");
    // Ids go through JSON-ish paths in the UI; keep them positive i64.
    u64::from_le_bytes(b) >> 1
}

impl Server {
    fn new(opts: &ServeOpts, dir: &Path, run_dir: PathBuf, lock: File, hooks: Hooks) -> Arc<Self> {
        Arc::new_cyclic(|weak: &Weak<Server>| {
            let weak = weak.clone();
            let sink: tern_bridge::HookSink = Arc::new(move |ev: HookEvent| weak.upgrade().and_then(|s| s.on_hook(ev)));
            let hooks = match hooks {
                Hooks::Off => None,
                Hooks::Start => HookServer::start(sink).map_err(|e| log::warn!("hook server unavailable: {e}")).ok(),
                #[cfg(unix)]
                Hooks::Adopt(listener, tokens) => {
                    HookServer::serve(listener, sink, tokens).map_err(|e| log::warn!("hook server unavailable: {e}")).ok()
                },
            };
            Server {
                state: Mutex::new(State {
                    sessions: HashMap::new(),
                    clients: 0,
                    uis: Vec::new(),
                    observers: Vec::new(),
                    connecting: 0,
                    idle_since: Some(Instant::now()),
                    exiting: false,
                    handing_off: false,
                }),
                hooks,
                sock: paths::socket_path(dir),
                pid_file: paths::pid_path(dir),
                run_dir,
                notifications: opts.notifications,
                max_queued: opts.max_queued,
                max_queued_session: opts.max_queued_session,
                lock: Mutex::new(Some(lock)),
                stopped: AtomicBool::new(false),
                end: Mutex::new(None),
                #[cfg(unix)]
                successor: opts.successor.clone(),
            }
        })
    }

    /// Mark the run dir as this process's, so no other Tern sweeps it while
    /// it runs (an adopted one is named for the daemon that made it).
    fn claim_run_dir(&self) {
        if let Err(e) = tern_bridge::claim_run_dir(&self.run_dir) {
            log::warn!("can't claim {}: {e}", self.run_dir.display());
        }
    }

    /// Serve on `listener` until idle for `idle_exit` or handed off.
    fn run(self: &Arc<Self>, listener: UnixListener, idle_exit: Duration) -> io::Result<()> {
        let (end_tx, end_rx) = mpsc::channel();
        *self.end.lock().unwrap() = Some(end_tx.clone());
        {
            let server = self.clone();
            std::thread::Builder::new().name("idle".into()).spawn(move || {
                // Wakes when an idle daemon may be due to exit, and once a
                // second otherwise to see whether it has gone idle.
                let mut wait = Duration::from_millis(100).min(idle_exit);
                loop {
                    std::thread::sleep(wait);
                    // Decided under the lock, so a connection either counted
                    // before it or is turned away after it.
                    let (idle, after) = {
                        let mut st = server.state.lock().unwrap();
                        // Ended programs are kept a while for their tabs.
                        let after = if st.sessions.is_empty() { idle_exit } else { idle_exit.max(ENDED_GRACE) };
                        let idle = st.idle_since.map(|t| t.elapsed());
                        if idle.is_some_and(|for_| for_ >= after) {
                            st.exiting = true;
                        }
                        (idle, after)
                    };
                    match idle {
                        Some(for_) if for_ >= after => {
                            server.stopped.store(true, Ordering::Release);
                            // Wake the accept loop.
                            let _ = UnixStream::connect(&server.sock);
                            let _ = end_tx.send(End::Idle);
                            return;
                        },
                        Some(for_) => wait = (after - for_).min(Duration::from_secs(1)),
                        None => wait = Duration::from_secs(1).min(idle_exit),
                    }
                }
            })?;
        }
        self.accept_on(listener)?;
        match end_rx.recv() {
            Ok(End::Idle) | Err(_) => {
                let _ = std::fs::remove_file(&self.sock);
                let _ = std::fs::remove_file(&self.pid_file);
                let _ = std::fs::remove_dir_all(&self.run_dir);
                // Observers learn at once that this daemon is gone.
                let observers = std::mem::take(&mut self.state.lock().unwrap().observers);
                for c in observers.iter().filter_map(Weak::upgrade) {
                    c.close();
                }
                log::info!("daemon idle, exiting");
            },
            #[cfg(unix)]
            Ok(End::HandedOff) => log::info!("daemon handed off, exiting"),
        }
        Ok(())
    }

    /// Accept connections on `listener` until the daemon stops.
    fn accept_on(self: &Arc<Self>, listener: UnixListener) -> io::Result<()> {
        let server = self.clone();
        std::thread::Builder::new().name("accept".into()).spawn(move || {
            for stream in listener.incoming() {
                if server.stopped.load(Ordering::Acquire) {
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
        })?;
        Ok(())
    }

    fn update_idle(state: &mut State) {
        let idle =
            state.clients == 0 && state.connecting == 0 && !state.handing_off && state.sessions.values().all(|s| s.host.exited().is_some());
        match (idle, state.idle_since) {
            (true, None) => state.idle_since = Some(Instant::now()),
            (false, Some(_)) => state.idle_since = None,
            _ => {},
        }
    }

    fn handle(self: &Arc<Self>, stream: UnixStream) -> io::Result<()> {
        {
            let mut st = self.state.lock().unwrap();
            if st.exiting {
                // Closed without a Hello: the client starts a new daemon (or,
                // during a handoff, finds the new one).
                return Err(io::Error::other("exiting"));
            }
            st.connecting += 1;
            Self::update_idle(&mut st);
        }
        let conn = match stream.try_clone().and_then(|s| Conn::start(s, self.max_queued, self.max_queued_session)) {
            Ok(c) => c,
            Err(e) => {
                let mut st = self.state.lock().unwrap();
                st.connecting -= 1;
                Self::update_idle(&mut st);
                return Err(e);
            },
        };
        conn.send(ServerMsg::Hello {
            major: MAJOR,
            minor: MINOR,
            app_version: env!("CARGO_PKG_VERSION").into(),
            pid: std::process::id(),
            build: paths::build_id().into(),
        });
        // A client that never says Hello mustn't keep the daemon forever.
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut reader = FrameReader::new(stream, proto::MAX_CLIENT_FRAME);
        let hello = reader.read::<ClientMsg>();
        let _ = reader.get_ref().set_read_timeout(None);
        let role = {
            // Past its Hello: a UI holds the daemon from now on, the others don't.
            let mut st = self.state.lock().unwrap();
            st.connecting -= 1;
            let role = match hello {
                Ok(Some(ClientMsg::Hello { major, role, .. })) if major == MAJOR => Some(role),
                _ => None,
            };
            match role {
                Some(Role::Ui) => {
                    st.clients += 1;
                    st.uis.push(Arc::downgrade(&conn));
                },
                Some(_) => st.observers.push(Arc::downgrade(&conn)),
                None => {},
            }
            Self::update_idle(&mut st);
            role
        };
        let Some(role) = role else {
            conn.close();
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("bad hello: {:?}", hello.ok().flatten())));
        };
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
        let sub: Arc<dyn Subscriber> = conn.clone();
        for (&id, s) in st.sessions.iter_mut() {
            if s.owner.as_ref().is_some_and(|o| o.as_ptr() == Arc::as_ptr(&conn)) {
                s.owner = None;
                s.host.detach(&sub);
            }
            if s.viewers.iter().any(|v| v.as_ptr() == Arc::as_ptr(&conn)) {
                s.viewers.retain(|v| v.as_ptr() != Arc::as_ptr(&conn));
                s.host.unview(&sub);
            }
            s.let_go(id, Arc::as_ptr(&conn));
        }
        Self::update_idle(&mut st);
        result
    }

    fn on_msg(self: &Arc<Self>, conn: &Arc<Conn>, msg: ClientMsg) {
        match msg {
            ClientMsg::Hello { .. } => {},
            ClientMsg::ContextGuard { id, limit, auto_switch } => {
                if let Some(s) = self.state.lock().unwrap().sessions.get_mut(&id) {
                    s.policy = Policy { limit: limit.min(100), auto_switch };
                }
            },
            ClientMsg::Mute { id, muted } => {
                if let Some(s) = self.state.lock().unwrap().sessions.get_mut(&id) {
                    s.muted = muted;
                }
            },
            ClientMsg::AskHandoff { id } => {
                if let Some(s) = self.state.lock().unwrap().sessions.get_mut(&id) {
                    // Waiting on the user (a permission) counts as busy: typing
                    // would answer it.
                    let busy = matches!(s.host.status(), ActivityStatus::Working | ActivityStatus::Blocked);
                    type_after_turn(&s.host, s.guard.request(busy));
                }
            },
            ClientMsg::ResumeHandoff { id, path } => {
                if let Some(s) = self.state.lock().unwrap().sessions.get_mut(&id) {
                    type_after_turn(&s.host, s.guard.resume_from(path));
                }
            },
            ClientMsg::Watch => self.watch(conn),
            ClientMsg::Reveal { req, id } => {
                let ui = self.state.lock().unwrap().uis.iter().rev().filter_map(Weak::upgrade).find(|c| !c.dead.load(Ordering::Acquire));
                if let Some(ui) = &ui {
                    ui.send(ServerMsg::Reveal { id });
                }
                conn.send(ServerMsg::Revealed { req, delivered: ui.is_some() });
            },
            ClientMsg::Spawn { req, cfg, hook, seed } => self.spawn(conn, req, cfg, hook, seed),
            ClientMsg::Attach { req, id, size } => self.attach(conn, req, id, size),
            ClientMsg::Detach { id } => {
                let mut st = self.state.lock().unwrap();
                if let Some(s) = st.sessions.get_mut(&id)
                    && s.owner.as_ref().is_some_and(|o| o.as_ptr() == Arc::as_ptr(conn))
                {
                    s.owner = None;
                    s.host.detach(&(conn.clone() as Arc<dyn Subscriber>));
                    s.let_go(id, Arc::as_ptr(conn));
                }
            },
            ClientMsg::Input { id, bytes } => {
                let mut st = self.state.lock().unwrap();
                if let Some(s) = st.sessions.get_mut(&id) {
                    let mut go = false;
                    s.update(id, |s| go = s.typed(conn, &bytes));
                    if go {
                        s.host.input(bytes);
                    }
                }
            },
            ClientMsg::Resize { id, size } => {
                let mut st = self.state.lock().unwrap();
                if let Some(s) = st.sessions.get_mut(&id) {
                    s.update(id, |s| s.want(conn, size));
                }
            },
            ClientMsg::Drive { id } => {
                let mut st = self.state.lock().unwrap();
                if let Some(s) = st.sessions.get_mut(&id) {
                    s.update(id, |s| s.drive(conn));
                }
            },
            ClientMsg::View { req, id } => self.view(conn, req, id),
            ClientMsg::Unview { id } => {
                let mut st = self.state.lock().unwrap();
                if let Some(s) = st.sessions.get_mut(&id) {
                    s.viewers.retain(|v| v.as_ptr() != Arc::as_ptr(conn));
                    s.host.unview(&(conn.clone() as Arc<dyn Subscriber>));
                    s.let_go(id, Arc::as_ptr(conn));
                }
            },
            ClientMsg::Close { id } => self.close(id),
            ClientMsg::List { req } => conn.send(ServerMsg::Sessions { req, list: self.list() }),
            #[cfg(unix)]
            ClientMsg::HandOff { req, exe } => {
                let (server, conn) = (self.clone(), conn.clone());
                // Off the connection's reader: it takes a while.
                let _ = std::thread::Builder::new().name("handoff".into()).spawn(move || {
                    if let Err(e) = server.hand_off(&exe) {
                        log::warn!("handoff failed: {e}");
                        conn.send(ServerMsg::HandOffFailed { req, message: e.to_string() });
                    }
                });
            },
            #[cfg(windows)]
            ClientMsg::HandOff { req, .. } => {
                conn.send(ServerMsg::HandOffFailed { req, message: "handoff isn't supported on Windows".into() })
            },
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
                    Some(l) if !ticks.is_multiple_of(WATCH_SCANS_EVERY) => server.list_keeping(l),
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

    fn spawn(self: &Arc<Self>, conn: &Arc<Conn>, req: u64, mut cfg: SpawnConfig, hook: bool, seed: Option<Vec<u8>>) {
        // A handoff exports the sessions there are: wait for it. If it
        // succeeds this connection is closed and the UI asks the new daemon.
        while self.state.lock().unwrap().handing_off {
            if conn.dead.load(Ordering::Acquire) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if conn.dead.load(Ordering::Acquire) {
            return;
        }
        let seed = seed.and_then(|s| proto::unpack_snapshot(&s).map_err(|e| log::warn!("bad seed: {e}")).ok());
        let id = random_id();
        let token = match (hook, &self.hooks) {
            (true, Some(hooks)) => {
                let token = hooks.register();
                match tern_bridge::write_claude_settings(
                    &self.run_dir,
                    &token,
                    &hooks.hook_url(&token),
                    std::env::current_exe().ok().as_deref(),
                ) {
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
        let host = match Host::spawn(id, &cfg, seed, observer) {
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
                Session {
                    host: host.clone(),
                    token: token.clone(),
                    owner: None,
                    viewers: Vec::new(),
                    sizes: vec![(Arc::downgrade(conn), cfg.size)],
                    driver: None,
                    held: false,
                    hooks: VecDeque::new(),
                    claude_session: None,
                    guard: Guard::default(),
                    policy: Policy::default(),
                    muted: false,
                },
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
        let (host, token, previous, hooks, size) = {
            let mut st = self.state.lock().unwrap();
            let Some(s) = st.sessions.get_mut(&id) else { return conn.send(ServerMsg::NotFound { req, id }) };
            let previous = s.owner.replace(Arc::downgrade(conn)).and_then(|w| w.upgrade()).filter(|p| !Arc::ptr_eq(p, conn));
            if let Some(p) = &previous {
                s.forget(Arc::as_ptr(p));
            }
            if let Some(size) = size {
                s.want(conn, size);
            }
            // While a viewer has it, the program repaints at the viewer's size.
            let size = size.and(s.target());
            (s.host.clone(), s.token.clone(), previous, std::mem::take(&mut s.hooks), size)
        };
        if let Some(p) = previous {
            p.send(ServerMsg::Detached { id });
        }
        // Closed on exec: programs spawned meanwhile mustn't inherit it.
        // SAFETY: dup of an fd the host keeps open.
        #[cfg(unix)]
        let fd = host
            .master_fd()
            .map(|fd| unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) })
            .filter(|&fd| fd >= 0)
            .map(|fd| unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) });
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
        self.tell_control(conn, id);
        // After `Attached`; changes from here on reach it too.
        host.send_images(&**conn);
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

    /// Let `conn` watch session `id` beside its owner.
    fn view(self: &Arc<Self>, conn: &Arc<Conn>, req: u64, id: u64) {
        let host = {
            let mut st = self.state.lock().unwrap();
            let Some(s) = st.sessions.get_mut(&id) else { return conn.send(ServerMsg::NotFound { req, id }) };
            if !s.viewers.iter().any(|v| v.as_ptr() == Arc::as_ptr(conn)) {
                s.viewers.push(Arc::downgrade(conn));
            }
            s.host.clone()
        };
        let pid = host.pid();
        let mut exited = None;
        host.view_with(conn.clone(), |snapshot| {
            conn.caught_up(id);
            conn.push(Out::Attached { req, id, token: None, pid, fd: None, snapshot: Box::new(snapshot) });
            exited = host.exited();
            if let Some(code) = exited {
                conn.send(ServerMsg::Marker { id, marker: Marker::Exited(code) });
            }
        });
        self.tell_control(conn, id);
        host.send_images(&**conn);
        // The snapshot misses the scroll region and charsets: a full-screen
        // program repaints at its own size (the owner sees the same bytes).
        if exited.is_none() && !host.shell_in_foreground() {
            host.jiggle(host.size());
        }
    }

    /// Tell `conn`, after `Attached`, who has session `id`.
    fn tell_control(&self, conn: &Arc<Conn>, id: u64) {
        let by = self.state.lock().unwrap().sessions.get(&id).map(|s| s.controller(Arc::as_ptr(conn)));
        if let Some(by) = by {
            conn.send(ServerMsg::Control { id, by });
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

    /// Hook `ev` came; the answer for Claude Code, if any.
    fn on_hook(&self, ev: HookEvent) -> Option<serde_json::Value> {
        let body = serde_json::to_vec(&ev.raw).unwrap_or_default();
        let mut st = self.state.lock().unwrap();
        let s = st.sessions.values_mut().find(|s| s.token.as_deref() == Some(ev.token.as_str()))?;
        if let Some(id) = &ev.session_id {
            s.claude_session = Some(id.clone());
        }
        let (reply, action) = s.guard.on_event(&ev, s.policy);
        type_after_turn(&s.host, action);
        match s.owner.as_ref().and_then(Weak::upgrade) {
            Some(conn) => conn.send(ServerMsg::Hook { token: ev.token, body }),
            // A tool call that changed no file tells a UI nothing later.
            None if ev.event == "PostToolUse" && ev.paths.is_empty() => {},
            None => {
                // Only the latest status line matters.
                if ev.event == tern_bridge::statusline::EVENT {
                    s.hooks.retain(|b| !tern_bridge::statusline::is_event(b));
                }
                if s.hooks.len() >= MAX_QUEUED_HOOKS {
                    s.hooks.pop_front();
                }
                s.hooks.push_back(body);
            },
        }
        reply
    }

    /// Whether session `id` notifies the user: no UI is attached (it would)
    /// and its tab didn't mute it.
    fn unattended(&self, id: u64) -> bool {
        let st = self.state.lock().unwrap();
        self.notifications && st.clients == 0 && !st.sessions.get(&id).is_some_and(|s| s.muted)
    }

    /// The agent session `id` runs, and its task (from its title; see
    /// [`tern_term::status::task_name`]).
    fn agent_name(&self, id: u64) -> (String, Option<String>) {
        let host = self.host(id);
        let programs = host.as_ref().map(|h| h.foreground_programs()).unwrap_or_default();
        let agent = if programs.iter().any(|p| p.contains("codex")) {
            "Codex"
        } else if programs.iter().any(|p| p.contains("opencode")) {
            "OpenCode"
        } else {
            "Claude"
        };
        let task = host.and_then(|h| h.title()).and_then(|t| tern_term::status::task_name(&t, agent));
        (agent.to_owned(), task)
    }
}

/// A handoff under way in the old daemon: until it is committed, dropping it
/// rolls back (ends the new daemon, drives the PTYs again, serves again).
#[cfg(unix)]
struct Handoff<'a> {
    server: &'a Arc<Server>,
    kill: Option<Box<dyn FnOnce() + Send>>,
    paused: Vec<Box<dyn Paused>>,
    /// The new daemon may have taken the socket path.
    rebind: bool,
}

#[cfg(unix)]
impl Drop for Handoff<'_> {
    fn drop(&mut self) {
        if let Some(kill) = self.kill.take() {
            kill();
        }
        for p in self.paused.drain(..) {
            p.resume();
        }
        if self.rebind {
            match bind_over(&self.server.sock) {
                Ok(listener) => {
                    if let Err(e) = self.server.accept_on(listener) {
                        log::error!("can't accept again: {e}");
                    }
                },
                Err(e) => log::error!("can't listen again on {}: {e}", self.server.sock.display()),
            }
        }
        let mut st = self.server.state.lock().unwrap();
        st.exiting = false;
        st.handing_off = false;
        Server::update_idle(&mut st);
    }
}

#[cfg(unix)]
impl Server {
    /// Hand every session to a new daemon started from `exe`. On success
    /// the new daemon serves them, every connection here is closed, and
    /// `serve` returns (without the files the new daemon now owns). On
    /// failure nothing changed.
    fn hand_off(self: &Arc<Self>, exe: &Path) -> io::Result<()> {
        let sessions: Vec<_> = {
            let mut st = self.state.lock().unwrap();
            if st.exiting || st.handing_off {
                return Err(io::Error::other("the daemon is exiting"));
            }
            st.exiting = true;
            st.handing_off = true;
            Self::update_idle(&mut st);
            let mut v: Vec<_> = st
                .sessions
                .iter()
                .map(|(&id, s)| {
                    (id, s.host.clone(), s.token.clone(), s.claude_session.clone(), s.hooks.iter().cloned().collect::<Vec<_>>())
                })
                .collect();
            v.sort_by_key(|s| s.0);
            v
        };
        log::info!("handing {} sessions to a new daemon from {}", sessions.len(), exe.display());
        let mut handoff = Handoff { server: self, kill: None, paused: Vec::new(), rebind: false };
        let (stream, kill) = match &self.successor {
            Some(start) => start(exe)?,
            None => spawn_successor(exe)?,
        };
        handoff.kill = Some(kill);
        stream.set_read_timeout(Some(HANDOFF_TIMEOUT))?;
        stream.set_write_timeout(Some(HANDOFF_TIMEOUT))?;
        let lock_fd = self.lock.lock().unwrap().as_ref().map(AsRawFd::as_raw_fd).ok_or_else(|| io::Error::other("no lock"))?;
        let mut fds = vec![lock_fd];
        if let Some(h) = &self.hooks {
            fds.push(h.listener().as_raw_fd());
        }
        let begin = HandoffMsg::Begin {
            version: HANDOFF_VERSION,
            major: MAJOR,
            app_version: env!("CARGO_PKG_VERSION").into(),
            run_dir: self.run_dir.clone(),
            hook_port: self.hooks.as_ref().map(HookServer::port),
            sessions: sessions.len() as u32,
        };
        proto::write_frame_with_fds(&stream, &begin, &fds)?;
        for (id, host, token, claude_session, hooks) in sessions {
            let paused = host.pause()?;
            let fd = paused.as_ref().and_then(|_| host.master_fd());
            let session = HandedSession {
                id,
                pid: host.pid(),
                fd: fd.is_some(),
                exited: host.exited(),
                size: host.size(),
                scrollback: host.scrollback(),
                title: host.title(),
                status: host.status(),
                token,
                claude_session,
                hooks: hooks.into_iter().map(serde_bytes::ByteBuf::from).collect(),
                writes: paused.as_ref().map(|p| p.writes()).unwrap_or_default(),
                scheme_reports: paused.as_ref().is_some_and(|p| p.scheme_reports()),
                snapshot: proto::pack_snapshot(&host.capture())?,
                images: host.handed_images(),
            };
            handoff.paused.extend(paused);
            let msg = HandoffMsg::Session(Box::new(session));
            match fd {
                Some(fd) => proto::write_frame_with_fds(&stream, &msg, &[fd])?,
                None => proto::write_frame(&mut &stream, &msg)?,
            }
        }
        let mut reader = FrameReader::new(stream.try_clone()?, proto::MAX_SERVER_FRAME);
        expect(&mut reader, |m| matches!(m, HandoffMsg::Restored))?;
        // From here the new daemon may take the socket path.
        handoff.rebind = true;
        proto::write_frame(&mut &stream, &HandoffMsg::Commit)?;
        expect(&mut reader, |m| matches!(m, HandoffMsg::Committed))?;

        // Committed: the programs are the new daemon's now.
        handoff.kill = None;
        handoff.rebind = false;
        for p in handoff.paused.drain(..) {
            p.forget();
        }
        std::mem::forget(handoff);
        log::info!("handoff committed");
        self.stopped.store(true, Ordering::Release);
        let conns: Vec<_> = {
            let mut st = self.state.lock().unwrap();
            // Its hosts aren't driven here any more.
            st.sessions.clear();
            st.uis.iter().chain(st.observers.iter()).filter_map(Weak::upgrade).collect()
        };
        // The UIs reattach to the new daemon by session id.
        for c in conns {
            c.close();
        }
        // The new daemon holds the lock too (the same open file).
        self.lock.lock().unwrap().take();
        if let Some(end) = self.end.lock().unwrap().take() {
            let _ = end.send(End::HandedOff);
        }
        Ok(())
    }
}

/// Read the next handoff message: `Ok` when `ok` accepts it.
#[cfg(unix)]
fn expect(reader: &mut FrameReader, ok: impl Fn(&HandoffMsg) -> bool) -> io::Result<()> {
    match reader.read::<HandoffMsg>()? {
        Some(m) if ok(&m) => Ok(()),
        Some(HandoffMsg::Failed { message }) => Err(io::Error::other(format!("the new daemon failed: {message}"))),
        Some(m) => Err(io::Error::new(io::ErrorKind::InvalidData, format!("unexpected {m:?}"))),
        None => Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the new daemon went away")),
    }
}

/// Start `<exe> --server --adopt` with one end of a socketpair as fd 3.
#[cfg(unix)]
fn spawn_successor(exe: &Path) -> io::Result<(UnixStream, Box<dyn FnOnce() + Send>)> {
    use std::os::unix::process::CommandExt;
    let (ours, theirs) = UnixStream::pair()?;
    let fd = theirs.as_raw_fd();
    let log = crate::client::log_file().and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok());
    let (out, err) = match log {
        Some(f) => (std::process::Stdio::from(f.try_clone()?), std::process::Stdio::from(f)),
        None => (std::process::Stdio::null(), std::process::Stdio::null()),
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["--server", "--adopt"]).stdin(std::process::Stdio::null()).stdout(out).stderr(err);
    if std::env::var_os("RUST_LOG").is_none() {
        cmd.env("RUST_LOG", "info");
    }
    // SAFETY: setsid, dup2 and fcntl are async-signal-safe.
    unsafe {
        cmd.pre_exec(move || {
            libc::setsid();
            // dup2 leaves the copy open on exec; an fd already at 3 isn't.
            let r = if fd == ADOPT_FD { libc::fcntl(fd, libc::F_SETFD, 0) } else { libc::dup2(fd, ADOPT_FD) };
            if r < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
        });
    }
    let mut child = cmd.spawn()?;
    drop(theirs);
    Ok((
        ours,
        Box::new(move || {
            let _ = child.kill();
            let _ = child.wait();
        }),
    ))
}

/// `tern --server --adopt`: take over the sessions of the daemon on the
/// other end of `stream`, then serve on `dir` as [`serve`] does.
#[cfg(unix)]
pub fn adopt(dir: &Path, opts: ServeOpts, stream: UnixStream) -> io::Result<()> {
    let result = adopt_sessions(dir, &opts, &stream);
    match result {
        Ok((server, listener)) => server.run(listener, opts.idle_exit),
        Err(e) => {
            let _ = proto::write_frame(&mut &stream, &HandoffMsg::Failed { message: e.to_string() });
            Err(e)
        },
    }
}

#[cfg(unix)]
fn adopt_sessions(dir: &Path, opts: &ServeOpts, stream: &UnixStream) -> io::Result<(Arc<Server>, UnixListener)> {
    paths::ensure_private_dir(dir)?;
    paths::build_id();
    stream.set_read_timeout(Some(HANDOFF_TIMEOUT))?;
    let mut reader = FrameReader::new(stream.try_clone()?, proto::MAX_SERVER_FRAME);
    let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_owned());
    let Some(HandoffMsg::Begin { version, major, app_version, run_dir, hook_port, sessions }) = reader.read()? else {
        return Err(bad("expected Begin"));
    };
    if version > HANDOFF_VERSION || major != MAJOR {
        return Err(io::Error::other(format!("can't adopt from Tern {app_version} (handoff {version}, protocol {major})")));
    }
    log::info!("adopting {sessions} sessions from Tern {app_version}");
    let lock = File::from(reader.take_fd().ok_or_else(|| bad("no lock fd"))?);
    let hook_listener = match hook_port {
        Some(_) => Some(std::net::TcpListener::from(reader.take_fd().ok_or_else(|| bad("no hook listener fd"))?)),
        None => None,
    };
    let mut handed = Vec::new();
    for _ in 0..sessions {
        let Some(HandoffMsg::Session(s)) = reader.read()? else { return Err(bad("expected Session")) };
        let fd: Option<OwnedFd> = if s.fd { Some(reader.take_fd().ok_or_else(|| bad("no PTY fd"))?) } else { None };
        handed.push((s, fd));
    }
    let tokens = handed.iter().filter_map(|(s, _)| s.token.clone()).collect();
    let hooks = match hook_listener {
        Some(l) if opts.hooks => Hooks::Adopt(l, tokens),
        _ => Hooks::Off,
    };
    let server = Server::new(opts, dir, run_dir, lock, hooks);
    let mut drivers = Vec::new();
    {
        let mut st = server.state.lock().unwrap();
        for (s, fd) in handed {
            let snapshot = proto::unpack_snapshot(&s.snapshot)?;
            let observer: Arc<dyn Observer> = server.clone();
            let (host, driver) =
                Host::adopt(s.id, s.pid, fd, snapshot, s.scrollback, s.exited, s.writes, s.scheme_reports, s.images, observer)?;
            drivers.extend(driver);
            let hooks = s.hooks.into_iter().map(serde_bytes::ByteBuf::into_vec).collect();
            st.sessions.insert(
                s.id,
                Session {
                    host,
                    token: s.token,
                    owner: None,
                    viewers: Vec::new(),
                    sizes: Vec::new(),
                    driver: None,
                    held: false,
                    hooks,
                    claude_session: s.claude_session,
                    guard: Guard::default(),
                    policy: Policy::default(),
                    muted: false,
                },
            );
        }
        Server::update_idle(&mut st);
    }
    proto::write_frame(&mut &stream.try_clone()?, &HandoffMsg::Restored)?;
    match reader.read::<HandoffMsg>() {
        Ok(Some(HandoffMsg::Commit)) => {},
        // The old daemon rolled back: the parked drivers go unstarted.
        other => return Err(io::Error::other(format!("no commit: {other:?}"))),
    }
    let listener = bind_over(&server.sock)?;
    write_pid(&server.pid_file);
    server.claim_run_dir();
    for d in drivers {
        d.resume();
    }
    proto::write_frame(&mut &stream.try_clone()?, &HandoffMsg::Committed)?;
    log::info!("daemon {} adopted the sessions; listening on {}", std::process::id(), server.sock.display());
    Ok((server, listener))
}

impl Observer for Server {
    fn status(&self, id: u64, _old: ActivityStatus, new: ActivityStatus) {
        if !self.unattended(id) {
            return;
        }
        let (agent, task) = self.agent_name(id);
        if let Some((title, body)) = tern_term::status::status_notification(new, &agent, task.as_deref()) {
            crate::notify::post(&title, &body);
        }
    }

    fn notify(&self, id: u64, title: Option<String>, body: String) {
        if self.unattended(id) {
            let title = title.filter(|t| !t.is_empty()).unwrap_or_else(|| {
                let (agent, task) = self.agent_name(id);
                task.unwrap_or(agent)
            });
            crate::notify::post(&title, &body);
        }
    }

    fn exited(&self, id: u64, code: Option<i32>) {
        log::info!("session {id}: exited ({code:?})");
        Self::update_idle(&mut self.state.lock().unwrap());
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

    fn images(&self, id: u64, msg: &ServerMsg) {
        self.send_output(id, msg.clone());
    }
}

/// How much a connection's writer gathers before it writes.
const WRITE_BATCH: usize = 256 << 10;

fn flush_batch(stream: &mut UnixStream, batch: &mut Vec<u8>) -> io::Result<()> {
    if !batch.is_empty() {
        stream.write_all(batch)?;
        batch.clear();
    }
    Ok(())
}

fn writer(
    mut stream: UnixStream,
    rx: Receiver<Out>,
    queued: Arc<AtomicUsize>,
    per_session: Arc<Mutex<HashMap<u64, usize>>>,
    conn: Weak<Conn>,
) {
    let result = (|| -> io::Result<()> {
        // Frames queued together go out in one write; the batch is sent
        // as soon as the queue is empty, so nothing waits.
        let mut batch: Vec<u8> = Vec::with_capacity(WRITE_BATCH);
        let mut next = rx.recv().ok();
        while let Some(out) = next.take() {
            match out {
                Out::Frame(frame, id) => {
                    if batch.is_empty() && frame.len() >= WRITE_BATCH {
                        stream.write_all(&frame)?;
                    } else {
                        batch.extend_from_slice(&frame);
                    }
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
                    flush_batch(&mut stream, &mut batch)?;
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
            next = match rx.try_recv() {
                Ok(out) if batch.len() < WRITE_BATCH => Some(out),
                Ok(out) => {
                    flush_batch(&mut stream, &mut batch)?;
                    Some(out)
                },
                Err(mpsc::TryRecvError::Empty) => {
                    flush_batch(&mut stream, &mut batch)?;
                    rx.recv().ok()
                },
                Err(mpsc::TryRecvError::Disconnected) => None,
            };
        }
        flush_batch(&mut stream, &mut batch)
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
    use super::{shell_quote, untyped};

    #[test]
    fn focus_reports_are_not_typing() {
        assert!(untyped(b"\x1b[I"));
        assert!(untyped(b"\x1b[O\x1b[I"));
        assert!(!untyped(b""));
        assert!(!untyped(b"a"));
        assert!(!untyped(b"\x1b[Ia"));
        assert!(!untyped(b"\x1b[A"));
    }

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
