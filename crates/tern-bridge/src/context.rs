//! Keeping a Claude conversation's context window in bounds. Past the limit
//! the user set, the agent is told so (a tool call's or prompt's hook
//! answer), and when it next ends its turn it is asked to write a handoff
//! first (its `Stop` hook is blocked). Once it has, the tab can switch to a
//! fresh conversation: `/clear`, then the new one is pointed at the handoff
//! (its `SessionStart` hook answer) and told to go on.
//!
//! The user can also ask for a handoff themselves ([`Guard::request`]), to
//! step away: it is asked for the same way, but the conversation then
//! stops, and goes on from the handoff only when they say
//! ([`Guard::resume_from`]).
//!
//! How full the window is comes from the status line's input (see
//! [`crate::statusline`]), the latest one before each hook. Headless Claude
//! (a chat) has no status line: [`Meter`] works it out from its output.
//!
//! Claude's steer: hook answers. Other agents' steer is added per agent
//! (spec §2 Steer).

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::{Value, json};

use crate::HookEvent;

/// What the user set: the share of the window past which a conversation
/// hands off, and whether the tab then switches to a fresh one by itself.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Policy {
    /// Percent of the window, 1 to 100; 0 for no limit.
    pub limit: u8,
    pub auto_switch: bool,
}

/// What the tab's terminal should be sent once the hook is answered.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    None,
    /// Clear the conversation (`/clear`), starting a fresh one.
    Clear,
    /// Submit this prompt.
    Prompt(String),
}

#[derive(Debug, Clone, Default, PartialEq)]
enum Phase {
    #[default]
    Under,
    /// The user asked for a handoff while it worked: it is told at its
    /// next hook.
    Requested,
    /// Told it is past the limit.
    Warned,
    /// Asked for a handoff when it tried to stop.
    Asked,
    /// It wrote the handoff (at the path it named, if it did).
    Written { handoff: Option<String>, clearing: bool },
}

/// One conversation's guard: how full its window is, and how far its
/// handoff has come.
#[derive(Debug, Clone, Default)]
pub struct Guard {
    percent: f32,
    phase: Phase,
    /// A fresh conversation, before its first reading.
    fresh: bool,
    /// It was past the limit at its first reading: a fresh one would be as
    /// full, so it isn't handed off (else it would hand off over and over).
    born_over: bool,
    /// The handoff under way is the user's (see [`Guard::request`]).
    by_user: bool,
}

/// The line the agent ends its handoff turn with.
const HANDOFF_LINE: &str = "Handoff:";

/// The prompt a fresh conversation gets to go on.
pub const CONTINUE_PROMPT: &str = "Continue the work from the handoff.";

impl Guard {
    /// How full the window is, in percent.
    pub fn percent(&self) -> f32 {
        self.percent
    }

    /// The window is `percent` full.
    pub fn reading(&mut self, percent: f32, policy: Policy) {
        self.percent = percent;
        if self.fresh && self.percent > 0. {
            self.fresh = false;
            self.born_over = self.over(policy);
        }
        if self.phase == Phase::Warned && !self.by_user && !self.over(policy) {
            self.phase = Phase::Under;
        }
    }

    /// The user asks for a handoff, to step away; `busy`: the agent is in
    /// a turn. What to send the terminal: an idle agent is asked by a
    /// prompt, a busy one at its next hook.
    pub fn request(&mut self, busy: bool) -> Action {
        if matches!(self.phase, Phase::Asked | Phase::Written { .. }) {
            return Action::None;
        }
        self.by_user = true;
        if busy {
            self.phase = Phase::Requested;
            Action::None
        } else {
            self.phase = Phase::Asked;
            Action::Prompt(self.handoff_request(Policy::default()))
        }
    }

    /// The user goes on from `handoff` (its path, when known) in a fresh
    /// conversation: what to send the terminal.
    pub fn resume_from(&mut self, handoff: Option<String>) -> Action {
        self.by_user = false;
        self.phase = Phase::Written { handoff, clearing: true };
        Action::Clear
    }

