//! What the drawn docks report back to the code that owns the arrangement.

use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;

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
    /// `part` floats at `rect` in an area of `area` (width, height).
    Float { part: SharedString, rect: arrange::Rect, area: (f32, f32) },
    /// Floating `part` docks again.
    Dock { part: SharedString, kind: SharedString },
    /// `part` is in a window of its own at `rect` (pixels) on display `display`.
    Window { part: SharedString, display: String, rect: arrange::Rect },
    /// `part` is among the panes, on side `edge` of `target` (a pane or a part there).
    Center { part: SharedString, target: crate::panes::Layout, edge: Edge },
    /// Panel group `group` lays its panels out as `mode`.
    GroupMode { group: SharedString, mode: arrange::GroupMode },
    /// Panel group `group` is drawn with its borders, or without.
    GroupBorder { group: SharedString, on: bool },
    /// Panel group `group` is closed: its panels go back to the docks.
    CloseGroup { group: SharedString },
    /// A new, empty panel group at `at` in `dock`.
    AddGroup { dock: SharedString, at: arrange::Drop },
}

/// Called with each change.
pub type OnChange = Rc<dyn Fn(Change, &mut Window, &mut App)>;

/// A part being dragged by its header.
#[derive(Clone)]
pub struct DraggedPart {
    pub part: SharedString,
    pub kind: SharedString,
    pub title: SharedString,
    /// It floats: it follows the pointer itself, with no chip.
    pub floating: bool,
    /// It is in a window of its own: its handle gives its menu, no drag.
    pub popped: bool,
}

impl Render for DraggedPart {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.floating {
            return div().into_any_element();
        }
        style::drag_chip(self.title.clone(), None, cx).into_any_element()
    }
}

/// The part being dragged, if any: gpui keeps the active drag's value to itself.
#[derive(Default)]
pub struct ActiveDrag(pub Option<DraggedPart>);

impl Global for ActiveDrag {}

/// What starts dragging part `p`: the docks and panes learn what is dragged.
pub fn start_drag(p: &DraggedPart, _: Point<Pixels>, window: &mut Window, cx: &mut App) -> Entity<DraggedPart> {
    cx.set_global(ActiveDrag(Some(p.clone())));
    track_from(window, cx);
    cx.refresh_windows();
    cx.new(|_| p.clone())
}

/// A floating part's side or corner being dragged to resize it.
#[derive(Clone)]
pub struct FloatResize {
    pub part: SharedString,
    pub sides: arrange::Sides,
}

