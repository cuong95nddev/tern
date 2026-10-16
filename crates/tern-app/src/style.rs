//! How each part of the window looks. Every element Tern draws (a panel, a
//! row, a bar) has its own properties: sizes, paddings, colors, a
//! background image. Out of the box most of them point at a shared token
//! (`$row_height`, `$bar_background`) or a theme color (`$muted.foreground`),
//! so the window is consistent; `settings.json` can give any one element its
//! own value, or point several at a token of the user's own to keep them
//! alike ([`crate::settings`] reads `tokens` and `elements`).
//!
//! A value is a number (logical pixels), `"0.875rem"` (scales with the UI
//! font size), a color (`#rrggbb`, `#rrggbbaa`, `blue-500/50`,
//! `transparent`), plain text, or `$name`: a token, another element's
//! property (`$explorer.row.height`), or a theme color by its key.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, ThemeColor, try_parse_color};
use gpui_kit::*;
use tern_fs::GitStatus;

/// A horizontal tab bar's height, without its bottom border line. It's
/// gpui-component's `Tab`, so it can't be changed; what sits in the bar
/// matches it.
pub const TAB_BAR_H: Pixels = px(30.);
/// A tab in a horizontal tab bar (the bar plus its bottom border line).
/// gpui-component's `Tab` sets it, so it can't be changed.
pub const TAB_H: Pixels = px(32.);

/// What a property holds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    /// A size: a number of pixels, `"12px"` or `"0.75rem"`.
    Len,
    Color,
    /// A plain number: a font weight, an opacity.
    Num,
    /// Text: a path, a choice like `cover`.
    Text,
}

use Kind::*;

/// A shared value: name, default, what it's for.
pub const TOKENS: &[(&str, &str, &str)] = &[
    ("bar_height", "30", "Every bar under the window title: panel headers, the file header, the vertical tabs' header"),
    ("row_height", "22", "A row in the explorer, changes and scratches"),
    ("indent", "14", "One level of nesting in a list"),
    ("radius", "6", "Corners of buttons, inputs and rows (also the component theme's `radius`)"),
    ("radius_lg", "8", "Corners of dialogs and popovers (the component theme's `radius_lg`)"),
    ("text_xs", "0.75rem", "Small text: panel titles, the status bar, details in rows"),
    ("text_sm", "0.875rem", "Row and tab text"),
    ("text", "$foreground", "Main text color"),
    ("text_muted", "$muted.foreground", "Secondary text color"),
    ("border", "$border", "Lines between parts"),
    ("bar_background", "$title_bar.background", "Window chrome: tool strips, status bar, menu bar"),
    ("tab_bar_background", "$tab_bar.background", "Tab bars and the file header"),
    ("panel_background", "$sidebar.background", "Explorer, changes, scratches"),
    ("row_hover", "$list.hover.background", "A row under the pointer"),
    ("row_selected", "$list.active.background", "A selected row"),
    ("git_added", "$success.background", "Untracked and added files"),
    ("git_modified", "$warning.background", "Modified and renamed files"),
    ("git_deleted", "$danger.background", "Deleted and conflicted files"),
];

/// A property: name, kind, default. An empty default leaves it unset: the
/// element looks as its code draws it until a value is given.
pub type Prop = (&'static str, Kind, &'static str);

/// An element: its name, what it is, its own properties (in groups, so
/// elements alike share them) with their defaults. Every element also has
/// all of [`BOX`].
pub struct Element {
    pub name: &'static str,
    pub about: &'static str,
    pub groups: &'static [&'static [Prop]],
}

impl Element {
    /// Its own properties, then those of [`BOX`] it doesn't redefine.
    pub fn props(&self) -> impl Iterator<Item = &'static Prop> + '_ {
        let own = self.groups.iter().flat_map(|g| g.iter());
        own.clone().chain(BOX.iter().filter(move |b| !self.groups.iter().any(|g| g.iter().any(|p| p.0 == b.0))))
    }

    pub(crate) fn prop(&self, name: &str) -> Option<&'static Prop> {
        self.props().find(|p| p.0 == name)
    }
}

/// What every element can set, unset unless the element gives a default.
/// `border_width` and the paddings, margins and radii by side or corner go
/// over the general one. `visible: 0` hides the element. `text_transform`
/// (`uppercase`, `lowercase`, `capitalize`, `none`) applies to the labels
/// Tern writes (titles, tab names).
pub const BOX: &[Prop] = &[
    ("visible", Num, ""),
    ("width", Len, ""),
    ("height", Len, ""),
    ("min_width", Len, ""),
    ("min_height", Len, ""),
    ("max_width", Len, ""),
    ("max_height", Len, ""),
    ("padding", Len, ""),
    ("padding_x", Len, ""),
    ("padding_y", Len, ""),
    ("padding_top", Len, ""),
    ("padding_right", Len, ""),
    ("padding_bottom", Len, ""),
    ("padding_left", Len, ""),
    ("margin", Len, ""),
    ("margin_x", Len, ""),
    ("margin_y", Len, ""),
    ("margin_top", Len, ""),
    ("margin_right", Len, ""),
    ("margin_bottom", Len, ""),
    ("margin_left", Len, ""),
    ("gap", Len, ""),
    ("radius", Len, ""),
    ("radius_top_left", Len, ""),
    ("radius_top_right", Len, ""),
    ("radius_bottom_right", Len, ""),
    ("radius_bottom_left", Len, ""),
    ("border_width", Len, ""),
    ("border_top_width", Len, ""),
    ("border_right_width", Len, ""),
    ("border_bottom_width", Len, ""),
    ("border_left_width", Len, ""),
    ("border", Color, ""),
    ("background", Color, ""),
    ("text", Color, ""),
    ("font_family", Text, ""),
    ("font_size", Len, ""),
    ("font_weight", Num, ""),
    ("italic", Num, ""),
    ("line_height", Len, ""),
    ("opacity", Num, ""),
    ("shadow", Text, ""),
    ("text_transform", Text, ""),
];

/// A container that can show an image behind its content: the file
/// (absolute, `~/…`, or relative to the config folder), how it fits
/// (`cover`, `contain`, `fill`, `scale_down`, `none`), and its opacity.
const IMAGE: &[Prop] = &[("background_image", Text, ""), ("background_image_fit", Text, "cover"), ("background_image_opacity", Num, "1")];

