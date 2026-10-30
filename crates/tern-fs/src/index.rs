//! A trigram index of a folder's files for Search in Files: which files hold
//! each three-byte sequence (ASCII case folded), so a search reads only the
//! files that can match instead of every file. Its own watcher keeps it
//! current. Until it is built, or when it cannot be trusted, a search scans
//! the folder as [`search::search`] does; the results are the same either way.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{BuildHasherDefault, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock, mpsc};
use std::time::{Duration, SystemTime};

use ignore::WalkState;
use regex_syntax::hir::{Class, Hir, HirKind};

use crate::search::{self, Query, Results};
use crate::watch::{ChangeBatch, FsWatcher};

/// Files indexed at most; in a bigger folder, the rest are read on every search.
const MAX_FILES: usize = 200_000;
/// Stale entries (files changed or gone since the build) tolerated before
/// the index is built again, at least.
const MIN_DEAD: usize = 1_000;
/// Strings a regex part may stand for and still be spelled out.
const MAX_EXACT: usize = 64;
/// Characters a class may hold and still be spelled out.
const MAX_CLASS: u32 = 16;
/// Files that change the ignore rules: the index is built again.
const IGNORE_FILES: &[&str] = &[".gitignore", ".ignore"];

/// The index of one folder, built in the background on first use.
pub struct SearchIndex {
    shared: Arc<Shared>,
    started: AtomicBool,
    watcher: Mutex<Option<FsWatcher>>,
    /// Asks the worker to build the index again.
    worker: Mutex<Option<mpsc::Sender<ChangeBatch>>>,
}

struct Shared {
    root: PathBuf,
    state: RwLock<State>,
    /// Counts the changes reported, to tell which are newer than a read.
    seq: AtomicU64,
    /// The index was dropped: stop building.
    closed: AtomicBool,
    /// Files indexed at most.
    max_files: usize,
    /// The files git's global ignore rules come from, and the configs that
    /// name them.
    global_files: fn() -> Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Phase {
    /// Not started, or no watcher: searches scan.
    #[default]
    Off,
    /// Being built (again): searches scan.
    Building,
    Ready,
}

#[derive(Default)]
struct State {
    phase: Phase,
    /// By id: the file (relative to the root), or `None` once it changed or
    /// went away (or was never one a search reads).
    files: Vec<Option<PathBuf>>,
    /// The live ids, by path.
    ids: BTreeMap<PathBuf, u32>,
    /// Folders the walk went into (relative; empty for the root): the nearest
    /// one decides whether a new file is ignored.
    dirs: BTreeSet<PathBuf>,
    /// Folders the watcher reports nothing in (`node_modules`, `target`, ...
    /// when not ignored): walked and searched every time.
    unwatched: BTreeSet<PathBuf>,
    /// Files left out of a full index (relative): read on every search.
    unindexed: BTreeSet<PathBuf>,
    /// By trigram, the ids of the files holding it.
    postings: TrigramMap,
    /// Ids that went stale.
    dead: usize,
    /// Changed on disk and not read again yet (relative), with the change's
    /// [`Shared::seq`]: searched whatever the index says.
    dirty: HashMap<PathBuf, u64>,
    /// When each repo's `.git/info/exclude` was last modified, at the build.
    excludes: HashMap<PathBuf, Option<SystemTime>>,
    /// [`Shared::global_ignores`] at the build.
    global: Vec<(PathBuf, Option<SystemTime>)>,
}

impl SearchIndex {
    pub fn new(root: PathBuf) -> Self {
        Self::with(root, MAX_FILES, global_ignore_files)
    }

    fn with(root: PathBuf, max_files: usize, global_files: fn() -> Vec<PathBuf>) -> Self {
        let shared = Arc::new(Shared {
            root,
            state: RwLock::default(),
            seq: AtomicU64::new(0),
            closed: AtomicBool::new(false),
            max_files,
            global_files,
        });
        Self { shared, started: AtomicBool::new(false), watcher: Mutex::new(None), worker: Mutex::new(None) }
    }

    /// Start watching the folder and building the index, once.
    pub fn warm(&self) {
        if self.started.swap(true, Ordering::Relaxed) {
            return;
        }
        let (tx, rx) = mpsc::channel::<ChangeBatch>();
        *self.worker.lock().unwrap() = Some(tx.clone());
        let shared = self.shared.clone();
        // Watch before reading, so no change made during the build is lost.
        let watcher = FsWatcher::new(&self.shared.root, Duration::from_millis(40), move |batch| {
            shared.mark_dirty(&batch.paths);
            let _ = tx.send(batch);
        });
        let Ok(watcher) = watcher.map_err(|e| log::warn!("search index: no watcher, searches scan: {e}")) else { return };
        *self.watcher.lock().unwrap() = Some(watcher);
        let shared = self.shared.clone();
        std::thread::Builder::new().name("search-index".into()).spawn(move || shared.run(rx)).expect("spawn search-index");
    }

