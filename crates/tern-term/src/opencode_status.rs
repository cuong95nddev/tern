//! An OpenCode tab's status from its events, which reach it as Claude Code's
//! hooks (see `tern_bridge::opencode_fold`), plus what OpenCode tells and
//! they don't: a turn interrupted, a permission answered.

use std::ops::{Deref, DerefMut};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_status::AgentStatus;
use crate::claude_status::ClaudeTracker;

/// What an OpenCode tab's events told: times are milliseconds since the Unix
/// epoch.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenCodeTracker(ClaudeTracker);

impl OpenCodeTracker {
    /// Take hook payload `raw` at `now`; the tab's status when the event
    /// tells one (see [`ClaudeTracker::on_event`]).
    pub fn on_event(&mut self, raw: &Value, now: u64, stop_blocked: bool) -> Option<AgentStatus> {
        match raw.get("hook_event_name").and_then(Value::as_str) {
            Some("Interrupt") => self.0.interrupted(now),
            Some("PermissionAnswered") => self.0.question_answered(now),
            _ => self.0.on_event(raw, now, stop_blocked),
        }
    }
}

impl Deref for OpenCodeTracker {
    type Target = ClaudeTracker;

    fn deref(&self) -> &ClaudeTracker {
        &self.0
    }
}

impl DerefMut for OpenCodeTracker {
    fn deref_mut(&mut self) -> &mut ClaudeTracker {
        &mut self.0
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::agent_status::TurnState;

    fn state(t: &mut OpenCodeTracker, event: &str, at: u64) -> Option<TurnState> {
        t.on_event(&json!({"hook_event_name": event, "session_id": "s", "prompt": "hi"}), at, false).map(|s| s.state)
    }

    #[test]
    fn interrupt_ends_the_turn() {
        let mut t = OpenCodeTracker::default();
        state(&mut t, "UserPromptSubmit", 1);
        assert!(t.main_agent_busy());
        state(&mut t, "Interrupt", 2);
        assert!(!t.main_agent_busy());
    }

    #[test]
    fn a_permission_waits_until_it_is_answered() {
        let mut t = OpenCodeTracker::default();
        state(&mut t, "UserPromptSubmit", 1);
        assert_eq!(state(&mut t, "PermissionRequest", 2), Some(TurnState::Waiting));
        assert_eq!(state(&mut t, "PermissionAnswered", 3), Some(TurnState::Working));
    }

    #[test]
    fn a_failed_turn_ends() {
        let mut t = OpenCodeTracker::default();
        state(&mut t, "UserPromptSubmit", 1);
        state(&mut t, "StopFailure", 2);
        assert!(!t.main_agent_busy());
    }
}
