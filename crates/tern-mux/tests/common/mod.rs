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
    // Short path: Unix socket paths are limited to 104 bytes.
    let tmp = tempfile::Builder::new().prefix("tm").tempdir_in("/tmp").unwrap();
    let dir = tmp.path().join("m");
    let opts =
        ServeOpts { idle_exit: idle, notifications: false, run_root: tmp.path().join("run"), hooks: true, max_queued, max_queued_session };
    let d = dir.clone();
    let thread = std::thread::spawn(move || serve(&d, opts));
    wait_until("socket", || paths::socket_path(&dir).exists());
    Daemon { _tmp: tmp, dir, thread: Some(thread) }
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

pub fn ready_id(ev: &Events) -> Option<u64> {
    ev.lock().unwrap().iter().find_map(|e| match e {
        TermEvent::Ready { id, .. } => Some(*id),
        _ => None,
    })
}