    /// Whether searches use the index (rather than scan).
    pub fn is_ready(&self) -> bool {
        self.shared.state.read().unwrap().phase == Phase::Ready
    }

    /// Search the folder's files for `q`, as [`search::search`] does: the
    /// first `limit` matching lines, by path. Stops early once `cancel` is set.
    pub fn search(&self, q: &Query, limit: usize, cancel: &AtomicBool) -> Result<Results, String> {
        if q.text.is_empty() {
            return Ok(Results::default());
        }
        let re = search::pattern(q)?;
        let root = &self.shared.root;
        let plan = plan(q);
        let global = self.shared.global_ignores();
        let (mut paths, unwatched) = {
            let state = self.shared.state.read().unwrap();
            // The watcher sees only the folder: git's global ignore rules are
            // checked here.
            let stale = state.phase == Phase::Ready && state.global != global;
            if state.phase != Phase::Ready || stale {
                drop(state);
                if let Some(worker) = self.worker.lock().unwrap().as_ref().filter(|_| stale) {
                    let _ = worker.send(ChangeBatch { rescan: true, ..ChangeBatch::default() });
                }
                return search::search(root, q, limit, cancel);
            }
            let mut paths: Vec<PathBuf> = match matching(&plan, &state.postings) {
                Some(ids) => ids.into_iter().filter_map(|id| state.files[id as usize].clone()).collect(),
                None => state.ids.keys().cloned().collect(),
            };
            paths.extend(state.dirty.keys().filter(|p| state.ids.contains_key(*p)).cloned());
            paths.extend(state.unindexed.iter().cloned());
            (paths, state.unwatched.iter().cloned().collect::<Vec<_>>())
        };
        for dir in unwatched {
            paths.extend(walk(root, &dir, false, |_| true).files);
        }
        paths.sort();
        paths.dedup();
        Ok(search::search_files(root, &paths, &re, q.whole_word, limit, cancel))
    }
}

impl Drop for SearchIndex {
    fn drop(&mut self) {
        // The watcher goes with it, and with it the worker's channel.
        self.shared.closed.store(true, Ordering::Relaxed);
    }
}

impl Shared {
    fn rel(&self, path: &Path) -> Option<PathBuf> {
        path.strip_prefix(&self.root).ok().map(Path::to_path_buf)
    }

    /// [`Shared::global_files`], each with when it was last modified.
    fn global_ignores(&self) -> Vec<(PathBuf, Option<SystemTime>)> {
        (self.global_files)().into_iter().map(|f| (modified(&f), f)).map(|(t, f)| (f, t)).collect()
    }

    fn mark_dirty(&self, paths: &[PathBuf]) {
        let mut state = self.state.write().unwrap();
        for rel in paths.iter().filter_map(|p| self.rel(p)) {
            let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
            state.dirty.insert(rel, seq);
        }
    }

    /// The worker: build, then apply the changes the watcher reports.
    fn run(&self, rx: mpsc::Receiver<ChangeBatch>) {
        self.build();
        loop {
            let Ok(mut batch) = rx.recv() else { return };
            for b in rx.try_iter() {
                batch.paths.extend(b.paths);
                batch.git_repos.extend(b.git_repos);
                batch.rescan |= b.rescan;
            }
            if self.closed.load(Ordering::Relaxed) {
                return;
            }
            let mut rebuild = batch.rescan
                || batch.paths.iter().any(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| IGNORE_FILES.contains(&n)))
                || batch.paths.iter().any(|p| p == &self.root)
                || self.excludes_changed(&batch.git_repos);
            if !rebuild && self.state.read().unwrap().phase == Phase::Ready {
                rebuild = self.update(&batch.paths);
            }
            if rebuild {
                self.build();
            }
        }
    }

    /// Whether a repo's `.git/info/exclude` changed since the build.
    fn excludes_changed(&self, repos: &[PathBuf]) -> bool {
        let state = self.state.read().unwrap();
        repos.iter().any(|r| state.excludes.get(r).is_some_and(|&t| t != modified(&exclude_file(r))))
    }

    /// Build the index from scratch; searches scan meanwhile.
    fn build(&self) {
        let upto = self.seq.load(Ordering::Relaxed);
        self.state.write().unwrap().phase = Phase::Building;
        let built = self.build_state();
        let mut state = self.state.write().unwrap();
        let Some(mut built) = built else { return };
        // Changes reported since the build started stay dirty: the worker
        // applies them next.
        built.dirty = std::mem::take(&mut state.dirty);
        built.dirty.retain(|_, seq| *seq > upto);
        built.phase = Phase::Ready;
        *state = built;
    }

