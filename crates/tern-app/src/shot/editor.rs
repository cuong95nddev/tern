//! The screenshot editor: the tray's captures as the tabs of one window. A
//! part of one can be copied and pasted into another, where it is moved and
//! scaled before it lands; pages are marked up, cropped, or grown by what
//! is pasted past their edges. Closing a tab drops that capture; Done
//! hands the rest back to the tray.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::ActiveTheme;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::*;
use image::RgbaImage;

use super::model::{self, COLORS, Doc, P, Shape, Style, Tool, p};
use super::paint::{self, View, from_point, hsla, picture};
use super::raster::{self, Crop};
use crate::template::{self, Framed, Scope, on_event};

/// Grab distance of a handle, in window points.
const HANDLE: f32 = 8.;
/// Room around the page.
const PAD: f32 = 24.;
/// A new page's size, in points.
const BLANK: (f32, f32) = (960., 640.);
/// A page's undo history keeps at most this many bytes of images other than
/// its current one (a step that only adds shapes shares the image): the
/// oldest steps go first. About seven pastes or erases on a 4K capture.
const MAX_UNDO_BYTES: usize = 256 << 20;

#[derive(Default)]
struct EditorData {
    open: Option<(AnyWindowHandle, WeakEntity<Editor>)>,
    /// What was last copied (PNG), for when the system clipboard has no image.
    clip: Option<Arc<Vec<u8>>>,
}

impl Global for EditorData {}

/// Whether the editor is open.
pub fn is_open(cx: &mut App) -> bool {
    cx.default_global::<EditorData>().open.is_some()
}

/// Open the editor on the tray's captures, at tab `at`, or bring it forward.
pub fn open(shots: Vec<PathBuf>, at: usize, cx: &mut App) {
    if let Some((window, _)) = cx.default_global::<EditorData>().open {
        let _ = window.update(cx, |_, w, _| w.activate_window());
        return;
    }
    let Some(display) = cx.primary_display() else { return };
    let area = display.bounds();
    let size = gpui_kit::size(px((area.size.width.as_f32() * 0.8).min(1400.)), px((area.size.height.as_f32() * 0.8).min(900.)));
    let opened = gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(Some(display.id()), size, cx))),
            titlebar: Some(TitlebarOptions { title: Some("Edit Screenshots".into()), ..Default::default() }),
            display_id: Some(display.id()),
            focus: true,
            show: true,
            window_background: crate::style::backdrop(cx).appearance,
            ..Default::default()
        },
        cx,
        |window, cx| cx.new(|cx| Editor::new(shots, at, window, cx)),
    );
    match opened {
        Ok((handle, editor)) => {
            cx.default_global::<EditorData>().open = Some((handle, editor.downgrade()));
            let _ = handle.update(cx, |_, w, _| w.activate_window());
        },
        Err(e) => log::error!("failed to open the screenshot editor: {e}"),
    }
}

/// The editor window while it is open.
#[cfg(debug_assertions)]
pub fn window(cx: &mut App) -> Option<AnyWindowHandle> {
    cx.default_global::<EditorData>().open.as_ref().map(|(w, _)| *w)
}

#[cfg(debug_assertions)]
pub fn debug_state(cx: &mut App) -> String {
    let editor = cx.default_global::<EditorData>().open.as_ref().and_then(|(_, e)| e.upgrade());
    editor.map_or_else(|| "editor open=false".into(), |e| e.read(cx).debug_state())
}

/// A new capture while the editor is open becomes its last tab. Returns
/// whether it was open.
pub fn add(path: PathBuf, cx: &mut App) -> bool {
    let Some((window, editor)) = cx.default_global::<EditorData>().open.clone() else { return false };
    let Some(editor) = editor.upgrade() else { return false };
    let _ = window.update(cx, |_, window, cx| {
        editor.update(cx, |e, cx| e.add_file(path, window, cx));
        window.activate_window();
    });
    true
}

/// One tab: an image with shapes on it, and its undo history.
struct Page {
    name: SharedString,
    /// The tray's file it was opened from; `None` for a new page.
    source: Option<PathBuf>,
    base: Arc<RgbaImage>,
    picture: Arc<RenderImage>,
    doc: Doc,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    changed: bool,
}

#[derive(Clone)]
struct Snapshot {
    base: Arc<RgbaImage>,
    shapes: Vec<Shape>,
}

impl Page {
    fn new(name: SharedString, source: Option<PathBuf>, base: RgbaImage, scale: f32) -> Self {
        Page {
            name,
            source,
            picture: picture(&base),
            base: Arc::new(base),
            doc: Doc::new(scale),
            undo: Vec::new(),
            redo: Vec::new(),
            changed: false,
        }
    }

    fn size(&self) -> P {
        p(self.base.width() as f32, self.base.height() as f32)
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot { base: self.base.clone(), shapes: self.doc.shapes.clone() }
    }

