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
//! It does follow how full the conversation's context window is, for the
//! guard a Claude tab has too (see [`tern_bridge::context`]): the program's
//! hooks come to [`ChatHost::on_hook`], and what the guard has to say to
//! Claude goes in as a user message.
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
use tern_bridge::HookEvent;
use tern_bridge::context::{Action, Guard, Policy};
use tern_chat::{ChatFollow, Said};
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
/// How long after a hook its guard's message goes in: Claude has its answer
/// by then.
const AFTER_HOOK: Duration = Duration::from_millis(500);

/// How long a message waits for a fresh conversation to open.
const FRESH_CONVERSATION_WAIT: Duration = Duration::from_secs(15);

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
    /// The conversation now, once Claude said (`/clear` starts another).
    session: Option<String>,
    follower: Follower,
    guard: Guard,
    policy: Policy,
}

/// What the daemon does with a line, besides keeping it.
#[derive(Default)]
struct Followed {
    /// It is a line of an MCP form: the request, the UI's answer to it, or
    /// the agent taking it back. Recorded.
    form: bool,
    /// It ends a turn: the turn's streamed lines go.
    ended: bool,
    /// It settles a request: written, it is kept for replay.
    settles: bool,
    /// A piece of a message: it goes when its turn ends.
    streamed: bool,
    /// What a notification is titled, as the first message says.
    title: Option<String>,
}

/// What reads a chat's lines for [`State`]: its agent's, if it has one.
struct Follower(Option<Box<dyn ChatFollow>>);

impl Follower {
    fn of(agent: &str) -> Self {
        Self(tern_chat::chat_follower(agent))
    }
}

impl Default for Follower {
    /// Claude's, as a [`ChatMeta`] is Claude's unless it says otherwise.
    fn default() -> Self {
        Self::of(tern_agent::CLAUDE)
    }
}

impl State {
    fn new(agent: &tern_agent::AgentId) -> Self {
        Self { follower: Follower::of(agent.as_str()), guard: crate::server::guard_for(Some(agent)), ..Default::default() }
    }

    /// Take in `v`, a line read from the program.
    fn follow(&mut self, v: &Value) -> Followed {
        let mut said = Vec::new();
        if let Some(follower) = &mut self.follower.0 {
            follower.follow(v, &mut said);
        }
        self.take_in(said)
    }

    /// Take in `v`, a line written to the program.
    fn wrote(&mut self, v: &Value) -> Followed {
        let mut said = Vec::new();
        if let Some(follower) = &mut self.follower.0 {
            follower.wrote(v, &mut said);
        }
        self.take_in(said)
    }

    /// What a line said of the turn and the requests waiting.
    fn take_in(&mut self, said: Vec<Said>) -> Followed {
        let mut line = Followed::default();
        for s in said {
            match s {
                Said::TurnStarted => self.busy = true,
                Said::TurnEnded => {
                    self.busy = false;
                    self.asks.clear();
                    line.ended = true;
                },
                Said::Session(id) => self.session = Some(id),
                Said::Ask(id) => {
                    self.asks.insert(id);
                },
                Said::Settled(id) => {
                    self.asks.remove(&id);
                    line.settles = true;
                },
                Said::Form(id) => {
                    self.forms.insert(id);
                    line.form = true;
                },
                Said::FormSettled(id) => line.form |= self.forms.remove(&id),
                Said::Reading(r) => self.guard.reading(r.percent, self.policy),
                Said::Streamed => line.streamed = true,
                Said::Title(title) => line.title = Some(title),
            }
        }
        line
    }

    /// Hook `ev` came: the answer for Claude Code, and what to tell it after.
    fn on_hook(&mut self, ev: &HookEvent) -> (Option<Value>, Action) {
        if let Some(id) = &ev.session_id {
            self.session = Some(id.clone());
        }
        self.guard.on_event(ev, self.policy)
    }
}

/// Whose chat it is: what a UI that finds it without a tab opens one with.
#[derive(Debug, Clone)]
pub(crate) struct ChatMeta {
    pub cwd: std::path::PathBuf,
    pub session: String,
    pub account: String,
    /// Where MCP forms' lines are kept (see [`ChatSpec::record`]).
    pub record: Option<std::path::PathBuf>,
    /// The agent it runs.
    pub agent: tern_agent::AgentId,
}

impl Default for ChatMeta {
    fn default() -> Self {
        Self {
            cwd: Default::default(),
            session: Default::default(),
            account: Default::default(),
            record: None,
            agent: tern_agent::AgentId::new(tern_agent::CLAUDE),
        }
    }
}

impl ChatMeta {
    /// Its agent's short name, as notifications call it.
    fn agent_name(&self) -> String {
        tern_agent::registry().get(self.agent.as_str()).map_or_else(|| self.agent.to_string(), |d| d.short_name.clone())
    }
}