const PANEL: &[Prop] = &[("background", Color, "$panel_background")];

/// A list row, the same in each panel by default.
const ROW: &[Prop] = &[
    ("height", Len, "$row_height"),
    ("indent", Len, "$indent"),
    ("padding_left", Len, "6"),
    ("padding_right", Len, "8"),
    ("gap", Len, "4"),
    ("radius", Len, "0"),
    ("font_size", Len, "$text_sm"),
    ("detail_font_size", Len, "$text_xs"),
    ("text", Color, "$text"),
    ("detail_text", Color, "$text_muted"),
    ("hover_background", Color, "$row_hover"),
    ("selected_background", Color, "$row_selected"),
    ("selected_text", Color, ""),
    ("icon_size", Len, "16"),
    ("chevron_size", Len, "12"),
    ("chevron_color", Color, "$text_muted"),
];

/// A panel title bar (EXPLORER, CHANGES, …). `title` replaces its text.
const HEADER: &[Prop] = &[
    ("height", Len, "$bar_height"),
    ("padding_left", Len, "12"),
    ("padding_right", Len, "4"),
    ("gap", Len, "2"),
    ("count_gap", Len, "8"),
    ("font_size", Len, "$text_xs"),
    ("font_weight", Num, "600"),
    ("text", Color, "$text_muted"),
    ("count_text", Color, ""),
    ("title", Text, ""),
    ("text_transform", Text, "uppercase"),
];

/// What a panel shows with nothing in it.
const EMPTY: &[Prop] = &[("gap", Len, "4"), ("font_size", Len, "$text_sm"), ("text", Color, "$text_muted")];

/// A tool strip down a window edge.
const STRIP: &[Prop] = &[
    ("width", Len, "34"),
    ("padding_y", Len, "6"),
    ("gap", Len, "4"),
    ("background", Color, "$bar_background"),
    ("border", Color, "$border"),
];

/// A tab in a horizontal bar (gpui-component's `Tab`; unset keeps its look).
const TAB: &[Prop] = &[("text_transform", Text, "none"), ("icon_text", Color, ""), ("close_text", Color, "")];

macro_rules! simple {
    ($name:literal, $about:literal) => {
        Element { name: $name, about: $about, groups: &[] }
    };
}

/// Properties only the GUI front end draws; others ignore them.
pub const GUI_ONLY: &[&str] = &[
    "radius",
    "radius_top_left",
    "radius_top_right",
    "radius_bottom_right",
    "radius_bottom_left",
    "shadow",
    "opacity",
    "background_image",
    "background_image_fit",
    "background_image_opacity",
];

