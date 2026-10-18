//! The capture overlay: one borderless window per display showing its
//! frozen screen, where a region is picked and marked up.

use std::sync::Arc;

use gpui_kit::base::Selectable as _;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable as _, Sizable};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use image::RgbaImage;

use super::icons;
use super::model::{self, COLORS, Doc, P, Shape, Style, Tool, p};
use super::paint::{self, View, from_point, hsla, picture, to_point};
use super::raster::{self, Crop};

const HANDLE: f32 = 8.;
/// The toolbar's size, to place it before it is laid out.
const BAR_W: f32 = 600.;
const BAR_H: f32 = 40.;
const STYLE_H: f32 = 36.;

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
}

impl Overlay {
    pub fn new(base: RgbaImage, width_pt: f32, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let scale = if width_pt > 0. { base.width() as f32 / width_pt } else { 1. };
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
        }
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
        Scene { shapes, sel: self.sel, scale: self.scale, base: self.base.clone(), accent: hsla(COLORS[1]) }
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

    fn tool_button(&self, tool: Tool, cx: &mut Context<Self>) -> Button {
        let (svg, tip) = icons::tool(tool);
        Button::new(SharedString::from(format!("tool-{tool:?}")))
            .icon(icons::icon(svg))
            .ghost()
            .small()
            .selected(self.tool == Some(tool))
            .tooltip(tip)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.commit_text(window, cx);
                this.tool = if this.tool == Some(tool) { None } else { Some(tool) };
                cx.notify();
            }))
    }

    fn action_button(id: &'static str, svg: &'static str, tip: &'static str) -> Button {
        Button::new(id).icon(icons::icon(svg)).ghost().small().tooltip(tip)
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let border = theme.border;
        let sep = move || div().w(px(1.)).h(px(20.)).mx_1().bg(border);
        let mut bar = div()
            .flex()
            .items_center()
            .gap_0p5()
            .h(px(BAR_H))
            .px_2()
            .rounded_lg()
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border)
            .shadow_lg()
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        for tool in Tool::ALL {
            bar = bar.child(self.tool_button(tool, cx));
        }
        bar.child(sep())
            .child(
                Self::action_button("undo", icons::UNDO, super::keys("Undo (⌘Z)", "Undo (Ctrl+Z)"))
                    .disabled(!self.doc.can_undo())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.doc.undo();
                        cx.notify();
                    })),
            )
            .child(
                Self::action_button("redo", icons::REDO, super::keys("Redo (⌘⇧Z)", "Redo (Ctrl+Shift+Z)"))
                    .disabled(!self.doc.can_redo())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.doc.redo();
                        cx.notify();
                    })),
            )
            .child(sep())
            .child(Self::action_button("cancel", icons::CLOSE, "Cancel (Esc)").on_click(|_, _, cx| super::cancel(cx)))
            .child(
                Self::action_button("copy", icons::COPY, super::keys("Copy (⌘C)", "Copy (Ctrl+C)"))
                    .on_click(cx.listener(|this, _, _, cx| this.copy(cx))),
            )
            .child(
                Self::action_button("save", icons::SAVE, super::keys("Save As… (⌘S)", "Save As… (Ctrl+S)"))
                    .on_click(cx.listener(|this, _, _, cx| this.save_as(cx))),
            )
            .child(
                Button::new("add")
                    .icon(icons::icon(icons::ADD))
                    .primary()
                    .small()
                    .label("Add")
                    .tooltip("Add to the tray (↵)")
                    .on_click(cx.listener(|this, _, window, cx| this.add(window, cx))),
            )
    }

    fn style_bar(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let mut bar = div()
            .flex()
            .items_center()
            .gap_1()
            .h(px(STYLE_H))
            .px_2()
            .rounded_lg()
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border)
            .shadow_lg()
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        for size in 0..3u8 {
            let dot = 4. + 3. * f32::from(size);
            let selected = self.style.size == size;
            bar = bar.child(
                div()
                    .id(("size", usize::from(size)))
                    .size(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .when(selected, |d| d.bg(theme.accent))
                    .hover(|d| d.bg(theme.secondary_hover))
                    .child(div().size(px(dot)).rounded_full().bg(theme.foreground))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.style.size = size;
                        cx.notify();
                    })),
            );
        }
        bar = bar.child(div().w(px(1.)).h(px(18.)).mx_1().bg(theme.border));
        for (i, color) in COLORS.into_iter().enumerate() {
            let selected = self.style.color == color;
            bar = bar.child(
                div()
                    .id(("color", i))
                    .size(px(18.))
                    .rounded_full()
                    .bg(hsla(color))
                    .border_2()
                    .border_color(if selected { theme.foreground } else { theme.border })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.style.color = color;
                        cx.notify();
                    })),
            );
        }
        bar
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
        let mut root = div()
            .id("shot-overlay")
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .bg(gpui_kit::black())
            .cursor(CursorStyle::Crosshair)
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_down))
            .on_mouse_move(cx.listener(Self::on_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_up))
            .child(img(ImageSource::Render(self.picture.clone())).absolute().top_0().left_0().w(size.width).h(size.height))
            .child(canvas(|_, _, _| {}, move |_, _, window, cx| scene.paint(window, cx)).absolute().top_0().left_0().size_full());

        let Some((lo, hi)) = self.sel else {
            return root.child(
                div().absolute().top(px(h / 2. - 20.)).left_0().w(size.width).flex().justify_center().child(
                    div()
                        .px_3()
                        .py_1p5()
                        .rounded_md()
                        .bg(hsla([0, 0, 0, 0xB0]))
                        .text_color(gpui_kit::white())
                        .text_sm()
                        .child("Drag to capture a region · Esc to cancel"),
                ),
            );
        };
        // Size label above the region (inside it at the top of the screen).
        let label_y = if lo.y >= 30. { lo.y - 28. } else { lo.y + 6. };
        root = root.child(
            div()
                .absolute()
                .left(px(lo.x))
                .top(px(label_y))
                .px_2()
                .py_0p5()
                .rounded_md()
                .bg(hsla([0x2F, 0x6B, 0xFF, 0xFF]))
                .text_color(gpui_kit::white())
                .text_xs()
                .child(format!("{} × {}", (hi.x - lo.x).round(), (hi.y - lo.y).round())),
        );
        if self.drag.as_ref().is_some_and(|d| matches!(d, Drag::Select { .. })) {
            return root;
        }
        // The toolbar under the region; above it, or inside, when there is no room.
        let styled = self.tool.is_some_and(|t| t != Tool::Mosaic);
        let need = BAR_H + if styled { STYLE_H + 6. } else { 0. } + 10.;
        let below = hi.y + need <= h;
        let bar_y = if below {
            hi.y + 8.
        } else if lo.y - need >= 0. {
            lo.y - 8. - BAR_H
        } else {
            hi.y - 8. - BAR_H
        };
        let right = (w - hi.x).min(w - BAR_W - 8.).max(8.);
        root = root.child(div().absolute().top(px(bar_y)).right(px(right)).child(self.toolbar(cx)));
        if styled {
            let style_y = if below || bar_y > lo.y { bar_y + BAR_H + 6. } else { bar_y - 6. - STYLE_H };
            root = root.child(div().absolute().top(px(style_y)).right(px(right)).child(self.style_bar(cx)));
        }
        if let Some(edit) = &self.text {
            let font = self.style.font();
            root = root.child(
                div()
                    .absolute()
                    .left(px(edit.at.x))
                    .top(px(edit.at.y - 4.))
                    .w(px((hi.x - edit.at.x).max(120.)))
                    .occlude()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(Input::new(&edit.input).appearance(false).text_color(hsla(self.style.color)).text_size(px(font))),
            );
        }
        root
    }
}

/// What the canvas paints: the dimmed surroundings, the shapes clipped to
/// the region, and the region's frame.
struct Scene {
    shapes: Vec<Shape>,
    sel: Option<(P, P)>,
    scale: f32,
    base: Arc<RgbaImage>,
    accent: Hsla,
}

impl Scene {
    fn paint(&self, window: &mut Window, cx: &mut App) {
        let size = window.viewport_size();
        let dim = hsla([0, 0, 0, 0x73]);
        let Some((lo, hi)) = self.sel else {
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
        for c in Overlay::handles((lo, hi)) {
            let b = Bounds::new(point(px(c.x - 4.), px(c.y - 4.)), gpui_kit::size(px(8.), px(8.)));
            window.paint_quad(fill(b, gpui_kit::white()));
            window.paint_quad(outline(b, self.accent, BorderStyle::Solid));
        }
    }
}
