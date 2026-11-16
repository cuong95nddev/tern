//! Git worktrees: the checkouts of one repository beside its main one, as
//! `git worktree` keeps them. Tern makes, lists and removes them for the
//! user and for agents (`tern --worktree`), so every agent puts its
//! worktrees in one place, named one way, instead of each its own.
//!
//! Nothing here keeps a list of its own: git's (`git worktree list`) is the
//! list, read again whenever it is needed.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The folder of a repository's main checkout that holds the worktrees Tern
/// makes, `REPO/.worktrees/NAME`: inside the repository, an agent's sandbox
/// and the folder trust of Claude Code and the like cover them as they
/// cover it. Git ignores it there by the repository's `info/exclude`
/// ([`exclude_dir`]), which is no file of the repository's own.
pub const DIR: &str = ".worktrees";

/// Have git ignore [`DIR`] in the repository at `main`: a line in its
/// `info/exclude`, unless one is there.
pub fn exclude_dir(main: &Path) -> Result<(), String> {
    let (_, common) = git_dirs(main).ok_or_else(|| format!("{} is no git checkout", main.display()))?;
    let file = common.join("info").join("exclude");
    let line = format!("/{DIR}/");
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    if text.lines().any(|l| l.trim() == line || l.trim() == format!("/{DIR}") || l.trim() == format!("{DIR}/")) {
        return Ok(());
    }
    let sep = if text.is_empty() || text.ends_with('\n') { "" } else { "\n" };
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("can't make {}: {e}", dir.display()))?;
    }
    std::fs::write(&file, format!("{text}{sep}# Tern's git worktrees\n{line}\n"))
        .map_err(|e| format!("can't write {}: {e}", file.display()))
}

/// One checkout of a repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// Its folder.
    pub path: PathBuf,
    /// The branch it has checked out (`main`, no `refs/heads/`); None when
    /// its HEAD is detached.
    pub branch: Option<String>,
    /// The commit its HEAD is at.
    pub head: String,
    /// The repository's main checkout (the one holding `.git`), which can't
    /// be removed.
    pub main: bool,
    /// Its folder is gone: `git worktree prune` would forget it.
    pub prunable: bool,
    /// Locked against removal (`git worktree lock`).
    pub locked: bool,
}

/// The git dirs of the checkout at `root`: its own (HEAD, index) and the
/// repository's common one (refs, objects, `info/`). The same dir twice for
/// a main checkout; None when `root` holds no `.git`.
pub fn git_dirs(root: &Path) -> Option<(PathBuf, PathBuf)> {
    let dot_git = root.join(".git");
    if dot_git.is_dir() {
        return Some((dot_git.clone(), dot_git));
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let own = root.join(text.strip_prefix("gitdir:")?.trim());
    let common = match std::fs::read_to_string(own.join("commondir")) {
        Ok(common) => own.join(common.trim()),
        Err(_) => own.clone(),
    };
    Some((normalize(&own), normalize(&common)))
}

/// `p` without its `.` and `..` parts (the common dir is often written
/// `../..`), without touching the disk.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {},
            std::path::Component::ParentDir => {
                out.pop();
            },
            c => out.push(c),
        }
    }
    out
}

