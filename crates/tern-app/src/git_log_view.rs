//! The Git Log tab, as IntelliJ's: the commits of a repository under the
//! workspace's folder with their graph, refs, author and date, filtered by
//! branch, user and a search; the selected commit's message and files on
//! the side, a file's diff in that commit on click.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui_kit::component::{ActiveTheme, Sizable as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::DropdownMenu as _;
use gpui_kit::*;
use tern_fs::ChangeBatch;
use tern_fs::git_log::{At, Commit, CommitDetails, CommitFile, GraphRow, LogQuery, Ref, RefKind};

use crate::template::{self, Framed, on_event};
use crate::{file_icons, style};

/// The element a commit row is drawn as ([`style`]).
const ROW: &str = "git_log.row";
/// The element a file of the selected commit is drawn as.
const FILE: &str = "git_log.file";
/// The most commits listed.
const LIMIT: usize = 5000;
/// The widest the graph gets, in lanes; lines past it are cut.
const MAX_LANES: usize = 24;

#[derive(Debug, Clone)]
pub enum GitLogEvent {
    /// Show `file`'s diff in commit `hash` of `repo`.
    PreviewDiff { repo: PathBuf, hash: String, file: CommitFile },
}

pub struct GitLog {
    root: PathBuf,
    repos: Vec<PathBuf>,
    repo: usize,
    branches: Vec<String>,
    /// `None`: every branch.
    branch: Option<String>,
    /// `None`: every author.
    user: Option<String>,
    /// The authors of the commits listed without a user filter.
    authors: Vec<String>,
    search: Entity<InputState>,
    commits: Vec<Commit>,
    graph: Vec<GraphRow>,
    /// Lanes the graph column is wide.
    lanes: usize,
    /// The commits the search lets through, by index.
    shown: Vec<usize>,
    selected: Option<String>,
    details: Option<CommitDetails>,
    selected_file: Option<usize>,
    loading: bool,
    error: Option<String>,
    scroll: UniformListScrollHandle,
    files_scroll: UniformListScrollHandle,
    focus: FocusHandle,
    _load: Option<Task<()>>,
    _details: Option<Task<()>>,
    _search_sub: Subscription,
}

impl EventEmitter<GitLogEvent> for GitLog {}

