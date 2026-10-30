//! Checks on who's asking: secrets compared in constant time, pages of other
//! sites kept off, and a limit on wrong pairing codes. Who has access is
//! [`crate::access`].

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Wrong pairing codes an address may send in [`TRY_WINDOW`].
const MAX_TRIES: u32 = 10;
const TRY_WINDOW: Duration = Duration::from_secs(10 * 60);

/// Compare without stopping at the first difference.
pub fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Whether `origin` (an `Origin` header) names the `host` the request went
/// to: a page of another site can't open the socket with the cookie.
pub fn same_origin(origin: Option<&str>, host: Option<&str>) -> bool {
    match (origin, host) {
        // Not from a browser page.
        (None, _) => true,
        (Some(o), Some(h)) => o.strip_prefix("http://").or_else(|| o.strip_prefix("https://")).is_some_and(|o| o.eq_ignore_ascii_case(h)),
        (Some(_), None) => false,
    }
}

/// Whether `host` (a `Host` header) names this machine by its loopback
/// address or `localhost`. A page of another site that points its own name
/// at 127.0.0.1 (DNS rebinding) sends that name, and is refused.
pub fn loopback_host(host: Option<&str>) -> bool {
    let Some(host) = host else { return false };
    let name = match host.strip_prefix('[') {
        Some(v6) => v6.split_once(']').map_or("", |(ip, _)| ip),
        None => host.rsplit_once(':').map_or(host, |(name, _)| name),
    };
    name.eq_ignore_ascii_case("localhost") || name.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Wrong pairing codes by address, so they can't be guessed (they're also
/// long, used once and short-lived).
#[derive(Default)]
pub struct Tries(Mutex<HashMap<IpAddr, (u32, Instant)>>);

impl Tries {
    /// Whether `ip` may try a code now.
    pub fn allowed(&self, ip: IpAddr) -> bool {
        let mut m = self.0.lock().unwrap();
        m.retain(|_, (_, since)| since.elapsed() < TRY_WINDOW);
        m.get(&ip).is_none_or(|(n, _)| *n < MAX_TRIES)
    }

    pub fn failed(&self, ip: IpAddr) {
        self.0.lock().unwrap().entry(ip).or_insert((0, Instant::now())).0 += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_secrets() {
        assert!(same("abcd", "abcd"));
        assert!(!same("abcd", "abce"));
        assert!(!same("abc", "abcd"));
    }

    #[test]
    fn checks_the_origin() {
        assert!(same_origin(None, Some("h:1")));
        assert!(same_origin(Some("http://h:1"), Some("h:1")));
        assert!(same_origin(Some("https://H:1"), Some("h:1")));
        assert!(!same_origin(Some("http://evil:1"), Some("h:1")));
        assert!(!same_origin(Some("http://h:1"), None));
        assert!(!same_origin(Some("null"), Some("h:1")));
    }

    #[test]
    fn knows_loopback_hosts() {
        for h in ["127.0.0.1:5500", "127.0.0.1", "localhost:5500", "LOCALHOST", "[::1]:5500", "[::1]"] {
            assert!(loopback_host(Some(h)), "{h}");
        }
        for h in ["evil.example:5500", "evil.example", "localhost.evil.example", "192.168.1.2:5500", "[::1", ""] {
            assert!(!loopback_host(Some(h)), "{h}");
        }
        assert!(!loopback_host(None));
    }

    #[test]
    fn limits_wrong_codes() {
        let t = Tries::default();
        let (a, b): (IpAddr, IpAddr) = ("100.1.1.1".parse().unwrap(), "100.1.1.2".parse().unwrap());
        for _ in 0..MAX_TRIES {
            assert!(t.allowed(a));
            t.failed(a);
        }
        assert!(!t.allowed(a));
        assert!(t.allowed(b));
    }
}
