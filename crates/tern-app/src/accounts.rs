//! Claude accounts a Claude tab can run as: Claude Code's own sign-in
//! (`~/.claude`, or the `CLAUDE_CONFIG_DIR` Tern was started with) and more,
//! each a config folder of its own that `claude` runs with
//! (`CLAUDE_CONFIG_DIR`) and signs in to with its own `/login`. Tern never
//! copies or swaps sign-ins: each account's Claude Code keeps its own, in a
//! keychain item named after its folder.
//!
//! An added account's folder links to the default one's settings,
//! instructions, skills, plugins and conversations, so any account sees
//! the same setup and can resume any conversation.
//!
//! The active accounts are those a new Claude tab may run as: with one, a
//! new tab runs as it; with several, the user picks.

use std::io;
use std::path::{Path, PathBuf};

use gpui_kit::*;
use serde::{Deserialize, Serialize};

/// Claude Code's own sign-in.
pub const DEFAULT: &str = "default";

/// What every account shares with the default one: its setup.
const SHARED: &[&str] =
    &["settings.json", "keybindings.json", "CLAUDE.md", "skills", "commands", "agents", "rules", "plugins", "hooks", "themes"];

/// Shared too, so any account can resume any conversation; made in the
/// default folder when missing, so an account never starts a history of
/// its own. `history.jsonl` is a file, the others folders.
const HISTORY: &[&str] = &["projects", "history.jsonl", "file-history", "plans", "todos"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Account {
    /// Stable: names the account's folder.
    pub id: String,
    /// What the user calls it; empty (the default account's until
    /// renamed): its email.
    pub name: String,
    /// A new Claude tab may run as it.
    #[serde(default = "yes")]
    pub active: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Saved {
    accounts: Vec<Account>,
    /// The number the next added account's id gets: ids are never used
    /// again, so a new account never meets a removed one's sign-in.
    #[serde(default)]
    next: u32,
}

/// The app's one account list.
pub struct Accounts(pub Entity<AccountStore>);

impl Global for Accounts {}

pub struct AccountStore {
    /// The default account first.
    pub list: Vec<Account>,
    next: u32,
}

/// The account list, made on first use. Its accounts' folders are brought
/// up to date in the background, and those of removed accounts a `claude`
/// still ran in are cleared away once none does.
pub fn store(cx: &mut App) -> Entity<AccountStore> {
    if let Some(a) = cx.try_global::<Accounts>() {
        return a.0.clone();
    }
    let saved = load();
    // Only a list read whole says which folders are removed accounts'.
    let read = saved.is_some();
    let saved = saved.unwrap_or_default();
    let list = normalized(saved.accounts);
    let ids: Vec<String> = list.iter().map(|a| a.id.clone()).collect();
    cx.background_executor()
        .spawn(async move {
            for id in ids.iter().filter(|id| *id != DEFAULT) {
                if let Err(e) = prepare(id) {
                    log::warn!("accounts: prepare {id}: {e}");
                }
            }
            if read {
                clear_removed(&ids);
            }
        })
        .detach();
    let store = cx.new(|_| AccountStore { list, next: saved.next });
    cx.set_global(Accounts(store.clone()));
    store
}

fn file() -> PathBuf {
    crate::dirs::data().join("claude-accounts.json")
}

fn load() -> Option<Saved> {
    serde_json::from_slice::<Saved>(&std::fs::read(file()).ok()?).ok()
}

/// The default account first, once; at least one active.
fn normalized(mut list: Vec<Account>) -> Vec<Account> {
    let default = list.iter().position(|a| a.id == DEFAULT).map(|i| list.remove(i)).map(|mut a| {
        // Its name before it went by its email.
        if a.name == "Default" {
            a.name.clear();
        }
        a
    });
    list.retain(|a| a.id != DEFAULT && !a.id.is_empty());
    list.insert(0, default.unwrap_or_else(|| Account { id: DEFAULT.into(), name: String::new(), active: true }));
    if !list.iter().any(|a| a.active) {
        list[0].active = true;
    }
    list
}

impl AccountStore {
    pub fn get(&self, id: &str) -> Option<&Account> {
        self.list.iter().find(|a| a.id == id)
    }

    pub fn active(&self) -> Vec<&Account> {
        self.list.iter().filter(|a| a.active).collect()
    }

    /// `id`'s name, or the default account's for one no longer listed; its
    /// email when it has none.
    pub fn name(&self, id: &str) -> String {
        let Some(a) = self.get(id).or_else(|| self.get(DEFAULT)) else { return String::new() };
        if a.name.is_empty() { email(&a.id).unwrap_or_else(|| "Claude Code".into()) } else { a.name.clone() }
    }

    /// Adds an active account named `name`, its folder made; its id.
    pub fn add(&mut self, name: &str, cx: &mut Context<Self>) -> io::Result<String> {
        let n = (self.next.max(2)..).find(|n| self.get(&format!("account-{n}")).is_none() && !folder(&format!("account-{n}")).exists());
        let n = n.ok_or_else(|| io::Error::other("no account id left"))?;
        let id = format!("account-{n}");
        prepare(&id)?;
        self.next = n + 1;
        let name = if name.trim().is_empty() { format!("Account {n}") } else { name.trim().to_owned() };
        self.list.push(Account { id: id.clone(), name, active: true });
        self.changed(cx);
        Ok(id)
    }

    /// Forgets `id`: its folder goes to the Trash (its conversations are
    /// the default account's: they stay), its sign-in and its usage go
    /// too. While a `claude` still runs in it, all stay so that one goes on
    /// working; the next start clears them. Not the default account.
    pub fn remove(&mut self, id: &str, cx: &mut Context<Self>) {
        if id == DEFAULT {
            return;
        }
        self.list.retain(|a| a.id != id);
        if !in_use(&folder(id)) {
            let id = id.to_owned();
            crate::usage::store(cx).update(cx, |u, _| u.forget(&id));
            cx.background_executor().spawn(async move { clear(&id) }).detach();
        }
        self.list = normalized(std::mem::take(&mut self.list));
        self.changed(cx);
    }

    pub fn rename(&mut self, id: &str, name: &str, cx: &mut Context<Self>) {
        let name = name.trim();
        if let Some(a) = self.list.iter_mut().find(|a| a.id == id)
            && !name.is_empty()
        {
            a.name = name.to_owned();
            self.changed(cx);
        }
    }

    /// Turns `id` on or off for new tabs; the last active one stays on.
    pub fn set_active(&mut self, id: &str, on: bool, cx: &mut Context<Self>) {
        if !on && self.active().iter().all(|a| a.id == id) {
            return;
        }
        if let Some(a) = self.list.iter_mut().find(|a| a.id == id) {
            a.active = on;
            self.changed(cx);
        }
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        let path = file();
        let saved = Saved { accounts: self.list.clone(), next: self.next };
        if let Ok(text) = serde_json::to_vec_pretty(&saved) {
            let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
            let tmp = path.with_extension(format!("json.{}", std::process::id()));
            if std::fs::write(&tmp, text).is_err() || std::fs::rename(&tmp, &path).is_err() {
                let _ = std::fs::remove_file(&tmp);
                log::warn!("accounts: can't save {}", path.display());
            }
        }
        cx.notify();
    }
}

/// Whether a `claude` runs in account folder `dir`: one of the processes
/// Claude Code keeps a file for (`sessions/<pid>.json`) is alive and is
/// Claude Code (a file left by one that crashed may name a pid reused
/// since). One of another user's, which can't be looked at, counts.
fn in_use(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir.join("sessions")) else { return false };
    entries.flatten().any(|e| {
        let pid = e.path().file_stem().and_then(|s| s.to_str()).and_then(|s| s.parse::<u32>().ok());
        pid.is_some_and(|pid| match alive(pid) {
            Some(true) => tern_term::procs::is_claude(pid),
            Some(false) => false,
            None => true,
        })
    })
}

/// Whether process `pid` runs: `None` when it is another user's, which
/// can't be told more about.
pub fn alive(pid: u32) -> Option<bool> {
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else { return Some(false) };
        // SAFETY: signal 0 only checks that the process exists.
        if unsafe { libc::kill(pid, 0) } == 0 {
            return Some(true);
        }
        (std::io::Error::last_os_error().raw_os_error() != Some(libc::EPERM)).then_some(false)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        Some(true)
    }
}

