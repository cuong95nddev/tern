//! The UI's side: one connection to the daemon for every tab.
//!
//! A reader thread applies what the daemon sends to each tab's [`Mirror`]
//! and forwards events to the tab's sink. [`RemoteSession`] has the surface
//! the terminal view used on an in-process session.

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tern_term::mirror::{self, Mirror, MirrorTerm};
use tern_term::vt::sync::FairMutex;
use tern_term::vt::term::TermMode;
use tern_term::{EventSink, SpawnConfig, TermEvent, TermSize};

use crate::UnixStream;
use crate::paths;
use crate::proto::{self, ClientMsg, Controller, FrameReader, GUARD_MINOR, HANDOFF_MINOR, MAJOR, MINOR, Role, ServerMsg, SessionInfo};

/// What the client asks who runs in a session's foreground with: the PTY
/// the daemon passed, on Windows the shell's pid.
#[cfg(unix)]
type Pty = std::fs::File;
#[cfg(windows)]
type Pty = u32;

/// Gets each hook request: the tab's token and the JSON body.
pub type HookSink = Arc<dyn Fn(String, Vec<u8>) + Send + Sync>;
/// Gets each request to show a session (from the menu bar agent).
pub type RevealSink = Arc<dyn Fn(u64) + Send + Sync>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const LIST_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a handoff may take (the daemons give up on each other sooner).
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(60);
/// Input is sent in pieces of at most this (a big paste stays under the
/// daemon's frame limit).
const INPUT_CHUNK: usize = 1 << 20;
/// Input typed before the daemon answered, kept to send once it has.
const MAX_EARLY_INPUT: usize = 16 << 20;
/// Input waiting for the writer thread past this makes the sender wait:
/// only a daemon that stopped reading lets it pile up so far.
const MAX_QUEUED_INPUT: usize = 64 << 20;
/// How long closing the connection waits for queued messages (a tab's
/// `Close` as the app quits) to go out.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(1);

pub enum Open {
    /// Start a program. With `hook`, `{tern-settings}` in its args becomes
    /// the path of its Claude settings file. With `seed` (a
    /// [`proto::pack_snapshot`] of `cfg.size`), the terminal starts with
    /// that screen's lines in its scrollback.
    Spawn { cfg: SpawnConfig, hook: bool, seed: Option<Vec<u8>> },
    /// Attach to a running (or exited) session.
    Attach { id: u64 },
}

/// One tab's state on the client side.
struct Slot {
    mirror: Mutex<Mirror>,
    term: Arc<FairMutex<MirrorTerm>>,
    wake: Arc<AtomicBool>,
    mode: Arc<AtomicU32>,
    sink: EventSink,
    /// Set once the daemon answered; while `None`, input waits in `early`
    /// (its lock is held to set the id).
    id: Mutex<Option<u64>>,
    early: Mutex<Vec<u8>>,
    /// The PTY master, for foreground-process queries.
    tty: Mutex<Option<Pty>>,
    /// The size the view wants; sent once the session is known.
    want: Mutex<TermSize>,
    /// The size the open request carried.
    sent: TermSize,
    /// The tab let go of it: `close` says whether to end the session or
    /// only detach, once its id is known.
    gone: AtomicBool,
    close: AtomicBool,
    /// Images its programs sent (kitty graphics), by image id.
    images: Mutex<HashMap<u32, ShownImage>>,
}

/// An image a session's program sent, for drawing its placeholder cells.
#[derive(Debug, Clone)]
pub struct ShownImage {
    /// Changes when the pixels do.
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    /// 4 bytes a pixel, row by row.
    pub rgba: Arc<[u8]>,
    /// Virtual placements: `(placement id, cols, rows)`.
    pub placements: Vec<(u32, u32, u32)>,
}

impl ShownImage {
    /// The box (cols, rows) of the placement a placeholder names, else of
    /// the image's first one; `(0, 0)` with none.
    pub fn placement(&self, id: u32) -> (u32, u32) {
        let p = self.placements.iter().find(|p| p.0 == id).or_else(|| self.placements.first());
        p.map_or((0, 0), |p| (p.1, p.2))
    }
}

struct Inner {
    outbox: Outbox,
    alive: AtomicBool,
    next_req: AtomicU64,
    pending: Mutex<HashMap<u64, Arc<Slot>>>,
    sessions: Mutex<HashMap<u64, Arc<Slot>>>,
    lists: Mutex<HashMap<u64, mpsc::Sender<Vec<SessionInfo>>>>,
    /// Handoffs asked for: each gets why it failed (dropped when the
    /// connection ends, as it does when one succeeds).
    handoffs: Mutex<HashMap<u64, mpsc::Sender<String>>>,
    hooks: HookSink,
    reveal: RevealSink,
    server_pid: u32,
    server_minor: u32,
    server_version: String,
    server_build: String,
}

