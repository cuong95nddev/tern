//! Painting a capture's shapes on screen, shared by the overlay and the
//! editor; `raster` draws the same shapes into the exported image.

use std::sync::Arc;

use gpui_kit::*;
use image::RgbaImage;
use lyon_tessellation::{LineCap, LineJoin, StrokeOptions};

use super::model::{self, MOSAIC_BLOCK, P, Rgba, Shape, p};

pub fn hsla(c: Rgba) -> Hsla {
    gpui_kit::Rgba { r: f32::from(c[0]) / 255., g: f32::from(c[1]) / 255., b: f32::from(c[2]) / 255., a: f32::from(c[3]) / 255. }.into()
}

pub fn to_point(q: P) -> Point<Pixels> {
    point(px(q.x), px(q.y))
}

pub fn from_point(q: Point<Pixels>) -> P {
    p(q.x.as_f32(), q.y.as_f32())
}

/// An image as GPUI wants it (BGRA).
pub fn picture(img: &RgbaImage) -> Arc<RenderImage> {
    let mut bgra = img.clone();
    for px in bgra.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    Arc::new(RenderImage::new(vec![image::Frame::new(bgra)]))
}

/// Where image pixels land in the window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    /// The image's top left, in window points.
    pub origin: P,
    /// Window points per image pixel.
    pub k: f32,
    /// The capture's pixels per point, which shape sizes are given in.
    pub scale: f32,
}

impl View {
    /// Image pixels to window points.
    pub fn pt(&self, q: P) -> Point<Pixels> {
        to_point(self.origin + q.scale(self.k))
    }

    /// Window points to image pixels.
    pub fn px(&self, q: P) -> P {
        (q - self.origin).scale(1. / self.k)
    }

    /// Window points per point of a shape's style.
    pub fn zoom(&self) -> f32 {
        self.scale * self.k
    }
}

fn stroke(points: &[Point<Pixels>], closed: bool, width: f32, color: Hsla, window: &mut Window) {
    if points.len() < 2 {
        return;
    }
    let opts = StrokeOptions::default().with_line_width(width).with_line_cap(LineCap::Round).with_line_join(LineJoin::Round);
    let mut pb = PathBuilder::stroke(px(width)).with_style(PathStyle::Stroke(opts));
    pb.add_polygon(points, closed);
    if let Ok(path) = pb.build() {
        window.paint_path(path, color);
    }
}

fn fill_poly(points: &[Point<Pixels>], color: Hsla, window: &mut Window) {
    if points.len() < 3 {
        return;
    }
    let mut pb = PathBuilder::fill();
    pb.add_polygon(points, true);
    if let Ok(path) = pb.build() {
        window.paint_path(path, color);
    }
}

fn text_line(text: &str, size: f32, color: Hsla, window: &Window) -> ShapedLine {
    let run = TextRun { len: text.len(), font: font(".SystemUIFont"), color, background_color: None, underline: None, strikethrough: None };
    window.text_system().shape_line(text.to_owned().into(), px(size), &[run], None)
}

/// Paint `shape`; a mosaic averages `base`, the image it is drawn on.
pub fn shape(view: View, shape: &Shape, base: &RgbaImage, window: &mut Window, cx: &mut App) {
    let k = view.scale;
    let z = view.zoom();
    match shape {
        Shape::Rect { a, b, style } => {
            let (lo, hi) = model::norm(*a, *b);
            let pts = [lo, p(hi.x, lo.y), hi, p(lo.x, hi.y)].map(|q| view.pt(q));
            stroke(&pts, true, style.stroke() * z, hsla(style.color), window);
        },
        Shape::Ellipse { a, b, style } => {
            let pts: Vec<_> = model::ellipse(*a, *b, 96).into_iter().map(|q| view.pt(q)).collect();
            stroke(&pts, true, style.stroke() * z, hsla(style.color), window);
        },
        Shape::Line { a, b, style } => stroke(&[view.pt(*a), view.pt(*b)], false, style.stroke() * z, hsla(style.color), window),
        Shape::Arrow { a, b, style } => {
            let (shaft, head) = model::arrow(*a, *b, style.stroke() * k);
            stroke(&[view.pt(*a), view.pt(shaft)], false, style.stroke() * z, hsla(style.color), window);
            fill_poly(&head.map(|q| view.pt(q)), hsla(style.color), window);
        },
        Shape::Pen { points, style } => {
            let pts: Vec<_> = points.iter().map(|q| view.pt(*q)).collect();
            if pts.len() == 1 {
                let r = style.stroke() / 2. * k;
                let dot: Vec<_> = model::ellipse(points[0] - p(r, r), points[0] + p(r, r), 16).into_iter().map(|q| view.pt(q)).collect();
                fill_poly(&dot, hsla(style.color), window);
            } else {
                stroke(&pts, false, style.stroke() * z, hsla(style.color), window);
            }
        },
        Shape::Text { at, text, style } => {
            let size = style.font() * z;
            let line = text_line(text, size, hsla(style.color), window);
            let _ = line.paint(view.pt(*at), px(size * 1.2), TextAlign::Left, None, window, cx);
        },
        Shape::Marker { at, n, style } => {
            let r = style.marker_radius() * k;
            let disc: Vec<_> = model::ellipse(*at - p(r, r), *at + p(r, r), 48).into_iter().map(|q| view.pt(q)).collect();
            fill_poly(&disc, hsla(style.color), window);
            let size = style.marker_radius() * 1.25 * z;
            let line = text_line(&n.to_string(), size, hsla(style.marker_text()), window);
            let c = view.pt(*at);
            let lh = px(size * 1.2);
            let _ = line.paint(point(c.x - line.width / 2., c.y - lh / 2.), lh, TextAlign::Left, None, window, cx);
        },
        Shape::Mosaic { a, b } => mosaic(view, *a, *b, base, window),
    }
}

/// The mosaic as it will be exported, from `base`.
fn mosaic(view: View, a: P, b: P, base: &RgbaImage, window: &mut Window) {
    let (lo, hi) = model::norm(a, b);
    let (iw, ih) = (base.width() as i64, base.height() as i64);
    let block = (MOSAIC_BLOCK * view.scale).round().max(2.) as i64;
    let x0 = (lo.x.floor() as i64).clamp(0, iw);
    let y0 = (lo.y.floor() as i64).clamp(0, ih);
    let x1 = (hi.x.ceil() as i64).clamp(0, iw);
    let y1 = (hi.y.ceil() as i64).clamp(0, ih);
    let mut by = y0;
    while by < y1 {
        let ey = (by + block).min(y1);
        let mut bx = x0;
        while bx < x1 {
            let ex = (bx + block).min(x1);
            let mut sum = [0u64; 4];
            for y in by..ey {
                for x in bx..ex {
                    let px = base.get_pixel(x as u32, y as u32).0;
                    for c in 0..4 {
                        sum[c] += u64::from(px[c]);
                    }
                }
            }
            let n = ((ey - by) * (ex - bx)) as u64;
            let color = hsla(sum.map(|s| (s / n) as u8));
            let cell = Bounds::from_corners(view.pt(p(bx as f32, by as f32)), view.pt(p(ex as f32, ey as f32)));
            window.paint_quad(fill(cell, color));
            bx = ex;
        }
        by = ey;
    }
}