/// When `root` is a linked worktree (not a repository's main checkout): the
/// main checkout's folder, and the branch it has checked out (None when
/// detached).
///
/// Only when the repository says so too: a `.git` file is the folder's own
/// word, which any folder (a cloned one) can put there to pass for a
/// worktree of a repository it is not, and so be trusted as that one is.
/// The worktree's git dir must be one of the repository's (`worktrees/X` in
/// its common dir), and name this very `.git` as its worktree's (`gitdir`).
pub fn linked(root: &Path) -> Option<(PathBuf, Option<String>)> {
    let (own, common) = git_dirs(root)?;
    if own == common {
        return None;
    }
    let real = |p: &Path| std::fs::canonicalize(p).ok();
    let (own_real, common_real) = (real(&own)?, real(&common)?);
    if own_real.parent()? != common_real.join("worktrees") {
        return None;
    }
    let back = std::fs::read_to_string(own.join("gitdir")).ok()?;
    let back = own.join(back.trim());
    if real(&back)? != real(&root.join(".git"))? {
        return None;
    }
    // A bare repository's common dir is no checkout's `.git`.
    let main = (common.file_name()? == ".git").then(|| common.parent().map(Path::to_path_buf))??;
    let head = std::fs::read_to_string(own.join("HEAD")).ok()?;
    Some((main, head.trim().strip_prefix("ref: refs/heads/").map(str::to_owned)))
}

/// The branch checkout `root` has checked out (None when detached or not
/// a checkout), read from its HEAD.
pub fn branch_of(root: &Path) -> Option<String> {
    let (own, _) = git_dirs(root)?;
    let head = std::fs::read_to_string(own.join("HEAD")).ok()?;
    head.trim().strip_prefix("ref: refs/heads/").map(str::to_owned)
}

/// Git, run in `dir` for a command the user (or an agent for them) asked
/// for: no prompt for a password ever waits on a terminal nobody sees.
fn git(dir: &Path) -> Command {
    let mut c = Command::new("git");
    c.current_dir(dir).env("GIT_TERMINAL_PROMPT", "0");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

/// Run `cmd`: its output, or what git said went wrong.
fn run(mut cmd: Command) -> Result<String, String> {
    let out = cmd.output().map_err(|e| format!("can't run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        Err(if err.is_empty() { format!("git failed ({})", out.status) } else { err.to_owned() })
    }
}

/// The main checkout's folder of the repository `path` is in (it may be in
/// a linked worktree of it).
pub fn main_root(path: &Path) -> Result<PathBuf, String> {
    let top = run({
        let mut c = git(path);
        c.args(["rev-parse", "--show-toplevel"]);
        c
    })
    .map_err(|_| format!("{} is not in a git repository", path.display()))?;
    let top = PathBuf::from(top.trim());
    Ok(linked(&top).map_or(top, |(main, _)| main))
}

/// Every checkout of the repository whose main checkout is `repo`, the main
/// one first.
pub fn list(repo: &Path) -> Result<Vec<Worktree>, String> {
    let out = run({
        let mut c = git(repo);
        c.args(["worktree", "list", "--porcelain"]);
        c
    })?;
    Ok(parse_list(&out))
}

/// `git worktree list --porcelain`: blocks of `key value` lines, one block
/// a checkout, the main one first.
fn parse_list(text: &str) -> Vec<Worktree> {
    let mut out = vec![];
    for (i, block) in text.split("\n\n").enumerate() {
        let mut wt = Worktree { path: PathBuf::new(), branch: None, head: String::new(), main: i == 0, prunable: false, locked: false };
        let mut bare = false;
        for line in block.lines() {
            let (key, value) = line.split_once(' ').unwrap_or((line, ""));
            match key {
                "worktree" => wt.path = PathBuf::from(value),
                "HEAD" => wt.head = value.to_owned(),
                "branch" => wt.branch = Some(value.strip_prefix("refs/heads/").unwrap_or(value).to_owned()),
                "bare" => bare = true,
                "prunable" => wt.prunable = true,
                "locked" => wt.locked = true,
                _ => {},
            }
        }
        // A bare repository's own block is no checkout.
        if !wt.path.as_os_str().is_empty() && !bare {
            out.push(wt);
        }
    }
    out
}

/// Whether branch `name` is in the repository at `repo`.
pub fn branch_exists(repo: &Path, name: &str) -> bool {
    git(repo).args(["show-ref", "--verify", "--quiet", &format!("refs/heads/{name}")]).status().is_ok_and(|s| s.success())
}

/// Make a worktree of the repository at `repo` in `dir` (which must not be
/// there yet), on branch `branch`: that branch when it is there, else a new
/// one made from `base` (HEAD when None).
pub fn create(repo: &Path, dir: &Path, branch: &str, base: Option<&str>) -> Result<(), String> {
    if dir.exists() {
        return Err(format!("{} is there already", dir.display()));
    }
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("can't make {}: {e}", parent.display()))?;
    }
    let mut c = git(repo);
    c.args(["worktree", "add"]);
    if branch_exists(repo, branch) {
        if base.is_some() {
            return Err(format!("branch {branch} is there already: --base only makes a new one"));
        }
        c.arg(dir).arg(branch);
    } else {
        c.args(["-b", branch]).arg(dir).arg(base.unwrap_or("HEAD"));
    }
    run(c).map(|_| ())
}

