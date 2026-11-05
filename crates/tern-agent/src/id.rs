use std::fmt;

use serde::{Deserialize, Serialize};

/// The built-in agents' ids.
pub const CLAUDE: &str = "claude";
pub const CODEX: &str = "codex";
pub const OPENCODE: &str = "opencode";
pub const GEMINI: &str = "gemini";
pub const CURSOR: &str = "cursor";
pub const GROK: &str = "grok";
pub const PI: &str = "pi";

/// An agent's id: a built-in's (see the constants) or one a user file adds.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentId(String);

impl AgentId {
    pub fn new(id: &str) -> Self {
        Self(id.to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// It is the agent `id`.
    pub fn is(&self, id: &str) -> bool {
        self.0 == id
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
