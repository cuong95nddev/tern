//! Git status via `git status --porcelain=v1 -z` (fast, no libgit2 dependency).

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
}

impl GitStatus {
    pub fn letter(self) -> char {
        match self {
            GitStatus::Modified => 'M',
            GitStatus::Added => 'A',
            GitStatus::Deleted => 'D',
            GitStatus::Renamed => 'R',
            GitStatus::Untracked => 'U',
            GitStatus::Conflicted => '!',
        }
    }
}

/// Changes of one repository, scoped to the opened folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoChanges {
    /// Repository worktree root.
    pub root: PathBuf,
    /// Branch line from `status -b`, e.g. `main...origin/main [ahead 1]`.
    pub branch: Option<String>,
    /// Changed files (absolute) in porcelain order.
    pub files: Vec<(PathBuf, GitStatus)>,
    /// Why status is unavailable (e.g. skipped for safety).
    pub error: Option<String>,
}

const MAX_SCAN_DEPTH: usize = 4;
const MAX_SCAN_DIRS: usize = 20_000;
/// Heavy build / dependency dirs: not searched for repos, and changes
/// inside them are not watched.
pub const SKIP_DIRS: &[&str] = &["node_modules", "target", ".venv", "venv", "__pycache__", "vendor", "dist", "build", ".build", "Pods"];

/// Git repositories relevant to `root`: the repo containing `root` (if any)
/// plus repos nested below it (up to [`MAX_SCAN_DEPTH`] levels; heavy build
/// dirs skipped). Sorted, outermost first. Symlinked dirs are not followed.
pub fn find_repos(root: &Path) -> Vec<PathBuf> {
    let mut repos = vec![];
    if let Some(top) = repo_for(root) {
        repos.push(top);
    }
    let mut queue = std::collections::VecDeque::from([(root.to_path_buf(), 0usize)]);
    let mut visited = 0;
    while let Some((dir, depth)) = queue.pop_front() {
        visited += 1;
        if visited > MAX_SCAN_DIRS {
            break;
        }
        if depth > 0 && dir.join(".git").exists() && !repos.contains(&dir) {
            repos.push(dir.clone());
        }
        if depth >= MAX_SCAN_DEPTH {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name == ".git" || SKIP_DIRS.contains(&name.as_ref()) {
                continue;
            }
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                queue.push_back((e.path(), depth + 1));
            }
        }
    }
    repos.sort();
    repos
}

/// Nearest ancestor of `path` (inclusive) that has a `.git` entry. Works for
/// paths that no longer exist (deleted files).
pub fn repo_for(path: &Path) -> Option<PathBuf> {
    path.ancestors().find(|a| a.join(".git").exists()).map(Path::to_path_buf)
}

/// The repo in `repos` that owns `path`: the deepest one containing it.
pub fn owner<'a>(repos: &'a [PathBuf], path: &Path) -> Option<&'a Path> {
    repos.iter().filter(|r| path.starts_with(r)).max_by_key(|r| r.components().count()).map(PathBuf::as_path)
}

/// Whether git is writing `repo`'s index right now. Status then would see a
/// half-done state (and may re-run clean filters without caching), so wait
/// for the lock to go away: the watcher reports that.
pub fn index_locked(repo: &Path) -> bool {
    repo.join(".git").join("index.lock").exists()
}

