//! The capture overlay: one borderless window per display showing its
//! frozen screen, where a region is picked and marked up.

use std::sync::Arc;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::*;
use image::RgbaImage;

use super::model::{self, COLORS, Doc, P, Shape, Style, Tool, p};
use super::paint::{self, View, from_point, hsla, picture, to_point};
use super::raster::{self, Crop};
use crate::style;
use crate::template::{self, Framed, Scope, on_event};

const HANDLE: f32 = 8.;
/// The toolbar's width, to place it before it is laid out.
const BAR_W: f32 = 600.;

enum Drag {
    Select {
        start: P,
    },
    Move {
        grab: P,
        orig: (P, P),
    },
    /// Handle index: 0 top left, then clockwise.
    Resize {
        handle: usize,
        orig: (P, P),
    },
    /// A shape in image pixels.
    Draw(Shape),
}

struct TextEdit {
    /// Top left, in points.
    at: P,
    input: Entity<InputState>,
    _sub: Subscription,
}

pub struct Overlay {
    focus: FocusHandle,
    base: Arc<RgbaImage>,
    picture: Arc<RenderImage>,
    /// Image pixels per point.
    scale: f32,
    /// The region, in points, normalised.
    sel: Option<(P, P)>,
    drag: Option<Drag>,
    tool: Option<Tool>,
    style: Style,
    doc: Doc,
    text: Option<TextEdit>,
    /// In window mode, the windows to pick from (in points, topmost
    /// first): a click takes the one under it as the region.
    windows: Option<Vec<(P, P)>>,
    /// The window under the pointer, before one is picked.
    hover: Option<(P, P)>,
}

