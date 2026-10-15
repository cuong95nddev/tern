//! Finding the link under the mouse in a line of terminal text: a URL, or a
//! file path with an optional `:line:col` suffix (what Claude Code, compilers
//! and `grep -n` print). Whether a path names an existing file is up to the
//! caller.

use std::ops::Range;
use std::path::PathBuf;

use crate::viewer::normalize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Url(String),
    Path { path: String, line: Option<u32>, col: Option<u32> },
}

const SCHEMES: [&str; 3] = ["https://", "http://", "file://"];

/// The link covering char `ix` of `text`, and the chars it spans.
pub fn find_at(text: &[char], ix: usize) -> Option<(Range<usize>, Link)> {
    if ix >= text.len() {
        return None;
    }
    url_at(text, ix).or_else(|| path_at(text, ix))
}

/// The link an OSC 8 hyperlink points to. Only web and file links: what
/// a program prints is untrusted, and its text can hide the target, so
/// other schemes (`smb:`, `x-apple.systempreferences:`, app URL handlers)
/// are never handed to the OS.
pub fn from_uri(uri: &str) -> Option<Link> {
    let lower = uri.get(..8).unwrap_or(uri).to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("file://")) {
        return None;
    }
    Some(match lower.starts_with("file://").then(|| &uri[7..]) {
        Some(rest) => {
            // `file://host/path`: the host is ours.
            let path = &rest[rest.find('/').unwrap_or(rest.len())..];
            let (path, line) = split_fragment_line(path);
            Link::Path { path: percent_decode(path), line, col: None }
        },
        None => Link::Url(uri.to_owned()),
    })
}

/// `file:///a/b.rs#L12` → (`/a/b.rs`, 12).
fn split_fragment_line(path: &str) -> (&str, Option<u32>) {
    match path.split_once('#') {
        Some((p, frag)) => (p, frag.trim_start_matches('L').parse().ok()),
        None => (path, None),
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn is_url_char(c: char) -> bool {
    c.is_alphanumeric() || "-_./~:@%+=#?&,;!$*()[]{}'".contains(c)
}

/// Paths stop at brackets so `Read(src/main.rs)` and `[x](a.md)` work.
fn is_path_char(c: char) -> bool {
    c.is_alphanumeric() || "-_./~:@%+=#,".contains(c)
}

/// The run of chars around `ix` that satisfy `pred`.
fn span(text: &[char], ix: usize, pred: impl Fn(char) -> bool) -> Option<Range<usize>> {
    if !pred(text[ix]) {
        return None;
    }
    let mut start = ix;
    while start > 0 && pred(text[start - 1]) {
        start -= 1;
    }
    let mut end = ix + 1;
    while end < text.len() && pred(text[end]) {
        end += 1;
    }
    Some(start..end)
}

fn url_at(text: &[char], ix: usize) -> Option<(Range<usize>, Link)> {
    let run = span(text, ix, is_url_char)?;
    let s: String = text[run.clone()].iter().collect();
    // The last scheme starting at or before the mouse.
    let (byte_start, _) = SCHEMES
        .iter()
        .flat_map(|sc| s.match_indices(sc))
        .filter(|(b, _)| run.start + s[..*b].chars().count() <= ix)
        .max_by_key(|(b, _)| *b)?;
    let start = run.start + s[..byte_start].chars().count();
    let mut end = run.end;
    // Trailing punctuation, and a `)` closing a paren opened before the URL.
    loop {
        let c = text[end - 1];
        let unbalanced = |open, close| {
            c == close && text[start..end].iter().filter(|&&x| x == close).count() > text[start..end].iter().filter(|&&x| x == open).count()
        };
        if ".,;:!?'".contains(c) || unbalanced('(', ')') || unbalanced('[', ']') || unbalanced('{', '}') {
            end -= 1;
        } else {
            break;
        }
    }
    let url: String = text[start..end].iter().collect();
    if ix >= end || SCHEMES.iter().any(|sc| url == *sc) {
        return None;
    }
    Some((start..end, from_uri(&url)?))
}

fn path_at(text: &[char], ix: usize) -> Option<(Range<usize>, Link)> {
    let run = span(text, ix, is_path_char)?;
    let (start, mut end) = (run.start, run.end);
    while end > start && ".,:#".contains(text[end - 1]) {
        end -= 1;
    }
    if ix >= end {
        return None;
    }
    let s: String = text[start..end].iter().collect();
    if !s.chars().any(char::is_alphanumeric) {
        return None;
    }
    let (path, line, col) = split_line_col(&s);
    Some((start..end, Link::Path { path: path.to_owned(), line, col }))
}

/// `a.rs:12:5` → (`a.rs`, 12, 5); `a.rs:12` → (`a.rs`, 12, None).
fn split_line_col(s: &str) -> (&str, Option<u32>, Option<u32>) {
    let num = |t: &str| (!t.is_empty() && t.bytes().all(|b| b.is_ascii_digit())).then(|| t.parse().ok()).flatten();
    if let Some((rest, c)) = s.rsplit_once(':')
        && let Some(c) = num(c)
    {
        if let Some((p, l)) = rest.rsplit_once(':')
            && let Some(l) = num(l)
        {
            return (p, Some(l), Some(c));
        }
        return (rest, Some(c), None);
    }
    (s, None, None)
}

/// `path` made absolute against the first of `bases` it exists under
/// (`~` is the home folder); `None` when it exists nowhere. Diff prefixes
/// (`a/src/x.rs`) are tried without the `a/`. A path relative to a project
/// below a base (Claude Code started in a parent folder, printing paths of a
/// repo it `cd`'d into) is looked up in the base's subfolders, two deep.
pub fn resolve(path: &str, bases: &[PathBuf]) -> Option<PathBuf> {
    let expanded = match path.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var_os("HOME")?).join(rest),
        None if path == "~" => PathBuf::from(std::env::var_os("HOME")?),
        None => PathBuf::from(path),
    };
    if expanded.is_absolute() {
        return expanded.exists().then(|| normalize(&expanded));
    }
    if path.starts_with("../") {
        return bases.iter().map(|b| b.join(path)).find(|p| p.exists()).map(|p| normalize(&p));
    }
    let diff_less = path.strip_prefix("a/").or_else(|| path.strip_prefix("b/"));
    let candidates: Vec<&str> = std::iter::once(path).chain(diff_less).collect();
    let find = |dirs: &[PathBuf]| dirs.iter().flat_map(|d| candidates.iter().map(move |p| d.join(p))).find(|p| p.exists());
    let mut level = bases.to_vec();
    for _ in 0..3 {
        if let Some(p) = find(&level) {
            return Some(normalize(&p));
        }
        level = level.iter().flat_map(subdirs).take(400).collect();
    }
    None
}

