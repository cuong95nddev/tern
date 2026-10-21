//! Background sessions: the `tern --server` daemon that keeps terminals
//! running while Tern is closed, and the client the UI talks to it with.

pub mod client;
#[cfg(unix)]
pub mod fdpass;
pub mod host;
#[cfg(unix)]
pub mod images;
pub mod notify;
pub mod observer;
pub mod paths;
pub mod proto;
pub mod server;
pub mod viewer;

/// The daemon's socket: a Unix domain socket, on Windows too (AF_UNIX,
/// Windows 10 1803 and later).
#[cfg(unix)]
pub(crate) use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(windows)]
pub(crate) use uds_windows::{UnixListener, UnixStream};
