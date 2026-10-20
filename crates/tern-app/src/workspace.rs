//! One workspace = one project folder: explorer / scratches | tabs (terminals,
//! files, diffs, scratches) | changes. A window shows one of its workspaces at a time (see
//! `main_window`), and a workspace can move between windows.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName as AssetIcon;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _};
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, WindowExt, h_resizable, resizable_panel, v_resizable};
use gpui_kit::*;
use tern_bridge::HookEvent;
use tern_fs::{ChangeBatch, GitStatus};
use tern_mux::client::Connector;
use tern_term::{SpawnConfig, TermSize};

use crate::agents::Agent;
use crate::arrange::{self, Arrangement, Kept};
use crate::changes_view::{ChangesEvent, ChangesView};
use crate::diff_pane::DiffPane;
use crate::dock::{ActiveDrag, Change, DraggedPart, DropHint, OnChange};
use crate::file_icons;
use crate::file_tree_view::{FileTreeEvent, FileTreeView};
use crate::palette::{self, Palette, PaletteEvent};
use crate::panes::{self, Edge, Layout, PaneId, Zone};
use crate::platform::bind;
use crate::scratch::{self, Scratch};
use crate::scratches_view::{ScratchesEvent, ScratchesView};
use crate::settings;
use crate::shells_view::{ShellRow, ShellsEvent, ShellsView};
use crate::style;
use crate::template::{self, Framed, on_event};
use crate::terminal_view::{ActivityStatus, TerminalConfig, TerminalEvent, TerminalView};
use crate::theme_picker::OpenSettingsFile;
use crate::usage_view::{UsageEvent, UsageView};
use crate::viewer::{self, Viewer, ViewerEvent};
use crate::window_state::{self, SavedGroup, SavedPanels, SavedTab, SavedWorkspace, TabLayout};

/// What a name dialog does with the typed name (see `Workspace::ask_name`).
type ApplyName = Rc<dyn Fn(&mut Workspace, &str, &mut Window, &mut Context<Workspace>)>;

actions!(
    workspace,
    [
        NewClaudeTab,
        NewShellTab,
        NewScratch,
        OpenScratchesFolder,
        CloseTab,
        NextTab,
        PrevTab,
        ToggleExplorer,
        ToggleChanges,
        ToggleScratches,
        CommandPalette,
        GoToFile,
        SearchInFiles,
        ToggleShells,
        ToggleUsage,
        ResetLayout,
        FocusTerminal,
        SplitRight,
        SplitDown,
        ToggleZoomPane,
        TileTabs,
        FocusPaneLeft,
        FocusPaneRight,
        FocusPaneUp,
        FocusPaneDown,
        Tab1,
        Tab2,
        Tab3,
        Tab4,
        Tab5,
        Tab6,
        Tab7,
        Tab8,
        Tab9
    ]
);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        bind("cmd-t", NewClaudeTab, None),
        bind("cmd-shift-t", NewShellTab, None),
        bind("cmd-n", NewScratch, None),
        bind("cmd-w", CloseTab, None),
        bind("cmd-shift-]", NextTab, None),
        bind("cmd-shift-[", PrevTab, None),
        bind("cmd-b", ToggleExplorer, None),
        bind("cmd-shift-g", ToggleChanges, None),
        bind("cmd-shift-s", ToggleScratches, None),
        bind("cmd-shift-p", CommandPalette, None),
        bind("cmd-p", GoToFile, None),
        bind("cmd-shift-f", SearchInFiles, None),
        bind("cmd-j", FocusTerminal, None),
        bind("cmd-\\", SplitRight, None),
        bind("cmd-shift-\\", SplitDown, None),
        bind("cmd-shift-enter", ToggleZoomPane, None),
        bind("ctrl-cmd-left", FocusPaneLeft, None),
        bind("ctrl-cmd-right", FocusPaneRight, None),
        bind("ctrl-cmd-up", FocusPaneUp, None),
        bind("ctrl-cmd-down", FocusPaneDown, None),
        bind("cmd-1", Tab1, None),
        bind("cmd-2", Tab2, None),
        bind("cmd-3", Tab3, None),
        bind("cmd-4", Tab4, None),
        bind("cmd-5", Tab5, None),
        bind("cmd-6", Tab6, None),
        bind("cmd-7", Tab7, None),
        bind("cmd-8", Tab8, None),
        bind("cmd-9", Tab9, None),
    ]);
    if cfg!(target_os = "macos") {
        // Shift-\ is | on US layouts; elsewhere Ctrl+Shift+\ already splits right.
        cx.bind_keys([bind("cmd-|", SplitDown, None)]);
    } else {
        cx.bind_keys([bind("ctrl-tab", NextTab, None), bind("ctrl-shift-tab", PrevTab, None)]);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TabKind {
    Claude,
    Shell,
}

struct TermTab {
    view: Entity<TerminalView>,
    /// Title set with Rename…, shown instead of the terminal's own.
    rename: Option<SharedString>,
    kind: TabKind,
    /// Hook token for Claude tabs.
    token: Option<String>,
    /// The Claude conversation running here (from hooks), to resume it.
    session: Option<String>,
    /// Its program was started with `--resume`: if that fails at once
    /// (Claude saved no conversation), start a new one.
    resuming: bool,
    /// When the tab last showed a notification, to drop a second one for
    /// the same event (OSC 9 and the status change of one turn).
    notified: Option<Instant>,
    /// Rang the bell while not shown; cleared when shown.
    rang: bool,
    /// The output file of an agent's shell it follows (see `open_shell`).
    follows: Option<PathBuf>,
    _subs: Vec<Subscription>,
}

/// A tab in the center tab bar: a terminal, a file opened from the
/// explorer, a file diff opened from the Changes preview, or a scratch file.
enum CenterTab {
    Term(TermTab),
    File { view: Entity<Viewer>, _subs: Vec<Subscription> },
    Diff(Entity<DiffPane>),
    Scratch { view: Entity<Scratch>, _sub: Subscription },
}

impl CenterTab {
    fn term(&self) -> Option<&TermTab> {
        match self {
            CenterTab::Term(t) => Some(t),
            CenterTab::File { .. } | CenterTab::Diff(_) | CenterTab::Scratch { .. } => None,
        }
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            CenterTab::Term(t) => t.view.focus_handle(cx),
            CenterTab::File { view, .. } => view.focus_handle(cx),
            CenterTab::Diff(d) => d.focus_handle(cx),
            CenterTab::Scratch { view: s, .. } => s.focus_handle(cx),
        }
    }

    fn entity_id(&self) -> EntityId {
        match self {
            CenterTab::Term(t) => t.view.entity_id(),
            CenterTab::File { view, .. } => view.entity_id(),
            CenterTab::Diff(d) => d.entity_id(),
            CenterTab::Scratch { view, .. } => view.entity_id(),
        }
    }

    /// The file a file, diff or scratch tab shows.
    fn file_path(&self, cx: &App) -> Option<PathBuf> {
        match self {
            CenterTab::Term(_) => None,
            CenterTab::File { view, .. } => view.read(cx).path().map(Path::to_owned),
            CenterTab::Diff(d) => Some(d.read(cx).path().to_owned()),
            CenterTab::Scratch { view, .. } => Some(view.read(cx).path().to_owned()),
        }
    }
}

/// The payload of a center tab drag.
#[derive(Clone)]
struct DraggedTab {
    tab: EntityId,
    name: SharedString,
}

impl Render for DraggedTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        style::drag_chip(self.name.clone(), None, cx)
    }
}

/// The payload of a tab group drag (its chip).
#[derive(Clone)]
struct DraggedGroup {
    group: u64,
    name: SharedString,
    color: Hsla,
}

impl Render for DraggedGroup {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let scope = template::Scope::new("drag_group").var("text", self.name.clone()).var("group_color", self.color);
        template::render_plain("drag_group", scope, cx)
    }
}

/// Tab group colors, as Chrome offers them: saved name, menu label, color.
const GROUP_COLORS: [(&str, &str, u32); 9] = [
    ("grey", "Grey", 0x8b8f94),
    ("blue", "Blue", 0x4c8bf5),
    ("red", "Red", 0xe5534b),
    ("yellow", "Yellow", 0xe8a317),
    ("green", "Green", 0x34a853),
    ("pink", "Pink", 0xe8459a),
    ("purple", "Purple", 0xa25ce8),
    ("cyan", "Cyan", 0x14a3b8),
    ("orange", "Orange", 0xf28b30),
];

fn group_color(ix: usize) -> Hsla {
    rgb(GROUP_COLORS[ix % GROUP_COLORS.len()].2).into()
}

/// A named, colored run of center tabs that can collapse to its chip, like
/// Chrome's tab groups. Its tabs always sit next to each other.
struct TabGroup {
    id: u64,
    name: SharedString,
    /// Index into `GROUP_COLORS`.
    color: usize,
    collapsed: bool,
}

impl TabGroup {
    /// The name, or its color's when it has none.
    fn title(&self) -> String {
        if self.name.is_empty() { format!("{} Group", GROUP_COLORS[self.color].1) } else { self.name.to_string() }
    }
}

/// What the tab bar shows, in order: each group's chip, then its tabs unless
/// the group is collapsed.
#[derive(Clone, Copy, PartialEq)]
enum Slot {
    Group(u64),
    Tab(usize),
}

/// An entry of a tab list as its frames need it.
struct TabEntry {
    slot: Slot,
    /// What a drag of it shows.
    name: SharedString,
    /// Its line in the overflow menu.
    label: SharedString,
    /// Its group's color.
    color: Hsla,
    /// The tab, for a drag.
    tab: Option<EntityId>,
}

/// A pane's tab strip scroll state.
struct PaneUi {
    /// The tab strip's scroll, when tabs don't wrap.
    tab_scroll: ScrollHandle,
    /// The vertical tab list's scroll.
    vtab_scroll: ScrollHandle,
    /// The slot last scrolled into view, to reveal a newly active tab once.
    revealed: Option<usize>,
}

impl Default for PaneUi {
    fn default() -> Self {
        Self { tab_scroll: ScrollHandle::new(), vtab_scroll: ScrollHandle::new(), revealed: None }
    }
}

/// Shared app services (one per process).
#[derive(Clone)]
pub struct Services {
    /// The background daemon the terminals run in.
    pub mux: Arc<Connector>,
    pub hook_events: flume::Receiver<HookEvent>,
    /// Sessions the menu bar agent asked to show.
    pub reveal_events: flume::Receiver<u64>,
    pub terminal: TerminalConfig,
}

pub struct Workspace {
    root: PathBuf,
    services: Services,
    tree: Entity<FileTreeView>,
    changes: Entity<ChangesView>,
    scratches: Entity<ScratchesView>,
    shells: Entity<ShellsView>,
    usage: Entity<UsageView>,
    tabs: Vec<CenterTab>,
    active: usize,
    /// The tab last right-clicked, for the tab context menu.
    menu_tab: Option<usize>,
    /// The group whose chip was last right-clicked, for the group menu.
    menu_group: Option<u64>,
    groups: Vec<TabGroup>,
    /// The group each grouped tab is in, by the tab's entity.
    tab_group: HashMap<EntityId, u64>,
    /// The file tab in preview mode, like VS Code's: a single click in the
    /// explorer shows its file here instead of opening another tab. It is
    /// kept (pinned) once double-clicked or edited.
    preview_tab: Option<EntityId>,
    next_group: u64,
    _rename: Option<Subscription>,
    /// The diff shown in the Changes preview popup, while it is open.
    preview: Option<WeakEntity<DiffPane>>,
    show_explorer: bool,
    show_changes: bool,
    show_scratches: bool,
    /// The command palette and Search in Files, made when first opened and
    /// kept, so a search is still there when it opens again.
    palette: Option<(Entity<Palette>, Subscription)>,
    show_shells: bool,
    show_usage: bool,
    /// How the center is split into panes, like VS Code's editor groups.
    layout: Layout,
    /// The pane each tab is in.
    tab_pane: HashMap<EntityId, PaneId>,
    /// The tab each pane shows.
    pane_active: HashMap<PaneId, EntityId>,
    /// The pane keys and new tabs go to; `active` is its tab when it has one.
    focused: PaneId,
    next_pane: PaneId,
    /// The pane filling the center by itself, while one is maximized.
    zoomed: Option<PaneId>,
    /// Bumped to give the splits even sizes again.
    layout_epoch: usize,
    /// A tab drag started here is on: the panes show their drop zones.
    tab_drag: bool,
    pane_ui: RefCell<HashMap<PaneId, PaneUi>>,
    /// Where the side panels sit, per template and docks (`arrange::key`).
    arrangements: Kept,
    /// The `Panels` revision the side panels follow.
    panels_rev: u64,
    /// Bumped when the parts move, the layout resets or the arrangement is
    /// fitted again: the docks' splits start over from the arrangement.
    docks_epoch: usize,
    /// The templates generation and key the arrangement was last fitted to.
    arranged_for: Option<(u64, String)>,
    /// The arrangement for `arranged_for`, shared by every frame until it
    /// changes.
    arranged: Option<Arc<Arrangement>>,
    /// What fitting the stored arrangement to the templates found.
    arrange_problems: Vec<String>,
    /// Where the floating parts sit, as last drawn.
    float_area: Option<Bounds<Pixels>>,
    /// The agents panel of the window showing this workspace (see `main_window`).
    agents: Option<AnyView>,
    /// The windows of the parts in windows of their own, by part (None
    /// while one opens).
    part_windows: HashMap<String, Option<AnyWindowHandle>>,
    _subs: Vec<Subscription>,
}

/// A terminal tab, as the screenshot tray and the agent monitor list it.
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))] // for the screenshot tray
pub struct TermTabInfo {
    pub id: EntityId,
    pub title: String,
    pub agent: Option<Agent>,
    /// The workspace's active tab.
    pub active: bool,
    pub view: Entity<TerminalView>,
}

pub enum WorkspaceEvent {
    /// Tabs opened, closed, switched or renamed, or a side panel shown or
    /// hidden: the session should be saved.
    TabsChanged,
}

impl EventEmitter<WorkspaceEvent> for Workspace {}

/// The side panels, the same in every workspace: those hidden and where they
/// sit. A workspace whose panels change publishes them here; the others follow.
#[derive(Default)]
pub struct Panels {
    pub saved: SavedPanels,
    /// Bumped on every change.
    rev: u64,
}

impl Global for Panels {}

impl Panels {
    pub fn new(saved: SavedPanels) -> Self {
        Self { saved, rev: 0 }
    }
}

/// Common `claude` flags, toggled from the new-tab menu.
const CLAUDE_FLAGS: [(&str, &str); 2] = [("Skip Permissions", "--dangerously-skip-permissions"), ("Chrome", "--chrome")];

/// The tab layout choices (scroll, wrap, vertical), for every window: the
/// `layouts` list of `menu.tab_layout`.
fn tab_layout_entries() -> Vec<template::Scope> {
    let now = window_state::tab_layout();
    TabLayout::ALL.iter().map(|(layout, label)| template::Scope::default().var("label", *label).var("on", now == *layout)).collect()
}

/// A menu item that runs `f` on the workspace.
fn on_ws(
    ws: &WeakEntity<Workspace>,
    f: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) -> Option<template::MenuHandler> {
    let ws = ws.clone();
    Some(Rc::new(move |_, window, cx| {
        let _ = ws.update(cx, |this, cx| f(this, window, cx));
    }))
}

/// `on_ws`, then bring `ws` forward in its window.
fn on_ws_shown(
    ws: &WeakEntity<Workspace>,
    f: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) -> Option<template::MenuHandler> {
    let ws = ws.clone();
    Some(Rc::new(move |_, window, cx| {
        let Some(this) = ws.upgrade() else { return };
        this.update(cx, |this, cx| f(this, window, cx));
        // Deferred: the window is still being updated.
        let handle = window.window_handle();
        cx.defer(move |cx| crate::main_window::reveal(handle, &this, cx));
    }))
}

/// `s` as one word of the shell Claude tabs run in (see `platform::shell`).
fn shell_quote(s: &str) -> String {
    if cfg!(windows) {
        // PowerShell: inside single quotes only `'` is special, and it takes
        // the curly quotes U+2018–U+201B for one too. Double them all.
        let mut out = String::from("'");
        for c in s.chars() {
            if matches!(c, '\'' | '\u{2018}'..='\u{201B}') {
                out.push(c);
            }
            out.push(c);
        }
        out.push('\'');
        out
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// Claude Code's own folder (`CLAUDE_CONFIG_DIR`, else `~/.claude`).
fn claude_dir() -> Option<PathBuf> {
    match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        Some(dir) => Some(PathBuf::from(dir)),
        None => crate::dirs::home().map(|h| h.join(".claude")),
    }
}

/// The background job (`/bg`) running conversation `session`, if one still
/// does: Claude won't `--resume` it then, only `claude attach` it. Read from
/// the session files Claude keeps in `sessions/<pid>.json`.
fn claude_bg_job(sessions: &Path, session: &str) -> Option<String> {
    std::fs::read_dir(sessions).ok()?.flatten().find_map(|entry| {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            return None;
        }
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
        if v["sessionId"].as_str() != Some(session) || v["kind"].as_str() != Some("bg") {
            return None;
        }
        // A file left behind by a Claude that's gone.
        #[cfg(unix)]
        {
            let pid = i32::try_from(v["pid"].as_i64()?).ok()?;
            // SAFETY: signal 0 only checks that the process exists.
            if unsafe { libc::kill(pid, 0) } != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EPERM) {
                return None;
            }
        }
        v["jobId"].as_str().filter(|j| !j.is_empty()).map(str::to_owned)
    })
}

impl Workspace {
    /// Open `saved.folder` with `saved`'s tabs (none when none of them can
    /// come back: the workspace stays open without tabs).
    pub fn new(saved: SavedWorkspace, services: Services, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let root = saved.folder;
        let (panels, panels_rev) = cx.try_global::<Panels>().map(|p| (p.saved.clone(), p.rev)).unwrap_or_default();
        let shown = |name: &str| !panels.hidden.iter().any(|h| h == name);
        let tree = cx.new(|cx| FileTreeView::new(root.clone(), cx));
        let changes = cx.new(|cx| ChangesView::new(root.clone(), cx));
        let scratches = cx.new(ScratchesView::new);
        let shells = cx.new(ShellsView::new);
        let usage = cx.new(UsageView::new);

        let tree_sub = cx.subscribe_in(&tree, window, |this, _, ev: &FileTreeEvent, window, cx| match ev {
            FileTreeEvent::OpenFile { path, pin } => {
                this.open_file(path.clone(), *pin, window, cx);
                // A preview keeps the keyboard in the explorer, as in VS Code.
                if !pin {
                    window.focus(&this.tree.focus_handle(cx), cx);
                }
            },
            FileTreeEvent::Moved { from, to } => this.file_moved(from, to, window, cx),
            FileTreeEvent::Deleted(paths) => this.files_deleted(paths, window, cx),
            FileTreeEvent::Changed(batch) => {
                this.changes.update(cx, |c, cx| c.files_changed(batch, cx));
                this.files_changed(batch, window, cx);
            },
            FileTreeEvent::Hide => {
                this.show_explorer = false;
                this.panels_changed(cx);
            },
        });
        let changes_sub = cx.subscribe_in(&changes, window, |this, _, ev: &ChangesEvent, window, cx| match ev {
            ChangesEvent::PreviewDiff(p, status) => {
                this.tree.update(cx, |t, cx| t.reveal(p, cx));
                this.preview_diff(p.clone(), *status, window, cx);
            },
            ChangesEvent::Updated(map) => {
                let map = map.clone();
                this.tree.update(cx, |t, cx| t.set_git(map, cx));
            },
            ChangesEvent::Hide => {
                this.show_changes = false;
                this.panels_changed(cx);
            },
        });
        let scratches_sub = cx.subscribe_in(&scratches, window, |this, _, ev: &ScratchesEvent, window, cx| match ev {
            ScratchesEvent::Open(p) => this.open_scratch(p.clone(), window, cx),
            ScratchesEvent::New(ext) => this.new_scratch(ext, window, cx),
            ScratchesEvent::Delete(p) => this.confirm_delete_scratch(p.clone(), window, cx),
            ScratchesEvent::Hide => {
                this.show_scratches = false;
                this.panels_changed(cx);
            },
        });
        let shells_sub = cx.subscribe_in(&shells, window, |this, _, ev: &ShellsEvent, window, cx| match ev {
            ShellsEvent::Open(row) => this.open_shell(row, window, cx),
            ShellsEvent::Hide => {
                this.show_shells = false;
                this.panels_changed(cx);
            },
        });
        let usage_sub = cx.subscribe(&usage, |this, _, ev: &UsageEvent, cx| match ev {
            UsageEvent::Hide => {
                this.show_usage = false;
                this.panels_changed(cx);
            },
        });

        let mut this = Self {
            root,
            services,
            tree,
            changes,
            scratches,
            shells,
            usage,
            tabs: vec![],
            active: 0,
            menu_tab: None,
            menu_group: None,
            groups: vec![],
            tab_group: HashMap::new(),
            preview_tab: None,
            next_group: 0,
            _rename: None,
            preview: None,
            show_explorer: shown("explorer"),
            show_changes: shown("changes"),
            show_scratches: shown("scratches"),
            palette: None,
            show_shells: shown("shells"),
            show_usage: shown("usage"),
            layout: Layout::Pane(0),
            tab_pane: HashMap::new(),
            pane_active: HashMap::new(),
            focused: 0,
            next_pane: 1,
            zoomed: None,
            layout_epoch: 0,
            tab_drag: false,
            pane_ui: RefCell::new(HashMap::new()),
            docks_epoch: 0,
            arrangements: Kept::new(panels.arrangements.iter().map(|(k, e)| (k.as_str(), e.as_str()))),
            panels_rev,
            arranged_for: None,
            arranged: None,
            arrange_problems: vec![],
            float_area: None,
            agents: None,
            part_windows: HashMap::new(),
            _subs: vec![tree_sub, changes_sub, scratches_sub, shells_sub, usage_sub, cx.observe_global::<Panels>(Self::follow_panels)],
        };
        let (panes, tab_panes, pane_active) = (saved.panes, saved.tab_panes, saved.pane_active);
        // Saved tab index → the tab it became (some don't come back).
        let mut made = vec![];
        let groups: Vec<u64> = saved
            .groups
            .iter()
            .map(|g| {
                let color = GROUP_COLORS.iter().position(|c| c.0 == g.color).unwrap_or(0);
                this.add_group(g.name.clone().into(), color, g.collapsed)
            })
            .collect();
        for (i, tab) in saved.tabs.into_iter().enumerate() {
            let before = this.tabs.len();
            match tab {
                SavedTab::Claude { name, session, mux } => this.add_term(TabKind::Claude, name, session, mux, window, cx),
                SavedTab::Shell { name, mux } => this.add_term(TabKind::Shell, name, None, mux, window, cx),
                SavedTab::File(p) if p.is_file() => this.open_file(p, true, window, cx),
                SavedTab::Diff(p) => this.open_diff_tab(cx.new(|cx| DiffPane::new(p, cx)), window, cx),
                SavedTab::Scratch(p) if p.is_file() => this.open_scratch(p, window, cx),
                SavedTab::File(_) | SavedTab::Scratch(_) => {},
            }
            if this.tabs.len() > before {
                made.push((i, this.tabs[before].entity_id()));
            }
            if this.tabs.len() > before
                && let Some(&g) = saved.tab_groups.get(i).copied().flatten().and_then(|g| groups.get(g))
            {
                this.tab_group.insert(this.tabs[before].entity_id(), g);
            }
        }
        this.regroup();
        this.restore_panes(&panes, &tab_panes, &pane_active, &made);
        if !this.tabs.is_empty() {
            this.activate(saved.active.min(this.tabs.len() - 1), window, cx);
        }
        this
    }

