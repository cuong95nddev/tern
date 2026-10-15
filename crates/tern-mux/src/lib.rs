//! Background sessions: the `tern --server` daemon that keeps terminals
//! running while Tern is closed, and the client the UI talks to it with.

pub mod fdpass;
pub mod host;
pub mod proto;
