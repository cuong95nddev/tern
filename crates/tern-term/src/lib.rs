//! Terminal core: PTY session, VT state, key encoding, OSC taps. No UI deps.

pub mod env;
pub mod keys;
pub mod mouse;
pub mod osc;
pub mod palette;
pub mod procs;
pub mod session;
pub mod status;

pub use alacritty_terminal as vt;
pub use session::{EventSink, Session, SessionTerm, SpawnConfig, TermEvent, TermSize};
