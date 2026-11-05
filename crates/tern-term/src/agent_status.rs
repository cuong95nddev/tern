//! What an agent tab is doing, as its hook lane tells it: the main agent's
//! turn and the work it leaves running (sub-agents, background shells,
//! crons). One model for every agent; Claude Code's hooks fill all of it.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnState {
    Working,
    /// Waits for you: a permission, an AskUserQuestion.
    Waiting,
    Done,
}

/// How the main agent's last turn ended, when not plainly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Cancellation,
    Failure,
}

/// Why a tab whose main agent is between turns is about to work again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Wakeup {
    /// A task ended and its notification has not reached the main agent.
    Notification,
    /// The main agent is in the turn a notification opened.
    FinishingTurn,
}

/// The main agent's own state, apart from its children.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MainAgent {
    pub state: TurnState,
    /// Only while `state` is done.
    pub outcome: Option<Outcome>,
    /// When `state` began.
    pub since: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subagent {
    pub id: String,
    pub agent_type: Option<String>,
    pub description: Option<String>,
    pub started: u64,
    /// Else a teammate between its turns.
    pub working: bool,
}

/// What keeps a tab whose main agent's turn is over from being done.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    /// Sub-agents at work.
    pub agents: usize,
    /// A background shell or Monitor runs.
    pub shells: bool,
    /// A session cron is scheduled.
    pub cron: bool,
    /// A task ended and the main agent is still to be told.
    pub owed: bool,
}

/// An agent tab's status, from its hook lane (see [`crate::agent_lane`]).
/// Fields an agent's events can't fill stay empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStatus {
    /// Its main agent and children together: working while either works,
    /// waiting while either waits for you.
    pub state: TurnState,
    /// Working only because watch loops (shells, Monitors, crons) run.
    pub monitoring: bool,
    pub wakeup: Option<Wakeup>,
    /// `None` until a main-agent event was seen.
    pub main_agent: Option<MainAgent>,
    /// When the main agent's turn ended while running child work kept the
    /// tab working: that turn's end, to announce once (and not again when
    /// the work drains).
    pub turn_completed_at: Option<u64>,
    pub subagents: Vec<Subagent>,
    /// A session boundary (start, resume, clear, manual compact): not a
    /// turn that completed.
    pub session_boundary: bool,
    pub held: Held,
}

impl AgentStatus {
    /// The main agent is done and child work alone keeps the tab open.
    pub fn held_open(&self) -> bool {
        self.main_agent.is_some_and(|m| m.state == TurnState::Done) && self.state != TurnState::Done
    }
}
