//! The UI's side: one connection to the daemon for every tab.
//!
//! A reader thread applies what the daemon sends to each tab's [`Mirror`]
//! and forwards events to the tab's sink. [`RemoteSession`] has the surface
//! the terminal view used on an in-process session.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tern_term::mirror::{Mirror, MirrorTerm};
use tern_term::vt::sync::FairMutex;
use tern_term::{EventSink, SpawnConfig, TermEvent, TermSize};

use crate::paths;
use crate::proto::{self, ClientMsg, FrameReader, MAJOR, MINOR, Role, ServerMsg, SessionInfo};

/// Gets each hook request: the tab's token and the JSON body.
pub type HookSink = Arc<dyn Fn(String, Vec<u8>) + Send + Sync>;
/// Gets each request to show a session (from the menu bar agent).
pub type RevealSink = Arc<dyn Fn(u64) + Send + Sync>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const LIST_TIMEOUT: Duration = Duration::from_secs(2);
/// Input is sent in pieces of at most this (a big paste stays under the
/// daemon's frame limit).
const INPUT_CHUNK: usize = 1 << 20;
/// Input typed before the daemon answered, kept to send once it has.
const MAX_EARLY_INPUT: usize = 16 << 20;

pub enum Open {
    /// Start a program. With `hook`, `{tern-settings}` in its args becomes
    /// the path of its Claude settings file.
    Spawn { cfg: SpawnConfig, hook: bool },
    /// Attach to a running (or exited) session.
    Attach { id: u64 },
}

/// One tab's state on the client side.
struct Slot {
    mirror: Mutex<Mirror>,
    term: Arc<FairMutex<MirrorTerm>>,
    wake: Arc<AtomicBool>,
    sink: EventSink,
    /// Set once the daemon answered; while `None`, input waits in `early`
    /// (its lock is held to set the id).
    id: Mutex<Option<u64>>,
    early: Mutex<Vec<u8>>,
    /// The PTY master, for foreground-process queries.
    tty: Mutex<Option<File>>,
    /// The size the view wants; sent once the session is known.
    want: Mutex<TermSize>,
    /// The size the open request carried.
    sent: TermSize,
    /// The tab let go of it: `close` says whether to end the session or
    /// only detach, once its id is known.
    gone: AtomicBool,
    close: AtomicBool,
}

struct Inner {
    writer: Mutex<UnixStream>,
    alive: AtomicBool,
    next_req: AtomicU64,
    pending: Mutex<HashMap<u64, Arc<Slot>>>,
    sessions: Mutex<HashMap<u64, Arc<Slot>>>,
    lists: Mutex<HashMap<u64, mpsc::Sender<Vec<SessionInfo>>>>,
    hooks: HookSink,
    reveal: RevealSink,
    server_pid: u32,
    server_version: String,
}

impl Inner {
    fn send(&self, msg: &ClientMsg) -> io::Result<()> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(io::ErrorKind::NotConnected.into());
        }
        let frame = proto::encode(msg)?;
        self.writer.lock().unwrap().write_all(&frame)
    }

    fn req(&self) -> u64 {
        self.next_req.fetch_add(1, Ordering::Relaxed)
    }

    fn input(&self, id: u64, bytes: &[u8]) {
        for chunk in bytes.chunks(INPUT_CHUNK) {
            let _ = self.send(&ClientMsg::Input { id, bytes: chunk.to_vec() });
        }
    }
}

/// Clones share the connection; it closes when the last one is dropped
/// (open sessions then get `ServerLost`).
#[derive(Clone)]
pub struct MuxClient {
    inner: Arc<Inner>,
    _conn: Arc<Closer>,
}

struct Closer(UnixStream);