impl Inner {
    /// Queue `msg` for the writer thread. Never waits on the socket.
    fn send(&self, msg: ClientMsg) -> io::Result<()> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(io::ErrorKind::NotConnected.into());
        }
        self.outbox.push(msg)
    }

    fn req(&self) -> u64 {
        self.next_req.fetch_add(1, Ordering::Relaxed)
    }

    fn input(&self, id: u64, bytes: &[u8]) {
        for chunk in bytes.chunks(INPUT_CHUNK) {
            let _ = self.send(ClientMsg::Input { id, bytes: chunk.to_vec() });
        }
    }
}

/// Messages waiting for the connection's writer thread. The UI thread only
/// queues: a daemon that is slow to read would otherwise freeze it (for up
/// to the write timeout per message), and a write timing out halfway would
/// leave half a frame on the wire.
#[derive(Default)]
struct Outbox {
    state: Mutex<OutboxState>,
    /// Something to write, or closed.
    ready: Condvar,
    /// Queued input went down, a batch was written, or closed.
    room: Condvar,
}

#[derive(Default)]
struct OutboxState {
    msgs: VecDeque<ClientMsg>,
    /// Input bytes in `msgs`.
    input: usize,
    /// The writer thread is writing a batch.
    writing: bool,
    closed: bool,
}

impl Outbox {
    fn push(&self, msg: ClientMsg) -> io::Result<()> {
        let size = match &msg {
            ClientMsg::Input { bytes, .. } => bytes.len(),
            _ => 0,
        };
        let mut st = self.state.lock().unwrap();
        while !st.closed && st.input > 0 && st.input + size > MAX_QUEUED_INPUT {
            st = self.room.wait(st).unwrap();
        }
        if st.closed {
            return Err(io::ErrorKind::NotConnected.into());
        }
        st.input += size;
        st.msgs.push_back(msg);
        self.ready.notify_one();
        Ok(())
    }

    /// Everything queued, once there is something; `None` once closed.
    fn take(&self) -> Option<Vec<ClientMsg>> {
        let mut st = self.state.lock().unwrap();
        while st.msgs.is_empty() && !st.closed {
            st = self.ready.wait(st).unwrap();
        }
        if st.closed {
            return None;
        }
        st.input = 0;
        st.writing = true;
        self.room.notify_all();
        Some(st.msgs.drain(..).collect())
    }

    fn written(&self) {
        self.state.lock().unwrap().writing = false;
        self.room.notify_all();
    }

    /// Wait (up to `timeout`) until everything queued has been written.
    fn flush(&self, timeout: Duration) {
        let st = self.state.lock().unwrap();
        let _ = self.room.wait_timeout_while(st, timeout, |st| !st.closed && (st.writing || !st.msgs.is_empty()));
    }

    /// Drop what's queued and refuse more; the writer thread ends.
    fn close(&self) {
        let mut st = self.state.lock().unwrap();
        st.closed = true;
        st.msgs.clear();
        st.input = 0;
        self.ready.notify_all();
        self.room.notify_all();
    }
}

