//! An agent tab's hook lane: what turns the events its agent reports
//! (Claude Code's hooks, ...) into its [`AgentStatus`]. One variant per
//! agent Tern has hooks for, so the state serializes and a daemon taking a
//! session over takes its lane along.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_status::AgentStatus;
use crate::claude_status::ClaudeTracker;
use crate::codex_status::CodexTracker;

/// A hook lane: times are milliseconds since the Unix epoch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AgentLane {
    Claude(ClaudeTracker),
    Codex(CodexTracker),
}

impl AgentLane {
    /// The lane for agent `id`'s events, when Tern has one.
    pub fn for_agent(id: &str) -> Option<Self> {
        match id {
            tern_agent::CLAUDE => Some(Self::Claude(ClaudeTracker::default())),
            tern_agent::CODEX => Some(Self::Codex(CodexTracker::default())),
            _ => None,
        }
    }

    /// The agent it is the lane of.
    pub fn agent(&self) -> &'static str {
        match self {
            Self::Claude(_) => tern_agent::CLAUDE,
            Self::Codex(_) => tern_agent::CODEX,
        }
    }

    /// Fold in event `raw`; `stop_blocked`: Tern's own answer kept the turn
    /// going. The status, when it changed.
    pub fn on_event(&mut self, raw: &Value, now: u64, stop_blocked: bool) -> Option<AgentStatus> {
        match self {
            Self::Claude(t) => t.on_event(raw, now, stop_blocked),
            Self::Codex(t) => t.on_event(raw, now, stop_blocked),
        }
    }

    /// The user interrupted the turn.
    pub fn interrupted(&mut self, now: u64) -> Option<AgentStatus> {
        match self {
            Self::Claude(t) => t.interrupted(now),
            Self::Codex(t) => t.interrupted(now),
        }
    }

    /// The user answered the question it asked.
    pub fn question_answered(&mut self, now: u64) -> Option<AgentStatus> {
        match self {
            Self::Claude(t) => t.question_answered(now),
            Self::Codex(t) => t.question_answered(now),
        }
    }

    pub fn main_agent_busy(&self) -> bool {
        match self {
            Self::Claude(t) => t.main_agent_busy(),
            Self::Codex(t) => t.main_agent_busy(),
        }
    }

    pub fn asking(&self) -> bool {
        match self {
            Self::Claude(t) => t.asking(),
            Self::Codex(t) => t.asking(),
        }
    }

    /// The status now (leases may have run out).
    pub fn current(&mut self, now: u64) -> Option<AgentStatus> {
        match self {
            Self::Claude(t) => t.current(now),
            Self::Codex(t) => t.current(now),
        }
    }

    /// When the status changes by itself (a lease running out).
    pub fn deadline(&self) -> Option<u64> {
        match self {
            Self::Claude(t) => t.deadline(),
            Self::Codex(t) => t.deadline(),
        }
    }

    /// An event came.
    pub fn knows(&self) -> bool {
        match self {
            Self::Claude(t) => t.knows(),
            Self::Codex(t) => t.knows(),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::agent_status::TurnState;
    use crate::claude_status::ClaudeTracker;

    #[test]
    fn claude_lane_folds_hooks_like_the_tracker() {
        let mut lane = AgentLane::for_agent("claude").unwrap();
        let mut tracker = ClaudeTracker::default();
        let events = [
            json!({"hook_event_name": "UserPromptSubmit", "prompt": "hi", "session_id": "s"}),
            json!({"hook_event_name": "Stop", "session_id": "s"}),
        ];
        let mut states = Vec::new();
        for (i, e) in events.iter().enumerate() {
            let now = 1_000 + i as u64;
            let got = lane.on_event(e, now, false);
            assert_eq!(got, tracker.on_event(e, now, false));
            states.push(got.unwrap().state);
        }
        assert_eq!(states, [TurnState::Working, TurnState::Done]);
        assert_eq!(lane.agent(), "claude");
        assert!(lane.knows());
    }

    #[test]
    fn unknown_agents_have_no_lane() {
        assert!(AgentLane::for_agent("gemini").is_none());
    }

    #[test]
    fn a_codex_lane_folds_and_serializes() {
        let mut lane = AgentLane::for_agent("codex").unwrap();
        assert_eq!(lane.agent(), "codex");
        lane.on_event(&json!({"hook_event_name": "UserPromptSubmit", "prompt": "hi", "session_id": "s"}), 1, false);
        let back: AgentLane = serde_json::from_value(serde_json::to_value(&lane).unwrap()).unwrap();
        assert!(back.main_agent_busy());
        assert_eq!(back.agent(), "codex");
    }

    #[test]
    fn a_lane_serializes_with_its_tracker() {
        let mut lane = AgentLane::for_agent("claude").unwrap();
        lane.on_event(&json!({"hook_event_name": "UserPromptSubmit", "prompt": "hi", "session_id": "s"}), 1, false);
        let back: AgentLane = serde_json::from_value(serde_json::to_value(&lane).unwrap()).unwrap();
        assert!(back.main_agent_busy());
    }
}
