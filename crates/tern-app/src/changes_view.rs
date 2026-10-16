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
use tern_fs::GitStatus;
use tern_fs::git::RepoChanges;

use crate::{file_icons, style};

/// The element a row is drawn as ([`style`]).
const ROW: &str = "changes.row";

const MIN_REFRESH_INTERVAL: Duration = Duration::from_millis(500);

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
    /// A refresh is running; further requests are coalesced into `pending`.
    refreshing: bool,
    /// `Some(rediscover)` if another refresh was requested meanwhile.
    pending: Option<bool>,
    /// When the last refresh started: runs are at least
    /// [`MIN_REFRESH_INTERVAL`] apart, however busy the disk.
    last_refresh: Option<Instant>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    _task: Option<Task<()>>,
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
            refreshing: false,
            pending: None,
            last_refresh: None,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _task: None,
        };
        this.refresh(true, cx);
        this
    }

    /// Paths changed on disk (or git metadata changed): refresh status, and
    /// rediscover repos if a `.git` entry appeared or vanished.
    pub fn files_changed(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        let rediscover = paths.iter().any(|p| p.file_name().is_some_and(|n| n == ".git"));
        self.refresh(rediscover, cx);
    }

    pub fn refresh(&mut self, rediscover: bool, cx: &mut Context<Self>) {
        if self.refreshing {
            self.pending = Some(self.pending.unwrap_or(false) | rediscover);
            return;
        }
        self.refreshing = true;
        let wait = self.last_refresh.map_or(Duration::ZERO, |t| MIN_REFRESH_INTERVAL.saturating_sub(t.elapsed()));
        self.last_refresh = Some(Instant::now() + wait);
        let root = self.root.clone();
        let known: Vec<PathBuf> = self.repos.iter().map(|r| r.root.clone()).collect();
        self._task = Some(cx.spawn(async move |this, cx| {
            if !wait.is_zero() {
                cx.background_executor().timer(wait).await;
            }
            let repos = cx
                .background_spawn(async move {
                    let list = if rediscover || known.is_empty() { tern_fs::git::find_repos(&root) } else { known };
                    // One git process per repo, in parallel.
                    std::thread::scope(|s| {
                        let handles: Vec<_> = list.iter().map(|r| s.spawn(|| tern_fs::git::repo_changes(r, &root))).collect();
                        handles.into_iter().filter_map(|h| h.join().ok()).collect::<Vec<_>>()
                    })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.repos = repos;
                this.rebuild_rows();
                this.refreshing = false;
                cx.emit(ChangesEvent::Updated(tern_fs::git::status_map(&this.repos, &this.root)));
                cx.notify();
                if let Some(rediscover) = this.pending.take() {
                    this.refresh(rediscover, cx);
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
