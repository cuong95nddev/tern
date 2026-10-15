//! Recursive FSEvents watcher with debouncing. Emits batches of changed paths.
//! `.git/` internals (of the root repo and any nested repo) are dropped except
//! the files that change git status.

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
    /// Something under `.git` changed (HEAD/index/refs) — git status is stale.
    pub git_changed: bool,
}

pub struct FsWatcher {
    _watcher: RecommendedWatcher,
}

impl FsWatcher {
    /// Watch `root` recursively. `on_batch` runs on a background thread at most
    /// once per `debounce` window.
    pub fn new(
        root: &Path,
        debounce: Duration,
        on_batch: impl Fn(ChangeBatch) + Send + 'static,
    ) -> notify::Result<Self> {
        let (tx, rx) = mpsc::channel::<notify::Event>();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                let _ = tx.send(ev);
            }
        })?;
        watcher.watch(root, RecursiveMode::Recursive)?;
        thread::Builder::new()
            .name("fs-debounce".into())
            .spawn(move || debounce_loop(rx, debounce, on_batch))
            .expect("spawn fs-debounce");
        Ok(Self { _watcher: watcher })
    }
}

fn debounce_loop(rx: mpsc::Receiver<notify::Event>, window: Duration, on_batch: impl Fn(ChangeBatch)) {
    loop {
        // Block for the first event.
        let Ok(first) = rx.recv() else { return };
        let deadline = Instant::now() + window;
        let mut paths: HashSet<PathBuf> = HashSet::new();
        let mut git_changed = false;
        let mut absorb = |ev: notify::Event| {
            if matches!(ev.kind, EventKind::Access(_)) {
                return;
            }
            for p in ev.paths {
                if let Some(rel) = git_internal(&p) {
                    git_changed |= is_git_status_file(rel);
                } else {
                    paths.insert(p);
                }
            }
        };
        absorb(first);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(ev) => absorb(ev),
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
        if paths.is_empty() && !git_changed {
            continue;
        }
        let mut paths: Vec<_> = paths.into_iter().collect();
        paths.sort();
        on_batch(ChangeBatch { paths, git_changed });
    }
}

/// For a path inside some `.git` dir, the part after `.git/`.
fn git_internal(p: &Path) -> Option<&Path> {
    let mut comps = p.components();
    while let Some(c) = comps.next() {
        if c.as_os_str() == ".git" {
            let rest = comps.as_path();
            return (!rest.as_os_str().is_empty()).then_some(rest);
        }
    }
    None
}

fn is_git_status_file(rel: &Path) -> bool {
    let s = rel.to_string_lossy();
    s == "HEAD" || s == "index" || s.starts_with("refs/")
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
    fn classifies_nested_git_internals() {
        assert_eq!(git_internal(Path::new("/r/sub/.git/index")), Some(Path::new("index")));
        assert_eq!(git_internal(Path::new("/r/.git")), None);
        assert_eq!(git_internal(Path::new("/r/src/a.rs")), None);
        assert!(is_git_status_file(Path::new("refs/heads/main")));
        assert!(!is_git_status_file(Path::new("objects/ab/cd")));
    }
}
