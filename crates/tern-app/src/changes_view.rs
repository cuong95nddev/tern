//! Right pane: git changes of every repository under the opened folder
//! (the enclosing repo plus nested repos), grouped per repo. Clicking a file
//! previews its diff in a popup.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui_kit::component::ActiveTheme;
use gpui_kit::*;
use tern_fs::git::RepoChanges;
use tern_fs::{ChangeBatch, GitStatus};

use crate::template::{self, Framed, on_event};
use crate::{file_icons, style};

/// The element a row is drawn as ([`style`]).
const ROW: &str = "changes.row";

/// Changes are gathered until the disk is quiet this long... Longer than
/// the explorer watcher's batches are apart in a long burst (10 × 40ms), or
/// each of its batches would start a refresh.
const QUIET: Duration = Duration::from_millis(500);
/// ...or, in a long burst, at most this long after the first one.
const MAX_WAIT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub enum ChangesEvent {
    PreviewDiff(PathBuf, GitStatus),
    /// New status available; payload is the explorer's path → status map.
    Updated(HashMap<PathBuf, GitStatus>),
    /// The header's close button: hide the panel.
    Hide,
}

#[derive(Clone, Copy)]
enum RowKind {
    Repo(usize),
    File(usize, usize),
}

pub struct ChangesView {
    root: PathBuf,
    repos: Vec<RepoChanges>,
    collapsed: HashSet<PathBuf>,
    rows: Vec<RowKind>,
    selected: Option<PathBuf>,
    /// What changed since the last refresh started.
    dirty: Option<Dirty>,
    /// A refresh is running; changes meanwhile wait in `dirty`.
    refreshing: bool,
    /// The timer that starts the next refresh is running.
    armed: bool,
    /// Per repo, whether its config defines filter commands (status is then
    /// not run): a `git config` process saved per refresh. Forgotten when
    /// the repo's `.git` changes or on a full refresh. Not watched: files a
    /// config includes, and the `.git` of a repo enclosing the opened folder;
    /// the refresh button rereads those.
    defines_commands: HashMap<PathBuf, bool>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    _timer: Option<Task<()>>,
    _task: Option<Task<()>>,
}

/// Changes waiting for the next refresh.
struct Dirty {
    /// Look for repos again, then refresh all.
    rediscover: bool,
    /// Dirs whose `.git` entry was reported: repos are looked for again if
    /// one appeared or vanished. Mostly it is just a repo's `.git` dir
    /// changing (Windows reports the dir as well as the file in it).
    dot_git: HashSet<PathBuf>,
    /// Refresh every known repo.
    all: bool,
    /// Repos whose `.git` changed: refresh them whatever the paths.
    git: HashSet<PathBuf>,
    /// Changed worktree paths: refresh the repos owning ones git doesn't ignore.
    paths: HashSet<PathBuf>,
    /// Start without waiting (opening, the refresh button).
    now: bool,
    first: Instant,
    last: Instant,
}

impl Dirty {
    fn new() -> Self {
        let t = Instant::now();
        Self {
            rediscover: false,
            dot_git: HashSet::new(),
            all: false,
            git: HashSet::new(),
            paths: HashSet::new(),
            now: false,
            first: t,
            last: t,
        }
    }

    fn due(&self) -> Instant {
        if self.now { self.first } else { (self.last + QUIET).min(self.first + MAX_WAIT) }
    }

    /// The repos (of `repos`) to refresh, each with the changed paths to
    /// ask `check-ignore` about: empty means refresh regardless.
    fn plan(&self, repos: &[PathBuf]) -> Vec<(PathBuf, Vec<PathBuf>)> {
        if self.all || self.rediscover {
            return repos.iter().map(|r| (r.clone(), vec![])).collect();
        }
        let mut owned: HashMap<&Path, Vec<PathBuf>> = HashMap::new();
        for p in &self.paths {
            if let Some(r) = tern_fs::git::owner(repos, p) {
                owned.entry(r).or_default().push(p.clone());
            }
        }
        repos
            .iter()
            .filter_map(|r| match owned.remove(r.as_path()) {
                _ if self.git.contains(r) => Some((r.clone(), vec![])),
                Some(paths) => Some((r.clone(), paths)),
                None => None,
            })
            .collect()
    }
}