/// Clears away removed account `id`: its folder to the Trash, its
/// sign-in and its usage numbers deleted.
fn clear(id: &str) {
    let dir = folder(id);
    if dir.symlink_metadata().is_ok()
        && let Err(e) = tern_fs::ops::trash(&dir)
    {
        log::warn!("accounts: trash {}: {e}", dir.display());
    }
    forget_sign_in(id);
    crate::usage::forget_cached(id);
}

/// Clears away the folders of accounts not in `listed` (removed while a
/// `claude` ran in them) that no `claude` runs in now.
fn clear_removed(listed: &[String]) {
    let Ok(entries) = std::fs::read_dir(accounts_dir()) else { return };
    for e in entries.flatten() {
        let Some(id) = e.file_name().to_str().map(str::to_owned) else { continue };
        if id.starts_with("account-") && !listed.contains(&id) && !in_use(&e.path()) {
            clear(&id);
        }
    }
}

/// Deletes account `id`'s sign-in from the login keychain: Claude Code
/// keeps one item per user name it signed in as.
#[cfg(target_os = "macos")]
fn forget_sign_in(id: &str) {
    if id == DEFAULT {
        return;
    }
    let service = keychain_service(id);
    for _ in 0..4 {
        let deleted = std::process::Command::new("/usr/bin/security")
            .args(["delete-generic-password", "-s", &service])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !deleted {
            break;
        }
    }
}