    fn restore(&mut self, s: Snapshot) {
        if !Arc::ptr_eq(&s.base, &self.base) {
            self.picture = picture(&s.base);
            self.base = s.base;
        }
        self.doc.shapes = s.shapes;
    }

    /// Remember the page before a change, for undo.
    fn checkpoint(&mut self) {
        self.undo.push(self.snapshot());
        self.redo.clear();
        self.changed = true;
        while self.undo.len() > 1 && self.undo_bytes() > MAX_UNDO_BYTES {
            self.undo.remove(0);
        }
    }

    /// Bytes of the images the undo history holds, other than the current one.
    fn undo_bytes(&self) -> usize {
        let mut seen = vec![Arc::as_ptr(&self.base)];
        let mut bytes = 0;
        for s in &self.undo {
            let ptr = Arc::as_ptr(&s.base);
            if !seen.contains(&ptr) {
                seen.push(ptr);
                bytes += s.base.as_raw().len();
            }
        }
        bytes
    }

    fn undo(&mut self) -> bool {
        let Some(s) = self.undo.pop() else { return false };
        self.redo.push(self.snapshot());
        self.restore(s);
        true
    }

    fn redo(&mut self) -> bool {
        let Some(s) = self.redo.pop() else { return false };
        self.undo.push(self.snapshot());
        self.restore(s);
        true
    }

    fn push(&mut self, shape: Shape) {
        self.checkpoint();
        self.doc.push(shape);
    }

    /// Replace the image with `img`, which has the shapes in it.
    fn set_flat(&mut self, img: RgbaImage) {
        self.checkpoint();
        self.picture = picture(&img);
        self.base = Arc::new(img);
        self.doc.shapes.clear();
    }

    /// The page as it exports: its image with its shapes drawn in.
    fn flat(&self) -> RgbaImage {
        raster::flatten(&self.base, &self.doc, raster::font())
    }

    /// The part of the page in `sel` (image pixels).
    fn region(&self, sel: (P, P)) -> RgbaImage {
        raster::render(&self.base, crop(sel), &self.doc, raster::font())
    }

    /// Lay a pasted image down where it floats.
    fn place(&mut self, f: &Floating) {
        let (lo, hi) = f.rect;
        let (w, h) = ((hi.x - lo.x).round().max(1.) as u32, (hi.y - lo.y).round().max(1.) as u32);
        let (img, _) = raster::place(&self.flat(), &f.img, lo.x.round() as i64, lo.y.round() as i64, w, h);
        self.set_flat(img);
    }

    /// Fill `sel` with blank paper.
    fn erase(&mut self, sel: (P, P)) {
        let c = crop(sel);
        let mut img = self.flat();
        let (iw, ih) = img.dimensions();
        for y in c.y.min(ih)..(c.y + c.h).min(ih) {
            for x in c.x.min(iw)..(c.x + c.w).min(iw) {
                img.put_pixel(x, y, image::Rgba(raster::PAPER));
            }
        }
        self.set_flat(img);
    }
}

/// The pixels `sel` covers.
fn crop(sel: (P, P)) -> Crop {
    let (lo, hi) = sel;
    Crop {
        x: lo.x.max(0.).round() as u32,
        y: lo.y.max(0.).round() as u32,
        w: (hi.x - lo.x.max(0.)).round().max(1.) as u32,
        h: (hi.y - lo.y.max(0.)).round().max(1.) as u32,
    }
}

/// A pasted image not laid down yet, in image pixels.
struct Floating {
    img: Arc<RgbaImage>,
    picture: Arc<RenderImage>,
    rect: (P, P),
}

enum Drag {
    /// Picking a region.
    Select {
        start: P,
    },
    MoveFloat {
        grab: P,
        orig: (P, P),
    },
    /// Handle index: 0 top left, then clockwise.
    ScaleFloat {
        handle: usize,
        orig: (P, P),
    },
    /// A shape in image pixels.
    Draw(Shape),
}

struct TextEdit {
    /// Top left, in image pixels.
    at: P,
    input: Entity<InputState>,
    _sub: Subscription,
}

/// The eight handles of a box: top left, then clockwise.
fn handles((lo, hi): (P, P)) -> [P; 8] {
    let mid = (lo + hi).scale(0.5);
    [lo, p(mid.x, lo.y), p(hi.x, lo.y), p(hi.x, mid.y), hi, p(mid.x, hi.y), p(lo.x, hi.y), p(lo.x, mid.y)]
}

/// `orig` with `handle` dragged to `q`. Corners keep the aspect ratio, the
/// opposite corner staying put; edges move alone.
fn scale_box(handle: usize, orig: (P, P), q: P) -> (P, P) {
    let (mut lo, mut hi) = orig;
    if handle.is_multiple_of(2) {
        let fixed = handles(orig)[(handle + 4) % 8];
        let aspect = (hi.x - lo.x) / (hi.y - lo.y).max(1e-3);
        let d = q - fixed;
        let w = d.x.abs().max(d.y.abs() * aspect).max(2.);
        let h = w / aspect;
        // Flipping past the fixed corner mirrors the box, not the image.
        let corner = fixed + p(w.copysign(d.x), h.copysign(d.y));
        return model::norm(fixed, corner);
    }
    match handle {
        1 => lo.y = q.y.min(hi.y - 1.),
        3 => hi.x = q.x.max(lo.x + 1.),
        5 => hi.y = q.y.max(lo.y + 1.),
        _ => lo.x = q.x.min(hi.x - 1.),
    }
    (lo, hi)
}

