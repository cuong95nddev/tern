//! Go to definition without a language server: each grammar's tags query
//! finds the definitions (types, functions, ...) in the workspace's files,
//! and a symbol jumps to the definitions with its name.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use gpui_kit::component::input::Rope;
use gpui_kit::{App, AppContext};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

/// Files indexed in a workspace, at most.
const MAX_FILES: usize = 50_000;
/// Larger files are generated or vendored more often than not; skip them.
const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// Definitions handed back for one symbol, at most.
const MAX_RESULTS: usize = 50;

/// Definitions the stock tags queries leave out (Rust constants, TypeScript
/// type aliases and enums, ...).
const RUST_EXTRA: &str = r#"
(const_item name: (identifier) @name) @definition.constant
(static_item name: (identifier) @name) @definition.constant
(associated_type name: (type_identifier) @name) @definition.type
"#;
const TS_EXTRA: &str = r#"
(type_alias_declaration name: (type_identifier) @name) @definition.type
(enum_declaration name: (identifier) @name) @definition.enum
"#;
const CPP_EXTRA: &str = r#"
(namespace_definition name: (namespace_identifier) @name) @definition.module
(alias_declaration name: (type_identifier) @name) @definition.type
"#;

/// A grammar and its definitions query.
struct Lang {
    /// Files of one family can see each other's definitions (`.ts` and `.js`).
    family: &'static str,
    language: tree_sitter::Language,
    query: Query,
    /// The `@name` capture.
    name: u32,
    /// Per pattern: does it capture a definition (not a reference)?
    defines: Vec<bool>,
}

impl Lang {
    fn new(family: &'static str, language: tree_sitter::Language, queries: &[&str]) -> Self {
        let query = Query::new(&language, &queries.concat()).unwrap_or_else(|e| panic!("{family} tags query: {e}"));
        let name = query.capture_index_for_name("name").expect("tags query captures @name");
        let defines = (0..query.pattern_count())
            .map(|p| {
                let quantifiers = query.capture_quantifiers(p);
                query
                    .capture_names()
                    .iter()
                    .zip(quantifiers)
                    .any(|(n, q)| n.starts_with("definition.") && *q != tree_sitter::CaptureQuantifier::Zero)
            })
            .collect();
        Self { family, language, query, name, defines }
    }
}

/// The grammars with a tags query, by key.
fn langs() -> &'static HashMap<&'static str, Lang> {
    static LANGS: OnceLock<HashMap<&'static str, Lang>> = OnceLock::new();
    LANGS.get_or_init(|| {
        let js = tree_sitter_javascript::TAGS_QUERY;
        let ts = tree_sitter_typescript::TAGS_QUERY;
        HashMap::from([
            ("rust", Lang::new("rust", tree_sitter_rust::LANGUAGE.into(), &[tree_sitter_rust::TAGS_QUERY, RUST_EXTRA])),
            ("c", Lang::new("c", tree_sitter_c::LANGUAGE.into(), &[tree_sitter_c::TAGS_QUERY])),
            ("cpp", Lang::new("c", tree_sitter_cpp::LANGUAGE.into(), &[tree_sitter_cpp::TAGS_QUERY, CPP_EXTRA])),
            ("go", Lang::new("go", tree_sitter_go::LANGUAGE.into(), &[tree_sitter_go::TAGS_QUERY])),
            ("java", Lang::new("java", tree_sitter_java::LANGUAGE.into(), &[tree_sitter_java::TAGS_QUERY])),
            ("javascript", Lang::new("js", tree_sitter_javascript::LANGUAGE.into(), &[js])),
            // TypeScript's own query only adds to JavaScript's.
            ("typescript", Lang::new("js", tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), &[js, ts, TS_EXTRA])),
            ("tsx", Lang::new("js", tree_sitter_typescript::LANGUAGE_TSX.into(), &[js, ts, TS_EXTRA])),
            ("python", Lang::new("python", tree_sitter_python::LANGUAGE.into(), &[tree_sitter_python::TAGS_QUERY])),
            ("swift", Lang::new("swift", tree_sitter_swift::LANGUAGE.into(), &[tree_sitter_swift::TAGS_QUERY])),
        ])
    })
}

/// The grammar for `path`, by its extension.
fn lang_for(path: &Path) -> Option<&'static Lang> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let key = match ext.as_str() {
        "rs" => "rust",
        "c" => "c",
        // Headers are parsed as C++, which reads C headers too.
        "h" | "hh" | "hpp" | "hxx" | "cc" | "cpp" | "cxx" | "c++" | "ipp" => "cpp",
        "go" => "go",
        "java" => "java",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "py" | "pyi" => "python",
        "swift" => "swift",
        _ => return None,
    };
    langs().get(key)
}

/// Whether `path` is in a language that has definitions to jump to.
pub fn supports(path: &Path) -> bool {
    lang_for(path).is_some()
}

/// A definition: its name and where the name is (0-based line, columns in
/// characters, as the editor counts them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Def {
    pub name: String,
    pub line: u32,
    pub col: u32,
    pub end_col: u32,
}

