//! Integration with Claude Code: hook receiver (and later the IDE protocol).

pub mod hooks;

pub use hooks::{HookEvent, HookServer, HookSink, claude_settings_json, remove_claude_settings, write_claude_settings};