impl Drop for Closer {
    fn drop(&mut self) {
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

impl MuxClient {
    /// Connect to the daemon in `dir`, starting it first when `spawn_daemon`
    /// and none runs.
    pub fn connect(dir: &Path, spawn_daemon: bool, hooks: HookSink, reveal: RevealSink) -> io::Result<Self> {
        let sock = paths::socket_path(dir);
        let stream = match UnixStream::connect(&sock) {
            Ok(s) => s,
            Err(e) if !spawn_daemon => return Err(e),
            Err(_) => {
                spawn()?;
                let deadline = Instant::now() + CONNECT_TIMEOUT;
                let mut wait = Duration::from_millis(10);
                loop {
                    match UnixStream::connect(&sock) {
                        Ok(s) => break s,
                        Err(e) if Instant::now() >= deadline => return Err(e),
                        Err(_) => {
                            std::thread::sleep(wait);
                            wait = (wait * 2).min(Duration::from_millis(200));
                        },
                    }
                }
            },
        };
        if !paths::peer_uid_ok(&stream) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "daemon runs as another user"));
        }
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        // A daemon that accepts but never answers mustn't hang the UI.
        stream.set_read_timeout(Some(CONNECT_TIMEOUT))?;
        let mut writer = stream.try_clone()?;
        proto::write_frame(
            &mut writer,
            &ClientMsg::Hello { major: MAJOR, minor: MINOR, app_version: env!("CARGO_PKG_VERSION").into(), role: Role::Ui },
        )?;
        let mut reader = FrameReader::new(stream, proto::MAX_SERVER_FRAME);
        let (server_pid, server_version) = match reader.read::<ServerMsg>()? {
            Some(ServerMsg::Hello { major, pid, app_version, .. }) if major == MAJOR => {
                log::info!("connected to daemon {pid} (Tern {app_version})");
                (pid, app_version)
            },
            other => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("bad hello: {other:?}"))),
        };
        // The same socket: the reader blocks from now on.
        writer.set_read_timeout(None)?;
        let inner = Arc::new(Inner {
            writer: Mutex::new(writer),
            alive: AtomicBool::new(true),
            next_req: AtomicU64::new(1),
            pending: Mutex::default(),
            sessions: Mutex::default(),
            lists: Mutex::default(),
            hooks,
            reveal,
            server_pid,
            server_version,
        });
        let closer = Closer(inner.writer.lock().unwrap().try_clone()?);
        let i = inner.clone();
        std::thread::Builder::new().name("mux reader".into()).spawn(move || read_loop(&i, reader))?;
        Ok(Self { inner, _conn: Arc::new(closer) })
    }

    pub fn alive(&self) -> bool {
        self.inner.alive.load(Ordering::Acquire)
    }

    pub fn server_pid(&self) -> u32 {
        self.inner.server_pid
    }

    /// The Tern version the daemon runs (an older one keeps running after
    /// an update until it has no terminals left).
    pub fn server_version(&self) -> &str {
        &self.inner.server_version
    }

    /// Open a tab's session. Returns at once: the sink gets `Ready` (or
    /// `NotFound` / `SpawnError` / `ServerLost`) when the daemon answers.
    pub fn open(&self, open: Open, size: TermSize, scrollback: usize, sink: EventSink) -> RemoteSession {
        let mirror = Mirror::new(size, scrollback, sink.clone());
        let req = self.inner.req();
        let slot = Arc::new(Slot {
            sent: size,
            gone: AtomicBool::new(false),
            close: AtomicBool::new(false),
            term: mirror.term().clone(),
            wake: mirror.wake_flag().clone(),
            mirror: Mutex::new(mirror),
            sink,
            id: Mutex::new(None),
            early: Mutex::new(Vec::new()),
            tty: Mutex::new(None),
            want: Mutex::new(size),
        });
        self.inner.pending.lock().unwrap().insert(req, slot.clone());
        let msg = match open {
            Open::Spawn { mut cfg, hook } => {
                cfg.size = size;
                ClientMsg::Spawn { req, cfg, hook }
            },
            Open::Attach { id } => ClientMsg::Attach { req, id, size },
        };
        if self.inner.send(&msg).is_err() && self.inner.pending.lock().unwrap().remove(&req).is_some() {
            (slot.sink)(TermEvent::ServerLost);
        }
        RemoteSession { inner: self.inner.clone(), slot, closed: false }
    }

    /// The daemon's sessions.
    pub fn list(&self) -> io::Result<Vec<SessionInfo>> {
        let req = self.inner.req();
        let (tx, rx) = mpsc::channel();
        self.inner.lists.lock().unwrap().insert(req, tx);
        self.inner.send(&ClientMsg::List { req })?;
        let r = rx.recv_timeout(LIST_TIMEOUT).map_err(|_| io::Error::from(io::ErrorKind::TimedOut));
        self.inner.lists.lock().unwrap().remove(&req);
        r
    }

    /// End session `id` (one this client has no tab for).
    pub fn close(&self, id: u64) {
        let _ = self.inner.send(&ClientMsg::Close { id });
    }
}

/// The UI's way to the daemon: one connection, made again (starting the
/// daemon if needed) after the old one was lost.
pub struct Connector {
    dir: PathBuf,
    spawn_daemon: bool,
    hooks: HookSink,
    reveal: RevealSink,
    cur: Mutex<Option<MuxClient>>,
}

impl Connector {
    pub fn new(dir: PathBuf, spawn_daemon: bool, hooks: HookSink, reveal: RevealSink) -> Self {
        Self { dir, spawn_daemon, hooks, reveal, cur: Mutex::new(None) }
    }

    pub fn get(&self) -> io::Result<MuxClient> {
        let mut cur = self.cur.lock().unwrap();
        if let Some(c) = cur.as_ref().filter(|c| c.alive()) {
            return Ok(c.clone());
        }
        let c = MuxClient::connect(&self.dir, self.spawn_daemon, self.hooks.clone(), self.reveal.clone())?;
        *cur = Some(c.clone());
        Ok(c)
    }

