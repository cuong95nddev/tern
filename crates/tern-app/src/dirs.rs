//! Where Tern keeps its files: macOS `~/Library/…/dev.tern`, Windows
//! `%APPDATA%` / `%LOCALAPPDATA%\Tern`, Linux the XDG base directories.

use std::path::PathBuf;

/// The user's home folder (`HOME`, on Windows `USERPROFILE`).
pub fn home() -> Option<PathBuf> {
    let var = |k| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    var("HOME").or_else(|| if cfg!(windows) { var("USERPROFILE") } else { None })
}

fn home_or_tmp() -> PathBuf {
    home().unwrap_or_else(std::env::temp_dir)
}

/// Kept files: the session, window places, scratches.
pub fn data() -> PathBuf {
    if cfg!(target_os = "macos") {
        home_or_tmp().join("Library/Application Support/dev.tern")
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(home_or_tmp).join("Tern")
    } else {
        xdg("XDG_DATA_HOME", ".local/share").join("tern")
    }
}

/// Settings and themes the user edits: `~/.config/tern` on macOS and Linux
/// (`XDG_CONFIG_HOME`), `%APPDATA%\Tern` on Windows.
pub fn config() -> PathBuf {
    if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(home_or_tmp).join("Tern")
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home_or_tmp().join(".config"))
            .join("tern")
    }
}

/// Files Tern can lose: the instance lock, per-run settings files.
pub fn cache() -> PathBuf {
    if cfg!(target_os = "macos") {
        home_or_tmp().join("Library/Caches/dev.tern")
    } else if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(home_or_tmp).join("Tern")
    } else {
        xdg("XDG_CACHE_HOME", ".cache").join("tern")
    }
}

/// An XDG base directory: the variable when it holds an absolute path, else
/// `fallback` under the home folder.
#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var).map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home_or_tmp().join(fallback))
}
