//! Tasks one agent gives another (`tern --task`, see `agent_cli`): Claude
//! asks Codex to review a diff, and gets Codex's answer back.
//!
//! A task runs in a new agent tab of the asking agent's workspace, left
//! behind the shown tab, so the user can watch it, answer its questions and
//! approve its tools. Tern hands it the task as a user would (pasted, then
//! Enter: `prompt_input`). Its answer is the closing message of its turn,
//! from its `Stop` hook (`last_assistant_message`), as T3 Code takes a
//! delegated thread's last assistant message: not text read off the screen.
//! An agent Tern has no hooks for answers with its screen once its status
//! says the turn is done (as Herdr reads a pane).
//!
//! A task's id is its terminal's entity id: any agent tab can be read and
//! asked again, not only those a task opened.
//!
//! The agent that gave a task need not wait for it: once it is done, asks
//! something or ends, Tern tells that agent in its own tab, as Orca wakes a
//! coordinator with a mailbox pointer: a line typed into its terminal, then
//! Enter half a second later, held back while that agent shows a dialog or
//! the user has a draft in its prompt (`composer_draft`), and not while it
//! waits on the task (a `wait` already hears). A giver that is no agent (a
//! shell, where the line would run) is told nothing; it waits instead, as
//! Herdr's callers do.
//!
//! What a task's agent asks while blocked can be answered by the keys the
//! user would press, while it still asks it (Herdr's `agent send-keys`); as
//! Herdr's guide says, the giving agent asks the user before it answers.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui_kit::*;
use serde_json::{Value, json};
use tern_agent::AgentId;
use tern_bridge::HookEvent;

use crate::main_window;
use crate::prompt_input::{Step, plan, run, wait_ready};
use crate::terminal_view::{ActivityStatus, TerminalView};
use crate::workspace::Workspace;

/// The screen lines an answer read off the screen keeps.
const SCREEN_LINES: usize = 60;
/// How long a new tab's agent may take to start, a question it asks the
/// user as it starts included, before its task is given up.
const START_LIMIT: Duration = Duration::from_secs(15 * 60);
/// How often tasks are looked at for news to tell the agents that gave them.
const WAKE_TICK: Duration = Duration::from_secs(1);
/// A giver that read its task this recently waits on it (`--task wait`
/// reads every second): it hears the news so, not by a wake.
const READ_RECENTLY: Duration = Duration::from_secs(3);
/// A draft in a giver's prompt holds its wake back when the user typed there
/// this recently (Orca's `USER_INPUT_HOLD_CAP_MS`): an older one is taken
/// for abandoned or misread.
const DRAFT_HOLD: Duration = Duration::from_secs(10 * 60);
/// How long a turn's end stays put before its giver hears of it (Orca's
/// `AGENT_DONE_QUIET_MS`): sub-agents and the last output settle.
const DONE_QUIET: Duration = Duration::from_millis(1500);
/// From a wake's line to its Enter (Orca's pointer Enter delay).
const WAKE_ENTER_DELAY: Duration = Duration::from_millis(500);
/// How much of what an agent asks a report keeps.
const ASK_DETAIL: usize = 600;

/// What a blocked agent asks, as its hooks told.
#[derive(Clone, Debug, PartialEq)]
pub enum Asks {
    /// To run a tool (a command, an edit): `detail` is what it would run.
    Approval { tool: String, detail: String },
    /// A question for the user.
    Question { detail: String },
}

/// What a tool call's input says, short: its command when it has one, and
/// why (Codex's `justification`, Claude's `description`).
fn tool_detail(input: Option<&Value>) -> String {
    let Some(input) = input else { return String::new() };
    let mut text = match input.get("command") {
        Some(Value::String(c)) => c.clone(),
        Some(Value::Array(parts)) => parts.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "),
        _ => input.to_string(),
    };
    let why = ["justification", "description"].iter().find_map(|k| input.get(*k).and_then(Value::as_str));
    if let Some(why) = why.filter(|w| !w.trim().is_empty() && input.get("command").is_some()) {
        text = format!("{text} (why: {})", why.trim());
    }
    if text.chars().count() > ASK_DETAIL { text.chars().take(ASK_DETAIL).collect::<String>() + "…" } else { text }
}

/// A question tool's questions, each with its options numbered as its
/// screen numbers them (Claude's `AskUserQuestion` and Codex's
/// `request_user_input` take `questions: [{question, options: [{label}]}]`,
/// Codex's `request_user_input_async` `questions: [{title, options: [text]}]`).
fn question_detail(input: Option<&Value>) -> String {
    let Some(questions) = input.and_then(|i| i.get("questions")).and_then(Value::as_array) else { return tool_detail(input) };
    let text = questions
        .iter()
        .map(|q| {
            let ask = q.get("question").or_else(|| q.get("title")).and_then(Value::as_str).unwrap_or_default();
            let options = q.get("options").and_then(Value::as_array).into_iter().flatten();
            let labels = options.filter_map(|o| o.as_str().or_else(|| o.get("label").and_then(Value::as_str)));
            let numbered: Vec<String> = labels.enumerate().map(|(i, l)| format!("{}. {l}", i + 1)).collect();
            if numbered.is_empty() { ask.to_owned() } else { format!("{ask} ({})", numbered.join(", ")) }
        })
        .collect::<Vec<_>>()
        .join(" / ");
    if text.chars().count() > ASK_DETAIL { text.chars().take(ASK_DETAIL).collect::<String>() + "…" } else { text }
}

