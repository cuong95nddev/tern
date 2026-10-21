//! Localhost HTTP endpoint for Claude Code `type: "http"` hooks.
//!
//! Each terminal tab gets a random token; its `claude` process is launched with
//! `--settings <json>` (see [`claude_settings_json`]) pointing PostToolUse hooks
//! (and per-turn hooks, which carry the session id) at
//! `http://127.0.0.1:<port>/hook/<token>`. Requests with unknown tokens are
//! rejected, so other local processes can't spoof events without the token.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

const MAX_BODY: usize = 8 * 1024 * 1024;
const MAX_HEADER_BYTES: u64 = 16 * 1024;
const MAX_CONNECTIONS: usize = 32;
const REQUEST_DEADLINE: Duration = Duration::from_secs(2);

/// Reader that fails once an absolute deadline passes.
struct DeadlineReader {
    stream: TcpStream,
    deadline: Instant,
}

impl Read for DeadlineReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self
            .deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::TimedOut, "request deadline"))?;
        self.stream.set_read_timeout(Some(left))?;
        self.stream.read(buf)
    }
}

/// The tools whose `file_path` (or `notebook_path`) is a file they changed.
const EDIT_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

#[derive(Debug, Clone, PartialEq)]
pub struct HookEvent {
    pub token: String,
    pub event: String,
    pub tool_name: Option<String>,
    pub session_id: Option<String>,
    pub cwd: Option<PathBuf>,
    /// Files this tool call touched (Write/Edit/MultiEdit/NotebookEdit file_path,
    /// plus Bash `bashEditDiff.changedFiles` when available).
    pub paths: Vec<PathBuf>,
    pub raw: Value,
}

impl HookEvent {
    pub fn parse(token: String, body: &[u8]) -> Option<Self> {
        let raw: Value = serde_json::from_slice(body).ok()?;
        let s = |v: &Value| v.as_str().map(str::to_owned);
        let cwd = raw.get("cwd").and_then(Value::as_str).map(PathBuf::from);
        let mut paths = vec![];
        // Only tools that write: a `Read` names a file it left alone.
        let writes = raw.get("tool_name").and_then(Value::as_str).is_some_and(|t| EDIT_TOOLS.contains(&t));
        let input = raw.get("tool_input").filter(|_| writes);
        for key in ["file_path", "notebook_path"] {
            if let Some(p) = input.and_then(|i| i.get(key)).and_then(Value::as_str) {
                paths.push(PathBuf::from(p));
            }
        }
        if let Some(files) = raw.pointer("/tool_response/bashEditDiff/changedFiles").and_then(Value::as_array) {
            for f in files.iter().filter_map(Value::as_str) {
                let p = PathBuf::from(f);
                paths.push(match (&cwd, p.is_absolute()) {
                    (Some(c), false) => c.join(p),
                    _ => p,
                });
            }
        }
        Some(Self {
            token,
            event: raw.get("hook_event_name").and_then(s).unwrap_or_default(),
            tool_name: raw.get("tool_name").and_then(s),
            session_id: raw.get("session_id").and_then(s),
            cwd,
            paths,
            raw,
        })
    }
}

/// Takes each hook event; returns the answer for Claude Code, if any (else
/// `{}`).
pub type HookSink = Arc<dyn Fn(HookEvent) -> Option<Value> + Send + Sync>;

#[derive(Clone)]
pub struct HookServer {
    port: u16,
    tokens: Arc<Mutex<HashSet<String>>>,
    /// Kept to hand to another process (a daemon taking over).
    listener: Arc<TcpListener>,
}

impl HookServer {
    pub fn start(sink: HookSink) -> std::io::Result<Self> {
        Self::serve(TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?, sink, [])
    }

    /// Serve on `listener` (another process's, handed over) with the tokens
    /// it already gave out.
    pub fn serve(listener: TcpListener, sink: HookSink, tokens: impl IntoIterator<Item = String>) -> std::io::Result<Self> {
        let port = listener.local_addr()?.port();
        let tokens: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(tokens.into_iter().collect()));
        let t = tokens.clone();
        let active = Arc::new(AtomicUsize::new(0));
        let accept = listener.try_clone()?;
        thread::Builder::new().name("hook-server".into()).spawn(move || {
            for stream in accept.incoming().flatten() {
                // Bound concurrent connections so a local flood can't exhaust threads.
                if active.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
                    active.fetch_sub(1, Ordering::AcqRel);
                    continue; // drop
                }
                let (sink, tokens, active) = (sink.clone(), t.clone(), active.clone());
                thread::spawn(move || {
                    let _ = handle(stream, &tokens, &sink);
                    active.fetch_sub(1, Ordering::AcqRel);
                });
            }
        })?;
        Ok(Self { port, tokens, listener: Arc::new(listener) })
    }

    /// The listening socket.
    pub fn listener(&self) -> &TcpListener {
        &self.listener
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Register a new tab; returns its token.
    pub fn register(&self) -> String {
        let token = random_token();
        self.tokens.lock().unwrap().insert(token.clone());
        token
    }

    pub fn unregister(&self, token: &str) {
        self.tokens.lock().unwrap().remove(token);
    }

    pub fn hook_url(&self, token: &str) -> String {
        format!("http://127.0.0.1:{}/hook/{token}", self.port)
    }
}

