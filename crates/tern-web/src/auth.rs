//! The access token: random, kept in the daemon's private folder so a
//! bookmarked link keeps working, given once in the link's fragment (never
//! sent in a request line or logged) and then held in an HttpOnly cookie.

use std::io;
use std::path::Path;

const TOKEN_BYTES: usize = 16;

/// The token in `path`, made (or remade, with `renew`) when missing.
pub fn load_token(path: &Path, renew: bool) -> io::Result<String> {
    if !renew
        && let Ok(t) = std::fs::read_to_string(path)
        && valid(t.trim())
    {
        return Ok(t.trim().to_owned());
    }
    let mut b = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut b).map_err(io::Error::other)?;
    let token: String = b.iter().map(|x| format!("{x:02x}")).collect();
    std::fs::write(path, &token)?;
    Ok(token)
}

fn valid(t: &str) -> bool {
    t.len() == TOKEN_BYTES * 2 && t.bytes().all(|b| b.is_ascii_hexdigit())
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_kept_and_renewed() {
        let dir = tempfile_dir();
        let p = dir.join("web-token");
        let a = load_token(&p, false).unwrap();
        assert!(valid(&a));
        assert_eq!(load_token(&p, false).unwrap(), a);
        let b = load_token(&p, true).unwrap();
        assert_ne!(a, b);
        std::fs::write(&p, "junk").unwrap();
        assert_ne!(load_token(&p, false).unwrap(), "junk");
        let _ = std::fs::remove_dir_all(dir);
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let mut b = [0u8; 8];
        getrandom::fill(&mut b).unwrap();
        let d = std::env::temp_dir().join(format!("tern-web-{}", u64::from_le_bytes(b)));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn compares_tokens() {
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
}
