//! Explorer pane: virtualized rows over [`tern_fs::FileTree`], live-updated by
//! the fs watcher, with git status and "edited by Claude" badges. Files and
//! folders are created, renamed, moved, copied and trashed the way VS Code's
//! explorer does it: inline name editing, a context menu, multi-selection,
//! drag and drop and the same keys.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu};
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::{ActiveTheme, Sizable, WindowExt};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tern_fs::{ChangeBatch, FileTree, FsWatcher, GitStatus, ops};

use crate::file_icons;
use crate::platform::bind;
use crate::style;
use crate::template::{self, Framed, on_event};

const CONTEXT: &str = "FileTree";
/// The element a row is drawn as ([`style`]).
const ROW: &str = "explorer.row";

actions!(
    file_tree,
    [
        NewFile,
        NewFolder,
        Rename,
        Delete,
        CopyEntries,
        CutEntries,
        Paste,
        CopyPath,
        CopyRelativePath,
        RevealInFinder,
        OpenLiveServer,
        CollapseAll,
        Refresh,
        SelectPrev,
        SelectNext,
        CollapseEntry,
        ExpandEntry,
        OpenEntry,
        OpenEntryPinned,
        Cancel,
    ]
);

/// VS Code's explorer keys (macOS). Not while the name field is focused,
/// where the same keys edit text.
pub fn bind_keys(cx: &mut App) {
    let ctx = Some("FileTree && !Input");
    cx.bind_keys([
        bind("enter", Rename, ctx),
        bind("f2", Rename, ctx),
        bind("cmd-backspace", Delete, ctx),
        bind("delete", Delete, ctx),
        bind("cmd-c", CopyEntries, ctx),
        bind("cmd-x", CutEntries, ctx),
        bind("cmd-v", Paste, ctx),
        bind("cmd-alt-c", CopyPath, ctx),
        bind("cmd-alt-shift-c", CopyRelativePath, ctx),
        bind("cmd-alt-r", RevealInFinder, ctx),
        bind("up", SelectPrev, ctx),
        bind("down", SelectNext, ctx),
        bind("left", CollapseEntry, ctx),
        bind("right", ExpandEntry, ctx),
        bind("space", OpenEntry, ctx),
        bind("cmd-down", OpenEntryPinned, ctx),
        // The name field lets escape through when it has nothing to undo.
        bind("escape", Cancel, Some(CONTEXT)),
    ]);
}

#[derive(Debug, Clone)]
pub enum FileTreeEvent {
    /// A file clicked: once to preview it, twice (`pin`) to keep its tab.
    OpenFile { path: PathBuf, pin: bool },
    /// Changed paths from disk (for the viewer to reload) and repos whose
    /// git metadata changed.
    Changed(ChangeBatch),
    /// A file or folder was renamed or moved from `from` to `to`.
    Moved { from: PathBuf, to: PathBuf },
    /// Files or folders moved to the Trash.
    Deleted(Vec<PathBuf>),
    /// The header's close button: hide the explorer.
    Hide,
}

/// What the inline name field is for.
#[derive(Debug, Clone, PartialEq)]
enum EditKind {
    New { parent: PathBuf, dir: bool },
    Rename(PathBuf),
}

struct Edit {
    kind: EditKind,
    input: Entity<InputState>,
    _sub: Subscription,
}

/// Entries copied or cut with ⌘C / ⌘X, waiting for ⌘V.
struct Clip {
    paths: Vec<PathBuf>,
    cut: bool,
}

/// The payload of an explorer drag.
#[derive(Clone)]
struct DraggedEntries {
    paths: Vec<PathBuf>,
    label: SharedString,
}

impl Render for DraggedEntries {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        style::drag_chip(self.label.clone(), None, cx)
    }
}