/// Write what's queued, as one write per batch. A failed write ends the
/// connection: the reader then sees it closed and tells every tab.
fn write_loop(inner: &Inner, mut stream: UnixStream) {
    let mut buf = Vec::new();
    while let Some(msgs) = inner.outbox.take() {
        buf.clear();
        for msg in &msgs {
            match proto::encode(msg) {
                Ok(frame) => buf.extend_from_slice(&frame),
                Err(e) => log::error!("can't encode a message for the daemon: {e}"),
            }
        }
        if let Err(e) = stream.write_all(&buf) {
            log::warn!("writing to the daemon failed: {e}");
            inner.alive.store(false, Ordering::Release);
            inner.outbox.close();
            let _ = stream.shutdown(std::net::Shutdown::Both);
            return;
        }
        inner.outbox.written();
        // A big paste needn't keep its buffer.
        if buf.capacity() > INPUT_CHUNK * 2 {
            buf = Vec::new();
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

struct Closer(UnixStream, Arc<Inner>);

impl Drop for Closer {
    fn drop(&mut self) {
        // Tabs dropped just before (the app quitting) queued their `Close`
        // or `Detach`: send those first.
        self.1.outbox.flush(FLUSH_TIMEOUT);
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

impl MuxClient {
    /// Connect to the daemon in `dir`, starting it first when `spawn_daemon`
    /// and none runs.
    pub fn connect(dir: &Path, spawn_daemon: bool, hooks: HookSink, reveal: RevealSink) -> io::Result<Self> {
        // A daemon that was exiting (idle) turns us away: once it has gone,
        // a new one starts.
        let deadline = Instant::now() + 2 * CONNECT_TIMEOUT;
        let mut turned_away = false;
        loop {
            match Self::connect_once(dir, spawn_daemon, hooks.clone(), reveal.clone()) {
                Err(e) if spawn_daemon && (turned_away || e.kind() == io::ErrorKind::ConnectionAborted) && Instant::now() < deadline => {
                    log::info!("daemon went away while connecting ({e}); again");
                    turned_away = true;
                    std::thread::sleep(Duration::from_millis(100));
                },
                r => return r,
            }
        }
    }

    fn connect_once(dir: &Path, spawn_daemon: bool, hooks: HookSink, reveal: RevealSink) -> io::Result<Self> {
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
        let gone = |e: io::Error| match e.kind() {
            io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset | io::ErrorKind::UnexpectedEof => {
                io::Error::new(io::ErrorKind::ConnectionAborted, e)
            },
            _ => e,
        };
        proto::write_frame(
            &mut writer,
            &ClientMsg::Hello { major: MAJOR, minor: MINOR, app_version: env!("CARGO_PKG_VERSION").into(), role: Role::Ui },
        )
        .map_err(gone)?;
        let mut reader = FrameReader::new(stream, proto::MAX_SERVER_FRAME);
        let (server_pid, server_minor, server_version, server_build) = match reader.read::<ServerMsg>().map_err(gone)? {
            Some(ServerMsg::Hello { major, minor, pid, app_version, build }) if major == MAJOR => {
                log::info!("connected to daemon {pid} (Tern {app_version}, build {build})");
                (pid, minor, app_version, build)
            },
            None => return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "daemon closed the connection")),
            other => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("bad hello: {other:?}"))),
        };
        // The reader blocks from now on. Cleared on the reader's own handle:
        // on Windows a clone's timeout is its own, and a quiet session would
        // drop the connection every few seconds.
        reader.get_ref().set_read_timeout(None)?;
        let closer = writer.try_clone()?;
        let inner = Arc::new(Inner {
            outbox: Outbox::default(),
            alive: AtomicBool::new(true),
            next_req: AtomicU64::new(1),
            pending: Mutex::default(),
            sessions: Mutex::default(),
            lists: Mutex::default(),
            handoffs: Mutex::default(),
            hooks,
            reveal,
            server_pid,
            server_minor,
            server_version,
            server_build,
        });
        let i = inner.clone();
        std::thread::Builder::new().name("mux writer".into()).spawn(move || write_loop(&i, writer))?;
        let i = inner.clone();
        let spawned = std::thread::Builder::new().name("mux reader".into()).spawn(move || read_loop(&i, reader));
        if let Err(e) = spawned {
            inner.outbox.close();
            return Err(e);
        }
        Ok(Self { _conn: Arc::new(Closer(closer, inner.clone())), inner })
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

    /// Whether the daemon runs this very build (a daemon left from an
    /// earlier build of the same version doesn't).
    pub fn server_same_build(&self) -> bool {
        let ours = paths::build_id();
        // A build that couldn't be read tells nothing.
        self.inner.server_build.is_empty() || ours.is_empty() || self.inner.server_build == ours
    }

    /// Whether the daemon runs another version or build than this app.
    pub fn server_outdated(&self) -> bool {
        self.inner.server_version != env!("CARGO_PKG_VERSION") || !self.server_same_build()
    }

    /// Whether the daemon can hand its sessions to another (Unix only).
    pub fn can_hand_off(&self) -> bool {
        cfg!(unix) && self.inner.server_minor >= HANDOFF_MINOR
    }

    /// Ask the daemon to hand its sessions to a new one started from `exe`.
    /// `Ok` once it let go (this connection ends); the daemon's reason when
    /// it didn't.
    pub fn request_hand_off(&self, exe: PathBuf) -> io::Result<()> {
        let req = self.inner.req();
        let (tx, rx) = mpsc::channel();
        self.inner.handoffs.lock().unwrap().insert(req, tx);
        self.inner.send(ClientMsg::HandOff { req, exe })?;
        let r = match rx.recv_timeout(HANDOFF_TIMEOUT) {
            Ok(message) => Err(io::Error::other(message)),
            Err(mpsc::RecvTimeoutError::Disconnected) => Ok(()),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(io::ErrorKind::TimedOut.into()),
        };
        self.inner.handoffs.lock().unwrap().remove(&req);
        r
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
            mode: mirror.mode_flag().clone(),
            mirror: Mutex::new(mirror),
            sink,
            id: Mutex::new(None),
            early: Mutex::new(Vec::new()),
            tty: Mutex::new(None),
            want: Mutex::new(size),
            images: Mutex::new(HashMap::new()),
        });
        self.inner.pending.lock().unwrap().insert(req, slot.clone());
        let msg = match open {
            Open::Spawn { mut cfg, hook, seed } => {
                cfg.size = size;
                ClientMsg::Spawn { req, cfg, hook, seed }
            },
            Open::Attach { id } => ClientMsg::Attach { req, id, size },
        };
        if self.inner.send(msg).is_err() && self.inner.pending.lock().unwrap().remove(&req).is_some() {
            (slot.sink)(TermEvent::ServerLost);
        }
        RemoteSession { inner: self.inner.clone(), slot, closed: false }
    }

    /// The daemon's sessions.
    pub fn list(&self) -> io::Result<Vec<SessionInfo>> {
        let req = self.inner.req();
        let (tx, rx) = mpsc::channel();
        self.inner.lists.lock().unwrap().insert(req, tx);
        self.inner.send(ClientMsg::List { req })?;
        let r = rx.recv_timeout(LIST_TIMEOUT).map_err(|_| io::Error::from(io::ErrorKind::TimedOut));
        self.inner.lists.lock().unwrap().remove(&req);
        r
    }

    /// End session `id` (one this client has no tab for).
    pub fn close(&self, id: u64) {
        let _ = self.inner.send(ClientMsg::Close { id });
    }

    /// Tell the daemon how session `id`'s Claude conversation keeps its
    /// context window in bounds (an older daemon can't: it is left as it is).
    pub fn set_context_guard(&self, id: u64, limit: u8, auto_switch: bool) {
        if self.inner.server_minor >= GUARD_MINOR {
            let _ = self.inner.send(ClientMsg::ContextGuard { id, limit, auto_switch });
        }
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
        // Before an update can replace the executable.
        paths::build_id();
        Self { dir, spawn_daemon, hooks, reveal, cur: Mutex::new(None) }
    }

    /// The connection, starting a daemon if none runs. A daemon of another
    /// build that runs no program is replaced by one of this build (nothing
    /// is lost), so an update applies once its terminals are gone.
    pub fn get(&self) -> io::Result<MuxClient> {
        let mut cur = self.cur.lock().unwrap();
        let c = match cur.as_ref().filter(|c| c.alive()) {
            Some(c) => c.clone(),
            None => MuxClient::connect(&self.dir, self.spawn_daemon, self.hooks.clone(), self.reveal.clone())?,
        };
        if self.spawn_daemon && c.server_outdated() && c.list().is_ok_and(|l| l.iter().all(|s| s.exited)) {
            log::info!("replacing idle daemon {} (Tern {})", c.server_pid(), c.server_version());
            let pid = c.server_pid();
            drop(c);
            *cur = None;
            kill_process(pid)?;
            let c = MuxClient::connect(&self.dir, true, self.hooks.clone(), self.reveal.clone())?;
            *cur = Some(c.clone());
            return Ok(c);
        }
        *cur = Some(c.clone());
        Ok(c)
    }

    /// The live connection, when it needs no work: connected, to a daemon
    /// of this build (doesn't connect).
    pub fn ready(&self) -> Option<MuxClient> {
        self.cur.lock().unwrap().as_ref().filter(|c| c.alive() && !c.server_outdated()).cloned()
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

    /// Keep session `id`'s Claude conversation under `limit` percent of its
    /// context window (0 for no limit), going on in a fresh conversation
    /// with `auto_switch`. Only over a live connection (doesn't connect):
    /// the tab tells it again each time it attaches.
    pub fn set_context_guard(&self, id: u64, limit: u8, auto_switch: bool) {
        if let Some(c) = self.cur.lock().unwrap().as_ref().filter(|c| c.alive()) {
            c.set_context_guard(id, limit, auto_switch);
        }
    }

    /// Whether a live connection exists (doesn't connect).
    pub fn connected(&self) -> bool {
        self.cur.lock().unwrap().as_ref().is_some_and(MuxClient::alive)
    }

    /// Move the daemon's sessions to a new daemon of this build, if it runs
    /// another one (their programs keep running; tabs reattach). Fails when
    /// the daemon can't (Windows, older versions) or the handoff failed,
    /// which leaves the old daemon serving as before.
    pub fn hand_off(&self) -> io::Result<()> {
        let Some(c) = self.existing() else { return Ok(()) };
        if !c.server_outdated() {
            return Ok(());
        }
        if !c.can_hand_off() {
            return Err(io::Error::new(io::ErrorKind::Unsupported, "the background server can't hand off"));
        }
        let old = c.server_pid();
        c.request_hand_off(std::env::current_exe()?)?;
        drop(c);
        *self.cur.lock().unwrap() = None;
        // The old daemon let go once the new one listened on the socket.
        let c = self.existing().ok_or_else(|| io::Error::other("no background server after the handoff"))?;
        if c.server_pid() == old || c.server_outdated() {
            return Err(io::Error::other(format!("still on daemon {} (Tern {})", c.server_pid(), c.server_version())));
        }
        log::info!("handed off from daemon {old} to {}", c.server_pid());
        Ok(())
    }

    /// End the daemon and every program it runs, found by the pid it gave
    /// (so an older daemon too, one that knows no request for it). The next
    /// [`get`](Self::get) starts a new daemon. Fine when none runs.
    pub fn stop_server(&self) -> io::Result<()> {
        let pid = match self.existing() {
            Some(client) => client.server_pid(),
            // Not answering: a stuck daemon still holds its lock.
            None => match stuck_daemon_pid(&self.dir) {
                Some(pid) => pid,
                None => return Ok(()),
            },
        };
        *self.cur.lock().unwrap() = None;
        kill_process(pid)
    }

    /// Daemons of other protocol majors (an older Tern's) that still run.
    /// No window can show their terminals.
    pub fn other_daemons(&self) -> Vec<OtherDaemon> {
        paths::other_majors(&self.dir).into_iter().filter_map(|major| OtherDaemon::probe(&self.dir, major)).collect()
    }
}

/// A daemon of another protocol major.
#[derive(Clone, Debug)]
pub struct OtherDaemon {
    pub major: u32,
    pub pid: u32,
    pub version: String,
}

impl OtherDaemon {
    /// Its `Hello`, which every major sends first and encodes alike.
    fn probe(dir: &Path, major: u32) -> Option<Self> {
        let stream = UnixStream::connect(paths::socket_path_of(dir, major)).ok()?;
        if !paths::peer_uid_ok(&stream) {
            return None;
        }
        stream.set_read_timeout(Some(Duration::from_secs(1))).ok()?;
        match FrameReader::new(stream, proto::MAX_SERVER_FRAME).read::<ServerMsg>() {
            Ok(Some(ServerMsg::Hello { major, pid, app_version, .. })) => Some(Self { major, pid, version: app_version }),
            _ => None,
        }
    }

    /// End it and every program it runs.
    pub fn stop(&self) -> io::Result<()> {
        kill_process(self.pid)
    }
}

/// The pid of a daemon that holds the lock in `dir` (so runs) but doesn't
/// answer.
fn stuck_daemon_pid(dir: &Path) -> Option<u32> {
    let lock = std::fs::OpenOptions::new().write(true).open(paths::lock_path(dir)).ok()?;
    if lock.try_lock().is_ok() {
        // Free: no daemon.
        return None;
    }
    std::fs::read_to_string(paths::pid_path(dir)).ok()?.trim().parse().ok()
}

impl Connector {
    /// The live connection's daemon version (doesn't connect).
    pub fn server_version(&self) -> Option<String> {
        self.cur.lock().unwrap().as_ref().filter(|c| c.alive()).map(|c| c.server_version().to_owned())
    }

    /// Whether the live connection's daemon runs this build (doesn't connect).
    pub fn server_same_build(&self) -> Option<bool> {
        self.cur.lock().unwrap().as_ref().filter(|c| c.alive()).map(MuxClient::server_same_build)
    }
}

/// End process `pid`: asked first, then forced.
#[cfg(unix)]
pub fn kill_process(pid: u32) -> io::Result<()> {
    let pid = pid as libc::pid_t;
    // SAFETY: plain signals to a pid the daemon gave us.
    let alive = || unsafe { libc::kill(pid, 0) } == 0;
    if pid <= 1 || !alive() {
        return Ok(());
    }
    unsafe { libc::kill(pid, libc::SIGTERM) };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while alive() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    if alive() && unsafe { libc::kill(pid, libc::SIGKILL) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
pub fn kill_process(pid: u32) -> io::Result<()> {
    let status = std::process::Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).status()?;
    if status.success() { Ok(()) } else { Err(io::Error::other(format!("taskkill exited with {status}"))) }
}

/// Start the daemon, detached from us.
fn spawn() -> io::Result<()> {
    spawn_background(&["--server"], log_file().as_deref())
}

/// Run this executable with `args`, detached from us, for a process that
/// outlives the app (the daemon, `--web`).
#[cfg(unix)]
pub fn spawn_background(args: &[&str], log: Option<&Path>) -> io::Result<()> {
    spawn_detached(args, log)
}

/// Run a copy of this executable with `args`, detached from us: a running
/// `tern.exe` can't be replaced, and these processes outlive updates.
#[cfg(windows)]
pub fn spawn_background(args: &[&str], log: Option<&Path>) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let dir = paths::mux_dir();
    paths::ensure_private_dir(&dir)?;
    // Named for the protocol (a running daemon's copy can't be replaced,
    // and another major's would only find its own daemon's lock taken) and
    // for the build, so a copy of that name is this very executable.
    let build = paths::build_id();
    let name = format!("tern-server-{}-p{MAJOR}-{}", env!("CARGO_PKG_VERSION"), build.get(..12).unwrap_or(build));
    let copy = dir.join(format!("{name}.exe"));
    let same_len = || std::fs::metadata(&exe).ok().map(|m| m.len()) == std::fs::metadata(&copy).ok().map(|m| m.len());
    // An unknown build is only told by its size.
    if !copy.is_file() || (build.is_empty() && !same_len()) {
        // Another UI may be copying it too: copy beside it, then move.
        let tmp = dir.join(format!("{name}.exe.{}", std::process::id()));
        std::fs::copy(&exe, &tmp)?;
        if let Err(e) = std::fs::rename(&tmp, &copy) {
            let _ = std::fs::remove_file(&tmp);
            // Another UI put it there first: the same build, use it.
            if !copy.is_file() {
                return Err(e);
            }
        }
    }
    sweep_old_copies(&dir, &copy);
    spawn_detached_exe(&copy, args, log)
}

/// Delete the daemon copies of other builds (those still running stay), and
/// copies left half made a while ago.
#[cfg(windows)]
fn sweep_old_copies(dir: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let stale =
        |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|d| d > Duration::from_secs(600)));
    for e in entries.flatten() {
        let p = e.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !name.starts_with("tern-server-") || p == keep {
            continue;
        }
        // Another UI may be copying into a fresh one.
        if name.ends_with(".exe") || stale(&p) {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// Run this executable with `args` in its own session (it outlives us;
/// nobody waits for it), output appended to `log`.
pub fn spawn_detached(args: &[&str], log: Option<&Path>) -> io::Result<()> {
    spawn_detached_exe(&std::env::current_exe()?, args, log)
}

/// `spawn_detached` for another executable.
pub fn spawn_detached_exe(exe: &Path, args: &[&str], log: Option<&Path>) -> io::Result<()> {
    let (out, err) = match log.and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok()) {
        Some(f) => (std::process::Stdio::from(f.try_clone()?), std::process::Stdio::from(f)),
        None => (std::process::Stdio::null(), std::process::Stdio::null()),
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args).stdin(std::process::Stdio::null()).stdout(out).stderr(err);
    if std::env::var_os("RUST_LOG").is_none() {
        cmd.env("RUST_LOG", "info");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        cmd.spawn().map(drop)
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        // Out of our job too, if it lets us: a job that ends with Tern
        // would take the daemon with it.
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
        cmd.spawn().map(drop).or_else(|_| {
            cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
            cmd.spawn().map(drop)
        })
    }
}

/// `<name>.log` in [`paths::logs`] (`<name>-dev.log` for debug builds).
pub fn log_path(name: &str) -> Option<PathBuf> {
    let dir = paths::logs();
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(if cfg!(debug_assertions) { format!("{name}-dev.log") } else { format!("{name}.log") }))
}

