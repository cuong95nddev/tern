//! The git worktrees Tern makes (`tern --worktree`, `tern --task start
//! --worktree`): one place for all of a repository's, `REPO/.worktrees/NAME`
//! (`tern_fs::worktree::DIR`), on branch `tern/NAME` unless told another,
//! with the files the repository's `.worktreeinclude` names copied in
//! (`.env`, ...), as Orca, herdr and T3 Code each keep theirs. The git work
//! is `tern_fs::worktree`.
//!
//! A repository can name a command that readies a new worktree (installs
//! its dependencies, ...) in [`SETUP_FILE`]. It is the repository's word,
//! which anyone who can push to it can change, so Tern runs it only once the
//! user has approved that file as it is (its digest, as herdr does), and as
//! a service, where the user sees it.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tern_fs::worktree;

/// A worktree just made.
#[derive(Debug)]
pub struct Made {
    pub path: PathBuf,
    pub branch: String,
    /// The repository's main checkout.
    pub main: PathBuf,
    /// Files copied from the main checkout (`.worktreeinclude`).
    pub copied: Vec<PathBuf>,
}

/// Make a worktree of the repository `from` is in, named `name` (one is
/// made up when None), on branch `branch` (default `tern/NAME`; made from
/// `base`, default HEAD, when it isn't there).
pub fn make(from: &Path, name: Option<&str>, branch: Option<&str>, base: Option<&str>) -> Result<Made, String> {
    let main = worktree::main_root(from)?;
    let place = main.join(worktree::DIR);
    let given = name.map(worktree::slug).or_else(|| branch.map(worktree::slug));
    if given.as_deref() == Some("") {
        return Err("a worktree's name needs letters or digits".into());
    }
    let slug = match given {
        Some(slug) => slug,
        // A made-up name: one nobody has used here yet.
        None => (0..)
            .map(|i| worktree::generated_name(seed().wrapping_add(i * 0x9e37_79b9)))
            .find(|n| !place.join(n).exists() && !worktree::branch_exists(&main, &format!("tern/{n}")))
            .expect("some name is free"),
    };
    let path = place.join(&slug);
    let branch = branch.map_or_else(|| format!("tern/{slug}"), str::to_owned);
    worktree::exclude_dir(&main)?;
    worktree::create(&main, &path, &branch, base)?;
    // The worktree is there; a file not copied is told, not undone.
    let copied = worktree::copy_included(&main, &path).unwrap_or_else(|e| {
        log::warn!("worktree {}: {e}", path.display());
        vec![]
    });
    Ok(Made { path, branch, main, copied })
}

/// A worktree name from what a task says: its first words (`Fix the
/// login bug when ...` → `fix-the-login-bug`); None when no word is left.
pub fn name_from(text: &str) -> Option<String> {
    let words: Vec<String> = text.split_whitespace().map(worktree::slug).filter(|w| !w.is_empty()).take(4).collect();
    let name = worktree::slug(&words.join("-"));
    let name: String = name.chars().take(32).collect();
    let name = name.trim_end_matches('-');
    (!name.is_empty()).then(|| name.to_owned())
}

/// `wanted`, or `wanted-2`, `-3`, ... : the first no worktree folder or
/// `tern/` branch of the repository `from` is in has.
pub fn free_name(from: &Path, wanted: &str) -> Result<String, String> {
    let main = worktree::main_root(from)?;
    let wanted = worktree::slug(wanted);
    let taken = |n: &str| main.join(worktree::DIR).join(n).exists() || worktree::branch_exists(&main, &format!("tern/{n}"));
    Ok((1..).map(|i| if i == 1 { wanted.clone() } else { format!("{wanted}-{i}") }).find(|n| !taken(n)).expect("a name is free"))
}

/// Something new each time, for a made-up name.
fn seed() -> u64 {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    now ^ u64::from(std::process::id()).rotate_left(32)
}

