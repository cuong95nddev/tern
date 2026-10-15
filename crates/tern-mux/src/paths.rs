//! Where the daemon listens, and who may talk to it.

use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use crate::proto::MAJOR;

fn caches() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/tmp".into());
    home.join("Library/Caches/dev.tern")
}

/// The daemon's directory. Debug builds have their own daemon, so a dev
/// build runs next to the installed app.
pub fn mux_dir() -> PathBuf {
    caches().join(if cfg!(debug_assertions) { "mux-dev" } else { "mux" })
}

/// Where the daemon keeps per-session files (Claude settings).
pub fn run_root() -> PathBuf {
    caches().join("run")
}

/// The socket of this protocol major: other majors use other sockets.
pub fn socket_path(dir: &Path) -> PathBuf {
    dir.join(format!("mux-{MAJOR}.sock"))
}

/// The lock a daemon of this protocol major holds while it serves: a
/// daemon of another major (an older Tern's, still running its programs)
/// doesn't keep this one from starting.
pub fn lock_path(dir: &Path) -> PathBuf {
    dir.join(format!("server-{MAJOR}.lock"))
}

/// Create `dir` (0700), or check that it is a directory only we can use.
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

/// Whether the process at the other end of `s` runs as us.
#[cfg(not(target_os = "linux"))]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_dir_is_created_and_checked() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("mux");
        ensure_private_dir(&dir).unwrap();
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(ensure_private_dir(&dir).unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn same_user_peer_is_accepted() {
        let (a, _b) = UnixStream::pair().unwrap();
        assert!(peer_uid_ok(&a));
    }
}