fn inside((lo, hi): (P, P), q: P) -> bool {
    q.x >= lo.x && q.x <= hi.x && q.y >= lo.y && q.y <= hi.y
}

/// The page centred in `bounds`, shrunk to fit, never past its pixels.
fn fit(bounds: Bounds<Pixels>, page: P, scale: f32) -> View {
    let (bw, bh) = (bounds.size.width.as_f32(), bounds.size.height.as_f32());
    let k = ((bw - 2. * PAD).max(1.) / page.x).min((bh - 2. * PAD).max(1.) / page.y).min(1. / scale);
    let origin = from_point(bounds.origin) + p((bw - page.x * k) / 2., (bh - page.y * k) / 2.);
    View { origin, k, scale }
}

pub struct Editor {
    focus: FocusHandle,
    pages: Vec<Page>,
    active: usize,
    /// `None` selects regions and moves what is pasted.
    tool: Option<Tool>,
    style: Style,
    /// The selected region of the page, in image pixels.
    sel: Option<(P, P)>,
    floating: Option<Floating>,
    drag: Option<Drag>,
    text: Option<TextEdit>,
    /// Where the page was last painted.
    view: Rc<Cell<View>>,
    scale: f32,
    /// New pages so far, to name the next.
    blanks: usize,
    done: bool,
    /// The pictures the last render used: one no longer used is dropped
    /// from the window's sprite atlas, which would keep it otherwise.
    shown: Vec<Arc<RenderImage>>,
    /// What the desktop does behind the window, as last set.
    backdrop: crate::style::Backdrop,
}

