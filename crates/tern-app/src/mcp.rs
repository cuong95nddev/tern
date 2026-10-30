//! `tern --mcp`: an MCP server on stdio through which a coding agent runs
//! its dev servers and other long-running commands as Tern services
//! (`tern_mux::services`), so the user sees them in the Services panel and
//! they outlive the agent. JSON-RPC 2.0, one message per line.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tern_mux::proto::{ServiceSpec, SessionInfo};
use tern_mux::services::{self, Services};
use tern_term::{SpawnConfig, TermSize};

/// The protocol versions this server speaks, newest first.
const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
/// How long `run_service` waits for a new service to listen (or end).
const START_WAIT: Duration = Duration::from_secs(8);
const DEFAULT_LOG_LINES: u32 = 100;
const MAX_LOG_LINES: u32 = 2000;

const INSTRUCTIONS: &str = "Run dev servers, watchers and other long-running commands with run_service \
instead of in your own shell (or in the background): Tern runs them in terminals the user can watch and stop, \
and they keep running after you finish. Check them with list_services and service_logs.";

pub fn run() -> i32 {
    let ctx = Context {
        cwd: std::env::current_dir().unwrap_or_default(),
        by: std::env::var("TERN_SESSION_ID").ok().and_then(|s| s.parse().ok()),
    };
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let answer = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&ctx, &msg),
            Err(e) => Some(error(Value::Null, -32700, &format!("parse error: {e}"))),
        };
        if let Some(answer) = answer
            && writeln!(stdout, "{answer}").and_then(|()| stdout.flush()).is_err()
        {
            break;
        }
    }
    0
}

/// Who asks: the folder the agent runs in, and its Tern session.
struct Context {
    cwd: PathBuf,
    by: Option<u64>,
}

/// The answer to one message; none for a notification.
fn handle(ctx: &Context, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned()?;
    let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    Some(match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or_default();
            let version = PROTOCOL_VERSIONS.iter().find(|v| **v == asked).unwrap_or(&PROTOCOL_VERSIONS[0]);
            result(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "tern", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": INSTRUCTIONS,
                }),
            )
        },
        "ping" => result(id, json!({})),
        "tools/list" => result(id, json!({ "tools": tools() })),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            let (text, is_error) = match call(ctx, name, &args) {
                Ok(text) => (text, false),
                Err(e) => (e, true),
            };
            result(id, json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }))
        },
        _ => error(id, -32601, &format!("unknown method {method}")),
    })
}

fn result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn tools() -> Value {
    let name = json!({ "type": "string", "description": "The service's name, as given to run_service (or its id)." });
    json!([
        {
            "name": "run_service",
            "description": "Start a long-running command (a dev server, a watcher, docker compose, ...) as a Tern service: \
    it runs in a terminal the user can see and stop, and keeps running after you finish. A running service of the same \
    name in the same folder is left as it is unless `restart` is true. Waits a few seconds, then reports its state, the \
    ports it listens on and its first output.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "A short name for it: web, api, worker, ..." },
                    "command": { "type": "string", "description": "The shell command to run, e.g. `npm run dev`." },
                    "cwd": { "type": "string", "description": "The folder to run it in (default: yours)." },
                    "restart": { "type": "boolean", "description": "Restart it when it already runs." },
                },
                "required": ["name", "command"],
            },
        },
        {
            "name": "list_services",
            "description": "The Tern services in your folder: state, ports, command, uptime.",
            "inputSchema": {
                "type": "object",
                "properties": { "all": { "type": "boolean", "description": "Every folder's services, not only yours." } },
            },
        },
        {
            "name": "service_logs",
            "description": "The last lines a service printed.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": name,
                    "lines": { "type": "integer", "description": "How many lines (default 100)." },
                },
                "required": ["name"],
            },
        },
        {
            "name": "stop_service",
            "description": "Stop a service and remove it.",
            "inputSchema": { "type": "object", "properties": { "name": name }, "required": ["name"] },
        },
        {
            "name": "restart_service",
            "description": "Stop a service and start its command again.",
            "inputSchema": { "type": "object", "properties": { "name": name }, "required": ["name"] },
        },
    ])
}

