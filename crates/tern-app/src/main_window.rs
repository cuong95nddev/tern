//! A window: a bar of workspace tabs over the active workspace, or a welcome
//! page when it has none (like VS Code with no folder open). Workspaces move
//! between windows: drag a tab out of the bar onto another Tern window to
//! join it, or anywhere else to give it a window of its own; the tab's menu
//! does the same.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::assets::IconName as AssetIcon;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::platform::bind;
use crate::workspace::{Workspace, WorkspaceEvent, confirm_close};
use crate::{AppServices, OpenFolder, window_state};

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
/// Everything quitting would end, across windows.
pub fn running(cx: &mut App) -> Vec<String> {
    windows(cx).into_iter().flat_map(|(_, m)| m.read(cx).running(cx)).collect()
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
    window_state::save_session(&saved);
}

/// Reopen the last session's windows. False when there was none.
pub fn restore_session(cx: &mut App) -> bool {
    let saved = window_state::load_session();
    for w in &saved {
        let at = w.placement.as_ref().and_then(|(uuid, bounds)| window_state::resolve(uuid, *bounds, cx));
        open_window(Content::Workspaces(w.workspaces.clone(), w.active), at, cx);
    }
    !saved.is_empty()
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
            titlebar: Some(TitlebarOptions { title: Some("Tern".into()), ..Default::default() }),
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

struct Slot {
    ws: Entity<Workspace>,
    _sub: Subscription,
}

/// The payload of a workspace tab drag.
#[derive(Clone)]
struct DraggedWorkspace {
    ws: EntityId,
    name: SharedString,
}

impl Render for DraggedWorkspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px_3()
            .py_1()
            .flex()
            .items_center()
            .gap_1p5()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .shadow_md()
            .text_sm()
            // Drawn outside the root view, so it doesn't inherit its text style.
            .text_color(theme.foreground)
            .child(Icon::new(AssetIcon::FolderOpen).xsmall())
            .child(self.name.clone())
    }
}