    /// The connection, if a daemon already runs (never starts one).
    pub fn existing(&self) -> Option<MuxClient> {
        let mut cur = self.cur.lock().unwrap();
        if let Some(c) = cur.as_ref().filter(|c| c.alive()) {
            return Some(c.clone());
        }
        let c = MuxClient::connect(&self.dir, false, self.hooks.clone(), self.reveal.clone()).ok()?;
        *cur = Some(c.clone());
        Some(c)
    }

    /// Whether a live connection exists (doesn't connect).
    pub fn connected(&self) -> bool {
        self.cur.lock().unwrap().as_ref().is_some_and(MuxClient::alive)
    }

    /// The live connection's daemon version (doesn't connect).
    pub fn server_version(&self) -> Option<String> {
        self.cur.lock().unwrap().as_ref().filter(|c| c.alive()).map(|c| c.server_version().to_owned())
    }
}

/// Start the daemon from this executable, detached from us.
fn spawn() -> io::Result<()> {
    spawn_detached(&["--server"], log_file().as_deref())
}

/// Run this executable with `args` in its own session (it outlives us;
/// nobody waits for it), output appended to `log`.
pub fn spawn_detached(args: &[&str], log: Option<&Path>) -> io::Result<()> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe()?;
    let (out, err) = match log.and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok()) {
        Some(f) => (std::process::Stdio::from(f.try_clone()?), std::process::Stdio::from(f)),
        None => (std::process::Stdio::null(), std::process::Stdio::null()),
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args).stdin(std::process::Stdio::null()).stdout(out).stderr(err);
    if std::env::var_os("RUST_LOG").is_none() {
        cmd.env("RUST_LOG", "info");
    }
    // SAFETY: setsid is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().map(drop)
}

/// `~/Library/Logs/Tern/<name>.log` (`<name>-dev.log` for debug builds).
pub fn log_path(name: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let dir = PathBuf::from(home).join("Library/Logs/Tern");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(if cfg!(debug_assertions) { format!("{name}-dev.log") } else { format!("{name}.log") }))
}

/// The daemon's log.
pub fn log_file() -> Option<PathBuf> {
    log_path("daemon")
}

fn read_loop(inner: &Arc<Inner>, mut reader: FrameReader) {
    loop {
        let msg = match reader.read::<ServerMsg>() {
            Ok(Some(m)) => m,
            Ok(None) => break,
            Err(e) => {
                log::warn!("daemon connection: {e}");
                break;
            },
        };
        match msg {
            ServerMsg::Hello { .. } => {},
            ServerMsg::Reveal { id } => (inner.reveal)(id),
            // For observers.
            ServerMsg::SessionsChanged { .. } | ServerMsg::Revealed { .. } => {},
            ServerMsg::Attached { req, id, token, pid: _, fd, snapshot } => {
                let tty = if fd { reader.take_fd().map(File::from) } else { None };
                let Some(slot) = inner.pending.lock().unwrap().remove(&req) else { continue };
                let snap = match proto::unpack_snapshot(&snapshot) {
                    Ok(s) => s,
                    Err(e) => {
                        log::error!("bad snapshot for {id}: {e}");
                        (slot.sink)(TermEvent::SpawnError(format!("bad snapshot: {e}")));
                        continue;
                    },
                };
                slot.mirror.lock().unwrap().install(&snap);
                let early = {
                    let mut early = slot.early.lock().unwrap();
                    *slot.id.lock().unwrap() = Some(id);
                    std::mem::take(&mut *early)
                };
                if slot.gone.load(Ordering::SeqCst) {
                    // Its tab went away before the answer.
                    let msg = if slot.close.load(Ordering::SeqCst) { ClientMsg::Close { id } } else { ClientMsg::Detach { id } };
                    let _ = inner.send(&msg);
                    continue;
                }
                *slot.tty.lock().unwrap() = tty;
                inner.sessions.lock().unwrap().insert(id, slot.clone());
                (slot.sink)(TermEvent::Ready { id, token, status: snap.status });
                // The daemon sized it as the request said; the view may
                // have moved on since.
                let want = *slot.want.lock().unwrap();
                if want != slot.sent {
                    let _ = inner.send(&ClientMsg::Resize { id, size: want });
                }
                inner.input(id, &early);
            },
            ServerMsg::NotFound { req, .. } => {
                if let Some(slot) = inner.pending.lock().unwrap().remove(&req) {
                    (slot.sink)(TermEvent::NotFound);
                }
            },
            ServerMsg::Error { req, message } => {
                if let Some(slot) = inner.pending.lock().unwrap().remove(&req) {
                    (slot.sink)(TermEvent::SpawnError(message));
                }
            },
            ServerMsg::Output { id, bytes } => {
                let slot = inner.sessions.lock().unwrap().get(&id).cloned();
                if let Some(slot) = slot {
                    slot.mirror.lock().unwrap().feed(&bytes);
                }
            },
            ServerMsg::Marker { id, marker } => {
                let slot = inner.sessions.lock().unwrap().get(&id).cloned();
                if let Some(slot) = slot {
                    slot.mirror.lock().unwrap().marker(&marker);
                }
            },
            ServerMsg::Hook { token, body } => (inner.hooks)(token, body),
            // Attached elsewhere, or fell behind: the tab reattaches.
            ServerMsg::Detached { id } => {
                let slot = inner.sessions.lock().unwrap().remove(&id);
                if let Some(slot) = slot {
                    slot.tty.lock().unwrap().take();
                    (slot.sink)(TermEvent::ServerLost);
                }
            },
            ServerMsg::Sessions { req, list } => {
                if let Some(tx) = inner.lists.lock().unwrap().remove(&req) {
                    let _ = tx.send(list);
                }
            },
        }
    }
    inner.alive.store(false, Ordering::Release);
    let slots: Vec<_> =
        inner.pending.lock().unwrap().drain().map(|(_, s)| s).chain(inner.sessions.lock().unwrap().drain().map(|(_, s)| s)).collect();
    for slot in slots {
        // Holding the PTY would keep its programs from getting a hangup.
        slot.tty.lock().unwrap().take();
        (slot.sink)(TermEvent::ServerLost);
    }
}

