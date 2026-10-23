//! Lazily-loaded, gitignore-aware file tree flattened into visible rows for a
//! virtualized list. Directories are read only when expanded; a big one is
//! best read off the main thread with [`list_dir`] and handed over with
//! [`FileTree::loaded`]. Watcher changes are applied entry by entry, without
//! reading the folder again.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ignore::Match;
use ignore::gitignore::Gitignore;

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

/// What [`FileTree::apply_changes`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// The rows changed.
    pub changed: bool,
    /// Folders to read again: their ignore rules changed, or too many of
    /// their entries did to update one by one.
    pub reread: Vec<PathBuf>,
}

/// More changes than this in one folder and it is read again instead.
const MAX_ONE_BY_ONE: usize = 256;

#[derive(Debug)]
pub struct FileTree {
    root: PathBuf,
    show_hidden: bool,
    /// Each read folder's entries, in display order.
    children: HashMap<PathBuf, Vec<Entry>>,
    expanded: HashSet<PathBuf>,
    rows: Vec<Row>,
    widest: usize,
    /// Folders being read elsewhere, each with whether a change in it came
    /// in meanwhile (which the listing may have missed).
    loading: HashMap<PathBuf, bool>,
}

impl FileTree {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let mut t = Self::unloaded(root);
        let root = t.root.clone();
        t.load_dir(&root);
        t.rebuild_rows();
        t
    }

    /// A tree with nothing read yet: the root's listing comes in through
    /// [`FileTree::loaded`].
    pub fn unloaded(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            expanded: HashSet::from([root.clone()]),
            root,
            show_hidden: true,
            children: HashMap::new(),
            rows: vec![],
            widest: 0,
            loading: HashMap::new(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The row likely to be widest: two characters of indent a level, then
    /// the name.
    pub fn widest_row(&self) -> usize {
        self.widest
    }

    pub fn is_expanded(&self, path: &Path) -> bool {
        self.expanded.contains(path)
    }

    /// `dir`, if it is a folder not read yet: what [`FileTree::expand`] reads.
    pub fn unread(&self, dir: &Path) -> Vec<PathBuf> {
        if self.children.contains_key(dir) || !dir.is_dir() { vec![] } else { vec![dir.to_path_buf()] }
    }

    /// The folders, the root down to `path`'s parent, not read yet: what
    /// [`FileTree::reveal`] reads.
    pub fn unread_above(&self, path: &Path) -> Vec<PathBuf> {
        let Ok(rel) = path.strip_prefix(&self.root) else { return vec![] };
        let mut dirs = vec![self.root.clone()];
        let comps: Vec<_> = rel.components().collect();
        for c in comps.iter().take(comps.len().saturating_sub(1)) {
            dirs.push(dirs[dirs.len() - 1].join(c));
        }
        dirs.retain(|d| !self.children.contains_key(d));
        dirs
    }

    /// Every folder read so far.
    pub fn loaded_dirs(&self) -> Vec<PathBuf> {
        self.children.keys().cloned().collect()
    }

    /// `dir` is being read elsewhere: changes in it meanwhile are noted.
    pub fn start_loading(&mut self, dir: &Path) {
        self.loading.entry(dir.to_path_buf()).or_insert(false);
    }

    /// Put in listings read elsewhere ([`list_dir`]). Returns the folders
    /// that changed while being read: read them again.
    pub fn loaded(&mut self, listings: Vec<(PathBuf, Vec<Entry>)>) -> Vec<PathBuf> {
        let mut again = vec![];
        for (dir, entries) in listings {
            if self.loading.remove(&dir) == Some(true) {
                again.push(dir.clone());
            }
            if dir.is_dir() {
                self.children.insert(dir, entries);
            } else if dir != self.root {
                self.forget(&dir);
            }
        }
        self.rebuild_rows();
        again
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
        for dir in self.unread_above(path) {
            self.load_dir(&dir);
        }
        let rel = path.strip_prefix(&self.root).ok()?;
        let mut cur = self.root.clone();
        let comps: Vec<_> = rel.components().collect();
        for c in comps.iter().take(comps.len().saturating_sub(1)) {
            cur.push(c);
            self.expanded.insert(cur.clone());
        }
        self.rebuild_rows();
        self.rows.iter().position(|r| &*r.entry.path == path)
    }

    /// Apply a batch of changed paths from the watcher: each one's entry in
    /// its folder, if that folder was read, is added, removed or kept as the
    /// disk now has it.
    pub fn apply_changes<'a>(&mut self, paths: impl IntoIterator<Item = &'a Path>) -> Applied {
        let mut changed = false;
        let mut reread: HashSet<PathBuf> = HashSet::new();
        let mut by_dir: HashMap<PathBuf, Vec<&Path>> = HashMap::new();
        for p in paths {
            // A read folder now gone (or a file): forget what was under it.
            if p != self.root && self.children.contains_key(p) && !p.is_dir() {
                self.forget(p);
                changed = true;
            }
            let Some(parent) = p.parent() else { continue };
            if let Some(missed) = self.loading.get_mut(parent) {
                *missed = true;
            }
            if !self.children.contains_key(parent) {
                continue;
            }
            if matches!(p.file_name().and_then(OsStr::to_str), Some(".gitignore" | ".ignore")) {
                reread.extend(self.children.keys().filter(|d| d.starts_with(parent)).cloned());
            }
            by_dir.entry(parent.to_path_buf()).or_default().push(p);
        }
        for (dir, paths) in by_dir {
            if paths.len() > MAX_ONE_BY_ONE {
                reread.insert(dir);
                continue;
            }
            let rules = Rules::for_dir(&dir);
            for p in paths {
                changed |= self.update_entry(&dir, p, &rules);
            }
        }
        if changed {
            self.rebuild_rows();
        }
        let mut reread: Vec<PathBuf> = reread.into_iter().collect();
        reread.sort();
        Applied { changed, reread }
    }

    /// Fold every folder back to the root's children.
    pub fn collapse_all(&mut self) {
        self.expanded = HashSet::from([self.root.clone()]);
        self.rebuild_rows();
    }

    /// `from` was renamed or moved to `to`: what was read under it and its
    /// expanded folders carry over, and both parents are updated.
    pub fn moved(&mut self, from: &Path, to: &Path) -> Applied {
        let follow =
            |p: &Path| p.strip_prefix(from).ok().map(|rel| if rel.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rel) });
        self.children = std::mem::take(&mut self.children)
            .into_iter()
            .map(|(dir, entries)| match follow(&dir) {
                Some(dir) => {
                    let entries = entries.into_iter().map(|e| Entry { path: follow(&e.path).map_or(e.path, Into::into), ..e }).collect();
                    (dir, entries)
                },
                None => (dir, entries),
            })
            .collect();
        self.expanded = std::mem::take(&mut self.expanded).into_iter().map(|p| follow(&p).unwrap_or(p)).collect();
        self.apply_changes([from, to])
    }

    /// The visible row showing `path`.
    pub fn row_of(&self, path: &Path) -> Option<usize> {
        self.rows.iter().position(|r| &*r.entry.path == path)
    }

    fn load_dir(&mut self, dir: &Path) {
        self.children.insert(dir.to_path_buf(), list_dir(dir, self.show_hidden));
    }

    /// Drop what was read under `dir` (and it), and its expanded folders.
    fn forget(&mut self, dir: &Path) {
        self.children.retain(|p, _| !p.starts_with(dir));
        let root = &self.root;
        self.expanded.retain(|p| p == root || !p.starts_with(dir));
    }

    /// Bring `path`'s entry in `dir` in line with the disk; whether it changed.
    fn update_entry(&mut self, dir: &Path, path: &Path, rules: &Rules) -> bool {
        let Some(name) = path.file_name() else { return false };
        let name = name.to_string_lossy();
        let now = entry_for(path, rules, self.show_hidden);
        let Some(entries) = self.children.get_mut(dir) else { return false };
        let was = [true, false].into_iter().find_map(|is_dir| entries.binary_search_by(|e| order(e.is_dir, &e.name, is_dir, &name)).ok());
        let gone_dir = match (was, &now) {
            (Some(i), Some(e)) if entries[i].is_dir == e.is_dir => return false,
            (None, None) => return false,
            (Some(i), _) => Some(entries.remove(i)).filter(|e| e.is_dir),
            (None, Some(_)) => None,
        };
        if let Some(e) = now {
            let at = entries.binary_search_by(|x| order(x.is_dir, &x.name, e.is_dir, &e.name)).unwrap_or_else(|i| i);
            entries.insert(at, e);
        }
        if let Some(gone) = gone_dir {
            self.forget(&gone.path);
        }
        true
    }

    fn rebuild_rows(&mut self) {
        let mut rows = Vec::with_capacity(self.rows.len().max(64));
        self.push_children(&self.root, 0, &mut rows);
        self.widest = rows.iter().enumerate().max_by_key(|(_, r)| r.depth * 2 + r.entry.name.chars().count()).map_or(0, |(i, _)| i);
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

/// Display order: folders first, then by name ignoring case.
fn order(a_dir: bool, a: &str, b_dir: bool, b: &str) -> Ordering {
    b_dir.cmp(&a_dir).then_with(|| a.to_lowercase().cmp(&b.to_lowercase())).then_with(|| a.cmp(b))
}

fn shown_name(name: &OsStr, show_hidden: bool) -> bool {
    name != ".git" && name != ".DS_Store" && (show_hidden || !name.as_encoded_bytes().starts_with(b"."))
}

/// One folder's entries in display order, honoring .gitignore / .ignore /
/// global excludes.
pub fn list_dir(dir: &Path, show_hidden: bool) -> Vec<Entry> {
    let rules = Rules::for_dir(dir);
    let Ok(read) = std::fs::read_dir(dir) else { return vec![] };
    let mut entries: Vec<Entry> = read
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name();
            if !shown_name(&name, show_hidden) {
                return None;
            }
            let path = e.path();
            let ft = e.file_type().ok()?;
            let is_dir = ft.is_dir() || (ft.is_symlink() && path.is_dir());
            (!rules.ignored(&path, is_dir)).then(|| Entry { name: name.to_string_lossy().into(), path: path.into(), is_dir })
        })
        .collect();
    entries.sort_by_cached_key(|e| (!e.is_dir, e.name.to_lowercase(), e.name.clone()));
    entries
}

