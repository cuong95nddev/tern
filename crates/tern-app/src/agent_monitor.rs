//! The agent monitor: a small panel floating over every app, listing the
//! coding agents running in Tern's terminals with their status. Drag it by
//! its header, resize it by its right and bottom edges; click an agent to
//! bring its tab forward.

use std::time::{Duration, Instant};

use gpui_kit::component::ActiveTheme;
use gpui_kit::*;

use crate::agents::Agent;
use crate::main_window;
use crate::template::{self, Framed, Scope};
use crate::terminal_view::ActivityStatus;
use crate::window_grip::{Edge, Grips};
use crate::workspace::Workspace;

actions!(tern, [ToggleAgentMonitor]);

const WIDTH: f32 = 220.;
/// The starting height; the window then fits its content.
const HEIGHT: f32 = 60.;
/// The smallest the window can be made.
const MIN: (f32, f32) = (160., 48.);
/// How often the list is read again: statuses change in other windows.
const REFRESH: Duration = Duration::from_millis(500);
/// One cycle of a working dot's pulse.
const PULSE: Duration = Duration::from_secs(2);

/// The monitor window while it is open, and where it last was.
#[derive(Default)]
struct MonitorData {
    window: Option<AnyWindowHandle>,
    origin: Option<Point<Pixels>>,
    /// The width it was made, if not the starting one.
    width: Option<Pixels>,
    /// The height it was made: its list then scrolls. Else it fits its
    /// content.
    height: Option<Pixels>,
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
    let data = cx.default_global::<MonitorData>();
    let (width, height) = (data.width.unwrap_or(px(WIDTH)), data.height.unwrap_or(px(HEIGHT)));
    let origin = data.origin.unwrap_or_else(|| point(size.width - width - px(24.), px(64.)));
    let opened = gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(origin, gpui_kit::size(width, height)))),
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
    view: Entity<crate::terminal_view::TerminalView>,
}

