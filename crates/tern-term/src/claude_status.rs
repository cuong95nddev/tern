//! What a Claude Code tab is doing, from its hooks: a port of Orca's Claude
//! hook lane (`refs/orca/src/shared/agent-hook-listener/providers/claude-*.ts`,
//! `claude-owed-task-notifications.ts`, `claude-subagent-roster.ts`,
//! `claude-background-task-inventory.ts`, `agent-lead-status-fold.ts`).
//!
//! The main agent's own turn is one fact; the work it leaves running is
//! another. A turn that ended while a sub-agent works, or a background shell,
//! Monitor or session cron runs, is not finished: the tab stays working (or
//! monitoring, for watch loops only). Nor is a task that stopped running:
//! Claude owes the main agent a `<task-notification>` for it, which starts
//! another main-agent turn, up to a whole turn later and after a Stop whose
//! inventory is already empty. Nothing says the queue is empty, so an owed
//! notification is waited on for a lease after the main agent went idle.
//!
//! Times are milliseconds since the Unix epoch; the tracker serializes, so a
//! daemon taking over a session takes its tracker along.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_status::{AgentStatus, Held, MainAgent, Outcome, Subagent, TurnState, Wakeup};

/// How long an idle main agent is still expected to be woken by a
/// notification it is owed. Deliveries land 40–130 ms after the main agent
/// goes idle, but hooks are separate posts that can be lost or come out of
/// order; Claude's own idle prompt fires 60 s after the last Stop.
pub const OWED_NOTIFICATION_LEASE_MS: u64 = 60_000;

/// A session can launch tasks without bound; past this, settled ones are
/// forgotten first, and a launch that finds none is not tracked rather than
/// displacing a task still being waited on.
const TASK_LIMIT: usize = 256;

/// Longer sub-agent ids are not tracked.
const SUBAGENT_ID_MAX: usize = 64;

/// What Claude starts a parent agent's turn with to tell it of a background
/// task.
const TASK_NOTIFICATION: &str = "<task-notification>";

/// The turn after a manual or auto compact: it has no Stop of its own.
const COMPACT_CONTINUATION: &str = "this session is being continued from a previous conversation";

/// Task statuses in `background_tasks` that say a task is over.
const TERMINAL_STATUSES: &[&str] = &[
    "idle",
    "done",
    "success",
    "succeeded",
    "complete",
    "completed",
    "finished",
    "failed",
    "error",
    "terminated",
    "exited",
    "aborted",
    "expired",
    "skipped",
    "crashed",
    "killed",
    "cancelled",
    "canceled",
    "timed_out",
];

/// The main agent's turn as Claude's hooks told it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Lead {
    state: TurnState,
    /// A known background task opened this turn.
    task_wakeup: bool,
    outcome: Option<Outcome>,
    since: u64,
    /// The sub-agent whose request for you displaced the main agent's
    /// state; only its next tool event clears it.
    waiting_agent: Option<String>,
    /// The tool call that waits; late completions of parallel siblings must
    /// not clear it.
    waiting_tool_use: Option<String>,
    /// When the turn ended while child work kept the tab working.
    turn_completed_at: Option<u64>,
    /// The main agent's state a child's wait displaced.
    before_wait: Option<Stash>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Stash {
    state: TurnState,
    outcome: Option<Outcome>,
    since: u64,
    turn_completed_at: Option<u64>,
    task_wakeup: bool,
}

impl Lead {
    fn new(state: TurnState) -> Self {
        Self {
            state,
            task_wakeup: false,
            outcome: None,
            since: 0,
            waiting_agent: None,
            waiting_tool_use: None,
            turn_completed_at: None,
            before_wait: None,
        }
    }

    fn from_stash(s: Stash) -> (Self, Option<u64>) {
        let lead = Self { outcome: s.outcome, turn_completed_at: s.turn_completed_at, task_wakeup: s.task_wakeup, ..Self::new(s.state) };
        (lead, Some(s.since))
    }

    fn stash(&self) -> Stash {
        Stash {
            state: self.state,
            outcome: self.outcome,
            since: self.since,
            turn_completed_at: self.turn_completed_at,
            task_wakeup: self.task_wakeup,
        }
    }

    /// The main agent's own state: what a child's wait displaced, if one
    /// waits.
    fn own(&self) -> Option<Stash> {
        if self.waiting_agent.is_some() { self.before_wait } else { Some(self.stash()) }
    }

    /// When its turn last ended, while it has not started another.
    fn idle_since(&self) -> Option<u64> {
        (self.state == TurnState::Done).then_some(self.since)
    }
}

/// A live sub-agent or teammate, keyed by its `agent_id`. One-shot children
/// (hyphen-free ids) are tracked while working: their SubagentStop means
/// finished. Teammate-shaped ids (`a<name>-<hex>`) end only a turn at each
/// SubagentStop / TeammateIdle and stay resumable, so they park idle; idle
/// rows never keep the tab working.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Child {
    agent_type: Option<String>,
    description: Option<String>,
    started: u64,
    working: bool,
    /// A TeammateIdle matched it by name: a persistent teammate, not a
    /// workflow lane that merely has the id shape.
    confirmed_teammate: bool,
    /// Known only from `background_tasks`, not lifecycle events: a later
    /// complete inventory omitting it removes it.
    inventory_authoritative: bool,
    /// A subagent-typed task listed this id exactly: a later complete
    /// inventory omitting it means it ended, whatever its shape.
    listed_as_task: bool,
}