impl EventEmitter<ChangesEvent> for ChangesView {}

impl Focusable for ChangesView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ChangesView {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            root,
            repos: vec![],
            collapsed: HashSet::new(),
            rows: vec![],
            selected: None,
            dirty: None,
            refreshing: false,
            armed: false,
            defines_commands: HashMap::new(),
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _timer: None,
            _task: None,
        };
        this.refresh(true, cx);
        this
    }

    /// Paths changed on disk (or git metadata changed): refresh the status
    /// of the repos they belong to once the burst is over, and look for
    /// repos again if a `.git` entry appeared or vanished.
    pub fn files_changed(&mut self, batch: &ChangeBatch, cx: &mut Context<Self>) {
        let d = self.dirty.get_or_insert_with(Dirty::new);
        d.last = Instant::now();
        d.dot_git.extend(
            batch.paths.iter().filter(|p| p.file_name().is_some_and(|n| n == ".git")).filter_map(|p| p.parent()).map(Path::to_path_buf),
        );
        d.git.extend(batch.git_repos.iter().cloned());
        d.paths.extend(batch.paths.iter().cloned());
        self.schedule(cx);
    }

    /// Refresh every repo now (or as soon as the running refresh ends).
    pub fn refresh(&mut self, rediscover: bool, cx: &mut Context<Self>) {
        let d = self.dirty.get_or_insert_with(Dirty::new);
        d.all = true;
        d.rediscover |= rediscover;
        d.now = true;
        self.schedule(cx);
    }

    /// Start the timer for the next refresh, unless it or a refresh runs.
    fn schedule(&mut self, cx: &mut Context<Self>) {
        if self.refreshing || self.armed {
            return;
        }
        self.armed = true;
        self._timer = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(Some(due)) = this.update(cx, |this, _| this.dirty.as_ref().map(Dirty::due)) else { return };
                let left = due.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    let _ = this.update(cx, |this, cx| {
                        this.armed = false;
                        this.start(cx);
                    });
                    return;
                }
                // `due` moves while changes keep coming: look again after.
                cx.background_executor().timer(left).await;
            }
        }));
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        let Some(dirty) = self.dirty.take() else { return };
        self.refreshing = true;
        cx.notify();
        if dirty.all {
            self.defines_commands.clear();
        }
        self.defines_commands.retain(|r, _| !dirty.git.contains(r));
        let cached = self.defines_commands.clone();
        let root = self.root.clone();
        let known: Vec<PathBuf> = self.repos.iter().map(|r| r.root.clone()).collect();
        self._task = Some(cx.spawn(async move |this, cx| {
            let (list, fresh) = cx
                .background_spawn(async move {
                    let mut dirty = dirty;
                    // Otherwise a `.git` reported is its repo's metadata changing.
                    // A repo removed whole may be reported as its dir alone.
                    dirty.rediscover |= dirty.dot_git.iter().any(|d| d.join(".git").exists() != known.contains(d))
                        || known.iter().any(|r| !r.join(".git").exists());
                    dirty.git.extend(dirty.dot_git.drain());
                    let list = (dirty.rediscover || known.is_empty()).then(|| tern_fs::git::find_repos(&root));
                    let plan = dirty.plan(list.as_ref().unwrap_or(&known));
                    // One thread per repo: a `check-ignore` when only worktree
                    // files changed, then `status` unless they're all ignored
                    // or git holds the index lock (its release wakes us again).
                    let fresh = std::thread::scope(|s| {
                        let handles: Vec<_> = plan
                            .iter()
                            .map(|(r, paths)| {
                                s.spawn(|| {
                                    let skip = (!paths.is_empty() && tern_fs::git::all_ignored(r, paths)) || tern_fs::git::index_locked(r);
                                    (!skip).then(|| {
                                        let defines = cached.get(r).copied().unwrap_or_else(|| tern_fs::git::repo_defines_commands(r));
                                        (defines, tern_fs::git::repo_changes_with(r, &root, defines))
                                    })
                                })
                            })
                            .collect();
                        handles.into_iter().filter_map(|h| h.join().ok().flatten()).collect::<Vec<_>>()
                    });
                    (list, fresh)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.refreshing = false;
                let fresh = fresh
                    .into_iter()
                    .map(|(defines, rc)| {
                        this.defines_commands.insert(rc.root.clone(), defines);
                        rc
                    })
                    .collect();
                let repos = merge(&this.repos, list, fresh);
                if repos != this.repos {
                    this.repos = repos;
                    this.rebuild_rows();
                    cx.emit(ChangesEvent::Updated(tern_fs::git::status_map(&this.repos, &this.root)));
                }
                cx.notify();
                if this.dirty.is_some() {
                    this.schedule(cx);
                }
            });
        }));
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        for (ri, repo) in self.repos.iter().enumerate() {
            self.rows.push(RowKind::Repo(ri));
            if !self.collapsed.contains(&repo.root) {
                self.rows.extend((0..repo.files.len()).map(|fi| RowKind::File(ri, fi)));
            }
        }
    }

    fn total(&self) -> usize {
        self.repos.iter().map(|r| r.files.len()).sum()
    }

    fn repo_label(&self, repo: &Path) -> String {
        match repo.strip_prefix(&self.root) {
            Ok(rel) if !rel.as_os_str().is_empty() => rel.display().to_string(),
            _ => repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| repo.display().to_string()),
        }
    }

    fn click(&mut self, ix: usize, cx: &mut Context<Self>) {
        match self.rows.get(ix).copied() {
            Some(RowKind::Repo(ri)) => {
                let root = self.repos[ri].root.clone();
                if !self.collapsed.remove(&root) {
                    self.collapsed.insert(root);
                }
                self.rebuild_rows();
            },
            Some(RowKind::File(ri, fi)) => {
                let (path, status) = self.repos[ri].files[fi].clone();
                self.selected = Some(path.clone());
                cx.emit(ChangesEvent::PreviewDiff(path, status));
            },
            None => {},
        }
        cx.notify();
    }

    fn render_rows(&mut self, range: Range<usize>, _: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        range
            .filter_map(|ix| {
                let (name, scope) = match *self.rows.get(ix)? {
                    RowKind::Repo(ri) => {
                        let repo = &self.repos[ri];
                        let branch = repo.branch.as_deref().map(|b| b.split("...").next().unwrap_or(b).to_owned());
                        let detail: SharedString = match (&repo.error, repo.files.len()) {
                            (Some(e), _) => e.clone().into(),
                            (None, 0) => "clean".into(),
                            (None, n) => n.to_string().into(),
                        };
                        let scope = template::Scope::new("changes.repo")
                            .var("indent", style::row_indent(ROW, 0, cx))
                            .var("folder", true)
                            .var("open", !self.collapsed.contains(&repo.root))
                            .var("name", self.repo_label(&repo.root))
                            .var("branch", branch.unwrap_or_default())
                            .var("detail", detail)
                            .var("error", repo.error.is_some());
                        ("changes.repo", scope)
                    },
                    RowKind::File(ri, fi) => {
                        let repo = &self.repos[ri];
                        let (path, st) = &repo.files[fi];
                        let rel = path.strip_prefix(&repo.root).unwrap_or(path);
                        let name = rel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        let dir = rel.parent().map(|p| p.display().to_string()).unwrap_or_default();
                        let (icon, icon_color) = file_icons::file_icon(&name, theme);
                        let scope = template::Scope::new("changes.file")
                            .var("indent", style::row_indent(ROW, 1, cx))
                            .var("selected", self.selected.as_deref() == Some(path.as_path()))
                            .var("name", name)
                            .var("name_color", style::git_color(*st, cx))
                            .var("dir", dir)
                            .var("status", st.letter().to_string())
                            .slot("icon", style::row_icon(ROW, icon, icon_color, cx));
                        ("changes.file", scope)
                    },
                };
                Some(template::render(
                    name,
                    scope,
                    &mut |name, _, f| match (name, f) {
                        ("row", Framed::Box(d)) => {
                            d.id(ix).on_click(cx.listener(move |this, _, _, cx| this.click(ix, cx))).into_any_element()
                        },
                        (_, f) => f.into_any_element(),
                    },
                    cx,
                ))
            })
            .collect()
    }
}

