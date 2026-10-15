//! Explorer file operations: create, rename, move, copy and trash, with the
//! naming rules VS Code's explorer follows. No UI deps.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

fn err(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
}

/// Check a name typed into the explorer. Like VS Code, `a/b/c.txt` is
/// allowed (it creates the folders in between); empty names, absolute paths
/// and `..` are not.
pub fn validate_name(name: &str) -> io::Result<()> {
    if name.trim().is_empty() {
        return Err(err("A file or folder name must be provided."));
    }
    if name.trim() != name {
        return Err(err("Leading or trailing whitespace detected in file or folder name."));
    }
    let path = Path::new(name);
    if path.is_absolute() {
        return Err(err("A file or folder name cannot start with a slash."));
    }
    if path.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(err(format!("The name {name} is not valid as a file or folder name.")));
    }
    Ok(())
}

/// The file or folder `name` would name inside `dir`.
pub fn child_path(dir: &Path, name: &str) -> io::Result<PathBuf> {
    validate_name(name)?;
    Ok(dir.join(name))
}

fn exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

fn already_exists(path: &Path) -> io::Error {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("A file or folder {name} already exists at this location. Please choose a different name."),
    )
}

/// Create an empty file, and any missing folders above it.
pub fn create_file(path: &Path) -> io::Result<()> {
    if exists(path) {
        return Err(already_exists(path));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::OpenOptions::new().write(true).create_new(true).open(path).map(drop)
}

/// Create a folder, and any missing folders above it.
pub fn create_dir(path: &Path) -> io::Result<()> {
    if exists(path) {
        return Err(already_exists(path));
    }
    fs::create_dir_all(path)
}

/// Rename or move `from` to `to`, never over an existing entry. A change of
/// case only is allowed on case-insensitive volumes, where `to` "exists"
/// because it is `from`.
pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
    if from == to {
        return Ok(());
    }
    if to.starts_with(from) {
        return Err(err("Cannot move a folder into itself."));
    }
    if exists(to) && !same_entry(from, to) {
        return Err(already_exists(to));
    }
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(from, to)
}

fn same_entry(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Move `src` into folder `dir`, keeping its name. Returns where it went.
pub fn move_into(src: &Path, dir: &Path) -> io::Result<PathBuf> {
    let name = src.file_name().ok_or_else(|| err("Cannot move the root folder."))?;
    let to = dir.join(name);
    rename(src, &to)?;
    Ok(to)
}

/// Copy `src` (a file or a whole folder) into folder `dir`. A name already
/// taken there gets VS Code's ` copy`, ` copy 2`, ... suffix. Returns the copy.
pub fn copy_into(src: &Path, dir: &Path) -> io::Result<PathBuf> {
    if dir.starts_with(src) {
        return Err(err("Cannot copy a folder into itself."));
    }
    let name = src.file_name().ok_or_else(|| err("Cannot copy the root folder."))?.to_string_lossy();
    let to = free_name(dir, &name, src.is_dir());
    copy_recursive(src, &to)?;
    Ok(to)
}

/// `name` inside `dir`, or the first of `stem copy.ext`, `stem copy 2.ext`, ...
/// that isn't taken.
pub fn free_name(dir: &Path, name: &str, is_dir: bool) -> PathBuf {
    let first = dir.join(name);
    if !exists(&first) {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 && !is_dir => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..)
        .map(|n| match n {
            1 => dir.join(format!("{stem} copy{ext}")),
            n => dir.join(format!("{stem} copy {n}{ext}")),
        })
        .find(|p| !exists(p))
        .expect("unbounded")
}

fn copy_recursive(src: &Path, to: &Path) -> io::Result<()> {
    let meta = src.symlink_metadata()?;
    if meta.file_type().is_symlink() {
        let target = fs::read_link(src)?;
        return symlink(&target, to);
    }
    if meta.is_dir() {
        fs::create_dir(to)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &to.join(entry.file_name()))?;
        }
        return Ok(());
    }
    fs::copy(src, to).map(drop)
}

#[cfg(unix)]
fn symlink(target: &Path, at: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, at)
}

#[cfg(not(unix))]
fn symlink(target: &Path, at: &Path) -> io::Result<()> {
    fs::copy(target, at).map(drop)
}

/// Move `path` to the Trash, where Finder can put it back.
#[cfg(target_os = "macos")]
pub fn trash(path: &Path) -> io::Result<()> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};

    if !exists(path) {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("{} does not exist", path.display())));
    }
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    NSFileManager::defaultManager().trashItemAtURL_resultingItemURL_error(&url, None).map_err(|e| err(e.localizedDescription().to_string()))
}

/// Move `path` to the Recycle Bin (Windows) or the freedesktop Trash (Linux).
#[cfg(not(target_os = "macos"))]
pub fn trash(path: &Path) -> io::Result<()> {
    if !exists(path) {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("{} does not exist", path.display())));
    }
    trash::delete(path).map_err(|e| err(e.to_string()))
}

/// Drop paths inside another path of the list, so a folder and its contents
/// are handled once.
pub fn outermost(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths.iter().filter(|p| !paths.iter().any(|q| q != *p && p.starts_with(q))).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(validate_name("a.txt").is_ok());
        assert!(validate_name("a/b/c.txt").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name(" a").is_err());
        assert!(validate_name("/a").is_err());
        assert!(validate_name("../a").is_err());
        assert!(validate_name("a/./b").is_ok()); // `.` components are dropped by Path
    }

    #[test]
    fn create_rename_and_refuse_overwrite() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("x/y/new.rs");
        create_file(&f).unwrap();
        assert!(f.is_file());
        assert!(create_file(&f).is_err());
        create_dir(&d.path().join("dir")).unwrap();
        assert!(create_dir(&d.path().join("dir")).is_err());

        let g = d.path().join("x/other.rs");
        fs::write(&g, "").unwrap();
        assert_eq!(rename(&f, &g).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        rename(&f, &d.path().join("x/renamed.rs")).unwrap();
        assert!(!f.exists());
        assert!(rename(&d.path().join("x"), &d.path().join("x/y/z")).is_err());
    }

    #[test]
    fn copy_suffixes_like_vscode() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        fs::create_dir_all(r.join("src/inner")).unwrap();
        fs::write(r.join("src/inner/a.txt"), "hi").unwrap();
        fs::write(r.join("main.rs"), "fn").unwrap();

        assert_eq!(copy_into(&r.join("main.rs"), r).unwrap(), r.join("main copy.rs"));
        assert_eq!(copy_into(&r.join("main.rs"), r).unwrap(), r.join("main copy 2.rs"));
        let copy = copy_into(&r.join("src"), r).unwrap();
        assert_eq!(copy, r.join("src copy"));
        assert_eq!(fs::read_to_string(copy.join("inner/a.txt")).unwrap(), "hi");
        assert!(copy_into(&r.join("src"), &r.join("src/inner")).is_err());

        let moved = move_into(&r.join("main.rs"), &r.join("src")).unwrap();
        assert_eq!(moved, r.join("src/main.rs"));
        assert!(moved.is_file());
    }

    #[test]
    fn outermost_drops_nested() {
        let p = |s: &str| PathBuf::from(s);
        assert_eq!(outermost(&[p("/a/b"), p("/a"), p("/c")]), vec![p("/a"), p("/c")]);
    }
}