/// Write the tab's settings to a private (0600) file and return its path, so the
/// hook token never appears in the process list (`ps` shows argv).
///
/// `dir` must be (or become) a 0700 directory owned by the current user; the
/// file is created exclusively without following symlinks. Remove it with
/// [`remove_claude_settings`] when the tab closes. On Windows `dir` is under
/// the user's local app data, which only they can read.
///
/// With `exe` (Tern's own binary), Claude Code's status line runs through it
/// (see [`crate::statusline`]), and so do the hooks it can't send over HTTP
/// (see [`crate::relay`]).
pub fn write_claude_settings(
    dir: &std::path::Path,
    token: &str,
    hook_url: &str,
    exe: Option<&std::path::Path>,
) -> std::io::Result<PathBuf> {
    let path = dir.join(format!("claude-settings-{token}.json"));
    let status_line = exe.map(|e| crate::statusline::command(e, &path));
    let relay = exe.map(|e| crate::relay::command(e, &path));
    #[cfg(unix)]
    let mut f = {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        let meta = std::fs::symlink_metadata(dir)?;
        let uid = unsafe { libc::getuid() };
        if !meta.is_dir() || meta.uid() != uid || meta.permissions().mode() & 0o077 != 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "settings dir is not private"));
        }
        std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(&path)?
    };
    #[cfg(not(unix))]
    let mut f = {
        std::fs::create_dir_all(dir)?;
        if !std::fs::symlink_metadata(dir)?.is_dir() {
            return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "settings dir is not a directory"));
        }
        std::fs::OpenOptions::new().write(true).create_new(true).open(&path)?
    };
    f.write_all(claude_settings_json(hook_url, status_line.as_deref(), relay.as_deref()).as_bytes())?;
    Ok(path)
}

pub fn remove_claude_settings(dir: &std::path::Path, token: &str) {
    let _ = std::fs::remove_file(dir.join(format!("claude-settings-{token}.json")));
}

/// The file in a run dir that names the process using it, when that isn't
/// the one it is named for (a daemon that took over another's sessions).
const RUN_DIR_OWNER: &str = "owner";

/// Mark run dir `dir` (created if needed) as this process's. It is made
/// private, as [`write_claude_settings`] wants it.
pub fn claim_run_dir(dir: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(RUN_DIR_OWNER), std::process::id().to_string())
}

/// Drop what earlier Tern processes left in `root` (the parent of every
/// process's own run dir, named by its pid): the dirs of processes no longer
/// running, and 0.1.0's loose settings files once they are a day old.
pub fn sweep_run_dirs(root: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            let owner = std::fs::read_to_string(e.path().join(RUN_DIR_OWNER)).ok().and_then(|s| s.trim().parse::<i32>().ok());
            if let Some(pid) = owner.or_else(|| name.parse::<i32>().ok())
                && !process_alive(pid)
            {
                let _ = std::fs::remove_dir_all(e.path());
            }
        } else if name.starts_with("claude-settings-")
            && name.ends_with(".json")
            && e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|age| age > Duration::from_secs(86_400)))
        {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

#[cfg(unix)]
fn process_alive(pid: i32) -> bool {
    pid > 0 && (unsafe { libc::kill(pid, 0) } == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
}

#[cfg(windows)]
fn process_alive(pid: i32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    if pid <= 0 {
        return false;
    }
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32);
        if h.is_null() {
            // Gone, or not ours to open (then it isn't a Tern of this user).
            return false;
        }
        let mut code = 0u32;
        let alive = GetExitCodeProcess(h, &mut code) != 0 && code == STILL_ACTIVE as u32;
        CloseHandle(h);
        alive
    }
}

