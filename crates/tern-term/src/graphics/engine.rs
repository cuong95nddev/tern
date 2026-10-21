//! The protocol engine: parses commands, loads and stores images, replies.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};

pub use super::load::Image;
pub use super::store::{ImageStore, StoredImage, VirtualPlacement};
use super::{load, reply};

use super::apc::{Apc, ApcParser};
use super::command::{self, Action, Command};

/// Most base64 bytes one transmission may carry, all chunks together.
const MAX_TRANSMISSION: usize = 64 * 1024 * 1024;
/// Most command bytes waiting for the worker; a flood beyond this is dropped.
const MAX_QUEUED: usize = 128 * 1024 * 1024;
/// Decoded pixels kept per session before the least recently used images go.
pub const STORE_CAP: usize = 256 * 1024 * 1024;

/// A protocol error, answered as `<code>:<msg>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub code: &'static str,
    pub msg: String,
}

impl Error {
    pub(crate) fn new(code: &'static str, msg: impl Into<String>) -> Self {
        Self { code, msg: msg.into() }
    }
}

/// Whether a command is shaped like one of our answers (`OK` or `CODE:message`,
/// `:` being outside base64): a tty echoing an answer back must not get one.
fn is_reply(payload: &[u8]) -> bool {
    payload == b"OK" || payload.contains(&b':')
}

/// Runs graphics commands on a thread of their own: reading a file and
/// decoding an image must not hold up the PTY reader, which may hold the
/// terminal lock the UI draws with. The thread starts with the first
/// command, so programs that never send one cost none.
pub struct GraphicsWorker {
    parser: ApcParser,
    tx: Option<mpsc::Sender<Apc>>,
    start: Option<Box<dyn FnOnce() -> Option<mpsc::Sender<Apc>> + Send>>,
    /// Command bytes sent and not yet handled.
    queued: Arc<AtomicUsize>,
}

impl GraphicsWorker {
    /// `reply` gets the bytes to write back to the program; `changed` is
    /// called after each command, to show what changed.
    pub fn new(store: Arc<Mutex<ImageStore>>, reply: impl Fn(Vec<u8>) + Send + 'static, changed: impl Fn() + Send + 'static) -> Self {
        let queued = Arc::new(AtomicUsize::new(0));
        let done = queued.clone();
        let start = move || {
            let (tx, rx) = mpsc::channel::<Apc>();
            let spawned = std::thread::Builder::new().name("tern-graphics".into()).spawn(move || {
                let mut graphics = Graphics::new(store);
                for apc in rx {
                    let size = apc_size(&apc);
                    let mut out = Vec::new();
                    graphics.handle(apc, &mut out);
                    done.fetch_sub(size, Ordering::AcqRel);
                    if !out.is_empty() {
                        reply(out);
                    }
                    changed();
                }
            });
            spawned.ok().map(|_| tx)
        };
        Self { parser: ApcParser::new(), tx: None, start: Some(Box::new(start)), queued }
    }

    /// Feeds raw program output; complete commands go to the worker.
    pub fn feed(&mut self, bytes: &[u8]) {
        let Self { parser, tx, start, queued } = self;
        parser.feed(bytes, |apc| {
            if let Some(start) = start.take() {
                *tx = start();
            }
            let Some(tx) = tx.as_ref() else { return };
            let size = apc_size(&apc);
            if queued.fetch_add(size, Ordering::AcqRel) + size > MAX_QUEUED {
                queued.fetch_sub(size, Ordering::AcqRel);
                return;
            }
            if tx.send(apc).is_err() {
                queued.fetch_sub(size, Ordering::AcqRel);
            }
        });
    }
}

fn apc_size(apc: &Apc) -> usize {
    match apc {
        Apc::Command { control, payload } => control.len() + payload.len(),
        Apc::TooLarge { control } => control.len(),
    }
}

/// A chunked transmission waiting for its last chunk (`m=0`).
struct Pending {
    cmd: Command,
    data: Vec<u8>,
    overflow: bool,
}

pub struct Graphics {
    parser: ApcParser,
    store: Arc<Mutex<ImageStore>>,
    pending: Option<Pending>,
}

impl Graphics {
    pub fn new(store: Arc<Mutex<ImageStore>>) -> Self {
        Self { parser: ApcParser::new(), store, pending: None }
    }

