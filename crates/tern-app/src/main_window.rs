//! A window: its active workspace, whose agents panel lists the window's
//! workspaces (one per folder) with the terminals running an agent, like T3
//! Code's projects and threads; or a welcome page when it has none (like VS
//! Code with no folder open). Workspaces move between windows: drag a folder
//! out of the agents panel onto another Tern window to join it, or anywhere
//! else to give it a window of its own; the folder's menu does the same.

use std::collections::{HashMap, HashSet};

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu};
use gpui_kit::component::{ActiveTheme, WindowExt as _};
use gpui_kit::*;

use crate::platform::bind;
use crate::template::{self, Framed, on_event};
use crate::theme_picker::{self, OpenSettings, OpenSettingsFile, SelectTheme};
use crate::workspace::{Panels, ToggleAgents, Workspace, WorkspaceEvent, confirm_close};
use crate::{AppServices, OpenFolder, window_state};
use tern_term::status::ActivityStatus;

actions!(window, [CloseWorkspace, NextWorkspace, PrevWorkspace, NewWindow]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        bind("cmd-shift-w", CloseWorkspace, None),
        bind("cmd-alt-right", NextWorkspace, None),
        bind("cmd-alt-left", PrevWorkspace, None),
        bind("cmd-shift-n", NewWindow, None),
    ]);
}

/// Every Tern window, in opening order.
#[derive(Default)]
struct Registry(Vec<(AnyWindowHandle, WeakEntity<MainWindow>)>);

impl Global for Registry {}

/// Live windows, dropping entries for closed ones.
/// Every workspace with its window: the frontmost window first, and in
/// each window its shown workspace first.
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))] // for the screenshot tray
pub fn workspaces(cx: &mut App) -> Vec<(AnyWindowHandle, Entity<Workspace>)> {
    let mut wins = windows(cx);
    if let Some(stack) = cx.window_stack() {
        wins.sort_by_key(|(h, _)| stack.iter().position(|s| s == h).unwrap_or(usize::MAX));
    }
    let mut out = Vec::new();
    for (h, m) in wins {
        let m = m.read(cx);
        let mut ws: Vec<_> = m.slots.iter().map(|s| s.ws.clone()).collect();
        if m.active < ws.len() {
            let active = ws.remove(m.active);
            ws.insert(0, active);
        }
        out.extend(ws.into_iter().map(|w| (h, w)));
    }
    out
}

/// Every workspace with its window, in a steady order: windows as they
/// opened, and in each its tabs' order.
pub fn all_workspaces(cx: &mut App) -> Vec<(AnyWindowHandle, Entity<Workspace>)> {
    let mut out = Vec::new();
    for (h, m) in windows(cx) {
        out.extend(m.read(cx).slots.iter().map(|s| (h, s.ws.clone())));
    }
    out
}

/// Bring `ws` forward in its window `handle`, and Tern with it.
pub fn reveal(handle: AnyWindowHandle, ws: &Entity<Workspace>, cx: &mut App) {
    let id = ws.entity_id();
    let main = windows(cx).into_iter().find(|(h, _)| *h == handle).map(|(_, m)| m);
    let _ = handle.update(cx, |_, window, cx| {
        if let Some(main) = main {
            main.update(cx, |m, cx| {
                if let Some(ix) = m.position(id) {
                    m.activate(ix, window, cx);
                }
            });
        }
        window.activate_window();
    });
    cx.activate(true);
}

fn windows(cx: &mut App) -> Vec<(AnyWindowHandle, Entity<MainWindow>)> {
    let open = cx.windows();
    let reg = cx.default_global::<Registry>();
    reg.0.retain(|(h, w)| w.upgrade().is_some() && open.contains(h));
    reg.0.iter().filter_map(|(h, w)| Some((*h, w.upgrade()?))).collect()
}

/// Session bookkeeping: whether to save it, and whether the app is quitting
/// (windows closing then must not shrink the saved session).
#[derive(Default)]
pub struct Session {
    pub persist: bool,
    pub quitting: bool,
}

impl Global for Session {}

/// Save every window and its workspaces, to reopen at the next launch.
pub fn save_session(cx: &mut App) {
    let s = cx.default_global::<Session>();
    if !s.persist || s.quitting {
        return;
    }
    let saved: Vec<window_state::SavedWindow> = windows(cx)
        .into_iter()
        .map(|(_, main)| {
            let m = main.read(cx);
            window_state::SavedWindow {
                placement: m.placement.clone(),
                workspaces: m.slots.iter().map(|s| s.ws.read(cx).saved(cx)).collect(),
                active: m.active,
            }
        })
        .collect();
    let panels = cx.try_global::<Panels>().map(|p| p.saved.clone()).unwrap_or_default();
    window_state::save_session(&saved, &panels);
}

/// Reopen the last session's windows. False when there was none.
pub fn restore_session(cx: &mut App) -> bool {
    let (saved, panels) = window_state::load_session();
    cx.set_global(Panels::new(panels));
    for w in &saved {
        let at = w.placement.as_ref().and_then(|(uuid, bounds)| window_state::resolve(uuid, *bounds, cx));
        open_window(Content::Workspaces(w.workspaces.clone(), w.active), at, cx);
    }
    let known = saved
        .iter()
        .flat_map(|w| &w.workspaces)
        .flat_map(|ws| &ws.tabs)
        .filter_map(|t| match t {
            window_state::SavedTab::Claude { mux, .. } | window_state::SavedTab::Shell { mux, .. } => *mux,
            _ => None,
        })
        .collect();
    adopt_orphans(known, cx);
    !saved.is_empty()
}

/// Background sessions no saved tab knows (the session wasn't saved before
/// a crash): running ones open as tabs in the first window, so nothing runs
/// unseen; ended ones go.
pub fn adopt_orphans(known: std::collections::HashSet<u64>, cx: &mut App) {
    let mux = cx.global::<AppServices>().0.mux.clone();
    // Asking the daemon can take a while: off the UI thread.
    let task = cx.background_spawn(async move {
        let client = mux.existing()?;
        let list = client.list().ok()?;
        let mut live = vec![];
        for s in list.into_iter().filter(|s| !known.contains(&s.id)) {
            if s.exited {
                client.close(s.id);
            } else {
                live.push(s);
            }
        }
        Some(live)
    });
    cx.spawn(async move |cx| {
        if let Some(live) = task.await.filter(|l| !l.is_empty()) {
            cx.update(|cx| adopt(live, cx));
        }
    })
    .detach();
}

fn adopt(live: Vec<tern_mux::proto::SessionInfo>, cx: &mut App) {
    log::info!("adopting {} background sessions", live.len());
    if windows(cx).is_empty() {
        // No window to put them in: open one at the first one's folder.
        let root = live[0].cwd.clone().unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default());
        open_folder(root, cx);
    }
    adopt_into_first(live, cx);
}

fn adopt_into_first(live: Vec<tern_mux::proto::SessionInfo>, cx: &mut App) {
    let Some((handle, main)) = windows(cx).into_iter().next() else { return };
    let _ = handle.update(cx, |_, window, cx| {
        main.update(cx, |m, cx| {
            if let Some(ws) = m.active_ws().cloned() {
                ws.update(cx, |w, cx| {
                    for s in &live {
                        w.adopt_session(s, window, cx);
                    }
                });
            }
        })
    });
}

