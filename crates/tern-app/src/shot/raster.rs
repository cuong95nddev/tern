//! The exported image: the selected part of a capture with its shapes
//! drawn in, at the capture's full resolution.

use std::sync::OnceLock;

use ab_glyph::{Font, FontVec, PxScale, ScaleFont, point};
use image::RgbaImage;
use tiny_skia::{FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform};

use super::model::{self, Doc, MOSAIC_BLOCK, P, Rgba, Shape};

/// A region of the capture in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crop {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// The system UI font (macOS: SF, else Helvetica; Windows: Segoe UI, else
/// Arial); `None` leaves text out.
pub fn font() -> Option<&'static FontVec> {
    static FONT: OnceLock<Option<FontVec>> = OnceLock::new();
    FONT.get_or_init(|| {
        font_paths().into_iter().find_map(|path| {
            let bytes = std::fs::read(path).ok()?;
            FontVec::try_from_vec_and_index(bytes, 0).ok()
        })
    })
    .as_ref()
}

/// `crop` of `base` with `doc`'s shapes drawn on it, oldest first.
fn font_paths() -> Vec<std::path::PathBuf> {
    if cfg!(windows) {
        let windir = std::env::var_os("WINDIR").map_or_else(|| r"C:\Windows".into(), std::path::PathBuf::from);
        ["segoeui.ttf", "arial.ttf"].iter().map(|f| windir.join("Fonts").join(f)).collect()
    } else {
        ["/System/Library/Fonts/SFNS.ttf", "/System/Library/Fonts/Helvetica.ttc"].iter().map(Into::into).collect()
    }
}

pub fn render(base: &RgbaImage, crop: Crop, doc: &Doc, font: Option<&FontVec>) -> RgbaImage {
    let crop = clamp(crop, base.width(), base.height());
    let mut pm = Pixmap::new(crop.w.max(1), crop.h.max(1)).expect("non-empty pixmap");
    {
        // Screenshots are opaque, so the RGBA bytes are already premultiplied.
        let data = pm.data_mut();
        for y in 0..crop.h {
            for x in 0..crop.w {
                let px = base.get_pixel(crop.x + x, crop.y + y).0;
                let a = u16::from(px[3]);
                let i = ((y * crop.w + x) * 4) as usize;
                for c in 0..3 {
                    data[i + c] = (u16::from(px[c]) * a / 255) as u8;
                }
                data[i + 3] = px[3];
            }
        }
    }
    let off = model::p(crop.x as f32, crop.y as f32);
    for shape in &doc.shapes {
        draw(&mut pm, shape, off, doc, font);
    }
    let mut out = RgbaImage::new(crop.w.max(1), crop.h.max(1));
    for (i, px) in pm.pixels().iter().enumerate() {
        let c = px.demultiply();
        let buf: &mut [u8] = &mut out;
        buf[i * 4..i * 4 + 4].copy_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    out
}

fn clamp(c: Crop, w: u32, h: u32) -> Crop {
    let x = c.x.min(w.saturating_sub(1));
    let y = c.y.min(h.saturating_sub(1));
    Crop { x, y, w: c.w.min(w - x).max(1), h: c.h.min(h - y).max(1) }
}

fn paint(color: Rgba) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8(color[0], color[1], color[2], color[3]);
    paint.anti_alias = true;
    paint
}

fn stroke(width: f32) -> Stroke {
    Stroke { width, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() }
}

fn polyline(points: &[P], off: P, close: bool) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    let (first, rest) = points.split_first()?;
    pb.move_to(first.x - off.x, first.y - off.y);
    for q in rest {
        pb.line_to(q.x - off.x, q.y - off.y);
    }
    if rest.is_empty() {
        // A single point: a dot.
        pb.line_to(first.x - off.x + 0.01, first.y - off.y);
    }
    if close {
        pb.close();
    }
    pb.finish()
}

