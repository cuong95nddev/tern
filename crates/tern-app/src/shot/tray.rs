//! The tray: a small floating panel collecting captures, with the prompt
//! and the tab they go to, until they are sent.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::base::StyledExt as _;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Disableable as _, Sizable};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::icons;
use super::targets::{self, Choice, Dest};

const WIDTH: f32 = 420.;
/// The starting height; the window then fits its content.
const HEIGHT: f32 = 200.;
/// About the height with one capture, to place the tray by a region.
const PLACE_H: f32 = 240.;
/// The gap between the tray and the region or the display's edges.
const GAP: f32 = 8.;
/// A capture's thumbnail, in points.
const THUMB: (f32, f32) = (104., 68.);

/// What the tray holds; it outlives the tray window, which closes while a
/// capture is on screen.
#[derive(Default)]
struct TrayData {
    shots: Vec<PathBuf>,
    /// Small copies of the captures, made in the background: `img(path)`
    /// would keep each full-size capture in the app's asset cache for good.
    thumbs: HashMap<PathBuf, Arc<RenderImage>>,
    /// Thumbnails being made.
    making: HashSet<PathBuf>,
    prompt: String,
    dest: Option<Dest>,
    window: Option<AnyWindowHandle>,
}

impl Global for TrayData {}

/// Add a capture and show the tray, by `near` (a display and a region on
/// it, in the display's points) when the tray opens.
/// While the editor is open, the capture goes to it instead.
pub fn add(path: PathBuf, near: Option<(DisplayId, Bounds<Pixels>)>, cx: &mut App) {
    cx.default_global::<TrayData>().shots.push(path.clone());
    if !super::editor::add(path, cx) {
        open(near, cx);
    }
}

/// The captures back from the editor; the tray shows again if it holds
/// anything.
pub fn set_shots(shots: Vec<PathBuf>, cx: &mut App) {
    let data = cx.default_global::<TrayData>();
    data.shots = shots;
    if !data.shots.is_empty() || !data.prompt.trim().is_empty() {
        show(cx);
    }
}

/// Edit the captures, starting at the one at `at`; the tray steps aside.
pub(super) fn edit(at: usize, cx: &mut App) {
    let shots = cx.default_global::<TrayData>().shots.clone();
    hide(cx);
    super::editor::open(shots, at, cx);
}

/// Whether the tray has captures waiting.
pub fn has_shots(cx: &mut App) -> bool {
    !cx.default_global::<TrayData>().shots.is_empty()
}

/// Open the tray window (bottom right of the main display), or refresh it.
pub fn show(cx: &mut App) {
    open(None, cx);
}

fn open(near: Option<(DisplayId, Bounds<Pixels>)>, cx: &mut App) {
    // The editor holds the captures until it is done.
    if super::editor::is_open(cx) {
        return;
    }
    if let Some(window) = cx.default_global::<TrayData>().window
        && window.update(cx, |_, w, _| w.refresh()).is_ok()
    {
        return;
    }
    let display = match near {
        Some((id, _)) => cx.displays().into_iter().find(|d| d.id() == id).or_else(|| cx.primary_display()),
        None => cx.primary_display(),
    };
    let Some(display) = display else { return };
    let region = near.filter(|(id, _)| *id == display.id()).map(|(_, r)| r);
    let origin = super::window_origin(display.bounds()) + place(display.bounds().size, region);
    let opened = gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(origin, gpui_kit::size(px(WIDTH), px(HEIGHT))))),
            titlebar: None,
            focus: true,
            show: true,
            kind: WindowKind::PopUp,
            is_movable: true,
            is_resizable: false,
            is_minimizable: false,
            display_id: Some(display.id()),
            // Only the rounded panel shows.
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        },
        cx,
        |window, cx| cx.new(|cx| Tray::new(window, cx)),
    );
    match opened {
        Ok((handle, _)) => {
            cx.default_global::<TrayData>().window = Some(handle);
            let _ = handle.update(cx, |_, w, _| w.activate_window());
        },
        Err(e) => log::error!("failed to open the screenshot tray: {e}"),
    }
}

