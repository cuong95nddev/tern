use std::future::Future as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::executor::block_on;
use serde_json::{Value, json};
use tern_lsp::{LspError, LspEvent, Manager, ServerConfig, Timeouts};

const MOCK: &str = env!("CARGO_BIN_EXE_tern-lsp-mock");

struct Fixture {
    _dir: tempfile::TempDir,
    ws: PathBuf,
    log: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("ws");
        std::fs::create_dir_all(ws.join("src")).unwrap();
        std::fs::write(ws.join("Cargo.toml"), "").unwrap();
        std::fs::write(ws.join("src/main.rs"), "fn main() {}\n").unwrap();
        let log = dir.path().join("log.jsonl");
        Self { _dir: dir, ws, log }
    }

    /// A mock server config running `script` (its `log` set to this fixture's).
    fn mock(&self, name: &str, mut script: Value) -> ServerConfig {
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
            fallback: false,
            initialization_options: None,
            settings: None,
            install_hint: Some(format!("install {name}")),
        }
    }

    fn missing(&self, name: &str) -> ServerConfig {
        ServerConfig { command: format!("tern-no-such-{name}"), ..self.mock(name, json!({})) }
    }

    fn main_rs(&self) -> PathBuf {
        self.ws.join("src/main.rs")
    }

    /// The messages the mock(s) received, by method, in order.
    fn received(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.log).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
    }

    fn methods(&self) -> Vec<String> {
        self.received().iter().filter_map(|m| m["method"].as_str().map(str::to_owned)).collect()
    }

    fn manager(&self, configs: Vec<ServerConfig>) -> (Arc<Manager>, flume::Receiver<LspEvent>) {
        self.manager_with(configs, fast())
    }

    fn manager_with(&self, configs: Vec<ServerConfig>, t: Timeouts) -> (Arc<Manager>, flume::Receiver<LspEvent>) {
        let (tx, rx) = flume::unbounded();
        (Manager::with_timeouts(self.ws.clone(), configs, tx, t), rx)
    }
}

fn fast() -> Timeouts {
    Timeouts {
        initialize: Duration::from_secs(10),
        request: Duration::from_secs(10),
        restart_backoff: Duration::ZERO,
        ..Timeouts::default()
    }
}

fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ok() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn hover(m: &Manager, path: &Path) -> Result<Value, LspError> {
    block_on(m.request(path, "textDocument/hover", json!({"position": {"line": 0, "character": 3}})))
}

#[test]
fn starts_on_first_use_and_opens_document() {
    let f = Fixture::new();
    let (m, _) = f.manager(vec![f.mock("a", json!({"responses": {"textDocument/hover": {"contents": "hi"}}}))]);
    m.open(&f.main_rs(), "fn main() { x }\n".into());
    assert_eq!(hover(&m, &f.main_rs()).unwrap()["contents"], "hi");
    let got = f.received();
    let methods = f.methods();
    assert_eq!(&methods[..3], ["initialize", "initialized", "textDocument/didOpen"]);
    let open = &got[2]["params"]["textDocument"];
    assert_eq!(open["text"], "fn main() { x }\n");
    assert_eq!(open["languageId"], "rust");
    assert_eq!(got[0]["params"]["rootUri"].as_str().unwrap(), format!("file://{}", f.ws.display()));
    // The request names the document.
    let req = got.iter().find(|m| m["method"] == "textDocument/hover").unwrap();
    assert!(req["params"]["textDocument"]["uri"].as_str().unwrap().ends_with("/src/main.rs"));
}

