//! Inheritance gutter icons of a file tab ("is implemented by" ↓,
//! "implements" ↑), after OpenIDEA's `implementationGutter.ts`: which
//! declarations to ask about, the answers cached per document text, and the
//! pass that asks the language server. The viewer schedules passes and draws
//! what [`Markers::shown`] gives.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use lsp_types::{DocumentSymbol, Position};
use tern_lsp::nav::{self, Candidate, LineMarker, SuperDeps};
use tern_lsp::{HierarchyItem, LspError, Manager};

/// Lines above and below the viewport whose declarations are asked about.
const MARGIN_LINES: u32 = 60;
/// Declarations asked about in one pass, at most.
const MAX_PER_PASS: usize = 60;
/// Quiet time after the last edit or scroll before a pass.
pub const DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(500);

/// The two kinds of icons, each turned off on its own when the server can't answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Group {
    /// ↓ from implementation requests.
    Inheritors,
    /// ↑ from the type hierarchy.
    Supers,
}

#[derive(Debug, Default, Clone)]
struct Entry {
    /// `Some(None)`: asked, nothing to show.
    inheritors: Option<Option<LineMarker>>,
    supers: Option<Option<LineMarker>>,
}

/// What a pass found, for [`Markers::apply`].
#[derive(Debug, Default)]
pub struct Filled {
    pub inheritors: Vec<(String, Option<LineMarker>)>,
    pub supers: Vec<(String, Option<LineMarker>)>,
    pub unsupported: Vec<Group>,
}

/// A file tab's gutter icon state. Everything is for one document text: an
/// edit drops it (the editor keeps the icons on their lines meanwhile).
#[derive(Default)]
pub struct Markers {
    entries: HashMap<String, Entry>,
    unsupported: HashSet<Group>,
    outline: Option<(Vec<DocumentSymbol>, Vec<Candidate>)>,
    /// Bumped by every pass and edit: a pass that sees another one when it
    /// ends is stale.
    generation: u64,
}

impl Markers {
    /// The text changed: what was known is for the old one.
    pub fn note_edit(&mut self) {
        self.generation += 1;
        self.invalidate();
    }

    /// Forget the answers (the server finished indexing, or another file may
    /// have added an implementation): the next pass asks again.
    pub fn invalidate(&mut self) {
        self.entries.clear();
        self.unsupported.clear();
        self.outline = None;
    }

    /// Start a pass; its generation.
    pub fn begin(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    pub fn is_current(&self, generation: u64) -> bool {
        generation == self.generation
    }

    pub fn outline(&self) -> Option<&(Vec<DocumentSymbol>, Vec<Candidate>)> {
        self.outline.as_ref()
    }

    /// The outline a pass of `generation` got; dropped if stale.
    pub fn set_outline(&mut self, generation: u64, outline: Vec<DocumentSymbol>, candidates: Vec<Candidate>) -> bool {
        if !self.is_current(generation) {
            return false;
        }
        self.outline = Some((outline, candidates));
        true
    }

    /// Of `visible`, those still to ask about: (↓ implementations, ↑ super methods).
    pub fn wanted(&self, visible: &[Candidate]) -> (Vec<Candidate>, Vec<Candidate>) {
        let entry = |c: &Candidate| self.entries.get(&c.key());
        let down = if self.unsupported.contains(&Group::Inheritors) {
            vec![]
        } else {
            visible.iter().filter(|c| c.inheritable && entry(c).is_none_or(|e| e.inheritors.is_none())).cloned().collect()
        };
        let up = if self.unsupported.contains(&Group::Supers) {
            vec![]
        } else {
            visible.iter().filter(|c| c.overriding && c.owner.is_some() && entry(c).is_none_or(|e| e.supers.is_none())).cloned().collect()
        };
        (down, up)
    }

    /// Keep what a pass of `generation` found; dropped (false) if stale.
    pub fn apply(&mut self, generation: u64, filled: Filled) -> bool {
        if !self.is_current(generation) {
            return false;
        }
        for (key, m) in filled.inheritors {
            self.entries.entry(key).or_default().inheritors = Some(m);
        }
        for (key, m) in filled.supers {
            self.entries.entry(key).or_default().supers = Some(m);
        }
        self.unsupported.extend(filled.unsupported);
        true
    }

    /// The icons known for the document, in its order (↓ before ↑ on a line).
    pub fn shown(&self) -> Vec<LineMarker> {
        let Some((_, candidates)) = &self.outline else { return vec![] };
        let mut out = vec![];
        for c in candidates {
            let Some(e) = self.entries.get(&c.key()) else { continue };
            out.extend(e.inheritors.iter().flatten().cloned());
            out.extend(e.supers.iter().flatten().cloned());
        }
        out
    }
}

/// The candidates within [`MARGIN_LINES`] of the viewport (`first..=last`), at most [`MAX_PER_PASS`].
pub fn visible_candidates(c: &[Candidate], first: u32, last: u32) -> Vec<Candidate> {
    let (from, to) = (first.saturating_sub(MARGIN_LINES), last.saturating_add(MARGIN_LINES));
    c.iter().filter(|c| c.line >= from && c.line <= to).take(MAX_PER_PASS).cloned().collect()
}

/// The server can't answer this group for the file (until it changes).
fn unavailable(e: &LspError) -> bool {
    matches!(e, LspError::NotFound | LspError::Unavailable(_) | LspError::Unsupported)
}

/// The server calls of one super method search, each asked once a pass; this
/// file's outline is the one known already.
struct PassDeps<'a> {
    lsp: &'a Manager,
    path: &'a Path,
    supertypes: Mutex<HashMap<(PathBuf, u32, u32), Vec<HierarchyItem>>>,
    outlines: Mutex<HashMap<PathBuf, Vec<DocumentSymbol>>>,
    /// No type hierarchy for this file.
    unsupported: AtomicBool,
}

