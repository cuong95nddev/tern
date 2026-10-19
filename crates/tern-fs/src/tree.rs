//! Lazily-loaded, gitignore-aware file tree flattened into visible rows for a
//! virtualized list. Directories are read only when expanded.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ignore::WalkBuilder;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: Arc<Path>,
    pub name: Arc<str>,
    pub is_dir: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub entry: Entry,
    pub depth: usize,
    pub expanded: bool,
}

#[derive(Debug)]
pub struct FileTree {
    root: PathBuf,
    show_hidden: bool,
    children: HashMap<PathBuf, Vec<Entry>>,
    expanded: HashSet<PathBuf>,
    rows: Vec<Row>,
}

impl FileTree {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let mut t =
            Self { root: root.clone(), show_hidden: true, children: HashMap::new(), expanded: HashSet::from([root.clone()]), rows: vec![] };
        t.load_dir(&root);
        t.rebuild_rows();
        t
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn is_expanded(&self, path: &Path) -> bool {
        self.expanded.contains(path)
    }

    pub fn toggle(&mut self, path: &Path) {
        if self.expanded.contains(path) {
            self.collapse(path);
        } else {
            self.expand(path);
        }
    }

    pub fn expand(&mut self, path: &Path) {
        if !path.is_dir() {
            return;
        }
        if !self.children.contains_key(path) {
            self.load_dir(path);
        }
        self.expanded.insert(path.to_path_buf());
        self.rebuild_rows();
    }

    pub fn collapse(&mut self, path: &Path) {
        if path != self.root && self.expanded.remove(path) {
            self.rebuild_rows();
        }
    }

    /// Expand every ancestor of `path` so it becomes visible; returns its row index.
    pub fn reveal(&mut self, path: &Path) -> Option<usize> {
        let rel = path.strip_prefix(&self.root).ok()?;
        let mut cur = self.root.clone();
        let comps: Vec<_> = rel.components().collect();
        for c in comps.iter().take(comps.len().saturating_sub(1)) {
            cur.push(c);
            if !self.children.contains_key(&cur) {
                self.load_dir(&cur);
            }
            self.expanded.insert(cur.clone());
        }
        self.rebuild_rows();
        self.rows.iter().position(|r| &*r.entry.path == path)
    }

    /// Apply a batch of changed paths from the watcher: reload the parent
    /// directory of each change if that directory is loaded.
    /// Returns true if visible rows changed.
    pub fn apply_changes<'a>(&mut self, paths: impl IntoIterator<Item = &'a Path>) -> bool {
        let mut dirs: HashSet<PathBuf> = HashSet::new();
        for p in paths {
            if let Some(parent) = p.parent()
                && self.children.contains_key(parent)
            {
                dirs.insert(parent.to_path_buf());
            }
            // A changed directory itself (e.g. removed) — drop stale cache.
            if self.children.contains_key(p) && !p.is_dir() {
                self.children.remove(p);
                self.expanded.remove(p);
                dirs.extend(p.parent().map(Path::to_path_buf));
            }
        }
        if dirs.is_empty() {
            return false;
        }
        let before = self.rows.clone();
        for d in dirs {
            if d.is_dir() {
                self.load_dir(&d);
            } else {
                self.children.remove(&d);
                self.expanded.remove(&d);
            }
        }
        self.rebuild_rows();
        before != self.rows
    }

    /// Fold every folder back to the root's children.
    pub fn collapse_all(&mut self) {
        self.expanded = HashSet::from([self.root.clone()]);
        self.rebuild_rows();
    }

    /// `from` was renamed or moved to `to`: carry its expanded folders over
    /// and reload both parents.
    pub fn moved(&mut self, from: &Path, to: &Path) {
        let opened: Vec<PathBuf> = self.expanded.iter().filter_map(|p| p.strip_prefix(from).ok().map(|r| to.join(r))).collect();
        self.expanded.retain(|p| !p.starts_with(from));
        self.children.retain(|p, _| !p.starts_with(from));
        for p in opened {
            if p.is_dir() {
                self.load_dir(&p);
                self.expanded.insert(p);
            }
        }
        for parent in [from.parent(), to.parent()].into_iter().flatten() {
            if self.children.contains_key(parent) {
                self.load_dir(parent);
            }
        }
        self.rebuild_rows();
    }

    /// The visible row showing `path`.
    pub fn row_of(&self, path: &Path) -> Option<usize> {
        self.rows.iter().position(|r| &*r.entry.path == path)
    }

    pub fn refresh_all(&mut self) {
        let dirs: Vec<PathBuf> = self.children.keys().cloned().collect();
        for d in dirs {
            if d.is_dir() {
                self.load_dir(&d);
            } else {
                self.children.remove(&d);
                self.expanded.remove(&d);
            }
        }
        self.rebuild_rows();
    }

    fn load_dir(&mut self, dir: &Path) {
        let mut entries = read_dir_filtered(dir, self.show_hidden);
        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| natural_lower(&a.name).cmp(&natural_lower(&b.name))));
        self.children.insert(dir.to_path_buf(), entries);
    }

    fn rebuild_rows(&mut self) {
        let mut rows = Vec::with_capacity(self.rows.len().max(64));
        self.push_children(&self.root.clone(), 0, &mut rows);
        self.rows = rows;
    }

    fn push_children(&self, dir: &Path, depth: usize, rows: &mut Vec<Row>) {
        let Some(children) = self.children.get(dir) else { return };
        for e in children {
            let expanded = e.is_dir && self.expanded.contains(&*e.path);
            rows.push(Row { entry: e.clone(), depth, expanded });
            if expanded {
                self.push_children(&e.path, depth + 1, rows);
            }
        }
    }
}

