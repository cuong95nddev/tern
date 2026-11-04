//! Chats in the daemon: Claude Code run headless, its stdin and stdout JSON
//! lines, kept running while Tern is closed like a terminal is.
//!
//! The daemon doesn't read the conversation; it keeps every line the
//! program wrote since it started (a turn's streamed deltas are dropped once
//! the turn ends: its whole messages follow them), plus the client lines
//! that answered its requests, so a UI that attaches again replays them
//! after the history Claude saved before the program started. While no UI
//! is connected, a permission request or a finished turn posts a
//! notification.
//!
//! On Unix a chat's pipes go to a newer daemon with the rest (see
//! `server::hand_off`): its reader parks, the fds and the unended line move.

use std::fs::File;
use std::io::{self, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
#[cfg(unix)]
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::Value;
use tern_term::status::ActivityStatus;

use crate::proto::{ChatExit, ChatSpec};

/// Where a chat's lines go: the connection attached to it.
pub(crate) trait ChatSink: Send + Sync {
    fn chat_line(&self, id: u64, line: &str);
    fn chat_exited(&self, id: u64, exit: &ChatExit);
}

/// Posts a notification (title, body) when no UI is connected.
pub(crate) type Notify = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// How much of stderr is kept.
const STDERR_TAIL: usize = 4096;

/// A line kept for replay.
struct Kept {
    text: String,
    /// A streamed delta, dropped when its turn ends.
    stream: bool,
}

#[derive(Default)]
struct State {
    lines: Vec<Kept>,
    owner: Option<Weak<dyn ChatSink>>,
    exited: Option<ChatExit>,
    /// The first message the user sent: what a notification is titled.
    title: Option<String>,
    stderr: String,
    /// A turn runs: from a user message to its `result`.
    busy: bool,
    /// Permission requests not answered yet, by request id.
    asks: std::collections::HashSet<String>,
    /// MCP forms not settled yet, by request id: their lines are recorded.
    forms: std::collections::HashSet<String>,
}

impl State {
    /// `v` is a line of an MCP form: the request, the UI's answer to it,
    /// or Claude taking it back.
    fn form_line(&mut self, v: &Value) -> bool {
        let id = |p: &str| v.pointer(p).and_then(Value::as_str).map(str::to_owned);
        match v.get("type").and_then(Value::as_str) {
            Some("control_request") if v.pointer("/request/subtype").and_then(Value::as_str) == Some("elicitation") => {
                id("/request_id").map(|i| self.forms.insert(i)).is_some()
            },
            Some("control_response") => id("/response/request_id").is_some_and(|i| self.forms.remove(&i)),
            Some("control_cancel_request") => id("/request_id").is_some_and(|i| self.forms.remove(&i)),
            _ => false,
        }
    }

    /// What `v`, a line read from the program or written to it, says of
    /// the turn and the requests waiting.
    fn follow(&mut self, v: &Value) {
        let s = |p: &str| v.pointer(p).and_then(Value::as_str).unwrap_or_default().to_owned();
        match v.get("type").and_then(Value::as_str).unwrap_or_default() {
            "user" => self.busy = true,
            "result" => {
                self.busy = false;
                self.asks.clear();
            },
            // A permission, a question, or an MCP server's form: the user's turn.
            "control_request" if matches!(s("/request/subtype").as_str(), "can_use_tool" | "elicitation") => {
                self.asks.insert(s("/request_id"));
            },
            "control_cancel_request" => {
                self.asks.remove(&s("/request_id"));
            },
            "control_response" => {
                self.asks.remove(&s("/response/request_id"));
            },
            _ => {},
        }
    }
}

/// Whose chat it is: what a UI that finds it without a tab opens one with.
#[derive(Debug, Clone, Default)]
pub(crate) struct ChatMeta {
    pub cwd: std::path::PathBuf,
    pub session: String,
    pub account: String,
    /// Where MCP forms' lines are kept (see [`ChatSpec::record`]).
    pub record: Option<std::path::PathBuf>,
}

pub(crate) struct ChatHost {
    pub id: u64,
    pub pid: u32,
    /// When the program started, in ms since the Unix epoch.
    pub started: u64,
    pub meta: ChatMeta,
    stdin: Mutex<Option<File>>,
    state: Mutex<State>,
    /// The program, when this daemon started it (to learn its exit status).
    child: Mutex<Option<Child>>,
    /// The reader, to park for a handoff (Unix: Windows has no handoff,
    /// its reader threads end with the program's pipes).
    #[cfg(unix)]
    reader: Mutex<Option<Reader>>,
    notify: Notify,
    /// Its tab muted its notifications (not handed off: the tab says it
    /// again when it attaches).
    muted: AtomicBool,
}

/// The thread reading the program's stdout and stderr.
#[cfg(unix)]
struct Reader {
    /// Written to park it.
    wake: File,
    thread: JoinHandle<Option<Parked>>,
}

/// A reader stopped for a handoff: the pipes and the unended line.
pub(crate) struct Parked {
    pub stdout: File,
    pub stderr: File,
    pub partial: Vec<u8>,
}

/// What a handoff carries of a chat.
#[cfg(unix)]
pub(crate) struct Handed {
    pub meta: ChatMeta,
    pub title: Option<String>,
    pub lines: Vec<String>,
    pub stderr: String,
    pub exited: Option<ChatExit>,
    /// stdin, and the parked reader, while it runs.
    pub io: Option<(File, Parked)>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

impl ChatHost {
    pub fn spawn(id: u64, spec: &ChatSpec, notify: Notify) -> io::Result<Arc<Self>> {
        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.args).current_dir(&spec.cwd).envs(&spec.env).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        // Set for a Claude run inside another one; this one is its own.
        for k in ["CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_SESSION_ID", "CLAUDE_CODE_CHILD_SESSION"] {
            cmd.env_remove(k);
        }
        cmd.env("TERN_SESSION_ID", id.to_string());
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().map(to_file);
        let stdout = to_file(child.stdout.take().expect("piped"));
        let stderr = to_file(child.stderr.take().expect("piped"));
        let host = Arc::new(Self {
            id,
            pid: child.id(),
            started: now_ms(),
            meta: ChatMeta {
                cwd: spec.cwd.clone(),
                session: spec.session.clone(),
                account: spec.account.clone(),
                record: spec.record.clone(),
            },
            stdin: Mutex::new(stdin),
            state: Mutex::new(State::default()),
            child: Mutex::new(Some(child)),
            #[cfg(unix)]
            reader: Mutex::new(None),
            notify,
            muted: AtomicBool::new(false),
        });
        host.read(Parked { stdout, stderr, partial: Vec::new() })?;
        Ok(host)
    }

    /// A chat a previous daemon handed over; its reader starts with
    /// [`Self::resume`] once the handoff is committed.
    #[cfg(unix)]
    pub fn adopt(id: u64, pid: u32, started: u64, handed: Handed, notify: Notify) -> (Arc<Self>, Option<Parked>) {
        let (stdin, parked) = match handed.io {
            Some((stdin, parked)) => (Some(stdin), Some(parked)),
            None => (None, None),
        };
        let lines: Vec<Kept> = handed.lines.into_iter().map(|text| Kept { text, stream: false }).collect();
        let mut state = State { owner: None, exited: handed.exited, title: handed.title, stderr: handed.stderr, ..Default::default() };
        for k in &lines {
            if let Ok(v) = serde_json::from_str::<Value>(&k.text) {
                state.follow(&v);
                // Recorded by the daemon before.
                state.form_line(&v);
            }
        }
        state.lines = lines;
        let host = Arc::new(Self {
            id,
            pid,
            started,
            meta: handed.meta,
            stdin: Mutex::new(stdin),
            state: Mutex::new(state),
            child: Mutex::new(None),
            #[cfg(unix)]
            reader: Mutex::new(None),
            notify,
            muted: AtomicBool::new(false),
        });
        (host, parked)
    }

    /// Read the program's output again from `parked`.
    #[cfg(unix)]
    pub fn resume(self: &Arc<Self>, parked: Parked) {
        if let Err(e) = self.read(parked) {
            log::warn!("chat {}: can't read: {e}", self.id);
        }
    }

    pub fn exited(&self) -> Option<ChatExit> {
        self.state.lock().unwrap().exited.clone()
    }

    /// Attach `owner` (the one before lets go); `send` gets what to replay,
    /// under the lock, so no line comes before it or goes missing.
    pub fn attach(&self, owner: &Arc<dyn ChatSink>, send: impl FnOnce(Vec<String>, Option<ChatExit>)) {
        let mut st = self.state.lock().unwrap();
        st.owner = Some(Arc::downgrade(owner));
        send(st.lines.iter().map(|k| k.text.clone()).collect(), st.exited.clone());
    }

    /// `owner` lets go, if it is the one attached.
    /// Whether it waits for the user, runs a turn, or neither (or ended).
    pub fn status(&self) -> ActivityStatus {
        let st = self.state.lock().unwrap();
        match () {
            _ if st.exited.is_some() => ActivityStatus::None,
            _ if !st.asks.is_empty() => ActivityStatus::Blocked,
            _ if st.busy => ActivityStatus::Working,
            _ => ActivityStatus::None,
        }
    }

    /// The first message the user sent.
    pub fn title(&self) -> Option<String> {
        self.state.lock().unwrap().title.clone()
    }

    /// A UI holds it.
    pub fn attached(&self) -> bool {
        self.state.lock().unwrap().owner.as_ref().is_some_and(|o| o.strong_count() > 0)
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    pub fn detach(&self, owner: *const ()) {
        let mut st = self.state.lock().unwrap();
        if st.owner.as_ref().is_some_and(|o| o.as_ptr() as *const () == owner) {
            st.owner = None;
        }
    }

    /// Write `line` to the program. One that answers a request is kept for
    /// replay (Claude doesn't always echo it).
    pub fn write(&self, line: &str) {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            let form = {
                let mut st = self.state.lock().unwrap();
                st.follow(&v);
                if v.get("type").and_then(Value::as_str) == Some("control_response") {
                    st.lines.push(Kept { text: line.to_owned(), stream: false });
                }
                st.form_line(&v)
            };
            if form {
                self.record(&v);
            }
        }
        let mut stdin = self.stdin.lock().unwrap();
        let Some(w) = stdin.as_mut() else { return };
        let r = w.write_all(line.as_bytes()).and_then(|_| w.write_all(b"\n")).and_then(|_| w.flush());
        if let Err(e) = r {
            log::warn!("chat {}: write: {e}", self.id);
            *stdin = None;
        }
    }

    /// End the program: its input closes (Claude saves and exits), and it is
    /// killed if it is still there a few seconds later.
    pub fn kill(self: &Arc<Self>) {
        self.stdin.lock().unwrap().take();
        let host = self.clone();
        std::thread::spawn(move || {
            for _ in 0..30 {
                if host.exited().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let _ = crate::client::kill_process(host.pid);
        });
    }

    /// Stop reading for a handoff. `None` when the program has ended.
    #[cfg(unix)]
    pub fn park(self: &Arc<Self>) -> io::Result<Option<(File, Parked)>> {
        let Some(reader) = self.reader.lock().unwrap().take() else { return Ok(None) };
        (&reader.wake).write_all(b"x")?;
        let parked = reader.thread.join().map_err(|_| io::Error::other("chat reader panicked"))?;
        match (parked, self.stdin.lock().unwrap().as_ref()) {
            (Some(p), Some(stdin)) => Ok(Some((stdin.try_clone()?, p))),
            (Some(p), None) => {
                // No input any more: it goes on as it ends.
                let _ = self.read(p);
                Ok(None)
            },
            (None, _) => Ok(None),
        }
    }

    /// What a handoff carries, with the parked reader (see [`Self::park`]).
    #[cfg(unix)]
    pub fn handed(&self, io: Option<(File, Parked)>) -> Handed {
        let st = self.state.lock().unwrap();
        Handed {
            meta: self.meta.clone(),
            title: st.title.clone(),
            lines: st.lines.iter().map(|k| k.text.clone()).collect(),
            stderr: st.stderr.clone(),
            exited: st.exited.clone(),
            io,
        }
    }

    fn read(self: &Arc<Self>, parked: Parked) -> io::Result<()> {
        let host = self.clone();
        #[cfg(unix)]
        {
            let (wake_rx, wake_tx) = pipe()?;
            let thread = std::thread::Builder::new().name("chat reader".into()).spawn(move || host.read_loop(parked, wake_rx))?;
            *self.reader.lock().unwrap() = Some(Reader { wake: wake_tx, thread });
        }
        #[cfg(windows)]
        {
            let Parked { stdout, stderr, partial } = parked;
            let errs = self.clone();
            std::thread::Builder::new().name("chat stderr".into()).spawn(move || {
                let mut buf = [0u8; 4096];
                let mut stderr = stderr;
                while let Ok(n) = stderr.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    errs.on_stderr(&buf[..n]);
                }
            })?;
            std::thread::Builder::new().name("chat reader".into()).spawn(move || {
                let mut partial = partial;
                let mut stdout = stdout;
                let mut buf = vec![0u8; 64 << 10];
                while let Ok(n) = stdout.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    host.on_bytes(&mut partial, &buf[..n]);
                }
                host.on_exit();
            })?;
        }
        Ok(())
    }

    /// Read stdout and stderr until both end (the program exited) or
    /// `wake` is written to (parked for a handoff).
    #[cfg(unix)]
    fn read_loop(self: Arc<Self>, parked: Parked, wake: File) -> Option<Parked> {
        use std::os::fd::AsRawFd;
        let Parked { mut stdout, mut stderr, mut partial } = parked;
        let (mut out_open, mut err_open) = (true, true);
        let mut buf = vec![0u8; 64 << 10];
        while out_open || err_open {
            let fd = |f: &File, open: bool| if open { f.as_raw_fd() } else { -1 };
            let mut fds = [
                libc::pollfd { fd: fd(&stdout, out_open), events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: fd(&stderr, err_open), events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: wake.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            ];
            // SAFETY: `fds` is a valid array of 3 pollfds for the call.
            let r = unsafe { libc::poll(fds.as_mut_ptr(), 3, -1) };
            if r < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                log::warn!("chat {}: poll: {}", self.id, io::Error::last_os_error());
                break;
            }
            if fds[2].revents != 0 {
                return Some(Parked { stdout, stderr, partial });
            }
            let ready = |p: &libc::pollfd| p.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0;
            if out_open && ready(&fds[0]) {
                match stdout.read(&mut buf) {
                    Ok(0) | Err(_) => out_open = false,
                    Ok(n) => self.on_bytes(&mut partial, &buf[..n]),
                }
            }
            if err_open && ready(&fds[1]) {
                match stderr.read(&mut buf) {
                    Ok(0) | Err(_) => err_open = false,
                    Ok(n) => self.on_stderr(&buf[..n]),
                }
            }
        }
        self.on_exit();
        None
    }

    fn on_bytes(&self, partial: &mut Vec<u8>, bytes: &[u8]) {
        partial.extend_from_slice(bytes);
        while let Some(end) = partial.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = partial.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line[..end]).trim_end_matches('\r').to_owned();
            // A login shell may print before Claude starts.
            if line.starts_with('{') {
                self.on_line(line);
            }
        }
    }

    fn on_stderr(&self, bytes: &[u8]) {
        let mut st = self.state.lock().unwrap();
        st.stderr.push_str(&String::from_utf8_lossy(bytes));
        if st.stderr.len() > STDERR_TAIL {
            let mut cut = st.stderr.len() - STDERR_TAIL;
            while !st.stderr.is_char_boundary(cut) {
                cut += 1;
            }
            st.stderr.drain(..cut);
        }
    }

    /// Keep `v`, a line of an MCP form, where the chat's history reads it.
    fn record(&self, v: &Value) {
        let Some(path) = &self.meta.record else { return };
        let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        let entry = serde_json::json!({ "ms": ms, "line": v }).to_string();
        let r = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| {
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
            writeln!(f, "{entry}")
        });
        if let Err(e) = r {
            log::warn!("chat {}: record {}: {e}", self.id, path.display());
        }
    }

    fn on_line(&self, line: String) {
        let v: Option<Value> = serde_json::from_str(&line).ok();
        if let Some(v) = &v {
            let form = self.state.lock().unwrap().form_line(v);
            if form {
                self.record(v);
            }
        }
        let kind = v.as_ref().and_then(|v| v.get("type")).and_then(Value::as_str).unwrap_or_default().to_owned();
        let notice = {
            let mut st = self.state.lock().unwrap();
            if kind == "result" {
                st.lines.retain(|k| !k.stream);
            }
            if let Some(v) = &v {
                st.follow(v);
            }
            if st.title.is_none()
                && kind == "user"
                && let Some(text) = v.as_ref().and_then(user_text)
            {
                st.title = Some(text);
            }
            st.lines.push(Kept { stream: kind == "stream_event", text: line.clone() });
            match st.owner.as_ref().and_then(Weak::upgrade) {
                Some(owner) => {
                    owner.chat_line(self.id, &line);
                    None
                },
                None => v.as_ref().and_then(notice).map(|body| (st.title.clone().unwrap_or_else(|| "Claude".into()), body)),
            }
        };
        if let Some((title, body)) = notice
            && !self.muted.load(Ordering::Relaxed)
        {
            (self.notify)(&title, &body);
        }
    }

    fn on_exit(&self) {
        let code = self.child.lock().unwrap().as_mut().and_then(|c| c.wait().ok()).and_then(|s| s.code());
        let mut st = self.state.lock().unwrap();
        let lines: Vec<&str> = st.stderr.lines().filter(|l| !l.trim().is_empty()).collect();
        let stderr = lines[lines.len().saturating_sub(5)..].join("\n");
        let exit = ChatExit { code, stderr };
        log::info!("chat {}: exited ({code:?})", self.id);
        st.exited = Some(exit.clone());
        if let Some(owner) = st.owner.as_ref().and_then(Weak::upgrade) {
            owner.chat_exited(self.id, &exit);
        }
    }
}