impl Editor {
    fn new(shots: Vec<PathBuf>, at: usize, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let scale = window.scale_factor();
        let me = cx.entity().downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            if let Some(me) = me.upgrade() {
                me.update(cx, |e, cx| e.hand_back(cx));
            }
            true
        });
        let mut this = Self {
            focus,
            pages: Vec::new(),
            active: 0,
            tool: None,
            style: Style::default(),
            sel: None,
            floating: None,
            drag: None,
            text: None,
            view: Rc::new(Cell::new(View { origin: P::default(), k: 1., scale })),
            scale,
            blanks: 0,
            done: false,
            shown: Vec::new(),
            backdrop: crate::style::Backdrop::default(),
        };
        for path in shots {
            this.load(path);
        }
        this.active = at.min(this.pages.len().saturating_sub(1));
        this
    }

    fn load(&mut self, path: PathBuf) {
        match image::open(&path) {
            Ok(img) => {
                let name = format!("Screenshot {}", self.pages.len() + 1 - self.blanks).into();
                self.pages.push(Page::new(name, Some(path), img.to_rgba8(), self.scale));
            },
            Err(e) => log::warn!("can't open {}: {e}", path.display()),
        }
    }

    fn add_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.pages.len();
        self.load(path);
        if self.pages.len() > n {
            self.switch(n, window, cx);
        }
    }

    fn page(&self) -> Option<&Page> {
        self.pages.get(self.active)
    }

    fn page_mut(&mut self) -> Option<&mut Page> {
        self.pages.get_mut(self.active)
    }

    /// Finish what is in progress on the page: text being typed, a paste.
    fn settle(&mut self, cx: &mut Context<Self>) {
        if let Some(edit) = self.text.take() {
            let text = edit.input.read(cx).value().trim_end().to_string();
            if !text.is_empty() {
                let style = self.style;
                if let Some(page) = self.page_mut() {
                    page.push(Shape::Text { at: edit.at, text, style });
                }
            }
        }
        if let Some(f) = self.floating.take()
            && let Some(page) = self.page_mut()
        {
            page.place(&f);
        }
        self.drag = None;
    }

    fn switch(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.settle(cx);
        self.sel = None;
        self.active = ix.min(self.pages.len().saturating_sub(1));
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix == self.active {
            self.text = None;
            self.floating = None;
            self.drag = None;
            self.sel = None;
        }
        if ix < self.pages.len() {
            self.pages.remove(ix);
        }
        if ix < self.active || self.active >= self.pages.len() {
            self.active = self.active.saturating_sub(1);
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn new_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settle(cx);
        self.blanks += 1;
        let img = raster::blank((BLANK.0 * self.scale) as u32, (BLANK.1 * self.scale) as u32);
        self.pages.push(Page::new(format!("New {}", self.blanks).into(), None, img, self.scale));
        let last = self.pages.len() - 1;
        self.switch(last, window, cx);
    }

    fn on_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if self.text.is_some() {
            self.settle(cx);
            return cx.notify();
        }
        if self.page().is_none() {
            return;
        }
        let view = self.view.get();
        let at = view.px(from_point(ev.position));
        if let Some(f) = &self.floating {
            let reach = HANDLE / view.k;
            let hit = handles(f.rect).iter().position(|h| (at.x - h.x).abs() <= reach && (at.y - h.y).abs() <= reach);
            self.drag = match hit {
                Some(handle) => Some(Drag::ScaleFloat { handle, orig: f.rect }),
                None if inside(f.rect, at) => Some(Drag::MoveFloat { grab: at, orig: f.rect }),
                None => None,
            };
            if self.drag.is_some() {
                return cx.notify();
            }
            self.settle(cx);
        }
        let style = self.style;
        self.drag = match self.tool {
            None => {
                self.sel = None;
                Some(Drag::Select { start: at })
            },
            Some(Tool::Rect) => Some(Drag::Draw(Shape::Rect { a: at, b: at, style })),
            Some(Tool::Ellipse) => Some(Drag::Draw(Shape::Ellipse { a: at, b: at, style })),
            Some(Tool::Line) => Some(Drag::Draw(Shape::Line { a: at, b: at, style })),
            Some(Tool::Arrow) => Some(Drag::Draw(Shape::Arrow { a: at, b: at, style })),
            Some(Tool::Pen) => Some(Drag::Draw(Shape::Pen { points: vec![at], style })),
            Some(Tool::Mosaic) => Some(Drag::Draw(Shape::Mosaic { a: at, b: at })),
            Some(Tool::Marker) => {
                if let Some(page) = self.page_mut() {
                    let n = page.doc.next_marker();
                    page.push(Shape::Marker { at, n, style });
                }
                None
            },
            Some(Tool::Text) => {
                self.start_text(at, window, cx);
                None
            },
        };
        cx.notify();
    }

    fn on_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = &mut self.drag else { return };
        let at = self.view.get().px(from_point(ev.position));
        match drag {
            Drag::Select { start } => {
                let size = self.pages.get(self.active).map_or(P::default(), Page::size);
                let clamp = |q: P| p(q.x.clamp(0., size.x), q.y.clamp(0., size.y));
                self.sel = Some(model::norm(clamp(*start), clamp(at)));
            },
            Drag::MoveFloat { grab, orig } => {
                let d = at - *grab;
                if let Some(f) = &mut self.floating {
                    f.rect = (orig.0 + d, orig.1 + d);
                }
            },
            Drag::ScaleFloat { handle, orig } => {
                if let Some(f) = &mut self.floating {
                    f.rect = scale_box(*handle, *orig, at);
                }
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
                if self.sel.is_some_and(|(lo, hi)| hi.x - lo.x < 3. || hi.y - lo.y < 3.) {
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
                if keep && let Some(page) = self.page_mut() {
                    page.push(shape);
                }
            },
            _ => {},
        }
        cx.notify();
    }

    fn start_text(&mut self, at: P, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx));
        let sub = cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { .. } | InputEvent::Blur => {
                this.settle(cx);
                // Later, so the Enter that committed doesn't also reach the editor.
                cx.defer_in(window, |this, window, cx| window.focus(&this.focus, cx));
                cx.notify();
            },
            _ => {},
        });
        // After this click, which focuses the editor when it ends.
        let handle = input.focus_handle(cx);
        window.defer(cx, move |window, cx| handle.focus(window, cx));
        self.text = Some(TextEdit { at, input, _sub: sub });
    }

    fn set_tool(&mut self, tool: Option<Tool>, cx: &mut Context<Self>) {
        self.settle(cx);
        self.tool = tool;
        if tool.is_some() {
            self.sel = None;
        }
        cx.notify();
    }

    fn undo(&mut self, redo: bool, cx: &mut Context<Self>) {
        // Undoing a paste that hasn't landed drops it.
        if !redo && (self.floating.take().is_some() || self.text.take().is_some()) {
            return cx.notify();
        }
        self.settle(cx);
        self.sel = None;
        if let Some(page) = self.page_mut() {
            if redo {
                page.redo()
            } else {
                page.undo()
            };
        }
        cx.notify();
    }

    /// The selection, the floating paste, or else the whole page.
    fn copy(&mut self, cx: &mut Context<Self>) -> Option<RgbaImage> {
        let img = if let Some(f) = &self.floating {
            let (lo, hi) = f.rect;
            let (w, h) = ((hi.x - lo.x).round().max(1.) as u32, (hi.y - lo.y).round().max(1.) as u32);
            image::imageops::resize(f.img.as_ref(), w, h, image::imageops::FilterType::Triangle)
        } else {
            self.settle(cx);
            let page = self.page()?;
            match self.sel {
                Some(sel) => page.region(sel),
                None => page.flat(),
            }
        };
        match super::overlay::png_bytes(&img) {
            Ok(bytes) => {
                cx.default_global::<EditorData>().clip = Some(Arc::new(bytes.clone()));
                cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(ImageFormat::Png, bytes)));
            },
            Err(e) => log::warn!("can't copy the image: {e}"),
        }
        Some(img)
    }

    fn cut(&mut self, cx: &mut Context<Self>) {
        let Some(sel) = self.sel else { return };
        if self.floating.is_some() || self.copy(cx).is_none() {
            return;
        }
        if let Some(page) = self.page_mut() {
            page.erase(sel);
        }
        self.sel = None;
        cx.notify();
    }

    fn delete(&mut self, cx: &mut Context<Self>) {
        if self.floating.take().is_none()
            && let Some(sel) = self.sel.take()
            && let Some(page) = self.page_mut()
        {
            page.erase(sel);
        }
        cx.notify();
    }

    /// An image on the system clipboard (or files of images), else what
    /// was last copied here.
    fn clipboard_image(cx: &mut App) -> Option<RgbaImage> {
        let from_system = cx.read_from_clipboard().and_then(|item| {
            item.entries().iter().find_map(|e| match e {
                ClipboardEntry::Image(img) => raster::decode(img.bytes()),
                ClipboardEntry::ExternalPaths(paths) => paths.paths().iter().find_map(|p| image::open(p).ok().map(|i| i.to_rgba8())),
                ClipboardEntry::String(_) => None,
            })
        });
        from_system.or_else(|| cx.default_global::<EditorData>().clip.clone().and_then(|png| raster::decode(&png)))
    }

    fn paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(img) = Self::clipboard_image(cx) else { return };
        self.settle(cx);
        if self.page().is_none() {
            // Nothing to paste into: the image is the new page.
            self.blanks += 1;
            self.pages.push(Page::new(format!("New {}", self.blanks).into(), None, img, self.scale));
            if let Some(page) = self.pages.last_mut() {
                page.changed = true;
            }
            let last = self.pages.len() - 1;
            return self.switch(last, window, cx);
        }
        let page = self.page().map_or(P::default(), Page::size);
        let size = p(img.width() as f32, img.height() as f32);
        // Into the selected region's corner, else the middle of the page.
        let lo = match self.sel {
            Some((lo, _)) => lo,
            None => p(((page.x - size.x) / 2.).max(0.).round(), ((page.y - size.y) / 2.).max(0.).round()),
        };
        self.sel = None;
        self.tool = None;
        self.floating = Some(Floating { picture: picture(&img), img: Arc::new(img), rect: (lo, lo + size) });
        cx.notify();
    }

    fn crop(&mut self, cx: &mut Context<Self>) {
        self.settle(cx);
        let Some(sel) = self.sel.take() else { return };
        if let Some(page) = self.page_mut() {
            let img = page.region(sel);
            page.set_flat(img);
        }
        cx.notify();
    }

    fn save_as(&mut self, cx: &mut Context<Self>) {
        self.settle(cx);
        let Some(img) = self.page().map(Page::flat) else { return };
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

    /// Give the pages back to the tray, edited ones as new files, in tab
    /// order. Untouched new pages are left out.
    fn hand_back(&mut self, cx: &mut Context<Self>) {
        if self.done {
            return;
        }
        self.done = true;
        self.settle(cx);
        let mut shots = Vec::new();
        for page in &self.pages {
            match (&page.source, page.changed) {
                (Some(path), false) => shots.push(path.clone()),
                (None, false) => {},
                _ => match super::grab::save(&page.flat(), &super::grab::dir()) {
                    Ok(path) => shots.push(path),
                    Err(e) => {
                        super::notify("Couldn't save an edited screenshot", &e, cx);
                        shots.extend(page.source.clone());
                    },
                },
            }
        }
        cx.default_global::<EditorData>().open = None;
        cx.defer(move |cx| super::tray::set_shots(shots, cx));
    }

    fn done(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.hand_back(cx);
        window.remove_window();
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) {
            return;
        }
        let ks = &ev.keystroke;
        let m = ks.modifiers;
        match ks.key.as_str() {
            "escape" if self.drag.is_some() => self.drag = None,
            "escape" if self.floating.is_some() => self.floating = None,
            "escape" => self.sel = None,
            "enter" => self.settle(cx),
            "delete" | "backspace" => self.delete(cx),
            "z" if m.secondary() && m.shift => self.undo(true, cx),
            "z" if m.secondary() => self.undo(false, cx),
            "y" if m.secondary() => self.undo(true, cx),
            "c" if m.secondary() => {
                self.copy(cx);
            },
            "x" if m.secondary() => self.cut(cx),
            "v" if m.secondary() => self.paste(window, cx),
            "s" if m.secondary() => self.save_as(cx),
            "w" if m.secondary() => self.close_tab(self.active, window, cx),
            "t" if m.secondary() => self.new_page(window, cx),
            "tab" if m.control => {
                let n = self.pages.len().max(1);
                let next = if m.shift { self.active + n - 1 } else { self.active + 1 } % n;
                self.switch(next, window, cx);
            },
            key if !m.platform && !m.control && !m.alt => {
                let tool = match key {
                    "v" => None,
                    "r" => Some(Tool::Rect),
                    "e" => Some(Tool::Ellipse),
                    "l" => Some(Tool::Line),
                    "a" => Some(Tool::Arrow),
                    "p" => Some(Tool::Pen),
                    "t" => Some(Tool::Text),
                    "n" => Some(Tool::Marker),
                    "m" => Some(Tool::Mosaic),
                    _ => return,
                };
                self.set_tool(tool, cx);
            },
            _ => return,
        }
        cx.notify();
    }

    #[cfg(debug_assertions)]
    pub fn debug_state(&self) -> String {
        let sizes: Vec<_> = self.pages.iter().map(|p| format!("{}:{}x{}", p.name, p.base.width(), p.base.height())).collect();
        format!(
            "editor active={} pages={sizes:?} tool={:?} sel={:?} floating={:?} shapes={}",
            self.active,
            self.tool,
            self.sel,
            self.floating.as_ref().map(|f| f.rect),
            self.page().map_or(0, |p| p.doc.shapes.len())
        )
    }

    fn scene(&self, cx: &App) -> Option<Scene> {
        let page = self.page()?;
        let mut shapes = page.doc.shapes.clone();
        if let Some(Drag::Draw(s)) = &self.drag {
            shapes.push(s.clone());
        }
        Some(Scene {
            base: page.base.clone(),
            picture: page.picture.clone(),
            shapes,
            floating: self.floating.as_ref().map(|f| (f.picture.clone(), f.rect)),
            sel: self.sel,
            accent: hsla(COLORS[1]),
            paper: cx.theme().border,
        })
    }
}