pub(crate) struct ChatHost {
    pub id: u64,
    pub pid: u32,
    /// When the program started, in ms since the Unix epoch.
    pub started: u64,
    pub meta: ChatMeta,
    /// What its hooks say they are from; `None` without hooks.
    pub token: Option<String>,
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
    pub token: Option<String>,
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
    pub fn spawn(id: u64, spec: &ChatSpec, token: Option<String>, notify: Notify) -> io::Result<Arc<Self>> {
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
                agent: spec.agent_id(),
            },
            token,
            stdin: Mutex::new(stdin),
            state: Mutex::new(State::new(&spec.agent_id())),
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
        let mut state = State { exited: handed.exited, title: handed.title, stderr: handed.stderr, ..State::new(&handed.meta.agent) };
        for k in &lines {
            if let Ok(v) = serde_json::from_str::<Value>(&k.text) {
                // Its forms were recorded by the daemon before.
                state.follow(&v);
            }
        }
        state.lines = lines;
        let host = Arc::new(Self {
            id,
            pid,
            started,
            meta: handed.meta,
            token: handed.token,
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

    /// The conversation it is in now.
    pub fn session(&self) -> String {
        self.state.lock().unwrap().session.clone().unwrap_or_else(|| self.meta.session.clone())
    }

    /// Its tab keeps the context window in bounds so.
    pub fn set_policy(&self, policy: Policy) {
        self.state.lock().unwrap().policy = policy;
    }

    /// Hook `ev` of its program came; the answer for Claude Code, if any.
    pub fn on_hook(self: &Arc<Self>, ev: &HookEvent) -> Option<Value> {
        let (reply, action) = self.state.lock().unwrap().on_hook(ev);
        self.tell(action);
        reply
    }

    /// The user asks for a handoff, to step away.
    pub fn ask_handoff(self: &Arc<Self>) {
        let busy = matches!(self.status(), ActivityStatus::Working | ActivityStatus::Blocked);
        let action = self.state.lock().unwrap().guard.request(busy);
        self.tell(action);
    }

    /// The user goes on from the handoff at `path` in a fresh conversation.
    pub fn resume_handoff(self: &Arc<Self>, path: Option<String>) {
        let action = self.state.lock().unwrap().guard.resume_from(path);
        self.tell(action);
    }

    /// Say what the guard wants to the agent, as a user message: it takes
    /// it when its turn is over. A clear comes first: a message that goes
    /// on after it waits for the fresh conversation to open.
    fn tell(self: &Arc<Self>, action: Action) {
        let (clear, text) = match action {
            Action::None => return,
            Action::Clear => (true, None),
            Action::Prompt(p) => (false, Some(p)),
            Action::ClearPrompt(p) => (true, Some(p)),
        };
        let host = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(AFTER_HOOK);
            // Its agent's own lines (only agents with a chat driver run as chats).
            let Some(mut driver) = tern_chat::chat_driver(host.meta.agent.as_str()) else { return };
            if clear {
                let old = host.session();
                host.write(&driver.clear_line(&old, &host.meta.cwd));
                if text.is_some() && !host.wait_for_another_session(&old) {
                    log::warn!("chat {}: no fresh conversation came", host.id);
                    return;
                }
            }
            if let Some(text) = text {
                let (_, line) = driver.user_line(&host.session(), &text, &[]);
                host.write(&line);
            }
        });
    }

