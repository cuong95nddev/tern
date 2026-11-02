//! A scripted language server for tern-lsp's tests. `MOCK_LSP_SCRIPT` names
//! a JSON file:
//!
//! - `capabilities`: the `initialize` result's capabilities;
//! - `responses`: results by `method`, or by `method@line:col` for a
//!   position (tried first);
//! - `delays_ms`: how long to wait before answering, by method;
//! - `crash_after`: exit after answering this many requests (not counting
//!   `initialize`);
//! - `log`: a file each message received is appended to, one JSON per line;
//! - `exit_marker`: a file written on `exit`;
//! - `ask_configuration`: after `initialized`, ask `workspace/configuration`
//!   for this section (the answer is logged like any message).

use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

fn read_message(r: &mut impl BufRead) -> Option<Value> {
    let mut len = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length: ") {
            len = v.parse().ok();
        }
    }
    let mut buf = vec![0; len?];
    r.read_exact(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

fn write_message(out: &Mutex<std::io::Stdout>, v: &Value) {
    let body = v.to_string();
    let mut out = out.lock().unwrap();
    let _ = write!(out, "Content-Length: {}\r\n\r\n{body}", body.len());
    let _ = out.flush();
}

fn main() {
    let script: Value = std::env::var("MOCK_LSP_SCRIPT")
        .ok()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(json!({}));
    let script = Arc::new(script);
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let answered = Arc::new(AtomicUsize::new(0));
    let log = script["log"].as_str().map(|p| Mutex::new(std::fs::OpenOptions::new().create(true).append(true).open(p).unwrap()));
    let mut input = BufReader::new(std::io::stdin());
    while let Some(msg) = read_message(&mut input) {
        if let Some(log) = &log {
            let _ = writeln!(log.lock().unwrap(), "{msg}");
        }
        let method = msg["method"].as_str().unwrap_or("").to_owned();
        let id = msg.get("id").cloned();
        match (method.as_str(), id) {
            ("exit", _) => {
                if let Some(p) = script["exit_marker"].as_str() {
                    let _ = std::fs::write(p, "exit");
                }
                std::process::exit(0);
            },
            ("initialized", None) => {
                if let Some(section) = script["ask_configuration"].as_str() {
                    let ask = json!({"jsonrpc": "2.0", "id": "c1", "method": "workspace/configuration", "params": {"items": [{"section": section}]}});
                    write_message(&out, &ask);
                }
            },
            ("initialize", Some(id)) => {
                let result = json!({"capabilities": script.get("capabilities").cloned().unwrap_or(json!({}))});
                let delay = script["delays_ms"]["initialize"].as_u64().unwrap_or(0);
                let out = out.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(delay));
                    write_message(&out, &json!({"jsonrpc": "2.0", "id": id, "result": result}));
                });
            },
            (m, Some(id)) if !m.is_empty() => {
                let pos = &msg["params"]["position"];
                let keyed = format!("{m}@{}:{}", pos["line"], pos["character"]);
                let result = script["responses"].get(&keyed).or_else(|| script["responses"].get(m)).cloned().unwrap_or(Value::Null);
                let delay = script["delays_ms"][m].as_u64().unwrap_or(0);
                let crash_after = script["crash_after"].as_u64().map(|n| n as usize);
                let (out, answered) = (out.clone(), answered.clone());
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(delay));
                    write_message(&out, &json!({"jsonrpc": "2.0", "id": id, "result": result}));
                    let n = answered.fetch_add(1, Ordering::SeqCst) + 1;
                    if crash_after.is_some_and(|c| n >= c) {
                        std::process::exit(3);
                    }
                });
            },
            _ => {},
        }
    }
}
