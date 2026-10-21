//! A remote viewer's side (the web server): an observer connection that
//! follows the session list and views sessions beside the UI that owns them.
//! It types into them and resizes them, which takes them from the UI until
//! it takes them back (see `ClientMsg::Drive`), but never attaches to them,
//! never starts the daemon and doesn't keep it alive.

use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tern_term::TermSize;
use tern_term::mirror::Marker;
use tern_term::snapshot::Snapshot;

use crate::UnixStream;
use crate::paths;
use crate::proto::{self, ClientMsg, Controller, FrameReader, MAJOR, MINOR, Role, ServerMsg, SessionInfo};

const HELLO_TIMEOUT: Duration = Duration::from_secs(3);

/// What the daemon sent a viewer.
#[derive(Debug)]
pub enum ViewEvent {
    /// The session list (now, and whenever it changes).
    Sessions(Vec<SessionInfo>),
    /// A view began: the state the output that follows starts from.
    Viewing {
        id: u64,
        snapshot: Box<Snapshot>,
    },
    Output {
        id: u64,
        bytes: Vec<u8>,
    },
    Marker {
        id: u64,
        marker: Marker,
    },
    /// The view's output fell behind and was dropped: view it again.
    Lagged {
        id: u64,
    },
    /// No such session.
    NotFound {
        id: u64,
    },
    /// Who has the session viewed: after `Viewing`, and when it changes.
    Control {
        id: u64,
        by: Controller,
    },
}

/// Requests to the daemon; clones share the connection.
#[derive(Clone)]
pub struct Viewer {
    writer: Arc<Mutex<UnixStream>>,
    next_req: Arc<AtomicU64>,
}

/// What the daemon sends, until the connection ends.
pub struct ViewStream {
    reader: FrameReader,
}

impl Viewer {
    /// Connect to the daemon in `dir` (never starts it). Fails with
    /// `Unsupported` for a daemon older than viewing (MINOR 2).
    pub fn connect(dir: &Path) -> io::Result<(Viewer, ViewStream)> {
        let stream = UnixStream::connect(paths::socket_path(dir))?;
        if !paths::peer_uid_ok(&stream) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "daemon runs as another user"));
        }
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        stream.set_read_timeout(Some(HELLO_TIMEOUT))?;
        let mut writer = stream.try_clone()?;
        proto::write_frame(
            &mut writer,
            &ClientMsg::Hello { major: MAJOR, minor: MINOR, app_version: env!("CARGO_PKG_VERSION").into(), role: Role::Observer },
        )?;
        let mut reader = FrameReader::new(stream, proto::MAX_SERVER_FRAME);
        match reader.read::<ServerMsg>()? {
            Some(ServerMsg::Hello { major, minor, .. }) if major == MAJOR && minor >= 2 => {},
            Some(ServerMsg::Hello { major, minor, .. }) if major == MAJOR => {
                return Err(io::Error::new(io::ErrorKind::Unsupported, format!("daemon protocol {major}.{minor} can't be viewed")));
            },
            other => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("bad hello: {other:?}"))),
        }
        reader.get_ref().set_read_timeout(None)?;
        let viewer = Viewer { writer: Arc::new(Mutex::new(writer)), next_req: Arc::new(AtomicU64::new(1)) };
        viewer.send(&ClientMsg::Watch)?;
        Ok((viewer, ViewStream { reader }))
    }

    fn send(&self, msg: &ClientMsg) -> io::Result<()> {
        let frame = proto::encode(msg)?;
        self.writer.lock().unwrap().write_all(&frame)
    }

    /// Start viewing session `id`; answered by `Viewing` or `NotFound`.
    pub fn view(&self, id: u64) -> io::Result<()> {
        let req = self.next_req.fetch_add(1, Ordering::Relaxed);
        self.send(&ClientMsg::View { req, id })
    }

    pub fn unview(&self, id: u64) -> io::Result<()> {
        self.send(&ClientMsg::Unview { id })
    }

    pub fn input(&self, id: u64, bytes: Vec<u8>) -> io::Result<()> {
        self.send(&ClientMsg::Input { id, bytes })
    }

    /// Resize the session's PTY (the owning UI follows).
    pub fn resize(&self, id: u64, size: TermSize) -> io::Result<()> {
        self.send(&ClientMsg::Resize { id, size })
    }

    /// Take session `id`: the PTY takes this viewer's size, and the owner's
    /// typing is dropped until it takes it back.
    pub fn drive(&self, id: u64) -> io::Result<()> {
        self.send(&ClientMsg::Drive { id })
    }

    /// End the connection; the stream's `next` returns `None`.
    pub fn shutdown(&self) {
        let _ = self.writer.lock().unwrap().shutdown(std::net::Shutdown::Both);
    }
}

impl ViewStream {
    /// The next event; `None` once the connection is gone.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<ViewEvent> {
        loop {
            let msg = match self.reader.read::<ServerMsg>() {
                Ok(Some(m)) => m,
                Ok(None) => return None,
                Err(e) => {
                    log::info!("viewer: {e}");
                    return None;
                },
            };
            return Some(match msg {
                ServerMsg::SessionsChanged { list } | ServerMsg::Sessions { list, .. } => ViewEvent::Sessions(list),
                ServerMsg::Attached { id, snapshot, .. } => match proto::unpack_snapshot(&snapshot) {
                    Ok(snapshot) => ViewEvent::Viewing { id, snapshot: Box::new(snapshot) },
                    Err(e) => {
                        log::warn!("viewer: snapshot of {id}: {e}");
                        ViewEvent::Lagged { id }
                    },
                },
                ServerMsg::Output { id, bytes } => ViewEvent::Output { id, bytes },
                ServerMsg::Marker { id, marker } => ViewEvent::Marker { id, marker },
                ServerMsg::Detached { id } => ViewEvent::Lagged { id },
                ServerMsg::NotFound { id, .. } => ViewEvent::NotFound { id },
                ServerMsg::Control { id, by } => ViewEvent::Control { id, by },
                _ => continue,
            });
        }
    }
}