    /// A fresh index of the folder; `None` once closed.
    fn build_state(&self) -> Option<State> {
        let global = self.global_ignores();
        let excludes =
            crate::git::find_repos(&self.root).into_iter().map(|r| (exclude_file(&r), r)).map(|(f, r)| (r, modified(&f))).collect();
        let found = walk(&self.root, Path::new(""), true, |_| true);
        if self.closed.load(Ordering::Relaxed) {
            return None;
        }
        let mut files = found.files;
        files.sort();
        let unindexed = files.split_off(files.len().min(self.max_files)).into_iter().collect();
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(8);
        // Runs of ids, in order, so each run's postings append to the ones
        // before; many more runs than threads, so a run of big files does
        // not hold the rest up.
        let per = files.len().div_ceil(threads * 8).clamp(64, 2048);
        let runs: Vec<&[PathBuf]> = files.chunks(per).collect();
        let next = AtomicUsize::new(0);
        let closed = &self.closed;
        let mut done: Vec<(usize, Grammed)> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..threads)
                .map(|_| {
                    let (root, runs, next) = (&self.root, &runs, &next);
                    s.spawn(move || {
                        let mut grams = Grams::new();
                        let mut pairs: Vec<u64> = vec![];
                        let mut out = vec![];
                        while let Some(run) = runs.get(next.fetch_add(1, Ordering::Relaxed)) {
                            if closed.load(Ordering::Relaxed) {
                                break;
                            }
                            let n = out.len();
                            let first = (run.as_ptr() as usize - runs[0].as_ptr() as usize) / size_of::<PathBuf>();
                            out.push((first, Grammed::default()));
                            let g = &mut out[n].1;
                            for (i, rel) in run.iter().enumerate() {
                                let read = search::read_text(&root.join(rel));
                                if let Some(bytes) = &read {
                                    let id = (first + i) as u64;
                                    pairs.extend(grams.of(bytes).iter().map(|&t| u64::from(t) << 32 | id));
                                }
                                g.read.push(read.is_some());
                            }
                            // Grouped by trigram, each list's ids ascending.
                            pairs.sort_unstable();
                            for chunk in pairs.chunk_by(|a, b| a >> 32 == b >> 32) {
                                let mut p = Postings::default();
                                chunk.iter().for_each(|&pair| p.push(pair as u32));
                                g.postings.push(((chunk[0] >> 32) as u32, p));
                            }
                            pairs.clear();
                        }
                        out
                    })
                })
                .collect();
            handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
        });
        if closed.load(Ordering::Relaxed) {
            return None;
        }
        done.sort_by_key(|(first, _)| *first);
        let mut state = State {
            dirs: found.dirs.into_iter().collect(),
            unwatched: found.unwatched.into_iter().collect(),
            unindexed,
            excludes,
            global,
            ..State::default()
        };
        let mut read = Vec::with_capacity(files.len());
        for (_, g) in done {
            read.extend(g.read);
            for (t, p) in g.postings {
                state.postings.entry(t).or_default().append(p);
            }
        }
        for (id, (rel, read)) in files.into_iter().zip(read).enumerate() {
            if read {
                state.ids.insert(rel.clone(), id as u32);
                state.files.push(Some(rel));
            } else {
                state.files.push(None);
            }
        }
        Some(state)
    }

    /// Read the changed `paths` (absolute) again. Whether the index should
    /// rather be built again (too much of it went stale).
    fn update(&self, paths: &[PathBuf]) -> bool {
        let upto = self.seq.load(Ordering::Relaxed);
        let rels: BTreeSet<PathBuf> = paths.iter().filter_map(|p| self.rel(p)).collect();
        // Only this thread changes the index, so it holds still between the
        // locks taken here. A path still there is walked from the nearest
        // folder known to be searched, which applies the ignore rules on the
        // way down; one gone goes with everything under it.
        let starts: Vec<(&PathBuf, Option<PathBuf>)> = {
            let state = self.state.read().unwrap();
            rels.iter().map(|rel| (rel, rel.ancestors().skip(1).find(|a| state.dirs.contains(*a)).map(Path::to_path_buf))).collect()
        };
        let mut gone: Vec<&PathBuf> = vec![];
        let mut walked: Vec<&PathBuf> = vec![];
        let mut found = Found::default();
        for (rel, from) in starts {
            let target = self.root.join(rel);
            let there = std::fs::symlink_metadata(&target).is_ok_and(|m| m.is_file() || m.is_dir());
            let Some(from) = from.filter(|_| there) else {
                gone.push(rel);
                continue;
            };
            let more = walk(&self.root, &from, true, move |p| target.starts_with(p) || p.starts_with(&target));
            found.files.extend(more.files.into_iter().filter(|p| p.starts_with(rel)));
            found.dirs.extend(more.dirs.into_iter().filter(|p| p.starts_with(rel)));
            found.unwatched.extend(more.unwatched.into_iter().filter(|p| p.starts_with(rel)));
            walked.push(rel);
        }
        found.files.sort();
        found.files.dedup();
        // Read the files changed or new, while the index has room (the rest
        // stay out of it); drop the ones under a walked folder the walk no
        // longer finds.
        let (to_read, spill, stale) = {
            let state = self.state.read().unwrap();
            let mut room = self.max_files.saturating_sub(state.ids.len());
            let (mut to_read, mut spill) = (vec![], vec![]);
            for f in &found.files {
                if state.ids.contains_key(f) {
                    if rels.contains(f) {
                        to_read.push(f.clone());
                    }
                } else if room > 0 && !state.unindexed.contains(f) {
                    room -= 1;
                    to_read.push(f.clone());
                } else {
                    spill.push(f.clone());
                }
            }
            let stale: Vec<PathBuf> = walked
                .iter()
                .flat_map(|rel| under(&state.ids, rel).into_iter().chain(under_set(&state.unindexed, rel)))
                .filter(|p| found.files.binary_search(p).is_err())
                .collect();
            (to_read, spill, stale)
        };
        let mut grams = Grams::new();
        let read: Vec<(PathBuf, Option<Vec<u32>>)> = to_read
            .into_iter()
            .map(|rel| {
                let t = search::read_text(&self.root.join(&rel)).map(|b| grams.of(&b).to_vec());
                (rel, t)
            })
            .collect();

        let mut state = self.state.write().unwrap();
        let state = &mut *state;
        let dropped: Vec<PathBuf> =
            gone.iter().flat_map(|rel| under(&state.ids, rel).into_iter().chain(under_set(&state.unindexed, rel))).chain(stale).collect();
        for p in dropped.iter().chain(read.iter().map(|(p, _)| p)) {
            if let Some(id) = state.ids.remove(p) {
                state.files[id as usize] = None;
                state.dead += 1;
            }
            state.unindexed.remove(p);
        }
        state.unindexed.extend(spill);
        for rel in gone.iter().chain(&walked) {
            state.dirs.retain(|d| !d.starts_with(rel));
            state.unwatched.retain(|d| !d.starts_with(rel));
        }
        state.dirs.extend(found.dirs);
        state.unwatched.extend(found.unwatched);
        for (rel, trigrams) in read {
            let Some(trigrams) = trigrams else { continue };
            let Ok(id) = u32::try_from(state.files.len()) else { return true };
            for t in trigrams {
                state.postings.entry(t).or_default().push(id);
            }
            state.ids.insert(rel.clone(), id);
            state.files.push(Some(rel));
        }
        for rel in &rels {
            if state.dirty.get(rel).is_some_and(|&seq| seq <= upto) {
                state.dirty.remove(rel);
            }
        }
        state.dead > MIN_DEAD.max(state.ids.len() / 2)
    }
}