/// `dir`'s subfolders, minus hidden ones and build/dependency output.
fn subdirs(dir: &PathBuf) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    rd.flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            !name.starts_with('.') && !matches!(name.as_ref(), "target" | "node_modules" | "build" | "dist" | "vendor")
        })
        .map(|e| e.path())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str, needle: &str) -> Option<(String, Link)> {
        let text: Vec<char> = s.chars().collect();
        let ix = s[..s.find(needle).unwrap()].chars().count();
        find_at(&text, ix).map(|(r, l)| (text[r].iter().collect(), l))
    }

    fn path(p: &str, line: Option<u32>, col: Option<u32>) -> Link {
        Link::Path { path: p.into(), line, col }
    }

    #[test]
    fn urls() {
        assert_eq!(at("see https://x.dev/a?b=1.", "x.dev"), Some(("https://x.dev/a?b=1".into(), Link::Url("https://x.dev/a?b=1".into()))));
        assert_eq!(at("(https://x.dev/a_(b))", "x.dev").unwrap().0, "https://x.dev/a_(b)");
        assert_eq!(at("[doc](https://x.dev/y)", "x.dev").unwrap().0, "https://x.dev/y");
        assert_eq!(at("go file:///tmp/a%20b.rs#L3 now", "tmp").unwrap().1, path("/tmp/a b.rs", Some(3), None));
    }

    #[test]
    fn hyperlinks_only_web_and_file() {
        assert_eq!(from_uri("https://x.dev"), Some(Link::Url("https://x.dev".into())));
        assert_eq!(from_uri("HTTP://x.dev"), Some(Link::Url("HTTP://x.dev".into())));
        assert_eq!(from_uri("FILE:///a.rs"), Some(path("/a.rs", None, None)));
        for bad in ["smb://evil/share", "x-apple.systempreferences:com.apple", "javascript:alert(1)", "vscode://file/x", "ssh://h"] {
            assert_eq!(from_uri(bad), None, "{bad}");
        }
    }

    #[test]
    fn paths() {
        assert_eq!(at("⎿ Read(crates/a/src/main.rs)", "main"), Some(("crates/a/src/main.rs".into(), path("crates/a/src/main.rs", None, None))));
        assert_eq!(at("Updated `src/lib.rs:42` and", "lib"), Some(("src/lib.rs:42".into(), path("src/lib.rs", Some(42), None))));
        assert_eq!(at("error at src/x.rs:3:14: oops", "x.rs"), Some(("src/x.rs:3:14".into(), path("src/x.rs", Some(3), Some(14)))));
        assert_eq!(at("in ~/ws/Tiếng việt.md.", "ws"), Some(("~/ws/Tiếng".into(), path("~/ws/Tiếng", None, None))));
        assert_eq!(at("see README.md.", "README"), Some(("README.md".into(), path("README.md", None, None))));
        assert_eq!(at("a ... b", "..."), None);
        assert_eq!(at("a b", " "), None);
    }

    #[test]
    fn resolves_against_bases() {
        let root = std::env::temp_dir().join(format!("tern-links-{}", std::process::id()));
        let dir = root.join("repo");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(root.join("target/src")).unwrap();
        std::fs::write(dir.join("src/x.rs"), "").unwrap();
        let bases = [PathBuf::from("/nonexistent"), dir.clone()];
        assert_eq!(resolve("src/x.rs", &bases), Some(dir.join("src/x.rs")));
        assert_eq!(resolve("./src/../src/x.rs", &bases), Some(dir.join("src/x.rs")));
        assert_eq!(resolve("b/src/x.rs", &bases), Some(dir.join("src/x.rs")));
        assert_eq!(resolve("src/y.rs", &bases), None);
        // Printed relative to a project below the folder the program runs in.
        assert_eq!(resolve("src/x.rs", std::slice::from_ref(&root)), Some(dir.join("src/x.rs")));
        assert_eq!(resolve("repo/src/x.rs", std::slice::from_ref(&root)), Some(dir.join("src/x.rs")));
        assert_eq!(resolve("../repo/src/x.rs", &[root.join("target")]), Some(dir.join("src/x.rs")));
        std::fs::remove_dir_all(root).unwrap();
    }
}
