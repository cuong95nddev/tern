//! Background sessions: the `tern --server` daemon that keeps terminals
//! running while Tern is closed, and the client the UI talks to it with.

pub mod client;
pub mod fdpass;
pub mod host;
pub mod notify;
pub mod observer;
pub mod paths;
pub mod proto;
pub mod server;
