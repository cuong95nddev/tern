//! Toolbar icons (24×24, Lucide style), drawn as masks in the text colour.

use gpui_kit::component::Icon;

use super::model::Tool;

macro_rules! svg {
    ($($body:literal)*) => {
        concat!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">"#,
            $($body,)*
            "</svg>"
        )
    };
}

pub const RECT: &str = svg!(r#"<rect x="3.5" y="5" width="17" height="14" rx="1.5"/>"#);
pub const ELLIPSE: &str = svg!(r#"<circle cx="12" cy="12" r="8.5"/>"#);
pub const LINE: &str = svg!(r#"<path d="M5 19 19 5"/>"#);
pub const ARROW: &str = svg!(r#"<path d="M18 6 6 18"/><path d="M8 6h10v10"/>"#);
pub const PEN: &str = svg!(r#"<path d="M21.2 6.8a2.8 2.8 0 0 0-4-4L3.8 16.2 3 21l4.8-.8z"/><path d="m15 5 4 4"/>"#);
pub const TEXT: &str = svg!(r#"<path d="M5 6V4h14v2"/><path d="M12 4v16"/><path d="M9 20h6"/>"#);
pub const MARKER: &str = svg!(r#"<circle cx="12" cy="12" r="9"/><path d="M10.5 9 12.5 7.5V16.5"/>"#);
pub const MOSAIC: &str = svg!(
    r#"<rect x="3.5" y="3.5" width="17" height="17" rx="1.5"/>"#
    r#"<path d="M9.2 3.5v17M14.8 3.5v17M3.5 9.2h17M3.5 14.8h17"/>"#
);
pub const UNDO: &str = svg!(r#"<path d="M9 14 4 9l5-5"/><path d="M4 9h10.5a5.5 5.5 0 0 1 0 11H11"/>"#);
pub const REDO: &str = svg!(r#"<path d="m15 14 5-5-5-5"/><path d="M20 9H9.5a5.5 5.5 0 0 0 0 11H13"/>"#);
pub const CLOSE: &str = svg!(r#"<path d="M18 6 6 18"/><path d="m6 6 12 12"/>"#);
pub const COPY: &str = svg!(r#"<rect x="8" y="8" width="13" height="13" rx="2"/><path d="M4 16V5a1 1 0 0 1 1-1h11"/>"#);
pub const SAVE: &str = svg!(r#"<path d="M12 3v12"/><path d="m7 10 5 5 5-5"/><path d="M5 21h14"/>"#);
pub const ADD: &str = svg!(r#"<path d="M20 6 9 17l-5-5"/>"#);
pub const CAMERA: &str = svg!(
    r#"<path d="M14.5 4h-5L7 7H4a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2V9a2 2 0 0 0-2-2h-3z"/>"#
    r#"<circle cx="12" cy="13" r="3"/>"#
);
pub const SEND: &str = svg!(r#"<path d="M22 2 11 13"/><path d="M22 2 15 22l-4-9-9-4z"/>"#);

pub fn icon(svg: &str) -> Icon {
    Icon::default().data(svg.as_bytes())
}

pub fn tool(tool: Tool) -> (&'static str, &'static str) {
    match tool {
        Tool::Rect => (RECT, "Rectangle (R)"),
        Tool::Ellipse => (ELLIPSE, "Ellipse (E)"),
        Tool::Line => (LINE, "Line (L)"),
        Tool::Arrow => (ARROW, "Arrow (A)"),
        Tool::Pen => (PEN, "Pen (P)"),
        Tool::Text => (TEXT, "Text (T)"),
        Tool::Marker => (MARKER, "Numbered marker (N)"),
        Tool::Mosaic => (MOSAIC, "Mosaic (M)"),
    }
}
