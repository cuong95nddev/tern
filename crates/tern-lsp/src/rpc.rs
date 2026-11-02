//! JSON-RPC over a server's stdio: `Content-Length` framing, a writer thread,
//! a reader thread that hands each response to the request waiting for it,
//! and the server's own requests and notifications to a handler.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
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

/// The file `command` runs on Windows: there `Command` finds only `.exe`
/// files on `PATH`, and npm's servers are `.cmd` (jdtls a `.bat`), so it is
/// looked up the way the shell does, directory by directory in `PATHEXT`
/// order. A command with an extension or a folder is taken as it is.
#[cfg_attr(not(windows), allow(dead_code))]
fn windows_program(command: &str, path: &OsStr, pathext: &str) -> Option<PathBuf> {
    if command.contains(['/', '\\']) || Path::new(command).extension().is_some() {
        return Some(PathBuf::from(command));
    }
    let exts: Vec<String> = pathext.split(';').filter(|e| !e.is_empty()).map(str::to_ascii_lowercase).collect();
    std::env::split_paths(path)
        .filter(|d| d.is_absolute())
        .flat_map(|d| exts.iter().map(move |e| d.join(format!("{command}{e}"))))
        .find(|p| p.is_file())
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
        #[cfg(windows)]
        let program = {
            let path = env.iter().find(|(k, _)| k.eq_ignore_ascii_case("PATH")).map(|(_, v)| v.into()).or_else(|| std::env::var_os("PATH"));
            let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
            // Rust runs a `.cmd` / `.bat` through cmd.exe, its arguments escaped.
            windows_program(program, &path.unwrap_or_default(), &pathext)
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, format!("{program} is not on PATH")))?
        };
        let mut cmd = Command::new(program);
        cmd.args(args).current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        cmd.envs(env.iter().map(|(k, v)| (k, v)));
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000); // CREATE_NO_WINDOW
        // A group of its own, so killing it takes what it started (a wrapper's
        // server, `cargo check`).
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
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
                log_lines(BufReader::new(stderr), |line| log::debug!("{name}: {line}"));
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
        // The reader may have exited (and failed what was pending) since the check above.
        if !self.alive.load(Ordering::SeqCst) {
            guard.done = true;
            return Err(LspError::Unavailable("the server has exited".into()));
        }
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let result = futures::select! {
            r = rx.fuse() => r.unwrap_or_else(|_| Err(LspError::Unavailable("the server has exited".into()))),
            _ = futures_timer::Delay::new(timeout).fuse() => Err(LspError::Timeout),
        };
        // Answered (or exited): nothing to cancel. A timeout cancels on drop.
        guard.done = !matches!(result, Err(LspError::Timeout));
        result
    }

    /// Kill the process and the ones it started, now.
    pub(crate) fn kill(&self) {
        let mut child = self.child.lock().unwrap();
        // Not reaped yet, so its id (its group's) isn't anyone else's.
        #[cfg(unix)]
        if let Ok(pid) = i32::try_from(child.id()) {
            // SAFETY: a plain signal to the group the server leads.
            unsafe { libc::kill(-pid, libc::SIGKILL) };
        }
        #[cfg(windows)]
        {
            let mut tree = Command::new("taskkill");
            tree.args(["/T", "/F", "/PID", &child.id().to_string()]).stdout(Stdio::null()).stderr(Stdio::null());
            std::os::windows::process::CommandExt::creation_flags(&mut tree, 0x0800_0000); // CREATE_NO_WINDOW
            let _ = tree.status();
        }
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

/// Each line of a server's stderr, as text even when it isn't UTF-8.
/// (A reader that stopped at a bad line would close the pipe, and the
/// server's next write there would kill it.)
fn log_lines(mut r: impl BufRead, mut log: impl FnMut(String)) {
    let mut line = vec![];
    while r.read_until(b'\n', &mut line).is_ok_and(|n| n > 0) {
        log(String::from_utf8_lossy(&line).trim_end().to_owned());
        line.clear();
    }
}

/// The largest message read from a server.
const MAX_MESSAGE: usize = 256 * 1024 * 1024;

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
        // A length no answer has: the stream is broken, not worth the memory.
        if len > MAX_MESSAGE {
            log::warn!("lsp: message of {len} bytes, giving up on the server");
            return None;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_lines_that_are_not_utf8_keep_the_pipe_read() {
        let mut lines = vec![];
        log_lines(&b"a\n\xff\xfe bad\nb\n"[..], |l| lines.push(l));
        assert_eq!(lines, ["a", "\u{fffd}\u{fffd} bad", "b"]);
    }

    #[test]
    fn huge_content_length_ends_the_stream() {
        let msg = format!("Content-Length: {}\r\n\r\n{{}}", usize::MAX / 2);
        assert_eq!(read_message(&mut msg.as_bytes()), None);
        assert_eq!(read_message(&mut &b"Content-Length: 2\r\n\r\n{}"[..]), Some(json!({})));
    }

    #[test]
    fn windows_finds_cmd_and_bat_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        // npm puts a shell script beside the .cmd: never the one to run.
        std::fs::write(a.join("vtsls"), "").unwrap();
        std::fs::write(b.join("vtsls.cmd"), "").unwrap();
        std::fs::write(b.join("jdtls.bat"), "").unwrap();
        std::fs::write(a.join("clangd.exe"), "").unwrap();
        let path = std::env::join_paths([&a, &b]).unwrap();
        let ext = ".COM;.EXE;.BAT;.CMD";
        assert_eq!(windows_program("vtsls", &path, ext), Some(b.join("vtsls.cmd")));
        assert_eq!(windows_program("jdtls", &path, ext), Some(b.join("jdtls.bat")));
        assert_eq!(windows_program("clangd", &path, ext), Some(a.join("clangd.exe")));
        assert_eq!(windows_program("gopls", &path, ext), None);
        assert_eq!(windows_program("C:\\bin\\x.exe", &path, ext), Some(PathBuf::from("C:\\bin\\x.exe")));
        assert_eq!(windows_program("x.cmd", &path, ext), Some(PathBuf::from("x.cmd")));
    }
}
