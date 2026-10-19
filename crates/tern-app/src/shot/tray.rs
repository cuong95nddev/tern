//! The tray: a small floating panel to capture a region, a window or the
//! screen, collecting the captures with the prompt and the tab they go to.
//! It stays until it is closed, stepping aside while a capture is on screen.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::component::Sizable as _;
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::*;

use super::Mode;
use super::targets::{self, Choice, Dest};
use crate::style;
use crate::template::{self, Framed, Scope, on_event};

const WIDTH: f32 = 340.;
/// The starting height; the window then fits its content.
const HEIGHT: f32 = 160.;
/// About the height with one capture, to place the tray by a region.
const PLACE_H: f32 = 180.;
/// The gap between the tray and the region or the display's edges.
const GAP: f32 = 8.;

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
    /// Shown until closed: it comes back after a capture even when empty.
    open: bool,
    /// Where the tray was last, to come back there.
    origin: Option<Point<Pixels>>,
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
    cx.default_global::<TrayData>().shots = shots;
    restore(cx);
}

/// Edit the captures, starting at the one at `at`; the tray steps aside.
pub(super) fn edit(at: usize, cx: &mut App) {
    let shots = cx.default_global::<TrayData>().shots.clone();
    hide(cx);
    super::editor::open(shots, at, cx);
}

/// Open the tray window (bottom right of the main display, or where it
/// was last), or refresh it.
pub fn show(cx: &mut App) {
    open(None, cx);
}

/// Show the tray again after a capture if it was open or holds anything.
pub fn restore(cx: &mut App) {
    let data = cx.default_global::<TrayData>();
    if data.open || !data.shots.is_empty() || !data.prompt.trim().is_empty() {
        show(cx);
    }
}

/// Whether the tray is showing.
pub fn is_open(cx: &mut App) -> bool {
    cx.default_global::<TrayData>().window.is_some()
}

/// Open the tray, or close it.
pub fn toggle(cx: &mut App) {
    if is_open(cx) { close(cx) } else { show(cx) }
}

