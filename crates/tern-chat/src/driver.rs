//! How a chat talks to its agent: one driver per agent's headless protocol,
//! folding what the agent writes into the neutral [`Transcript`] and making
//! the lines the chat writes back. [`chat_driver`] is the table.

use std::io;
use std::path::Path;

use serde_json::Value;

use crate::claude::ClaudeDriver;
use crate::model::{Answer, Ask, Elicit, ElicitAnswer, PermissionMode, Transcript};

/// What a driver's agent can do in a chat; the chat shows a control only
/// when its agent has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChatCaps {
    /// Its permission mode can be switched.
    pub permission_modes: bool,
    /// Its model can be switched.
    pub models: bool,
    /// MCP servers can ask the user for input through it.
    pub elicitation: bool,
    /// User messages can carry images.
    pub images: bool,
    /// A running turn can be stopped.
    pub interrupt: bool,
}

/// One agent's chat protocol.
pub trait ChatDriver: Send {
    /// The agent's registry id.
    fn agent(&self) -> &'static str;

    fn caps(&self) -> ChatCaps;

    /// The shell line the daemon runs: `user_args` as the user typed them,
    /// conversation `session` (resumed or started), each argument quoted
    /// with `quote` for the shell that runs it.
    fn command(
        &self,
        user_args: &str,
        session: &str,
        resume: bool,
        model: Option<&str>,
        mode: Option<PermissionMode>,
        quote: fn(&str) -> String,
    ) -> String;

    /// The lines written before anything the user sends.
    fn start_lines(&self) -> Vec<String>;

    /// Fold one line the agent wrote into `t`; a reply line the chat must
    /// write back, if any.
    fn fold_line(&mut self, t: &mut Transcript, line: &str) -> Option<String>;

    /// A user message in conversation `session`: `images` (media type,
    /// bytes) then `text`. Gives its id and the line.
    fn user_line(&mut self, session: &str, text: &str, images: &[(&str, &[u8])]) -> (String, String);

    fn answer_line(&self, ask: &Ask, answer: &Answer) -> Option<String>;

    fn elicit_line(&self, e: &Elicit, answer: &ElicitAnswer) -> Option<String>;

    fn interrupt_line(&self) -> Option<String>;

    fn mode_line(&self, mode: PermissionMode) -> Option<String>;

    fn model_line(&self, model: &str) -> Option<String>;

    /// Replace `t` with the conversation saved at `path`, up to the lines
    /// saved at or after `before` (ms since the Unix epoch), with `extra`
    /// lines (ms, frame) the agent doesn't save, each where its time puts it.
    fn load(&mut self, t: &mut Transcript, path: &Path, before: Option<u64>, extra: Vec<(u64, Value)>) -> io::Result<()>;

    /// The agent's process ended: nothing streams or waits any more.
    fn ended(&mut self, t: &mut Transcript, why: Option<String>);
}

/// The chat driver of agent `agent`, if it has one.
pub fn chat_driver(agent: &str) -> Option<Box<dyn ChatDriver>> {
    match agent {
        tern_agent::CLAUDE => Some(Box::new(ClaudeDriver::default())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_has_no_chat_driver_yet() {
        assert!(chat_driver("codex").is_none());
    }

    #[test]
    fn claude_driver_caps_are_all_on() {
        let d = chat_driver("claude").unwrap();
        assert_eq!(d.agent(), "claude");
        assert_eq!(d.caps(), ChatCaps { permission_modes: true, models: true, elicitation: true, images: true, interrupt: true });
    }

    #[test]
    fn a_sent_message_is_not_shown_twice_when_replayed() {
        let mut d = chat_driver("claude").unwrap();
        let mut t = Transcript::default();
        let (uuid, _) = d.user_line("s", "hi", &[]);
        t.push_user("hi", Vec::new());
        let replay = serde_json::json!({"type":"user","uuid":uuid,"message":{"role":"user","content":[{"type":"text","text":"hi"}]}});
        d.fold_line(&mut t, &replay.to_string());
        assert_eq!(t.entries.len(), 1, "{:?}", t.entries);
    }
}