/// What a terminal tab's agent was asked and answered.
#[derive(Default)]
pub struct Track {
    /// It was opened for a task: the agent that gave it may close it.
    opened: bool,
    /// It was opened for a task it has not been handed yet.
    starting: bool,
    /// When Tern handed it a prompt it has not answered yet.
    asked: Option<Instant>,
    /// Its last turn's closing message, from its `Stop` hook.
    reply: Option<String>,
    /// Handing it the prompt failed.
    failed: Option<String>,
    /// What it asks, from its hooks, until its tool runs or its turn ends.
    asks: Option<Asks>,
    /// That is a question asked without waiting (Codex's
    /// `request_user_input_async`): its tool returns at once, and it is
    /// asked until the turn ends or the user sends a prompt.
    asks_async: bool,
    /// The agent tab (its background session) that gave it its task: told
    /// when it is done or asks something.
    by: Option<u64>,
    /// The news its giver last read or was told (a state's name).
    told: Option<&'static str>,
    /// When its giver last read it.
    read_at: Option<Instant>,
    /// When its last turn ended.
    stopped: Option<Instant>,
}

impl Track {
    /// It is opened for a task from agent tab `by`, handed to it once it
    /// takes input.
    pub fn starting(&mut self, by: Option<u64>) {
        self.opened = true;
        self.starting = true;
        self.failed = None;
        self.gave(by);
    }

    /// Tern handed it a prompt at `at`.
    pub fn asked(&mut self, at: Instant) {
        self.starting = false;
        self.asked = Some(at);
        self.failed = None;
    }

    /// Agent tab `by` gave it its task (or more of it): the news to tell
    /// it starts afresh.
    pub fn gave(&mut self, by: Option<u64>) {
        if by.is_some() {
            self.by = by;
        }
        self.told = None;
    }

    /// Its giver read it as `report` says.
    pub fn read(&mut self, report: &Report) {
        self.read_at = Some(Instant::now());
        self.told = report.news();
    }

    /// The agent tab that gave it its task.
    pub fn giver(&self) -> Option<u64> {
        self.by
    }

    /// It has no news now (it works): what comes next is news again.
    pub fn quiet(&mut self) {
        self.told = None;
    }

    /// Its giver was told `news`.
    pub fn told(&mut self, news: &'static str) {
        self.told = Some(news);
    }

    /// The agent tab to tell `report`'s news to, when it hasn't heard it
    /// and isn't waiting on it.
    pub fn to_tell(&self, report: &Report) -> Option<u64> {
        let news = report.news()?;
        let waiting = self.read_at.is_some_and(|t| t.elapsed() < READ_RECENTLY);
        if report.state == State::Done && self.stopped.is_some_and(|t| t.elapsed() < DONE_QUIET) {
            return None;
        }
        (self.told != Some(news) && !waiting).then_some(self.by?)
    }

    /// What it asked was answered by its giver, which reads it next (`tern
    /// --task answer` does): no wake while the answer lands.
    pub fn answered(&mut self) {
        self.asks = None;
        self.asks_async = false;
        self.told = None;
        self.read_at = Some(Instant::now());
    }

    pub fn failed(&mut self, error: String) {
        self.starting = false;
        self.asked = None;
        self.failed = Some(error);
    }

    /// What it asks is a question asked without waiting (see `asks_async`).
    pub fn asks_async(&self) -> bool {
        self.asks_async
    }

    /// It was opened for a task (not by the user).
    pub fn opened(&self) -> bool {
        self.opened
    }

    pub fn on_hook(&mut self, ev: &HookEvent) {
        let question = tern_term::claude_status::is_ask_user_question(ev.tool_name.as_deref());
        let asks_async = tern_term::claude_status::is_async_question(ev.tool_name.as_deref());
        let tool = ev.tool_name.clone().unwrap_or_default();
        match ev.event.as_str() {
            "Stop" => {
                let text = ev.raw.get("last_assistant_message").and_then(Value::as_str).unwrap_or_default();
                self.reply = Some(text.trim().to_owned());
                self.asked = None;
                self.asks = None;
                self.asks_async = false;
                self.stopped = Some(Instant::now());
            },
            // Codex: the user stopped the turn.
            "Interrupt" => {
                self.reply = Some(String::new());
                self.asked = None;
                self.asks = None;
                self.asks_async = false;
                self.stopped = Some(Instant::now());
            },
            "PreToolUse" if question || asks_async => {
                self.asks = Some(Asks::Question { detail: question_detail(ev.raw.get("tool_input")) });
                self.asks_async = asks_async;
            },
            // Newer Claude Code asks leave to show its question too: still
            // the question.
            "PermissionRequest" if question => {
                if !matches!(self.asks, Some(Asks::Question { .. })) {
                    self.asks = Some(Asks::Question { detail: question_detail(ev.raw.get("tool_input")) });
                }
            },
            "PermissionRequest" => {
                self.asks = Some(Asks::Approval { detail: tool_detail(ev.raw.get("tool_input")), tool });
                self.asks_async = false;
            },
            // A question asked without waiting is still asked once its tool
            // returned, while the turn goes on.
            "PostToolUse" if self.asks_async => {},
            "PostToolUse" | "UserPromptSubmit" => {
                self.asks = None;
                self.asks_async = false;
            },
            _ => {},
        }
    }