/// Elsewhere the sign-in is a file in the account's folder.
#[cfg(not(target_os = "macos"))]
fn forget_sign_in(_id: &str) {}

/// Claude Code's own folder (`CLAUDE_CONFIG_DIR`, else `~/.claude`).
pub fn default_dir() -> Option<PathBuf> {
    match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        Some(dir) => Some(PathBuf::from(dir)),
        None => crate::dirs::home().map(|h| h.join(".claude")),
    }
}

/// Where the added accounts' folders are.
pub fn accounts_dir() -> PathBuf {
    crate::dirs::data().join("claude-accounts")
}

/// An added account's folder.
fn folder(id: &str) -> PathBuf {
    accounts_dir().join(id)
}

/// Whether account `id` is one of the list's (the default one always is).
pub fn listed(id: &str, cx: &mut App) -> bool {
    id == DEFAULT || store(cx).read(cx).get(id).is_some()
}

/// The `CLAUDE_CONFIG_DIR` account `id` runs with; `None` for the default
/// one (and an unknown id), which runs as Claude Code would.
pub fn config_dir(id: &str) -> Option<PathBuf> {
    (id != DEFAULT && !id.is_empty()).then(|| folder(id))
}

/// The folder Claude Code keeps account `id`'s files in.
pub fn claude_dir(id: &str) -> Option<PathBuf> {
    config_dir(id).or_else(default_dir)
}

/// The `CLAUDE_CONFIG_DIR` account `id`'s `claude` runs with: its own
/// folder, or for the default account the one Tern was started with.
fn explicit_dir(id: &str) -> Option<PathBuf> {
    config_dir(id).or_else(|| std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()).map(PathBuf::from))
}

/// Where Claude Code keeps account `id`'s settings and sign-in details
/// (`.claude.json`): in its `CLAUDE_CONFIG_DIR`, else in the home folder
/// (or, as older versions did, in `~/.claude`).
pub fn claude_json(id: &str) -> Vec<PathBuf> {
    match explicit_dir(id) {
        Some(dir) => vec![dir.join(".claude.json")],
        None => {
            let mut paths: Vec<PathBuf> = crate::dirs::home().map(|h| h.join(".claude.json")).into_iter().collect();
            paths.extend(default_dir().map(|d| d.join(".claude.json")));
            paths
        },
    }
}

/// The account's email, from Claude Code's settings once it signed in.
pub fn email(id: &str) -> Option<String> {
    claude_json(id).iter().find_map(|p| {
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(p).ok()?).ok()?;
        v["oauthAccount"]["emailAddress"].as_str().map(str::to_owned)
    })
}

/// The keychain item Claude Code keeps an account's sign-in in: its own
/// name without a `CLAUDE_CONFIG_DIR`, with the first 8 hex digits of the
/// folder's SHA-256 with one.
pub fn keychain_service(id: &str) -> String {
    match explicit_dir(id) {
        Some(dir) => {
            use sha2::Digest as _;
            let digest = sha2::Sha256::digest(dir.to_string_lossy().as_bytes());
            let hex: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
            format!("Claude Code-credentials-{hex}")
        },
        None => "Claude Code-credentials".into(),
    }
}

/// Makes account `id`'s folder ready for a `claude` to start in it: made,
/// linked to the default folder's setup and history, and given the MCP
/// servers it lacks. Links already there, and anything Claude made itself,
/// stay. Blocking file work: done when the account is added, and at start
/// in the background.
pub fn prepare(id: &str) -> io::Result<()> {
    let Some(dir) = config_dir(id) else { return Ok(()) };
    let Some(default) = default_dir() else { return Err(io::Error::other("no home folder")) };
    // Owner only: Claude Code keeps the account's sign-in details here.
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(&dir)?;
    for item in HISTORY {
        let target = default.join(item);
        if !target.exists() {
            if item.contains('.') {
                std::fs::OpenOptions::new().create(true).append(true).open(&target)?;
            } else {
                std::fs::create_dir_all(&target)?;
            }
        }
    }
    for item in SHARED.iter().chain(HISTORY) {
        link(&default.join(item), &dir.join(item))?;
    }
    if let Err(e) = mirror_mcp_servers(id) {
        log::warn!("accounts: MCP servers for {id}: {e}");
    }
    Ok(())
}

