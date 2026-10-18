//! Right pane: git changes of every repository under the opened folder
//! (the enclosing repo plus nested repos), grouped per repo. Clicking a file
//! previews its diff in a popup.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui_kit::component::{ActiveTheme, IconName};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tern_fs::git::RepoChanges;
use tern_fs::{ChangeBatch, GitStatus};

use crate::{file_icons, style};

/// The element a row is drawn as ([`style`]).
const ROW: &str = "changes.row";

/// Changes are gathered until the disk is quiet this long...
const QUIET: Duration = Duration::from_millis(250);
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
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    _timer: Option<Task<()>>,
    _task: Option<Task<()>>,
}

/// Changes waiting for the next refresh.
struct Dirty {
    /// Look for repos again (a `.git` appeared or vanished), then refresh all.
    rediscover: bool,
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
        Self { rediscover: false, all: false, git: HashSet::new(), paths: HashSet::new(), now: false, first: t, last: t }
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
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _timer: None,
            _task: None,
        };
        this.refresh(true, cx);
        this
    }

    /// Paths changed on disk (or git metadata changed): refresh the status
    /// of the repos they belong to once the burst is over, and rediscover
    /// repos if a `.git` entry appeared or vanished.
    pub fn files_changed(&mut self, batch: &ChangeBatch, cx: &mut Context<Self>) {
        let d = self.dirty.get_or_insert_with(Dirty::new);
        d.last = Instant::now();
        d.rediscover |= batch.paths.iter().any(|p| p.file_name().is_some_and(|n| n == ".git"));
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
        let root = self.root.clone();
        let known: Vec<PathBuf> = self.repos.iter().map(|r| r.root.clone()).collect();
        self._task = Some(cx.spawn(async move |this, cx| {
            let (list, fresh) = cx
                .background_spawn(async move {
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
                                    (!skip).then(|| tern_fs::git::repo_changes(r, &root))
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
        let look = style::el(ROW, cx);
        let (fg, muted, hover, active) =
            (look.color("text"), look.color("detail_text"), look.color("hover_background"), look.color("selected_background"));
        let detail_size = look.len("detail_font_size");
        let selected_text = look.is_set("selected_text").then(|| look.color("selected_text"));
        let danger = theme.danger;
        range
            .filter_map(|ix| {
                let row = |depth| {
                    style::list_row(ROW, ix, depth, cx)
                        .hover(|d| d.bg(hover))
                        .on_click(cx.listener(move |this, _, _, cx| this.click(ix, cx)))
                };
                Some(match *self.rows.get(ix)? {
                    RowKind::Repo(ri) => {
                        let repo = &self.repos[ri];
                        let open = !self.collapsed.contains(&repo.root);
                        let branch = repo.branch.as_deref().map(|b| b.split("...").next().unwrap_or(b).to_owned());
                        let detail: SharedString = match (&repo.error, repo.files.len()) {
                            (Some(e), _) => e.clone().into(),
                            (None, 0) => "clean".into(),
                            (None, n) => n.to_string().into(),
                        };
                        row(0)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(style::chevron(ROW, open, cx))
                            .child(div().flex_shrink_0().text_color(fg).child(self.repo_label(&repo.root)))
                            .when_some(branch, |d, b| {
                                d.child(div().flex_1().min_w_0().truncate().text_size(detail_size).text_color(muted).child(b))
                            })
                            .when(repo.branch.is_none(), |d| d.child(div().flex_1()))
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_size(detail_size)
                                    .text_color(if repo.error.is_some() { danger } else { muted })
                                    .child(detail),
                            )
                            .into_any_element()
                    },
                    RowKind::File(ri, fi) => {
                        let repo = &self.repos[ri];
                        let (path, st) = &repo.files[fi];
                        let color = style::git_color(*st, cx);
                        let rel = path.strip_prefix(&repo.root).unwrap_or(path);
                        let name = rel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        let dir = rel.parent().map(|p| p.display().to_string()).unwrap_or_default();
                        let selected = self.selected.as_deref() == Some(path.as_path());
                        let (icon, icon_color) = file_icons::file_icon(&name, theme);
                        row(1)
                            .when(selected, |d| d.bg(active).when_some(selected_text, |d, c| d.text_color(c)))
                            .child(style::chevron_space(ROW, cx))
                            .child(style::row_icon(ROW, icon, icon_color, cx))
                            .child(div().flex_shrink_0().text_color(color).child(name))
                            .child(div().flex_1().min_w_0().truncate().text_size(detail_size).text_color(muted).child(dir))
                            .child(div().flex_shrink_0().text_size(detail_size).text_color(color).child(st.letter().to_string()))
                            .into_any_element()
                    },
                })
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
        let buttons = [
            style::header_button("changes.header.button", "refresh", IconName::RefreshCw, cx)
                .loading(self.refreshing)
                .tooltip("Refresh")
                .on_click(cx.listener(|this, _, _, cx| this.refresh(true, cx)))
                .into_any_element(),
            style::header_button("changes.header.button", "hide-changes", IconName::Close, cx)
                .tooltip(format!("Hide Changes ({})", crate::platform::shortcut("⌘⇧G")))
                .on_click(cx.listener(|_, _, _, cx| cx.emit(ChangesEvent::Hide)))
                .into_any_element(),
        ];
        let header = crate::dock::panel_header("changes", "changes.header", "Changes", Some(total), buttons, cx);
        let body: AnyElement = if self.repos.is_empty() {
            style::el("changes.empty", cx)
                .apply(div().flex_1().flex().items_center().justify_center())
                .child(if self.refreshing { "Scanning…" } else { "No git repositories" })
                .into_any_element()
        } else {
            style::el("changes.list", cx)
                .apply(uniform_list("changes", self.rows.len(), cx.processor(Self::render_rows)).track_scroll(&self.scroll).flex_1())
                .into_any_element()
        };
        let panel = style::el("changes", cx);
        panel.container(div().track_focus(&self.focus).size_full().flex().flex_col()).child(header).child(body)
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
