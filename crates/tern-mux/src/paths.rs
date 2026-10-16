//! Where the daemon listens, and who may talk to it.

use std::io;
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::UnixStream;
use crate::proto::MAJOR;

fn home() -> PathBuf {
    let var = |k| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    var("HOME").or_else(|| if cfg!(windows) { var("USERPROFILE") } else { None }).unwrap_or_else(std::env::temp_dir)
}

/// An XDG base directory: the variable when it holds an absolute path, else
/// `fallback` under the home folder.
#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var).map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home().join(fallback))
}

/// `%LOCALAPPDATA%\Tern`.
#[cfg_attr(not(windows), allow(dead_code))]
fn local_app_data() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(home).join("Tern")
}

/// Files Tern can lose (as `tern-app`'s `dirs::cache`).
fn caches() -> PathBuf {
    if cfg!(target_os = "macos") {
        home().join("Library/Caches/dev.tern")
    } else if cfg!(windows) {
        local_app_data()
    } else {
        xdg("XDG_CACHE_HOME", ".cache").join("tern")
    }
}

/// Where the daemon and the menu bar agent log.
pub fn logs() -> PathBuf {
    if cfg!(target_os = "macos") {
        home().join("Library/Logs/Tern")
    } else if cfg!(windows) {
        local_app_data().join("logs")
    } else {
        xdg("XDG_STATE_HOME", ".local/state").join("tern")
    }
}

/// The daemon's directory. Debug builds have their own daemon, so a dev
/// build runs next to the installed app.
pub fn mux_dir() -> PathBuf {
    caches().join(if cfg!(debug_assertions) { "mux-dev" } else { "mux" })
}

/// The terminal palette of the app's theme, written by the app and read by
/// the daemon to answer programs asking for colors.
pub fn palette_path() -> PathBuf {
    mux_dir().join("palette")
}

/// This executable's build: a hash of its bytes, taken on the first call
/// (call it before an update can replace the file). Tells apart a daemon
/// left from an earlier build of the same version, and matches the Windows
/// daemon's copy of the app. Empty when the file can't be read.
pub fn build_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        use std::hash::Hasher;
        use std::io::Read;
        let hash = || -> io::Result<u64> {
            let mut f = std::fs::File::open(std::env::current_exe()?)?;
            // Fixed keys: the same bytes hash the same in every process.
            let mut h = std::hash::DefaultHasher::new();
            let mut buf = vec![0; 1 << 20];
            loop {
                match f.read(&mut buf)? {
                    0 => return Ok(h.finish()),
                    n => h.write(&buf[..n]),
                }
            }
        };
        hash().map(|h| format!("{h:016x}")).unwrap_or_default()
    })
}

/// Where the daemon keeps per-session files (Claude settings).
pub fn run_root() -> PathBuf {
    caches().join("run")
}

/// The socket of this protocol major: other majors use other sockets.
pub fn socket_path(dir: &Path) -> PathBuf {
    socket_path_of(dir, MAJOR)
}

/// The lock a daemon of this protocol major holds while it serves: a
/// daemon of another major (an older Tern's, still running its programs)
/// doesn't keep this one from starting.
pub fn lock_path(dir: &Path) -> PathBuf {
    dir.join(format!("server-{MAJOR}.lock"))
}

/// Where the daemon of this protocol major writes its pid, for when it
/// stops answering (on Windows the locked file itself can't be read).
pub fn pid_path(dir: &Path) -> PathBuf {
    dir.join(format!("server-{MAJOR}.pid"))
}

/// The protocol majors other than ours that have a socket in `dir` (each
/// may still have a daemon, an older Tern's).
pub fn other_majors(dir: &Path) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir(dir) else { return vec![] };
    let mut majors: Vec<u32> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_prefix("mux-")?.strip_suffix(".sock")?.parse().ok())
        .filter(|&m| m != MAJOR)
        .collect();
    majors.sort_unstable();
    majors
}

/// The socket of protocol major `major`.
pub fn socket_path_of(dir: &Path, major: u32) -> PathBuf {
    dir.join(format!("mux-{major}.sock"))
}

/// Create `dir` (0700), or check that it is a directory only we can use.
#[cfg(unix)]
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let meta = std::fs::symlink_metadata(dir)?;
    // SAFETY: getuid can't fail.
    let uid = unsafe { libc::getuid() };
    if !meta.is_dir() || meta.uid() != uid || meta.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("{} is not a private directory", dir.display())));
    }
    Ok(())
}

/// Create `dir`. It is under `%LOCALAPPDATA%`, which only the user (and
/// administrators) can open, and its socket inherits that.
#[cfg(windows)]
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// Whether the process at the other end of `s` runs as us.
#[cfg(all(unix, not(target_os = "linux")))]
pub fn peer_uid_ok(s: &UnixStream) -> bool {
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: getpeereid writes the two ids of a connected Unix socket.
    let r = unsafe { libc::getpeereid(s.as_raw_fd(), &mut uid, &mut gid) };
    r == 0 && uid == unsafe { libc::getuid() }
}

/// Whether the process at the other end of `s` runs as us.
#[cfg(target_os = "linux")]
pub fn peer_uid_ok(s: &UnixStream) -> bool {
    // SAFETY: ucred is plain data; SO_PEERCRED fills it for a connected Unix socket.
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let r = unsafe { libc::getsockopt(s.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&raw mut cred).cast(), &mut len) };
    r == 0 && cred.uid == unsafe { libc::getuid() }
}

/// Whether the process at the other end of `s` runs as us. Windows' AF_UNIX
/// has no peer credentials; the socket's folder keeps other users out.
#[cfg(windows)]
pub fn peer_uid_ok(_: &UnixStream) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn private_dir_is_created_and_checked() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("mux");
        ensure_private_dir(&dir).unwrap();
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(ensure_private_dir(&dir).unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn other_majors_are_found_by_their_sockets() {
        let tmp = tempfile::tempdir().unwrap();
        for f in ["mux-1.sock", "mux-7.sock", "mux-x.sock", "server-1.lock", "palette"] {
            std::fs::write(tmp.path().join(f), "").unwrap();
        }
        std::fs::write(socket_path(tmp.path()), "").unwrap();
        assert_eq!(other_majors(tmp.path()), vec![1, 7]);
    }

    #[test]
    fn same_user_peer_is_accepted() {
        let (a, _b) = UnixStream::pair().unwrap();
        assert!(peer_uid_ok(&a));
    }
}