/// A background task the main agent launched, keyed by the id Claude
/// reports at launch and repeats in its `<task-notification>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Task {
    id: String,
    /// A sub-agent; else a shell (or a Monitor, notified like one).
    agent: bool,
    /// A current child ended before its main-agent launch was seen.
    launch_unconfirmed: bool,
    /// When it was seen to end, while its notification is owed.
    owed_at: Option<u64>,
    /// A sub-agent announced (or given up on), kept as it can be resumed.
    settled: bool,
    /// Its notification came (not merely given up on).
    delivered: bool,
}

/// What `background_tasks` (on Stop and SubagentStop) lists.
#[derive(Debug, Default)]
struct Inventory {
    /// `false`: no list (an older Claude); the tracked state stands.
    present: bool,
    agents: Vec<InventoryAgent>,
    truncated: bool,
    running_non_agent: bool,
    running_non_agent_ids: Vec<String>,
}

#[derive(Debug)]
struct InventoryAgent {
    id: String,
    agent_type: Option<String>,
    description: Option<String>,
    running: bool,
    /// `type: "teammate"`: its id never matches lifecycle ids and it reads
    /// "running" for good, so it says nothing per agent.
    teammate: bool,
}

/// One Claude tab's hook state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeTracker {
    /// The conversation whose hooks these are.
    session: Option<String>,
    lead: Option<Lead>,
    roster: Vec<(String, Child)>,
    tasks: Vec<Task>,
    /// The last inventory listed a running shell (or an unknown task).
    running_non_agent: bool,
    cron: bool,
    /// The main agent's last tool.
    last_tool: Option<String>,
}

fn read_string<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn bare_tool(tool: Option<&str>) -> Option<String> {
    tool.map(|t| t.chars().filter(char::is_ascii_alphanumeric).collect::<String>().to_lowercase())
}

/// The tool by which an agent asks the user a question and waits for the
/// answer (Claude's `AskUserQuestion`, Codex's `request_user_input`).
pub fn is_ask_user_question(tool: Option<&str>) -> bool {
    bare_tool(tool).is_some_and(|b| b == "askuserquestion" || b == "requestuserinput")
}

/// The tool by which Codex asks the user a question without waiting: it
/// returns at once (`{"accepted": true}`) and the turn goes on, sleeping,
/// until the user answers on its screen.
pub fn is_async_question(tool: Option<&str>) -> bool {
    bare_tool(tool).is_some_and(|b| b == "requestuserinputasync")
}

fn is_compact_continuation(prompt: &str) -> bool {
    prompt.trim_start().get(..COMPACT_CONTINUATION.len()).is_some_and(|head| head.eq_ignore_ascii_case(COMPACT_CONTINUATION))
}

/// Teammate lifecycle ids are `a<name>-<hex>`, one-shot ids hyphen-free.
fn is_teammate_id(id: &str) -> bool {
    id.rfind('-').is_some_and(|sep| {
        let hex = &id[sep + 1..];
        sep > 1 && id.starts_with('a') && !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit())
    })
}

/// Whether teammate `name`'s ids include `id`: `a<name>-<hex>`, so "rev"
/// doesn't match "rev-two"'s.
fn teammate_id_matches(id: &str, name: &str) -> bool {
    id.strip_prefix('a').and_then(|r| r.strip_prefix(name)).and_then(|r| r.strip_prefix('-')).is_some_and(|rest| !rest.contains('-'))
}

/// The task a notification turn (`prompt`) is about, and its status (only
/// once the task ended: a Monitor's `<event>` turns reuse the tag while it
/// runs). The status is written before `<summary>`; anything after is task
/// output, which may hold a status tag of its own.
fn read_task_notification(prompt: &str) -> Option<(String, Option<String>)> {
    if !prompt.starts_with(TASK_NOTIFICATION) {
        return None;
    }
    let tag = |text: &str, name: &str| -> Option<String> {
        let open = format!("<{name}>");
        let start = text.find(&open)? + open.len();
        let end = text[start..].find(&format!("</{name}>"))? + start;
        Some(text[start..end].to_owned())
    };
    let id = tag(prompt, "task-id")?.trim().to_owned();
    if id.is_empty() {
        return None;
    }
    let head = prompt.find("<summary>").map_or(prompt, |at| &prompt[..at]);
    let status = tag(head, "status").filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
    Some((id, status))
}

/// The task a main-agent PostToolUse left running in the background: an
/// async sub-agent's `agentId`, a shell's `backgroundTaskId`, a Monitor's
/// `taskId` (listed and notified like a shell).
fn read_launch(tool: Option<&str>, response: &Value) -> Option<(String, bool)> {
    let response = response.as_object()?;
    if response.get("isAsync") == Some(&Value::Bool(true))
        && let Some(id) = response.get("agentId").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
    {
        return Some((id.to_owned(), true));
    }
    let shell = response.get("backgroundTaskId").or(if tool == Some("Monitor") { response.get("taskId") } else { None });
    let id = shell.and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())?;
    Some((id.to_owned(), false))
}