#[test]
fn change_sends_full_text_with_increasing_version() {
    let f = Fixture::new();
    let (m, _) = f.manager(vec![f.mock("a", json!({}))]);
    m.open(&f.main_rs(), "a".into());
    hover(&m, &f.main_rs()).unwrap();
    m.change(&f.main_rs(), "ab".into());
    m.change(&f.main_rs(), "abc".into());
    m.save(&f.main_rs());
    m.close(&f.main_rs());
    wait_for("didClose", || f.methods().contains(&"textDocument/didClose".to_owned()));
    let got = f.received();
    let changes: Vec<_> = got.iter().filter(|m| m["method"] == "textDocument/didChange").collect();
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[0]["params"]["textDocument"]["version"], 2);
    assert_eq!(changes[1]["params"]["textDocument"]["version"], 3);
    assert_eq!(changes[1]["params"]["contentChanges"][0]["text"], "abc");
    assert!(f.methods().contains(&"textDocument/didSave".to_owned()));
}

#[test]
fn open_starts_the_server_without_a_request() {
    let f = Fixture::new();
    let (m, _) = f.manager(vec![f.mock("a", json!({}))]);
    m.open(&f.main_rs(), "x".into());
    wait_for("didOpen", || f.methods().contains(&"textDocument/didOpen".to_owned()));
}

#[test]
fn missing_server_tries_fallback_then_reports_once() {
    let f = Fixture::new();
    let fallback = ServerConfig { fallback: true, ..f.mock("b", json!({"responses": {"textDocument/hover": 1}})) };
    let (m, rx) = f.manager(vec![f.missing("a"), fallback]);
    assert_eq!(hover(&m, &f.main_rs()).unwrap(), 1);
    assert!(rx.try_recv().is_err(), "a fallback that runs is no problem");

    let f = Fixture::new();
    let (m, rx) = f.manager(vec![f.missing("a"), ServerConfig { fallback: true, ..f.missing("b") }]);
    assert!(matches!(hover(&m, &f.main_rs()), Err(LspError::NotFound)));
    assert!(matches!(hover(&m, &f.main_rs()), Err(LspError::NotFound)));
    let events: Vec<_> = rx.try_iter().collect();
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(matches!(&events[0], LspEvent::Missing { server, hint } if server == "a" && hint.as_deref() == Some("install a")));
}

#[test]
fn initialize_timeout_falls_back() {
    let f = Fixture::new();
    let t = Timeouts { initialize: Duration::from_millis(300), restart_backoff: Duration::from_secs(60), ..fast() };
    let (m, _) = f.manager_with(vec![f.mock("a", json!({"delays_ms": {"initialize": 120000}}))], t);
    let start = Instant::now();
    assert!(matches!(hover(&m, &f.main_rs()), Err(LspError::Unavailable(_))));
    assert!(start.elapsed() < Duration::from_secs(5));
    // Within the backoff: no new attempt, an answer at once.
    let start = Instant::now();
    assert!(matches!(hover(&m, &f.main_rs()), Err(LspError::Unavailable(_))));
    assert!(start.elapsed() < Duration::from_millis(200));
}

#[test]
fn request_timeout() {
    let f = Fixture::new();
    let t = Timeouts { request: Duration::from_millis(200), ..fast() };
    let (m, _) = f.manager_with(vec![f.mock("a", json!({"delays_ms": {"textDocument/hover": 5000}}))], t);
    assert!(matches!(hover(&m, &f.main_rs()), Err(LspError::Timeout)));
}

#[test]
fn crashed_server_fails_pending_and_restarts() {
    let f = Fixture::new();
    let script = json!({"crash_after": 1, "delays_ms": {"textDocument/hover": 1000}, "responses": {"textDocument/hover": 1, "textDocument/definition": 2}});
    let (m, _) = f.manager(vec![f.mock("a", script)]);
    let slow = std::thread::spawn({
        let (m, p) = (m.clone(), f.main_rs());
        move || hover(&m, &p)
    });
    wait_for("the slow request", || f.methods().contains(&"textDocument/hover".to_owned()));
    assert_eq!(block_on(m.request(&f.main_rs(), "textDocument/definition", json!({}))).unwrap(), 2);
    assert!(matches!(slow.join().unwrap(), Err(LspError::Unavailable(_))));
    // Restarted on the next use.
    assert_eq!(block_on(m.request(&f.main_rs(), "textDocument/definition", json!({}))).unwrap(), 2);
    assert_eq!(f.methods().iter().filter(|m| *m == "initialize").count(), 2);
}