/// The daemon's log.
pub fn log_file() -> Option<PathBuf> {
    log_path("daemon")
}

/// Keep `data` for drawing; only its placements when it carries no pixels.
fn show_image(slot: &Slot, data: proto::ImageData) {
    let mut images = slot.images.lock().unwrap();
    let rgba: Arc<[u8]> = match &data.pixels {
        Some(packed) => match proto::unpack_pixels(packed) {
            Ok(px) if px.len() == data.width as usize * data.height as usize * 4 => px.into(),
            _ => return log::warn!("bad pixels for image {}", data.image),
        },
        None => match images.get(&data.image) {
            Some(old) if old.generation == data.generation => old.rgba.clone(),
            _ => return,
        },
    };
    images.insert(
        data.image,
        ShownImage { generation: data.generation, width: data.width, height: data.height, rgba, placements: data.placements },
    );
    drop(images);
    slot.mirror.lock().unwrap().redraw();
}

fn read_loop(inner: &Arc<Inner>, mut reader: FrameReader) {
    // Mirrors holding bytes back (their UI drew): parsed before waiting for
    // more, so nothing stays unshown while the daemon is quiet.
    let mut held: Vec<Arc<Slot>> = Vec::new();
    loop {
        if !reader.has_frame() {
            for slot in held.drain(..) {
                slot.mirror.lock().unwrap().flush();
            }
        }
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
            ServerMsg::Attached { req, id, token, pid, fd, snapshot } => {
                #[cfg(unix)]
                let tty = if fd { reader.take_fd().map(Pty::from) } else { None };
                #[cfg(unix)]
                let _ = pid;
                #[cfg(windows)]
                let tty = (!fd && pid != 0).then_some(pid);
                let Some(slot) = inner.pending.lock().unwrap().remove(&req) else { continue };
                let snap = match proto::unpack_snapshot(&snapshot) {
                    Ok(s) => s,
                    Err(e) => {
                        log::error!("bad snapshot for {id}: {e}");
                        (slot.sink)(TermEvent::SpawnError(format!("bad snapshot: {e}")));
                        continue;
                    },
                };
                let status = snap.status;
                // The images it holds follow.
                slot.images.lock().unwrap().clear();
                slot.mirror.lock().unwrap().install(snap);
                let early = {
                    let mut early = slot.early.lock().unwrap();
                    *slot.id.lock().unwrap() = Some(id);
                    std::mem::take(&mut *early)
                };
                if slot.gone.load(Ordering::SeqCst) {
                    // Its tab went away before the answer.
                    let msg = if slot.close.load(Ordering::SeqCst) { ClientMsg::Close { id } } else { ClientMsg::Detach { id } };
                    let _ = inner.send(msg);
                    continue;
                }
                *slot.tty.lock().unwrap() = tty;
                inner.sessions.lock().unwrap().insert(id, slot.clone());
                (slot.sink)(TermEvent::Ready { id, token, status });
                // The daemon sized it as the request said; the view may
                // have moved on since.
                let want = *slot.want.lock().unwrap();
                if want != slot.sent {
                    let _ = inner.send(ClientMsg::Resize { id, size: want });
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
                    let mut mirror = slot.mirror.lock().unwrap();
                    mirror.feed(&bytes);
                    if mirror.has_deferred() && !held.iter().any(|s| Arc::ptr_eq(s, &slot)) {
                        drop(mirror);
                        held.push(slot);
                    }
                }
            },
            ServerMsg::Marker { id, marker } => {
                let slot = inner.sessions.lock().unwrap().get(&id).cloned();
                if let Some(slot) = slot {
                    slot.mirror.lock().unwrap().marker(&marker);
                }
            },
            ServerMsg::Image { id, image } => {
                let slot = inner.sessions.lock().unwrap().get(&id).cloned();
                if let Some(slot) = slot {
                    show_image(&slot, image);
                }
            },
            ServerMsg::ImagesGone { id, images } => {
                let slot = inner.sessions.lock().unwrap().get(&id).cloned();
                if let Some(slot) = slot {
                    slot.images.lock().unwrap().retain(|k, _| !images.contains(k));
                    slot.mirror.lock().unwrap().redraw();
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
            ServerMsg::Control { id, by } => {
                let slot = inner.sessions.lock().unwrap().get(&id).cloned();
                if let Some(slot) = slot {
                    (slot.sink)(TermEvent::RemoteControl(by == Controller::Viewer));
                }
            },
            ServerMsg::HandOffFailed { req, message } => {
                if let Some(tx) = inner.handoffs.lock().unwrap().remove(&req) {
                    let _ = tx.send(message);
                }
            },
        }
    }
    inner.alive.store(false, Ordering::Release);
    inner.outbox.close();
    // A handoff asked for ends this connection.
    inner.handoffs.lock().unwrap().clear();
    let slots: Vec<_> =
        inner.pending.lock().unwrap().drain().map(|(_, s)| s).chain(inner.sessions.lock().unwrap().drain().map(|(_, s)| s)).collect();
    for slot in slots {
        // Holding the PTY would keep its programs from getting a hangup.
        slot.tty.lock().unwrap().take();
        (slot.sink)(TermEvent::ServerLost);
    }
}

/// What runs in a session's foreground, read from any thread.
pub struct Foreground(Arc<Slot>);

impl Foreground {
    /// See [`tern_term::procs::foreground_programs`].
    pub fn programs(&self) -> Vec<String> {
        #[cfg(unix)]
        return self.0.tty.lock().unwrap().as_ref().map(tern_term::procs::foreground_programs).unwrap_or_default();
        #[cfg(windows)]
        return self.0.tty.lock().unwrap().map(tern_term::procs::foreground_programs).unwrap_or_default();
    }

    /// See [`tern_term::procs::foreground_pid`]. `None` on Windows, which
    /// has no process groups.
    pub fn pid(&self) -> Option<u32> {
        #[cfg(unix)]
        return self.0.tty.lock().unwrap().as_ref().and_then(tern_term::procs::foreground_pid);
        #[cfg(windows)]
        return None;
    }

    /// See [`tern_term::procs::foreground_cwd`].
    pub fn cwd(&self) -> Option<PathBuf> {
        #[cfg(unix)]
        return self.0.tty.lock().unwrap().as_ref().and_then(tern_term::procs::foreground_cwd);
        #[cfg(windows)]
        return self.0.tty.lock().unwrap().and_then(tern_term::procs::foreground_cwd);
    }

    /// See [`tern_term::procs::agent_shells`]. None on Windows.
    pub fn agent_shells(&self) -> Vec<tern_term::procs::AgentShell> {
        #[cfg(unix)]
        return self.0.tty.lock().unwrap().as_ref().map(tern_term::procs::agent_shells).unwrap_or_default();
        #[cfg(windows)]
        return Vec::new();
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
    /// The images its programs sent (kitty graphics). Lock briefly.
    pub fn images(&self) -> std::sync::MutexGuard<'_, HashMap<u32, ShownImage>> {
        self.slot.images.lock().unwrap()
    }

    /// The mirror terminal. Lock briefly (render prep only).
    pub fn term(&self) -> &Arc<FairMutex<MirrorTerm>> {
        &self.slot.term
    }

    /// The terminal's mode, without locking it.
    pub fn mode(&self) -> TermMode {
        mirror::mode_of(&self.slot.mode)
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
            let _ = self.inner.send(ClientMsg::Resize { id, size });
        }
    }

    /// The size the view asked for.
    pub fn size(&self) -> TermSize {
        *self.slot.want.lock().unwrap()
    }

    /// See [`tern_term::procs::foreground_programs`].
    pub fn foreground_programs(&self) -> Vec<String> {
        self.foreground().programs()
    }

    /// Reads [`Self::foreground_programs`] off the UI thread: a scan of
    /// every process can take a while.
    pub fn foreground(&self) -> Foreground {
        Foreground(self.slot.clone())
    }

    /// See [`tern_term::procs::foreground_cwd`].
    pub fn foreground_cwd(&self) -> Option<PathBuf> {
        self.foreground().cwd()
    }

    /// Take the session back from the viewer that has it: its size and
    /// typing are this one's again.
    pub fn take_back(&self) {
        if let Some(id) = self.id() {
            let _ = self.inner.send(ClientMsg::Drive { id });
        }
    }

    /// End the program and forget the session.
    pub fn close(mut self) {
        self.closed = true;
        // Holding the PTY would keep its programs from getting a hangup.
        self.slot.tty.lock().unwrap().take();
        if let Some(id) = self.id() {
            let _ = self.inner.send(ClientMsg::Close { id });
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
                let _ = self.inner.send(ClientMsg::Detach { id });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outbox_batches_and_waits_for_room() {
        let outbox = Arc::new(Outbox::default());
        outbox.push(ClientMsg::Close { id: 1 }).unwrap();
        outbox.push(ClientMsg::Input { id: 1, bytes: vec![0; MAX_QUEUED_INPUT] }).unwrap();
        // Over the limit: the sender waits until the writer takes the queue.
        let o = outbox.clone();
        let sender = std::thread::spawn(move || o.push(ClientMsg::Input { id: 1, bytes: vec![1] }));
        std::thread::sleep(Duration::from_millis(50));
        assert!(!sender.is_finished());
        assert_eq!(outbox.take().unwrap().len(), 2);
        sender.join().unwrap().unwrap();
        outbox.written();
        assert_eq!(outbox.take().unwrap().len(), 1);
        outbox.written();
        // Nothing queued: flush returns at once.
        let t = Instant::now();
        outbox.flush(Duration::from_secs(5));
        assert!(t.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn closed_outbox_refuses_and_frees_waiters() {
        let outbox = Arc::new(Outbox::default());
        let o = outbox.clone();
        let writer = std::thread::spawn(move || o.take().is_none());
        std::thread::sleep(Duration::from_millis(50));
        outbox.close();
        assert!(writer.join().unwrap());
        assert!(outbox.push(ClientMsg::Close { id: 1 }).is_err());
    }
}