/// The repos after a refresh: `list` if repos were looked for again (else the
/// same ones as `old`), each with its `fresh` status, else its old one.
fn merge(old: &[RepoChanges], list: Option<Vec<PathBuf>>, mut fresh: Vec<RepoChanges>) -> Vec<RepoChanges> {
    let roots = list.unwrap_or_else(|| old.iter().map(|r| r.root.clone()).collect());
    roots
        .into_iter()
        .map(|root| match fresh.iter().position(|r| r.root == root) {
            Some(i) => fresh.swap_remove(i),
            None => old.iter().find(|r| r.root == root).cloned().unwrap_or(RepoChanges { root, ..Default::default() }),
        })
        .collect()
}

impl Render for ChangesView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let total = self.total();
        let buttons = template::Scope::new("changes.header.buttons")
            .var("refreshing", self.refreshing)
            .var("hide_key", crate::platform::shortcut("⌘⇧G"));
        let me = cx.entity().downgrade();
        let header = crate::dock::panel_header(
            "changes",
            "changes.header",
            "Changes",
            Some(total),
            buttons,
            &mut |name, _, f| match (name, f) {
                ("refresh", Framed::Button(b)) => {
                    b.on_click(on_event(&me, |this, _: &ClickEvent, _, cx| this.refresh(true, cx))).into_any_element()
                },
                ("hide", Framed::Button(b)) => {
                    b.on_click(on_event(&me, |_, _: &ClickEvent, _, cx| cx.emit(ChangesEvent::Hide))).into_any_element()
                },
                (_, f) => f.into_any_element(),
            },
            cx,
        );
        let mut scope =
            template::Scope::new("changes").var("empty", self.repos.is_empty()).var("refreshing", self.refreshing).slot("header", header);
        if !self.repos.is_empty() {
            let list = uniform_list("changes", self.rows.len(), cx.processor(Self::render_rows)).track_scroll(&self.scroll).flex_1();
            scope = scope.slot("list", style::el("changes.list", cx).apply(list));
        }
        let focus = &self.focus;
        template::render(
            "changes",
            scope,
            &mut |name, _, f| match (name, f) {
                ("panel", Framed::Box(d)) => d.track_focus(focus).into_any_element(),
                (_, f) => f.into_any_element(),
            },
            cx,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use tern_fs::GitStatus;
    use tern_fs::git::RepoChanges;

    use super::{Dirty, MAX_WAIT, QUIET, merge};

    #[test]
    fn plans_only_the_repos_that_changed() {
        let repos = [PathBuf::from("/r"), PathBuf::from("/r/ext/a"), PathBuf::from("/r/ext/b")];
        let mut d = Dirty::new();
        d.paths.insert("/r/ext/a/src/x.rs".into());
        d.paths.insert("/r/ext/a/out/y.js".into());
        d.paths.insert("/elsewhere/z".into());
        d.git.insert("/r/ext/b".into());
        let mut plan = d.plan(&repos);
        plan.iter_mut().for_each(|(_, p)| p.sort());
        assert_eq!(
            plan,
            vec![
                (PathBuf::from("/r/ext/a"), vec![PathBuf::from("/r/ext/a/out/y.js"), PathBuf::from("/r/ext/a/src/x.rs")]),
                (PathBuf::from("/r/ext/b"), vec![]),
            ]
        );
        d.all = true;
        assert_eq!(d.plan(&repos).len(), 3);
    }

    #[test]
    fn merge_keeps_status_of_repos_not_refreshed() {
        let rc = |root: &str, n: usize| RepoChanges {
            root: root.into(),
            files: vec![(PathBuf::from("f"), GitStatus::Modified); n],
            ..Default::default()
        };
        let old = vec![rc("/r", 1), rc("/r/a", 2)];
        assert_eq!(merge(&old, None, vec![rc("/r/a", 0)]), vec![rc("/r", 1), rc("/r/a", 0)]);
        let found = Some(vec!["/r".into(), "/r/b".into()]);
        assert_eq!(merge(&old, found, vec![]), vec![rc("/r", 1), rc("/r/b", 0)]);
    }

    #[test]
    fn waits_for_quiet_but_not_forever() {
        let mut d = Dirty::new();
        assert_eq!(d.due(), d.first + QUIET);
        d.last = d.first + Duration::from_secs(5);
        assert_eq!(d.due(), d.first + MAX_WAIT);
        d.now = true;
        assert_eq!(d.due(), d.first);
    }
}