/// The indexed files at or under `rel`.
fn under(ids: &BTreeMap<PathBuf, u32>, rel: &Path) -> Vec<PathBuf> {
    ids.range(rel.to_path_buf()..).map(|(p, _)| p).take_while(|p| p.starts_with(rel)).cloned().collect()
}

/// The files at or under `rel` in `set`.
fn under_set(set: &BTreeSet<PathBuf>, rel: &Path) -> Vec<PathBuf> {
    set.range(rel.to_path_buf()..).take_while(|p| p.starts_with(rel)).cloned().collect()
}

/// The files git's global ignore rules come from, and the configs that name
/// them (as `ignore` finds them).
fn global_ignore_files() -> Vec<PathBuf> {
    let home = std::env::home_dir();
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|x| !x.as_os_str().is_empty())
        .or_else(|| home.as_ref().map(|h| h.join(".config")));
    let mut files: Vec<PathBuf> = std::env::var_os("GIT_CONFIG_GLOBAL").map(PathBuf::from).into_iter().collect();
    files.extend(home.map(|h| h.join(".gitconfig")));
    files.extend(xdg.iter().flat_map(|x| [x.join("git").join("config"), x.join("git").join("ignore")]));
    files.extend(ignore::gitignore::gitconfig_excludes_path());
    files
}

fn exclude_file(repo: &Path) -> PathBuf {
    repo.join(".git").join("info").join("exclude")
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// What a walk found, relative to the root.
#[derive(Default)]
struct Found {
    files: Vec<PathBuf>,
    /// The folders it went into.
    dirs: Vec<PathBuf>,
    /// The folders the watcher reports nothing in; not gone into.
    unwatched: Vec<PathBuf>,
}

/// Walk `from` (relative to `root`) as a search does, going only into the
/// entries `keep` lets through, and, with `split`, not into the folders the
/// watcher reports nothing in.
fn walk(root: &Path, from: &Path, split: bool, keep: impl Fn(&Path) -> bool + Send + Sync + 'static) -> Found {
    let found = Mutex::new(Found::default());
    let start = root.join(from);
    search::walker_keeping(&start, keep).build_parallel().run(|| {
        Box::new(|entry| {
            let Ok(entry) = entry else { return WalkState::Continue };
            let Some(rel) = entry.path().strip_prefix(root).ok().map(Path::to_path_buf) else { return WalkState::Continue };
            let Some(kind) = entry.file_type() else { return WalkState::Continue };
            let mut found = found.lock().unwrap();
            if kind.is_dir() {
                let skipped = split && entry.depth() > 0 && entry.file_name().to_str().is_some_and(|n| crate::git::SKIP_DIRS.contains(&n));
                if skipped {
                    found.unwatched.push(rel);
                    return WalkState::Skip;
                }
                found.dirs.push(rel);
            } else if kind.is_file() {
                found.files.push(rel);
            }
            WalkState::Continue
        })
    });
    found.into_inner().unwrap()
}

/// By trigram.
type TrigramMap = HashMap<u32, Postings, BuildHasherDefault<TrigramHasher>>;

/// Hashes a trigram with a multiply: hashing is most of the cost of a build,
/// and the keys are not chosen by an attacker.
#[derive(Default)]
struct TrigramHasher(u64);

impl Hasher for TrigramHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u32(u32::from(b) ^ (self.0 as u32));
        }
    }

    fn write_u32(&mut self, n: u32) {
        let h = u64::from(n).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        self.0 = h ^ (h >> 29);
    }
}