fn call(ctx: &Context, tool: &str, args: &Value) -> Result<String, String> {
    let str_arg = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    let connect = |start| {
        Services::connect(&tern_mux::paths::mux_dir(), start).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => "Tern's daemon isn't running: no services.".to_owned(),
            _ => format!("can't reach Tern: {e}"),
        })
    };
    match tool {
        "run_service" => {
            let name = str_arg("name").ok_or("`name` is required")?;
            let command = str_arg("command").ok_or("`command` is required")?;
            let cwd = match str_arg("cwd") {
                Some(c) => absolute(&ctx.cwd, c),
                None => ctx.cwd.clone(),
            };
            if !cwd.is_dir() {
                return Err(format!("no folder {}", cwd.display()));
            }
            let restart = args.get("restart").and_then(Value::as_bool).unwrap_or(false);
            let mut s = connect(true)?;
            let list = s.list().map_err(|e| e.to_string())?;
            let same = list.iter().filter(|i| i.service.as_ref().is_some_and(|sv| sv.name == name && sv.cwd == cwd));
            for old in same {
                if !old.exited && !restart {
                    return Ok(format!("{name} already runs (restart: true to restart it).\n\n{}", describe(old, &ports_of(&[old]))));
                }
                s.close(old.id).map_err(|e| e.to_string())?;
            }
            let spec = ServiceSpec { name: name.to_owned(), command: command.to_owned(), cwd, by: ctx.by, started: 0 };
            start(&mut s, spec)
        },
        "list_services" => {
            let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
            let mut s = connect(false)?;
            let list = s.list().map_err(|e| e.to_string())?;
            let shown: Vec<&SessionInfo> =
                list.iter().filter(|i| i.service.as_ref().is_some_and(|sv| all || related(&sv.cwd, &ctx.cwd))).collect();
            if shown.is_empty() {
                return Ok(if all { "No services." } else { "No services in this folder (all: true lists every folder's)." }.into());
            }
            let ports = ports_of(&shown);
            Ok(shown.iter().map(|i| describe(i, &ports)).collect::<Vec<_>>().join("\n\n"))
        },
        "service_logs" => {
            let mut s = connect(false)?;
            let info = find(&mut s, ctx, str_arg("name").ok_or("`name` is required")?)?;
            let lines = args.get("lines").and_then(Value::as_u64).map_or(DEFAULT_LOG_LINES, |n| (n as u32).clamp(1, MAX_LOG_LINES));
            let text = s.text(info.id, lines).map_err(|e| e.to_string())?.ok_or("it has gone")?;
            Ok(format!("{}\n\n{text}", describe(&info, &ports_of(&[&info]))))
        },
        "stop_service" => {
            let mut s = connect(false)?;
            let info = find(&mut s, ctx, str_arg("name").ok_or("`name` is required")?)?;
            s.close(info.id).map_err(|e| e.to_string())?;
            Ok(format!("Stopped {}.", service_name(&info)))
        },
        "restart_service" => {
            let mut s = connect(true)?;
            let info = find(&mut s, ctx, str_arg("name").ok_or("`name` is required")?)?;
            let spec = info.service.clone().ok_or("not a service")?;
            s.close(info.id).map_err(|e| e.to_string())?;
            start(&mut s, ServiceSpec { by: ctx.by.or(spec.by), ..spec })
        },
        _ => Err(format!("unknown tool {tool}")),
    }
}

/// Start `spec`, wait for it to listen or end (a while at most), and say
/// how it went.
fn start(s: &mut Services, spec: ServiceSpec) -> Result<String, String> {
    let (id, pid) = s.run(spawn_config(&spec), spec).map_err(|e| format!("can't start it: {e}"))?;
    let deadline = Instant::now() + START_WAIT;
    let (info, ports) = loop {
        std::thread::sleep(Duration::from_millis(500));
        let info = s.list().map_err(|e| e.to_string())?.into_iter().find(|i| i.id == id).ok_or("it ended and was removed")?;
        let ports = services::listening_ports(&[pid]);
        if info.exited || ports.get(&pid).is_some_and(|p| !p.is_empty()) || Instant::now() >= deadline {
            break (info, ports);
        }
    };
    let text = s.text(id, 30).map_err(|e| e.to_string())?.unwrap_or_default();
    let head = if info.exited { "It ended already." } else { "Started; the user sees it in Tern's Services panel." };
    Ok(format!("{head}\n\n{}\n\nOutput so far:\n{text}", describe(&info, &ports)))
}

/// Stop service `id` and start `spec` again (the Services panel's Restart).
pub fn restart(id: u64, spec: ServiceSpec) -> Result<(), String> {
    let mut s = Services::connect(&tern_mux::paths::mux_dir(), true).map_err(|e| e.to_string())?;
    s.close(id).map_err(|e| e.to_string())?;
    s.run(spawn_config(&spec), spec).map(|_| ()).map_err(|e| e.to_string())
}

/// How to run `spec`'s command: in a login shell, so the user's PATH holds.
fn spawn_config(spec: &ServiceSpec) -> SpawnConfig {
    let args =
        if cfg!(windows) { crate::platform::shell_command_args(&spec.command) } else { vec!["-lic".to_owned(), spec.command.clone()] };
    let mut env = std::collections::HashMap::new();
    env.insert("TERN_SERVICE".into(), spec.name.clone());
    SpawnConfig {
        program: Some(crate::platform::shell()),
        args,
        cwd: Some(spec.cwd.clone()),
        env,
        size: TermSize { cols: 120, rows: 40, cell_width: 8, cell_height: 16 },
        scrollback: 10_000,
    }
}