/// The changes in checkout `path` that removing it would lose: `git status`
/// lines (untracked files too), empty when there are none.
pub fn changes(path: &Path) -> Result<Vec<String>, String> {
    let out = run({
        let mut c = git(path);
        c.args(["status", "--porcelain"]);
        c
    })?;
    Ok(out.lines().filter(|l| !l.is_empty()).map(str::to_owned).collect())
}

/// Remove checkout `path` of the repository at `repo`; `force` also when it
/// has changes, which are lost.
pub fn remove(repo: &Path, path: &Path, force: bool) -> Result<(), String> {
    let mut c = git(repo);
    c.args(["worktree", "remove"]);
    if force {
        c.arg("--force");
    }
    c.arg(path);
    run(c).map(|_| ())
}

/// Delete branch `name` when it is merged (`git branch -d`): whether it
/// was. One with commits nowhere else is kept, never lost.
pub fn delete_merged_branch(repo: &Path, name: &str) -> bool {
    git(repo).args(["branch", "-d", name]).output().is_ok_and(|o| o.status.success())
}

/// Whether branch `name` was worked on: its reflog holds more than its
/// making (a commit, a reset). A branch just made is "merged" into the
/// commit it was made from, but there is nothing of it to keep or lose.
pub fn worked_on(repo: &Path, name: &str) -> bool {
    run({
        let mut c = git(repo);
        c.args(["reflog", "show", "--format=%H", &format!("refs/heads/{name}"), "--"]);
        c
    })
    .is_ok_and(|out| out.lines().count() > 1)
}

/// Whether every commit of branch `name` is in what the main checkout at
/// `repo` has checked out (merged into it, not squashed).
pub fn merged(repo: &Path, name: &str) -> bool {
    git(repo).args(["merge-base", "--is-ancestor", &format!("refs/heads/{name}"), "HEAD"]).status().is_ok_and(|s| s.success())
}

/// Forget the worktrees whose folders are gone (`git worktree prune`).
pub fn prune(repo: &Path) -> Result<(), String> {
    run({
        let mut c = git(repo);
        c.args(["worktree", "prune"]);
        c
    })
    .map(|_| ())
}

/// Copy into worktree `to` the files of checkout `from` that git doesn't
/// track and `from`'s `.worktreeinclude` names (gitignore patterns: `.env`,
/// `config/local.json`, ...): what a fresh checkout lacks to run. The files
/// copied, relative to `from`.
pub fn copy_included(from: &Path, to: &Path) -> Result<Vec<PathBuf>, String> {
    let list = from.join(".worktreeinclude");
    if !list.is_file() {
        return Ok(vec![]);
    }
    let out = run({
        let mut c = git(from);
        c.args(["ls-files", "-z", "--others", "--ignored", "--exclude-from"]).arg(&list);
        c
    })?;
    let mut copied = vec![];
    for rel in out.split('\0').filter(|r| !r.is_empty()) {
        let rel = PathBuf::from(rel);
        let (src, dst) = (from.join(&rel), to.join(&rel));
        // A file the checkout has (tracked there) is the branch's own.
        if dst.exists() || !src.is_file() {
            continue;
        }
        if let Some(dir) = dst.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("can't make {}: {e}", dir.display()))?;
        }
        std::fs::copy(&src, &dst).map_err(|e| format!("can't copy {}: {e}", rel.display()))?;
        copied.push(rel);
    }
    Ok(copied)
}