/// The text of a user message the program echoed, cut to a title.
fn user_text(v: &Value) -> Option<String> {
    let content = v.pointer("/message/content")?;
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => {
            parts.iter().find_map(|p| (p.get("type")?.as_str()? == "text").then(|| p.get("text")?.as_str().map(str::to_owned))?)?
        },
        _ => return None,
    };
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.chars().take(60).collect())
}

/// What a line tells the user while no UI is connected.
fn notice(v: &Value) -> Option<String> {
    match v.get("type")?.as_str()? {
        "control_request" if v.pointer("/request/subtype")?.as_str()? == "can_use_tool" => {
            Some(match v.pointer("/request/tool_name").and_then(Value::as_str) {
                Some("AskUserQuestion") => "Claude has a question".into(),
                Some("ExitPlanMode") => "Claude has a plan to approve".into(),
                Some(tool) => format!("Claude asks to use {tool}"),
                None => "Claude asks for permission".into(),
            })
        },
        "control_request" if v.pointer("/request/subtype")?.as_str()? == "elicitation" => {
            Some(match v.pointer("/request/mcp_server_name").and_then(Value::as_str) {
                Some(server) => format!("{server} asks for your input"),
                None => "An MCP server asks for your input".into(),
            })
        },
        "result" => {
            // Stopped by the user: no news (older CLIs say so only by an
            // error during execution without errors).
            let stopped = match v.get("terminal_reason").and_then(Value::as_str) {
                Some(r) => r.starts_with("aborted"),
                None => {
                    v.get("subtype").and_then(Value::as_str) == Some("error_during_execution")
                        && v.get("errors").and_then(Value::as_array).is_none_or(|e| e.is_empty())
                },
            };
            Some(match v.get("subtype").and_then(Value::as_str) {
                _ if stopped => return None,
                Some("success") => "Claude finished".into(),
                _ => "Claude stopped with an error".into(),
            })
        },
        _ => None,
    }
}