/// Settings JSON passed to `claude --settings` for a tab, its status line
/// `status_line` when given, and its `SessionStart` hook (which Claude Code
/// runs only as a command) `relay`.
pub fn claude_settings_json(hook_url: &str, status_line: Option<&str>, relay: Option<&str>) -> String {
    let handler = serde_json::json!([{ "type": "http", "url": hook_url, "timeout": 5 }]);
    let mut settings = serde_json::json!({
        "hooks": {
            // Every tool: the files edited come from these, and Claude
            // learns from their answers that its context is past the limit.
            "PostToolUse": [{ "hooks": handler }],
            // The tab learns its session id from these, to resume it later.
            "UserPromptSubmit": [{ "hooks": handler }],
            "Stop": [{ "hooks": handler }]
        }
    });
    if let Some(command) = status_line {
        settings["statusLine"] = serde_json::json!({ "type": "command", "command": command });
    }
    if let Some(command) = relay {
        // A fresh conversation learns of the handoff it goes on from.
        settings["hooks"]["SessionStart"] = serde_json::json!([{ "hooks": [{ "type": "command", "command": command, "timeout": 5 }] }]);
    }
    settings.to_string()
}

pub fn random_token() -> String {
    let mut buf = [0u8; 16];
    getrandom::fill(&mut buf).expect("getrandom");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn handle(stream: TcpStream, tokens: &Mutex<HashSet<String>>, sink: &HookSink) -> std::io::Result<()> {
    // One deadline for the whole request so slow-drip clients can't pin slots.
    let deadline = Instant::now() + REQUEST_DEADLINE;
    let mut reader = BufReader::new(DeadlineReader { stream: stream.try_clone()?, deadline });
    // Cap total header bytes read before authentication.
    let mut budget = MAX_HEADER_BYTES;
    let mut read_line = |reader: &mut BufReader<DeadlineReader>, line: &mut String| -> std::io::Result<usize> {
        let n = reader.by_ref().take(budget).read_line(line)?;
        budget -= n as u64;
        Ok(n)
    };
    let mut request_line = String::new();
    read_line(&mut reader, &mut request_line)?;
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if read_line(&mut reader, &mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((k, v)) = line.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut parts = request_line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let token = path.strip_prefix("/hook/").map(str::to_owned);
    let authorized = token.as_ref().is_some_and(|t| tokens.lock().unwrap().iter().any(|k| constant_time_eq(k.as_bytes(), t.as_bytes())));

    let mut out = stream;
    if method != "POST" || !authorized || content_length > MAX_BODY {
        // Read what was sent first: closing with unread bytes resets the
        // connection (on Linux) before the client reads the answer.
        if content_length <= MAX_BODY {
            let _ = std::io::copy(&mut reader.by_ref().take(content_length as u64), &mut std::io::sink());
        }
        out.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
        return Ok(());
    }
    let mut body = Vec::with_capacity(content_length);
    reader.by_ref().take(content_length as u64).read_to_end(&mut body)?;
    if body.len() != content_length {
        return Ok(());
    }
    let reply =
        HookEvent::parse(token.unwrap(), &body).and_then(|ev| sink(ev)).map_or_else(|| b"{}".to_vec(), |v| v.to_string().into_bytes());
    let head = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", reply.len());
    out.write_all(head.as_bytes())?;
    out.write_all(&reply)?;
    Ok(())
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    #[test]
    fn sweeps_dead_run_dirs() {
        let root = std::env::temp_dir().join(format!("tern-sweep-{}", std::process::id()));
        let me = root.join(std::process::id().to_string());
        // No process has pid i32::MAX.
        let dead = root.join(i32::MAX.to_string());
        std::fs::create_dir_all(&me).unwrap();
        std::fs::create_dir_all(&dead).unwrap();
        std::fs::write(root.join("claude-settings-new.json"), "{}").unwrap();
        super::sweep_run_dirs(&root);
        assert!(me.exists());
        assert!(!dead.exists());
        assert!(root.join("claude-settings-new.json").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn settings_go_in_a_claimed_run_dir() {
        let root = std::env::temp_dir().join(format!("tern-claim-settings-{}", std::process::id()));
        let dir = root.join("run");
        super::claim_run_dir(&dir).unwrap();
        let path = super::write_claude_settings(&dir, "tok", "http://127.0.0.1:1/hook/tok", None).unwrap();
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_claimed_run_dir_follows_its_owner() {
        let root = std::env::temp_dir().join(format!("tern-claim-{}", std::process::id()));
        // Named for a process that is gone, used by one that runs.
        let adopted = root.join(i32::MAX.to_string());
        super::claim_run_dir(&adopted).unwrap();
        super::sweep_run_dirs(&root);
        assert!(adopted.exists());
        // Named for a process that runs, claimed by one that is gone.
        let me = root.join(std::process::id().to_string());
        std::fs::create_dir_all(&me).unwrap();
        std::fs::write(me.join("owner"), i32::MAX.to_string()).unwrap();
        super::sweep_run_dirs(&root);
        assert!(!me.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    use super::*;
    use std::sync::mpsc;

    fn post(port: u16, path: &str, body: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(s, "POST {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len())
            .unwrap();
        let mut resp = String::new();
        s.read_to_string(&mut resp).unwrap();
        resp
    }

    #[test]
    fn receives_authorized_hooks_only() {
        let (tx, rx) = mpsc::channel();
        let srv = HookServer::start(Arc::new(move |e| {
            let _ = tx.send(e);
            None
        }))
        .unwrap();
        let token = srv.register();
        let body = r#"{"hook_event_name":"PostToolUse","tool_name":"Write","cwd":"/p","session_id":"s1","tool_input":{"file_path":"/p/a.rs","content":"x"},"tool_response":{"filePath":"/p/a.rs","type":"create"}}"#;
        assert!(post(srv.port(), &format!("/hook/{token}"), body).starts_with("HTTP/1.1 200"));
        let ev = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(ev.tool_name.as_deref(), Some("Write"));
        assert_eq!(ev.paths, vec![PathBuf::from("/p/a.rs")]);
        assert_eq!(ev.token, token);

        assert!(post(srv.port(), "/hook/bogus", body).starts_with("HTTP/1.1 403"));
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn answers_with_the_sinks_reply() {
        let srv =
            HookServer::start(Arc::new(|e: HookEvent| (e.event == "Stop").then(|| serde_json::json!({"decision": "block"})))).unwrap();
        let token = srv.register();
        let resp = post(srv.port(), &format!("/hook/{token}"), r#"{"hook_event_name":"Stop"}"#);
        assert!(resp.ends_with(r#"{"decision":"block"}"#), "{resp}");
        let resp = post(srv.port(), &format!("/hook/{token}"), r#"{"hook_event_name":"PostToolUse"}"#);
        assert!(resp.ends_with("\r\n\r\n{}"), "{resp}");
    }

    #[test]
    fn a_read_changes_no_file() {
        let body = br#"{"hook_event_name":"PostToolUse","tool_name":"Read","cwd":"/p","tool_input":{"file_path":"/p/a.rs"}}"#;
        assert!(HookEvent::parse("t".into(), body).unwrap().paths.is_empty());
    }

    #[test]
    fn parses_bash_changed_files() {
        let body = br#"{"hook_event_name":"PostToolUse","tool_name":"Bash","cwd":"/p","tool_input":{"command":"sed -i x"},"tool_response":{"bashEditDiff":{"changedFiles":["src/x.rs","/abs/y.rs"]}}}"#;
        let ev = HookEvent::parse("t".into(), body).unwrap();
        assert_eq!(ev.paths, vec![PathBuf::from("/p/src/x.rs"), PathBuf::from("/abs/y.rs")]);
    }

    #[test]
    fn settings_file_private_and_removed() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("run");
        let p = write_claude_settings(&dir, "abc", "http://127.0.0.1:1/hook/abc", None).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert!(write_claude_settings(&dir, "abc", "x", None).is_err(), "must not overwrite");
        remove_claude_settings(&dir, "abc");
        assert!(!p.exists());
    }

    #[test]
    fn settings_json_shape() {
        let v: Value = serde_json::from_str(&claude_settings_json("http://127.0.0.1:1/hook/t", None, None)).unwrap();
        assert_eq!(v.pointer("/hooks/PostToolUse/0/hooks/0/type").unwrap(), "http");
        assert_eq!(v.pointer("/hooks/UserPromptSubmit/0/hooks/0/type").unwrap(), "http");
        assert_eq!(v.pointer("/hooks/Stop/0/hooks/0/type").unwrap(), "http");
        assert!(v.get("statusLine").is_none());
        assert!(v.pointer("/hooks/SessionStart").is_none());
        let v: Value =
            serde_json::from_str(&claude_settings_json("u", Some("'tern' --statusline 's'"), Some("'tern' --hook 's'"))).unwrap();
        assert_eq!(v.pointer("/statusLine/command").unwrap(), "'tern' --statusline 's'");
        assert_eq!(v.pointer("/hooks/SessionStart/0/hooks/0/command").unwrap(), "'tern' --hook 's'");
    }
}