impl Focusable for Editor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Editor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::style::sync_backdrop(&mut self.backdrop, window, cx);
        let live: Vec<_> = self.pages.iter().map(|p| p.picture.clone()).chain(self.floating.as_ref().map(|f| f.picture.clone())).collect();
        for old in std::mem::take(&mut self.shown) {
            if !live.iter().any(|l| Arc::ptr_eq(l, &old)) {
                let _ = window.drop_image(old);
            }
        }
        self.shown = live;
        let page = self.page();
        let (can_undo, can_redo) = page.map_or((false, false), |p| (!p.undo.is_empty() || self.floating.is_some(), !p.redo.is_empty()));
        // Select first, then the drawing tools.
        let tools = std::iter::once(None)
            .chain(Tool::ALL.into_iter().map(Some))
            .map(|tool| {
                let (icon, tip) = tool.map_or(("shot-select", "Select (V)"), super::tool_icon);
                Scope::new("tool").var("icon", icon).var("tooltip", tip).var("selected", self.tool == tool)
            })
            .collect();
        let (sizes, colors) = super::style_lists(&self.style);
        let pages = self
            .pages
            .iter()
            .enumerate()
            .map(|(i, p)| Scope::new("page").var("name", p.name.clone()).var("active", i == self.active))
            .collect();
        let mut scope = Scope::new("shot-editor")
            .list("tools", tools)
            .list("sizes", sizes)
            .list("colors", colors)
            .list("pages", pages)
            .var("pages", page.is_some())
            .var("selection", self.sel.is_some())
            .var("can_undo", can_undo)
            .var("can_redo", can_redo)
            .var("floating", self.floating.is_some())
            .var("tool", self.tool.is_some())
            .var("undo_key", super::keys("⌘Z", "Ctrl+Z"))
            .var("redo_key", super::keys("⌘⇧Z", "Ctrl+Y"))
            .var("copy_key", super::keys("⌘C", "Ctrl+C"))
            .var("cut_key", super::keys("⌘X", "Ctrl+X"))
            .var("paste_key", super::keys("⌘V", "Ctrl+V"))
            .var("save_key", super::keys("⌘S", "Ctrl+S"))
            .var("close_key", super::keys("⌘W", "Ctrl+W"))
            .var("new_key", super::keys("⌘T", "Ctrl+T"));
        if let Some(scene) = self.scene(cx) {
            let cell = self.view.clone();
            let size = p(scene.base.width() as f32, scene.base.height() as f32);
            let scale = page.map_or(self.scale, |p| p.doc.scale);
            let painted = canvas(
                move |bounds, window, _| {
                    let view = fit(bounds, size, scale);
                    if cell.replace(view) != view {
                        // The text box is placed from the view.
                        window.refresh();
                    }
                    view
                },
                move |bounds, view, window, cx| scene.paint(bounds, view, window, cx),
            )
            .absolute()
            .inset_0();
            scope = scope.slot("scene", painted);
        }
        // In window points, as the root is the window.
        if let Some(edit) = &self.text {
            let view = self.view.get();
            let at = view.pt(edit.at);
            let font = self.style.font() * view.zoom();
            let input = Input::new(&edit.input).appearance(false).text_color(hsla(self.style.color)).text_size(px(font));
            scope = scope
                .var("text", true)
                .var("text_left", f64::from(at.x.as_f32()))
                .var("text_top", f64::from(at.y.as_f32() - 4.))
                .slot("input", input);
        }
        let me = cx.entity().downgrade();
        let focus = self.focus.clone();
        let cursor = if self.tool.is_some() { CursorStyle::Crosshair } else { CursorStyle::Arrow };
        template::render(
            "shot_editor",
            scope,
            &mut |name, ix, f| match (name, f) {
                ("editor", Framed::Box(d)) => d
                    .id("shot-editor")
                    .track_focus(&focus)
                    .key_context("ShotEditor")
                    .on_key_down(on_event(&me, Self::on_key))
                    .on_mouse_move(on_event(&me, Self::on_move))
                    .on_mouse_up(MouseButton::Left, on_event(&me, Self::on_up))
                    .on_mouse_up_out(MouseButton::Left, on_event(&me, Self::on_up))
                    .into_any_element(),
                ("page", Framed::Box(d)) => {
                    d.id("page-area").cursor(cursor).on_mouse_down(MouseButton::Left, on_event(&me, Self::on_down)).into_any_element()
                },
                ("text", Framed::Box(d)) => {
                    d.occlude().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).into_any_element()
                },
                ("tool", Framed::Button(b)) => {
                    let tool = ix.and_then(|i| i.checked_sub(1)).and_then(|i| Tool::ALL.get(i)).copied();
                    b.on_click(on_event(&me, move |this, _: &ClickEvent, window, cx| {
                        this.set_tool(tool, cx);
                        window.focus(&this.focus, cx);
                    }))
                    .into_any_element()
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
                ("undo", Framed::Button(b)) => {
                    b.on_click(on_event(&me, |this, _: &ClickEvent, _, cx| this.undo(false, cx))).into_any_element()
                },
                ("redo", Framed::Button(b)) => {
                    b.on_click(on_event(&me, |this, _: &ClickEvent, _, cx| this.undo(true, cx))).into_any_element()
                },
                ("copy", Framed::Button(b)) => b
                    .on_click(on_event(&me, |this, _: &ClickEvent, _, cx| {
                        this.copy(cx);
                    }))
                    .into_any_element(),
                ("paste", Framed::Button(b)) => {
                    b.on_click(on_event(&me, |this, _: &ClickEvent, window, cx| this.paste(window, cx))).into_any_element()
                },
                ("crop", Framed::Button(b)) => b.on_click(on_event(&me, |this, _: &ClickEvent, _, cx| this.crop(cx))).into_any_element(),
                ("save", Framed::Button(b)) => b.on_click(on_event(&me, |this, _: &ClickEvent, _, cx| this.save_as(cx))).into_any_element(),
                ("done", Framed::Button(b)) => {
                    b.on_click(on_event(&me, |this, _: &ClickEvent, window, cx| this.done(window, cx))).into_any_element()
                },
                ("pages", Framed::Box(d)) => d.id("pages").overflow_x_scroll().into_any_element(),
                ("tab", Framed::Box(d)) => {
                    let i = ix.unwrap_or(0);
                    d.id(("page-tab", i))
                        .on_click(on_event(&me, move |this, _: &ClickEvent, window, cx| this.switch(i, window, cx)))
                        .into_any_element()
                },
                ("close", Framed::Button(b)) => {
                    let i = ix.unwrap_or(0);
                    b.on_click(on_event(&me, move |this, _: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        this.close_tab(i, window, cx);
                    }))
                    .into_any_element()
                },
                ("new_page", Framed::Button(b)) => {
                    b.on_click(on_event(&me, |this, _: &ClickEvent, window, cx| this.new_page(window, cx))).into_any_element()
                },
                (_, f) => f.into_any_element(),
            },
            cx,
        )
    }
}

