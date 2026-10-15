//! One agent at a time, the newest: the lock file holds `<pid> <version>`
//! of the agent that has it. A newer agent stops an older one (after an
//! update); otherwise the one starting exits.

use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a newer agent waits for an older one to let go.
pub const TAKEOVER_WAIT: Duration = Duration::from_secs(2);

pub fn lock_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/tmp".into());
    home.join("Library/Caches/dev.tern").join(if cfg!(debug_assertions) { "menubar-dev.lock" } else { "menubar.lock" })
}

pub enum Taken {
    /// Ours while the file stays open.
    Ours(File),
    /// An agent as new as this one, or newer, runs.
    Busy,
}

fn open(path: &Path) -> io::Result<File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(path)
}

fn try_lock(f: &File) -> io::Result<bool> {
    match f.try_lock() {
        Ok(()) => Ok(true),
        Err(std::fs::TryLockError::WouldBlock) => Ok(false),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

fn write_holder(mut f: &File, pid: u32, version: &str) -> io::Result<()> {
    f.set_len(0)?;
    f.rewind()?;
    writeln!(f, "{pid} {version}")?;
    f.sync_all()
}

fn read_holder(path: &Path) -> Option<(u32, semver::Version)> {
    let mut s = String::new();
    File::open(path).ok()?.read_to_string(&mut s).ok()?;
    let mut it = s.split_whitespace();
    let pid = it.next()?.parse().ok()?;
    let version = semver::Version::parse(it.next()?).ok()?;
    Some((pid, version))
}

/// Take the lock at `path` for an agent of `version`; an older holder is
/// asked to go with `stop(pid)` (SIGTERM in the real agent).
pub fn take(path: &Path, version: &str, stop: impl Fn(u32)) -> io::Result<Taken> {
    let f = open(path)?;
    if !try_lock(&f)? {
        let ours = semver::Version::parse(version).map_err(io::Error::other)?;
        match read_holder(path) {
            Some((pid, theirs)) if theirs < ours => {
                log::info!("replacing menu bar agent {pid} (Tern {theirs})");
                stop(pid);
                let deadline = Instant::now() + TAKEOVER_WAIT;
                loop {
                    if try_lock(&f)? {
                        break;
                    }
                    if Instant::now() >= deadline {
                        return Ok(Taken::Busy);
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            },
            _ => return Ok(Taken::Busy),
        }
    }
    write_holder(&f, std::process::id(), version)?;
    Ok(Taken::Ours(f))
}

/// The pid of the agent holding `path`, if one does.
pub fn holder(path: &Path) -> Option<u32> {
    let f = File::open(path).ok()?;
    // Free (we got it) = nobody holds it; let go at once.
    if try_lock(&f).ok()? {
        return None;
    }
    read_holder(path).map(|(pid, _)| pid)
}

// Windows' locks keep others from reading the file; the agent is macOS only.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Hold `path` as another agent would: locked, with `<pid> <version>`.
    fn hold(path: &Path, pid: u32, version: &str) -> std::fs::File {
        let f = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(path).unwrap();
        f.try_lock().unwrap();
        write_holder(&f, pid, version).unwrap();
        f
    }

    #[test]
    fn free_lock_is_taken_and_names_us() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("menubar.lock");
        let Taken::Ours(_f) = take(&path, "0.1.2", |_| panic!("nothing to stop")).unwrap() else { panic!("busy") };
        assert_eq!(std::fs::read_to_string(&path).unwrap(), format!("{} 0.1.2\n", std::process::id()));
        assert_eq!(holder(&path), Some(std::process::id()));
    }

    #[test]
    fn older_holder_is_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("menubar.lock");
        let old = Mutex::new(Some(hold(&path, 4242, "0.1.1")));
        let stopped = Mutex::new(vec![]);
        // Stopping it releases its lock, as its exit would.
        let got = take(&path, "0.1.2", |pid| {
            stopped.lock().unwrap().push(pid);
            old.lock().unwrap().take();
        })
        .unwrap();
        assert!(matches!(got, Taken::Ours(_)));
        assert_eq!(*stopped.lock().unwrap(), [4242]);
    }

    #[test]
    fn equal_or_newer_holder_stays() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("menubar.lock");
        let _same = hold(&path, 4242, "0.1.2");
        assert!(matches!(take(&path, "0.1.2", |_| panic!("stopped an equal agent")).unwrap(), Taken::Busy));
        drop(_same);
        let _newer = hold(&path, 4242, "0.2.0");
        assert!(matches!(take(&path, "0.1.2", |_| panic!("stopped a newer agent")).unwrap(), Taken::Busy));
    }

    #[test]
    fn holder_that_wont_stop_keeps_it() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("menubar.lock");
        let _old = hold(&path, 4242, "0.1.1");
        let t = std::time::Instant::now();
        assert!(matches!(take(&path, "0.1.2", |_| {}).unwrap(), Taken::Busy));
        assert!(t.elapsed() >= TAKEOVER_WAIT);
    }

    #[test]
    fn no_holder_without_a_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("menubar.lock");
        std::fs::write(&path, "4242 0.1.1\n").unwrap(); // stale: nobody holds it
        assert_eq!(holder(&path), None);
    }
}