/// Where the tray goes on a display of `size`: under `region`, its right
/// edge on the region's (as the overlay's toolbar was); above it, or over
/// its bottom, when there is no room. Without one, bottom right.
fn place(size: Size<Pixels>, region: Option<Bounds<Pixels>>) -> Point<Pixels> {
    let (w, h) = (size.width.as_f32(), size.height.as_f32());
    let Some(r) = region else { return point(px(w - WIDTH - 24.), px(h - HEIGHT - 96.)) };
    let x = (r.right().as_f32() - WIDTH).min(w - WIDTH - GAP).max(GAP);
    let (top, bottom) = (r.top().as_f32(), r.bottom().as_f32());
    let y = if bottom + GAP + PLACE_H <= h - GAP {
        bottom + GAP
    } else if top - GAP - PLACE_H >= GAP {
        top - GAP - PLACE_H
    } else {
        (bottom - GAP - PLACE_H).max(GAP)
    };
    point(px(x), px(y))
}

/// The tray window while it is open.
#[cfg(debug_assertions)]
pub fn window(cx: &mut App) -> Option<AnyWindowHandle> {
    cx.default_global::<TrayData>().window
}

#[cfg(debug_assertions)]
fn tray_view(cx: &mut App) -> Option<Entity<Tray>> {
    let handle = window(cx)?.downcast::<gpui_kit::base::Root>()?;
    handle.update(cx, |root, _, _| root.view().clone().downcast::<Tray>().ok()).ok().flatten()
}

#[cfg(debug_assertions)]
pub fn debug_state(cx: &mut App) -> String {
    let choices = tray_view(cx).map(|t| {
        t.update(cx, |t, cx| {
            t.refresh_choices(cx);
            t.choices.iter().enumerate().map(|(i, c)| format!("{i}:{}", c.label)).collect::<Vec<_>>()
        })
    });
    let error = tray_view(cx).and_then(|t| t.read(cx).error.clone());
    let data = cx.default_global::<TrayData>();
    let chosen = data.dest.is_some();
    format!(
        "tray open={} shots={} prompt={:?} chosen={chosen} error={error:?} choices={choices:?}",
        data.window.is_some(),
        data.shots.len(),
        data.prompt
    )
}

#[cfg(debug_assertions)]
pub fn control(cmd: &str, arg: &str, cx: &mut App) -> String {
    let Some(tray) = tray_view(cx) else { return "error: tray not open".into() };
    let Some(handle) = window(cx) else { return "error: tray not open".into() };
    handle
        .update(cx, |_, window, cx| {
            tray.update(cx, |t, cx| match cmd {
                "tray-prompt" => {
                    t.prompt.update(cx, |p, cx| p.set_value(arg.to_owned(), window, cx));
                    cx.default_global::<TrayData>().prompt = arg.to_owned();
                    "ok".to_owned()
                },
                "tray-target" => {
                    t.refresh_choices(cx);
                    match arg.parse::<usize>().ok().and_then(|i| t.choices.get(i)) {
                        Some(c) => {
                            cx.default_global::<TrayData>().dest = Some(c.dest.clone());
                            cx.notify();
                            "ok".to_owned()
                        },
                        None => "error: no such target".to_owned(),
                    }
                },
                _ => {
                    t.send(window, cx);
                    "ok".to_owned()
                },
            })
        })
        .unwrap_or_else(|_| "error: window closed".into())
}

/// Close the tray window, keeping what it holds.
/// Returns whether it was open. It closes once the current update ends:
/// this may run in one of the tray's own handlers (its camera button).
pub fn hide(cx: &mut App) -> bool {
    let Some(window) = cx.default_global::<TrayData>().window.take() else { return false };
    cx.defer(move |cx| {
        let _ = window.update(cx, |_, w, _| w.remove_window());
    });
    true
}

/// Drop everything and close.
fn discard(cx: &mut App) {
    let data = cx.default_global::<TrayData>();
    data.shots.clear();
    data.prompt.clear();
    hide(cx);
}

pub struct Tray {
    prompt: Entity<TextareaState>,
    choices: Vec<Choice>,
    sending: bool,
    error: Option<String>,
    _sub: Subscription,
}