fn natural_lower(s: &str) -> String {
    s.to_lowercase()
}

/// Read one directory level honoring .gitignore / .ignore / global excludes.
pub fn read_dir_filtered(dir: &Path, show_hidden: bool) -> Vec<Entry> {
    let walker = WalkBuilder::new(dir)
        .max_depth(Some(1))
        .hidden(!show_hidden)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .require_git(false)
        .filter_entry(|e| e.file_name() != ".git" && e.file_name() != ".DS_Store")
        .build();
    walker
        .filter_map(Result::ok)
        .filter(|e| e.depth() == 1)
        .map(|e| {
            let is_dir = e.file_type().is_some_and(|t| t.is_dir()) || (e.path_is_symlink() && e.path().is_dir());
            Entry { name: e.file_name().to_string_lossy().into(), path: e.path().into(), is_dir }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        fs::create_dir_all(r.join("src/nested")).unwrap();
        fs::create_dir_all(r.join("target/debug")).unwrap();
        fs::create_dir_all(r.join(".git")).unwrap();
        fs::write(r.join(".gitignore"), "target/\n*.log\n").unwrap();
        fs::write(r.join("README.md"), "").unwrap();
        fs::write(r.join("b.txt"), "").unwrap();
        fs::write(r.join("debug.log"), "").unwrap();
        fs::write(r.join("src/main.rs"), "").unwrap();
        fs::write(r.join("src/nested/x.rs"), "").unwrap();
        d
    }

    fn names(t: &FileTree) -> Vec<String> {
        t.rows().iter().map(|r| format!("{}{}", "  ".repeat(r.depth), r.entry.name)).collect()
    }

    #[test]
    fn lists_root_dirs_first_and_respects_gitignore() {
        let d = fixture();
        let t = FileTree::new(d.path());
        assert_eq!(names(&t), vec!["src", ".gitignore", "b.txt", "README.md"]);
    }

    #[test]
    fn expand_collapse_and_reveal() {
        let d = fixture();
        let mut t = FileTree::new(d.path());
        t.expand(&d.path().join("src"));
        assert_eq!(names(&t), vec!["src", "  nested", "  main.rs", ".gitignore", "b.txt", "README.md"]);
        t.collapse(&d.path().join("src"));
        assert_eq!(t.rows().len(), 4);
        let idx = t.reveal(&d.path().join("src/nested/x.rs")).unwrap();
        assert_eq!(&*t.rows()[idx].entry.name, "x.rs");
        assert_eq!(t.rows()[idx].depth, 2);
    }

    #[test]
    fn apply_changes_reloads_parent() {
        let d = fixture();
        let mut t = FileTree::new(d.path());
        let new = d.path().join("new.rs");
        fs::write(&new, "").unwrap();
        assert!(t.apply_changes([new.as_path()]));
        assert!(names(&t).contains(&"new.rs".to_string()));
        fs::remove_file(&new).unwrap();
        assert!(t.apply_changes([new.as_path()]));
        assert!(!names(&t).contains(&"new.rs".to_string()));
        // Change in an unloaded dir is a no-op.
        assert!(!t.apply_changes([d.path().join("src/zzz.rs").as_path()]));
    }

    #[test]
    fn moved_keeps_expanded_folders() {
        let d = fixture();
        let mut t = FileTree::new(d.path());
        t.reveal(&d.path().join("src/nested/x.rs"));
        fs::rename(d.path().join("src"), d.path().join("lib")).unwrap();
        t.moved(&d.path().join("src"), &d.path().join("lib"));
        assert_eq!(names(&t), vec!["lib", "  nested", "    x.rs", "  main.rs", ".gitignore", "b.txt", "README.md"]);
        t.collapse_all();
        assert_eq!(names(&t), vec!["lib", ".gitignore", "b.txt", "README.md"]);
    }
}
