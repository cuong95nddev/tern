//! Messages between the UI and the daemon, and their framing.
//!
//! A frame is a little-endian `u32` length followed by a msgpack message
//! (struct fields by name, so fields can be added). The first message each
//! way is `Hello`; a client says whether it is a UI or an observer (the menu
//! bar agent). A message of a kind the receiver doesn't know is skipped,
//! so a minor version can add kinds; the major is part of the socket name,
//! so incompatible versions never talk.

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

pub const MAJOR: u32 = 1;
pub const MINOR: u32 = 1;

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
    /// a Claude settings file pointing at its hook server.
    Spawn {
        req: u64,
        cfg: SpawnConfig,
        hook: bool,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMsg {
    Hello {
        major: u32,
        minor: u32,
        app_version: String,
        pid: u32,
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
        bytes: Vec<u8>,
    },
    Marker {
        id: u64,
        marker: Marker,
    },
    /// A Claude Code hook request for `token` (JSON body).
    Hook {
        token: String,
        body: Vec<u8>,
    },
    /// Another connection attached to `id`.
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
    let body = rmp_serde::to_vec_named(msg).map_err(io::Error::other)?;
    let len = u32::try_from(body.len()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame too large"))?;
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&body);
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

/// Reads frames, and the fds that come with them (not on Windows), from a
/// Unix socket.
pub struct FrameReader {
    sock: UnixStream,
    buf: Vec<u8>,
    #[cfg(unix)]
    fds: VecDeque<OwnedFd>,
    max: usize,
}

impl FrameReader {
    pub fn new(sock: UnixStream, max: usize) -> Self {
        Self {
            sock,
            buf: Vec::with_capacity(64 << 10),
            #[cfg(unix)]
            fds: VecDeque::new(),
            max,
        }
    }

    /// The next message; `Ok(None)` at end of stream. Unknown kinds are
    /// skipped.
    pub fn read<T: DeserializeOwned>(&mut self) -> io::Result<Option<T>> {
        loop {
            if self.buf.len() >= 4 {
                let len = u32::from_le_bytes(self.buf[..4].try_into().unwrap()) as usize;
                if len > self.max {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, format!("frame of {len} bytes")));
                }
                if self.buf.len() >= 4 + len {
                    let msg = decode(&self.buf[4..4 + len]);
                    self.buf.drain(..4 + len);
                    match msg? {
                        Some(m) => return Ok(Some(m)),
                        None => continue,
                    }
                }
            }
            let mut chunk = [0u8; 64 << 10];
            #[cfg(unix)]
            let n = fdpass::recv_with_fds(&self.sock, &mut chunk, &mut self.fds)?;
            #[cfg(windows)]
            let n = loop {
                match (&self.sock).read(&mut chunk) {
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    r => break r?,
                }
            };
            if n == 0 {
                return if self.buf.is_empty() { Ok(None) } else { Err(io::ErrorKind::UnexpectedEof.into()) };
            }
            self.buf.extend_from_slice(&chunk[..n]);
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
            ClientMsg::Spawn { req: 1, cfg: cfg(), hook: true },
            ClientMsg::Attach { req: 2, id: 7, size: SIZE },
            ClientMsg::Detach { id: 7 },
            ClientMsg::Input { id: 7, bytes: b"ls\r".to_vec() },
            ClientMsg::Resize { id: 7, size: SIZE },
            ClientMsg::Close { id: 7 },
            ClientMsg::List { req: 4 },
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
    fn minor_1_messages_round_trip() {
        let msgs = vec![
            ClientMsg::Hello { major: MAJOR, minor: MINOR, app_version: "0.1.2".into(), role: Role::Observer },
            ClientMsg::Watch,
            ClientMsg::Reveal { req: 3, id: 9 },
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
