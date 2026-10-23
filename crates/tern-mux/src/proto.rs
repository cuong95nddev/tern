//! Messages between the UI and the daemon, and their framing.
//!
//! A frame is a little-endian `u32` length followed by a msgpack message
//! (struct fields by name, so fields can be added). The first message each
//! way is `Hello`; a client says whether it is a UI or an observer (the menu
//! bar agent). A message of a kind the receiver doesn't know is skipped,
//! so a minor version can add kinds; the major is part of the socket name,
//! so incompatible versions never talk. Byte buffers (output, input, hook
//! bodies, snapshots) are msgpack `bin`, not arrays of numbers (MAJOR 2).
//!
//! A daemon hands its sessions to a daemon of a newer build over a private
//! socket with [`HandoffMsg`] (Unix only; see `server::hand_off`).

#[cfg(unix)]
use std::collections::VecDeque;
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tern_term::mirror::Marker;
use tern_term::snapshot::Snapshot;
use tern_term::status::ActivityStatus;
use tern_term::{SpawnConfig, TermSize};

use crate::UnixStream;
#[cfg(unix)]
use crate::fdpass;

pub const MAJOR: u32 = 2;
pub const MINOR: u32 = 9;
/// The first MINOR whose daemon takes `HandOff` (and `Spawn`'s `seed`).
pub const HANDOFF_MINOR: u32 = 3;
/// The first MINOR whose daemon takes `ContextGuard`.
pub const GUARD_MINOR: u32 = 5;
// MINOR 6: the daemon sends `Image` and `ImagesGone` (older clients skip them).
// MINOR 7: the daemon sends `Control` and takes `Drive`.
/// The first MINOR whose daemon takes `AskHandoff` and `ResumeHandoff`.
pub const ASK_HANDOFF_MINOR: u32 = 8;
/// The first MINOR whose daemon takes `Mute`.
pub const MUTE_MINOR: u32 = 9;
/// The handoff messages' layout; a daemon adopts from every version up to
/// its own.
pub const HANDOFF_VERSION: u32 = 2;

/// What a client is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    /// Tern's UI (a MINOR 0 client is one).
    #[default]
    Ui,
    /// The menu bar agent: watches and asks, doesn't keep the daemon alive.
    Observer,
}

/// Largest frame the daemon accepts (a big paste fits).
pub const MAX_CLIENT_FRAME: usize = 16 << 20;
/// Largest frame the UI accepts (a snapshot with full scrollback fits).
pub const MAX_SERVER_FRAME: usize = 256 << 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClientMsg {
    Hello {
        major: u32,
        minor: u32,
        app_version: String,
        #[serde(default)]
        role: Role,
    },
    /// Start a program and attach to it. With `hook`, the daemon gives it a
    /// hook token and replaces `{tern-settings}` in its args with the path of
    /// a Claude settings file pointing at its hook server. With `seed` (a
    /// [`pack_snapshot`] of `cfg.size`), the terminal starts with that
    /// screen's lines in its scrollback (MINOR 3; older daemons ignore it).
    Spawn {
        req: u64,
        cfg: SpawnConfig,
        hook: bool,
        #[serde(default, with = "serde_bytes")]
        seed: Option<Vec<u8>>,
    },
    Attach {
        req: u64,
        id: u64,
        size: TermSize,
    },
    Detach {
        id: u64,
    },
    Input {
        id: u64,
        #[serde(with = "serde_bytes")]
        bytes: Vec<u8>,
    },
    Resize {
        id: u64,
        size: TermSize,
    },
    /// End the program and forget the session.
    Close {
        id: u64,
    },
    List {
        req: u64,
    },
    /// From now on send `SessionsChanged` whenever the list changes (and
    /// once now).
    Watch,
    /// Ask the UI to show session `id`; answered by `Revealed`.
    Reveal {
        req: u64,
        id: u64,
    },
    /// Watch session `id` beside whoever is attached, without taking it
    /// (MINOR 2). Answered by `Attached` (no token, no fd); output and
    /// markers follow as for an attach, until `Unview`.
    View {
        req: u64,
        id: u64,
    },
    Unview {
        id: u64,
    },
    /// Hand every session to a new daemon started from `exe` (MINOR 3).
    /// On success the daemon closes every connection and exits; on failure
    /// it answers `HandOffFailed` and goes on.
    HandOff {
        req: u64,
        exe: PathBuf,
    },
    /// How session `id`'s Claude conversation keeps its context window in
    /// bounds (see `tern_bridge::context`): past `limit` percent (0 for
    /// none) it hands off, then with `auto_switch` goes on in a fresh one
    /// (MINOR 5).
    ContextGuard {
        id: u64,
        limit: u8,
        auto_switch: bool,
    },
    /// Take session `id`: from a viewer, the PTY takes its size and the
    /// owner's typing is dropped until the owner takes it back; from the
    /// owner, that (MINOR 7).
    Drive {
        id: u64,
    },
    /// The user asks session `id`'s Claude for a handoff, to step away: it
    /// writes one and stops (MINOR 8).
    AskHandoff {
        id: u64,
    },
    /// Session `id`'s Claude goes on in a fresh conversation from the
    /// handoff at `path` (when known) (MINOR 8).
    ResumeHandoff {
        id: u64,
        path: Option<String>,
    },
    /// Session `id`'s tab mutes (or unmutes) its notifications: the daemon
    /// doesn't post them while no UI is attached either (MINOR 9).
    Mute {
        id: u64,
        muted: bool,
    },
}

