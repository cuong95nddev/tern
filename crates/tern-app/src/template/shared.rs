//! What every template can use, wherever it is drawn: the values and the
//! buttons that belong to the app, not to one view. A view's own values go
//! over these, so a template can place these parts in any bar or panel. The
//! main window publishes them as it draws, since some can only be read with
//! a `&mut App`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use gpui_kit::component::button::Button;
use gpui_kit::component::menu::DropdownMenu as _;
use gpui_kit::*;

use super::Value;
use crate::theme_picker::OpenSettings;
use crate::window_state;
use crate::workspace::{ToggleEditLayout, Workspace};

/// The shared values, by name.
pub type Values = HashMap<&'static str, Value>;

#[derive(Default)]
struct Shared {
    values: Arc<Values>,
    /// The workspace of the window being drawn, for the buttons that open its menus.
    ws: Option<WeakEntity<Workspace>>,
}

impl Global for Shared {}

/// What the `texts` settings put in place of what Tern writes.
#[derive(Default)]
struct Texts(BTreeMap<String, String>);

impl Global for Texts {}

pub fn set_texts(texts: BTreeMap<String, String>, cx: &mut App) {
    cx.set_global(Texts(texts));
}

/// `text`, or what the `texts` settings say instead of it.
pub fn text(text: String, cx: &App) -> String {
    cx.try_global::<Texts>().and_then(|t| t.0.get(&text).cloned()).unwrap_or(text)
}

/// Tells the windows apart: the workspace of the window being drawn.
pub(super) fn window_key(cx: &App) -> u64 {
    cx.try_global::<Shared>().and_then(|s| s.ws.as_ref()).map_or(0, |w| w.entity_id().as_u64())
}

/// The values last published.
pub(super) fn values(cx: &App) -> Arc<Values> {
    cx.try_global::<Shared>().map(|s| s.values.clone()).unwrap_or_default()
}

/// Publishes the app's values with `parts`, those of the window being drawn,
/// which shows `ws`.
pub fn publish(parts: impl IntoIterator<Item = (&'static str, Value)>, ws: Option<WeakEntity<Workspace>>, cx: &mut App) {
    let (caffeine_icon, caffeine_tooltip, caffeine_on) = crate::caffeine::button(cx);
    let shortcut = |k| crate::platform::shortcut(k).to_string();
    let mut v = Values::from([
        ("caffeine_icon", caffeine_icon.into()),
        ("caffeine_tooltip", caffeine_tooltip.into()),
        ("caffeine_on", caffeine_on.into()),
        ("settings_key", shortcut("⌘,").into()),
        ("open_key", shortcut("⌘O").into()),
        ("monitor", crate::agent_monitor::window(cx).is_some().into()),
        ("title_bar_border", window_state::title_bar_border().into()),
    ]);
    if let Some((release, installing)) = crate::update::offered(cx) {
        let label = if installing { "Installing update…".to_owned() } else { format!("Update to {}", release.version) };
        v.extend([("update", label.into()), ("installing", installing.into())]);
    }
    v.extend(parts);
    cx.set_global(Shared { values: Arc::new(v), ws });
}

/// The bounds of a dock, by its name.
type Docks = HashMap<SharedString, Bounds<Pixels>>;

/// Where each window's docks were laid out: in its last frame (`seen`), and
/// as its templates were last given them (`shown`).
#[derive(Default)]
struct DockBounds(HashMap<WindowId, (Docks, Docks)>);

impl Global for DockBounds {}

/// Notes where dock `name` was laid out in `window` this frame.
pub(super) fn dock_laid_out(name: SharedString, bounds: Bounds<Pixels>, window: &Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    cx.default_global::<DockBounds>().0.entry(id).or_default().0.insert(name, bounds);
}

/// The values that place `window`'s docks as its last frame laid them out:
/// `dock_<name>_left`, `_right`, `_top`, `_bottom`, `_width` and `_height`,
/// in pixels from the window's top left corner. A dock not drawn has none.
pub fn dock_values(window: &Window, cx: &mut App) -> Vec<(&'static str, Value)> {
    let id = window.window_handle().window_id();
    let (seen, shown) = cx.default_global::<DockBounds>().0.entry(id).or_default();
    *shown = std::mem::take(seen);
    let mut out = vec![];
    for (name, b) in shown.iter() {
        for (side, v) in [
            ("left", b.left()),
            ("right", b.right()),
            ("top", b.top()),
            ("bottom", b.bottom()),
            ("width", b.size.width),
            ("height", b.size.height),
        ] {
            out.push((intern(format!("dock_{name}_{side}")), f64::from(f32::from(v)).into()));
        }
    }
    out
}

/// Once `window` is laid out: draws it again when its docks are not where
/// its templates were told they are (one moved, came or went).
pub fn check_docks(window: &mut Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    let moved = cx.try_global::<DockBounds>().and_then(|d| d.0.get(&id)).is_some_and(|(seen, shown)| seen != shown);
    if moved {
        window.refresh();
    }
}

/// Forgets the docks of a window that closed.
pub fn forget_docks(id: WindowId, cx: &mut App) {
    if cx.has_global::<DockBounds>() {
        cx.global_mut::<DockBounds>().0.remove(&id);
    }
}

/// `name` as a `&'static str`, leaked once: the value names of the docks the
/// templates have, a handful.
fn intern(name: String) -> &'static str {
    static NAMES: std::sync::Mutex<BTreeSet<&'static str>> = std::sync::Mutex::new(BTreeSet::new());
    let mut names = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(n) = names.get(name.as_str()) {
        return n;
    }
    let n: &'static str = Box::leak(name.into_boxed_str());
    names.insert(n);
    n
}