pub struct MainWindow {
    slots: Vec<Slot>,
    active: usize,
    /// The workspace tab last right-clicked, for its menu.
    menu_ix: Option<usize>,
    /// The workspace whose tab is being dragged.
    dragging: Option<EntityId>,
    title: String,
    /// Where the window is, for the saved session.
    placement: Option<(String, WindowBounds)>,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl MainWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let bounds_sub = cx.observe_window_bounds(window, |this: &mut Self, window, cx| {
            window_state::save(window, cx);
            this.placement = window_state::placement(window, cx);
            this.changed(cx);
        });
        // The close button asks first when that ends running programs.
        let me = cx.entity().downgrade();
        let confirmed = Rc::new(Cell::new(false));
        window.on_window_should_close(cx, move |window, cx| {
            let jobs = me.upgrade().map(|m| m.read(cx).running(cx)).unwrap_or_default();
            if jobs.is_empty() || confirmed.get() {
                return true;
            }
            let Some(me) = me.upgrade() else { return true };
            let confirmed = confirmed.clone();
            me.update(cx, |_, cx| {
                confirm_close(jobs, "this window", window, cx, move |_, window, _| {
                    confirmed.set(true);
                    window.remove_window();
                })
            });
            false
        });
        cx.on_release(|this: &mut Self, cx| {
            for slot in &this.slots {
                slot.ws.update(cx, |w, cx| w.shutdown(cx));
            }
        })
        .detach();
        Self {
            slots: vec![],
            active: 0,
            menu_ix: None,
            dragging: None,
            title: String::new(),
            placement: window_state::placement(window, cx),
            focus: cx.focus_handle(),
            _subs: vec![bounds_sub],
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
        let sub = cx.subscribe_in(&ws, window, |this, ws, ev: &WorkspaceEvent, window, cx| match ev {
            WorkspaceEvent::Close => {
                if let Some(ix) = this.position(ws.entity_id()) {
                    this.close(ix, window, cx);
                }
            },
            WorkspaceEvent::TabsChanged => this.changed(cx),
        });
        let ix = at.unwrap_or(self.slots.len()).min(self.slots.len());
        self.slots.insert(ix, Slot { ws, _sub: sub });
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
            ws.update(cx, |w, cx| w.shutdown(cx));
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

    fn render_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (warn, border, drop_bg) = (theme.warning, theme.border, theme.list_hover);
        let me = cx.entity().downgrade();
        let tabs = self.slots.iter().enumerate().map(|(i, slot)| {
            let ws = slot.ws.read(cx);
            let name: SharedString = ws.name().into();
            let busy = i != self.active && ws.busy(cx);
            let drag = DraggedWorkspace { ws: slot.ws.entity_id(), name: name.clone() };
            let me = me.clone();
            Tab::new()
                .prefix(
                    div()
                        .pl_2()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(Icon::new(AssetIcon::FolderOpen).xsmall())
                        .when(busy, |d| d.child(div().text_color(warn).child("●"))),
                )
                .label(name)
                .suffix(
                    div().pr_1().child(
                        Button::new(("close-ws", i))
                            .xsmall()
                            .ghost()
                            .icon(IconName::Close)
                            .tooltip(format!("Close Workspace ({})", crate::platform::shortcut("⌘⇧W")))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.request_close(i, window, cx);
                            })),
                    ),
                )
                .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, _| this.menu_ix = Some(i)))
                .on_drag(drag, move |d, _, _, cx| {
                    let id = d.ws;
                    let _ = me.update(cx, |m, _| m.dragging = Some(id));
                    cx.new(|_| d.clone())
                })
                .drag_over::<DraggedWorkspace>(move |s, _, _, _| s.bg(drop_bg))
                .on_drop(cx.listener(move |this, d: &DraggedWorkspace, window, cx| {
                    this.dragging = None;
                    this.reorder(d.ws, i, window, cx);
                }))
        });
        let bar = TabBar::new("workspace-tabs")
            .selected_index(self.active)
            .children(tabs)
            .last_empty_space(
                div()
                    .id("ws-bar-end")
                    .flex_1()
                    .h_full()
                    .px_1()
                    .flex()
                    .items_center()
                    .drag_over::<DraggedWorkspace>(move |s, _, _, _| s.bg(drop_bg))
                    .on_drop(cx.listener(|this, d: &DraggedWorkspace, window, cx| {
                        this.dragging = None;
                        let last = this.slots.len().saturating_sub(1);
                        this.reorder(d.ws, last, window, cx);
                    }))
                    .child(
                        Button::new("open-workspace")
                            .xsmall()
                            .ghost()
                            .icon(Icon::new(AssetIcon::FolderPlus))
                            .tooltip(format!("Open Workspace… ({})", crate::platform::shortcut("⌘O")))
                            .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenFolder), cx)),
                    )
                    .child(div().flex_1())
                    .children(crate::update::offered(cx).map(|(release, installing)| {
                        let label = if installing { "Installing update…".to_owned() } else { format!("Update to {}", release.version) };
                        Button::new("update")
                            .xsmall()
                            .ghost()
                            .icon(Icon::new(AssetIcon::CircleArrowDown))
                            .label(label)
                            .loading(installing)
                            .tooltip("A new version of Tern is available")
                            .on_click(move |_, _, cx| {
                                if !installing {
                                    crate::update::offer(release.clone(), cx)
                                }
                            })
                    })),
            )
            // The tab bar shows `last_empty_space` only alongside a suffix.
            .suffix(div().w_1())
            .on_click(cx.listener(|this, ix: &usize, window, cx| this.activate(*ix, window, cx)));
        let _ = window;
        div().id("workspace-bar").border_b_1().border_color(border).child(bar).context_menu(move |menu, _, cx| {
            let Some(this) = me.upgrade() else { return menu };
            let this = this.read(cx);
            let Some(ix) = this.menu_ix.filter(|&ix| ix < this.slots.len()) else { return menu };
            let (root, single) = (this.slots[ix].ws.read(cx).root().to_owned(), this.slots.len() < 2);
            Self::tab_menu(menu, &me, ix, root, single, cx)
        })
    }

    /// Right-click menu of workspace tab `ix`.
    fn tab_menu(menu: PopupMenu, me: &WeakEntity<Self>, ix: usize, root: PathBuf, single: bool, cx: &mut App) -> PopupMenu {
        type Handler = fn(&mut MainWindow, usize, &mut Window, &mut Context<MainWindow>);
        let item = |label: &'static str, f: Handler| {
            let me = me.clone();
            PopupMenuItem::new(label).on_click(move |_, window, cx| {
                let _ = me.update(cx, |this, cx| f(this, ix, window, cx));
            })
        };
        let mut menu = menu
            .item(item("Close Workspace", Self::request_close))
            .item(item("Close Other Workspaces", Self::request_close_others).disabled(single))
            .separator()
            .item(item("Move to New Window", |this, ix, window, cx| this.detach(ix, None, window, cx)).disabled(single));
        let me_id = me.entity_id();
        for (handle, main) in windows(cx).into_iter().filter(|(_, m)| m.entity_id() != me_id) {
            let names: Vec<String> = main.read(cx).slots.iter().map(|s| s.ws.read(cx).name()).collect();
            let label = if names.is_empty() { "Empty Window".to_owned() } else { names.join(", ") };
            let me = me.clone();
            menu = menu.item(PopupMenuItem::new(format!("Move to Window: {label}")).on_click(move |_, window, cx| {
                let _ = me.update(cx, |this, cx| this.move_to(ix, handle, window, cx));
            }));
        }
        let (copy, reveal) = (root.clone(), root);
        menu.separator()
            .item(PopupMenuItem::new("Copy Path").on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy.display().to_string()));
            }))
            .item(PopupMenuItem::new(crate::platform::REVEAL).on_click(move |_, _, cx| cx.reveal_path(&reveal)))
    }

    fn render_welcome(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, fg, hover, border) = (theme.muted_foreground, theme.foreground, theme.list_hover, theme.border);
        let recent = window_state::recent_folders();
        let home = crate::dirs::home();
        let pretty = move |p: &Path| match home.as_deref().and_then(|h| p.strip_prefix(h).ok()) {
            Some(rel) => format!("~/{}", rel.display()),
            None => p.display().to_string(),
        };
        let recent_list = div().flex().flex_col().gap_0p5().children(recent.into_iter().enumerate().map(|(i, path)| {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
            let shown = pretty(&path);
            div()
                .id(("recent", i))
                .px_2()
                .py_1()
                .rounded_md()
                .flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .child(Icon::new(AssetIcon::FolderOpen).small().text_color(muted))
                .child(div().text_color(fg).child(name))
                .child(div().text_xs().text_color(muted).overflow_hidden().text_ellipsis().child(shown))
                .on_click(cx.listener(move |this, _, window, cx| this.open_folder(path.clone(), window, cx)))
        }));
        let key = |k: String, what: &'static str| {
            div().flex().justify_between().gap_8().child(div().text_color(muted).child(what)).child(div().text_color(fg).child(k))
        };
        div().size_full().flex().items_center().justify_center().child(
            div()
                .w(px(520.))
                .flex()
                .flex_col()
                .gap_6()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().text_3xl().font_weight(FontWeight::SEMIBOLD).text_color(fg).child("Tern"))
                        .child(div().text_color(muted).child("Terminals, files and git changes of one folder per workspace.")),
                )
                .child(
                    div().flex().gap_2().child(
                        Button::new("welcome-open")
                            .primary()
                            .icon(Icon::new(AssetIcon::FolderPlus))
                            .label("Open Workspace…")
                            .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenFolder), cx)),
                    ),
                )
                .when(!window_state::recent_folders().is_empty(), |d| {
                    d.child(div().flex().flex_col().gap_1().child(div().text_sm().text_color(muted).child("Recent")).child(recent_list))
                })
                .child(
                    div()
                        .pt_4()
                        .border_t_1()
                        .border_color(border)
                        .flex()
                        .flex_col()
                        .gap_1()
                        .text_sm()
                        .child(key(crate::platform::shortcut("⌘O"), "Open workspace"))
                        .child(key(crate::platform::shortcut("⌘⇧N"), "New window"))
                        .child(key(crate::platform::shortcut("⌘⇧W"), "Close workspace"))
                        .child(key(crate::platform::shortcut("⌘⌥← ⌘⌥→"), "Switch workspace")),
                ),
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
    // Inject input outside any entity update, so handlers can update views.
    handle
        .update(cx, |_, window, cx| match crate::control::input(&line, window, cx) {
            Some(out) => out,
            None => main.update(cx, |m, cx| m.control_query(&line, window, cx)),
        })
        .unwrap_or_else(|_| "error: window closed".into())
}

impl Render for MainWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self.active_ws().map_or_else(|| "Tern".to_owned(), |ws| ws.read(cx).name());
        if title != self.title {
            window.set_window_title(&title);
            self.title = title;
        }
        let bg = cx.theme().background;
        let body = match self.active_ws() {
            Some(ws) => div()
                .size_full()
                .flex()
                .flex_col()
                .child(self.render_bar(window, cx))
                .child(div().flex_1().min_h_0().child(ws.clone()))
                .into_any_element(),
            None => self.render_welcome(cx).into_any_element(),
        };
        div()
            .id("main-window")
            .key_context("MainWindow")
            .track_focus(&self.focus)
            .size_full()
            .bg(bg)
            .on_action(cx.listener(|_, _: &OpenFolder, _, cx| prompt_open_folder(cx)))
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