/// The definitions in `src`, parsed as `lang`.
fn extract(lang: &Lang, src: &str) -> Vec<Def> {
    let mut parser = Parser::new();
    if parser.set_language(&lang.language).is_err() {
        return vec![];
    }
    let Some(tree) = parser.parse(src, None) else { return vec![] };
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&lang.query, tree.root_node(), src.as_bytes());
    let mut defs = vec![];
    let mut seen = HashSet::new();
    while let Some(m) = matches.next() {
        if !lang.defines[m.pattern_index] {
            continue;
        }
        for c in m.captures.iter().filter(|c| c.index == lang.name) {
            let (start, end) = (c.node.start_byte(), c.node.end_byte());
            // A method is matched as a method and as a function: keep one.
            if end <= start || !seen.insert(start) {
                continue;
            }
            let pos = c.node.start_position();
            let line_start = start - pos.column;
            let Some(name) = src.get(start..end) else { continue };
            let col = src.get(line_start..start).map_or(0, |s| s.chars().count()) as u32;
            defs.push(Def { name: name.to_string(), line: pos.row as u32, col, end_col: col + name.chars().count() as u32 });
        }
    }
    defs.sort_by_key(|d| (d.line, d.col));
    defs
}

/// The definitions in the file at `path`; none if it can't be read or is too large.
fn extract_file(lang: &Lang, path: &Path) -> Vec<Def> {
    let Ok(meta) = std::fs::metadata(path) else { return vec![] };
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return vec![];
    }
    let Ok(bytes) = std::fs::read(path) else { return vec![] };
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return vec![];
    }
    extract(lang, &String::from_utf8_lossy(&bytes))
}

/// The open file's definitions, with the path and text they were parsed from.
struct Current {
    path: PathBuf,
    text: Rope,
    defs: Arc<Vec<Def>>,
}

#[derive(Default)]
struct State {
    files: HashMap<PathBuf, Vec<Def>>,
    /// Changed on disk since they were read: read again before the next lookup.
    stale: HashSet<PathBuf>,
}

/// The definitions in one workspace's files, built in the background on first use.
pub struct Index {
    root: PathBuf,
    started: AtomicBool,
    state: Mutex<State>,
    /// The open file's definitions, from its (maybe unsaved) text.
    current: Mutex<Option<Current>>,
}

impl Index {
    pub fn new(root: PathBuf) -> Arc<Self> {
        Arc::new(Self { root, started: AtomicBool::new(false), state: Mutex::default(), current: Mutex::default() })
    }

    /// Start reading the workspace's files, once.
    pub fn warm(self: &Arc<Self>, cx: &App) {
        if self.started.swap(true, Ordering::Relaxed) {
            return;
        }
        let this = self.clone();
        cx.background_spawn(async move { this.build() }).detach();
    }

    fn build(&self) {
        let files: Vec<(PathBuf, &'static Lang)> = tern_fs::search::files(&self.root, MAX_FILES, &AtomicBool::new(false))
            .into_iter()
            .filter_map(|rel| {
                let path = self.root.join(rel);
                lang_for(&path).map(|l| (path, l))
            })
            .collect();
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(8);
        let chunk = files.len().div_ceil(threads).max(1);
        let parsed: Vec<(PathBuf, Vec<Def>)> = std::thread::scope(|s| {
            let handles: Vec<_> = files
                .chunks(chunk)
                .map(|part| s.spawn(move || part.iter().map(|(p, l)| (p.clone(), extract_file(l, p))).collect::<Vec<_>>()))
                .collect();
            handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
        });
        let mut state = self.state.lock().unwrap();
        for (path, defs) in parsed {
            // A file changed during the build stays stale and is read again.
            state.files.entry(path).or_insert(defs);
        }
    }

    /// Files changed on disk: read them again before the next lookup.
    pub fn changed(&self, paths: &[PathBuf]) {
        let mut state = self.state.lock().unwrap();
        state.stale.extend(paths.iter().filter(|p| supports(p)).cloned());
    }

    /// The definitions in `text`, the open file at `path`; parsed again only
    /// when the text changed.
    fn current_defs(&self, path: &Path, text: &Rope) -> Arc<Vec<Def>> {
        let mut current = self.current.lock().unwrap();
        if let Some(c) = current.as_ref()
            && c.path == path
            && c.text == *text
        {
            return c.defs.clone();
        }
        let defs = Arc::new(lang_for(path).map(|l| extract(l, &text.to_string())).unwrap_or_default());
        *current = Some(Current { path: path.to_path_buf(), text: text.clone(), defs: defs.clone() });
        defs
    }

    /// The definitions of `name`, for the symbol at `at` (line, column) in
    /// `text`, the open file at `from`: the open file's first, then the files
    /// closest to it. The definition at `at` itself is left out.
    pub fn lookup(&self, name: &str, from: &Path, text: &Rope, at: (u32, u32)) -> Vec<(PathBuf, Def)> {
        let Some(lang) = lang_for(from) else { return vec![] };
        let here = self.current_defs(from, text);
        let mut hits: Vec<(PathBuf, Def)> = here
            .iter()
            .filter(|d| d.name == name && !(d.line == at.0 && (d.col..=d.end_col).contains(&at.1)))
            .map(|d| (from.to_path_buf(), d.clone()))
            .collect();
        let mut state = self.state.lock().unwrap();
        for path in std::mem::take(&mut state.stale) {
            match lang_for(&path) {
                Some(l) if path.is_file() => {
                    let defs = extract_file(l, &path);
                    state.files.insert(path, defs);
                },
                _ => {
                    state.files.remove(&path);
                },
            }
        }
        let mut others: Vec<(PathBuf, Def)> = state
            .files
            .iter()
            .filter(|(p, _)| p.as_path() != from && lang_for(p).is_some_and(|l| l.family == lang.family))
            .flat_map(|(p, defs)| defs.iter().filter(|d| d.name == name).map(move |d| (p.clone(), d.clone())))
            .collect();
        drop(state);
        others.sort_by(|(a, da), (b, db)| {
            shared_dirs(from, b).cmp(&shared_dirs(from, a)).then_with(|| a.cmp(b)).then_with(|| (da.line, da.col).cmp(&(db.line, db.col)))
        });
        hits.extend(others);
        hits.truncate(MAX_RESULTS);
        hits
    }
}

/// How many leading path components `a` and `b` have in common.
fn shared_dirs(a: &Path, b: &Path) -> usize {
    a.components().zip(b.components()).take_while(|(x, y)| x == y).count()
}

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