pub struct FileTreeView {
    tree: FileTree,
    git: HashMap<PathBuf, GitStatus>,
    claude_touched: HashSet<PathBuf>,
    /// Every selected entry (⌘-click and ⇧-click add to it).
    selected: Vec<PathBuf>,
    /// The entry the keyboard acts on, and where a ⇧-click range starts.
    cursor: Option<PathBuf>,
    edit: Option<Edit>,
    clip: Option<Clip>,
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
            while let Ok(mut batch) = rx.recv_async().await {
                for b in rx.try_iter() {
                    batch.paths.extend(b.paths);
                    batch.git_repos.extend(b.git_repos);
                }
                tern_web::live::changed(&batch.paths);
                let ok = this.update(cx, |view, cx| {
                    if view.tree.apply_changes(batch.paths.iter().map(PathBuf::as_path)) {
                        cx.notify();
                    }
                    cx.emit(FileTreeEvent::Changed(batch));
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
            selected: vec![],
            cursor: None,
            edit: None,
            clip: None,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _watcher: watcher,
            _tasks: vec![pump],
        }
    }

    /// Git status from the changes panel (all repos under the root).
    pub fn set_git(&mut self, git: HashMap<PathBuf, GitStatus>, cx: &mut Context<Self>) {
        if self.git != git {
            self.git = git;
            cx.notify();
        }
    }

    /// Mark files Claude edited (from hooks).
    pub fn mark_claude_edit(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        self.claude_touched.extend(paths.iter().cloned());
        self.tree.apply_changes(paths.iter().map(PathBuf::as_path));
        cx.notify();
    }

    pub fn reveal(&mut self, path: &Path, cx: &mut Context<Self>) {
        if let Some(ix) = self.tree.reveal(path) {
            self.select_only(path.to_path_buf());
            self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
            cx.notify();
        }
    }

    fn root(&self) -> &Path {
        self.tree.root()
    }

    fn select_only(&mut self, path: PathBuf) {
        self.selected = vec![path.clone()];
        self.cursor = Some(path);
    }

    /// What the selection-wide commands act on: the selection, or the cursor.
    fn targets(&self) -> Vec<PathBuf> {
        let mut paths = self.selected.clone();
        if let Some(c) = &self.cursor
            && !paths.contains(c)
        {
            paths = vec![c.clone()];
        }
        paths.retain(|p| p != self.root());
        ops::outermost(&paths)
    }

    /// The folder new entries and pastes go into: the cursor if it is a
    /// folder, else the cursor's folder, else the root.
    fn target_dir(&self) -> PathBuf {
        match &self.cursor {
            Some(p) if p.is_dir() => p.clone(),
            Some(p) => p.parent().map(Path::to_path_buf).unwrap_or_else(|| self.root().to_path_buf()),
            None => self.root().to_path_buf(),
        }
    }

    fn rel(&self, path: &Path) -> String {
        path.strip_prefix(self.root()).unwrap_or(path).display().to_string()
    }

    // ----- rows -----

    /// Where the "new entry" field sits among the rows, and its depth.
    fn new_row(&self) -> Option<(usize, usize)> {
        let EditKind::New { parent, .. } = &self.edit.as_ref()?.kind else { return None };
        if parent == self.root() {
            return Some((0, 0));
        }
        let ix = self.tree.row_of(parent)?;
        Some((ix + 1, self.tree.rows()[ix].depth + 1))
    }

    fn display_len(&self) -> usize {
        self.tree.rows().len() + self.new_row().is_some() as usize
    }

    /// The tree row at list index `ix`; `None` for the new entry field.
    fn real_ix(&self, ix: usize) -> Option<usize> {
        match self.new_row() {
            Some((at, _)) if ix == at => None,
            Some((at, _)) if ix > at => Some(ix - 1),
            _ => Some(ix),
        }
    }

    fn display_ix(&self, real: usize) -> usize {
        match self.new_row() {
            Some((at, _)) if real >= at => real + 1,
            _ => real,
        }
    }

    fn scroll_to(&self, path: &Path) {
        if let Some(ix) = self.tree.row_of(path) {
            self.scroll.scroll_to_item(self.display_ix(ix), ScrollStrategy::Nearest);
        }
    }

    // ----- mouse -----

    fn click(&mut self, ix: usize, ev: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.tree.rows().get(ix) else { return };
        let (path, is_dir) = (row.entry.path.to_path_buf(), row.entry.is_dir);
        window.focus(&self.focus, cx);
        let m = ev.modifiers();
        if m.platform {
            match self.selected.iter().position(|p| *p == path) {
                Some(i) => {
                    self.selected.remove(i);
                },
                None => self.selected.push(path.clone()),
            }
            self.cursor = Some(path);
            return cx.notify();
        }
        if m.shift
            && let Some(anchor) = self.cursor.as_deref().and_then(|c| self.tree.row_of(c))
        {
            let range = if anchor <= ix { anchor..=ix } else { ix..=anchor };
            self.selected = self.tree.rows()[range].iter().map(|r| r.entry.path.to_path_buf()).collect();
            return cx.notify();
        }
        self.select_only(path.clone());
        if is_dir {
            // A double click's second click would fold it back.
            if ev.click_count() == 1 {
                self.tree.toggle(&path);
            }
        } else {
            cx.emit(FileTreeEvent::OpenFile { path, pin: ev.click_count() > 1 });
        }
        cx.notify();
    }

    /// A right click picks the entry the menu acts on, keeping a selection
    /// it belongs to (VS Code does the same).
    fn right_click(&mut self, ix: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        match ix.and_then(|ix| self.tree.rows().get(ix)) {
            Some(row) => {
                let path = row.entry.path.to_path_buf();
                if !self.selected.contains(&path) {
                    self.selected = vec![path.clone()];
                }
                self.cursor = Some(path);
            },
            None => {
                self.selected.clear();
                self.cursor = None;
            },
        }
        cx.notify();
    }

    /// Entries dropped on `dir`: moved there, or copied with ⌥ held.
    fn drop_on(&mut self, dir: PathBuf, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        let copy = window.modifiers().alt;
        let paths: Vec<PathBuf> = ops::outermost(paths).into_iter().filter(|p| copy || p.parent() != Some(dir.as_path())).collect();
        if !paths.is_empty() {
            self.transfer(&paths, &dir, !copy, window, cx);
        }
    }

    // ----- name field -----

    fn new_entry(&mut self, dir: bool, window: &mut Window, cx: &mut Context<Self>) {
        let parent = self.target_dir();
        self.tree.expand(&parent);
        self.start_edit(EditKind::New { parent, dir }, "", window, cx);
        if let Some((ix, _)) = self.new_row() {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        }
    }

    fn start_edit(&mut self, kind: EditKind, value: &str, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(value.to_owned()));
        // Select the name without its extension, like VS Code.
        let stem = match value.rfind('.') {
            Some(i) if i > 0 && matches!(&kind, EditKind::Rename(p) if !p.is_dir()) => i,
            _ => value.len(),
        };
        input.update(cx, |s, cx| s.set_selected_range(0..stem, cx));
        let sub = cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { .. } => this.commit_edit(false, window, cx),
            // Clicking away keeps a good name and drops a bad one.
            InputEvent::Blur => this.commit_edit(true, window, cx),
            _ => {},
        });
        input.focus_handle(cx).focus(window, cx);
        self.edit = Some(Edit { kind, input, _sub: sub });
        cx.notify();
    }

    fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit.take().is_some() {
            window.focus(&self.focus, cx);
            cx.notify();
        }
    }

    /// Create or rename from the typed name. On a bad name the field stays
    /// open to fix it, unless `drop_bad` (focus left it).
    fn commit_edit(&mut self, drop_bad: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = &self.edit else { return };
        let name = edit.input.read(cx).value().to_string();
        let kind = edit.kind.clone();
        let result = match &kind {
            EditKind::Rename(from) if from.file_name().is_some_and(|n| n.to_string_lossy() == name) => Ok(None),
            EditKind::Rename(from) => {
                let parent = from.parent().unwrap_or(self.root()).to_path_buf();
                ops::child_path(&parent, &name).and_then(|to| ops::rename(from, &to).map(|()| Some(to)))
            },
            EditKind::New { .. } if name.is_empty() => Ok(None),
            EditKind::New { parent, dir } => ops::child_path(parent, &name)
                .and_then(|to| if *dir { ops::create_dir(&to) } else { ops::create_file(&to) }.map(|()| Some(to))),
        };
        match result {
            Err(e) if !drop_bad => window.push_notification(e.to_string(), cx),
            Err(e) => {
                self.edit = None;
                window.push_notification(e.to_string(), cx);
            },
            Ok(done) => {
                self.edit = None;
                if !drop_bad {
                    window.focus(&self.focus, cx);
                }
                if let Some(to) = done {
                    match kind {
                        EditKind::Rename(from) => self.moved(&from, &to, cx),
                        EditKind::New { dir, .. } => {
                            self.show_new(&to);
                            self.reveal(&to, cx);
                            if !dir {
                                cx.emit(FileTreeEvent::OpenFile { path: to, pin: true });
                            }
                        },
                    }
                }
            },
        }
        cx.notify();
    }

    /// Reload the folders `path` and any new folders above it (`a/b/c.txt`)
    /// appeared in.
    fn show_new(&mut self, path: &Path) {
        let root = self.root().to_path_buf();
        let fresh: Vec<PathBuf> = path.ancestors().take_while(|a| *a != root).map(Path::to_path_buf).collect();
        self.tree.apply_changes(fresh.iter().map(PathBuf::as_path));
    }

    /// `from` now lives at `to`: update the tree and selection, tell the tabs.
    fn moved(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) {
        self.tree.moved(from, to);
        self.show_new(to);
        let follow = |p: &PathBuf| p.strip_prefix(from).map(|r| to.join(r)).unwrap_or_else(|_| p.clone());
        self.selected = self.selected.iter().map(follow).collect();
        self.cursor = self.cursor.as_ref().map(follow);
        self.claude_touched = self.claude_touched.iter().map(follow).collect();
        self.tree.reveal(to);
        self.scroll_to(to);
        cx.emit(FileTreeEvent::Moved { from: from.to_path_buf(), to: to.to_path_buf() });
    }

    // ----- commands -----

    fn on_new_file(&mut self, _: &NewFile, window: &mut Window, cx: &mut Context<Self>) {
        self.new_entry(false, window, cx);
    }

    fn on_new_folder(&mut self, _: &NewFolder, window: &mut Window, cx: &mut Context<Self>) {
        self.new_entry(true, window, cx);
    }

    fn on_rename(&mut self, _: &Rename, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.cursor.clone().filter(|p| p != self.root()) else { return };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.scroll_to(&path);
        self.start_edit(EditKind::Rename(path), &name, window, cx);
    }

    fn on_cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit.is_some() {
            return self.cancel_edit(window, cx);
        }
        if self.clip.take().is_some() || self.selected.len() > 1 {
            self.selected.retain(|p| Some(p) == self.cursor.as_ref());
            return cx.notify();
        }
        cx.propagate();
    }

    fn on_delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        let paths = self.targets();
        if paths.is_empty() {
            return;
        }
        let question = match paths.as_slice() {
            [one] => format!("Are you sure you want to delete '{}'?", one.file_name().unwrap_or_default().to_string_lossy()),
            many => {
                let mut q = format!("Are you sure you want to delete the following {} files/folders?\n", many.len());
                for p in many.iter().take(10) {
                    q.push_str(&format!("\n{}", self.rel(p)));
                }
                if many.len() > 10 {
                    q.push_str(&format!("\n…and {} more", many.len() - 10));
                }
                q
            },
        };
        let me = cx.entity().downgrade();
        let width = style::el("dialog", cx).px("width");
        window.open_dialog(cx, move |dialog, _, cx| {
            let question = question.clone();
            let footer = template::render(
                "dialog.trash.footer",
                template::Scope::new("trash"),
                &mut |name, _, f| match (name, f) {
                    ("cancel", Framed::Button(b)) => b.on_click(|_, window, cx| window.close_dialog(cx)).into_any_element(),
                    ("ok", Framed::Button(b)) => {
                        let (me, paths) = (me.clone(), paths.clone());
                        b.on_click(move |_, window, cx| {
                            window.close_dialog(cx);
                            let _ = me.update(cx, |this, cx| this.trash(&paths, window, cx));
                        })
                        .into_any_element()
                    },
                    (_, f) => f.into_any_element(),
                },
                cx,
            );
            dialog
                .w(width)
                .title("Delete")
                .content(move |c, _, cx| {
                    let scope = template::Scope::new("trash").var("question", question.clone());
                    c.child(template::render_plain("dialog.trash", scope, cx))
                })
                .footer(footer)
        });
    }

    fn trash(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        // The row after the last deleted one takes the selection.
        let next = paths
            .iter()
            .filter_map(|p| self.tree.row_of(p))
            .max()
            .and_then(|ix| self.tree.rows()[ix + 1..].iter().find(|r| !paths.iter().any(|p| r.entry.path.starts_with(p))))
            .map(|r| r.entry.path.to_path_buf());
        let mut gone = vec![];
        for p in paths {
            match ops::trash(p) {
                Ok(()) => gone.push(p.clone()),
                Err(e) => window.push_notification(format!("Could not delete '{}': {e}", self.rel(p)), cx),
            }
        }
        self.tree.apply_changes(gone.iter().map(PathBuf::as_path));
        self.selected.retain(|s| !gone.iter().any(|g| s.starts_with(g)));
        if self.cursor.as_ref().is_some_and(|c| gone.iter().any(|g| c.starts_with(g))) {
            self.cursor = None;
            if let Some(n) = next {
                self.select_only(n);
            }
        }
        window.focus(&self.focus, cx);
        if !gone.is_empty() {
            cx.emit(FileTreeEvent::Deleted(gone));
        }
        cx.notify();
    }

    fn on_copy(&mut self, _: &CopyEntries, _: &mut Window, cx: &mut Context<Self>) {
        self.clip = Some(Clip { paths: self.targets(), cut: false });
        cx.notify();
    }

    fn on_cut(&mut self, _: &CutEntries, _: &mut Window, cx: &mut Context<Self>) {
        self.clip = Some(Clip { paths: self.targets(), cut: true });
        cx.notify();
    }

    fn on_paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let Some(clip) = &self.clip else { return };
        let (paths, cut) = (clip.paths.clone(), clip.cut);
        let dir = self.target_dir();
        if cut {
            self.clip = None;
        }
        self.transfer(&paths, &dir, cut, window, cx);
    }

    /// Move (or copy) `paths` into `dir` and select what landed there.
    fn transfer(&mut self, paths: &[PathBuf], dir: &Path, move_: bool, window: &mut Window, cx: &mut Context<Self>) {
        let mut landed = vec![];
        for p in paths {
            let result = if move_ { ops::move_into(p, dir) } else { ops::copy_into(p, dir) };
            match result {
                Ok(to) if move_ => {
                    self.moved(p, &to, cx);
                    landed.push(to);
                },
                Ok(to) => landed.push(to),
                Err(e) => window.push_notification(format!("Could not {} '{}': {e}", if move_ { "move" } else { "copy" }, self.rel(p)), cx),
            }
        }
        self.tree.expand(dir);
        for p in &landed {
            self.show_new(p);
        }
        if let Some(last) = landed.last() {
            self.tree.reveal(last);
            self.scroll_to(last);
            self.cursor = Some(last.clone());
            self.selected = landed;
        }
        cx.notify();
    }

    fn on_copy_path(&mut self, _: &CopyPath, _: &mut Window, cx: &mut Context<Self>) {
        let text = self.targets_or_root().iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn on_copy_relative_path(&mut self, _: &CopyRelativePath, _: &mut Window, cx: &mut Context<Self>) {
        let text = self.targets_or_root().iter().map(|p| self.rel(p)).collect::<Vec<_>>().join("\n");
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn on_reveal(&mut self, _: &RevealInFinder, _: &mut Window, cx: &mut Context<Self>) {
        for p in self.targets_or_root() {
            cx.reveal_path(&p);
        }
    }

    /// The one HTML file selected, which Live Server can open.
    fn html_target(&self) -> Option<PathBuf> {
        let [path] = self.targets().try_into().ok()?;
        let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
        (matches!(ext.as_str(), "html" | "htm") && path.is_file()).then_some(path)
    }

    /// Serve the explorer's folder and open the page in the browser; it
    /// reloads when a file under the folder changes.
    fn on_live_server(&mut self, _: &OpenLiveServer, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.html_target() else { return };
        match tern_web::live::url_for(self.root(), &path) {
            Ok(url) => cx.open_url(&url),
            Err(e) => window.push_notification(format!("Could not start Live Server for '{}': {e}", self.rel(&path)), cx),
        }
    }

    fn targets_or_root(&self) -> Vec<PathBuf> {
        let t = self.targets();
        if t.is_empty() { vec![self.root().to_path_buf()] } else { t }
    }

    fn on_collapse_all(&mut self, _: &CollapseAll, _: &mut Window, cx: &mut Context<Self>) {
        self.tree.collapse_all();
        cx.notify();
    }

    fn on_refresh(&mut self, _: &Refresh, _: &mut Window, cx: &mut Context<Self>) {
        self.tree.refresh_all();
        cx.notify();
    }

    // ----- keyboard navigation -----

    fn cursor_ix(&self) -> Option<usize> {
        self.cursor.as_deref().and_then(|c| self.tree.row_of(c))
    }

    fn select_ix(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(row) = self.tree.rows().get(ix) {
            self.select_only(row.entry.path.to_path_buf());
            self.scroll.scroll_to_item(self.display_ix(ix), ScrollStrategy::Nearest);
            cx.notify();
        }
    }

    fn on_select_prev(&mut self, _: &SelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        let ix = self.cursor_ix().map_or(0, |i| i.saturating_sub(1));
        self.select_ix(ix, cx);
    }

    fn on_select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        let last = self.tree.rows().len().saturating_sub(1);
        let ix = self.cursor_ix().map_or(0, |i| (i + 1).min(last));
        self.select_ix(ix, cx);
    }

    fn on_collapse(&mut self, _: &CollapseEntry, _: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.cursor_ix() else { return };
        let row = &self.tree.rows()[ix];
        if row.expanded {
            let path = row.entry.path.to_path_buf();
            self.tree.collapse(&path);
            return cx.notify();
        }
        let depth = row.depth;
        if let Some(parent) = (0..ix).rev().find(|&i| self.tree.rows()[i].depth < depth) {
            self.select_ix(parent, cx);
        }
    }

    fn on_expand(&mut self, _: &ExpandEntry, _: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.cursor_ix() else { return };
        let row = &self.tree.rows()[ix];
        if !row.entry.is_dir {
            return;
        }
        if row.expanded {
            return self.select_ix(ix + 1, cx);
        }
        let path = row.entry.path.to_path_buf();
        self.tree.expand(&path);
        cx.notify();
    }

    fn open_cursor(&mut self, pin: bool, cx: &mut Context<Self>) {
        let Some(path) = self.cursor.clone() else { return };
        if path.is_dir() {
            self.tree.toggle(&path);
            cx.notify();
        } else {
            cx.emit(FileTreeEvent::OpenFile { path, pin });
        }
    }

    fn on_open(&mut self, _: &OpenEntry, _: &mut Window, cx: &mut Context<Self>) {
        self.open_cursor(false, cx);
    }

    fn on_open_pinned(&mut self, _: &OpenEntryPinned, _: &mut Window, cx: &mut Context<Self>) {
        self.open_cursor(true, cx);
    }

    /// Rows, selection, cursor and name field, for the control socket.
    #[cfg(debug_assertions)]
    pub fn debug_state(&self, cx: &App) -> String {
        let rows: Vec<String> = self.tree.rows().iter().map(|r| format!("{}{}", "  ".repeat(r.depth), r.entry.name)).collect();
        let rel = |p: &PathBuf| self.rel(p);
        let edit = self.edit.as_ref().map(|e| format!("{:?}={:?}", e.kind, e.input.read(cx).value()));
        format!(
            "rows={rows:?} selected={:?} cursor={:?} edit={edit:?} clip={:?}",
            self.selected.iter().map(rel).collect::<Vec<_>>(),
            self.cursor.as_ref().map(rel),
            self.clip.as_ref().map(|c| (c.cut, c.paths.iter().map(rel).collect::<Vec<_>>())),
        )
    }

    // ----- render -----

    fn context_menu(menu: PopupMenu, me: &WeakEntity<Self>, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
        let Some(this) = me.upgrade() else { return menu };
        let this = this.read(cx);
        let scope = template::Scope::new("explorer-menu")
            .var("on_entry", !this.targets().is_empty())
            .var("can_paste", this.clip.as_ref().is_some_and(|c| !c.paths.is_empty()))
            .var("reveal", crate::platform::REVEAL)
            .var("is_html", this.html_target().is_some());
        let menu = menu.action_context(this.focus.clone());
        template::menu("menu.explorer", scope, menu, Rc::new(|_, _| None), window, cx)
    }

    /// The row likely to be widest, which the list measures for its
    /// scrollable width.
    fn widest_row(&self) -> usize {
        let width = |r: &tern_fs::tree::Row| r.depth as f32 * 14. + r.entry.name.chars().count() as f32 * 7.;
        let rows = self.tree.rows();
        let widest = (0..rows.len()).max_by(|&a, &b| width(&rows[a]).total_cmp(&width(&rows[b]))).unwrap_or(0);
        self.display_ix(widest)
    }

    fn render_rows(&mut self, range: Range<usize>, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let focused = self.focus.contains_focused(window, cx);
        range
            .filter_map(|ix| match self.real_ix(ix) {
                Some(real) => self.render_row(ix, real, focused, cx),
                None => self.render_new_row(ix, cx),
            })
            .collect()
    }

    /// The name field, filling the rest of a row.
    fn name_field(input: &Entity<InputState>) -> impl IntoElement {
        div()
            .flex_grow(1.)
            .min_w(px(120.))
            // A click in the field must not reach the row and steal focus.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(Input::new(input).xsmall())
    }

    fn render_new_row(&self, ix: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let edit = self.edit.as_ref()?;
        let EditKind::New { dir, .. } = edit.kind else { return None };
        let (_, depth) = self.new_row()?;
        let theme = cx.theme();
        let (icon, color) = if dir { file_icons::dir_icon("", false, theme) } else { file_icons::file_icon("", theme) };
        let scope = template::Scope::new(ROW)
            .var("indent", style::row_indent(ROW, depth, cx))
            .var("editing", true)
            .slot("icon", style::row_icon(ROW, icon, color, cx))
            .slot("field", Self::name_field(&edit.input));
        Some(template::render(
            ROW,
            scope,
            &mut |name, _, f| match (name, f) {
                ("row", Framed::Box(d)) => d.id(ix).into_any_element(),
                (_, f) => f.into_any_element(),
            },
            cx,
        ))
    }

    fn render_row(&self, ix: usize, real: usize, focused: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme();
        let drop_bg = theme.drop_target;
        let row = self.tree.rows().get(real)?;
        let path: &Path = &row.entry.path;
        let selected = self.selected.iter().any(|p| p == path);
        let git = self.git.get(path).copied();
        let name = &*row.entry.name;
        let is_dir = row.entry.is_dir;
        let (icon, icon_color) = if is_dir { file_icons::dir_icon(name, row.expanded, theme) } else { file_icons::file_icon(name, theme) };
        let renaming = self.edit.as_ref().filter(|e| e.kind == EditKind::Rename(path.to_path_buf()));

        let drag_paths = if selected && self.selected.len() > 1 { self.selected.clone() } else { vec![path.to_path_buf()] };
        let label: SharedString = match drag_paths.len() {
            1 => name.to_string().into(),
            n => format!("{n} items").into(),
        };
        let drop_dir = if is_dir { path.to_path_buf() } else { path.parent().unwrap_or(self.root()).to_path_buf() };

        let mut scope = template::Scope::new(ROW)
            .var("indent", style::row_indent(ROW, row.depth, cx))
            .var("folder", is_dir)
            .var("open", row.expanded)
            .var("selected", selected)
            .var("cursor", self.cursor.as_deref() == Some(path) && focused)
            .var("cut", self.clip.as_ref().is_some_and(|c| c.cut && c.paths.iter().any(|p| path.starts_with(p))))
            .var("editing", renaming.is_some())
            .var("name", name)
            .var("claude", self.claude_touched.contains(path))
            .var("git", git.map(|g| g.letter().to_string()).unwrap_or_default())
            .slot("icon", style::row_icon(ROW, icon, icon_color, cx))
            .slot_opt("field", renaming.map(|e| Self::name_field(&e.input)));
        if let Some(g) = git {
            scope = scope.var("name_color", style::git_color(g, cx));
        }
        let mut drag = renaming.is_none().then_some(DraggedEntries { paths: drag_paths, label });
        Some(template::render(
            ROW,
            scope,
            &mut |name, _, f| match (name, f) {
                ("row", Framed::Box(d)) => d
                    .id(ix)
                    .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| this.click(real, ev, window, cx)))
                    .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, window, cx| this.right_click(Some(real), window, cx)))
                    .when_some(drag.take(), |d, drag| d.on_drag(drag, |d, _, _, cx| cx.new(|_| d.clone())))
                    .drag_over::<DraggedEntries>(move |s, _, _, _| s.bg(drop_bg))
                    .on_drop(cx.listener({
                        let drop_dir = drop_dir.clone();
                        move |this, d: &DraggedEntries, window, cx| this.drop_on(drop_dir.clone(), &d.paths, window, cx)
                    }))
                    .into_any_element(),
                (_, f) => f.into_any_element(),
            },
            cx,
        ))
    }
}

