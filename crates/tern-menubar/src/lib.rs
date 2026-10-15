//! The menu bar agent: `tern --menubar`. A small separate process that
//! lists the background daemon's terminals in the macOS menu bar, also
//! while Tern is closed. It holds no programs, so it can be replaced at any
//! time.

pub mod instance;
pub mod model;
pub mod setting;
