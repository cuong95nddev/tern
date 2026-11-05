//! How a chat talks to its agent: one driver per agent's headless protocol,
//! folding what the agent writes into the neutral [`Transcript`] and making
//! the lines the chat writes back. [`chat_driver`] is the table.

use std::io;
use std::path::Path;

use serde_json::Value;

use crate::claude::{ClaudeDriver, ClaudeFollow};
use crate::codex::app_server::{CodexDriver, CodexFollow};
use crate::model::{Answer, Ask, Elicit, ElicitAnswer, PermissionMode, Transcript};
use crate::opencode::acp::{OpenCodeDriver, OpenCodeFollow};

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

    /// The lines written before anything the user sends, in conversation
    /// `session`: resumed with `resume`, else it may get another id. It runs
    /// in folder `cwd`.
    fn start_lines(&mut self, session: &str, resume: bool, cwd: &Path) -> Vec<String>;

    /// Fold one line the agent wrote into `t`; a reply line the chat must
    /// write back, if any.
    fn fold_line(&mut self, t: &mut Transcript, line: &str) -> Option<String>;

    /// A user message in conversation `session`: `images` (media type,
    /// bytes) then `text`. Gives its id and the line; an empty line has
    /// nothing to write yet, it goes in by [`Self::fold_line`]'s reply.
    fn user_line(&mut self, session: &str, text: &str, images: &[(&str, &[u8])]) -> (String, String);

    /// The line that goes on in a fresh conversation in place of `session`,
    /// in folder `cwd`.
    fn clear_line(&mut self, session: &str, cwd: &Path) -> String;

    /// `session` is a saved conversation, without a file of Tern's to tell:
    /// an agent that keeps them where Tern doesn't read.
    fn resumes(&self, _session: &str) -> bool {
        false
    }

    fn answer_line(&self, ask: &Ask, answer: &Answer) -> Option<String>;

    fn elicit_line(&self, e: &Elicit, answer: &ElicitAnswer) -> Option<String>;

    fn interrupt_line(&self) -> Option<String>;

    fn mode_line(&self, mode: PermissionMode) -> Option<String>;

    fn model_line(&self, model: &str) -> Option<String>;

    /// Forget what was folded so far: the transcript it folded into starts over.
    fn reset(&mut self);

    /// Replace `t` with the conversation saved at `path`, up to the lines
    /// saved at or after `before` (ms since the Unix epoch), with `extra`
    /// lines (ms, frame) the agent doesn't save, each where its time puts it.
    fn load(&mut self, t: &mut Transcript, path: &Path, before: Option<u64>, extra: Vec<(u64, Value)>) -> io::Result<()>;

    /// The agent's process ended: nothing streams or waits any more.
    fn ended(&mut self, t: &mut Transcript, why: Option<String>);
}

/// What a chat line, read from the agent or written to it, says.
#[derive(Debug, Clone, PartialEq)]
pub enum Said {
    TurnStarted,
    TurnEnded,
    /// The conversation is now this one.
    Session(String),
    /// A request waits for the user, by id.
    Ask(String),
    /// The request with this id doesn't wait any more.
    Settled(String),
    /// An MCP form came, by id: its lines are recorded.
    Form(String),
    /// A line that settles the form with this id, if there is one.
    FormSettled(String),
    /// How full the context window is.
    Reading(tern_bridge::context::Reading),
    /// The line is a piece of a message that comes whole once its turn ends.
    Streamed,
    /// What a notification about the conversation is titled.
    Title(String),
}

/// How the daemon follows a chat without reading the conversation: one per
/// agent's protocol, as [`ChatDriver`] is for the UI.
pub trait ChatFollow: Send {
    /// What `line`, one the agent wrote, says.
    fn follow(&mut self, line: &Value, out: &mut Vec<Said>);

    /// What `line`, one written to the agent, says: an agent whose answers
    /// look like the ones written to it tells them apart here.
    fn wrote(&mut self, line: &Value, out: &mut Vec<Said>) {
        self.follow(line, out);
    }

    /// What `line`, one the agent wrote, tells the user while no UI is
    /// connected; `agent` names it.
    fn notice(&self, line: &Value, agent: &str) -> Option<String>;
}

/// The chat follower of agent `agent`, if it has a chat.
pub fn chat_follower(agent: &str) -> Option<Box<dyn ChatFollow>> {
    match agent {
        tern_agent::CLAUDE => Some(Box::<ClaudeFollow>::default()),
        tern_agent::CODEX => Some(Box::<CodexFollow>::default()),
        tern_agent::OPENCODE => Some(Box::<OpenCodeFollow>::default()),
        _ => None,
    }
}

/// The chat driver of agent `agent`, if it has one.
pub fn chat_driver(agent: &str) -> Option<Box<dyn ChatDriver>> {
    match agent {
        tern_agent::CLAUDE => Some(Box::new(ClaudeDriver::default())),
        tern_agent::CODEX => Some(Box::new(CodexDriver::default())),
        tern_agent::OPENCODE => Some(Box::new(OpenCodeDriver::default())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_has_a_chat_driver() {
        let d = chat_driver("codex").unwrap();
        assert_eq!(d.agent(), "codex");
        assert_eq!(d.caps(), ChatCaps { permission_modes: false, models: false, elicitation: true, images: false, interrupt: true });
    }

    #[test]
    fn an_agent_without_a_chat_has_no_driver() {
        assert!(chat_driver("gemini").is_none());
        assert!(chat_follower("gemini").is_none());
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

    #[test]
    fn a_reset_driver_forgets_what_it_sent() {
        let mut d = chat_driver("claude").unwrap();
        let (uuid, _) = d.user_line("s", "hi", &[]);
        d.reset();
        let mut t = Transcript::default();
        let replay = serde_json::json!({"type":"user","uuid":uuid,"message":{"role":"user","content":[{"type":"text","text":"hi"}]}});
        d.fold_line(&mut t, &replay.to_string());
        assert!(matches!(&t.entries[..], [crate::Entry::User { text, .. }] if text == "hi"), "{:?}", t.entries);
    }
}
