//! What the drawn docks report back to the code that owns the arrangement.

use std::rc::Rc;

use gpui_kit::*;

use crate::arrange;

/// A change the user made to the parts in the docks.
pub enum Change {
    /// A part dropped into a dock.
    #[allow(dead_code)] // Sent by dragging, which comes next.
    Move { part: SharedString, kind: SharedString, dock: SharedString, at: arrange::Drop },
    /// The split at `path` in `dock` resized.
    Resize { dock: SharedString, path: Vec<usize>, sizes: Vec<arrange::Size> },
    /// A dock's own panel resized in the split that holds it.
    Width { dock: SharedString, width: f32 },
}

/// Called with each change.
pub type OnChange = Rc<dyn Fn(Change, &mut Window, &mut App)>;
