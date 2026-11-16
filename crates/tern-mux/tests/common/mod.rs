#![allow(dead_code)]
//! Helpers shared by the daemon and observer tests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tern_mux::client::MuxClient;
use tern_mux::paths;
use tern_mux::server::{ServeOpts, serve};
use tern_term::{SpawnConfig, TermEvent, TermSize};

pub const SIZE: TermSize = TermSize { cols: 60, rows: 10, cell_width: 8, cell_height: 16 };

pub struct Daemon {
    pub _tmp: tempfile::TempDir,
    pub dir: PathBuf,
    pub thread: Option<std::thread::JoinHandle<std::io::Result<()>>>,
}

pub fn daemon(idle: Duration) -> Daemon {
    daemon_with(idle, 1 << 20, usize::MAX)
}

pub fn daemon_with(idle: Duration, max_queued: usize, max_queued_session: usize) -> Daemon {
    let tmp = short_tempdir();
    let dir = tmp.path().join("m");
    let opts = ServeOpts {
        idle_exit: idle,
        notifications: false,
        run_root: tmp.path().join("run"),
        hooks: true,
        max_queued,
        max_queued_session,
        #[cfg(unix)]
        successor: None,
    };
    let d = dir.clone();
    let thread = std::thread::spawn(move || serve(&d, opts));
    // Windows binds the socket's file before it listens: a client in
    // between is refused. Up once one connects.
    #[cfg(unix)]
    use std::os::unix::net::UnixStream;
    #[cfg(windows)]
    use uds_windows::UnixStream;
    wait_until("socket", || UnixStream::connect(paths::socket_path(&dir)).is_ok());
    Daemon { _tmp: tmp, dir, thread: Some(thread) }
}

/// A temporary folder with a short path: Unix socket paths are limited to
/// 104 bytes (108 on Windows).
pub fn short_tempdir() -> tempfile::TempDir {
    let base = if cfg!(windows) { std::env::temp_dir() } else { PathBuf::from("/tmp") };
    tempfile::Builder::new().prefix("tm").tempdir_in(base).unwrap()
}

pub fn client(dir: &Path) -> MuxClient {
    MuxClient::connect(dir, false, Arc::new(|_, _| {}), Arc::new(|_| {})).unwrap()
}

pub fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub type Events = Arc<Mutex<Vec<TermEvent>>>;

pub fn sink() -> (tern_term::EventSink, Events) {
    let ev: Events = Arc::default();
    let e = ev.clone();
    (Arc::new(move |x| e.lock().unwrap().push(x)), ev)
}

#[cfg(unix)]
pub fn sh(script: &str) -> SpawnConfig {
    SpawnConfig {
        program: Some("/bin/sh".into()),
        args: vec!["-c".into(), script.into()],
        cwd: Some("/tmp".into()),
        env: HashMap::new(),
        size: SIZE,
        scrollback: 1000,
    }
}

/// Windows PowerShell running `script`.
#[cfg(windows)]
pub fn ps(script: &str) -> SpawnConfig {
    SpawnConfig {
        program: Some("powershell.exe".into()),
        args: vec!["-NoLogo".into(), "-NoProfile".into(), "-Command".into(), script.into()],
        cwd: Some(std::env::temp_dir()),
        env: HashMap::new(),
        size: SIZE,
        scrollback: 1000,
    }
}

pub fn ready_id(ev: &Events) -> Option<u64> {
    ev.lock().unwrap().iter().find_map(|e| match e {
        TermEvent::Ready { id, .. } => Some(*id),
        _ => None,
    })
}

#[cfg(unix)]
/// A folder with a `codex` that answers `--version` with `version`, for
/// the PATH of a tab's env.
pub fn fake_codex(version: &str) -> (tempfile::TempDir, String) {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let codex = dir.path().join("codex");
    std::fs::write(&codex, format!("#!/bin/sh\necho 'codex-cli {version}'\n")).unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:/usr/bin:/bin", dir.path().display());
    (dir, path)
}

#[cfg(unix)]
/// A Codex tab running `script`, in which `$@` is what the daemon put
/// in for Tern's hooks.
pub fn codex_tab(path: &str, script: &str) -> tern_mux::client::Open {
    let mut cfg = sh(&format!("set -- {{tern-settings}}; {script}"));
    cfg.env.insert("PATH".into(), path.to_owned());
    tern_mux::client::Open::Spawn { cfg, hook: true, seed: None, agent: Some("codex".into()) }
}

#[cfg(unix)]
/// A folder with an `opencode` that answers `--version` with `version`, for
/// the PATH of a tab's env.
pub fn fake_opencode(version: &str) -> (tempfile::TempDir, String) {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let opencode = dir.path().join("opencode");
    std::fs::write(&opencode, format!("#!/bin/sh\necho '{version}'\n")).unwrap();
    std::fs::set_permissions(&opencode, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:/usr/bin:/bin", dir.path().display());
    (dir, path)
}

#[cfg(unix)]
/// An OpenCode tab running `script`, in which `$@` is what the daemon put
/// in for Tern's plugin.
pub fn opencode_tab(path: &str, script: &str) -> tern_mux::client::Open {
    let mut cfg = sh(&format!("set -- {{tern-settings}}; {script}"));
    cfg.env.insert("PATH".into(), path.to_owned());
    tern_mux::client::Open::Spawn { cfg, hook: true, seed: None, agent: Some("opencode".into()) }
}

#[cfg(unix)]
/// Sets `$url` to the hook endpoint of a Codex tab (see [`codex_tab`]).
pub const CODEX_URL: &str = r#"file=$(printf %s "$3" | sed -n "s/.*--hook '\([^']*\)'.*/\1/p" | head -1); url=$(sed -n 's/.*"hook_url":"\([^"]*\)".*/\1/p' "$file"); "#;
