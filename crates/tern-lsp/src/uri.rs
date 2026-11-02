//! `file://` URIs and paths, both ways (Windows drive letters included).

use std::path::{Component, Path, PathBuf, Prefix};

/// A `file://` URI for `path`, as the editor's go to definition takes it.
pub fn path_to_uri(path: &Path) -> Option<lsp_types::Uri> {
    let mut authority = String::new();
    let mut parts = vec![];
    for c in path.components() {
        match c {
            Component::RootDir => {},
            Component::Prefix(p) => {
                let (server, first) = prefix_parts(p.kind())?;
                authority = server;
                parts.extend(first);
            },
            c => parts.push(c.as_os_str().to_string_lossy().into_owned()),
        }
    }
    let mut s = format!("file://{authority}");
    for part in parts {
        s.push('/');
        for b in part.bytes() {
            if b.is_ascii_alphanumeric() || b"-._~:".contains(&b) {
                s.push(b as char);
            } else {
                s.push_str(&format!("%{b:02X}"));
            }
        }
    }
    s.parse().ok()
}

/// The path of a `file://` URI made by [`path_to_uri`].
pub fn uri_to_path(uri: &lsp_types::Uri) -> Option<PathBuf> {
    let rest = uri.as_str().strip_prefix("file://")?;
    let (authority, rest) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(b) = rest.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let path = String::from_utf8(out).ok()?;
    Some(PathBuf::from(local_path(authority, &path, cfg!(windows))))
}

/// What a Windows path prefix is in a URI: its authority (a UNC server)
/// and first segment (a drive, a UNC share). `None` for device paths.
fn prefix_parts(p: Prefix) -> Option<(String, Option<String>)> {
    let s = |o: &std::ffi::OsStr| o.to_string_lossy().into_owned();
    match p {
        Prefix::Disk(d) | Prefix::VerbatimDisk(d) => Some((String::new(), Some(format!("{}:", d as char)))),
        Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => Some((s(server), Some(s(share)))),
        Prefix::Verbatim(_) | Prefix::DeviceNS(_) => None,
    }
}

/// A URI's path as a local one: `/C:/x` is `C:/x` and `//server/share/x`
/// is `\\server\share\x` on Windows.
fn local_path(authority: &str, path: &str, windows: bool) -> String {
    if !windows {
        return path.to_owned();
    }
    if !authority.is_empty() && !authority.eq_ignore_ascii_case("localhost") {
        return format!(r"\\{authority}{}", path.replace('/', r"\"));
    }
    match path.as_bytes().get(2) {
        Some(b':') => path[1..].to_owned(),
        _ => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_prefixes() {
        use std::ffi::OsStr;
        let unc = Prefix::UNC(OsStr::new("srv"), OsStr::new("share"));
        assert_eq!(prefix_parts(unc), Some(("srv".into(), Some("share".into()))));
        let verbatim_unc = Prefix::VerbatimUNC(OsStr::new("srv"), OsStr::new("share"));
        assert_eq!(prefix_parts(verbatim_unc), Some(("srv".into(), Some("share".into()))));
        assert_eq!(prefix_parts(Prefix::Disk(b'C')), Some((String::new(), Some("C:".into()))));
        assert_eq!(prefix_parts(Prefix::VerbatimDisk(b'C')), Some((String::new(), Some("C:".into()))));
        assert_eq!(prefix_parts(Prefix::DeviceNS(OsStr::new("pipe"))), None);
        assert_eq!(local_path("srv", "/share/a b/x.rs", true), r"\\srv\share\a b\x.rs");
        assert_eq!(local_path("", "/C:/x.rs", true), "C:/x.rs");
        assert_eq!(local_path("", "/x.rs", false), "/x.rs");
    }

    #[test]
    fn uris_round_trip() {
        for p in ["/tmp/a b/ü#?.rs", "/x/y%z.rs"] {
            let uri = path_to_uri(Path::new(p)).unwrap();
            assert!(uri.as_str().starts_with("file:///"));
            assert_eq!(uri_to_path(&uri).unwrap(), PathBuf::from(p));
        }
    }
}