impl Focusable for GitLog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl GitLog {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search subject, hash, author"));
        let search_sub = cx.subscribe(&search, |this, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                this.filter(cx);
            }
        });
        let mut this = Self {
            root,
            repos: vec![],
            repo: 0,
            branches: vec![],
            branch: None,
            user: None,
            authors: vec![],
            search,
            commits: vec![],
            graph: vec![],
            lanes: 0,
            shown: vec![],
            selected: None,
            details: None,
            selected_file: None,
            loading: true,
            error: None,
            scroll: UniformListScrollHandle::new(),
            files_scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _load: None,
            _details: None,
            _search_sub: search_sub,
        };
        this.find_repos(cx);
        this
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Look for the repos under the folder, then list the first one's log.
    fn find_repos(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        self.loading = true;
        self._load = Some(cx.spawn(async move |this, cx| {
            let repos = cx.background_spawn(async move { tern_fs::git::find_repos(&root) }).await;
            let _ = this.update(cx, |this, cx| {
                let current = this.repos.get(this.repo).cloned();
                this.repo = current.and_then(|c| repos.iter().position(|r| *r == c)).unwrap_or(0);
                this.repos = repos;
                this.load(cx);
            });
        }));
    }

    /// List the log again with the current filters.
    pub fn load(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repos.get(self.repo).cloned() else {
            self.loading = false;
            self.commits.clear();
            self.graph.clear();
            self.error = Some("No git repository".into());
            self.filter(cx);
            return;
        };
        self.loading = true;
        cx.notify();
        let query = LogQuery { branch: self.branch.clone(), author: self.user.clone(), limit: LIMIT };
        self._load = Some(cx.spawn(async move |this, cx| {
            let (log, branches) = cx
                .background_spawn(async move {
                    let log = tern_fs::git_log::log(&repo, &query).map(|commits| {
                        let graph = tern_fs::git_log::graph(&commits);
                        (commits, graph)
                    });
                    (log, tern_fs::git_log::branches(&repo))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                this.branches = branches;
                match log {
                    Ok((commits, graph)) => {
                        this.error = None;
                        if this.user.is_none() {
                            let mut authors: Vec<String> = commits.iter().map(|c| c.author.clone()).collect();
                            authors.sort_by_key(|a| a.to_lowercase());
                            authors.dedup();
                            this.authors = authors;
                        }
                        this.lanes = graph.iter().map(|g| g.width).max().unwrap_or(0).min(MAX_LANES);
                        this.commits = commits;
                        this.graph = graph;
                    },
                    Err(e) => {
                        this.error = Some(e);
                        this.commits.clear();
                        this.graph.clear();
                    },
                }
                this.filter(cx);
            });
        }));
    }

    /// Git metadata changed (a commit, a checkout, a fetch): list again.
    pub fn files_changed(&mut self, batch: &ChangeBatch, cx: &mut Context<Self>) {
        if self.repos.get(self.repo).is_some_and(|r| batch.git_repos.contains(r)) {
            self.load(cx);
        }
    }

    /// Show the commits the search lets through; keep the selection if it
    /// is among them, else select the first.
    fn filter(&mut self, cx: &mut Context<Self>) {
        let q = self.search.read(cx).value().trim().to_lowercase();
        self.shown = (0..self.commits.len())
            .filter(|&i| {
                let c = &self.commits[i];
                q.is_empty() || c.hash.starts_with(&q) || c.subject.to_lowercase().contains(&q) || c.author.to_lowercase().contains(&q)
            })
            .collect();
        let keep = self.selected.as_ref().is_some_and(|h| self.shown.iter().any(|&i| self.commits[i].hash == *h));
        if !keep {
            match self.shown.first() {
                Some(&i) => self.select(i, cx),
                None => {
                    self.selected = None;
                    self.details = None;
                },
            }
        } else if let Some(h) = self.selected.clone() {
            // The commit may have new refs, or a reworded amend under the same name.
            if self.details.as_ref().is_none_or(|d| d.hash != h) {
                self.load_details(h, cx);
            }
        }
        cx.notify();
    }

    /// Whether the graph shows: a search or a user filter leaves gaps it
    /// can't draw across.
    fn graph_shown(&self, cx: &App) -> bool {
        self.user.is_none() && self.search.read(cx).value().trim().is_empty() && self.lanes > 0
    }

    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(c) = self.commits.get(ix) else { return };
        if self.selected.as_deref() == Some(c.hash.as_str()) {
            return;
        }
        let hash = c.hash.clone();
        self.selected = Some(hash.clone());
        self.selected_file = None;
        self.load_details(hash, cx);
        cx.notify();
    }

    fn load_details(&mut self, hash: String, cx: &mut Context<Self>) {
        let Some(repo) = self.repos.get(self.repo).cloned() else { return };
        self._details = Some(cx.spawn(async move |this, cx| {
            let details = cx.background_spawn(async move { tern_fs::git_log::details(&repo, &hash) }).await;
            let _ = this.update(cx, |this, cx| {
                this.details = details.ok().filter(|d| this.selected.as_deref() == Some(d.hash.as_str()));
                cx.notify();
            });
        }));
    }

    /// Move the selection `by` rows among the shown commits.
    fn step(&mut self, by: isize, cx: &mut Context<Self>) {
        let at = self.selected.as_ref().and_then(|h| self.shown.iter().position(|&i| self.commits[i].hash == *h));
        let next = match at {
            Some(p) => p.saturating_add_signed(by).min(self.shown.len().saturating_sub(1)),
            None => 0,
        };
        if let Some(&i) = self.shown.get(next) {
            self.select(i, cx);
            self.scroll.scroll_to_item(next, ScrollStrategy::Nearest);
        }
    }

    fn open_file(&mut self, fi: usize, cx: &mut Context<Self>) {
        let (Some(d), Some(repo)) = (&self.details, self.repos.get(self.repo)) else { return };
        let Some(file) = d.files.get(fi).cloned() else { return };
        self.selected_file = Some(fi);
        cx.emit(GitLogEvent::PreviewDiff { repo: repo.clone(), hash: d.hash.clone(), file });
        cx.notify();
    }

    fn set_repo(&mut self, repo: usize, cx: &mut Context<Self>) {
        self.repo = repo;
        self.branch = None;
        self.user = None;
        self.selected = None;
        self.details = None;
        self.load(cx);
    }

    fn repo_label(&self, repo: &Path) -> String {
        match repo.strip_prefix(&self.root) {
            Ok(rel) if !rel.as_os_str().is_empty() => rel.display().to_string(),
            _ => repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| repo.display().to_string()),
        }
    }

    fn render_rows(&mut self, range: Range<usize>, _: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        let graph = self.graph_shown(cx).then(|| GraphLook::new(self.lanes, cx));
        range
            .filter_map(|ix| {
                let ci = *self.shown.get(ix)?;
                let c = &self.commits[ci];
                let mut scope = template::Scope::new(ROW)
                    .var("selected", self.selected.as_deref() == Some(c.hash.as_str()))
                    .var("subject", c.subject.clone())
                    .var("author", c.author.clone())
                    .var("date", when(c, now))
                    .list("refs", refs(&c.refs));
                if let (Some(look), Some(row)) = (&graph, self.graph.get(ci)) {
                    scope = scope.slot("graph", look.row(row.clone()));
                }
                Some(template::render(
                    ROW,
                    scope,
                    &mut |name, _, f| match (name, f) {
                        ("row", Framed::Box(d)) => d
                            .id(ix)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                window.focus(&this.focus, cx);
                                this.select(ci, cx);
                            }))
                            .into_any_element(),
                        (_, f) => f.into_any_element(),
                    },
                    cx,
                ))
            })
            .collect()
    }

    fn render_files(&mut self, range: Range<usize>, _: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let Some(d) = &self.details else { return vec![] };
        range
            .filter_map(|ix| {
                let f = d.files.get(ix)?;
                let (dir, name) = match f.path.rsplit_once('/') {
                    Some((dir, name)) => (dir.to_owned(), name.to_owned()),
                    None => (String::new(), f.path.clone()),
                };
                let (icon, icon_color) = file_icons::file_icon(&name, theme);
                let scope = template::Scope::new(FILE)
                    .var("selected", self.selected_file == Some(ix))
                    .var("name", name)
                    .var("name_color", style::git_color(f.status, cx))
                    .var("dir", f.from.as_ref().map_or(dir, |from| format!("from {from}")))
                    .var("status", f.status.letter().to_string())
                    .slot("icon", style::row_icon(FILE, icon, icon_color, cx));
                Some(template::render(
                    FILE,
                    scope,
                    &mut |name, _, f| match (name, f) {
                        ("row", Framed::Box(d)) => {
                            d.id(ix).on_click(cx.listener(move |this, _, _, cx| this.open_file(ix, cx))).into_any_element()
                        },
                        (_, f) => f.into_any_element(),
                    },
                    cx,
                ))
            })
            .collect()
    }

    /// The filter menu of `what` (`repo`, `branch`, `user`): its options and
    /// which one is on.
    fn options(&self, what: &str) -> (Vec<String>, usize) {
        let pick = |all: &[String], on: &Option<String>| {
            let opts: Vec<String> = std::iter::once("All".to_owned()).chain(all.iter().cloned()).collect();
            let at = on.as_ref().and_then(|o| all.iter().position(|a| a == o)).map_or(0, |p| p + 1);
            (opts, at)
        };
        match what {
            "repo" => (self.repos.iter().map(|r| self.repo_label(r)).collect(), self.repo),
            "branch" => pick(&self.branches, &self.branch),
            _ => pick(&self.authors, &self.user),
        }
    }

    fn pick(&mut self, what: &str, at: usize, cx: &mut Context<Self>) {
        let opt = |all: &[String]| at.checked_sub(1).and_then(|i| all.get(i).cloned());
        match what {
            "repo" => self.set_repo(at, cx),
            "branch" => {
                self.branch = opt(&self.branches);
                self.load(cx);
            },
            _ => {
                self.user = opt(&self.authors);
                self.load(cx);
            },
        }
    }

    /// The details side's values, for the selected commit.
    fn details_scope(&self, scope: template::Scope) -> template::Scope {
        let Some(d) = &self.details else { return scope.var("selected", false) };
        let (subject, body) = match d.message.split_once('\n') {
            Some((s, b)) => (s.to_owned(), b.trim().to_owned()),
            None => (d.message.clone(), String::new()),
        };
        let commit_refs = self.commits.iter().find(|c| c.hash == d.hash).map(|c| refs(&c.refs)).unwrap_or_default();
        let committed = d.committer != d.author || d.commit_date != d.date;
        let files = match d.files.len() {
            1 => "1 file changed".to_owned(),
            n => format!("{n} files changed"),
        };
        scope
            .var("selected", true)
            .var("subject", subject)
            .var("body", body)
            .var("hash", d.hash.clone())
            .var("short", d.hash.chars().take(8).collect::<String>())
            .var("author", d.author.clone())
            .var("email", d.email.clone())
            .var("date", d.date.clone())
            .var("committer", if committed { d.committer.clone() } else { String::new() })
            .var("commit_date", d.commit_date.clone())
            .var("files_text", files)
            .list("refs", commit_refs)
    }
}

