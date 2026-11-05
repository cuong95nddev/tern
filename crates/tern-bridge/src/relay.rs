//! Hooks Claude Code runs only as commands (`SessionStart`), run through
//! Tern: `tern --hook <settings file>` hands the hook's input on stdin to the
//! tab's hook endpoint (named in its settings file) and prints the answer
//! for Claude Code, with what the agent should know of Tern's commands.

use std::io::{Read, Write};
use std::path::Path;

use serde_json::{Value, json};

/// The command hook for a tab whose settings are at `settings`, run by
/// `exe`. Quoted for the POSIX shell Claude Code runs it in.
pub fn command(exe: &Path, settings: &Path) -> String {
    use crate::statusline::quote;
    format!("{} --hook {}", quote(&exe.to_string_lossy()), quote(&settings.to_string_lossy()))
}

/// `tern --hook <settings>`, with `guide` for the conversation. Always exits
/// 0: Tern being gone mustn't get in the conversation's way.
pub fn run(settings: &Path, guide: &str) -> i32 {
    let mut input = vec![];
    if std::io::stdin().read_to_end(&mut input).is_err() {
        return 0;
    }
    let Some(url) = std::fs::read(settings).ok().and_then(|s| crate::statusline::hook_url(&s)) else { return 0 };
    let answer = crate::statusline::post(&url, &input).ok().and_then(|a| serde_json::from_slice::<Value>(&a).ok());
    let _ = std::io::stdout().write_all(with_guide(answer, guide).to_string().as_bytes());
    0
}

/// The tab's `answer` with `guide` added to the context it gives Claude.
fn with_guide(answer: Option<Value>, guide: &str) -> Value {
    let mut answer = answer.filter(Value::is_object).unwrap_or_else(|| json!({}));
    let context = match answer.pointer("/hookSpecificOutput/additionalContext").and_then(Value::as_str) {
        Some(context) => format!("{context}\n\n{guide}"),
        None => guide.to_owned(),
    };
    answer["hookSpecificOutput"] = json!({ "hookEventName": "SessionStart", "additionalContext": context });
    answer
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn adds_the_guide_to_the_answer() {
        let context = |v: serde_json::Value| v["hookSpecificOutput"]["additionalContext"].as_str().unwrap().to_owned();
        for nothing in [None, Some(json!({})), Some(json!(1))] {
            let v = super::with_guide(nothing, "guide");
            assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
            assert_eq!(context(v), "guide");
        }
        let handoff = json!({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": "go on"}, "x": 1});
        let v = super::with_guide(Some(handoff), "guide");
        assert_eq!(v["x"], 1);
        assert_eq!(context(v), "go on\n\nguide");
    }

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
