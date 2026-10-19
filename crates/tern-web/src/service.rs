//! `tern --web` as a background service the app turns on and off (its
//! Settings ▸ Connections): it runs detached, like the daemon, so it keeps
//! serving after the app quits. A running server holds `web.lock` and says
//! what it serves in `web.json`, both in the daemon's folder; the app reads
//! them, and ends it by the pid it gave.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

const LOCK: &str = "web.lock";
const STATUS: &str = "web.json";
/// The app's own choice, kept to start the server again at its next launch:
/// `tailscale` or `all`. Missing: off. A server started by hand leaves it be.
const WANTED: &str = "web-wanted";
pub(crate) const TOKEN: &str = "web-token";

/// What a running server serves.
#[derive(Clone, Debug, PartialEq)]
pub struct Serving {
    pub pid: u32,
    /// The secret links to open, the address one first (then MagicDNS).
    pub links: Vec<String>,
    /// Listening on every network, not just Tailscale.
    pub all_networks: bool,
    /// Only this machine can reach it (no Tailscale address was found).
    pub local_only: bool,
}

fn dir() -> PathBuf {
    tern_mux::paths::mux_dir()
}

/// Taken by a running server; `Err` when another one holds it.
pub(crate) fn lock(dir: &Path) -> io::Result<File> {
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(LOCK))?;
    match f.try_lock() {
        Ok(()) => Ok(f),
        Err(std::fs::TryLockError::WouldBlock) => Err(io::Error::other("another tern --web is already serving")),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// Written once the server listens.
pub(crate) fn write_status(dir: &Path, s: &Serving) -> io::Result<()> {
    let v = serde_json::json!({
        "pid": s.pid,
        "links": s.links,
        "all_networks": s.all_networks,
        "local_only": s.local_only,
    });
    std::fs::write(dir.join(STATUS), v.to_string())
}

/// What the running server serves; `None` when none runs (or it hasn't
/// started listening yet).
pub fn serving() -> Option<Serving> {
    serving_in(&dir())
}

fn serving_in(dir: &Path) -> Option<Serving> {
    // Free: no server, whatever an old status file says.
    if lock(dir).is_ok() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join(STATUS)).ok()?).ok()?;
    Some(Serving {
        pid: v.get("pid")?.as_u64()? as u32,
        links: v.get("links")?.as_array()?.iter().filter_map(|l| Some(l.as_str()?.to_owned())).collect(),
        all_networks: v.get("all_networks").and_then(|b| b.as_bool()).unwrap_or(false),
        local_only: v.get("local_only").and_then(|b| b.as_bool()).unwrap_or(false),
    })
}

/// Start a server in the background, on every network or on Tailscale's,
/// and remember to start it at the app's next launch. Returns before it
/// listens: watch [`serving`].
pub fn start(all_networks: bool) -> io::Result<()> {
    let dir = dir();
    tern_mux::paths::ensure_private_dir(&dir)?;
    std::fs::write(dir.join(WANTED), if all_networks { "all" } else { "tailscale" })?;
    let args: &[&str] = if all_networks { &["--web", "--bind", "0.0.0.0"] } else { &["--web"] };
    tern_mux::client::spawn_background(args, log_file().as_deref())
}

/// End the running server (fine when none runs), and don't start it at
/// the next launch.
pub fn stop() -> io::Result<()> {
    let _ = std::fs::remove_file(dir().join(WANTED));
    match serving() {
        Some(s) => tern_mux::client::kill_process(s.pid),
        None => Ok(()),
    }
}

/// The app turned it on last: `Some(all_networks)`.
pub fn wanted() -> Option<bool> {
    match std::fs::read_to_string(dir().join(WANTED)).ok()?.trim() {
        "all" => Some(true),
        "tailscale" => Some(false),
        _ => None,
    }
}

/// Make a new secret link; the old ones stop working once the server
/// starts again.
pub fn renew_token() -> io::Result<()> {
    let dir = dir();
    tern_mux::paths::ensure_private_dir(&dir)?;
    crate::auth::load_token(&dir.join(TOKEN), true).map(drop)
}

/// The server's log.
pub fn log_file() -> Option<PathBuf> {
    tern_mux::client::log_path("web")
}

/// The last line the server logged, to say why it didn't start.
pub fn last_log_line() -> Option<String> {
    let text = std::fs::read_to_string(log_file()?).ok()?;
    text.lines().rev().find(|l| !l.trim().is_empty()).map(|l| l.trim().to_owned())
}

/// `url` as a QR code: its width in modules, and each module (row by row),
/// dark or not.
pub fn qr(url: &str) -> Option<(usize, Vec<bool>)> {
    let code = qrcode::QrCode::new(url.as_bytes()).ok()?;
    let dark = code.to_colors().into_iter().map(|c| c == qrcode::Color::Dark).collect();
    Some((code.width(), dark))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_reads_back() {
        let dir = std::env::temp_dir().join(format!("tern-web-status-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let s = Serving { pid: 42, links: vec!["http://a:1/#t=x".into()], all_networks: true, local_only: false };
        write_status(&dir, &s).unwrap();
        assert_eq!(serving_in(&dir), None);
        let held = lock(&dir).unwrap();
        assert_eq!(serving_in(&dir), Some(s));
        // A second server can't take it.
        assert!(lock(&dir).is_err());
        drop(held);
        assert_eq!(serving_in(&dir), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn draws_a_qr_code() {
        let (width, dark) = qr("http://box:7317/#t=0123456789abcdef0123456789abcdef").unwrap();
        assert_eq!(dark.len(), width * width);
        assert!(dark.iter().any(|d| *d) && dark.iter().any(|d| !*d));
    }
}