    /// Where its task stands, by its terminal's activity `status` (since
    /// `since`), whether its program `exited`, and whether its agent runs
    /// with Tern's `hooks` (its turns end with a `Stop` hook).
    fn state(&self, status: ActivityStatus, since: Instant, exited: bool, hooks: bool) -> State {
        if let Some(e) = &self.failed {
            State::Failed(e.clone())
        } else if exited {
            State::Ended
        } else if status == ActivityStatus::Blocked {
            State::Blocked
        } else if self.starting {
            State::Working
        } else if let Some(asked) = self.asked {
            // Without hooks, its status says when the turn it was asked for ends.
            if !hooks && status == ActivityStatus::Done && since > asked { State::Done } else { State::Working }
        } else if status == ActivityStatus::Working {
            State::Working
        } else if self.reply.is_some() {
            State::Done
        } else {
            State::Idle
        }
    }

    /// Where its task stands, its terminal `view` showing what the agent does.
    pub fn report(&self, id: u64, agent: Option<AgentId>, title: String, folder: PathBuf, view: &TerminalView) -> Report {
        let hooks = view.has_hooks();
        // What it asks shows on its screen, read now: before it takes its
        // task, a question as it starts tells no status (and its status, read
        // every so often, would still say it asks just after the user
        // answered); later, a question asked without waiting leaves its
        // turn working by its hooks.
        let status = match self.starting {
            _ if view.screen_asks() => ActivityStatus::Blocked,
            true => ActivityStatus::None,
            false => view.status(),
        };
        let state = self.state(status, view.status_since(), view.has_exited(), hooks);
        let screen = || {
            let lines = view.screen_lines();
            lines[lines.len().saturating_sub(SCREEN_LINES)..].join("\n")
        };
        let (reply, screen) = match state {
            State::Done if hooks => (self.reply.clone(), None),
            State::Done => (None, Some(screen())),
            State::Blocked | State::Ended => (None, Some(screen())),
            _ => (None, None),
        };
        let asks = (state == State::Blocked).then(|| match (&self.asks, self.starting) {
            (_, true) => Asking::Startup,
            (Some(a), false) => Asking::Hooked(a.clone()),
            (None, false) => Asking::Unknown,
        });
        Report { id, agent, title, folder, state, reply, screen, asks }
    }
}

/// What a blocked agent asks, as a report tells it.
#[derive(Clone, Debug, PartialEq)]
pub enum Asking {
    /// A question as it starts (whether to trust the folder, to update):
    /// the user's alone.
    Startup,
    Hooked(Asks),
    /// Its hooks didn't say (an agent without them): maybe an approval.
    Unknown,
}

impl Asking {
    fn kind(&self) -> &'static str {
        match self {
            Asking::Startup => "startup",
            Asking::Hooked(Asks::Approval { .. }) => "approval",
            Asking::Hooked(Asks::Question { .. }) => "question",
            Asking::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// Asked nothing yet.
    Idle,
    /// Its turn runs (or it is starting to take the task).
    Working,
    /// It asks the user something or waits for an approval, in its tab.
    Blocked,
    /// Its turn ended.
    Done,
    /// Its program ended.
    Ended,
    /// The task never reached it.
    Failed(String),
}

impl State {
    fn name(&self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Working => "working",
            State::Blocked => "blocked",
            State::Done => "done",
            State::Ended => "ended",
            State::Failed(_) => "failed",
        }
    }
}

/// An agent tab's task as `tern --task` tells it.
#[derive(Clone, Debug)]
pub struct Report {
    pub id: u64,
    pub agent: Option<AgentId>,
    pub title: String,
    pub folder: PathBuf,
    pub state: State,
    /// Its turn's closing message, once it is done.
    pub reply: Option<String>,
    /// Its screen, when there's no reply to tell it by.
    pub screen: Option<String>,
    /// What it asks, when blocked.
    pub asks: Option<Asking>,
}