/// Who has a session, as told to one connection: whose size the PTY takes
/// and whose typing goes to the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Controller {
    /// The connection told.
    You,
    /// The UI attached to it (or no one).
    Desktop,
    /// Another viewer.
    Viewer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMsg {
    Hello {
        major: u32,
        minor: u32,
        app_version: String,
        pid: u32,
        /// [`paths::build_id`](crate::paths::build_id) (empty from a daemon
        /// older than it).
        #[serde(default)]
        build: String,
    },
    /// Reply to `Spawn` and `Attach`. When `fd` is set, the PTY's
    /// master fd came with the frame (for foreground-process queries only).
    /// `snapshot` is [`pack_snapshot`]'s output.
    Attached {
        req: u64,
        id: u64,
        token: Option<String>,
        pid: u32,
        fd: bool,
        #[serde(with = "serde_bytes")]
        snapshot: Vec<u8>,
    },
    NotFound {
        req: u64,
        id: u64,
    },
    Error {
        req: u64,
        message: String,
    },
    Output {
        id: u64,
        #[serde(with = "serde_bytes")]
        bytes: Vec<u8>,
    },
    Marker {
        id: u64,
        marker: Marker,
    },
    /// An image session `id`'s program sent (kitty graphics), new or
    /// changed; also every image right after `Attached` (MINOR 6).
    Image {
        id: u64,
        image: ImageData,
    },
    /// Images of session `id` that went (deleted, replaced by none, evicted).
    ImagesGone {
        id: u64,
        images: Vec<u32>,
    },
    /// A Claude Code hook request for `token` (JSON body).
    Hook {
        token: String,
        #[serde(with = "serde_bytes")]
        body: Vec<u8>,
    },
    /// Another connection attached to `id`, or (also to a viewer) its
    /// output fell behind: attach or view again for a fresh snapshot.
    Detached {
        id: u64,
    },
    Sessions {
        req: u64,
        list: Vec<SessionInfo>,
    },
    /// The session list, for a `Watch`er.
    SessionsChanged {
        list: Vec<SessionInfo>,
    },
    /// Show session `id` (asked by an observer).
    Reveal {
        id: u64,
    },
    /// `delivered`: a UI got the `Reveal`.
    Revealed {
        req: u64,
        delivered: bool,
    },
    /// Who has session `id` now: sent to its owner and viewers when that
    /// changes, and after `Attached` (MINOR 7).
    Control {
        id: u64,
        by: Controller,
    },
    /// The `HandOff` didn't happen; the daemon serves on.
    HandOffFailed {
        req: u64,
        message: String,
    },
}

