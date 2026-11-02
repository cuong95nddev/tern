//! `file://` URIs and paths, both ways (Windows drive letters included).

use std::path::{Component, Path, PathBuf};

/// A `file://` URI for `path`, as the editor's go to definition takes it.
pub fn path_to_uri(path: &Path) -> Option<lsp_types::Uri> {
    let mut s = String::from("file://");
    for c in path.components() {
        let part = match c {
            Component::RootDir => continue,
            Component::Prefix(p) => p.as_os_str().to_string_lossy().replace('\\', "/"),
            c => c.as_os_str().to_string_lossy().into_owned(),
        };
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
    // `/C:/x` on Windows is `C:/x`.
    if cfg!(windows) && path.as_bytes().get(2) == Some(&b':') {
        return Some(PathBuf::from(&path[1..]));
    }
    Some(PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_round_trip() {
        for p in ["/tmp/a b/ü#?.rs", "/x/y%z.rs"] {
            let uri = path_to_uri(Path::new(p)).unwrap();
            assert!(uri.as_str().starts_with("file:///"));
            assert_eq!(uri_to_path(&uri).unwrap(), PathBuf::from(p));
        }
    }
}