fn draw(pm: &mut Pixmap, shape: &Shape, off: P, doc: &Doc, font: Option<&FontVec>) {
    let id = Transform::identity();
    match shape {
        Shape::Rect { a, b, style } => {
            let (lo, hi) = model::norm(*a, *b);
            let pts = [lo, model::p(hi.x, lo.y), hi, model::p(lo.x, hi.y)];
            if let Some(path) = polyline(&pts, off, true) {
                pm.stroke_path(&path, &paint(style.color), &stroke(doc.px(style.stroke())), id, None);
            }
        },
        Shape::Ellipse { a, b, style } => {
            if let Some(path) = polyline(&model::ellipse(*a, *b, 96), off, true) {
                pm.stroke_path(&path, &paint(style.color), &stroke(doc.px(style.stroke())), id, None);
            }
        },
        Shape::Line { a, b, style } => {
            if let Some(path) = polyline(&[*a, *b], off, false) {
                pm.stroke_path(&path, &paint(style.color), &stroke(doc.px(style.stroke())), id, None);
            }
        },
        Shape::Arrow { a, b, style } => {
            let w = doc.px(style.stroke());
            let (shaft, head) = model::arrow(*a, *b, w);
            if let Some(path) = polyline(&[*a, shaft], off, false) {
                pm.stroke_path(&path, &paint(style.color), &stroke(w), id, None);
            }
            if let Some(path) = polyline(&head, off, true) {
                pm.fill_path(&path, &paint(style.color), FillRule::Winding, id, None);
            }
        },
        Shape::Pen { points, style } => {
            if let Some(path) = polyline(points, off, false) {
                pm.stroke_path(&path, &paint(style.color), &stroke(doc.px(style.stroke())), id, None);
            }
        },
        Shape::Text { at, text, style } => {
            if let Some(font) = font {
                draw_text(pm, font, text, *at - off, doc.px(style.font()), style.color);
            }
        },
        Shape::Marker { at, n, style } => {
            let r = doc.px(style.marker_radius());
            let c = *at - off;
            if let Some(path) = PathBuilder::from_circle(c.x, c.y, r) {
                pm.fill_path(&path, &paint(style.color), FillRule::Winding, id, None);
            }
            if let Some(font) = font {
                let label = n.to_string();
                let size = r * 1.25;
                let (w, h) = text_size(font, &label, size);
                draw_text(pm, font, &label, model::p(c.x - w / 2., c.y - h / 2.), size, style.marker_text());
            }
        },
        Shape::Mosaic { a, b } => mosaic(pm, *a - off, *b - off, doc.px(MOSAIC_BLOCK)),
    }
}

/// Fill each `block`-sized cell of the rectangle with its average colour.
fn mosaic(pm: &mut Pixmap, a: P, b: P, block: f32) {
    let (lo, hi) = model::norm(a, b);
    let (w, h) = (pm.width() as i64, pm.height() as i64);
    let x0 = (lo.x.floor() as i64).clamp(0, w);
    let y0 = (lo.y.floor() as i64).clamp(0, h);
    let x1 = (hi.x.ceil() as i64).clamp(0, w);
    let y1 = (hi.y.ceil() as i64).clamp(0, h);
    let block = block.round().max(2.) as i64;
    let data = pm.data_mut();
    let mut by = y0;
    while by < y1 {
        let ey = (by + block).min(y1);
        let mut bx = x0;
        while bx < x1 {
            let ex = (bx + block).min(x1);
            let mut sum = [0u64; 4];
            for y in by..ey {
                for x in bx..ex {
                    let i = ((y * w + x) * 4) as usize;
                    for c in 0..4 {
                        sum[c] += u64::from(data[i + c]);
                    }
                }
            }
            let n = ((ey - by) * (ex - bx)) as u64;
            let avg = sum.map(|s| (s / n) as u8);
            for y in by..ey {
                for x in bx..ex {
                    let i = ((y * w + x) * 4) as usize;
                    data[i..i + 4].copy_from_slice(&avg);
                }
            }
            bx = ex;
        }
        by = ey;
    }
}

/// The size of `text` (one line) at `size` pixels: (advance, ascent - descent).
pub fn text_size(font: &FontVec, text: &str, size: f32) -> (f32, f32) {
    let sf = font.as_scaled(PxScale::from(size));
    let w = text.chars().map(|c| sf.h_advance(font.glyph_id(c))).sum();
    (w, sf.ascent() - sf.descent())
}

