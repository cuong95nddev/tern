//! A Claude Code conversation shown as a chat instead of a terminal.
//!
//! Claude Code runs headless (`--input-format stream-json --output-format
//! stream-json`): the same CLI, tools, settings, hooks, MCP servers and
//! login as in a terminal, only its TUI is off. Each line it writes is a JSON
//! frame (a message, a streamed delta, a permission request, the end of a
//! turn); [`Transcript`] folds them into the entries a chat draws, and
//! [`wire`] makes the lines the chat writes back: the user's messages and
//! answers. Tern's daemon runs the process (`tern_mux::chat`), so a chat
//! goes on while Tern is closed.
//! The same folding reads a saved conversation (`~/.claude/projects/…/<id>.jsonl`)
//! back, so a resumed chat shows its history.
//!
//! Nothing here draws: [`summary`] words tool calls and groups of them the
//! way a timeline shows them, for any front end.

mod launch;
mod recent;
pub mod summary;
mod transcript;

pub use launch::{Launch, PermissionMode, new_session_id, transcript_path, wire};
pub use recent::{Recent, read_recent, recent};
pub use transcript::{
    Answer, Ask, Elicit, ElicitAnswer, Entry, Field, FieldKind, Model, Tool, Transcript, TurnEnd, UserImage, read_record,
};
