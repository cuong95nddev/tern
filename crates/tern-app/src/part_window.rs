//! A part in an OS window of its own (layout phase D): the workspace opens
//! one per part its arrangement puts in a window, and this draws the part
//! there with the workspace's values.

use std::time::Duration;

use gpui_kit::*;

use crate::dock::{DockPart, FloatPart, PopOutPart};
use crate::workspace::Workspace;
use crate::{arrange, template, window_state};

/// The open part windows: they don't keep Tern running by themselves.
#[derive(Default)]
pub struct PartWindows(pub Vec<AnyWindowHandle>);

impl Global for PartWindows {}

/// Whether `window` holds a part of its own.
pub fn is_part_window(window: AnyWindowHandle, cx: &App) -> bool {
    cx.try_global::<PartWindows>().is_some_and(|p| p.0.contains(&window))
}

pub struct PartWindow {
    ws: WeakEntity<Workspace>,
    part: SharedString,
    title: String,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
    _save: Task<()>,
}

/// Opens part `part` of `ws` in a window at `bounds` (content bounds on
/// display `display`, else centered on the main display).
pub fn open(
    ws: Entity<Workspace>,
    part: SharedString,
    display: Option<DisplayId>,
    bounds: Bounds<Pixels>,
    cx: &mut App,
) -> Option<AnyWindowHandle> {
    let opened = gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            display_id: display,
            titlebar: Some(TitlebarOptions { title: Some(crate::dock::part_title(&part)), ..Default::default() }),
            window_min_size: Some(size(px(120.), px(80.))),
            app_id: Some("dev.tern.Tern".into()),
            ..Default::default()
        },
        cx,
        |window, cx| cx.new(|cx| PartWindow::new(ws, part, window, cx)),
    );
    match opened {
        Ok((handle, _)) => {
            cx.default_global::<PartWindows>().0.push(handle);
            Some(handle)
        },
        Err(e) => {
            log::error!("failed to open a part's window: {e}");
            None
        },
    }
}

/// Closes part window `window` without docking its part (hidden, or its
/// workspace gone).
pub fn close(window: AnyWindowHandle, cx: &mut App) {
    cx.default_global::<PartWindows>().0.retain(|w| *w != window);
    cx.defer(move |cx| {
        let _ = window.update(cx, |_, w, _| w.remove_window());
    });
}

impl PartWindow {
    fn new(ws: Entity<Workspace>, part: SharedString, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let subs = vec![
            // Its values (which panels show, the folder) are the workspace's.
            cx.observe(&ws, |_, _, cx| cx.notify()),
            // Gone with its workspace; the arrangement keeps it for next time.
            cx.observe_release_in(&ws, window, |_, _, window, cx| {
                let me = window.window_handle();
                cx.default_global::<PartWindows>().0.retain(|w| *w != me);
                window.remove_window();
            }),
            // A drag or a resize sends many: kept once they stop.
            cx.observe_window_bounds(window, |this: &mut Self, window, cx| {
                let Some((display, WindowBounds::Windowed(b) | WindowBounds::Maximized(b) | WindowBounds::Fullscreen(b))) =
                    window_state::placement(window, cx)
                else {
                    return;
                };
                let rect = arrange::Rect {
                    x: f32::from(b.origin.x),
                    y: f32::from(b.origin.y),
                    w: f32::from(b.size.width),
                    h: f32::from(b.size.height),
                };
                let (ws, part) = (this.ws.clone(), this.part.clone());
                this._save = cx.spawn(async move |_, cx| {
                    cx.background_executor().timer(Duration::from_millis(400)).await;
                    let _ = ws.update(cx, |ws, cx| ws.part_window_moved(&part, &display, rect, cx));
                });
            }),
        ];
        // Its close button docks the part again.
        let (weak, name) = (ws.downgrade(), part.clone());
        window.on_window_should_close(cx, move |window, cx| {
            let me = window.window_handle();
            cx.default_global::<PartWindows>().0.retain(|w| *w != me);
            let _ = weak.update(cx, |ws, cx| ws.dock_named(&name, cx));
            true
        });
        PartWindow { ws: ws.downgrade(), part, title: String::new(), focus, _subs: subs, _save: Task::ready(()) }
    }
}

impl Render for PartWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(ws) = self.ws.upgrade() else { return div().into_any_element() };
        let title = format!("{} — {}", crate::dock::part_title(&self.part), ws.read(cx).name());
        if title != self.title {
            window.set_window_title(&title);
            self.title = title;
        }
        let part = template::render_part(&self.part, ws.read(cx).arranged_scope(self.ws.clone()), cx);
        let body = template::render(
            "part_window",
            template::Scope::new("part_window").slot("part", part),
            &mut |_, _, d| d.into_any_element(),
            cx,
        );
        let (to_ws, part) = (self.ws.clone(), self.part.clone());
        let float = move |_: &FloatPart, _: &mut Window, cx: &mut App| {
            let _ = to_ws.update(cx, |ws, cx| ws.float_from_window(&part, cx));
        };
        let (to_ws, part) = (self.ws.clone(), self.part.clone());
        let dock = move |_: &DockPart, _: &mut Window, cx: &mut App| {
            let _ = to_ws.update(cx, |ws, cx| ws.dock_named(&part, cx));
        };
        div()
            .id("part-window")
            .key_context("PartWindow")
            .track_focus(&self.focus)
            .size_full()
            .on_action(float)
            .on_action(dock)
            // Already in a window of its own.
            .on_action(|_: &PopOutPart, _, _| {})
            .child(body)
            .into_any_element()
    }
}