impl Tray {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let text = cx.default_global::<TrayData>().prompt.clone();
        let prompt = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 5)
                .placeholder(super::keys("What should the agent do? (⌘↵ sends)", "What should the agent do? (Ctrl+Enter sends)"))
                .default_value(text)
        });
        let sub = cx.subscribe_in(&prompt, window, |this, input, ev: &InputEvent, window, cx| match ev {
            InputEvent::Change => cx.default_global::<TrayData>().prompt = input.read(cx).value().to_string(),
            InputEvent::PressEnter { secondary: true, .. } => this.send(window, cx),
            _ => {},
        });
        prompt.focus_handle(cx).focus(window, cx);
        let mut this = Self { prompt, choices: Vec::new(), sending: false, error: None, _sub: sub };
        this.refresh_choices(cx);
        this
    }

    /// Re-read the tabs; keep the chosen one while it exists.
    fn refresh_choices(&mut self, cx: &mut Context<Self>) {
        self.choices = targets::choices(cx);
        let dest = cx.default_global::<TrayData>().dest.clone();
        if dest.as_ref().is_none_or(|d| !self.choices.iter().any(|c| &c.dest == d)) {
            let def = targets::default_choice(&self.choices, cx).map(|i| self.choices[i].dest.clone());
            cx.default_global::<TrayData>().dest = def;
        }
    }

    fn chosen(&self, cx: &mut App) -> Option<Choice> {
        let dest = cx.default_global::<TrayData>().dest.clone()?;
        self.choices.iter().find(|c| c.dest == dest).cloned()
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sending {
            return;
        }
        self.refresh_choices(cx);
        let Some(choice) = self.chosen(cx) else {
            self.error = Some("Open a Tern window first".into());
            return cx.notify();
        };
        let data = cx.default_global::<TrayData>();
        let (shots, prompt) = (data.shots.clone(), data.prompt.clone());
        if shots.is_empty() && prompt.trim().is_empty() {
            return;
        }
        self.sending = true;
        self.error = None;
        cx.notify();
        let task = targets::deliver(choice.dest, shots, prompt, cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.sending = false;
                match result {
                    Ok(()) => cx.defer(discard),
                    Err(e) => {
                        this.error = Some(e);
                        cx.notify();
                    },
                }
            });
        })
        .detach();
    }

    fn remove(&mut self, ix: usize, cx: &mut Context<Self>) {
        let data = cx.default_global::<TrayData>();
        if ix < data.shots.len() {
            data.shots.remove(ix);
        }
        if data.shots.is_empty() && data.prompt.trim().is_empty() {
            cx.defer(|cx| {
                hide(cx);
            });
        }
        cx.notify();
    }

    fn target_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label = self.chosen(cx).map_or_else(|| "No tab".to_owned(), |c| c.label);
        let this = cx.entity().downgrade();
        Button::new("target").small().ghost().label(label).dropdown_caret(true).dropdown_menu(move |mut menu, _, cx| {
            let Some(tray) = this.upgrade() else { return menu };
            tray.update(cx, |t, cx| t.refresh_choices(cx));
            let (choices, chosen) = {
                let t = tray.read(cx);
                (t.choices.clone(), cx.global::<TrayData>().dest.clone())
            };
            let mut new_section = false;
            for c in choices {
                if matches!(c.dest, Dest::New { .. }) && !new_section {
                    new_section = true;
                    menu = menu.separator();
                }
                let dest = c.dest.clone();
                let tray = tray.downgrade();
                menu = menu.item(PopupMenuItem::new(c.label).checked(chosen.as_ref() == Some(&dest)).on_click(move |_, _, cx| {
                    cx.default_global::<TrayData>().dest = Some(dest.clone());
                    let _ = tray.update(cx, |_, cx| cx.notify());
                }));
            }
            menu
        })
    }
}

/// `path`'s image shrunk to fit `w` × `h` pixels.
fn thumbnail(path: &Path, w: u32, h: u32) -> Option<Arc<RenderImage>> {
    let img = image::open(path).ok()?.to_rgba8();
    let scale = (w as f32 / img.width() as f32).min(h as f32 / img.height() as f32).min(1.);
    let (tw, th) = (((img.width() as f32 * scale).round() as u32).max(1), ((img.height() as f32 * scale).round() as u32).max(1));
    Some(super::paint::picture(&image::imageops::thumbnail(&img, tw, th)))
}