pub const ELEMENTS: &[Element] = &[
    Element { name: "window", about: "The whole window, behind everything", groups: &[&[("background", Color, "$background")], IMAGE] },
    Element {
        name: "menu_bar",
        about: "The menu bar under the title (Windows and Linux)",
        groups: &[&[
            ("height", Len, "$bar_height"),
            ("padding_x", Len, "4"),
            ("background", Color, "$bar_background"),
            ("border", Color, "$border"),
            ("border_bottom_width", Len, "1"),
        ]],
    },
    Element {
        name: "workspace_tabs",
        about: "The bar of workspace tabs at the top",
        groups: &[&[("background", Color, "$tab_bar_background"), ("border", Color, "$border"), ("border_bottom_width", Len, "1")]],
    },
    Element {
        name: "workspace_tabs.tab",
        about: "A workspace tab. `icon_text` and `close_text` color its folder icon and close button",
        groups: &[TAB],
    },
    Element { name: "workspace_tabs.tab_active", about: "The active workspace tab, over `workspace_tabs.tab`", groups: &[TAB] },
    Element { name: "workspace_tabs.button", about: "The buttons in the workspace bar (open, update, close)", groups: &[] },
    Element {
        name: "workspace",
        about: "One workspace: panels, tabs and the status bar",
        groups: &[&[("background", Color, "$background")], IMAGE],
    },
    Element {
        name: "workspace.status_bar",
        about: "The bar at the bottom of a workspace",
        groups: &[
            &[
                ("height", Len, "22"),
                ("padding_x", Len, "12"),
                ("gap", Len, "16"),
                ("font_size", Len, "$text_xs"),
                ("text", Color, "$text_muted"),
                ("error_text", Color, "$danger.background"),
                ("background", Color, "$bar_background"),
            ],
            IMAGE,
        ],
    },
    Element {
        name: "tool_strip.left",
        about: "The panel buttons down the window's left edge",
        groups: &[STRIP, &[("border_right_width", Len, "1")]],
    },
    Element {
        name: "tool_strip.right",
        about: "The panel buttons down the window's right edge",
        groups: &[STRIP, &[("border_left_width", Len, "1")]],
    },
    simple!("tool_strip.button", "A button in a tool strip"),
    Element { name: "explorer", about: "The file explorer panel", groups: &[PANEL, &[("start_width", Len, "250")], IMAGE] },
    Element { name: "explorer.header", about: "The explorer's title bar (its title is the folder's name)", groups: &[HEADER] },
    simple!("explorer.header.button", "A button in the explorer's title bar"),
    simple!("explorer.list", "The explorer's list of rows"),
    Element { name: "explorer.row", about: "A file or folder in the explorer", groups: &[ROW, &[("focus_border", Color, "$ring")]] },
    Element { name: "changes", about: "The git changes panel", groups: &[PANEL, &[("start_width", Len, "300")], IMAGE] },
    Element { name: "changes.header", about: "The changes panel's title bar", groups: &[HEADER] },
    simple!("changes.header.button", "A button in the changes panel's title bar"),
    simple!("changes.list", "The changes panel's list of rows"),
    Element { name: "changes.row", about: "A repository or changed file", groups: &[ROW] },
    Element { name: "changes.empty", about: "What the changes panel shows without repositories", groups: &[EMPTY] },
    Element { name: "scratches", about: "The scratch files panel", groups: &[PANEL, &[("start_height", Len, "220")], IMAGE] },
    Element { name: "scratches.header", about: "The scratches panel's title bar", groups: &[HEADER] },
    simple!("scratches.header.button", "A button in the scratches panel's title bar"),
    simple!("scratches.list", "The scratches panel's list of rows"),
    Element { name: "scratches.row", about: "A scratch file", groups: &[ROW] },
    Element { name: "scratches.empty", about: "What the scratches panel shows without files", groups: &[EMPTY] },
    Element {
        name: "pane",
        about: "A pane of the center: its tabs and what the active one shows. `focused_border` is its border color while \
                it has focus, `focus_line_*` the line along its top while it has focus with more than one pane",
        groups: &[
            &[("focused_border", Color, ""), ("focus_line_height", Len, "2"), ("focus_line_color", Color, "$primary.background")],
            IMAGE,
        ],
    },
    Element {
        name: "pane.body",
        about: "What a pane's active tab shows, under (or beside) its tabs. `focused_border` is its border color while the \
                pane has focus",
        groups: &[&[("focused_border", Color, "")]],
    },
    Element {
        name: "pane.title",
        about: "The active tab's name on the top edge of `pane.body`, like a TUI box title (`visible: 1` to show it)",
        groups: &[&[
            ("visible", Num, "0"),
            ("top", Len, "-9"),
            ("left", Len, "12"),
            ("padding_x", Len, "6"),
            ("font_size", Len, "$text_sm"),
            ("font_weight", Num, "600"),
            ("text", Color, "$text_muted"),
            ("focused_text", Color, "$primary.background"),
            ("background", Color, "$background"),
            ("text_transform", Text, "none"),
        ]],
    },
    Element {
        name: "pane.empty",
        about: "What an empty pane shows",
        groups: &[&[("gap", Len, "12"), ("padding_x", Len, "16"), ("font_size", Len, "$text_sm"), ("text", Color, "$text_muted")]],
    },
    Element {
        name: "pane_tab_bar",
        about: "A pane's horizontal tab bar",
        groups: &[&[("background", Color, "$tab_bar_background"), ("border", Color, "$border"), ("border_bottom_width", Len, "1")]],
    },
    Element {
        name: "pane_tab_bar.tab",
        about: "A tab in a pane's tab bar. `icon_text` colors a terminal or diff icon (file and agent icons keep theirs), \
                `close_text` its close button",
        groups: &[TAB],
    },
    Element { name: "pane_tab_bar.tab_active", about: "The shown tab of a pane, over `pane_tab_bar.tab`", groups: &[TAB] },
    Element {
        name: "pane_tab_bar.group",
        about: "A tab group's chip in a tab bar (its color is the group's)",
        groups: &[&[
            ("height", Len, "18"),
            ("padding_x", Len, "8"),
            ("radius", Len, "5"),
            ("font_size", Len, "$text_xs"),
            ("font_weight", Num, "600"),
            ("text", Color, "$background"),
        ]],
    },
    simple!("pane_tab_bar.button", "The buttons in a tab bar (new tab, split, zoom, layout, close tab)"),
    Element {
        name: "vertical_tabs",
        about: "A pane's tab list down its left",
        groups: &[&[("start_width", Len, "220"), ("background", Color, "$tab_bar_background")], IMAGE],
    },
    Element {
        name: "vertical_tabs.header",
        about: "The bar above the vertical tab list",
        groups: &[&[
            ("height", Len, "$bar_height"),
            ("padding_left", Len, "4"),
            ("border", Color, "$border"),
            ("border_bottom_width", Len, "1"),
        ]],
    },
    Element { name: "vertical_tabs.list", about: "The vertical tab list", groups: &[&[("padding", Len, "4"), ("gap", Len, "2")]] },
    Element {
        name: "vertical_tabs.row",
        about: "A tab in the vertical list",
        groups: &[&[
            ("height", Len, "$bar_height"),
            ("group_indent", Len, "14"),
            ("padding_left", Len, "8"),
            ("padding_right", Len, "4"),
            ("gap", Len, "4"),
            ("radius", Len, "$radius"),
            ("font_size", Len, "$text_sm"),
            ("text", Color, "$text"),
            ("hover_background", Color, "$row_hover"),
            ("selected_background", Color, "$row_selected"),
            ("selected_text", Color, ""),
            ("text_transform", Text, "none"),
        ]],
    },
    Element {
        name: "vertical_tabs.group",
        about: "A tab group's header in the vertical list (its color is the group's)",
        groups: &[&[
            ("height", Len, "$bar_height"),
            ("padding_left", Len, "8"),
            ("padding_right", Len, "6"),
            ("gap", Len, "8"),
            ("radius", Len, "$radius"),
            ("font_size", Len, "$text_sm"),
            ("font_weight", Num, "600"),
            ("text", Color, "$background"),
        ]],
    },
    Element {
        name: "file_header",
        about: "The bar above an open file: its path and the Preview / Diff buttons",
        groups: &[&[
            ("height", Len, "$bar_height"),
            ("padding_x", Len, "8"),
            ("gap", Len, "4"),
            ("font_size", Len, "$text_sm"),
            ("text", Color, "$text"),
            ("background", Color, "$tab_bar_background"),
            ("border", Color, "$border"),
            ("border_bottom_width", Len, "1"),
            ("dirty_text", Color, "$warning.background"),
        ]],
    },
    simple!("file_header.button", "The Preview / Diff buttons above a file"),
    Element {
        name: "terminal",
        about: "A terminal tab. Its colors are `terminal.colors`; a `background_opacity` below 1 lets what's behind \
                it (a background image) show through",
        groups: &[&[("padding_x", Len, "8"), ("padding_top", Len, "4"), ("background_opacity", Num, "1")], IMAGE],
    },
    Element {
        name: "drag_chip",
        about: "What follows the pointer while dragging files, tabs or workspaces",
        groups: &[&[
            ("padding_x", Len, "12"),
            ("padding_y", Len, "4"),
            ("gap", Len, "6"),
            ("radius", Len, "$radius"),
            ("font_size", Len, "$text_sm"),
            ("text", Color, "$text"),
            ("background", Color, "$background"),
            ("border", Color, "$border"),
            ("border_width", Len, "1"),
            ("shadow", Text, "md"),
        ]],
    },
    Element {
        name: "drop_zone",
        about: "Where a dragged panel or tab would go; `size` is the strip an empty dock shows while a panel is dragged",
        groups: &[&[
            ("background", Color, "$primary/15"),
            ("border", Color, "$primary/60"),
            ("border_width", Len, "2"),
            ("radius", Len, "$radius"),
            ("size", Len, "24"),
        ]],
    },
    Element { name: "drop_zone.hover", about: "A drop zone under the pointer", groups: &[&[("background", Color, "$primary/30")]] },
    Element { name: "dialog", about: "Prompts and confirmations", groups: &[&[("width", Len, "420"), ("wide_width", Len, "520")]] },
    Element { name: "welcome", about: "The window with no workspace open", groups: &[&[("width", Len, "520"), ("gap", Len, "24")], IMAGE] },
    Element {
        name: "welcome.title",
        about: "\"Tern\" on the welcome screen",
        groups: &[&[("font_size", Len, "1.875rem"), ("font_weight", Num, "600"), ("text", Color, "$text")]],
    },
    Element { name: "welcome.subtitle", about: "The line under the title", groups: &[&[("text", Color, "$text_muted")]] },
    Element {
        name: "welcome.recent",
        about: "A recent folder on the welcome screen",
        groups: &[&[
            ("padding_x", Len, "8"),
            ("padding_y", Len, "4"),
            ("gap", Len, "8"),
            ("radius", Len, "6"),
            ("text", Color, "$text"),
            ("detail_text", Color, "$text_muted"),
            ("detail_font_size", Len, "$text_xs"),
            ("hover_background", Color, "$row_hover"),
        ]],
    },
    Element {
        name: "welcome.keys",
        about: "The list of shortcuts on the welcome screen",
        groups: &[&[
            ("padding_top", Len, "16"),
            ("gap", Len, "4"),
            ("font_size", Len, "$text_sm"),
            ("text", Color, "$text"),
            ("detail_text", Color, "$text_muted"),
            ("border", Color, "$border"),
            ("border_top_width", Len, "1"),
        ]],
    },
];