/// Where Open (from the menu bar) puts session `id`.
#[derive(Debug, PartialEq, Eq)]
pub enum RevealTarget {
    /// A tab shows it already.
    Focus { window: usize, ws: usize, tab: usize },
    /// A new tab in the active window's active workspace.
    AdoptIntoActive,
    /// No window: one opens, with it.
    NewWindow,
    /// It ended meanwhile.
    Nothing,
}

/// `tabs[w][ws]` = the mux ids of window `w`'s workspace `ws`'s tabs
/// (`None` for non-terminal tabs); `known`: the daemon has the session.
pub fn reveal_target(tabs: &[Vec<Vec<Option<u64>>>], id: u64, known: bool) -> RevealTarget {
    if !known {
        return RevealTarget::Nothing;
    }
    for (window, wss) in tabs.iter().enumerate() {
        for (ws, ids) in wss.iter().enumerate() {
            if let Some(tab) = ids.iter().position(|t| *t == Some(id)) {
                return RevealTarget::Focus { window, ws, tab };
            }
        }
    }
    if tabs.is_empty() { RevealTarget::NewWindow } else { RevealTarget::AdoptIntoActive }
}

/// Show background session `id` (asked from the menu bar): focus the tab
/// showing it, else adopt it into the active workspace, else into a new
/// window at its folder. Nothing if it has ended.
pub fn reveal_session(id: u64, cx: &mut App) {
    let info = cx.global::<AppServices>().0.mux.existing().and_then(|c| c.list().ok()).and_then(|l| l.into_iter().find(|s| s.id == id));
    let wins = windows(cx);
    let tabs: Vec<Vec<Vec<Option<u64>>>> =
        wins.iter().map(|(_, m)| m.read(cx).slots.iter().map(|s| s.ws.read(cx).mux_ids(cx)).collect()).collect();
    let target = reveal_target(&tabs, id, info.is_some());
    log::info!("show session {id}: {target:?}");
    match target {
        RevealTarget::Nothing => log::info!("session {id} is gone; nothing to show"),
        RevealTarget::Focus { window: w, ws, tab } => {
            let (handle, main) = wins[w].clone();
            let _ = handle.update(cx, |_, window, cx| {
                main.update(cx, |m, cx| {
                    m.activate(ws, window, cx);
                    if let Some(ws) = m.active_ws().cloned() {
                        ws.update(cx, |w, cx| w.show_tab_at(tab, window, cx));
                    }
                });
                window.activate_window();
            });
        },
        RevealTarget::AdoptIntoActive | RevealTarget::NewWindow => {
            let Some(info) = info else { return };
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            // The active window, else the first (as `open_folder` picks).
            let target = cx.active_window().and_then(|a| wins.iter().find(|(h, _)| *h == a).cloned()).or_else(|| wins.first().cloned());
            let Some((handle, main)) = target else {
                // No window: one at its folder, holding just it.
                open_window(Content::Workspaces(vec![adoption_workspace(&info, &home)], 0), None, cx);
                cx.activate(true);
                return;
            };
            let _ = handle.update(cx, |_, window, cx| {
                main.update(cx, |m, cx| match m.active_ws().cloned() {
                    Some(ws) => ws.update(cx, |w, cx| w.adopt_session(&info, window, cx)),
                    // A welcome page: a workspace at its folder, holding just it.
                    None => {
                        let services = cx.global::<AppServices>().0.clone();
                        let ws = cx.new(|cx| Workspace::new(adoption_workspace(&info, &home), services, window, cx));
                        m.insert(ws, None, window, cx);
                    },
                });
                window.activate_window();
            });
        },
    }
    cx.activate(true);
}

/// A workspace at session `s`'s folder (`home` without one) whose only tab
/// shows it: no default Claude tab beside it.
pub fn adoption_workspace(s: &tern_mux::proto::SessionInfo, home: &Path) -> window_state::SavedWorkspace {
    let tab = if s.claude {
        window_state::SavedTab::Claude { name: None, session: s.claude_session.clone(), mux: Some(s.id), account: None, guard: None }
    } else {
        window_state::SavedTab::Shell { name: s.title.clone(), mux: Some(s.id) }
    };
    window_state::SavedWorkspace {
        tabs: vec![tab],
        ..window_state::SavedWorkspace::fresh(s.cwd.clone().unwrap_or_else(|| home.to_owned()))
    }
}

/// What a new window starts with.
pub enum Content {
    Welcome,
    /// Workspaces to open, and which one is active.
    Workspaces(Vec<window_state::SavedWorkspace>, usize),
    Workspace(Entity<Workspace>),
}

/// Open a window. With `at`, it is placed there; otherwise where the last
/// window was, cascaded when another window is open.
pub fn open_window(content: Content, at: Option<(DisplayId, WindowBounds)>, cx: &mut App) {
    let (display_id, window_bounds) = match at {
        Some((display, bounds)) => (Some(display), bounds),
        None => {
            let (display, mut bounds) = match window_state::load(cx) {
                Some((display, bounds)) => (Some(display), bounds),
                None => (None, WindowBounds::Windowed(Bounds::centered(None, size(px(1440.), px(900.)), cx))),
            };
            if !cx.windows().is_empty()
                && let WindowBounds::Windowed(b) = &mut bounds
            {
                b.origin += point(px(30.), px(30.));
            }
            (display, bounds)
        },
    };
    let opened = gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(window_bounds),
            display_id,
            // The window draws its own title bar (`render_title_bar`).
            titlebar: Some(TitlebarOptions { title: Some("Tern".into()), ..gpui_kit::component::TitleBar::title_bar_options() }),
            app_owns_titlebar_drag: true,
            window_background: crate::style::backdrop(cx).appearance,
            // Matches the Linux desktop entry (packaging/linux), for its icon.
            app_id: Some("dev.tern.Tern".into()),
            ..Default::default()
        },
        cx,
        move |window, cx| {
            cx.new(|cx| {
                let mut this = MainWindow::new(window, cx);
                match content {
                    Content::Welcome => {},
                    Content::Workspaces(saved, active) => {
                        let services = cx.global::<AppServices>().0.clone();
                        for saved in saved {
                            let services = services.clone();
                            let ws = cx.new(|cx| Workspace::new(saved, services, window, cx));
                            this.insert(ws, None, window, cx);
                        }
                        this.activate(active, window, cx);
                    },
                    Content::Workspace(ws) => this.adopt(ws, None, window, cx),
                }
                this
            })
        },
    );
    match opened {
        Ok((handle, main)) => {
            cx.default_global::<Registry>().0.push((handle, main.downgrade()));
            let _ = handle.update(cx, |_, window, _| window.activate_window());
        },
        Err(e) => log::error!("failed to open window: {e}"),
    }
}

