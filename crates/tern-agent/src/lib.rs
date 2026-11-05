//! The coding-agent CLIs Tern knows (Claude Code, Codex, OpenCode, ...):
//! each is an id with a descriptor (its name, command, the programs that
//! identify it, its logo, how to resume it, its screen cues) and the
//! capabilities Tern has for it. Built-in descriptors ship compiled in; a
//! user file in `agents/<id>.json` overrides a built-in's fields or adds an
//! agent. Features ask this registry, never a specific agent.
//!
//! What a capability does lives in the crate that owns it (the hook lane in
//! `tern-term`, the chat driver in `tern-chat`, ...), keyed by agent id.

mod descriptor;
mod id;
mod registry;
mod screen;

pub use descriptor::{Capability, Descriptor, Logo};
pub use id::{AgentId, CLAUDE, CODEX, CURSOR, GEMINI, GROK, OPENCODE, PI};
pub use registry::{Registry, init, registry};
pub use screen::{Rule, ScreenRules, ScreenState};
