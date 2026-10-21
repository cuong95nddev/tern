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
//! property (`$explorer.row.height`), or a theme color by its key. A color
//! `$name` takes `/NN` for NN% of its opacity (`$primary/15`).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use gpui_kit::component::{ActiveTheme, Icon, ThemeColor, try_parse_color};
use gpui_kit::*;
use tern_fs::GitStatus;

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
    ("tab_bar_background", "$background", "Tab bars and the file header"),
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
/// over the general one. `visible: 0` hides the element. `grow` is its
/// share of its parent's room, grown from nothing (a box's own `grow`, but
/// one the code can give: `"{used}"`). `text_transform`
/// (`uppercase`, `lowercase`, `capitalize`, `none`) applies to the labels
/// Tern writes (titles, tab names).
pub const BOX: &[Prop] = &[
    ("visible", Num, ""),
    ("grow", Num, ""),
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

/// A template box's own properties: those of [`BOX`], of [`IMAGE`] (any
/// box can show an image behind its children), and of [`RING`] (for a
/// `ring`).
pub fn box_prop(name: &str) -> Option<&'static Prop> {
    BOX.iter().chain(IMAGE).chain(RING).find(|p| p.0 == name)
}

/// A template `ring`: its track's color, the width of its track and arc, and
/// how many seconds it takes to turn once (0 keeps it still). Its arc takes
/// its `text` color.
pub const RING: &[Prop] = &[("track", Color, ""), ("stroke_width", Len, ""), ("spin", Num, "")];

/// A container that can show an image behind its content: the file
/// (absolute, `~/…`, or relative to the config folder), how it fits
/// (`cover`, `contain`, `fill`, `scale_down`, `none`), and its opacity.
const IMAGE: &[Prop] = &[("background_image", Text, ""), ("background_image_fit", Text, "cover"), ("background_image_opacity", Num, "1")];

const PANEL: &[Prop] = &[("background", Color, "$panel_background")];

/// A panel's list of rows: rows sit inset from the panel's edges.
const LIST: &[Prop] = &[("padding_x", Len, "8")];

/// A list row, the same in each panel by default.
const ROW: &[Prop] = &[
    ("height", Len, "$row_height"),
    ("indent", Len, "$indent"),
    ("padding_left", Len, "6"),
    ("padding_right", Len, "8"),
    ("gap", Len, "4"),
    ("radius", Len, "$radius"),
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

/// A tab in a horizontal tab bar drawn by a template (`pane_tab_bar.tab`,
/// `shot_editor.tab`).
const BAR_TAB: &[Prop] = &[
    ("height", Len, "26"),
    ("margin_y", Len, "3"),
    ("radius", Len, "$radius"),
    ("gap", Len, "4"),
    ("font_size", Len, "$text_sm"),
    ("text", Color, "$tab.foreground"),
    ("hover_text", Color, "$tab.active.foreground"),
    ("border", Color, "transparent"),
    ("border_left_width", Len, "1"),
    ("border_right_width", Len, "1"),
];

/// The shown tab, over [`BAR_TAB`]: a gray rounded chip.
const BAR_TAB_ACTIVE: &[Prop] = &[("background", Color, "$muted"), ("text", Color, "$tab.active.foreground")];

/// The screenshot overlay's floating bars.
const SHOT_BAR: &[Prop] = &[
    ("padding_x", Len, "0.5rem"),
    ("radius", Len, "0.5rem"),
    ("background", Color, "$popover"),
    ("border", Color, "$border"),
    ("border_width", Len, "1"),
    ("shadow", Text, "lg"),
];

macro_rules! simple {
    ($name:literal, $about:literal) => {
        Element { name: $name, about: $about, groups: &[] }
    };
}

/// Elements drawn over other content, not in the window's flow: in a
/// see-through window they keep a background the color of the window's.
const OVER_CONTENT: &[&str] = &["floating", "drag_chip", "settings.switch.knob", "terminal.find"];

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
    "background_opacity",
    "background_blur",
];