/// `c`'s date as the list shows it: how long ago within a day, else the date.
fn when(c: &Commit, now: i64) -> String {
    let ago = now - c.time;
    match ago {
        ..0 => c.date.clone(),
        0..60 => "Just now".into(),
        60..3600 => format!("{} min ago", ago / 60),
        3600..86400 => format!("{} h ago", ago / 3600),
        _ => c.date.clone(),
    }
}

/// A commit's refs as the templates' `refs` list.
fn refs(refs: &[Ref]) -> Vec<template::Scope> {
    refs.iter()
        .map(|r| {
            template::Scope::default()
                .var("name", r.name.clone())
                .var("head", matches!(r.kind, RefKind::Head | RefKind::Branch { head: true }))
                .var("remote", r.kind == RefKind::Remote)
                .var("tag", r.kind == RefKind::Tag)
        })
        .collect()
}

/// How the graph is drawn (element `git_log.graph`).
#[derive(Clone)]
struct GraphLook {
    lanes: usize,
    lane: Pixels,
    line: Pixels,
    dot: Pixels,
    pad: Pixels,
    colors: Rc<[Hsla]>,
}

impl GraphLook {
    fn new(lanes: usize, cx: &App) -> Self {
        let el = style::el("git_log.graph", cx);
        let colors = ["color_1", "color_2", "color_3", "color_4", "color_5", "color_6"].map(|c| el.color(c));
        Self {
            lanes,
            lane: el.px("lane_width"),
            line: el.px("line_width"),
            dot: el.px("dot_size"),
            pad: el.px("padding_left"),
            colors: Rc::new(colors),
        }
    }

