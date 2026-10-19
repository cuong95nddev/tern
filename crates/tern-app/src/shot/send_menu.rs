//! The tray's send menu: the folders, and the tabs of the one pointed at,
//! where captures can go. A window of its own above (or under) the tray:
//! the tray's window is too small to hold it.

use std::time::{Duration, Instant};

use gpui_kit::*;

use super::targets::{self, Choice, Dest};
use crate::style;
use crate::template::{self, Framed, Scope, on_event};

/// The gap between the menu and the tray.
const GAP: f32 = 4.;

#[derive(Default)]
struct MenuData {
    window: Option<AnyWindowHandle>,
    /// When it last closed: the click on the send button that closes it
    /// (by taking the focus) shouldn't open it again.
    closed: Option<Instant>,
}

impl Global for MenuData {}

/// Open the menu by the tray, which is `tray` on screen, or close it.
pub fn toggle(tray: Bounds<Pixels>, cx: &mut App) {
    let just_closed = cx.default_global::<MenuData>().closed.is_some_and(|t| t.elapsed() < Duration::from_millis(300));
    if !close(cx) && !just_closed {
        open(tray, cx);
    }
}

/// Close the menu. Returns whether it was open.
pub fn close(cx: &mut App) -> bool {
    let data = cx.default_global::<MenuData>();
    let Some(window) = data.window.take() else { return false };
    data.closed = Some(Instant::now());
    cx.defer(move |cx| {
        let _ = window.update(cx, |_, w, _| w.remove_window());
    });
    true
}

fn open(tray: Bounds<Pixels>, cx: &mut App) {
    let choices = targets::choices(cx);
    if choices.is_empty() {
        return;
    }
    let folders = folders(&choices);
    let longest = folders.iter().map(|f| choices.iter().filter(|c| &c.folder == f).count()).max().unwrap_or(0);
    let menu = style::el("shot_send_menu", cx);
    let row = style::el("shot_send_menu.row", cx).px("height").as_f32();
    let pad = menu.px("padding").as_f32() + menu.px("border_width").as_f32();
    let w = style::el("shot_send_menu.folders", cx).px("width").as_f32()
        + style::el("shot_send_menu.items", cx).px("width").as_f32()
        + menu.px("gap").as_f32()
        + 2. * pad;
    let h = folders.len().max(longest) as f32 * row + 2. * pad;
    // Above the tray, its right edge on the tray's; under it without room.
    let display = cx.displays().into_iter().find(|d| d.bounds().intersects(&tray)).or_else(|| cx.primary_display());
    let screen = display.as_ref().map_or(tray, |d| d.bounds());
    let x = (tray.right().as_f32() - w).max(screen.left().as_f32() + GAP);
    let above = tray.top().as_f32() - GAP - h;
    let y = if above >= screen.top().as_f32() + GAP { above } else { tray.bottom().as_f32() + GAP };
    let opened = gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(point(px(x), px(y)), size(px(w), px(h))))),
            titlebar: None,
            focus: true,
            show: true,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        },
        cx,
        |window, cx| cx.new(|cx| SendMenu::new(choices, window, cx)),
    );
    match opened {
        Ok((handle, _)) => {
            cx.default_global::<MenuData>().window = Some(handle);
            let _ = handle.update(cx, |_, w, _| w.activate_window());
        },
        Err(e) => log::error!("failed to open the send menu: {e}"),
    }
}

/// The choices' folders, in their order.
fn folders(choices: &[Choice]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in choices {
        if !out.contains(&c.folder) {
            out.push(c.folder.clone());
        }
    }
    out
}

struct SendMenu {
    choices: Vec<Choice>,
    folders: Vec<String>,
    /// The folder pointed at, whose tabs show.
    folder: usize,
    focus: FocusHandle,
    /// Whether the window was active yet: it closes once it is not.
    active: bool,
    _activation: Subscription,
}

impl SendMenu {
    fn new(choices: Vec<Choice>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let folders = folders(&choices);
        // Start at the folder of the tab picked last.
        let dest = super::tray::chosen_dest(cx);
        let folder =
            choices.iter().find(|c| Some(&c.dest) == dest.as_ref()).and_then(|c| folders.iter().position(|f| *f == c.folder)).unwrap_or(0);
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.active = true;
            } else if this.active {
                close(cx);
            }
        });
        Self { choices, folders, folder, focus, active: false, _activation: activation }
    }

    fn items(&self) -> Vec<&Choice> {
        let Some(folder) = self.folders.get(self.folder) else { return Vec::new() };
        self.choices.iter().filter(|c| &c.folder == folder).collect()
    }
}

impl Render for SendMenu {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dest = super::tray::chosen_dest(cx);
        let folders = self
            .folders
            .iter()
            .enumerate()
            .map(|(i, f)| Scope::new("folder").var("name", f.clone()).var("active", i == self.folder))
            .collect();
        let items: Vec<Dest> = self.items().into_iter().map(|c| c.dest.clone()).collect();
        let rows = self
            .items()
            .into_iter()
            .map(|c| {
                Scope::new("item")
                    .var("name", c.item.clone())
                    .var("checked", Some(&c.dest) == dest.as_ref())
                    .var("new", matches!(c.dest, Dest::New { .. }))
            })
            .collect();
        let scope = Scope::new("shot-send-menu").list("folders", folders).list("items", rows);
        let me = cx.entity().downgrade();
        let focus = self.focus.clone();
        template::render(
            "shot_send_menu",
            scope,
            &mut |name, ix, f| match (name, f) {
                ("menu", Framed::Box(d)) => d
                    .id("shot-send-menu")
                    .track_focus(&focus)
                    .on_key_down(|ev: &KeyDownEvent, _, cx| {
                        if ev.keystroke.key == "escape" {
                            close(cx);
                        }
                    })
                    .into_any_element(),
                ("folder", Framed::Box(d)) => {
                    let i = ix.unwrap_or(0);
                    let pick = on_event(&me, move |this: &mut Self, _: &bool, _, cx| {
                        this.folder = i;
                        cx.notify();
                    });
                    d.id(("folder", i))
                        .cursor_pointer()
                        .on_hover(move |hovered, window, cx| {
                            if *hovered {
                                pick(hovered, window, cx)
                            }
                        })
                        .into_any_element()
                },
                ("item", Framed::Box(d)) => {
                    let i = ix.unwrap_or(0);
                    let Some(dest) = items.get(i).cloned() else { return d.into_any_element() };
                    d.id(("item", i))
                        .cursor_pointer()
                        .on_click(move |_, _, cx| {
                            close(cx);
                            super::tray::send_to(dest.clone(), cx);
                        })
                        .into_any_element()
                },
                (_, f) => f.into_any_element(),
            },
            cx,
        )
    }
}
