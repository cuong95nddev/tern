//! The window: the workspace (every folder the user added, with its
//! terminals, files and git changes), or a welcome page while it has no
//! folder and no tab (like VS Code with no folder open). Tern has one
//! window and one workspace; opening a folder adds it to the workspace.

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::*;

use crate::template::{self, Framed, on_event};
use crate::theme_picker::{self, OpenSettings, SelectTheme};
use crate::workspace::{Workspace, WorkspaceEvent};
use crate::{AppServices, OpenFolder, window_state};

pub fn bind_keys(_: &mut App) {}

/// The window, while it is open.
#[derive(Default)]
struct Registry(Option<(AnyWindowHandle, WeakEntity<MainWindow>)>);

impl Global for Registry {}

/// The window and its main view, if it is open.
fn main_window(cx: &mut App) -> Option<(AnyWindowHandle, Entity<MainWindow>)> {
    let open = cx.windows();
    let reg = cx.default_global::<Registry>();
    if reg.0.as_ref().is_some_and(|(h, w)| w.upgrade().is_none() || !open.contains(h)) {
        reg.0 = None;
    }
    reg.0.as_ref().and_then(|(h, w)| Some((*h, w.upgrade()?)))
}

/// The workspace with its window (none or one).
pub fn workspaces(cx: &mut App) -> Vec<(AnyWindowHandle, Entity<Workspace>)> {
    main_window(cx).map(|(h, m)| (h, m.read(cx).ws.clone())).into_iter().collect()
}

/// As [`workspaces`].
pub fn all_workspaces(cx: &mut App) -> Vec<(AnyWindowHandle, Entity<Workspace>)> {
    workspaces(cx)
}

/// Bring the window forward, and Tern with it.
pub fn reveal(handle: AnyWindowHandle, _: &Entity<Workspace>, cx: &mut App) {
    let _ = handle.update(cx, |_, window, _| window.activate_window());
    cx.activate(true);
}

/// Session bookkeeping: whether to save it, and whether the app is quitting
/// (the window closing then must not empty the saved session).
#[derive(Default)]
pub struct Session {
    pub persist: bool,
    pub quitting: bool,
}

impl Global for Session {}

/// Save the window's place and the workspace, to reopen at the next launch.
pub fn save_session(cx: &mut App) {
    let s = cx.default_global::<Session>();
    if !s.persist || s.quitting {
        return;
    }
    let Some((_, main)) = main_window(cx) else { return };
    let m = main.read(cx);
    let saved = window_state::SavedWindow { placement: m.placement.clone(), workspace: m.ws.read(cx).saved(cx) };
    window_state::save_session(&saved);
}

/// Reopen the last session's window. False when there was none.
pub fn restore_session(cx: &mut App) -> bool {
    let Some(saved) = window_state::load_session() else { return false };
    let at = saved.placement.as_ref().and_then(|(uuid, bounds)| window_state::resolve(uuid, *bounds, cx));
    let known = saved
        .workspace
        .tabs
        .iter()
        .filter_map(|t| match t {
            window_state::SavedTab::Claude { mux, .. } | window_state::SavedTab::Shell { mux, .. } => *mux,
            _ => None,
        })
        .collect();
    open_window(saved.workspace, at, cx);
    adopt_orphans(known, cx);
    true
}

/// Background sessions no saved tab knows (the session wasn't saved before
/// a crash, or an older build's window was closed): running ones open as
/// tabs, so nothing runs unseen; ended ones go.
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
    if main_window(cx).is_none() {
        open_window(window_state::SavedWorkspace::default(), None, cx);
    }
    let Some((handle, main)) = main_window(cx) else { return };
    let ws = main.read(cx).ws.clone();
    let _ = handle.update(cx, |_, window, cx| {
        ws.update(cx, |w, cx| {
            for s in &live {
                w.adopt_session(s, window, cx);
            }
        })
    });
}

/// Where Open (from the menu bar) puts session `id`.
#[derive(Debug, PartialEq, Eq)]
pub enum RevealTarget {
    /// Tab `tab` shows it already.
    Focus(usize),
    /// A new tab in the workspace.
    Adopt,
    /// It ended meanwhile.
    Nothing,
}

