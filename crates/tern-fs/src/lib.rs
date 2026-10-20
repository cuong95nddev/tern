//! Filesystem model for the explorer: lazy gitignore-aware tree, debounced
//! watcher, git status. No UI deps.

pub mod fuzzy;
pub mod git;
pub mod git_log;
pub mod ops;
pub mod search;
pub mod tree;
pub mod watch;

pub use git::GitStatus;
pub use tree::{Entry, FileTree, Row};
pub use watch::{ChangeBatch, FsWatcher};
