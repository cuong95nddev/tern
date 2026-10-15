//! The agent as its own executable, for the helper app inside Tern.app
//! (`Contents/Helpers/Tern Menu Bar.app`).

fn main() {
    env_logger::init();
    #[cfg(target_os = "macos")]
    tern_menubar::run();
}
