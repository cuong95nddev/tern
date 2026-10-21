//! Kitty graphics protocol, Unicode-placeholder subset
//! (https://sw.kovidgoyal.net/kitty/graphics-protocol/).
//!
//! [`Graphics`] taps the raw PTY output for `ESC _ G <control> ; <payload> ESC \`
//! commands, keeps transmitted images in an [`ImageStore`] and produces the
//! replies. Programs show an image by printing U+10EEEE placeholder cells (see
//! [`placeholder`]). Placements floating over the grid ("direct placements")
//! are not supported and answered with `ENOTSUPPORTED`.

//!
//! Unix only: on Windows, ConPTY doesn't pass the commands through.

#[cfg(unix)]
mod apc;
#[cfg(unix)]
mod command;
#[cfg(unix)]
mod engine;
#[cfg(unix)]
mod load;
pub mod placeholder;
#[cfg(unix)]
mod reply;
#[cfg(unix)]
mod store;

#[cfg(unix)]
pub use engine::*;