#[cfg(unix)]
fn to_file(f: impl Into<std::os::fd::OwnedFd>) -> File {
    File::from(f.into())
}

#[cfg(windows)]
fn to_file(f: impl Into<std::os::windows::io::OwnedHandle>) -> File {
    File::from(f.into())
}

/// A pipe: read end, write end (close-on-exec).
#[cfg(unix)]
fn pipe() -> io::Result<(File, File)> {
    use std::os::fd::FromRawFd;
    let mut fds = [0; 2];
    // SAFETY: `fds` has room for the two fds `pipe` writes.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    for fd in fds {
        // SAFETY: `fd` was just opened by `pipe`.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    // SAFETY: both fds are open and owned by nothing else.
    Ok(unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Lines(Mutex<Vec<String>>, Mutex<Option<ChatExit>>);

    impl ChatSink for Lines {
        fn chat_line(&self, _: u64, line: &str) {
            self.0.lock().unwrap().push(line.to_owned());
        }
        fn chat_exited(&self, _: u64, exit: &ChatExit) {
            *self.1.lock().unwrap() = Some(exit.clone());
        }
    }

    fn wait(what: &str, f: impl Fn() -> bool) {
        for _ in 0..200 {
            if f() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("timed out waiting for {what}");
    }

    #[test]
    fn follows_turns_and_requests() {
        let mut st = State::default();
        let mut feed = |line: &str| st.follow(&serde_json::from_str(line).unwrap());
        feed(r#"{"type":"user","message":{}}"#);
        feed(r#"{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool"}}"#);
        feed(r#"{"type":"control_request","request_id":"r2","request":{"subtype":"can_use_tool"}}"#);
        feed(r#"{"type":"control_response","response":{"request_id":"r1","subtype":"success"}}"#);
        feed(r#"{"type":"control_cancel_request","request_id":"r2"}"#);
        drop(feed);
        assert!(st.busy && st.asks.is_empty());
        st.follow(&serde_json::json!({"type":"control_request","request_id":"r3","request":{"subtype":"can_use_tool"}}));
        assert_eq!(st.asks.len(), 1);
        st.follow(&serde_json::json!({"type":"result"}));
        assert!(!st.busy && st.asks.is_empty());
    }

    #[test]
    fn keeps_the_lines_of_forms_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("chats").join("s.jsonl");
        let script = r#"echo '{"type":"control_request","request_id":"p1","request":{"subtype":"can_use_tool"}}'
echo '{"type":"control_request","request_id":"e1","request":{"subtype":"elicitation"}}'
read a; echo '{"type":"result","subtype":"success"}'; read b"#;
        let spec = ChatSpec { record: Some(record.clone()), ..sh(script) };
        let host = ChatHost::spawn(4, &spec, Arc::new(|_: &str, _: &str| {})).unwrap();
        wait("the requests", || host.state.lock().unwrap().lines.len() == 2);
        host.write(r#"{"type":"control_response","response":{"subtype":"success","request_id":"p1","response":{}}}"#);
        host.write(r#"{"type":"control_response","response":{"subtype":"success","request_id":"e1","response":{"action":"decline"}}}"#);
        wait("the result", || host.state.lock().unwrap().lines.iter().any(|k| k.text.contains("result")));
        let kept: Vec<Value> =
            std::fs::read_to_string(&record).unwrap().lines().map(|l| serde_json::from_str::<Value>(l).unwrap()["line"].clone()).collect();
        let ids: Vec<&str> = kept
            .iter()
            .map(|v| v.pointer("/request_id").or_else(|| v.pointer("/response/request_id")).and_then(Value::as_str).unwrap())
            .collect();
        assert_eq!(ids, ["e1", "e1"], "the form's request and answer, not the permission's");
        host.kill();
    }

    #[test]
    fn tells_forms_and_failures_but_not_a_stop() {
        use serde_json::json;
        let form = json!({"type":"control_request","request_id":"e","request":{"subtype":"elicitation","mcp_server_name":"deploy"}});
        assert_eq!(notice(&form).as_deref(), Some("deploy asks for your input"));
        let mut st = State::default();
        st.follow(&form);
        assert_eq!(st.asks.len(), 1);
        let stopped = json!({"type":"result","subtype":"error_during_execution","terminal_reason":"aborted_streaming"});
        assert_eq!(notice(&stopped), None);
        let failed = json!({"type":"result","subtype":"error_during_execution","terminal_reason":"model_error","errors":["500"]});
        assert_eq!(notice(&failed).as_deref(), Some("Claude stopped with an error"));
    }

    fn sh(script: &str) -> ChatSpec {
        ChatSpec { program: "/bin/sh".into(), args: vec!["-c".into(), script.into()], cwd: "/".into(), ..Default::default() }
    }

    #[test]
    fn replays_then_streams_and_drops_finished_deltas() {
        let notified = Arc::new(Mutex::new(Vec::new()));
        let n = notified.clone();
        let notify: Notify = Arc::new(move |t: &str, b: &str| n.lock().unwrap().push(format!("{t}: {b}")));
        let script = r#"echo 'shell noise'
echo '{"type":"user","isReplay":true,"message":{"content":[{"type":"text","text":"fix the bug"}]}}'
echo '{"type":"stream_event","event":{}}'
echo '{"type":"result","subtype":"success"}'
read line; echo "{\"type\":\"echo\",\"got\":$line}"; echo oops >&2; exit 3"#;
        let host = ChatHost::spawn(1, &sh(script), notify).unwrap();
        wait("the result", || host.state.lock().unwrap().lines.len() == 2);
        // Unattended: the finished turn notified, titled by the first message.
        assert_eq!(notified.lock().unwrap().as_slice(), ["fix the bug: Claude finished"]);
        let sink = Arc::new(Lines::default());
        let owner: Arc<dyn ChatSink> = sink.clone();
        let mut replay = Vec::new();
        host.attach(&owner, |lines, exited| {
            assert!(exited.is_none());
            replay = lines;
        });
        assert_eq!(replay.len(), 2, "the delta went with its turn: {replay:?}");
        host.write(r#"{"type":"control_response","n":1}"#);
        wait("the exit", || sink.1.lock().unwrap().is_some());
        assert_eq!(sink.0.lock().unwrap().len(), 1);
        let exit = sink.1.lock().unwrap().clone().unwrap();
        assert_eq!((exit.code, exit.stderr.as_str()), (Some(3), "oops"));
        // The answer is kept for the next replay.
        assert!(host.state.lock().unwrap().lines.iter().any(|k| k.text.contains("control_response")));
    }

    #[test]
    fn a_muted_chat_posts_nothing() {
        let notified = Arc::new(Mutex::new(Vec::new()));
        let n = notified.clone();
        let notify: Notify = Arc::new(move |t: &str, b: &str| n.lock().unwrap().push(format!("{t}: {b}")));
        let script =
            r#"read a; echo '{"type":"result","subtype":"success"}'; read b; echo '{"type":"result","subtype":"success"}'; read c"#;
        let host = ChatHost::spawn(3, &sh(script), notify).unwrap();
        host.set_muted(true);
        host.write("go");
        wait("the first result", || host.state.lock().unwrap().lines.len() == 1);
        // Unmuted, the next one is told; the first one (done before the
        // second line is read) wasn't.
        host.set_muted(false);
        host.write("go");
        wait("the second notification", || !notified.lock().unwrap().is_empty());
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(notified.lock().unwrap().len(), 1);
        host.kill();
    }

    #[test]
    fn parks_and_resumes_without_losing_a_line() {
        let notify: Notify = Arc::new(|_: &str, _: &str| {});
        let host = ChatHost::spawn(2, &sh(r#"read a; printf '{"n":1}\n{"n":'; read b; printf '2}\n'; read c"#), notify).unwrap();
        let sink = Arc::new(Lines::default());
        let owner: Arc<dyn ChatSink> = sink.clone();
        host.attach(&owner, |_, _| {});
        host.write("go");
        wait("the first line", || sink.0.lock().unwrap().len() == 1);
        std::thread::sleep(Duration::from_millis(50));
        let (stdin, parked) = host.park().unwrap().expect("running");
        assert_eq!(parked.partial, br#"{"n":"#);
        // As the new daemon would: a host on the same pipes.
        let notify: Notify = Arc::new(|_: &str, _: &str| {});
        let handed = host.handed(Some((stdin, parked)));
        let (next, parked) = ChatHost::adopt(2, host.pid, host.started, handed, notify);
        next.attach(&owner, |lines, _| assert_eq!(lines.len(), 1));
        next.resume(parked.unwrap());
        next.write("more");
        wait("the line split across the handoff", || sink.0.lock().unwrap().len() == 2);
        assert_eq!(sink.0.lock().unwrap()[1], r#"{"n":2}"#);
        // The old daemon lets go of its copy of the pipes.
        drop(host);
        next.kill();
        wait("the exit", || next.exited().is_some());
    }
}