    /// Split the center as saved: `layout` (`Layout::encode`), each saved
    /// tab's pane and each pane's tab; `made` maps saved tabs to new ones.
    fn restore_panes(&mut self, layout: &str, tab_panes: &[usize], pane_active: &[Option<usize>], made: &[(usize, EntityId)]) {
        let Some(layout) = Layout::decode(layout) else { return };
        let ids: Vec<PaneId> = layout.panes().iter().map(|_| self.new_pane()).collect();
        self.layout = layout.renumber(&ids);
        self.focused = ids[0];
        let tab = |i: usize| made.iter().find(|(s, _)| *s == i).map(|(_, id)| *id);
        for &(i, id) in made {
            self.tab_pane.insert(id, tab_panes.get(i).and_then(|k| ids.get(*k)).copied().unwrap_or(ids[0]));
        }
        for (k, a) in pane_active.iter().enumerate() {
            if let (Some(&p), Some(id)) = (ids.get(k), a.and_then(tab)) {
                self.pane_active.insert(p, id);
            }
        }
    }

    /// What reopens this workspace at the next launch.
    pub fn saved(&self, cx: &App) -> SavedWorkspace {
        let tabs = self
            .tabs
            .iter()
            .map(|t| match t {
                CenterTab::Term(t) => {
                    let name = t.rename.as_ref().map(|r| r.to_string());
                    let mux = t.view.read(cx).mux_id();
                    match t.kind {
                        TabKind::Claude => SavedTab::Claude { name, session: t.session.clone(), mux },
                        TabKind::Shell => SavedTab::Shell { name, mux },
                    }
                },
                CenterTab::File { view, .. } => SavedTab::File(view.read(cx).path().map(Path::to_owned).unwrap_or_default()),
                CenterTab::Diff(d) => SavedTab::Diff(d.read(cx).path().to_owned()),
                CenterTab::Scratch { view, .. } => SavedTab::Scratch(view.read(cx).path().to_owned()),
            })
            .collect();
        let groups = self
            .groups
            .iter()
            .map(|g| SavedGroup { name: g.name.to_string(), color: GROUP_COLORS[g.color].0.into(), collapsed: g.collapsed })
            .collect();
        let tab_groups = (0..self.tabs.len()).map(|i| self.group_of(i).and_then(|g| self.groups.iter().position(|x| x.id == g))).collect();
        let order = self.layout.panes();
        let (panes, tab_panes, pane_active) = if order.len() > 1 || !self.layout.parts().is_empty() {
            (
                self.layout.encode(&order),
                (0..self.tabs.len()).map(|i| order.iter().position(|p| *p == self.pane_of(i)).unwrap_or(0)).collect(),
                order.iter().map(|p| self.pane_active_ix(*p)).collect(),
            )
        } else {
            Default::default()
        };
        SavedWorkspace {
            folder: self.root.clone(),
            tabs,
            tab_groups,
            groups,
            active: self.active,
            panes,
            tab_panes,
            pane_active,
        }
    }

    /// The side panels as this workspace has them.
    fn saved_panels(&self) -> SavedPanels {
        let hidden = [
            ("explorer", self.show_explorer),
            ("changes", self.show_changes),
            ("scratches", self.show_scratches),
            ("shells", self.show_shells),
            ("usage", self.show_usage),
        ]
        .into_iter()
        .filter(|(_, shown)| !shown)
        .map(|(name, _)| name.to_owned())
        .collect();
        SavedPanels { hidden, arrangements: self.saved_arrangements() }
    }

    /// Another workspace changed the side panels: show them the same.
    fn follow_panels(&mut self, cx: &mut Context<Self>) {
        let Some(p) = cx.try_global::<Panels>() else { return };
        if p.rev == self.panels_rev {
            return;
        }
        self.panels_rev = p.rev;
        let shown = |name: &str| !p.saved.hidden.iter().any(|h| h == name);
        (self.show_explorer, self.show_changes, self.show_scratches, self.show_shells, self.show_usage) =
            (shown("explorer"), shown("changes"), shown("scratches"), shown("shells"), shown("usage"));
        self.arrangements = Kept::new(p.saved.arrangements.iter().map(|(k, e)| (k.as_str(), e.as_str())));
        // Fit again: the docks' splits start over from the new arrangement.
        self.arranged_for = None;
        self.arranged = None;
        cx.notify();
    }

    fn subscribe_viewer(viewer: &Entity<Viewer>, window: &Window, cx: &mut Context<Self>) -> Vec<Subscription> {
        vec![
            cx.subscribe_in(viewer, window, |this, _, ev: &ViewerEvent, window, cx| match ev {
                ViewerEvent::OpenFile(p) => this.open_file(p.clone(), false, window, cx),
            }),
            // Dirty marker in the tab label.
            cx.observe(viewer, |_, _, cx| cx.notify()),
        ]
    }

    /// The workspace moved into `window`. Views holding text inputs listen
    /// for focus in the window they were made in, so rebuild those here,
    /// keeping what they showed (unsaved file edits included).
    pub fn rehost(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._rename = None;
        self.preview = None;
        for tab in &mut self.tabs {
            let old = tab.entity_id();
            if let CenterTab::File { view, _subs } = tab {
                let state = view.read(cx).state(cx);
                let root = self.root.clone();
                *view = cx.new(|cx| {
                    let mut v = Viewer::new(root, window, cx);
                    v.restore(state, window, cx);
                    v
                });
                *_subs = Self::subscribe_viewer(view, window, cx);
            }
            if let CenterTab::Scratch { view, _sub } = tab {
                view.update(cx, |s, cx| s.flush(cx));
                let path = view.read(cx).path().to_owned();
                *view = cx.new(|cx| Scratch::open(path, window, cx));
                *_sub = cx.observe(view, |_, _, cx| cx.notify());
            }
            // The views above are new entities: keep their tabs' groups.
            if let Some(g) = self.tab_group.remove(&old) {
                self.tab_group.insert(tab.entity_id(), g);
            }
            if let Some(p) = self.tab_pane.remove(&old) {
                self.tab_pane.insert(tab.entity_id(), p);
            }
            for shown in self.pane_active.values_mut().filter(|id| **id == old) {
                *shown = tab.entity_id();
            }
            if self.preview_tab == Some(old) {
                self.preview_tab = Some(tab.entity_id());
            }
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    /// The workspace is being closed: save scratches (dropping empty ones).
    /// Terminals and hook registrations go when the workspace is dropped.
    /// End every terminal's program (the workspace or its window was
    /// closed). Quitting Tern doesn't: they keep running in the background.
    pub fn end_terminals(&mut self, cx: &mut Context<Self>) {
        for t in self.tabs.iter().filter_map(CenterTab::term) {
            t.view.update(cx, |v, _| v.close());
        }
    }

    pub fn shutdown(&mut self, cx: &mut Context<Self>) {
        for tab in &self.tabs {
            if let CenterTab::Scratch { view, .. } = tab {
                view.update(cx, |s, cx| s.close(cx));
            }
        }
    }

    /// The folder's name, for tab and window titles.
    pub fn name(&self) -> String {
        self.root.file_name().map_or_else(|| self.root.display().to_string(), |n| n.to_string_lossy().into_owned())
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_active(window, cx);
    }

    fn spawn_config(&self, kind: TabKind, resume: Option<&str>) -> (SpawnConfig, &'static str) {
        let shell = crate::platform::shell();
        let mut env = std::collections::HashMap::new();
        env.insert("TERM_PROGRAM_VERSION".into(), env!("CARGO_PKG_VERSION").to_owned());
        let (args, title) = match kind {
            TabKind::Shell => (crate::platform::shell_args(), "shell"),
            TabKind::Claude => {
                // Run through an interactive login shell so PATH from rc files applies.
                // The daemon puts the hook settings file's path in place of
                // the placeholder (or drops the flag without hooks).
                let settings = " --settings {tern-settings}";
                // The user's own arguments, as typed (shell syntax).
                let args = window_state::claude_args();
                let claude = if args.is_empty() { format!("claude{settings}") } else { format!("claude {args}{settings}") };
                // A conversation that can't be resumed (deleted, ...) ends
                // Claude at once: the tab then starts a new one (see
                // `on_terminal_event`), not on every failed exit.
                // One moved to the background (`/bg`) is attached to instead.
                let bg = resume.and_then(|id| claude_bg_job(&claude_dir()?.join("sessions"), id));
                let claude = match (bg, resume) {
                    (Some(job), _) => format!("claude attach {}", shell_quote(&job)),
                    (None, Some(id)) => format!("{claude} --resume {}", shell_quote(id)),
                    (None, None) => claude,
                };
                // Hooks post to 127.0.0.1; keep them off the user's proxy.
                let no_proxy = ["NO_PROXY", "no_proxy"].iter().find_map(|k| std::env::var(k).ok()).filter(|v| !v.is_empty());
                let no_proxy = match no_proxy {
                    Some(v) => format!("{v},127.0.0.1,localhost"),
                    None => "127.0.0.1,localhost".to_owned(),
                };
                env.insert("NO_PROXY".into(), no_proxy.clone());
                env.insert("no_proxy".into(), no_proxy);
                (crate::platform::shell_command_args(&claude), "claude")
            },
        };
        (
            SpawnConfig {
                program: Some(shell),
                args,
                cwd: Some(self.root.clone()),
                env,
                size: TermSize { cols: 120, rows: 40, cell_width: 8, cell_height: 16 },
                scrollback: 10_000,
            },
            title,
        )
    }

    fn add_tab(&mut self, kind: TabKind, window: &mut Window, cx: &mut Context<Self>) {
        self.add_term(kind, None, None, None, window, cx);
    }

    /// A new terminal tab named `rename`: background session `mux` when it
    /// still runs, else a new program (a Claude one resumes `session`).
    fn add_term(
        &mut self,
        kind: TabKind,
        rename: Option<String>,
        session: Option<String>,
        mux: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Known once the daemon answers (`TerminalEvent::Ready`).
        let token = None;
        let (cfg, title) = self.spawn_config(kind, session.as_deref());
        let config = self.services.terminal.clone();
        let connector = self.services.mux.clone();
        let hook = kind == TabKind::Claude;
        let view = cx.new(|cx| TerminalView::new(cfg, hook, mux, title, config, connector, cx));
        let sub = cx.subscribe_in(&view, window, Self::on_terminal_event);
        let rename = rename.map(Into::into);
        // Set again at each start (`Ready`): only a `--resume` it starts,
        // not a running Claude it reattaches to, says something by exiting
        // at once.
        let resuming = session.is_some();
        let tab = TermTab { view, rename, kind, token, session, resuming, notified: None, rang: false, follows: None, _subs: vec![sub] };
        let ix = self.push_tab(CenterTab::Term(tab));
        self.activate(ix, window, cx);
    }

    /// The shells the agents in this workspace's terminals run commands in,
    /// oldest first.
    fn shell_rows(&self, cx: &App) -> Vec<ShellRow> {
        let mut rows = vec![];
        for (i, t) in self.tabs.iter().enumerate() {
            let Some(term) = t.term() else { continue };
            let v = term.view.read(cx);
            let Some(agent) = v.agent().filter(|_| !v.shells().is_empty()) else { continue };
            let agent = crate::agents::display_name(&self.tab_info(i, cx).0, agent);
            rows.extend(v.shells().iter().map(|s| ShellRow { tab: t.entity_id(), agent: agent.clone(), shell: s.clone() }));
        }
        rows.sort_by_key(|r| (r.shell.started, r.shell.pid));
        rows
    }

    /// Show what shell `row` runs: a tab following its output file (the
    /// one already open, else a new one), or its agent's tab when its
    /// output goes to no file.
    fn open_shell(&mut self, row: &ShellRow, window: &mut Window, cx: &mut Context<Self>) {
        let Some(output) = row.shell.output.clone() else {
            return self.show_tab(row.tab, window, cx);
        };
        if let Some(ix) = self.tabs.iter().position(|t| t.term().is_some_and(|t| t.follows.as_ref() == Some(&output))) {
            return self.activate(ix, window, cx);
        }
        let (mut cfg, _) = self.spawn_config(TabKind::Shell, None);
        // From its start, and on past the shell's end. Run directly, not
        // through a shell: the path is whatever the agent's shell opened.
        let Some(path) = output.to_str() else { return self.show_tab(row.tab, window, cx) };
        cfg.program = Some("/usr/bin/tail".to_owned());
        cfg.args = ["-n", "+1", "-F", "--", path].map(str::to_owned).to_vec();
        let title = crate::shells_view::first_line(&row.shell.command);
        let (config, connector) = (self.services.terminal.clone(), self.services.mux.clone());
        let view = cx.new(|cx| TerminalView::new(cfg, false, None, title.clone(), config, connector, cx));
        let sub = cx.subscribe_in(&view, window, Self::on_terminal_event);
        let tab = TermTab {
            view,
            rename: Some(title.into()),
            kind: TabKind::Shell,
            token: None,
            session: None,
            resuming: false,
            notified: None,
            rang: false,
            follows: Some(output),
            _subs: vec![sub],
        };
        let ix = self.push_tab(CenterTab::Term(tab));
        self.activate(ix, window, cx);
    }

    /// A tab for background session `s` that no tab shows. A Claude one
    /// resumes its conversation if it has to start again.
    pub fn adopt_session(&mut self, s: &tern_mux::proto::SessionInfo, window: &mut Window, cx: &mut Context<Self>) {
        if s.claude {
            // Claude names its tab itself.
            self.add_term(TabKind::Claude, None, s.claude_session.clone(), Some(s.id), window, cx);
        } else {
            self.add_term(TabKind::Shell, s.title.clone(), None, Some(s.id), window, cx);
        }
    }

    /// Each tab's background session id (`None` for other tabs), by index.
    pub fn mux_ids(&self, cx: &App) -> Vec<Option<u64>> {
        self.tabs.iter().map(|t| t.term().and_then(|t| t.view.read(cx).mux_id())).collect()
    }

    /// Make tab `ix` the active one.
    pub fn show_tab_at(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.activate(ix, window, cx);
    }

    /// Create a scratch file of type `ext` and open it.
    fn new_scratch(&mut self, ext: &str, window: &mut Window, cx: &mut Context<Self>) {
        match scratch::create(&scratch::dir(), ext) {
            Ok(path) => self.open_scratch(path, window, cx),
            Err(e) => notify_system("Cannot create scratch file", &e.to_string(), cx),
        }
    }

    /// Open a scratch file in a tab, or switch to the tab already showing it.
    fn open_scratch(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let existing = self.tabs.iter().position(|t| matches!(t, CenterTab::Scratch { view, .. } if view.read(cx).path() == path));
        if let Some(ix) = existing {
            return self.activate(ix, window, cx);
        }
        let view = cx.new(|cx| Scratch::open(path, window, cx));
        let sub = cx.observe(&view, |_, _, cx| cx.notify());
        let ix = self.push_tab(CenterTab::Scratch { view, _sub: sub });
        self.activate(ix, window, cx);
    }

    /// Ask before deleting scratch file `path`.
    fn confirm_delete_scratch(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let ws = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let footer = template::render(
                "dialog.delete_scratch.footer",
                template::Scope::new("delete-scratch"),
                &mut |n, _, f| match (n, f) {
                    ("cancel", Framed::Button(b)) => b.on_click(|_, window, cx| window.close_dialog(cx)).into_any_element(),
                    ("ok", Framed::Button(b)) => {
                        let (ws, path) = (ws.clone(), path.clone());
                        b.on_click(move |_, window, cx| {
                            window.close_dialog(cx);
                            let _ = ws.update(cx, |this, cx| this.delete_scratch(&path, window, cx));
                        })
                        .into_any_element()
                    },
                    (_, f) => f.into_any_element(),
                },
                cx,
            );
            let name = name.clone();
            dialog
                .w(style::dialog_width(false, cx))
                .title("Delete Scratch File")
                .content(move |c, _, cx| {
                    let scope = template::Scope::new("delete-scratch").var("name", name.clone());
                    c.child(template::render_plain("dialog.delete_scratch", scope, cx))
                })
                .footer(footer)
        });
    }

    /// Close the tab showing scratch `path` (if any), then delete the file.
    fn delete_scratch(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let open = self.tabs.iter().position(|t| matches!(t, CenterTab::Scratch { view, .. } if view.read(cx).path() == path));
        if let Some(ix) = open {
            self.close_tab(ix, window, cx);
        }
        if let Err(e) = std::fs::remove_file(path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            window.push_notification(format!("Delete failed: {e}"), cx);
        }
        self.scratches.update(cx, |s, cx| s.refresh(cx));
    }

    /// What closing tab `ix` would end: a Claude turn in progress (an idle
    /// conversation resumes at the next launch), or a program a shell runs
    /// (not the `tail` following an agent's shell: it opens again).
    fn running_in(&self, ix: usize, cx: &App) -> Option<String> {
        let t = self.tabs.get(ix)?.term()?;
        if t.follows.is_some() {
            return None;
        }
        let v = t.view.read(cx);
        match t.kind {
            TabKind::Claude => {
                matches!(v.status(), ActivityStatus::Working | ActivityStatus::Blocked).then(|| "Claude, mid-turn".to_owned())
            },
            TabKind::Shell => v.foreground_job(),
        }
    }

    /// Everything closing this workspace would end (see `running_in`).
    pub fn running(&self, cx: &App) -> Vec<String> {
        (0..self.tabs.len()).filter_map(|i| self.running_in(i, cx)).collect()
    }