impl Report {
    /// The news its giver hears of: it is done, asks something, or can go
    /// on no more.
    pub fn news(&self) -> Option<&'static str> {
        matches!(self.state, State::Done | State::Blocked | State::Ended | State::Failed(_)).then(|| self.state.name())
    }

    /// Whether it can be handed a new prompt now.
    pub fn can_ask(&self) -> Result<(), String> {
        match self.state {
            State::Working => Err(format!("agent tab {} is still working (tern --task wait {} waits for it)", self.id, self.id)),
            State::Blocked => Err(format!("agent tab {} waits for the user to answer in its tab", self.id)),
            State::Ended => Err(format!("the agent in tab {} has ended", self.id)),
            _ => Ok(()),
        }
    }

    /// As `tern --task` reads it.
    pub fn json(&self) -> Value {
        json!({
            "id": self.id,
            "agent": self.agent.as_ref().map(ToString::to_string),
            "name": self.agent.as_ref().map(crate::agents::agent_name),
            "title": self.title,
            "folder": self.folder,
            "state": self.state.name(),
            "error": match &self.state { State::Failed(e) => Some(e.as_str()), _ => None },
            "reply": self.reply,
            "screen": self.screen,
            "asks": self.asks.as_ref().map(|a| {
                let (tool, detail) = match a {
                    Asking::Hooked(Asks::Approval { tool, detail }) => (Some(tool.as_str()), Some(detail.as_str())),
                    Asking::Hooked(Asks::Question { detail }) => (None, Some(detail.as_str())),
                    _ => (None, None),
                };
                json!({ "kind": a.kind(), "tool": tool, "detail": detail })
            }),
        })
    }
}

/// Answer `tern --task`'s request `op` (see `agent_ui`).
pub fn answer(op: &str, request: &Value, cx: &mut App) -> Value {
    let result = match op {
        "task_start" => start(request, cx),
        "task_read" => id_of(request).and_then(|id| read(id, by_of(request), cx)),
        "task_send" => send(request, cx),
        "task_answer" => answer_task(request, cx),
        "task_list" => Ok(list(request, cx)),
        "task_close" => id_of(request).and_then(|id| close(id, by_of(request), cx)),
        _ => Err("unknown request".to_owned()),
    };
    result.unwrap_or_else(|e| json!({ "error": e }))
}

fn id_of(request: &Value) -> Result<u64, String> {
    request.get("id").and_then(Value::as_u64).ok_or_else(|| "`id` is required".to_owned())
}

/// The agent tab (its background session) that asks: the daemon sets it
/// from the process at the other end of the socket, not from what the
/// request says of itself (`tern_mux` `stamp_caller`), so a task's giver
/// can't be claimed by another agent.
fn by_of(request: &Value) -> Option<u64> {
    request.get("by").and_then(Value::as_u64)
}

fn prompt_of(request: &Value) -> Result<String, String> {
    let prompt = request.get("prompt").and_then(Value::as_str).map(str::trim).unwrap_or_default();
    if prompt.is_empty() { Err("a task to give is required".into()) } else { Ok(prompt.to_owned()) }
}