fn read_inventory(raw: &Value) -> Inventory {
    let Some(list) = raw.get("background_tasks").and_then(Value::as_array) else { return Inventory::default() };
    let mut inv = Inventory { present: true, ..Default::default() };
    for item in list {
        let Some(obj) = item.as_object() else {
            inv.truncated = true;
            inv.running_non_agent = true;
            continue;
        };
        let text = |k: &str| obj.get(k).and_then(Value::as_str).map(|s| s.trim().to_lowercase()).unwrap_or_default();
        let (kind, status) = (text("type"), text("status"));
        let terminal = !status.is_empty() && TERMINAL_STATUSES.contains(&status.as_str());
        if kind.is_empty() {
            inv.truncated = true;
            inv.running_non_agent |= !terminal;
            continue;
        }
        let agent = matches!(kind.as_str(), "local_agent" | "local_subagent" | "subagent" | "teammate");
        let id = obj.get("id").and_then(Value::as_str).map(str::trim).unwrap_or("");
        // Unknown kinds and statuses count as running: only typed agents or
        // a known end can retire work.
        if !agent && !terminal {
            inv.running_non_agent = true;
            if !id.is_empty() {
                inv.running_non_agent_ids.push(id.to_owned());
            }
        }
        if !agent {
            continue;
        }
        if id.is_empty() {
            inv.truncated = true;
            continue;
        }
        let field = |k: &str| obj.get(k).and_then(Value::as_str).map(str::to_owned);
        inv.agents.push(InventoryAgent {
            id: id.to_owned(),
            agent_type: field("agent_type"),
            description: field("description"),
            running: !terminal,
            teammate: kind == "teammate",
        });
    }
    inv
}

/// Events that may change which conversation a tab's hooks belong to; one
/// that can't be told apart (unknown, a child's without `agent_id`) voids
/// nothing.
const SESSION_OWNER_EVENTS: &[&str] =
    &["SessionStart", "UserPromptSubmit", "Stop", "StopFailure", "PreToolUse", "PostToolUse", "PostToolUseFailure", "PermissionRequest"];