/// What the page area paints.
struct Scene {
    base: Arc<RgbaImage>,
    picture: Arc<RenderImage>,
    shapes: Vec<Shape>,
    floating: Option<(Arc<RenderImage>, (P, P))>,
    sel: Option<(P, P)>,
    accent: Hsla,
    paper: Hsla,
}

impl Scene {
    fn paint(&self, bounds: Bounds<Pixels>, view: View, window: &mut Window, cx: &mut App) {
        let page = Bounds::from_corners(view.pt(P::default()), view.pt(p(self.base.width() as f32, self.base.height() as f32)));
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            window.paint_quad(outline(page.dilate(px(1.)), self.paper, BorderStyle::Solid));
            let _ = window.paint_image(page, page, Corners::default(), self.picture.clone(), 0, false);
            window.with_content_mask(Some(ContentMask { bounds: page }), |window| {
                for shape in &self.shapes {
                    paint::shape(view, shape, &self.base, window, cx);
                }
            });
            if let Some((picture, rect)) = &self.floating {
                let r = Bounds::from_corners(view.pt(rect.0), view.pt(rect.1));
                let _ = window.paint_image(r, r, Corners::default(), picture.clone(), 0, false);
                self.frame(r, &handles(*rect).map(|h| view.pt(h)), window);
            }
            if let Some((lo, hi)) = self.sel {
                let r = Bounds::from_corners(view.pt(lo), view.pt(hi));
                window.paint_quad(fill(r, self.accent.opacity(0.12)));
                window.paint_quad(outline(r, self.accent, BorderStyle::Dashed));
            }
        });
    }

    fn frame(&self, r: Bounds<Pixels>, handles: &[Point<Pixels>], window: &mut Window) {
        window.paint_quad(outline(r, self.accent, BorderStyle::Solid));
        for c in handles {
            let b = Bounds::new(point(c.x - px(4.), c.y - px(4.)), gpui_kit::size(px(8.), px(8.)));
            window.paint_quad(fill(b, gpui_kit::white()));
            window.paint_quad(outline(b, self.accent, BorderStyle::Solid));
        }
    }
}

