//! How a chat starts Claude Code headless, and the lines it writes to it.

use std::path::{Path, PathBuf};

/// How Claude asks before it acts (`--permission-mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    /// Asks before edits and commands.
    Default,
    /// Edits files without asking.
    AcceptEdits,
    /// Only plans, changes nothing.
    Plan,
    /// Decides itself what is safe to run.
    Auto,
    /// Never asks (`--dangerously-skip-permissions`).
    Bypass,
}

impl PermissionMode {
    /// The modes the user cycles through (Bypass only when Claude started in it).
    pub const ALL: [Self; 4] = [Self::Default, Self::AcceptEdits, Self::Plan, Self::Auto];

    /// Its name on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::AcceptEdits => "acceptEdits",
            Self::Plan => "plan",
            Self::Auto => "auto",
            Self::Bypass => "bypassPermissions",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().chain([Self::Bypass]).find(|m| m.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "Ask first",
            Self::AcceptEdits => "Accept edits",
            Self::Plan => "Plan",
            Self::Auto => "Auto",
            Self::Bypass => "Bypass",
        }
    }

    /// The one after it, as Shift+Tab cycles them in the terminal.
    pub fn next(self) -> Self {
        match Self::ALL.iter().position(|m| *m == self) {
            Some(i) => Self::ALL[(i + 1) % Self::ALL.len()],
            None => Self::Default,
        }
    }
}

/// What a chat starts Claude with.
#[derive(Debug, Clone)]
pub struct Launch {
    /// The conversation: resumed when it was saved, else started under this id.
    pub session: String,
    pub resume: bool,
    pub model: Option<String>,
    pub mode: Option<PermissionMode>,
    /// `--mcp-config` (JSON).
    pub mcp_config: Option<String>,
}

impl Launch {
    /// Claude's arguments (after the program name).
    pub fn args(&self) -> Vec<String> {
        let mut a: Vec<String> =
            ["--output-format", "stream-json", "--input-format", "stream-json", "--verbose", "--include-partial-messages"]
                .map(str::to_owned)
                .into();
        // Permission requests come to the chat (as `can_use_tool`).
        a.extend(["--permission-prompt-tool".into(), "stdio".into()]);
        // The user's messages come back once received.
        a.push("--replay-user-messages".into());
        if self.resume {
            a.extend(["--resume".into(), self.session.clone()]);
        } else {
            a.extend(["--session-id".into(), self.session.clone()]);
        }
        if let Some(m) = &self.model {
            a.extend(["--model".into(), m.clone()]);
        }
        if let Some(m) = self.mode {
            a.extend(["--permission-mode".into(), m.as_str().into()]);
        }
        if let Some(c) = &self.mcp_config {
            a.extend(["--mcp-config".into(), c.clone()]);
        }
        a
    }
}

/// The lines a chat writes to Claude's stdin (one JSON message each).
pub mod wire {
    use serde_json::{Value, json};

    use super::{PermissionMode, deny_message, new_session_id};
    use crate::transcript::{Answer, Ask, Elicit, ElicitAnswer, success};

    fn control(request: Value) -> String {
        json!({ "type": "control_request", "request_id": new_session_id(), "request": request }).to_string()
    }

    /// The first line: Claude answers with its commands, models and the rest.
    pub fn initialize() -> String {
        control(json!({ "subtype": "initialize", "hooks": null }))
    }

    /// A user message in conversation `session`: `images` (media type,
    /// bytes) then `text`. Gives its id and the line. Sent while a turn runs,
    /// Claude queues it.
    pub fn user(session: &str, text: &str, images: &[(&str, &[u8])]) -> (String, String) {
        use base64::Engine as _;
        let uuid = new_session_id();
        let mut content: Vec<Value> = images
            .iter()
            .map(|(media, bytes)| {
                let data = base64::engine::general_purpose::STANDARD.encode(bytes);
                json!({ "type": "image", "source": { "type": "base64", "media_type": media, "data": data } })
            })
            .collect();
        if !text.is_empty() || content.is_empty() {
            content.push(json!({ "type": "text", "text": text }));
        }
        let line = json!({
            "type": "user",
            "uuid": uuid,
            "session_id": session,
            "parent_tool_use_id": null,
            "message": { "role": "user", "content": content },
        })
        .to_string();
        (uuid, line)
    }

