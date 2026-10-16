//! One workspace = one project folder: explorer / scratches | tabs (terminals,
//! files, diffs, scratches) | changes. A window shows one of its workspaces at a time (see
//! `main_window`), and a workspace can move between windows.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName as AssetIcon;
use gpui_kit::base::Selectable as _;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, WindowExt, h_resizable, resizable_panel, v_resizable};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tern_bridge::HookEvent;
use tern_fs::GitStatus;
use tern_mux::client::Connector;
use tern_term::{SpawnConfig, TermSize};

use crate::agents::Agent;
use crate::arrange::{self, Arrangement};
use crate::changes_view::{ChangesEvent, ChangesView};
use crate::diff_pane::DiffPane;
use crate::dock::{ActiveDrag, Change, OnChange};
use crate::file_icons;
use crate::file_tree_view::{FileTreeEvent, FileTreeView};
use crate::panes::{self, Edge, Layout, PaneId, Zone};
use crate::platform::bind;
use crate::scratch::{self, Scratch};
use crate::scratches_view::{ScratchesEvent, ScratchesView};
use crate::settings;
use crate::style;
use crate::template;
use crate::terminal_view::{ActivityStatus, TerminalConfig, TerminalEvent, TerminalView};
use crate::theme_picker::OpenSettings;
use crate::viewer::{self, Viewer, ViewerEvent};
use crate::window_state::{self, SavedGroup, SavedTab, SavedWorkspace, TabLayout};

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
    /// Started with `--resume`: if that fails at once, start a new one.
    resuming: bool,
    /// When the tab last showed a notification, to drop a second one for
    /// the same event (OSC 9 and the status change of one turn).
    notified: Option<Instant>,
    /// Rang the bell while not shown; cleared when shown.
    rang: bool,
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
        style::drag_chip(cx).child(self.name.clone())
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
        group_pill(self.name.clone(), self.color, None, cx).shadow_md()
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

/// A group's chip: its name on its color, a dot when it has no name, and
/// `count` (its tabs) when collapsed.
fn group_pill(name: SharedString, color: Hsla, count: Option<usize>, cx: &App) -> Div {
    let text = match count {
        Some(n) if name.is_empty() => n.to_string(),
        Some(n) => format!("{name}  {n}"),
        None => name.to_string(),
    };
    if text.is_empty() {
        return div().mx_1().size(px(10.)).rounded_full().bg(color);
    }
    let look = style::el("pane_tab_bar.group", cx);
    look.apply(div().flex().items_center().bg(color).whitespace_nowrap()).child(look.label(text))
}

/// How far a group outline piece runs past its side; the tab clips it.
const EDGE_RUN: Pixels = px(4000.);

/// Which piece of a group's outline a tab draws, like Chrome's: a line along
/// the bottom of its tabs that climbs around the active one (`active_outline`).
#[derive(Clone, Copy)]
enum GroupEdge {
    /// Under the chip, starting a little in.
    Start,
    /// Under a tab.
    Under,
}

/// A tab's prefix or suffix holder, as tall as the tab so a `group_edge` in
/// it lines up with the tab's edges. The tab's own left/right 1px borders
/// sit just outside it.
fn tab_side() -> Div {
    div().h(style::TAB_H).flex().items_center()
}

/// One piece of a group's outline, placed in a `tab_side`. It runs far past
/// the tab on one side and the tab, which clips its children, cuts it off.
fn group_edge(edge: GroupEdge, color: Hsla) -> Div {
    let piece = div().absolute().bottom_0().w(EDGE_RUN).h(px(2.)).bg(color);
    match edge {
        GroupEdge::Start => piece.left(px(6.)).rounded_l_full(),
        GroupEdge::Under => piece.left(px(-1.)),
    }
}

/// The active tab's outline: its sides and rounded top, open at the bottom.
/// Each piece goes in a `tab_side`: (prefix piece, suffix piece).
fn active_outline(color: Hsla) -> (impl IntoElement, impl IntoElement) {
    over_tab(move |b, window| {
        let corners = Corners { top_left: px(8.), top_right: px(8.), ..Default::default() };
        let w = px(2.);
        let edges = Edges { top: w, left: w, right: w, bottom: px(0.) };
        window.paint_quad(quad(b, corners, transparent_black(), edges, color, BorderStyle::Solid));
    })
}

/// `tab` labelled `text` as element `look` writes it. gpui-component colors
/// the label itself (the selected and hovered colors come from the theme),
/// so an element's `text` color goes on a box of its own around it.
pub(crate) fn tab_label(tab: Tab, look: &style::El, text: impl Into<SharedString>) -> Tab {
    let text = look.label(text);
    if look.is_set("text") { tab.child(div().text_color(look.color("text")).child(text)) } else { tab.label(text) }
}

/// The selected tab's 1px side borders in `color`, over gpui-component's
/// (always the theme's `border`). Pieces as for [`active_outline`].
pub(crate) fn tab_sides(color: Hsla) -> (impl IntoElement, impl IntoElement) {
    over_tab(move |b, window| {
        let side = |x: Pixels| Bounds::from_corners(point(x, b.top()), point(x + px(1.), b.bottom()));
        window.paint_quad(fill(side(b.left()), color));
        window.paint_quad(fill(side(b.right() - px(1.)), color));
    })
}