impl SuperDeps for PassDeps<'_> {
    async fn supertypes(&self, path: &Path, pos: Position) -> Vec<HierarchyItem> {
        let key = (path.to_path_buf(), pos.line, pos.character);
        if let Some(found) = self.supertypes.lock().unwrap().get(&key).cloned() {
            return found;
        }
        let found = match self.lsp.supertypes(path, pos).await {
            Ok(found) => found,
            Err(e) => {
                if path == self.path && unavailable(&e) {
                    self.unsupported.store(true, Ordering::Relaxed);
                }
                vec![]
            },
        };
        self.supertypes.lock().unwrap().insert(key, found.clone());
        found
    }

    async fn outline(&self, path: &Path) -> Vec<DocumentSymbol> {
        if let Some(found) = self.outlines.lock().unwrap().get(path).cloned() {
            return found;
        }
        let found = self.lsp.outline(path).await.unwrap_or_default();
        self.outlines.lock().unwrap().insert(path.to_path_buf(), found.clone());
        found
    }
}

/// Ask the server about `down` (implementations) and `up` (super methods)
/// of `path`, whose outline is `outline` and lines `lines`.
pub async fn fill(
    lsp: &Manager,
    path: &Path,
    outline: &[DocumentSymbol],
    lines: &[String],
    down: Vec<Candidate>,
    up: Vec<Candidate>,
) -> Filled {
    let mut filled = Filled::default();
    let mut implementations = vec![];
    if !down.is_empty() {
        let at: Vec<Position> = down.iter().map(|c| Position::new(c.line, c.col)).collect();
        match lsp.implementations(path, &at).await {
            Ok(found) => implementations = found,
            Err(e) if unavailable(&e) => filled.unsupported.push(Group::Inheritors),
            Err(e) => log::debug!("gutter icons of {}: {e}", path.display()),
        }
    }
    let deps = PassDeps {
        lsp,
        path,
        supertypes: Default::default(),
        outlines: Mutex::new(HashMap::from([(path.to_path_buf(), outline.to_vec())])),
        unsupported: AtomicBool::new(false),
    };
    let mut supers = vec![];
    for c in &up {
        let owner = c.owner.expect("wanted: methods with an owner");
        supers.push(nav::super_method_targets(path, owner, &c.symbol, &deps).await);
        if deps.unsupported.load(Ordering::Relaxed) {
            filled.unsupported.push(Group::Supers);
            supers.clear();
            break;
        }
    }
    let implementations: Vec<_> = down.iter().zip(implementations).map(|(c, found)| nav::without_self(path, c, found)).collect();
    let mut files: HashMap<PathBuf, Vec<String>> = HashMap::from([(path.to_path_buf(), lines.to_vec())]);
    let targets = implementations.iter().flatten().map(|l| &l.path).chain(supers.iter().flatten().map(|s| &s.location.path));
    for p in targets {
        if !files.contains_key(p) {
            let text = tern_lsp::read_text(p).unwrap_or_default();
            files.insert(p.clone(), text.lines().map(str::to_owned).collect());
        }
    }
    for (c, found) in down.iter().zip(implementations) {
        filled.inheritors.push((c.key(), nav::inheritors_marker(c, found, &files)));
    }
    for (c, found) in up.iter().zip(&supers) {
        filled.supers.push((c.key(), nav::supers_marker(c, found, &files)));
    }
    filled
}

