use std::future::Future as _;
use std::path::Path;
use std::time::{Duration, Instant};

use futures::executor::block_on;
use serde_json::{Value, json};
use tern_lsp::{LspError, LspEvent, Manager, ServerConfig, Timeouts};

mod common;
use common::*;

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
fn definition_now_does_not_wait_for_a_starting_server() {
    let f = Fixture::new();
    let script = json!({"delays_ms": {"initialize": 1500}, "capabilities": {"definitionProvider": true}, "responses": {"textDocument/definition": []}});
    let (m, _) = f.manager(vec![f.mock("a", script)]);
    m.open(&f.main_rs(), "fn main() {}\n".into());
    wait_for("the start", || f.methods().contains(&"initialize".to_owned()));
    // A tab's F12 while its server starts: names (tree-sitter) answer at once.
    let start = Instant::now();
    assert!(matches!(block_on(m.definition_now(&f.main_rs(), lsp_types::Position::new(0, 3))), Err(LspError::Unavailable(_))));
    assert!(start.elapsed() < Duration::from_millis(500), "waited {:?}", start.elapsed());
    wait_for("the server", || f.methods().contains(&"initialized".to_owned()));
    assert_eq!(block_on(m.definition_now(&f.main_rs(), lsp_types::Position::new(0, 3))), Ok(vec![]));
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
fn crash_after_running_restarts_at_once() {
    let f = Fixture::new();
    let t = Timeouts { restart_backoff: Duration::from_millis(500), ..fast() };
    let (m, events) = f.manager_with(vec![f.mock("a", json!({"crash_after": 1, "responses": {"textDocument/definition": 2}}))], t);
    assert_eq!(block_on(m.request(&f.main_rs(), "textDocument/definition", json!({}))).unwrap(), 2);
    // It ran longer than the backoff before it crashed: the next use restarts it.
    std::thread::sleep(Duration::from_millis(700));
    assert_eq!(block_on(m.request(&f.main_rs(), "textDocument/definition", json!({}))).unwrap(), 2);
    assert_eq!(f.methods().iter().filter(|m| *m == "initialize").count(), 2);
    // It started, then stopped: said as that, not as a failed start.
    assert_eq!(events.try_iter().collect::<Vec<_>>(), [LspEvent::Exited { server: "a".into() }]);
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
fn answers_workspace_folders_with_the_root() {
    let f = Fixture::new();
    let (m, _) = f.manager(vec![f.mock("a", json!({"ask_folders": true}))]);
    m.open(&f.main_rs(), "x".into());
    wait_for("the folders answer", || f.received().iter().any(|m| m["id"] == "w1"));
    let answer = f.received().into_iter().find(|m| m["id"] == "w1").unwrap();
    let uri = tern_lsp::uri::path_to_uri(&f.ws).unwrap();
    assert_eq!(answer["result"], json!([{"uri": uri.as_str(), "name": "ws"}]));
}

#[test]
fn settled_names_its_server() {
    let f = Fixture::new();
    let (m, events) = f.manager(vec![f.mock("a", json!({"progress": true}))]);
    m.open(&f.main_rs(), "x".into());
    let event = events.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(event, LspEvent::Settled { server: "a".into() });
    assert!(m.serves(&f.main_rs(), "a"));
    assert!(!m.serves(&f.main_rs(), "b"));
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
fn shutting_down_waits_for_servers_to_exit() {
    let f = Fixture::new();
    let marker = f.ws.parent().unwrap().join("exited");
    let (m, _) = f.manager(vec![f.mock("a", json!({"exit_marker": marker}))]);
    hover(&m, &f.main_rs()).unwrap();
    // As at quit: done only once the server has gone.
    m.shut_down_servers();
    assert!(marker.exists());
}

#[cfg(unix)]
#[test]
fn killing_a_server_takes_its_children() {
    let f = Fixture::new();
    let pid_file = f.ws.parent().unwrap().join("child.pid");
    let t = Timeouts { shutdown: Duration::from_millis(300), exit: Duration::from_millis(300), ..fast() };
    let (m, _) = f.manager_with(vec![f.mock("a", json!({"ignore_exit": true, "child_pid_file": pid_file}))], t);
    hover(&m, &f.main_rs()).unwrap();
    let pid = std::fs::read_to_string(&pid_file).unwrap();
    m.shut_down_servers();
    let alive = || std::process::Command::new("kill").args(["-0", pid.trim()]).status().unwrap().success();
    wait_for("the child to go", || !alive());
}

#[test]
fn files_opened_without_a_tab_are_closed_again() {
    let f = Fixture::new();
    let (m, _) = f.manager(vec![f.mock("a", json!({}))]);
    m.open(&f.main_rs(), "fn main() {}\n".into());
    // Outlines of supertypes and the like, in files no tab shows.
    for i in 0..40 {
        let p = f.ws.join(format!("src/f{i}.rs"));
        std::fs::write(&p, "fn f() {}\n").unwrap();
        hover(&m, &p).unwrap();
    }
    hover(&m, &f.main_rs()).unwrap();
    let closed: Vec<String> = f
        .received()
        .iter()
        .filter(|m| m["method"] == "textDocument/didClose")
        .filter_map(|m| m["params"]["textDocument"]["uri"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(closed.len(), 8, "{closed:?}");
    assert!(closed[0].ends_with("/src/f0.rs"));
    assert!(!closed.iter().any(|u| u.ends_with("/main.rs")), "a tab's file stays open");
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

#[cfg(unix)]
#[test]
fn exiting_at_start_reports_failed_with_hint() {
    // A rustup proxy for a component that isn't installed runs, then exits.
    let f = Fixture::new();
    let config = ServerConfig { command: "/usr/bin/false".into(), ..f.mock("ra", json!({})) };
    let t = Timeouts { restart_backoff: Duration::from_secs(60), ..fast() };
    let (m, rx) = f.manager_with(vec![config], t);
    assert!(matches!(hover(&m, &f.main_rs()), Err(LspError::Unavailable(_))));
    let events: Vec<_> = rx.try_iter().collect();
    assert!(
        matches!(&events[..], [LspEvent::Failed { server, hint, .. }] if server == "ra" && hint.as_deref() == Some("install ra")),
        "{events:?}"
    );
}
