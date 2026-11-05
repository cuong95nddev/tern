//! Integration with Claude Code: hook receiver (and later the IDE protocol).

pub mod claude_version;
pub mod codex_hooks;
pub mod context;
pub mod hooks;
pub mod opencode_hooks;
pub mod relay;
pub mod statusline;
pub mod version;

pub use hooks::{
    HookEvent, HookServer, HookSink, claim_run_dir, claude_settings_json, remove_claude_settings, sweep_run_dirs, write_claude_settings,
};