impl ClaudeTracker {
    /// Take hook payload `raw` at `now`; the tab's status when the event
    /// tells one. `stop_blocked`: Tern's own Stop hook answered it with a
    /// block, so the turn goes on.
    pub fn on_event(&mut self, raw: &Value, now: u64, stop_blocked: bool) -> Option<AgentStatus> {
        let event = read_string(raw, "hook_event_name").unwrap_or("");
        let agent_id = read_string(raw, "agent_id").map(str::to_owned);
        if matches!(event, "SubagentStart" | "TeammateIdle" | "SubagentStop") || (event == "StopFailure" && agent_id.is_some()) {
            return self.lifecycle(event, raw, now);
        }
        self.void_replaced_session(event, agent_id.as_deref(), raw);
        if event == "SessionStart" {
            // Allow-listed idle boundaries only: a compact restart (or an
            // unknown source) comes mid-turn, and a child's mustn't end the
            // main agent's turn.
            if agent_id.is_some() || !matches!(read_string(raw, "source"), Some("startup" | "resume" | "clear")) {
                return None;
            }
            // A new process owns the tab: its children, tasks and crons are
            // not this one's.
            self.roster.clear();
            self.running_non_agent = false;
            self.cron = false;
            self.tasks.clear();
            self.set_lead(Lead::new(TurnState::Done), Some(now), now);
            self.last_tool = None;
            let resolution = self.resolve(TurnState::Done, false, None, now);
            return Some(self.build(resolution, true));
        }
        if event == "SessionEnd" {
            return None;
        }
        let prev = self.lead.clone();
        let boundary = matches!(event, "Stop" | "StopFailure") && !stop_blocked;
        // Absent means unknown: a plain Stop is never a success, so a cancel
        // can't read as one. Claude sends no hook on a cancel; it comes from
        // [`Self::interrupted`] and is carried to the turn's boundary.
        let outcome = if !boundary {
            None
        } else if (agent_id.is_none() && raw.get("is_interrupt") == Some(&Value::Bool(true)))
            || prev.as_ref().is_some_and(|p| p.outcome == Some(Outcome::Cancellation))
        {
            Some(Outcome::Cancellation)
        } else if event == "StopFailure" {
            Some(Outcome::Failure)
        } else {
            None
        };
        let inventory = read_inventory(raw);
        let task_wakeup = agent_id.is_none() && self.track_delivery(event, raw, &inventory, now);
        let crons = raw.get("session_crons").and_then(Value::as_array);
        let prompt = read_string(raw, "prompt").unwrap_or("");
        if event == "UserPromptSubmit" && is_compact_continuation(prompt) {
            return None;
        }
        let tool = read_string(raw, "tool_name");
        // AskUserQuestion is pending while its PreToolUse is (newer builds
        // also send a PermissionRequest for it).
        let ask_wait = matches!(event, "PreToolUse" | "PermissionRequest") && is_ask_user_question(tool);
        let ask = event == "PreToolUse" && ask_wait;
        // A manual /compact ends at the prompt with no Stop; an auto compact
        // runs inside a turn with a Stop of its own.
        let manual_compact = event == "PostCompact" && read_string(raw, "trigger") == Some("manual");
        let reported = match event {
            "UserPromptSubmit" | "PostToolUse" | "PostToolUseFailure" => TurnState::Working,
            "PreToolUse" if !ask => TurnState::Working,
            "PermissionRequest" => TurnState::Waiting,
            _ if ask => TurnState::Waiting,
            "Stop" | "StopFailure" if stop_blocked => TurnState::Working,
            _ if boundary || manual_compact => TurnState::Done,
            _ => return None,
        };
        if inventory.present && agent_id.is_none() {
            self.running_non_agent = inventory.running_non_agent;
        }
        if let (Some(crons), None) = (crons, &agent_id) {
            self.cron = !crons.is_empty();
        } else if agent_id.is_none() && boundary && inventory.present {
            // Current Claude may leave out an empty cron list.
            self.cron = false;
        }
        let tool_use = read_string(raw, "tool_use_id").or_else(|| read_string(raw, "toolUseId")).map(str::to_owned);
        let differs = |waiting: &Option<String>| waiting.is_some() && tool_use.is_some() && *waiting != tool_use;
        let post = matches!(event, "PostToolUse" | "PostToolUseFailure");
        // A parallel sibling finishing while AskUserQuestion waits.
        if agent_id.is_none()
            && post
            && let Some(p) = &prev
            && p.state == TurnState::Waiting
            && is_ask_user_question(self.last_tool.as_deref())
            && differs(&p.waiting_tool_use)
        {
            return self.cached(Evidence::default(), now);
        }
        // A child's tool activity keeps it live but is not the main agent's
        // state; except what needs you, and the waiting child's own next
        // tool event (its approval came), which clear as the main agent's
        // would.
        let waiting_inducing = reported == TurnState::Waiting;
        let child_origin =
            (!waiting_inducing && matches!(event, "PreToolUse" | "PostToolUse" | "PostToolUseFailure")).then(|| agent_id.clone()).flatten();
        if let Some(id) = &agent_id
            && (child_origin.is_some() || waiting_inducing)
        {
            self.upsert_working(id, read_string(raw, "agent_type"), None, now);
        }
        if let Some(origin) = child_origin {
            let lead = self.lead.clone();
            let Some(lead) = lead.filter(|l| l.state == TurnState::Waiting && l.waiting_agent.as_deref() == Some(&origin)) else {
                return self.cached(Evidence { working_child: true, ..Default::default() }, now);
            };
            if post && differs(&lead.waiting_tool_use) {
                return self.cached(Evidence { working_child: true, ..Default::default() }, now);
            }
            // Approval came: back to what the wait displaced (the main agent
            // may be done already).
            let (restored, since) = lead.before_wait.map_or((Lead::new(TurnState::Working), None), Lead::from_stash);
            self.set_lead(restored, since, now);
            self.note_tool(event, tool);
            let lead = self.lead.clone().expect("just set");
            let resolution = self.resolve_lead(&lead, now);
            return Some(self.build(resolution, false));
        }
        // Main-agent events never carry `agent_id`: a child missed by the
        // lifecycle can't own the main agent's turn.
        if agent_id.is_some() && !waiting_inducing {
            let working_child = self.has_working_child();
            return self.cached(Evidence { working_child, ..Default::default() }, now);
        }
        if boundary && agent_id.is_none() && inventory.present {
            self.fold_inventory(&inventory.agents, now, !inventory.truncated);
        }
        // A child's wait displaces the main agent's state; stash it to go
        // back to. A second child's wait keeps the first stash.
        let before_wait = match (&prev, &agent_id) {
            (Some(p), Some(_)) if waiting_inducing => {
                if p.state == TurnState::Waiting {
                    p.before_wait
                } else {
                    Some(p.stash())
                }
            },
            _ => None,
        };
        let waiting_tool_use = tool_use.clone().or_else(|| prev.as_ref().and_then(|p| p.waiting_tool_use.clone()));
        let wakeup_turn = if event == "UserPromptSubmit" {
            task_wakeup
        } else {
            !boundary && !manual_compact && prev.as_ref().is_some_and(|p| p.task_wakeup)
        };
        let resolution = self.resolve(reported, wakeup_turn, None, now);
        // The main agent ended and running child work keeps the tab working:
        // that end is this turn's to announce. Not a cancel, and not when only
        // an owed notification holds it (the wake-up turn follows within
        // ~100 ms, with its own end).
        let turn_completed_at = (agent_id.is_none()
            && boundary
            && outcome != Some(Outcome::Cancellation)
            && reported == TurnState::Done
            && resolution.state != TurnState::Done
            && self.has_running_child_work())
        .then_some(now);
        let lead = Lead {
            task_wakeup: wakeup_turn,
            outcome,
            waiting_agent: agent_id.clone().filter(|_| waiting_inducing),
            waiting_tool_use: waiting_tool_use.filter(|_| ask_wait),
            before_wait,
            turn_completed_at,
            ..Lead::new(reported)
        };
        self.set_lead(lead, None, now);
        // A finished compact clears the tab or says nothing.
        if manual_compact && resolution.state != TurnState::Done {
            return None;
        }
        self.note_tool(event, tool);
        Some(self.build(resolution, manual_compact))
    }

