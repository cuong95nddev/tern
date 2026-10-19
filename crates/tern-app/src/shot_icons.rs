//! The screenshot tool's icons (24×24, Lucide style), drawn as masks in the
//! text colour. Templates name them like bundled icons (`shot-rect`); they
//! are here on every platform, so the built-in templates check everywhere.

use gpui_kit::SharedString;

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
pub const SELECT: &str = svg!(r#"<path d="M4 4l7 17 2.5-7.5L21 11z"/>"#);
pub const CROP: &str = svg!(r#"<path d="M6 2v14a2 2 0 0 0 2 2h14"/><path d="M18 22V8a2 2 0 0 0-2-2H2"/>"#);
pub const PASTE: &str = svg!(
    r#"<rect x="8" y="2" width="8" height="4" rx="1"/>"#
    r#"<path d="M16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2"/>"#
);
pub const EDIT: &str = svg!(r#"<path d="M12 20h9"/><path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4z"/>"#);
pub const PLUS: &str = svg!(r#"<path d="M12 5v14"/><path d="M5 12h14"/>"#);
pub const SEND: &str = svg!(r#"<path d="M22 2 11 13"/><path d="M22 2 15 22l-4-9-9-4z"/>"#);
pub const REGION: &str = svg!(
    r#"<path d="M3 7V5a2 2 0 0 1 2-2h2"/><path d="M17 3h2a2 2 0 0 1 2 2v2"/>"#
    r#"<path d="M21 17v2a2 2 0 0 1-2 2h-2"/><path d="M7 21H5a2 2 0 0 1-2-2v-2"/>"#
);
pub const WINDOW: &str = svg!(r#"<rect x="2" y="4" width="20" height="16" rx="2"/><path d="M2 9h20"/><path d="M6 6.5h.01M9 6.5h.01"/>"#);
pub const SCREEN: &str = svg!(r#"<rect x="2" y="3" width="20" height="14" rx="2"/><path d="M8 21h8"/><path d="M12 17v4"/>"#);
pub const TRASH: &str = svg!(
    r#"<path d="M3 6h18"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"/>"#
    r#"<path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/>"#
);

/// The icons by the names templates give them (`shot-rect`), each served
/// as `icons/<name>.svg`.
const NAMED: &[(&str, &str)] = &[
    ("shot-rect", RECT),
    ("shot-ellipse", ELLIPSE),
    ("shot-line", LINE),
    ("shot-arrow", ARROW),
    ("shot-pen", PEN),
    ("shot-text", TEXT),
    ("shot-marker", MARKER),
    ("shot-mosaic", MOSAIC),
    ("shot-undo", UNDO),
    ("shot-redo", REDO),
    ("shot-close", CLOSE),
    ("shot-copy", COPY),
    ("shot-save", SAVE),
    ("shot-done", ADD),
    ("shot-camera", CAMERA),
    ("shot-select", SELECT),
    ("shot-crop", CROP),
    ("shot-paste", PASTE),
    ("shot-edit", EDIT),
    ("shot-plus", PLUS),
    ("shot-send", SEND),
    ("shot-region", REGION),
    ("shot-window", WINDOW),
    ("shot-screen", SCREEN),
    ("shot-trash", TRASH),
];

/// The SVG at asset `path`, when it is one of these icons.
pub fn asset(path: &str) -> Option<&'static [u8]> {
    let name = path.strip_prefix("icons/")?.strip_suffix(".svg")?;
    NAMED.iter().find(|(n, _)| *n == name).map(|(_, svg)| svg.as_bytes())
}

pub fn asset_paths(prefix: &str) -> impl Iterator<Item = SharedString> + '_ {
    NAMED.iter().map(|(n, _)| format!("icons/{n}.svg")).filter(move |p| p.starts_with(prefix)).map(SharedString::from)
}
