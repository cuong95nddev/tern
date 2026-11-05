//! A Codex tab's status from its hooks. Codex's hooks are Claude Code's
//! protocol (the same events and fields, see the fixtures), so they fold
//! the same way; `Interrupt` is what Codex has and Claude doesn't.

use std::ops::{Deref, DerefMut};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_status::AgentStatus;
use crate::claude_status::ClaudeTracker;

/// What a Codex tab's hooks told: times are milliseconds since the Unix epoch.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CodexTracker(ClaudeTracker);

impl CodexTracker {
    /// Take hook payload `raw` at `now`; the tab's status when the event
    /// tells one (see [`ClaudeTracker::on_event`]).
    pub fn on_event(&mut self, raw: &Value, now: u64, stop_blocked: bool) -> Option<AgentStatus> {
        match raw.get("hook_event_name").and_then(Value::as_str) {
            Some("Interrupt") => self.0.interrupted(now),
            _ => self.0.on_event(raw, now, stop_blocked),
        }
    }
}

impl Deref for CodexTracker {
    type Target = ClaudeTracker;

    fn deref(&self) -> &ClaudeTracker {
        &self.0
    }
}

impl DerefMut for CodexTracker {
    fn deref_mut(&mut self) -> &mut ClaudeTracker {
        &mut self.0
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::agent_status::TurnState;

    fn fixture(text: &str) -> Vec<Value> {
        text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    /// The state after each event of `events` that told one.
    fn states(t: &mut CodexTracker, events: &[Value], stop_blocked: bool) -> Vec<TurnState> {
        events.iter().enumerate().filter_map(|(i, e)| t.on_event(e, 1_000 + i as u64, stop_blocked)).map(|s| s.state).collect()
    }

    #[test]
    fn a_turn_goes_working_then_done() {
        let got = states(&mut CodexTracker::default(), &fixture(include_str!("codex_status/fixtures/turn.jsonl")), false);
        assert_eq!(got.last(), Some(&TurnState::Done), "{got:?}");
        assert!(got.contains(&TurnState::Working), "{got:?}");
    }

    #[test]
    fn tools_keep_it_working() {
        let events = fixture(include_str!("codex_status/fixtures/tools.jsonl"));
        let (stop, tools) = events.split_last().unwrap();
        let mut t = CodexTracker::default();
        assert!(states(&mut t, tools, false).iter().all(|s| *s == TurnState::Working));
        assert_eq!(states(&mut t, std::slice::from_ref(stop), false), [TurnState::Done]);
    }

    #[test]
    fn a_blocked_stop_keeps_the_turn_going() {
        let events = fixture(include_str!("codex_status/fixtures/blocked.jsonl"));
        let mut t = CodexTracker::default();
        t.on_event(&json!({"hook_event_name": "UserPromptSubmit", "prompt": "hi", "session_id": "s"}), 1, false);
        assert_eq!(t.on_event(&events[0], 2, true).map(|s| s.state), Some(TurnState::Working));
        assert!(t.main_agent_busy());
        assert_eq!(t.on_event(&events[1], 3, false).map(|s| s.state), Some(TurnState::Done));
    }

    #[test]
    fn interrupt_ends_the_turn_as_a_cancellation() {
        let events = fixture(include_str!("codex_status/fixtures/interrupt.jsonl"));
        let mut t = CodexTracker::default();
        let mut claude = ClaudeTracker::default();
        t.on_event(&events[0], 1, false);
        claude.on_event(&events[0], 1, false);
        assert_eq!(t.on_event(&events[1], 2, false), claude.interrupted(2));
        assert!(!t.main_agent_busy());
    }

    #[test]
    fn a_permission_request_waits() {
        let mut t = CodexTracker::default();
        t.on_event(&json!({"hook_event_name": "UserPromptSubmit", "prompt": "hi", "session_id": "s"}), 1, false);
        let ask =
            json!({"hook_event_name": "PermissionRequest", "session_id": "s", "tool_name": "Bash", "tool_input": {"command": "rm x"}});
        assert_eq!(t.on_event(&ask, 2, false).map(|s| s.state), Some(TurnState::Waiting));
    }

    #[test]
    fn subagents_come_and_go() {
        let mut t = CodexTracker::default();
        t.on_event(&json!({"hook_event_name": "UserPromptSubmit", "prompt": "hi", "session_id": "s"}), 1, false);
        let start = json!({"hook_event_name": "SubagentStart", "session_id": "s", "agent_id": "a", "agent_type": "worker"});
        assert_eq!(t.on_event(&start, 2, false).map(|s| s.subagents.len()), Some(1));
        let stop = json!({"hook_event_name": "SubagentStop", "session_id": "s", "agent_id": "a", "agent_type": "worker"});
        assert_eq!(t.on_event(&stop, 3, false).map(|s| s.subagents.len()), Some(0));
    }
}
