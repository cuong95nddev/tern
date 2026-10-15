//! Whether the menu bar icon is shown: a file next to `claude-args`,
//! `off` to hide it (missing = shown).

use std::io;
use std::path::{Path, PathBuf};

pub fn file() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join("Library/Application Support/dev.tern/menubar")
}

pub fn enabled(file: &Path) -> bool {
    std::fs::read_to_string(file).map_or(true, |s| s.trim() != "off")
}

pub fn set_enabled(file: &Path, on: bool) -> io::Result<()> {
    if on {
        return match std::fs::remove_file(file) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(file, "off\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_means_on_and_off_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("sub/menubar");
        assert!(enabled(&f));
        set_enabled(&f, false).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "off\n");
        assert!(!enabled(&f));
        set_enabled(&f, true).unwrap();
        assert!(enabled(&f));
        std::fs::write(&f, "  off  \n").unwrap();
        assert!(!enabled(&f));
        std::fs::write(&f, "garbage").unwrap();
        assert!(enabled(&f));
    }
}
