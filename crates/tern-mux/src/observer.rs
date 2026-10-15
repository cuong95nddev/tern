//! The menu bar agent's side: an observer connection that follows the
//! daemon's session list and asks it to show or end a session. It never
//! starts the daemon and doesn't keep it alive.

use std::collections::HashMap;
use std::io::{self, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use crate::paths;
use crate::proto::{self, ClientMsg, FrameReader, MAJOR, MINOR, Role, ServerMsg, SessionInfo};

/// How often a MINOR 0 daemon (no `Watch`) is asked for its list.
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);
const REVEAL_TIMEOUT: Duration = Duration::from_secs(2);
const HELLO_TIMEOUT: Duration = Duration::from_secs(3);

struct Inner {
    writer: Mutex<UnixStream>,
    alive: AtomicBool,
    /// The daemon's protocol minor: 0 has no `Watch`/`Reveal`.
    minor: u32,
    next_req: AtomicU64,
    reveals: Mutex<HashMap<u64, mpsc::Sender<bool>>>,
}

impl Inner {
    fn send(&self, msg: &ClientMsg) -> io::Result<()> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(io::ErrorKind::NotConnected.into());
        }
        let frame = proto::encode(msg)?;
        self.writer.lock().unwrap().write_all(&frame)
    }
}

/// Requests to the daemon; clones share the connection.
#[derive(Clone)]
pub struct Watcher {
    inner: Arc<Inner>,
}

/// The session lists the daemon sends, until the connection ends.
pub struct WatchStream {
    inner: Arc<Inner>,
    reader: FrameReader,
}

impl Watcher {
    /// Connect to the daemon in `dir` as an observer (never starts it).
    pub fn connect(dir: &Path) -> io::Result<(Watcher, WatchStream)> {
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
        let minor = match reader.read::<ServerMsg>()? {
            Some(ServerMsg::Hello { major, minor, .. }) if major == MAJOR => minor,
            other => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("bad hello: {other:?}"))),
        };
        // Lists may be far apart: no read timeout from here on.
        writer.set_read_timeout(None)?;
        let inner = Arc::new(Inner {
            writer: Mutex::new(writer),
            alive: AtomicBool::new(true),
            minor,
            next_req: AtomicU64::new(1),
            reveals: Mutex::default(),
        });
        if minor >= 1 {
            inner.send(&ClientMsg::Watch)?;
        } else {
            let weak = Arc::downgrade(&inner);
            std::thread::Builder::new().name("watch poll".into()).spawn(move || {
                while let Some(i) = weak.upgrade() {
                    if i.send(&ClientMsg::List { req: 0 }).is_err() {
                        return;
                    }
                    drop(i);
                    std::thread::sleep(POLL_INTERVAL);
                }
            })?;
        }
        Ok((Watcher { inner: inner.clone() }, WatchStream { inner, reader }))
    }

    /// Ask the UI to show session `id`. False when no UI got it (none
    /// connected, an old daemon, the connection lost, no answer in 2 s).
    pub fn reveal(&self, id: u64) -> bool {
        if self.inner.minor < 1 {
            return false;
        }
        let req = self.inner.next_req.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.inner.reveals.lock().unwrap().insert(req, tx);
        let ok = self.inner.send(&ClientMsg::Reveal { req, id }).is_ok() && rx.recv_timeout(REVEAL_TIMEOUT).unwrap_or(false);
        self.inner.reveals.lock().unwrap().remove(&req);
        ok
    }

    /// End session `id`.
    pub fn close(&self, id: u64) {
        let _ = self.inner.send(&ClientMsg::Close { id });
    }
}

impl WatchStream {
    /// The next list; `None` once the connection is gone.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<Vec<SessionInfo>> {
        loop {
            match self.reader.read::<ServerMsg>() {
                Ok(Some(ServerMsg::SessionsChanged { list } | ServerMsg::Sessions { list, .. })) => return Some(list),
                Ok(Some(ServerMsg::Revealed { req, delivered })) => {
                    if let Some(tx) = self.inner.reveals.lock().unwrap().remove(&req) {
                        let _ = tx.send(delivered);
                    }
                },
                Ok(Some(_)) => {},
                Ok(None) | Err(_) => {
                    self.inner.alive.store(false, Ordering::Release);
                    // Waiting reveals get `false` now rather than at their timeout.
                    self.inner.reveals.lock().unwrap().clear();
                    return None;
                },
            }
        }
    }
}

impl Drop for WatchStream {
    fn drop(&mut self) {
        self.inner.alive.store(false, Ordering::Release);
        let _ = self.inner.writer.lock().unwrap().shutdown(std::net::Shutdown::Both);
    }
}
