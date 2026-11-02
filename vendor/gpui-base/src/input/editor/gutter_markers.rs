use std::{ops::Range, rc::Rc};

use gpui::{AnyView, App, Bounds, Context, Hsla, Pixels, SharedString, Window};
use ropey::Rope;

use super::{EditorState, RopeExt as _, decorations::adjust_range_for_edit};

/// An icon in the gutter of one line (inheritance, breakpoints, ...).
///
/// It stays on its line through edits, and goes away with it.
#[derive(Clone, Debug, PartialEq)]
pub struct GutterMarker {
    /// The host's id for it, given back on a click.
    pub id: usize,
    /// The 0-based line it sits on.
    pub line: usize,
    /// The svg asset to draw (`icons/arrow-down.svg`).
    pub icon: SharedString,
    /// The icon's color; the editor's text color when `None`.
    pub color: Option<Hsla>,
    /// Shown while the pointer is over it.
    pub tooltip: SharedString,
}

/// Called with a marker's `id` and the icon's bounds (window coordinates)
/// when it is clicked.
pub type GutterMarkerHandler = Rc<dyn Fn(usize, Bounds<Pixels>, &mut Window, &mut App)>;

/// Builds the tooltip view for a marker's `tooltip` text (the host's look).
pub type GutterMarkerTooltip = Rc<dyn Fn(SharedString, &mut Window, &mut App) -> AnyView>;

/// The markers of an editor, each tracked by the byte range of its line
/// (newline included): typing in the line keeps it there, deleting the line
/// deletes it.
#[derive(Default)]
pub(crate) struct GutterMarkers {
    markers: Vec<(Range<usize>, GutterMarker)>,
    pub(crate) handler: Option<GutterMarkerHandler>,
    pub(crate) tooltip: Option<GutterMarkerTooltip>,
    /// Keep the gutter's column even with no markers, so it doesn't jump.
    pub(crate) reserved: bool,
}

impl GutterMarkers {
    pub(crate) fn set(&mut self, text: &Rope, markers: Vec<GutterMarker>) {
        let lines = text.lines_len();
        self.markers = markers
            .into_iter()
            .filter(|m| m.line < lines)
            .map(|m| {
                let start = text.line_start_offset(m.line);
                let end = if m.line + 1 < lines { text.line_start_offset(m.line + 1) } else { text.len() };
                // The last line may be empty: it still holds its marker until edited.
                (start..end.max(start + 1), m)
            })
            .collect();
        self.markers.sort_by_key(|(r, m)| (r.start, m.id));
    }

    pub(crate) fn adjust_for_edit(&mut self, edited: &Range<usize>, inserted_len: usize) {
        self.markers.retain_mut(|(range, _)| {
            *range = adjust_range_for_edit(range, edited, inserted_len);
            range.start < range.end
        });
    }

    pub(crate) fn clear(&mut self) {
        self.markers.clear();
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.markers.is_empty()
    }

    /// The markers on buffer line `line` of `text`, in id order.
    pub(crate) fn on_line<'a>(&'a self, text: &'a Rope, line: usize) -> impl Iterator<Item = &'a GutterMarker> + 'a {
        let start = text.line_start_offset(line);
        let end = start + text.line_len(line);
        self.markers.iter().filter(move |(r, _)| r.start >= start && r.start <= end).map(|(_, m)| m)
    }

    /// The markers with their current lines.
    pub(crate) fn current(&self, text: &Rope) -> Vec<GutterMarker> {
        self.markers
            .iter()
            .map(|(r, m)| GutterMarker { line: text.offset_to_point(r.start.min(text.len())).row, ..m.clone() })
            .collect()
    }
}

impl EditorState {
    /// Replace the gutter markers.
    pub fn set_gutter_markers(&mut self, markers: Vec<GutterMarker>, cx: &mut Context<Self>) {
        let text = self.text.clone();
        self.extras.gutter_markers.set(&text, markers);
        cx.notify();
    }

    /// The gutter markers, on the lines they sit on now.
    pub fn gutter_markers(&self) -> Vec<GutterMarker> {
        self.extras.gutter_markers.current(&self.text)
    }

    /// What a click on a gutter marker does.
    pub fn set_gutter_marker_handler(&mut self, handler: Option<GutterMarkerHandler>) {
        self.extras.gutter_markers.handler = handler;
    }

    /// How marker tooltips are shown; none are without it.
    pub fn set_gutter_marker_tooltip(&mut self, tooltip: Option<GutterMarkerTooltip>) {
        self.extras.gutter_markers.tooltip = tooltip;
    }

    /// Keep room for gutter markers even while there are none.
    pub fn reserve_gutter_markers(&mut self, reserve: bool, cx: &mut Context<Self>) {
        if self.extras.gutter_markers.reserved != reserve {
            self.extras.gutter_markers.reserved = reserve;
            cx.notify();
        }
    }
}