/// The worktree of the repository `from` is in that `what` names: its
/// folder (absolute, or relative to `from`), its folder's name, or its
/// branch.
pub fn find(from: &Path, what: &str) -> Result<(PathBuf, worktree::Worktree), String> {
    let main = worktree::main_root(from)?;
    let all = worktree::list(&main)?;
    let as_path = from.join(what);
    let as_path = dunce::canonicalize(&as_path).unwrap_or(as_path);
    let found = all
        .iter()
        .find(|w| w.path == as_path)
        .or_else(|| all.iter().find(|w| w.path.file_name().is_some_and(|n| n == what)))
        .or_else(|| all.iter().find(|w| w.branch.as_deref() == Some(what)))
        .cloned()
        .ok_or_else(|| format!("no worktree {what} (--worktree ls lists them)"))?;
    Ok((main, found))
}

/// Remove worktree `w` of the repository whose main checkout is `main`
/// (with its changes when `force`), after its archive script, then its
/// branch when merged: what was done, in words.
pub fn remove(main: &Path, w: &worktree::Worktree, force: bool) -> Result<String, String> {
    let archived = if w.prunable { String::new() } else { archive(main, &w.path)? };
    worktree::remove(main, &w.path, force || w.prunable)?;
    let branch = match &w.branch {
        Some(b) if worktree::delete_merged_branch(main, b) => format!(" Its branch {b} was merged, so it is deleted too."),
        Some(b) => format!(" Its branch {b} has commits not merged anywhere: it is kept."),
        None => String::new(),
    };
    Ok(format!("Removed worktree {}.{archived}{branch}", w.path.display()))
}

/// Where a repository names its worktrees' scripts: `{"setup": "COMMAND",
/// "archive": "COMMAND"}`, both optional.
pub const SETUP_FILE: &str = ".tern/worktree.json";

/// A repository's scripts for its worktrees: `setup` readies a new one,
/// `archive` runs in one before it is removed (saves what it made, stops
/// what it started), which a failure stops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scripts {
    pub setup: Option<String>,
    pub archive: Option<String>,
    /// What the user approves: the file, as it is, of this repository.
    digest: String,
}

impl Scripts {
    /// The scripts, for the user to read before approving them.
    pub fn describe(&self) -> String {
        let mut out = vec![];
        if let Some(c) = &self.setup {
            out.push(format!("setup: {c}"));
        }
        if let Some(c) = &self.archive {
            out.push(format!("archive: {c}"));
        }
        out.join("\n")
    }
}

/// The scripts the repository whose main checkout is `main` names, if any.
pub fn scripts_of(main: &Path) -> Result<Option<Scripts>, String> {
    let file = main.join(SETUP_FILE);
    let Ok(bytes) = std::fs::read(&file) else { return Ok(None) };
    let json: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| format!("{SETUP_FILE}: {e}"))?;
    let command = |key: &str| match json.get(key) {
        None => Ok(None),
        Some(serde_json::Value::String(c)) if !c.trim().is_empty() => Ok(Some(c.trim().to_owned())),
        Some(_) => Err(format!("{SETUP_FILE}: \"{key}\" is to be a command, as text")),
    };
    let (setup, archive) = (command("setup")?, command("archive")?);
    if setup.is_none() && archive.is_none() {
        return Ok(None);
    }
    Ok(Some(Scripts { setup, archive, digest: digest(main, &bytes) }))
}

