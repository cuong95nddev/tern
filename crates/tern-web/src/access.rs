//! Who may use the terminals from a browser, as in T3 Code: a device pairs
//! with a one-time link (a short code, good for a few minutes), and then
//! holds a session of its own, which Settings ▸ Connections lists and can
//! revoke. The server ends a revoked device's open pages at once.
//!
//! Kept in `web-access.json` in the daemon's private folder, changed by the
//! server and by the app under `web-access.lock`. Only hashes of the codes
//! and the session secrets are kept.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const FILE: &str = "web-access.json";
const LOCK: &str = "web-access.lock";
/// How long a pairing link works (once).
pub const PAIRING_TTL: u64 = 5 * 60;
/// How long a device stays paired without being used.
pub const SESSION_TTL: u64 = 30 * 24 * 3600;
/// Letters of a pairing code: no 0/O, 1/I/L to mix up when typed.
const CODE_ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";
const CODE_LEN: usize = 12;

/// The paired devices and the pairing links not used yet.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Access {
    #[serde(default)]
    pub pairings: Vec<Pairing>,
    #[serde(default)]
    pub devices: Vec<Device>,
}

/// A pairing link not used yet.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pairing {
    pub id: String,
    hash: String,
    pub created: u64,
    pub expires: u64,
}

/// A paired device: one browser.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    hash: String,
    /// Its browser and system, from its user agent ("Safari on iPhone").
    pub name: String,
    /// The address it paired from.
    pub ip: String,
    /// Unix seconds.
    pub paired: u64,
    pub last_seen: u64,
}

impl Device {
    pub fn expires(&self) -> u64 {
        self.last_seen.saturating_add(SESSION_TTL)
    }
}

/// A pairing link just made: its code goes in the link, nowhere else.
#[derive(Clone, Debug, PartialEq)]
pub struct NewPairing {
    pub id: String,
    pub code: String,
    pub expires: u64,
}

impl NewPairing {
    /// The link to open, on the server at `base` (`http://host:port/`).
    pub fn link(&self, base: &str) -> String {
        let base = base.split('#').next().unwrap_or(base);
        format!("{base}#p={}", self.code)
    }
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn dir() -> PathBuf {
    tern_mux::paths::mux_dir()
}

/// Who has access now (without what's expired).
pub fn load() -> Access {
    let dir = dir();
    change(&dir, |_| ()).map(|(a, ())| a).unwrap_or_else(|_| read(&dir))
}

/// Make a pairing link, in place of any other not used yet.
pub fn pair() -> io::Result<NewPairing> {
    let dir = dir();
    tern_mux::paths::ensure_private_dir(&dir)?;
    pair_in(&dir)
}

/// Stop a pairing link from working.
pub fn revoke_pairing(id: &str) -> io::Result<()> {
    change(&dir(), |a| a.pairings.retain(|p| p.id != id)).map(drop)
}

/// Unpair a device: it needs a new pairing link to come back.
pub fn revoke(id: &str) -> io::Result<()> {
    change(&dir(), |a| a.devices.retain(|d| d.id != id)).map(drop)
}

/// Unpair every device, and stop every pairing link.
pub fn revoke_all() -> io::Result<()> {
    change(&dir(), |a| *a = Access::default()).map(drop)
}

pub(crate) fn pair_in(dir: &Path) -> io::Result<NewPairing> {
    let code = new_code()?;
    let new = NewPairing { id: random_hex(8)?, code, expires: now() + PAIRING_TTL };
    change(dir, |a| {
        a.pairings = vec![Pairing { id: new.id.clone(), hash: hash(&new.code), created: now(), expires: new.expires }];
    })?;
    Ok(new)
}

/// Trade a pairing code for a new device's session: the cookie's value.
/// `None` when the code isn't one (or is used, or expired).
pub(crate) fn redeem(dir: &Path, code: &str, user_agent: &str, ip: &str) -> io::Result<Option<(String, Device)>> {
    let code = code.trim().to_ascii_uppercase();
    let h = hash(&code);
    let secret = random_hex(16)?;
    let id = random_hex(8)?;
    let (_, made) = change(dir, |a| {
        let i = a.pairings.iter().position(|p| crate::auth::same(&p.hash, &h))?;
        a.pairings.remove(i);
        let t = now();
        let d = Device { id: id.clone(), hash: hash(&secret), name: device_name(user_agent), ip: ip.to_owned(), paired: t, last_seen: t };
        a.devices.push(d.clone());
        Some(d)
    })?;
    Ok(made.map(|d| (format!("{id}.{secret}"), d)))
}

/// The device a session cookie's value is for, if it's paired.
pub(crate) fn check<'a>(access: &'a Access, cookie: &str) -> Option<&'a Device> {
    let (id, secret) = cookie.split_once('.')?;
    let d = access.devices.iter().find(|d| d.id == id)?;
    (crate::auth::same(&d.hash, &hash(secret)) && d.expires() > now()).then_some(d)
}

/// Note that a device was just used.
pub(crate) fn touch(dir: &Path, id: &str) -> io::Result<()> {
    change(dir, |a| {
        if let Some(d) = a.devices.iter_mut().find(|d| d.id == id) {
            d.last_seen = now();
        }
    })
    .map(drop)
}

