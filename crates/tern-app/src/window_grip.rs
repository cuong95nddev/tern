//! Grips that resize a small window of its own (the agent monitor, the
//! screenshot tray) by its right and bottom edges. These windows have no
//! system frame, and gpui starts a system resize only on Linux.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::*;

/// Which edges a grip moves.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Right,
    Bottom,
    Corner,
}

impl Edge {
    fn cursor(self) -> CursorStyle {
        match self {
            Edge::Right => CursorStyle::ResizeLeftRight,
            Edge::Bottom => CursorStyle::ResizeUpDown,
            Edge::Corner => CursorStyle::ResizeUpLeftDownRight,
        }
    }
}

/// A resize under way: the edges moved, where it started (screen
/// coordinates) and the window's size then.
#[derive(Clone, Copy)]
struct Start {
    edge: Edge,
    at: Point<Pixels>,
    size: Size<Pixels>,
}

/// A window's grips: held by its view, shared by the grips and the
/// tracker that follows the pointer.
#[derive(Clone)]
pub struct Grips {
    start: Rc<Cell<Option<Start>>>,
    min: Size<Pixels>,
}

/// The pointer in screen coordinates: the window may move as it resizes.
fn on_screen(position: Point<Pixels>, window: &Window) -> Point<Pixels> {
    window.bounds().origin + position
}

impl Grips {
    pub fn new(min: Size<Pixels>) -> Self {
        Self { start: Rc::default(), min }
    }

    /// Make `d` the grip of `edge`. A double click on it calls `reset`.
    pub fn grip(&self, d: Div, id: &'static str, edge: Edge, reset: impl Fn(&mut Window, &mut App) + 'static) -> AnyElement {
        let start = self.start.clone();
        d.id(id)
            .cursor(edge.cursor())
            .on_mouse_down(MouseButton::Left, move |e, window, cx| {
                cx.stop_propagation();
                if e.click_count > 1 {
                    start.set(None);
                    return reset(window, cx);
                }
                start.set(Some(Start { edge, at: on_screen(e.position, window), size: window.viewport_size() }));
            })
            .into_any_element()
    }

    /// Whether a resize is under way.
    pub fn resizing(&self) -> bool {
        self.start.get().is_some()
    }

    /// An empty element that follows the pointer while a grip is held,
    /// resizing the window; `done` gets the size the window took.
    pub fn tracker(&self, done: impl Fn(Size<Pixels>, Edge, &mut App) + 'static) -> impl IntoElement {
        let (start, min) = (self.start.clone(), self.min);
        let done = Rc::new(done);
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let (moving, up) = (start.clone(), start.clone());
                let done = done.clone();
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                    let Some(s) = moving.get().filter(|_| phase == DispatchPhase::Capture) else { return };
                    if e.pressed_button != Some(MouseButton::Left) {
                        moving.set(None);
                        return;
                    }
                    let d = on_screen(e.position, window) - s.at;
                    let mut size = s.size;
                    if s.edge != Edge::Bottom {
                        size.width = (s.size.width + d.x).max(min.width);
                    }
                    if s.edge != Edge::Right {
                        size.height = (s.size.height + d.y).max(min.height);
                    }
                    if size != window.viewport_size() {
                        window.resize(size);
                        done(size, s.edge, cx);
                    }
                    cx.stop_propagation();
                });
                window.on_mouse_event(move |e: &MouseUpEvent, phase, window, _| {
                    // Drawn again: the window may fit its content again.
                    if phase == DispatchPhase::Capture && e.button == MouseButton::Left && up.take().is_some() {
                        window.refresh();
                    }
                });
            },
        )
        .absolute()
        .size_0()
    }
}
