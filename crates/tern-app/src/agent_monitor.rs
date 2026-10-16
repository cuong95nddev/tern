//! The agent monitor: a small panel floating over every app, listing the
//! coding agents running in Tern's terminals with their status. Drag it by
//! its header; click an agent to bring its tab forward.

use std::time::Duration;

use gpui_kit::assets::IconName as AssetIcon;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::agents::Agent;
use crate::main_window;
use crate::terminal_view::ActivityStatus;
use crate::workspace::Workspace;

actions!(tern, [ToggleAgentMonitor]);

const WIDTH: f32 = 200.;
/// The starting height; the window then fits its content.
const HEIGHT: f32 = 60.;
/// How often the list is read again: statuses change in other windows.
const REFRESH: Duration = Duration::from_millis(500);

/// The monitor window while it is open, and where it last was.
#[derive(Default)]
struct MonitorData {
    window: Option<AnyWindowHandle>,
    origin: Option<Point<Pixels>>,
}

impl Global for MonitorData {}

pub fn init(cx: &mut App) {
    cx.on_action(|_: &ToggleAgentMonitor, cx| toggle(cx));
}

/// The monitor window while it is open.
pub fn window(cx: &mut App) -> Option<AnyWindowHandle> {
    cx.default_global::<MonitorData>().window
}

pub fn toggle(cx: &mut App) {
    if !hide(cx) {
        show(cx);
    }
}

/// Open the monitor (top right of the main display, or where it was last).
fn show(cx: &mut App) {
    let Some(display) = cx.primary_display() else { return };
    let size = display.bounds().size;
    let origin = cx.default_global::<MonitorData>().origin.unwrap_or_else(|| point(size.width - px(WIDTH + 24.), px(64.)));
    let opened = gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(origin, gpui_kit::size(px(WIDTH), px(HEIGHT))))),
            titlebar: None,
            focus: false,
            show: true,
            // Stays on top of other apps' windows.
            kind: WindowKind::PopUp,
            is_movable: true,
            is_resizable: false,
            is_minimizable: false,
            display_id: Some(display.id()),
            // Only the rounded panel shows.
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        },
        cx,
        |_, cx| cx.new(Monitor::new),
    );
    match opened {
        Ok((handle, _)) => {
            #[cfg(windows)]
            let _ = handle.update(cx, |_, window, _| see_through(window));
            cx.default_global::<MonitorData>().window = Some(handle);
            // The workspace bars show it open.
            cx.refresh_windows();
        },
        Err(e) => log::error!("failed to open the agent monitor: {e}"),
    }
}

/// How opaque the window is, of 255.
#[cfg(windows)]
const OPACITY: u8 = 220;

/// Make the window a little see-through, so it hides less of what it
/// floats over. The whole window: Windows doesn't blend a transparent
/// window background with what is under it here.
#[cfg(windows)]
fn see_through(window: &Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, LWA_ALPHA, SetLayeredWindowAttributes, SetWindowLongPtrW, WS_EX_LAYERED,
    };
    let Ok(handle) = HasWindowHandle::window_handle(window) else { return };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else { return };
    let hwnd = handle.hwnd.get() as windows_sys::Win32::Foundation::HWND;
    // SAFETY: `hwnd` is this live window's handle.
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | WS_EX_LAYERED as isize);
        SetLayeredWindowAttributes(hwnd, 0, OPACITY, LWA_ALPHA);
    }
}

/// Close the monitor, remembering where it was. Returns whether it was
/// open. It closes once the current update ends: this may run in one of
/// its own handlers.
pub fn hide(cx: &mut App) -> bool {
    let Some(window) = cx.default_global::<MonitorData>().window.take() else { return false };
    if let Ok(origin) = window.update(cx, |_, w, _| w.bounds().origin) {
        cx.default_global::<MonitorData>().origin = Some(origin);
    }
    cx.defer(move |cx| {
        let _ = window.update(cx, |_, w, _| w.remove_window());
        cx.refresh_windows();
    });
    true
}

/// An agent running in some terminal tab.
struct Entry {
    handle: AnyWindowHandle,
    ws: Entity<Workspace>,
    workspace: String,
    tab: EntityId,
    title: String,
    agent: Agent,
    status: ActivityStatus,
}

/// Every agent, by window, workspace and tab order (so rows stay put).
fn entries(cx: &mut App) -> Vec<Entry> {
    let mut out = Vec::new();
    for (handle, ws) in main_window::all_workspaces(cx) {
        let workspace = ws.read(cx).name();
        for t in ws.read(cx).term_tabs(cx) {
            let Some(agent) = t.agent else { continue };
            let status = t.view.read(cx).status();
            out.push(Entry { handle, ws: ws.clone(), workspace: workspace.clone(), tab: t.id, title: t.title, agent, status });
        }
    }
    out
}

