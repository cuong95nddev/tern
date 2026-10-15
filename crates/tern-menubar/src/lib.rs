//! The menu bar agent: `tern --menubar`. A small separate process that
//! lists the background daemon's terminals in the macOS menu bar, also
//! while Tern is closed. It holds no programs, so it can be replaced at any
//! time.

#[cfg(target_os = "macos")]
mod app;
pub mod instance;
pub mod link;
pub mod model;
pub mod setting;

/// `tern --menubar`: run the agent until stopped. One agent at a time, the
/// newest. macOS only.
#[cfg(target_os = "macos")]
pub fn run() -> ! {
    let version = env!("CARGO_PKG_VERSION");
    let stop = |pid: u32| {
        // SAFETY: plain kill.
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    };
    let _lock = match instance::take(&instance::lock_path(), version, stop) {
        Ok(instance::Taken::Ours(f)) => f,
        Ok(instance::Taken::Busy) => {
            log::info!("a menu bar agent as new runs already");
            std::process::exit(0)
        },
        Err(e) => {
            log::error!("menu bar lock: {e}");
            std::process::exit(1)
        },
    };
    log::info!("menu bar agent {} (Tern {version})", std::process::id());
    app::run(tern_mux::paths::mux_dir())
}

/// Start the agent, detached (it outlives the UI). It sorts out by itself
/// whether another one stays.
pub fn launch() {
    let log = tern_mux::client::log_path("menubar");
    if let Err(e) = tern_mux::client::spawn_detached(&["--menubar"], log.as_deref()) {
        log::warn!("can't start the menu bar agent: {e}");
    }
}

/// Stop the running agent, if any ("Hide Menu Bar Icon" in Tern).
pub fn stop() {
    if let Some(pid) = instance::holder(&instance::lock_path()) {
        // SAFETY: plain kill.
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    }
}
