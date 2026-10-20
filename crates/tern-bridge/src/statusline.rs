//! Claude Code's status line, run through Tern. A tab's `--settings` point
//! `statusLine` at `tern --statusline <settings file>`: its input (the
//! session's model, how full its context window is) goes to the tab's hook
//! endpoint as a `StatusLine` event, so the agents panel can show it; then
//! the user's own status line, if they set one, draws as it would without
//! Tern (the `--settings` one takes its place).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;

/// The event name the hook endpoint sees for a status line's input.
pub const EVENT: &str = "StatusLine";

/// Whether hook body `body` is a status line's input.
pub fn is_event(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body).is_ok_and(|v| v.get("hook_event_name").and_then(Value::as_str) == Some(EVENT))
}

/// The `statusLine` command for a tab whose settings are at `settings`, run
/// by `exe`. Quoted for the POSIX shell Claude Code runs it in (Git Bash on
/// Windows).
pub fn command(exe: &Path, settings: &Path) -> String {
    format!("{} --statusline {}", quote(&exe.to_string_lossy()), quote(&settings.to_string_lossy()))
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `tern --statusline <settings>`: hand the input on stdin to the tab's hook
/// endpoint (named in its settings file), then run the user's own status
/// line on it. Returns the exit code.
pub fn run(settings: &Path) -> i32 {
    let mut input = vec![];
    if std::io::stdin().read_to_end(&mut input).is_err() {
        return 1;
    }
    let Ok(mut line) = serde_json::from_slice::<Value>(&input) else { return 0 };
    if let Some(url) = std::fs::read(settings).ok().and_then(|s| hook_url(&s))
        && let Some(o) = line.as_object_mut()
    {
        o.insert("hook_event_name".into(), EVENT.into());
        let _ = post(&url, &serde_json::to_vec(&line).unwrap_or_default());
    }
    let project = line.pointer("/workspace/project_dir").or_else(|| line.get("cwd")).and_then(Value::as_str).map(PathBuf::from);
    match user_command(project.as_deref()) {
        Some(cmd) => run_user(&cmd, &input),
        None => 0,
    }
}

/// The hook URL in a tab's settings JSON.
fn hook_url(settings: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(settings).ok()?;
    v.pointer("/hooks/Stop/0/hooks/0/url")?.as_str().map(str::to_owned)
}

/// POST `body` to `url` (`http://127.0.0.1:<port>/<path>`), waiting for the
/// answer so the request isn't cut off.
fn post(url: &str, body: &[u8]) -> std::io::Result<()> {
    let rest = url.strip_prefix("http://").ok_or(std::io::ErrorKind::InvalidInput)?;
    let (host, path) = rest.split_once('/').ok_or(std::io::ErrorKind::InvalidInput)?;
    let addr = host.parse().map_err(|_| std::io::ErrorKind::InvalidInput)?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(1))?;
    s.set_read_timeout(Some(Duration::from_secs(2)))?;
    let head = format!(
        "POST /{path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes())?;
    s.write_all(body)?;
    std::io::copy(&mut s, &mut std::io::sink())?;
    Ok(())
}

/// The status line command the user set, as Claude Code would pick it
/// without Tern's: the project's local settings, then its shared ones, then
/// the user's (in `CLAUDE_CONFIG_DIR`, else `~/.claude`).
fn user_command(project: Option<&Path>) -> Option<String> {
    let config = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(|h| PathBuf::from(h).join(".claude")));
    let project = project.map(|p| p.join(".claude"));
    let files = [
        project.as_ref().map(|p| p.join("settings.local.json")),
        project.as_ref().map(|p| p.join("settings.json")),
        config.map(|c| c.join("settings.json")),
    ];
    files.into_iter().flatten().find_map(|f| command_in(&std::fs::read(f).ok()?))
}

/// The `statusLine` command in settings JSON `s`.
fn command_in(s: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(s).ok()?;
    let line = v.get("statusLine")?;
    if line.get("type").and_then(Value::as_str).is_some_and(|t| t != "command") {
        return None;
    }
    line.get("command")?.as_str().filter(|c| !c.trim().is_empty()).map(str::to_owned)
}

/// Run `cmd` in a shell with `input` on its stdin, its output Claude Code's.
fn run_user(cmd: &str, input: &[u8]) -> i32 {
    let shell = if cfg!(windows) { "bash" } else { "sh" };
    let Ok(mut child) = Command::new(shell).arg("-c").arg(cmd).stdin(Stdio::piped()).spawn() else { return 1 };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input);
    }
    child.wait().ok().and_then(|s| s.code()).unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_paths() {
        let c = command(Path::new("/Applications/Tern.app/Contents/MacOS/tern"), Path::new("/tmp/it's/s.json"));
        assert_eq!(c, r"'/Applications/Tern.app/Contents/MacOS/tern' --statusline '/tmp/it'\''s/s.json'");
    }

    #[test]
    fn reads_the_hook_url() {
        let s = crate::claude_settings_json("http://127.0.0.1:9/hook/t", None);
        assert_eq!(hook_url(s.as_bytes()).as_deref(), Some("http://127.0.0.1:9/hook/t"));
    }

    #[test]
    fn user_command_only_for_commands() {
        assert_eq!(command_in(br#"{"statusLine":{"type":"command","command":"ccline"}}"#).as_deref(), Some("ccline"));
        assert_eq!(command_in(br#"{"statusLine":{"type":"other","command":"x"}}"#), None);
        assert_eq!(command_in(br#"{"statusLine":{"type":"command","command":" "}}"#), None);
        assert_eq!(command_in(br#"{"model":"opus"}"#), None);
    }

    #[test]
    fn posts_to_the_hook_server() {
        let (tx, rx) = std::sync::mpsc::channel();
        let server = crate::HookServer::start(std::sync::Arc::new(move |ev| tx.send(ev).unwrap())).unwrap();
        let token = server.register();
        post(&server.hook_url(&token), br#"{"hook_event_name":"StatusLine","session_id":"s"}"#).unwrap();
        let ev = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!((ev.event.as_str(), ev.session_id.as_deref()), (EVENT, Some("s")));
    }
}