impl Overlay {
    /// `windows`, in `base`'s pixels, puts the overlay in window mode.
    pub fn new(base: RgbaImage, width_pt: f32, windows: Option<Vec<[i32; 4]>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let scale = if width_pt > 0. { base.width() as f32 / width_pt } else { 1. };
        let windows = windows.map(|ws| {
            ws.into_iter()
                .map(|[x, y, w, h]| {
                    let lo = p(x as f32, y as f32).scale(1. / scale);
                    (lo, lo + p(w as f32, h as f32).scale(1. / scale))
                })
                .collect()
        });
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            focus,
            picture: picture(&base),
            base: Arc::new(base),
            scale,
            sel: None,
            drag: None,
            tool: None,
            style: Style::default(),
            doc: Doc::new(scale),
            text: None,
            windows,
            hover: None,
        }
    }

    /// The topmost window under `q`, in window mode.
    fn window_at(&self, q: P) -> Option<(P, P)> {
        self.windows.as_ref()?.iter().find(|(lo, hi)| q.x >= lo.x && q.x < hi.x && q.y >= lo.y && q.y < hi.y).copied()
    }

    fn to_px(&self, q: P) -> P {
        q.scale(self.scale)
    }

    fn handles(sel: (P, P)) -> [P; 8] {
        let (lo, hi) = sel;
        let mid = (lo + hi).scale(0.5);
        [lo, p(mid.x, lo.y), p(hi.x, lo.y), p(hi.x, mid.y), hi, p(mid.x, hi.y), p(lo.x, hi.y), p(lo.x, mid.y)]
    }

    fn handle_at(&self, q: P) -> Option<usize> {
        let sel = self.sel?;
        Self::handles(sel).iter().position(|h| (q.x - h.x).abs() <= HANDLE && (q.y - h.y).abs() <= HANDLE)
    }

    fn inside(&self, q: P) -> bool {
        self.sel.is_some_and(|(lo, hi)| q.x >= lo.x && q.x <= hi.x && q.y >= lo.y && q.y <= hi.y)
    }

    fn on_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let q = from_point(ev.position);
        if self.text.is_some() {
            self.commit_text(window, cx);
            return;
        }
        if ev.click_count == 2 && self.inside(q) && self.tool.is_none() {
            return self.add(window, cx);
        }
        // Window mode: a click picks a window, or another one while nothing
        // is drawn.
        let repick = self.sel.is_none() || (!self.inside(q) && self.handle_at(q).is_none() && self.doc.shapes.is_empty());
        if repick && let Some(picked) = self.window_at(q) {
            self.sel = Some(picked);
            self.hover = None;
            return cx.notify();
        }
        self.drag = match (self.sel, self.handle_at(q), self.tool) {
            (None, _, _) => Some(Drag::Select { start: q }),
            (Some(sel), Some(handle), _) => Some(Drag::Resize { handle, orig: sel }),
            (Some(_), None, Some(tool)) if self.inside(q) => self.start_tool(tool, q, window, cx),
            (Some(sel), None, None) if self.inside(q) => Some(Drag::Move { grab: q, orig: sel }),
            // Outside the region: pick a new one, unless something is drawn.
            (Some(_), None, _) if self.doc.shapes.is_empty() => Some(Drag::Select { start: q }),
            _ => None,
        };
        cx.notify();
    }

    fn start_tool(&mut self, tool: Tool, q: P, window: &mut Window, cx: &mut Context<Self>) -> Option<Drag> {
        let at = self.to_px(q);
        let style = self.style;
        Some(Drag::Draw(match tool {
            Tool::Rect => Shape::Rect { a: at, b: at, style },
            Tool::Ellipse => Shape::Ellipse { a: at, b: at, style },
            Tool::Line => Shape::Line { a: at, b: at, style },
            Tool::Arrow => Shape::Arrow { a: at, b: at, style },
            Tool::Pen => Shape::Pen { points: vec![at], style },
            Tool::Mosaic => Shape::Mosaic { a: at, b: at },
            Tool::Marker => {
                self.doc.push(Shape::Marker { at, n: self.doc.next_marker(), style });
                return None;
            },
            Tool::Text => {
                self.start_text(q, window, cx);
                return None;
            },
        }))
    }

    fn on_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let q = from_point(ev.position);
        let at = self.to_px(q);
        if self.sel.is_none() && self.drag.is_none() {
            let hover = self.window_at(q);
            if hover != self.hover {
                self.hover = hover;
                cx.notify();
            }
        }
        let Some(drag) = &mut self.drag else { return };
        match drag {
            Drag::Select { start } => self.sel = Some(model::norm(*start, q)),
            Drag::Move { grab, orig } => {
                let d = q - *grab;
                self.sel = Some((orig.0 + d, orig.1 + d));
            },
            Drag::Resize { handle, orig } => {
                let (mut lo, mut hi) = *orig;
                match handle {
                    0 => lo = q,
                    1 => lo.y = q.y,
                    2 => (hi.x, lo.y) = (q.x, q.y),
                    3 => hi.x = q.x,
                    4 => hi = q,
                    5 => hi.y = q.y,
                    6 => (lo.x, hi.y) = (q.x, q.y),
                    _ => lo.x = q.x,
                }
                self.sel = Some(model::norm(lo, hi));
            },
            Drag::Draw(shape) => match shape {
                Shape::Rect { b, .. }
                | Shape::Ellipse { b, .. }
                | Shape::Line { b, .. }
                | Shape::Arrow { b, .. }
                | Shape::Mosaic { b, .. } => *b = at,
                Shape::Pen { points, .. } => {
                    if points.last().is_none_or(|l| (*l - at).len() >= 1.) {
                        points.push(at)
                    }
                },
                Shape::Text { .. } | Shape::Marker { .. } => {},
            },
        }
        cx.notify();
    }

    fn on_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        match self.drag.take() {
            Some(Drag::Select { .. }) => {
                if self.sel.is_some_and(|(lo, hi)| hi.x - lo.x < 4. || hi.y - lo.y < 4.) {
                    self.sel = None;
                }
            },
            Some(Drag::Draw(shape)) => {
                let keep = match &shape {
                    Shape::Rect { a, b, .. }
                    | Shape::Ellipse { a, b, .. }
                    | Shape::Line { a, b, .. }
                    | Shape::Arrow { a, b, .. }
                    | Shape::Mosaic { a, b } => !model::too_small(*a, *b, self.scale),
                    _ => true,
                };
                if keep {
                    self.doc.push(shape);
                }
            },
            _ => {},
        }
        cx.notify();
    }

    fn start_text(&mut self, q: P, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx));
        let sub = cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { .. } | InputEvent::Blur => this.commit_text(window, cx),
            _ => {},
        });
        // After this click, which focuses the overlay when it ends.
        let handle = input.focus_handle(cx);
        window.defer(cx, move |window, cx| handle.focus(window, cx));
        self.text = Some(TextEdit { at: q, input, _sub: sub });
        cx.notify();
    }

    fn commit_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.text.take() else { return };
        let text = edit.input.read(cx).value().trim_end().to_string();
        if !text.is_empty() {
            let at = self.to_px(edit.at);
            self.doc.push(Shape::Text { at, text, style: self.style });
        }
        // Later, so the Enter that committed doesn't also reach the overlay.
        cx.defer_in(window, |this, window, cx| window.focus(&this.focus, cx));
        cx.notify();
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) {
            return;
        }
        let ks = &ev.keystroke;
        let m = ks.modifiers;
        match ks.key.as_str() {
            "escape" if self.drag.is_some() => self.drag = None,
            "escape" => return super::cancel(cx),
            "enter" if self.sel.is_some() => return self.add(window, cx),
            "z" if m.secondary() && m.shift => {
                self.doc.redo();
            },
            "z" if m.secondary() => {
                self.doc.undo();
            },
            "c" if m.secondary() => return self.copy(cx),
            "s" if m.secondary() => return self.save_as(cx),
            key if !m.platform && !m.control && !m.alt && self.sel.is_some() => {
                let tool = match key {
                    "r" => Tool::Rect,
                    "e" => Tool::Ellipse,
                    "l" => Tool::Line,
                    "a" => Tool::Arrow,
                    "p" => Tool::Pen,
                    "t" => Tool::Text,
                    "n" => Tool::Marker,
                    "m" => Tool::Mosaic,
                    _ => return,
                };
                self.tool = Some(tool);
            },
            _ => return,
        }
        cx.notify();
    }

    /// The region with its markup, at full resolution.
    fn export(&self) -> Option<RgbaImage> {
        let (lo, hi) = self.sel?;
        let (lo, hi) = (self.to_px(lo), self.to_px(hi));
        let crop = Crop {
            x: lo.x.max(0.).round() as u32,
            y: lo.y.max(0.).round() as u32,
            w: (hi.x - lo.x).round().max(1.) as u32,
            h: (hi.y - lo.y).round().max(1.) as u32,
        };
        Some(raster::render(&self.base, crop, &self.doc, raster::font()))
    }

    fn finish_text(&mut self, cx: &mut Context<Self>) {
        if let Some(edit) = self.text.take() {
            let text = edit.input.read(cx).value().trim_end().to_string();
            if !text.is_empty() {
                let at = self.to_px(edit.at);
                self.doc.push(Shape::Text { at, text, style: self.style });
            }
        }
    }

    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_text(cx);
        let Some(img) = self.export() else { return };
        // The tray opens by the region; the overlay covers its display.
        let near = window.display(cx).zip(self.sel).map(|(d, (lo, hi))| (d.id(), Bounds::from_corners(to_point(lo), to_point(hi))));
        super::cancel(cx);
        match super::grab::save(&img, &super::grab::dir()) {
            Ok(path) => super::tray::add(path, near, cx),
            Err(e) => super::notify("Couldn't save the screenshot", &e, cx),
        }
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        self.finish_text(cx);
        let Some(img) = self.export() else { return };
        super::cancel(cx);
        match png_bytes(&img) {
            Ok(bytes) => cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(ImageFormat::Png, bytes))),
            Err(e) => super::notify("Couldn't copy the screenshot", &e, cx),
        }
    }

    fn save_as(&mut self, cx: &mut Context<Self>) {
        self.finish_text(cx);
        let Some(img) = self.export() else { return };
        // The save panel would open under the overlay.
        super::cancel(cx);
        let home = crate::dirs::home().unwrap_or_default();
        let rx = cx.prompt_for_new_path(&home.join("Desktop"), Some("Screenshot.png"));
        cx.spawn(async move |_, cx| {
            let Ok(Ok(Some(path))) = rx.await else { return };
            if let Err(e) = img.save_with_format(&path, image::ImageFormat::Png) {
                cx.update(|cx| super::notify("Couldn't save the screenshot", &e.to_string(), cx));
            }
        })
        .detach();
    }

    fn scene(&self) -> Scene {
        let mut shapes = self.doc.shapes.clone();
        if let Some(Drag::Draw(s)) = &self.drag {
            shapes.push(s.clone());
        }
        let hover = if self.sel.is_none() { self.hover } else { None };
        Scene { shapes, sel: self.sel, hover, scale: self.scale, base: self.base.clone(), accent: hsla(COLORS[1]) }
    }

    #[cfg(debug_assertions)]
    pub fn debug_state(&self) -> String {
        let sel = self.sel.map(|(lo, hi)| format!("{},{} {}x{}", lo.x, lo.y, hi.x - lo.x, hi.y - lo.y));
        format!(
            "overlay sel={sel:?} tool={:?} shapes={} scale={} text={}",
            self.tool,
            self.doc.shapes.len(),
            self.scale,
            self.text.is_some()
        )
    }
}

