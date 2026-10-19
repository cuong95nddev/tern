//! What the drawn docks report back to the code that owns the arrangement.

use std::collections::BTreeSet;
use std::rc::Rc;

use gpui_kit::*;

use crate::arrange::{Edge, Zone};
use crate::{arrange, style, template};

/// A change the user made to the parts in the docks.
pub enum Change {
    /// A part dropped into a dock.
    Move { part: SharedString, kind: SharedString, dock: SharedString, at: arrange::Drop },
    /// The split at `path` in `dock` resized.
    Resize { dock: SharedString, path: Vec<usize>, sizes: Vec<arrange::Size> },
    /// A dock's own panel resized in the split that holds it.
    Width { dock: SharedString, width: f32 },
    /// The stack at `path` in `dock` shows `part`.
    Activate { dock: SharedString, path: Vec<usize>, part: SharedString },
}

/// Called with each change.
pub type OnChange = Rc<dyn Fn(Change, &mut Window, &mut App)>;

/// A part being dragged by its header.
#[derive(Clone)]
pub struct DraggedPart {
    pub part: SharedString,
    pub kind: SharedString,
    pub title: SharedString,
}

impl Render for DraggedPart {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        style::drag_chip(self.title.clone(), None, cx)
    }
}

/// The part being dragged, if any: gpui keeps the active drag's value to itself.
#[derive(Default)]
pub struct ActiveDrag(pub Option<DraggedPart>);

impl Global for ActiveDrag {}

/// What starts dragging part `p`: the docks and panes learn what is dragged.
pub fn start_drag(p: &DraggedPart, _: Point<Pixels>, _: &mut Window, cx: &mut App) -> Entity<DraggedPart> {
    cx.set_global(ActiveDrag(Some(p.clone())));
    cx.refresh_windows();
    cx.new(|_| p.clone())
}

/// Where a drag would drop now: the area under the pointer and the zone in it.
#[derive(Default)]
pub struct DropHint(pub Option<(SharedString, Zone)>);

impl Global for DropHint {}

/// The zone of `area` a drop would go to, while the pointer is over it.
pub fn hint(area: &str, cx: &App) -> Option<Zone> {
    cx.try_global::<DropHint>().and_then(|h| h.0.as_ref()).filter(|(a, _)| a == area).map(|(_, z)| *z)
}

/// Over an area that takes drops: where the one under the pointer would go
/// lights up (`drop_zone`). `on_zones` makes it follow drags.
pub fn zone_overlay(area: &str, cx: &App) -> Div {
    let lit = hint(area, cx).map(|zone| {
        let d = style::el("drop_zone", cx).apply(div().absolute());
        match zone {
            Zone::Center => d.inset_0(),
            Zone::Edge(Edge::Left) => d.top_0().bottom_0().left_0().w(relative(0.5)),
            Zone::Edge(Edge::Right) => d.top_0().bottom_0().right_0().w(relative(0.5)),
            Zone::Edge(Edge::Top) => d.left_0().right_0().top_0().h(relative(0.5)),
            Zone::Edge(Edge::Bottom) => d.left_0().right_0().bottom_0().h(relative(0.5)),
        }
    });
    div().absolute().inset_0().children(lit)
}

/// `overlay` (see `zone_overlay`) taking drags of `T`: on the area's sides
/// when `edges`, onto it when `center`. A drop goes to `on_drop` with its zone.
pub fn on_zones<T: 'static>(
    overlay: Div,
    area: SharedString,
    edges: bool,
    center: bool,
    on_drop: impl Fn(&T, Zone, &mut Window, &mut App) + 'static,
) -> Div {
    let moved = area.clone();
    overlay
        .on_drag_move(move |ev: &DragMoveEvent<T>, window, cx| {
            let (b, at) = (ev.bounds, ev.event.position);
            let next = if b.contains(&at) {
                let x = f32::from(at.x - b.left()) / f32::from(b.size.width).max(1.);
                let y = f32::from(at.y - b.top()) / f32::from(b.size.height).max(1.);
                Zone::pick(x, y, edges, center).map(|z| (moved.clone(), z))
            } else if hint(&moved, cx).is_some() {
                None
            } else {
                return;
            };
            if cx.try_global::<DropHint>().and_then(|h| h.0.clone()) != next {
                cx.set_global(DropHint(next));
                window.refresh();
            }
        })
        .on_drop(move |d: &T, window, cx| {
            let zone = hint(&area, cx);
            cx.set_global(DropHint(None));
            if let Some(zone) = zone.or(center.then_some(Zone::Center)) {
                on_drop(d, zone, window, cx);
            }
        })
}

/// A part's title where it has none of its own: its name, capitalized.
pub fn part_title(part: &str) -> SharedString {
    let mut c = part.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect::<String>()).unwrap_or_default().into()
}

/// Whether moving and resizing panels is turned off (`layout.locked`).
#[derive(Default)]
pub struct LayoutLock(pub bool);

impl Global for LayoutLock {}

/// The parts that can't be dragged now: their `drag` doesn't hold with the
/// values of the workspace that draws them.
#[derive(Default)]
pub struct PinnedParts(pub BTreeSet<&'static str>);

impl Global for PinnedParts {}

/// A side panel's title bar, drawn with the template named after its element
/// (`explorer.header`, …): its title (or the element's `title`), a count
/// when it has one, and its buttons (template `<element>.buttons`, drawn
/// with `buttons` and `frames`). Dragging it drags `part`.
pub fn panel_header(
    part: &'static str,
    element: &'static str,
    title: impl Into<SharedString>,
    count: Option<usize>,
    buttons: template::Scope,
    frames: &mut template::Frames,
    cx: &App,
) -> AnyElement {
    let e = style::el(element, cx);
    let own = e.text("title");
    let title = e.label(if own.is_empty() { title.into() } else { own });
    let buttons = template::render(&format!("{element}.buttons"), buttons, frames, cx);
    // A press on a button is the button's, not the start of a panel drag.
    let buttons = div().flex().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(buttons);
    let mut scope = template::Scope::new(element).var("part", part).var("title", title).slot("buttons", buttons);
    if let Some(n) = count {
        scope = scope.var("count", n);
    }
    template::render(element, scope, &mut |_, _, d| d.into_any_element(), cx)
}