    /// Hook `ev` came: the answer for Claude Code (`None` for `{}`), and
    /// what to send the terminal after.
    pub fn on_event(&mut self, ev: &HookEvent, policy: Policy) -> (Option<Value>, Action) {
        let none = (None, Action::None);
        match ev.event.as_str() {
            crate::statusline::EVENT => {
                self.reading(used_percent(&ev.raw), policy);
                none
            },
            "SessionStart" => {
                let fresh = matches!(ev.raw.get("source").and_then(Value::as_str), Some("clear" | "startup"));
                let phase = std::mem::take(&mut self.phase);
                self.percent = 0.;
                (self.fresh, self.born_over, self.by_user) = (fresh, false, self.by_user && !fresh);
                match phase {
                    Phase::Written { handoff, clearing } if fresh => {
                        let context = continue_context(handoff.as_deref());
                        let reply = json!({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": context}});
                        let action = if clearing { Action::Prompt(CONTINUE_PROMPT.into()) } else { Action::None };
                        (Some(reply), action)
                    },
                    // Compacted, resumed: it goes on where it is. A program
                    // just starting has yet to take the clear on its way.
                    phase => {
                        let starting = ev.raw.get("source").and_then(Value::as_str) == Some("resume");
                        self.phase = match phase {
                            p @ Phase::Written { clearing: true, .. } if starting => p,
                            Phase::Written { .. } => Phase::Under,
                            p => p,
                        };
                        none
                    },
                }
            },
            "PostToolUse" | "UserPromptSubmit" if self.phase == Phase::Requested || (self.phase == Phase::Under && self.over(policy)) => {
                self.phase = Phase::Warned;
                let reply = json!({"hookSpecificOutput": {"hookEventName": ev.event, "additionalContext": self.warning(policy)}});
                (Some(reply), Action::None)
            },
            "Stop" => match self.phase {
                Phase::Requested | Phase::Under | Phase::Warned if (self.by_user || self.over(policy)) && !stop_hook_active(&ev.raw) => {
                    self.phase = Phase::Asked;
                    (Some(json!({"decision": "block", "reason": self.handoff_request(policy)})), Action::None)
                },
                Phase::Asked => {
                    let handoff = last_message(&ev.raw).as_deref().and_then(handoff_path);
                    // The user's own handoff stops: they go on when they say.
                    let clearing = policy.auto_switch && !std::mem::take(&mut self.by_user);
                    self.phase = Phase::Written { handoff, clearing };
                    (None, if clearing { Action::Clear } else { Action::None })
                },
                _ => none,
            },
            _ => none,
        }
    }

    fn over(&self, policy: Policy) -> bool {
        policy.limit > 0 && self.percent >= f32::from(policy.limit) && !self.born_over
    }

    /// Why it hands off.
    fn reason(&self, policy: Policy) -> String {
        if self.by_user {
            "the user asked for a handoff: they are stepping away".into()
        } else {
            format!("this conversation's context window is {:.0}% full, past the {}% limit", self.percent, policy.limit)
        }
    }

    fn warning(&self, policy: Policy) -> String {
        format!(
            "Tern: {}. Finish the step you are on and end your turn at the next good stopping point; you will then be asked for a handoff.",
            self.reason(policy)
        )
    }

    fn handoff_request(&self, policy: Policy) -> String {
        let next = if self.by_user {
            "Then stop: the user goes on later, in a fresh conversation from your handoff."
        } else if policy.auto_switch {
            "Tern then clears this conversation and a fresh one goes on from your handoff."
        } else {
            "The user then starts a fresh conversation that goes on from your handoff."
        };
        format!(
            "Tern: {}. Before stopping, write a handoff in this repository, where you see fit: what the task is, what is done, what is left, the decisions taken and the files that matter, so a fresh conversation can go on without this one. Update your memory to point at it too. Then end your turn with a last line `{HANDOFF_LINE} <path of the handoff file>`. {next}",
            self.reason(policy)
        )
    }
}

/// How full a window is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    /// Percent of the window used, 0 to 100.
    pub percent: f32,
    /// The window's size, in tokens.
    pub size: u64,
}

/// How full a headless conversation's window is, from the lines it writes:
/// its last message's input against its model's window, which only a turn's
/// `result` names. Nothing is known before the first one.
#[derive(Debug, Clone, Default)]
pub struct Meter {
    /// The last message's input, in tokens.
    used: Option<u64>,
    model: Option<String>,
    size: Option<u64>,
}

impl Meter {
    /// Take in `line`, one the program wrote.
    pub fn read(&mut self, line: &Value) {
        match line.get("type").and_then(Value::as_str) {
            Some("assistant") if line.get("parent_tool_use_id").is_none_or(Value::is_null) => {
                let tokens = |k: &str| line.pointer(&format!("/message/usage/{k}")).and_then(Value::as_u64).unwrap_or(0);
                let used = tokens("input_tokens") + tokens("cache_creation_input_tokens") + tokens("cache_read_input_tokens");
                if used > 0 {
                    self.used = Some(used);
                    self.model = line.pointer("/message/model").and_then(Value::as_str).map(str::to_owned);
                }
            },
            Some("result") => {
                let Some(models) = line.get("modelUsage").and_then(Value::as_object) else { return };
                let window = |m: &Value| m.get("contextWindow").and_then(Value::as_u64).filter(|s| *s > 0);
                let named = self.model.as_ref().and_then(|m| models.get(m)).and_then(window);
                self.size = named.or_else(|| models.values().filter_map(window).max()).or(self.size);
            },
            Some("conversation_reset") => self.used = None,
            Some("system") if line.get("subtype").and_then(Value::as_str) == Some("compact_boundary") => self.used = None,
            _ => {},
        }
    }