/// A run of files read for a build.
#[derive(Default)]
struct Grammed {
    /// By trigram.
    postings: Vec<(u32, Postings)>,
    /// Per file, whether it is one a search reads.
    read: Vec<bool>,
}

/// Ids, ascending, each stored as a varint of its difference from the one
/// before (the first, from 0).
#[derive(Default)]
struct Postings {
    bytes: Vec<u8>,
    last: u32,
}

impl Postings {
    /// Add `id`, greater than every id in already.
    fn push(&mut self, id: u32) {
        put_varint(&mut self.bytes, id - self.last);
        self.last = id;
    }

    /// Add the ids of `other`, all greater than the ones in already.
    fn append(&mut self, other: Postings) {
        if other.bytes.is_empty() {
            return;
        }
        let (first, n) = get_varint(&other.bytes);
        put_varint(&mut self.bytes, first - self.last);
        self.bytes.extend_from_slice(&other.bytes[n..]);
        self.last = other.last;
    }

    fn ids(&self) -> Vec<u32> {
        let mut out = vec![];
        let (mut at, mut id) = (0, 0);
        while at < self.bytes.len() {
            let (d, n) = get_varint(&self.bytes[at..]);
            id += d;
            at += n;
            out.push(id);
        }
        out
    }
}

fn put_varint(out: &mut Vec<u8>, mut v: u32) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// The value and the bytes it took.
fn get_varint(bytes: &[u8]) -> (u32, usize) {
    let (mut v, mut shift) = (0u32, 0);
    for (i, &b) in bytes.iter().enumerate() {
        v |= u32::from(b & 0x7f) << shift;
        if b < 0x80 {
            return (v, i + 1);
        }
        shift += 7;
    }
    (v, bytes.len())
}

/// The trigram of three bytes, ASCII lowercased.
fn trigram(w: &[u8]) -> u32 {
    u32::from(w[0].to_ascii_lowercase()) << 16 | u32::from(w[1].to_ascii_lowercase()) << 8 | u32::from(w[2].to_ascii_lowercase())
}

/// Finds a text's distinct trigrams, reusing its buffers.
struct Grams {
    /// One bit per trigram (2^24 of them).
    seen: Vec<u64>,
    out: Vec<u32>,
}

impl Grams {
    fn new() -> Self {
        Self { seen: vec![0; 1 << 18], out: vec![] }
    }

    fn of(&mut self, bytes: &[u8]) -> &[u32] {
        for &t in &self.out {
            self.seen[t as usize >> 6] = 0;
        }
        self.out.clear();
        for w in bytes.windows(3) {
            let t = trigram(w);
            let (word, bit) = (t as usize >> 6, 1u64 << (t & 63));
            if self.seen[word] & bit == 0 {
                self.seen[word] |= bit;
                self.out.push(t);
            }
        }
        &self.out
    }
}

/// The files a query can match, as trigrams they must hold.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Plan {
    /// Any file.
    All,
    Tri(u32),
    And(Vec<Plan>),
    Or(Vec<Plan>),
}

impl Plan {
    fn and(parts: Vec<Plan>) -> Plan {
        let mut out = vec![];
        for p in parts {
            match p {
                Plan::All => {},
                Plan::And(more) => out.extend(more),
                p => out.push(p),
            }
        }
        out.sort_by_key(|p| format!("{p:?}"));
        out.dedup();
        if out.len() == 1 {
            out.pop().unwrap()
        } else if out.is_empty() {
            Plan::All
        } else {
            Plan::And(out)
        }
    }

    fn or(parts: Vec<Plan>) -> Plan {
        let mut out = vec![];
        for p in parts {
            match p {
                Plan::All => return Plan::All,
                Plan::Or(more) => out.extend(more),
                p => out.push(p),
            }
        }
        out.sort_by_key(|p| format!("{p:?}"));
        out.dedup();
        if out.len() == 1 {
            out.pop().unwrap()
        } else if out.is_empty() {
            Plan::All
        } else {
            Plan::Or(out)
        }
    }
}