/// `settings.json` in a tab of the active Tern window (else the first).
pub fn open_settings_file(cx: &mut App) {
    let active = cx.active_window();
    let all = windows(cx);
    let Some((handle, _)) = all.iter().find(|(h, _)| Some(*h) == active).or(all.first()) else {
        cx.open_with_system(&crate::settings::settings_path(cx));
        return;
    };
    let _ = handle.update(cx, |_, window, cx| {
        window.activate_window();
        window.dispatch_action(Box::new(OpenSettingsFile), cx);
    });
}

/// Open `root` in the active window as a new workspace tab (or focus it
/// wherever it is already open); a new window when no window is open.
pub fn open_folder(root: PathBuf, cx: &mut App) {
    let target = cx.active_window().and_then(|a| windows(cx).into_iter().find(|(h, _)| *h == a)).or_else(|| windows(cx).into_iter().next());
    match target {
        Some((handle, main)) => {
            let _ = handle.update(cx, |_, window, cx| main.update(cx, |m, cx| m.open_folder(root, window, cx)));
        },
        None => {
            let root = dunce::canonicalize(&root).unwrap_or(root);
            window_state::push_recent(&root);
            crate::menus::install(cx);
            open_window(Content::Workspaces(vec![window_state::SavedWorkspace::fresh(root)], 0), None, cx)
        },
    }
}

/// Ask for a folder and open it as a workspace.
pub fn prompt_open_folder(cx: &mut App) {
    let rx = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Open".into()) });
    cx.spawn(async move |cx| {
        let picked = rx.await.ok().and_then(Result::ok).flatten().and_then(|p| p.into_iter().next());
        if let Some(dir) = picked {
            cx.update(|cx| open_folder(dir, cx));
        }
    })
    .detach();
}

/// Hand a Claude hook event to every workspace; each keeps its own tabs'.
pub fn dispatch_hook(ev: tern_bridge::HookEvent, cx: &mut App) {
    for (handle, main) in windows(cx) {
        let _ = handle.update(cx, |_, window, cx| {
            for ws in main.read(cx).slots.iter().map(|s| s.ws.clone()).collect::<Vec<_>>() {
                ws.update(cx, |w, cx| w.on_hook(ev.clone(), window, cx));
            }
        });
    }
}

/// What a box of the agents panel does, on its window even when the panel
/// is in a window of its own (which then gives way to it).
fn on_main<E: ?Sized + 'static>(
    me: &WeakEntity<MainWindow>,
    f: impl Fn(&mut MainWindow, &E, &mut Window, &mut Context<MainWindow>) + 'static,
) -> impl Fn(&E, &mut Window, &mut App) + 'static {
    let me = me.clone();
    move |e, window, cx| {
        let Some(main) = me.upgrade() else { return };
        let handle = main.read(cx).handle;
        if window.window_handle() == handle {
            main.update(cx, |this, cx| f(this, e, window, cx));
        } else {
            let _ = handle.update(cx, |_, window, cx| {
                window.activate_window();
                main.update(cx, |this, cx| f(this, e, window, cx));
            });
        }
    }
}

/// The agents panel of a window, drawn by its `MainWindow` wherever a
/// workspace docks it.
pub struct AgentsView {
    main: WeakEntity<MainWindow>,
    /// Drawn again with its window, also when in a window of its own.
    _sub: Option<Subscription>,
}

impl Render for AgentsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.main.upgrade() {
            Some(main) => main.update(cx, |m, cx| m.render_agents(cx)),
            None => div().into_any_element(),
        }
    }
}

struct Slot {
    ws: Entity<Workspace>,
    _subs: [Subscription; 2],
}

/// The payload of a workspace tab drag.
#[derive(Clone)]
struct DraggedWorkspace {
    ws: EntityId,
    name: SharedString,
}

impl Render for DraggedWorkspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::style::drag_chip(self.name.clone(), Some("folder-open"), cx)
    }
}

pub struct MainWindow {
    /// Closed by the user (not by quitting): its terminals end with it.
    ends_terminals: bool,
    handle: AnyWindowHandle,
    /// The agents panel its workspaces draw.
    agents: Entity<AgentsView>,
    slots: Vec<Slot>,
    active: usize,
    /// The workspaces whose agents the agents panel hides.
    collapsed: HashSet<EntityId>,
    /// The workspace whose tab is being dragged.
    dragging: Option<EntityId>,
    /// The agents panel's list, and the workspace last scrolled into view.
    tab_scroll: ScrollHandle,
    revealed: Option<usize>,
    title: String,
    /// What the desktop does behind the window, as last set.
    backdrop: crate::style::Backdrop,
    /// Where the window is, for the saved session.
    placement: Option<(String, WindowBounds)>,
    focus: FocusHandle,
    /// Keeps the window's bounds and activation observed.
    _subs: Vec<Subscription>,
    _save_placement: Task<()>,
    /// The agents panel's next redraw for its times: in a second (while an
    /// agent works) or in half a minute.
    _tick: Option<(bool, Task<()>)>,
}

