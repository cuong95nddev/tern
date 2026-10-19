//! Settings and themes from files the user (or an agent) edits: the
//! config folder ([`crate::dirs::config`]) holds `settings.json` and a
//! `themes` folder. Saving either applies it at once; a file that can't be
//! read keeps what was applied and puts the reason in the status bar.
//!
//! Tern writes `settings.schema.json` (every key, with its meaning) and
//! `README.md` there on each start, and the built-in themes under
//! `reference/` as templates to copy into `themes/`.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::{ActiveTheme, Theme, ThemeConfig, ThemeConfigColors, ThemeRegistry, try_parse_color};
use gpui_kit::*;
use schemars::JsonSchema;
use serde::Deserialize;
use tern_fs::FsWatcher;
use tern_term::palette::{Palette, parse_hex};
use tern_term::vt::vte::ansi::Rgb;

use crate::style::{self, Overrides};
use crate::terminal_view::TerminalLook;

const SETTINGS: &str = "settings.json";
const SCHEMA: &str = "settings.schema.json";
const README: &str = "README.md";
const THEMES: &str = "themes";
const REFERENCE: &str = "reference";
const ELEMENTS_DOC: &str = "elements.md";
pub const DEFAULT_THEME: &str = "Tern Dark";
const LIGHT_THEME: &str = "Tern Light";