/// The file as it is now, without taking the lock.
pub(crate) fn read(dir: &Path) -> Access {
    std::fs::read_to_string(dir.join(FILE)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub(crate) fn file(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

/// Run `f` on the file under the lock, then drop what has expired and write
/// it back if anything changed.
fn change<R>(dir: &Path, f: impl FnOnce(&mut Access) -> R) -> io::Result<(Access, R)> {
    let lock = File::create(dir.join(LOCK))?;
    lock.lock()?;
    let before = read(dir);
    let mut a = before.clone();
    let r = f(&mut a);
    let t = now();
    a.pairings.retain(|p| p.expires > t);
    a.devices.retain(|d| d.expires() > t);
    if a != before {
        let tmp = dir.join(format!("{FILE}.tmp"));
        std::fs::write(&tmp, serde_json::to_string_pretty(&a).map_err(io::Error::other)?)?;
        std::fs::rename(&tmp, dir.join(FILE))?;
    }
    Ok((a, r))
}

fn hash(s: &str) -> String {
    Sha256::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn random_hex(n: usize) -> io::Result<String> {
    let mut b = vec![0u8; n];
    getrandom::fill(&mut b).map_err(io::Error::other)?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// A pairing code: letters drawn evenly from [`CODE_ALPHABET`].
fn new_code() -> io::Result<String> {
    let limit = 256 - 256 % CODE_ALPHABET.len();
    let mut code = String::with_capacity(CODE_LEN);
    while code.len() < CODE_LEN {
        let mut b = [0u8; 32];
        getrandom::fill(&mut b).map_err(io::Error::other)?;
        for x in b.into_iter().filter(|x| (*x as usize) < limit) {
            if code.len() < CODE_LEN {
                code.push(CODE_ALPHABET[x as usize % CODE_ALPHABET.len()] as char);
            }
        }
    }
    Ok(code)
}

/// "Safari on iPhone", from a user agent.
pub(crate) fn device_name(ua: &str) -> String {
    let has = |s: &str| ua.contains(s);
    let system = if has("iPhone") {
        "iPhone"
    } else if has("iPad") {
        "iPad"
    } else if has("Android") {
        "Android"
    } else if has("CrOS") {
        "ChromeOS"
    } else if has("Mac OS X") || has("Macintosh") {
        "Mac"
    } else if has("Windows") {
        "Windows"
    } else if has("Linux") {
        "Linux"
    } else {
        ""
    };
    let browser = if has("Edg/") || has("EdgA/") || has("EdgiOS/") {
        "Edge"
    } else if has("Firefox/") || has("FxiOS/") {
        "Firefox"
    } else if has("OPR/") {
        "Opera"
    } else if has("SamsungBrowser/") {
        "Samsung Internet"
    } else if has("Chrome/") || has("CriOS/") {
        "Chrome"
    } else if has("Safari/") {
        "Safari"
    } else {
        ""
    };
    match (browser, system) {
        ("", "") => "Browser".into(),
        ("", s) => s.into(),
        (b, "") => b.into(),
        (b, s) => format!("{b} on {s}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("tern-web-access-{}", random_hex(8).unwrap()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_pairing_link_works_once() {
        let dir = temp_dir();
        let p = pair_in(&dir).unwrap();
        assert_eq!(p.code.len(), CODE_LEN);
        assert!(p.code.bytes().all(|b| CODE_ALPHABET.contains(&b)));
        assert!(!std::fs::read_to_string(file(&dir)).unwrap().contains(&p.code), "only its hash is kept");
        assert!(redeem(&dir, "NOTACODE", "", "").unwrap().is_none());
        let (cookie, d) = redeem(&dir, &p.code.to_lowercase(), "Mozilla (iPhone) Safari/604.1", "100.1.2.3").unwrap().unwrap();
        assert_eq!(d.name, "Safari on iPhone");
        assert!(redeem(&dir, &p.code, "", "").unwrap().is_none(), "used");
        let a = read(&dir);
        assert_eq!(check(&a, &cookie).map(|d| d.id.as_str()), Some(d.id.as_str()));
        assert!(check(&a, &format!("{}.{}", d.id, "0".repeat(32))).is_none());
        assert!(check(&a, "junk").is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_new_link_replaces_the_old_and_revoking_ends_a_session() {
        let dir = temp_dir();
        let old = pair_in(&dir).unwrap();
        let new = pair_in(&dir).unwrap();
        assert!(redeem(&dir, &old.code, "", "").unwrap().is_none());
        let (cookie, d) = redeem(&dir, &new.code, "", "").unwrap().unwrap();
        change(&dir, |a| a.devices.retain(|x| x.id != d.id)).unwrap();
        assert!(check(&read(&dir), &cookie).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn what_expired_is_dropped() {
        let dir = temp_dir();
        let p = pair_in(&dir).unwrap();
        change(&dir, |a| a.pairings[0].expires = now() - 1).unwrap();
        assert!(read(&dir).pairings.is_empty());
        assert!(redeem(&dir, &p.code, "", "").unwrap().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn names_devices() {
        let chrome_android = "Mozilla/5.0 (Linux; Android 14) AppleWebKit/537.36 Chrome/129.0 Mobile Safari/537.36";
        assert_eq!(device_name(chrome_android), "Chrome on Android");
        let edge = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/129.0 Safari/537.36 Edg/129.0";
        assert_eq!(device_name(edge), "Edge on Windows");
        let ios_chrome = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) CriOS/129.0 Mobile/15E148 Safari/604.1";
        assert_eq!(device_name(ios_chrome), "Chrome on iPhone");
        assert_eq!(device_name("curl/8"), "Browser");
    }
}