#[cfg(test)]
mod tests {
    // Not gpui's `test`, which the glob brings in.
    use core::prelude::v1::test;

    use super::*;

    #[test]
    fn corners_scale_keeping_the_aspect() {
        let orig = (p(0., 0.), p(100., 50.));
        // Bottom right dragged out: the top left stays, 2:1 kept.
        assert_eq!(scale_box(4, orig, p(200., 60.)), (p(0., 0.), p(200., 100.)));
        // Top left dragged in.
        assert_eq!(scale_box(0, orig, p(50., 40.)), (p(50., 25.), p(100., 50.)));
    }

    #[test]
    fn edges_scale_one_side() {
        let orig = (p(0., 0.), p(100., 50.));
        assert_eq!(scale_box(3, orig, p(150., 999.)), (p(0., 0.), p(150., 50.)));
        assert_eq!(scale_box(1, orig, p(999., -20.)), (p(0., -20.), p(100., 50.)));
        // Not past the other side.
        assert_eq!(scale_box(7, orig, p(300., 0.)), (p(99., 0.), p(100., 50.)));
    }

    #[test]
    fn fit_shrinks_big_pages_but_not_small_ones() {
        let area = Bounds::new(point(px(10.), px(20.)), gpui_kit::size(px(548.), px(448.)));
        let v = fit(area, p(1000., 400.), 1.);
        assert!((v.k - 0.5).abs() < 1e-4);
        assert_eq!(v.origin, p(10. + 24., 20. + (448. - 200.) / 2.));
        // Shown at its pixels on a 2× screen: half as many points.
        let v = fit(area, p(100., 100.), 2.);
        assert!((v.k - 0.5).abs() < 1e-4);
        assert_eq!(v.px(from_point(v.pt(p(30., 40.)))), p(30., 40.));
    }