/// One folder is in the other.
fn related(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// Where the agent asking runs: the workspace with its tab (`by`, its
/// background session), else one whose folder is its folder's. With
/// `exact` (a task in a worktree of its own): the workspace of that very
/// folder.
fn home(request: &Value, cx: &mut App) -> Option<(AnyWindowHandle, Entity<Workspace>)> {
    let all = main_window::workspaces(cx);
    let folder = request.get("folder").and_then(Value::as_str).map(PathBuf::from);
    if request.get("exact").and_then(Value::as_bool) == Some(true) {
        let folder = folder?;
        let folder = dunce::canonicalize(&folder).unwrap_or(folder);
        return all.into_iter().find(|(_, ws)| ws.read(cx).root() == folder);
    }
    let mine = by_of(request).and_then(|by| all.iter().find(|(_, ws)| ws.read(cx).mux_ids(cx).contains(&Some(by))).cloned());
    mine.or_else(|| {
        let folder = folder?;
        all.into_iter().find(|(_, ws)| related(ws.read(cx).root(), &folder))
    })
}

/// The workspace with agent tab `id`.
fn holder(id: u64, cx: &mut App) -> Option<(AnyWindowHandle, Entity<Workspace>)> {
    main_window::workspaces(cx).into_iter().find(|(_, ws)| ws.read(cx).task_report(id, cx).is_some())
}

/// Agent tab `id`'s task, read by agent tab `by`: when that is its giver,
/// it has heard its news (and, reading again and again, waits on it).
fn read(id: u64, by: Option<u64>, cx: &mut App) -> Result<Value, String> {
    let (_, ws) = holder(id, cx).ok_or_else(|| format!("no agent tab {id} (it was closed?)"))?;
    let report = ws.read(cx).task_report(id, cx).ok_or_else(|| format!("no agent tab {id}"))?;
    if by.is_some() && ws.read(cx).task_giver(id) == by {
        ws.update(cx, |ws, cx| ws.update_task(id, |t| t.read(&report), cx));
    }
    Ok(report.json())
}

fn start(request: &Value, cx: &mut App) -> Result<Value, String> {
    let agent = request.get("agent").and_then(Value::as_str).ok_or("`agent` is required")?;
    let registry = tern_agent::registry();
    if registry.get(agent).is_none() {
        let known: Vec<&str> = registry.all().map(|d| d.id.as_str()).collect();
        return Err(format!("no agent {agent} (Tern knows: {})", known.join(", ")));
    }
    let prompt = prompt_of(request)?;
    let (handle, ws) = home(request, cx).ok_or("no Tern window shows this folder: open it in Tern first")?;
    let agent = AgentId::new(agent);
    let by = by_of(request);
    let view =
        handle.update(cx, |_, window, cx| ws.update(cx, |ws, cx| ws.start_task(agent, by, window, cx))).map_err(|e| e.to_string())?;
    let id = view.entity_id().as_u64();
    deliver(&ws, view, id, prompt, true, cx);
    read(id, None, cx)
}

fn send(request: &Value, cx: &mut App) -> Result<Value, String> {
    let id = id_of(request)?;
    let prompt = prompt_of(request)?;
    let (_, ws) = holder(id, cx).ok_or_else(|| format!("no agent tab {id} (it was closed?)"))?;
    let view = ws.update(cx, |ws, cx| ws.ask_task(id, by_of(request), cx))?;
    deliver(&ws, view, id, prompt, false, cx);
    read(id, None, cx)
}

/// What to type to answer a question: `option` picks one by its number,
/// or `text` is typed as the answer.
fn answer_keys(text: &str, option: Option<u64>) -> Result<Vec<Step>, String> {
    match option {
        Some(n @ 1..=9) => Ok(vec![Step::Type(n.to_string().into_bytes())]),
        Some(n) => Err(format!("no option {n}: options go from 1 to 9")),
        None if text.trim().is_empty() => Err("an answer is required".into()),
        None => Ok(plan(&[], text)),
    }
}

/// Answer what agent tab `id` asks, by the keys the user would press, while
/// it still asks it. Only the agent that gave it the task answers (the
/// daemon vouches for who asks, see `by_of`): a question, by an option or
/// text, or an approval (to run a command, to edit a file), by its choice's
/// number only. What it asks as it starts (whether to trust the folder) and
/// what its hooks didn't name are the user's alone.
fn answer_task(request: &Value, cx: &mut App) -> Result<Value, String> {
    let id = id_of(request)?;
    let text = request.get("answer").and_then(Value::as_str).unwrap_or_default();
    let option = request.get("option").and_then(Value::as_u64);
    let (_, ws) = holder(id, cx).ok_or_else(|| format!("no agent tab {id} (it was closed?)"))?;
    let report = ws.read(cx).task_report(id, cx).ok_or_else(|| format!("no agent tab {id}"))?;
    let Some(asking) = report.asks.as_ref().filter(|_| report.state == State::Blocked) else {
        return Err(format!("agent tab {id} asks nothing now (it is {})", report.state.name()));
    };
    let approval = match asking {
        Asking::Hooked(Asks::Question { .. }) => false,
        Asking::Hooked(Asks::Approval { .. }) => true,
        Asking::Startup | Asking::Unknown => {
            return Err("only the user answers this, in its tab: what it asks as it starts, or what its hooks didn't name".into());
        },
    };
    let giver = ws.read(cx).task_giver(id);
    if giver.is_none() || giver != by_of(request) {
        return Err(format!("only the agent that gave task {id} answers what it asks"));
    }
    // An approval is one of the choices its screen numbers (yes, yes and
    // don't ask again, no), never text that could land elsewhere.
    if approval && option.is_none() {
        return Err("an approval is answered by its choice's number: --option N (see its screen)".into());
    }
    // One approval at a time: a choice that allows more from then on is
    // the user's.
    if approval
        && let Some(n) = option
        && let Some(view) = ws.read(cx).task_view(id)
        && lasting_choice(&view.read(cx).screen_lines(), n)
    {
        return Err(format!("option {n} allows more than this once (don't ask again): only the user chooses it, in its tab"));
    }
    let mut steps = answer_keys(text, option)?;
    // Codex shows a question asked without waiting among its queued inputs:
    // Shift+Left ("⇧← to answer") opens it, as the user would.
    if ws.read(cx).task_asks_async(id) {
        steps.splice(0..0, [Step::Type(b"\x1b[1;2D".to_vec()), Step::Wait(Duration::from_millis(300))]);
    }
    let view = ws.read(cx).task_view(id).ok_or_else(|| format!("no agent tab {id}"))?;
    ws.update(cx, |ws, cx| ws.update_task(id, Track::answered, cx));
    cx.spawn(async move |cx| run(&view, steps, cx).await).detach();
    Ok(json!({ "id": id, "answered": true }))
}

/// Whether choice `n` on an approval's `screen` allows more than this once
/// ("Yes, and don't ask again for ...", "always allow").
fn lasting_choice(screen: &[String], n: u64) -> bool {
    let mark = format!("{n}.");
    screen.iter().any(|line| {
        let t = line.trim_start().trim_start_matches(['›', '❯', '>', ' ']);
        t.starts_with(&mark) && {
            let l = t.to_lowercase().replace('’', "'");
            l.contains("don't ask again") || l.contains("always")
        }
    })
}

/// Close agent tab `id`, which a task opened, ending its agent: only the
/// agent that gave it the task closes it.
fn close(id: u64, by: Option<u64>, cx: &mut App) -> Result<Value, String> {
    let (handle, ws) = holder(id, cx).ok_or_else(|| format!("no agent tab {id} (it was closed?)"))?;
    let giver = ws.read(cx).task_giver(id);
    if giver.is_some() && giver != by {
        return Err(format!("only the agent that gave task {id} closes its tab"));
    }
    handle.update(cx, |_, window, cx| ws.update(cx, |ws, cx| ws.close_task(id, window, cx))).map_err(|e| e.to_string())??;
    Ok(json!({ "id": id, "closed": true }))
}

/// Hand `prompt` to the agent in `view` (once it takes input, when it has
/// just been started); a failure is told by its task.
fn deliver(ws: &Entity<Workspace>, view: Entity<TerminalView>, id: u64, prompt: String, fresh: bool, cx: &mut App) {
    let ws = ws.downgrade();
    cx.spawn(async move |cx| {
        let handed = async {
            if fresh {
                wait_ready(&view, START_LIMIT, cx).await?;
            }
            run(&view, plan(&[], &prompt), cx).await
        }
        .await;
        let _ = ws.update(cx, |ws, cx| match handed {
            Ok(()) if fresh => ws.update_task(id, |t| t.asked(Instant::now()), cx),
            Ok(()) => {},
            Err(e) => ws.update_task(id, |t| t.failed(e), cx),
        });
    })
    .detach();
}

/// The agent tabs of the workspaces in the asking agent's folder (or of
/// every workspace, with `all`).
fn list(request: &Value, cx: &mut App) -> Value {
    let all = request.get("all").and_then(Value::as_bool).unwrap_or(false);
    let folder = request.get("folder").and_then(Value::as_str).map(PathBuf::from);
    let tasks: Vec<Value> = main_window::workspaces(cx)
        .into_iter()
        .filter(|(_, ws)| all || folder.as_deref().is_some_and(|f| related(ws.read(cx).root(), f)))
        .flat_map(|(_, ws)| ws.read(cx).task_reports(cx))
        .map(|r| r.json())
        .collect();
    json!({ "tasks": tasks })
}

/// Tell the agents that gave tasks their news, from now on.
pub fn init(cx: &mut App) {
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(WAKE_TICK).await;
            cx.update(tell_givers);
        }
    })
    .detach();
}