impl MainWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // A drag or a resize sends many: kept once they stop.
        let bounds_sub = cx.observe_window_bounds(window, |this: &mut Self, window, cx| {
            this.placement = window_state::placement(window, cx);
            this._save_placement = cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(400)).await;
                let _ = this.update(cx, |this, cx| {
                    if let Some((uuid, bounds)) = &this.placement {
                        window_state::save_placement(uuid, *bounds);
                    }
                    this.changed(cx);
                });
            });
        });
        // The agent's own menu may have hidden it meanwhile.
        let activation_sub = cx.observe_window_activation(window, |_: &mut Self, window, cx| {
            if window.is_window_active() {
                crate::menus::install(cx);
                crate::dock_badge::clear(cx);
            }
        });
        // The close button asks first when that ends running programs.
        let me = cx.entity().downgrade();
        let confirmed = Rc::new(Cell::new(false));
        window.on_window_should_close(cx, move |window, cx| {
            // The last window going quits Tern: like ⌘Q, its terminals keep
            // running and it comes back at the next launch.
            if windows(cx).len() <= 1 {
                return true;
            }
            let jobs = me.upgrade().map(|m| m.read(cx).running(cx)).unwrap_or_default();
            if jobs.is_empty() || confirmed.get() {
                // Closed by the user: its terminals end with it.
                if let Some(me) = me.upgrade() {
                    me.update(cx, |m, _| m.ends_terminals = true);
                }
                return true;
            }
            let Some(me) = me.upgrade() else { return true };
            let confirmed = confirmed.clone();
            me.update(cx, |_, cx| {
                confirm_close(jobs, "this window", window, cx, move |this: &mut Self, window, _| {
                    confirmed.set(true);
                    this.ends_terminals = true;
                    window.remove_window();
                })
            });
            false
        });
        cx.on_release(|this: &mut Self, cx| {
            let end = this.ends_terminals;
            for slot in &this.slots {
                slot.ws.update(cx, |w, cx| {
                    w.shutdown(cx);
                    if end {
                        w.end_terminals(cx);
                    }
                });
            }
        })
        .detach();
        let main = cx.entity();
        let agents = cx.new(|cx| AgentsView { main: main.downgrade(), _sub: Some(cx.observe(&main, |_, _, cx| cx.notify())) });
        Self {
            ends_terminals: false,
            handle: window.window_handle(),
            agents,
            slots: vec![],
            active: 0,
            collapsed: HashSet::new(),
            dragging: None,
            tab_scroll: ScrollHandle::new(),
            revealed: None,
            title: String::new(),
            backdrop: crate::style::Backdrop::default(),
            placement: window_state::placement(window, cx),
            focus: cx.focus_handle(),
            _subs: vec![bounds_sub, activation_sub],
            _save_placement: Task::ready(()),
            _tick: None,
        }
    }

    /// Workspaces or placement changed: save the session once this update ends.
    fn changed(&self, cx: &mut Context<Self>) {
        cx.defer(save_session);
    }

    fn active_ws(&self) -> Option<&Entity<Workspace>> {
        self.slots.get(self.active).map(|s| &s.ws)
    }

    fn position(&self, id: EntityId) -> Option<usize> {
        self.slots.iter().position(|s| s.ws.entity_id() == id)
    }

    /// Open `root` as a new workspace here, unless some window already has
    /// it open: then that one comes forward.
    pub fn open_folder(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let root = dunce::canonicalize(&root).unwrap_or(root);
        window_state::push_recent(&root);
        crate::menus::install(cx);
        if let Some(ix) = self.slots.iter().position(|s| s.ws.read(cx).root() == root) {
            return self.activate(ix, window, cx);
        }
        let me = cx.entity_id();
        for (handle, main) in windows(cx) {
            if main.entity_id() == me {
                continue;
            }
            if let Some(ix) = main.read(cx).slots.iter().position(|s| s.ws.read(cx).root() == root) {
                let _ = handle.update(cx, |_, window, cx| {
                    window.activate_window();
                    main.update(cx, |m, cx| m.activate(ix, window, cx));
                });
                return;
            }
        }
        let services = cx.global::<AppServices>().0.clone();
        let ws = cx.new(|cx| Workspace::new(window_state::SavedWorkspace::fresh(root), services, window, cx));
        self.insert(ws, None, window, cx);
    }

    fn insert(&mut self, ws: Entity<Workspace>, at: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let sub = cx.subscribe_in(&ws, window, |this, _, ev: &WorkspaceEvent, _, cx| match ev {
            WorkspaceEvent::TabsChanged => {
                // A closed tab may have been the last working agent.
                crate::caffeine::settle(cx);
                this.changed(cx)
            },
        });
        // The agents panel shows its tabs' titles and activity.
        let agents = self.agents.clone();
        let observe = cx.observe(&ws, move |_, _, cx| agents.update(cx, |_, cx| cx.notify()));
        ws.update(cx, |w, _| w.set_agents(self.agents.clone().into()));
        let ix = at.unwrap_or(self.slots.len()).min(self.slots.len());
        self.slots.insert(ix, Slot { ws, _subs: [sub, observe] });
        self.activate(ix, window, cx);
    }

    /// Take in a workspace that was showing in another window.
    fn adopt(&mut self, ws: Entity<Workspace>, at: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        ws.update(cx, |w, cx| w.rehost(window, cx));
        self.insert(ws, at, window, cx);
    }

    /// Remove workspace `ix` without dropping it (it is moving elsewhere).
    fn take(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<Workspace>> {
        if ix >= self.slots.len() {
            return None;
        }
        // Dialogs (diff preview, rename) belong to the workspace leaving.
        window.close_all_dialogs(cx);
        let slot = self.slots.remove(ix);
        if ix < self.active || self.active >= self.slots.len() {
            self.active = self.active.saturating_sub(1);
        }
        self.focus_active(window, cx);
        self.changed(cx);
        cx.notify();
        Some(slot.ws)
    }

    pub fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix < self.slots.len() {
            self.active = ix;
            self.focus_active(window, cx);
            self.changed(cx);
            cx.notify();
        }
    }

    fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.active_ws() {
            Some(ws) => ws.update(cx, |w, cx| w.focus(window, cx)),
            None => window.focus(&self.focus, cx),
        }
    }

    /// Close workspace `ix`: its terminals end. The window stays, showing
    /// the welcome page when it was the last.
    fn close(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ws) = self.take(ix, window, cx) {
            ws.update(cx, |w, cx| {
                w.shutdown(cx);
                w.end_terminals(cx);
            });
        }
    }

    /// Close workspace `ix`, asking first when that ends running programs.
    fn request_close(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.slots.get(ix).map(|s| s.ws.clone()) else { return };
        let jobs = ws.read(cx).running(cx);
        if jobs.is_empty() {
            return self.close(ix, window, cx);
        }
        confirm_close(jobs, "this workspace", window, cx, move |this: &mut Self, window, cx| {
            if let Some(ix) = this.position(ws.entity_id()) {
                this.close(ix, window, cx);
            }
        });
    }

    fn request_close_others(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(keep) = self.slots.get(ix).map(|s| s.ws.entity_id()) else { return };
        let jobs: Vec<String> = self.slots.iter().filter(|s| s.ws.entity_id() != keep).flat_map(|s| s.ws.read(cx).running(cx)).collect();
        let close_others = move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
            while let Some(other) = this.slots.iter().position(|s| s.ws.entity_id() != keep) {
                this.close(other, window, cx);
            }
        };
        if jobs.is_empty() {
            close_others(self, window, cx);
        } else {
            confirm_close(jobs, "the other workspaces", window, cx, close_others);
        }
    }

    /// What closing this window would end.
    fn running(&self, cx: &App) -> Vec<String> {
        self.slots.iter().flat_map(|s| s.ws.read(cx).running(cx)).collect()
    }

    /// Move workspace `ix` to a new window (at `at`, else cascaded).
    fn detach(&mut self, ix: usize, at: Option<(DisplayId, WindowBounds)>, window: &mut Window, cx: &mut Context<Self>) {
        if self.slots.len() < 2 {
            return;
        }
        if let Some(ws) = self.take(ix, window, cx) {
            cx.defer(move |cx| open_window(Content::Workspace(ws), at, cx));
        }
    }

    /// Move workspace `ix` into `target`'s window. A window left empty by
    /// the move closes, like dragging a browser's only tab into another.
    fn move_to(&mut self, ix: usize, target: AnyWindowHandle, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, main)) = windows(cx).into_iter().find(|(h, _)| *h == target) else { return };
        let Some(ws) = self.take(ix, window, cx) else { return };
        let _ = target.update(cx, |_, window, cx| {
            window.activate_window();
            main.update(cx, |m, cx| m.adopt(ws, None, window, cx));
        });
        if self.slots.is_empty() {
            window.remove_window();
        }
    }

    /// Move a workspace tab to slot `to` within this window.
    fn reorder(&mut self, id: EntityId, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(from) = self.position(id) else { return };
        let to = to.min(self.slots.len() - 1);
        let slot = self.slots.remove(from);
        self.slots.insert(to, slot);
        self.activate(to, window, cx);
    }

    /// A workspace tab was dropped at `pos` (window coordinates) outside the
    /// window: join the Tern window under it, or open a new window there.
    fn drop_outside(&mut self, id: EntityId, pos: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.position(id) else { return };
        let Some(display) = window.display(cx) else { return };
        let frame = window.bounds();
        // Window bounds are relative to the window's display, frame included.
        let chrome = frame.size.height - window.viewport_size().height;
        let at = frame.origin + point(px(0.), chrome) + pos;
        let me = window.window_handle();
        let others: Vec<AnyWindowHandle> = windows(cx).into_iter().map(|(h, _)| h).filter(|h| *h != me).collect();
        // Front to back, so the visible window under the pointer wins.
        let mut order = cx.window_stack().unwrap_or_default();
        order.retain(|h| others.contains(h));
        let unlisted: Vec<AnyWindowHandle> = others.iter().filter(|h| !order.contains(h)).copied().collect();
        order.extend(unlisted);
        let display_id = display.id();
        let target = order.into_iter().find(|h| {
            h.update(cx, |_, w, cx| w.display(cx).is_some_and(|d| d.id() == display_id) && w.bounds().contains(&at)).unwrap_or(false)
        });
        match target {
            Some(target) => self.move_to(ix, target, window, cx),
            None => {
                let size = window.viewport_size();
                // Put the dragged tab roughly under the pointer.
                let origin = at - point(px(80.), chrome + px(16.));
                self.detach(ix, Some((display_id, WindowBounds::Windowed(Bounds::new(origin, size)))), window, cx)
            },
        }
    }

    /// The agents panel (`agents`), a part every workspace of the window
    /// draws: each workspace's folder with its agents' terminals under it,
    /// unless collapsed, and each agent's sub-agents at work under it.
    /// Picking one shows its (agent's) tab and gives it focus.
    fn render_agents(&mut self, cx: &mut Context<Self>) -> AnyElement {
        #[derive(Clone, Copy)]
        enum Entry {
            Folder(usize),
            Tab(usize, EntityId),
            /// A sub-agent of the agent in a tab.
            Child(usize, EntityId),
        }
        let shortcut = |k| crate::platform::shortcut(k).to_string();
        let close_key = shortcut("⌘⇧W");
        let mut entries = vec![];
        let mut items = vec![];
        // What each agent's ring says, in words.
        let mut tips: HashMap<EntityId, SharedString> = HashMap::new();
        // The id of the sub-agent at each of their rows.
        let mut child_ids: HashMap<usize, SharedString> = HashMap::new();
        let mut active_row = 0;
        let mut working = false;
        for (i, slot) in self.slots.iter().enumerate() {
            let ws = slot.ws.read(cx);
            let open = !self.collapsed.contains(&slot.ws.entity_id());
            if i == self.active {
                active_row = entries.len();
            }
            let tabs = ws.sidebar_tabs(i == self.active, cx);
            let top = tabs.iter().map(|(_, _, s, _)| *s).max_by_key(|s| crate::agents::priority(*s));
            // Sub-agents' times count seconds too.
            working |= tabs.iter().any(|(_, _, s, children)| *s == ActivityStatus::Working || !children.is_empty());
            entries.push(Entry::Folder(i));
            let mut folder = template::Scope::new(format!("side-ws-{}", slot.ws.entity_id().as_u64()))
                .var("folder", true)
                .var("name", ws.name())
                .var("active", i == self.active)
                .var("open", open)
                .var("buttons", true)
                .var("close_key", close_key.clone());
            if let Some(s) = top.filter(|s| *s != ActivityStatus::None) {
                folder = folder.var("dot", true).var("dot_color", crate::agents::status_color(s, cx.theme()));
            }
            let needs = tabs.iter().filter(|(_, _, s, _)| *s == ActivityStatus::Blocked).count();
            if needs > 0 {
                folder = folder.var("needs", crate::agents::needs_label(needs));
            }
            items.push(folder);
            if open {
                for (id, scope, _, children) in tabs {
                    if let Some(tip) = scope.text_var("tip") {
                        tips.insert(id, tip);
                    }
                    entries.push(Entry::Tab(i, id));
                    items.push(scope);
                    for child in children {
                        if let Some(agent) = child.text_var("agent") {
                            child_ids.insert(entries.len(), agent);
                        }
                        entries.push(Entry::Child(i, id));
                        items.push(child);
                    }
                }
            }
        }
        if self.revealed.replace(self.active) != Some(self.active) {
            self.tab_scroll.scroll_to_item(active_row);
        }
        // Working times count seconds; ended turns' ages, minutes.
        if self._tick.as_ref().is_none_or(|(fast, _)| working && !fast) {
            let wait = Duration::from_secs(if working { 1 } else { 30 });
            self._tick = Some((
                working,
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(wait).await;
                    let _ = this.update(cx, |this, cx| {
                        this._tick = None;
                        cx.notify();
                    });
                }),
            ));
        }
        let buttons = template::Scope::new("agents.header.buttons")
            .var("open_key", shortcut("⌘O"))
            .var("monitor", crate::agent_monitor::window(cx).is_some());
        // Screenshots are on macOS and Windows.
        #[cfg(any(target_os = "macos", windows))]
        let buttons = buttons.var("can_shot", true).var("shots", crate::shot::panel_open(cx));
        // The close button hides the panel in the workspace it shows.
        let active = self.active_ws().map(|w| w.downgrade());
        let header = crate::dock::panel_header(
            "agents",
            "agents.header",
            "Agents",
            None,
            buttons,
            &mut |name, _, f| match (name, f) {
                ("open", Framed::Button(b)) => {
                    b.on_click(|_, window, cx| window.dispatch_action(Box::new(OpenFolder), cx)).into_any_element()
                },
                ("agent_monitor", Framed::Button(b)) => b.on_click(|_, _, cx| crate::agent_monitor::toggle(cx)).into_any_element(),
                ("close", Framed::Button(b)) => {
                    let ws = active.clone();
                    b.on_click(move |_, window, cx| {
                        let _ = ws.as_ref().map(|w| w.update(cx, |w, cx| w.toggle_agents(&ToggleAgents, window, cx)));
                    })
                    .into_any_element()
                },
                #[cfg(any(target_os = "macos", windows))]
                ("screenshots", Framed::Button(b)) => b.on_click(|_, _, cx| crate::shot::toggle_panel(cx)).into_any_element(),
                (_, f) => f.into_any_element(),
            },
            cx,
        );
        let scope = template::Scope::new("agents").slot("header", header).list("items", items);
        let me = cx.entity().downgrade();
        let workspaces: Vec<Entity<Workspace>> = self.slots.iter().map(|s| s.ws.clone()).collect();
        let drag: Vec<DraggedWorkspace> =
            self.slots.iter().map(|s| DraggedWorkspace { ws: s.ws.entity_id(), name: s.ws.read(cx).name().into() }).collect();
        let drop_bg = |cx: &App| cx.theme().list_hover;
        let scroll = self.tab_scroll.clone();
        template::render(
            "agents",
            scope,
            &mut |name, ix, f| match (name, f, ix.and_then(|i| entries.get(i).copied())) {
                ("list", Framed::Box(d), _) => d.id("agents-list").overflow_y_scroll().track_scroll(&scroll).into_any_element(),
                ("folder", Framed::Box(d), Some(Entry::Folder(i))) if i < drag.len() => {
                    let start = me.clone();
                    let menu_me = me.clone();
                    d.id(("agents-folder", i))
                        .on_click(on_main(&me, move |this, _: &ClickEvent, window, cx| {
                            if let Some(s) = this.slots.get(i) {
                                this.collapsed.remove(&s.ws.entity_id());
                            }
                            this.activate(i, window, cx);
                        }))
                        .on_drag(drag[i].clone(), move |d, _, _, cx| {
                            let id = d.ws;
                            let _ = start.update(cx, |m, _| m.dragging = Some(id));
                            cx.new(|_| d.clone())
                        })
                        .drag_over::<DraggedWorkspace>(move |s, _, _, cx| s.bg(drop_bg(cx)))
                        .on_drop(on_main(&me, move |this, d: &DraggedWorkspace, window, cx| {
                            this.dragging = None;
                            this.reorder(d.ws, i, window, cx);
                        }))
                        .context_menu(move |menu, window, cx| {
                            let Some(this) = menu_me.upgrade() else { return menu };
                            let this = this.read(cx);
                            let Some(slot) = this.slots.get(i) else { return menu };
                            let (root, single) = (slot.ws.read(cx).root().to_owned(), this.slots.len() < 2);
                            Self::tab_menu(menu, &menu_me, i, root, single, window, cx)
                        })
                        .into_any_element()
                },
                ("toggle", Framed::Box(d), Some(Entry::Folder(i))) => d
                    .id(("agents-toggle", i))
                    .on_click(on_main(&me, move |this, _: &ClickEvent, _, cx| {
                        // Don't let the folder's own click activate it.
                        cx.stop_propagation();
                        if let Some(id) = this.slots.get(i).map(|s| s.ws.entity_id())
                            && !this.collapsed.remove(&id)
                        {
                            this.collapsed.insert(id);
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
                ("new_tab", Framed::Button(b), Some(Entry::Folder(i))) => match workspaces.get(i) {
                    Some(ws) => Workspace::new_tab_button(b, ws.downgrade()),
                    None => b.into_any_element(),
                },
                ("close", Framed::Button(b), Some(Entry::Folder(i))) => b
                    .on_click(on_main(&me, move |this, _: &ClickEvent, window, cx| {
                        // Don't let the folder's own click activate it.
                        cx.stop_propagation();
                        this.request_close(i, window, cx);
                    }))
                    .into_any_element(),
                ("tab", Framed::Box(d), Some(Entry::Tab(i, id))) => {
                    let d = d.id(("agents-tab", id.as_u64() as usize)).on_click(on_main(&me, move |this, _: &ClickEvent, window, cx| {
                        let Some(ws) = this.slots.get(i).map(|s| s.ws.clone()) else { return };
                        if i != this.active {
                            this.activate(i, window, cx);
                        }
                        ws.update(cx, |w, cx| w.show_tab(id, window, cx));
                    }));
                    match workspaces.get(i) {
                        Some(ws) => Workspace::sidebar_tab_row(&ws.downgrade(), id, d, cx),
                        None => d.into_any_element(),
                    }
                },
                ("child", Framed::Box(d), Some(Entry::Child(i, id))) => {
                    let row = ix.unwrap_or_default();
                    let agent = child_ids.get(&row).cloned().unwrap_or_default();
                    d.id(("agents-child", row))
                        .on_click(on_main(&me, move |this, _: &ClickEvent, window, cx| {
                            let Some(ws) = this.slots.get(i).map(|s| s.ws.clone()) else { return };
                            if i != this.active {
                                this.activate(i, window, cx);
                            }
                            ws.update(cx, |w, cx| w.open_subagent_log(id, &agent, window, cx));
                        }))
                        .into_any_element()
                },
                ("status", Framed::Box(d), Some(Entry::Tab(_, id))) => match tips.get(&id).cloned() {
                    Some(tip) => d
                        .id(("agents-status", id.as_u64() as usize))
                        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx))
                        .into_any_element(),
                    None => d.into_any_element(),
                },
                ("close", Framed::Button(b), Some(Entry::Tab(i, id))) => b
                    .on_click(on_main(&me, move |this, _: &ClickEvent, window, cx| {
                        // Don't let the row's own click show it.
                        cx.stop_propagation();
                        if let Some(ws) = this.slots.get(i).map(|s| s.ws.clone()) {
                            ws.update(cx, |w, cx| w.request_close_id(id, window, cx));
                        }
                    }))
                    .into_any_element(),
                ("end", Framed::Box(d), _) => d
                    .id("agents-end")
                    .drag_over::<DraggedWorkspace>(move |s, _, _, cx| s.bg(drop_bg(cx)))
                    .on_drop(on_main(&me, |this, d: &DraggedWorkspace, window, cx| {
                        this.dragging = None;
                        let last = this.slots.len().saturating_sub(1);
                        this.reorder(d.ws, last, window, cx);
                    }))
                    .into_any_element(),
                (_, f, _) => f.into_any_element(),
            },
            cx,
        )
    }

    /// Right-click menu of workspace `ix`'s folder in the agents panel.
    fn tab_menu(
        menu: PopupMenu,
        me: &WeakEntity<Self>,
        ix: usize,
        root: PathBuf,
        single: bool,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let me_id = me.entity_id();
        let mut others = vec![];
        let mut labels = vec![];
        for (handle, main) in windows(cx).into_iter().filter(|(_, m)| m.entity_id() != me_id) {
            let names: Vec<String> = main.read(cx).slots.iter().map(|s| s.ws.read(cx).name()).collect();
            labels
                .push(template::Scope::default().var("label", if names.is_empty() { "Empty Window".to_owned() } else { names.join(", ") }));
            others.push(handle);
        }
        let me = me.clone();
        let items: template::MenuItems = Rc::new(move |name, entry| {
            type Handler = Box<dyn Fn(&mut MainWindow, &mut Window, &mut Context<MainWindow>)>;
            let f: Handler = match name {
                "close" => Box::new(move |this, window, cx| this.request_close(ix, window, cx)),
                "close_others" => Box::new(move |this, window, cx| this.request_close_others(ix, window, cx)),
                "move_new_window" => Box::new(move |this, window, cx| this.detach(ix, None, window, cx)),
                "move_to_window" => {
                    let handle = *others.get(entry?)?;
                    Box::new(move |this, window, cx| this.move_to(ix, handle, window, cx))
                },
                "copy_path" => {
                    let root = root.clone();
                    return Some(Rc::new(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(root.display().to_string()))));
                },
                "reveal" => {
                    let root = root.clone();
                    return Some(Rc::new(move |_, _, cx| cx.reveal_path(&root)));
                },
                _ => return None,
            };
            let me = me.clone();
            Some(Rc::new(move |_, window, cx| {
                let _ = me.update(cx, |this, cx| f(this, window, cx));
            }))
        });
        let scope =
            template::Scope::new("workspace-menu").var("single", single).var("reveal", crate::platform::REVEAL).list("windows", labels);
        template::menu("menu.workspace_tab", scope, menu, items, window, cx)
    }

    /// The page of a window with no workspace (`welcome`).
    fn render_welcome(&self, cx: &mut Context<Self>) -> AnyElement {
        let shortcut = |k| crate::platform::shortcut(k).to_string();
        let recent = window_state::recent_folders();
        let items = recent
            .iter()
            .enumerate()
            .map(|(i, path)| {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
                template::Scope::new(format!("recent-{i}")).var("name", name).var("path", crate::dirs::tilde(path))
            })
            .collect();
        let scope = template::Scope::new("welcome")
            .var("recents", recent.len())
            .var("open_key", shortcut("⌘O"))
            .var("new_window_key", shortcut("⌘⇧N"))
            .var("close_key", shortcut("⌘⇧W"))
            .var("switch_key", shortcut("⌘⌥← ⌘⌥→"))
            .list("recent", items);
        let me = cx.entity().downgrade();
        template::render(
            "welcome",
            scope,
            &mut |name, ix, f| match (name, f, ix.and_then(|i| Some((i, recent.get(i)?)))) {
                ("open", Framed::Button(b), _) => {
                    b.on_click(|_, window, cx| window.dispatch_action(Box::new(OpenFolder), cx)).into_any_element()
                },
                ("recent", Framed::Box(d), Some((i, path))) => {
                    let path = path.clone();
                    d.id(("recent", i))
                        .cursor_pointer()
                        .on_click(on_event(&me, move |this, _: &ClickEvent, window, cx| this.open_folder(path.clone(), window, cx)))
                        .into_any_element()
                },
                (_, f, _) => f.into_any_element(),
            },
            cx,
        )
    }

    /// Window-level control commands, else the active workspace's (see `control`).
    #[cfg(debug_assertions)]
    fn control_query(&mut self, line: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let mut parts = line.split_whitespace();
        let cmd = parts.next().unwrap_or_default();
        let mut num = || parts.next().and_then(|s| s.parse::<f32>().ok());
        let names: Vec<String> = self.slots.iter().map(|s| s.ws.read(cx).name()).collect();
        let workspaces = format!("workspaces={names:?} active_ws={}", self.active);
        match cmd {
            "ws" => workspaces,
            "ws-open" => {
                let path = line["ws-open".len()..].trim();
                self.open_folder(PathBuf::from(path), window, cx);
                "ok".into()
            },
            "ws-close" => {
                let ix = num().map_or(self.active, |n| n as usize);
                self.close(ix, window, cx);
                "ok".into()
            },
            "ws-activate" => {
                let Some(ix) = num() else { return "error: ws-activate N".into() };
                self.activate(ix as usize, window, cx);
                "ok".into()
            },
            "ws-detach" => {
                let Some(ix) = num() else { return "error: ws-detach N".into() };
                self.detach(ix as usize, None, window, cx);
                "ok".into()
            },
            "ws-move" => {
                let (Some(ix), Some(w)) = (num(), num()) else { return "error: ws-move N WINDOW".into() };
                match windows(cx).get(w as usize) {
                    Some((h, _)) => {
                        let h = *h;
                        self.move_to(ix as usize, h, window, cx);
                        "ok".into()
                    },
                    None => "error: no such window".into(),
                }
            },
            "ws-reorder" => {
                let (Some(from), Some(to)) = (num(), num()) else { return "error: ws-reorder FROM TO".into() };
                let Some(id) = self.slots.get(from as usize).map(|s| s.ws.entity_id()) else { return "error: no such workspace".into() };
                self.reorder(id, to as usize, window, cx);
                "ok".into()
            },
            // What dropping a dragged tab at window coordinates X Y does.
            "ws-dropout" => {
                let (Some(ix), Some(x), Some(y)) = (num(), num(), num()) else { return "error: ws-dropout N X Y".into() };
                let Some(id) = self.slots.get(ix as usize).map(|s| s.ws.entity_id()) else { return "error: no such workspace".into() };
                self.drop_outside(id, point(px(x), px(y)), window, cx);
                "ok".into()
            },
            _ => match self.active_ws().cloned() {
                Some(ws) => {
                    let out = ws.update(cx, |w, cx| w.control_query(line, window, cx));
                    if cmd == "state" { format!("{workspaces} {out}") } else { out }
                },
                None if cmd == "state" => format!("{workspaces} welcome"),
                None => "error: no workspace open".into(),
            },
        }
    }
}