    fn names(path: &str, src: &str) -> Vec<String> {
        extract(lang_for(Path::new(path)).unwrap(), src).into_iter().map(|d| d.name).collect()
    }

    #[test]
    fn every_query_compiles() {
        assert_eq!(langs().len(), 10);
    }

    #[test]
    fn rust_definitions() {
        let src = "struct Foo;\nenum E { A }\ntrait T {}\ntype Alias = u8;\nconst MAX: u8 = 1;\nimpl Foo {\n    fn new() -> Foo { bar() }\n}\nfn bar() {}\n";
        assert_eq!(names("a.rs", src), ["Foo", "E", "T", "Alias", "MAX", "new", "bar"]);
    }

    #[test]
    fn typescript_definitions() {
        let src = "interface I {}\ntype T = string;\nenum E { A }\nclass C { m() {} }\nfunction f() {}\nconst g = () => 1;\n";
        assert_eq!(names("a.ts", src), ["I", "T", "E", "C", "m", "f", "g"]);
    }

    #[test]
    fn other_languages() {
        assert_eq!(names("a.py", "class A:\n    def m(self): pass\ndef f(): pass\n"), ["A", "m", "f"]);
        assert_eq!(names("a.go", "package p\ntype S struct{}\nfunc F() {}\n"), ["S", "F"]);
        assert_eq!(names("A.java", "class A { void m() {} }\n"), ["A", "m"]);
        assert_eq!(names("a.hpp", "namespace n { class K {}; }\n"), ["n", "K"]);
    }

    #[test]
    fn columns_count_characters() {
        let defs = extract(lang_for(Path::new("a.rs")).unwrap(), "// é\n/* é */ struct Foo;\n");
        assert_eq!(defs, [Def { name: "Foo".into(), line: 1, col: 15, end_col: 18 }]);
    }

    #[test]
    fn lookup_prefers_the_open_file_then_the_closest() {
        let base = std::env::temp_dir().join(format!("tern-symbols-{}", std::process::id()));
        std::fs::create_dir_all(base.join("a/b")).unwrap();
        std::fs::create_dir_all(base.join("z")).unwrap();
        std::fs::write(base.join("a/b/near.rs"), "struct Foo;\n").unwrap();
        std::fs::write(base.join("z/far.rs"), "struct Foo;\n").unwrap();
        std::fs::write(base.join("a/b/other.py"), "class Foo: pass\n").unwrap();
        let index = Index::new(base.clone());
        index.build();
        let from = base.join("a/b/main.rs");
        let text = Rope::from("fn main() { let x: Foo; }\nstruct Foo;\n");

        let hits = index.lookup("Foo", &from, &text, (0, 20));
        let paths: Vec<_> = hits.iter().map(|(p, _)| p.strip_prefix(&base).unwrap().to_path_buf()).collect();
        assert_eq!(paths, [PathBuf::from("a/b/main.rs"), "a/b/near.rs".into(), "z/far.rs".into()]);
        assert_eq!(hits[0].1.line, 1);

        // On the definition itself: the others.
        let hits = index.lookup("Foo", &from, &text, (1, 8));
        assert_eq!(hits.len(), 2);

        // A file changed on disk is read again.
        std::fs::write(base.join("z/far.rs"), "struct Bar;\n").unwrap();
        index.changed(&[base.join("z/far.rs")]);
        assert_eq!(index.lookup("Foo", &from, &text, (1, 8)).len(), 1);
        std::fs::remove_dir_all(&base).unwrap();
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