/// [`fill`] as a task of its own, for the background executor: a pass
/// asks a lot and reads files, none of it on the UI thread.
pub async fn run(
    lsp: std::sync::Arc<Manager>,
    path: PathBuf,
    outline: Vec<DocumentSymbol>,
    lines: Vec<String>,
    down: Vec<Candidate>,
    up: Vec<Candidate>,
) -> Filled {
    fill(&lsp, &path, &outline, &lines, down, up).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tern_lsp::Location;
    use tern_lsp::nav::MarkerKind;

    fn candidate(line: u32, inheritable: bool, overriding: bool) -> Candidate {
        Candidate {
            symbol: format!("m{line}"),
            name: format!("m{line}"),
            line,
            col: 4,
            method: true,
            is_abstract: false,
            inheritable,
            overriding,
            owner: Some(Position::new(0, 6)),
        }
    }

    fn marker(c: &Candidate) -> LineMarker {
        LineMarker {
            kind: MarkerKind::Overridden,
            line: c.line,
            name: c.name.clone(),
            targets: vec![Location { path: "/p/b.rs".into(), range: Default::default() }],
            tooltip: "Is overridden in B".into(),
            chooser_title: String::new(),
        }
    }

    #[test]
    fn pass_runs_off_the_ui_thread() {
        fn off_thread<F: std::future::Future + Send + 'static>(_: &F) {}
        let (tx, _rx) = flume::unbounded();
        let lsp = Manager::new("/w".into(), vec![], tx);
        off_thread(&run(lsp, "/w/a.rs".into(), vec![], vec![], vec![], vec![]));
    }

    #[test]
    fn visible_candidates_window_and_cap() {
        let all: Vec<Candidate> = (0..300).map(|l| candidate(l, true, true)).collect();
        let lines = |v: Vec<Candidate>| v.iter().map(|c| c.line).collect::<Vec<_>>();
        // 60 lines around 100..=120, but only 60 of them: 40..=99.
        assert_eq!(lines(visible_candidates(&all, 100, 120)), (40..100).collect::<Vec<_>>());
        let sparse: Vec<Candidate> = [0, 30, 61, 150, 181, 182].into_iter().map(|l| candidate(l, true, true)).collect();
        assert_eq!(lines(visible_candidates(&sparse, 121, 121)), vec![61, 150, 181]);
        assert_eq!(lines(visible_candidates(&sparse, 0, 0)), vec![0, 30]);
    }

    #[test]
    fn stale_pass_is_dropped() {
        let c = candidate(3, true, false);
        let mut m = Markers::default();
        let pass = m.begin();
        assert!(m.set_outline(pass, vec![], vec![c.clone()]));
        // Typing while the pass waits for the server.
        m.note_edit();
        let filled = Filled { inheritors: vec![(c.key(), Some(marker(&c)))], ..Default::default() };
        assert!(!m.apply(pass, filled));
        assert!(m.shown().is_empty());
        assert!(!m.set_outline(pass, vec![], vec![c.clone()]));
        // The next pass gets in.
        let pass = m.begin();
        assert!(m.set_outline(pass, vec![], vec![c.clone()]));
        assert!(m.apply(pass, Filled { inheritors: vec![(c.key(), Some(marker(&c)))], ..Default::default() }));
        assert_eq!(m.shown(), vec![marker(&c)]);
        // Asked once: not wanted again.
        assert!(m.wanted(std::slice::from_ref(&c)).0.is_empty());
    }

    #[test]
    fn unsupported_turns_group_off() {
        let cs = vec![candidate(1, true, true), candidate(2, true, true)];
        let mut m = Markers::default();
        let pass = m.begin();
        m.set_outline(pass, vec![], cs.clone());
        assert_eq!(m.wanted(&cs).0.len(), 2);
        m.apply(pass, Filled { unsupported: vec![Group::Inheritors], ..Default::default() });
        let (down, up) = m.wanted(&cs);
        assert!(down.is_empty());
        assert_eq!(up.len(), 2, "the other group still asks");
        // Until the server changes (indexing done) or the text does.
        m.invalidate();
        assert_eq!(m.wanted(&cs).0.len(), 2);
    }
}