/// `name` as a folder and branch name: lower case letters, digits and
/// single `-`s (`Fix the login!` → `fix-the-login`); empty when nothing
/// of it is left.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    out.chars().take(48).collect::<String>().trim_end_matches('-').to_owned()
}

/// A name for a worktree nobody named: two words and four hex digits
/// (`calm-otter-3f2a`), from `seed`.
pub fn generated_name(seed: u64) -> String {
    const FIRST: [&str; 16] = [
        "amber", "brave", "calm", "deep", "eager", "fair", "gentle", "keen", "lively", "mellow", "nimble", "proud", "quiet", "rapid",
        "swift", "warm",
    ];
    const SECOND: [&str; 16] = [
        "badger", "crane", "dolphin", "falcon", "gecko", "heron", "ibis", "lynx", "marten", "newt", "otter", "panda", "raven", "salmon",
        "tern", "wren",
    ];
    let a = FIRST[(seed & 15) as usize];
    let b = SECOND[((seed >> 4) & 15) as usize];
    format!("{a}-{b}-{:04x}", (seed >> 8) & 0xffff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn git_ok(dir: &Path, args: &[&str]) {
        let out = Command::new("git").current_dir(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// A repository with one commit on `main`.
    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        git_ok(r, &["init", "-q", "-b", "main"]);
        git_ok(r, &["config", "user.email", "t@example.com"]);
        git_ok(r, &["config", "user.name", "T"]);
        git_ok(r, &["config", "commit.gpgsign", "false"]);
        fs::write(r.join("a.txt"), "a").unwrap();
        fs::write(r.join(".gitignore"), ".env\nbig/\n").unwrap();
        git_ok(r, &["add", "."]);
        git_ok(r, &["commit", "-q", "-m", "one"]);
        d
    }

    #[test]
    fn names_become_slugs() {
        assert_eq!(slug("Fix the login!"), "fix-the-login");
        assert_eq!(slug("  --x__y--  "), "x-y");
        assert_eq!(slug("ü"), "");
        assert_eq!(slug(&"a".repeat(80)).len(), 48);
        assert_eq!(generated_name(0), "amber-badger-0000");
        assert_eq!(generated_name(0x12_345f), "warm-heron-1234");
    }

    #[test]
    fn reads_gits_list() {
        let text = "worktree /r\nHEAD abc\nbranch refs/heads/main\n\nworktree /w/x\nHEAD def\ndetached\nlocked\n\nworktree /w/y\nHEAD 123\nbranch refs/heads/feat/y\nprunable gitdir file points to non-existent location\n";
        let list = parse_list(text);
        assert_eq!(list.len(), 3);
        assert!(list[0].main && list[0].branch.as_deref() == Some("main"));
        assert!(!list[1].main && list[1].branch.is_none() && list[1].locked);
        assert!(list[2].prunable && list[2].branch.as_deref() == Some("feat/y"));
    }

    #[test]
    fn makes_lists_and_removes_a_worktree() {
        let d = repo();
        let r = dunce::canonicalize(d.path()).unwrap();
        fs::write(r.join(".env"), "KEY=1").unwrap();
        fs::write(r.join(".worktreeinclude"), ".env\n").unwrap();
        let place = tempfile::tempdir().unwrap();
        let wt = dunce::canonicalize(place.path()).unwrap().join("feat");
        create(&r, &wt, "tern/feat", None).unwrap();
        assert!(create(&r, &wt, "tern/feat", None).unwrap_err().contains("there already"));

        // The worktree knows its main checkout and its branch.
        assert_eq!(linked(&wt), Some((r.clone(), Some("tern/feat".to_owned()))));
        // A folder whose `.git` file only claims to be one of its worktrees
        // is not: the repository names another.
        let fake = place.path().join("fake");
        fs::create_dir_all(&fake).unwrap();
        fs::write(fake.join(".git"), fs::read_to_string(wt.join(".git")).unwrap()).unwrap();
        assert_eq!(linked(&fake), None);
        // Nor one pointing at a made-up git dir naming the repository's.
        let made_up = place.path().join("gitdir");
        fs::create_dir_all(&made_up).unwrap();
        fs::write(made_up.join("commondir"), r.join(".git").to_string_lossy().as_bytes()).unwrap();
        fs::write(made_up.join("gitdir"), fake.join(".git").to_string_lossy().as_bytes()).unwrap();
        fs::write(made_up.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(fake.join(".git"), format!("gitdir: {}\n", made_up.display())).unwrap();
        assert_eq!(linked(&fake), None);
        assert_eq!(linked(&r), None);
        assert_eq!(branch_of(&r).as_deref(), Some("main"));
        assert_eq!(branch_of(&wt).as_deref(), Some("tern/feat"));
        assert_eq!(main_root(&wt).unwrap(), r);
        let (own, common) = git_dirs(&wt).unwrap();
        assert_eq!(common, r.join(".git"));
        assert!(own.starts_with(r.join(".git/worktrees")), "{}", own.display());

        let list = list(&r).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[1].branch.as_deref(), Some("tern/feat"));

        assert_eq!(copy_included(&r, &wt).unwrap(), vec![PathBuf::from(".env")]);

        // Tern's worktrees folder is ignored, once.
        exclude_dir(&r).unwrap();
        exclude_dir(&r).unwrap();
        let exclude = fs::read_to_string(r.join(".git/info/exclude")).unwrap();
        assert_eq!(exclude.matches("/.worktrees/").count(), 1, "{exclude}");
        fs::create_dir_all(r.join(".worktrees/x")).unwrap();
        fs::write(r.join(".worktrees/x/f"), "").unwrap();
        assert!(changes(&r).unwrap().iter().all(|l| !l.contains(".worktrees")));
        assert_eq!(fs::read_to_string(wt.join(".env")).unwrap(), "KEY=1");

        // Changes stop a plain removal.
        fs::write(wt.join("new.txt"), "x").unwrap();
        assert_eq!(changes(&wt).unwrap(), vec!["?? new.txt".to_owned()]);
        assert!(remove(&r, &wt, false).is_err());
        fs::remove_file(wt.join("new.txt")).unwrap();
        remove(&r, &wt, false).unwrap();
        assert!(!wt.exists());
        // Its branch has nothing main lacks: merged, so it goes.
        assert!(merged(&r, "tern/feat") && !worked_on(&r, "tern/feat"));
        assert!(delete_merged_branch(&r, "tern/feat"));
        assert!(!branch_exists(&r, "tern/feat"));
    }

    #[test]
    fn keeps_a_branch_with_work_of_its_own() {
        let d = repo();
        let r = d.path();
        let place = tempfile::tempdir().unwrap();
        let wt = place.path().join("w");
        create(r, &wt, "work", None).unwrap();
        fs::write(wt.join("b.txt"), "b").unwrap();
        git_ok(&wt, &["add", "."]);
        git_ok(&wt, &["commit", "-q", "-m", "two"]);
        assert!(worked_on(r, "work") && !merged(r, "work"));
        remove(r, &wt, false).unwrap();
        assert!(!delete_merged_branch(r, "work"));
        assert!(branch_exists(r, "work"));
        // Made again on the branch that is there.
        create(r, &wt, "work", None).unwrap();
        assert!(wt.join("b.txt").exists());
        assert!(create(r, &place.path().join("v"), "work2", Some("nope")).is_err());
    }
}
