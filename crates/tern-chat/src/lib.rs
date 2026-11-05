//! An agent's conversation shown as a chat instead of a terminal.
//!
//! The chat model ([`Transcript`] and its entries) is neutral: it is what a
//! chat draws, whatever agent runs. Each agent with a headless protocol has
//! a [`ChatDriver`] ([`chat_driver`] is the table) that folds the lines the
//! agent writes into the transcript and makes the lines the chat writes
//! back: the user's messages and answers. Tern's daemon runs the process
//! (`tern_mux::chat`), so a chat goes on while Tern is closed.
//!
//! Claude Code is the first driver ([`claude`]): the CLI run headless
//! (`--input-format stream-json --output-format stream-json`), with the
//! same tools, settings, hooks, MCP servers and login as in a terminal. The
//! same folding reads a saved conversation (`~/.claude/projects/…/<id>.jsonl`)
//! back, so a resumed chat shows its history.
//!
//! Nothing here draws: [`summary`] words tool calls and groups of them the
//! way a timeline shows them, for any front end.

pub mod claude;
pub mod codex;
mod driver;
mod model;
mod recent;
pub mod sessions;
pub mod summary;

pub use claude::{Launch, new_session_id, read_record, wire};
pub use driver::{ChatCaps, ChatDriver, chat_driver};
pub use model::{Answer, Ask, Elicit, ElicitAnswer, Entry, Field, FieldKind, Model, PermissionMode, Tool, Transcript, TurnEnd, UserImage};
pub use recent::{Recent, recent};