/// Draw `text` with its first line's top left at `at`.
fn draw_text(pm: &mut Pixmap, font: &FontVec, text: &str, at: P, size: f32, color: Rgba) {
    let scale = PxScale::from(size);
    let sf = font.as_scaled(scale);
    let line_h = sf.height() + sf.line_gap();
    let (w, h) = (pm.width() as i32, pm.height() as i32);
    let data = pm.data_mut();
    for (row, line) in text.split('\n').enumerate() {
        let baseline = at.y + sf.ascent() + row as f32 * line_h;
        let mut x = at.x;
        let mut prev = None;
        for c in line.chars() {
            let id = font.glyph_id(c);
            if let Some(prev) = prev {
                x += sf.kern(prev, id);
            }
            prev = Some(id);
            let glyph = id.with_scale_and_position(scale, point(x, baseline));
            x += sf.h_advance(id);
            let Some(outline) = font.outline_glyph(glyph) else { continue };
            let bounds = outline.px_bounds();
            outline.draw(|gx, gy, cov| {
                let px = bounds.min.x as i32 + gx as i32;
                let py = bounds.min.y as i32 + gy as i32;
                if px < 0 || py < 0 || px >= w || py >= h {
                    return;
                }
                let i = ((py * w + px) * 4) as usize;
                let a = cov.clamp(0., 1.) * f32::from(color[3]) / 255.;
                for ch in 0..3 {
                    let src = f32::from(color[ch]) * a;
                    data[i + ch] = (src + f32::from(data[i + ch]) * (1. - a)).round() as u8;
                }
                data[i + 3] = (a * 255. + f32::from(data[i + 3]) * (1. - a)).round() as u8;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shot::model::{COLORS, Style, p};

    fn gray(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_pixel(w, h, image::Rgba([100, 100, 100, 255]))
    }

    #[test]
    fn crops_to_the_region() {
        let mut base = gray(50, 40);
        base.put_pixel(10, 5, image::Rgba([1, 2, 3, 255]));
        let out = render(&base, Crop { x: 10, y: 5, w: 20, h: 10 }, &Doc::new(1.), None);
        assert_eq!(out.dimensions(), (20, 10));
        assert_eq!(out.get_pixel(0, 0).0, [1, 2, 3, 255]);
        // A region running past the edge is cut to the image.
        let out = render(&base, Crop { x: 40, y: 30, w: 100, h: 100 }, &Doc::new(1.), None);
        assert_eq!(out.dimensions(), (10, 10));
    }

    #[test]
    fn rect_draws_its_border_only() {
        let base = gray(100, 100);
        let mut doc = Doc::new(1.);
        doc.push(Shape::Rect { a: p(20., 20.), b: p(80., 80.), style: Style { color: COLORS[0], size: 1 } });
        let out = render(&base, Crop { x: 0, y: 0, w: 100, h: 100 }, &doc, None);
        assert_eq!(out.get_pixel(20, 50).0, COLORS[0]);
        assert_eq!(out.get_pixel(50, 50).0, [100, 100, 100, 255]);
        assert_eq!(out.get_pixel(5, 5).0, [100, 100, 100, 255]);
    }

    #[test]
    fn shapes_follow_the_crop() {
        let base = gray(100, 100);
        let mut doc = Doc::new(1.);
        doc.push(Shape::Line { a: p(30., 50.), b: p(70., 50.), style: Style { color: COLORS[1], size: 2 } });
        let out = render(&base, Crop { x: 20, y: 40, w: 60, h: 20 }, &doc, None);
        assert_eq!(out.get_pixel(30, 10).0, COLORS[1]);
    }

    #[test]
    fn mosaic_makes_uniform_blocks() {
        let mut base = gray(32, 32);
        for y in 0..32 {
            for x in 0..32 {
                base.put_pixel(x, y, image::Rgba([(x * 8) as u8, (y * 8) as u8, 0, 255]));
            }
        }
        let mut doc = Doc::new(1.);
        doc.push(Shape::Mosaic { a: p(0., 0.), b: p(16., 16.) });
        let out = render(&base, Crop { x: 0, y: 0, w: 32, h: 32 }, &doc, None);
        let block = out.get_pixel(0, 0).0;
        assert_eq!(out.get_pixel(7, 7).0, block);
        assert_ne!(out.get_pixel(8, 0).0, block);
        // Outside the mosaic nothing changes.
        assert_eq!(out.get_pixel(20, 20).0, base.get_pixel(20, 20).0);
    }

    #[test]
    fn text_and_markers_draw_when_a_font_is_there() {
        let font = font().expect("system font");
        let base = gray(200, 100);
        let mut doc = Doc::new(1.);
        doc.push(Shape::Text { at: p(10., 10.), text: "Hi Việt".into(), style: Style { color: COLORS[0], size: 2 } });
        doc.push(Shape::Marker { at: p(160., 50.), n: 3, style: Style { color: COLORS[1], size: 2 } });
        let out = render(&base, Crop { x: 0, y: 0, w: 200, h: 100 }, &doc, Some(font));
        let red = out.pixels().filter(|px| px.0 == COLORS[0]).count();
        assert!(red > 20, "text pixels: {red}");
        assert_eq!(out.get_pixel(160 + 15, 50).0, COLORS[1], "marker disc");
        assert!(out.pixels().any(|px| px.0 == [255, 255, 255, 255]), "marker number");
    }
}
