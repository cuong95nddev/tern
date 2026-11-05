//! Where each agent keeps its conversations, and reading them: the table
//! the sidebar preview and the handoff read through. Claude's entries are
//! its `~/.claude/projects/…/<id>.jsonl` files, Codex's its rollouts (see
//! [`crate::codex::rollout`]); other agents have none yet.

use std::io;
use std::path::{Path, PathBuf};

use crate::Recent;

/// Where `agent` saved conversation `session` started in `cwd`, under its
/// config folder `config`, if it did.
pub fn transcript_path(agent: &str, config: &Path, cwd: &Path, session: &str) -> Option<PathBuf> {
    match agent {
        tern_agent::CLAUDE => crate::claude::launch::transcript_path(config, cwd, session),
        tern_agent::CODEX => crate::codex::rollout::rollout_path(config, session),
        _ => None,
    }
}

/// What `agent`'s conversation saved at `path` is doing now (see
/// [`crate::recent`]); nothing for an agent Tern can't read yet.
pub fn read_recent(agent: &str, path: &Path, cwd: Option<&Path>) -> io::Result<Recent> {
    match agent {
        tern_agent::CLAUDE => crate::recent::read_recent(path, cwd),
        tern_agent::CODEX => crate::codex::rollout::read_recent(path, cwd),
        _ => Ok(Recent::default()),
    }
}

/// The text of the last answer in `agent`'s conversation saved at `path`.
pub fn last_text(agent: &str, path: &Path) -> Option<String> {
    match agent {
        tern_agent::CLAUDE => tern_bridge::context::last_transcript_text(path),
        tern_agent::CODEX => crate::codex::rollout::last_text(path),
        _ => None,
    }
}

/// How full `agent`'s conversation saved at `path` is, for an agent that
/// tells only there (Claude's status line tells it, see
/// [`tern_bridge::statusline`]).
pub fn context_reading(agent: &str, path: &Path) -> Option<tern_bridge::context::Reading> {
    match agent {
        tern_agent::CODEX => crate::codex::rollout::context_reading(path),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn claude_transcript_path_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("projects").join("-a-b");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("x.jsonl"), "").unwrap();
        let cwd = Path::new("/a/b");
        assert_eq!(transcript_path("claude", dir.path(), cwd, "x"), crate::claude::launch::transcript_path(dir.path(), cwd, "x"));
        assert_eq!(transcript_path("claude", dir.path(), cwd, "x"), Some(folder.join("x.jsonl")));
    }

    #[test]
    fn codex_reads_its_rollouts() {
        let home = tempfile::tempdir().unwrap();
        let day = home.path().join("sessions/2026/11/05");
        std::fs::create_dir_all(&day).unwrap();
        let path = day.join("rollout-x-ID.jsonl");
        std::fs::write(&path, include_str!("codex/fixtures/rollout.jsonl")).unwrap();
        assert_eq!(transcript_path("codex", home.path(), Path::new("/a"), "ID"), Some(path.clone()));
        assert_eq!(last_text("codex", &path).as_deref(), Some("Done."));
        assert_eq!(read_recent("codex", &path, None).unwrap().text.as_deref(), Some("Done."));
        assert_eq!(context_reading("codex", &path).map(|r| r.size), Some(258_400));
        assert!(context_reading("claude", &path).is_none());
    }

    #[test]
    fn other_agents_have_no_transcript_yet() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("projects").join("-a-b");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("x.jsonl"), r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#).unwrap();
        let path = folder.join("x.jsonl");
        assert_eq!(transcript_path("gemini", dir.path(), Path::new("/a/b"), "x"), None);
        assert_eq!(read_recent("gemini", &path, None).unwrap(), crate::Recent::default());
        assert_eq!(last_text("gemini", &path), None);
        assert_eq!(last_text("claude", &path).as_deref(), Some("hi"));
    }
}