pub fn element(name: &str) -> Option<&'static Element> {
    ELEMENTS.iter().find(|e| e.name == name)
}

/// A resolved value.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Len(AbsoluteLength),
    Color(Hsla),
    Num(f32),
    Text(SharedString),
}

/// Every element's properties, resolved for the theme in effect.
#[derive(Default)]
pub struct LooksData {
    props: HashMap<(&'static str, &'static str), Val>,
    /// The built-in tokens' values, for what draws them directly (git colors).
    tokens: HashMap<&'static str, Val>,
    /// What 1rem is: the UI font size.
    rem: Pixels,
}

/// [`LooksData`], cheap to clone, so an [`El`] holds it without borrowing
/// the app.
#[derive(Clone, Default)]
pub struct Looks(std::sync::Arc<LooksData>);

impl std::ops::Deref for Looks {
    type Target = LooksData;

    fn deref(&self) -> &LooksData {
        &self.0
    }
}

impl Global for Looks {}

/// What `settings.json` asks for: tokens, and element properties over the
/// defaults. Values are as written (`22`, `"$row_height"`, `"#ff0000"`).
#[derive(Default)]
pub struct Overrides<'a> {
    pub tokens: BTreeMap<&'a str, String>,
    pub elements: BTreeMap<(&'a str, &'a str), String>,
}

/// `v` as the text a value is read from: a number as written, a string as is.
pub fn raw(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// Resolves every element's properties: `over` on top of the defaults,
/// `$` names through tokens, other elements and `theme`'s colors. A value
/// that can't be read is reported in `problems` and keeps its default.
pub fn resolve(over: &Overrides, theme: &ThemeColor, rem: Pixels, config_dir: &Path, problems: &mut Vec<String>) -> Looks {
    let colors = theme_colors(theme);
    let r = Resolver { over, colors: &colors, config_dir };
    let mut props = HashMap::new();
    for e in ELEMENTS {
        for &(prop, kind, default) in e.props() {
            let given = over.elements.get(&(e.name, prop)).filter(|v| !v.trim().is_empty());
            if given.is_none() && default.is_empty() {
                continue;
            }
            let v = match given {
                Some(raw) => match r.value(raw, kind, 0) {
                    Ok(v) => Some(v),
                    Err(err) => {
                        problems.push(format!("elements.{}.{prop}: {err}", e.name));
                        None
                    },
                },
                None => None,
            };
            let v = v.or_else(|| match r.value(default, kind, 0) {
                Ok(v) => Some(v),
                // A token the user broke; the token's own entry says why.
                Err(_) => r.builtin(default, kind).ok(),
            });
            let v = match v {
                // Images are read from the config folder.
                Some(Val::Text(t)) if prop == "background_image" && !t.is_empty() => {
                    let path = r.path(&t);
                    if !path.is_file() {
                        problems.push(format!("elements.{}.{prop}: no file {}", e.name, path.display()));
                    }
                    Some(Val::Text(path.to_string_lossy().into_owned().into()))
                },
                v => v,
            };
            if let Some(v) = v {
                props.insert((e.name, prop), v);
            }
        }
    }
    let tokens = TOKENS
        .iter()
        .filter_map(|&(name, _, _)| {
            let raw = format!("${name}");
            [Color, Len, Num].into_iter().find_map(|k| r.value(&raw, k, 0).ok()).map(|v| (name, v))
        })
        .collect();
    Looks(std::sync::Arc::new(LooksData { props, tokens, rem }))
}

/// Token `name` as a size, when `over` sets it (it can't name a color).
pub fn token_px(over: &Overrides, name: &str, rem: Pixels) -> Option<Pixels> {
    over.tokens.get(name)?;
    let colors = HashMap::new();
    let r = Resolver { over, colors: &colors, config_dir: Path::new("") };
    match r.value(&format!("${name}"), Len, 0) {
        Ok(Val::Len(l)) => Some(l.to_pixels(rem)),
        _ => None,
    }
}

/// Problems with the names in `over`: elements, properties and tokens that
/// don't exist, and tokens that can't be read.
pub fn check_names(over: &Overrides, theme: &ThemeColor, problems: &mut Vec<String>) {
    for &(name, prop) in over.elements.keys() {
        match element(name) {
            None => problems.push(format!("elements: no element \"{name}\"")),
            Some(e) if e.prop(prop).is_none() => {
                let have: Vec<&str> = e.props().map(|p| p.0).collect();
                problems.push(format!("elements.{name}: no property \"{prop}\" (there are: {})", have.join(", ")));
            },
            Some(_) => {},
        }
    }
    let colors = theme_colors(theme);
    let r = Resolver { over, colors: &colors, config_dir: Path::new("") };
    for (name, raw) in &over.tokens {
        // A token takes the kind its users need; one that reads as none is wrong.
        if [Len, Color, Num, Text].iter().all(|k| r.value(raw, *k, 0).is_err())
            && let Err(e) = r.value(raw, Text, 0)
        {
            problems.push(format!("tokens.{name}: {e}"));
        }
    }
}

/// What resolving a `$element.prop` that has no value says: a template
/// leaves such a property unset rather than report it.
pub const UNSET: &str = "unset";

/// Turns the properties a template node writes (`"style"`, `"states"`) and
/// its sizes into values, as [`resolve`] does for elements.
pub struct Props<'a> {
    over: &'a Overrides<'a>,
    colors: HashMap<String, Hsla>,
    config_dir: &'a Path,
}