    #[test]
    fn pages_undo_pastes_and_shapes() {
        let mut page = Page::new("t".into(), None, raster::blank(40, 30), 1.);
        let red = RgbaImage::from_pixel(10, 10, image::Rgba(COLORS[0]));
        page.push(Shape::Rect { a: p(1., 1.), b: p(5., 5.), style: Style::default() });
        page.place(&Floating { picture: picture(&red), img: Arc::new(red), rect: (p(35., 0.), p(45., 10.)) });
        assert_eq!(page.base.dimensions(), (45, 30), "grew to hold the paste");
        assert!(page.doc.shapes.is_empty(), "the shapes went into the image");
        assert_eq!(page.base.get_pixel(40, 5).0, COLORS[0]);
        assert!(page.undo());
        assert_eq!((page.base.dimensions(), page.doc.shapes.len()), ((40, 30), 1));
        assert!(page.redo());
        assert_eq!(page.base.dimensions(), (45, 30));
        assert!(page.changed);
    }

    #[test]
    fn undo_counts_each_old_image_once() {
        let mut page = Page::new("t".into(), None, raster::blank(10, 10), 1.);
        let shape = || Shape::Rect { a: p(1., 1.), b: p(5., 5.), style: Style::default() };
        page.push(shape());
        assert_eq!(page.undo_bytes(), 0, "shapes share the current image");
        page.erase((p(0., 0.), p(2., 2.)));
        page.push(shape());
        page.push(shape());
        assert_eq!(page.undo_bytes(), 10 * 10 * 4, "the image before the erase, once");
    }

    #[test]
    fn erase_blanks_the_region() {
        let mut page = Page::new("t".into(), None, RgbaImage::from_pixel(20, 20, image::Rgba(COLORS[5])), 1.);
        page.erase((p(5., 5.), p(10., 10.)));
        assert_eq!(page.base.get_pixel(7, 7).0, raster::PAPER);
        assert_eq!(page.base.get_pixel(12, 7).0, COLORS[5]);
    }
}