/// Between a daemon handing off its sessions and the one adopting them. The
/// old daemon sends `Begin`, a `Session` for each, waits for `Restored`,
/// sends `Commit` and waits for `Committed`. Fields are added with defaults;
/// `version` says which an old daemon knew.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HandoffMsg {
    /// Comes with the daemon's lock fd, then its hook listener's fd when
    /// `hook_port` is set.
    Begin {
        version: u32,
        major: u32,
        app_version: String,
        /// Where the Claude settings files of its sessions are.
        run_dir: PathBuf,
        hook_port: Option<u16>,
        sessions: u32,
    },
    /// Comes with the PTY master fd when `fd` is set (the program runs).
    Session(Box<HandedSession>),
    Restored,
    Commit,
    Committed,
    Failed {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandedSession {
    pub id: u64,
    pub pid: u32,
    pub fd: bool,
    /// The program's end, if it ended (`Some(None)`: code unknown).
    pub exited: Option<Option<i32>>,
    pub size: TermSize,
    pub scrollback: usize,
    pub title: Option<String>,
    pub status: ActivityStatus,
    pub token: Option<String>,
    pub claude_session: Option<String>,
    /// Hook bodies that came while no one was attached.
    pub hooks: Vec<serde_bytes::ByteBuf>,
    /// Input not written to the program yet.
    #[serde(with = "serde_bytes")]
    pub writes: Vec<u8>,
    /// The program asked for color scheme reports.
    pub scheme_reports: bool,
    /// [`pack_snapshot`]'s output.
    #[serde(with = "serde_bytes")]
    pub snapshot: Vec<u8>,
    /// Kitty graphics images its programs sent (handoff 2).
    #[serde(default)]
    pub images: Vec<HandedImage>,
}

/// An image a session holds, as handed to the new daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandedImage {
    pub image: u32,
    /// The number (`I=`) it goes by.
    pub number: Option<u32>,
    pub width: u32,
    pub height: u32,
    /// Virtual placements: `(placement id, cols, rows)`.
    pub placements: Vec<(u32, u32, u32)>,
    /// [`pack_pixels`]'s output.
    #[serde(with = "serde_bytes")]
    pub pixels: Vec<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: u64,
    pub pid: u32,
    pub exited: bool,
    pub cwd: Option<PathBuf>,
    pub title: Option<String>,
    #[serde(default)]
    pub status: ActivityStatus,
    /// Some UI connection shows it.
    #[serde(default)]
    pub attached: bool,
    /// The first foreground program (for a label when there is no title).
    #[serde(default)]
    pub program: Option<String>,
    /// Spawned with a hook token (a Claude tab).
    #[serde(default)]
    pub claude: bool,
    /// `session_id` of its latest Claude hook event.
    #[serde(default)]
    pub claude_session: Option<String>,
}

/// An image as the client needs it to draw placeholder cells.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageData {
    pub image: u32,
    /// Changes when the pixels do.
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    /// Virtual placements: `(placement id, cols, rows)`.
    pub placements: Vec<(u32, u32, u32)>,
    /// RGBA pixels, LZ4-compressed ([`pack_pixels`]); `None` when only the
    /// placements changed.
    #[serde(with = "serde_bytes")]
    pub pixels: Option<Vec<u8>>,
}

pub fn pack_pixels(rgba: &[u8]) -> Vec<u8> {
    lz4_flex::compress_prepend_size(rgba)
}