impl<'a> Props<'a> {
    pub fn new(over: &'a Overrides<'a>, theme: &ThemeColor, config_dir: &'a Path) -> Self {
        Props { over, colors: theme_colors(theme), config_dir }
    }

    fn resolver(&self) -> Resolver<'_> {
        Resolver { over: self.over, colors: &self.colors, config_dir: self.config_dir }
    }

    /// `raw`'s properties: those of [`BOX`] and of `element` (when there is
    /// one). `at` names where they are written, for `problems`.
    pub fn compile(
        &self,
        element: Option<&'static Element>,
        raw: &BTreeMap<String, serde_json::Value>,
        at: &str,
        problems: &mut Vec<String>,
    ) -> HashMap<&'static str, Val> {
        let r = self.resolver();
        let mut out = HashMap::new();
        for (name, v) in raw {
            let found = element.and_then(|e| e.prop(name)).or_else(|| BOX.iter().find(|p| p.0 == name));
            let Some(&(prop, kind, _)) = found else {
                problems.push(format!("{at}: no property \"{name}\""));
                continue;
            };
            let Some(text) = self::raw(v) else {
                problems.push(format!("{at}.{name}: a value is a number or a string"));
                continue;
            };
            match r.value(&text, kind, 0) {
                Ok(v) => _ = out.insert(prop, v),
                Err(e) if e == UNSET => {},
                Err(e) => problems.push(format!("{at}.{name}: {e}")),
            }
        }
        out
    }

    /// `raw` as a size; `None` when it is unset.
    pub fn len(&self, raw: &serde_json::Value, at: &str, problems: &mut Vec<String>) -> Option<AbsoluteLength> {
        let Some(text) = self::raw(raw) else {
            problems.push(format!("{at}: a size is a number or a string"));
            return None;
        };
        match self.resolver().value(&text, Len, 0) {
            Ok(Val::Len(l)) => Some(l),
            Ok(_) => None,
            Err(e) if e == UNSET => None,
            Err(e) => {
                problems.push(format!("{at}: {e}"));
                None
            },
        }
    }
}

struct Resolver<'a> {
    over: &'a Overrides<'a>,
    colors: &'a HashMap<String, Hsla>,
    config_dir: &'a Path,
}

impl Resolver<'_> {
    fn value(&self, raw: &str, kind: Kind, depth: usize) -> Result<Val, String> {
        self.value_in(raw, kind, depth, None)
    }

    /// `raw`, read as the value of token `within` (if any): there `$within`
    /// is the theme color of that name, so `"border": "$border"` works.
    fn value_in(&self, raw: &str, kind: Kind, depth: usize, within: Option<&str>) -> Result<Val, String> {
        let Some(name) = raw.strip_prefix('$') else { return self.literal(raw, kind) };
        // `$name/NN`: the color at NN% of its opacity.
        if kind == Color
            && let Some((base, pct)) = name.rsplit_once('/')
        {
            let pct: f32 = pct.trim().parse().map_err(|_| format!("\"{pct}\" is not a percentage"))?;
            return match self.value_in(&format!("${base}"), kind, depth + 1, within)? {
                Val::Color(c) => Ok(Val::Color(c.opacity(pct / 100.))),
                v => Ok(v),
            };
        }
        if depth > 16 {
            return Err(format!("\"${name}\" refers to itself"));
        }
        if within != Some(name) {
            if let Some(v) = self.over.tokens.get(name) {
                return self.value_in(v, kind, depth + 1, Some(name));
            }
            if let Some((_, v, _)) = TOKENS.iter().find(|t| t.0 == name) {
                return self.value_in(v, kind, depth + 1, Some(name));
            }
        }
        // A theme color key before an element's property: `$window.border` is the theme's.
        if kind == Color
            && let Some(c) = theme_color(self.colors, name)
        {
            return Ok(Val::Color(c));
        }
        if let Some((el, prop)) = name.rsplit_once('.')
            && let Some(e) = element(el)
            && let Some(&(_, k, default)) = e.prop(prop)
        {
            if k != kind {
                return Err(format!("\"${name}\" is a {k:?}, not a {kind:?}"));
            }
            let raw = self.over.elements.get(&(e.name, prop)).map_or(default, |s| s.as_str());
            if raw.trim().is_empty() {
                return Err(UNSET.to_owned());
            }
            return self.value(raw, kind, depth + 1);
        }
        Err(format!("no token, element property or theme color \"${name}\""))
    }

    /// `raw` with the built-in tokens only, for when the user's broke it.
    fn builtin(&self, raw: &str, kind: Kind) -> Result<Val, String> {
        let empty = Overrides::default();
        Resolver { over: &empty, colors: self.colors, config_dir: self.config_dir }.value(raw, kind, 0)
    }

    fn literal(&self, raw: &str, kind: Kind) -> Result<Val, String> {
        let raw = raw.trim();
        match kind {
            Len => parse_len(raw).map(Val::Len).ok_or_else(|| format!("\"{raw}\" is not a size (22, \"22px\", \"1.5rem\")")),
            Num => raw.parse().map(Val::Num).map_err(|_| format!("\"{raw}\" is not a number")),
            Color => parse_color(raw).map(Val::Color).ok_or_else(|| format!("\"{raw}\" is not a color")),
            Text => Ok(Val::Text(raw.to_owned().into())),
        }
    }

    fn path(&self, p: &str) -> PathBuf {
        if let Some(rest) = p.strip_prefix("~/")
            && let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
        {
            return PathBuf::from(home).join(rest);
        }
        self.config_dir.join(p)
    }
}