/// `tabs` = the mux ids of the workspace's tabs (`None` for non-terminal
/// tabs); `known`: the daemon has the session.
pub fn reveal_target(tabs: &[Option<u64>], id: u64, known: bool) -> RevealTarget {
    if !known {
        return RevealTarget::Nothing;
    }
    match tabs.iter().position(|t| *t == Some(id)) {
        Some(tab) => RevealTarget::Focus(tab),
        None => RevealTarget::Adopt,
    }
}

/// Show background session `id` (asked from the menu bar): focus the tab
/// showing it, else adopt it into the workspace (opening the window if it
/// is closed). Nothing if it has ended.
pub fn reveal_session(id: u64, cx: &mut App) {
    let info = cx.global::<AppServices>().0.mux.existing().and_then(|c| c.list().ok()).and_then(|l| l.into_iter().find(|s| s.id == id));
    let tabs = main_window(cx).map(|(_, m)| m.read(cx).ws.read(cx).mux_ids(cx)).unwrap_or_default();
    let target = reveal_target(&tabs, id, info.is_some());
    log::info!("show session {id}: {target:?}");
    match target {
        RevealTarget::Nothing => log::info!("session {id} is gone; nothing to show"),
        RevealTarget::Focus(tab) => {
            let Some((handle, main)) = main_window(cx) else { return };
            let ws = main.read(cx).ws.clone();
            let _ = handle.update(cx, |_, window, cx| {
                ws.update(cx, |w, cx| w.show_tab_at(tab, window, cx));
                window.activate_window();
            });
        },
        RevealTarget::Adopt => {
            let Some(info) = info else { return };
            let Some((handle, main)) = main_window(cx) else {
                // No window: one whose only tab shows it.
                open_window(adoption_workspace(&info), None, cx);
                cx.activate(true);
                return;
            };
            let ws = main.read(cx).ws.clone();
            let _ = handle.update(cx, |_, window, cx| {
                ws.update(cx, |w, cx| w.adopt_session(&info, window, cx));
                window.activate_window();
            });
        },
    }
    cx.activate(true);
}

/// A workspace whose only tab shows session `s`: no default Claude tab
/// beside it, and no folder (the session runs in its own).
pub fn adoption_workspace(s: &tern_mux::proto::SessionInfo) -> window_state::SavedWorkspace {
    let cwd = s.cwd.clone();
    let tab = if s.claude {
        window_state::SavedTab::Claude { name: None, session: s.claude_session.clone(), mux: Some(s.id), cwd }
    } else {
        window_state::SavedTab::Shell { name: s.title.clone(), mux: Some(s.id), cwd }
    };
    window_state::SavedWorkspace { tabs: vec![tab], tab_groups: vec![None], ..Default::default() }
}

/// Open the window with `saved`, unless it is open. With `at`, it is
/// placed there; otherwise where it last was.
pub fn open_window(saved: window_state::SavedWorkspace, at: Option<(DisplayId, WindowBounds)>, cx: &mut App) {
    if let Some((handle, _)) = main_window(cx) {
        let _ = handle.update(cx, |_, window, _| window.activate_window());
        return;
    }
    let (display_id, window_bounds) = match at.or_else(|| window_state::load(cx)) {
        Some((display, bounds)) => (Some(display), bounds),
        None => (None, WindowBounds::Windowed(Bounds::centered(None, size(px(1440.), px(900.)), cx))),
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
        move |window, cx| cx.new(|cx| MainWindow::new(saved, window, cx)),
    );
    match opened {
        Ok((handle, main)) => {
            cx.default_global::<Registry>().0 = Some((handle, main.downgrade()));
            let _ = handle.update(cx, |_, window, _| window.activate_window());
        },
        Err(e) => log::error!("failed to open window: {e}"),
    }
}

/// Add `root` to the workspace (or show it, when it is in already),
/// opening the window if it is closed.
pub fn open_folder(root: PathBuf, cx: &mut App) {
    let root = dunce::canonicalize(&root).unwrap_or(root);
    window_state::push_recent(&root);
    crate::menus::install(cx);
    match main_window(cx) {
        Some((handle, main)) => {
            let _ = handle.update(cx, |_, window, cx| {
                main.update(cx, |m, cx| m.open_folder(root, window, cx));
                window.activate_window();
            });
        },
        None => open_window(window_state::SavedWorkspace::fresh(root), None, cx),
    }
}