    /// Close tab `ix`, asking first when that ends a running program.
    fn request_close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.tabs.get(ix).map(CenterTab::entity_id) else { return };
        match self.running_in(ix, cx) {
            None => self.close_tab(ix, window, cx),
            Some(job) => confirm_close(vec![job], "this tab", window, cx, move |this: &mut Self, window, cx| {
                if let Some(ix) = this.tabs.iter().position(|t| t.entity_id() == id) {
                    this.close_tab(ix, window, cx);
                }
            }),
        }
    }

    fn close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        let pane = self.pane_of(ix);
        let active_id = self.tabs.get(self.active).map(CenterTab::entity_id);
        let tab = self.tabs.remove(ix);
        let id = tab.entity_id();
        self.tab_group.remove(&id);
        self.tab_pane.remove(&id);
        self.prune_groups();
        if let CenterTab::Scratch { view, .. } = &tab {
            view.update(cx, |s, cx| s.close(cx));
        }
        if let Some(t) = tab.term() {
            t.view.update(cx, |v, _| v.close());
        }
        if self.tabs.is_empty() {
            cx.emit(WorkspaceEvent::TabsChanged);
            return;
        }
        // Its pane shows its next tab (else the one before); a pane left
        // without tabs closes.
        let next = (ix..self.tabs.len()).chain((0..ix).rev()).find(|&i| self.pane_of(i) == pane && !self.hidden(i));
        let next = next.or_else(|| (0..self.tabs.len()).find(|&i| self.pane_of(i) == pane));
        if self.pane_active.get(&pane) == Some(&id)
            && let Some(n) = next
        {
            self.pane_active.insert(pane, self.tabs[n].entity_id());
        }
        self.active = match active_id.filter(|a| *a != id).and_then(|a| self.tabs.iter().position(|t| t.entity_id() == a)) {
            Some(i) => i,
            None => next.unwrap_or(0),
        };
        self.drop_if_empty(pane);
        self.sync_panes();
        self.focus_active(window, cx);
        cx.emit(WorkspaceEvent::TabsChanged);
        cx.notify();
    }

    /// Move the tab showing entity `id` to slot `to`, into group `join` (or
    /// out of any), keeping it active.
    fn move_tab(&mut self, id: EntityId, to: usize, join: Option<u64>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(from) = self.tabs.iter().position(|t| t.entity_id() == id) else { return };
        let to = to.min(self.tabs.len() - 1);
        // The pane of the tab (or group) it is dropped on.
        let old = self.pane_of(from);
        let pane = match join.and_then(|g| self.group_tabs(g)) {
            Some(r) => self.pane_of(r.start),
            None => self.pane_of(to),
        };
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        match join.filter(|&g| self.group(g).is_some()) {
            Some(g) => self.tab_group.insert(id, g),
            None => self.tab_group.remove(&id),
        };
        self.tab_pane.insert(id, pane);
        self.regroup();
        if let Some(ix) = self.tabs.iter().position(|t| t.entity_id() == id) {
            self.activate(ix, window, cx);
        }
        if old != pane {
            self.drop_if_empty(old);
        }
    }

    /// Move the tab showing entity `id` to the front of group `g`.
    fn move_tab_to_group(&mut self, id: EntityId, g: u64, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(from), Some(r)) = (self.tabs.iter().position(|t| t.entity_id() == id), self.group_tabs(g)) else { return };
        let to = if from < r.start { r.start - 1 } else { r.start };
        self.move_tab(id, to, Some(g), window, cx);
    }

    fn group(&self, id: u64) -> Option<&TabGroup> {
        self.groups.iter().find(|g| g.id == id)
    }

    fn group_mut(&mut self, id: u64) -> Option<&mut TabGroup> {
        self.groups.iter_mut().find(|g| g.id == id)
    }

    /// The group tab `ix` is in.
    fn group_of(&self, ix: usize) -> Option<u64> {
        self.tabs.get(ix).and_then(|t| self.tab_group.get(&t.entity_id()).copied())
    }

    /// Group `id`'s tabs (they are next to each other).
    fn group_tabs(&self, id: u64) -> Option<Range<usize>> {
        let first = (0..self.tabs.len()).find(|&i| self.group_of(i) == Some(id))?;
        let last = (0..self.tabs.len()).rfind(|&i| self.group_of(i) == Some(id))?;
        Some(first..last + 1)
    }

    /// Whether tab `ix` is folded away in a collapsed group.
    fn hidden(&self, ix: usize) -> bool {
        self.group_of(ix).and_then(|g| self.group(g)).is_some_and(|g| g.collapsed)
    }

    /// The shown tab of `ix`'s pane closest to it, looking right first.
    fn nearest_visible(&self, ix: usize) -> Option<usize> {
        let pane = self.pane_of(ix);
        (ix..self.tabs.len()).chain((0..ix).rev()).find(|&i| self.pane_of(i) == pane && !self.hidden(i))
    }

    fn expand_group_of(&mut self, ix: usize) {
        if let Some(g) = self.group_of(ix).and_then(|g| self.group_mut(g)) {
            g.collapsed = false;
        }
    }

    /// The bar's slots: group chips and the tabs that show, of every pane.
    #[cfg(debug_assertions)] // the debug control socket
    fn slots(&self) -> Vec<Slot> {
        self.layout.panes().into_iter().flat_map(|p| self.slots_in(p)).collect()
    }

    /// Pane `pane`'s bar slots.
    fn slots_in(&self, pane: PaneId) -> Vec<Slot> {
        let mut out = vec![];
        let mut last = None;
        for i in (0..self.tabs.len()).filter(|&i| self.pane_of(i) == pane) {
            let g = self.group_of(i);
            if let Some(g) = g
                && last != Some(g)
            {
                out.push(Slot::Group(g));
            }
            last = g;
            if !self.hidden(i) {
                out.push(Slot::Tab(i));
            }
        }
        out
    }

    fn add_group(&mut self, name: SharedString, color: usize, collapsed: bool) -> u64 {
        let id = self.next_group;
        self.next_group += 1;
        self.groups.push(TabGroup { id, name, color: color % GROUP_COLORS.len(), collapsed });
        id
    }

    /// Run `f`, then point `active` back at the tab it was on.
    fn keep_active(&mut self, f: impl FnOnce(&mut Self)) {
        let id = self.tabs.get(self.active).map(CenterTab::entity_id);
        f(self);
        if let Some(ix) = id.and_then(|id| self.tabs.iter().position(|t| t.entity_id() == id)) {
            self.active = ix;
        }
    }

    /// Gather each group's tabs where its first tab is, and drop groups
    /// left without tabs.
    fn regroup(&mut self) {
        self.keep_active(|this| {
            let ids: Vec<Option<u64>> = (0..this.tabs.len()).map(|i| this.group_of(i)).collect();
            let mut seen = HashSet::new();
            let mut order = vec![];
            for (i, g) in ids.iter().enumerate() {
                match g {
                    None => order.push(i),
                    Some(g) if seen.insert(*g) => order.extend((i..ids.len()).filter(|&j| ids[j] == Some(*g))),
                    Some(_) => {},
                }
            }
            let mut old: Vec<Option<CenterTab>> = std::mem::take(&mut this.tabs).into_iter().map(Some).collect();
            this.tabs = order.into_iter().filter_map(|i| old[i].take()).collect();
        });
        self.prune_groups();
    }

    /// Forget closed tabs and groups without tabs.
    fn prune_groups(&mut self) {
        let live: HashSet<EntityId> = self.tabs.iter().map(CenterTab::entity_id).collect();
        let groups = &self.groups;
        self.tab_group.retain(|id, g| live.contains(id) && groups.iter().any(|x| x.id == *g));
        let used: HashSet<u64> = self.tab_group.values().copied().collect();
        self.groups.retain(|g| used.contains(&g.id));
    }

    /// Put tab `ix` in group `g`, beside its tabs, or take it out of its
    /// group (it goes right after the group).
    fn set_tab_group(&mut self, ix: usize, g: Option<u64>) {
        let Some(id) = self.tabs.get(ix).map(CenterTab::entity_id) else { return };
        let old = self.pane_of(ix);
        match g {
            Some(g) => {
                if let Some(r) = self.group_tabs(g)
                    && !r.contains(&ix)
                {
                    // A group's tabs share a pane.
                    self.tab_pane.insert(id, self.pane_of(r.start));
                    // The tab goes to the group, not the group to the tab.
                    self.keep_active(|this| {
                        let tab = this.tabs.remove(ix);
                        this.tabs.insert(if ix < r.start { r.end - 1 } else { r.end }, tab);
                    });
                }
                self.tab_group.insert(id, g);
            },
            None => {
                self.tab_group.remove(&id);
            },
        }
        self.regroup();
        self.drop_if_empty(old);
    }

    fn groups_changed(&mut self, cx: &mut Context<Self>) {
        self.sync_panes();
        cx.emit(WorkspaceEvent::TabsChanged);
        cx.notify();
    }

    /// A new group holding tab `ix`, in a color no group has yet.
    fn new_group(&mut self, ix: usize) -> u64 {
        let n = GROUP_COLORS.len();
        // Blue first, grey last.
        let color = (1..=n).map(|c| c % n).find(|c| !self.groups.iter().any(|g| g.color == *c)).unwrap_or(self.groups.len() + 1);
        let g = self.add_group(SharedString::default(), color, false);
        self.set_tab_group(ix, Some(g));
        g
    }

    /// Group tab `ix` on its own, then ask for the group's name, as Chrome does.
    fn add_to_new_group(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let g = self.new_group(ix);
        self.groups_changed(cx);
        self.rename_group(g, window, cx);
    }

    fn add_to_group(&mut self, ix: usize, g: u64, cx: &mut Context<Self>) {
        self.set_tab_group(ix, Some(g));
        self.groups_changed(cx);
    }

    fn remove_from_group(&mut self, ix: usize, _: &mut Window, cx: &mut Context<Self>) {
        self.set_tab_group(ix, None);
        self.groups_changed(cx);
    }

    /// Collapse group `g` to its chip, or expand it. The active tab moves
    /// out of a group that collapses; a group holding every tab stays open.
    fn toggle_group(&mut self, g: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(group) = self.group_mut(g) else { return };
        group.collapsed = !group.collapsed;
        if self.hidden(self.active) {
            match self.nearest_visible(self.active) {
                Some(ix) => self.activate(ix, window, cx),
                None => self.expand_group_of(self.active),
            }
        }
        self.groups_changed(cx);
    }

    fn set_group_color(&mut self, g: u64, color: usize, cx: &mut Context<Self>) {
        if let Some(group) = self.group_mut(g) {
            group.color = color;
        }
        self.groups_changed(cx);
    }

    fn set_group_name(&mut self, g: u64, name: &str, cx: &mut Context<Self>) {
        if let Some(group) = self.group_mut(g) {
            group.name = name.trim().to_owned().into();
        }
        self.groups_changed(cx);
    }

    fn ungroup(&mut self, g: u64, _: &mut Window, cx: &mut Context<Self>) {
        self.tab_group.retain(|_, x| *x != g);
        self.prune_groups();
        self.groups_changed(cx);
    }

    fn close_group(&mut self, g: u64, window: &mut Window, cx: &mut Context<Self>) {
        let doomed: HashSet<EntityId> = self.tab_group.iter().filter(|(_, x)| **x == g).map(|(id, _)| *id).collect();
        self.close_where(|_, t| doomed.contains(&t.entity_id()), window, cx);
    }

    /// A new terminal tab at the end of group `g`.
    fn new_tab_in_group(&mut self, kind: TabKind, g: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.add_tab(kind, window, cx);
        self.set_tab_group(self.tabs.len() - 1, Some(g));
        self.groups_changed(cx);
    }

    /// Move group `g`'s tabs to where tab `to` is (`to` = the tab count: the
    /// end), never into another group.
    fn move_group(&mut self, g: u64, to: usize, cx: &mut Context<Self>) {
        let Some(r) = self.group_tabs(g) else { return };
        if r.contains(&to) {
            return;
        }
        let pane = (to < self.tabs.len()).then(|| self.pane_of(to));
        self.keep_active(|this| {
            let target = match this.group_of(to).and_then(|h| this.group_tabs(h)) {
                Some(t) => t,
                None => to.min(this.tabs.len())..(to + 1).min(this.tabs.len()),
            };
            let block: Vec<CenterTab> = this.tabs.drain(r.clone()).collect();
            // Dropped further right: after the target; else before it.
            let at = if target.start > r.start { target.end - r.len() } else { target.start };
            this.tabs.splice(at..at, block);
        });
        if let Some(p) = pane {
            self.set_group_pane(g, p);
        }
        self.groups_changed(cx);
    }

    /// Move group `g`'s tabs to pane `pane`.
    fn set_group_pane(&mut self, g: u64, pane: PaneId) {
        let Some(r) = self.group_tabs(g) else { return };
        let old = self.pane_of(r.start);
        for i in r {
            self.tab_pane.insert(self.tabs[i].entity_id(), pane);
        }
        if old != pane {
            self.drop_if_empty(old);
        }
    }

    /// The next (or previous) tab that shows, wrapping around.
    fn step_tab(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let tabs: Vec<usize> = self.pane_tabs(self.focused).into_iter().filter(|&i| !self.hidden(i)).collect();
        let n = tabs.len();
        if n == 0 {
            return;
        }
        let at = tabs.iter().position(|&i| i == self.active).unwrap_or(0);
        let next = if forward { (at + 1) % n } else { (at + n - 1) % n };
        self.activate(tabs[next], window, cx);
    }

    fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix < self.tabs.len() {
            // A tab picked by number or from a menu opens its group.
            self.expand_group_of(ix);
            self.active = ix;
            self.focused = self.pane_of(ix);
            self.pane_active.insert(self.focused, self.tabs[ix].entity_id());
            self.sync_panes();
            if let CenterTab::Term(t) = &mut self.tabs[ix] {
                t.rang = false;
                t.view.update(cx, |v, cx| v.clear_done_status(cx));
            }
            self.focus_active(window, cx);
            cx.emit(WorkspaceEvent::TabsChanged);
            cx.notify();
        }
    }

    fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.tabs.get(self.active) {
            Some(t) => window.focus(&t.focus_handle(cx), cx),
            // No tab: keys still reach the workspace through the explorer.
            None => window.focus(&self.tree.focus_handle(cx), cx),
        }
    }

    /// Show `path`'s diff in a popup centered on the window, with a button
    /// to move it into a center tab.
    fn preview_diff(&mut self, path: PathBuf, status: GitStatus, window: &mut Window, cx: &mut Context<Self>) {
        window.close_all_dialogs(cx);
        let rel = path.strip_prefix(&self.root).unwrap_or(&path).display().to_string();
        let title: SharedString = format!("{rel}  {}", status.letter()).into();
        let pane = cx.new(|cx| DiffPane::new(path, cx));
        self.preview = Some(pane.downgrade());
        let ws = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let vp = window.viewport_size();
            let body = pane.clone();
            let footer = template::render(
                "dialog.diff_preview.footer",
                template::Scope::new("diff-preview"),
                &mut |n, _, f| match (n, f) {
                    ("open_in_tab", Framed::Button(b)) => {
                        let (pane, ws) = (pane.clone(), ws.clone());
                        b.on_click(move |_, window, cx| {
                            window.close_dialog(cx);
                            let _ = ws.update(cx, |this, cx| this.open_diff_tab(pane.clone(), window, cx));
                        })
                        .into_any_element()
                    },
                    (_, f) => f.into_any_element(),
                },
                cx,
            );
            dialog
                .w(vp.width * 0.8)
                .margin_top(vp.height * 0.06)
                .title(title.clone())
                .content(move |c, window, cx| {
                    let height = f64::from(f32::from(window.viewport_size().height * 0.7));
                    let scope = template::Scope::new("diff-preview").var("height", height).slot("diff", body.clone());
                    c.child(template::render_plain("dialog.diff_preview", scope, cx))
                })
                .footer(footer)
        });
    }

    /// Show `pane` as a center tab; a tab already showing that file is
    /// reused instead.
    fn open_diff_tab(&mut self, pane: Entity<DiffPane>, window: &mut Window, cx: &mut Context<Self>) {
        let path = pane.read(cx).path().to_owned();
        let existing = self.tabs.iter().position(|t| matches!(t, CenterTab::Diff(d) if d.read(cx).path() == path));
        match existing {
            Some(ix) => {
                if let CenterTab::Diff(d) = &self.tabs[ix] {
                    d.update(cx, |d, cx| d.reload(cx));
                }
                self.activate(ix, window, cx);
            },
            None => {
                let ix = self.push_tab(CenterTab::Diff(pane));
                self.activate(ix, window, cx);
            },
        }
    }

    /// Reload open files and diffs (tabs and the preview popup) affected by `batch`.
    fn files_changed(&mut self, batch: &ChangeBatch, window: &mut Window, cx: &mut Context<Self>) {
        let preview = self.preview.as_ref().and_then(|p| p.upgrade());
        let mut diffs = vec![];
        for t in &self.tabs {
            match t {
                CenterTab::File { view, .. } => view.update(cx, |v, cx| v.files_changed(&batch.paths, window, cx)),
                CenterTab::Diff(d) => diffs.push(d.clone()),
                CenterTab::Term(_) | CenterTab::Scratch { .. } => {},
            }
        }
        for d in diffs.into_iter().chain(preview) {
            d.update(cx, |d, cx| d.files_changed(batch, cx));
        }
    }

    /// Point file tabs showing `from` (or something inside it) at `to`.
    fn file_moved(&mut self, from: &Path, to: &Path, window: &mut Window, cx: &mut Context<Self>) {
        for t in &self.tabs {
            if let CenterTab::File { view, .. } = t
                && let Some(rest) = view.read(cx).path().and_then(|p| p.strip_prefix(from).ok())
            {
                let path = to.join(rest);
                view.update(cx, |v, cx| v.set_path(path, window, cx));
            }
        }
        cx.emit(WorkspaceEvent::TabsChanged);
        cx.notify();
    }

    /// Close the file and diff tabs of trashed `paths`, except files with
    /// unsaved edits (saving brings them back).
    fn files_deleted(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        let gone = |t: &CenterTab| {
            let dirty = matches!(t, CenterTab::File { view, .. } if view.read(cx).is_dirty());
            !dirty && !matches!(t, CenterTab::Scratch { .. }) && t.file_path(cx).is_some_and(|f| paths.iter().any(|p| f.starts_with(p)))
        };
        let doomed: HashSet<EntityId> = self.tabs.iter().filter(|t| gone(t)).map(CenterTab::entity_id).collect();
        if doomed.is_empty() {
            return;
        }
        self.close_where(|_, t| doomed.contains(&t.entity_id()), window, cx);
    }

    /// Open `path` in a center tab, or switch to the tab already showing it.
    /// Unless `pin`, it replaces the file in the preview tab (VS Code's
    /// single click); `pin` keeps the tab it ends up in (double click).
    fn open_file(&mut self, path: PathBuf, pin: bool, window: &mut Window, cx: &mut Context<Self>) {
        let existing =
            self.tabs.iter().position(|t| matches!(t, CenterTab::File { view, .. } if view.read(cx).path() == Some(path.as_path())));
        let reuse = existing.or_else(|| self.preview_ix(cx));
        if let Some(ix) = reuse {
            if let CenterTab::File { view, .. } = &self.tabs[ix] {
                view.update(cx, |v, cx| v.open(path, window, cx));
            }
            if pin && self.preview_tab == Some(self.tabs[ix].entity_id()) {
                self.preview_tab = None;
            }
            return self.activate(ix, window, cx);
        }
        let root = self.root.clone();
        let view = cx.new(|cx| {
            let mut v = Viewer::new(root, window, cx);
            v.open(path, window, cx);
            v
        });
        let _subs = Self::subscribe_viewer(&view, window, cx);
        if !pin {
            self.preview_tab = Some(view.entity_id());
        }
        let ix = self.push_tab(CenterTab::File { view, _subs });
        self.activate(ix, window, cx);
    }

    /// The preview tab's index, while it still shows an unedited file.
    fn preview_ix(&self, cx: &App) -> Option<usize> {
        let id = self.preview_tab?;
        self.tabs.iter().position(|t| match t {
            CenterTab::File { view, .. } => view.entity_id() == id && !view.read(cx).is_dirty(),
            _ => false,
        })
    }

    /// Keep tab `i` instead of letting the next preview replace it.
    fn pin_tab(&mut self, i: usize, cx: &mut Context<Self>) {
        if self.tabs.get(i).is_some_and(|t| self.preview_tab == Some(t.entity_id())) {
            self.preview_tab = None;
            cx.notify();
        }
    }

    /// Whether `view` is the tab shown in one of the panes.
    fn is_shown(&self, view: &Entity<TerminalView>) -> bool {
        self.shown_panes().into_iter().any(|p| self.pane_active_ix(p).and_then(|i| self.tabs[i].term()).is_some_and(|t| &t.view == view))
    }

    fn term_tab_mut(&mut self, view: &Entity<TerminalView>) -> Option<&mut TermTab> {
        self.tabs.iter_mut().find_map(|t| match t {
            CenterTab::Term(t) if &t.view == view => Some(t),
            _ => None,
        })
    }

    /// Show a notification for `view`'s tab unless it showed one moments
    /// ago (Claude Code sends OSC 9 and a status change for one event).
    fn notify_for(&mut self, view: &Entity<TerminalView>, title: &str, body: &str, cx: &mut Context<Self>) {
        let Some(tab) = self.term_tab_mut(view) else { return };
        if tab.notified.is_some_and(|t| t.elapsed() < Duration::from_secs(3)) {
            return;
        }
        tab.notified = Some(Instant::now());
        notify_system(title, body, cx);
    }

    fn on_terminal_event(&mut self, view: &Entity<TerminalView>, ev: &TerminalEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            TerminalEvent::TitleChanged => cx.notify(),
            TerminalEvent::Ready { token, attached } => {
                if let Some(t) = self.term_tab_mut(view) {
                    t.token = token.clone();
                    // A Claude started anew runs `--resume` of its session
                    // (see `spawn_config`); one it reattached to doesn't.
                    t.resuming = !*attached && t.kind == TabKind::Claude && t.session.is_some();
                }
                // Its id goes into the saved session.
                cx.emit(WorkspaceEvent::TabsChanged);
            },
            // A Claude tab starting again resumes its conversation; if that
            // fails at once, a new one starts (see `Exited`).
            TerminalEvent::Restored => {
                if let Some(t) = self.term_tab_mut(view) {
                    t.resuming = t.kind == TabKind::Claude && t.session.is_some();
                }
            },
            TerminalEvent::Exited { code, quick, live } => {
                // Claude ended on purpose (/exit): the tab goes on as a
                // shell, as if Claude had been started from one.
                if *live
                    && *code == Some(0)
                    && let Some(t) = self.term_tab_mut(view)
                    && t.kind == TabKind::Claude
                {
                    t.kind = TabKind::Shell;
                    t.token = None;
                    t.session = None;
                    t.resuming = false;
                    let (cfg, _) = self.spawn_config(TabKind::Shell, None);
                    view.update(cx, |v, cx| {
                        v.set_spawn_config(cfg);
                        v.set_hook(false);
                        v.start(cx);
                    });
                    cx.emit(WorkspaceEvent::TabsChanged);
                    cx.notify();
                    return;
                }
                // `claude --resume` failing at once: the conversation is
                // gone; start a new one in its place.
                let restart = match self.term_tab_mut(view) {
                    Some(t) if t.resuming && *quick && code.is_some_and(|c| c != 0) => {
                        t.resuming = false;
                        t.session = None;
                        Some(t.token.clone())
                    },
                    Some(t) => {
                        t.resuming = false;
                        None
                    },
                    None => None,
                };
                if restart.is_some() {
                    let (cfg, _) = self.spawn_config(TabKind::Claude, None);
                    view.update(cx, |v, cx| {
                        v.set_spawn_config(cfg);
                        v.start(cx);
                    });
                    cx.emit(WorkspaceEvent::TabsChanged);
                } else if let Some(t) = self.term_tab_mut(view)
                    && t.kind == TabKind::Claude
                {
                    // Enter restarts it: resume the conversation it had.
                    let session = t.session.clone();
                    let (cfg, _) = self.spawn_config(TabKind::Claude, session.as_deref());
                    view.update(cx, |v, _| v.set_spawn_config(cfg));
                }
                cx.notify();
            },
            TerminalEvent::StatusChanged(status) => {
                // Finished in front of you: seen, so not left as done.
                if *status == ActivityStatus::Done && window.is_window_active() && self.is_shown(view) {
                    view.update(cx, |v, cx| v.clear_done_status(cx));
                }
                if !window.is_window_active() || !self.is_shown(view) {
                    let (title, body) = match status {
                        ActivityStatus::Blocked => ("Claude needs your input", "Waiting for permission or an answer"),
                        ActivityStatus::Done => ("Claude finished", "The turn is complete"),
                        _ => ("", ""),
                    };
                    if !title.is_empty() {
                        self.notify_for(view, title, body, cx);
                    }
                }
                cx.notify();
            },
            TerminalEvent::Notify { title, body } => {
                if !window.is_window_active() || !self.is_shown(view) {
                    let title = match title {
                        Some(t) if !t.is_empty() => t.clone(),
                        _ => {
                            let v = view.read(cx);
                            v.agent().map_or_else(|| v.title().to_string(), |a| a.name().to_owned())
                        },
                    };
                    self.notify_for(view, &title, body, cx);
                }
            },
            TerminalEvent::Bell => {
                let shown = self.is_shown(view);
                if shown && window.is_window_active() {
                    window.play_system_bell();
                    return;
                }
                if !shown && let Some(t) = self.term_tab_mut(view) {
                    t.rang = true;
                }
                if !window.is_window_active() {
                    window.request_attention();
                }
                cx.notify();
            },
            TerminalEvent::OpenPath { path, line, col } => {
                if path.is_dir() && !path.starts_with(&self.root) {
                    return cx.reveal_path(path);
                }
                if path.is_dir() {
                    if !self.show_explorer {
                        self.show_explorer = true;
                        self.panels_changed(cx);
                    }
                    self.tree.update(cx, |t, cx| t.reveal(path, cx));
                    return cx.notify();
                }
                match *line {
                    Some(line) => self.open_file_at(path.clone(), line, *col, window, cx),
                    None => self.open_file(path.clone(), false, window, cx),
                }
            },
        }
    }

    pub fn on_hook(&mut self, ev: HookEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Only events from this window's tabs.
        let Some(tab) = self.tabs.iter_mut().find_map(|t| match t {
            CenterTab::Term(t) if t.token.as_deref() == Some(ev.token.as_str()) => Some(t),
            _ => None,
        }) else {
            return;
        };
        if ev.session_id.is_some() && tab.session != ev.session_id {
            tab.session = ev.session_id.clone();
            // Should Claude have to start again (Enter, its daemon
            // restarted), it resumes this conversation.
            if tab.kind == TabKind::Claude {
                let view = tab.view.clone();
                let (cfg, _) = self.spawn_config(TabKind::Claude, ev.session_id.as_deref());
                view.update(cx, |v, _| v.set_spawn_config(cfg));
            }
            cx.emit(WorkspaceEvent::TabsChanged);
        }
        let paths: Vec<PathBuf> = ev.paths.into_iter().filter(|p| p.starts_with(&self.root)).collect();
        if paths.is_empty() {
            return;
        }
        self.tree.update(cx, |t, cx| t.mark_claude_edit(&paths, cx));
        let batch = ChangeBatch { paths, git_repos: vec![] };
        self.changes.update(cx, |c, cx| c.files_changed(&batch, cx));
        self.files_changed(&batch, window, cx);
    }

    /// The terminal tabs, in tab order: where a screenshot can go, and the
    /// agents the monitor lists.
    pub fn term_tabs(&self, cx: &App) -> Vec<TermTabInfo> {
        self.tabs
            .iter()
            .enumerate()
            .filter_map(|(i, tab)| {
                let t = tab.term()?;
                Some(TermTabInfo {
                    id: t.view.entity_id(),
                    title: self.tab_info(i, cx).0,
                    agent: t.view.read(cx).agent().or((t.kind == TabKind::Claude).then_some(Agent::Claude)),
                    active: i == self.active,
                    view: t.view.clone(),
                })
            })
            .collect()
    }

    /// The terminal tabs running an agent, as the window's agents panel lists
    /// them under this workspace's folder, in tab order: each one's entity
    /// and its `sidebar.tab` values. `here` is whether this workspace is the
    /// shown one.
    pub fn sidebar_tabs(&self, here: bool, cx: &App) -> Vec<(EntityId, template::Scope, ActivityStatus)> {
        let close_key = crate::platform::shortcut("⌘W").to_string();
        (0..self.tabs.len())
            .filter(|&i| self.tabs[i].term().is_some_and(|t| t.kind == TabKind::Claude || t.view.read(cx).agent().is_some()))
            .map(|i| {
                let (label, _, _) = self.tab_info(i, cx);
                let id = self.tabs[i].entity_id();
                let Some(t) = self.tabs[i].term() else { unreachable!("filtered to terminals") };
                let v = t.view.read(cx);
                let (now, agent) = (v.status(), v.agent().unwrap_or(Agent::Claude));
                let (status, icon, color) = crate::agents::status_look(now, t.rang, v.status_since().elapsed(), cx.theme());
                let label = crate::agents::display_name(&label, agent);
                let mut scope = template::Scope::new(format!("side-tab-{}", id.as_u64()))
                    .var("label", label)
                    .var("active", here && i == self.active)
                    .var("shown", self.pane_active.get(&self.pane_of(i)) == Some(&id))
                    .var("close_key", close_key.clone())
                    .var("buttons", true)
                    .slot_opt("icon", self.tab_icon(i, None, cx));
                scope = scope.var("status", status).var("status_color", color).var("working", now == ActivityStatus::Working);
                if !icon.is_empty() {
                    scope = scope.var("status_icon", icon);
                }
                (id, scope, now)
            })
            .collect()
    }

    /// `d`, the agents panel row of tab `id`: dragged into a pane, the tab moves
    /// there; a right click opens the tab's menu.
    pub fn sidebar_tab_row(ws: &WeakEntity<Self>, id: EntityId, d: Stateful<Div>, cx: &App) -> AnyElement {
        let name = ws
            .upgrade()
            .and_then(|w| {
                let w = w.read(cx);
                let ix = w.tabs.iter().position(|t| t.entity_id() == id)?;
                Some(w.tab_info(ix, cx).1)
            })
            .unwrap_or_default();
        let menu_ws = ws.clone();
        d.on_drag(DraggedTab { tab: id, name }, Self::drag_start(ws.clone()))
            .context_menu(move |menu, window, cx| {
                let Some(this) = menu_ws.upgrade() else { return menu };
                let this = this.read(cx);
                let Some(ix) = this.tabs.iter().position(|t| t.entity_id() == id) else { return menu };
                let (name, scope, items) = Self::tab_menu(&menu_ws, ix, this, cx);
                template::menu(name, scope, menu, items, window, cx)
            })
            .into_any_element()
    }

    /// Close the tab with entity `id`, asking first when that ends a running program.
    pub fn request_close_id(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.tabs.iter().position(|t| t.entity_id() == id) {
            self.request_close_tab(ix, window, cx);
        }
    }

    /// `b` with the new-tab menu (Claude, shell, agents, scratch).
    pub fn new_tab_button(b: Button, ws: WeakEntity<Self>) -> AnyElement {
        Self::new_tab_menu(b, ws).into_any_element()
    }

    /// Show the tab with entity `id` and give it focus.
    pub fn show_tab(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.tabs.iter().position(|t| t.entity_id() == id) {
            // Another pane filling the center would hide it.
            if self.zoomed.is_some_and(|z| z != self.pane_of(ix)) {
                self.zoomed = None;
            }
            self.activate(ix, window, cx);
        }
    }

    /// Open a tab running `agent`: a Claude tab, or a shell that starts it.
    #[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))] // for the screenshot tray
    pub fn new_agent_tab(&mut self, agent: Agent, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<TerminalView>> {
        let kind = if agent == Agent::Claude { TabKind::Claude } else { TabKind::Shell };
        self.add_tab(kind, window, cx);
        let view = self.tabs.get(self.active)?.term()?.view.clone();
        if kind == TabKind::Shell {
            view.update(cx, |v, cx| v.write_input(format!("{}\r", agent.command()).into_bytes(), cx));
        }
        Some(view)
    }

    /// What tab `i` shows: its label, its name for drags, and a terminal's
    /// activity badge.
    fn tab_info(&self, i: usize, cx: &App) -> (String, SharedString, Option<(&'static str, Hsla)>) {
        let theme = cx.theme();
        let t = match &self.tabs[i] {
            CenterTab::Term(t) => t,
            CenterTab::File { view, .. } => {
                let v = view.read(cx);
                let name = v.path().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let dot = if v.is_dirty() { " ●" } else { "" };
                return (format!("{name}{dot}"), name.into(), None);
            },
            CenterTab::Diff(d) => {
                let path = d.read(cx).path();
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                return (name.clone(), format!("± {name}").into(), None);
            },
            CenterTab::Scratch { view: s, .. } => {
                let s = s.read(cx);
                let dot = if s.is_dirty() { " ●" } else { "" };
                return (format!("{}{dot}", s.title()), s.title().to_string().into(), None);
            },
        };
        let v = t.view.read(cx);
        let badge = match v.status() {
            ActivityStatus::Working => ("● ", theme.warning),
            ActivityStatus::Blocked => ("! ", theme.danger),
            ActivityStatus::Done => ("✓ ", theme.success),
            ActivityStatus::None if t.rang => ("🔔 ", theme.warning),
            ActivityStatus::None => ("", theme.success),
        };
        let mut label = t.rename.as_ref().map_or_else(|| v.title().to_string(), |r| r.to_string());
        if label.chars().count() > 28 {
            label = label.chars().take(27).collect::<String>() + "…";
        }
        let prefix = if t.kind == TabKind::Claude { "✦ " } else { "" };
        let name = format!("{prefix}{label}").into();
        (label, name, Some(badge))
    }

    /// Tab `i`'s whole name for the pane title, which cuts it to its own width.
    fn full_title(&self, i: usize, cx: &App) -> String {
        match &self.tabs[i] {
            CenterTab::Term(t) => t.rename.as_ref().map_or_else(|| t.view.read(cx).title().to_string(), |r| r.to_string()),
            _ => self.tab_info(i, cx).0,
        }
    }

    /// Whether tab `i` is the preview tab (shown in italics).
    fn is_preview(&self, i: usize, cx: &App) -> bool {
        self.preview_ix(cx) == Some(i)
    }

    /// Tab `i`'s icon, so every tab's label starts at the same place: a
    /// terminal's agent (else its kind's), a file's type, a diff's.
    fn tab_icon(&self, i: usize, mono: Option<Hsla>, cx: &App) -> Option<AnyElement> {
        let theme = cx.theme();
        let mono = mono.unwrap_or(theme.muted_foreground);
        let file = |path: Option<&Path>| {
            let name = path.and_then(Path::file_name).map(|n| n.to_string_lossy()).unwrap_or_default();
            let (icon, color) = file_icons::file_icon(&name, theme);
            Icon::new(icon).text_color(color).small().flex_shrink_0().into_any_element()
        };
        let t = match &self.tabs[i] {
            CenterTab::Term(t) => t,
            CenterTab::File { view, .. } => return Some(file(view.read(cx).path())),
            CenterTab::Scratch { view, .. } => return Some(file(Some(view.read(cx).path()))),
            CenterTab::Diff(_) => {
                return Some(Icon::new(AssetIcon::FileDiff).text_color(mono).small().flex_shrink_0().into_any_element());
            },
        };
        let agent = t.view.read(cx).agent().or((t.kind == TabKind::Claude).then_some(Agent::Claude));
        Some(match agent {
            Some(a) => a.icon(theme).into_any_element(),
            None => Icon::new(AssetIcon::SquareTerminal).text_color(mono).small().flex_shrink_0().into_any_element(),
        })
    }

    /// The tool strip button of `part`: its icon, whether its panel shows,
    /// its tooltip.
    fn part_button(&self, part: &str) -> Option<(&'static str, bool, String)> {
        let shortcut = crate::platform::shortcut;
        Some(match part {
            "explorer" => ("folder", self.show_explorer, format!("Explorer ({})", shortcut("⌘B"))),
            "scratches" => ("sticky-note", self.show_scratches, format!("Scratches ({})", shortcut("⌘⇧S"))),
            "changes" => ("git-branch", self.show_changes, format!("Changes ({})", shortcut("⌘⇧G"))),
            "shells" => ("square-terminal", self.show_shells, "Shells".to_owned()),
            "usage" => ("chart-pie", self.show_usage, "Usage".to_owned()),
            _ => return None,
        })
    }

    /// The buttons of the panels docked on the right, for the main window's
    /// title bar (template `title_bar`, list `parts`), with what each does.
    pub fn title_parts(&mut self, cx: &mut Context<Self>) -> (Vec<template::Scope>, Vec<TitleClick>) {
        let arrangement = self.arrangement(cx);
        let parts = self.strip_parts("right", arrangement.as_deref(), cx);
        let me = cx.entity().downgrade();
        parts
            .into_iter()
            .map(|(p, (icon, on, tooltip))| {
                let toggle = Self::strip_toggle(&p);
                let me = me.clone();
                let click: TitleClick = Rc::new(move |w, cx| {
                    let _ = me.update(cx, |this, cx| toggle(this, w, cx));
                });
                (template::Scope::new(format!("part-{p}")).var("icon", icon).var("on", on).var("tooltip", tooltip), click)
            })
            .unzip()
    }

    /// What the title bar says is wrong: with the background terminals
    /// (empty while they run in this build's daemon), with the settings
    /// (empty without a problem).
    pub fn title_status(&self, cx: &App) -> (String, String) {
        let background = match self.services.mux.server_version() {
            // An update's new UI keeps talking to the old daemon until
            // its terminals end.
            Some(v) if v != env!("CARGO_PKG_VERSION") => format!("Background: Tern {v} (updates once its terminals end)"),
            Some(_) if self.services.mux.server_same_build() == Some(false) => {
                "Background: an older build (updates once its terminals end)".to_owned()
            },
            Some(_) => String::new(),
            None => "Background: unavailable".to_owned(),
        };
        let mut settings_error: Vec<String> =
            cx.try_global::<crate::settings::SettingsStatus>().and_then(|s| s.error.as_ref().map(|e| e.to_string())).into_iter().collect();
        settings_error.extend(self.arrange_problems.iter().cloned());
        (background, settings_error.join("; "))
    }

    /// What the strip button of `part` does: toggle its panel.
    fn strip_toggle(part: &str) -> fn(&mut Self, &mut Window, &mut Context<Self>) {
        match part {
            "explorer" => |this, w, cx| this.toggle_explorer(&ToggleExplorer, w, cx),
            "scratches" => |this, w, cx| this.toggle_scratches(&ToggleScratches, w, cx),
            "shells" => |this, w, cx| this.toggle_shells(&ToggleShells, w, cx),
            "usage" => |this, w, cx| this.toggle_usage(&ToggleUsage, w, cx),
            _ => |this, w, cx| this.toggle_changes(&ToggleChanges, w, cx),
        }
    }

    /// The parts with a button on the `left` or `right` strip: those in the
    /// docks it shows, in their order (the built-in sides without an
    /// arrangement), with their buttons.
    fn strip_parts(&self, side: &str, arrangement: Option<&Arrangement>, cx: &App) -> Vec<(String, (&'static str, bool, String))> {
        let parts: Vec<String> = match (arrangement, cx.try_global::<template::Templates>()) {
            (Some(a), Some(t)) => t
                .docks("workspace")
                .iter()
                .filter(|d| t.dock_strip("workspace", &d.name).as_deref() == Some(side))
                .flat_map(|d| {
                    let docked = a.docks.get(&d.name).cloned().flatten();
                    let mut parts: Vec<String> = docked.iter().flat_map(|t| t.parts()).map(str::to_owned).collect();
                    // A floating part's button stays with the dock it came from, as does one in the center.
                    parts.extend(a.floating.iter().filter(|(_, f)| f.from == *d.name).map(|(p, _)| p.clone()));
                    parts.extend(a.center.iter().filter(|(_, from)| **from == *d.name).map(|(p, _)| p.clone()));
                    parts
                })
                .collect(),
            _ if side == "left" => vec!["explorer".into(), "scratches".into()],
            _ => vec!["changes".into(), "shells".into()],
        };
        parts.into_iter().filter_map(|p| self.part_button(&p).map(|b| (p, b))).collect()
    }

    /// The `left` tool strip, IntelliJ style (template `tool_strip.left`):
    /// a button per part in the docks it shows. Each toggles its panel, so a
    /// hidden panel is one click away. The right one's buttons sit in the
    /// main window's title bar (see `title_parts`).
    fn strip(&self, side: &str, arrangement: Option<&Arrangement>, cx: &mut Context<Self>) -> AnyElement {
        let parts = self.strip_parts(side, arrangement, cx);
        let items = parts
            .iter()
            .map(|(p, (icon, on, tooltip))| {
                template::Scope::new("strip").var("part", p.as_str()).var("icon", *icon).var("on", *on).var("tooltip", tooltip.as_str())
            })
            .collect();
        let name = if side == "left" { "tool_strip.left" } else { "tool_strip.right" };
        let scope = template::Scope::new(name).list("parts", items);
        template::render(
            name,
            scope,
            &mut |frame, ix, f| match (frame, ix.and_then(|i| parts.get(i)), f) {
                ("part", Some((part, _)), Framed::Button(b)) => {
                    let toggle = Self::strip_toggle(part);
                    b.on_click(cx.listener(move |this, _, w, cx| toggle(this, w, cx))).into_any_element()
                },
                (_, _, f) => f.into_any_element(),
            },
            cx,
        )
    }

    /// The values `pane.buttons` reads for pane `pane`, on `scope`.
    fn pane_vars(&self, pane: PaneId, scope: template::Scope) -> template::Scope {
        let shortcut = |k| crate::platform::shortcut(k).to_string();
        scope
            .var("panes", self.layout.panes().len())
            .var("zoomed", self.zoomed == Some(pane))
            // The pane layout menu sits on the top right pane.
            .var("corner", self.zoomed.unwrap_or_else(|| self.layout.top_right()) == pane)
            .var("split_key", shortcut("⌘\\"))
            .var("zoom_key", shortcut("⌘⇧↩"))
            .var("close_key", shortcut("⌘W"))
    }

    /// What pane `pane`'s buttons do (`pane.buttons`, `pane.new_tab`,
    /// `pane.empty`); `f` back when `name` is none of them.
    #[allow(clippy::result_large_err)] // `Framed` is gpui's builders, which move by value.
    fn pane_frame(ws: &WeakEntity<Self>, pane: PaneId, name: &str, f: Framed) -> Result<AnyElement, Framed> {
        type Act = fn(&mut Workspace, PaneId, &mut Window, &mut Context<Workspace>);
        let Framed::Button(b) = f else { return Err(f) };
        let on = |act: Act| on_event(ws, move |this, _: &ClickEvent, window, cx| act(this, pane, window, cx));
        Ok(match name {
            "split" => b.on_click(on(|this, pane, window, cx| {
                this.focus_pane_state(pane);
                let ix = this.pane_active_ix(pane).unwrap_or(this.active);
                this.split(ix, Edge::Right, window, cx);
            })),
            "zoom" => b.on_click(on(|this, pane, window, cx| {
                this.focus_pane(pane, window, cx);
                this.toggle_zoom(cx);
            })),
            "close_pane" => b.on_click(on(|this, pane, window, cx| this.close_pane(pane, window, cx))),
            "new_claude" => b.on_click(on(|this, pane, window, cx| {
                this.focus_pane_state(pane);
                this.add_tab(TabKind::Claude, window, cx);
            })),
            "new_shell" => b.on_click(on(|this, pane, window, cx| {
                this.focus_pane_state(pane);
                this.add_tab(TabKind::Shell, window, cx);
            })),
            "pane_layout" => {
                let ws = ws.clone();
                return Ok(b
                    .dropdown_menu(move |menu, window, cx| {
                        let Some(this) = ws.upgrade() else { return menu };
                        let scope = this.read(cx).layout_menus(template::Scope::new("pane-layout"));
                        let ws = ws.clone();
                        template::menu("menu.pane_layout", scope, menu, Rc::new(move |n, e| Self::layout_item(&ws, n, e)), window, cx)
                    })
                    .into_any_element());
            },
            "new_tab" => return Ok(Self::new_tab_menu(b, ws.clone()).into_any_element()),
            _ => return Err(Framed::Button(b)),
        }
        .into_any_element())
    }

    /// The new-tab button's menu on `b`.
    fn new_tab_menu(b: Button, ws: WeakEntity<Self>) -> impl IntoElement {
        b.dropdown_menu(move |menu, window, cx| {
            let Some(this) = ws.upgrade() else { return menu };
            let args = window_state::claude_args();
            let flags = CLAUDE_FLAGS
                .iter()
                .map(|(label, flag)| {
                    template::Scope::default().var("label", *label).var("flag", *flag).var("on", window_state::has_flag(&args, flag))
                })
                .collect();
            let files: Vec<PathBuf> = scratch::list(&scratch::dir()).into_iter().take(30).collect();
            let names = files
                .iter()
                .map(|p| {
                    template::Scope::default().var("name", p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
                })
                .collect();
            let scope = this
                .read(cx)
                .layout_menus(template::Scope::new("new-tab"))
                .list("flags", flags)
                .list("kinds", scratch::kind_entries())
                .var("scratch_count", files.len())
                .list("scratch_files", names);
            let ws = ws.clone();
            let items: template::MenuItems = Rc::new(move |name, entry| match name {
                // The menu of a folder that isn't the shown one: the action
                // would reach the shown workspace, so open the tab in this one.
                "workspace::NewClaudeTab" => on_ws_shown(&ws, |this, window, cx| this.add_tab(TabKind::Claude, window, cx)),
                "workspace::NewShellTab" => on_ws_shown(&ws, |this, window, cx| this.add_tab(TabKind::Shell, window, cx)),
                "workspace::NewScratch" => on_ws_shown(&ws, |this, window, cx| this.new_scratch("txt", window, cx)),
                "flag" => {
                    let (_, flag) = *CLAUDE_FLAGS.get(entry?)?;
                    Some(Rc::new(move |_, _, _| {
                        window_state::set_claude_args(&window_state::toggle_flag(&window_state::claude_args(), flag));
                    }))
                },
                "edit_args" => on_ws(&ws, |this, window, cx| this.edit_claude_args(window, cx)),
                "new" => {
                    let (_, ext) = *scratch::KINDS.get(entry?)?;
                    on_ws(&ws, move |this, window, cx| this.new_scratch(ext, window, cx))
                },
                "open_scratch" => {
                    let path = files.get(entry?)?.clone();
                    on_ws(&ws, move |this, window, cx| this.open_scratch(path.clone(), window, cx))
                },
                _ => Self::layout_item(&ws, name, entry),
            });
            template::menu("menu.new_tab", scope, menu, items, window, cx)
        })
    }

    /// The entries of a tab list for `slots`: what each is, and its values
    /// for the tab and group templates. `mono` gives a tab's icon color by
    /// whether it is the shown one.
    fn tab_entries(&self, slots: &[Slot], mono: impl Fn(bool) -> Option<Hsla>, cx: &App) -> (Vec<TabEntry>, Vec<template::Scope>) {
        let close_key = crate::platform::shortcut("⌘W").to_string();
        let mut entries = vec![];
        let mut scopes = vec![];
        for slot in slots {
            match *slot {
                Slot::Group(g) => {
                    let Some(group) = self.group(g) else { continue };
                    let tabs = self.group_tabs(g).map_or(0, |r| r.len());
                    let count = group.collapsed.then_some(tabs);
                    let name = group.name.clone();
                    let text = match count {
                        Some(n) if name.is_empty() => n.to_string(),
                        Some(n) => format!("{name}  {n}"),
                        None => name.to_string(),
                    };
                    let color = group_color(group.color);
                    scopes.push(
                        template::Scope::new(format!("group-{g}"))
                            .var("group", true)
                            .var("name", name.clone())
                            .var("text", text)
                            .var("collapsed", group.collapsed)
                            .var("open", !group.collapsed && tabs > 0)
                            .var("count", tabs)
                            .var("group_color", color),
                    );
                    entries.push(TabEntry { slot: *slot, name: name.clone(), label: group.title().into(), color, tab: None });
                },
                Slot::Tab(i) => {
                    let (label, name, badge) = self.tab_info(i, cx);
                    let active = self.pane_active_ix(self.pane_of(i)) == Some(i);
                    let group = self.group_of(i).and_then(|g| Some((self.group(g)?, self.group_tabs(g)?)));
                    let mut scope = template::Scope::new(format!("tab-{}", self.tabs[i].entity_id().as_u64()))
                        .var("label", label.clone())
                        .var("active", active)
                        .var("preview", self.is_preview(i, cx))
                        .var("grouped", group.is_some())
                        .var("close_key", close_key.clone())
                        .slot_opt("icon", self.tab_icon(i, mono(active), cx));
                    if let Some((b, color)) = badge.filter(|(b, _)| !b.is_empty()) {
                        scope = scope.var("badge", b.trim()).var("badge_color", color);
                    }
                    let mut color = Hsla::default();
                    if let Some((g, r)) = group {
                        color = group_color(g.color);
                        scope = scope.var("group_color", color).var("group_first", i == r.start).var("group_last", i + 1 == r.end);
                    }
                    scopes.push(scope);
                    entries.push(TabEntry { slot: *slot, name, label: label.into(), color, tab: Some(self.tabs[i].entity_id()) });
                },
            }
        }
        (entries, scopes)
    }

    /// What the boxes of a tab list do (`pane_tab_bar`, `vertical_tabs`):
    /// tabs and group chips, their close buttons, the space after them, the
    /// right-click menu on `root`; the pane's buttons. `f` back for others.
    #[allow(clippy::result_large_err)] // As `pane_frame`.
    fn tab_list_frame(
        ws: &WeakEntity<Self>,
        pane: PaneId,
        entries: &[TabEntry],
        handle: &Option<DraggedPart>,
        (name, ix, f): (&str, Option<usize>, Framed),
    ) -> Result<AnyElement, Framed> {
        let drop_bg = |cx: &App| cx.theme().list_hover;
        let entry = ix.and_then(|ix| entries.get(ix));
        Ok(match (name, f, entry) {
            ("root", Framed::Box(d), _) => Self::tab_menu_frame(d, pane, ws).into_any_element(),
            ("end", Framed::Box(d), _) => {
                let d = d.id(("tabs-end", pane as usize));
                // Its empty space drags the whole pane.
                match handle.clone() {
                    Some(p) => d.on_drag(p, crate::dock::start_drag),
                    None => d,
                }
            }
            .drag_over::<DraggedTab>(move |s, _, _, cx| s.bg(drop_bg(cx)))
            .drag_over::<DraggedGroup>(move |s, _, _, cx| s.bg(drop_bg(cx)))
            .on_drop(on_event(ws, move |this, d: &DraggedTab, window, cx| this.move_tab_to_pane(d.tab, pane, window, cx)))
            .on_drop(on_event(ws, move |this, d: &DraggedGroup, _, cx| {
                this.move_group(d.group, this.tabs.len(), cx);
                this.set_group_pane(d.group, pane);
                this.groups_changed(cx);
            }))
            .into_any_element(),
            ("tab", Framed::Box(d), Some(&TabEntry { slot: Slot::Tab(i), ref name, tab: Some(tab), .. })) => {
                let drag = DraggedTab { tab, name: name.clone() };
                d.id(("tab", i))
                    .on_click(on_event(ws, move |this, ev: &ClickEvent, window, cx| {
                        if ev.click_count() > 1 {
                            this.pin_tab(i, cx);
                        }
                        this.activate(i, window, cx);
                    }))
                    .on_mouse_down(MouseButton::Right, on_event(ws, move |this, _: &MouseDownEvent, _, _| this.menu_tab = Some(i)))
                    .on_drag(drag, Self::drag_start(ws.clone()))
                    .drag_over::<DraggedTab>(move |s, _, _, cx| s.bg(drop_bg(cx)))
                    .drag_over::<DraggedGroup>(move |s, _, _, cx| s.bg(drop_bg(cx)))
                    .on_drop(on_event(ws, move |this, d: &DraggedTab, window, cx| {
                        let join = this.group_of(i);
                        this.move_tab(d.tab, i, join, window, cx);
                    }))
                    .on_drop(on_event(ws, move |this, d: &DraggedGroup, _, cx| this.move_group(d.group, i, cx)))
                    .into_any_element()
            },
            ("chip" | "group", Framed::Box(d), Some(&TabEntry { slot: Slot::Group(g), ref name, color, .. })) => {
                let drag = DraggedGroup { group: g, name: name.clone(), color };
                d.id(("group", g as usize))
                    .on_click(on_event(ws, move |this, _: &ClickEvent, window, cx| this.toggle_group(g, window, cx)))
                    .on_mouse_down(MouseButton::Right, on_event(ws, move |this, _: &MouseDownEvent, _, _| this.menu_group = Some(g)))
                    .on_drag(drag, Self::drag_start(ws.clone()))
                    .drag_over::<DraggedTab>(move |s, _, _, cx| s.bg(drop_bg(cx)))
                    .drag_over::<DraggedGroup>(move |s, _, _, cx| s.bg(drop_bg(cx)))
                    .on_drop(on_event(ws, move |this, d: &DraggedTab, window, cx| this.move_tab_to_group(d.tab, g, window, cx)))
                    .on_drop(on_event(ws, move |this, d: &DraggedGroup, _, cx| {
                        if let Some(r) = this.group_tabs(g) {
                            this.move_group(d.group, r.start, cx);
                        }
                    }))
                    .into_any_element()
            },
            ("close", Framed::Button(b), Some(&TabEntry { slot: Slot::Tab(i), .. })) => b
                .on_click(on_event(ws, move |this, _: &ClickEvent, window, cx| {
                    // Don't let the tab's own click activate it.
                    cx.stop_propagation();
                    this.request_close_tab(i, window, cx);
                }))
                .into_any_element(),
            (name, f, _) => return Self::pane_frame(ws, pane, name, f),
        })
    }

    /// `d` with the tab bar's right-click menu: the tab or group menu, picked
    /// by what was clicked; the tab layouts on empty space.
    fn tab_menu_frame(d: Div, pane: PaneId, ws: &WeakEntity<Self>) -> impl IntoElement {
        let menu_ws = ws.clone();
        d.id(("tab-menu", pane as usize))
            // Before the tabs' own handlers pick what the menu is for.
            .capture_any_mouse_down(on_event(ws, |this, ev: &MouseDownEvent, _, _| {
                if ev.button == MouseButton::Right {
                    this.menu_tab = None;
                    this.menu_group = None;
                }
            }))
            .context_menu(move |menu, window, cx| {
                let ws = &menu_ws;
                let Some(this) = ws.upgrade() else { return menu };
                let this = this.read(cx);
                let (name, scope, items) = if let Some(g) = this.menu_group.filter(|&g| this.group(g).is_some()) {
                    Self::group_menu(ws, g, this)
                } else if let Some(ix) = this.menu_tab.filter(|&ix| ix < this.tabs.len()) {
                    Self::tab_menu(ws, ix, this, cx)
                } else {
                    let ws = ws.clone();
                    let items: template::MenuItems = Rc::new(move |n, e| Self::layout_item(&ws, n, e));
                    ("menu.tab_bar", this.layout_menus(template::Scope::new("tab-bar")), items)
                };
                template::menu(name, scope, menu, items, window, cx)
            })
    }

    /// Pane `pane`'s horizontal tab bar (`pane_tab_bar`): one scrolling row,
    /// or rows that wrap.
    fn render_tab_bar(&self, pane: PaneId, cx: &mut Context<Self>) -> AnyElement {
        let slots = self.slots_in(pane);
        let shown = self.pane_active_ix(pane);
        let active_slot = slots.iter().position(|s| Some(*s) == shown.map(Slot::Tab)).unwrap_or(0);
        let scroll = self.ui(pane, |u| u.tab_scroll.clone());
        let wrap = window_state::tab_layout() == TabLayout::Wrap;
        let mono = |active| {
            let look = style::el(if active { "pane_tab_bar.tab_active" } else { "pane_tab_bar.tab" }, cx);
            look.is_set("icon_text").then(|| look.color("icon_text"))
        };
        let (entries, items) = self.tab_entries(&slots, mono, cx);
        if wrap {
            // Reveal the active tab again once tabs scroll.
            self.ui(pane, |u| u.revealed = None);
        } else if self.ui(pane, |u| u.revealed.replace(active_slot)) != Some(active_slot) {
            scroll.scroll_to_item(active_slot);
        }
        let overflows = !wrap && scroll.max_offset().x > px(0.);
        // A thin bar along the strip's bottom edge while hovered, like VS Code's.
        let scrollbar = overflows.then(|| {
            Scrollbar::horizontal(&scroll).id(("tab-scrollbar", pane as usize)).mode(ScrollbarMode::Hover).styles(|s| {
                s.track(|t| t.width(px(4.)))
                    .track_hover(|t| t.width(px(4.)))
                    .track_active(|t| t.width(px(4.)))
                    .thumb(|t| t.width(px(3.)).inset(px(0.)))
                    .thumb_hover(|t| t.width(px(4.)).inset(px(0.)))
                    .thumb_active(|t| t.width(px(4.)).inset(px(0.)))
            })
        });
        let scope = self
            .pane_vars(pane, template::Scope::new(format!("tab-bar-{pane}")))
            .var("wrap", wrap)
            .var("overflows", overflows)
            .list("items", items)
            .slot_opt("scrollbar", scrollbar);
        let ws = cx.entity().downgrade();
        let handle = self.pane_handle(pane, cx);
        template::render(
            "pane_tab_bar",
            scope,
            &mut |name, ix, f| match (name, f) {
                ("bar", Framed::Box(d)) => Self::tab_menu_frame(d, pane, &ws).into_any_element(),
                ("tabs", Framed::Box(d)) => {
                    // The strip only takes sideways scrolls; let a mouse wheel move it too.
                    let wheel = scroll.clone();
                    d.id(("term-tabs", pane as usize))
                        .overflow_x_scroll()
                        .track_scroll(&scroll)
                        .on_scroll_wheel(on_event(&ws, move |_, ev: &ScrollWheelEvent, window, cx| {
                            let d = ev.delta.pixel_delta(window.line_height());
                            if d.y.abs() > d.x.abs() {
                                let max = wheel.max_offset().x;
                                let mut offset = wheel.offset();
                                offset.x = (offset.x + d.y).min(px(0.)).max(-max);
                                wheel.set_offset(offset);
                                cx.notify();
                            }
                        }))
                        .into_any_element()
                },
                // Lists every tab once they don't all fit.
                ("more", Framed::Button(b)) => {
                    let ws = ws.clone();
                    let menu: Vec<(Slot, SharedString)> = entries.iter().map(|e| (e.slot, e.label.clone())).collect();
                    b.dropdown_caret(true)
                        .dropdown_menu(move |m, window, cx| {
                            let tabs = menu
                                .iter()
                                .map(|(slot, label)| {
                                    template::Scope::default().var("label", label.clone()).var("on", shown.map(Slot::Tab) == Some(*slot))
                                })
                                .collect();
                            let slots: Vec<Slot> = menu.iter().map(|(slot, _)| *slot).collect();
                            let ws = ws.clone();
                            let items: template::MenuItems = Rc::new(move |name, entry| {
                                let slot = *slots.get(entry?).filter(|_| name == "show")?;
                                on_ws(&ws, move |this, window, cx| match slot {
                                    Slot::Group(g) => this.toggle_group(g, window, cx),
                                    Slot::Tab(i) => this.activate(i, window, cx),
                                })
                            });
                            let scope = template::Scope::new("tabs").list("tabs", tabs);
                            template::menu("menu.tabs", scope, m.scrollable(true), items, window, cx)
                        })
                        .anchor(Anchor::TopRight)
                        .into_any_element()
                },
                (name, f) => Self::tab_list_frame(&ws, pane, &entries, &handle, (name, ix, f)).unwrap_or_else(Framed::into_any_element),
            },
            cx,
        )
    }

    /// Pane `pane`'s tabs as a list down its left (`vertical_tabs`), like
    /// Edge's or Arc's vertical tabs.
    fn render_vertical_tabs(&self, pane: PaneId, cx: &mut Context<Self>) -> AnyElement {
        let slots = self.slots_in(pane);
        let shown = self.pane_active_ix(pane);
        let active_slot = slots.iter().position(|s| Some(*s) == shown.map(Slot::Tab)).unwrap_or(0);
        let scroll = self.ui(pane, |u| u.vtab_scroll.clone());
        if self.ui(pane, |u| u.revealed.replace(active_slot)) != Some(active_slot) {
            scroll.scroll_to_item(active_slot);
        }
        let (entries, items) = self.tab_entries(&slots, |_| None, cx);
        let scope = self.pane_vars(pane, template::Scope::new(format!("vertical-tabs-{pane}"))).list("items", items);
        let ws = cx.entity().downgrade();
        let handle = self.pane_handle(pane, cx);
        template::render(
            "vertical_tabs",
            scope,
            &mut |name, ix, f| match (name, f) {
                ("list", Framed::Box(d)) => d.id(("vtabs", pane as usize)).overflow_y_scroll().track_scroll(&scroll).into_any_element(),
                (name, f) => Self::tab_list_frame(&ws, pane, &entries, &handle, (name, ix, f)).unwrap_or_else(Framed::into_any_element),
            },
            cx,
        )
    }

    /// Right-click menu of tab `ix`: the template, its values, what its items do.
    fn tab_menu(ws: &WeakEntity<Self>, ix: usize, this: &Self, cx: &App) -> (&'static str, template::Scope, template::MenuItems) {
        let is_file = |t: &CenterTab| !matches!(t, CenterTab::Term(_));
        let tab = &this.tabs[ix];
        let path = tab.file_path(cx);
        let siblings = this.pane_tabs(this.pane_of(ix));
        let own = this.group_of(ix);
        let others: Vec<&TabGroup> = this.groups.iter().filter(|g| Some(g.id) != own).collect();
        let ids: Vec<u64> = others.iter().map(|g| g.id).collect();
        let groups = others.iter().map(|g| template::Scope::default().var("title", g.title()).var("color", group_color(g.color))).collect();
        let scope = template::Scope::new("tab-menu")
            .var("others", siblings.len() >= 2)
            .var("right", siblings.last().is_some_and(|&l| l > ix))
            // Terminals stay, so this never empties the window.
            .var("file_tabs", !this.tabs.iter().all(is_file) && this.tabs.iter().any(is_file))
            .var("grouped", own.is_some())
            .var("renamable", matches!(tab, CenterTab::Term(_) | CenterTab::Scratch { .. }))
            .var("diff_file", matches!(tab, CenterTab::Diff(_)) && path.as_ref().is_some_and(|p| p.is_file()))
            .var("has_path", path.is_some())
            .var("reveal", crate::platform::REVEAL)
            .list("groups", groups)
            .list("layouts", tab_layout_entries());
        let ws = ws.clone();
        let items: template::MenuItems = Rc::new(move |name, entry| {
            type Handler = fn(&mut Workspace, usize, &mut Window, &mut Context<Workspace>);
            let f: Handler = match name {
                "close" => Self::close_tab,
                "close_others" => Self::close_others,
                "close_right" => Self::close_to_right,
                "close_file_tabs" => Self::close_file_tabs,
                "new_group" => Self::add_to_new_group,
                "add_to_group" => {
                    let id = *ids.get(entry?)?;
                    return on_ws(&ws, move |this, _, cx| this.add_to_group(ix, id, cx));
                },
                "remove_from_group" => Self::remove_from_group,
                "rename" => Self::rename_tab,
                "open_file" => Self::open_tab_file,
                "copy_path" => Self::copy_tab_path,
                "reveal" => Self::reveal_tab_file,
                "split_right" => |this, ix, w, cx| this.split(ix, Edge::Right, w, cx),
                "split_left" => |this, ix, w, cx| this.split(ix, Edge::Left, w, cx),
                "split_down" => |this, ix, w, cx| this.split(ix, Edge::Bottom, w, cx),
                "split_up" => |this, ix, w, cx| this.split(ix, Edge::Top, w, cx),
                _ => return Self::layout_item(&ws, name, entry),
            };
            on_ws(&ws, move |this, window, cx| f(this, ix, window, cx))
        });
        ("menu.tab", scope, items)
    }

    /// Right-click menu of group `g`'s chip: the template, its values, what its items do.
    fn group_menu(ws: &WeakEntity<Self>, g: u64, this: &Self) -> (&'static str, template::Scope, template::MenuItems) {
        let (collapsed, color) = this.group(g).map_or((false, 0), |group| (group.collapsed, group.color));
        let colors = GROUP_COLORS
            .iter()
            .enumerate()
            .map(|(ix, (_, label, _))| template::Scope::default().var("label", *label).var("color", group_color(ix)).var("on", color == ix))
            .collect();
        let scope = template::Scope::new("group-menu")
            .var("collapsed", collapsed)
            .var("all", this.group_tabs(g).is_some_and(|r| r.len() == this.tabs.len()))
            .list("colors", colors);
        let ws = ws.clone();
        let items: template::MenuItems = Rc::new(move |name, entry| {
            type Handler = fn(&mut Workspace, u64, &mut Window, &mut Context<Workspace>);
            let f: Handler = match name {
                "rename" => Self::rename_group,
                "new_claude" => |this, g, w, cx| this.new_tab_in_group(TabKind::Claude, g, w, cx),
                "new_shell" => |this, g, w, cx| this.new_tab_in_group(TabKind::Shell, g, w, cx),
                "toggle" => Self::toggle_group,
                "color" => {
                    let ix = entry?;
                    return on_ws(&ws, move |this, _, cx| this.set_group_color(g, ix, cx));
                },
                "ungroup" => Self::ungroup,
                "close" => Self::close_group,
                _ => return None,
            };
            on_ws(&ws, move |this, window, cx| f(this, g, window, cx))
        });
        ("menu.tab_group", scope, items)
    }

    /// Close every tab `close` picks, last first, asking first when that
    /// ends running programs. Refuses to close them all.
    fn close_where(&mut self, close: impl Fn(usize, &CenterTab) -> bool, window: &mut Window, cx: &mut Context<Self>) {
        let doomed: HashSet<EntityId> = self.tabs.iter().enumerate().filter(|(i, t)| close(*i, t)).map(|(_, t)| t.entity_id()).collect();
        if doomed.len() == self.tabs.len() {
            return;
        }
        let jobs: Vec<String> =
            (0..self.tabs.len()).filter(|&i| doomed.contains(&self.tabs[i].entity_id())).filter_map(|i| self.running_in(i, cx)).collect();
        let close_now = move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
            while let Some(ix) = this.tabs.iter().rposition(|t| doomed.contains(&t.entity_id())) {
                this.close_tab(ix, window, cx);
            }
        };
        if jobs.is_empty() {
            close_now(self, window, cx);
        } else {
            confirm_close(jobs, "these tabs", window, cx, close_now);
        }
    }

    fn close_others(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(keep) = self.tabs.get(ix).map(CenterTab::entity_id) else { return };
        let pane = self.pane_of(ix);
        let doomed: HashSet<EntityId> =
            self.pane_tabs(pane).into_iter().map(|i| self.tabs[i].entity_id()).filter(|id| *id != keep).collect();
        self.close_where(|_, t| doomed.contains(&t.entity_id()), window, cx);
        if let Some(ix) = self.tabs.iter().position(|t| t.entity_id() == keep) {
            self.activate(ix, window, cx);
        }
    }

    fn close_to_right(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.pane_of(ix);
        let doomed: HashSet<EntityId> = self.pane_tabs(pane).into_iter().filter(|&i| i > ix).map(|i| self.tabs[i].entity_id()).collect();
        self.close_where(|_, t| doomed.contains(&t.entity_id()), window, cx);
    }

    fn close_file_tabs(&mut self, _: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.close_where(|_, t| !matches!(t, CenterTab::Term(_)), window, cx);
    }

    fn open_tab_file(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.tabs.get(ix).and_then(|t| t.file_path(cx)) {
            self.open_file(path, true, window, cx);
        }
    }

    fn copy_tab_path(&mut self, ix: usize, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.tabs.get(ix).and_then(|t| t.file_path(cx)) {
            cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()));
        }
    }

    fn reveal_tab_file(&mut self, ix: usize, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.tabs.get(ix).and_then(|t| t.file_path(cx)) {
            cx.reveal_path(&path);
        }
    }

    /// Ask for a new name: a terminal tab's title, or a scratch file's name.
    fn rename_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let current: SharedString = match self.tabs.get(ix) {
            Some(CenterTab::Term(t)) => t.rename.clone().unwrap_or_else(|| t.view.read(cx).title().to_string().into()),
            Some(CenterTab::Scratch { view, .. }) => view.read(cx).title().clone(),
            _ => return,
        };
        let id = self.tabs[ix].entity_id();
        self.ask_name("Rename", current, Rc::new(move |this, name, window, cx| this.apply_rename(id, name, window, cx)), window, cx);
    }

    /// Ask for group `g`'s name.
    fn rename_group(&mut self, g: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(current) = self.group(g).map(|g| g.name.clone()) else { return };
        let apply = move |this: &mut Self, name: &str, window: &mut Window, cx: &mut Context<Self>| {
            this.set_group_name(g, name, cx);
            this._rename = None;
            window.close_dialog(cx);
            this.focus_active(window, cx);
        };
        self.ask_name("Name Group", current, Rc::new(apply), window, cx);
    }

    /// A dialog titled `title` asking for a name, starting from `current`;
    /// `apply` takes what was typed (and closes the dialog when it is good).
    fn ask_name(&mut self, title: &'static str, current: SharedString, apply: ApplyName, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(current));
        let on_enter = apply.clone();
        self._rename = Some(cx.subscribe_in(&input, window, move |this, input, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                let name = input.read(cx).value().to_string();
                on_enter(this, &name, window, cx);
            }
        }));
        let ws = cx.entity().downgrade();
        let first = std::cell::Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                input.focus_handle(cx).focus(window, cx);
            }
            let field = input.clone();
            let footer = template::render(
                "dialog.name.footer",
                template::Scope::new("name"),
                &mut |n, _, f| match (n, f) {
                    ("cancel", Framed::Button(b)) => b.on_click(|_, window, cx| window.close_dialog(cx)).into_any_element(),
                    ("ok", Framed::Button(b)) => {
                        let (input, ws, apply) = (input.clone(), ws.clone(), apply.clone());
                        b.on_click(move |_, window, cx| {
                            let name = input.read(cx).value().to_string();
                            let _ = ws.update(cx, |this, cx| apply(this, &name, window, cx));
                        })
                        .into_any_element()
                    },
                    (_, f) => f.into_any_element(),
                },
                cx,
            );
            dialog
                .w(style::dialog_width(false, cx))
                .title(title)
                .content(move |c, _, cx| {
                    let scope = template::Scope::new("name").slot("field", Input::new(&field));
                    c.child(template::render_plain("dialog.name", scope, cx))
                })
                .footer(footer)
        });
    }

    /// Ask for the arguments every new `claude` gets.
    fn edit_claude_args(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            InputState::new(window, cx).default_value(window_state::claude_args()).placeholder("--dangerously-skip-permissions --chrome")
        });
        self._rename = Some(cx.subscribe_in(&input, window, |this, input, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                window_state::set_claude_args(&input.read(cx).value());
                this._rename = None;
                window.close_dialog(cx);
                this.focus_active(window, cx);
            }
        }));
        let first = std::cell::Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                input.focus_handle(cx).focus(window, cx);
            }
            let field = input.clone();
            let footer = template::render(
                "dialog.claude_args.footer",
                template::Scope::new("claude-args"),
                &mut |n, _, f| match (n, f) {
                    ("cancel", Framed::Button(b)) => b.on_click(|_, window, cx| window.close_dialog(cx)).into_any_element(),
                    ("ok", Framed::Button(b)) => {
                        let input = input.clone();
                        b.on_click(move |_, window, cx| {
                            window_state::set_claude_args(&input.read(cx).value());
                            window.close_dialog(cx);
                        })
                        .into_any_element()
                    },
                    (_, f) => f.into_any_element(),
                },
                cx,
            );
            dialog
                .w(style::dialog_width(true, cx))
                .title("Claude Arguments")
                .content(move |c, _, cx| {
                    let scope = template::Scope::new("claude-args").slot("field", Input::new(&field));
                    c.child(template::render_plain("dialog.claude_args", scope, cx))
                })
                .footer(footer)
        });
    }

    fn apply_rename(&mut self, id: EntityId, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let result = match self.tabs.iter_mut().find(|t| t.entity_id() == id) {
            Some(CenterTab::Term(t)) => {
                let name = name.trim();
                t.rename = (!name.is_empty()).then(|| name.to_owned().into());
                Ok(())
            },
            Some(CenterTab::Scratch { view, .. }) => view.update(cx, |s, cx| s.rename(name, cx)),
            _ => Ok(()),
        };
        match result {
            Ok(()) => {
                self._rename = None;
                window.close_dialog(cx);
                self.focus_active(window, cx);
                cx.emit(WorkspaceEvent::TabsChanged);
            },
            Err(e) => window.push_notification(e, cx),
        }
        cx.notify();
    }

    /// The arrangements to keep, by key: those the user changed or a session
    /// brought. Defaults aren't kept, so start sizes from the settings apply.
    fn saved_arrangements(&self) -> Vec<(String, String)> {
        self.arrangements.saved()
    }

    /// The arrangement of the `workspace` template's docks, fitted to the
    /// templates once per change of settings; None when it has no docks.
    fn arrangement(&mut self, cx: &App) -> Option<Arc<Arrangement>> {
        let templates = cx.try_global::<template::Templates>()?;
        let docks = templates.docks("workspace");
        if docks.is_empty() {
            return None;
        }
        let key = arrange::key("workspace", &docks);
        let now = Some((templates.generation(), key.clone()));
        if self.arranged_for != now {
            self.arranged_for = now;
            self.arranged = None;
            let parts = templates.parts();
            let parts: Vec<(&str, &str)> = parts.iter().map(|(n, k)| (*n, k.as_str())).collect();
            let center = templates.pane_dock("workspace").parts;
            self.arrange_problems = self.arrangements.fit(&key, &docks, &parts, &center);
            self.docks_epoch += 1;
        }
        if self.arranged.is_none() {
            self.arranged = self.arrangements.get(&key).cloned().map(Arc::new);
        }
        self.arranged.clone()
    }

    /// The user moved or resized a part: keep it in the arrangement.
    fn arrangement_changed(&mut self, change: Change, cx: &mut Context<Self>) {
        let Some((_, key)) = &self.arranged_for else { return };
        let key = key.clone();
        let Some(a) = self.arrangements.get_mut(&key) else { return };
        self.arranged = None;
        match change {
            Change::Move { part, kind, dock, at } => {
                let docks = cx.try_global::<template::Templates>().map(|t| t.docks("workspace")).unwrap_or_default();
                if !a.move_part(&part, &kind, &dock, at, &docks) {
                    return;
                }
                // The splits' sizes were the old parts'.
                self.docks_epoch += 1;
                // A part moved by hand is one the user wants to see.
                match &*part {
                    "explorer" => self.show_explorer = true,
                    "scratches" => self.show_scratches = true,
                    "changes" => self.show_changes = true,
                    "shells" => self.show_shells = true,
                    "usage" => self.show_usage = true,
                    _ => {},
                }
            },
            Change::Resize { dock, path, sizes } => a.set_sizes(&dock, &path, &sizes),
            Change::Activate { dock, path, part } => {
                if !a.set_active(&dock, &path, &part) {
                    return;
                }
            },
            Change::Width { dock, width } => {
                a.widths.insert(dock.to_string(), width);
            },
            Change::Float { part, rect, area } => {
                let docked = !a.floating.contains_key(&*part);
                if !a.float_part(&part, rect, area) {
                    return;
                }
                if docked {
                    self.docks_epoch += 1;
                }
            },
            Change::Window { part, display, rect } => {
                let docked = !a.floating.contains_key(&*part);
                if !a.pop_out(&part, &display, rect) {
                    return;
                }
                if docked {
                    self.docks_epoch += 1;
                }
            },
            Change::Dock { part, kind } => {
                let docks = cx.try_global::<template::Templates>().map(|t| t.docks("workspace")).unwrap_or_default();
                if !a.dock_part(&part, &kind, &docks) {
                    return;
                }
                self.docks_epoch += 1;
            },
            Change::Center { part, target, edge } => {
                let leaf = Layout::Part(part.to_string());
                if target == leaf || !self.layout.has(&target) {
                    return;
                }
                if a.center_part(&part) {
                    self.docks_epoch += 1;
                }
                self.layout.remove_part(&part);
                self.layout.split_leaf(&target, leaf, edge);
                self.layout_epoch += 1;
                self.zoomed = None;
                match &*part {
                    "explorer" => self.show_explorer = true,
                    "scratches" => self.show_scratches = true,
                    "changes" => self.show_changes = true,
                    "shells" => self.show_shells = true,
                    "usage" => self.show_usage = true,
                    _ => {},
                }
            },
        }
        self.arrangements.touch(&key);
        self.panels_changed(cx);
    }

    /// Floats `part` from its header's menu, under the pointer.
    fn float_part(&mut self, a: &crate::dock::FloatPart, window: &mut Window, cx: &mut Context<Self>) {
        let Some(area) = self.float_area else { return };
        if cx.try_global::<crate::dock::LayoutLock>().is_some_and(|l| l.0) {
            return;
        }
        let rect = crate::dock::float_rect(window.mouse_position(), point(px(40.), px(12.)), area, cx);
        let change = Change::Float { part: a.0.clone(), rect, area: crate::dock::area_size(area) };
        self.arrangement_changed(change, cx);
    }

    /// Docks floating `part` again, from its header's menu.
    fn dock_part(&mut self, a: &crate::dock::DockPart, _: &mut Window, cx: &mut Context<Self>) {
        self.dock_named(&a.0, cx);
    }

    /// Docks `part` again, floating or in a window of its own.
    pub fn dock_named(&mut self, part: &str, cx: &mut Context<Self>) {
        let kind = cx.try_global::<template::Templates>().and_then(|t| t.parts().into_iter().find(|(n, _)| *n == part).map(|(_, k)| k));
        let Some(kind) = kind else { return };
        self.arrangement_changed(Change::Dock { part: part.to_owned().into(), kind: kind.into() }, cx);
    }

    /// Moves `part` to a window of its own, from its header's menu: its size
    /// as it floats (else the `floating` element's), its top-left near the
    /// pointer.
    fn pop_out(&mut self, a: &crate::dock::PopOutPart, window: &mut Window, cx: &mut Context<Self>) {
        self.pop_out_near(&a.0, window.mouse_position(), window, cx);
    }

    /// Moves `part` to a window of its own with its top-left near `at`
    /// (window coordinates, maybe outside it).
    fn pop_out_near(&mut self, part: &str, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let frame = window.bounds();
        let chrome = frame.size.height - window.viewport_size().height;
        let at = frame.origin + point(px(0.), chrome) + at - point(px(40.), px(12.));
        let display = window.display(cx).and_then(|d| d.uuid().ok()).map(|u| u.to_string()).unwrap_or_default();
        self.pop_out_at(part, display, (f32::from(at.x), f32::from(at.y)), None, cx);
    }

    /// A drag let go outside the window: a part dragged there moves to a
    /// window of its own where it was let go.
    fn dropped_outside(&mut self, ev: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        let size = window.viewport_size();
        let p = ev.position;
        if p.x >= px(0.) && p.y >= px(0.) && p.x < size.width && p.y < size.height {
            return;
        }
        let Some(d) = cx.try_global::<ActiveDrag>().and_then(|d| d.0.clone()) else { return };
        if d.kind == "pane" || d.popped {
            return;
        }
        self.pop_out_near(&d.part, p, window, cx);
    }

    /// Moves `part` to a window of its own with its top-left at `at` on
    /// display `display`, `size` big (else as it floats, or the `floating`
    /// element's size).
    fn pop_out_at(&mut self, part: &str, display: String, at: (f32, f32), size: Option<(f32, f32)>, cx: &mut Context<Self>) {
        let locked = cx.try_global::<crate::dock::LayoutLock>().is_some_and(|l| l.0);
        let windowless = cx.try_global::<crate::dock::Floats>().is_some_and(|f| f.windowless.contains(part));
        if locked || windowless {
            return;
        }
        let float = self.arranged.as_ref().and_then(|a| a.floating.get(part));
        // Its window moves with the system's title bar, which says where it is.
        if float.is_some_and(|f| f.window.is_some()) {
            return;
        }
        let floating = float.map(|f| (f.w, f.h));
        let look = style::el("floating", cx);
        let (w, h) = size.or(floating).unwrap_or((f32::from(look.px("width")), f32::from(look.px("height"))));
        let rect = arrange::Rect { x: at.0, y: at.1, w, h };
        self.arrangement_changed(Change::Window { part: part.to_owned().into(), display, rect }, cx);
    }

    /// Part `part`'s own window moved or resized: keep where it is.
    pub fn part_window_moved(&mut self, part: &str, display: &str, rect: arrange::Rect, cx: &mut Context<Self>) {
        let popped = self.arranged.as_ref().and_then(|a| a.floating.get(part)).is_some_and(|f| f.window.is_some());
        if popped {
            self.arrangement_changed(Change::Window { part: part.to_owned().into(), display: display.to_owned(), rect }, cx);
        }
    }

    /// Floats `part`, in a window of its own, over the docks again: in the
    /// middle of the workspace.
    pub fn float_from_window(&mut self, part: &str, cx: &mut Context<Self>) {
        let Some(area) = self.float_area else { return };
        if cx.try_global::<crate::dock::LayoutLock>().is_some_and(|l| l.0) {
            return;
        }
        let (aw, ah) = crate::dock::area_size(area);
        let look = style::el("floating", cx);
        let (w, h) = (f32::from(look.px("width")).min(aw), f32::from(look.px("height")).min(ah));
        let rect = arrange::Rect { x: (aw - w) / 2., y: (ah - h) / 2., w, h };
        self.arrangement_changed(Change::Float { part: part.to_owned().into(), rect, area: (aw, ah) }, cx);
    }

    /// Opens a window for each part the arrangement puts in one and that
    /// shows, and closes the others.
    fn sync_part_windows(&mut self, arrangement: Option<&Arrangement>, cx: &mut Context<Self>) {
        let wanted: Vec<(String, arrange::Float)> = arrangement
            .iter()
            .flat_map(|a| a.floating.iter())
            .filter(|(p, f)| f.window.is_some() && self.part_shown(p))
            .map(|(p, f)| (p.clone(), f.clone()))
            .collect();
        let open = cx.windows();
        // Closed from outside (its close button docks it; this is anything else).
        self.part_windows.retain(|_, h| h.is_none_or(|h| open.contains(&h)));
        let stale: Vec<String> = self.part_windows.keys().filter(|p| !wanted.iter().any(|(w, _)| w == *p)).cloned().collect();
        for p in stale {
            if let Some(Some(h)) = self.part_windows.remove(&p) {
                crate::part_window::close(h, cx);
            }
        }
        for (part, f) in wanted {
            if self.part_windows.contains_key(&part) {
                continue;
            }
            let Some(r) = f.window_rect() else { continue };
            self.part_windows.insert(part.clone(), None);
            let bounds = Bounds::new(point(px(r.x), px(r.y)), size(px(r.w), px(r.h)));
            let display = f.window.clone().unwrap_or_default();
            let ws = cx.entity();
            cx.defer(move |cx| {
                let (display, bounds) = match window_state::resolve(&display, WindowBounds::Windowed(bounds), cx) {
                    Some((d, _)) => (Some(d), bounds),
                    // Its display is gone, or it would be off it.
                    None => (None, Bounds::centered(None, bounds.size, cx)),
                };
                let handle = crate::part_window::open(ws.clone(), part.clone().into(), display, bounds, cx);
                ws.update(cx, |this, cx| {
                    if let Some(slot) = this.part_windows.get_mut(&part) {
                        *slot = handle;
                    } else if let Some(h) = handle {
                        // No longer wanted meanwhile.
                        crate::part_window::close(h, cx);
                    }
                });
            });
        }
    }

    /// Whether `part` shows (its strip button is on).
    fn part_shown(&self, part: &str) -> bool {
        match part {
            "explorer" => self.show_explorer,
            "scratches" => self.show_scratches,
            "changes" => self.show_changes,
            "shells" => self.show_shells,
            "usage" => self.show_usage,
            _ => true,
        }
    }

    /// The values and slots the parts are drawn with, here or in windows of their own.
    pub fn part_scope(&self) -> template::Scope {
        template::Scope::new(format!("workspace-{}", self.docks_epoch))
            .var("explorer", self.show_explorer)
            .var("scratches", self.show_scratches)
            .var("changes", self.show_changes)
            .var("shells", self.show_shells)
            .var("usage", self.show_usage)
            .var("path", self.root.display().to_string())
            .slot("explorer", self.tree.clone().cached(cache_layout()))
            .slot("scratches", self.scratches.clone().cached(cache_layout()))
            .slot("changes", self.changes.clone().cached(cache_layout()))
            .slot("shells", self.shells.clone().cached(cache_layout()))
            .slot("usage", self.usage.clone().cached(cache_layout()))
            .slot_opt("agents", self.agents.clone())
    }

    /// The agents panel to draw: its window's.
    pub fn set_agents(&mut self, agents: AnyView) {
        self.agents = Some(agents);
    }

    /// Puts the side panels back where the templates place them.
    fn reset_layout(&mut self, _: &ResetLayout, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, key)) = self.arranged_for.take() {
            self.arrangements.reset(&key);
        }
        self.arrange_problems.clear();
        self.docks_epoch += 1;
        self.panels_changed(cx);
    }

    /// A side panel was shown, hidden, moved or resized: show it so in every
    /// workspace, and keep it so at the next launch.
    fn panels_changed(&mut self, cx: &mut Context<Self>) {
        let saved = self.saved_panels();
        let panels = cx.default_global::<Panels>();
        panels.rev += 1;
        panels.saved = saved;
        self.panels_rev = panels.rev;
        cx.emit(WorkspaceEvent::TabsChanged);
        cx.notify();
    }

    fn toggle_explorer(&mut self, _: &ToggleExplorer, _: &mut Window, cx: &mut Context<Self>) {
        self.show_explorer = !self.show_explorer;
        self.panels_changed(cx);
    }

    fn toggle_changes(&mut self, _: &ToggleChanges, _: &mut Window, cx: &mut Context<Self>) {
        self.show_changes = !self.show_changes;
        self.panels_changed(cx);
    }

    fn toggle_scratches(&mut self, _: &ToggleScratches, _: &mut Window, cx: &mut Context<Self>) {
        self.show_scratches = !self.show_scratches;
        self.panels_changed(cx);
    }

    /// Show the command palette, or Search in Files.
    fn open_palette(&mut self, mode: palette::Mode, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_none() {
            let root = self.root.clone();
            let p = cx.new(|cx| Palette::new(root, window, cx));
            let sub = cx.subscribe_in(&p, window, |this, _, ev: &PaletteEvent, window, cx| {
                window.close_dialog(cx);
                this.focus_active(window, cx);
                match ev {
                    PaletteEvent::Run(action) => {
                        // Once focus is back in the workspace, which handles most of them.
                        let action = action.boxed_clone();
                        window.defer(cx, move |window, cx| window.dispatch_action(action, cx));
                    },
                    PaletteEvent::Open { path, line, col } => this.open_file_at(path.clone(), *line, Some(*col), window, cx),
                    PaletteEvent::OpenFile(path) => this.open_file(path.clone(), false, window, cx),
                    PaletteEvent::Close => {},
                }
            });
            self.palette = Some((p, sub));
        }
        if let Some((p, _)) = &self.palette {
            palette::open(p, mode, window, cx);
        }
    }

    /// Open `path` (as a preview) with the cursor at 1-based `line`, `col`.
    fn open_file_at(&mut self, path: PathBuf, line: u32, col: Option<u32>, window: &mut Window, cx: &mut Context<Self>) {
        self.open_file(path.clone(), false, window, cx);
        let view = self.tabs.iter().find_map(|t| match t {
            CenterTab::File { view, .. } if view.read(cx).path() == Some(path.as_path()) => Some(view.clone()),
            _ => None,
        });
        if let Some(view) = view {
            view.update(cx, |v, cx| v.go_to(line, col, window, cx));
        }
    }

    fn toggle_shells(&mut self, _: &ToggleShells, _: &mut Window, cx: &mut Context<Self>) {
        self.show_shells = !self.show_shells;
        self.panels_changed(cx);
    }

    fn toggle_usage(&mut self, _: &ToggleUsage, _: &mut Window, cx: &mut Context<Self>) {
        self.show_usage = !self.show_usage;
        self.panels_changed(cx);
    }

    fn ui<R>(&self, pane: PaneId, f: impl FnOnce(&mut PaneUi) -> R) -> R {
        f(self.pane_ui.borrow_mut().entry(pane).or_default())
    }

    fn new_pane(&mut self) -> PaneId {
        let p = self.next_pane;
        self.next_pane += 1;
        p
    }

    /// The pane tab `ix` is in.
    fn pane_of(&self, ix: usize) -> PaneId {
        self.tabs
            .get(ix)
            .and_then(|t| self.tab_pane.get(&t.entity_id()))
            .copied()
            .filter(|p| self.layout.contains(*p))
            .unwrap_or(self.focused)
    }

    /// Pane `pane`'s tabs, in order.
    fn pane_tabs(&self, pane: PaneId) -> Vec<usize> {
        (0..self.tabs.len()).filter(|&i| self.pane_of(i) == pane).collect()
    }

    /// The tab pane `pane` shows, if it has any.
    fn pane_active_ix(&self, pane: PaneId) -> Option<usize> {
        let tabs = self.pane_tabs(pane);
        let shown = self.pane_active.get(&pane).and_then(|id| tabs.iter().copied().find(|&i| self.tabs[i].entity_id() == *id));
        shown.or_else(|| tabs.iter().copied().find(|&i| !self.hidden(i))).or(tabs.first().copied())
    }

    /// The panes on screen.
    fn shown_panes(&self) -> Vec<PaneId> {
        match self.zoomed.filter(|z| self.layout.contains(*z)) {
            Some(z) => vec![z],
            None => self.layout.panes(),
        }
    }

    /// Add `tab` at the end, in the focused pane; its index.
    fn push_tab(&mut self, tab: CenterTab) -> usize {
        self.tab_pane.insert(tab.entity_id(), self.focused);
        self.tabs.push(tab);
        self.tabs.len() - 1
    }

    /// Make the panes agree with the tabs: every tab in a pane that exists,
    /// `focused` one of them, and each pane showing one of its tabs that
    /// is not folded away. `active` picks the focused pane when that has tabs.
    fn sync_panes(&mut self) {
        let panes = self.layout.panes();
        if !panes.contains(&self.focused) {
            self.focused = panes[0];
        }
        let live: HashSet<EntityId> = self.tabs.iter().map(CenterTab::entity_id).collect();
        let focused = self.focused;
        self.tab_pane.retain(|id, _| live.contains(id));
        for t in &self.tabs {
            let p = self.tab_pane.entry(t.entity_id()).or_insert(focused);
            if !panes.contains(p) {
                *p = focused;
            }
        }
        self.pane_active.retain(|p, id| panes.contains(p) && live.contains(id));
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
        if self.active < self.tabs.len() && !self.pane_tabs(self.focused).is_empty() {
            self.focused = self.pane_of(self.active);
            self.pane_active.insert(self.focused, self.tabs[self.active].entity_id());
        }
        for p in panes {
            let Some(mut ix) = self.pane_active_ix(p) else { continue };
            if self.hidden(ix) {
                match self.nearest_visible(ix) {
                    Some(v) => ix = v,
                    None => self.expand_group_of(ix),
                }
            }
            self.pane_active.insert(p, self.tabs[ix].entity_id());
            if p == self.focused {
                self.active = ix;
            }
        }
    }

    /// Close pane `pane` if it has no tabs; the pane before it takes focus,
    /// as in VS Code. The last pane stays.
    fn drop_if_empty(&mut self, pane: PaneId) {
        if !self.pane_tabs(pane).is_empty() {
            return;
        }
        let order = self.layout.panes();
        if !self.layout.remove(pane) {
            return;
        }
        self.pane_active.remove(&pane);
        self.pane_ui.borrow_mut().remove(&pane);
        if self.zoomed == Some(pane) {
            self.zoomed = None;
        }
        if self.focused == pane {
            let k = order.iter().position(|p| *p == pane).unwrap_or(0);
            self.focus_pane_state(if k > 0 { order[k - 1] } else { order[1] });
        }
        self.sync_panes();
    }

    fn focus_pane_state(&mut self, pane: PaneId) {
        self.focused = pane;
        if let Some(ix) = self.pane_active_ix(pane) {
            self.active = ix;
        }
    }

    /// Make `pane` the one keys and new tabs go to.
    fn focus_pane(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_pane_state(pane);
        self.sync_panes();
        self.focus_active(window, cx);
        cx.emit(WorkspaceEvent::TabsChanged);
        cx.notify();
    }

    /// Focus the pane next to the focused one on side `edge`.
    fn focus_toward(&mut self, edge: Edge, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = self.layout.neighbor(self.focused, edge) {
            if self.zoomed.is_some() {
                self.zoomed = Some(p);
            }
            self.focus_pane(p, window, cx);
        }
    }

    /// Close pane `pane`. Its tabs move to the pane before it rather than
    /// closing, so no terminal is lost.
    fn close_pane(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let order = self.layout.panes();
        let Some(k) = order.iter().position(|p| *p == pane).filter(|_| order.len() > 1) else { return };
        let into = if k > 0 { order[k - 1] } else { order[1] };
        for i in self.pane_tabs(pane) {
            self.tab_pane.insert(self.tabs[i].entity_id(), into);
        }
        if self.focused == pane {
            self.focus_pane_state(into);
        }
        self.drop_if_empty(pane);
        self.focus_active(window, cx);
        cx.emit(WorkspaceEvent::TabsChanged);
        cx.notify();
    }

    /// Move the tab showing entity `id` to the end of pane `pane`, out of
    /// its group; a pane it leaves empty closes.
    fn move_tab_to_pane(&mut self, id: EntityId, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(from) = self.tabs.iter().position(|t| t.entity_id() == id) else { return };
        let old = self.pane_of(from);
        // The pane it leaves shows its neighbor.
        if self.pane_active.get(&old) == Some(&id)
            && let Some(n) = (from + 1..self.tabs.len()).chain((0..from).rev()).find(|&i| self.pane_of(i) == old && !self.hidden(i))
        {
            self.pane_active.insert(old, self.tabs[n].entity_id());
        }
        let tab = self.tabs.remove(from);
        self.tabs.push(tab);
        self.tab_group.remove(&id);
        self.tab_pane.insert(id, pane);
        self.regroup();
        if let Some(ix) = self.tabs.iter().position(|t| t.entity_id() == id) {
            self.activate(ix, window, cx);
        }
        if old != pane {
            self.drop_if_empty(old);
        }
    }

    /// Split tab `ix`'s pane on side `edge`. The tab moves to the new pane;
    /// when it is its pane's only tab (or the focused pane is empty) the new
    /// pane gets a new terminal like it instead, as iTerm's split does.
    fn split(&mut self, ix: usize, edge: Edge, window: &mut Window, cx: &mut Context<Self>) {
        let empty = self.pane_tabs(self.focused).is_empty();
        let pane = if empty || ix >= self.tabs.len() { self.focused } else { self.pane_of(ix) };
        let tabs = self.pane_tabs(pane);
        let new = self.new_pane();
        self.layout.split(pane, new, edge);
        self.zoomed = None;
        if tabs.len() >= 2 && tabs.contains(&ix) {
            self.move_tab_to_pane(self.tabs[ix].entity_id(), new, window, cx);
        } else {
            let kind = tabs.contains(&ix).then(|| self.tabs[ix].term()).flatten().map_or(TabKind::Shell, |t| t.kind);
            self.focused = new;
            self.add_tab(kind, window, cx);
        }
    }

    /// What dropping the tab showing `id` on part `zone` of pane `pane`'s
    /// content does: join that pane, or split it on that side.
    fn drop_in_pane(&mut self, id: EntityId, pane: PaneId, zone: Zone, window: &mut Window, cx: &mut Context<Self>) {
        let Some(from) = self.tabs.iter().position(|t| t.entity_id() == id) else { return };
        let same = self.pane_of(from) == pane;
        match zone {
            Zone::Center if same => self.activate(from, window, cx),
            Zone::Center => self.move_tab_to_pane(id, pane, window, cx),
            // Nothing to split off from its own pane.
            Zone::Edge(_) if same && self.pane_tabs(pane).len() < 2 => self.activate(from, window, cx),
            Zone::Edge(edge) => {
                let new = self.new_pane();
                self.layout.split(pane, new, edge);
                self.zoomed = None;
                self.move_tab_to_pane(id, new, window, cx);
            },
        }
    }

    /// Lay the panes out as `cols` × `rows`, like VS Code's Editor Layout:
    /// panes keep their tabs in order, new ones start empty, and the tabs
    /// of panes that no longer fit join the last one.
    fn apply_layout(&mut self, cols: usize, rows: usize, window: &mut Window, cx: &mut Context<Self>) {
        let old = self.layout.panes();
        let n = cols * rows;
        let mut ids: Vec<PaneId> = old.iter().copied().take(n).collect();
        while ids.len() < n {
            ids.push(self.new_pane());
        }
        let last = ids[n - 1];
        for p in old.iter().skip(n) {
            for i in self.pane_tabs(*p) {
                self.tab_pane.insert(self.tabs[i].entity_id(), last);
            }
            self.pane_active.remove(p);
            self.pane_ui.borrow_mut().remove(p);
        }
        self.layout = Layout::grid(cols, rows, &ids);
        self.layout_epoch += 1;
        self.zoomed = None;
        self.sync_panes();
        self.focus_active(window, cx);
        cx.emit(WorkspaceEvent::TabsChanged);
        cx.notify();
    }

    /// One tab per pane in a grid that fits them all (tmux's tiled layout);
    /// past 12 tabs, panes share. A group's tabs stay together.
    fn tile_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.tabs.len();
        let (cols, rows) = panes::grid_for(n);
        self.apply_layout(cols, rows, window, cx);
        let order = self.layout.panes();
        let k = order.len();
        for i in 0..n {
            let p = if n <= k { order[i] } else { order[i * k / n] };
            self.tab_pane.insert(self.tabs[i].entity_id(), p);
        }
        let groups: Vec<u64> = self.groups.iter().map(|g| g.id).collect();
        for g in groups {
            if let Some(r) = self.group_tabs(g) {
                let p = self.pane_of(r.start);
                for i in r {
                    self.tab_pane.insert(self.tabs[i].entity_id(), p);
                }
            }
        }
        self.pane_active.clear();
        self.focused = self.pane_of(self.active);
        for p in order {
            self.drop_if_empty(p);
        }
        self.sync_panes();
        self.focus_active(window, cx);
        cx.emit(WorkspaceEvent::TabsChanged);
        cx.notify();
    }

    /// Let the focused pane fill the center, or put the others back.
    fn toggle_zoom(&mut self, cx: &mut Context<Self>) {
        self.zoomed = if self.zoomed.is_some() || self.layout.panes().len() < 2 { None } else { Some(self.focused) };
        cx.notify();
    }

    /// Show the focused pane's `n`th tab (`usize::MAX`: its last).
    fn activate_nth(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        let tabs = self.pane_tabs(self.focused);
        let ix = if n == usize::MAX { tabs.last() } else { tabs.get(n) };
        if let Some(&ix) = ix {
            self.activate(ix, window, cx);
        }
    }

    /// A drag's start. While a tab is dragged, the panes show where it can go.
    fn drag_start<T: Clone + Render>(ws: WeakEntity<Self>) -> impl Fn(&T, Point<Pixels>, &mut Window, &mut App) -> Entity<T> + 'static {
        move |d, _, _, cx| {
            if std::any::TypeId::of::<T>() == std::any::TypeId::of::<DraggedTab>() {
                let _ = ws.update(cx, |this, cx| {
                    this.tab_drag = true;
                    cx.notify();
                });
            }
            cx.new(|_| d.clone())
        }
    }

    /// `scope` with what `menu.pane_layout` and `menu.tab_layout` show here.
    fn layout_menus(&self, scope: template::Scope) -> template::Scope {
        let order = self.layout.panes();
        let presets = panes::PRESETS
            .iter()
            .map(|&(cols, rows, label)| {
                let on = order.len() == cols * rows && self.layout == Layout::grid(cols, rows, &order);
                template::Scope::default().var("label", label).var("on", on)
            })
            .collect();
        scope.var("panes", order.len()).list("presets", presets).list("layouts", tab_layout_entries())
    }

    /// What the items of `menu.pane_layout` and `menu.tab_layout` do.
    fn layout_item(ws: &WeakEntity<Self>, name: &str, entry: Option<usize>) -> Option<template::MenuHandler> {
        match name {
            "preset" => {
                let (cols, rows, _) = *panes::PRESETS.get(entry?)?;
                on_ws(ws, move |this, window, cx| this.apply_layout(cols, rows, window, cx))
            },
            "even" => on_ws(ws, |this, _, cx| {
                this.layout_epoch += 1;
                cx.notify();
            }),
            "tab_layout" => {
                let (layout, _) = *TabLayout::ALL.get(entry?)?;
                Some(Rc::new(move |_, _, cx| {
                    window_state::set_tab_layout(layout);
                    cx.refresh_windows();
                }))
            },
            _ => None,
        }
    }

    /// The center's split `node`; `path` names its resizable groups.
    fn render_layout(&self, node: &Layout, path: &str, cx: &mut Context<Self>) -> AnyElement {
        match node {
            Layout::Pane(p) => self.render_pane(*p, cx),
            Layout::Part(name) => self.render_center_part(name, cx),
            Layout::Split(axis, children) => {
                let id = SharedString::from(format!("panes-{}-{path}", self.layout_epoch));
                let group = match axis {
                    Axis::Horizontal => h_resizable(id),
                    Axis::Vertical => v_resizable(id),
                };
                let panels: Vec<_> = children
                    .iter()
                    .enumerate()
                    .map(|(k, c)| resizable_panel().child(self.render_layout(c, &format!("{path}.{k}"), cx)))
                    .collect();
                group.children(panels).into_any_element()
            },
        }
    }

    /// Pane `pane`: its tabs and the one it shows, its drop zones while a
    /// tab is dragged, and a line along its top when it has focus.
    fn render_pane(&self, pane: PaneId, cx: &mut Context<Self>) -> AnyElement {
        let has_focus = pane == self.focused;
        let focus_line = self.zoomed.is_none() && self.layout.panes().len() > 1 && has_focus;
        let vertical = window_state::tab_layout() == TabLayout::Vertical;
        let title = self.pane_active_ix(pane).map(|ix| self.full_title(ix, cx)).unwrap_or_default();
        let content = match self.pane_active_ix(pane).map(|ix| &self.tabs[ix]) {
            // Cached: one terminal's output then redraws only itself.
            Some(CenterTab::Term(t)) => t.view.clone().cached(cache_layout()).into_any_element(),
            Some(CenterTab::File { view, .. }) => view.clone().cached(cache_layout()).into_any_element(),
            Some(CenterTab::Diff(d)) => d.clone().cached(cache_layout()).into_any_element(),
            Some(CenterTab::Scratch { view: s, .. }) => s.clone().cached(cache_layout()).into_any_element(),
            None => self.empty_pane(pane, cx).into_any_element(),
        };
        let scope = template::Scope::new(format!("pane-{pane}"))
            .var("focused", has_focus)
            .var("focus_line", focus_line)
            .var("vertical", vertical)
            .var("title", title)
            .var("tabs", self.pane_tabs(pane).len())
            .slot("content", content)
            .slot_opt("drop_overlay", self.drops_on_panes(cx).map(|dock| self.drop_overlay(pane, dock, cx)));
        let scope = if vertical {
            scope.slot("vertical_tabs", self.render_vertical_tabs(pane, cx))
        } else {
            scope.slot("tab_bar", self.render_tab_bar(pane, cx))
        };
        let focus = cx.listener(move |this, _: &MouseDownEvent, window, cx| {
            if this.focused != pane {
                this.focus_pane(pane, window, cx);
            }
        });
        let mut focus = Some(focus);
        template::render(
            "pane",
            scope,
            &mut |frame, _, d| match (frame, d, focus.take()) {
                ("pane", Framed::Box(d), Some(f)) => d.id(("pane", pane as usize)).capture_any_mouse_down(f).into_any_element(),
                (_, d, _) => d.into_any_element(),
            },
            cx,
        )
    }

    /// What an empty pane (from a layout preset) shows (`pane.empty`).
    fn empty_pane(&self, pane: PaneId, cx: &mut Context<Self>) -> AnyElement {
        let ws = cx.entity().downgrade();
        let scope = template::Scope::new(format!("empty-{pane}"));
        template::render(
            "pane.empty",
            scope,
            &mut |name, _, f| Self::pane_frame(&ws, pane, name, f).unwrap_or_else(Framed::into_any_element),
            cx,
        )
    }

    /// What the pane dock allows (`"dock": "center"`).
    fn pane_dock(cx: &App) -> template::PaneDock {
        cx.try_global::<template::Templates>().map(|t| t.pane_dock("workspace")).unwrap_or_default()
    }

    /// While a tab or a pane of this workspace is dragged and the pane dock
    /// takes it somewhere: what it allows.
    fn drops_on_panes(&self, cx: &Context<Self>) -> Option<template::PaneDock> {
        let dock = Self::pane_dock(cx);
        let dragged = cx.try_global::<ActiveDrag>().and_then(|d| d.0.as_ref());
        let pane = dragged.is_some_and(|p| self.dragged_pane(p, cx.entity_id()).is_some());
        let panel = dragged.is_some_and(|p| dock.takes(&p.part, &p.kind));
        (cx.has_active_drag() && (self.tab_drag || pane && dock.panes || panel) && (dock.edges || dock.stack)).then_some(dock)
    }

    /// Whether a panel (not a tab or a pane) is dragged.
    fn panel_dragged(cx: &App) -> bool {
        cx.try_global::<ActiveDrag>().and_then(|d| d.0.as_ref()).is_some_and(|p| p.kind != "pane")
    }

    /// Part `name` among the panes, drawn as in a dock, with drop zones on
    /// its sides while a tab, a pane or a panel the pane dock takes is dragged.
    fn render_center_part(&self, name: &str, cx: &mut Context<Self>) -> AnyElement {
        let body = template::render_part(name, self.part_scope(), cx);
        let d = div().relative().size_full().flex().flex_col().min_w_0().min_h_0().child(body);
        let Some(dock) = self.drops_on_panes(cx).filter(|d| d.edges) else { return d.into_any_element() };
        let own = cx.try_global::<ActiveDrag>().and_then(|d| d.0.as_ref()).is_some_and(|p| *p.part == *name);
        if own {
            // Let go on its own place: nothing happens (it doesn't float).
            return d.child(div().absolute().inset_0().on_drop(|_: &DraggedPart, _, _| {})).into_any_element();
        }
        let area = format!("{}:part:{name}", cx.entity_id());
        let overlay = crate::dock::zone_overlay(&area, cx);
        let ws = cx.entity().downgrade();
        let target = Layout::Part(name.to_owned());
        let on_tab = {
            let (ws, target) = (ws.clone(), target.clone());
            move |t: &DraggedTab, zone: Zone, window: &mut Window, cx: &mut App| {
                let Zone::Edge(edge) = zone else { return };
                let _ = ws.update(cx, |this, cx| {
                    this.tab_drag = false;
                    this.tab_beside(t.tab, &target, edge, window, cx);
                });
            }
        };
        let overlay = crate::dock::on_zones(overlay, area.clone().into(), true, false, on_tab);
        let overlay = crate::dock::on_zones(overlay, area.into(), true, false, move |p: &DraggedPart, zone, window, cx| {
            let Zone::Edge(edge) = zone else { return };
            let _ = ws.update(cx, |this, cx| this.drop_part_beside(p, &target, edge, &dock, window, cx));
        });
        d.child(overlay).into_any_element()
    }

    /// What dropping `p` (a pane or a panel) on side `edge` of `target` among
    /// the panes does: it moves there.
    fn drop_part_beside(
        &mut self,
        p: &DraggedPart,
        target: &Layout,
        edge: Edge,
        dock: &template::PaneDock,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(from) = self.dragged_pane(p, cx.entity_id()) {
            return self.move_pane_beside(from, target, edge, window, cx);
        }
        let locked = cx.try_global::<crate::dock::LayoutLock>().is_some_and(|l| l.0);
        if !locked && dock.takes(&p.part, &p.kind) {
            self.arrangement_changed(Change::Center { part: p.part.clone(), target: target.clone(), edge }, cx);
        }
    }

    /// The tab showing `id` in a new pane on side `edge` of `target`.
    fn tab_beside(&mut self, id: EntityId, target: &Layout, edge: Edge, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tabs.iter().any(|t| t.entity_id() == id) || !self.layout.has(target) {
            return;
        }
        let new = self.new_pane();
        self.layout.split_leaf(target, Layout::Pane(new), edge);
        self.zoomed = None;
        self.move_tab_to_pane(id, new, window, cx);
    }

    /// Pane `from` (dragged by its tab bar) moved to side `edge` of `target`.
    fn move_pane_beside(&mut self, from: PaneId, target: &Layout, edge: Edge, window: &mut Window, cx: &mut Context<Self>) {
        if *target == Layout::Pane(from) || !self.layout.has(target) || !self.layout.remove(from) {
            return;
        }
        self.layout.split_leaf(target, Layout::Pane(from), edge);
        self.layout_epoch += 1;
        self.zoomed = None;
        self.sync_panes();
        self.focus_active(window, cx);
        cx.emit(WorkspaceEvent::TabsChanged);
        cx.notify();
    }

    /// What dragging pane `pane` by its tab bar's empty space drags, when the
    /// pane dock takes panes and there is another pane to drop it on.
    fn pane_handle(&self, pane: PaneId, cx: &Context<Self>) -> Option<DraggedPart> {
        let locked = cx.try_global::<crate::dock::LayoutLock>().is_some_and(|l| l.0);
        if locked || !Self::pane_dock(cx).panes || self.layout.panes().len() < 2 {
            return None;
        }
        let title = self.pane_active_ix(pane).map(|ix| self.full_title(ix, cx)).unwrap_or_default();
        Some(DraggedPart {
            part: format!("pane:{}:{pane}", cx.entity_id()).into(),
            kind: "pane".into(),
            title: title.into(),
            floating: false,
            popped: false,
        })
    }

    /// The pane of this workspace that `p` drags, if it drags one.
    fn dragged_pane(&self, p: &DraggedPart, me: EntityId) -> Option<PaneId> {
        let (owner, pane) = p.part.strip_prefix("pane:")?.split_once(':')?;
        (owner == me.to_string()).then(|| pane.parse().ok()).flatten()
    }

    /// Over pane `pane`'s content while a tab or a pane is dragged: the part
    /// under the pointer lights up where it would go (VS Code's drop zones).
    fn drop_overlay(&self, pane: PaneId, dock: template::PaneDock, cx: &mut Context<Self>) -> Div {
        let area = format!("{}:pane:{pane}", cx.entity_id());
        let overlay = crate::dock::zone_overlay(&area, cx);
        let ws = cx.entity().downgrade();
        let on_tab = {
            let ws = ws.clone();
            move |d: &DraggedTab, zone: Zone, window: &mut Window, cx: &mut App| {
                let _ = ws.update(cx, |this, cx| {
                    this.tab_drag = false;
                    this.drop_in_pane(d.tab, pane, zone, window, cx);
                });
            }
        };
        let overlay = crate::dock::on_zones(overlay, area.clone().into(), dock.edges, dock.stack, on_tab);
        // A panel goes on a pane's side, never into it.
        let stack = dock.stack && !Self::panel_dragged(cx);
        crate::dock::on_zones(overlay, area.into(), dock.edges, stack, move |p: &DraggedPart, zone, window, cx| {
            let _ = ws.update(cx, |this, cx| match (this.dragged_pane(p, cx.entity_id()), zone) {
                (Some(from), zone) => this.drop_pane(from, pane, zone, window, cx),
                (None, Zone::Edge(edge)) => this.drop_part_beside(p, &Layout::Pane(pane), edge, &dock, window, cx),
                (None, Zone::Center) => {},
            });
        })
    }

    /// What dropping pane `from` (dragged by its tab bar) on part `zone` of
    /// pane `to` does: its tabs join `to`, or it moves to that side of `to`.
    fn drop_pane(&mut self, from: PaneId, to: PaneId, zone: Zone, window: &mut Window, cx: &mut Context<Self>) {
        if from == to || !self.layout.contains(from) || !self.layout.contains(to) {
            return;
        }
        match zone {
            Zone::Center => {
                for i in self.pane_tabs(from) {
                    self.tab_pane.insert(self.tabs[i].entity_id(), to);
                }
                if self.focused == from {
                    self.focus_pane_state(to);
                }
                self.drop_if_empty(from);
            },
            Zone::Edge(edge) => return self.move_pane_beside(from, &Layout::Pane(to), edge, window, cx),
        }
        self.zoomed = None;
        self.sync_panes();
        self.focus_active(window, cx);
        cx.emit(WorkspaceEvent::TabsChanged);
        cx.notify();
    }

    /// Non-input control commands (see `control`).
    #[cfg(debug_assertions)]
    pub fn control_query(&mut self, line: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        match line.trim() {
            "state" => {
                let tabs: Vec<String> = self
                    .tabs
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let g = self
                            .group_of(i)
                            .and_then(|g| self.groups.iter().position(|x| x.id == g))
                            .map(|g| format!("@{g}:"))
                            .unwrap_or_default();
                        g + &match t {
                            CenterTab::Term(t) => {
                                format!("term:{}", t.rename.clone().unwrap_or_else(|| t.view.read(cx).title().to_string().into()))
                            },
                            CenterTab::File { view, .. } => format!("file:{}", view.read(cx).debug_state()),
                            CenterTab::Diff(d) => format!("diff:{}", d.read(cx).path().display()),
                            CenterTab::Scratch { view: s, .. } => {
                                let s = s.read(cx);
                                format!("scratch:{}{}", s.path().display(), if s.is_dirty() { "*" } else { "" })
                            },
                        }
                    })
                    .collect();
                let groups: Vec<String> = self
                    .groups
                    .iter()
                    .map(|g| format!("{}:{}{}", g.name, GROUP_COLORS[g.color].0, if g.collapsed { ":collapsed" } else { "" }))
                    .collect();
                let order = self.layout.panes();
                let pane_tabs: Vec<Vec<usize>> = order.iter().map(|p| self.pane_tabs(*p)).collect();
                let shown: Vec<Option<usize>> = order.iter().map(|p| self.pane_active_ix(*p)).collect();
                let arrange =
                    self.arranged_for.as_ref().and_then(|(_, k)| self.arrangements.get(k)).map(Arrangement::encode).unwrap_or_default();
                format!(
                    "tabs={tabs:?} groups={groups:?} panes={} pane_tabs={pane_tabs:?} shown={shown:?} focused={} zoomed={} slots={} active={} dialog={} changes={} scratches={} arrange {arrange}",
                    self.layout.encode(&order),
                    order.iter().position(|p| *p == self.focused).unwrap_or(0),
                    self.zoomed.is_some(),
                    self.slots().len(),
                    self.active,
                    window.has_active_dialog(cx),
                    self.show_changes,
                    if self.show_scratches { self.scratches.read(cx).len() as isize } else { -1 }
                )
            },
            cmd if cmd.starts_with("term-") => {
                let Some(view) = self.tabs.get(self.active).and_then(CenterTab::term).map(|t| t.view.clone()) else {
                    return "error: the active tab is no terminal".into();
                };
                let (name, arg) = cmd.split_once(' ').unwrap_or((cmd, ""));
                match name {
                    // `\n` = Enter, `\e` = Escape, `\a` = BEL.
                    "term-type" => {
                        let bytes = arg.replace("\\n", "\r").replace("\\e", "\x1b").replace("\\a", "\x07").into_bytes();
                        view.update(cx, |v, cx| v.write_input(bytes, cx));
                        "ok".into()
                    },
                    "term-screen" => view.read(cx).debug_screen(),
                    "term-info" => {
                        let rang: Vec<bool> = self.tabs.iter().filter_map(CenterTab::term).map(|t| t.rang).collect();
                        format!("{} rang={rang:?} running={:?}", view.read(cx).debug_info(), self.running(cx))
                    },
                    "term-paste-path" => {
                        let paths: Vec<PathBuf> = arg.split('|').map(PathBuf::from).collect();
                        view.update(cx, |v, cx| v.debug_paste_paths(&paths, cx));
                        "ok".into()
                    },
                    _ => format!("error: unknown command {name:?}"),
                }
            },
            "tree" => format!("{} focused={}", self.tree.read(cx).debug_state(cx), self.tree.focus_handle(cx).contains_focused(window, cx)),
            cmd if cmd.starts_with("scratch-new ") => {
                self.new_scratch(cmd["scratch-new ".len()..].trim(), window, cx);
                "ok".into()
            },
            cmd if cmd.starts_with("scratch-open ") => {
                let name = cmd["scratch-open ".len()..].trim();
                match scratch::list(&scratch::dir()).into_iter().find(|p| p.file_name().is_some_and(|n| n == name)) {
                    Some(path) => {
                        self.open_scratch(path, window, cx);
                        "ok".into()
                    },
                    None => format!("error: no scratch named {name:?}"),
                }
            },
            cmd if cmd.starts_with("scratch-text ") => match self.tabs.get(self.active) {
                Some(CenterTab::Scratch { view: s, .. }) => {
                    let text = cmd["scratch-text ".len()..].replace("\\n", "\n");
                    s.update(cx, |s, cx| s.set_text(&text, window, cx));
                    "ok".into()
                },
                _ => "error: active tab is not a scratch buffer".into(),
            },
            // Drop tab FROM onto slot TO, as a drag in the tab bar does.
            cmd if cmd.starts_with("tab-move ") => {
                let mut n = cmd["tab-move ".len()..].split_whitespace().map(str::parse::<usize>);
                match (n.next(), n.next()) {
                    (Some(Ok(from)), Some(Ok(to))) if from < self.tabs.len() => {
                        let join = self.group_of(to.min(self.tabs.len() - 1));
                        self.move_tab(self.tabs[from].entity_id(), to, join, window, cx);
                        "ok".into()
                    },
                    _ => "error: tab-move FROM TO".into(),
                }
            },
            // Scroll, wrap or list tabs vertically (as the menus' Tab Layout does).
            cmd if cmd.starts_with("tab-layout ") => match TabLayout::parse(&cmd["tab-layout ".len()..]) {
                Some(layout) => {
                    window_state::set_tab_layout(layout);
                    cx.refresh_windows();
                    "ok".into()
                },
                None => "error: tab-layout scroll|wrap|vertical".into(),
            },
            "tab-scroll" => {
                let scroll = self.ui(self.focused, |u| u.tab_scroll.clone());
                let (offset, max) = (scroll.offset().x, scroll.max_offset().x);
                format!("offset={} max={} layout={}", f32::from(offset), f32::from(max), window_state::tab_layout().key())
            },
            // Tab groups, by tab index and group index (as in `state`).
            cmd if cmd.starts_with("group-") => {
                let mut args = cmd.split_whitespace();
                let op = args.next().unwrap_or_default();
                let mut num = || args.next().and_then(|n| n.parse::<usize>().ok());
                let rest = cmd.splitn(3, ' ').nth(2).unwrap_or_default().to_owned();
                let group = |this: &Self, n: Option<usize>| n.and_then(|n| this.groups.get(n)).map(|g| g.id);
                match op {
                    "group-new" => match num().filter(|&ix| ix < self.tabs.len()) {
                        Some(ix) => {
                            self.new_group(ix);
                            self.groups_changed(cx);
                            "ok".into()
                        },
                        None => "error: group-new TAB".into(),
                    },
                    "group-add" => match (num().filter(|&ix| ix < self.tabs.len()), num()) {
                        (Some(ix), g) => match group(self, g) {
                            Some(g) => {
                                self.add_to_group(ix, g, cx);
                                "ok".into()
                            },
                            None => "error: no such group".into(),
                        },
                        _ => "error: group-add TAB GROUP".into(),
                    },
                    "group-remove" => match num() {
                        Some(ix) => {
                            self.remove_from_group(ix, window, cx);
                            "ok".into()
                        },
                        None => "error: group-remove TAB".into(),
                    },
                    "group-move" => match (group(self, num()), num()) {
                        (Some(g), Some(to)) => {
                            self.move_group(g, to, cx);
                            "ok".into()
                        },
                        _ => "error: group-move GROUP TAB".into(),
                    },
                    _ => {
                        let Some(g) = group(self, num()) else { return "error: no such group".into() };
                        match op {
                            "group-toggle" => self.toggle_group(g, window, cx),
                            "group-name" => self.set_group_name(g, &rest, cx),
                            "group-color" => match GROUP_COLORS.iter().position(|c| c.0 == rest.trim()) {
                                Some(c) => self.set_group_color(g, c, cx),
                                None => return "error: unknown color".into(),
                            },
                            "group-ungroup" => self.ungroup(g, window, cx),
                            "group-close" => self.close_group(g, window, cx),
                            _ => return format!("error: unknown command {op:?}"),
                        }
                        "ok".into()
                    },
                }
            },
            // Panes, by tab index and pane index (as in `state`).
            cmd if cmd.starts_with("pane-") => {
                let mut args = cmd.split_whitespace();
                let op = args.next().unwrap_or_default();
                let args: Vec<&str> = args.collect();
                let num = |k: usize| args.get(k).and_then(|n| n.parse::<usize>().ok());
                let order = self.layout.panes();
                let tab = num(0).filter(|&i| i < self.tabs.len());
                match op {
                    // What dropping tab TAB on part ZONE of pane PANE's content does.
                    "pane-drop" => match (tab, num(1).and_then(|k| order.get(k)), args.get(2).and_then(|z| Zone::parse(z))) {
                        (Some(ix), Some(&p), Some(zone)) => self.drop_in_pane(self.tabs[ix].entity_id(), p, zone, window, cx),
                        _ => return "error: pane-drop TAB PANE center|left|right|up|down".into(),
                    },
                    "pane-split" => match (tab, args.get(1).and_then(|e| Edge::parse(e))) {
                        (Some(ix), Some(edge)) => self.split(ix, edge, window, cx),
                        _ => return "error: pane-split TAB left|right|up|down".into(),
                    },
                    "pane-layout" => match (num(0), num(1)) {
                        (Some(c), Some(r)) if (1..=4).contains(&c) && (1..=4).contains(&r) => self.apply_layout(c, r, window, cx),
                        _ => return "error: pane-layout COLS ROWS".into(),
                    },
                    // What dropping pane FROM (dragged by its tab bar) on part ZONE of pane TO does.
                    "pane-move" => match (
                        num(0).and_then(|k| order.get(k)),
                        num(1).and_then(|k| order.get(k)),
                        args.get(2).and_then(|z| Zone::parse(z)),
                    ) {
                        (Some(&from), Some(&to), Some(zone)) => self.drop_pane(from, to, zone, window, cx),
                        _ => return "error: pane-move FROM TO center|left|right|up|down".into(),
                    },
                    "pane-tile" => self.tile_tabs(window, cx),
                    "pane-zoom" => self.toggle_zoom(cx),
                    "pane-focus" => match (num(0).and_then(|k| order.get(k)), args.first().and_then(|e| Edge::parse(e))) {
                        (Some(&p), _) => self.focus_pane(p, window, cx),
                        (_, Some(edge)) => self.focus_toward(edge, window, cx),
                        _ => return "error: pane-focus PANE|left|right|up|down".into(),
                    },
                    "pane-close" => match num(0).and_then(|k| order.get(k)) {
                        Some(&p) => self.close_pane(p, window, cx),
                        None => return "error: pane-close PANE".into(),
                    },
                    _ => return format!("error: unknown command {op:?}"),
                }
                "ok".into()
            },
            // Floats PART at X Y (pixels from the workspace's top-left), W wide and H high.
            cmd if cmd.starts_with("part-float ") => {
                let args: Vec<&str> = cmd.split_whitespace().skip(1).collect();
                let n: Option<Vec<f32>> = args.iter().skip(1).map(|a| a.parse().ok()).collect();
                let (Some(part), Some([x, y, w, h]), Some(area)) = (args.first(), n.as_deref(), self.float_area) else {
                    return "error: part-float PART X Y W H".into();
                };
                let rect = arrange::Rect { x: *x, y: *y, w: *w, h: *h };
                let change = Change::Float { part: (*part).to_owned().into(), rect, area: crate::dock::area_size(area) };
                self.arrangement_changed(change, cx);
                "ok".into()
            },
            // Moves PART to a window of its own at X Y (pixels on this window's display), W wide and H high.
            cmd if cmd.starts_with("part-window ") => {
                let args: Vec<&str> = cmd.split_whitespace().skip(1).collect();
                let n: Option<Vec<f32>> = args.iter().skip(1).map(|a| a.parse().ok()).collect();
                let (Some(part), Some([x, y, w, h])) = (args.first(), n.as_deref()) else {
                    return "error: part-window PART X Y W H".into();
                };
                let display = window.display(cx).and_then(|d| d.uuid().ok()).map(|u| u.to_string()).unwrap_or_default();
                self.pop_out_at(part, display, (*x, *y), Some((*w, *h)), cx);
                "ok".into()
            },
            cmd if cmd.starts_with("part-dock ") => {
                self.dock_part(&crate::dock::DockPart(cmd["part-dock ".len()..].trim().to_owned().into()), window, cx);
                "ok".into()
            },
            "layout-reset" => {
                self.reset_layout(&ResetLayout, window, cx);
                "ok".into()
            },
            // What dropping PART in DOCK does: before, after, onto or on a side of TARGET, or at its end.
            cmd if cmd.starts_with("part-move ") => {
                let args: Vec<&str> = cmd.split_whitespace().skip(1).collect();
                let at = match (args.get(2).copied(), args.get(3)) {
                    (Some("before"), Some(t)) => arrange::Drop::Before((*t).to_owned()),
                    (Some("after"), Some(t)) => arrange::Drop::After((*t).to_owned()),
                    (Some("stack"), Some(t)) => arrange::Drop::Stack((*t).to_owned()),
                    (Some(e), Some(t)) if arrange::Edge::parse(e).is_some() => {
                        arrange::Drop::Edge((*t).to_owned(), arrange::Edge::parse(e).unwrap())
                    },
                    (Some("end"), None) => arrange::Drop::End,
                    _ => return "error: part-move PART DOCK before|after|stack|left|right|up|down|end [TARGET]".into(),
                };
                let (part, dock) = (args[0], args[1]);
                let kind =
                    cx.try_global::<template::Templates>().and_then(|t| t.parts().into_iter().find(|(n, _)| *n == part).map(|(_, k)| k));
                let Some(kind) = kind else { return format!("error: no part {part:?}") };
                let change = Change::Move { part: part.to_owned().into(), kind: kind.into(), dock: dock.to_owned().into(), at };
                self.arrangement_changed(change, cx);
                "ok".into()
            },
            // What dropping PART on side EDGE of TARGET (a pane's index, or a part among the panes) does.
            cmd if cmd.starts_with("part-center ") => {
                let args: Vec<&str> = cmd.split_whitespace().skip(1).collect();
                let order = self.layout.panes();
                let target = match args.get(1) {
                    Some(t) if t.chars().all(|c| c.is_ascii_digit()) => {
                        t.parse::<usize>().ok().and_then(|k| order.get(k)).map(|p| Layout::Pane(*p))
                    },
                    Some(t) => Some(Layout::Part((*t).to_owned())),
                    None => None,
                };
                let (Some(part), Some(target), Some(edge)) = (args.first(), target, args.get(2).and_then(|e| Edge::parse(e))) else {
                    return "error: part-center PART PANE|PART left|right|up|down".into();
                };
                let kind =
                    cx.try_global::<template::Templates>().and_then(|t| t.parts().into_iter().find(|(n, _)| n == part).map(|(_, k)| k));
                let Some(kind) = kind else { return format!("error: no part {part:?}") };
                let p =
                    DraggedPart { part: (*part).to_owned().into(), kind: kind.into(), title: "".into(), floating: false, popped: false };
                self.drop_part_beside(&p, &target, edge, &Self::pane_dock(cx), window, cx);
                "ok".into()
            },
            cmd if cmd.starts_with("file-open ") => {
                let path = self.root.join(cmd["file-open ".len()..].trim());
                self.open_file(path, true, window, cx);
                "ok".into()
            },
            other => format!("error: unknown command {other:?}"),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// Ask before closing `what` while `jobs` run in it; `then` closes it.