/// Close the tray for good (until it is opened again), keeping what it
/// holds.
fn close(cx: &mut App) {
    cx.default_global::<TrayData>().open = false;
    hide(cx);
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
    let last = cx.default_global::<TrayData>().origin;
    let display = match near {
        Some((id, _)) if last.is_none() => cx.displays().into_iter().find(|d| d.id() == id).or_else(|| cx.primary_display()),
        _ => cx.primary_display(),
    };
    let Some(display) = display else { return };
    let region = near.filter(|(id, _)| *id == display.id()).map(|(_, r)| r);
    let origin = last.unwrap_or_else(|| super::window_origin(display.bounds()) + place(display.bounds().size, region));
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
            let data = cx.default_global::<TrayData>();
            data.window = Some(handle);
            data.open = true;
            let _ = handle.update(cx, |_, w, _| w.activate_window());
            // The agents panel's button shows it open.
            cx.refresh_windows();
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
pub fn window(cx: &mut App) -> Option<AnyWindowHandle> {
    cx.default_global::<TrayData>().window
}

fn tray_view(cx: &mut App) -> Option<Entity<Tray>> {
    let handle = window(cx)?.downcast::<gpui_kit::base::Root>()?;
    handle.update(cx, |root, _, _| root.view().clone().downcast::<Tray>().ok()).ok().flatten()
}

/// The tab the captures go to by the send shortcut.
pub(super) fn chosen_dest(cx: &mut App) -> Option<Dest> {
    cx.default_global::<TrayData>().dest.clone()
}

/// Send what the tray holds to `dest`, which the send shortcut then uses.
pub(super) fn send_to(dest: Dest, cx: &mut App) {
    cx.default_global::<TrayData>().dest = Some(dest);
    let (Some(tray), Some(handle)) = (tray_view(cx), window(cx)) else { return };
    let _ = handle.update(cx, |_, window, cx| tray.update(cx, |t, cx| t.send(window, cx)));
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
    super::send_menu::close(cx);
    let Some(window) = cx.default_global::<TrayData>().window.take() else { return false };
    if let Ok(origin) = window.update(cx, |_, w, _| w.bounds().origin) {
        cx.default_global::<TrayData>().origin = Some(origin);
    }
    cx.defer(move |cx| {
        let _ = window.update(cx, |_, w, _| w.remove_window());
        cx.refresh_windows();
    });
    true
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
                .auto_grow(1, 4)
                .placeholder(super::keys("Prompt (⌘↵ sends)", "Prompt (Ctrl+Enter sends)"))
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
            let _ = this.update_in(cx, |this, window, cx| {
                this.sending = false;
                match result {
                    Ok(()) => this.clear(window, cx),
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
        cx.notify();
    }

    /// Drop the captures and the prompt; the tray stays.
    fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let data = cx.default_global::<TrayData>();
        data.shots.clear();
        data.prompt.clear();
        self.prompt.update(cx, |p, cx| p.set_value(String::new(), window, cx));
        self.error = None;
        cx.notify();
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
        let thumb = style::el("shot_tray.shot", cx);
        let (w, h) = ((thumb.px("width").as_f32() * scale) as u32, (thumb.px("height").as_f32() * scale) as u32);
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
        let items = thumbs
            .into_iter()
            .map(|thumb| {
                let shown = thumb.map(|t| img(ImageSource::Render(t)).size_full().object_fit(ObjectFit::Contain));
                Scope::new("shot").slot_opt("thumb", shown)
            })
            .collect();
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
        let target = self.chosen(cx).map_or_else(|| "No tab".to_owned(), |c| c.label);
        let mut scope = Scope::new("shot-tray")
            .var("count", shots.len())
            .list("shots", items)
            .var("target", target)
            .var("sending", self.sending)
            .var("capture_key", super::keys("⌘⇧2", "Ctrl+Shift+2"))
            .var("send_key", super::keys("⌘↵", "Ctrl+Enter"))
            .slot("prompt", Textarea::new(&self.prompt).small())
            .slot("fit", fit);
        if let Some(e) = self.error.clone() {
            scope = scope.var("error", e);
        }
        let me = cx.entity().downgrade();
        let panel = template::render(
            "shot_tray",
            scope,
            &mut |name, ix, f| match (name, f) {
                ("edit", Framed::Button(b)) => b.on_click(|_, _, cx| edit(0, cx)).into_any_element(),
                ("region", Framed::Button(b)) => b.on_click(|_, _, cx| super::capture(Mode::Region, cx)).into_any_element(),
                ("window", Framed::Button(b)) => b.on_click(|_, _, cx| super::capture(Mode::Window, cx)).into_any_element(),
                ("screen", Framed::Button(b)) => b.on_click(|_, _, cx| super::capture(Mode::Screen, cx)).into_any_element(),
                ("clear", Framed::Button(b)) => {
                    b.on_click(on_event(&me, |this, _: &ClickEvent, window, cx| this.clear(window, cx))).into_any_element()
                },
                ("close", Framed::Button(b)) => b.on_click(|_, _, cx| close(cx)).into_any_element(),
                // The title moves the window: the system drags it on Windows,
                // the mouse down starts the move elsewhere.
                ("drag", Framed::Box(d)) => d
                    .id("tray-drag")
                    .window_control_area(WindowControlArea::Drag)
                    .on_mouse_down(MouseButton::Left, |_, window, cx| {
                        super::send_menu::close(cx);
                        window.start_window_move()
                    })
                    .into_any_element(),
                ("shots", Framed::Box(d)) => d.id("shots").min_w_0().overflow_x_scroll().into_any_element(),
                ("shot", Framed::Box(d)) => {
                    let i = ix.unwrap_or(0);
                    d.id(("shot", i)).cursor_pointer().on_click(move |_, _, cx| edit(i, cx)).into_any_element()
                },
                ("remove", Framed::Button(b)) => {
                    let i = ix.unwrap_or(0);
                    b.on_click(on_event(&me, move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        this.remove(i, cx);
                    }))
                    .into_any_element()
                },
                ("send", Framed::Button(b)) => b.on_click(|_, window, cx| super::send_menu::toggle(window.bounds(), cx)).into_any_element(),
                (_, f) => f.into_any_element(),
            },
            cx,
        );
        // The window is the panel's width; its height follows the panel.
        div().w_full().child(panel)
    }
}
