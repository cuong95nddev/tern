//! Hooks Claude Code runs only as commands (`SessionStart`), run through
//! Tern: `tern --hook <settings file>` hands the hook's input on stdin to the
//! tab's hook endpoint (named in its settings file) and prints the answer
//! for Claude Code.

use std::io::{Read, Write};
use std::path::Path;

/// The command hook for a tab whose settings are at `settings`, run by
/// `exe`. Quoted for the POSIX shell Claude Code runs it in.
pub fn command(exe: &Path, settings: &Path) -> String {
    use crate::statusline::quote;
    format!("{} --hook {}", quote(&exe.to_string_lossy()), quote(&settings.to_string_lossy()))
}

/// `tern --hook <settings>`. Always exits 0: Tern being gone mustn't get in
/// the conversation's way.
pub fn run(settings: &Path) -> i32 {
    let mut input = vec![];
    if std::io::stdin().read_to_end(&mut input).is_err() {
        return 0;
    }
    let Some(url) = std::fs::read(settings).ok().and_then(|s| crate::statusline::hook_url(&s)) else { return 0 };
    if let Ok(answer) = crate::statusline::post(&url, &input)
        && serde_json::from_slice::<serde_json::Value>(&answer).is_ok_and(|v| v.as_object().is_some_and(|o| !o.is_empty()))
    {
        let _ = std::io::stdout().write_all(&answer);
    }
    0
}

#[cfg(test)]
mod tests {
    #[test]
    fn quotes_the_command() {
        let c = super::command(std::path::Path::new("/A pp/tern"), std::path::Path::new("/tmp/s.json"));
        assert_eq!(c, "'/A pp/tern' --hook '/tmp/s.json'");
    }

    #[test]
    fn relays_the_answer() {
        let server = crate::HookServer::start(std::sync::Arc::new(|_| Some(serde_json::json!({"x": 1})))).unwrap();
        let token = server.register();
        let answer = crate::statusline::post(&server.hook_url(&token), br#"{"hook_event_name":"SessionStart"}"#).unwrap();
        assert_eq!(answer, br#"{"x":1}"#);
    }
}