/// The service `name` (or id) is: of those so named, the one in the
/// caller's folder, a running one, the newest.
fn find(s: &mut Services, ctx: &Context, name: &str) -> Result<SessionInfo, String> {
    let list = s.list().map_err(|e| e.to_string())?;
    let mut named: Vec<SessionInfo> = list
        .into_iter()
        .filter(|i| i.service.as_ref().is_some_and(|sv| sv.name == name) || (i.service.is_some() && i.id.to_string() == name))
        .collect();
    named.sort_by_key(|i| {
        let sv = i.service.as_ref().unwrap();
        (!related(&sv.cwd, &ctx.cwd), i.exited, std::cmp::Reverse(sv.started))
    });
    named.into_iter().next().ok_or_else(|| format!("no service {name} (list_services lists them)"))
}

/// One folder is in the other.
fn related(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

fn absolute(base: &Path, p: &str) -> PathBuf {
    let p = Path::new(p);
    if p.is_absolute() { p.to_owned() } else { base.join(p) }
}

fn ports_of(infos: &[&SessionInfo]) -> std::collections::HashMap<u32, Vec<u16>> {
    let pids: Vec<u32> = infos.iter().filter(|i| !i.exited).map(|i| i.pid).collect();
    services::listening_ports(&pids)
}

fn service_name(info: &SessionInfo) -> String {
    info.service.as_ref().map_or_else(|| info.id.to_string(), |s| s.name.clone())
}

/// A service as the agent reads it.
fn describe(info: &SessionInfo, ports: &std::collections::HashMap<u32, Vec<u16>>) -> String {
    let Some(sv) = &info.service else { return String::new() };
    let state = match (info.exited, info.exit_code) {
        (false, _) => format!("running (pid {}, up {})", info.pid, uptime(sv.started)),
        (true, Some(code)) => format!("exited with code {code}"),
        (true, None) => "exited".to_owned(),
    };
    let ports = ports
        .get(&info.pid)
        .filter(|p| !p.is_empty())
        .map(|p| p.iter().map(|p| format!("http://localhost:{p}")).collect::<Vec<_>>().join(", "));
    let mut out = format!("{} [{}]: {state}\n  command: {}\n  folder: {}", sv.name, info.id, sv.command, sv.cwd.display());
    if let Some(ports) = ports {
        out.push_str(&format!("\n  listening: {ports}"));
    }
    out
}

fn uptime(started: u64) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let s = now.saturating_sub(started);
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m {}s", s / 60, s % 60),
        _ => format!("{}h {}m", s / 3600, s % 3600 / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        Context { cwd: "/w/app".into(), by: Some(1) }
    }

    #[test]
    fn initialize_answers_the_asked_version() {
        let msg = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } });
        let a = handle(&ctx(), &msg).unwrap();
        assert_eq!(a["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(a["result"]["serverInfo"]["name"], "tern");
        let msg = json!({ "jsonrpc": "2.0", "id": 2, "method": "initialize", "params": { "protocolVersion": "1999-01-01" } });
        assert_eq!(handle(&ctx(), &msg).unwrap()["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
    }

    #[test]
    fn notifications_get_no_answer() {
        assert!(handle(&ctx(), &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).is_none());
    }

    #[test]
    fn tools_are_listed_and_bad_calls_are_tool_errors() {
        let a = handle(&ctx(), &json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" })).unwrap();
        let names: Vec<&str> = a["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["run_service", "list_services", "service_logs", "stop_service", "restart_service"]);
        let call = json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": { "name": "run_service", "arguments": {} } });
        let a = handle(&ctx(), &call).unwrap();
        assert_eq!(a["result"]["isError"], true);
        let a = handle(&ctx(), &json!({ "jsonrpc": "2.0", "id": 5, "method": "nope" })).unwrap();
        assert_eq!(a["error"]["code"], -32601);
    }

    #[test]
    fn describes_a_service() {
        let info = SessionInfo {
            id: 9,
            pid: 42,
            service: Some(ServiceSpec { name: "web".into(), command: "npm run dev".into(), cwd: "/w/app".into(), by: None, started: 0 }),
            exited: true,
            exit_code: Some(1),
            ..Default::default()
        };
        let text = describe(&info, &Default::default());
        assert_eq!(text, "web [9]: exited with code 1\n  command: npm run dev\n  folder: /w/app");
        let running = SessionInfo { exited: false, ..info };
        let ports = [(42, vec![5173])].into_iter().collect();
        assert!(describe(&running, &ports).ends_with("listening: http://localhost:5173"));
    }

    #[test]
    fn folders_relate_either_way() {
        assert!(related(Path::new("/w/app/web"), Path::new("/w/app")));
        assert!(related(Path::new("/w"), Path::new("/w/app")));
        assert!(!related(Path::new("/w/other"), Path::new("/w/app")));
    }
}