pub fn unpack_pixels(bytes: &[u8]) -> io::Result<Vec<u8>> {
    lz4_flex::decompress_size_prepended(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// A snapshot as sent: positional msgpack (cells are most of it; field names
/// would double it), LZ4-compressed. Its layout changes only with `MAJOR`.
pub fn pack_snapshot(snap: &Snapshot) -> io::Result<Vec<u8>> {
    let raw = rmp_serde::to_vec(snap).map_err(io::Error::other)?;
    Ok(lz4_flex::compress_prepend_size(&raw))
}

pub fn unpack_snapshot(bytes: &[u8]) -> io::Result<Snapshot> {
    let raw = lz4_flex::decompress_size_prepended(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    rmp_serde::from_slice(&raw).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn encode(msg: &impl Serialize) -> io::Result<Vec<u8>> {
    // The length goes in front once the body is written after it.
    let mut out = vec![0u8; 4];
    rmp_serde::encode::write_named(&mut out, msg).map_err(io::Error::other)?;
    let len = u32::try_from(out.len() - 4).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame too large"))?;
    out[..4].copy_from_slice(&len.to_le_bytes());
    Ok(out)
}

pub fn write_frame(w: &mut impl Write, msg: &impl Serialize) -> io::Result<()> {
    w.write_all(&encode(msg)?)
}

/// Write `msg` with `fd` attached (SCM_RIGHTS) to its first bytes.
#[cfg(unix)]
pub fn write_frame_with_fd(sock: &UnixStream, msg: &impl Serialize, fd: &impl AsRawFd) -> io::Result<()> {
    let frame = encode(msg)?;
    let n = fdpass::send_with_fd(sock, &frame, fd.as_raw_fd())?;
    (&*sock).write_all(&frame[n..])
}

/// Write `msg` with `fds` attached (SCM_RIGHTS) to its first bytes.
#[cfg(unix)]
pub fn write_frame_with_fds(sock: &UnixStream, msg: &impl Serialize, fds: &[std::os::fd::RawFd]) -> io::Result<()> {
    let frame = encode(msg)?;
    let n = fdpass::send_with_fds(sock, &frame, fds)?;
    (&*sock).write_all(&frame[n..])
}

/// Decode one message; `Ok(None)` for a kind this build doesn't know.
fn decode<T: DeserializeOwned>(body: &[u8]) -> io::Result<Option<T>> {
    match rmp_serde::from_slice(body) {
        Ok(m) => Ok(Some(m)),
        Err(e) if e.to_string().contains("unknown variant") => {
            log::debug!("skipping unknown message: {e}");
            Ok(None)
        },
        Err(e) => Err(io::Error::new(io::ErrorKind::InvalidData, e)),
    }
}

/// How much one read of the socket takes at most.
const READ_CHUNK: usize = 64 << 10;

/// Reads frames, and the fds that come with them (not on Windows), from a
/// Unix socket.
pub struct FrameReader {
    sock: UnixStream,
    /// Bytes read; those before `start` are decoded already.
    buf: Vec<u8>,
    start: usize,
    #[cfg(unix)]
    fds: VecDeque<OwnedFd>,
    max: usize,
}

impl FrameReader {
    pub fn new(sock: UnixStream, max: usize) -> Self {
        Self {
            sock,
            buf: Vec::with_capacity(READ_CHUNK),
            start: 0,
            #[cfg(unix)]
            fds: VecDeque::new(),
            max,
        }
    }

    /// The socket it reads.
    pub fn get_ref(&self) -> &UnixStream {
        &self.sock
    }

    /// Whether a whole frame is in already: [`Self::read`] won't wait.
    pub fn has_frame(&self) -> bool {
        let pending = &self.buf[self.start..];
        pending.len() >= 4 && pending.len() >= 4 + u32::from_le_bytes(pending[..4].try_into().unwrap()) as usize
    }

    /// The next message; `Ok(None)` at end of stream. Unknown kinds are
    /// skipped.
    pub fn read<T: DeserializeOwned>(&mut self) -> io::Result<Option<T>> {
        loop {
            let pending = &self.buf[self.start..];
            if pending.len() >= 4 {
                let len = u32::from_le_bytes(pending[..4].try_into().unwrap()) as usize;
                if len > self.max {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, format!("frame of {len} bytes")));
                }
                if pending.len() >= 4 + len {
                    let msg = decode(&pending[4..4 + len]);
                    self.start += 4 + len;
                    match msg? {
                        Some(m) => return Ok(Some(m)),
                        None => continue,
                    }
                }
            }
            // Only the undecoded rest moves to the front, once per read.
            self.buf.drain(..self.start);
            self.start = 0;
            // A big frame (a snapshot, a paste) is in once: don't hold its
            // room for the life of the connection.
            if self.buf.capacity() > 16 * READ_CHUNK && self.buf.len() < READ_CHUNK {
                self.buf.shrink_to(2 * READ_CHUNK);
            }
            let filled = self.buf.len();
            self.buf.resize(filled + READ_CHUNK, 0);
            let read = {
                let chunk = &mut self.buf[filled..];
                #[cfg(unix)]
                let n = fdpass::recv_with_fds(&self.sock, chunk, &mut self.fds);
                #[cfg(windows)]
                let n = loop {
                    match (&self.sock).read(chunk) {
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        r => break r,
                    }
                };
                n
            };
            self.buf.truncate(filled + *read.as_ref().unwrap_or(&0));
            let n = read?;
            if n == 0 {
                return if self.buf.is_empty() { Ok(None) } else { Err(io::ErrorKind::UnexpectedEof.into()) };
            }
        }
    }

    /// The oldest fd received and not taken yet.
    #[cfg(unix)]
    pub fn take_fd(&mut self) -> Option<OwnedFd> {
        self.fds.pop_front()
    }
}

/// Reads plain frames from any reader (tests, and peers that pass no fds).
pub fn read_frame<T: DeserializeOwned>(r: &mut impl Read, max: usize) -> io::Result<Option<T>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > max {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("frame of {len} bytes")));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    decode(&body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::Cursor;

    const SIZE: TermSize = TermSize { cols: 80, rows: 24, cell_width: 8, cell_height: 16 };

    fn cfg() -> SpawnConfig {
        SpawnConfig {
            program: Some("/bin/sh".into()),
            args: vec!["-c".into(), "echo hi".into()],
            cwd: Some("/tmp".into()),
            env: HashMap::from([("A".into(), "b".into())]),
            size: SIZE,
            scrollback: 1000,
        }
    }

    #[test]
    fn client_messages_round_trip() {
        let msgs = vec![
            ClientMsg::Hello { major: MAJOR, minor: MINOR, app_version: "0.1.2".into(), role: Role::Ui },
            ClientMsg::Spawn { req: 1, cfg: cfg(), hook: true, seed: None },
            ClientMsg::Spawn { req: 5, cfg: cfg(), hook: false, seed: Some(vec![1, 2, 3]) },
            ClientMsg::HandOff { req: 6, exe: "/usr/bin/tern".into() },
            ClientMsg::Attach { req: 2, id: 7, size: SIZE },
            ClientMsg::Detach { id: 7 },
            ClientMsg::Input { id: 7, bytes: b"ls\r".to_vec() },
            ClientMsg::Resize { id: 7, size: SIZE },
            ClientMsg::Close { id: 7 },
            ClientMsg::List { req: 4 },
            ClientMsg::AskHandoff { id: 7 },
            ClientMsg::ResumeHandoff { id: 7, path: Some("docs/h.md".into()) },
        ];
        let mut buf = vec![];
        for m in &msgs {
            write_frame(&mut buf, m).unwrap();
        }
        let mut r = Cursor::new(buf);
        for m in &msgs {
            assert_eq!(&read_frame::<ClientMsg>(&mut r, MAX_CLIENT_FRAME).unwrap().unwrap(), m);
        }
    }

    #[test]
    fn server_messages_round_trip() {
        let mut buf = vec![];
        write_frame(&mut buf, &ServerMsg::Marker { id: 1, marker: Marker::Resized(SIZE) }).unwrap();
        write_frame(&mut buf, &ServerMsg::Output { id: 1, bytes: b"\x1b[1mhi".to_vec() }).unwrap();
        let mut r = Cursor::new(buf);
        assert!(
            matches!(read_frame(&mut r, MAX_SERVER_FRAME).unwrap(), Some(ServerMsg::Marker { id: 1, marker: Marker::Resized(s) }) if s == SIZE)
        );
        assert!(matches!(read_frame(&mut r, MAX_SERVER_FRAME).unwrap(), Some(ServerMsg::Output { bytes, .. }) if bytes == b"\x1b[1mhi"));
    }

    #[test]
    fn bytes_travel_as_bin() {
        // Box drawing: every byte is >= 0x80, which as an array of numbers
        // took two bytes each.
        let bytes = "─".repeat(1000).into_bytes();
        let frame = encode(&ServerMsg::Output { id: 1, bytes: bytes.clone() }).unwrap();
        assert!(frame.len() < bytes.len() + 64, "{} bytes for {}", frame.len(), bytes.len());
        let back = read_frame::<ServerMsg>(&mut Cursor::new(frame), MAX_SERVER_FRAME).unwrap();
        assert!(matches!(back, Some(ServerMsg::Output { bytes: b, .. }) if b == bytes));
    }

    #[test]
    fn many_frames_in_one_read() {
        let (a, b) = UnixStream::pair().unwrap();
        let mut all = vec![];
        for i in 0..500u64 {
            all.extend(encode(&ServerMsg::Output { id: i, bytes: vec![b'x'; (i % 300) as usize] }).unwrap());
        }
        // Then one frame split across writes.
        let big = encode(&ServerMsg::Output { id: 999, bytes: vec![7; 3 * READ_CHUNK] }).unwrap();
        let writer = std::thread::spawn(move || {
            (&a).write_all(&all).unwrap();
            for part in big.chunks(1000) {
                (&a).write_all(part).unwrap();
            }
        });
        let mut r = FrameReader::new(b, MAX_SERVER_FRAME);
        for i in 0..500u64 {
            let Some(ServerMsg::Output { id, bytes }) = r.read().unwrap() else { panic!("frame {i}") };
            assert_eq!((id, bytes.len()), (i, (i % 300) as usize));
        }
        let Some(ServerMsg::Output { id, bytes }) = r.read().unwrap() else { panic!("big frame") };
        assert_eq!((id, bytes.len()), (999, 3 * READ_CHUNK));
        writer.join().unwrap();
    }

    #[test]
    fn snapshot_packs_small() {
        use tern_term::vt::event::VoidListener;
        use tern_term::vt::grid::Dimensions;
        use tern_term::vt::term::{Config, Term};
        use tern_term::vt::vte::ansi::{Processor, StdSyncHandler};
        let mut t = Term::new(Config { scrolling_history: 2000, ..Config::default() }, &SIZE, VoidListener);
        let mut p = Processor::<StdSyncHandler>::new();
        for i in 0..2000 {
            p.advance(&mut t, format!("\x1b[3{}mline {i}\r\n", i % 8).as_bytes());
        }
        let snap = tern_term::snapshot::capture(&mut t, SIZE, Some("t".into()), Default::default());
        let packed = pack_snapshot(&snap).unwrap();
        assert!(packed.len() < 1 << 20, "{} bytes", packed.len());
        let back = unpack_snapshot(&packed).unwrap();
        assert_eq!(back.title.as_deref(), Some("t"));
        assert_eq!(back.grid.grid.history_size(), snap.grid.grid.history_size());
        assert!(unpack_snapshot(b"garbage").is_err());
    }

    #[test]
    fn oversize_frame_is_refused() {
        let mut buf = vec![];
        write_frame(&mut buf, &ClientMsg::Input { id: 1, bytes: vec![0; 2000] }).unwrap();
        let err = read_frame::<ClientMsg>(&mut Cursor::new(buf), 1000).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn unknown_kind_is_skipped() {
        #[derive(Serialize)]
        enum Future {
            Teleport { to: u32 },
        }
        let (a, b) = UnixStream::pair().unwrap();
        write_frame(&mut &a, &Future::Teleport { to: 1 }).unwrap();
        write_frame(&mut &a, &ClientMsg::Close { id: 9 }).unwrap();
        let mut r = FrameReader::new(b, MAX_CLIENT_FRAME);
        assert_eq!(r.read::<ClientMsg>().unwrap(), Some(ClientMsg::Close { id: 9 }));
        drop(a);
        assert_eq!(r.read::<ClientMsg>().unwrap(), None);
    }

    #[test]
    #[cfg(unix)]
    fn fd_travels_with_its_frame() {
        let (a, b) = UnixStream::pair().unwrap();
        let (pipe_r, pipe_w) = std::io::pipe().unwrap();
        write_frame(&mut &a, &ClientMsg::List { req: 1 }).unwrap();
        write_frame_with_fd(&a, &ClientMsg::Close { id: 2 }, &pipe_w).unwrap();
        drop(pipe_w);
        let mut r = FrameReader::new(b, MAX_CLIENT_FRAME);
        assert_eq!(r.read::<ClientMsg>().unwrap(), Some(ClientMsg::List { req: 1 }));
        assert_eq!(r.read::<ClientMsg>().unwrap(), Some(ClientMsg::Close { id: 2 }));
        let fd = r.take_fd().expect("fd");
        let mut w = std::fs::File::from(fd);
        w.write_all(b"through").unwrap();
        drop(w);
        let mut got = String::new();
        let mut pipe_r = pipe_r;
        pipe_r.read_to_string(&mut got).unwrap();
        assert_eq!(got, "through");
    }

    #[test]
    fn minor_1_and_2_messages_round_trip() {
        let msgs = vec![
            ClientMsg::Hello { major: MAJOR, minor: MINOR, app_version: "0.1.2".into(), role: Role::Observer },
            ClientMsg::Watch,
            ClientMsg::Reveal { req: 3, id: 9 },
            ClientMsg::View { req: 4, id: 9 },
            ClientMsg::Unview { id: 9 },
        ];
        let mut buf = vec![];
        for m in &msgs {
            write_frame(&mut buf, m).unwrap();
        }
        let mut r = Cursor::new(buf);
        for m in &msgs {
            assert_eq!(&read_frame::<ClientMsg>(&mut r, MAX_CLIENT_FRAME).unwrap().unwrap(), m);
        }
        let info = SessionInfo {
            id: 1,
            status: ActivityStatus::Blocked,
            attached: true,
            claude: true,
            claude_session: Some("s".into()),
            ..Default::default()
        };
        let mut buf = vec![];
        write_frame(&mut buf, &ServerMsg::SessionsChanged { list: vec![info.clone()] }).unwrap();
        write_frame(&mut buf, &ServerMsg::Revealed { req: 3, delivered: true }).unwrap();
        let mut r = Cursor::new(buf);
        assert!(matches!(read_frame(&mut r, MAX_SERVER_FRAME).unwrap(), Some(ServerMsg::SessionsChanged { list }) if list == vec![info]));
        assert!(matches!(read_frame(&mut r, MAX_SERVER_FRAME).unwrap(), Some(ServerMsg::Revealed { req: 3, delivered: true })));
    }

    /// What a MINOR 0 peer sent and parsed.
    mod v0 {
        use serde::{Deserialize, Serialize};
        use std::path::PathBuf;
        #[derive(Serialize)]
        pub enum ClientMsg {
            Hello { major: u32, minor: u32, app_version: String },
        }
        #[derive(Serialize, Deserialize, Debug)]
        pub struct SessionInfo {
            pub id: u64,
            pub pid: u32,
            pub exited: bool,
            pub cwd: Option<PathBuf>,
            pub title: Option<String>,
        }
        #[derive(Deserialize, Debug)]
        pub enum ServerMsg {
            Sessions { req: u64, list: Vec<SessionInfo> },
        }
        /// `ServerMsg::Sessions` as a MINOR 0 daemon encoded it.
        #[derive(Serialize)]
        pub enum ServerMsgOut {
            Sessions { req: u64, list: Vec<SessionInfo> },
        }
    }

    #[test]
    fn minor_0_peers_still_talk() {
        // An old UI's Hello reads as a UI.
        let mut buf = vec![];
        write_frame(&mut buf, &v0::ClientMsg::Hello { major: 1, minor: 0, app_version: "0.1.1".into() }).unwrap();
        let got = read_frame::<ClientMsg>(&mut Cursor::new(buf), MAX_CLIENT_FRAME).unwrap().unwrap();
        assert!(matches!(got, ClientMsg::Hello { role: Role::Ui, .. }));
        // An old daemon's list reads with defaults.
        let mut buf = vec![];
        let old = v0::SessionInfo { id: 4, pid: 5, exited: false, cwd: None, title: Some("t".into()) };
        write_frame(&mut buf, &v0::ServerMsgOut::Sessions { req: 1, list: vec![old] }).unwrap();
        let got = read_frame::<ServerMsg>(&mut Cursor::new(buf), MAX_SERVER_FRAME).unwrap().unwrap();
        let ServerMsg::Sessions { list, .. } = got else { panic!("{got:?}") };
        assert_eq!(list[0].id, 4);
        assert_eq!(list[0].status, ActivityStatus::None);
        assert!(!list[0].claude);
        // A new list reads on an old peer (extra fields ignored).
        let mut buf = vec![];
        write_frame(&mut buf, &ServerMsg::Sessions { req: 2, list: vec![SessionInfo { id: 6, claude: true, ..Default::default() }] })
            .unwrap();
        let got = read_frame::<v0::ServerMsg>(&mut Cursor::new(buf), MAX_SERVER_FRAME).unwrap().unwrap();
        let v0::ServerMsg::Sessions { list, .. } = got;
        assert_eq!(list[0].id, 6);
        // New kinds are skipped by an old peer.
        let (a, b) = UnixStream::pair().unwrap();
        write_frame(&mut &a, &ServerMsg::Reveal { id: 1 }).unwrap();
        write_frame(&mut &a, &ServerMsg::Sessions { req: 3, list: vec![] }).unwrap();
        let mut r = FrameReader::new(b, MAX_SERVER_FRAME);
        assert!(matches!(r.read::<v0::ServerMsg>().unwrap(), Some(v0::ServerMsg::Sessions { req: 3, .. })));
    }
}