/// Debug control: `windows` lists them; `@N CMD` sends CMD to window N
/// (default: the active window, else the first).
#[cfg(debug_assertions)]
pub fn control(line: &str, cx: &mut App) -> String {
    #[cfg(any(target_os = "macos", windows))]
    if let Some(out) = crate::shot::control(line, cx) {
        return out;
    }
    let all = windows(cx);
    if line.trim() == "windows" {
        let list: Vec<String> = all
            .iter()
            .enumerate()
            .map(|(i, (_, m))| {
                let names: Vec<String> = m.read(cx).slots.iter().map(|s| s.ws.read(cx).name()).collect();
                format!("{i}:{names:?}")
            })
            .collect();
        return list.join(" ");
    }
    let (target, line) = match line.strip_prefix('@').and_then(|r| r.split_once(' ')) {
        Some((n, rest)) => (n.parse::<usize>().ok().and_then(|n| all.get(n)).cloned(), rest.to_owned()),
        None => {
            let active = cx.active_window();
            (all.iter().find(|(h, _)| Some(*h) == active).or(all.first()).cloned(), line.to_owned())
        },
    };
    let Some((handle, main)) = target else { return "error: no such window".into() };
    if line.trim() == "new-window" {
        open_window(Content::Welcome, None, cx);
        return "ok".into();
    }
    if let Some(cmd) = line.trim().strip_prefix("settings-input ") {
        return crate::settings_window::input(cmd, cx);
    }
    if let Some(section) = line.trim().strip_prefix("settings ") {
        return crate::settings_window::show(section.trim(), cx);
    }
    if line.trim() == "agent-monitor" {
        crate::agent_monitor::toggle(cx);
        return "ok".into();
    }
    // Input to the agent monitor.
    if let Some(cmd) = line.trim().strip_prefix("monitor-input ") {
        let Some(monitor) = crate::agent_monitor::window(cx) else { return "error: not open".into() };
        return monitor
            .update(cx, |_, window, cx| crate::control::input(cmd, window, cx).unwrap_or_else(|| "error: unknown input".into()))
            .unwrap_or_else(|_| "error: window closed".into());
    }
    if line.trim() == "restart-background" {
        crate::restart::restart(cx);
        return "ok".into();
    }
    // Inject input outside any entity update, so handlers can update views.
    handle
        .update(cx, |_, window, cx| match crate::control::input(&line, window, cx) {
            Some(out) => out,
            None => main.update(cx, |m, cx| m.control_query(&line, window, cx)),
        })
        .unwrap_or_else(|_| "error: window closed".into())
}