#[test]
fn cancel_on_drop_sends_cancel_request() {
    let f = Fixture::new();
    let (m, _) = f.manager(vec![f.mock("a", json!({"delays_ms": {"textDocument/hover": 5000}}))]);
    m.open(&f.main_rs(), "x".into());
    {
        let path = f.main_rs();
        let mut fut = Box::pin(m.request(&path, "textDocument/hover", json!({})));
        let waker = futures::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        wait_for("the request to be sent", || {
            let _ = fut.as_mut().poll(&mut cx);
            f.methods().contains(&"textDocument/hover".to_owned())
        });
    }
    wait_for("$/cancelRequest", || f.methods().contains(&"$/cancelRequest".to_owned()));
    let got = f.received();
    let id = &got.iter().find(|m| m["method"] == "textDocument/hover").unwrap()["id"];
    let cancel = got.iter().find(|m| m["method"] == "$/cancelRequest").unwrap();
    assert_eq!(&cancel["params"]["id"], id);
}

#[test]
fn answers_workspace_configuration() {
    let f = Fixture::new();
    let config = ServerConfig { settings: Some(json!({"gopls": {"x": 1}})), ..f.mock("a", json!({"ask_configuration": "gopls"})) };
    let (m, _) = f.manager(vec![config]);
    m.open(&f.main_rs(), "x".into());
    wait_for("the configuration answer", || f.received().iter().any(|m| m["id"] == "c1"));
    let answer = f.received().into_iter().find(|m| m["id"] == "c1").unwrap();
    assert_eq!(answer["result"], json!([{"x": 1}]));
    assert!(f.methods().contains(&"workspace/didChangeConfiguration".to_owned()));
}

#[test]
fn outside_workspace_reuses_running_server_or_unavailable() {
    let f = Fixture::new();
    let outside = f.ws.parent().unwrap().join("dep/lib.rs");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    std::fs::write(&outside, "pub fn f() {}\n").unwrap();
    let (m, _) = f.manager(vec![f.mock("a", json!({"responses": {"textDocument/hover": 7}}))]);
    assert!(matches!(hover(&m, &outside), Err(LspError::Unavailable(_))));
    assert_eq!(f.methods().iter().filter(|m| *m == "initialize").count(), 0, "never started for an outside file");
    hover(&m, &f.main_rs()).unwrap();
    assert_eq!(hover(&m, &outside).unwrap(), 7);
    let opened: Vec<_> = f.received().into_iter().filter(|m| m["method"] == "textDocument/didOpen").collect();
    assert!(opened.iter().any(|m| m["params"]["textDocument"]["text"] == "pub fn f() {}\n"));
    assert_eq!(f.methods().iter().filter(|m| *m == "initialize").count(), 1);
}

#[test]
fn drop_shuts_servers_down() {
    let f = Fixture::new();
    let marker = f.ws.parent().unwrap().join("exited");
    let (m, _) = f.manager(vec![f.mock("a", json!({"exit_marker": marker}))]);
    hover(&m, &f.main_rs()).unwrap();
    drop(m);
    wait_for("exit", || marker.exists());
    assert!(f.methods().contains(&"shutdown".to_owned()));
}

#[test]
fn files_changed_are_sent_as_watched_files() {
    let f = Fixture::new();
    let (m, _) = f.manager(vec![f.mock("a", json!({}))]);
    hover(&m, &f.main_rs()).unwrap();
    m.files_changed(&[f.main_rs(), f.ws.join("gone.rs")]);
    wait_for("didChangeWatchedFiles", || f.methods().contains(&"workspace/didChangeWatchedFiles".to_owned()));
    let msg = f.received().into_iter().find(|m| m["method"] == "workspace/didChangeWatchedFiles").unwrap();
    let changes = msg["params"]["changes"].as_array().unwrap();
    assert_eq!(changes[0]["type"], 2); // Changed
    assert_eq!(changes[1]["type"], 3); // Deleted
}