/// `settings.json`. Every key is optional; a missing one keeps Tern's default.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct SettingsFile {
    /// The schema this file follows (`./settings.schema.json`).
    #[serde(rename = "$schema")]
    pub schema: Option<String>,
    /// The theme: "Tern Dark", "Tern Light", or the `name` of a theme in a
    /// `themes/*.json` file.
    pub theme: Option<String>,
    /// The window's own text: panels, tabs, menus, dialogs.
    pub ui: UiSettings,
    /// The file editor and code blocks.
    pub editor: EditorSettings,
    /// Terminal tabs.
    pub terminal: TerminalSettings,
    /// Where the side panels sit.
    pub layout: LayoutSettings,
    /// Shared values that elements point at with `"$name"`: the built-in
    /// ones (`row_height`, `bar_background`, …; see `reference/elements.md`)
    /// or new ones of your own. A value is a number (pixels), `"0.875rem"`, a
    /// color, text, or `"$name"` of another token, an element property
    /// (`"$explorer.row.height"`) or a theme color (`"$muted.foreground"`);
    /// a color `"$name/NN"` is that color at NN% of its opacity.
    #[schemars(with = "BTreeMap<String, serde_json::Value>")]
    pub tokens: BTreeMap<String, serde_json::Value>,
    /// Each element's own properties over its defaults, by element:
    /// `{"explorer.row": {"height": 26, "text": "#e0e0e0"}}`. Values are
    /// as in `tokens`.
    #[schemars(with = "BTreeMap<String, BTreeMap<String, serde_json::Value>>")]
    pub elements: BTreeMap<String, BTreeMap<String, serde_json::Value>>,
    /// The window's parts as trees of boxes, by name, over the built-in ones
    /// (see `reference/templates.md` and `reference/templates.json`).
    pub templates: BTreeMap<String, crate::template::Spec>,
    /// UI colors over the theme's, by theme key (`background`, `border`,
    /// `tab_bar.background`, `list.active.background`, …). A value is
    /// `#rrggbb`, `#rrggbbaa`, or a Tailwind name like `neutral-800` or
    /// `blue-500/50`.
    #[schemars(with = "ThemeConfigColors")]
    pub colors: BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct LayoutSettings {
    /// Turns off moving and resizing panels.
    pub locked: bool,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct UiSettings {
    /// Font family of the UI text (default: the system font).
    pub font_family: Option<String>,
    /// Base UI font size (default 16); every UI text size scales with it.
    pub font_size: Option<f32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct EditorSettings {
    /// Monospace font of the editor and code blocks (default: the terminal's).
    pub font_family: Option<String>,
    /// Editor font size (default 13).
    pub font_size: Option<f32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct TerminalSettings {
    /// Monospace font family (default: Menlo on macOS, Cascadia Mono or
    /// Consolas on Windows, JetBrains Mono or DejaVu Sans Mono on Linux).
    pub font_family: Option<String>,
    /// Font size (default 13).
    pub font_size: Option<f32>,
    /// Line height as a multiple of the font size (default 1.25).
    pub line_height: Option<f32>,
    /// Windows: Ctrl+V pastes (default true). False sends ^V to the
    /// program instead (Vim's block selection); Ctrl+Shift+V still pastes.
    pub ctrl_v_paste: Option<bool>,
    /// Terminal colors over the theme's.
    pub colors: TerminalColors,
}

/// A terminal palette: each color `#rrggbb`, a missing one keeps the theme's.
#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct TerminalColors {
    pub background: Option<String>,
    pub foreground: Option<String>,
    pub cursor: Option<String>,
    /// Behind selected text.
    pub selection: Option<String>,
    /// Faint (SGR 2) text.
    pub dim_foreground: Option<String>,
    pub black: Option<String>,
    pub red: Option<String>,
    pub green: Option<String>,
    pub yellow: Option<String>,
    pub blue: Option<String>,
    pub magenta: Option<String>,
    pub cyan: Option<String>,
    pub white: Option<String>,
    pub bright_black: Option<String>,
    pub bright_red: Option<String>,
    pub bright_green: Option<String>,
    pub bright_yellow: Option<String>,
    pub bright_blue: Option<String>,
    pub bright_magenta: Option<String>,
    pub bright_cyan: Option<String>,
    pub bright_white: Option<String>,
}

/// What the last load found wrong, shown in the status bar until fixed.
#[derive(Default)]
pub struct SettingsStatus {
    pub error: Option<SharedString>,
}

impl Global for SettingsStatus {}

/// The config folder watcher, kept alive with the app.
struct Watch(#[allow(dead_code)] FsWatcher);

impl Global for Watch {}

/// What a load needs, and what the last one found.
struct Loader {
    dir: PathBuf,
    /// The platform's terminal font, for when the settings name none.
    mono_font: SharedString,
    /// A theme shown in place of the settings' one while the theme picker
    /// moves over it.
    preview: Option<SharedString>,
    /// The themes there are, as of the last load.
    themes: Vec<ThemeChoice>,
    /// The theme `settings.json` names (or the default).
    current: SharedString,
}

impl Global for Loader {}

/// A theme the picker offers.
#[derive(Clone)]
pub struct ThemeChoice {
    pub name: SharedString,
    pub dark: bool,
}

/// A theme: its UI part (`gpui-component`'s theme format) and the terminal
/// palette in its `terminal` key.
#[derive(Clone, Default)]
struct TernTheme {
    ui: ThemeConfig,
    terminal: TerminalColors,
    /// Its `tokens` and `elements`, under the settings' ones.
    tokens: BTreeMap<String, serde_json::Value>,
    elements: BTreeMap<String, BTreeMap<String, serde_json::Value>>,
    /// Its `templates`, under the settings' ones.
    templates: BTreeMap<String, crate::template::Spec>,
}

/// Writes the reference files, loads the settings and applies them, and
/// applies them again whenever the config folder changes. `mono_font` is
/// the platform's terminal font, for when the settings name none.
pub fn init(mono_font: SharedString, cx: &mut App) {
    let dir = crate::dirs::config();
    if let Err(e) = write_reference_files(&dir, cx) {
        log::warn!("settings: can't write {}: {e}", dir.display());
    }
    cx.set_global(SettingsStatus::default());
    cx.set_global(Loader { dir: dir.clone(), mono_font, preview: None, themes: vec![], current: DEFAULT_THEME.into() });
    load(cx);

    let (tx, rx) = flume::unbounded::<()>();
    let watched = dir.clone();
    match FsWatcher::new(&dir, Duration::from_millis(100), move |b| {
        if !b.paths.iter().all(|p| is_app_data(&watched, p)) {
            let _ = tx.send(());
        }
    }) {
        Ok(w) => cx.set_global(Watch(w)),
        Err(e) => log::warn!("settings: can't watch {}: {e}", dir.display()),
    }
    cx.spawn(async move |cx| {
        while rx.recv_async().await.is_ok() {
            while rx.try_recv().is_ok() {}
            cx.update(load);
        }
    })
    .detach();
}

/// Whether `p` is one of the files Tern keeps for itself in the config
/// folder `dir` (on Windows it is also `dirs::data()`): the session, window
/// places, recent folders, Claude arguments, scratches, or the temp file of
/// one being written. They change all the time and aren't settings.
fn is_app_data(dir: &Path, p: &Path) -> bool {
    let Some(first) = p.strip_prefix(dir).ok().and_then(|rel| rel.components().next()) else { return false };
    let name = first.as_os_str().to_string_lossy();
    // `write_atomic`'s temp file: `.<name>.tern-<pid>.tmp`.
    let name = name.strip_prefix('.').and_then(|n| n.split_once(".tern-")).map_or(&*name, |(n, _)| n);
    matches!(name, "session" | "window" | "recent" | "claude-args" | "scratches")
}

/// The themes there are, and the one `settings.json` names.
pub fn themes(cx: &App) -> (Vec<ThemeChoice>, SharedString) {
    let l = cx.global::<Loader>();
    (l.themes.clone(), l.current.clone())
}

/// Shows theme `name` without saving it, or with `None` goes back to the
/// settings' theme.
pub fn preview_theme(name: Option<SharedString>, cx: &mut App) {
    if cx.global::<Loader>().preview == name {
        return;
    }
    cx.global_mut::<Loader>().preview = name;
    load(cx);
}

/// Saves theme `name` in `settings.json`, keeping the rest of the file.
pub fn choose_theme(name: &str, cx: &mut App) {
    cx.global_mut::<Loader>().preview = None;
    set_value(&["theme"], Some(name.into()), cx);
}

/// Sets the key at `path` (`["terminal", "font_size"]`) in `settings.json`,
/// or with `None` removes it (back to the default), keeping the rest of the
/// file, and applies it.
pub fn set_value(path: &[&str], value: Option<serde_json::Value>, cx: &mut App) {
    let file = settings_path(cx);
    let old = match std::fs::read_to_string(&file) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return set_error(Some(format!("{SETTINGS}: {e}")), cx),
    };
    match with_value(old.as_deref(), path, value).and_then(|text| std::fs::write(&file, text).map_err(|e| e.to_string())) {
        Ok(()) => load(cx),
        Err(e) => {
            load(cx);
            set_error(Some(format!("can't save {} in {SETTINGS}: {e}", path.join("."))), cx);
        },
    }
}

/// `settings.json` as written (an empty object when there is none or it
/// can't be read), for showing what it sets.
pub fn file_value(cx: &App) -> serde_json::Value {
    std::fs::read_to_string(settings_path(cx)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| serde_json::json!({}))
}

/// `settings.json`, the file to edit.
pub fn settings_path(cx: &App) -> PathBuf {
    cx.global::<Loader>().dir.join(SETTINGS)
}

/// `text` (a settings file, `None` when there is none) with the key at
/// `path` set to `value`, or removed (with the objects that leaves empty).
/// Keys keep their order; the file's own spacing doesn't survive.
fn with_value(text: Option<&str>, path: &[&str], value: Option<serde_json::Value>) -> Result<String, String> {
    let mut root: serde_json::Value = match text {
        Some(t) if !t.trim().is_empty() => serde_json::from_str(t).map_err(|e| e.to_string())?,
        _ => serde_json::json!({ "$schema": format!("./{SCHEMA}") }),
    };
    let (last, parents) = path.split_last().ok_or("no key")?;
    let obj = root.as_object_mut().ok_or("the file isn't a JSON object")?;
    match value {
        Some(v) => {
            let mut obj = obj;
            for key in parents {
                let entry = obj.entry(key.to_string()).or_insert_with(|| serde_json::json!({}));
                obj = entry.as_object_mut().ok_or_else(|| format!("\"{key}\" isn't a JSON object"))?;
            }
            obj.insert(last.to_string(), v);
        },
        None => remove_at(obj, path),
    }
    Ok(serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?
        + "
")
}

/// Removes the key at `path`, and the objects on the way that it leaves empty.
fn remove_at(obj: &mut serde_json::Map<String, serde_json::Value>, path: &[&str]) {
    match path {
        [] => {},
        [key] => {
            obj.shift_remove(*key);
        },
        [key, rest @ ..] => {
            if let Some(child) = obj.get_mut(*key).and_then(|c| c.as_object_mut()) {
                remove_at(child, rest);
                if child.is_empty() {
                    obj.shift_remove(*key);
                }
            }
        },
    }
}

/// Reads the settings and themes and applies them. On an error the
/// settings applied last stay.
fn load(cx: &mut App) {
    let l = cx.global::<Loader>();
    let (dir, mono_font, preview) = (l.dir.clone(), l.mono_font.clone(), l.preview.clone());
    let (dir, mono_font) = (dir.as_path(), &mono_font);
    let mut problems = vec![];
    let settings = match std::fs::read_to_string(dir.join(SETTINGS)) {
        Ok(text) => match serde_json::from_str::<SettingsFile>(&text) {
            Ok(s) => s,
            Err(e) => {
                set_error(Some(format!("{SETTINGS}: {e}")), cx);
                return;
            },
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => SettingsFile::default(),
        Err(e) => {
            set_error(Some(format!("{SETTINGS}: {e}")), cx);
            return;
        },
    };

    let mut themes = builtin_themes(cx);
    for (name, theme) in user_themes(&dir.join(THEMES), &mut problems) {
        themes.retain(|(n, _)| *n != name);
        themes.push((name, theme));
    }
    let current: SharedString = settings.theme.clone().map_or(DEFAULT_THEME.into(), Into::into);
    let l = cx.global_mut::<Loader>();
    l.themes = themes.iter().map(|(name, t)| ThemeChoice { name: name.clone(), dark: t.ui.mode.is_dark() }).collect();
    l.current = current.clone();
    let wanted = preview.as_deref().unwrap_or(&current);
    let theme = match themes.iter().find(|(n, _)| n.as_ref() == wanted) {
        Some((_, t)) => t.clone(),
        None => {
            let names: Vec<&str> = themes.iter().map(|(n, _)| n.as_ref()).collect();
            problems.push(format!("no theme \"{wanted}\" (there are: {})", names.join(", ")));
            themes.into_iter().find(|(n, _)| n.as_ref() == DEFAULT_THEME).map(|(_, t)| t).expect("built-in theme")
        },
    };

    let over = overrides(&theme, &settings, &mut problems);
    let ui = match ui_config(&theme.ui, &settings, &over, &mut problems) {
        Ok(ui) => ui,
        Err(e) => {
            set_error(Some(e), cx);
            return;
        },
    };
    let palette = terminal_palette(&theme, &settings.terminal.colors, &mut problems);
    if let Err(e) = write_if_changed(&tern_mux::paths::palette_path(), &palette.to_text()) {
        log::warn!("settings: can't write the daemon's palette: {e}");
    }

    let editor_font: SharedString = settings.editor.font_family.clone().map_or_else(|| mono_font.clone(), Into::into);
    let editor_size = settings.editor.font_size;
    Theme::update(cx, |t| {
        t.apply_config(&Rc::new(ui.clone()));
        t.mode = ui.mode;
        // The theme's default barely shows on a black window.
        t.overlay = hsla(0., 0., 0., if ui.mode.is_dark() { 0.55 } else { 0.3 });
        if ui.mono_font_family.is_none() {
            t.mono_font_family = editor_font.clone();
        }
        if let Some(size) = editor_size {
            t.mono_font_size = px(size);
        }
    });
    // The native title bar and menus follow the theme, not the system.
    cx.set_window_appearance(Some(if ui.mode.is_dark() { WindowAppearance::Dark } else { WindowAppearance::Light }));

    let (colors, rem) = (cx.theme().colors, cx.theme().font_size);
    style::check_names(&over, &colors, &mut problems);
    let looks = style::resolve(&over, &colors, rem, dir, &mut problems);
    cx.set_global(looks);
    let mut specs = crate::template::builtin();
    specs.extend(theme.templates.clone());
    specs.extend(settings.templates.clone());
    let templates = crate::template::compile(&specs, &style::Props::new(&over, &colors, dir), &mut problems);
    templates.check_actions(cx.all_action_names(), &mut problems);
    cx.set_global(templates);
    cx.set_global(crate::dock::LayoutLock(settings.layout.locked));

    let t = &settings.terminal;
    cx.set_global(TerminalLook {
        font_family: t.font_family.clone().map_or_else(|| mono_font.clone(), Into::into),
        font_size: px(t.font_size.filter(|v| *v > 0.).unwrap_or(13.)),
        line_height: t.line_height.filter(|v| *v > 0.).unwrap_or(1.25),
        palette: Arc::new(palette),
        ctrl_v_paste: t.ctrl_v_paste.unwrap_or(true),
    });

    set_error((!problems.is_empty()).then(|| problems.join("; ")), cx);
    cx.refresh_windows();
}

fn set_error(error: Option<String>, cx: &mut App) {
    if let Some(e) = &error {
        log::warn!("settings: {e}");
    }
    cx.set_global(SettingsStatus { error: error.map(Into::into) });
    cx.refresh_windows();
}

/// "Tern Dark" and "Tern Light": the component's default themes with
/// Tern's terminal palettes.
fn builtin_themes(cx: &App) -> Vec<(SharedString, TernTheme)> {
    let registry = ThemeRegistry::global(cx);
    let named = |config: &Rc<ThemeConfig>, name: &str| {
        let mut c = (**config).clone();
        c.name = name.into();
        c.is_default = false;
        c
    };
    vec![
        (
            DEFAULT_THEME.into(),
            TernTheme {
                ui: named(registry.default_dark_theme(), DEFAULT_THEME),
                terminal: colors_of(&Palette::default()),
                ..Default::default()
            },
        ),
        (
            LIGHT_THEME.into(),
            TernTheme { ui: named(registry.default_light_theme(), LIGHT_THEME), terminal: light_terminal(), ..Default::default() },
        ),
    ]
}

/// A light palette (One Light's ANSI colors) on the light theme's white.
fn light_terminal() -> TerminalColors {
    let ansi = [
        0x383a42, 0xe45649, 0x50a14f, 0xc18401, 0x0184bc, 0xa626a4, 0x0997b3, 0xa0a1a7, //
        0x4f525e, 0xca1243, 0x3e953a, 0x986801, 0x4078f2, 0x9a3bb0, 0x0e7c8b, 0x202227,
    ]
    .map(|h| Rgb { r: (h >> 16) as u8, g: (h >> 8) as u8, b: h as u8 });
    let hex = |h: u32| Rgb { r: (h >> 16) as u8, g: (h >> 8) as u8, b: h as u8 };
    colors_of(&Palette::new(ansi, hex(0x383a42), hex(0xffffff), hex(0x526fff), hex(0xc9d7f5), hex(0x9a9ca5)))
}

/// Every color of `p`, as settings would name it.
fn colors_of(p: &Palette) -> TerminalColors {
    let h = |c: Rgb| Some(format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b));
    let a = p.ansi();
    TerminalColors {
        background: h(p.background),
        foreground: h(p.foreground),
        cursor: h(p.cursor),
        selection: h(p.selection),
        dim_foreground: h(p.dim_foreground),
        black: h(a[0]),
        red: h(a[1]),
        green: h(a[2]),
        yellow: h(a[3]),
        blue: h(a[4]),
        magenta: h(a[5]),
        cyan: h(a[6]),
        white: h(a[7]),
        bright_black: h(a[8]),
        bright_red: h(a[9]),
        bright_green: h(a[10]),
        bright_yellow: h(a[11]),
        bright_blue: h(a[12]),
        bright_magenta: h(a[13]),
        bright_cyan: h(a[14]),
        bright_white: h(a[15]),
    }
}

impl TerminalColors {
    /// The colors in palette order: the five special ones, then ANSI 0..16.
    fn slots(&self) -> [(&'static str, &Option<String>); 21] {
        [
            ("background", &self.background),
            ("foreground", &self.foreground),
            ("cursor", &self.cursor),
            ("selection", &self.selection),
            ("dim_foreground", &self.dim_foreground),
            ("black", &self.black),
            ("red", &self.red),
            ("green", &self.green),
            ("yellow", &self.yellow),
            ("blue", &self.blue),
            ("magenta", &self.magenta),
            ("cyan", &self.cyan),
            ("white", &self.white),
            ("bright_black", &self.bright_black),
            ("bright_red", &self.bright_red),
            ("bright_green", &self.bright_green),
            ("bright_yellow", &self.bright_yellow),
            ("bright_blue", &self.bright_blue),
            ("bright_magenta", &self.bright_magenta),
            ("bright_cyan", &self.bright_cyan),
            ("bright_white", &self.bright_white),
        ]
    }
}

/// The theme's terminal palette with the settings' colors over it. A theme
/// without one gets the dark or light palette on its own background and
/// text colors.
fn terminal_palette(theme: &TernTheme, over: &TerminalColors, problems: &mut Vec<String>) -> Palette {
    let base = if theme.ui.mode.is_dark() { colors_of(&Palette::default()) } else { light_terminal() };
    let derived = derive_from_ui(&theme.ui);
    let mut out: [Rgb; 21] = [Rgb { r: 0, g: 0, b: 0 }; 21];
    for (i, (((name, b), (_, d)), ((_, t), (_, o)))) in
        base.slots().iter().zip(derived.slots()).zip(theme.terminal.slots().iter().zip(over.slots())).enumerate()
    {
        let pick = [o, t, d, b].into_iter().flatten().find_map(|v| match parse_hex(v) {
            Some(c) => Some(c),
            None => {
                problems.push(format!("terminal color {name}: \"{v}\" is not #rrggbb"));
                None
            },
        });
        // `base` names every color, so only a broken built-in gets here.
        out[i] = pick.unwrap_or(Rgb { r: 0, g: 0, b: 0 });
    }
    let mut ansi = [out[5]; 16];
    ansi.copy_from_slice(&out[5..]);
    Palette::new(ansi, out[1], out[0], out[2], out[3], out[4])
}

/// A terminal's background and text from a UI theme's, for themes that
/// give no terminal colors.
fn derive_from_ui(ui: &ThemeConfig) -> TerminalColors {
    let hex = |v: &Option<SharedString>| {
        let c = try_parse_color(v.as_deref()?).ok()?.to_rgb();
        let b = |x: f32| (x * 255.).round() as u8;
        Some(format!("#{:02x}{:02x}{:02x}", b(c.r), b(c.g), b(c.b)))
    };
    TerminalColors { background: hex(&ui.colors.background), foreground: hex(&ui.colors.foreground), ..Default::default() }
}

/// `tokens` and `elements` as the style resolver takes them. A value that is
/// neither a number nor a string is reported and left out.
fn overrides<'a>(theme: &'a TernTheme, s: &'a SettingsFile, problems: &mut Vec<String>) -> Overrides<'a> {
    let mut over = Overrides::default();
    // The theme's first, so the settings' go over them.
    for (tokens, elements) in [(&theme.tokens, &theme.elements), (&s.tokens, &s.elements)] {
        for (name, v) in tokens {
            match style::raw(v) {
                Some(raw) => _ = over.tokens.insert(name.as_str(), raw),
                None => problems.push(format!("tokens.{name}: a value is a number or a string")),
            }
        }
        for (el, props) in elements {
            for (prop, v) in props {
                match style::raw(v) {
                    Some(raw) => _ = over.elements.insert((el.as_str(), prop.as_str()), raw),
                    None => problems.push(format!("elements.{el}.{prop}: a value is a number or a string")),
                }
            }
        }
    }
    over
}

/// The theme's UI config with the settings applied over it: colors, fonts,
/// corner radii. An unknown color key or an unreadable color is an error.
fn ui_config(theme: &ThemeConfig, s: &SettingsFile, over: &Overrides, problems: &mut Vec<String>) -> Result<ThemeConfig, String> {
    let mut value = serde_json::to_value(theme).map_err(|e| e.to_string())?;
    let known: HashSet<String> = match serde_json::to_value(ThemeConfigColors::default()) {
        Ok(serde_json::Value::Object(m)) => m.into_iter().map(|(k, _)| k).collect(),
        _ => HashSet::new(),
    };
    let colors = value.get_mut("colors").and_then(|c| c.as_object_mut()).ok_or("theme has no colors")?;
    for (key, color) in &s.colors {
        if !known.contains(key) {
            problems.push(format!("colors: no key \"{key}\""));
        } else if try_parse_color(color).is_err() {
            problems.push(format!("colors.{key}: can't read \"{color}\""));
        } else {
            colors.insert(key.clone(), color.clone().into());
        }
    }
    let mut config: ThemeConfig = serde_json::from_value(value).map_err(|e| e.to_string())?;
    if let Some(f) = &s.ui.font_family {
        config.font_family = Some(f.clone().into());
    }
    if let Some(size) = s.ui.font_size.filter(|v| *v > 0.) {
        config.font_size = Some(size);
    }
    if let Some(f) = &s.editor.font_family {
        config.mono_font_family = Some(f.clone().into());
    }
    if let Some(size) = s.editor.font_size.filter(|v| *v > 0.) {
        config.mono_font_size = Some(size);
    }
    let rem = px(config.font_size.unwrap_or(16.));
    if let Some(r) = style::token_px(over, "radius", rem).filter(|v| *v >= px(0.)) {
        config.radius = Some(f32::from(r).round() as usize);
    }
    if let Some(r) = style::token_px(over, "radius_lg", rem).filter(|v| *v >= px(0.)) {
        config.radius_lg = Some(f32::from(r).round() as usize);
    }
    Ok(config)
}

/// The themes in `dir/*.json`, each file a theme set:
/// `{"name": …, "themes": [{"name": …, "mode": "dark", "colors": {…}, "terminal": {…}}]}`.
fn user_themes(dir: &Path, problems: &mut Vec<String>) -> Vec<(SharedString, TernTheme)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return vec![] };
    let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
    files.sort();
    let mut out = vec![];
    for path in files {
        let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match read_theme_set(&path) {
            Ok(themes) => out.extend(themes),
            Err(e) => problems.push(format!("{THEMES}/{file}: {e}")),
        }
    }
    out
}

fn read_theme_set(path: &Path) -> Result<Vec<(SharedString, TernTheme)>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut set: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let themes = set.get_mut("themes").and_then(|t| t.as_array_mut()).ok_or("no \"themes\" list")?;
    let mut out = vec![];
    for theme in themes.iter_mut() {
        let terminal = match theme.as_object_mut().and_then(|t| t.remove("terminal")) {
            Some(t) => serde_json::from_value::<TerminalColors>(t).map_err(|e| format!("terminal: {e}"))?,
            None => TerminalColors::default(),
        };
        let mut take = |key: &str| theme.as_object_mut().and_then(|t| t.remove(key));
        let tokens = match take("tokens") {
            Some(t) => serde_json::from_value(t).map_err(|e| format!("tokens: {e}"))?,
            None => BTreeMap::new(),
        };
        let elements = match take("elements") {
            Some(e) => serde_json::from_value(e).map_err(|e| format!("elements: {e}"))?,
            None => BTreeMap::new(),
        };
        let templates = match take("templates") {
            Some(t) => serde_json::from_value(t).map_err(|e| format!("templates: {e}"))?,
            None => BTreeMap::new(),
        };
        let ui: ThemeConfig = serde_json::from_value(theme.take()).map_err(|e| e.to_string())?;
        out.push((ui.name.clone(), TernTheme { ui, terminal, tokens, elements, templates }));
    }
    Ok(out)
}

/// The schema, the README, a `settings.json` to start from when there is
/// none, and the built-in themes in `reference/`.
fn write_reference_files(dir: &Path, cx: &App) -> std::io::Result<()> {
    std::fs::create_dir_all(dir.join(THEMES))?;
    std::fs::create_dir_all(dir.join(REFERENCE))?;
    let mut schema = serde_json::to_value(schemars::schema_for!(SettingsFile)).unwrap_or_default();
    if let Some(props) = schema.get_mut("properties").and_then(|p| p.as_object_mut()) {
        let (tokens, elements) = style_schema();
        props.insert("tokens".into(), tokens);
        props.insert("elements".into(), elements);
    }
    write_if_changed(&dir.join(SCHEMA), &serde_json::to_string_pretty(&schema).unwrap_or_default())?;
    write_if_changed(&dir.join(REFERENCE).join(ELEMENTS_DOC), &elements_doc())?;
    write_if_changed(&dir.join(REFERENCE).join("templates.json"), crate::template::builtin_text())?;
    write_if_changed(&dir.join(REFERENCE).join("actions.md"), &actions_doc(cx))?;
    write_if_changed(&dir.join(README), &readme(dir))?;
    let settings = dir.join(SETTINGS);
    if !settings.exists() {
        std::fs::write(&settings, format!("{{\n  \"$schema\": \"./{SCHEMA}\",\n  \"theme\": \"{DEFAULT_THEME}\"\n}}\n"))?;
    }
    for (name, theme) in builtin_themes(cx) {
        let mut ui = serde_json::to_value(&theme.ui).unwrap_or_default();
        if let Some(obj) = ui.as_object_mut() {
            obj.insert("name".into(), format!("My {name}").into());
            obj.insert("terminal".into(), terminal_json(&theme.terminal));
        }
        let set = serde_json::json!({ "name": format!("My {name}"), "themes": [ui] });
        let file = format!("{}.json", name.to_lowercase().replace(' ', "-"));
        write_if_changed(&dir.join(REFERENCE).join(file), &serde_json::to_string_pretty(&set).unwrap_or_default())?;
    }
    Ok(())
}

/// Every action a template's `action` may name.
fn actions_doc(cx: &App) -> String {
    let mut names: Vec<&str> = cx.all_action_names().to_vec();
    names.sort_unstable();
    let mut out = String::from(
        "# Actions\n\nWhat a menu entry's or a button's `\"action\"` may name in `templates` (see `README.md`). \
         Actions that take values (Open Recent) can't be named.\n\n",
    );
    for n in names {
        out.push_str(&format!("- `{n}`\n"));
    }
    out
}

/// The schema of `tokens` and `elements`: every built-in token, element
/// and property with its default, so an editor completes and checks them.
fn style_schema() -> (serde_json::Value, serde_json::Value) {
    use serde_json::json;
    let any = json!(["number", "string"]);
    let tokens: serde_json::Map<String, serde_json::Value> = style::TOKENS
        .iter()
        .map(|(name, default, about)| (name.to_string(), json!({ "description": format!("{about}. Default: {default}"), "type": any })))
        .collect();
    let elements: serde_json::Map<String, serde_json::Value> = style::ELEMENTS
        .iter()
        .map(|e| {
            let props: serde_json::Map<String, serde_json::Value> = e
                .props()
                .map(|(name, kind, default)| {
                    let shown = if default.is_empty() { "none" } else { default };
                    let what = match kind {
                        style::Kind::Len => "A size: pixels, \"Npx\" or \"Nrem\"",
                        style::Kind::Color => "A color",
                        style::Kind::Num => "A number",
                        style::Kind::Text => "Text",
                    };
                    let ty = if *kind == style::Kind::Color || *kind == style::Kind::Text { json!("string") } else { any.clone() };
                    let gui = if style::GUI_ONLY.contains(name) { "GUI only: " } else { "" };
                    (name.to_string(), json!({ "description": format!("{gui}{what}, or \"$name\". Default: {shown}"), "type": ty }))
                })
                .collect();
            (e.name.to_string(), json!({ "description": e.about, "type": "object", "properties": props, "additionalProperties": false }))
        })
        .collect();
    (
        json!({
            "description": "Shared values that elements point at with \"$name\": the built-in ones below or new ones of your own.",
            "type": "object",
            "properties": tokens,
            "additionalProperties": any,
        }),
        json!({
            "description": "Each element's own properties over its defaults. See reference/elements.md.",
            "type": "object",
            "properties": elements,
            "additionalProperties": false,
        }),
    )
}

/// `reference/elements.md`: the tokens and every element's properties with
/// their defaults.
fn elements_doc() -> String {
    let mut out = String::from(
        "# Tokens and elements\n\n\
         Tern rewrites this file on every start. Set tokens and element properties in\n\
         `settings.json` (`tokens`, `elements`); see `../README.md`.\n\n## Tokens\n\n\
         | Token | Default | Used for |\n|---|---|---|\n",
    );
    for (name, default, about) in style::TOKENS {
        out += &format!("| `{name}` | `{default}` | {about} |\n");
    }
    out += "\n## Elements\n";
    for e in style::ELEMENTS {
        out += &format!("\n### `{}`\n\n{}.\n\n| Property | Default |\n|---|---|\n", e.name, e.about);
        for (name, _, default) in e.props() {
            let shown = if default.is_empty() { "none".to_owned() } else { format!("`{default}`") };
            let gui = if style::GUI_ONLY.contains(name) { " (GUI only)" } else { "" };
            out += &format!("| `{name}` | {shown}{gui} |\n");
        }
    }
    out
}

fn terminal_json(t: &TerminalColors) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    for (name, v) in t.slots() {
        if let Some(v) = v {
            m.insert(name.into(), v.clone().into());
        }
    }
    serde_json::Value::Object(m)
}

