//! Integration with Claude Code: hook receiver (and later the IDE protocol).

pub mod context;
pub mod hooks;
pub mod relay;
pub mod statusline;

pub use hooks::{
    HookEvent, HookServer, HookSink, claim_run_dir, claude_settings_json, remove_claude_settings, sweep_run_dirs, write_claude_settings,
};