    /// Feeds raw program output; appends what must be written back to the
    /// program to `reply`.
    pub fn feed(&mut self, bytes: &[u8], reply: &mut Vec<u8>) {
        let mut found = Vec::new();
        self.parser.feed(bytes, |apc| found.push(apc));
        for apc in found {
            self.handle(apc, reply);
        }
    }

    fn handle(&mut self, apc: Apc, reply: &mut Vec<u8>) {
        let (control, payload) = match apc {
            Apc::TooLarge { control } => {
                self.pending = None;
                reply::error(&command::parse(&control).0, &Error::new("EFBIG", "command too large"), reply);
                return;
            },
            Apc::Command { control, payload } => (control, payload),
        };
        if let Some(mut p) = self.pending.take() {
            // A continuation chunk: only `m` matters.
            let more = command::parse(&control).0.more;
            if !p.overflow && p.data.len() + payload.len() > MAX_TRANSMISSION {
                p.overflow = true;
                p.data = Vec::new();
            }
            if !p.overflow {
                p.data.extend_from_slice(&payload);
            }
            if more {
                self.pending = Some(p);
            } else if p.overflow {
                reply::error(&p.cmd, &Error::new("EFBIG", "transmission too large"), reply);
            } else {
                self.execute(p.cmd, &p.data, reply);
            }
            return;
        }
        if is_reply(&payload) {
            return;
        }
        let (cmd, err) = command::parse(&control);
        if let Some(e) = err {
            reply::error(&cmd, &e, reply);
        } else if cmd.more && matches!(cmd.action, Action::Transmit | Action::TransmitAndPlace | Action::Query) {
            self.pending = Some(Pending { cmd, data: payload, overflow: false });
        } else {
            self.execute(cmd, &payload, reply);
        }
    }

