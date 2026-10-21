//! Terminal core: PTY session, VT state, key encoding, OSC and graphics taps. No UI deps.

pub mod env;
pub mod graphics;
pub mod keys;
pub mod mirror;
pub mod mouse;
pub mod osc;
pub mod palette;
pub mod procs;
pub mod replay;
pub mod session;
pub mod snapshot;
pub mod status;

pub use alacritty_terminal as vt;

/// Whether programs may turn on the Kitty keyboard protocol. Not on
/// Windows, as in Warp: ConPTY turns input into console key events and
/// drops the protocol's key sequences and query replies, so a program in
/// it (WSL's included) that asked for it gets no keys, or waits forever.
pub const KITTY_KEYBOARD: bool = cfg!(not(windows));
pub use session::{EventSink, Session, SessionTerm, SpawnConfig, TermEvent, TermSize};