impl Render for FloatResize {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// The floating part being resized, if any.
#[derive(Default)]
pub struct Resizing(pub Option<FloatResize>);

impl Global for Resizing {}

/// What starts resizing a floating part.
pub fn start_resize(r: &FloatResize, _: Point<Pixels>, window: &mut Window, cx: &mut App) -> Entity<FloatResize> {
    cx.set_global(Resizing(Some(r.clone())));
    track_from(window, cx);
    cx.new(|_| r.clone())
}

/// The pointer during a drag, for the floating parts: where the drag started
/// and where it is now (window coordinates), and the area floating parts sit
/// in, once known.
#[derive(Default, Clone, Copy)]
pub struct FloatTrack {
    pub start: Point<Pixels>,
    pub at: Point<Pixels>,
    pub area: Option<Bounds<Pixels>>,
}

impl Global for FloatTrack {}

fn track_from(window: &Window, cx: &mut App) {
    let at = window.mouse_position();
    let area = cx.try_global::<FloatTrack>().and_then(|t| t.area);
    cx.set_global(FloatTrack { start: at, at, area });
}

/// Which parts float, are in windows of their own or among the panes, and
/// which may not float or leave the window (`float`, `window` don't hold):
/// read by the panels' headers, drawn apart from the docks.
#[derive(Default, PartialEq)]
pub struct Floats {
    pub floating: BTreeSet<String>,
    pub popped: BTreeSet<String>,
    pub centered: BTreeSet<String>,
    pub grounded: BTreeSet<&'static str>,
    pub windowless: BTreeSet<&'static str>,
}

impl Global for Floats {}

/// Floats a part over the docks (its header's menu).
#[derive(Clone, PartialEq, Action)]
#[action(namespace = workspace, no_json)]
pub struct FloatPart(pub SharedString);

/// Moves a part to a window of its own (its header's menu).
#[derive(Clone, PartialEq, Action)]
#[action(namespace = workspace, no_json)]
pub struct PopOutPart(pub SharedString);

/// Docks a floating part again (its header's menu).
#[derive(Clone, PartialEq, Action)]
#[action(namespace = workspace, no_json)]
pub struct DockPart(pub SharedString);

pub fn area_size(b: Bounds<Pixels>) -> (f32, f32) {
    (f32::from(b.size.width), f32::from(b.size.height))
}

/// How far the pointer went since the drag started.
fn delta(t: &FloatTrack) -> (f32, f32) {
    (f32::from(t.at.x - t.start.x), f32::from(t.at.y - t.start.y))
}

/// Where floating part `part` (stored as `f`) is drawn while it is moved or
/// resized, in pixels from the area's top-left; None when it isn't.
pub fn live_rect(part: &str, f: &arrange::Float, cx: &App) -> Option<arrange::Rect> {
    let t = cx.try_global::<FloatTrack>()?;
    let area = area_size(t.area?);
    let (dx, dy) = delta(t);
    let r = f.rect(area);
    if let Some(rs) = cx.try_global::<Resizing>().and_then(|r| r.0.as_ref()).filter(|r| r.part == part) {
        let look = style::el("floating", cx);
        let min = (f32::from(look.px("min_width")), f32::from(look.px("min_height")));
        return Some(arrange::resized(r, rs.sides, dx, dy, min, area));
    }
    let moving = cx.try_global::<ActiveDrag>().and_then(|d| d.0.as_ref()).is_some_and(|p| p.floating && p.part == part);
    moving.then(|| arrange::moved(r, dx, dy, area))
}

/// Where a part that starts floating at the pointer goes: its size is the
/// `floating` element's, its top-left where the grip was grabbed.
/// `at` and `grab` in window coordinates and from the grip's top-left.
pub fn float_rect(at: Point<Pixels>, grab: Point<Pixels>, area: Bounds<Pixels>, cx: &App) -> arrange::Rect {
    let look = style::el("floating", cx);
    let size = area_size(area);
    let (w, h) = (f32::from(look.px("width")).min(size.0), f32::from(look.px("height")).min(size.1));
    let at = at - area.origin - grab;
    arrange::moved(arrange::Rect { x: 0., y: 0., w, h }, f32::from(at.x), f32::from(at.y), size)
}

/// The workspace's root `d`, where the floating parts are: it follows the
/// pointer while a floating part moves or resizes, and takes the drops no
/// zone took: a part let go there floats (`arrangement` says where they are).
pub fn floats(d: Div, arrangement: Option<Arc<arrange::Arrangement>>, on_change: OnChange) -> Div {
    fn follow(at: Point<Pixels>, area: Bounds<Pixels>, live: bool, window: &mut Window, cx: &mut App) {
        let mut t = cx.try_global::<FloatTrack>().copied().unwrap_or_default();
        let moved = t.at != at;
        t.at = at;
        t.area = Some(area);
        cx.set_global(t);
        if live && moved {
            window.refresh();
        }
    }
    let resize_a = arrangement.clone();
    let resize_change = on_change.clone();
    d.on_drag_move(|ev: &DragMoveEvent<DraggedPart>, window, cx| {
        let live = ev.drag(cx).floating;
        follow(ev.event.position, ev.bounds, live, window, cx);
    })
    .on_drag_move(|ev: &DragMoveEvent<FloatResize>, window, cx| follow(ev.event.position, ev.bounds, true, window, cx))
    .on_drop(move |p: &DraggedPart, window, cx| {
        let locked = cx.try_global::<LayoutLock>().is_some_and(|l| l.0);
        let grounded = cx.try_global::<Floats>().is_some_and(|f| f.grounded.contains(p.part.as_ref()));
        let Some(t) = cx.try_global::<FloatTrack>().copied() else { return };
        let Some(b) = t.area.filter(|_| p.kind != "pane" && !locked && !grounded) else { return };
        let area = area_size(b);
        // Let go outside every zone, a floating part moves; a docked one stays.
        let Some(f) = arrangement.as_ref().and_then(|a| a.floating.get(p.part.as_ref())) else { return };
        let (dx, dy) = delta(&t);
        let rect = arrange::moved(f.rect(area), dx, dy, area);
        on_change(Change::Float { part: p.part.clone(), rect, area }, window, cx);
    })
    .on_drop(move |r: &FloatResize, window, cx| {
        let f = resize_a.as_ref().and_then(|a| a.floating.get(r.part.as_ref()));
        let rect = f.and_then(|f| live_rect(&r.part, f, cx));
        let area = cx.try_global::<FloatTrack>().and_then(|t| t.area);
        cx.set_global(Resizing(None));
        if let (Some(rect), Some(b)) = (rect, area) {
            resize_change(Change::Float { part: r.part.clone(), rect, area: area_size(b) }, window, cx);
        }
    })
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

/// The strips along the sides of an area holding a panel group, each with
/// its side, for the code to make take drags (`drop_zone`, `size` wide; the
/// pointer over one lights it as `drop_zone.hover` does for `T`). A drop on
/// one goes on that side of the group; the group's middle is its own, so a
/// band takes at most a quarter of the area (a group as thin as a status
/// bar keeps a middle to drop onto).
pub fn side_bands<T: 'static>(sides: &[Edge], cx: &App) -> Vec<(Edge, Div)> {
    let look = style::el("drop_zone", cx);
    let hover = style::el("drop_zone.hover", cx);
    let size = look.px("size");
    sides
        .iter()
        .map(|&edge| {
            let d = look.apply(div().absolute());
            let d = match edge {
                Edge::Left => d.top_0().bottom_0().left_0().w(size).max_w(relative(0.25)),
                Edge::Right => d.top_0().bottom_0().right_0().w(size).max_w(relative(0.25)),
                Edge::Top => d.left_0().right_0().top_0().h(size).max_h(relative(0.25)),
                Edge::Bottom => d.left_0().right_0().bottom_0().h(size).max_h(relative(0.25)),
            };
            let hover = hover.clone();
            (edge, d.drag_over::<T>(move |s, _, _, _| hover.apply(s)))
        })
        .collect()
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
    if arrange::is_group(part) {
        return "Panel".into();
    }
    let mut c = part.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect::<String>()).unwrap_or_default().into()
}

/// Whether the layout is being edited (View > Edit Layout): only then do
/// panels drag, panel groups show their title bars, and the seams between
/// them offer to add one.
#[derive(Default)]
pub struct LayoutEdit(pub bool);

impl Global for LayoutEdit {}

/// True while the layout is being edited.
pub fn editing(cx: &App) -> bool {
    cx.try_global::<LayoutEdit>().is_some_and(|e| e.0)
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
    part: impl Into<SharedString>,
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
    let mut scope = template::Scope::new(element).var("part", part.into()).var("title", title).slot("buttons", buttons);
    if let Some(n) = count {
        scope = scope.var("count", n);
    }
    template::render(element, scope, &mut |_, _, d| d.into_any_element(), cx)
}
