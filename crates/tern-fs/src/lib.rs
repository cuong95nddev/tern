//! Filesystem model for the explorer: lazy gitignore-aware tree, debounced
//! watcher, git status. No UI deps.

pub mod git;
pub mod tree;
pub mod watch;

pub use git::GitStatus;
pub use tree::{Entry, FileTree, Row};
pub use watch::{ChangeBatch, FsWatcher};