    /// The answer to permission request `ask`.
    pub fn answer(ask: &Ask, answer: &Answer) -> String {
        let id = ask.tool_use_id.clone();
        let response = match answer {
            Answer::Allow => json!({ "behavior": "allow", "updatedInput": ask.input, "toolUseID": id }),
            Answer::AllowSession if ask.suggestions.is_array() => {
                json!({ "behavior": "allow", "updatedInput": ask.input, "updatedPermissions": ask.suggestions, "toolUseID": id })
            },
            Answer::AllowSession => json!({ "behavior": "allow", "updatedInput": ask.input, "toolUseID": id }),
            Answer::Answered(answers) => {
                let mut input = ask.input.clone();
                if let Some(o) = input.as_object_mut() {
                    o.insert("answers".into(), answers.clone());
                }
                json!({ "behavior": "allow", "updatedInput": input, "toolUseID": id })
            },
            Answer::Deny | Answer::Withdrawn => json!({ "behavior": "deny", "message": deny_message(&ask.tool_name), "toolUseID": id }),
        };
        success(&ask.request_id, response).to_string()
    }

    /// The answer to elicitation `e`: accept with `content` (the form's
    /// values), or decline.
    pub fn elicit(e: &Elicit, answer: &ElicitAnswer) -> String {
        let response = match answer {
            ElicitAnswer::Accept(content) => json!({ "action": "accept", "content": content }),
            ElicitAnswer::Decline | ElicitAnswer::Withdrawn => json!({ "action": "decline" }),
        };
        success(&e.request_id, response).to_string()
    }

    /// Stop the running turn.
    pub fn interrupt() -> String {
        control(json!({ "subtype": "interrupt" }))
    }

    pub fn set_permission_mode(mode: PermissionMode) -> String {
        control(json!({ "subtype": "set_permission_mode", "mode": mode.as_str() }))
    }

    pub fn set_model(model: &str) -> String {
        control(json!({ "subtype": "set_model", "model": model }))
    }
}

/// What Claude is told when the user says no.
fn deny_message(tool: &str) -> &'static str {
    match tool {
        "AskUserQuestion" => "The user dismissed these questions without answering. End your turn and wait for them.",
        "ExitPlanMode" => "The user asked you to keep planning. Revise the plan and call ExitPlanMode again.",
        _ => "The user denied this action.",
    }
}

/// A random UUID (v4): a new conversation's id, a message's.
pub fn new_session_id() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("random bytes");
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

/// Where Claude saved conversation `id` started in `cwd`, under its config
/// folder `config` (`~/.claude`), if it did.
pub fn transcript_path(config: &Path, cwd: &Path, id: &str) -> Option<PathBuf> {
    let file = format!("{id}.jsonl");
    let projects = config.join("projects");
    // Its folder is the path with every other character turned into `-`.
    let slug: String = cwd.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let direct = projects.join(slug).join(&file);
    if direct.is_file() {
        return Some(direct);
    }
    // Long paths are cut and hashed: look in every folder.
    std::fs::read_dir(&projects).ok()?.flatten().map(|e| e.path().join(&file)).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_messages_carry_images_before_text() {
        let (uuid, line) = wire::user("s", "look", &[("image/png", b"\x89PNG")]);
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["uuid"], uuid.as_str());
        let content = v["message"]["content"].as_array().unwrap();
        assert_eq!(content[0]["source"], serde_json::json!({"type":"base64","media_type":"image/png","data":"iVBORw=="}));
        assert_eq!(content[1], serde_json::json!({"type":"text","text":"look"}));
        let (_, line) = wire::user("s", "", &[("image/png", b"x")]);
        assert_eq!(serde_json::from_str::<serde_json::Value>(&line).unwrap()["message"]["content"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn ids_are_uuids() {
        let id = new_session_id();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert_ne!(id, new_session_id());
    }

    #[test]
    fn args_resume_or_start() {
        let mut l = Launch { session: "s".into(), resume: false, model: None, mode: Some(PermissionMode::Plan), mcp_config: None };
        let a = l.args();
        assert!(a.windows(2).any(|w| w == ["--session-id", "s"]));
        assert!(a.windows(2).any(|w| w == ["--permission-mode", "plan"]));
        l.resume = true;
        assert!(l.args().windows(2).any(|w| w == ["--resume", "s"]));
    }

    #[test]
    fn finds_a_saved_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("projects").join("-a-b-c");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("x.jsonl"), "").unwrap();
        assert_eq!(transcript_path(dir.path(), Path::new("/a/b.c"), "x"), Some(folder.join("x.jsonl")));
        assert_eq!(transcript_path(dir.path(), Path::new("/a/b.c"), "y"), None);
    }

    #[test]
    fn modes_cycle() {
        assert_eq!(PermissionMode::Auto.next(), PermissionMode::Default);
        assert_eq!(PermissionMode::parse("acceptEdits"), Some(PermissionMode::AcceptEdits));
        assert_eq!(PermissionMode::parse("bypassPermissions"), Some(PermissionMode::Bypass));
        assert_eq!(PermissionMode::Bypass.next(), PermissionMode::Default);
    }
}