    /// The graph's piece in one commit's row.
    fn row(&self, row: GraphRow) -> AnyElement {
        let look = self.clone();
        let width = look.pad + look.lane * look.lanes as f32;
        div()
            .flex_shrink_0()
            .w(width)
            .h_full()
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        let o = bounds.origin;
                        let h = bounds.size.height;
                        let x = |lane: usize| o.x + look.pad + look.lane * (lane as f32 + 0.5);
                        let y = |at: At| match at {
                            At::Top => o.y,
                            At::Middle => o.y + h / 2.,
                            At::Bottom => o.y + h,
                        };
                        let color = |k: usize| look.colors[k % look.colors.len()];
                        for e in &row.edges {
                            if e.from.0 >= look.lanes || e.to.0 >= look.lanes {
                                continue;
                            }
                            let (a, b) = (point(x(e.from.0), y(e.from.1)), point(x(e.to.0), y(e.to.1)));
                            let mut pb = PathBuilder::stroke(look.line);
                            pb.move_to(a);
                            if a.x == b.x {
                                pb.line_to(b);
                            } else {
                                // Bend toward the other lane through the row's half.
                                let mid = point(a.x, (a.y + b.y) / 2.);
                                pb.curve_to(b, point(b.x, mid.y));
                            }
                            if let Ok(path) = pb.build() {
                                window.paint_path(path, color(e.color));
                            }
                        }
                        if row.lane < look.lanes {
                            let c = point(x(row.lane), y(At::Middle));
                            let r = look.dot / 2.;
                            let points: Vec<Point<Pixels>> = (0..16)
                                .map(|i| {
                                    let t = i as f32 / 16. * std::f32::consts::TAU;
                                    point(c.x + r * t.cos(), c.y + r * t.sin())
                                })
                                .collect();
                            let mut pb = PathBuilder::fill();
                            pb.add_polygon(&points, true);
                            if let Ok(path) = pb.build() {
                                window.paint_path(path, color(row.color));
                            }
                        }
                    },
                )
                .size_full(),
            )
            .into_any_element()
    }
}