/// `path` as an entry of its folder, if the folder shows it.
fn entry_for(path: &Path, rules: &Rules, show_hidden: bool) -> Option<Entry> {
    let name = path.file_name()?;
    if !shown_name(name, show_hidden) {
        return None;
    }
    let ft = std::fs::symlink_metadata(path).ok()?.file_type();
    let is_dir = ft.is_dir() || (ft.is_symlink() && path.is_dir());
    (!rules.ignored(path, is_dir)).then(|| Entry { name: name.to_string_lossy().into(), path: path.into(), is_dir })
}

static GLOBAL_EXCLUDES: Mutex<Option<Arc<Gitignore>>> = Mutex::new(None);

fn global_excludes() -> Arc<Gitignore> {
    GLOBAL_EXCLUDES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(|| Arc::new(Gitignore::global().0)).clone()
}

/// Read the user's global git excludes again on next use.
pub fn reload_global_excludes() {
    *GLOBAL_EXCLUDES.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// The ignore rules for the entries of one folder, as `ignore`'s walker
/// applies them: the `.ignore` files in it and every folder above, then the
/// `.gitignore` files up to the repo root, the repo's `info/exclude`, the
/// global excludes. The first that has a pattern for an entry decides.
struct Rules {
    files: Vec<Gitignore>,
    global: Arc<Gitignore>,
}

impl Rules {
    fn for_dir(dir: &Path) -> Self {
        let read = |path: PathBuf| Some(Gitignore::new(path).0).filter(|g| !g.is_empty());
        let (mut ignore, mut git, mut exclude) = (vec![], vec![], None);
        let mut in_repo = true;
        for d in dir.ancestors() {
            ignore.extend(read(d.join(".ignore")));
            if in_repo {
                git.extend(read(d.join(".gitignore")));
                let dot_git = d.join(".git");
                if dot_git.exists() {
                    exclude = git_dir(&dot_git).and_then(|g| read(g.join("info").join("exclude")));
                    in_repo = false;
                }
            }
        }
        ignore.extend(git);
        ignore.extend(exclude);
        Self { files: ignore, global: global_excludes() }
    }

    fn ignored(&self, path: &Path, is_dir: bool) -> bool {
        for rules in self.files.iter().chain([&*self.global]) {
            match rules.matched(path, is_dir) {
                Match::Ignore(_) => return true,
                Match::Whitelist(_) => return false,
                Match::None => {},
            }
        }
        false
    }
}

/// The git dir a `.git` entry stands for: itself, or the one a worktree's
/// `.git` file names, through to the repo's common dir that holds `info/`.
fn git_dir(dot_git: &Path) -> Option<PathBuf> {
    if dot_git.is_dir() {
        return Some(dot_git.to_path_buf());
    }
    let text = std::fs::read_to_string(dot_git).ok()?;
    let dir = dot_git.parent()?.join(text.strip_prefix("gitdir:")?.trim());
    match std::fs::read_to_string(dir.join("commondir")) {
        Ok(common) => Some(dir.join(common.trim())),
        Err(_) => Some(dir),
    }
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
        assert!(t.apply_changes([new.as_path()]).changed);
        assert!(names(&t).contains(&"new.rs".to_string()));
        // Its content changing leaves the rows be.
        fs::write(&new, "x").unwrap();
        assert!(!t.apply_changes([new.as_path()]).changed);
        fs::remove_file(&new).unwrap();
        assert!(t.apply_changes([new.as_path()]).changed);
        assert!(!names(&t).contains(&"new.rs".to_string()));
        // Change in an unloaded dir is a no-op.
        assert_eq!(t.apply_changes([d.path().join("src/zzz.rs").as_path()]), Applied::default());
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

    #[test]
    fn changes_go_in_one_by_one_in_order_and_honor_ignores() {
        let d = fixture();
        let mut t = FileTree::new(d.path());
        fs::create_dir(d.path().join("a_dir")).unwrap();
        fs::write(d.path().join("C.txt"), "").unwrap();
        fs::write(d.path().join("other.log"), "").unwrap();
        let paths = ["a_dir", "C.txt", "other.log"].map(|n| d.path().join(n));
        let applied = t.apply_changes(paths.iter().map(PathBuf::as_path));
        assert_eq!(applied, Applied { changed: true, reread: vec![] });
        assert_eq!(names(&t), vec!["a_dir", "src", ".gitignore", "b.txt", "C.txt", "README.md"]);
        // A file replaced by a folder of the same name moves up.
        fs::remove_file(d.path().join("b.txt")).unwrap();
        fs::create_dir(d.path().join("b.txt")).unwrap();
        t.apply_changes([d.path().join("b.txt").as_path()]);
        assert_eq!(names(&t), vec!["a_dir", "b.txt", "src", ".gitignore", "C.txt", "README.md"]);
    }

    #[test]
    fn removed_folder_forgets_what_was_under_it() {
        let d = fixture();
        let mut t = FileTree::new(d.path());
        t.reveal(&d.path().join("src/nested/x.rs"));
        fs::remove_dir_all(d.path().join("src")).unwrap();
        let src = d.path().join("src");
        assert!(t.apply_changes([src.as_path(), src.join("nested").as_path()]).changed);
        assert_eq!(names(&t), vec![".gitignore", "b.txt", "README.md"]);
        assert!(!t.loaded_dirs().iter().any(|p| p.starts_with(&src)));
        fs::create_dir(&src).unwrap();
        t.apply_changes([src.as_path()]);
        assert!(!t.is_expanded(&src));
    }

    #[test]
    fn many_changes_or_new_ignore_rules_ask_for_a_reread() {
        let d = fixture();
        let mut t = FileTree::new(d.path());
        t.expand(&d.path().join("src"));
        let many: Vec<PathBuf> = (0..=MAX_ONE_BY_ONE).map(|i| d.path().join(format!("f{i}"))).collect();
        assert_eq!(t.apply_changes(many.iter().map(PathBuf::as_path)).reread, vec![d.path().to_path_buf()]);
        let applied = t.apply_changes([d.path().join(".gitignore").as_path()]);
        assert_eq!(applied.reread, vec![d.path().to_path_buf(), d.path().join("src")]);
    }

    #[test]
    fn loads_elsewhere_and_rereads_what_changed_meanwhile() {
        let d = fixture();
        let mut t = FileTree::unloaded(d.path());
        assert!(t.rows().is_empty());
        let x = d.path().join("src/nested/x.rs");
        assert_eq!(t.unread_above(&x), vec![d.path().to_path_buf(), d.path().join("src"), d.path().join("src/nested")]);
        for dir in [d.path(), &d.path().join("src")] {
            t.start_loading(dir);
        }
        let listings = [d.path().to_path_buf(), d.path().join("src")].map(|p| (p.clone(), list_dir(&p, true)));
        t.apply_changes([d.path().join("src/new.rs").as_path()]);
        assert_eq!(t.loaded(listings.into()), vec![d.path().join("src")]);
        assert_eq!(names(&t), vec!["src", ".gitignore", "b.txt", "README.md"]);
        assert_eq!(t.unread_above(&x), vec![d.path().join("src/nested")]);
        assert_eq!(t.reveal(&x), Some(2));
    }

    #[test]
    fn list_dir_follows_the_walkers_ignore_rules() {
        let d = tempfile::tempdir().unwrap();
        let outer = d.path();
        // Above the repo: not applied. Inside it: nested files, negation,
        // `.ignore` over `.gitignore`, `info/exclude`.
        fs::write(outer.join(".gitignore"), "*.md\n").unwrap();
        let r = outer.join("repo");
        fs::create_dir_all(r.join(".git/info")).unwrap();
        fs::create_dir_all(r.join("sub")).unwrap();
        fs::write(r.join(".git/info/exclude"), "secret\n").unwrap();
        fs::write(r.join(".gitignore"), "*.log\nbuild/\n").unwrap();
        fs::write(r.join("sub/.gitignore"), "!keep.log\n").unwrap();
        fs::write(r.join("sub/.ignore"), "keep.log\n").unwrap();
        for f in ["a.md", "secret", "x.log", "sub/keep.log", "sub/y.log", "sub/z.rs"] {
            fs::write(r.join(f), "").unwrap();
        }
        fs::create_dir_all(r.join("sub/build")).unwrap();
        let ls = |dir: &Path| list_dir(dir, true).iter().map(|e| e.name.to_string()).collect::<Vec<_>>();
        assert_eq!(ls(&r), vec!["sub", ".gitignore", "a.md"]);
        assert_eq!(ls(&r.join("sub")), vec![".gitignore", ".ignore", "z.rs"]);
        // Outside any repo, a parent's `.gitignore` counts.
        let plain = outer.join("plain");
        fs::create_dir(&plain).unwrap();
        fs::write(plain.join("b.md"), "").unwrap();
        fs::write(plain.join("c.rs"), "").unwrap();
        assert_eq!(ls(&plain), vec!["c.rs"]);
    }
}
