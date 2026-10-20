//! Search the contents of a folder's files, honoring .gitignore like the
//! explorer: the lines a query matches, grouped by file, at most `limit`.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ignore::{WalkBuilder, WalkState};
use regex::{Regex, RegexBuilder};

/// Files bigger than this are skipped: generated or data, not code.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// At most this many matching lines from one file.
const MAX_LINES_PER_FILE: usize = 100;
/// A line's text is kept up to this many bytes (minified files).
const MAX_LINE_BYTES: usize = 1000;
/// A NUL in this many first bytes means a binary file.
const BINARY_PROBE: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Query {
    pub text: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
}

/// A matching line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// Relative to the folder searched.
    pub path: PathBuf,
    /// 1-based.
    pub line: u32,
    /// The line, without its end of line (cut when very long).
    pub text: String,
    /// Byte ranges of `text` that matched, in order.
    pub ranges: Vec<Range<usize>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Results {
    /// By path, then line.
    pub matches: Vec<Match>,
    /// More lines matched than `limit` (or the search was stopped).
    pub truncated: bool,
}

/// The pattern `q` searches with; an error for a bad regular expression.
pub fn pattern(q: &Query) -> Result<Regex, String> {
    let source = if q.regex { q.text.clone() } else { regex::escape(&q.text) };
    RegexBuilder::new(&source).case_insensitive(!q.case_sensitive).multi_line(false).build().map_err(|e| match e {
        regex::Error::Syntax(s) => s.lines().last().unwrap_or("invalid regular expression").trim_start_matches("error: ").to_owned(),
        e => e.to_string(),
    })
}

/// Search the files under `root` for `q`. Stops early once `cancel` is set
/// (the results so far come back, `truncated`).
pub fn search(root: &Path, q: &Query, limit: usize, cancel: &AtomicBool) -> Result<Results, String> {
    if q.text.is_empty() {
        return Ok(Results::default());
    }
    let re = pattern(q)?;
    let found = Mutex::new(Vec::new());
    let count = AtomicUsize::new(0);
    let truncated = AtomicBool::new(false);
    walker(root).build_parallel().run(|| {
        Box::new(|entry| {
            if cancel.load(Ordering::Relaxed) || count.load(Ordering::Relaxed) >= limit {
                truncated.store(true, Ordering::Relaxed);
                return WalkState::Quit;
            }
            let Ok(entry) = entry else { return WalkState::Continue };
            if !entry.file_type().is_some_and(|t| t.is_file()) || entry.metadata().is_ok_and(|m| m.len() > MAX_FILE_BYTES) {
                return WalkState::Continue;
            }
            let lines = search_file(entry.path(), &re, q.whole_word);
            if !lines.is_empty() {
                count.fetch_add(lines.len(), Ordering::Relaxed);
                let rel = entry.path().strip_prefix(root).unwrap_or(entry.path()).to_path_buf();
                found.lock().unwrap().push((rel, lines));
            }
            WalkState::Continue
        })
    });
    let mut files = found.into_inner().unwrap();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut matches: Vec<Match> = files
        .into_iter()
        .flat_map(|(path, lines)| lines.into_iter().map(move |(line, text, ranges)| Match { path: path.clone(), line, text, ranges }))
        .collect();
    let truncated = truncated.into_inner() || matches.len() > limit;
    matches.truncate(limit);
    Ok(Results { matches, truncated })
}

/// The files under `root` (relative to it), sorted, at most `limit`; empty
/// once `cancel` is set. Hidden files are in, ignored ones and `.git` out.
pub fn files(root: &Path, limit: usize, cancel: &AtomicBool) -> Vec<PathBuf> {
    let found = Mutex::new(Vec::new());
    let count = AtomicUsize::new(0);
    walker(root).build_parallel().run(|| {
        Box::new(|entry| {
            if cancel.load(Ordering::Relaxed) || count.load(Ordering::Relaxed) >= limit {
                return WalkState::Quit;
            }
            let Ok(entry) = entry else { return WalkState::Continue };
            if entry.file_type().is_some_and(|t| t.is_file()) {
                count.fetch_add(1, Ordering::Relaxed);
                let rel = entry.path().strip_prefix(root).unwrap_or(entry.path()).to_path_buf();
                found.lock().unwrap().push(rel);
            }
            WalkState::Continue
        })
    });
    if cancel.load(Ordering::Relaxed) {
        return vec![];
    }
    let mut files = found.into_inner().unwrap();
    files.sort();
    files.truncate(limit);
    files
}

/// A walk of `root` as the explorer shows it: .gitignore honored, hidden
/// files in, `.git` out.
fn walker(root: &Path) -> WalkBuilder {
    let mut w = WalkBuilder::new(root);
    w.hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .require_git(false)
        .filter_entry(|e| e.file_name() != ".git" && e.file_name() != ".DS_Store");
    w
}