impl Render for GitLog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let me = cx.entity().downgrade();
        let message = match (&self.error, self.loading, self.shown.is_empty()) {
            (Some(e), _, _) => e.clone(),
            (None, true, true) => "Loading…".to_owned(),
            (None, false, true) if !self.commits.is_empty() => "No commits match".to_owned(),
            (None, false, true) => "No commits".to_owned(),
            _ => String::new(),
        };
        let count_text = match self.shown.len() {
            _ if self.loading && self.commits.is_empty() => String::new(),
            n if n == self.commits.len() && n >= LIMIT => format!("latest {n} commits"),
            n if n == self.commits.len() => format!("{n} commits"),
            n => format!("{n} of {}", self.commits.len()),
        };
        let mut scope = template::Scope::new("git_log")
            .var("message", message)
            .var("loading", self.loading)
            .var("repos", self.repos.len())
            .var("repo", self.repos.get(self.repo).map(|r| self.repo_label(r)).unwrap_or_default())
            .var("branch", self.branch.clone().unwrap_or_else(|| "All".into()))
            .var("user", self.user.clone().unwrap_or_else(|| "All".into()))
            .var("count_text", count_text)
            .slot("input", Input::new(&self.search).small().cleanable(true));
        scope = self.details_scope(scope);
        if !self.shown.is_empty() {
            let list = uniform_list("git-log", self.shown.len(), cx.processor(Self::render_rows)).track_scroll(&self.scroll).size_full();
            scope = scope.slot("list", style::el("git_log.list", cx).apply(div().id("git-log-list").flex_1().min_h_0().child(list)));
        }
        if let Some(d) = &self.details {
            let list = uniform_list("git-log-files", d.files.len(), cx.processor(Self::render_files)).track_scroll(&self.files_scroll).size_full();
            scope = scope.slot("files", style::el("git_log.files", cx).apply(div().id("git-log-files").size_full().child(list)));
        }
        let focus = &self.focus;
        template::render(
            "git_log",
            scope,
            &mut |name, _, f| match (name, f) {
                ("git_log", Framed::Box(d)) => d
                    .track_focus(focus)
                    .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| match ev.keystroke.key.as_str() {
                        "up" => this.step(-1, cx),
                        "down" => this.step(1, cx),
                        "pageup" => this.step(-20, cx),
                        "pagedown" => this.step(20, cx),
                        _ => {},
                    }))
                    .into_any_element(),
                ("refresh", Framed::Button(b)) => {
                    b.on_click(on_event(&me, |this, _: &ClickEvent, _, cx| this.find_repos(cx))).into_any_element()
                },
                ("copy_hash", Framed::Button(b)) => {
                    let hash = self.details.as_ref().map(|d| d.hash.clone()).unwrap_or_default();
                    b.on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(hash.clone()))).into_any_element()
                },
                (what @ ("repo" | "branch" | "user"), Framed::Button(b)) => {
                    let what: &'static str = match what {
                        "repo" => "repo",
                        "branch" => "branch",
                        _ => "user",
                    };
                    let me = me.clone();
                    let (options, on) = self.options(what);
                    b.dropdown_caret(true)
                        .dropdown_menu(move |m, window, cx| {
                            let entries =
                                options.iter().enumerate().map(|(i, o)| template::Scope::default().var("label", o.clone()).var("on", i == on)).collect();
                            let me = me.clone();
                            let items: template::MenuItems = Rc::new(move |name, entry| {
                                let at = entry.filter(|_| name == "pick")?;
                                let me = me.clone();
                                Some(Rc::new(move |_, _, cx| {
                                    let _ = me.update(cx, |this, cx| this.pick(what, at, cx));
                                }))
                            });
                            let scope = template::Scope::new("git-log-filter").list("options", entries);
                            template::menu("menu.git_log_filter", scope, m.scrollable(true), items, window, cx)
                        })
                        .into_any_element()
                },
                (_, f) => f.into_any_element(),
            },
            cx,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::when;
    use tern_fs::git_log::Commit;

    #[test]
    fn dates_within_a_day_read_as_how_long_ago() {
        let c = Commit {
            hash: "h".into(),
            parents: vec![],
            author: String::new(),
            email: String::new(),
            time: 1_000_000,
            date: "2026-10-20 14:03".into(),
            refs: vec![],
            subject: String::new(),
        };
        assert_eq!(when(&c, 1_000_030), "Just now");
        assert_eq!(when(&c, 1_000_000 + 125), "2 min ago");
        assert_eq!(when(&c, 1_000_000 + 7300), "2 h ago");
        assert_eq!(when(&c, 1_000_000 + 90_000), "2026-10-20 14:03");
    }
}
