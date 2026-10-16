//! The menu bar agent: `tern --menubar`. A small separate process that
//! lists the background daemon's terminals in the macOS menu bar, also
//! while Tern is closed. It holds no programs, so it can be replaced at any
//! time.

#[cfg(target_os = "macos")]
mod app;
use std::path::{Path, PathBuf};

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

/// The agent's executable in an app bundle whose main executable is
/// `app_exe`: a helper app of its own, so LaunchServices doesn't take the
/// running agent for Tern (`open -b dev.tern`, `tern://` URLs, the Dock).
pub fn helper_exe(app_exe: &Path) -> PathBuf {
    let contents = app_exe.parent().and_then(Path::parent).unwrap_or(Path::new("/"));
    contents.join("Helpers/Tern Menu Bar.app/Contents/MacOS/tern-menubar")
}

/// Start the agent, detached (it outlives the UI). It sorts out by itself
/// whether another one stays. Outside a bundle (dev builds) it is this
/// executable with `--menubar`.
pub fn launch() {
    let log = tern_mux::client::log_path("menubar");
    let helper = std::env::current_exe().map(|e| helper_exe(&e)).ok().filter(|h| h.is_file());
    let r = match &helper {
        Some(h) => tern_mux::client::spawn_detached_exe(h, &[], log.as_deref()),
        None => tern_mux::client::spawn_detached(&["--menubar"], log.as_deref()),
    };
    if let Err(e) = r {
        log::warn!("can't start the menu bar agent: {e}");
    }
}

/// Stop the running agent, if any ("Hide Menu Bar Icon" in Tern).
pub fn stop() {
    #[cfg(unix)]
    if let Some(pid) = instance::holder(&instance::lock_path()) {
        // SAFETY: plain kill.
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn helper_sits_inside_the_app_bundle() {
        assert_eq!(
            super::helper_exe(Path::new("/Applications/Tern.app/Contents/MacOS/tern")),
            Path::new("/Applications/Tern.app/Contents/Helpers/Tern Menu Bar.app/Contents/MacOS/tern-menubar")
        );
    }
}