/// The lines of the file at `path` that `re` matches: number, text, ranges.
fn search_file(path: &Path, re: &Regex, whole_word: bool) -> Vec<(u32, String, Vec<Range<usize>>)> {
    let Ok(bytes) = std::fs::read(path) else { return vec![] };
    if bytes[..bytes.len().min(BINARY_PROBE)].contains(&0) {
        return vec![];
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut out = vec![];
    for (i, line) in text.lines().enumerate() {
        let ranges: Vec<Range<usize>> =
            re.find_iter(line).map(|m| m.range()).filter(|r| !r.is_empty() && (!whole_word || is_whole_word(line, r))).collect();
        if ranges.is_empty() {
            continue;
        }
        let (line, ranges) = clip(line, ranges);
        out.push((i as u32 + 1, line, ranges));
        if out.len() >= MAX_LINES_PER_FILE {
            break;
        }
    }
    out
}

/// `line` cut to [`MAX_LINE_BYTES`] (on a char boundary), with the ranges
/// that still fit in it.
fn clip(line: &str, ranges: Vec<Range<usize>>) -> (String, Vec<Range<usize>>) {
    if line.len() <= MAX_LINE_BYTES {
        return (line.to_owned(), ranges);
    }
    let end = (0..=MAX_LINE_BYTES).rev().find(|&i| line.is_char_boundary(i)).unwrap_or(0);
    let ranges = ranges.into_iter().filter(|r| r.start < end).map(|r| r.start..r.end.min(end)).collect();
    (line[..end].to_owned(), ranges)
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether `r` of `line` stands as a whole word. Checked after matching,
/// not with `\b` in the pattern: `\b` never matches next to a query's
/// punctuation (`.foo`), and the query's own edges may not be word chars.
fn is_whole_word(line: &str, r: &Range<usize>) -> bool {
    let (before, inner, after) = (line[..r.start].chars().next_back(), &line[r.clone()], line[r.end..].chars().next());
    let left = before.is_none_or(|b| !is_word(b)) || inner.chars().next().is_none_or(|c| !is_word(c));
    let right = after.is_none_or(|a| !is_word(a)) || inner.chars().next_back().is_none_or(|c| !is_word(c));
    left && right
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::AtomicBool;

    use super::*;

    fn fixture() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        fs::create_dir_all(r.join("src")).unwrap();
        fs::create_dir_all(r.join("target")).unwrap();
        fs::create_dir_all(r.join(".git")).unwrap();
        fs::write(r.join(".gitignore"), "target/\n").unwrap();
        fs::write(r.join("src/a.rs"), "fn foo() {}\nlet foobar = Foo;\n// foo foo\n").unwrap();
        fs::write(r.join("src/b.rs"), "nothing here\n").unwrap();
        fs::write(r.join("target/out.rs"), "foo\n").unwrap();
        fs::write(r.join(".git/config"), "foo\n").unwrap();
        fs::write(r.join("bin.dat"), b"foo\0\x01").unwrap();
        d
    }

    fn run(root: &Path, text: &str, f: impl FnOnce(&mut Query)) -> Results {
        let mut q = Query { text: text.into(), ..Default::default() };
        f(&mut q);
        search(root, &q, 500, &AtomicBool::new(false)).unwrap()
    }

    fn lines(r: &Results) -> Vec<(String, u32)> {
        r.matches.iter().map(|m| (m.path.display().to_string(), m.line)).collect()
    }

    #[test]
    fn finds_lines_skipping_ignored_and_binary_files() {
        let d = fixture();
        let r = run(d.path(), "foo", |_| {});
        assert_eq!(lines(&r), [("src/a.rs".into(), 1), ("src/a.rs".into(), 2), ("src/a.rs".into(), 3)]);
        assert_eq!(r.matches[1].ranges, [4..7, 13..16]);
        assert_eq!(r.matches[2].ranges, [3..6, 7..10]);
        assert!(!r.truncated);
    }

    #[test]
    fn case_word_and_regex() {
        let d = fixture();
        assert_eq!(lines(&run(d.path(), "Foo", |q| q.case_sensitive = true)), [("src/a.rs".into(), 2)]);
        let word = run(d.path(), "foo", |q| q.whole_word = true);
        assert_eq!(lines(&word), [("src/a.rs".into(), 1), ("src/a.rs".into(), 2), ("src/a.rs".into(), 3)]);
        // `foobar` is not the word, `Foo` is.
        assert_eq!(word.matches[1].ranges, [Range { start: 13, end: 16 }]);
        assert_eq!(lines(&run(d.path(), r"fn \w+\(", |q| q.regex = true)), [("src/a.rs".into(), 1)]);
        // Without regex, the query is literal.
        assert!(run(d.path(), "fn \\w+", |_| {}).matches.is_empty());
    }

    #[test]
    fn bad_regex_is_an_error() {
        let q = Query { text: "foo(".into(), regex: true, ..Default::default() };
        assert!(search(Path::new("."), &q, 10, &AtomicBool::new(false)).is_err());
    }

    #[test]
    fn stops_at_the_limit() {
        let d = fixture();
        let q = Query { text: "foo".into(), ..Default::default() };
        let r = search(d.path(), &q, 2, &AtomicBool::new(false)).unwrap();
        assert_eq!(r.matches.len(), 2);
        assert!(r.truncated);
    }

    #[test]
    fn lists_files_skipping_ignored_ones() {
        let d = fixture();
        let names: Vec<String> = files(d.path(), 100, &AtomicBool::new(false)).iter().map(|p| p.display().to_string()).collect();
        assert_eq!(names, [".gitignore", "bin.dat", "src/a.rs", "src/b.rs"]);
        assert_eq!(files(d.path(), 2, &AtomicBool::new(false)).len(), 2);
    }

    #[test]
    fn whole_word_edges() {
        assert!(is_whole_word("a.foo b", &(1..5)));
        assert!(!is_whole_word("xfoo", &(1..4)));
        assert!(is_whole_word("foo_bar", &(0..7)));
        assert!(!is_whole_word("foo_bar", &(0..3)));
    }

    #[test]
    fn long_lines_are_cut_on_a_char_boundary() {
        let line = "é".repeat(MAX_LINE_BYTES);
        let (cut, ranges) = clip(&line, vec![0..2, MAX_LINE_BYTES + 2..MAX_LINE_BYTES + 4]);
        assert!(cut.len() <= MAX_LINE_BYTES);
        assert_eq!(ranges, [Range { start: 0, end: 2 }]);
    }
}
