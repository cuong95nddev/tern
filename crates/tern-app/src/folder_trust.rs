//! Whether Claude may start in a folder for a chat. A terminal's Claude asks
//! itself; headless it doesn't, and runs the folder's hooks at once, so a
//! chat asks first (template `chat.trust`). A folder is trusted when Claude
//! Code says so of it or a folder above it, or when the user said so in a
//! chat: Tern only reads `.claude.json`, which Claude Code rewrites as it
//! runs, and keeps those answers in a list of its own.

use std::path::{Path, PathBuf};

/// Whether Claude, running as `account`, may start in `folder`: a git
/// worktree as its repository's main checkout may, made from it, the same
/// files, wherever it is kept.
pub fn trusted(account: &str, folder: &Path) -> bool {
    trusted_here(account, folder) || tern_fs::worktree::linked(folder).is_some_and(|(main, _)| trusted_here(account, &main))
}

fn trusted_here(account: &str, folder: &Path) -> bool {
    let said = crate::accounts::claude_json(account).iter().any(|p| {
        let config = std::fs::read(p).ok().and_then(|b| serde_json::from_slice(&b).ok());
        config.is_some_and(|c| claude_trusts(&c, folder))
    });
    said || std::fs::read_to_string(list_file()).is_ok_and(|text| listed(&text, folder))
}

/// The user trusts `folder`.
pub fn trust(folder: &Path) -> std::io::Result<()> {
    add(&list_file(), folder)
}

fn list_file() -> PathBuf {
    crate::dirs::data().join("trusted-folders")
}

/// How `path` compares: `/` between its parts, and on Windows in one case
/// and without the verbatim prefix.
fn key(path: &str) -> String {
    let path = path.strip_prefix(r"\\?\").unwrap_or(path).replace('\\', "/");
    let path = if path.len() > 1 { path.trim_end_matches('/') } else { &path };
    if cfg!(windows) { path.to_lowercase() } else { path.to_owned() }
}

/// `folder` or a folder above it is among `known`.
fn covered<'a>(mut known: impl Iterator<Item = &'a str>, folder: &Path) -> bool {
    let above: Vec<String> = folder.ancestors().map(|p| key(&p.to_string_lossy())).collect();
    known.any(|k| above.contains(&key(k)))
}

/// Whether `config`, a `.claude.json`, trusts `folder`.
fn claude_trusts(config: &serde_json::Value, folder: &Path) -> bool {
    let Some(projects) = config.get("projects").and_then(|p| p.as_object()) else { return false };
    let accepted = |p: &serde_json::Value| p.get("hasTrustDialogAccepted").and_then(|t| t.as_bool()) == Some(true);
    covered(projects.iter().filter(|(_, p)| accepted(p)).map(|(k, _)| k.as_str()), folder)
}

/// Whether the list `text`, a folder a line, trusts `folder`.
fn listed(text: &str, folder: &Path) -> bool {
    covered(text.lines().filter(|l| !l.trim().is_empty()), folder)
}

/// Add `folder` to the list at `file`.
fn add(file: &Path, folder: &Path) -> std::io::Result<()> {
    let mut text = std::fs::read_to_string(file).unwrap_or_default();
    if listed(&text, folder) {
        return Ok(());
    }
    text.push_str(&folder.to_string_lossy());
    text.push('\n');
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(file, text)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::*;

    #[test]
    fn claude_trusts_a_folder_and_what_is_below_it() {
        let config = json!({"projects": {
            "/work/repo": {"hasTrustDialogAccepted": true},
            "/work/other": {"hasTrustDialogAccepted": false},
            "/work/plain": {},
        }});
        assert!(claude_trusts(&config, Path::new("/work/repo")));
        assert!(claude_trusts(&config, Path::new("/work/repo/crates/app")));
        assert!(!claude_trusts(&config, Path::new("/work/other")));
        assert!(!claude_trusts(&config, Path::new("/work/plain/sub")));
        assert!(!claude_trusts(&config, Path::new("/work/repository")));
        assert!(!claude_trusts(&config, Path::new("/work")));
        assert!(!claude_trusts(&json!({}), Path::new("/work/repo")));
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_match_as_windows_does() {
        let config = json!({"projects": {"D:/ws/Tern": {"hasTrustDialogAccepted": true}}});
        assert!(claude_trusts(&config, Path::new(r"d:\ws\tern\crates")));
        assert!(claude_trusts(&config, Path::new(r"\\?\D:\ws\tern")));
    }

    #[test]
    fn the_users_answers_cover_what_is_below_them() {
        assert!(listed("/work/a\n/work/repo\n", Path::new("/work/repo/sub")));
        assert!(!listed("/work/a\n", Path::new("/work/b")));
        assert!(!listed("", Path::new("/")));
    }

    #[test]
    fn an_answer_is_kept_once() {
        let dir = std::env::temp_dir().join(format!("tern-trust-{}", std::process::id()));
        let file = dir.join("deep").join("trusted-folders");
        add(&file, Path::new("/work/repo")).unwrap();
        add(&file, Path::new("/work/repo")).unwrap();
        add(&file, Path::new("/work/b")).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(listed(&text, Path::new("/work/b")));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