/// Bring the agent's tab forward, in its workspace and window.
fn jump(handle: AnyWindowHandle, ws: Entity<Workspace>, tab: EntityId, cx: &mut App) {
    let _ = handle.update(cx, |_, window, cx| ws.update(cx, |w, cx| w.show_tab(tab, window, cx)));
    main_window::reveal(handle, &ws, cx);
}

pub struct Monitor {
    _refresh: Task<()>,
}

impl Monitor {
    fn new(cx: &mut Context<Self>) -> Self {
        let refresh = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        Self { _refresh: refresh }
    }
}

/// What a row calls the agent: its tab's title without the status glyph
/// some agents put first (Claude Code's "✳"), else the agent's name.
fn display_name(title: &str, agent: Agent) -> String {
    let bare = title.trim_start_matches(|c: char| !c.is_alphanumeric()).trim();
    if bare.is_empty() { agent.name().to_owned() } else { bare.to_owned() }
}

/// A status's dot color, as the tab badges show it.
fn status_color(status: ActivityStatus, cx: &App) -> Hsla {
    let theme = cx.theme();
    match status {
        ActivityStatus::Working => theme.warning,
        ActivityStatus::Blocked => theme.danger,
        ActivityStatus::Done => theme.success,
        ActivityStatus::None => theme.muted_foreground,
    }
}

impl Render for Monitor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entries = entries(cx);
        let theme = cx.theme().clone();
        let waiting = entries.iter().filter(|e| e.status == ActivityStatus::Blocked).count();
        let working = entries.iter().filter(|e| e.status == ActivityStatus::Working).count();
        let summary = match (working, waiting) {
            (0, 0) => String::new(),
            (w, 0) => format!("{w} working"),
            (0, b) => format!("{b} waiting"),
            (w, b) => format!("{w} working · {b} waiting"),
        };
        // The header moves the window: the system drags it on Windows, the
        // mouse down starts the move elsewhere.
        let header = div()
            .id("monitor-header")
            .flex()
            .items_center()
            .gap_1()
            .pl_1()
            .h_5()
            .text_xs()
            .text_color(theme.muted_foreground)
            .window_control_area(WindowControlArea::Drag)
            .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
            .child(Icon::new(AssetIcon::Bot).xsmall())
            .child(div().flex_1().truncate().child(if summary.is_empty() { "Agents".to_owned() } else { summary }))
            .child(Button::new("close").icon(IconName::Close).ghost().xsmall().on_click(|_, _, cx| {
                hide(cx);
            }));
        // Grouped by workspace: a label (which shows it) over its agents.
        let mut rows = Vec::new();
        let mut group = None;
        for (i, e) in entries.into_iter().enumerate() {
            if group != Some(e.ws.entity_id()) {
                group = Some(e.ws.entity_id());
                let (handle, ws) = (e.handle, e.ws.clone());
                rows.push(
                    div()
                        .id(("workspace", i))
                        .flex()
                        .items_center()
                        .gap_1()
                        .h_5()
                        .px_1()
                        .rounded_sm()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.list_hover))
                        .child(Icon::new(AssetIcon::FolderOpen).xsmall())
                        .child(div().min_w_0().truncate().child(e.workspace.clone()))
                        .on_click(move |_, _, cx| main_window::reveal(handle, &ws, cx)),
                );
            }
            let color = status_color(e.status, cx);
            let name = display_name(&e.title, e.agent);
            let Entry { handle, ws, tab, .. } = e;
            rows.push(
                div()
                    .id(("agent", i))
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .h_6()
                    .pl_3()
                    .pr_1()
                    .rounded_sm()
                    .text_xs()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.list_hover))
                    .child(e.agent.icon(&theme))
                    .child(div().flex_1().min_w_0().truncate().child(name))
                    .child(div().flex_shrink_0().size_2().rounded_full().bg(color))
                    .on_click(move |_, _, cx| jump(handle, ws.clone(), tab, cx)),
            );
        }
        let empty = rows.is_empty();
        // Fit the window to the panel's height, which changes with the list.
        let fit = canvas(
            |bounds, window, cx| {
                let h = bounds.size.height;
                if (window.viewport_size().height - h).abs() > px(1.) {
                    let w = window.viewport_size().width;
                    window.defer(cx, move |window, _| window.resize(gpui_kit::size(w, h)));
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0();
        div()
            .relative()
            .w_full()
            .flex()
            .flex_col()
            .p_1()
            .bg(theme.background)
            .text_color(theme.foreground)
            .border_1()
            .border_color(theme.border)
            .rounded_lg()
            .child(header)
            .when(empty, |d| d.child(div().px_1().pb_1().text_xs().text_color(theme.muted_foreground).child("No agents running")))
            .children(rows)
            .child(fit)
    }
}