    pub fn reading(&self) -> Option<Reading> {
        let (used, size) = (self.used?, self.size?);
        Some(Reading { percent: (used as f64 * 100. / size as f64).clamp(0., 100.) as f32, size })
    }
}

/// What a fresh conversation is told of the handoff it goes on from.
fn continue_context(handoff: Option<&str>) -> String {
    match handoff {
        Some(path) => format!(
            "Tern: this conversation goes on from the previous one, which handed off. Its handoff is at `{path}`: read it (and your memory) before anything else, then go on with the work."
        ),
        None => "Tern: this conversation goes on from the previous one, which wrote a handoff in this repository; your memory points at it. Read it before anything else, then go on with the work.".into(),
    }
}

/// Percent of the window used, from a status line's input (none before the
/// first answer).
fn used_percent(line: &Value) -> f32 {
    line.pointer("/context_window/used_percentage").and_then(Value::as_f64).unwrap_or(0.).clamp(0., 100.) as f32
}

fn stop_hook_active(raw: &Value) -> bool {
    raw.get("stop_hook_active").and_then(Value::as_bool).unwrap_or(false)
}

/// The handoff path a `Stop` hook event's `raw` names: its last message's
/// `Handoff: <path>` line.
pub fn handoff_of(raw: &Value) -> Option<String> {
    last_message(raw).as_deref().and_then(handoff_path)
}

/// The text of the agent's last message: what the `Stop` hook says, else
/// the end of the transcript.
fn last_message(raw: &Value) -> Option<String> {
    if let Some(m) = raw.get("last_assistant_message").and_then(Value::as_str) {
        return Some(m.to_owned());
    }
    last_transcript_text(Path::new(raw.get("transcript_path")?.as_str()?))
}

/// The text of the last assistant message in transcript `path`, read from
/// its end.
pub fn last_transcript_text(path: &Path) -> Option<String> {
    const TAIL: u64 = 512 * 1024;
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut buf = vec![];
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    text.lines().rev().filter_map(|l| serde_json::from_str::<Value>(l).ok()).find_map(|v| {
        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            return None;
        }
        let parts = v.pointer("/message/content")?.as_array()?;
        let text: Vec<&str> = parts
            .iter()
            .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect();
        (!text.is_empty()).then(|| text.join("\n"))
    })
}