pub(super) fn png_bytes(img: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(buf.into_inner())
}

impl Focusable for Overlay {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Overlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let size = window.viewport_size();
        let (w, h) = (size.width.as_f32(), size.height.as_f32());
        let scene = self.scene();
        let shown = img(ImageSource::Render(self.picture.clone())).absolute().top_0().left_0().w(size.width).h(size.height);
        let painted = canvas(|_, _, _| {}, move |_, _, window, cx| scene.paint(window, cx)).absolute().top_0().left_0().size_full();
        let mut scope = Scope::new("shot-overlay").slot("picture", shown).slot("scene", painted).var("hint_top", f64::from(h / 2. - 20.));
        let hint = if self.windows.is_some() {
            "Click a window to capture it · Esc to cancel"
        } else {
            "Drag to capture a region · Esc to cancel"
        };
        scope = scope.var("hint", hint);
        if let Some((lo, hi)) = self.sel {
            // Size label above the region (inside it at the top of the screen).
            let label_top = if lo.y >= 30. { lo.y - 28. } else { lo.y + 6. };
            scope = scope
                .var("selection", true)
                .var("label_left", f64::from(lo.x))
                .var("label_top", f64::from(label_top))
                .var("size", format!("{} × {}", (hi.x - lo.x).round(), (hi.y - lo.y).round()));
            if !self.drag.as_ref().is_some_and(|d| matches!(d, Drag::Select { .. })) {
                scope = self.bars(scope, (lo, hi), (w, h), cx);
            }
        }
        let me = cx.entity().downgrade();
        let focus = self.focus.clone();
        template::render(
            "shot_overlay",
            scope,
            &mut |name, ix, f| match (name, f) {
                ("overlay", Framed::Box(d)) => d
                    .id("shot-overlay")
                    .track_focus(&focus)
                    .cursor(CursorStyle::Crosshair)
                    .on_key_down(on_event(&me, Self::on_key))
                    .on_mouse_down(MouseButton::Left, on_event(&me, Self::on_down))
                    .on_mouse_move(on_event(&me, Self::on_move))
                    .on_mouse_up(MouseButton::Left, on_event(&me, Self::on_up))
                    .into_any_element(),
                // Clicks on the bars and the text box don't reach the overlay.
                ("bar" | "text", Framed::Box(d)) => {
                    d.occlude().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).into_any_element()
                },
                ("tool", Framed::Button(b)) => {
                    let Some(tool) = ix.and_then(|i| Tool::ALL.get(i)).copied() else { return b.into_any_element() };
                    b.on_click(on_event(&me, move |this, _: &ClickEvent, window, cx| {
                        this.commit_text(window, cx);
                        this.tool = if this.tool == Some(tool) { None } else { Some(tool) };
                        cx.notify();
                    }))
                    .into_any_element()
                },
                ("undo", Framed::Button(b)) => b
                    .on_click(on_event(&me, |this, _: &ClickEvent, _, cx| {
                        this.doc.undo();
                        cx.notify();
                    }))
                    .into_any_element(),
                ("redo", Framed::Button(b)) => b
                    .on_click(on_event(&me, |this, _: &ClickEvent, _, cx| {
                        this.doc.redo();
                        cx.notify();
                    }))
                    .into_any_element(),
                ("cancel", Framed::Button(b)) => b.on_click(|_, _, cx| super::cancel(cx)).into_any_element(),
                ("copy", Framed::Button(b)) => b.on_click(on_event(&me, |this, _: &ClickEvent, _, cx| this.copy(cx))).into_any_element(),
                ("save", Framed::Button(b)) => b.on_click(on_event(&me, |this, _: &ClickEvent, _, cx| this.save_as(cx))).into_any_element(),
                ("add", Framed::Button(b)) => {
                    b.on_click(on_event(&me, |this, _: &ClickEvent, window, cx| this.add(window, cx))).into_any_element()
                },
                ("size", Framed::Box(d)) => {
                    let size = ix.unwrap_or(0) as u8;
                    d.id(("size", usize::from(size)))
                        .on_click(on_event(&me, move |this, _: &ClickEvent, _, cx| {
                            this.style.size = size;
                            cx.notify();
                        }))
                        .into_any_element()
                },
                ("color", Framed::Box(d)) => {
                    let Some(color) = ix.and_then(|i| COLORS.get(i)).copied() else { return d.into_any_element() };
                    d.id(("color", ix.unwrap_or(0)))
                        .on_click(on_event(&me, move |this, _: &ClickEvent, _, cx| {
                            this.style.color = color;
                            cx.notify();
                        }))
                        .into_any_element()
                },
                (_, f) => f.into_any_element(),
            },
            cx,
        )
    }
}

