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

/// What a running server serves.
#[derive(Clone, Debug, PartialEq)]
pub struct Serving {
    pub pid: u32,
    /// Where it is (`http://host:port/`), the address first (then
    /// MagicDNS): a pairing link goes on one of these.
    pub links: Vec<String>,
    /// Listening on every network, not just Tailscale.
    pub all_networks: bool,
    /// Only this machine can reach it (no Tailscale address was found).
    pub local_only: bool,
    /// The paired devices with a page open (see [`crate::access`]).
    pub connected: Vec<String>,
    /// The server's [`build_id`](tern_mux::paths::build_id); `None` from a
    /// server older than the field.
    pub build: Option<String>,
}

impl Serving {
    /// Whether the server runs another build than this app: it serves the
    /// page built into it, so an update shows only once it is replaced.
    pub fn outdated(&self) -> bool {
        let ours = tern_mux::paths::build_id();
        // A build that couldn't be read tells nothing.
        self.build.as_deref().is_none_or(|b| !b.is_empty() && !ours.is_empty() && b != ours)
    }
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
        "connected": s.connected,
        "build": s.build,
    });
    // Whole, for the app reading it.
    let tmp = dir.join(format!("{STATUS}.tmp"));
    std::fs::write(&tmp, v.to_string())?;
    std::fs::rename(tmp, dir.join(STATUS))
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
        // A server of old gave its secret link.
        links: v.get("links")?.as_array()?.iter().filter_map(|l| Some(l.as_str()?.split('#').next()?.to_owned())).collect(),
        all_networks: v.get("all_networks").and_then(|b| b.as_bool()).unwrap_or(false),
        local_only: v.get("local_only").and_then(|b| b.as_bool()).unwrap_or(false),
        connected: v
            .get("connected")
            .and_then(|c| c.as_array())
            .map(|c| c.iter().filter_map(|id| Some(id.as_str()?.to_owned())).collect())
            .unwrap_or_default(),
        build: v.get("build").and_then(|b| b.as_str()).map(str::to_owned),
    })
}

/// Start a server in the background, on every network or on Tailscale's,
/// and remember to start it at the app's next launch. Returns before it
/// listens: watch [`serving`].
pub fn start(all_networks: bool) -> io::Result<()> {
    let dir = dir();
    tern_mux::paths::ensure_private_dir(&dir)?;
    std::fs::write(dir.join(WANTED), if all_networks { "all" } else { "tailscale" })?;
    // The app makes its own pairing links.
    let args: &[&str] = if all_networks { &["--web", "--no-pair", "--bind", "0.0.0.0"] } else { &["--web", "--no-pair"] };
    tern_mux::client::spawn_background(args, log_file().as_deref())
}

/// Replace a running server of another build with one of this build, on
/// the same networks. Its pages reconnect, and reload for the new page.
pub fn replace_outdated(all_networks: bool) -> io::Result<()> {
    let dir = dir();
    let Some(old) = serving_in(&dir).filter(Serving::outdated) else { return Ok(()) };
    tern_mux::client::kill_process(old.pid)?;
    // The new one can't serve until the old one lets go of the lock.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while lock(&dir).is_err() {
        if std::time::Instant::now() > deadline {
            return Err(io::Error::other("the old server didn't stop"));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    start(all_networks)
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
        let s = Serving {
            pid: 42,
            links: vec!["http://a:1/".into()],
            all_networks: true,
            local_only: false,
            connected: vec!["d1".into()],
            build: Some("abc".into()),
        };
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
    fn a_server_of_another_build_is_outdated() {
        let ours = tern_mux::paths::build_id().to_owned();
        let s = |build: Option<&str>| Serving {
            pid: 1,
            links: Vec::new(),
            all_networks: false,
            local_only: false,
            connected: Vec::new(),
            build: build.map(str::to_owned),
        };
        assert!(!s(Some(&ours)).outdated());
        // Older than the field: it can't serve this build's page.
        assert!(s(None).outdated());
        // Unreadable: tells nothing.
        assert!(!s(Some("")).outdated());
        if !ours.is_empty() {
            assert!(s(Some("0000")).outdated());
        }
    }

    #[test]
    fn draws_a_qr_code() {
        let (width, dark) = qr("http://box:7317/#p=ABCDEFGHJKMN").unwrap();
        assert_eq!(dark.len(), width * width);
        assert!(dark.iter().any(|d| *d) && dark.iter().any(|d| !*d));
    }
}
