//! What the drawn docks report back to the code that owns the arrangement.

use std::rc::Rc;

use gpui_kit::*;

use crate::{arrange, style, template};

/// A change the user made to the parts in the docks.
pub enum Change {
    /// A part dropped into a dock.
    Move { part: SharedString, kind: SharedString, dock: SharedString, at: arrange::Drop },
    /// The split at `path` in `dock` resized.
    Resize { dock: SharedString, path: Vec<usize>, sizes: Vec<arrange::Size> },
    /// A dock's own panel resized in the split that holds it.
    Width { dock: SharedString, width: f32 },
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
        style::drag_chip(cx).child(self.title.clone())
    }
}

/// The part being dragged, if any: gpui keeps the active drag's value to itself.
#[derive(Default)]
pub struct ActiveDrag(pub Option<DraggedPart>);

impl Global for ActiveDrag {}

/// Whether moving and resizing panels is turned off (`layout.locked`).
#[derive(Default)]
pub struct LayoutLock(pub bool);

impl Global for LayoutLock {}

/// A side panel's title bar, drawn with the template named after its element
/// (`explorer.header`, …): its title (or the element's `title`), a count
/// when it has one, and its `buttons`. Dragging it drags `part`.
pub fn panel_header(
    part: &'static str,
    element: &'static str,
    title: impl Into<SharedString>,
    count: Option<usize>,
    buttons: impl IntoIterator<Item = AnyElement>,
    cx: &App,
) -> AnyElement {
    let e = style::el(element, cx);
    let own = e.text("title");
    let title = e.label(if own.is_empty() { title.into() } else { own });
    let buttons = div().flex().items_center().gap(e.len("gap")).children(buttons);
    let mut scope = template::Scope::new(element).var("part", part).var("title", title).slot("buttons", buttons);
    if let Some(n) = count {
        scope = scope.var("count", n);
    }
    template::render(element, scope, &mut |_, _, d| d.into_any_element(), cx)
}