/// The trigrams a file must hold to match `q`.
fn plan(q: &Query) -> Plan {
    let source = if q.regex { q.text.clone() } else { regex_syntax::escape(&q.text) };
    let Ok(hir) = regex_syntax::ParserBuilder::new().case_insensitive(!q.case_sensitive).build().parse(&source) else {
        return Plan::All;
    };
    analyze(&hir).plan()
}

/// What a part of a regex matches: exactly one of a few strings (ASCII
/// lowercased), or text holding the trigrams of a plan.
enum Info {
    Exact(BTreeSet<Vec<u8>>),
    Plan(Plan),
}

impl Info {
    fn plan(self) -> Plan {
        match self {
            Info::Plan(p) => p,
            Info::Exact(set) => Plan::or(set.iter().map(|s| plan_of(s)).collect()),
        }
    }
}

/// The trigrams of `s`, all needed.
fn plan_of(s: &[u8]) -> Plan {
    // A file's invalid UTF-8 reads as U+FFFD, which its bytes do not hold.
    if s.windows(3).any(|w| w == "\u{FFFD}".as_bytes()) {
        return Plan::All;
    }
    Plan::and(s.windows(3).map(|w| Plan::Tri(trigram(w))).collect())
}

fn exact(strings: impl IntoIterator<Item = Vec<u8>>) -> Info {
    Info::Exact(strings.into_iter().map(|s| s.to_ascii_lowercase()).collect())
}

fn analyze(hir: &Hir) -> Info {
    match hir.kind() {
        HirKind::Empty | HirKind::Look(_) => exact([vec![]]),
        HirKind::Literal(lit) => exact([lit.0.to_vec()]),
        HirKind::Class(Class::Unicode(c)) => {
            let n: u32 = c.ranges().iter().map(|r| u32::from(r.end()) - u32::from(r.start()) + 1).sum();
            if n > MAX_CLASS {
                return Info::Plan(Plan::All);
            }
            exact(c.ranges().iter().flat_map(|r| r.start()..=r.end()).map(|ch| ch.to_string().into_bytes()))
        },
        HirKind::Class(Class::Bytes(c)) => {
            let n: u32 = c.ranges().iter().map(|r| u32::from(r.end()) - u32::from(r.start()) + 1).sum();
            if n > MAX_CLASS {
                return Info::Plan(Plan::All);
            }
            exact(c.ranges().iter().flat_map(|r| r.start()..=r.end()).map(|b| vec![b]))
        },
        HirKind::Capture(c) => analyze(&c.sub),
        HirKind::Repetition(r) => match (r.min, r.max) {
            (1, Some(1)) => analyze(&r.sub),
            (0, _) => Info::Plan(Plan::All),
            // Holds the part at least once.
            _ => Info::Plan(analyze(&r.sub).plan()),
        },
        HirKind::Concat(parts) => {
            // Runs of exact parts join into exact strings; the rest is ANDed.
            let mut plans = vec![];
            let mut run: Option<BTreeSet<Vec<u8>>> = Some([vec![]].into());
            for part in parts {
                match (analyze(part), run.take()) {
                    (Info::Exact(next), Some(cur)) if cur.len() * next.len() <= MAX_EXACT => {
                        run = Some(cur.iter().flat_map(|a| next.iter().map(move |b| [a.as_slice(), b].concat())).collect());
                    },
                    (Info::Exact(next), cur) => {
                        plans.extend(cur.map(|c| Info::Exact(c).plan()));
                        run = Some(next);
                    },
                    (Info::Plan(p), cur) => {
                        plans.extend(cur.map(|c| Info::Exact(c).plan()));
                        plans.push(p);
                    },
                }
            }
            match run {
                Some(run) if plans.is_empty() => Info::Exact(run),
                run => {
                    plans.extend(run.map(|c| Info::Exact(c).plan()));
                    Info::Plan(Plan::and(plans))
                },
            }
        },
        HirKind::Alternation(alts) => {
            let infos: Vec<Info> = alts.iter().map(analyze).collect();
            let all_exact = infos.iter().all(|i| matches!(i, Info::Exact(_)));
            let size: usize = infos.iter().map(|i| if let Info::Exact(s) = i { s.len() } else { 0 }).sum();
            if all_exact && size <= MAX_EXACT {
                return Info::Exact(infos.into_iter().flat_map(|i| if let Info::Exact(s) = i { s } else { BTreeSet::new() }).collect());
            }
            Info::Plan(Plan::or(infos.into_iter().map(Info::plan).collect()))
        },
    }
}

/// The ids of the files `plan` lets through, ascending; `None` for all.
fn matching(plan: &Plan, postings: &TrigramMap) -> Option<Vec<u32>> {
    match plan {
        Plan::All => None,
        Plan::Tri(t) => Some(postings.get(t).map(Postings::ids).unwrap_or_default()),
        Plan::And(parts) => {
            let mut lists: Vec<Vec<u32>> = parts.iter().filter_map(|p| matching(p, postings)).collect();
            lists.sort_by_key(Vec::len);
            let mut lists = lists.into_iter();
            let first = lists.next()?;
            Some(lists.fold(first, |acc, l| intersect(&acc, &l)))
        },
        Plan::Or(parts) => {
            let mut out = vec![];
            for p in parts {
                out.extend(matching(p, postings)?);
            }
            out.sort_unstable();
            out.dedup();
            Some(out)
        },
    }
}