impl Overlay {
    /// The toolbar under the region `(lo, hi)` (above it, or inside, when
    /// there is no room), the style bar by it while a drawing tool is
    /// picked, and the text box being typed in.
    fn bars(&self, scope: Scope, (lo, hi): (P, P), (w, h): (f32, f32), cx: &App) -> Scope {
        let bar_h = style::el("shot_overlay.toolbar", cx).px("height").as_f32();
        let style_h = style::el("shot_overlay.style_bar", cx).px("height").as_f32();
        let styled = self.tool.is_some_and(|t| t != Tool::Mosaic);
        let need = bar_h + if styled { style_h + 6. } else { 0. } + 10.;
        let below = hi.y + need <= h;
        let bar_y = if below {
            hi.y + 8.
        } else if lo.y - need >= 0. {
            lo.y - 8. - bar_h
        } else {
            hi.y - 8. - bar_h
        };
        let right = (w - hi.x).min(w - BAR_W - 8.).max(8.);
        let tools = Tool::ALL
            .into_iter()
            .map(|tool| {
                let (icon, tip) = super::tool_icon(tool);
                Scope::new("tool").var("icon", icon).var("tooltip", tip).var("selected", self.tool == Some(tool))
            })
            .collect();
        let mut scope = scope
            .var("toolbar", true)
            .var("bar_top", f64::from(bar_y))
            .var("bar_right", f64::from(right))
            .list("tools", tools)
            .var("can_undo", self.doc.can_undo())
            .var("can_redo", self.doc.can_redo())
            .var("undo_key", super::keys("⌘Z", "Ctrl+Z"))
            .var("redo_key", super::keys("⌘⇧Z", "Ctrl+Shift+Z"))
            .var("copy_key", super::keys("⌘C", "Ctrl+C"))
            .var("save_key", super::keys("⌘S", "Ctrl+S"));
        if styled {
            let style_y = if below || bar_y > lo.y { bar_y + bar_h + 6. } else { bar_y - 6. - style_h };
            let (sizes, colors) = super::style_lists(&self.style);
            scope = scope.var("style_bar", true).var("style_top", f64::from(style_y)).list("sizes", sizes).list("colors", colors);
        }
        if let Some(edit) = &self.text {
            let font = self.style.font();
            let input = Input::new(&edit.input).appearance(false).text_color(hsla(self.style.color)).text_size(px(font));
            scope = scope
                .var("text", true)
                .var("text_left", f64::from(edit.at.x))
                .var("text_top", f64::from(edit.at.y - 4.))
                .var("text_width", f64::from((hi.x - edit.at.x).max(120.)))
                .slot("input", input);
        }
        scope
    }
}