/// Ask for folders and add them to the workspace.
pub fn prompt_open_folder(cx: &mut App) {
    let rx =
        cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: true, prompt: Some("Add to Workspace".into()) });
    cx.spawn(async move |cx| {
        let picked = rx.await.ok().and_then(Result::ok).flatten().unwrap_or_default();
        cx.update(|cx| {
            for dir in picked {
                open_folder(dir, cx);
            }
        });
    })
    .detach();
}

/// Hand a Claude hook event to the workspace.
pub fn dispatch_hook(ev: tern_bridge::HookEvent, cx: &mut App) {
    let Some((handle, main)) = main_window(cx) else { return };
    let ws = main.read(cx).ws.clone();
    let _ = handle.update(cx, |_, window, cx| ws.update(cx, |w, cx| w.on_hook(ev, window, cx)));
}

pub struct MainWindow {
    ws: Entity<Workspace>,
    /// The welcome page shows (no folder, no tab).
    was_empty: bool,
    title: String,
    /// Where the window is, for the saved session.
    placement: Option<(String, WindowBounds)>,
    focus: FocusHandle,
    /// The application menus, drawn by the window off macOS (see `menus`).
    #[cfg(not(target_os = "macos"))]
    menu_bar: Entity<gpui_kit::component::menu::AppMenuBar>,
    _subs: Vec<Subscription>,
    _save_placement: Task<()>,
}

impl MainWindow {
    fn new(saved: window_state::SavedWorkspace, window: &mut Window, cx: &mut Context<Self>) -> Self {
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
            }
        });
        let services = cx.global::<AppServices>().0.clone();
        let ws = cx.new(|cx| Workspace::new(saved, services, window, cx));
        let ws_sub = cx.subscribe_in(&ws, window, |this, _, ev: &WorkspaceEvent, window, cx| match ev {
            WorkspaceEvent::TabsChanged => {
                this.changed(cx);
                // The welcome page comes and goes with the last folder and tab.
                let empty = this.ws.read(cx).is_empty();
                if empty != this.was_empty {
                    this.was_empty = empty;
                    this.focus_active(window, cx);
                }
                cx.notify();
            },
        });
        // Closing the window quits Tern (see `main`): like ⌘Q, its
        // terminals keep running and come back at the next launch.
        cx.on_release(|this: &mut Self, cx| this.ws.update(cx, |w, cx| w.shutdown(cx))).detach();
        let this = Self {
            was_empty: ws.read(cx).is_empty(),
            ws,
            title: String::new(),
            placement: window_state::placement(window, cx),
            focus: cx.focus_handle(),
            #[cfg(not(target_os = "macos"))]
            menu_bar: gpui_kit::component::menu::AppMenuBar::new(cx),
            _subs: vec![bounds_sub, activation_sub, ws_sub],
            _save_placement: Task::ready(()),
        };
        this.focus_active(window, cx);
        this
    }

    /// The workspace or placement changed: save the session once this update ends.
    fn changed(&self, cx: &mut Context<Self>) {
        cx.defer(save_session);
    }

    /// Add `root` to the workspace.
    pub fn open_folder(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.ws.update(cx, |w, cx| w.add_folder(root, window, cx));
        self.focus_active(window, cx);
        cx.notify();
    }

    fn focus_active(&self, window: &mut Window, cx: &mut App) {
        if self.ws.read(cx).is_empty() {
            window.focus(&self.focus, cx);
        } else {
            self.ws.update(cx, |w, cx| w.focus(window, cx));
        }
    }

    fn render_welcome(&self, cx: &mut Context<Self>) -> AnyElement {
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
            .var("open_key", crate::platform::shortcut("⌘O").to_string())
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

    /// Window-level control commands, else the workspace's (see `control`).
    #[cfg(debug_assertions)]
    fn control_query(&mut self, line: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let cmd = line.split_whitespace().next().unwrap_or_default();
        let folders = |cx: &App| format!("folders={:?}", self.ws.read(cx).folders());
        match cmd {
            "ws" => folders(cx),
            "ws-open" => {
                self.open_folder(PathBuf::from(line["ws-open".len()..].trim()), window, cx);
                "ok".into()
            },
            "ws-remove" => {
                let path = PathBuf::from(line["ws-remove".len()..].trim());
                self.ws.update(cx, |w, cx| w.remove_folder(&path, cx));
                "ok".into()
            },
            "state" if self.ws.read(cx).is_empty() => format!("{} welcome", folders(cx)),
            _ => {
                let out = self.ws.update(cx, |w, cx| w.control_query(line, window, cx));
                if cmd == "state" { format!("{} {out}", folders(cx)) } else { out }
            },
        }
    }
}