    /// Claude's turn was cancelled (Esc, Ctrl+C: it sends no hook; Codex does): the main
    /// agent's turn is done, as a cancellation. The work it left running
    /// stays until it reports its own end.
    pub fn interrupted(&mut self, now: u64) -> Option<AgentStatus> {
        self.lead.as_ref()?;
        self.set_lead(Lead { outcome: Some(Outcome::Cancellation), ..Lead::new(TurnState::Done) }, None, now);
        self.last_tool = None;
        let lead = self.lead.clone().expect("just set");
        let resolution = self.resolve_lead(&lead, now);
        Some(self.build(resolution, false))
    }

    /// An AskUserQuestion was answered or dismissed (no hook says so):
    /// back to what the wait displaced.
    pub fn question_answered(&mut self, now: u64) -> Option<AgentStatus> {
        let lead = self.lead.clone()?;
        let (restored, since) = match lead.state {
            TurnState::Waiting => {
                lead.before_wait.map_or((Lead { task_wakeup: lead.task_wakeup, ..Lead::new(TurnState::Working) }, None), Lead::from_stash)
            },
            _ => (Lead::new(TurnState::Working), None),
        };
        self.set_lead(restored, since, now);
        self.last_tool = None;
        let lead = self.lead.clone().expect("just set");
        let resolution = self.resolve_lead(&lead, now);
        Some(self.build(resolution, false))
    }

    /// Whether the main agent itself is in a turn (or waits for you on its
    /// own account).
    pub fn main_agent_busy(&self) -> bool {
        self.lead.as_ref().is_some_and(|l| l.state == TurnState::Working || (l.state == TurnState::Waiting && l.waiting_agent.is_none()))
    }

    /// Whether the tab waits on an AskUserQuestion.
    pub fn asking(&self) -> bool {
        self.lead.as_ref().is_some_and(|l| l.state == TurnState::Waiting) && is_ask_user_question(self.last_tool.as_deref())
    }

    /// The status now, when there is one: what a lease expiring changes.
    pub fn current(&mut self, now: u64) -> Option<AgentStatus> {
        self.cached(Evidence::default(), now)
    }

    /// When the last notification an idle main agent is still owed stops
    /// being waited for; then [`Self::current`] tells the new status.
    pub fn deadline(&self) -> Option<u64> {
        let idle_since = self.lead.as_ref()?.idle_since()?;
        self.tasks
            .iter()
            .filter(|t| !t.launch_unconfirmed)
            .filter_map(|t| t.owed_at)
            .map(|owed| owed.max(idle_since) + OWED_NOTIFICATION_LEASE_MS)
            .max()
    }

    /// Whether any hook of a main agent was seen.
    pub fn knows(&self) -> bool {
        self.lead.is_some()
    }

    // ---- the main agent's record ----

    /// The only writer of the main agent's record. Its clock goes on across
    /// writes of the same state; `since` (a stash's) wins.
    fn set_lead(&mut self, mut next: Lead, since: Option<u64>, now: u64) {
        let prev = self.lead.as_ref();
        next.since = since.unwrap_or_else(|| match prev {
            Some(p) if p.state == next.state => p.since,
            _ => now,
        });
        if next.state != TurnState::Done {
            next.outcome = None;
        }
        self.lead = Some(next);
    }

    fn note_tool(&mut self, event: &str, tool: Option<&str>) {
        if event == "UserPromptSubmit" {
            self.last_tool = None;
        }
        if let Some(t) = tool {
            self.last_tool = Some(t.to_owned());
        }
    }

    /// A child's wait ended with it: back to what it displaced.
    fn clear_wait_of(&mut self, owns: impl Fn(&str) -> bool, now: u64) {
        let Some(lead) = self.lead.clone() else { return };
        if lead.state != TurnState::Waiting || !lead.waiting_agent.as_deref().is_some_and(&owns) {
            return;
        }
        let (restored, since) = lead.before_wait.map_or((Lead::new(TurnState::Working), None), Lead::from_stash);
        self.set_lead(restored, since, now);
        self.last_tool = None;
    }

    /// A tab whose `session_id` changed runs another conversation: void what
    /// the previous one owned. Not a running shell (an OS process outlives
    /// /clear; only an inventory retires it), nor confirmed teammates.
    fn void_replaced_session(&mut self, event: &str, agent_id: Option<&str>, raw: &Value) {
        if agent_id.is_some() || !SESSION_OWNER_EVENTS.contains(&event) {
            return;
        }
        // Compact and unknown starts are ignored: they mustn't move the anchor.
        if event == "SessionStart" && !matches!(read_string(raw, "source"), Some("startup" | "resume" | "clear")) {
            return;
        }
        let Some(session) = read_string(raw, "session_id") else { return };
        let prev = self.session.replace(session.to_owned());
        if prev.is_none() || prev.as_deref() == Some(session) || event == "SessionStart" {
            return;
        }
        self.cron = false;
        // Its notifications will never reach this conversation.
        self.tasks.clear();
        self.roster.retain(|(_, c)| c.confirmed_teammate);
    }

    // ---- what holds the tab ----