fn intersect(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (mut i, mut j, mut out) = (0, 0, vec![]);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            },
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::OnceLock;
    use std::time::Instant;

    use super::*;

    fn q(text: &str, f: impl FnOnce(&mut Query)) -> Query {
        let mut q = Query { text: text.into(), ..Default::default() };
        f(&mut q);
        q
    }

    fn tri(s: &str) -> Plan {
        Plan::Tri(trigram(s.as_bytes()))
    }

    #[test]
    fn postings_round_trip_and_append() {
        let mut a = Postings::default();
        for id in [0, 1, 200, 70_000] {
            a.push(id);
        }
        let mut b = Postings::default();
        for id in [70_001, 1 << 30] {
            b.push(id);
        }
        a.append(b);
        assert_eq!(a.ids(), [0, 1, 200, 70_000, 70_001, 1 << 30]);
    }

    #[test]
    fn literal_plans() {
        assert_eq!(plan(&q("foob", |_| {})), Plan::and(vec![tri("foo"), tri("oob")]));
        // Too short to narrow anything down.
        assert_eq!(plan(&q("fo", |_| {})), Plan::All);
        // Case folded either way: the index is.
        assert_eq!(plan(&q("FOO", |q| q.case_sensitive = true)), tri("foo"));
        assert_eq!(plan(&q("Foo", |_| {})), tri("foo"));
        // Regex characters are literal without regex.
        assert_eq!(plan(&q("a.bc", |_| {})), Plan::and(vec![tri("a.b"), tri(".bc")]));
    }

    #[test]
    fn regex_plans() {
        let r = |text: &str| plan(&q(text, |q| q.regex = true));
        assert_eq!(r("foo|barx"), Plan::or(vec![tri("foo"), Plan::and(vec![tri("bar"), tri("arx")])]));
        assert_eq!(r(r"fn \w+\("), tri("fn "));
        assert_eq!(r("ab[cd]e"), Plan::or(vec![Plan::and(vec![tri("abc"), tri("bce")]), Plan::and(vec![tri("abd"), tri("bde")])]));
        assert_eq!(r("foo.*bar"), Plan::and(vec![tri("foo"), tri("bar")]));
        assert_eq!(r("(foo)+"), tri("foo"));
        assert_eq!(r("x(foo)?y"), Plan::All);
        assert_eq!(r("foo|.*"), Plan::All);
        assert_eq!(r(r"\w+"), Plan::All);
    }

    #[test]
    fn non_ascii_case_folds_by_alternatives() {
        // É and é differ in their bytes: either will do.
        assert_eq!(plan(&q("éab", |_| {})), Plan::or(vec![plan_of("éab".as_bytes()), plan_of("Éab".as_bytes())]));
    }

    fn fixture() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        fs::create_dir_all(r.join("src")).unwrap();
        fs::create_dir_all(r.join("target")).unwrap();
        fs::create_dir_all(r.join("node_modules/pkg")).unwrap();
        fs::create_dir_all(r.join(".git")).unwrap();
        fs::write(r.join(".gitignore"), "target/\n").unwrap();
        fs::write(r.join("src/a.rs"), "fn foo() {}\nlet foobar = Foo;\n// foo foo\n").unwrap();
        fs::write(r.join("src/b.rs"), "nothing here\nfn bar_baz() {}\n").unwrap();
        fs::write(r.join("src/é.txt"), "Élan vital\n").unwrap();
        fs::write(r.join("target/out.rs"), "foo\n").unwrap();
        fs::write(r.join("node_modules/pkg/index.js"), "export const foo = 1;\n").unwrap();
        fs::write(r.join(".git/config"), "foo\n").unwrap();
        fs::write(r.join("bin.dat"), b"foo\0\x01").unwrap();
        d
    }

    fn ready(root: &Path) -> SearchIndex {
        ready_with(root, MAX_FILES, Vec::new)
    }

    fn ready_with(root: &Path, max_files: usize, global_files: fn() -> Vec<PathBuf>) -> SearchIndex {
        let index = SearchIndex::with(root.canonicalize().unwrap(), max_files, global_files);
        index.warm();
        wait(|| index.is_ready());
        index
    }

    fn wait(mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn found(index: &SearchIndex, q: &Query) -> Vec<(String, u32)> {
        let r = index.search(q, 500, &AtomicBool::new(false)).unwrap();
        r.matches.iter().map(|m| (m.path.to_string_lossy().replace('\\', "/"), m.line)).collect()
    }

    /// Index and scan find the same lines.
    fn same_as_a_scan(index: &SearchIndex) {
        let root = index.shared.root.clone();
        let queries = [
            q("foo", |_| {}),
            q("Foo", |q| q.case_sensitive = true),
            q("foo", |q| q.whole_word = true),
            q(r"fn \w+\(", |q| q.regex = true),
            q("bar_baz|nothing", |q| q.regex = true),
            q("élan", |_| {}),
            q("fo", |_| {}),
            q("nope nope", |_| {}),
        ];
        for query in &queries {
            let mut scan = search::search(&root, query, 500, &AtomicBool::new(false)).unwrap();
            scan.matches.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
            let indexed = index.search(query, 500, &AtomicBool::new(false)).unwrap();
            assert_eq!(indexed, scan, "{query:?}");
        }
    }

    #[test]
    fn same_results_as_a_scan() {
        let d = fixture();
        let index = ready(d.path());
        same_as_a_scan(&index);
        // `node_modules` is not ignored here: searched, though unwatched.
        assert!(found(&index, &q("export const", |_| {})).contains(&("node_modules/pkg/index.js".into(), 1)));
    }

    #[test]
    fn follows_changes_on_disk() {
        let d = fixture();
        let index = ready(d.path());
        let root = index.shared.root.clone();
        let foo = q("zebra", |_| {});

        fs::write(root.join("src/new.rs"), "a zebra\n").unwrap();
        wait(|| found(&index, &foo) == [("src/new.rs".into(), 1)]);

        fs::write(root.join("src/a.rs"), "zebra crossing\n").unwrap();
        wait(|| found(&index, &foo).len() == 2);

        fs::remove_file(root.join("src/new.rs")).unwrap();
        wait(|| found(&index, &foo) == [("src/a.rs".into(), 1)]);

        // A new folder, files and all.
        fs::create_dir_all(root.join("lib/deep")).unwrap();
        fs::write(root.join("lib/deep/z.rs"), "zebra\n").unwrap();
        wait(|| found(&index, &foo).contains(&("lib/deep/z.rs".into(), 1)));
        fs::rename(root.join("lib/deep"), root.join("lib/moved")).unwrap();
        wait(|| found(&index, &foo) == [("lib/moved/z.rs".into(), 1), ("src/a.rs".into(), 1)]);

        // New files in an ignored folder stay out; in a build folder that is
        // not ignored, they are in.
        fs::write(root.join("target/z.rs"), "zebra\n").unwrap();
        fs::create_dir_all(root.join("src/build")).unwrap();
        fs::write(root.join("src/build/z.rs"), "zebra\n").unwrap();
        wait(|| found(&index, &foo).contains(&("src/build/z.rs".into(), 1)));
        assert!(!found(&index, &foo).contains(&("target/z.rs".into(), 1)));
        wait(|| index.shared.state.read().unwrap().dirty.is_empty());
        assert!(index.is_ready());

        // Ignoring a folder takes its files out.
        fs::write(root.join(".gitignore"), "target/\nlib/\nbuild/\n").unwrap();
        wait(|| found(&index, &foo) == [("src/a.rs".into(), 1)]);
        wait(|| index.is_ready());
        assert_eq!(found(&index, &foo), [("src/a.rs".into(), 1)]);
    }

    #[test]
    fn a_full_index_reads_the_rest_every_time() {
        let d = fixture();
        // Only `.gitignore` goes in: the index is full.
        let index = ready_with(d.path(), 1, Vec::new);
        assert_eq!(index.shared.state.read().unwrap().ids.len(), 1);
        same_as_a_scan(&index);
        let root = index.shared.root.clone();
        let foo = q("zebra", |_| {});
        fs::write(root.join("src/new.rs"), "a zebra\n").unwrap();
        wait(|| found(&index, &foo) == [("src/new.rs".into(), 1)]);
        assert!(index.shared.state.read().unwrap().unindexed.contains(Path::new("src/new.rs")));
        fs::remove_file(root.join("src/new.rs")).unwrap();
        wait(|| !index.shared.state.read().unwrap().unindexed.contains(Path::new("src/new.rs")));
        assert!(found(&index, &foo).is_empty());
    }

    static GLOBAL: OnceLock<PathBuf> = OnceLock::new();

    #[test]
    fn a_changed_global_ignore_builds_again() {
        let d = fixture();
        let global = d.path().join("global-ignore");
        fs::write(&global, "").unwrap();
        GLOBAL.set(global.clone()).unwrap();
        let index = ready_with(d.path(), MAX_FILES, || vec![GLOBAL.get().unwrap().clone()]);
        let before = index.shared.state.read().unwrap().global.clone();
        std::thread::sleep(Duration::from_millis(20));
        fs::write(&global, "*.rs\n").unwrap();
        assert_ne!(index.shared.global_ignores(), before);
        // That search scans and asks for a build.
        assert!(!found(&index, &q("foo", |_| {})).is_empty());
        wait(|| index.is_ready() && index.shared.state.read().unwrap().global != before);
    }
}