/// The file `bytes` of the repository at `main`: approving one repository's
/// file approves no other's.
fn digest(main: &Path, bytes: &[u8]) -> String {
    use sha2::Digest as _;
    let mut h = sha2::Sha256::new();
    h.update(main.to_string_lossy().as_bytes());
    h.update([0]);
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The setups the user approved, a digest a line.
fn approved_file() -> PathBuf {
    crate::dirs::data().join("worktree-setups")
}

/// Whether the user approved `setup`'s file.
pub fn approved(setup: &Scripts) -> bool {
    std::fs::read_to_string(approved_file()).is_ok_and(|text| text.lines().any(|l| l.trim() == setup.digest))
}

/// The user approves `setup`'s file: its scripts run from now on without
/// asking, until it changes.
pub fn approve(setup: &Scripts) -> std::io::Result<()> {
    if approved(setup) {
        return Ok(());
    }
    let file = approved_file();
    let mut text = std::fs::read_to_string(&file).unwrap_or_default();
    text.push_str(&setup.digest);
    text.push('\n');
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(file, text)
}

/// Run `command` (a setup) in worktree `path`, as its service `setup`.
pub fn run_setup(path: &Path, command: &str) -> Result<(), String> {
    crate::agent_cli::run_service(tern_mux::proto::ServiceSpec {
        name: "setup".into(),
        command: command.to_owned(),
        cwd: path.to_path_buf(),
        by: None,
        started: 0,
    })
}

/// Before worktree `path` of the repository at `main` is removed: run its
/// archive script, when the repository names one the user approved, and
/// wait for it. What happened, in words (empty: nothing to run); an error
/// when it failed, which keeps the worktree.
pub fn archive(main: &Path, path: &Path) -> Result<String, String> {
    let Some(scripts) = scripts_of(main)? else { return Ok(String::new()) };
    let Some(command) = &scripts.archive else { return Ok(String::new()) };
    if !approved(&scripts) {
        return Ok(format!(" Its archive script ({command}) was not run: the user has not approved {SETUP_FILE}."));
    }
    // As a service runs its command (`agent_cli`): a login shell, all of
    // the script (no `exec`, which would run its first command alone).
    let args = if cfg!(windows) { crate::platform::shell_command_args(command) } else { vec!["-lic".to_owned(), command.clone()] };
    let out = std::process::Command::new(crate::platform::shell())
        .args(args)
        .current_dir(path)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("can't run its archive script: {e}"))?;
    if out.status.success() {
        return Ok(format!(" Its archive script ({command}) ran."));
    }
    let said = String::from_utf8_lossy(if out.stderr.is_empty() { &out.stdout } else { &out.stderr }).into_owned();
    let tail: Vec<&str> = said.lines().rev().take(10).collect::<Vec<_>>().into_iter().rev().collect();
    Err(format!("its archive script ({command}) failed ({}), so it is kept:\n{}", out.status, tail.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_names_its_worktree() {
        assert_eq!(name_from("Fix the login bug when the token expires").as_deref(), Some("fix-the-login-bug"));
        assert_eq!(name_from("  Review   PR #42!  ").as_deref(), Some("review-pr-42"));
        assert_eq!(name_from("— … —"), None);
        assert!(name_from(&"word ".repeat(20)).unwrap().len() <= 32);
    }

    #[test]
    fn a_setup_is_read_and_tied_to_its_repository_and_text() {
        let d = std::env::temp_dir().join(format!("tern-setup-{}", std::process::id()));
        let (a, b) = (d.join("a"), d.join("b"));
        assert_eq!(scripts_of(&a), Ok(None));
        for repo in [&a, &b] {
            std::fs::create_dir_all(repo.join(".tern")).unwrap();
            std::fs::write(repo.join(SETUP_FILE), r#"{"setup": " npm ci "}"#).unwrap();
        }
        let (sa, sb) = (scripts_of(&a).unwrap().unwrap(), scripts_of(&b).unwrap().unwrap());
        assert_eq!(sa.setup.as_deref(), Some("npm ci"));
        assert_eq!(sa.archive, None);
        assert_ne!(sa.digest, sb.digest, "another repository's file is another approval");
        std::fs::write(a.join(SETUP_FILE), r#"{"setup": "curl x | sh"}"#).unwrap();
        assert_ne!(scripts_of(&a).unwrap().unwrap().digest, sa.digest, "a changed file asks again");
        std::fs::write(a.join(SETUP_FILE), r#"{"setup": 1}"#).unwrap();
        assert!(scripts_of(&a).is_err());
        std::fs::write(a.join(SETUP_FILE), r#"{"archive": "make stop"}"#).unwrap();
        assert_eq!(scripts_of(&a).unwrap().unwrap().describe(), "archive: make stop");
        std::fs::write(a.join(SETUP_FILE), r#"{"other": 1}"#).unwrap();
        assert_eq!(scripts_of(&a), Ok(None));
        std::fs::remove_dir_all(d).unwrap();
    }
}