/// What the canvas paints: the dimmed surroundings, the shapes clipped to
/// the region, and the region's frame.
struct Scene {
    shapes: Vec<Shape>,
    sel: Option<(P, P)>,
    /// The window to pick, framed without handles.
    hover: Option<(P, P)>,
    scale: f32,
    base: Arc<RgbaImage>,
    accent: Hsla,
}

impl Scene {
    fn paint(&self, window: &mut Window, cx: &mut App) {
        let size = window.viewport_size();
        let dim = hsla([0, 0, 0, 0x73]);
        let Some((lo, hi)) = self.sel.or(self.hover) else {
            window.paint_quad(fill(Bounds::new(point(px(0.), px(0.)), size), dim));
            return;
        };
        let (w, h) = (size.width, size.height);
        let rect = |x0: f32, y0: f32, x1: Pixels, y1: Pixels| Bounds::from_corners(point(px(x0), px(y0)), point(x1, y1));
        window.paint_quad(fill(rect(0., 0., w, px(lo.y)), dim));
        window.paint_quad(fill(rect(0., hi.y, w, h), dim));
        window.paint_quad(fill(rect(0., lo.y, px(lo.x), px(hi.y)), dim));
        window.paint_quad(fill(rect(hi.x, lo.y, w, px(hi.y)), dim));

        let region = Bounds::from_corners(to_point(lo), to_point(hi));
        window.with_content_mask(Some(ContentMask { bounds: region }), |window| {
            let view = View { origin: P::default(), k: 1. / self.scale, scale: self.scale };
            for shape in &self.shapes {
                paint::shape(view, shape, &self.base, window, cx);
            }
        });
        window.paint_quad(outline(region, self.accent, BorderStyle::Solid));
        if self.sel.is_none() {
            return;
        }
        for c in Overlay::handles((lo, hi)) {
            let b = Bounds::new(point(px(c.x - 4.), px(c.y - 4.)), gpui_kit::size(px(8.), px(8.)));
            window.paint_quad(fill(b, gpui_kit::white()));
            window.paint_quad(outline(b, self.accent, BorderStyle::Solid));
        }
    }
}