impl MainWindow {
    /// The title bar, drawn by the window itself (as in T3 Code): the title,
    /// what is wrong with the background terminals or the settings, then the buttons of the workspace's panels docked on the right, then
    /// the window controls. `TitleBar` moves the window and draws those.
    fn render_title_bar(&self, ws: Option<&Entity<Workspace>>, cx: &mut Context<Self>) -> AnyElement {
        let (parts, clicks) = ws.map(|ws| ws.update(cx, |w, cx| w.title_parts(cx))).unwrap_or_default();
        let (background, settings_error) = ws.map(|ws| ws.read(cx).title_status(cx)).unwrap_or_default();
        let update = crate::update::offered(cx);
        let (caffeine_icon, caffeine_tooltip, caffeine_on) = crate::caffeine::button(cx);
        let mut scope = template::Scope::new("title_bar")
            .var("title", self.title.clone())
            .var("background", background)
            .var("settings_error", settings_error)
            .var("settings_key", crate::platform::shortcut("⌘,").to_string())
            .var("caffeine_icon", caffeine_icon)
            .var("caffeine_tooltip", caffeine_tooltip)
            .var("caffeine_on", caffeine_on)
            .list("parts", parts);
        if let Some((release, installing)) = &update {
            let label = if *installing { "Installing update…".to_owned() } else { format!("Update to {}", release.version) };
            scope = scope.var("update", label).var("installing", *installing);
        }
        let bar = template::render(
            "title_bar",
            scope,
            &mut |name, ix, f| match (name, f, ix.and_then(|i| clicks.get(i))) {
                ("part", Framed::Button(b), Some(click)) => {
                    let click = click.clone();
                    b.on_click(move |_, w, cx| click(w, cx)).into_any_element()
                },
                ("caffeine", Framed::Button(b), _) => b
                    .on_click(|_, _, cx| crate::caffeine::toggle(cx))
                    .on_mouse_down(MouseButton::Right, |_, window, cx| {
                        cx.stop_propagation();
                        if let Err(e) = crate::caffeine::toggle_until_agents_finish(cx) {
                            window.push_notification(e, cx);
                        }
                    })
                    .into_any_element(),
                ("settings", Framed::Button(b), _) => {
                    b.on_click(|_, window, cx| window.dispatch_action(Box::new(OpenSettings), cx)).into_any_element()
                },
                ("update", Framed::Button(b), _) => {
                    let update = update.clone();
                    b.on_click(move |_, _, cx| {
                        if let Some((release, false)) = &update {
                            crate::update::offer(release.clone(), cx)
                        }
                    })
                    .into_any_element()
                },
                // Over the bar's drag area: without this, Windows takes a click on a button for the caption's.
                ("buttons", Framed::Box(d), _) => d.occlude().into_any_element(),
                (_, f, _) => f.into_any_element(),
            },
            cx,
        );
        crate::style::el("title_bar", cx).apply(gpui_kit::component::TitleBar::new()).child(bar).into_any_element()
    }
}