    fn execute(&mut self, mut cmd: Command, payload: &[u8], reply: &mut Vec<u8>) {
        let result = match cmd.action {
            Action::Delete => {
                self.delete(&cmd);
                return;
            },
            Action::Query => load::load(&cmd, payload).map(drop),
            Action::Transmit | Action::TransmitAndPlace => self.transmit(&mut cmd, payload),
            Action::Place => self.place(&cmd),
            Action::Other(a) => Err(Error::new("EINVAL", format!("unsupported action {}", a as char))),
        };
        match result {
            Ok(()) => reply::ok(&cmd, reply),
            Err(e) => reply::error(&cmd, &e, reply),
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, ImageStore>, Error> {
        self.store.lock().map_err(|_| Error::new("EIO", "image store unavailable"))
    }

    fn transmit(&mut self, cmd: &mut Command, payload: &[u8]) -> Result<(), Error> {
        if cmd.action == Action::TransmitAndPlace && !cmd.unicode {
            return Err(Error::new("ENOTSUPPORTED", "only Unicode placeholder placements are supported"));
        }
        let image = load::load(cmd, payload)?;
        let mut store = self.lock()?;
        if cmd.id == 0 {
            cmd.id = store.allocate_id();
        }
        if cmd.number != 0 {
            store.set_number(cmd.number, cmd.id);
        }
        store.insert(cmd.id, image)?;
        if cmd.action == Action::TransmitAndPlace {
            store.place(cmd.id, VirtualPlacement { id: cmd.placement, cols: cmd.cols, rows: cmd.rows })?;
        }
        Ok(())
    }

    fn place(&mut self, cmd: &Command) -> Result<(), Error> {
        if !cmd.unicode {
            return Err(Error::new("ENOTSUPPORTED", "only Unicode placeholder placements are supported"));
        }
        let mut store = self.lock()?;
        let id = store.resolve(cmd.id, cmd.number).ok_or_else(|| Error::new("ENOENT", "no such image"))?;
        store.place(id, VirtualPlacement { id: cmd.placement, cols: cmd.cols, rows: cmd.rows })
    }

    fn delete(&mut self, cmd: &Command) {
        let Ok(mut store) = self.lock() else { return };
        match cmd.delete {
            0 | b'a' | b'A' => store.clear(),
            b'i' | b'I' | b'n' | b'N' => {
                if let Some(id) = store.resolve(cmd.id, cmd.number) {
                    store.delete(id);
                }
            },
            // Deleting by position or z-index means nothing without direct placements.
            _ => {},
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> (Graphics, Arc<Mutex<ImageStore>>) {
        let store = Arc::new(Mutex::new(ImageStore::new(STORE_CAP)));
        (Graphics::new(store.clone()), store)
    }

    fn run(g: &mut Graphics, bytes: &[u8]) -> Vec<u8> {
        let mut out = vec![];
        g.feed(bytes, &mut out);
        out
    }

    #[test]
    fn worker_runs_commands_off_the_reading_thread() {
        let store = Arc::new(Mutex::new(ImageStore::new(STORE_CAP)));
        let (tx, rx) = std::sync::mpsc::channel();
        let mut w = GraphicsWorker::new(
            store.clone(),
            move |r| {
                let _ = tx.send(r);
            },
            || {},
        );
        let held = store.lock().unwrap();
        // The worker can't store while the lock is held, yet feeding returns at once.
        w.feed(b"\x1b_Ga=t,i=3,f=24,s=1,v=1;AAAA\x1b\\");
        drop(held);
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(), b"\x1b_Gi=3;OK\x1b\\");
        assert!(store.lock().unwrap().get(3).is_some());
    }

    #[test]
    fn capability_probe_is_answered_and_not_stored() {
        let (mut g, store) = engine();
        assert_eq!(run(&mut g, b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\"), b"\x1b_Gi=31;OK\x1b\\");
        assert_eq!(store.lock().unwrap().bytes(), 0);
    }

    #[test]
    fn chunked_transmit_and_virtual_place() {
        let (mut g, store) = engine();
        // RGBA 2x1 = bytes 1..=8 = "AQIDBAUGBwg=", sent as two chunks.
        let out = run(&mut g, b"\x1b_Ga=T,U=1,i=5,f=32,s=2,v=1,c=3,r=2,q=1,m=1;AQID\x1b\\\x1b_Gm=0;BAUGBwg=\x1b\\");
        assert!(out.is_empty());
        let mut s = store.lock().unwrap();
        let img = s.get(5).unwrap();
        assert_eq!(&*img.image.rgba, &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(img.placement(0), Some(VirtualPlacement { id: 0, cols: 3, rows: 2 }));
    }

    #[test]
    fn direct_placement_is_refused() {
        let (mut g, store) = engine();
        assert!(run(&mut g, b"\x1b_Ga=T,i=6,f=24,s=1,v=1;AAAA\x1b\\").starts_with(b"\x1b_Gi=6;ENOTSUPPORTED:"));
        assert!(store.lock().unwrap().get(6).is_none());
    }

    #[test]
    fn delete_by_id() {
        let (mut g, store) = engine();
        run(&mut g, b"\x1b_Ga=t,i=7,f=24,s=1,v=1,q=2;AAAA\x1b\\");
        assert!(store.lock().unwrap().get(7).is_some());
        run(&mut g, b"\x1b_Ga=d,d=I,i=7,q=2\x1b\\");
        assert!(store.lock().unwrap().get(7).is_none());
    }

    #[test]
    fn number_gets_an_id() {
        let (mut g, _) = engine();
        let out = run(&mut g, b"\x1b_Ga=t,I=3,f=24,s=1,v=1;AAAA\x1b\\");
        assert!(out.starts_with(b"\x1b_Gi=") && out.ends_with(b",I=3;OK\x1b\\"), "{:?}", String::from_utf8_lossy(&out));
    }

    #[test]
    fn echoed_replies_are_ignored() {
        let (mut g, _) = engine();
        // A tty that echoes our answer back must not start a reply loop.
        assert!(run(&mut g, b"\x1b_Gi=31;OK\x1b\\").is_empty());
        assert!(run(&mut g, b"\x1b_Gi=4;EINVAL:bad value for f\x1b\\").is_empty());
    }

    #[test]
    fn errors_are_answered() {
        let (mut g, _) = engine();
        assert!(run(&mut g, b"\x1b_Gi=4,f=7;AAAA\x1b\\").starts_with(b"\x1b_Gi=4;EINVAL:"));
        assert!(run(&mut g, b"\x1b_Ga=p,U=1,i=99\x1b\\").starts_with(b"\x1b_Gi=99;ENOENT:"));
        assert!(run(&mut g, b"\x1b_Ga=f,i=8\x1b\\").starts_with(b"\x1b_Gi=8;EINVAL:"));
    }
}