/// Debug control: CMD goes to the window.
#[cfg(debug_assertions)]
pub fn control(line: &str, cx: &mut App) -> String {
    #[cfg(any(target_os = "macos", windows))]
    if let Some(out) = crate::shot::control(line, cx) {
        return out;
    }
    if line.trim() == "restart-background" {
        crate::restart::restart(cx);
        return "ok".into();
    }
    let Some((handle, main)) = main_window(cx) else { return "error: no window".into() };
    let line = line.to_owned();
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
        let empty = self.ws.read(cx).is_empty();
        let name = self.ws.read(cx).name();
        let title = if name.is_empty() { "Tern".to_owned() } else { name };
        if title != self.title {
            window.set_window_title(&title);
            self.title = title;
        }
        let scope = template::Scope::new("window").var("workspace", !empty);
        let scope = if empty { scope.slot("welcome", self.render_welcome(cx)) } else { scope.slot("workspace", self.ws.clone()) };
        #[cfg(not(target_os = "macos"))]
        let scope = scope.var("menu_bar", true).slot("menu_bar", self.menu_bar.clone());
        let body = template::render("window", scope, &mut |_, _, d| d.into_any_element(), cx);
        div()
            .id("main-window")
            .key_context("MainWindow")
            .track_focus(&self.focus)
            .size_full()
            .on_action(cx.listener(|_, _: &OpenFolder, _, cx| prompt_open_folder(cx)))
            .on_action(cx.listener(|_, _: &SelectTheme, window, cx| theme_picker::open(window, cx)))
            .on_action(cx.listener(|_, _: &crate::restart::RestartBackground, window, cx| crate::restart::confirm(window, cx)))
            // The workspace opens it in a tab; without one, the system does.
            .on_action(cx.listener(|_, _: &OpenSettings, _, cx| cx.open_with_system(&crate::settings::settings_path(cx))))
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
        assert_eq!(reveal_target(&[None, Some(1), Some(3)], 3, true), RevealTarget::Focus(2));
        assert_eq!(reveal_target(&[None, Some(1)], 1, true), RevealTarget::Focus(1));
    }

    #[test]
    fn reveal_target_adopts_what_no_tab_shows() {
        assert_eq!(reveal_target(&[Some(1)], 9, true), RevealTarget::Adopt);
        assert_eq!(reveal_target(&[], 9, true), RevealTarget::Adopt);
    }

    #[test]
    fn adoption_workspace_holds_just_the_session() {
        // No default Claude tab and no folder: the session alone, in its own.
        let shell = SessionInfo { id: 7, cwd: Some("/p".into()), title: Some("vim".into()), ..Default::default() };
        let w = adoption_workspace(&shell);
        assert!(w.folders.is_empty());
        assert_eq!(w.tabs, [SavedTab::Shell { name: Some("vim".into()), mux: Some(7), cwd: Some("/p".into()) }]);
        let claude = SessionInfo { id: 8, claude: true, claude_session: Some("s".into()), ..Default::default() };
        let w = adoption_workspace(&claude);
        assert_eq!(w.tabs, [SavedTab::Claude { name: None, session: Some("s".into()), mux: Some(8), cwd: None }]);
    }

    #[test]
    fn reveal_target_unknown_session_does_nothing() {
        // Ended between the menu and the click; a stale tab may still name it.
        assert_eq!(reveal_target(&[Some(9)], 9, false), RevealTarget::Nothing);
        assert_eq!(reveal_target(&[], 9, false), RevealTarget::Nothing);
    }
}
