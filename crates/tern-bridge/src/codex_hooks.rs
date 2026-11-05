//! Tern's hooks in a Codex tab. Codex runs hooks only as commands, so each
//! goes through `tern --hook` (see [`crate::relay`]) to the tab's endpoint.
//! They come in by `-c` for this one launch, with the trust Codex wants of
//! them: nothing is written to the user's `~/.codex`.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::json;
use sha2::{Digest, Sha256};

use crate::version::Version;

/// The events a Codex tab's hooks report: the name Codex's config has for
/// each, and the label its trust keys and hashes have.
pub const CODEX_EVENTS: [(&str, &str); 9] = [
    ("SessionStart", "session_start"),
    ("UserPromptSubmit", "user_prompt_submit"),
    ("PreToolUse", "pre_tool_use"),
    ("PermissionRequest", "permission_request"),
    ("PostToolUse", "post_tool_use"),
    ("SubagentStart", "subagent_start"),
    ("SubagentStop", "subagent_stop"),
    ("Stop", "stop"),
    ("Interrupt", "interrupt"),
];

/// The first Codex Tern's hooks were checked on; an older one, or one whose
/// version can't be told, runs without them.
const FIRST_WIRED: Version = (0, 166, 0);

/// Whether a Codex of `version` gets Tern's hooks.
pub fn codex_wired(version: Option<Version>) -> bool {
    version.is_some_and(|v| v >= FIRST_WIRED)
}

/// Hooks passed by `-c` have this for their source in a trust key.
const SESSION_FLAGS_SOURCE: &str = "/<session-flags>/config.toml";

/// Seconds Codex gives hook `event`. Always written out: the hash Codex
/// checks has the value it ends up with, which differs by event by default
/// (`Interrupt` allows 3 at most).
pub fn codex_hook_timeout(event: &str) -> u64 {
    if event == "Interrupt" { 3 } else { 10 }
}