fn parse_len(raw: &str) -> Option<AbsoluteLength> {
    let n = |s: &str| s.trim().parse::<f32>().ok().filter(|v| v.is_finite());
    if let Some(r) = raw.strip_suffix("rem") {
        return n(r).map(|v| rems(v).into());
    }
    n(raw.strip_suffix("px").unwrap_or(raw)).map(|v| px(v).into())
}

fn parse_color(raw: &str) -> Option<Hsla> {
    match raw {
        "transparent" | "none" => Some(hsla(0., 0., 0., 0.)),
        _ => try_parse_color(raw).ok(),
    }
}

/// The theme's colors by field name (`muted_foreground`, `tab_bar`).
fn theme_colors(theme: &ThemeColor) -> HashMap<String, Hsla> {
    let Ok(serde_json::Value::Object(m)) = serde_json::to_value(theme) else { return HashMap::new() };
    m.into_iter().filter_map(|(k, v)| Some((k, serde_json::from_value(v).ok()?))).collect()
}

/// A theme color by its key in theme files (`muted.foreground`,
/// `tab_bar.background`) or its field name (`muted_foreground`).
fn theme_color(colors: &HashMap<String, Hsla>, key: &str) -> Option<Hsla> {
    // The keys whose field isn't the key less `.background`, dots as `_`.
    let field = match key {
        "input.border" => "input".to_owned(),
        "slider.background" => "slider_bar".to_owned(),
        "group_box.title.foreground" => "group_box_foreground".to_owned(),
        _ => {
            let k = key.strip_prefix("base.").unwrap_or(key);
            k.strip_suffix(".background").unwrap_or(k).replace('.', "_")
        },
    };
    colors.get(&field).or_else(|| colors.get(key)).copied()
}

/// The look of element `name`. It reads the properties the element lists in
/// [`ELEMENTS`]; asking for another is a bug, and gives a zero.
pub fn el(name: &'static str, cx: &App) -> El {
    debug_assert!(element(name).is_some(), "no element {name}");
    El { name, looks: looks(cx), inline: None }
}

/// The looks in effect.
fn looks(cx: &App) -> Looks {
    static EMPTY: std::sync::LazyLock<Looks> = std::sync::LazyLock::new(|| {
        let theme = ThemeColor::default();
        resolve(&Overrides::default(), &theme, px(16.), Path::new(""), &mut vec![])
    });
    cx.try_global::<Looks>().unwrap_or(&EMPTY).clone()
}

/// A template node's look: element `name` (or none, `""`) with `inline`
/// properties over its own.
pub fn el_with(name: &'static str, inline: std::sync::Arc<HashMap<&'static str, Val>>, cx: &App) -> El {
    El { name, looks: looks(cx), inline: Some(inline) }
}

#[derive(Clone)]
pub struct El {
    name: &'static str,
    looks: Looks,
    /// A template node's own properties, over the element's.
    inline: Option<std::sync::Arc<HashMap<&'static str, Val>>>,
}

impl El {
    fn get(&self, prop: &'static str) -> Option<&Val> {
        if let Some(v) = self.inline.as_ref().and_then(|m| m.get(prop)) {
            return Some(v);
        }
        if self.name.is_empty() {
            return None;
        }
        debug_assert!(element(self.name).is_some_and(|e| e.prop(prop).is_some()), "no property {}.{prop}", self.name);
        self.looks.props.get(&(self.name, prop))
    }

    /// Whether `prop` has a value (a default or the user's).
    pub fn is_set(&self, prop: &'static str) -> bool {
        self.get(prop).is_some()
    }

    fn len_opt(&self, prop: &'static str) -> Option<AbsoluteLength> {
        match self.get(prop) {
            Some(Val::Len(l)) => Some(*l),
            _ => None,
        }
    }

    pub fn len(&self, prop: &'static str) -> AbsoluteLength {
        self.len_opt(prop).unwrap_or_else(|| px(0.).into())
    }

    /// A size in pixels (rems at the UI font size).
    pub fn px(&self, prop: &'static str) -> Pixels {
        self.len(prop).to_pixels(self.looks.rem)
    }

    fn color_opt(&self, prop: &'static str) -> Option<Hsla> {
        match self.get(prop) {
            Some(Val::Color(c)) => Some(*c),
            _ => None,
        }
    }

    pub fn color(&self, prop: &'static str) -> Hsla {
        self.color_opt(prop).unwrap_or_else(|| hsla(0., 0., 0., 0.))
    }

    fn num_opt(&self, prop: &'static str) -> Option<f32> {
        match self.get(prop) {
            Some(Val::Num(n)) => Some(*n),
            _ => None,
        }
    }

    pub fn num(&self, prop: &'static str) -> f32 {
        self.num_opt(prop).unwrap_or(0.)
    }

    pub fn text(&self, prop: &'static str) -> SharedString {
        match self.get(prop) {
            Some(Val::Text(t)) => t.clone(),
            _ => SharedString::default(),
        }
    }

    /// Whether `visible` is unset or not 0.
    pub fn visible(&self) -> bool {
        self.num_opt("visible").is_none_or(|v| v != 0.)
    }

    /// `text` as this element writes labels (`text_transform`).
    pub fn label(&self, text: impl Into<SharedString>) -> SharedString {
        let text = text.into();
        match self.text("text_transform").as_ref() {
            "uppercase" => text.to_uppercase().into(),
            "lowercase" => text.to_lowercase().into(),
            "capitalize" => {
                let mut c = text.chars();
                c.next().map_or_else(SharedString::default, |f| (f.to_uppercase().collect::<String>() + c.as_str()).into())
            },
            _ => text,
        }
    }