impl Render for FileTreeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root_name = self.tree.root().file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let me = cx.entity().downgrade();
        let root = self.root().to_path_buf();
        let header = crate::dock::panel_header(
            "explorer",
            "explorer.header",
            root_name,
            None,
            template::Scope::new("explorer.header.buttons").var("hide_key", crate::platform::shortcut("⌘B")),
            &mut |name, _, f| {
                let Framed::Button(b) = f else { return f.into_any_element() };
                let b = match name {
                    "new_file" => b.on_click(on_event(&me, |this, _: &ClickEvent, window, cx| {
                        window.focus(&this.focus, cx);
                        this.new_entry(false, window, cx);
                    })),
                    "new_folder" => b.on_click(on_event(&me, |this, _: &ClickEvent, window, cx| {
                        window.focus(&this.focus, cx);
                        this.new_entry(true, window, cx);
                    })),
                    "refresh" => b.on_click(on_event(&me, |this, _: &ClickEvent, window, cx| this.on_refresh(&Refresh, window, cx))),
                    "collapse_all" => {
                        b.on_click(on_event(&me, |this, _: &ClickEvent, window, cx| this.on_collapse_all(&CollapseAll, window, cx)))
                    },
                    "hide" => b.on_click(on_event(&me, |_, _: &ClickEvent, _, cx| cx.emit(FileTreeEvent::Hide))),
                    _ => b,
                };
                b.into_any_element()
            },
            cx,
        );
        let list = div()
            .id("file-tree-body")
            .relative()
            .flex_1()
            .min_h_0()
            // A right click on empty space acts on the root; a row's
            // own handler (which runs after this one) picks the row.
            .capture_any_mouse_down(cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                if ev.button == MouseButton::Right {
                    this.right_click(None, window, cx);
                }
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| window.focus(&this.focus, cx)))
            .on_drop(cx.listener(move |this, d: &DraggedEntries, window, cx| this.drop_on(root.clone(), &d.paths, window, cx)))
            .child(
                uniform_list("file-tree", self.display_len(), cx.processor(Self::render_rows))
                    .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                    .with_width_from_item(Some(self.widest_row()))
                    .track_scroll(&self.scroll)
                    .size_full(),
            )
            // Both bars, so names wider than the panel can be scrolled to.
            .child(Scrollbar::new(&self.scroll).id("file-tree-scrollbar"))
            .map(|d| style::el("explorer.list", cx).apply(d))
            .context_menu(move |menu, window, cx| Self::context_menu(menu, &me, window, cx));
        let scope = template::Scope::new("explorer").slot("header", header).slot("list", list);
        let focus = &self.focus;
        template::render(
            "explorer",
            scope,
            &mut |name, _, f| match (name, f) {
                ("panel", Framed::Box(d)) => d
                    .key_context(CONTEXT)
                    .track_focus(focus)
                    .on_action(cx.listener(Self::on_new_file))
                    .on_action(cx.listener(Self::on_new_folder))
                    .on_action(cx.listener(Self::on_rename))
                    .on_action(cx.listener(Self::on_delete))
                    .on_action(cx.listener(Self::on_copy))
                    .on_action(cx.listener(Self::on_cut))
                    .on_action(cx.listener(Self::on_paste))
                    .on_action(cx.listener(Self::on_copy_path))
                    .on_action(cx.listener(Self::on_copy_relative_path))
                    .on_action(cx.listener(Self::on_reveal))
                    .on_action(cx.listener(Self::on_collapse_all))
                    .on_action(cx.listener(Self::on_refresh))
                    .on_action(cx.listener(Self::on_select_prev))
                    .on_action(cx.listener(Self::on_select_next))
                    .on_action(cx.listener(Self::on_collapse))
                    .on_action(cx.listener(Self::on_expand))
                    .on_action(cx.listener(Self::on_live_server))
                    .on_action(cx.listener(Self::on_open))
                    .on_action(cx.listener(Self::on_open_pinned))
                    .on_action(cx.listener(Self::on_cancel))
                    .into_any_element(),
                (_, f) => f.into_any_element(),
            },
            cx,
        )
    }
}
