//! Language servers for file tabs: configs, one process per (server, project
//! root), document sync, and the navigation requests (definition,
//! implementations, outline, type hierarchy). No UI deps.

pub mod config;
mod manager;
pub mod nav;
mod requests;
mod rpc;
pub mod text;
pub mod uri;

use std::path::PathBuf;

pub use config::{ServerConfig, builtin, builtin_text, find_root, language_id, resolve, servers_for};
pub use manager::{Manager, Timeouts};
pub use requests::HierarchyItem;

/// Why a server couldn't answer.
#[derive(Debug, Clone, PartialEq)]
pub enum LspError {
    /// No server for the file's language is installed.
    NotFound,
    /// The server isn't running (didn't start, crashed, timed out starting).
    Unavailable(String),
    /// No server for the file, or it can't answer this.
    Unsupported,
    Cancelled,
    Timeout,
    Server {
        code: i64,
        message: String,
    },
}

impl std::fmt::Display for LspError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "no language server is installed for this file"),
            Self::Unavailable(why) => write!(f, "the language server is not available: {why}"),
            Self::Unsupported => write!(f, "no language server can answer this for this file"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::Timeout => write!(f, "the language server took too long"),
            Self::Server { code, message } => write!(f, "the language server failed ({code}): {message}"),
        }
    }
}

impl std::error::Error for LspError {}

/// What the UI hears of the servers.
#[derive(Debug, Clone, PartialEq)]
pub enum LspEvent {
    /// The file's server isn't on `PATH` (reported once per server).
    Missing { server: String, hint: Option<String> },
    /// A server failed to start or exited.
    Failed { server: String, message: String },
    /// A server finished its work in progress (indexing): what it answered
    /// before may be incomplete.
    Settled,
}

/// A place in a file: its range in characters, as the editor counts.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Location {
    pub path: PathBuf,
    pub range: lsp_types::Range,
}