    /// The owed notifications still waited for, once those an idle main
    /// agent was owed for a whole lease are given up on: (agent, shell).
    /// `main_idle`: the main agent's state as of this event is done.
    fn owed(&mut self, main_idle: bool, now: u64) -> (bool, bool) {
        if main_idle {
            let idle_since = self.lead.as_ref().and_then(Lead::idle_since).unwrap_or(now);
            let expired: Vec<String> = self
                .tasks
                .iter()
                .filter(|t| !t.launch_unconfirmed)
                .filter(|t| t.owed_at.is_some_and(|owed| owed.max(idle_since) + OWED_NOTIFICATION_LEASE_MS <= now))
                .map(|t| t.id.clone())
                .collect();
            for id in expired {
                self.stop_owing(&id);
            }
        }
        let owed = |agent: bool| self.tasks.iter().any(|t| t.agent == agent && !t.launch_unconfirmed && t.owed_at.is_some());
        (owed(true), owed(false))
    }

    fn has_working_child(&self) -> bool {
        self.roster.iter().any(|(_, c)| c.working)
    }

    /// Child work running now (and maybe for long), as opposed to a
    /// notification the main agent is about to be woken by.
    fn has_running_child_work(&self) -> bool {
        self.has_working_child() || self.running_non_agent || self.cron
    }

    fn held(&mut self, main_state: TurnState, now: u64) -> (Held, bool, bool) {
        let (owed_agent, owed_shell) = self.owed(main_state == TurnState::Done, now);
        let held = Held {
            agents: self.roster.iter().filter(|(_, c)| c.working).count(),
            shells: self.running_non_agent,
            cron: self.cron,
            owed: owed_agent || owed_shell,
        };
        (held, owed_agent, owed_shell)
    }

    /// Fold the main agent's state with its child work: its request for you
    /// wins; a main agent in a turn is that; one that is done with sub-agents
    /// at work (or owed) is working, with only watch loops monitoring.
    /// `own` is the main agent's own state when a child's wait displaced
    /// it (`None` inside: none to go back to); by default `lead`'s.
    fn resolve(&mut self, lead: TurnState, task_wakeup: bool, own: Option<Option<Stash>>, now: u64) -> Resolution {
        let (held, owed_agent, owed_shell) = self.held(lead, now);
        let agent_work = held.agents > 0 || owed_agent;
        let non_agent_work = held.shells || held.cron || owed_shell;
        let (state, monitoring) = match lead {
            TurnState::Waiting => (TurnState::Waiting, false),
            TurnState::Working => (TurnState::Working, false),
            TurnState::Done if agent_work => (TurnState::Working, false),
            TurnState::Done if non_agent_work => (TurnState::Working, true),
            TurnState::Done => (TurnState::Done, false),
        };
        let own = own.unwrap_or(Some(Stash { state: lead, outcome: None, since: 0, turn_completed_at: None, task_wakeup }));
        let wakeup = if owed_agent || owed_shell {
            Some(Wakeup::Notification)
        } else if own.is_some_and(|o| o.state != TurnState::Done && o.task_wakeup) {
            Some(Wakeup::FinishingTurn)
        } else {
            None
        };
        Resolution { state, monitoring, wakeup, held }
    }

    fn resolve_lead(&mut self, lead: &Lead, now: u64) -> Resolution {
        self.resolve(lead.state, lead.task_wakeup, Some(lead.own()), now)
    }

    /// The main agent's last state again with the current child list:
    /// what child churn and lease expiry tell.
    fn cached(&mut self, evidence: Evidence, now: u64) -> Option<AgentStatus> {
        let lead = match self.lead.clone() {
            Some(l) => l,
            // A child at work or ending wakes its parent; only a main-agent
            // boundary can prove the whole tab done.
            None if evidence.working_child || evidence.ended_child => Lead::new(TurnState::Working),
            None => return None,
        };
        let resolution = self.resolve_lead(&lead, now);
        Some(self.build(resolution, false))
    }

    fn build(&self, r: Resolution, session_boundary: bool) -> AgentStatus {
        let lead = self.lead.as_ref();
        let main_agent = lead.and_then(Lead::own).map(|o| MainAgent {
            state: o.state,
            outcome: o.outcome.filter(|_| o.state == TurnState::Done),
            since: o.since,
        });
        let mut subagents: Vec<Subagent> = self
            .roster
            .iter()
            .map(|(id, c)| Subagent {
                id: id.clone(),
                agent_type: c.agent_type.clone(),
                description: c.description.clone(),
                started: c.started,
                working: c.working,
            })
            .collect();
        subagents.sort_by(|a, b| a.started.cmp(&b.started).then_with(|| a.id.cmp(&b.id)));
        AgentStatus {
            state: r.state,
            monitoring: r.monitoring,
            wakeup: if r.state == TurnState::Done { None } else { r.wakeup },
            main_agent,
            turn_completed_at: lead.and_then(|l| l.turn_completed_at),
            subagents,
            session_boundary,
            held: r.held,
        }
    }

    // ---- sub-agents ----

