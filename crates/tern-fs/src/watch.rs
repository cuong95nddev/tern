//! Recursive FSEvents watcher with debouncing. Emits batches of changed paths.
//! `.git/` internals (of the root repo and any nested repo) are dropped except
//! the files that change git status (reported per repo), and so is everything inside heavy build
//! dirs ([`crate::git::SKIP_DIRS`]: `node_modules`, `target`, ...).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeBatch {
    /// Changed worktree paths (absolute).
    pub paths: Vec<PathBuf>,
    /// Worktree roots of the repos whose `.git` changed (HEAD/index/refs):
    /// their git status is stale.
    pub git_repos: Vec<PathBuf>,
}

pub struct FsWatcher {
    _watcher: RecommendedWatcher,
}

impl FsWatcher {
    /// Watch `root` recursively. `on_batch` runs on a background thread once
    /// changes have been quiet for `debounce`, or `MAX_WAIT_FACTOR` times
    /// that after the first one in a long burst.
    pub fn new(root: &Path, debounce: Duration, on_batch: impl Fn(ChangeBatch) + Send + 'static) -> notify::Result<Self> {
        let (tx, rx) = mpsc::channel::<notify::Event>();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                let _ = tx.send(ev);
            }
        })?;
        watcher.watch(root, RecursiveMode::Recursive)?;
        let root = root.to_owned();
        thread::Builder::new()
            .name("fs-debounce".into())
            .spawn(move || debounce_loop(&root, rx, debounce, on_batch))
            .expect("spawn fs-debounce");
        Ok(Self { _watcher: watcher })
    }
}

/// A burst is cut into batches at most this many debounce windows long.
const MAX_WAIT_FACTOR: u32 = 10;

fn debounce_loop(root: &Path, rx: mpsc::Receiver<notify::Event>, window: Duration, on_batch: impl Fn(ChangeBatch)) {
    loop {
        // Block for the first event.
        let Ok(first) = rx.recv() else { return };
        let start = Instant::now();
        let cap = start + window * MAX_WAIT_FACTOR;
        let mut deadline = start + window;
        let mut paths: HashSet<PathBuf> = HashSet::new();
        let mut git_repos: HashSet<PathBuf> = HashSet::new();
        // Whether `ev` kept a path: only that extends the wait, so a busy
        // `target/` or `.git/objects` can't hold a batch back.
        let mut absorb = |ev: notify::Event| {
            let mut kept = false;
            if matches!(ev.kind, EventKind::Access(_)) {
                return kept;
            }
            for p in ev.paths {
                if let Some((repo, rel)) = git_internal(&p) {
                    if is_git_status_file(rel) {
                        git_repos.insert(repo.to_path_buf());
                        kept = true;
                    }
                } else if !in_skipped_dir(root, &p) {
                    paths.insert(p);
                    kept = true;
                }
            }
            kept
        };
        absorb(first);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(ev) => {
                    if absorb(ev) {
                        deadline = (Instant::now() + window).min(cap);
                    }
                },
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
        if paths.is_empty() && git_repos.is_empty() {
            continue;
        }
        let mut paths: Vec<_> = paths.into_iter().collect();
        paths.sort();
        let mut git_repos: Vec<_> = git_repos.into_iter().collect();
        git_repos.sort();
        on_batch(ChangeBatch { paths, git_repos });
    }
}

/// For a path inside some `.git` dir, the worktree root (the dir holding
/// `.git`) and the part after `.git/`.
fn git_internal(p: &Path) -> Option<(&Path, &Path)> {
    let at = p.components().position(|c| c.as_os_str() == ".git")?;
    let git = p.ancestors().nth(p.components().count() - at - 1)?;
    let rest = p.strip_prefix(git).ok()?;
    (!rest.as_os_str().is_empty()).then_some((git.parent()?, rest))
}

/// Whether `p` is inside (not just named) a skipped dir below `root`.
fn in_skipped_dir(root: &Path, p: &Path) -> bool {
    let Ok(rel) = p.strip_prefix(root) else { return false };
    let mut comps: Vec<_> = rel.components().collect();
    comps.pop();
    comps.iter().any(|c| c.as_os_str().to_str().is_some_and(|n| crate::git::SKIP_DIRS.contains(&n)))
}

fn is_git_status_file(rel: &Path) -> bool {
    let s = rel.to_string_lossy();
    // `index.lock` too: when git gives up a lock without writing `index`,
    // its removal is the only sign that a skipped status can run now. And
    // the config: it decides whether status is safe to run at all.
    matches!(s.as_ref(), "HEAD" | "index" | "index.lock" | "config" | "config.worktree") || s.starts_with("refs/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn batches_changes() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().canonicalize().unwrap();
        let (tx, rx) = mpsc::channel();
        let _w = FsWatcher::new(&root, Duration::from_millis(100), move |b| {
            let _ = tx.send(b);
        })
        .unwrap();
        thread::sleep(Duration::from_millis(200));
        fs::write(root.join("a.txt"), "1").unwrap();
        fs::write(root.join("b.txt"), "2").unwrap();
        let mut seen = HashSet::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while seen.len() < 2 && Instant::now() < deadline {
            if let Ok(b) = rx.recv_timeout(Duration::from_millis(500)) {
                seen.extend(b.paths);
            }
        }
        assert!(seen.contains(&root.join("a.txt")), "{seen:?}");
        assert!(seen.contains(&root.join("b.txt")), "{seen:?}");
    }

    #[test]
    fn skips_build_dirs() {
        let root = Path::new("/r");
        assert!(in_skipped_dir(root, Path::new("/r/node_modules/x/index.js")));
        assert!(in_skipped_dir(root, Path::new("/r/app/target/debug/foo")));
        // The dir itself still shows up (created / deleted).
        assert!(!in_skipped_dir(root, Path::new("/r/node_modules")));
        assert!(!in_skipped_dir(root, Path::new("/r/src/build.rs")));
        // Only below the root: a root inside `target` is fine.
        assert!(!in_skipped_dir(Path::new("/x/target/r"), Path::new("/x/target/r/a.rs")));
    }

    #[test]
    fn classifies_nested_git_internals() {
        assert_eq!(git_internal(Path::new("/r/sub/.git/index")), Some((Path::new("/r/sub"), Path::new("index"))));
        assert_eq!(git_internal(Path::new("/r/.git")), None);
        assert_eq!(git_internal(Path::new("/r/src/a.rs")), None);
        assert!(is_git_status_file(Path::new("refs/heads/main")));
        assert!(is_git_status_file(Path::new("index.lock")));
        assert!(is_git_status_file(Path::new("config")));
        assert!(!is_git_status_file(Path::new("objects/ab/cd")));
    }
}