    /// Every box property of the element that is set (see [`BOX`]); call it
    /// after the code's own styling, so these win.
    pub fn apply<S: Styled>(&self, mut s: S) -> S {
        if !self.visible() {
            s.style().display = Some(Display::None);
        }
        let l = |p| self.len_opt(p);
        let size = |v: Option<AbsoluteLength>| v.map(|v| Length::Definite(v.into()));
        let st = s.style();
        if let Some(v) = size(l("width")) {
            st.size.width = Some(v);
        }
        if let Some(v) = size(l("height")) {
            st.size.height = Some(v);
        }
        if let Some(v) = size(l("min_width")) {
            st.min_size.width = Some(v);
        }
        if let Some(v) = size(l("min_height")) {
            st.min_size.height = Some(v);
        }
        if let Some(v) = size(l("max_width")) {
            st.max_size.width = Some(v);
        }
        if let Some(v) = size(l("max_height")) {
            st.max_size.height = Some(v);
        }
        // General, then by axis, then by side: the most specific wins.
        let pick =
            |side: &'static str, axis: &'static str, all: &'static str| self.len_opt(side).or(self.len_opt(axis)).or(self.len_opt(all));
        let pad = [
            pick("padding_top", "padding_y", "padding"),
            pick("padding_right", "padding_x", "padding"),
            pick("padding_bottom", "padding_y", "padding"),
            pick("padding_left", "padding_x", "padding"),
        ];
        let mar = [
            pick("margin_top", "margin_y", "margin"),
            pick("margin_right", "margin_x", "margin"),
            pick("margin_bottom", "margin_y", "margin"),
            pick("margin_left", "margin_x", "margin"),
        ];
        let pick1 = |side: &'static str, all: &'static str| self.len_opt(side).or(self.len_opt(all));
        let border = [
            pick1("border_top_width", "border_width"),
            pick1("border_right_width", "border_width"),
            pick1("border_bottom_width", "border_width"),
            pick1("border_left_width", "border_width"),
        ];
        let radius = [
            pick1("radius_top_left", "radius"),
            pick1("radius_top_right", "radius"),
            pick1("radius_bottom_right", "radius"),
            pick1("radius_bottom_left", "radius"),
        ];
        let st = s.style();
        let [t, r, b, lf] = pad;
        set(&mut st.padding.top, t.map(Into::into));
        set(&mut st.padding.right, r.map(Into::into));
        set(&mut st.padding.bottom, b.map(Into::into));
        set(&mut st.padding.left, lf.map(Into::into));
        let [t, r, b, lf] = mar;
        let m = |v: Option<AbsoluteLength>| v.map(|v| Length::Definite(v.into()));
        set(&mut st.margin.top, m(t));
        set(&mut st.margin.right, m(r));
        set(&mut st.margin.bottom, m(b));
        set(&mut st.margin.left, m(lf));
        let [t, r, b, lf] = border;
        set(&mut st.border_widths.top, t);
        set(&mut st.border_widths.right, r);
        set(&mut st.border_widths.bottom, b);
        set(&mut st.border_widths.left, lf);
        let [tl, tr, br, bl] = radius;
        set(&mut st.corner_radii.top_left, tl);
        set(&mut st.corner_radii.top_right, tr);
        set(&mut st.corner_radii.bottom_right, br);
        set(&mut st.corner_radii.bottom_left, bl);
        if let Some(g) = self.len_opt("gap") {
            st.gap.width = Some(g.into());
            st.gap.height = Some(g.into());
        }
        if let Some(c) = self.color_opt("border") {
            st.border_color = Some(c);
        }
        if let Some(c) = self.color_opt("background") {
            st.background = Some(c.into());
        }
        if let Some(o) = self.num_opt("opacity") {
            st.opacity = Some(o.clamp(0., 1.));
        }
        if let Some(c) = self.color_opt("text") {
            s = s.text_color(c);
        }
        if let Some(v) = self.len_opt("font_size") {
            s = s.text_size(v);
        }
        if let Some(w) = self.num_opt("font_weight") {
            s = s.font_weight(FontWeight(w));
        }
        if self.num_opt("italic").is_some_and(|v| v != 0.) {
            s = s.italic();
        }
        if let Some(v) = self.len_opt("line_height") {
            s = s.line_height(v);
        }
        let family = self.text("font_family");
        if !family.is_empty() {
            s = s.font_family(family);
        }
        match self.text("shadow").as_ref() {
            "none" => s = s.shadow_none(),
            "sm" => s = s.shadow_sm(),
            "md" => s = s.shadow_md(),
            "lg" => s = s.shadow_lg(),
            "xl" => s = s.shadow_xl(),
            _ => {},
        }
        s
    }

    /// The background image layer, to be the container's first child (the
    /// container must be `relative`), when the element has one set.
    pub fn backdrop(&self) -> Option<AnyElement> {
        if element(self.name).is_none_or(|e| e.prop("background_image").is_none()) {
            return None;
        }
        let path = self.text("background_image");
        if path.is_empty() {
            return None;
        }
        let fit = match self.text("background_image_fit").as_ref() {
            "contain" => ObjectFit::Contain,
            "fill" => ObjectFit::Fill,
            "scale_down" => ObjectFit::ScaleDown,
            "none" => ObjectFit::None,
            _ => ObjectFit::Cover,
        };
        Some(
            img(PathBuf::from(path.as_ref()))
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .object_fit(fit)
                .opacity(self.num("background_image_opacity").clamp(0., 1.))
                .into_any_element(),
        )
    }

    /// `apply`, with the background image first when there is one (`s`
    /// must not have children yet).
    pub fn container<S: Styled + ParentElement>(&self, s: S) -> S {
        self.apply(s).relative().children(self.backdrop())
    }
}

fn set<T>(slot: &mut Option<T>, v: Option<T>) {
    if v.is_some() {
        *slot = v;
    }
}

/// An icon button in a bar, drawn as element `name`.
pub fn header_button(name: &'static str, id: impl Into<ElementId>, icon: impl Into<Icon>, cx: &App) -> Button {
    el(name, cx).apply(Button::new(id).xsmall().ghost().icon(icon))
}

/// A list row, drawn as element `name` (`explorer.row`, …): children in a
/// line, indented for `depth`. Rows with children put a [`chevron`] first
/// and the rest [`chevron_space`], so icons line up at every level and
/// across panels.
pub fn list_row(name: &'static str, id: impl Into<ElementId>, depth: usize, cx: &App) -> Stateful<Div> {
    let e = el(name, cx);
    let indent = e.px("padding_left") + e.px("indent") * depth as f32;
    e.apply(div().id(id).w_full().flex().items_center()).pl(indent)
}

/// A row's chevron, open or closed.
pub fn chevron(name: &'static str, open: bool, cx: &App) -> Icon {
    let e = el(name, cx);
    let icon = if open { gpui_kit::component::IconName::ChevronDown } else { gpui_kit::component::IconName::ChevronRight };
    Icon::new(icon).size(e.px("chevron_size")).text_color(e.color("chevron_color"))
}