    /// SubagentStart, SubagentStop, TeammateIdle, and a child's StopFailure
    /// (an API error skips its SubagentStop): the roster changes, and the
    /// main agent's last state goes out again with it.
    fn lifecycle(&mut self, event: &str, raw: &Value, now: u64) -> Option<AgentStatus> {
        let id = read_string(raw, if event == "TeammateIdle" { "teammate_name" } else { "agent_id" })?.to_owned();
        let owner = self.session.clone();
        let current = owner.is_some() && owner.as_deref() == read_string(raw, "session_id");
        let compatible = owner.is_none() || current;
        let mut ended_child = false;
        let mut ended_runtime_child = false;
        if event == "TeammateIdle" {
            // A teammate's turn is over; it waits for mail, alive.
            let was_working = self.roster.iter().any(|(rid, c)| c.working && teammate_id_matches(rid, &id));
            ended_runtime_child = was_working;
            for (rid, c) in &mut self.roster {
                if teammate_id_matches(rid, &id) {
                    c.inventory_authoritative = false;
                    c.working = false;
                    c.confirmed_teammate = true;
                }
            }
            ended_child = was_working;
            self.clear_wait_of(|w| teammate_id_matches(w, &id), now);
        } else if event == "SubagentStart" {
            if compatible {
                self.mark_agent_running(&id);
            }
            self.upsert_working(&id, read_string(raw, "agent_type"), None, now);
        } else {
            if let Some(was_working) = self.child(&id).map(|c| c.working) {
                ended_runtime_child = was_working;
                self.stop_child(&id);
                ended_child = was_working && !self.child(&id).is_some_and(|c| c.working);
            }
            // A teammate parked idle only ended a turn: it owes nothing.
            if compatible && !self.child(&id).is_some_and(|c| !c.working) {
                if current && ended_runtime_child {
                    self.record_unconfirmed_agent_end(&id, now);
                }
                self.owe_agent(&id, now);
            }
            // A waiting child that dies would pin its wait on the tab forever.
            self.clear_wait_of(|w| w == id, now);
        }
        let working_child = self.has_working_child();
        self.cached(Evidence { working_child, ended_child: ended_child || ended_runtime_child }, now)
    }

    fn child(&self, id: &str) -> Option<&Child> {
        self.roster.iter().find(|(k, _)| k == id).map(|(_, c)| c)
    }

    fn child_mut(&mut self, id: &str) -> Option<&mut Child> {
        self.roster.iter_mut().find(|(k, _)| k == id).map(|(_, c)| c)
    }

    fn upsert_working(&mut self, id: &str, agent_type: Option<&str>, description: Option<&str>, now: u64) {
        if id.is_empty() || id.len() > SUBAGENT_ID_MAX {
            return;
        }
        if let Some(c) = self.child_mut(id) {
            c.working = true;
            c.agent_type = agent_type.map(str::to_owned).or(c.agent_type.take());
            c.description = description.map(str::to_owned).or(c.description.take());
            // Live activity: the lifecycle owns the id again.
            c.inventory_authoritative = false;
            return;
        }
        self.roster.push((
            id.to_owned(),
            Child {
                agent_type: agent_type.map(str::to_owned),
                description: description.map(str::to_owned),
                started: now,
                working: true,
                confirmed_teammate: false,
                inventory_authoritative: false,
                listed_as_task: false,
            },
        ));
    }

    /// SubagentStop: a one-shot child finished (it leaves); a teammate only
    /// ended a turn (it parks idle), unless an inventory proved it a lane.
    fn stop_child(&mut self, id: &str) {
        let Some(c) = self.child_mut(id) else { return };
        if !is_teammate_id(id) || c.listed_as_task {
            self.roster.retain(|(k, _)| k != id);
            return;
        }
        c.inventory_authoritative = false;
        c.working = false;
    }

    /// Fold a main-agent Stop's `background_tasks` into the roster: an
    /// empty complete list proves nothing is alive; a listed sub-agent is
    /// trusted (ended ones leave, unseen running ones are added); an
    /// unlisted one ended, unless it is a teammate the list can't name
    /// (teammate-typed tasks listed, teammate-shaped, never listed itself,
    /// working or confirmed).
    fn fold_inventory(&mut self, tasks: &[InventoryAgent], now: u64, complete: bool) {
        if tasks.is_empty() {
            if complete {
                self.roster.clear();
            }
            return;
        }
        let has_teammate_task = tasks.iter().any(|t| t.teammate);
        let mut listed: Vec<&str> = Vec::new();
        for task in tasks.iter().filter(|t| !t.teammate) {
            listed.push(&task.id);
            if let Some(c) = self.child_mut(&task.id) {
                if !task.running {
                    self.roster.retain(|(k, _)| *k != task.id);
                    continue;
                }
                c.working = true;
                c.agent_type = task.agent_type.clone().or(c.agent_type.take());
                c.description = task.description.clone().or(c.description.take());
                c.listed_as_task = true;
                continue;
            }
            if !task.running {
                continue;
            }
            self.upsert_working(&task.id, task.agent_type.as_deref(), task.description.as_deref(), now);
            if let Some(c) = self.child_mut(&task.id) {
                c.inventory_authoritative = true;
                c.listed_as_task = true;
            }
        }
        if complete {
            self.roster.retain(|(id, c)| {
                listed.contains(&id.as_str())
                    || (has_teammate_task
                        && !c.inventory_authoritative
                        && !c.listed_as_task
                        && is_teammate_id(id)
                        && (c.working || c.confirmed_teammate))
            });
        }
    }