pub const ELEMENTS: &[Element] = &[
    Element {
        name: "window",
        about: "The whole window, behind everything. A `background_opacity` below 1 lets the desktop show through every \
                part's background (the window's, the panels', the tab bars', the terminals'; not menus or dialogs), \
                blurred by `background_blur` points (0: not blurred; on Windows and Linux, the system's blur)",
        groups: &[&[("background", Color, "$background"), ("background_opacity", Num, "1"), ("background_blur", Num, "20")], IMAGE],
    },
    Element {
        name: "agents",
        about: "The agents panel: each workspace of the window with the terminals running an agent",
        groups: &[PANEL, &[("start_width", Len, "240")], IMAGE],
    },
    Element { name: "agents.header", about: "The agents panel's title bar", groups: &[HEADER] },
    simple!("agents.header.button", "A button in the agents panel's title bar"),
    Element {
        name: "agents.list",
        about: "The agents panel's list of workspaces and agents",
        groups: &[&[("padding_x", Len, "8"), ("padding_y", Len, "6"), ("gap", Len, "2")]],
    },
    Element {
        name: "agents.folder",
        about: "A workspace in the agents panel and the agent monitor: its folder, the state of its agents that most wants you (`dot_size`), its new tab and close buttons",
        groups: &[&[
            ("height", Len, "$bar_height"),
            ("padding_left", Len, "8"),
            ("padding_right", Len, "4"),
            ("gap", Len, "6"),
            ("radius", Len, "$radius"),
            ("font_size", Len, "$text_sm"),
            ("font_weight", Num, "600"),
            ("text", Color, "$text"),
            ("icon_text", Color, "$text_muted"),
            ("dot_size", Len, "0.375rem"),
            ("hover_background", Color, "$row_hover"),
            ("active_background", Color, ""),
            ("active_text", Color, ""),
        ]],
    },
    Element {
        name: "agents.folder.needs",
        about: "How many of a workspace's agents wait for you (\"1 needs you\"), in place of its dot",
        groups: &[&[("font_weight", Num, "400"), ("text", Color, "$warning.background")]],
    },
    Element {
        name: "agents.tab",
        about: "A terminal running an agent, under its workspace. `hidden_text` colors one its pane doesn't show",
        groups: &[&[
            ("height", Len, "$bar_height"),
            ("padding_left", Len, "8"),
            ("padding_right", Len, "4"),
            ("gap", Len, "6"),
            ("radius", Len, "$radius"),
            ("font_size", Len, "$text_sm"),
            ("text", Color, "$text"),
            ("hidden_text", Color, "$text_muted"),
            ("working_opacity", Num, "0.7"),
            ("done_weight", Num, "600"),
            ("hover_background", Color, "$row_hover"),
            ("selected_background", Color, "$row_selected"),
            ("selected_text", Color, ""),
        ]],
    },
    Element {
        name: "agents.status",
        about: "An agent's state: how long it has been working or since its turn ended, and its icon",
        groups: &[&[("gap", Len, "6"), ("padding_x", Len, "4"), ("font_size", Len, "$text_xs"), ("text", Color, "$text_muted")]],
    },
    Element {
        name: "agents.state",
        about: "An agent's state icon, as Harp's sidebar draws it (each color for its state): a filled tick once its turn is done (`done`), a quiet one once seen (`seen`), an amber ring and dot while it waits for you (`needs`), an amber dot after a bell (`bell`), an empty ring before its first turn (`pending`); `glyph` is the icons' size in it",
        groups: &[&[
            ("size", Len, "16"),
            ("glyph", Len, "15"),
            ("done", Color, "$foreground"),
            ("seen", Color, "$muted.foreground"),
            ("needs", Color, "$warning.background"),
            ("bell", Color, "$warning.background"),
            ("pending", Color, "$muted.foreground/25"),
        ]],
    },
    Element {
        name: "agents.ring",
        about: "An agent's working ring: its arc (`text`) turning on its track",
        groups: &[&[
            ("width", Len, "13.3"),
            ("height", Len, "13.3"),
            ("stroke_width", Len, "1.7"),
            ("text", Color, "$foreground"),
            ("track", Color, "$muted.foreground/25"),
            ("spin", Num, "1"),
        ]],
    },
    Element {
        name: "agents.shell_dot",
        about: "The dot on an agent's state icon once its turn is done while it left shells running in the background",
        groups: &[&[("size", Len, "5"), ("background", Color, "$muted.foreground")]],
    },
    simple!("agents.button", "The buttons on a workspace or agent row (new tab, close)"),
    Element { name: "workspace", about: "One workspace: its panels and tabs", groups: &[&[("background", Color, "$background")], IMAGE] },
    Element {
        name: "title_bar",
        about: "The window's title bar: its title, what is wrong with the background terminals or the settings, the buttons of the panels docked on the right, the window controls (Windows)",
        groups: &[&[
            ("height", Len, "34"),
            ("background", Color, "$background"),
            ("border", Color, "$border"),
            ("border_bottom_width", Len, "1"),
            ("font_size", Len, "$text_sm"),
            ("text", Color, "$text_muted"),
            ("error_text", Color, "$danger.background"),
            ("status_gap", Len, "16"),
            ("buttons_gap", Len, "2"),
            ("buttons_padding", Len, "6"),
        ]],
    },
    simple!("title_bar.button", "A button in the title bar: a panel's, or Settings"),
    simple!("title_bar.update", "The button in the title bar that offers a new version of Tern, left of the others"),
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
    Element { name: "explorer.list", about: "The explorer's list of rows", groups: &[LIST] },
    Element { name: "explorer.row", about: "A file or folder in the explorer", groups: &[ROW, &[("focus_border", Color, "$ring")]] },
    Element {
        name: "changes",
        about: "The git changes panel",
        groups: &[PANEL, &[("start_width", Len, "300"), ("start_height", Len, "260")], IMAGE],
    },
    Element { name: "changes.header", about: "The changes panel's title bar", groups: &[HEADER] },
    simple!("changes.header.button", "A button in the changes panel's title bar"),
    Element { name: "changes.list", about: "The changes panel's list of rows", groups: &[LIST] },
    Element { name: "changes.row", about: "A repository or changed file", groups: &[ROW] },
    Element { name: "changes.empty", about: "What the changes panel shows without repositories", groups: &[EMPTY] },
    simple!("changes.check", "A file's or repository's checkbox in the changes panel: the checked files are committed"),
    Element {
        name: "changes.commit",
        about: "The changes panel's commit box, under its list: the message, Amend, the Commit button",
        groups: &[&[
            ("padding", Len, "8"),
            ("gap", Len, "6"),
            ("border", Color, "$border"),
            ("border_top_width", Len, "1"),
            ("font_size", Len, "$text_sm"),
        ]],
    },
    simple!("changes.commit.button", "The commit box's Amend switch"),
    Element {
        name: "changes.commit.count",
        about: "How many files the commit box will commit",
        groups: &[&[("font_size", Len, "$text_xs"), ("text", Color, "$text_muted")]],
    },
    Element {
        name: "changes.commit.error",
        about: "Why the last commit failed (git's or a hook's output)",
        groups: &[&[("font_size", Len, "$text_xs"), ("text", Color, "$danger")]],
    },
    Element {
        name: "changes.commit.done",
        about: "The commit the commit box last made",
        groups: &[&[("font_size", Len, "$text_xs"), ("text", Color, "$text_muted")]],
    },
    Element {
        name: "git_log",
        about: "The Git Log tab: the history of the workspace's repositories, with its graph",
        groups: &[&[("background", Color, "$background"), ("font_size", Len, "$text_sm")]],
    },
    Element {
        name: "git_log.toolbar",
        about: "The Git Log's bar: search, the repository, branch and user filters, refresh",
        groups: &[&[
            ("height", Len, "$bar_height"),
            ("padding_x", Len, "6"),
            ("gap", Len, "4"),
            ("background", Color, "$tab_bar_background"),
            ("border", Color, "$border"),
            ("border_bottom_width", Len, "1"),
        ]],
    },
    Element { name: "git_log.search", about: "The Git Log's search field (subject, hash, author)", groups: &[&[("width", Len, "260")]] },
    simple!("git_log.button", "A button in the Git Log's bar"),
    Element {
        name: "git_log.count",
        about: "How many commits the Git Log shows",
        groups: &[&[("padding_x", Len, "6"), ("font_size", Len, "$text_xs"), ("text", Color, "$text_muted")]],
    },
    Element { name: "git_log.message", about: "What the Git Log shows without commits (loading, an error)", groups: &[EMPTY] },
    simple!("git_log.list", "The Git Log's list of commits"),
    Element {
        name: "git_log.row",
        about: "A commit in the Git Log: its graph, refs, subject, author and date",
        groups: &[&[
            ("height", Len, "$row_height"),
            ("padding_right", Len, "8"),
            ("gap", Len, "8"),
            ("text", Color, "$text"),
            ("hover_background", Color, "$row_hover"),
            ("selected_background", Color, "$row_selected"),
            ("selected_text", Color, ""),
        ]],
    },
    Element {
        name: "git_log.row.author",
        about: "A commit's author in the Git Log",
        groups: &[&[("width", Len, "160"), ("text", Color, "$text_muted")]],
    },
    Element {
        name: "git_log.row.date",
        about: "A commit's date in the Git Log",
        groups: &[&[("width", Len, "120"), ("text", Color, "$text_muted")]],
    },
    Element {
        name: "git_log.graph",
        about: "The Git Log's graph: a lane per line of history, its dots and lines; `color_1` … `color_6` color the \
                branches in turn",
        groups: &[&[
            ("lane_width", Len, "14"),
            ("line_width", Len, "1.5"),
            ("dot_size", Len, "8"),
            ("padding_left", Len, "4"),
            ("color_1", Color, "$base.blue"),
            ("color_2", Color, "$base.green"),
            ("color_3", Color, "$base.magenta"),
            ("color_4", Color, "$base.yellow"),
            ("color_5", Color, "$base.cyan"),
            ("color_6", Color, "$base.red"),
        ]],
    },
    Element {
        name: "git_log.ref",
        about: "A branch on a commit in the Git Log; `git_log.ref_head` is the one checked out, `git_log.ref_remote` a \
                remote one, `git_log.ref_tag` a tag",
        groups: &[&[
            ("padding_x", Len, "5"),
            ("radius", Len, "4"),
            ("font_size", Len, "$text_xs"),
            ("background", Color, "$base.green/20"),
            ("text", Color, "$base.green"),
        ]],
    },
    Element {
        name: "git_log.ref_head",
        about: "The branch checked out, on its commit in the Git Log",
        groups: &[&[("background", Color, "$base.yellow/25"), ("text", Color, "$base.yellow"), ("font_weight", Num, "600")]],
    },
    Element {
        name: "git_log.ref_remote",
        about: "A remote branch on a commit in the Git Log",
        groups: &[&[("background", Color, "$base.magenta/20"), ("text", Color, "$base.magenta")]],
    },
    Element {
        name: "git_log.ref_tag",
        about: "A tag on a commit in the Git Log",
        groups: &[&[("background", Color, "$base.cyan/20"), ("text", Color, "$base.cyan")]],
    },
    Element {
        name: "git_log.details",
        about: "The Git Log's side: the selected commit's message, author, refs and files",
        groups: &[&[
            ("start_width", Len, "380"),
            ("background", Color, "$panel_background"),
            ("border", Color, "$border"),
            ("border_left_width", Len, "1"),
        ]],
    },
    Element {
        name: "git_log.details.info",
        about: "The selected commit's message and who made it",
        groups: &[&[("padding", Len, "10"), ("gap", Len, "6"), ("border", Color, "$border"), ("border_bottom_width", Len, "1")]],
    },
    Element {
        name: "git_log.details.subject",
        about: "The selected commit's first line",
        groups: &[&[("font_size", Len, "$text_sm"), ("font_weight", Num, "600")]],
    },
    Element {
        name: "git_log.details.body",
        about: "The rest of the selected commit's message",
        groups: &[&[("font_size", Len, "$text_sm")]],
    },
    Element {
        name: "git_log.details.meta",
        about: "The selected commit's author, dates",
        groups: &[&[("font_size", Len, "$text_xs"), ("text", Color, "$text_muted")]],
    },
    simple!("git_log.details.hash", "The selected commit's hash: clicking it copies the whole hash"),
    Element {
        name: "git_log.details.files_title",
        about: "The line over the selected commit's files",
        groups: &[&[
            ("padding_x", Len, "10"),
            ("padding_y", Len, "4"),
            ("font_size", Len, "$text_xs"),
            ("font_weight", Num, "600"),
            ("text", Color, "$text_muted"),
        ]],
    },
    Element { name: "git_log.files", about: "The selected commit's files", groups: &[LIST] },
    Element { name: "git_log.file", about: "A file of the selected commit: clicking it shows its diff", groups: &[ROW] },
    Element { name: "scratches", about: "The scratch files panel", groups: &[PANEL, &[("start_height", Len, "220")], IMAGE] },
    Element { name: "scratches.header", about: "The scratches panel's title bar", groups: &[HEADER] },
    simple!("scratches.header.button", "A button in the scratches panel's title bar"),
    Element { name: "scratches.list", about: "The scratches panel's list of rows", groups: &[LIST] },
    Element { name: "scratches.row", about: "A scratch file", groups: &[ROW] },
    Element { name: "scratches.empty", about: "What the scratches panel shows without files", groups: &[EMPTY] },
    Element {
        name: "shells",
        about: "The shells panel: the shells the agents in the workspace's terminals run commands in",
        groups: &[PANEL, &[("start_height", Len, "180")], IMAGE],
    },
    Element { name: "shells.header", about: "The shells panel's title bar", groups: &[HEADER] },
    simple!("shells.header.button", "A button in the shells panel's title bar"),
    Element { name: "shells.list", about: "The shells panel's list of rows", groups: &[LIST] },
    Element { name: "shells.row", about: "A shell: the command it runs and how long it has run", groups: &[ROW] },
    Element { name: "shells.empty", about: "What the shells panel shows without shells", groups: &[EMPTY] },
    Element { name: "panel_group", about: "A panel group: a panel the user added, holding the panels dragged onto it", groups: &[PANEL] },
    Element {
        name: "panel_group.header",
        about: "A panel group's bar while the layout is edited, over its top: it drags the group, and holds its layout and close buttons",
        groups: &[&[
            ("margin_top", Len, "3"),
            ("padding_x", Len, "4"),
            ("height", Len, "24"),
            ("gap", Len, "2"),
            ("radius", Len, "$radius"),
            ("background", Color, "$background"),
            ("border", Color, "$primary/60"),
            ("border_width", Len, "1"),
            ("shadow", Text, "md"),
            ("title", Text, ""),
        ]],
    },
    Element {
        name: "panel_group.header.grip",
        about: "The grip on a panel group's bar",
        groups: &[&[("width", Len, "14"), ("height", Len, "14"), ("text", Color, "$text_muted")]],
    },
    simple!("panel_group.header.button", "A button in a panel group's title bar"),
    Element { name: "panel_group.empty", about: "What a panel group shows without panels", groups: &[EMPTY] },
    Element {
        name: "panel_group.edit",
        about: "A panel group while the layout is edited",
        groups: &[&[("border", Color, "$primary/40"), ("border_width", Len, "1")]],
    },
    Element {
        name: "panel.edit",
        about: "Over each panel while the layout is edited: it drags the panel, from anywhere on it",
        groups: &[&[("hover_background", Color, "$primary/10")]],
    },
    Element {
        name: "panel_group.seam",
        about: "A strip along a panel group's bottom, and one along its right side, while the layout is edited; `size` is \
                how thick each is",
        groups: &[&[("size", Len, "18")]],
    },
    simple!("panel_group.seam.button", "The button on a seam that adds an empty panel group there: below the group, or beside it"),
    Element {
        name: "usage",
        about: "The usage panel: the Claude account the shown tab runs as (the first one turned on when it isn't a \
                Claude tab) and how much of its plan is used, as a status bar",
        groups: &[
            PANEL,
            &[
                ("start_height", Len, "32"),
                ("padding_x", Len, "6"),
                ("gap", Len, "4"),
                ("font_size", Len, "$text_xs"),
                ("text", Color, "$text"),
            ],
        ],
    },
    Element {
        name: "usage.item",
        about: "The account in the usage bar: its name and limits. Clicking it opens `usage.card`",
        groups: &[&[
            ("height", Len, "22"),
            ("padding_x", Len, "6"),
            ("gap", Len, "6"),
            ("radius", Len, "$radius"),
            ("font_size", Len, "$text_xs"),
            ("text", Color, "$text"),
            ("detail_text", Color, "$text_muted"),
            ("hover_background", Color, "$row_hover"),
        ]],
    },
    Element { name: "usage.item.name", about: "An account's name in the usage bar", groups: &[&[("font_weight", Num, "600")]] },
    Element {
        name: "usage.context",
        about: "The shown Claude tab's context window in the usage bar: a ring of how full it is, the percent and the limit \
                it keeps. Clicking it opens `usage.context.card`",
        groups: &[&[
            ("height", Len, "22"),
            ("padding_x", Len, "6"),
            ("gap", Len, "5"),
            ("radius", Len, "$radius"),
            ("font_size", Len, "$text_xs"),
            ("text", Color, "$text"),
        ]],
    },
    Element {
        name: "usage.context.ring",
        about: "How full the context window is: its arc (`text`) on its track; `guarded` and `over` are its colors with a \
                limit, and past it",
        groups: &[&[
            ("width", Len, "13"),
            ("height", Len, "13"),
            ("stroke_width", Len, "2"),
            ("text", Color, "$muted.foreground"),
            ("track", Color, "$muted.foreground/25"),
            ("spin", Num, "0"),
            ("guarded", Color, "$info.background"),
            ("over", Color, "$warning.background"),
        ]],
    },
    Element {
        name: "usage.context.limit",
        about: "Where the limit is typed in `usage.context.card` (5 to 100)",
        groups: &[&[("width", Len, "52")]],
    },
    Element {
        name: "usage.item.limits",
        about: "A service's limits in the usage panel, in one row: those that don't fit are hidden",
        groups: &[&[("height", Len, "$row_height"), ("min_width", Len, "0"), ("gap", Len, "12")]],
    },
    Element {
        name: "usage.item.limit",
        about: "A limit in the usage panel: its name, how much is used, a small bar and when it resets, dropped from the \
                end when they don't fit",
        groups: &[&[("height", Len, "$row_height"), ("min_width", Len, "0"), ("gap", Len, "6")]],
    },
    Element {
        name: "usage.bar",
        about: "A limit's bar: the track. `usage.bar.fill` is the used part, `usage.bar.pace` marks how much would be \
                used by now at an even pace",
        groups: &[&[("height", Len, "6"), ("radius", Len, "3"), ("background", Color, "$border")]],
    },
    Element { name: "usage.item.bar", about: "The small bar of a limit in the usage panel", groups: &[&[("width", Len, "28")]] },
    Element {
        name: "usage.bar.fill",
        about: "The used part of a limit's bar; `warning` and `critical` are its colors from 80% and 95% used",
        groups: &[&[
            ("radius", Len, "3"),
            ("background", Color, "$info.background"),
            ("warning", Color, "$warning.background"),
            ("critical", Color, "$danger.background"),
        ]],
    },
    Element {
        name: "usage.bar.pace",
        about: "The pace mark on a limit's bar",
        groups: &[&[("width", Len, "2"), ("background", Color, "$text_muted")]],
    },
    Element {
        name: "usage.card",
        about: "What clicking the usage bar's account opens: its plan, every limit with its pace, extra usage, links, and every \
                account, to switch to, open a tab as, rename, turn on or off, add or remove",
        groups: &[&[
            ("width", Len, "340"),
            ("padding_y", Len, "4"),
            ("radius", Len, "$radius_lg"),
            ("background", Color, "$popover"),
            ("border", Color, "$border"),
            ("border_width", Len, "1"),
            ("shadow", Text, "lg"),
            ("font_size", Len, "$text_sm"),
            ("text", Color, "$text"),
        ]],
    },
    Element {
        name: "usage.card.section",
        about: "A part of the usage card, with a line above all but the first",
        groups: &[&[("padding_x", Len, "12"), ("padding_y", Len, "8"), ("gap", Len, "4"), ("border", Color, "$border")]],
    },
    Element {
        name: "usage.card.title",
        about: "The usage card's service name",
        groups: &[&[("font_size", Len, "1rem"), ("font_weight", Num, "600")]],
    },
    Element { name: "usage.card.heading", about: "A limit's name and how much is left", groups: &[&[("font_weight", Num, "600")]] },
    Element {
        name: "usage.card.detail",
        about: "Smaller, muted text in the usage card",
        groups: &[&[("font_size", Len, "$text_xs"), ("text", Color, "$text_muted")]],
    },
    Element {
        name: "usage.card.error",
        about: "Why the usage card's numbers could not be read",
        groups: &[&[("font_size", Len, "$text_xs"), ("text", Color, "$danger")]],
    },
    simple!("usage.card.link", "A link in the usage card (refresh, dashboard, status page, add account)"),
    Element {
        name: "usage.card.account",
        about: "An account in the usage card: clicking it runs the shown Claude tab as it (opens a Claude tab as it when \
                the shown tab isn't one); `shown` is the one the card is about",
        groups: &[&[
            ("padding_x", Len, "6"),
            ("padding_y", Len, "4"),
            ("gap", Len, "6"),
            ("radius", Len, "$radius"),
            ("hover_background", Color, "$row_hover"),
            ("shown_background", Color, "$row_selected"),
        ]],
    },
    simple!(
        "usage.card.switch",
        "Turns an account in the usage card on or off: an account turned off is dimmed and \
                can't be run as"
    ),
    Element { name: "usage.card.account.name", about: "An account's name in the usage card", groups: &[&[("font_weight", Num, "600")]] },
    Element {
        name: "system",
        about: "The system panel: how busy the machine is (CPU and memory), as a status bar",
        groups: &[PANEL, &[("start_height", Len, "32")], IMAGE],
    },
    Element { name: "system.empty", about: "What the system panel shows before its first sample", groups: &[EMPTY] },
    Element {
        name: "system.list",
        about: "The system panel's CPU and memory in one row: those that don't fit are hidden",
        groups: &[&[("height", Len, "$row_height"), ("margin_x", Len, "8"), ("margin_y", Len, "4"), ("gap", Len, "10")]],
    },
    Element {
        name: "system.item",
        about: "CPU or memory in the system panel: its label, a graph and the percent",
        groups: &[&[("height", Len, "$row_height"), ("gap", Len, "6"), ("font_size", Len, "$text_xs"), ("text", Color, "$text")]],
    },
    Element { name: "system.label", about: "The name before a system graph: `CPU`, `MEM`", groups: &[&[("text", Color, "$text_muted")]] },
    Element {
        name: "system.spark",
        about: "The CPU's sparkline: a bar per sample, oldest first",
        groups: &[&[("width", Len, "36"), ("height", Len, "10"), ("gap", Len, "1")]],
    },
    Element { name: "system.spark.bar", about: "A sample of the CPU sparkline: the room it may fill", groups: &[&[]] },
    Element {
        name: "system.spark.fill",
        about: "The busy part of a CPU sample; `warning` and `critical` are its colors from 80% and 95%",
        groups: &[&[
            ("min_height", Len, "1"),
            ("radius", Len, "1"),
            ("background", Color, "$text_muted"),
            ("warning", Color, "$warning.background"),
            ("critical", Color, "$danger.background"),
        ]],
    },
    Element {
        name: "system.bar",
        about: "The memory bar: the track. `system.bar.fill` is the part in use",
        groups: &[&[("width", Len, "36"), ("height", Len, "6"), ("radius", Len, "3"), ("background", Color, "$border")]],
    },
    Element {
        name: "system.bar.fill",
        about: "The memory in use on its bar; `warning` and `critical` are its colors from 80% and 95%",
        groups: &[&[
            ("radius", Len, "3"),
            ("background", Color, "$text_muted"),
            ("warning", Color, "$warning.background"),
            ("critical", Color, "$danger.background"),
        ]],
    },
    Element {
        name: "resources.card",
        about: "What clicking the system panel opens: what each workspace's terminals, the background sessions and \
                Tern itself use",
        groups: &[&[
            ("width", Len, "440"),
            ("padding", Len, "8"),
            ("gap", Len, "6"),
            ("radius", Len, "$radius_lg"),
            ("background", Color, "$popover"),
            ("border", Color, "$border"),
            ("border_width", Len, "1"),
            ("shadow", Text, "lg"),
            ("font_size", Len, "$text_sm"),
            ("text", Color, "$text"),
        ]],
    },
    Element {
        name: "resources.title",
        about: "The Resources card's title",
        groups: &[&[("padding_x", Len, "6"), ("font_weight", Num, "600")]],
    },
    Element {
        name: "resources.empty",
        about: "What the Resources card shows before its first sample",
        groups: &[EMPTY, &[("height", Len, "120")]],
    },
    Element {
        name: "resources.totals",
        about: "The Resources card's line of totals",
        groups: &[&[("gap", Len, "12"), ("padding_x", Len, "6")]],
    },
    Element {
        name: "resources.total",
        about: "A total: its label and value",
        groups: &[&[("gap", Len, "6"), ("font_size", Len, "$text_sm"), ("font_weight", Num, "600")]],
    },
    Element {
        name: "resources.total.label",
        about: "A total's label: `CPU`, `Memory`",
        groups: &[&[("text", Color, "$text_muted"), ("font_weight", Num, "400")]],
    },
    Element {
        name: "resources.machine",
        about: "The whole machine's CPU and memory, after the totals",
        groups: &[&[("font_size", Len, "$text_xs"), ("text", Color, "$text_muted")]],
    },
    Element {
        name: "resources.columns",
        about: "The column titles over the list; clicking one sorts by it",
        groups: &[&[
            ("padding_left", Len, "6"),
            ("padding_right", Len, "8"),
            ("gap", Len, "4"),
            ("border_bottom_width", Len, "1"),
            ("border", Color, "$border"),
            ("font_size", Len, "$text_xs"),
        ]],
    },
    simple!("resources.sort", "A column title: the column it sorts by is `selected`"),
    Element {
        name: "resources.list",
        about: "The Resources card's list of rows: as tall as its rows, up to `max_height`",
        groups: &[&[("max_height", Len, "308")]],
    },
    Element {
        name: "resources.row",
        about: "A row of the Resources card: a workspace, a terminal, a background session or a part of Tern",
        groups: &[ROW, &[("heading_weight", Num, "600"), ("busy_text", Color, "$warning.background")]],
    },
    Element { name: "resources.cpu", about: "The CPU column", groups: &[&[("width", Len, "52"), ("font_size", Len, "$text_xs")]] },
    Element { name: "resources.mem", about: "The memory column", groups: &[&[("width", Len, "60"), ("font_size", Len, "$text_xs")]] },
    Element {
        name: "resources.actions",
        about: "The buttons over a row's end, shown under the pointer",
        groups: &[&[("gap", Len, "2"), ("padding_x", Len, "4"), ("radius", Len, "$radius"), ("background", Color, "$popover")]],
    },
    simple!("resources.button", "A button at a row's end: show, close, end"),
    Element {
        name: "resources.spark",
        about: "A workspace's memory over the last minute: a bar per sample, oldest first",
        groups: &[&[("width", Len, "36"), ("height", Len, "12"), ("gap", Len, "1"), ("margin_right", Len, "4")]],
    },
    Element { name: "resources.spark.bar", about: "A sample of a memory sparkline: the room it may fill", groups: &[&[]] },
    Element {
        name: "resources.spark.fill",
        about: "A memory sample, as a share of the most in the sparkline",
        groups: &[&[("min_height", Len, "1"), ("radius", Len, "1"), ("background", Color, "$text_muted")]],
    },
    Element {
        name: "resources.footer",
        about: "Under the list: End Background Sessions, when there are any",
        groups: &[&[("gap", Len, "8"), ("padding_x", Len, "6")]],
    },
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
                `close_text` its close button, `hover_text` its text under the pointer",
        groups: &[BAR_TAB, TAB],
    },
    Element { name: "pane_tab_bar.tab_active", about: "The shown tab of a pane, over `pane_tab_bar.tab`", groups: &[BAR_TAB_ACTIVE, TAB] },
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
    simple!("file_header.button", "The Preview / Diff / VS Code buttons above a file"),
    Element {
        name: "terminal",
        about: "A terminal tab. Its colors are `terminal.colors`; a `background_opacity` below 1 lets what's behind \
                it (a background image) show through",
        groups: &[&[("padding_x", Len, "8"), ("padding_top", Len, "4"), ("background_opacity", Num, "1")], IMAGE],
    },
    Element {
        name: "terminal.find",
        about: "Find in a terminal (⌘F): the bar over its top right corner, and the matches it marks in the grid",
        groups: &[&[
            ("top", Len, "8"),
            ("right", Len, "16"),
            ("padding_x", Len, "8"),
            ("padding_y", Len, "4"),
            ("gap", Len, "4"),
            ("radius", Len, "$radius"),
            ("background", Color, "$popover"),
            ("border", Color, "$border"),
            ("border_width", Len, "1"),
            ("shadow", Text, "md"),
            ("field_width", Len, "200"),
            ("icon_size", Len, "14"),
            ("icon_color", Color, "$text_muted"),
            ("match_background", Color, "$warning.background/35"),
            ("current_background", Color, "$warning.background/80"),
        ]],
    },
    Element {
        name: "terminal.find.count",
        about: "The bar's `3/12`; `error_text` when the regex is not valid",
        groups: &[&[
            ("padding_x", Len, "4"),
            ("font_size", Len, "$text_xs"),
            ("text", Color, "$text_muted"),
            ("error_text", Color, "$danger.background"),
        ]],
    },
    simple!("terminal.find.button", "The bar's options, previous, next and close buttons"),
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
    Element {
        name: "floating",
        about: "A panel floating over the docks. `width` and `height` are its size when it starts floating,                 `min_width` and `min_height` the least it resizes to, `grip` how wide its resizing edges are",
        groups: &[&[
            ("background", Color, "$background"),
            ("border", Color, "$border"),
            ("border_width", Len, "1"),
            ("radius", Len, "$radius"),
            ("shadow", Text, "lg"),
            ("width", Len, "300"),
            ("height", Len, "400"),
            ("min_width", Len, "120"),
            ("min_height", Len, "80"),
            ("grip", Len, "6"),
        ]],
    },
    Element {
        name: "part_window",
        about: "A panel in a window of its own (GUI only): the window's background around it",
        groups: &[&[("background", Color, "$background")]],
    },
    Element {
        name: "agent_monitor",
        about: "The agent monitor: the small window listing the agents running in Tern's terminals",
        groups: &[&[
            ("padding", Len, "0.25rem"),
            ("radius", Len, "0.5rem"),
            ("font_size", Len, "$text_xs"),
            ("text", Color, "$text"),
            ("background", Color, "$background"),
            ("border", Color, "$border"),
            ("border_width", Len, "1"),
        ]],
    },
    Element {
        name: "agent_monitor.header",
        about: "The agent monitor's title bar (drag it to move the window)",
        groups: &[&[("height", Len, "1.25rem"), ("padding_left", Len, "0.25rem"), ("gap", Len, "0.25rem"), ("text", Color, "$text_muted")]],
    },
    Element {
        name: "agent_monitor.empty",
        about: "What the agent monitor shows with no agents running",
        groups: &[&[("padding_x", Len, "0.25rem"), ("padding_bottom", Len, "0.25rem"), ("text", Color, "$text_muted")]],
    },
    Element { name: "shot_overlay", about: "The screenshot overlay over a frozen display", groups: &[&[("background", Color, "#000000")]] },
    Element {
        name: "shot_overlay.toolbar",
        about: "The overlay's tools, by the captured region",
        groups: &[&[("height", Len, "40"), ("gap", Len, "0.125rem")], SHOT_BAR],
    },
    Element {
        name: "shot_overlay.style_bar",
        about: "The overlay's stroke sizes and colors, while a drawing tool is picked",
        groups: &[&[("height", Len, "36"), ("gap", Len, "0.25rem")], SHOT_BAR],
    },
    Element {
        name: "shot_overlay.hint",
        about: "What the overlay says before a region is dragged",
        groups: &[&[
            ("padding_x", Len, "0.75rem"),
            ("padding_y", Len, "0.375rem"),
            ("radius", Len, "0.375rem"),
            ("font_size", Len, "$text_sm"),
            ("text", Color, "#ffffff"),
            ("background", Color, "#000000b0"),
        ]],
    },
    Element {
        name: "shot_overlay.size_label",
        about: "The captured region's size, above it",
        groups: &[&[
            ("padding_x", Len, "0.5rem"),
            ("padding_y", Len, "0.125rem"),
            ("radius", Len, "0.375rem"),
            ("font_size", Len, "$text_xs"),
            ("text", Color, "#ffffff"),
            ("background", Color, "#2f6bff"),
        ]],
    },
    Element {
        name: "shot.separator",
        about: "The line between groups of screenshot tools",
        groups: &[&[("width", Len, "1"), ("height", Len, "20"), ("margin_x", Len, "0.25rem"), ("background", Color, "$border")]],
    },
    Element {
        name: "shot.size",
        about: "A stroke size to pick; `dot` colors its dot",
        groups: &[&[
            ("width", Len, "24"),
            ("height", Len, "24"),
            ("radius", Len, "0.375rem"),
            ("dot", Color, "$foreground"),
            ("selected_background", Color, "$accent"),
            ("hover_background", Color, "$secondary_hover"),
        ]],
    },
    Element {
        name: "shot.color",
        about: "A color to pick (filled with it)",
        groups: &[&[
            ("width", Len, "18"),
            ("height", Len, "18"),
            ("radius", Len, "9999"),
            ("border_width", Len, "2"),
            ("border", Color, "$border"),
            ("selected_border", Color, "$foreground"),
        ]],
    },
    Element {
        name: "shot_editor",
        about: "The screenshot editor window",
        groups: &[&[("background", Color, "$background"), ("text", Color, "$foreground")]],
    },
    Element {
        name: "shot_editor.toolbar",
        about: "The editor's tools, at the top",
        groups: &[&[
            ("padding_x", Len, "0.5rem"),
            ("padding_y", Len, "0.25rem"),
            ("gap", Len, "0.125rem"),
            ("border", Color, "$border"),
            ("border_bottom_width", Len, "1"),
        ]],
    },
    Element {
        name: "shot_editor.tabs",
        about: "The editor's tab per screenshot",
        groups: &[&[("background", Color, "$tab_bar_background"), ("border", Color, "$border"), ("border_bottom_width", Len, "1")]],
    },
    Element {
        name: "shot_editor.tab",
        about: "A screenshot's tab in the editor. `hover_text` is its text under the pointer",
        groups: &[BAR_TAB],
    },
    Element { name: "shot_editor.tab_active", about: "The shown screenshot's tab, over `shot_editor.tab`", groups: &[BAR_TAB_ACTIVE] },
    Element { name: "shot_editor.page", about: "Where the editor shows the page", groups: &[&[("background", Color, "$muted")]] },
    Element {
        name: "shot_editor.empty",
        about: "What the editor says with no screenshot open",
        groups: &[&[("font_size", Len, "$text_sm"), ("text", Color, "$text_muted")]],
    },
    Element {
        name: "shot_editor.hint",
        about: "The line of keys at the bottom of the editor",
        groups: &[&[
            ("padding_x", Len, "0.75rem"),
            ("padding_y", Len, "0.25rem"),
            ("font_size", Len, "$text_xs"),
            ("text", Color, "$text_muted"),
            ("border", Color, "$border"),
            ("border_top_width", Len, "1"),
        ]],
    },
    Element {
        name: "shot_tray",
        about: "The tray of screenshots to send to an agent",
        groups: &[&[
            ("padding", Len, "0.375rem"),
            ("gap", Len, "0.375rem"),
            ("radius", Len, "0.5rem"),
            ("text", Color, "$foreground"),
            ("background", Color, "$background"),
            ("border", Color, "$border"),
            ("border_width", Len, "1"),
        ]],
    },
    Element {
        name: "shot_tray.title",
        about: "The tray's title",
        groups: &[&[("font_size", Len, "$text_xs"), ("font_weight", Num, "600")]],
    },
    Element {
        name: "shot_tray.shot",
        about: "A screenshot's thumbnail in the tray",
        groups: &[&[
            ("width", Len, "54"),
            ("height", Len, "36"),
            ("radius", Len, "0.25rem"),
            ("background", Color, "$muted"),
            ("border", Color, "$border"),
            ("border_width", Len, "1"),
        ]],
    },
    Element {
        name: "shot_send_menu",
        about: "The tray's send menu: the folders, and the tabs in the one pointed at",
        groups: &[&[
            ("padding", Len, "4"),
            ("gap", Len, "4"),
            ("radius", Len, "0.5rem"),
            ("font_size", Len, "$text_sm"),
            ("text", Color, "$foreground"),
            ("background", Color, "$popover"),
            ("border", Color, "$border"),
            ("border_width", Len, "1"),
        ]],
    },
    Element {
        name: "shot_send_menu.folders",
        about: "The send menu's folder column",
        groups: &[&[("width", Len, "140"), ("padding_right", Len, "4"), ("border", Color, "$border"), ("border_right_width", Len, "1")]],
    },
    Element { name: "shot_send_menu.items", about: "The send menu's tab column", groups: &[&[("width", Len, "200")]] },
    Element {
        name: "shot_send_menu.row",
        about: "A folder or a tab in the send menu",
        groups: &[&[
            ("height", Len, "26"),
            ("padding_x", Len, "8"),
            ("gap", Len, "6"),
            ("radius", Len, "4"),
            ("active_background", Color, "$accent"),
            ("icon_text", Color, "$text_muted"),
        ]],
    },
    Element {
        name: "shot_tray.error",
        about: "Why the tray's last send failed",
        groups: &[&[("font_size", Len, "$text_xs"), ("text", Color, "$danger")]],
    },
    Element { name: "dialog", about: "Prompts and confirmations", groups: &[&[("width", Len, "420"), ("wide_width", Len, "520")]] },
    Element { name: "dialog.body", about: "A dialog's message, under its title", groups: &[&[("gap", Len, "16")]] },
    Element {
        name: "dialog.note",
        about: "The small line under a dialog's message",
        groups: &[&[("padding_top", Len, "0.5rem"), ("font_size", Len, "$text_xs"), ("text", Color, "$text_muted")]],
    },
    Element { name: "dialog.footer", about: "A dialog's buttons", groups: &[&[("gap", Len, "0.5rem")]] },
    Element {
        name: "dialog.pick_account.list",
        about: "The accounts a new Claude tab may run as, in the dialog that asks which",
        groups: &[&[("gap", Len, "2")]],
    },
    Element {
        name: "dialog.pick_account.item",
        about: "An account to run a new Claude tab as: its key, name, email and usage",
        groups: &[&[
            ("padding_x", Len, "8"),
            ("padding_y", Len, "6"),
            ("gap", Len, "10"),
            ("radius", Len, "$radius"),
            ("hover_background", Color, "$row_hover"),
        ]],
    },
    Element {
        name: "dialog.pick_account.key",
        about: "The number key that picks an account",
        groups: &[&[("font_size", Len, "$text_xs"), ("text", Color, "$text_muted")]],
    },
    Element { name: "dialog.pick_account.name", about: "An account's name", groups: &[&[("font_weight", Num, "600")]] },
    Element {
        name: "dialog.pick_account.detail",
        about: "An account's email and usage",
        groups: &[&[("font_size", Len, "$text_xs"), ("text", Color, "$text_muted")]],
    },
    Element {
        name: "palette",
        about: "The command palette (⌘⇧P), Go to File (⌘P) and Search in Files (⌘⇧F), in a dialog `dialog_width` wide",
        groups: &[&[("dialog_width", Len, "640"), ("height", Len, "420"), ("gap", Len, "4")]],
    },
    Element {
        name: "palette.field",
        about: "The palette's search field, with Search in Files' options (`options_gap` apart)",
        groups: &[&[
            ("padding_x", Len, "8"),
            ("gap", Len, "6"),
            ("radius", Len, "$radius"),
            ("border", Color, "$input"),
            ("border_width", Len, "1"),
            ("icon_size", Len, "16"),
            ("icon_color", Color, "$text_muted"),
            ("options_gap", Len, "2"),
        ]],
    },
    simple!("palette.option", "Search in Files' Match Case, Match Whole Word and Use Regular Expression buttons"),
    Element {
        name: "palette.status",
        about: "The line under the field in Search in Files: searching, how many results, or what is wrong with the query",
        groups: &[&[
            ("padding_x", Len, "8"),
            ("font_size", Len, "$text_xs"),
            ("text", Color, "$text_muted"),
            ("error_text", Color, "$danger.background"),
        ]],
    },
    Element { name: "palette.list", about: "The palette's list of commands, files or results", groups: &[&[]] },
    Element {
        name: "palette.row",
        about: "A command, a file in Go to File, or a matching line (its number `line_width` wide and `line_gap` from the text, the matched text in `match_*`)",
        groups: &[
            ROW,
            &[
                ("line_width", Len, "36"),
                ("line_gap", Len, "8"),
                ("code_font_size", Len, "$text_xs"),
                ("match_background", Color, "$warning.background/35"),
                ("match_text", Color, ""),
            ],
        ],
    },
    Element {
        name: "palette.file",
        about: "A file over its matching lines in Search in Files",
        groups: &[ROW, &[("font_weight", Num, "600")]],
    },
    Element { name: "palette.empty", about: "What the palette shows with nothing to list", groups: &[EMPTY] },
    Element {
        name: "palette.footer",
        about: "The keys the palette takes, under its list",
        groups: &[&[
            ("padding_x", Len, "8"),
            ("padding_top", Len, "6"),
            ("gap", Len, "16"),
            ("font_size", Len, "$text_xs"),
            ("text", Color, "$text_muted"),
        ]],
    },
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
    Element {
        name: "settings",
        about: "The Settings window",
        groups: &[&[("background", Color, "$background"), ("text", Color, "$text")]],
    },
    Element {
        name: "settings.nav",
        about: "The list of sections down the Settings window's side",
        groups: &[&[
            ("width", Len, "200"),
            ("padding", Len, "8"),
            ("gap", Len, "2"),
            ("background", Color, "$panel_background"),
            ("border", Color, "$border"),
            ("border_right_width", Len, "1"),
        ]],
    },
    Element {
        name: "settings.nav_item",
        about: "A section in the Settings window's list",
        groups: &[&[
            ("padding_x", Len, "8"),
            ("padding_y", Len, "6"),
            ("gap", Len, "8"),
            ("radius", Len, "$radius"),
            ("font_size", Len, "$text_sm"),
            ("text", Color, "$text_muted"),
            ("hover_background", Color, "$row_hover"),
        ]],
    },
    Element {
        name: "settings.nav_item_active",
        about: "The section shown",
        groups: &[&[("background", Color, "$row_selected"), ("text", Color, "$text")]],
    },
    Element {
        name: "settings.page",
        about: "A section's page",
        groups: &[&[("max_width", Len, "760"), ("padding_x", Len, "32"), ("padding_y", Len, "28"), ("gap", Len, "20")]],
    },
    Element {
        name: "settings.title",
        about: "A page's title",
        groups: &[&[("font_size", Len, "1.25rem"), ("font_weight", Num, "600"), ("text", Color, "$text")]],
    },
    Element {
        name: "settings.subtitle",
        about: "The line under a page's title",
        groups: &[&[("font_size", Len, "$text_sm"), ("text", Color, "$text_muted")]],
    },
    Element {
        name: "settings.group",
        about: "A card of settings rows",
        groups: &[&[("border", Color, "$border"), ("border_width", Len, "1"), ("radius", Len, "10"), ("background", Color, "$muted/30")]],
    },
    Element {
        name: "settings.row",
        about: "A setting: its name and description, its control on the right; `divider_width` is the line above all but the first",
        groups: &[&[
            ("padding_x", Len, "16"),
            ("padding_y", Len, "12"),
            ("gap", Len, "16"),
            ("border", Color, "$border"),
            ("divider_width", Len, "1"),
        ]],
    },
    Element {
        name: "settings.row.title",
        about: "A setting's name",
        groups: &[&[("font_size", Len, "$text_sm"), ("font_weight", Num, "500"), ("text", Color, "$text")]],
    },
    Element {
        name: "settings.row.description",
        about: "What a setting does",
        groups: &[&[("font_size", Len, "$text_xs"), ("text", Color, "$text_muted")]],
    },
    Element {
        name: "settings.row.problem",
        about: "Why a setting couldn't be applied",
        groups: &[&[("font_size", Len, "$text_xs"), ("text", Color, "$danger.background")]],
    },
    Element {
        name: "settings.row.value",
        about: "A setting's current value",
        groups: &[&[("font_size", Len, "$text_sm"), ("text", Color, "$text")]],
    },
    Element {
        name: "settings.switch",
        about: "An on/off switch, off",
        groups: &[&[
            ("width", Len, "34"),
            ("height", Len, "20"),
            ("padding", Len, "2"),
            ("radius", Len, "10"),
            ("background", Color, "$muted.foreground/40"),
        ]],
    },
    Element {
        name: "settings.switch_on",
        about: "An on/off switch, on",
        groups: &[&[("padding_left", Len, "16"), ("background", Color, "$primary")]],
    },
    Element {
        name: "settings.switch.knob",
        about: "The knob of a switch",
        groups: &[&[("width", Len, "16"), ("height", Len, "16"), ("radius", Len, "8"), ("background", Color, "$background")]],
    },
    Element {
        name: "settings.link",
        about: "The secret link to open Tern from another device",
        groups: &[&[
            ("padding_x", Len, "8"),
            ("padding_y", Len, "6"),
            ("radius", Len, "$radius"),
            ("font_size", Len, "$text_xs"),
            ("text", Color, "$text"),
            ("background", Color, "$muted/60"),
        ]],
    },
    Element {
        name: "settings.qr",
        about: "The link as a QR code: `size` is the code's, `color` its dark squares",
        groups: &[&[
            ("size", Len, "132"),
            ("padding", Len, "8"),
            ("radius", Len, "8"),
            ("background", Color, "#ffffff"),
            ("color", Color, "#000000"),
        ]],
    },
    Element {
        name: "settings.device.dot",
        about: "A paired device's dot: `connected_background` while it has a page open",
        groups: &[&[
            ("width", Len, "8"),
            ("height", Len, "8"),
            ("radius", Len, "4"),
            ("background", Color, "$muted.foreground/40"),
            ("connected_background", Color, "$success.background"),
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
    /// In a see-through window, the background under everything: the
    /// theme's, at the window's opacity.
    base: Option<Hsla>,
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
    let see_through = matches!(props.get(&("window", "background_opacity")), Some(Val::Num(n)) if *n < 1.);
    let base = see_through.then_some(theme.background);
    Looks(std::sync::Arc::new(LooksData { props, tokens, rem, base }))
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
    // An element's properties come one after another: one that doesn't
    // exist is told once, not once per property.
    let mut unknown = None;
    for &(name, prop) in over.elements.keys() {
        match element(name) {
            None if unknown == Some(name) => {},
            None => {
                problems.push(format!("elements: no element \"{name}\""));
                unknown = Some(name);
            },
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

    /// `raw`'s properties: those of [`box_prop`] and of `element` (when there is
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
            let found = element.and_then(|e| e.prop(name)).or_else(|| box_prop(name));
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
            let pct: f32 = pct
                .trim()
                .parse()
                .ok()
                .filter(|p: &f32| (0. ..=100.).contains(p))
                .ok_or_else(|| format!("\"{pct}\" is not a percentage (0 to 100)"))?;
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
pub fn looks(cx: &App) -> Looks {
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

    /// Whether a `background` of `c` would only paint the see-through
    /// window's base over itself. The window already paints it under
    /// everything (gpui-component's root), and a second layer would add up
    /// to a more opaque one; with an opaque window it changes nothing.
    /// Elements that float over other content keep theirs.
    fn repaints_base(&self, c: Hsla) -> bool {
        let Some(base) = self.looks.base else { return false };
        let rgba = |c: Hsla| {
            let c = c.to_rgb();
            [c.r, c.g, c.b, c.a].map(|v| (v * 255.).round() as u8)
        };
        !OVER_CONTENT.contains(&self.name) && rgba(c) == rgba(base)
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
        // A share of the parent's room, from nothing: `"grow": "{used}"` and
        // `"grow": "{left}"` side by side draw a bar `used` percent full.
        if let Some(g) = self.num_opt("grow") {
            st.flex_grow = Some(g.max(0.));
            st.flex_shrink = Some(1.);
            st.flex_basis = Some(relative(0.).into());
            st.min_size.width = Some(px(0.).into());
            st.min_size.height = Some(px(0.).into());
        }
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
        if let Some(c) = self.color_opt("background").filter(|c| !self.repaints_base(*c)) {
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
    /// container must be `relative`), when the element or the template box
    /// has one set.
    pub fn backdrop(&self) -> Option<AnyElement> {
        // A template box takes these without its element having them.
        let image = |prop: &'static str| {
            let own = element(self.name).is_some_and(|e| e.prop(prop).is_some());
            self.inline.as_ref().and_then(|m| m.get(prop)).or_else(|| if own { self.get(prop) } else { None })
        };
        let text = |prop| match image(prop) {
            Some(Val::Text(t)) => t.clone(),
            _ => SharedString::default(),
        };
        let path = text("background_image");
        if path.is_empty() {
            return None;
        }
        let opacity = match image("background_image_opacity") {
            Some(Val::Num(n)) => *n,
            _ => 1.,
        };
        let fit = match text("background_image_fit").as_ref() {
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
                .opacity(opacity.clamp(0., 1.))
                .into_any_element(),
        )
    }
}

fn set<T>(slot: &mut Option<T>, v: Option<T>) {
    if v.is_some() {
        *slot = v;
    }
}

/// How far a list row of element `name` (`explorer.row`, …) at `depth`
/// starts its content: the row templates' `indent`.
pub fn row_indent(name: &'static str, depth: usize, cx: &App) -> f64 {
    let e = el(name, cx);
    f64::from(f32::from(e.px("padding_left") + e.px("indent") * depth as f32))
}

/// A row's file icon.
pub fn row_icon(name: &'static str, icon: impl Into<Icon>, color: Hsla, cx: &App) -> Icon {
    icon.into().size(el(name, cx).px("icon_size")).text_color(color)
}

/// A dialog's width: a prompt or confirmation, or (`wide`) one with more to say.
pub fn dialog_width(wide: bool, cx: &App) -> Pixels {
    el("dialog", cx).px(if wide { "wide_width" } else { "width" })
}

/// What follows the pointer while dragging (files, tabs, workspaces): template
/// `drag_chip` with `label`, and `icon` (a Lucide name) when given.
pub fn drag_chip(label: SharedString, icon: Option<&'static str>, cx: &App) -> AnyElement {
    let mut scope = crate::template::Scope::new("drag_chip").var("label", label);
    if let Some(icon) = icon {
        scope = scope.var("icon", icon);
    }
    crate::template::render_plain("drag_chip", scope, cx)
}

/// How opaque the window's backgrounds are, 0 to 1: the `window` element's
/// `background_opacity`.
pub fn window_opacity(looks: &Looks) -> f32 {
    match looks.props.get(&("window", "background_opacity")) {
        Some(Val::Num(n)) => n.clamp(0., 1.),
        _ => 1.,
    }
}

/// What the desktop does behind a window: hidden, or showing through
/// once the window's backgrounds are see-through, blurred by `blur` points.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Backdrop {
    pub appearance: WindowBackgroundAppearance,
    pub blur: u32,
}

/// The [`Backdrop`] the `window` element asks for: see-through below a
/// `background_opacity` of 1, blurred by its `background_blur`. macOS blurs
/// by that radius; Windows and Linux blur as the system does, any radius
/// but 0.
pub fn backdrop(cx: &App) -> Backdrop {
    let looks = looks(cx);
    if window_opacity(&looks) >= 1. {
        return Backdrop::default();
    }
    let blur = match looks.props.get(&("window", "background_blur")) {
        Some(Val::Num(n)) => n.max(0.).round() as u32,
        _ => 0,
    };
    let appearance =
        if blur > 0 && !cfg!(target_os = "macos") { WindowBackgroundAppearance::Blurred } else { WindowBackgroundAppearance::Transparent };
    Backdrop { appearance, blur }
}

/// Gives `window` the [`backdrop`] when it changed from `last`, what it
/// was given before.
pub fn sync_backdrop(last: &mut Backdrop, window: &Window, cx: &App) {
    let now = backdrop(cx);
    if now == *last {
        return;
    }
    if now.appearance != last.appearance {
        window.set_background_appearance(now.appearance);
    }
    #[cfg(target_os = "macos")]
    crate::platform::set_blur_radius(window, now.blur);
    *last = now;
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
    use super::{ELEMENTS, El, Looks, Overrides, Val, check_names, resolve, theme_color, theme_colors, window_opacity};

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
    fn an_unknown_element_is_told_once() {
        let mut over = Overrides::default();
        over.elements.insert(("gone.tab", "border"), "#000000".into());
        over.elements.insert(("gone.tab", "text"), "#000000".into());
        over.elements.insert(("explorer.row", "nope"), "1".into());
        let (_, problems) = looks(&over);
        assert_eq!(problems.iter().filter(|p| p.contains("no element \"gone.tab\"")).count(), 1, "{problems:?}");
        assert_eq!(problems.len(), 2, "{problems:?}");
    }

    #[test]
    fn a_see_through_window_paints_its_background_once() {
        let el = |l: &Looks, name| El { name, looks: l.clone(), inline: None };
        let mut theme = ThemeColor::default();
        theme.background.a = 0.5;
        let (opaque, _) = looks(&Overrides::default());
        assert_eq!(window_opacity(&opaque), 1.);
        assert!(!el(&opaque, "workspace").repaints_base(theme.background));

        let mut over = Overrides::default();
        over.elements.insert(("window", "background_opacity"), "0.5".into());
        let l = resolve(&over, &theme, px(16.), Path::new("/cfg"), &mut vec![]);
        assert_eq!(window_opacity(&l), 0.5);
        assert!(el(&l, "workspace").repaints_base(theme.background));
        assert!(!el(&l, "workspace").repaints_base(theme.title_bar));
        // It floats over other content.
        assert!(!el(&l, "floating").repaints_base(theme.background));
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
        let sky = Path::new("/cfg").join("img/sky.png").to_string_lossy().into_owned();
        assert_eq!(get(&l, "explorer", "background_image"), &Val::Text(sky.into()));
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
    fn color_percentages() {
        let mut over = Overrides::default();
        over.elements.insert(("drop_zone", "border"), "$primary/50".into());
        let (l, problems) = looks(&over);
        assert_eq!(problems, Vec::<String>::new());
        let primary = theme_color(&theme_colors(&ThemeColor::default()), "primary").unwrap();
        assert_eq!(get(&l, "drop_zone", "border"), &Val::Color(primary.opacity(0.5)));
        for bad in ["NaN", "inf", "-5", "500", "x"] {
            let mut over = Overrides::default();
            over.elements.insert(("drop_zone", "border"), format!("$primary/{bad}"));
            let (l, problems) = looks(&over);
            assert_eq!(problems.len(), 1, "{bad}: {problems:?}");
            assert!(problems[0].contains("percentage"), "{bad}: {problems:?}");
            // The default stays.
            assert_eq!(get(&l, "drop_zone", "border"), &Val::Color(primary.opacity(0.6)));
        }
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