    /// Wait for the conversation to be another than `old`.
    fn wait_for_another_session(&self, old: &str) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed() < FRESH_CONVERSATION_WAIT {
            if self.session() != old {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
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
                let followed = st.wrote(&v);
                if followed.settles {
                    st.lines.push(Kept { text: line.to_owned(), stream: false });
                }
                followed.form
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
            meta: ChatMeta { session: st.session.clone().unwrap_or_else(|| self.meta.session.clone()), ..self.meta.clone() },
            token: self.token.clone(),
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
        let followed = v.as_ref().map_or_else(Followed::default, |v| self.state.lock().unwrap().follow(v));
        if let Some(v) = v.as_ref().filter(|_| followed.form) {
            self.record(v);
        }
        let notice = {
            let mut st = self.state.lock().unwrap();
            if followed.ended {
                st.lines.retain(|k| !k.stream);
            }
            if st.title.is_none() {
                st.title = followed.title;
            }
            st.lines.push(Kept { stream: followed.streamed, text: line.clone() });
            match st.owner.as_ref().and_then(Weak::upgrade) {
                Some(owner) => {
                    owner.chat_line(self.id, &line);
                    None
                },
                None => {
                    let name = self.meta.agent_name();
                    let body = v.as_ref().zip(st.follower.0.as_ref()).and_then(|(v, f)| f.notice(v, &name));
                    body.map(|body| (st.title.clone().unwrap_or(name), body))
                },
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

#[cfg(test)]
mod guard_tests {
    use serde_json::json;
    use tern_bridge::HookEvent;
    use tern_bridge::context::{Action, Policy};

    use super::*;

    fn hook(body: Value) -> HookEvent {
        HookEvent::parse("t".into(), &serde_json::to_vec(&body).unwrap()).unwrap()
    }

    /// A conversation `percent` full of a window of 100 tokens.
    fn turn(st: &mut State, percent: u64) {
        st.follow(&json!({"type": "assistant", "message": {"model": "m", "usage": {"input_tokens": percent}}}));
        st.follow(&json!({"type": "result", "modelUsage": {"m": {"contextWindow": 100}}}));
    }

    #[test]
    fn past_its_limit_a_chat_hands_off_when_it_stops() {
        let mut st = State { policy: Policy { limit: 50, auto_switch: true }, ..Default::default() };
        turn(&mut st, 60);
        let (reply, action) = st.on_hook(&hook(json!({"hook_event_name": "Stop", "stop_hook_active": false})));
        assert_eq!(reply.unwrap().get("decision").unwrap(), "block");
        assert_eq!(action, Action::None);
        let done = json!({"hook_event_name": "Stop", "stop_hook_active": true, "last_assistant_message": "Handoff: h.md"});
        assert_eq!(st.on_hook(&hook(done)), (None, Action::Clear));
    }

    #[test]
    fn under_its_limit_a_chat_stops_as_it_likes() {
        let mut st = State { policy: Policy { limit: 50, auto_switch: true }, ..Default::default() };
        turn(&mut st, 40);
        assert_eq!(st.on_hook(&hook(json!({"hook_event_name": "Stop"}))), (None, Action::None));
    }

    #[test]
    fn a_cleared_chat_is_not_born_over_its_limit() {
        let mut st = State { policy: Policy { limit: 50, auto_switch: true }, ..Default::default() };
        turn(&mut st, 60);
        st.on_hook(&hook(json!({"hook_event_name": "SessionStart", "source": "clear"})));
        // Lines of the old conversation's end say nothing of the new one.
        st.follow(&json!({"type": "command_lifecycle"}));
        st.follow(&json!({"type": "conversation_reset"}));
        st.follow(&json!({"type": "result", "modelUsage": {}}));
        turn(&mut st, 10);
        turn(&mut st, 60);
        let (reply, _) = st.on_hook(&hook(json!({"hook_event_name": "Stop", "stop_hook_active": false})));
        assert_eq!(reply.unwrap().get("decision").unwrap(), "block");
    }

    #[test]
    fn follows_the_conversation_a_clear_starts() {
        let mut st = State::default();
        st.follow(&json!({"type": "system", "subtype": "init", "session_id": "new"}));
        assert_eq!(st.session.as_deref(), Some("new"));
        st.on_hook(&hook(json!({"hook_event_name": "SessionStart", "session_id": "newer", "source": "clear"})));
        assert_eq!(st.session.as_deref(), Some("newer"));
    }
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
        let host = ChatHost::spawn(4, &spec, None, Arc::new(|_: &str, _: &str| {})).unwrap();
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

    fn sh(script: &str) -> ChatSpec {
        ChatSpec { program: "/bin/sh".into(), args: vec!["-c".into(), script.into()], cwd: "/".into(), ..Default::default() }
    }

    /// A Codex server that opens thread `one`, then answers a `thread/start`
    /// with thread `two`, echoing what it is told.
    fn codex_server() -> ChatSpec {
        let script = r#"echo '{"id":1,"result":{"thread":{"id":"one"}}}'
while read -r line; do
  echo "{\"got\":$line}"
  case "$line" in *thread/start*) echo '{"id":"x","result":{"thread":{"id":"two"}}}';; esac
done"#;
        ChatSpec { agent: Some("codex".into()), ..sh(script) }
    }

    fn turn_starts(host: &ChatHost) -> Vec<Value> {
        let st = host.state.lock().unwrap();
        let lines = st.lines.iter().filter_map(|k| serde_json::from_str::<Value>(&k.text).ok());
        lines.filter_map(|v| v.get("got").cloned()).filter(|v| v["method"] == "turn/start").collect()
    }

    #[test]
    fn codex_chat_runs_against_a_scripted_server() {
        let host = ChatHost::spawn(9, &codex_server(), None, Arc::new(|_: &str, _: &str| {})).unwrap();
        wait("the thread", || host.session() == "one");
        host.ask_handoff();
        wait("the guard's message", || !turn_starts(&host).is_empty());
        let turn = &turn_starts(&host)[0];
        assert_eq!(turn["params"]["threadId"], "one");
        assert!(turn["params"]["input"][0]["text"].as_str().unwrap().contains("handoff"), "{turn}");
        host.kill();
    }

    #[test]
    fn a_codex_chat_goes_on_in_a_fresh_thread() {
        let host = ChatHost::spawn(10, &codex_server(), None, Arc::new(|_: &str, _: &str| {})).unwrap();
        wait("the thread", || host.session() == "one");
        host.resume_handoff(None);
        wait("the prompt", || !turn_starts(&host).is_empty());
        let turn = &turn_starts(&host)[0];
        assert_eq!(turn["params"]["threadId"], "two");
        assert_eq!(turn["params"]["input"][0]["text"], tern_bridge::context::CONTINUE_PROMPT);
        host.kill();
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
        let host = ChatHost::spawn(1, &sh(script), None, notify).unwrap();
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
        let host = ChatHost::spawn(3, &sh(script), None, notify).unwrap();
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
        let host = ChatHost::spawn(2, &sh(r#"read a; printf '{"n":1}\n{"n":'; read b; printf '2}\n'; read c"#), None, notify).unwrap();
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