/// Every agent, by window, workspace and tab order (so rows stay put).
fn entries(cx: &mut App) -> Vec<Entry> {
    let mut out = Vec::new();
    for (handle, ws) in main_window::all_workspaces(cx) {
        let workspace = ws.read(cx).name();
        for t in ws.read(cx).term_tabs(cx) {
            let Some(agent) = t.agent else { continue };
            let status = t.view.read(cx).status();
            out.push(Entry {
                handle,
                ws: ws.clone(),
                workspace: workspace.clone(),
                tab: t.id,
                title: t.title,
                agent,
                status,
                view: t.view,
            });
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
    /// Where the working dots' pulse cycles start, so they pulse together.
    started: Instant,
    /// Whether the last render showed a working dot.
    pulsing: bool,
    grips: Grips,
    _refresh: Task<()>,
}

impl Monitor {
    fn new(cx: &mut Context<Self>) -> Self {
        let refresh = cx.spawn(async move |this, cx| {
            loop {
                let Ok(wait) = this.read_with(cx, |m, _| m.next_wake()) else { break };
                cx.background_executor().timer(wait).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        let grips = Grips::new(gpui_kit::size(px(MIN.0), px(MIN.1)));
        Self { started: Instant::now(), pulsing: false, grips, _refresh: refresh }
    }

    /// Where the pulse is in its cycle, from 0 to 1.
    fn phase(&self) -> f32 {
        self.started.elapsed().as_secs_f32() % PULSE.as_secs_f32() / PULSE.as_secs_f32()
    }

    /// When to draw again: when a working dot's opacity next steps, else at
    /// the next read of the list. Not every frame, as an animation would.
    fn next_wake(&self) -> Duration {
        if !self.pulsing {
            return REFRESH;
        }
        let until = PULSE.mul_f32(next_pulse_step(self.phase())) + Duration::from_millis(1);
        until.min(REFRESH)
    }
}

/// The opacity of a working dot along its 2 s cycle: full, a stepped fade to
/// half, half, a stepped return.
fn pulse(t: f32) -> f32 {
    let steps = |x: f32| (x * 6.).floor() / 6.;
    match t {
        t if t < 0.4 => 1.,
        t if t < 0.5 => 1. - 0.5 * steps((t - 0.4) / 0.1),
        t if t < 0.9 => 0.5,
        t => 0.5 + 0.5 * steps((t - 0.9) / 0.1),
    }
}

/// How far into the cycle, from `t`, the pulse next changes: the steps of
/// its fades, and the ends of its cycle.
fn next_pulse_step(t: f32) -> f32 {
    let steps = (0..=6).map(|k| 0.4 + k as f32 / 60.).chain((0..=6).map(|k| 0.9 + k as f32 / 60.));
    let next = steps.filter(|s| *s > t + 1e-4).fold(1., f32::min);
    next - t
}

/// A status dot's color: it pulses while working (`phase` is the pulse's
/// place in its cycle).
fn dot_color(status: ActivityStatus, phase: f32, color: Hsla) -> Hsla {
    if status == ActivityStatus::Working { color.opacity(pulse(phase)) } else { color }
}

/// What a row of the list does when clicked.
enum Row {
    Workspace(AnyWindowHandle, Entity<Workspace>),
    Agent(AnyWindowHandle, Entity<Workspace>, EntityId),
}

impl Render for Monitor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entries = entries(cx);
        self.pulsing = entries.iter().any(|e| e.status == ActivityStatus::Working);
        let phase = self.phase();
        let theme = cx.theme().clone();
        let count = |s| entries.iter().filter(|e| e.status == s).count();
        let (working, waiting, done) = (count(ActivityStatus::Working), count(ActivityStatus::Blocked), count(ActivityStatus::Done));
        let summary = [(waiting, "need input"), (working, "working"), (done, "done")]
            .into_iter()
            .filter(|(n, _)| *n > 0)
            .map(|(n, what)| format!("{n} {what}"))
            .collect::<Vec<_>>()
            .join(" · ");
        // Grouped by workspace: a line (which shows it) over its agents.
        let (mut rows, mut items) = (Vec::new(), Vec::new());
        let mut group = None;
        for e in &entries {
            if group != Some(e.ws.entity_id()) {
                group = Some(e.ws.entity_id());
                // As in the agents panel (`agents.folder`), without its buttons.
                let top = entries.iter().filter(|o| o.ws == e.ws).map(|o| o.status).max_by_key(|s| crate::agents::priority(*s));
                let mut item = Scope::new("workspace").var("folder", true).var("name", e.workspace.as_str()).var("open", true);
                let needs = entries.iter().filter(|o| o.ws == e.ws && o.status == ActivityStatus::Blocked).count();
                if needs > 0 {
                    item = item.var("needs", crate::agents::needs_label(needs));
                }
                if let Some(s) = top.filter(|s| *s != ActivityStatus::None) {
                    item = item.var("dot", true).var("dot_color", dot_color(s, phase, crate::agents::status_color(s, &theme)));
                }
                items.push(item);
                rows.push(Row::Workspace(e.handle, e.ws.clone()));
            }
            // As in the agents panel (`agents.tab`).
            let item = Scope::new("agent")
                .var("label", crate::agents::display_name(&e.title, e.agent))
                .var("shown", true)
                .slot("icon", e.agent.icon(&theme));
            let item = crate::agents::ring_vars(item, e.view.read(cx), false, None, None, &theme);
            items.push(item);
            rows.push(Row::Agent(e.handle, e.ws.clone(), e.tab));
        }
        // Fit the window to the panel's height, which changes with the list,
        // unless it was made a height.
        let fixed = cx.default_global::<MonitorData>().height.is_some();
        let resizing = self.grips.resizing();
        let fit = canvas(
            move |bounds, window, cx| {
                let h = bounds.size.height;
                if fixed || resizing {
                    return;
                }
                if (window.viewport_size().height - h).abs() > px(1.) {
                    let w = window.viewport_size().width;
                    window.defer(cx, move |window, _| window.resize(gpui_kit::size(w, h)));
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0();
        let scope =
            Scope::new("agent_monitor").var("summary", summary).var("agents", !entries.is_empty()).list("rows", items).slot("fit", fit);
        let panel = template::render(
            "agent_monitor",
            scope,
            &mut |name, ix, f| match (name, ix.and_then(|i| rows.get(i)), f) {
                // Made a height, the panel fills the window and its list scrolls.
                ("panel", _, Framed::Box(d)) => if fixed { d.size_full() } else { d }.into_any_element(),
                ("list", _, Framed::Box(d)) => {
                    let d = d.id("monitor-list").overflow_y_scroll();
                    if fixed { d.flex_1().min_h_0() } else { d }.into_any_element()
                },
                ("grip_right", _, Framed::Box(d)) => self.grips.grip(d, "monitor-grip-right", Edge::Right, reset),
                ("grip_bottom", _, Framed::Box(d)) => self.grips.grip(d, "monitor-grip-bottom", Edge::Bottom, reset),
                ("grip_corner", _, Framed::Box(d)) => self.grips.grip(d, "monitor-grip-corner", Edge::Corner, reset),
                // The header moves the window: the system drags it on Windows,
                // the mouse down starts the move elsewhere.
                ("header", _, Framed::Box(d)) => d
                    .id("monitor-header")
                    .window_control_area(WindowControlArea::Drag)
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
                    .into_any_element(),
                ("close", _, Framed::Button(b)) => b
                    .on_click(|_, _, cx| {
                        hide(cx);
                    })
                    .into_any_element(),
                ("folder", Some(Row::Workspace(handle, ws)), Framed::Box(d)) => {
                    let (handle, ws) = (*handle, ws.clone());
                    d.id(("workspace", ix.unwrap_or(0)))
                        .cursor_pointer()
                        .on_click(move |_, _, cx| main_window::reveal(handle, &ws, cx))
                        .into_any_element()
                },
                ("tab", Some(Row::Agent(handle, ws, tab)), Framed::Box(d)) => {
                    let (handle, ws, tab) = (*handle, ws.clone(), *tab);
                    d.id(("agent", ix.unwrap_or(0)))
                        .cursor_pointer()
                        .on_click(move |_, _, cx| jump(handle, ws.clone(), tab, cx))
                        .into_any_element()
                },
                (_, _, f) => f.into_any_element(),
            },
            cx,
        );
        // The window is the panel's width; its height follows the panel.
        let tracker = self.grips.tracker(|size, edge, cx| {
            let data = cx.default_global::<MonitorData>();
            data.width = Some(size.width);
            if edge != Edge::Right {
                data.height = Some(size.height);
            }
        });
        let root = div().w_full().child(panel).child(tracker);
        if fixed { root.h_full() } else { root }
    }
}

/// Back to the starting width, fitting the content (a double click on a
/// grip).
fn reset(window: &mut Window, cx: &mut App) {
    let data = cx.default_global::<MonitorData>();
    (data.width, data.height) = (None, None);
    window.resize(gpui_kit::size(px(WIDTH), window.viewport_size().height));
    window.refresh();
}

#[cfg(test)]
mod tests {
    use super::{next_pulse_step, pulse};

    /// Waking at each step misses no change: the pulse holds until then.
    #[test]
    fn pulse_holds_until_its_next_step() {
        let mut t = 0.;
        let mut wakes = 0;
        while t < 1. {
            let step = next_pulse_step(t);
            assert!(step > 0.);
            let held = pulse(t);
            for i in 1..20 {
                let u = t + step * i as f32 / 20.;
                assert!((pulse(u) - held).abs() < 1e-6, "pulse changed at {u} before the step at {}", t + step);
            }
            t += step + 1e-4;
            wakes += 1;
        }
        assert!(wakes < 20, "{wakes} wakes a cycle");
    }
}