/// Links `at` to `target` unless something is at `at` already or `target`
/// isn't there.
fn link(target: &Path, at: &Path) -> io::Result<()> {
    if at.symlink_metadata().is_ok() || !target.exists() {
        return Ok(());
    }
    #[cfg(unix)]
    return std::os::unix::fs::symlink(target, at);
    #[cfg(windows)]
    return if target.is_dir() { std::os::windows::fs::symlink_dir(target, at) } else { std::os::windows::fs::symlink_file(target, at) };
}

/// The user's MCP servers (`mcpServers` of the default account's
/// `.claude.json`) in account `id`'s, which Claude Code keeps per account:
/// those it lacks by name. Its own, and its own versions, stay.
fn mirror_mcp_servers(id: &str) -> io::Result<()> {
    let Some(dest) = claude_json(id).into_iter().next() else { return Ok(()) };
    let read = |p: &Path| -> Option<serde_json::Value> { serde_json::from_slice(&std::fs::read(p).ok()?).ok() };
    let servers = claude_json(DEFAULT).iter().find_map(|p| read(p)?.get("mcpServers")?.as_object().cloned());
    let Some(servers) = servers else { return Ok(()) };
    let mut v = read(&dest).unwrap_or_else(|| serde_json::json!({}));
    let Some(obj) = v.as_object_mut() else { return Ok(()) };
    let own = obj.entry("mcpServers").or_insert_with(|| serde_json::json!({}));
    let Some(own) = own.as_object_mut() else { return Ok(()) };
    if !merge_missing(own, servers) {
        return Ok(());
    }
    // Whole or not at all: a `claude` of this account may read it meanwhile.
    let tmp = dest.with_extension(format!("json.tern-{}", std::process::id()));
    // Owner only, as Claude Code keeps it: MCP servers may carry secrets.
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
    std::io::Write::write_all(&mut open.open(&tmp)?, &serde_json::to_vec_pretty(&v)?)?;
    std::fs::rename(&tmp, &dest).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Adds `from`'s entries whose keys `into` lacks; whether any were.
fn merge_missing(into: &mut serde_json::Map<String, serde_json::Value>, from: serde_json::Map<String, serde_json::Value>) -> bool {
    let mut added = false;
    for (k, v) in from {
        if !into.contains_key(&k) {
            into.insert(k, v);
            added = true;
        }
    }
    added
}

#[cfg(test)]
mod tests {
    use super::{Account, DEFAULT, merge_missing, normalized};

    fn acc(id: &str, active: bool) -> Account {
        Account { id: id.into(), name: id.into(), active }
    }

    #[test]
    fn default_first_and_one_active() {
        let l = normalized(vec![]);
        assert_eq!(l, [Account { id: DEFAULT.into(), name: String::new(), active: true }]);
        let l = normalized(vec![Account { id: DEFAULT.into(), name: "Default".into(), active: true }]);
        assert_eq!(l[0].name, "");
        let l = normalized(vec![acc("account-2", false), acc(DEFAULT, false)]);
        assert_eq!(l.iter().map(|a| (a.id.as_str(), a.active)).collect::<Vec<_>>(), [(DEFAULT, true), ("account-2", false)]);
        let l = normalized(vec![acc("account-2", true), acc(DEFAULT, false), acc(DEFAULT, true)]);
        assert_eq!(l.iter().map(|a| (a.id.as_str(), a.active)).collect::<Vec<_>>(), [(DEFAULT, false), ("account-2", true)]);
    }

    #[test]
    fn mcp_servers_merge_keeps_own() {
        let mut own = serde_json::json!({"a": {"command": "mine"}, "b": {}}).as_object().cloned().unwrap();
        let from = serde_json::json!({"a": {"command": "theirs"}, "c": {}}).as_object().cloned().unwrap();
        assert!(merge_missing(&mut own, from.clone()));
        assert_eq!(own["a"]["command"], "mine");
        assert!(own.contains_key("c"));
        assert!(!merge_missing(&mut own, from));
    }
}