pub fn confirm_close<T: 'static>(
    jobs: Vec<String>,
    what: &str,
    window: &mut Window,
    cx: &mut Context<T>,
    then: impl FnOnce(&mut T, &mut Window, &mut Context<T>) + 'static,
) {
    let detail = format!("Still running: {}. Closing ends {}.", jobs.join(", "), if jobs.len() == 1 { "it" } else { "them" });
    let answer = window.prompt(PromptLevel::Warning, &format!("Close {what}?"), Some(&detail), &["Close", "Cancel"], cx);
    cx.spawn_in(window, async move |this, cx| {
        if answer.await == Ok(0) {
            let _ = this.update_in(cx, |this, window, cx| then(this, window, cx));
        }
    })
    .detach();
}

/// The layout of a cached view: it fills the place it is given (a cached
/// view isn't measured from its contents).
fn cache_layout() -> StyleRefinement {
    StyleRefinement::default().size_full()
}

fn notify_system(title: &str, body: &str, cx: &mut App) {
    cx.show_system_notification(SystemNotification {
        tag: "tern".into(),
        title: title.to_owned().into(),
        body: body.to_owned().into(),
        actions: vec![],
    });
}

/// What a title bar button of a panel does.
pub type TitleClick = Rc<dyn Fn(&mut Window, &mut App)>;

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = window;
        // Its agents' shells come and go with their tabs (a terminal tells
        // of a change by notifying this): the panel redraws only on a change.
        let shells = self.shell_rows(cx);
        self.shells.update(cx, |s, cx| s.set(shells, cx));
        // A drag that ended outside every drop target leaves these behind.
        if !cx.has_active_drag() {
            self.tab_drag = false;
            if cx.try_global::<DropHint>().is_some_and(|h| h.0.is_some()) {
                cx.set_global(DropHint(None));
            }
            if cx.try_global::<crate::dock::Resizing>().is_some_and(|r| r.0.is_some()) {
                cx.set_global(crate::dock::Resizing(None));
            }
            if cx.try_global::<ActiveDrag>().is_some_and(|d| d.0.is_some()) {
                cx.set_global(ActiveDrag(None));
                // Other windows drew zones for the drag too.
                cx.defer(|cx| cx.refresh_windows());
            }
        }
        self.sync_panes();
        let arrangement = self.arrangement(cx);
        // The parts the arrangement puts among the panes, and no others.
        let centered: Vec<&str> = arrangement.iter().flat_map(|a| a.center.keys().map(String::as_str)).collect();
        if self.layout.fit_parts(&centered) {
            self.layout_epoch += 1;
        }
        let center = match self.zoomed.filter(|z| self.layout.contains(*z)) {
            Some(z) => self.render_pane(z, cx),
            None => {
                let layout = self.layout.shown(&|p| self.part_shown(p)).unwrap_or_else(|| self.layout.clone());
                self.render_layout(&layout, "0", cx)
            },
        };

        let left_strip = self.strip("left", arrangement.as_deref(), cx);
        let mut scope = self.part_scope();
        let ws = cx.entity().downgrade();
        let on_change: OnChange = Rc::new(move |change, _, cx| {
            let _ = ws.update(cx, |this, cx| this.arrangement_changed(change, cx));
        });
        let floats = crate::dock::Floats {
            floating: arrangement
                .iter()
                .flat_map(|a| a.floating.iter().filter(|(_, f)| f.window.is_none()).map(|(p, _)| p.clone()))
                .collect(),
            popped: arrangement
                .iter()
                .flat_map(|a| a.floating.iter().filter(|(_, f)| f.window.is_some()).map(|(p, _)| p.clone()))
                .collect(),
            centered: arrangement.iter().flat_map(|a| a.center.keys().cloned()).collect(),
            ..Default::default()
        };
        if let Some(a) = arrangement.clone() {
            scope = scope.docks(a, on_change.clone());
        }
        let scope = scope.slot("left_strip", left_strip).slot("center", center);
        // The panels draw their headers after this: a pinned part's header doesn't drag.
        let pinned = cx.try_global::<template::Templates>().map(|t| t.pinned(&scope)).unwrap_or_default();
        if cx.try_global::<crate::dock::PinnedParts>().is_none_or(|p| p.0 != pinned) {
            cx.set_global(crate::dock::PinnedParts(pinned));
        }
        let grounded = cx.try_global::<template::Templates>().map(|t| t.grounded(&scope)).unwrap_or_default();
        let windowless = cx.try_global::<template::Templates>().map(|t| t.windowless(&scope)).unwrap_or_default();
        let floats = crate::dock::Floats { grounded, windowless, ..floats };
        if cx.try_global::<crate::dock::Floats>().is_none_or(|f| *f != floats) {
            cx.set_global(floats);
        }
        self.sync_part_windows(arrangement.as_deref(), cx);
        let body = template::render("workspace", scope, &mut |_, _, d| d.into_any_element(), cx);

        // Where the floating parts sit, for the header menu's Float.
        let ws = cx.entity().downgrade();
        let area = canvas(
            move |bounds, _, cx| {
                let _ = ws.update(cx, |this, _| this.float_area = Some(bounds));
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        crate::dock::floats(div(), arrangement, on_change)
            .key_context("Workspace")
            .size_full()
            .relative()
            .on_action(cx.listener(Self::float_part))
            .on_action(cx.listener(Self::dock_part))
            .on_action(cx.listener(Self::pop_out))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::dropped_outside))
            .on_action(cx.listener(|this, _: &NewClaudeTab, w, cx| this.add_tab(TabKind::Claude, w, cx)))
            .on_action(cx.listener(|this, _: &NewShellTab, w, cx| this.add_tab(TabKind::Shell, w, cx)))
            .on_action(cx.listener(|this, _: &NewScratch, w, cx| this.new_scratch("txt", w, cx)))
            .on_action(cx.listener(|_, _: &OpenScratchesFolder, _, cx| {
                let dir = scratch::dir();
                let _ = std::fs::create_dir_all(&dir);
                cx.open_with_system(&dir);
            }))
            .on_action(cx.listener(|this, _: &CloseTab, w, cx| {
                // An empty pane closes itself.
                if this.pane_tabs(this.focused).is_empty() {
                    this.close_pane(this.focused, w, cx);
                } else {
                    this.request_close_tab(this.active, w, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SplitRight, w, cx| this.split(this.active, Edge::Right, w, cx)))
            .on_action(cx.listener(|this, _: &SplitDown, w, cx| this.split(this.active, Edge::Bottom, w, cx)))
            .on_action(cx.listener(|this, _: &ToggleZoomPane, _, cx| this.toggle_zoom(cx)))
            .on_action(cx.listener(|this, _: &TileTabs, w, cx| this.tile_tabs(w, cx)))
            .on_action(cx.listener(|this, _: &FocusPaneLeft, w, cx| this.focus_toward(Edge::Left, w, cx)))
            .on_action(cx.listener(|this, _: &FocusPaneRight, w, cx| this.focus_toward(Edge::Right, w, cx)))
            .on_action(cx.listener(|this, _: &FocusPaneUp, w, cx| this.focus_toward(Edge::Top, w, cx)))
            .on_action(cx.listener(|this, _: &FocusPaneDown, w, cx| this.focus_toward(Edge::Bottom, w, cx)))
            .on_action(cx.listener(|this, _: &NextTab, w, cx| this.step_tab(true, w, cx)))
            .on_action(cx.listener(|this, _: &PrevTab, w, cx| this.step_tab(false, w, cx)))
            .on_action(cx.listener(Self::toggle_explorer))
            .on_action(cx.listener(|this, _: &OpenSettingsFile, w, cx| this.open_file(settings::settings_path(cx), true, w, cx)))
            .on_action(cx.listener(Self::toggle_changes))
            .on_action(cx.listener(Self::toggle_scratches))
            .on_action(cx.listener(|this, _: &CommandPalette, w, cx| this.open_palette(palette::Mode::Commands, w, cx)))
            .on_action(cx.listener(|this, _: &GoToFile, w, cx| this.open_palette(palette::Mode::Files, w, cx)))
            .on_action(cx.listener(|this, _: &SearchInFiles, w, cx| this.open_palette(palette::Mode::Search, w, cx)))
            .on_action(cx.listener(Self::toggle_shells))
            .on_action(cx.listener(Self::toggle_usage))
            .on_action(cx.listener(Self::reset_layout))
            .on_action(cx.listener(|this, _: &FocusTerminal, w, cx| this.focus_active(w, cx)))
            // Reached only when focus is outside the file tab (it handles these itself).
            .on_action(cx.listener(|this, a: &viewer::ToggleDiff, w, cx| {
                if let Some(CenterTab::File { view, .. }) = this.tabs.get(this.active) {
                    view.clone().update(cx, |v, cx| v.toggle_diff(a, w, cx));
                }
            }))
            .on_action(cx.listener(|this, a: &viewer::TogglePreview, w, cx| {
                if let Some(CenterTab::File { view, .. }) = this.tabs.get(this.active) {
                    view.clone().update(cx, |v, cx| v.toggle_preview(a, w, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &Tab1, w, cx| this.activate_nth(0, w, cx)))
            .on_action(cx.listener(|this, _: &Tab2, w, cx| this.activate_nth(1, w, cx)))
            .on_action(cx.listener(|this, _: &Tab3, w, cx| this.activate_nth(2, w, cx)))
            .on_action(cx.listener(|this, _: &Tab4, w, cx| this.activate_nth(3, w, cx)))
            .on_action(cx.listener(|this, _: &Tab5, w, cx| this.activate_nth(4, w, cx)))
            .on_action(cx.listener(|this, _: &Tab6, w, cx| this.activate_nth(5, w, cx)))
            .on_action(cx.listener(|this, _: &Tab7, w, cx| this.activate_nth(6, w, cx)))
            .on_action(cx.listener(|this, _: &Tab8, w, cx| this.activate_nth(7, w, cx)))
            .on_action(cx.listener(|this, _: &Tab9, w, cx| this.activate_nth(usize::MAX, w, cx)))
            .child(area)
            .child(body)
    }
}

pub fn services(terminal: TerminalConfig) -> Services {
    let (tx, rx) = flume::unbounded();
    let hooks: tern_mux::client::HookSink = Arc::new(move |token, body| {
        if let Some(ev) = HookEvent::parse(token, &body) {
            let _ = tx.send(ev);
        }
    });
    let (reveal_tx, reveal_rx) = flume::unbounded();
    let reveal: tern_mux::client::RevealSink = Arc::new(move |id| {
        let _ = reveal_tx.send(id);
    });
    let mux = Arc::new(Connector::new(tern_mux::paths::mux_dir(), true, hooks, reveal));
    Services { mux, hook_events: rx, reveal_events: reveal_rx, terminal }
}

#[cfg(test)]
mod tests {
    use super::claude_bg_job;

    #[test]
    fn finds_the_background_job_of_a_live_session() {
        let dir = std::env::temp_dir().join(format!("tern-claude-sessions-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let me = std::process::id();
        let write = |name: &str, body: String| std::fs::write(dir.join(name), body).unwrap();
        write("1.json", format!(r#"{{"pid":{me},"sessionId":"aaa","kind":"interactive"}}"#));
        write("2.json", format!(r#"{{"pid":{me},"sessionId":"bbb","kind":"bg","jobId":"bbb1"}}"#));
        // No such process: left behind.
        write("3.json", r#"{"pid":999999,"sessionId":"ccc","kind":"bg","jobId":"ccc1"}"#.to_owned());
        write("4.key", "bbb".to_owned());

        assert_eq!(claude_bg_job(&dir, "bbb").as_deref(), Some("bbb1"));
        assert_eq!(claude_bg_job(&dir, "aaa"), None);
        assert_eq!(claude_bg_job(&dir, "ccc"), None);
        assert_eq!(claude_bg_job(&dir, "zzz"), None);
        assert_eq!(claude_bg_job(&dir.join("missing"), "bbb"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
