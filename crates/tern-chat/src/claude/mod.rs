//! Claude Code's chat driver: the CLI run headless with stream-json on
//! stdin and stdout.

pub(crate) mod fold;
pub mod launch;

use std::io;
use std::path::Path;

use serde_json::Value;

pub use fold::{ClaudeFold, read_record};
pub use launch::{Launch, claude_line, new_session_id, transcript_path, wire};

use crate::driver::{ChatCaps, ChatDriver};
use crate::model::{Answer, Ask, Elicit, ElicitAnswer, PermissionMode, Transcript};

/// Claude Code headless: its frames folded by [`ClaudeFold`], its input
/// written by [`wire`].
#[derive(Debug, Default)]
pub struct ClaudeDriver {
    fold: ClaudeFold,
}

impl ChatDriver for ClaudeDriver {
    fn agent(&self) -> &'static str {
        "claude"
    }

    fn caps(&self) -> ChatCaps {
        ChatCaps { permission_modes: true, models: true, elicitation: true, images: true, interrupt: true }
    }

    fn command(
        &self,
        user_args: &str,
        session: &str,
        resume: bool,
        model: Option<&str>,
        mode: Option<PermissionMode>,
        quote: fn(&str) -> String,
    ) -> String {
        let launch = Launch { session: session.to_owned(), resume, model: model.map(str::to_owned), mode };
        claude_line(user_args, &launch.args(), quote)
    }

    fn start_lines(&self) -> Vec<String> {
        vec![wire::initialize()]
    }

    fn fold_line(&mut self, t: &mut Transcript, line: &str) -> Option<String> {
        self.fold.apply_line(t, line).map(|v| v.to_string())
    }

    fn user_line(&mut self, session: &str, text: &str, images: &[(&str, &[u8])]) -> (String, String) {
        let (uuid, line) = wire::user(session, text, images);
        // Its replay is the message already shown.
        self.fold.sent.insert(uuid.clone());
        (uuid, line)
    }

    fn answer_line(&self, ask: &Ask, answer: &Answer) -> Option<String> {
        Some(wire::answer(ask, answer))
    }

    fn elicit_line(&self, e: &Elicit, answer: &ElicitAnswer) -> Option<String> {
        Some(wire::elicit(e, answer))
    }

    fn interrupt_line(&self) -> Option<String> {
        Some(wire::interrupt())
    }

    fn mode_line(&self, mode: PermissionMode) -> Option<String> {
        Some(wire::set_permission_mode(mode))
    }

    fn model_line(&self, model: &str) -> Option<String> {
        Some(wire::set_model(model))
    }

    fn load(&mut self, t: &mut Transcript, path: &Path, before: Option<u64>, extra: Vec<(u64, Value)>) -> io::Result<()> {
        let mut fold = ClaudeFold::default();
        *t = fold.load(path, before, extra)?;
        self.fold = fold;
        Ok(())
    }

    fn ended(&mut self, t: &mut Transcript, why: Option<String>) {
        self.fold.settle(t);
        t.ended(why);
    }
}