/// Paints over a tab, its side borders included, after the tab itself.
/// The tab's width isn't known when it's built, so the suffix's piece notes
/// where the tab ends and the prefix's piece paints from there (prepaint
/// runs for both before either paints). Each goes in a `tab_side`:
/// (prefix piece, suffix piece).
fn over_tab(paint: impl Fn(Bounds<Pixels>, &mut Window) + 'static) -> (impl IntoElement, impl IntoElement) {
    let right = Rc::new(Cell::new(None::<Pixels>));
    let r = right.clone();
    let end = canvas(move |b, _, _| r.set(Some(b.right())), |_, _, _, _| {}).absolute().top_0().bottom_0().right_0();
    let paint = Rc::new(paint);
    let start = canvas(
        move |b: Bounds<Pixels>, window, cx| {
            // The tab clips its children inside its 1px side borders; draw
            // later, clipped 1px wider.
            let mask = window.content_mask().bounds;
            let mask = ContentMask {
                bounds: Bounds::from_corners(point(mask.left() - px(1.), mask.top()), point(mask.right() + px(1.), mask.bottom())),
            };
            let (right, paint) = (right.clone(), paint.clone());
            let over = canvas(
                |_, _, _| {},
                move |_, _, window, _| {
                    let Some(right) = right.get() else { return };
                    paint(Bounds::from_corners(point(b.left() - px(1.), b.top()), point(right + px(1.), b.bottom())), window);
                },
            );
            let mut over = over.into_any_element();
            over.layout_as_root(AvailableSpace::min_size(), window, cx);
            window.defer_draw(over, b.origin, 0, Some(mask));
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .bottom_0()
    .left_0();
    (start, end)
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
    /// Where the dragged tab would land: a pane and the part of it.
    drop_target: Option<(PaneId, Zone)>,
    pane_ui: RefCell<HashMap<PaneId, PaneUi>>,
    /// Where the side panels sit, per template and docks (`arrange::key`).
    arrangements: HashMap<String, Arrangement>,
    /// The keys whose arrangement the user changed (or a session brought):
    /// the only ones saved. The others follow the templates' defaults.
    touched: HashSet<String>,
    /// The templates generation and key the arrangement was last fitted to.
    arranged_for: Option<(u64, String)>,
    /// What fitting the stored arrangement to the templates found.
    arrange_problems: Vec<String>,
    _subs: Vec<Subscription>,
}

/// A terminal tab, as the screenshot tray lists it.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))] // for the screenshot tray
pub struct TermTabInfo {
    pub id: EntityId,
    pub title: String,
    pub agent: Option<Agent>,
    /// The workspace's active tab.
    pub active: bool,
    pub view: Entity<TerminalView>,
}

pub enum WorkspaceEvent {
    /// The last tab closed: the workspace is done.
    Close,
    /// Tabs opened, closed, switched or renamed, or a side panel shown or
    /// hidden: the session should be saved.
    TabsChanged,
}

impl EventEmitter<WorkspaceEvent> for Workspace {}

/// Common `claude` flags, toggled from the new-tab menu.
const CLAUDE_FLAGS: [(&str, &str); 2] = [("Skip Permissions", "--dangerously-skip-permissions"), ("Chrome", "--chrome")];

/// The tab layout choices (scroll, wrap, vertical), for every window.
fn tab_layout_items(mut menu: PopupMenu) -> PopupMenu {
    for (layout, label) in TabLayout::ALL {
        menu = menu.item(PopupMenuItem::new(label).checked(window_state::tab_layout() == layout).on_click(move |_, _, cx| {
            window_state::set_tab_layout(layout);
            cx.refresh_windows();
        }));
    }
    menu
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

impl Workspace {
    /// Open `saved.folder` with `saved`'s tabs; a Claude tab when none of
    /// them can come back.
    pub fn new(saved: SavedWorkspace, services: Services, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let root = saved.folder;
        let tree = cx.new(|cx| FileTreeView::new(root.clone(), cx));
        let changes = cx.new(|cx| ChangesView::new(root.clone(), cx));
        let scratches = cx.new(ScratchesView::new);

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
            FileTreeEvent::Changed(paths) => {
                this.changes.update(cx, |c, cx| c.files_changed(paths, cx));
                this.files_changed(paths, window, cx);
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

        let mut this = Self {
            root,
            services,
            tree,
            changes,
            scratches,
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
            show_explorer: !saved.hidden.iter().any(|h| h == "explorer"),
            show_changes: !saved.hidden.iter().any(|h| h == "changes"),
            show_scratches: !saved.hidden.iter().any(|h| h == "scratches"),
            layout: Layout::Pane(0),
            tab_pane: HashMap::new(),
            pane_active: HashMap::new(),
            focused: 0,
            next_pane: 1,
            zoomed: None,
            layout_epoch: 0,
            tab_drag: false,
            drop_target: None,
            pane_ui: RefCell::new(HashMap::new()),
            // A key stored twice: the last one wins.
            arrangements: saved.arrangements.iter().filter_map(|(k, e)| Some((k.clone(), Arrangement::decode(k, e)?))).collect(),
            touched: saved.arrangements.iter().map(|(k, _)| k.clone()).collect(),
            arranged_for: None,
            arrange_problems: vec![],
            _subs: vec![tree_sub, changes_sub, scratches_sub],
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
        if this.tabs.is_empty() {
            this.add_tab(TabKind::Claude, window, cx);
        } else {
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
        let (panes, tab_panes, pane_active) = if order.len() > 1 {
            (
                self.layout.encode(&order),
                (0..self.tabs.len()).map(|i| order.iter().position(|p| *p == self.pane_of(i)).unwrap_or(0)).collect(),
                order.iter().map(|p| self.pane_active_ix(*p)).collect(),
            )
        } else {
            Default::default()
        };
        let hidden = [("explorer", self.show_explorer), ("changes", self.show_changes), ("scratches", self.show_scratches)]
            .into_iter()
            .filter(|(_, shown)| !shown)
            .map(|(name, _)| name.to_owned())
            .collect();
        SavedWorkspace {
            folder: self.root.clone(),
            tabs,
            tab_groups,
            groups,
            active: self.active,
            panes,
            tab_panes,
            pane_active,
            hidden,
            arrangements: self.saved_arrangements(),
        }
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

    /// Whether a Claude tab is mid-turn or waiting for input.
    pub fn busy(&self, cx: &App) -> bool {
        self.tabs
            .iter()
            .filter_map(CenterTab::term)
            .any(|t| matches!(t.view.read(cx).status(), ActivityStatus::Working | ActivityStatus::Blocked))
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
                let claude = match resume {
                    Some(id) => format!("{claude} --resume {}", shell_quote(id)),
                    None => claude,
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
        // Cleared when it reattaches to a running Claude (`Ready`): only a
        // `--resume` it starts says something by exiting at once.
        let resuming = session.is_some();
        let tab = TermTab { view, rename, kind, token, session, resuming, notified: None, rang: false, _subs: vec![sub] };
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
            let (ws, path) = (ws.clone(), path.clone());
            dialog
                .w(style::dialog_width(false, cx))
                .title("Delete Scratch File")
                .content({
                    let name = name.clone();
                    move |c, _, _| c.child(format!("Delete \"{name}\"? This cannot be undone."))
                })
                .footer(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(Button::new("delete-cancel").small().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx)))
                        .child(Button::new("delete-ok").small().danger().label("Delete").on_click(move |_, window, cx| {
                            window.close_dialog(cx);
                            let _ = ws.update(cx, |this, cx| this.delete_scratch(&path, window, cx));
                        })),
                )
        });
    }

    /// Close the tab showing scratch `path` (if any), then delete the file.
    fn delete_scratch(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let open = self.tabs.iter().position(|t| matches!(t, CenterTab::Scratch { view, .. } if view.read(cx).path() == path));
        if let Some(ix) = open {
            // Closing the last tab would close the workspace.
            if self.tabs.len() == 1 {
                self.add_tab(TabKind::Claude, window, cx);
            }
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
    /// conversation resumes at the next launch), or a program a shell runs.
    fn running_in(&self, ix: usize, cx: &App) -> Option<String> {
        let t = self.tabs.get(ix)?.term()?;
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
            cx.emit(WorkspaceEvent::Close);
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
        if let Some(t) = self.tabs.get(self.active) {
            window.focus(&t.focus_handle(cx), cx);
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
        window.open_dialog(cx, move |dialog, window, _| {
            let vp = window.viewport_size();
            let (body, pane, ws) = (pane.clone(), pane.clone(), ws.clone());
            dialog
                .w(vp.width * 0.8)
                .margin_top(vp.height * 0.06)
                .title(title.clone())
                .content(move |c, window, _| c.child(div().h(window.viewport_size().height * 0.7).child(body.clone())))
                .footer(div().flex().justify_end().child(Button::new("open-in-tab").small().primary().label("Open in Tab").on_click(
                    move |_, window, cx| {
                        window.close_dialog(cx);
                        let _ = ws.update(cx, |this, cx| this.open_diff_tab(pane.clone(), window, cx));
                    },
                )))
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

    /// Reload open files and diffs (tabs and the preview popup) affected by `paths`.
    fn files_changed(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        let preview = self.preview.as_ref().and_then(|p| p.upgrade());
        let mut diffs = vec![];
        for t in &self.tabs {
            match t {
                CenterTab::File { view, .. } => view.update(cx, |v, cx| v.files_changed(paths, window, cx)),
                CenterTab::Diff(d) => diffs.push(d.clone()),
                CenterTab::Term(_) | CenterTab::Scratch { .. } => {},
            }
        }
        for d in diffs.into_iter().chain(preview) {
            d.update(cx, |d, cx| d.files_changed(paths, cx));
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
        // Closing the last tab would close the workspace.
        if doomed.len() == self.tabs.len() {
            self.add_tab(TabKind::Claude, window, cx);
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
                    if *attached {
                        t.resuming = false;
                    }
                }
                // Its id goes into the saved session.
                cx.emit(WorkspaceEvent::TabsChanged);
            },
            TerminalEvent::Exited { code, quick } => {
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
                        cx.emit(WorkspaceEvent::TabsChanged);
                    }
                    self.tree.update(cx, |t, cx| t.reveal(path, cx));
                    return cx.notify();
                }
                self.open_file(path.clone(), false, window, cx);
                if let Some(line) = *line
                    && let Some(view) = self.tabs.iter().find_map(|t| match t {
                        CenterTab::File { view, .. } if view.read(cx).path() == Some(path.as_path()) => Some(view.clone()),
                        _ => None,
                    })
                {
                    view.update(cx, |v, _| v.go_to(line, *col));
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
            cx.emit(WorkspaceEvent::TabsChanged);
        }
        let paths: Vec<PathBuf> = ev.paths.into_iter().filter(|p| p.starts_with(&self.root)).collect();
        if paths.is_empty() {
            return;
        }
        self.tree.update(cx, |t, cx| t.mark_claude_edit(&paths, cx));
        self.changes.update(cx, |c, cx| c.files_changed(&paths, cx));
        self.files_changed(&paths, window, cx);
    }

    /// The terminal tabs, in tab order, for picking where a screenshot goes.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))] // for the screenshot tray
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

    /// Show the tab with entity `id`.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))] // for the screenshot tray
    pub fn show_tab(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.tabs.iter().position(|t| t.entity_id() == id) {
            self.activate(ix, window, cx);
        }
    }

    /// Open a tab running `agent`: a Claude tab, or a shell that starts it.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))] // for the screenshot tray
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

    /// Group `g`'s chip, with its tab count when collapsed.
    fn chip_pill(&self, g: &TabGroup, cx: &App) -> Div {
        let count = g.collapsed.then(|| self.group_tabs(g.id).map_or(0, |r| r.len()));
        group_pill(g.name.clone(), group_color(g.color), count, cx)
    }

    /// Tab `i`'s close button, in `fg` when its tab sets `close_text`.
    fn close_button(&self, i: usize, fg: Option<Hsla>, cx: &mut Context<Self>) -> Button {
        style::el("pane_tab_bar.button", cx)
            .apply(Button::new(("close-tab", i)))
            .xsmall()
            .ghost()
            .icon(IconName::Close)
            .when_some(fg, |b, c| b.text_color(c))
            .tooltip(format!("Close ({})", crate::platform::shortcut("⌘W")))
            .on_click(cx.listener(move |this, _, window, cx| {
                // Don't let the tab's own click activate it.
                cx.stop_propagation();
                this.request_close_tab(i, window, cx);
            }))
    }

    /// Tab `i` in a horizontal bar: right-click menu, drag to reorder (or
    /// into a group), close button, its piece of its group's outline.
    fn bar_tab(&self, i: usize, cx: &mut Context<Self>) -> Tab {
        let (label, name, badge) = self.tab_info(i, cx);
        let drop_bg = cx.theme().list_hover;
        let join = self.group_of(i);
        let color = join.and_then(|g| self.group(g)).map(|g| group_color(g.color));
        let active = self.pane_active_ix(self.pane_of(i)) == Some(i);
        let look = style::el(if active { "pane_tab_bar.tab_active" } else { "pane_tab_bar.tab" }, cx);
        let mut tab =
            style::el("pane_tab_bar.tab", cx).apply(tab_label(Tab::new(), &look, label).when(self.is_preview(i, cx), |t| t.italic()));
        if active {
            tab = look.apply(tab);
        }
        let close_fg = look.is_set("close_text").then(|| look.color("close_text"));
        let icon = self.tab_icon(i, look.is_set("icon_text").then(|| look.color("icon_text")), cx);
        let (mut start, mut end) = (None, None);
        match color {
            Some(c) if active => {
                let (s, e) = active_outline(c);
                (start, end) = (Some(s.into_any_element()), Some(e.into_any_element()));
            },
            Some(c) => start = Some(group_edge(GroupEdge::Under, c).into_any_element()),
            None if active && look.is_set("border") => {
                let (s, e) = tab_sides(look.color("border"));
                (start, end) = (Some(s.into_any_element()), Some(e.into_any_element()));
            },
            None => {},
        }
        let badge = badge.filter(|(b, _)| !b.is_empty());
        if badge.is_some() || start.is_some() || icon.is_some() {
            tab = tab.prefix(
                tab_side()
                    .children(start)
                    .when_some(icon, |d, icon| d.child(div().pl_2().child(icon)))
                    .when_some(badge, |d, (badge, color)| d.child(div().pl_2().text_color(color).child(badge))),
            );
        }
        let drag = DraggedTab { tab: self.tabs[i].entity_id(), name };
        tab.on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, _| this.menu_tab = Some(i)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    if ev.click_count > 1 {
                        this.pin_tab(i, cx);
                    }
                }),
            )
            .on_drag(drag, self.start_drag(cx))
            .drag_over::<DraggedTab>(move |s, _, _, _| s.bg(drop_bg))
            .drag_over::<DraggedGroup>(move |s, _, _, _| s.bg(drop_bg))
            .on_drop(cx.listener(move |this, d: &DraggedTab, window, cx| this.move_tab(d.tab, i, join, window, cx)))
            .on_drop(cx.listener(move |this, d: &DraggedGroup, _, cx| this.move_group(d.group, i, cx)))
            .suffix(tab_side().pr_1().children(end).child(self.close_button(i, close_fg, cx)))
    }

    /// Group `g`'s chip in a horizontal bar: click to collapse, right-click
    /// for its menu, drag to move the group; tabs dropped on it join it.
    fn bar_chip(&self, g: &TabGroup, cx: &mut Context<Self>) -> Tab {
        let id = g.id;
        let drop_bg = cx.theme().list_hover;
        let drag = DraggedGroup { group: id, name: g.name.clone(), color: group_color(g.color) };
        let open = !g.collapsed && self.group_tabs(id).is_some_and(|r| !r.is_empty());
        Tab::new()
            .when(open, |t| t.prefix(tab_side().child(group_edge(GroupEdge::Start, group_color(g.color)))))
            .child(self.chip_pill(g, cx))
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, _| this.menu_group = Some(id)))
            .on_drag(drag, self.start_drag(cx))
            .drag_over::<DraggedTab>(move |s, _, _, _| s.bg(drop_bg))
            .drag_over::<DraggedGroup>(move |s, _, _, _| s.bg(drop_bg))
            .on_drop(cx.listener(move |this, d: &DraggedTab, window, cx| this.move_tab_to_group(d.tab, id, window, cx)))
            .on_drop(cx.listener(move |this, d: &DraggedGroup, _, cx| {
                if let Some(r) = this.group_tabs(id) {
                    this.move_group(d.group, r.start, cx);
                }
            }))
    }

    fn new_tab_button(&self, pane: PaneId, cx: &mut Context<Self>) -> impl IntoElement {
        let ws = cx.entity().downgrade();
        style::el("pane_tab_bar.button", cx)
            .apply(Button::new(("new-tab", pane as usize)))
            .xsmall()
            .ghost()
            .icon(IconName::Plus)
            .dropdown_menu(move |menu, window, cx| {
                let (ws, ws2, ws3, ws4) = (ws.clone(), ws.clone(), ws.clone(), ws.clone());
                menu.menu("Open Workspace…", Box::new(crate::OpenFolder))
                    .separator()
                    .menu("New Claude Tab", Box::new(NewClaudeTab))
                    .menu("New Shell Tab", Box::new(NewShellTab))
                    .submenu("Claude Options", window, cx, move |mut menu, _, _| {
                        let args = window_state::claude_args();
                        for (label, flag) in CLAUDE_FLAGS {
                            menu = menu.item(
                                PopupMenuItem::new(format!("{label}  {flag}")).checked(window_state::has_flag(&args, flag)).on_click(
                                    move |_, _, _| {
                                        window_state::set_claude_args(&window_state::toggle_flag(&window_state::claude_args(), flag));
                                    },
                                ),
                            );
                        }
                        let ws = ws3.clone();
                        menu.separator()
                            .item(PopupMenuItem::new("Edit Arguments…").on_click(move |_, window, cx| {
                                let _ = ws.update(cx, |this, cx| this.edit_claude_args(window, cx));
                            }))
                            .item(PopupMenuItem::label("Applies to Claude tabs opened from now on"))
                    })
                    .separator()
                    .menu("New Scratch File", Box::new(NewScratch))
                    .submenu("New Scratch File of Type", window, cx, move |mut menu, _, _| {
                        for (label, ext) in scratch::KINDS {
                            let ws = ws.clone();
                            menu = menu.item(PopupMenuItem::new(format!("{label}  .{ext}")).on_click(move |_, window, cx| {
                                let _ = ws.update(cx, |this, cx| this.new_scratch(ext, window, cx));
                            }));
                        }
                        menu
                    })
                    .submenu("Scratches", window, cx, move |mut menu, _, _| {
                        let files = scratch::list(&scratch::dir());
                        if files.is_empty() {
                            return menu.item(PopupMenuItem::new("No scratch files").disabled(true));
                        }
                        for path in files.into_iter().take(30) {
                            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            let ws = ws2.clone();
                            menu = menu.item(PopupMenuItem::new(name).on_click(move |_, window, cx| {
                                let _ = ws.update(cx, |this, cx| this.open_scratch(path.clone(), window, cx));
                            }));
                        }
                        menu
                    })
                    .menu("Open Scratches Folder", Box::new(OpenScratchesFolder))
                    .separator()
                    .submenu("Pane Layout", window, cx, move |menu, _, cx| match ws4.upgrade() {
                        Some(this) => Self::pane_layout_items(menu, &ws4, this.read(cx)),
                        None => menu,
                    })
                    .submenu("Tab Layout", window, cx, |menu, _, _| tab_layout_items(menu))
            })
    }

    fn panel_buttons(&self, cx: &mut Context<Self>) -> Div {
        let ws = cx.entity().downgrade();
        div().px_1().flex().items_center().gap_0p5().child(
            Button::new("pane-layout")
                .xsmall()
                .ghost()
                .icon(Icon::new(AssetIcon::LayoutGrid))
                .selected(self.layout.panes().len() > 1)
                .tooltip("Pane Layout")
                .dropdown_menu(move |menu, _, cx| match ws.upgrade() {
                    Some(this) => Self::pane_layout_items(menu, &ws, this.read(cx)),
                    None => menu,
                }),
        )
    }

    /// The tool window buttons of one window edge, IntelliJ style: a thin
    /// column with `top` buttons from the top and `bottom` ones from the
    /// bottom. Each toggles its panel, so a hidden panel is one click away.
    fn tool_strip(name: &'static str, top: Vec<Button>, bottom: Vec<Button>, cx: &mut Context<Self>) -> Div {
        let look = style::el(name, cx);
        let column = |b: Vec<Button>| div().flex().flex_col().items_center().gap(look.len("gap")).children(b);
        look.container(div().h_full().flex_none().flex().flex_col().items_center().justify_between())
            .child(column(top))
            .child(column(bottom))
    }

    fn strip_button(id: &'static str, icon: AssetIcon, on: bool, tooltip: impl Into<SharedString>, cx: &App) -> Button {
        style::el("tool_strip.button", cx).apply(Button::new(id).small().ghost().icon(Icon::new(icon)).selected(on).tooltip(tooltip))
    }

    /// The tool strip button of `part`.
    fn part_button(&self, part: &str, cx: &mut Context<Self>) -> Option<Button> {
        let shortcut = crate::platform::shortcut;
        Some(match part {
            "explorer" => {
                Self::strip_button("strip-explorer", AssetIcon::Folder, self.show_explorer, format!("Explorer ({})", shortcut("⌘B")), cx)
                    .on_click(cx.listener(|this, _, w, cx| this.toggle_explorer(&ToggleExplorer, w, cx)))
            },
            "scratches" => Self::strip_button(
                "strip-scratches",
                AssetIcon::StickyNote,
                self.show_scratches,
                format!("Scratches ({})", shortcut("⌘⇧S")),
                cx,
            )
            .on_click(cx.listener(|this, _, w, cx| this.toggle_scratches(&ToggleScratches, w, cx))),
            "changes" => {
                Self::strip_button("strip-changes", AssetIcon::GitBranch, self.show_changes, format!("Changes ({})", shortcut("⌘⇧G")), cx)
                    .on_click(cx.listener(|this, _, w, cx| this.toggle_changes(&ToggleChanges, w, cx)))
            },
            _ => return None,
        })
    }

    /// The `left` or `right` tool strip: a button per part in the docks it
    /// shows, in their order (the built-in sides without an arrangement).
    fn strip(&self, side: &str, arrangement: Option<&Arrangement>, cx: &mut Context<Self>) -> Div {
        let parts: Vec<String> = match (arrangement, cx.try_global::<template::Templates>()) {
            (Some(a), Some(t)) => t
                .docks("workspace")
                .iter()
                .filter(|d| t.dock_strip("workspace", &d.name).as_deref() == Some(side))
                .filter_map(|d| a.docks.get(&d.name).cloned().flatten())
                .flat_map(|tree| tree.parts().into_iter().map(str::to_owned).collect::<Vec<_>>())
                .collect(),
            _ if side == "left" => vec!["explorer".into(), "scratches".into()],
            _ => vec!["changes".into()],
        };
        let top = parts.iter().filter_map(|p| self.part_button(p, cx)).collect();
        Self::tool_strip(if side == "left" { "tool_strip.left" } else { "tool_strip.right" }, top, vec![], cx)
    }

    /// Where a dragged tab or group goes to the end of pane `pane`, out of any group.
    fn end_drop_zone(&self, id: &'static str, pane: PaneId, cx: &mut Context<Self>) -> Stateful<Div> {
        let drop_bg = cx.theme().list_hover;
        div()
            .id((id, pane as usize))
            .drag_over::<DraggedTab>(move |s, _, _, _| s.bg(drop_bg))
            .drag_over::<DraggedGroup>(move |s, _, _, _| s.bg(drop_bg))
            .on_drop(cx.listener(move |this, d: &DraggedTab, window, cx| this.move_tab_to_pane(d.tab, pane, window, cx)))
            .on_drop(cx.listener(move |this, d: &DraggedGroup, _, cx| {
                this.move_group(d.group, this.tabs.len(), cx);
                this.set_group_pane(d.group, pane);
                this.groups_changed(cx);
            }))
    }

    /// The tab or group menu on right-click, picked by what was clicked;
    /// the tab layouts on empty space.
    fn with_tab_menu(&self, id: (&'static str, PaneId), child: AnyElement, fill: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let ws = cx.entity().downgrade();
        div()
            .id((id.0, id.1 as usize))
            .when(fill, |d| d.size_full())
            // Before the tabs' own handlers pick what the menu is for.
            .capture_any_mouse_down(cx.listener(|this, ev: &MouseDownEvent, _, _| {
                if ev.button == MouseButton::Right {
                    this.menu_tab = None;
                    this.menu_group = None;
                }
            }))
            .child(child)
            .context_menu(move |menu, _, cx| {
                let Some(this) = ws.upgrade() else { return menu };
                let this = this.read(cx);
                if let Some(g) = this.menu_group.filter(|&g| this.group(g).is_some()) {
                    return Self::group_menu(menu, &ws, g, this);
                }
                match this.menu_tab.filter(|&ix| ix < this.tabs.len()) {
                    Some(ix) => Self::tab_menu(menu, &ws, ix, this, cx),
                    None => {
                        let menu = Self::pane_layout_items(menu.item(PopupMenuItem::label("Pane Layout")), &ws, this);
                        tab_layout_items(menu.separator().item(PopupMenuItem::label("Tab Layout")))
                    },
                }
            })
    }

    /// Pane `pane`'s horizontal tab bar: one scrolling row, or rows that wrap.
    fn render_tab_bar(&self, pane: PaneId, cx: &mut Context<Self>) -> impl IntoElement {
        let look = style::el("pane_tab_bar", cx);
        let slots = self.slots_in(pane);
        let shown = self.pane_active_ix(pane);
        let active_slot = slots.iter().position(|s| Some(*s) == shown.map(Slot::Tab)).unwrap_or(0);
        let scroll = self.ui(pane, |u| u.tab_scroll.clone());
        let items: Vec<(Slot, Tab)> = slots
            .iter()
            .filter_map(|s| match *s {
                Slot::Group(g) => Some((*s, self.bar_chip(self.group(g)?, cx))),
                Slot::Tab(i) => Some((*s, self.bar_tab(i, cx))),
            })
            .collect();
        // Right after the last tab, like a browser's new-tab button.
        let end_space =
            self.end_drop_zone("term-bar-end", pane, cx).flex_1().px_1().flex().items_center().child(self.new_tab_button(pane, cx));
        let panels = self.pane_buttons(pane, cx);
        let bar = if window_state::tab_layout() == TabLayout::Wrap {
            // Reveal the active tab again once tabs scroll.
            self.ui(pane, |u| u.revealed = None);
            let tabs = items.into_iter().enumerate().map(|(n, (slot, tab))| {
                div().id(("wrapped-tab", n)).child(match slot {
                    Slot::Group(g) => tab.on_click(cx.listener(move |this, _, window, cx| this.toggle_group(g, window, cx))),
                    Slot::Tab(i) => {
                        tab.selected(Some(i) == shown).on_click(cx.listener(move |this, _, window, cx| this.activate(i, window, cx)))
                    },
                })
            });
            look.apply(div().flex().items_start())
                .child(div().flex_1().min_w_0().flex().flex_wrap().children(tabs).child(end_space.h(style::TAB_BAR_H).min_w(px(32.))))
                .child(panels.h(style::TAB_BAR_H))
                .into_any_element()
        } else {
            if self.ui(pane, |u| u.revealed.replace(active_slot)) != Some(active_slot) {
                scroll.scroll_to_item(active_slot);
            }
            let overflows = scroll.max_offset().x > px(0.);
            // The bar draws its own bottom line; a border here would be a second.
            let mut bar = look.apply(TabBar::new(("term-tabs", pane as usize)));
            bar.style().border_widths = Default::default();
            let bar = bar
                .track_scroll(&scroll)
                // Lists every tab once they don't all fit.
                .menu(overflows)
                .when(shown.is_some(), |b| b.selected_index(active_slot))
                .children(items.into_iter().map(|(_, tab)| tab))
                .last_empty_space(end_space.h_full())
                .suffix(panels.h_full())
                .on_click(cx.listener(move |this, ix: &usize, window, cx| match slots.get(*ix) {
                    Some(Slot::Group(g)) => this.toggle_group(*g, window, cx),
                    Some(Slot::Tab(i)) => this.activate(*i, window, cx),
                    None => {},
                }));
            // The strip only takes sideways scrolls; let a mouse wheel move it too.
            let wheel = scroll.clone();
            div()
                .on_scroll_wheel(cx.listener(move |_, ev: &ScrollWheelEvent, window, cx| {
                    let d = ev.delta.pixel_delta(window.line_height());
                    if d.y.abs() > d.x.abs() {
                        let max = wheel.max_offset().x;
                        let mut offset = wheel.offset();
                        offset.x = (offset.x + d.y).min(px(0.)).max(-max);
                        wheel.set_offset(offset);
                        cx.notify();
                    }
                }))
                .relative()
                .child(bar)
                // A thin bar along the strip's bottom edge while hovered, like VS Code's.
                .when(overflows, |this| {
                    this.child(Scrollbar::horizontal(&scroll).id(("tab-scrollbar", pane as usize)).mode(ScrollbarMode::Hover).styles(|s| {
                        s.track(|t| t.width(px(4.)))
                            .track_hover(|t| t.width(px(4.)))
                            .track_active(|t| t.width(px(4.)))
                            .thumb(|t| t.width(px(3.)).inset(px(0.)))
                            .thumb_hover(|t| t.width(px(4.)).inset(px(0.)))
                            .thumb_active(|t| t.width(px(4.)).inset(px(0.)))
                    }))
                })
                .into_any_element()
        };
        self.with_tab_menu(("tab-bar", pane), bar, false, cx)
    }

    /// Tab `i` as a row of the vertical list. A grouped tab sits in from a
    /// line in its group's color that runs down all the group's tabs.
    fn vertical_tab(&self, i: usize, cx: &mut Context<Self>) -> Stateful<Div> {
        let (label, name, badge) = self.tab_info(i, cx);
        let icon = self.tab_icon(i, None, cx);
        let look = style::el("vertical_tabs.row", cx);
        let (hover, selected_bg, drop_bg) = (look.color("hover_background"), look.color("selected_background"), cx.theme().list_hover);
        let selected = self.pane_active_ix(self.pane_of(i)) == Some(i);
        let join = self.group_of(i);
        let line = join.and_then(|g| Some((self.group(g)?, self.group_tabs(g)?))).map(|(g, r)| {
            // Over the gap to the next row, so the rows' pieces meet.
            let mut piece = div().absolute().left(px(4.)).w(px(3.)).top(px(-1.)).bottom(px(-1.)).bg(group_color(g.color));
            if i == r.start {
                piece = piece.top_0().rounded_t_full();
            }
            if i + 1 == r.end {
                piece = piece.bottom_0().rounded_b_full();
            }
            piece
        });
        let drag = DraggedTab { tab: self.tabs[i].entity_id(), name };
        div()
            .id(("vtab", i))
            .group("vtab")
            .relative()
            .flex_shrink_0()
            .h(look.len("height"))
            .flex()
            .when(join.is_some(), |d| d.pl(look.len("group_indent")))
            .children(line)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap(look.len("gap"))
                    .pl(look.len("padding_left"))
                    .pr(look.len("padding_right"))
                    .rounded(look.len("radius"))
                    .text_size(look.len("font_size"))
                    .text_color(look.color("text"))
                    .map(|d| if selected { d.bg(selected_bg) } else { d.hover(|s| s.bg(hover)) })
                    .children(icon)
                    .when_some(badge.filter(|(b, _)| !b.is_empty()), |d, (b, color)| d.child(div().text_color(color).child(b.trim())))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .when(self.is_preview(i, cx), |d| d.italic())
                            .child(label),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .when(!selected, |d| d.opacity(0.))
                            .group_hover("vtab", |s| s.opacity(1.))
                            .child(self.close_button(i, None, cx)),
                    ),
            )
            .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                if ev.click_count() > 1 {
                    this.pin_tab(i, cx);
                }
                this.activate(i, window, cx);
            }))
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, _| this.menu_tab = Some(i)))
            .on_drag(drag, self.start_drag(cx))
            .drag_over::<DraggedTab>(move |s, _, _, _| s.bg(drop_bg))
            .drag_over::<DraggedGroup>(move |s, _, _, _| s.bg(drop_bg))
            .on_drop(cx.listener(move |this, d: &DraggedTab, window, cx| this.move_tab(d.tab, i, join, window, cx)))
            .on_drop(cx.listener(move |this, d: &DraggedGroup, _, cx| this.move_group(d.group, i, cx)))
    }

    /// Group `g`'s header row in the vertical list: a bar in its color with
    /// its name (and tab count when collapsed) and a chevron, like Chrome's.
    fn vertical_chip(&self, g: &TabGroup, cx: &mut Context<Self>) -> Stateful<Div> {
        let id = g.id;
        let color = group_color(g.color);
        let look = style::el("vertical_tabs.group", cx);
        let (fg, drop_bg) = (look.color("text"), cx.theme().list_hover);
        let drag = DraggedGroup { group: id, name: g.name.clone(), color };
        let count = g.collapsed.then(|| self.group_tabs(id).map_or(0, |r| r.len()));
        let chevron = if g.collapsed { IconName::ChevronDown } else { IconName::ChevronUp };
        look.apply(div().id(("vgroup", id as usize)))
            .flex_shrink_0()
            .flex()
            .items_center()
            .bg(color)
            .hover(|s| s.opacity(0.9))
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(g.name.clone()))
            .children(count.map(|n| div().flex_shrink_0().text_xs().child(n.to_string())))
            .child(Icon::new(chevron).small().text_color(fg))
            .on_click(cx.listener(move |this, _, window, cx| this.toggle_group(id, window, cx)))
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, _| this.menu_group = Some(id)))
            .on_drag(drag, self.start_drag(cx))
            .drag_over::<DraggedTab>(move |s, _, _, _| s.bg(drop_bg))
            .drag_over::<DraggedGroup>(move |s, _, _, _| s.bg(drop_bg))
            .on_drop(cx.listener(move |this, d: &DraggedTab, window, cx| this.move_tab_to_group(d.tab, id, window, cx)))
            .on_drop(cx.listener(move |this, d: &DraggedGroup, _, cx| {
                if let Some(r) = this.group_tabs(id) {
                    this.move_group(d.group, r.start, cx);
                }
            }))
    }

    /// Pane `pane`'s tabs as a list down its left, like Edge's or Arc's
    /// vertical tabs.
    fn render_vertical_tabs(&self, pane: PaneId, cx: &mut Context<Self>) -> impl IntoElement {
        let slots = self.slots_in(pane);
        let shown = self.pane_active_ix(pane);
        let active_slot = slots.iter().position(|s| Some(*s) == shown.map(Slot::Tab)).unwrap_or(0);
        let scroll = self.ui(pane, |u| u.vtab_scroll.clone());
        if self.ui(pane, |u| u.revealed.replace(active_slot)) != Some(active_slot) {
            scroll.scroll_to_item(active_slot);
        }
        let rows: Vec<AnyElement> = slots
            .iter()
            .filter_map(|s| match *s {
                Slot::Group(g) => Some(self.vertical_chip(self.group(g)?, cx).into_any_element()),
                Slot::Tab(i) => Some(self.vertical_tab(i, cx).into_any_element()),
            })
            .collect();
        let end = self.end_drop_zone("vtabs-end", pane, cx).flex_1().min_h(px(40.));
        let list = style::el("vertical_tabs.list", cx)
            .apply(div().id(("vtabs", pane as usize)).flex_1().min_h_0().overflow_y_scroll().track_scroll(&scroll).flex().flex_col())
            .children(rows)
            .child(end);
        let header = style::el("vertical_tabs.header", cx)
            .apply(div().flex_shrink_0().flex().items_center())
            .child(self.new_tab_button(pane, cx))
            .child(div().flex_1())
            .child(self.pane_buttons(pane, cx));
        let body =
            style::el("vertical_tabs", cx).container(div().size_full().flex().flex_col()).child(header).child(list).into_any_element();
        self.with_tab_menu(("vertical-tabs", pane), body, true, cx)
    }

    /// Right-click menu of tab `ix`.
    fn tab_menu(menu: PopupMenu, ws: &WeakEntity<Self>, ix: usize, this: &Self, cx: &App) -> PopupMenu {
        type Handler = fn(&mut Workspace, usize, &mut Window, &mut Context<Workspace>);
        let item = |label: &'static str, f: Handler| {
            let ws = ws.clone();
            PopupMenuItem::new(label).on_click(move |_, window, cx| {
                let _ = ws.update(cx, |this, cx| f(this, ix, window, cx));
            })
        };
        let is_file = |t: &CenterTab| !matches!(t, CenterTab::Term(_));
        let tab = &this.tabs[ix];
        let path = tab.file_path(cx);
        let siblings = this.pane_tabs(this.pane_of(ix));
        let mut menu = menu
            .item(item("Close", Self::close_tab))
            .item(item("Close Others", Self::close_others).disabled(siblings.len() < 2))
            .item(item("Close to the Right", Self::close_to_right).disabled(siblings.last().is_none_or(|&l| l <= ix)))
            // Terminals stay, so this never empties the window.
            .item(item("Close File Tabs", Self::close_file_tabs).disabled(this.tabs.iter().all(is_file) || !this.tabs.iter().any(is_file)))
            .separator()
            .item(item("Add Tab to New Group", Self::add_to_new_group));
        let own = this.group_of(ix);
        for g in this.groups.iter().filter(|g| Some(g.id) != own) {
            let (ws, id, color) = (ws.clone(), g.id, group_color(g.color));
            let label = format!("Add Tab to {}", g.title());
            menu = menu.item(
                PopupMenuItem::element(move |_, _| {
                    div().flex().items_center().gap_2().child(div().size(px(8.)).rounded_full().bg(color)).child(label.clone())
                })
                .on_click(move |_, _, cx| {
                    let _ = ws.update(cx, |this, cx| this.add_to_group(ix, id, cx));
                }),
            );
        }
        if own.is_some() {
            menu = menu.item(item("Remove from Group", Self::remove_from_group));
        }
        menu = menu.separator();
        if matches!(tab, CenterTab::Term(_) | CenterTab::Scratch { .. }) {
            menu = menu.item(item("Rename…", Self::rename_tab));
        }
        if matches!(tab, CenterTab::Diff(_)) && path.as_ref().is_some_and(|p| p.is_file()) {
            menu = menu.item(item("Open File", Self::open_tab_file));
        }
        if path.is_some() {
            menu = menu.item(item("Copy Path", Self::copy_tab_path)).item(item(crate::platform::REVEAL, Self::reveal_tab_file));
        }
        menu = menu
            .separator()
            .item(item("Split Right", |this, ix, w, cx| this.split(ix, Edge::Right, w, cx)))
            .item(item("Split Left", |this, ix, w, cx| this.split(ix, Edge::Left, w, cx)))
            .item(item("Split Down", |this, ix, w, cx| this.split(ix, Edge::Bottom, w, cx)))
            .item(item("Split Up", |this, ix, w, cx| this.split(ix, Edge::Top, w, cx)));
        tab_layout_items(menu.separator().item(PopupMenuItem::label("Tab Layout")))
    }

    /// Right-click menu of group `g`'s chip.
    fn group_menu(menu: PopupMenu, ws: &WeakEntity<Self>, g: u64, this: &Self) -> PopupMenu {
        type Handler = fn(&mut Workspace, u64, &mut Window, &mut Context<Workspace>);
        let item = |label: &'static str, f: Handler| {
            let ws = ws.clone();
            PopupMenuItem::new(label).on_click(move |_, window, cx| {
                let _ = ws.update(cx, |this, cx| f(this, g, window, cx));
            })
        };
        let Some(group) = this.group(g) else { return menu };
        let mut menu = menu
            .item(item("Name Group…", Self::rename_group))
            .item(item("New Claude Tab in Group", |this, g, w, cx| this.new_tab_in_group(TabKind::Claude, g, w, cx)))
            .item(item("New Shell Tab in Group", |this, g, w, cx| this.new_tab_in_group(TabKind::Shell, g, w, cx)))
            .item(item(if group.collapsed { "Expand Group" } else { "Collapse Group" }, Self::toggle_group))
            .separator();
        for (ix, (_, label, _)) in GROUP_COLORS.iter().enumerate() {
            let (ws, color) = (ws.clone(), group_color(ix));
            menu = menu.item(
                PopupMenuItem::element(move |_, _| {
                    div().flex().items_center().gap_2().child(div().size(px(10.)).rounded_full().bg(color)).child(*label)
                })
                .checked(group.color == ix)
                .on_click(move |_, _, cx| {
                    let _ = ws.update(cx, |this, cx| this.set_group_color(g, ix, cx));
                }),
            );
        }
        menu.separator()
            .item(item("Ungroup", Self::ungroup))
            .item(item("Close Group", Self::close_group).disabled(this.group_tabs(g).is_some_and(|r| r.len() == this.tabs.len())))
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
            let (field, input, ws, apply) = (input.clone(), input.clone(), ws.clone(), apply.clone());
            dialog.w(style::dialog_width(false, cx)).title(title).content(move |c, _, _| c.child(Input::new(&field))).footer(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(Button::new("rename-cancel").small().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(Button::new("rename-ok").small().primary().label("OK").on_click(move |_, window, cx| {
                        let name = input.read(cx).value().to_string();
                        let _ = ws.update(cx, |this, cx| apply(this, &name, window, cx));
                    })),
            )
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
            let (field, input) = (input.clone(), input.clone());
            dialog
                .w(style::dialog_width(true, cx))
                .title("Claude Arguments")
                .content(move |c, _, cx| {
                    let muted = cx.theme().muted_foreground;
                    c.child(Input::new(&field))
                        .child(div().pt_2().text_xs().text_color(muted).child("Passed to every new Claude tab, in shell syntax."))
                })
                .footer(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(Button::new("args-cancel").small().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx)))
                        .child(Button::new("args-ok").small().primary().label("Save").on_click(move |_, window, cx| {
                            window_state::set_claude_args(&input.read(cx).value());
                            window.close_dialog(cx);
                        })),
                )
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
        let mut out: Vec<(String, String)> =
            self.arrangements.iter().filter(|(k, _)| self.touched.contains(*k)).map(|(k, a)| (k.clone(), a.encode())).collect();
        out.sort();
        out
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
            self.arrange_problems = match self.arrangements.get_mut(&key).filter(|_| self.touched.contains(&key)) {
                Some(a) => {
                    let parts = templates.parts();
                    let parts: Vec<(&str, &str)> = parts.iter().map(|(n, k)| (*n, k.as_str())).collect();
                    a.reconcile(&docks, &parts)
                },
                // Untouched: the defaults of the templates now in effect.
                None => {
                    self.arrangements.insert(key.clone(), Arrangement::defaults(&key, &docks));
                    vec![]
                },
            };
        }
        self.arrangements.get(&key).cloned().map(Arc::new)
    }

    /// The user moved or resized a part: keep it in the arrangement.
    fn arrangement_changed(&mut self, change: Change, cx: &mut Context<Self>) {
        let Some((_, key)) = &self.arranged_for else { return };
        let Some(a) = self.arrangements.get_mut(key) else { return };
        match change {
            Change::Move { part, kind, dock, at } => {
                let docks = cx.try_global::<template::Templates>().map(|t| t.docks("workspace")).unwrap_or_default();
                if !a.move_part(&part, &kind, &dock, at, &docks) {
                    return;
                }
                // A part moved by hand is one the user wants to see.
                match &*part {
                    "explorer" => self.show_explorer = true,
                    "scratches" => self.show_scratches = true,
                    "changes" => self.show_changes = true,
                    _ => {},
                }
            },
            Change::Resize { dock, path, sizes } => a.set_sizes(&dock, &path, &sizes),
            Change::Width { dock, width } => {
                a.widths.insert(dock.to_string(), width);
            },
        }
        self.touched.insert(key.clone());
        self.panels_changed(cx);
    }

    /// A side panel was shown or hidden: keep it so at the next launch.
    fn panels_changed(&mut self, cx: &mut Context<Self>) {
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
    fn start_drag<T: Clone + Render>(
        &self,
        cx: &mut Context<Self>,
    ) -> impl Fn(&T, Point<Pixels>, &mut Window, &mut App) -> Entity<T> + 'static {
        let ws = cx.entity().downgrade();
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

    /// Pane `pane`'s buttons at the end of its tab strip: split, maximize,
    /// and, on the top right pane, the pane layout menu.
    fn pane_buttons(&self, pane: PaneId, cx: &mut Context<Self>) -> Div {
        let multi = self.layout.panes().len() > 1;
        let zoomed = self.zoomed == Some(pane);
        let corner = self.zoomed.unwrap_or_else(|| self.layout.top_right()) == pane;
        div()
            .flex()
            .items_center()
            .pl_1()
            .child(
                style::el("pane_tab_bar.button", cx)
                    .apply(Button::new(("pane-split", pane as usize)))
                    .xsmall()
                    .ghost()
                    .icon(Icon::new(AssetIcon::Columns2))
                    .tooltip(format!("Split Right ({})", crate::platform::shortcut("⌘\\")))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.focus_pane_state(pane);
                        let ix = this.pane_active_ix(pane).unwrap_or(this.active);
                        this.split(ix, Edge::Right, window, cx);
                    })),
            )
            .when(multi, |d| {
                d.child(
                    style::el("pane_tab_bar.button", cx)
                        .apply(Button::new(("pane-zoom", pane as usize)))
                        .xsmall()
                        .ghost()
                        .icon(Icon::new(if zoomed { AssetIcon::Minimize2 } else { AssetIcon::Maximize2 }))
                        .selected(zoomed)
                        .tooltip(if zoomed {
                            format!("Restore Panes ({})", crate::platform::shortcut("⌘⇧↩"))
                        } else {
                            format!("Maximize Pane ({})", crate::platform::shortcut("⌘⇧↩"))
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.focus_pane(pane, window, cx);
                            this.toggle_zoom(cx);
                        })),
                )
                .child(
                    style::el("pane_tab_bar.button", cx)
                        .apply(Button::new(("pane-close", pane as usize)))
                        .xsmall()
                        .ghost()
                        .icon(IconName::Close)
                        .tooltip("Close Pane (its tabs join the pane before it)")
                        .on_click(cx.listener(move |this, _, window, cx| this.close_pane(pane, window, cx))),
                )
            })
            .when(corner, |d| d.child(self.panel_buttons(cx)))
    }

    /// The pane layouts (VS Code's Editor Layout, tmux's tiled), for menus.
    fn pane_layout_items(mut menu: PopupMenu, ws: &WeakEntity<Self>, this: &Self) -> PopupMenu {
        let order = this.layout.panes();
        for (cols, rows, label) in panes::PRESETS {
            let ws = ws.clone();
            let checked = order.len() == cols * rows && this.layout == Layout::grid(cols, rows, &order);
            menu = menu.item(PopupMenuItem::new(label).checked(checked).on_click(move |_, window, cx| {
                let _ = ws.update(cx, |this, cx| this.apply_layout(cols, rows, window, cx));
            }));
        }
        let ws = ws.clone();
        menu.separator()
            .menu("Tile All Tabs", Box::new(TileTabs))
            .menu("Split Right", Box::new(SplitRight))
            .menu("Split Down", Box::new(SplitDown))
            .menu("Maximize Pane", Box::new(ToggleZoomPane))
            .item(PopupMenuItem::new("Even Pane Sizes").disabled(order.len() < 2).on_click(move |_, _, cx| {
                let _ = ws.update(cx, |this, cx| {
                    this.layout_epoch += 1;
                    cx.notify();
                });
            }))
    }

    /// The center's split `node`; `path` names its resizable groups.
    fn render_layout(&self, node: &Layout, path: &str, cx: &mut Context<Self>) -> AnyElement {
        match node {
            Layout::Pane(p) => self.render_pane(*p, cx),
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
            Some(CenterTab::Term(t)) => t.view.clone().into_any_element(),
            Some(CenterTab::File { view, .. }) => view.clone().into_any_element(),
            Some(CenterTab::Diff(d)) => d.clone().into_any_element(),
            Some(CenterTab::Scratch { view: s, .. }) => s.clone().into_any_element(),
            None => self.empty_pane(pane, cx).into_any_element(),
        };
        let scope = template::Scope::new(format!("pane-{pane}"))
            .var("focused", has_focus)
            .var("focus_line", focus_line)
            .var("vertical", vertical)
            .var("title", title)
            .var("tabs", self.pane_tabs(pane).len())
            .slot("content", content)
            .slot_opt("drop_overlay", (self.tab_drag && cx.has_active_drag()).then(|| self.drop_overlay(pane, cx)));
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
            &mut |frame, _, d| match (frame, focus.take()) {
                ("pane", Some(f)) => d.id(("pane", pane as usize)).capture_any_mouse_down(f).into_any_element(),
                _ => d.into_any_element(),
            },
            cx,
        )
    }

    /// What an empty pane (from a layout preset) shows.
    fn empty_pane(&self, pane: PaneId, cx: &mut Context<Self>) -> Div {
        let look = style::el("pane.empty", cx);
        let new = move |kind| {
            move |this: &mut Self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>| {
                this.focus_pane_state(pane);
                this.add_tab(kind, window, cx);
            }
        };
        look.apply(div().size_full().flex().flex_col().items_center().justify_center().overflow_hidden())
            .child(div().text_center().child("Drag a tab here, or open one"))
            .child(
                // Wraps in a narrow pane rather than spilling into the neighbour.
                div()
                    .max_w_full()
                    .flex()
                    .flex_wrap()
                    .justify_center()
                    .gap_2()
                    .child(
                        Button::new(("pane-claude", pane as usize))
                            .small()
                            .label("New Claude Tab")
                            .on_click(cx.listener(new(TabKind::Claude))),
                    )
                    .child(
                        Button::new(("pane-shell", pane as usize))
                            .small()
                            .label("New Shell Tab")
                            .on_click(cx.listener(new(TabKind::Shell))),
                    )
                    .child(
                        Button::new(("pane-close-empty", pane as usize))
                            .small()
                            .ghost()
                            .label("Close Pane")
                            .on_click(cx.listener(move |this, _, window, cx| this.close_pane(pane, window, cx))),
                    ),
            )
    }

    /// Over pane `pane`'s content while a tab is dragged: the part under
    /// the pointer lights up where the tab would go (VS Code's drop zones).
    fn drop_overlay(&self, pane: PaneId, cx: &mut Context<Self>) -> Div {
        let hint = self.drop_target.filter(|t| t.0 == pane).map(|(_, zone)| {
            let d = style::el("drop_zone", cx).apply(div().absolute());
            match zone {
                Zone::Center => d.inset_0(),
                Zone::Edge(Edge::Left) => d.top_0().bottom_0().left_0().w(relative(0.5)),
                Zone::Edge(Edge::Right) => d.top_0().bottom_0().right_0().w(relative(0.5)),
                Zone::Edge(Edge::Top) => d.left_0().right_0().top_0().h(relative(0.5)),
                Zone::Edge(Edge::Bottom) => d.left_0().right_0().bottom_0().h(relative(0.5)),
            }
        });
        div()
            .absolute()
            .inset_0()
            .on_drag_move(cx.listener(move |this, ev: &DragMoveEvent<DraggedTab>, _, cx| {
                let (b, at) = (ev.bounds, ev.event.position);
                let next = if b.contains(&at) {
                    let x = f32::from(at.x - b.left()) / f32::from(b.size.width).max(1.);
                    let y = f32::from(at.y - b.top()) / f32::from(b.size.height).max(1.);
                    Some((pane, Zone::at(x, y)))
                } else if this.drop_target.is_some_and(|t| t.0 == pane) {
                    None
                } else {
                    return;
                };
                if this.drop_target != next {
                    this.drop_target = next;
                    cx.notify();
                }
            }))
            .on_drop(cx.listener(move |this, d: &DraggedTab, window, cx| {
                let zone = this.drop_target.filter(|t| t.0 == pane).map_or(Zone::Center, |t| t.1);
                this.drop_target = None;
                this.tab_drag = false;
                this.drop_in_pane(d.tab, pane, zone, window, cx);
            }))
            .children(hint)
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
            // What dropping PART in DOCK does: before or after TARGET, or at its end.
            cmd if cmd.starts_with("part-move ") => {
                let args: Vec<&str> = cmd.split_whitespace().skip(1).collect();
                let at = match (args.get(2).copied(), args.get(3)) {
                    (Some("before"), Some(t)) => arrange::Drop::Before((*t).to_owned()),
                    (Some("after"), Some(t)) => arrange::Drop::After((*t).to_owned()),
                    (Some("end"), None) => arrange::Drop::End,
                    _ => return "error: part-move PART DOCK before|after|end [TARGET]".into(),
                };
                let (part, dock) = (args[0], args[1]);
                let kind =
                    cx.try_global::<template::Templates>().and_then(|t| t.parts().into_iter().find(|(n, _)| *n == part).map(|(_, k)| k));
                let Some(kind) = kind else { return format!("error: no part {part:?}") };
                let change = Change::Move { part: part.to_owned().into(), kind: kind.into(), dock: dock.to_owned().into(), at };
                self.arrangement_changed(change, cx);
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

fn notify_system(title: &str, body: &str, cx: &mut App) {
    cx.show_system_notification(SystemNotification {
        tag: "tern".into(),
        title: title.to_owned().into(),
        body: body.to_owned().into(),
        actions: vec![],
    });
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = window;
        // A drag that ended outside every drop target leaves these behind.
        if !cx.has_active_drag() {
            self.tab_drag = false;
            self.drop_target = None;
            if cx.try_global::<ActiveDrag>().is_some_and(|d| d.0.is_some()) {
                cx.set_global(ActiveDrag(None));
            }
        }
        self.sync_panes();
        let center = match self.zoomed.filter(|z| self.layout.contains(*z)) {
            Some(z) => self.render_pane(z, cx),
            None => {
                let layout = self.layout.clone();
                self.render_layout(&layout, "0", cx)
            },
        };

        let background = match self.services.mux.server_version() {
            // An update's new UI keeps talking to the old daemon until
            // its terminals end.
            Some(v) if v != env!("CARGO_PKG_VERSION") => format!("background: Tern {v} (updates once its terminals end)"),
            Some(_) if self.services.mux.server_same_build() == Some(false) => {
                "background: an older build (updates once its terminals end)".to_owned()
            },
            Some(_) => "background: ✓".to_owned(),
            None => "background: unavailable".to_owned(),
        };
        let arrangement = self.arrangement(cx);
        let mut settings_error: Vec<String> =
            cx.try_global::<crate::settings::SettingsStatus>().and_then(|s| s.error.as_ref().map(|e| e.to_string())).into_iter().collect();
        settings_error.extend(self.arrange_problems.iter().cloned());
        let settings_error = settings_error.join("; ");
        let (left_strip, right_strip) = (self.strip("left", arrangement.as_deref(), cx), self.strip("right", arrangement.as_deref(), cx));
        let mut scope = template::Scope::new("workspace");
        if let Some(a) = arrangement {
            let ws = cx.entity().downgrade();
            let on_change: OnChange = Rc::new(move |change, _, cx| {
                let _ = ws.update(cx, |this, cx| this.arrangement_changed(change, cx));
            });
            scope = scope.docks(a, on_change);
        }
        let scope = scope
            .var("explorer", self.show_explorer)
            .var("scratches", self.show_scratches)
            .var("changes", self.show_changes)
            .var("path", self.root.display().to_string())
            .var("background", background)
            .var("settings_error", settings_error)
            .slot("left_strip", left_strip)
            .slot("right_strip", right_strip)
            .slot("explorer", self.tree.clone())
            .slot("scratches", self.scratches.clone())
            .slot("changes", self.changes.clone())
            .slot("center", center);
        let body = template::render("workspace", scope, &mut |_, _, d| d.into_any_element(), cx);

        div()
            .key_context("Workspace")
            .size_full()
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
            .on_action(cx.listener(|this, _: &OpenSettings, w, cx| this.open_file(settings::settings_path(cx), true, w, cx)))
            .on_action(cx.listener(Self::toggle_changes))
            .on_action(cx.listener(Self::toggle_scratches))
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