    // ---- background tasks and their notifications ----

    /// Fold a main-agent event into the record of launched tasks; whether
    /// it opens a turn for a known task's notification.
    fn track_delivery(&mut self, event: &str, raw: &Value, inventory: &Inventory, now: u64) -> bool {
        match event {
            "PostToolUse" => {
                let tool = read_string(raw, "tool_name");
                let response = raw.get("tool_response").unwrap_or(&Value::Null);
                if let Some((id, agent)) = read_launch(tool, response) {
                    self.record_launch(&id, agent);
                } else if tool == Some("TaskStop")
                    && response.is_object()
                    && let Some(id) = read_string(response, "task_id")
                {
                    // Claude sends no notification for a shell it was told
                    // to stop (a stopped sub-agent still gets one).
                    if self.task(id).is_some_and(|t| !t.agent) {
                        self.tasks.retain(|t| t.id != id);
                    }
                }
                false
            },
            "UserPromptSubmit" => {
                let Some((id, status)) = read_task_notification(read_string(raw, "prompt").unwrap_or("")) else {
                    return false;
                };
                // Its own notification can come before both its launch and end.
                if self.child(&id).is_some_and(|c| c.working) && self.task(&id).is_none() && self.make_room() {
                    self.tasks.push(Task::new(&id, true, true));
                }
                let known = self.task(&id).is_some_and(|t| !t.delivered);
                let own = self.lead.as_ref().and_then(Lead::own);
                // A repeated notification can't end its unfinished cycle.
                let continues = own.is_some_and(|o| o.state != TurnState::Done && o.task_wakeup);
                if status.is_some() {
                    self.settle(&id);
                }
                known || continues
            },
            "Stop" | "StopFailure" if inventory.present => {
                // A shell's end fires no hook: an inventory no longer
                // listing it is the only sign.
                for t in &mut self.tasks {
                    if !t.agent && !inventory.running_non_agent_ids.contains(&t.id) {
                        t.owed_at.get_or_insert(now);
                    }
                }
                false
            },
            _ => false,
        }
    }

    fn task(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    fn task_mut(&mut self, id: &str) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.id == id)
    }

    fn record_launch(&mut self, id: &str, agent: bool) {
        if let Some(t) = self.task_mut(id) {
            if t.agent == agent {
                t.launch_unconfirmed = false;
            }
            return;
        }
        if self.make_room() {
            self.tasks.push(Task::new(id, agent, false));
        }
    }

    /// Room for one more task: forget an unconfirmed launch, else a settled
    /// sub-agent; never one still waited on.
    fn make_room(&mut self) -> bool {
        if self.tasks.len() < TASK_LIMIT {
            return true;
        }
        if let Some(i) = self.tasks.iter().position(|t| t.launch_unconfirmed) {
            self.tasks.remove(i);
            return true;
        }
        if let Some(i) = self.tasks.iter().position(|t| t.settled) {
            self.tasks.remove(i);
            return true;
        }
        false
    }

    /// A current child ended before its launch was seen: owed only once the
    /// launch confirms it.
    fn record_unconfirmed_agent_end(&mut self, id: &str, now: u64) {
        if self.task(id).is_none() && self.make_room() {
            let mut t = Task::new(id, true, true);
            t.owed_at = Some(now);
            self.tasks.push(t);
        }
    }

    /// A recorded sub-agent resumed under its id: its next end may notify
    /// again.
    fn mark_agent_running(&mut self, id: &str) {
        if let Some(t) = self.task_mut(id).filter(|t| t.agent) {
            t.owed_at = None;
            t.settled = false;
            t.delivered = false;
        }
    }

    /// A launched sub-agent finished: the main agent is owed its
    /// notification.
    fn owe_agent(&mut self, id: &str, now: u64) {
        if let Some(t) = self.task_mut(id).filter(|t| t.agent && !t.settled) {
            t.owed_at.get_or_insert(now);
        }
    }

    /// Its notification came. A sub-agent can resume and notify again
    /// under its id, so it stays known; a shell is over.
    fn settle(&mut self, id: &str) {
        if self.task(id).is_some() {
            self.stop_owing(id);
            if let Some(t) = self.task_mut(id).filter(|t| t.agent) {
                t.delivered = true;
            }
        }
    }

    fn stop_owing(&mut self, id: &str) {
        match self.task_mut(id) {
            Some(t) if t.agent => {
                t.owed_at = None;
                t.settled = true;
            },
            Some(_) => self.tasks.retain(|t| t.id != id),
            None => {},
        }
    }
}

impl Task {
    fn new(id: &str, agent: bool, launch_unconfirmed: bool) -> Self {
        Self { id: id.to_owned(), agent, launch_unconfirmed, owed_at: None, settled: false, delivered: false }
    }
}

#[derive(Debug, Clone, Copy)]
struct Resolution {
    state: TurnState,
    monitoring: bool,
    wakeup: Option<Wakeup>,
    held: Held,
}

/// What a re-emitted state knows of child work when there is no main-agent
/// record yet.
#[derive(Debug, Default, Clone, Copy)]
struct Evidence {
    working_child: bool,
    ended_child: bool,
}

#[cfg(test)]
mod tests;
