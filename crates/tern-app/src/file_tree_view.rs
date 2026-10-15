//! Explorer pane: virtualized rows over [`tern_fs::FileTree`], live-updated by
//! the fs watcher, with git status and "edited by Claude" badges.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tern_fs::{ChangeBatch, FileTree, FsWatcher, GitStatus};

use crate::file_icons;

const ROW_HEIGHT: f32 = 22.;
const INDENT: f32 = 14.;

#[derive(Debug, Clone)]
pub enum FileTreeEvent {
    /// A file clicked: once to preview it, twice (`pin`) to keep its tab.
    OpenFile { path: PathBuf, pin: bool },
    /// Batch of changed paths from disk (for the viewer to reload). Empty
    /// when only git metadata changed.
    Changed(Vec<PathBuf>),
    /// The header's close button: hide the explorer.
    Hide,
}

pub struct FileTreeView {
    tree: FileTree,
    git: HashMap<PathBuf, GitStatus>,
    claude_touched: HashSet<PathBuf>,
    selected: Option<PathBuf>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    _watcher: Option<FsWatcher>,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<FileTreeEvent> for FileTreeView {}

impl Focusable for FileTreeView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl FileTreeView {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let (tx, rx) = flume::unbounded::<ChangeBatch>();
        let watcher = FsWatcher::new(&root, Duration::from_millis(40), move |b| {
            let _ = tx.send(b);
        })
        .map_err(|e| log::warn!("fs watcher failed: {e}"))
        .ok();

        let pump = cx.spawn(async move |this, cx| {
            while let Ok(first) = rx.recv_async().await {
                let mut paths = first.paths;
                let mut git_changed = first.git_changed;
                for b in rx.try_iter() {
                    paths.extend(b.paths);
                    git_changed |= b.git_changed;
                }
                let ok = this.update(cx, |view, cx| {
                    if view.tree.apply_changes(paths.iter().map(PathBuf::as_path)) {
                        cx.notify();
                    }
                    if !paths.is_empty() || git_changed {
                        cx.emit(FileTreeEvent::Changed(paths));
                    }
                });
                if ok.is_err() {
                    break;
                }
            }
        });

        Self {
            tree: FileTree::new(root),
            git: HashMap::new(),
            claude_touched: HashSet::new(),
            selected: None,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _watcher: watcher,
            _tasks: vec![pump],
        }
    }

    /// Git status from the changes panel (all repos under the root).
    pub fn set_git(&mut self, git: HashMap<PathBuf, GitStatus>, cx: &mut Context<Self>) {
        self.git = git;
        cx.notify();
    }

    /// Mark files Claude edited (from hooks).
    pub fn mark_claude_edit(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        self.claude_touched.extend(paths.iter().cloned());
        self.tree.apply_changes(paths.iter().map(PathBuf::as_path));
        cx.notify();
    }

    pub fn reveal(&mut self, path: &Path, cx: &mut Context<Self>) {
        if let Some(ix) = self.tree.reveal(path) {
            self.selected = Some(path.to_path_buf());
            self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
            cx.notify();
        }
    }

    fn click(&mut self, ix: usize, clicks: usize, cx: &mut Context<Self>) {
        let Some(row) = self.tree.rows().get(ix) else { return };
        let path = row.entry.path.to_path_buf();
        self.selected = Some(path.clone());
        if row.entry.is_dir {
            // A double click's second click would fold it back.
            if clicks == 1 {
                self.tree.toggle(&path);
            }
        } else {
            cx.emit(FileTreeEvent::OpenFile { path, pin: clicks > 1 });
        }
        cx.notify();
    }

    /// The row likely to be widest, which the list measures for its
    /// scrollable width.
    fn widest_row(&self) -> usize {
        let width = |r: &tern_fs::tree::Row| r.depth as f32 * INDENT + r.entry.name.chars().count() as f32 * 7.;
        (0..self.tree.rows().len())
            .max_by(|&a, &b| width(&self.tree.rows()[a]).total_cmp(&width(&self.tree.rows()[b])))
            .unwrap_or(0)
    }

    fn render_rows(&mut self, range: Range<usize>, _: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let (fg, muted, hover, active) = (theme.foreground, theme.muted_foreground, theme.list_hover, theme.list_active);
        let (warn, ok, accent) = (theme.warning, theme.success, theme.accent_foreground);
        range
            .filter_map(|ix| {
                let row = self.tree.rows().get(ix)?;
                let path: &Path = &row.entry.path;
                let selected = self.selected.as_deref() == Some(path);
                let git = self.git.get(path).copied();
                let claude = self.claude_touched.contains(path);
                let name_color = match git {
                    Some(GitStatus::Untracked | GitStatus::Added) => ok,
                    Some(GitStatus::Modified | GitStatus::Renamed) => warn,
                    Some(GitStatus::Deleted | GitStatus::Conflicted) => theme.danger,
                    None => fg,
                };
                let name = &*row.entry.name;
                let (icon, icon_color) = if row.entry.is_dir {
                    file_icons::dir_icon(name, row.expanded, theme)
                } else {
                    file_icons::file_icon(name, theme)
                };
                let chevron = row.entry.is_dir.then(|| {
                    if row.expanded { IconName::ChevronDown } else { IconName::ChevronRight }
                });
                Some(
                    div()
                        .id(ix)
                        .h(px(ROW_HEIGHT))
                        .w_full()
                        .flex()
                        .items_center()
                        .gap_1()
                        .pl(px(6. + INDENT * row.depth as f32))
                        .pr_2()
                        .text_sm()
                        .when(selected, |d| d.bg(active))
                        .hover(|d| d.bg(hover))
                        .on_click(cx.listener(move |this, ev: &ClickEvent, _, cx| this.click(ix, ev.click_count(), cx)))
                        .child(match chevron {
                            Some(c) => Icon::new(c).size_3().text_color(muted).into_any_element(),
                            None => div().size_3().flex_shrink_0().into_any_element(),
                        })
                        .child(Icon::new(icon).size_4().text_color(icon_color))
                        .child(div().flex_grow(1.).whitespace_nowrap().text_color(name_color).child(row.entry.name.to_string()))
                        .when(claude, |d| d.child(div().text_xs().text_color(accent).child("✦")))
                        .when_some(git.filter(|_| !row.entry.is_dir), |d, g| {
                            d.child(div().text_xs().text_color(name_color).child(g.letter().to_string()))
                        })
                        .into_any_element(),
                )
            })
            .collect()
    }
}

impl Render for FileTreeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let root_name = self
            .tree
            .root()
            .file_name()
            .map(|n| n.to_string_lossy().to_uppercase())
            .unwrap_or_default();
        div()
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.sidebar)
            .child(
                div()
                    .h(px(30.))
                    .pl_3()
                    .pr_1()
                    .flex()
                    .items_center()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted_foreground)
                    .child(div().flex_1().truncate().child(root_name))
                    .child(
                        Button::new("hide-explorer")
                            .xsmall()
                            .ghost()
                            .icon(IconName::Close)
                            .tooltip("Hide Explorer (⌘B)")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(FileTreeEvent::Hide))),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        uniform_list("file-tree", self.tree.rows().len(), cx.processor(Self::render_rows))
                            .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                            .with_width_from_item(Some(self.widest_row()))
                            .track_scroll(&self.scroll)
                            .size_full(),
                    )
                    // Both bars, so names wider than the panel can be scrolled to.
                    .child(Scrollbar::new(&self.scroll).id("file-tree-scrollbar")),
            )
    }
}