/// Attaches what shared button `name` does, or gives `b` back when it is
/// none of them.
#[allow(clippy::result_large_err)] // `Button` is one of gpui's builders, which move by value.
pub(super) fn button(name: &str, b: Button, cx: &App) -> Result<AnyElement, Button> {
    let ws = || cx.try_global::<Shared>().and_then(|s| s.ws.clone());
    Ok(match name {
        "new_tab_menu" => match ws() {
            Some(ws) => Workspace::new_tab_menu(b, ws).into_any_element(),
            None => b.into_any_element(),
        },
        "pane_layout_menu" => match ws() {
            Some(ws) => Workspace::pane_layout_menu(b, ws).into_any_element(),
            None => b.into_any_element(),
        },
        "window_minimize" => window_control(b, WindowControlArea::Min, |w| w.minimize_window()),
        "window_maximize" => window_control(b, WindowControlArea::Max, |w| w.zoom_window()),
        "window_close" => window_control(b, WindowControlArea::Close, |w| w.remove_window()),
        "caffeine" => b.dropdown_menu(crate::caffeine::menu).into_any_element(),
        "settings" => b.on_click(|_, window, cx| window.dispatch_action(Box::new(OpenSettings), cx)).into_any_element(),
        "agent_monitor" => b.on_click(|_, _, cx| crate::agent_monitor::toggle(cx)).into_any_element(),
        "edit_done" => b.on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleEditLayout), cx)).into_any_element(),
        "title_bar_border" => b
            .on_click(|_, _, cx| {
                window_state::set_title_bar_border(!window_state::title_bar_border());
                cx.refresh_windows();
            })
            .into_any_element(),
        "update" => b
            .on_click(|_, _, cx| {
                if let Some((release, false)) = crate::update::offered(cx) {
                    crate::update::offer(release, cx)
                }
            })
            .into_any_element(),
        _ => return Err(b),
    })
}

/// A window button: on Windows the system's (it hovers, snaps and asks before
/// closing as the caption's do), elsewhere `act`.
fn window_control(b: Button, area: WindowControlArea, act: fn(&mut Window)) -> AnyElement {
    if cfg!(windows) {
        div().id(SharedString::from(format!("window-control-{area:?}"))).window_control_area(area).child(b).into_any_element()
    } else {
        b.on_click(move |_, window, _| act(window)).into_any_element()
    }
}