/// The hash Codex wants in a hook's `trusted_hash` to run it: of the hook's
/// JSON, its keys sorted, no spaces (`hook_hash` in Codex's `discovery.rs`).
pub fn codex_trusted_hash(label: &str, command: &str, timeout: u64) -> String {
    // Written in sorted order: `Value` keeps it with `preserve_order`, and
    // sorts to the same without.
    let identity = json!({
        "event_name": label,
        "hooks": [{"async": false, "command": command, "timeout": timeout, "type": "command"}],
    });
    let digest = Sha256::digest(identity.to_string().as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

/// `s` as a TOML basic string.
fn toml_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The value of Codex's `hooks` config: `command` for every event in
/// [`CODEX_EVENTS`], each trusted.
pub fn codex_hooks_arg(command: &str) -> String {
    let quoted = toml_str(command);
    let mut parts = Vec::with_capacity(CODEX_EVENTS.len() + 1);
    let mut state = Vec::with_capacity(CODEX_EVENTS.len());
    for (event, label) in CODEX_EVENTS {
        let timeout = codex_hook_timeout(event);
        parts.push(format!(r#"{event}=[{{hooks=[{{type="command",command={quoted},timeout={timeout}}}]}}]"#));
        let key = toml_str(&format!("{SESSION_FLAGS_SOURCE}:{label}:0:0"));
        state.push(format!(r#"{key}={{trusted_hash="{}"}}"#, codex_trusted_hash(label, command, timeout)));
    }
    parts.push(format!("state={{{}}}", state.join(",")));
    format!("{{{}}}", parts.join(","))
}

/// Write the tab's hook endpoint to a private file and return its path: `ps`
/// shows Codex's arguments, so they name this file (itself named for the
/// token's hash) and not the endpoint's token.
pub fn write_codex_hook_file(dir: &Path, token: &str, hook_url: &str) -> io::Result<PathBuf> {
    let path = crate::hooks::token_file(dir, "codex-hooks", token);
    let mut f = crate::hooks::create_private(dir, &path)?;
    f.write_all(json!({ "hook_url": hook_url }).to_string().as_bytes())?;
    Ok(path)
}

pub fn remove_codex_hook_file(dir: &Path, token: &str) {
    let _ = std::fs::remove_file(crate::hooks::token_file(dir, "codex-hooks", token));
}

/// The command a hook of a tab with hook file `file` runs. Quoted for the
/// shell Codex runs hooks in: `cmd` on Windows, a POSIX shell elsewhere.
fn relay_command(exe: &Path, file: &Path) -> String {
    if cfg!(windows) { format!("\"{}\" --hook \"{}\"", exe.display(), file.display()) } else { crate::relay::command(exe, file) }
}

/// What a Codex tab's command line gets for Tern's hooks: `exe` is Tern's own
/// binary, `file` the tab's hook file, `quote` quotes for the shell that
/// runs the line.
pub fn codex_launch_args(exe: &Path, file: &Path, quote: fn(&str) -> String) -> String {
    // `-c` keeps it off the shared background server anyway, whose hooks
    // would run in another tab's env: saying so spares a startup warning.
    format!("--no-daemon -c {}", quote(&format!("hooks={}", codex_hooks_arg(&relay_command(exe, file)))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_known_new_enough_codex_is_wired() {
        assert!(codex_wired(Some((0, 166, 0))));
        assert!(codex_wired(Some((1, 0, 0))));
        assert!(!codex_wired(Some((0, 165, 9))));
        assert!(!codex_wired(None));
    }

    #[test]
    fn trusted_hash_matches_codex_0_166() {
        let cmd = "/private/tmp/claude-501/-Users-cuongpham-ws-tern-ws-tern/5f2e9412-232e-40cd-a0a5-dded8c428e40/scratchpad/spike/hook.sh PreToolUse";
        assert_eq!(codex_trusted_hash("pre_tool_use", cmd, 600), "sha256:c62946eb7aa1c4048196c116ff1952c564e533113073e686049504fdd3ec1efc");
    }

    #[test]
    fn hooks_arg_names_every_event_with_its_trust() {
        let command = r#"'/A "p/tern' --hook '/r/f.json'"#;
        let v: toml::Value = toml::from_str(&format!("hooks={}", codex_hooks_arg(command))).unwrap();
        for (event, label) in CODEX_EVENTS {
            let h = &v["hooks"][event][0]["hooks"][0];
            assert_eq!(h["command"].as_str(), Some(command));
            let timeout = h["timeout"].as_integer().unwrap() as u64;
            let key = format!("/<session-flags>/config.toml:{label}:0:0");
            assert_eq!(v["hooks"]["state"][&key]["trusted_hash"].as_str().unwrap(), codex_trusted_hash(label, command, timeout));
        }
        assert_eq!(v["hooks"]["Interrupt"][0]["hooks"][0]["timeout"].as_integer(), Some(3));
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["timeout"].as_integer(), Some(10));
    }

    #[test]
    fn hook_file_is_read_back_and_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path().join("run");
        let path = write_codex_hook_file(&run, "tok", "http://127.0.0.1:1/hook/tok").unwrap();
        let text = std::fs::read(&path).unwrap();
        assert_eq!(crate::statusline::hook_url(&text).as_deref(), Some("http://127.0.0.1:1/hook/tok"));
        assert!(write_codex_hook_file(&run, "tok", "x").is_err());
        remove_codex_hook_file(&run, "tok");
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn launch_args_show_no_token() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path().join("run");
        let token = "0123456789abcdef";
        let file = write_codex_hook_file(&run, token, "http://127.0.0.1:1/hook/x").unwrap();
        let args = codex_launch_args(std::path::Path::new("/A p/tern"), &file, crate::statusline::quote);
        assert!(args.starts_with("--no-daemon -c 'hooks={"), "{args}");
        assert!(args.contains("--hook"), "{args}");
        assert!(!args.contains(token), "{args}");
    }

    /// Needs Codex on the PATH: it must trust what Tern passes it.
    #[test]
    #[ignore]
    fn codex_trusts_tern_hooks() {
        use std::io::{BufRead, BufReader, Write};
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "").unwrap();
        let arg = format!("hooks={}", codex_hooks_arg("'/A p/tern' --hook '/r/f.json'"));
        let mut child = std::process::Command::new("codex")
            .args(["-c", &arg, "app-server"])
            .env("CODEX_HOME", home.path())
            .current_dir(home.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let cwd = home.path().to_string_lossy().into_owned();
        for m in [
            serde_json::json!({"id": 0, "method": "initialize", "params": {"clientInfo": {"name": "tern", "version": "0"}}}),
            serde_json::json!({"method": "initialized"}),
            serde_json::json!({"id": 1, "method": "hooks/list", "params": {"cwds": [cwd]}}),
        ] {
            writeln!(stdin, "{m}").unwrap();
        }
        let reply = lines.find_map(|l| serde_json::from_str::<serde_json::Value>(&l.unwrap()).ok().filter(|v| v["id"] == 1)).unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        let hooks = reply.pointer("/result/data/0/hooks").and_then(serde_json::Value::as_array).unwrap();
        let ours: Vec<_> = hooks.iter().filter(|h| h["sourcePath"].as_str().is_some_and(|p| p.contains("<session-flags>"))).collect();
        assert_eq!(ours.len(), CODEX_EVENTS.len(), "{hooks:?}");
        for h in ours {
            assert_eq!(h["trustStatus"], "trusted", "{h}");
        }
    }
}
