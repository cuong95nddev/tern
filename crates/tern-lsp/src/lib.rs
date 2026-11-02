//! Language servers for file tabs: configs, one process per (server, project
//! root), document sync, and the navigation requests (definition,
//! implementations, outline, type hierarchy). No UI deps.

pub mod config;
pub mod text;

pub use config::{ServerConfig, builtin, builtin_text, find_root, language_id, resolve, servers_for};