impl Render for MainWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self.active_ws().map_or_else(|| "Tern".to_owned(), |ws| ws.read(cx).name());
        if title != self.title {
            window.set_window_title(&title);
            self.title = title;
        }
        crate::style::sync_backdrop(&mut self.backdrop, window, cx);
        let ws = self.active_ws().cloned();
        let title_bar = self.render_title_bar(ws.as_ref(), cx);
        let scope = template::Scope::new("window").var("workspace", ws.is_some()).slot("title_bar", title_bar);
        let scope = match ws {
            Some(ws) => scope.slot("workspace", ws),
            None => scope.slot("welcome", self.render_welcome(cx)),
        };
        let body = template::render("window", scope, &mut |_, _, d| d.into_any_element(), cx);
        div()
            .id("main-window")
            .key_context("MainWindow")
            .track_focus(&self.focus)
            .size_full()
            .on_action(cx.listener(|_, _: &OpenFolder, _, cx| prompt_open_folder(cx)))
            .on_action(cx.listener(|_, _: &SelectTheme, window, cx| theme_picker::open(window, cx)))
            .on_action(cx.listener(|_, _: &crate::restart::RestartBackground, window, cx| crate::restart::confirm(window, cx)))
            // A workspace opens it in a tab; without one, the system does.
            .on_action(cx.listener(|_, _: &OpenSettingsFile, _, cx| cx.open_with_system(&crate::settings::settings_path(cx))))
            .on_action(cx.listener(|_, _: &NewWindow, _, cx| open_window(Content::Welcome, None, cx)))
            .on_action(cx.listener(|this, _: &CloseWorkspace, window, cx| {
                if !this.slots.is_empty() {
                    this.request_close(this.active, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &NextWorkspace, window, cx| {
                let n = this.slots.len().max(1);
                this.activate((this.active + 1) % n, window, cx);
            }))
            .on_action(cx.listener(|this, _: &PrevWorkspace, window, cx| {
                let n = this.slots.len().max(1);
                this.activate((this.active + n - 1) % n, window, cx);
            }))
            // A tab dragged out of the window and released: macOS keeps
            // sending the drag's events to this window, outside its bounds.
            .capture_any_mouse_up(cx.listener(|this, ev: &MouseUpEvent, window, cx| {
                let Some(id) = this.dragging.take() else { return };
                let inside = Bounds::new(Point::default(), window.viewport_size()).contains(&ev.position);
                if !inside {
                    this.drop_outside(id, ev.position, window, cx);
                }
            }))
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::{RevealTarget, adoption_workspace, reveal_target};
    use crate::window_state::SavedTab;
    use tern_mux::proto::SessionInfo;

    #[test]
    fn reveal_target_focuses_the_tab_showing_it() {
        let tabs = vec![vec![vec![None, Some(1)]], vec![vec![Some(2)], vec![None, None, Some(3)]]];
        assert_eq!(reveal_target(&tabs, 3, true), RevealTarget::Focus { window: 1, ws: 1, tab: 2 });
        assert_eq!(reveal_target(&tabs, 1, true), RevealTarget::Focus { window: 0, ws: 0, tab: 1 });
    }

    #[test]
    fn reveal_target_adopts_what_no_tab_shows() {
        let tabs = vec![vec![vec![Some(1)]]];
        assert_eq!(reveal_target(&tabs, 9, true), RevealTarget::AdoptIntoActive);
        assert_eq!(reveal_target(&[], 9, true), RevealTarget::NewWindow);
    }

    #[test]
    fn adoption_workspace_holds_just_the_session() {
        // No default Claude tab: the workspace opens with the session alone.
        let shell = SessionInfo { id: 7, cwd: Some("/p".into()), title: Some("vim".into()), ..Default::default() };
        let w = adoption_workspace(&shell, "/home".as_ref());
        assert_eq!(w.folder, std::path::Path::new("/p"));
        assert_eq!(w.tabs, [SavedTab::Shell { name: Some("vim".into()), mux: Some(7) }]);
        let claude = SessionInfo { id: 8, claude: true, claude_session: Some("s".into()), ..Default::default() };
        let w = adoption_workspace(&claude, "/home".as_ref());
        assert_eq!(w.folder, std::path::Path::new("/home"));
        assert_eq!(w.tabs, [SavedTab::Claude { name: None, session: Some("s".into()), mux: Some(8), account: None, guard: None }]);
    }

    #[test]
    fn reveal_target_unknown_session_does_nothing() {
        // Ended between the menu and the click; a stale tab may still name it.
        assert_eq!(reveal_target(&[vec![vec![Some(9)]]], 9, false), RevealTarget::Nothing);
        assert_eq!(reveal_target(&[], 9, false), RevealTarget::Nothing);
    }
}
