//! JSON-RPC over a server's stdio: `Content-Length` framing, a writer thread,
//! a reader thread that hands each response to the request waiting for it,
//! and the server's own requests and notifications to a handler.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use futures::FutureExt as _;
use futures::channel::oneshot;
use serde_json::{Value, json};

use crate::LspError;

/// A message the server sent that isn't a response.
pub(crate) enum Incoming {
    Notification { method: String, params: Value },
    Request { id: Value, method: String, params: Value },
}

type Handler = Box<dyn Fn(&Connection, Incoming) + Send + Sync>;
type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, LspError>>>>>;

/// A running server process and the requests waiting on it.
pub(crate) struct Connection {
    next_id: AtomicI64,
    out: flume::Sender<Vec<u8>>,
    pending: Pending,
    alive: Arc<AtomicBool>,
    child: Mutex<Child>,
}

impl Connection {
    /// Start `program` in `cwd`. `handler` gets what the server sends that
    /// isn't a response, on the reader thread.
    pub(crate) fn spawn(
        program: &str,
        args: &[String],
        env: &[(String, String)],
        cwd: &Path,
        name: &str,
        handler: Handler,
    ) -> std::io::Result<Arc<Connection>> {
        let mut cmd = Command::new(program);
        cmd.args(args).current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        cmd.envs(env.iter().map(|(k, v)| (k, v)));
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000); // CREATE_NO_WINDOW
        let mut child = cmd.spawn()?;
        let (stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
        let (out, outgoing) = flume::unbounded::<Vec<u8>>();
        let conn = Arc::new(Connection {
            next_id: AtomicI64::new(1),
            out,
            pending: Pending::default(),
            alive: Arc::new(AtomicBool::new(true)),
            child: Mutex::new(child),
        });
        if let Some(mut stdin) = stdin {
            std::thread::Builder::new().name(format!("lsp-write-{name}")).spawn(move || {
                for msg in outgoing.iter() {
                    let head = format!("Content-Length: {}\r\n\r\n", msg.len());
                    if stdin.write_all(head.as_bytes()).and_then(|_| stdin.write_all(&msg)).and_then(|_| stdin.flush()).is_err() {
                        break;
                    }
                }
            })?;
        }
        if let Some(stderr) = stderr {
            let name = name.to_owned();
            std::thread::Builder::new().name(format!("lsp-err-{name}")).spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    log::debug!("{name}: {line}");
                }
            })?;
        }
        if let Some(stdout) = stdout {
            let weak = Arc::downgrade(&conn);
            let (pending, alive) = (conn.pending.clone(), conn.alive.clone());
            std::thread::Builder::new()
                .name(format!("lsp-read-{name}"))
                .spawn(move || read_loop(BufReader::new(stdout), weak, &pending, &alive, handler))?;
        }
        Ok(conn)
    }

    pub(crate) fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst) && self.child.lock().unwrap().try_wait().ok().flatten().is_none()
    }

    fn send(&self, msg: &Value) {
        let _ = self.out.send(msg.to_string().into_bytes());
    }

    pub(crate) fn notify(&self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    pub(crate) fn respond(&self, id: Value, result: Result<Value, (i64, String)>) {
        match result {
            Ok(result) => self.send(&json!({"jsonrpc": "2.0", "id": id, "result": result})),
            Err((code, message)) => self.send(&json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})),
        }
    }

    /// Ask `method`; `Timeout` after `timeout`. Dropping the future before
    /// the answer cancels the request (`$/cancelRequest`).
    pub(crate) async fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, LspError> {
        if !self.alive.load(Ordering::SeqCst) {
            return Err(LspError::Unavailable("the server has exited".into()));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let mut guard = CancelGuard { conn: self, id, done: false };
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let result = futures::select! {
            r = rx.fuse() => r.unwrap_or_else(|_| Err(LspError::Unavailable("the server has exited".into()))),
            _ = futures_timer::Delay::new(timeout).fuse() => Err(LspError::Timeout),
        };
        // Answered (or exited): nothing to cancel. A timeout cancels on drop.
        guard.done = !matches!(result, Err(LspError::Timeout));
        result
    }

    /// Kill the process now.
    pub(crate) fn kill(&self) {
        let mut child = self.child.lock().unwrap();
        let _ = child.kill();
        let _ = child.wait();
    }

    /// Wait up to `timeout` for the process to end on its own.
    pub(crate) fn wait_exit(&self, timeout: Duration) -> bool {
        let start = std::time::Instant::now();
        loop {
            if self.child.lock().unwrap().try_wait().ok().flatten().is_some() {
                return true;
            }
            if start.elapsed() >= timeout {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// Forgets a request whose future went away, and tells the server.
struct CancelGuard<'a> {
    conn: &'a Connection,
    id: i64,
    done: bool,
}

impl Drop for CancelGuard<'_> {
    fn drop(&mut self) {
        self.conn.pending.lock().unwrap().remove(&self.id);
        if !self.done && self.conn.alive.load(Ordering::SeqCst) {
            self.conn.notify("$/cancelRequest", json!({"id": self.id}));
        }
    }
}

fn read_message(r: &mut impl BufRead) -> Option<Value> {
    loop {
        let mut len = None;
        loop {
            let mut line = String::new();
            if r.read_line(&mut line).ok()? == 0 {
                return None;
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':')
                && k.eq_ignore_ascii_case("content-length")
            {
                len = v.trim().parse::<usize>().ok();
            }
        }
        let Some(len) = len else { continue };
        let mut buf = vec![0; len];
        r.read_exact(&mut buf).ok()?;
        match serde_json::from_slice(&buf) {
            Ok(v) => return Some(v),
            Err(e) => log::warn!("lsp: unreadable message: {e}"),
        }
    }
}

fn read_loop(mut r: impl BufRead, conn: Weak<Connection>, pending: &Pending, alive: &AtomicBool, handler: Handler) {
    while let Some(msg) = read_message(&mut r) {
        let method = msg.get("method").and_then(Value::as_str).map(str::to_owned);
        let id = msg.get("id").cloned();
        match (method, id) {
            (None, Some(id)) => {
                let Some(id) = id.as_i64() else { continue };
                let result = match msg.get("error") {
                    Some(e) => Err(LspError::Server {
                        code: e.get("code").and_then(Value::as_i64).unwrap_or(0),
                        message: e.get("message").and_then(Value::as_str).unwrap_or("").to_owned(),
                    }),
                    None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                };
                if let Some(tx) = pending.lock().unwrap().remove(&id) {
                    let _ = tx.send(result);
                }
            },
            (Some(method), id) => {
                let Some(conn) = conn.upgrade() else { break };
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                let incoming = match id {
                    Some(id) => Incoming::Request { id, method, params },
                    None => Incoming::Notification { method, params },
                };
                handler(&conn, incoming);
            },
            (None, None) => {},
        }
    }
    alive.store(false, Ordering::SeqCst);
    for (_, tx) in pending.lock().unwrap().drain() {
        let _ = tx.send(Err(LspError::Unavailable("the server has exited".into())));
    }
}