/// The blank where a row without children would have its chevron.
pub fn chevron_space(name: &'static str, cx: &App) -> Div {
    div().size(el(name, cx).px("chevron_size")).flex_shrink_0()
}

/// A row's file icon.
pub fn row_icon(name: &'static str, icon: impl Into<Icon>, color: Hsla, cx: &App) -> Icon {
    icon.into().size(el(name, cx).px("icon_size")).text_color(color)
}

/// A dialog's width: a prompt or confirmation, or (`wide`) one with more to say.
pub fn dialog_width(wide: bool, cx: &App) -> Pixels {
    el("dialog", cx).px(if wide { "wide_width" } else { "width" })
}

/// What follows the pointer while dragging (files, tabs, workspaces).
pub fn drag_chip(cx: &App) -> Div {
    el("drag_chip", cx).apply(div().flex().items_center())
}

/// The tint of a file name with this git status, the same in every panel.
pub fn git_color(status: GitStatus, cx: &App) -> Hsla {
    let token = match status {
        GitStatus::Untracked | GitStatus::Added => "git_added",
        GitStatus::Modified | GitStatus::Renamed => "git_modified",
        GitStatus::Deleted | GitStatus::Conflicted => "git_deleted",
    };
    match el("window", cx).looks.tokens.get(token) {
        Some(Val::Color(c)) => *c,
        _ => cx.theme().foreground,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::Path;

    use gpui_kit::component::{ThemeColor, ThemeConfigColors};
    use gpui_kit::{px, rems};

    // Not `super::*`: GPUI's glob brings its own `test` macro.
    use super::{ELEMENTS, Looks, Overrides, Val, check_names, resolve, theme_color, theme_colors};

    fn looks(over: &Overrides) -> (Looks, Vec<String>) {
        let mut problems = vec![];
        let l = resolve(over, &ThemeColor::default(), px(16.), Path::new("/cfg"), &mut problems);
        check_names(over, &ThemeColor::default(), &mut problems);
        (l, problems)
    }

    fn get<'a>(l: &'a Looks, el: &'static str, prop: &'static str) -> &'a Val {
        l.props.get(&(el, prop)).unwrap_or_else(|| panic!("{el}.{prop} missing"))
    }

    #[test]
    fn every_default_resolves() {
        let (l, problems) = looks(&Overrides::default());
        assert!(problems.is_empty(), "{problems:?}");
        for e in ELEMENTS {
            let mut seen = HashSet::new();
            for (prop, _, default) in e.props() {
                assert!(seen.insert(*prop), "{}.{prop} twice", e.name);
                if default.is_empty() {
                    assert!(!l.props.contains_key(&(e.name, *prop)), "{}.{prop} set", e.name);
                } else {
                    get(&l, e.name, prop);
                }
            }
        }
    }

    #[test]
    fn every_theme_color_key_has_a_color() {
        let colors = theme_colors(&ThemeColor::default());
        let Ok(serde_json::Value::Object(keys)) = serde_json::to_value(ThemeConfigColors::default()) else { panic!() };
        let missing: Vec<&String> = keys.keys().filter(|k| theme_color(&colors, k).is_none()).collect();
        assert!(missing.is_empty(), "{missing:?}");
    }

    #[test]
    fn tokens_reach_every_element_that_points_at_them() {
        let mut over = Overrides::default();
        over.tokens.insert("row_height", "26".into());
        over.elements.insert(("changes.row", "height"), "30".into());
        let (l, problems) = looks(&over);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(get(&l, "explorer.row", "height"), &Val::Len(px(26.).into()));
        assert_eq!(get(&l, "scratches.row", "height"), &Val::Len(px(26.).into()));
        assert_eq!(get(&l, "changes.row", "height"), &Val::Len(px(30.).into()));
    }

    #[test]
    fn own_tokens_and_element_references() {
        let mut over = Overrides::default();
        over.tokens.insert("list_text", "#ff0000".into());
        over.elements.insert(("explorer.row", "text"), "$list_text".into());
        over.elements.insert(("explorer.row", "font_size"), "1rem".into());
        over.elements.insert(("scratches.row", "font_size"), "$explorer.row.font_size".into());
        over.elements.insert(("explorer", "background_image"), "img/sky.png".into());
        let (l, problems) = looks(&over);
        let red = gpui_kit::component::try_parse_color("#ff0000").unwrap();
        assert_eq!(get(&l, "explorer.row", "text"), &Val::Color(red));
        assert_eq!(get(&l, "scratches.row", "font_size"), &Val::Len(rems(1.).into()));
        assert_eq!(get(&l, "explorer", "background_image"), &Val::Text("/cfg/img/sky.png".into()));
        // The image doesn't exist.
        assert_eq!(problems.len(), 1, "{problems:?}");
    }

    #[test]
    fn every_element_takes_box_properties() {
        let mut over = Overrides::default();
        over.elements.insert(("changes.empty", "margin_top"), "10".into());
        over.elements.insert(("pane", "border_width"), "1".into());
        let (l, problems) = looks(&over);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(get(&l, "changes.empty", "margin_top"), &Val::Len(px(10.).into()));
        assert_eq!(get(&l, "pane", "border_width"), &Val::Len(px(1.).into()));
        // An element's own property shadows the box one of that name.
        let row = super::element("explorer.row").unwrap();
        assert_eq!(row.props().filter(|p| p.0 == "height").count(), 1);
    }

    #[test]
    fn mistakes_are_reported_and_keep_the_default() {
        let mut over = Overrides::default();
        over.tokens.insert("a", "$b".into());
        over.tokens.insert("b", "$a".into());
        over.elements.insert(("explorer.row", "height"), "$a".into());
        over.elements.insert(("explorer.row", "text"), "$row_height".into());
        over.elements.insert(("explorer.row", "colour"), "#fff".into());
        over.elements.insert(("explorer.rows", "height"), "1".into());
        let (l, problems) = looks(&over);
        assert_eq!(get(&l, "explorer.row", "height"), &Val::Len(px(22.).into()));
        // height, text, colour, explorer.rows, and tokens a and b.
        assert_eq!(problems.len(), 6, "{problems:?}");
    }
}