impl Tray {
    /// The thumbnail of each capture that has one; starts making the others.
    fn thumbs(&mut self, shots: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) -> Vec<Option<Arc<RenderImage>>> {
        let data = cx.default_global::<TrayData>();
        data.thumbs.retain(|p, _| shots.contains(p));
        data.making.retain(|p| shots.contains(p));
        let todo: Vec<PathBuf> =
            shots.iter().filter(|p| !data.thumbs.contains_key(*p) && data.making.insert((*p).clone())).cloned().collect();
        let out = shots.iter().map(|p| data.thumbs.get(p).cloned()).collect();
        let scale = window.scale_factor();
        let (w, h) = ((THUMB.0 * scale) as u32, (THUMB.1 * scale) as u32);
        for path in todo {
            cx.spawn(async move |this, cx| {
                let p = path.clone();
                let thumb = cx.background_spawn(async move { thumbnail(&p, w, h) }).await;
                // One that can't be read stays in `making`: not tried again.
                cx.update(|cx| {
                    if let Some(t) = thumb {
                        let data = cx.default_global::<TrayData>();
                        data.making.remove(&path);
                        data.thumbs.insert(path, t);
                    }
                });
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }
        out
    }
}

impl Render for Tray {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shots = cx.default_global::<TrayData>().shots.clone();
        let thumbs = self.thumbs(&shots, window, cx);
        let theme = cx.theme().clone();
        let header = div()
            .flex()
            .items_center()
            .gap_1()
            .child(div().flex_1().text_sm().font_semibold().child(format!("Screenshots ({})", shots.len())))
            .child(
                Button::new("edit")
                    .icon(icons::icon(icons::EDIT))
                    .ghost()
                    .small()
                    .disabled(shots.is_empty())
                    .tooltip("Edit: combine, crop, add text")
                    .on_click(|_, _, cx| edit(0, cx)),
            )
            .child(
                Button::new("more")
                    .icon(icons::icon(icons::CAMERA))
                    .ghost()
                    .small()
                    .tooltip(super::keys("Capture another (⌘⇧2)", "Capture another (Ctrl+Shift+2)"))
                    .on_click(|_, _, cx| super::start(cx)),
            )
            .child(
                Button::new("discard")
                    .icon(icons::icon(icons::CLOSE))
                    .ghost()
                    .small()
                    .tooltip("Discard all")
                    .on_click(|_, _, cx| discard(cx)),
            );
        let mut strip = div().id("shots").flex().gap_2().overflow_x_scroll().py_1();
        for (i, thumb) in thumbs.into_iter().enumerate() {
            strip = strip.child(
                div()
                    .id(("shot", i))
                    .relative()
                    .flex_shrink_0()
                    .cursor_pointer()
                    .on_click(move |_, _, cx| edit(i, cx))
                    .w(px(104.))
                    .h(px(68.))
                    .rounded_md()
                    .overflow_hidden()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.muted)
                    .when_some(thumb, |d, t| d.child(img(ImageSource::Render(t)).size_full().object_fit(ObjectFit::Contain)))
                    .child(div().absolute().top(px(2.)).right(px(2.)).child(
                        Button::new(("remove", i)).icon(icons::icon(icons::CLOSE)).xsmall().tooltip("Remove").on_click(cx.listener(
                            move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.remove(i, cx);
                            },
                        )),
                    )),
            );
        }
        let footer =
            div().flex().items_center().gap_2().child(div().flex_1().min_w_0().overflow_hidden().child(self.target_button(cx))).child(
                Button::new("send")
                    .primary()
                    .small()
                    .icon(icons::icon(icons::SEND))
                    .label(if self.sending { "Sending…" } else { "Send" })
                    .disabled(self.sending)
                    .tooltip(super::keys("Send (⌘↵)", "Send (Ctrl+Enter)"))
                    .on_click(cx.listener(|this, _, window, cx| this.send(window, cx))),
            );
        // Fit the window to the panel's height, which changes with the
        // shots and the prompt's lines.
        let fit = canvas(
            |bounds, window, cx| {
                let h = bounds.size.height;
                if (window.viewport_size().height - h).abs() > px(1.) {
                    let w = window.viewport_size().width;
                    window.defer(cx, move |window, _| window.resize(gpui_kit::size(w, h)));
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0();
        div()
            .relative()
            .w_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .bg(theme.background)
            .text_color(theme.foreground)
            .border_1()
            .border_color(theme.border)
            .rounded_lg()
            .child(header)
            .when(!shots.is_empty(), |d| d.child(strip))
            .child(Textarea::new(&self.prompt))
            .when_some(self.error.clone(), |d, e| d.child(div().text_xs().text_color(theme.danger).child(e)))
            .child(footer)
            .child(fit)
    }
}