/// The line that tells a giver task `report`'s news, `tern` the command
/// that reads it.
fn wake_line(report: &Report, tern: &str) -> String {
    let id = report.id;
    let name = report.agent.as_ref().map_or_else(|| "its agent".to_owned(), crate::agents::agent_name);
    let what = match report.state {
        State::Done => "is done",
        State::Blocked => "asks something (a question or an approval)",
        State::Ended => "has ended: its program exited",
        _ => "failed: the task never reached it",
    };
    format!("[Tern] Task {id} ({name}) {what}. Run `{tern} --task read {id}` to see it.")
}

/// Whether a giver's terminal takes a wake now: it shows no dialog (it
/// would take the Enter as its answer) and holds no draft the user typed
/// lately (it would be sent with the line), as Orca's
/// `isAgentSettledForDelivery` holds a pointer. A working agent takes it: it
/// queues typed input (or steers by it) as it would the user's.
fn takes_wake(giver: &TerminalView) -> bool {
    let dialog = giver.status() == ActivityStatus::Blocked || giver.screen_asks();
    let draft = giver.typed_within(DRAFT_HOLD) && giver.composer_draft().is_some();
    !dialog && !draft
}

/// Tell each agent that gave a task the task's news it hasn't heard, in its
/// tab, when that takes it. A giver that is no agent (a shell, where the
/// line would run) or is gone hears nothing.
fn tell_givers(cx: &mut App) {
    let all = main_window::workspaces(cx);
    let news: Vec<(Entity<Workspace>, Report, u64)> = all
        .iter()
        .flat_map(|(_, ws)| ws.update(cx, |ws, cx| ws.task_news(cx)).into_iter().map(move |(r, by)| (ws.clone(), r, by)))
        .collect();
    if news.is_empty() {
        return;
    }
    let exe = std::env::current_exe().unwrap_or_else(|_| "tern".into());
    let tern = tern_bridge::statusline::quote(&exe.to_string_lossy());
    for (ws, report, by) in news {
        let Some(news) = report.news() else { continue };
        let giver = all.iter().find_map(|(_, w)| w.read(cx).session_view(by, cx));
        let Some(giver) = giver.filter(|g| g.read(cx).agent().is_some()) else {
            ws.update(cx, |ws, cx| ws.update_task(report.id, |t| t.told(news), cx));
            continue;
        };
        if !takes_wake(giver.read(cx)) {
            continue;
        }
        ws.update(cx, |ws, cx| ws.update_task(report.id, |t| t.told(news), cx));
        let line = wake_line(&report, &tern);
        // Typed as is, then Enter on its own a moment later, unless a dialog
        // opened meanwhile (it would take the Enter as its answer).
        giver.update(cx, |v, cx| v.write_input(line.into_bytes(), cx));
        cx.spawn(async move |cx| {
            cx.background_executor().timer(WAKE_ENTER_DELAY).await;
            giver.update(cx, |v, cx| {
                if v.status() != ActivityStatus::Blocked && !v.screen_asks() {
                    v.write_input(b"\r".to_vec(), cx);
                }
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use serde_json::json;
    use tern_bridge::HookEvent;

    use super::{Asks, Report, State, Track, answer_keys, lasting_choice, wake_line};
    use crate::prompt_input::Step;
    use crate::terminal_view::ActivityStatus::{self, Blocked, Done, Working};

    fn hook(body: serde_json::Value) -> HookEvent {
        HookEvent::parse("t".into(), body.to_string().as_bytes()).unwrap()
    }

    fn hooked(t: &Track, status: ActivityStatus, since: Instant) -> State {
        t.state(status, since, false, true)
    }

    fn unhooked(t: &Track, status: ActivityStatus, since: Instant) -> State {
        t.state(status, since, false, false)
    }

    #[test]
    fn a_hooked_task_is_done_at_its_stop_with_its_last_message() {
        let mut t = Track::default();
        let now = Instant::now();
        assert_eq!(hooked(&t, ActivityStatus::None, now), State::Idle);
        t.starting(None);
        // Not yet handed its task: working, whatever its screen says.
        assert_eq!(hooked(&t, Done, now), State::Working);
        // A question it asks as it starts.
        assert_eq!(hooked(&t, Blocked, now), State::Blocked);
        t.asked(now);
        // Its status alone does not end a hooked task: its Stop does.
        assert_eq!(hooked(&t, Done, now + Duration::from_secs(1)), State::Working);
        t.on_hook(&hook(json!({ "hook_event_name": "Stop", "last_assistant_message": "  LGTM\n" })));
        assert_eq!(hooked(&t, Done, now), State::Done);
        assert_eq!(t.reply.as_deref(), Some("LGTM"));
        // The user starts a turn of their own in its tab.
        assert_eq!(hooked(&t, Working, now), State::Working);
    }

    #[test]
    fn a_task_without_hooks_is_done_when_its_status_says_so_after_it_was_asked() {
        let mut t = Track::default();
        t.starting(None);
        let asked = Instant::now();
        t.asked(asked);
        // A Done from before it was asked is its start's.
        assert_eq!(unhooked(&t, Done, asked - Duration::from_secs(1)), State::Working);
        assert_eq!(unhooked(&t, Working, asked + Duration::from_secs(1)), State::Working);
        assert_eq!(unhooked(&t, Done, asked + Duration::from_secs(2)), State::Done);
    }

    #[test]
    fn a_failed_or_ended_task_says_so_until_asked_again() {
        let mut t = Track::default();
        let now = Instant::now();
        t.starting(None);
        t.failed("The agent didn't start in time".into());
        assert_eq!(hooked(&t, Working, now), State::Failed("The agent didn't start in time".into()));
        t.asked(now);
        assert_eq!(hooked(&t, Working, now), State::Working);
        assert_eq!(t.state(Working, now, true, true), State::Ended);
    }

    #[test]
    fn what_it_asks_comes_from_its_hooks_until_its_tool_runs() {
        let mut t = Track::default();
        t.asked(Instant::now());
        t.on_hook(&hook(json!({
            "hook_event_name": "PermissionRequest", "tool_name": "Bash", "tool_input": { "command": "curl -sI https://example.com" },
        })));
        assert_eq!(t.asks, Some(Asks::Approval { tool: "Bash".into(), detail: "curl -sI https://example.com".into() }));
        t.on_hook(&hook(json!({
            "hook_event_name": "PermissionRequest", "tool_name": "Bash",
            "tool_input": { "command": "uv run x", "justification": "Download Pillow?" },
        })));
        assert_eq!(t.asks, Some(Asks::Approval { tool: "Bash".into(), detail: "uv run x (why: Download Pillow?)".into() }));
        t.on_hook(&hook(json!({ "hook_event_name": "PostToolUse", "tool_name": "Bash" })));
        assert_eq!(t.asks, None);
        t.on_hook(&hook(json!({ "hook_event_name": "PreToolUse", "tool_name": "AskUserQuestion", "tool_input": { "q": 1 } })));
        assert!(matches!(t.asks, Some(Asks::Question { .. })));
        t.on_hook(&hook(json!({ "hook_event_name": "Stop" })));
        assert_eq!(t.asks, None);
    }

    #[test]
    fn a_question_reads_with_its_numbered_options_and_stays_one_when_leave_is_asked() {
        let mut t = Track::default();
        let input = json!({ "questions": [{ "question": "Which color?", "options": [{ "label": "Red" }, { "label": "Blue" }] }] });
        t.on_hook(&hook(json!({ "hook_event_name": "PreToolUse", "tool_name": "AskUserQuestion", "tool_input": input })));
        let asked = Some(Asks::Question { detail: "Which color? (1. Red, 2. Blue)".into() });
        assert_eq!(t.asks, asked);
        // Codex asks without waiting: its tool returns at once and the turn
        // goes on (it sleeps) until the user answers.
        let mut c = Track::default();
        let input = json!({ "questions": [{ "title": "Which color?", "options": ["Red", "Blue"] }] });
        c.on_hook(&hook(json!({ "hook_event_name": "PreToolUse", "tool_name": "request_user_input_async", "tool_input": input })));
        c.on_hook(&hook(json!({ "hook_event_name": "PostToolUse", "tool_name": "request_user_input_async" })));
        c.on_hook(&hook(json!({ "hook_event_name": "PostToolUse", "tool_name": "sleep" })));
        assert_eq!(c.asks, asked, "still asked while its turn sleeps");
        c.on_hook(&hook(json!({ "hook_event_name": "Stop" })));
        assert_eq!(c.asks, None);
        // Newer Claude Code also asks leave for it.
        t.on_hook(&hook(json!({ "hook_event_name": "PermissionRequest", "tool_name": "AskUserQuestion", "tool_input": input })));
        assert_eq!(t.asks, asked);
    }

    #[test]
    fn an_agent_never_picks_an_approvals_lasting_choice() {
        let codex: Vec<String> = [
            "› 1. Yes, proceed (y)",
            "  2. Yes, and don't ask again for commands that start with `curl -sI` (p)",
            "  3. No, and tell Codex what to do differently (esc)",
        ]
        .map(String::from)
        .to_vec();
        assert!(!lasting_choice(&codex, 1));
        assert!(lasting_choice(&codex, 2));
        assert!(!lasting_choice(&codex, 3));
        let claude: Vec<String> = ["❯ 1. Yes", "  2. Yes, and don’t ask again for uv commands in /w", "  3. No"].map(String::from).to_vec();
        assert!(lasting_choice(&claude, 2));
    }

    #[test]
    fn a_question_is_answered_by_an_options_number_or_text() {
        assert_eq!(answer_keys("", Some(2)), Ok(vec![Step::Type(b"2".to_vec())]));
        assert!(answer_keys("", Some(12)).is_err());
        assert!(answer_keys("  ", None).is_err());
        assert_eq!(answer_keys("use the cache", None).unwrap().len(), 3, "pasted, a pause, then Enter");
    }

    #[test]
    fn a_giver_hears_each_piece_of_news_once_and_not_while_it_waits() {
        let mut t = Track::default();
        t.starting(Some(9));
        let report = |state: State| Report {
            id: 1,
            agent: None,
            title: String::new(),
            folder: Default::default(),
            state,
            reply: None,
            screen: None,
            asks: None,
        };
        assert_eq!(t.to_tell(&report(State::Working)), None, "no news while it works");
        assert_eq!(t.to_tell(&report(State::Blocked)), Some(9));
        t.told("blocked");
        assert_eq!(t.to_tell(&report(State::Blocked)), None);
        assert_eq!(t.to_tell(&report(State::Done)), Some(9));
        // A turn that just ended is let settle first.
        t.on_hook(&hook(json!({ "hook_event_name": "Stop" })));
        assert_eq!(t.to_tell(&report(State::Done)), None);
        t.stopped = Some(Instant::now() - Duration::from_secs(2));
        assert_eq!(t.to_tell(&report(State::Done)), Some(9));
        // The giver reads it: it has heard, and waits on it for a while.
        t.read(&report(State::Done));
        assert_eq!(t.to_tell(&report(State::Done)), None);
        t.gave(None);
        t.read_at = None;
        assert_eq!(t.to_tell(&report(State::Done)), Some(9), "more of the task: its end is news again");
        assert_eq!(Track::default().to_tell(&report(State::Done)), None, "no giver: no one to tell");
    }

    #[test]
    fn the_wake_line_names_the_task_and_how_to_read_it() {
        let report = Report {
            id: 7,
            agent: Some(tern_agent::AgentId::new("codex")),
            title: String::new(),
            folder: Default::default(),
            state: State::Done,
            reply: None,
            screen: None,
            asks: None,
        };
        assert_eq!(wake_line(&report, "tern"), "[Tern] Task 7 (Codex) is done. Run `tern --task read 7` to see it.");
    }

    #[test]
    fn an_interrupt_ends_the_turn_without_an_answer() {
        let mut t = Track::default();
        t.asked(Instant::now());
        t.on_hook(&hook(json!({ "hook_event_name": "Interrupt" })));
        assert_eq!(hooked(&t, ActivityStatus::None, Instant::now()), State::Done);
        assert_eq!(t.reply.as_deref(), Some(""));
    }
}