/// The path in the last `Handoff: <path>` line of `message`.
pub fn handoff_path(message: &str) -> Option<String> {
    message.lines().rev().find_map(|l| {
        let l = l.trim().trim_start_matches(['*', '_', '-', ' ']);
        let rest = l.strip_prefix(HANDOFF_LINE)?;
        let path = rest.trim().trim_matches(['*', '_', '`', '"', '\'', ' ']);
        (!path.is_empty()).then(|| path.to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(body: Value) -> HookEvent {
        HookEvent::parse("t".into(), &serde_json::to_vec(&body).unwrap()).unwrap()
    }

    fn line(percent: f64) -> HookEvent {
        ev(json!({"hook_event_name": "StatusLine", "context_window": {"context_window_size": 200000, "used_percentage": percent}}))
    }

    const POLICY: Policy = Policy { limit: 50, auto_switch: true };

    fn assistant(model: &str, tokens: u64) -> Value {
        json!({"type": "assistant", "parent_tool_use_id": null, "message": {"model": model, "usage": {
            "input_tokens": 2, "cache_creation_input_tokens": tokens - 2, "cache_read_input_tokens": 0, "output_tokens": 999
        }}})
    }

    fn result(windows: Value) -> Value {
        json!({"type": "result", "modelUsage": windows})
    }

    #[test]
    fn a_meter_knows_nothing_before_a_turn_ends() {
        let mut m = Meter::default();
        m.read(&assistant("big", 1000));
        assert_eq!(m.reading(), None);
    }

    #[test]
    fn a_meter_reads_the_last_message_against_its_models_window() {
        let mut m = Meter::default();
        m.read(&assistant("big", 1000));
        m.read(&result(json!({"small": {"contextWindow": 2000}, "big": {"contextWindow": 10000}})));
        assert_eq!(m.reading(), Some(Reading { percent: 10., size: 10000 }));
        // The window stays known, so a turn's messages read as they come.
        m.read(&assistant("big", 5000));
        assert_eq!(m.reading(), Some(Reading { percent: 50., size: 10000 }));
    }

    #[test]
    fn a_meter_takes_the_largest_window_of_an_unnamed_model() {
        let mut m = Meter::default();
        m.read(&assistant("alias", 1000));
        m.read(&result(json!({"a": {"contextWindow": 2000}, "b": {"contextWindow": 10000}})));
        assert_eq!(m.reading().map(|r| r.size), Some(10000));
    }

    #[test]
    fn a_meter_skips_subagents_and_empty_usage() {
        let mut m = Meter::default();
        m.read(&assistant("big", 1000));
        m.read(&result(json!({"big": {"contextWindow": 10000}})));
        let mut sub = assistant("big", 9000);
        sub["parent_tool_use_id"] = json!("toolu_1");
        m.read(&sub);
        m.read(
            &json!({"type": "assistant", "parent_tool_use_id": null, "message": {"model": "<synthetic>", "usage": {"input_tokens": 0}}}),
        );
        assert_eq!(m.reading().map(|r| r.percent), Some(10.));
    }

    #[test]
    fn a_meter_forgets_a_cleared_or_compacted_conversation() {
        for line in [json!({"type": "conversation_reset"}), json!({"type": "system", "subtype": "compact_boundary"})] {
            let mut m = Meter::default();
            m.read(&assistant("big", 1000));
            m.read(&result(json!({"big": {"contextWindow": 10000}})));
            m.read(&line);
            assert_eq!(m.reading(), None);
            m.read(&assistant("big", 2000));
            assert_eq!(m.reading().map(|r| r.percent), Some(20.));
        }
    }

    /// A chat's Claude may start only when the user asks: its own start
    /// comes after what they asked for.
    #[test]
    fn going_on_outlasts_the_program_starting() {
        let mut g = Guard::default();
        assert_eq!(g.resume_from(Some("h.md".into())), Action::Clear);
        let started = ev(json!({"hook_event_name": "SessionStart", "source": "resume"}));
        assert_eq!(g.on_event(&started, POLICY), (None, Action::None));
        let cleared = ev(json!({"hook_event_name": "SessionStart", "source": "clear"}));
        let (reply, action) = g.on_event(&cleared, POLICY);
        assert!(reply.unwrap().to_string().contains("h.md"));
        assert_eq!(action, Action::Prompt(CONTINUE_PROMPT.into()));
    }

    #[test]
    fn the_users_handoff_outlasts_the_program_starting() {
        let mut g = Guard::default();
        g.request(false);
        g.on_event(&ev(json!({"hook_event_name": "SessionStart", "source": "resume"})), POLICY);
        let done = ev(json!({"hook_event_name": "Stop", "stop_hook_active": false, "last_assistant_message": "Handoff: h.md"}));
        // It stops: they go on when they say.
        assert_eq!(g.on_event(&done, POLICY), (None, Action::None));
    }

    #[test]
    fn a_reading_counts_like_a_status_line() {
        let mut g = Guard::default();
        g.reading(60., POLICY);
        assert_eq!(g.percent(), 60.);
        let (reply, _) = g.on_event(&ev(json!({"hook_event_name": "Stop", "stop_hook_active": false})), POLICY);
        assert_eq!(reply.unwrap().get("decision").unwrap(), "block");
    }

    #[test]
    fn under_the_limit_says_nothing() {
        let mut g = Guard::default();
        g.on_event(&line(30.), POLICY);
        assert_eq!(g.on_event(&ev(json!({"hook_event_name": "PostToolUse"})), POLICY), (None, Action::None));
        assert_eq!(g.on_event(&ev(json!({"hook_event_name": "Stop"})), POLICY), (None, Action::None));
    }

    #[test]
    fn no_limit_says_nothing() {
        let mut g = Guard::default();
        g.on_event(&line(99.), Policy::default());
        assert_eq!(g.on_event(&ev(json!({"hook_event_name": "Stop"})), Policy::default()), (None, Action::None));
    }

    #[test]
    fn warns_once_past_the_limit() {
        let mut g = Guard::default();
        g.on_event(&line(52.), POLICY);
        let (reply, _) = g.on_event(&ev(json!({"hook_event_name": "PostToolUse"})), POLICY);
        let reply = reply.unwrap();
        assert_eq!(reply.pointer("/hookSpecificOutput/hookEventName").unwrap(), "PostToolUse");
        assert!(reply.pointer("/hookSpecificOutput/additionalContext").unwrap().as_str().unwrap().contains("52%"));
        assert_eq!(g.on_event(&ev(json!({"hook_event_name": "UserPromptSubmit"})), POLICY), (None, Action::None));
    }

    #[test]
    fn hands_off_then_switches() {
        let mut g = Guard::default();
        g.on_event(&line(60.), POLICY);
        let (reply, action) = g.on_event(&ev(json!({"hook_event_name": "Stop", "stop_hook_active": false})), POLICY);
        assert_eq!(reply.unwrap().get("decision").unwrap(), "block");
        assert_eq!(action, Action::None);

        let done =
            json!({"hook_event_name": "Stop", "stop_hook_active": true, "last_assistant_message": "Done.\n\nHandoff: `docs/handoff/x.md`"});
        assert_eq!(g.on_event(&ev(done), POLICY), (None, Action::Clear));

        let (reply, action) = g.on_event(&ev(json!({"hook_event_name": "SessionStart", "source": "clear"})), POLICY);
        let context = reply.unwrap().pointer("/hookSpecificOutput/additionalContext").unwrap().as_str().unwrap().to_owned();
        assert!(context.contains("docs/handoff/x.md"));
        assert_eq!(action, Action::Prompt(CONTINUE_PROMPT.into()));
        assert_eq!(g.percent(), 0.);
        // The fresh conversation starts over.
        assert_eq!(g.on_event(&ev(json!({"hook_event_name": "Stop"})), POLICY), (None, Action::None));
    }

    #[test]
    fn without_auto_switch_waits_for_the_user() {
        let policy = Policy { auto_switch: false, ..POLICY };
        let mut g = Guard::default();
        g.on_event(&line(60.), policy);
        g.on_event(&ev(json!({"hook_event_name": "Stop"})), policy);
        let done = json!({"hook_event_name": "Stop", "stop_hook_active": true, "last_assistant_message": "Handoff: a.md"});
        assert_eq!(g.on_event(&ev(done), policy), (None, Action::None));
        let (reply, action) = g.on_event(&ev(json!({"hook_event_name": "SessionStart", "source": "clear"})), policy);
        assert!(reply.is_some());
        assert_eq!(action, Action::None);
    }

    #[test]
    fn a_compaction_goes_on() {
        let mut g = Guard::default();
        g.on_event(&line(60.), POLICY);
        g.on_event(&ev(json!({"hook_event_name": "Stop"})), POLICY);
        g.on_event(&ev(json!({"hook_event_name": "Stop", "stop_hook_active": true})), POLICY);
        assert_eq!(g.on_event(&ev(json!({"hook_event_name": "SessionStart", "source": "compact"})), POLICY), (None, Action::None));
        g.on_event(&line(60.), POLICY);
        let (reply, _) = g.on_event(&ev(json!({"hook_event_name": "Stop"})), POLICY);
        assert!(reply.is_some());
    }

    #[test]
    fn a_conversation_born_past_the_limit_is_left_alone() {
        let mut g = Guard::default();
        g.on_event(&ev(json!({"hook_event_name": "SessionStart", "source": "clear"})), POLICY);
        g.on_event(&line(0.), POLICY);
        g.on_event(&line(60.), POLICY);
        assert_eq!(g.on_event(&ev(json!({"hook_event_name": "PostToolUse"})), POLICY), (None, Action::None));
        assert_eq!(g.on_event(&ev(json!({"hook_event_name": "Stop"})), POLICY), (None, Action::None));
        // A resumed one is judged as it is.
        g.on_event(&ev(json!({"hook_event_name": "SessionStart", "source": "resume"})), POLICY);
        g.on_event(&line(60.), POLICY);
        assert!(g.on_event(&ev(json!({"hook_event_name": "Stop"})), POLICY).0.is_some());
    }

    #[test]
    fn a_fresh_conversation_that_grows_hands_off() {
        let mut g = Guard::default();
        g.on_event(&ev(json!({"hook_event_name": "SessionStart", "source": "startup"})), POLICY);
        g.on_event(&line(8.), POLICY);
        g.on_event(&line(55.), POLICY);
        assert!(g.on_event(&ev(json!({"hook_event_name": "Stop"})), POLICY).0.is_some());
    }

    #[test]
    fn another_hooks_block_is_left_alone() {
        let mut g = Guard::default();
        g.on_event(&line(60.), POLICY);
        assert_eq!(g.on_event(&ev(json!({"hook_event_name": "Stop", "stop_hook_active": true})), POLICY), (None, Action::None));
    }

    #[test]
    fn the_users_handoff_of_a_busy_agent_stops() {
        let mut g = Guard::default();
        g.on_event(&line(10.), POLICY);
        assert_eq!(g.request(true), Action::None);
        let (reply, _) = g.on_event(&ev(json!({"hook_event_name": "PostToolUse"})), POLICY);
        assert!(reply.unwrap().pointer("/hookSpecificOutput/additionalContext").unwrap().as_str().unwrap().contains("the user asked"));
        // Under the limit, it isn't let off.
        g.on_event(&line(11.), POLICY);
        let (reply, _) = g.on_event(&ev(json!({"hook_event_name": "Stop"})), POLICY);
        assert_eq!(reply.unwrap().get("decision").unwrap(), "block");
        let done = json!({"hook_event_name": "Stop", "stop_hook_active": true, "last_assistant_message": "Handoff: a.md"});
        // Auto switch or not, it stops.
        assert_eq!(g.on_event(&ev(done), POLICY), (None, Action::None));
        // The limit's handoffs go on as before.
        g.on_event(&ev(json!({"hook_event_name": "SessionStart", "source": "clear"})), POLICY);
        g.on_event(&line(8.), POLICY);
        g.on_event(&line(60.), POLICY);
        g.on_event(&ev(json!({"hook_event_name": "Stop"})), POLICY);
        let done = json!({"hook_event_name": "Stop", "stop_hook_active": true, "last_assistant_message": "Handoff: b.md"});
        assert_eq!(g.on_event(&ev(done), POLICY), (None, Action::Clear));
    }

    #[test]
    fn the_users_handoff_of_an_idle_agent_is_a_prompt() {
        let mut g = Guard::default();
        let Action::Prompt(p) = g.request(false) else { panic!("no prompt") };
        assert!(p.contains("the user asked") && p.contains(HANDOFF_LINE));
        // Asked again meanwhile: nothing more.
        assert_eq!(g.request(false), Action::None);
        assert_eq!(g.on_event(&ev(json!({"hook_event_name": "UserPromptSubmit"})), POLICY), (None, Action::None));
        let done = json!({"hook_event_name": "Stop", "last_assistant_message": "Handoff: a.md"});
        assert_eq!(g.on_event(&ev(done), POLICY), (None, Action::None));
    }

    #[test]
    fn goes_on_from_a_handoff_when_the_user_says() {
        // As after a restart: the daemon knows nothing of it.
        let mut g = Guard::default();
        assert_eq!(g.resume_from(Some("docs/h.md".into())), Action::Clear);
        let (reply, action) = g.on_event(&ev(json!({"hook_event_name": "SessionStart", "source": "clear"})), Policy::default());
        assert!(reply.unwrap().pointer("/hookSpecificOutput/additionalContext").unwrap().as_str().unwrap().contains("docs/h.md"));
        assert_eq!(action, Action::Prompt(CONTINUE_PROMPT.into()));
    }

    #[test]
    fn reads_the_handoff_line() {
        assert_eq!(handoff_path("x\n**Handoff:** `docs/a b.md`").as_deref(), Some("docs/a b.md"));
        assert_eq!(handoff_path("Handoff: docs/a.md\nmore"), Some("docs/a.md".into()));
        assert_eq!(handoff_path("no line"), None);
        assert_eq!(handoff_path("Handoff:   "), None);
    }

    #[test]
    fn reads_the_transcripts_last_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let lines = [
            json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "old"}]}}),
            json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "Handoff: h.md"}]}}),
            json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "name": "x"}]}}),
            json!({"type": "user", "message": {"content": "hi"}}),
        ];
        std::fs::write(&path, lines.iter().map(Value::to_string).collect::<Vec<_>>().join("\n")).unwrap();
        let raw = json!({"transcript_path": path});
        assert_eq!(last_message(&raw).as_deref().and_then(handoff_path).as_deref(), Some("h.md"));
    }
}