/// True if git ignores every one of `paths` (all inside `repo`), as one
/// `check-ignore` process: `.gitignore` at any depth, `info/exclude` and the
/// user's global excludes all count, and tracked files are never ignored.
/// On any failure, false: a needless refresh beats a missed change.
pub fn all_ignored(repo: &Path, paths: &[PathBuf]) -> bool {
    let mut rels = std::collections::BTreeSet::new();
    for p in paths {
        match p.strip_prefix(repo) {
            // `/` separators: git takes a Windows `\` as part of the name.
            // A leading `:` would be read as pathspec magic, which
            // `check-ignore` refuses (and `--literal-pathspecs` with it).
            Ok(rel) if !rel.as_os_str().is_empty() && !rel.to_string_lossy().starts_with(':') => {
                rels.insert(rel.iter().map(|c| c.to_string_lossy()).collect::<Vec<_>>().join("/"))
            },
            _ => return false,
        };
    }
    let child =
        git(repo).args(["check-ignore", "-z", "--stdin"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn();
    let Ok(mut child) = child else { return false };
    let input: Vec<u8> = rels.iter().flat_map(|r| r.bytes().chain([0])).collect();
    let mut stdin = child.stdin.take();
    // Feed stdin from another thread: git answers as it reads, and a full
    // stdout pipe would otherwise block both sides.
    let out = std::thread::scope(|s| {
        s.spawn(move || stdin.as_mut().map(|w| w.write_all(&input)));
        child.wait_with_output()
    });
    // Exit 0: some paths are ignored; 1: none are.
    let Ok(out) = out else { return false };
    if !out.status.success() {
        return false;
    }
    let ignored: std::collections::BTreeSet<String> =
        out.stdout.split(|&b| b == 0).filter(|p| !p.is_empty()).map(|p| String::from_utf8_lossy(p).into_owned()).collect();
    ignored == rels
}

/// Status of `repo`, limited to files under `scope` when the repo encloses it.
pub fn repo_changes(repo: &Path, scope: &Path) -> RepoChanges {
    repo_changes_with(repo, scope, repo_defines_commands(repo))
}

/// [`repo_changes`] with the answer of [`repo_defines_commands`] known (and
/// cached by the caller): when true, status is not run.
pub fn repo_changes_with(repo: &Path, scope: &Path, defines_commands: bool) -> RepoChanges {
    let mut rc = RepoChanges { root: repo.to_path_buf(), ..Default::default() };
    if defines_commands {
        rc.error = Some("skipped: repo config defines filter commands".into());
        return rc;
    }
    let pathspec = if scope.starts_with(repo) { scope } else { repo };
    let out = git(repo)
        .args(["status", "--porcelain=v1", "-z", "-b", "--untracked-files=all", "--ignore-submodules=all", "--"])
        .arg(pathspec)
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let (branch, files) = parse_status(&o.stdout, repo);
            rc.branch = branch;
            rc.files = files;
        },
        Ok(o) => rc.error = Some(String::from_utf8_lossy(&o.stderr).lines().next().unwrap_or("git status failed").to_owned()),
        Err(e) => rc.error = Some(format!("git: {e}")),
    }
    rc
}

/// Map of absolute path → status for the explorer. Parent directories of
/// changed files (below `root`) are marked `Modified` so collapsed folders
/// show a dot.
pub fn status_map<'a>(repos: impl IntoIterator<Item = &'a RepoChanges>, root: &Path) -> HashMap<PathBuf, GitStatus> {
    let mut map = HashMap::new();
    for (path, st) in repos.into_iter().flat_map(|r| &r.files) {
        let mut dir = path.parent();
        while let Some(d) = dir {
            if !d.starts_with(root) || d == root {
                break;
            }
            map.entry(d.to_path_buf()).or_insert(GitStatus::Modified);
            dir = d.parent();
        }
        map.insert(path.clone(), *st);
    }
    map
}

