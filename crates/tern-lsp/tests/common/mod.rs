#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tern_lsp::{LspEvent, Manager, ServerConfig, Timeouts};

const MOCK: &str = env!("CARGO_BIN_EXE_tern-lsp-mock");

pub struct Fixture {
    _dir: tempfile::TempDir,
    pub ws: PathBuf,
    pub log: PathBuf,
}

impl Fixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("ws");
        std::fs::create_dir_all(ws.join("src")).unwrap();
        std::fs::write(ws.join("Cargo.toml"), "").unwrap();
        std::fs::write(ws.join("src/main.rs"), "fn main() {}\n").unwrap();
        let log = dir.path().join("log.jsonl");
        Self { _dir: dir, ws, log }
    }

    /// A mock server config running `script` (its `log` set to this fixture's).
    pub fn mock(&self, name: &str, mut script: Value) -> ServerConfig {
        script["log"] = json!(self.log);
        let path = self.ws.parent().unwrap().join(format!("{name}.json"));
        std::fs::write(&path, script.to_string()).unwrap();
        ServerConfig {
            name: name.into(),
            languages: vec!["rust".into()],
            command: MOCK.into(),
            args: vec![],
            env: [("MOCK_LSP_SCRIPT".to_owned(), path.display().to_string())].into(),
            root_markers: vec!["Cargo.toml".into()],
            require_root_marker: false,
            outermost_root: false,
            fallback: false,
            initialization_options: None,
            settings: None,
            install_hint: Some(format!("install {name}")),
        }
    }

    pub fn missing(&self, name: &str) -> ServerConfig {
        ServerConfig { command: format!("tern-no-such-{name}"), ..self.mock(name, json!({})) }
    }

    pub fn main_rs(&self) -> PathBuf {
        self.ws.join("src/main.rs")
    }

    /// The messages the mock(s) received, by method, in order.
    pub fn received(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.log).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
    }

    pub fn methods(&self) -> Vec<String> {
        self.received().iter().filter_map(|m| m["method"].as_str().map(str::to_owned)).collect()
    }

    pub fn manager(&self, configs: Vec<ServerConfig>) -> (Arc<Manager>, flume::Receiver<LspEvent>) {
        self.manager_with(configs, fast())
    }

    pub fn manager_with(&self, configs: Vec<ServerConfig>, t: Timeouts) -> (Arc<Manager>, flume::Receiver<LspEvent>) {
        let (tx, rx) = flume::unbounded();
        (Manager::with_timeouts(self.ws.clone(), configs, tx, t), rx)
    }
}

pub fn fast() -> Timeouts {
    Timeouts {
        initialize: Duration::from_secs(10),
        request: Duration::from_secs(10),
        restart_backoff: Duration::ZERO,
        ..Timeouts::default()
    }
}

pub fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ok() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