fn write_if_changed(path: &Path, text: &str) -> std::io::Result<()> {
    if std::fs::read_to_string(path).is_ok_and(|old| old == text) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)
}

fn readme(dir: &Path) -> String {
    format!(
        "# Tern settings

This folder ({dir}) configures Tern. Saving a file here applies it at once;
if it can't be read, Tern keeps what it applied last and shows why in the
status bar. Tern rewrites `README.md`, `{SCHEMA}` and `reference/` on every
start: edit `{SETTINGS}` and `themes/` only.

## {SETTINGS}

Every key is optional. `{SCHEMA}` lists them all with their meaning.

```json
{{
  \"$schema\": \"./{SCHEMA}\",
  \"theme\": \"{DEFAULT_THEME}\",
  \"ui\": {{ \"font_size\": 15 }},
  \"editor\": {{ \"font_family\": \"JetBrains Mono\", \"font_size\": 13 }},
  \"terminal\": {{
    \"font_family\": \"JetBrains Mono\",
    \"font_size\": 13,
    \"line_height\": 1.3,
    \"colors\": {{ \"background\": \"#101010\", \"red\": \"#ff6b6b\" }}
  }},
  \"tokens\": {{ \"row_height\": 24, \"radius\": 4 }},
  \"elements\": {{
    \"explorer.row\": {{ \"height\": 26, \"text\": \"#e0e0e0\" }},
    \"explorer\": {{ \"background_image\": \"images/sky.jpg\", \"background_image_opacity\": 0.2 }}
  }},
  \"colors\": {{ \"background\": \"#0d0d0d\", \"border\": \"#262626\" }}
}}
```

- `theme`: \"{DEFAULT_THEME}\", \"{LIGHT_THEME}\", or the `name` of a theme in `themes/`.
  Tern's theme picker (Cmd-K Cmd-T, Ctrl-Shift-K Ctrl-Shift-T on Windows and
  Linux) writes this key and leaves the rest of the file as it is.
- `colors`: UI colors over the theme's, by theme key. The keys are the
  properties of `colors` in `{SCHEMA}`; the common ones: `background` (panels,
  editor, terminal area), `foreground`, `muted.foreground` (secondary text),
  `border`, `title_bar.background` (window chrome, tool strips, status bar),
  `tab_bar.background`, `tab.active.background`, `sidebar.background`,
  `list.hover.background`, `list.active.background`, `primary.background`,
  `success.background` / `warning.background` / `danger.background` (git
  added / modified / deleted).
- `tokens` and `elements`: see \"Elements\" below.
- `terminal.colors`: `background`, `foreground`, `cursor`, `selection`,
  `dim_foreground` and the 16 ANSI colors (`black` … `white`,
  `bright_black` … `bright_white`), each `#rrggbb`.

## Elements

Every part of the window Tern draws is an element with its own properties:
`explorer.row` has `height`, `padding_left`, `font_size`, `text`,
`hover_background`, …; `workspace.status_bar` has `height`, `padding_x`, `background`,
…. `reference/{ELEMENTS_DOC}` lists them all with their defaults.

Out of the box most properties point at a shared token, so the window stays
consistent: `explorer.row`, `changes.row` and `scratches.row` all have
`\"height\": \"$row_height\"`. So there are two ways to change things:

- Change a token, and every element that points at it follows:
  `\"tokens\": {{ \"row_height\": 24 }}`.
- Give one element its own value, and only it changes:
  `\"elements\": {{ \"changes.row\": {{ \"height\": 28 }} }}`.

To keep some elements alike, make a token of your own and point them at it:

```json
\"tokens\": {{ \"list_text\": \"#d0d0d0\" }},
\"elements\": {{
  \"explorer.row\": {{ \"text\": \"$list_text\" }},
  \"scratches.row\": {{ \"text\": \"$list_text\" }}
}}
```

Every element also takes the full set of box properties, unset until you
give them: `visible` (0 hides it), `width`, `height`, `min_*` / `max_*`,
`padding` (and `padding_x`, `padding_y`, `padding_top` … by side), `margin`
(same), `gap`, `radius` (and `radius_top_left` … by corner), `border_width`
(and `border_top_width` … by side), `border` (its color), `background`,
`text`, `font_family`, `font_size`, `font_weight`, `italic`, `line_height`,
`opacity`, `shadow` (`sm`, `md`, `lg`, `xl`, `none`) and `text_transform`
(`uppercase`, `lowercase`, `capitalize`, `none`, for the labels Tern writes).
Panel headers have `title` to replace their text. `pane` has `focused_border`;
`pane.title` (`visible: 1`) puts the tab's name on the pane's top edge.

A theme in `themes/` may carry `tokens` and `elements` too, so a theme can
change sizes and shapes, not only colors; `{SETTINGS}`'s go over the theme's.

A value is a number (pixels), `\"Npx\"`, `\"Nrem\"` (scales with
`ui.font_size`), a color (`#rrggbb`, `#rrggbbaa`, `blue-500/50`,
`transparent`), text, or `\"$name\"`: a token, another element's property
(`\"$explorer.row.height\"`), or a theme color by its key
(`\"$muted.foreground\"`, `\"$tab_bar.background\"`). A color `\"$name/NN\"` is
that color at NN% of its opacity (`\"$primary/15\"`).

## Templates

Templates and elements describe structure and look for any front end. The
properties only a GUI can draw (`radius`, `shadow`, `opacity`, background
images) are marked \"GUI only\" in `{SCHEMA}` and `reference/{ELEMENTS_DOC}`;
other front ends ignore them.

Where things go is in `templates` (in `{SETTINGS}` or a theme): the window's
parts as trees of boxes. `reference/templates.json` holds the built-in ones;
copy one into `templates` under its name and change it. A box takes an element's
properties (`\"element\"`) and its own (`\"style\"`), lines up its `children`
(`\"layout\"`: `column`, `row`, `wrap`; `align`, `justify`, `grow`, `fill`), can
sit over its parent (`\"position\": {{\"top\": 0, \"right\": 8}}`, `z`,
`overlay`), show only when a condition holds (`\"when\": \"focused && tabs > 1\"`),
change while one does (`\"states\": {{\"focused\": {{\"border\": \"$accent\"}}}}`),
or split into panels you resize (`\"split\": \"row\"`, with `size`, `min`, `max`,
`fixed` on the panels). Leaves are text (`\"text\": \"{{title}}\"`), another
template (`\"use\"`), an icon (`\"icon\": \"folder-open\"`, a bundled Lucide
name), a button (`\"button\"`, with `icon`, `label`, `tooltip`, `variant`, `size`,
`selected`, `disabled`, `loading`), or a piece Tern draws (`\"slot\"`). A list
the code gives is drawn with `\"each\"` and `\"item\"`; the `children` of an
`each` come after its entries.

| Template | Values | Slots |
|---|---|---|
| `window` | `workspace`, `menu_bar` | `menu_bar`, `welcome`, `workspace_tabs`, `workspace` |
| `workspace` | `explorer`, `scratches`, `changes`, `path`, `background`, `settings_error` | `left_strip`, `right_strip`, `explorer`, `scratches`, `center`, `changes` |
| `status_bar` | as `workspace` | as `workspace` |
| `pane` | `focused`, `focus_line`, `vertical`, `title`, `tabs` | `tab_bar`, `vertical_tabs`, `content`, `drop_overlay` |
| `pane.body` | as `pane` | as `pane` |
| `panel_header` | `part`, `title`, `count` | `buttons` |
| `explorer.header`, `changes.header`, `scratches.header` | as `panel_header` | as `panel_header` |
| `explorer.header.buttons`, `changes.header.buttons`, `scratches.header.buttons` | `hide_key`; `refreshing` (changes) | |
| `explorer.row` | `indent`, `folder`, `open`, `selected`, `cursor`, `cut`, `editing`, `name`, `name_color`, `claude`, `git` | `icon`, `field` |
| `changes.repo` | `indent`, `folder`, `open`, `name`, `branch`, `detail`, `error` | |
| `changes.file` | `indent`, `selected`, `name`, `name_color`, `dir`, `status` | `icon` |
| `scratches.row` | `indent`, `selected`, `name`, `age` | `icon` |
| `file_header` | `name`, `dirty`, `markdown`, `image`, `preview`, `diff`, `preview_key`, `open_external_key` | |
| `terminal` | `background` (the palette's, with `terminal.background_opacity`) | `grid` |
| `viewer` | as `file_header`; `message` | `diff`, `image`, `preview`, `editor` (the one it shows) |
| `diff_pane` | `message` | `diff` |
| `scratch` | `message` | `editor` |
| `explorer` | | `header`, `list` |
| `changes`, `changes.empty` | `empty`, `refreshing` | `header`, `list` |
| `scratches`, `scratches.empty` | `empty`, `new_key` | `header`, `list` |
| `tool_strip`, `tool_strip.left`, `tool_strip.right` | list `parts` (`part`, `icon`, `on`, `tooltip`) | |
| `agent_monitor` | `summary`, `agents`; list `rows` | `fit` |
| `agent_monitor.workspace` | `workspace`, `name`, `dot`, `dot_color` | |
| `agent_monitor.agent` | `name`, `idle`, `status`, `status_color`, `dot_color` | `icon` |
| `drag_chip` | `label`, `icon` | |
| `drag_group` | `text`, `group_color` | |
| `pane_tab_bar` | `wrap`, `overflows`, `panes`, `zoomed`, `corner`; list `items` | `scrollbar` |
| `pane_tab_bar.tab`, `vertical_tabs.tab` | `label`, `active`, `preview`, `grouped`, `badge`, `group_color` | `icon` |
| `vertical_tabs` | as `pane_tab_bar` | |
| `workspace_tabs` | `monitor`, `update`, `installing`; list `items` | |
| `workspace_tabs.tab` | `name`, `active`, `busy` | |
| `welcome` | `recents`, `open_key`, `new_window_key`, `close_key`, `switch_key`; list `recent` | |
| `welcome.recent` | `name`, `path` | |
| `settings` | `title`, `subtitle`; lists `sections`, `rows` | `extra` |
| `settings.nav_item` | `label`, `icon`, `active` | |
| `settings.row` | `title`, `description`, `problem`, `value`, `default`, `switch`, `on`, `stepper`, `changed`, `at_min`, `at_max`, `action`, `busy` | |
| `settings.remote` | `link`, `dns_link`, `copy_label`, `copy_icon`, `busy` | `qr` |
| `dialog.trash` | `question` | |
| `dialog.restart` | `message` | |
| `dialog.restart.footer` | `at_launch` | |
| `dialog.delete_scratch` | `name` | |
| `dialog.diff_preview` | `height` | `diff` |
| `dialog.name`, `dialog.claude_args` | | `field` |
| `dialog.theme` | | `list` |
| `menu.explorer` | `on_entry`, `can_paste`, `reveal` | |
| `menu.scratch` | `reveal` | |
| `menu.new_scratch` | list `kinds` (`label`, `ext`) | |
| `menu.workspace_tab` | `single`, `reveal`; list `windows` (`label`) | |
| `menu.pane_layout` | `panes`; list `presets` (`label`, `on`) | |
| `menu.tab_layout` | list `layouts` (`label`, `on`) | |
| `menu.new_tab` | as `menu.pane_layout`, `menu.tab_layout`, `menu.new_scratch`; `scratch_count`; lists `flags` (`label`, `flag`, `on`), `scratch_files` (`name`) | |
| `menu.tab` | `others`, `right`, `file_tabs`, `grouped`, `renamable`, `diff_file`, `has_path`, `reveal`; lists `groups` (`title`, `color`), `layouts` | |
| `menu.tab_group` | `collapsed`, `all`; list `colors` (`label`, `color`, `on`) | |
| `menu.tab_bar` | as `menu.pane_layout` and `menu.tab_layout` | |
| `menu.tabs` | list `tabs` (`label`, `on`) | |

`\"element\"` on a `\"use\"` gives the used template's root that element, and
`$element` inside it names it (`\"$element.count_gap\"`): the three panel
headers are `panel_header` used with their own element (see Moving panels).

The side panels draw each row they show with a row template (`explorer.row`,
`changes.repo`, `changes.file`, `scratches.row`). A row's `indent` is its
element's `padding_left` plus its `indent` for each level it is nested; its
`name_color` is the git status color (unset in the explorer for a file git
doesn't report, so the row's text color shows). A header's buttons are the
`<header>.buttons` template, put in the header's `buttons` slot.

A dialog's message is template `dialog.<name>` and its buttons
`dialog.<name>.footer` (its title and frame are Tern's). A `\"wrap\": true` box
wraps its text at its width. A menu is template `menu.<name>`; its root's
children are the entries: `{{\"separator\": true}}`, `{{\"action\":
\"file_tree::NewFile\", \"label\": \"New File…\"}}` (runs that action and shows
its key), `{{\"button\": \"close\", \"label\": \"Close\"}}` (Tern attaches what it
does by name), both with `icon`, `selected` (a check) and `disabled`; `\"text\"`
(a line that can't be clicked), a box with a `label` and `children` (a
submenu), `each` and `use` (another menu's entries). A `button` with
`children` draws them as its item. A `button` with an `action` runs it when
clicked, in any template.

A slot you leave out is not drawn. Mistakes show in the status bar.

### Moving panels

The explorer, scratches and changes panels sit in docks and can be dragged
between them by their headers. In a template:

- `\"part\": \"explorer\"` draws a movable view where it sits by default
  (`explorer`, `scratches`, `changes`). Give it `\"drag\": false` (or a
  condition on the values its `when` reads) to pin it.
- `\"dock\": \"left\"` with a `split` is an area that holds parts; its
  children are the parts it starts with. `accepts` lists the part names or
  kinds it takes (`[\"panel\"]`; none listed: nothing can be dropped in).
  `zones` (`before`, `after`) say where a dropped part goes, next to the parts
  there. `empty` is `hide` (an empty dock takes no room and shows a thin drop
  strip only while a part it takes is dragged) or `keep`. `strip` (`left` or
  `right`) is the tool strip whose buttons show this dock's parts; a dock
  needs one. Its children are `part`s only.
  `part_size` is the size a part gets when it arrives from another dock.
- `\"handle\": true` on a box inside a part makes it the grip: drop the part
  on a half of a panel in a dock to put it before or after that panel. The
  built-in headers use `panel_header`. Dragging shows each zone as the `drop_zone`
  element (`drop_zone.hover` under the pointer). Colors take `$name/NN`: the
  color at NN% of its opacity.
- `\"layout\": {{\"locked\": true}}` in `{SETTINGS}` turns off moving and
  resizing the panels in docks. Pane splits and tab drags aren't locked yet:
  they will be once the panes are a dock too.

Where the panels are is kept per workspace and per template (the `workspace`
template and the names of its docks), so another theme with other docks has its
own arrangement and switching back restores the first. Only an arrangement you
changed is kept: until then the template's defaults and start sizes apply, and
a template you edit changes the layout. A panel a template no longer places is
put back in the first dock that takes it, with a note in the status bar.
**View > Reset Layout** drops the arrangement of the current template and goes
back to its defaults.

A `workspace` template copied from an older build that has `\"use\":
\"workspace.left\"` must be updated: that template is gone, the docks replaced
it. Copy the new `workspace` from `reference/templates.json`.

### Background images

`window`, `workspace`, `explorer`, `changes`, `scratches`, `vertical_tabs`,
`welcome` and `terminal` can show an image behind their content (so can any
box of a template, through its `style`): `background_image` (a path:
absolute, `~/…`, or relative to this folder), `background_image_fit` (`cover`,
`contain`, `fill`, `scale_down`, `none`) and `background_image_opacity` (0 to
1). An image shows only where what's in front of it is see-through: to see a
`workspace` image through the explorer, set the explorer's `background` to
`transparent` or a color with alpha (`#00000080`); in a terminal, set
`terminal.background_opacity` below 1.

### Light and dark in terminal programs

Terminal programs learn the theme's background the ways terminals tell them:
`COLORFGBG` when they start, OSC 10 / 11 / 4 color queries, `CSI ? 996 n`, and
`CSI ? 2031 h` reports when the theme turns light or dark. So programs with an
automatic theme follow Tern's: Claude Code with `/theme` set to auto, OpenCode
with its `system` theme. A program set to a fixed dark theme stays dark.

## Themes

A theme file in `themes/` holds one or more themes:

```json
{{
  \"name\": \"My Themes\",
  \"themes\": [
    {{
      \"name\": \"Midnight\",
      \"mode\": \"dark\",
      \"colors\": {{ \"background\": \"#0b1020\", \"foreground\": \"#d6deeb\" }},
      \"terminal\": {{ \"background\": \"#0b1020\", \"foreground\": \"#d6deeb\" }}
    }}
  ]
}}
```

Then set `\"theme\": \"Midnight\"` in `{SETTINGS}`. A color a theme leaves out
takes the default of its `mode`. `reference/` has the built-in themes in full:
copy one into `themes/`, rename it, and change what you like. The UI part is
the theme format of gpui-component, so its themes work here too.
",
        dir = dir.display()
    )
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::{ThemeConfig, ThemeMode};
    use tern_term::palette::DARK_ANSI;
    use tern_term::vt::vte::ansi::Rgb;

    // Not `super::*`: GPUI's glob brings its own `test` macro.
    use super::{SettingsFile, TerminalColors, TernTheme, elements_doc, is_app_data, read_theme_set, terminal_palette, with_value};

    /// The session and window places live next to the settings on Windows:
    /// saving them (every few seconds while a tab's status changes) must not
    /// reload the theme, which repaints the terminals.
    #[test]
    fn own_data_files_dont_reload() {
        let dir = std::path::Path::new("/cfg/Tern");
        for own in ["session", "window", "recent", "claude-args", ".session.tern-42.tmp", "scratches/a.md"] {
            assert!(is_app_data(dir, &dir.join(own)), "{own}");
        }
        for setting in ["settings.json", "themes/x.json", "bg.png", "sessions.json"] {
            assert!(!is_app_data(dir, &dir.join(setting)), "{setting}");
        }
        assert!(!is_app_data(dir, std::path::Path::new("/elsewhere/session")));
    }

    #[test]
    fn gui_only_props_are_marked() {
        let doc = elements_doc();
        assert!(doc.contains("`shadow`") && doc.contains("GUI only"));
        for p in crate::style::GUI_ONLY {
            assert!(crate::style::ELEMENTS.iter().any(|e| e.prop(p).is_some()), "{p} is not a property");
        }
    }

    #[test]
    fn choosing_a_theme_keeps_the_rest_of_the_file() {
        let old = r#"{"$schema": "./settings.schema.json", "theme": "Tern Dark", "ui": {"font_size": 15}, "colors": {}}"#;
        let new = with_value(Some(old), &["theme"], Some("Tern Light".into())).unwrap();
        let v: serde_json::Value = serde_json::from_str(&new).unwrap();
        assert_eq!(v["theme"], "Tern Light");
        assert_eq!(v["ui"]["font_size"], 15);
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["$schema", "theme", "ui", "colors"]);

        let fresh: serde_json::Value = serde_json::from_str(&with_value(None, &["theme"], Some("X".into())).unwrap()).unwrap();
        assert_eq!(fresh["theme"], "X");
        assert!(with_value(Some("[1]"), &["theme"], Some("X".into())).is_err());
        assert!(with_value(Some("{oops"), &["theme"], Some("X".into())).is_err());
    }

    #[test]
    fn setting_a_nested_value_makes_and_keeps_its_objects() {
        let old = r#"{"theme": "Tern Dark", "terminal": {"font_family": "Menlo"}}"#;
        let new = with_value(Some(old), &["terminal", "font_size"], Some(14.into())).unwrap();
        let v: serde_json::Value = serde_json::from_str(&new).unwrap();
        assert_eq!(v["terminal"]["font_size"], 14);
        assert_eq!(v["terminal"]["font_family"], "Menlo");
        let new = with_value(Some(&new), &["ui", "font_size"], Some(15.into())).unwrap();
        let v: serde_json::Value = serde_json::from_str(&new).unwrap();
        assert_eq!(v["ui"]["font_size"], 15);
        let new = with_value(Some(&new), &["terminal", "font_family"], None).unwrap();
        let v: serde_json::Value = serde_json::from_str(&new).unwrap();
        assert!(v["terminal"].get("font_family").is_none());
        // Back to the default leaves no empty object behind.
        let new = with_value(Some(&new), &["ui", "font_size"], None).unwrap();
        let v: serde_json::Value = serde_json::from_str(&new).unwrap();
        assert!(v.get("ui").is_none());
        assert!(with_value(Some(&new), &["editor", "font_size"], None).is_ok());
        assert!(with_value(Some(r#"{"ui": 3}"#), &["ui", "font_size"], Some(1.into())).is_err());
    }

    #[test]
    fn settings_reject_unknown_keys() {
        assert!(serde_json::from_str::<SettingsFile>(r#"{"theme": "x", "terminal": {"font_size": 12}}"#).is_ok());
        assert!(serde_json::from_str::<SettingsFile>(r#"{"them": "x"}"#).is_err());
        assert!(serde_json::from_str::<SettingsFile>(r#"{"terminal": {"colours": {}}}"#).is_err());
    }

    #[test]
    fn terminal_colors_layer() {
        let theme = TernTheme {
            ui: ThemeConfig { mode: ThemeMode::Dark, ..Default::default() },
            terminal: TerminalColors { red: Some("#110000".into()), ..Default::default() },
            ..Default::default()
        };
        let over = TerminalColors { background: Some("#000001".into()), green: Some("bad".into()), ..Default::default() };
        let mut problems = vec![];
        let p = terminal_palette(&theme, &over, &mut problems);
        assert_eq!(p.background, Rgb { r: 0, g: 0, b: 1 });
        assert_eq!(p.indexed[1], Rgb { r: 0x11, g: 0, b: 0 });
        assert_eq!(p.indexed[2], DARK_ANSI[2]);
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn theme_sets_carry_terminal_colors() {
        let dir = std::env::temp_dir().join(format!("tern-themes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.json");
        std::fs::write(
            &path,
            r##"{"name": "S", "themes": [{"name": "Night", "mode": "dark", "colors": {"background": "#000000"}, "terminal": {"red": "#ff0000"}, "tokens": {"row_height": 26}, "elements": {"pane": {"border_width": 1}}}]}"##,
        )
        .unwrap();
        let themes = read_theme_set(&path).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(themes.len(), 1);
        assert_eq!(themes[0].0.as_ref(), "Night");
        assert_eq!(themes[0].1.terminal.red.as_deref(), Some("#ff0000"));
        assert_eq!(themes[0].1.tokens["row_height"], 26);
        assert_eq!(themes[0].1.elements["pane"]["border_width"], 1);
    }
}