/// A `git` command hardened for untrusted repos: repo config can name an
/// fsmonitor hook program, which `status` would execute, and in a partial
/// clone a missing object triggers a lazy fetch that can run a repo-chosen
/// `core.sshCommand` / credential helper — so lazy fetch and every transport
/// are disabled. Also avoid taking the index lock so we never contend with git
/// commands Claude runs.
fn git(root: &Path) -> Command {
    let mut c = Command::new("git");
    c.args(["--no-optional-locks", "-c", "core.fsmonitor=false", "-c", "core.untrackedCache=false"])
        .args(["-c", "protocol.allow=never"])
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(root);
    #[cfg(windows)]
    {
        // A GUI app's console children would each flash a console window.
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

/// True if repo-scoped config (`local`/`worktree`, including files those
/// include) defines filter drivers, which `status`/`diff` would execute.
/// Global/system filters (e.g. git-lfs) are the user's own and stay allowed.
/// Fails closed: if the config can't be read, assume it does. Reading config
/// never executes anything.
pub fn repo_defines_commands(root: &Path) -> bool {
    let Ok(out) = git(root).args(["config", "--show-scope", "--get-regexp", r"^filter\..*\.(clean|smudge|process)$"]).output() else {
        return true;
    };
    // Exit 1 = no matching key; anything else non-zero is an error.
    if !out.status.success() {
        return out.status.code() != Some(1);
    }
    String::from_utf8_lossy(&out.stdout).lines().any(|l| {
        let scope = l.split('\t').next().unwrap_or("");
        !matches!(scope, "global" | "system")
    })
}

/// Parse `status --porcelain=v1 -z [-b]` output. Paths are joined to `toplevel`.
pub fn parse_status(raw: &[u8], toplevel: &Path) -> (Option<String>, Vec<(PathBuf, GitStatus)>) {
    let mut branch = None;
    let mut files = vec![];
    let mut parts = raw.split(|&b| b == 0).filter(|p| !p.is_empty());
    while let Some(rec) = parts.next() {
        if let Some(b) = rec.strip_prefix(b"## ") {
            let b = String::from_utf8_lossy(b);
            branch = Some(b.strip_prefix("No commits yet on ").unwrap_or(&b).to_owned());
            continue;
        }
        if rec.len() < 4 {
            continue;
        }
        let (x, y) = (rec[0], rec[1]);
        let path = toplevel.join(String::from_utf8_lossy(&rec[3..]).as_ref());
        let st = match (x, y) {
            (b'?', b'?') => GitStatus::Untracked,
            (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D') => GitStatus::Conflicted,
            (b'R', _) => {
                parts.next(); // rename source
                GitStatus::Renamed
            },
            (b'A', _) => GitStatus::Added,
            (b'D', _) | (_, b'D') => GitStatus::Deleted,
            _ => GitStatus::Modified,
        };
        files.push((path, st));
    }
    (branch, files)
}

/// Unified diff of `path` against HEAD in its own repository (or vs /dev/null
/// when untracked / not in a repo). External diff drivers and textconv are
/// disabled (repo config could name arbitrary programs).
pub fn diff_vs_head(path: &Path) -> Option<String> {
    let root = &repo_for(path).or_else(|| path.parent().map(Path::to_path_buf))?;
    if repo_defines_commands(root) {
        return None;
    }
    let base = ["diff", "--no-ext-diff", "--no-textconv", "--ignore-submodules=all", "--no-color", "-U3"];
    let out = git(root).args(base).arg("HEAD").arg("--").arg(path).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if !text.is_empty() {
        return Some(text);
    }
    // Untracked (or no HEAD yet): show the whole file as added. Relative to
    // the repo so the patch header names `dir/file`, not an absolute path.
    // With `/` separators: git takes a Windows `\` as part of the name.
    let rel = match path.strip_prefix(root) {
        Ok(rel) => rel.iter().map(|c| c.to_string_lossy()).collect::<Vec<_>>().join("/").into(),
        Err(_) => path.as_os_str().to_owned(),
    };
    let out = git(root).args(base).args(["--no-index", "--", "/dev/null"]).arg(rel).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_repo_with_local_filter_driver() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        let run = |args: &[&str]| assert!(Command::new("git").args(args).current_dir(r).output().unwrap().status.success());
        run(&["init", "-q"]);
        assert!(!repo_defines_commands(r));
        assert!(repo_changes(r, r).error.is_none());
        run(&["config", "filter.evil.clean", "touch pwned"]);
        assert!(repo_defines_commands(r));
        assert!(repo_changes(r, r).error.is_some());
        // Filters pulled in through an include outside the root are still repo-scoped.
        run(&["config", "--unset", "filter.evil.clean"]);
        let inc = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(inc.path(), "[filter \"x\"]\n\tsmudge = touch pwned\n").unwrap();
        run(&["config", "include.path", inc.path().to_str().unwrap()]);
        assert!(repo_defines_commands(r));
    }

    #[test]
    fn parses_status_and_marks_dirs() {
        let raw = b"## main...origin/main [ahead 1]\0 M src/a.rs\0?? new.txt\0R  b.rs\0old.rs\0A  src/x/y.rs\0";
        let root = Path::new("/r");
        let (branch, files) = parse_status(raw, root);
        assert_eq!(branch.as_deref(), Some("main...origin/main [ahead 1]"));
        assert_eq!(files.len(), 4);
        let rc = RepoChanges { root: root.into(), files, ..Default::default() };
        let m = status_map([&rc], root);
        assert_eq!(m[Path::new("/r/src/a.rs")], GitStatus::Modified);
        assert_eq!(m[Path::new("/r/new.txt")], GitStatus::Untracked);
        assert_eq!(m[Path::new("/r/b.rs")], GitStatus::Renamed);
        assert_eq!(m[Path::new("/r/src/x/y.rs")], GitStatus::Added);
        assert_eq!(m[Path::new("/r/src")], GitStatus::Modified);
        assert!(!m.contains_key(Path::new("/r/old.rs")));
    }

    #[test]
    fn finds_nested_repos_and_their_changes() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().canonicalize().unwrap();
        let git = |dir: &Path, args: &[&str]| assert!(Command::new("git").args(args).current_dir(dir).output().unwrap().status.success());
        for name in ["a", "group/b"] {
            let r = root.join(name);
            std::fs::create_dir_all(&r).unwrap();
            git(&r, &["init", "-q"]);
            std::fs::write(r.join("f.txt"), "x").unwrap();
        }
        std::fs::create_dir_all(root.join("node_modules/c/.git")).unwrap();
        let repos = find_repos(&root);
        assert_eq!(repos, vec![root.join("a"), root.join("group/b")]);
        let rc = repo_changes(&repos[1], &root);
        assert_eq!(rc.files, vec![(root.join("group/b/f.txt"), GitStatus::Untracked)]);
        assert_eq!(repo_for(&root.join("group/b/gone/x.rs")), Some(root.join("group/b")));
    }

    #[test]
    fn scopes_enclosing_repo_to_opened_subdir() {
        let d = tempfile::tempdir().unwrap();
        let top = d.path().canonicalize().unwrap();
        assert!(Command::new("git").args(["init", "-q"]).current_dir(&top).output().unwrap().status.success());
        std::fs::create_dir_all(top.join("sub")).unwrap();
        std::fs::write(top.join("sub/in.txt"), "x").unwrap();
        std::fs::write(top.join("out.txt"), "x").unwrap();
        let sub = top.join("sub");
        assert_eq!(find_repos(&sub), vec![top.clone()]);
        let rc = repo_changes(&top, &sub);
        assert_eq!(rc.files, vec![(sub.join("in.txt"), GitStatus::Untracked)]);
    }

    #[test]
    fn checks_ignored_paths_in_one_go() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().canonicalize().unwrap();
        let git = |args: &[&str]| assert!(Command::new("git").args(args).current_dir(&root).output().unwrap().status.success());
        git(&["init", "-q"]);
        std::fs::write(
            root.join(".gitignore"),
            "*.log
out/
",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("src/out")).unwrap();
        std::fs::write(
            root.join("src/.gitignore"),
            "gen.rs
",
        )
        .unwrap();
        let p = |s: &str| root.join(s);
        assert!(all_ignored(&root, &[p("a.log"), p("src/out/x.js"), p("src/gen.rs")]));
        assert!(all_ignored(&root, &[p("gone/b.log")]));
        assert!(!all_ignored(&root, &[p("a.log"), p("src/main.rs")]));
        // Tracked files count as changes even when a pattern matches them.
        std::fs::write(root.join("keep.log"), "x").unwrap();
        git(&["add", "-f", "keep.log"]);
        assert!(!all_ignored(&root, &[p("keep.log")]));
        // Outside the repo, or the root itself: not something to skip.
        assert!(!all_ignored(&root, std::slice::from_ref(&root)));
        assert!(!all_ignored(&root, &[p(":(top)x.log")]));
        assert!(!all_ignored(&root.join("src"), &[p("a.log")]));
    }

    #[test]
    fn owner_is_the_deepest_repo() {
        let repos = [PathBuf::from("/r"), PathBuf::from("/r/ext/a")];
        assert_eq!(owner(&repos, Path::new("/r/ext/a/f.rs")), Some(Path::new("/r/ext/a")));
        assert_eq!(owner(&repos, Path::new("/r/ext/ab/f.rs")), Some(Path::new("/r")));
        assert_eq!(owner(&repos, Path::new("/x/f.rs")), None);
    }

    #[test]
    fn untracked_diff_names_repo_relative_path() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().canonicalize().unwrap();
        assert!(Command::new("git").args(["init", "-q"]).current_dir(&root).output().unwrap().status.success());
        std::fs::create_dir_all(root.join("dir")).unwrap();
        std::fs::write(root.join("dir/new.txt"), "x\n").unwrap();
        let patch = diff_vs_head(&root.join("dir/new.txt")).unwrap();
        assert!(patch.contains("+++ b/dir/new.txt\n"), "{patch}");
        // A file named like an option must stay a path (`--output` would write a file).
        std::fs::write(root.join("--output=pwned"), "x\n").unwrap();
        let patch = diff_vs_head(&root.join("--output=pwned")).unwrap();
        assert!(patch.contains("+++ b/--output=pwned\n"), "{patch}");
        assert!(!root.join("pwned").exists());
    }
}