/// A tab's session in the daemon. Dropping it detaches (the program keeps
/// running); [`RemoteSession::close`] ends it.
pub struct RemoteSession {
    inner: Arc<Inner>,
    slot: Arc<Slot>,
    closed: bool,
}

impl RemoteSession {
    /// The mirror terminal. Lock briefly (render prep only).
    pub fn term(&self) -> &Arc<FairMutex<MirrorTerm>> {
        &self.slot.term
    }

    /// Clears the pending-wakeup flag; returns whether one was pending.
    pub fn take_wakeup(&self) -> bool {
        self.slot.wake.swap(false, Ordering::AcqRel)
    }

    /// The session's id, once the daemon answered.
    pub fn id(&self) -> Option<u64> {
        *self.slot.id.lock().unwrap()
    }

    /// Send input; before the daemon answered, it is kept and sent then.
    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let bytes = bytes.into();
        let id = {
            let mut early = self.slot.early.lock().unwrap();
            let id = *self.slot.id.lock().unwrap();
            if id.is_none() && early.len() + bytes.len() <= MAX_EARLY_INPUT {
                early.extend_from_slice(&bytes);
            }
            id
        };
        if let Some(id) = id {
            self.inner.input(id, &bytes);
        }
    }

    /// Ask for a new size; the mirror follows when the daemon applied it.
    pub fn resize(&self, size: TermSize) {
        {
            let mut want = self.slot.want.lock().unwrap();
            if *want == size {
                return;
            }
            *want = size;
        }
        if let Some(id) = self.id() {
            let _ = self.inner.send(&ClientMsg::Resize { id, size });
        }
    }

    /// The size the view asked for.
    pub fn size(&self) -> TermSize {
        *self.slot.want.lock().unwrap()
    }

    /// See [`tern_term::procs::foreground_programs`].
    pub fn foreground_programs(&self) -> Vec<String> {
        self.slot.tty.lock().unwrap().as_ref().map(tern_term::procs::foreground_programs).unwrap_or_default()
    }

    /// See [`tern_term::procs::foreground_cwd`].
    pub fn foreground_cwd(&self) -> Option<PathBuf> {
        self.slot.tty.lock().unwrap().as_ref().and_then(tern_term::procs::foreground_cwd)
    }

    /// End the program and forget the session.
    pub fn close(mut self) {
        self.closed = true;
        // Holding the PTY would keep its programs from getting a hangup.
        self.slot.tty.lock().unwrap().take();
        if let Some(id) = self.id() {
            let _ = self.inner.send(&ClientMsg::Close { id });
        }
    }
}

impl Drop for RemoteSession {
    fn drop(&mut self) {
        // When the daemon hasn't answered yet, the reader deals with the
        // session (see `gone`); either side may do it, twice is harmless.
        self.slot.close.store(self.closed, Ordering::SeqCst);
        self.slot.gone.store(true, Ordering::SeqCst);
        if let Some(id) = self.id() {
            self.inner.sessions.lock().unwrap().remove(&id);
            if !self.closed {
                let _ = self.inner.send(&ClientMsg::Detach { id });
            }
        }
    }
}
