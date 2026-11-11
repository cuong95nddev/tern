//! Pictures an actor shows: a still, an animated GIF, or a sprite sheet. gpui's
//! `img` draws none of them mirrored or cut into frames, so they are decoded
//! here (once, kept until the templates change) and painted as they are.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use gpui_kit::*;
use image::{AnimationDecoder as _, Frame, RgbaImage};

/// A picture decoded, and the same mirrored.
pub struct Pic {
    img: Arc<RenderImage>,
    flipped: Arc<RenderImage>,
    /// Frames to play: a GIF's, or a sprite sheet's cells.
    pub cells: usize,
    /// The size of one frame in pixels.
    cell: (f32, f32),
    sprite: bool,
    /// A GIF's frame lengths in seconds.
    delays: Vec<f32>,
}

type Key = (PathBuf, u32);

static CACHE: Mutex<Option<HashMap<Key, Option<Arc<Pic>>>>> = Mutex::new(None);

/// Forgets the pictures read: the files may have changed.
pub fn clear() {
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Whether `path` is an SVG, which gpui draws itself (and can't mirror).
pub fn is_svg(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg"))
}

/// The picture at `path`; a sheet of `sprite` equal frames when it is given.
pub fn load(path: &Path, sprite: Option<u32>) -> Option<Arc<Pic>> {
    let key = (path.to_path_buf(), sprite.unwrap_or(0));
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache
        .get_or_insert_with(HashMap::new)
        .entry(key)
        .or_insert_with(|| match decode(path, sprite) {
            Ok(p) => Some(Arc::new(p)),
            Err(e) => {
                log::warn!("actors: can't read {}: {e}", path.display());
                None
            },
        })
        .clone()
}

/// RGBA to the BGRA gpui draws.
fn bgra(mut f: Frame) -> Frame {
    for px in f.buffer_mut().as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    f
}

/// `buf` with each of its `cells` equal cells mirrored on its own (a sheet's
/// frames keep their order).
fn flip_cells(buf: &RgbaImage, cells: u32) -> RgbaImage {
    let (w, h) = buf.dimensions();
    let cw = (w / cells.max(1)).max(1);
    let mut out = buf.clone();
    for y in 0..h {
        for x in 0..w {
            let (cell, at) = (x / cw, x % cw);
            let from = cell * cw + (cw - 1 - at);
            if from < w {
                out.put_pixel(x, y, *buf.get_pixel(from, y));
            }
        }
    }
    out
}

fn decode(path: &Path, sprite: Option<u32>) -> Result<Pic, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let is_gif = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("gif"));
    let (frames, delays): (Vec<Frame>, Vec<f32>) = if is_gif {
        let decoder = image::codecs::gif::GifDecoder::new(Cursor::new(&bytes)).map_err(|e| e.to_string())?;
        let frames = decoder.into_frames().collect_frames().map_err(|e| e.to_string())?;
        let delays = frames
            .iter()
            .map(|f| {
                let (n, d) = f.delay().numer_denom_ms();
                // Browsers show a 0 or 10 ms frame for 100 ms.
                let ms = if d == 0 { 100. } else { n as f32 / d as f32 };
                (if ms < 20. { 100. } else { ms }) / 1000.
            })
            .collect();
        (frames, delays)
    } else {
        let rgba = image::load_from_memory(&bytes).map_err(|e| e.to_string())?.to_rgba8();
        (vec![Frame::new(rgba)], vec![])
    };
    if frames.is_empty() {
        return Err("no frames".into());
    }
    let cells = if sprite.is_some() { sprite.unwrap_or(1).max(1) } else { 1 };
    let (w, h) = frames[0].buffer().dimensions();
    let flipped: Vec<Frame> =
        frames.iter().map(|f| Frame::from_parts(flip_cells(f.buffer(), cells), f.left(), f.top(), f.delay())).collect();
    let count = if sprite.is_some() { cells as usize } else { frames.len() };
    Ok(Pic {
        img: Arc::new(RenderImage::new(frames.into_iter().map(bgra).collect::<Vec<_>>())),
        flipped: Arc::new(RenderImage::new(flipped.into_iter().map(bgra).collect::<Vec<_>>())),
        cells: count,
        cell: ((w / cells) as f32, h as f32),
        sprite: sprite.is_some(),
        delays,
    })
}

impl Pic {
    /// The frame to show `secs` into a loop that plays at `fps` (sprites) or
    /// by the GIF's own frame lengths.
    pub fn frame_at(&self, secs: f32, fps: f32, looping: bool) -> usize {
        if self.sprite {
            let i = (secs * fps).floor().max(0.) as usize;
            return if looping { i % self.cells } else { i.min(self.cells - 1) };
        }
        let total: f32 = self.delays.iter().sum();
        if self.cells < 2 || total <= 0. {
            return 0;
        }
        let mut t = secs.rem_euclid(total);
        for (i, d) in self.delays.iter().enumerate() {
            if t < *d {
                return i;
            }
            t -= d;
        }
        self.cells - 1
    }

    /// Seconds a frame lasts at least, when the picture is animated.
    pub fn interval(&self, fps: f32) -> Option<f32> {
        if self.cells < 2 {
            None
        } else if self.sprite {
            Some(1. / fps)
        } else {
            self.delays.iter().copied().reduce(f32::min)
        }
    }

    /// Paints `frame` in `bounds`, fitted whole and standing on its bottom edge.
    pub fn paint(&self, window: &mut Window, bounds: Bounds<Pixels>, frame: usize, mirrored: bool) {
        let (cw, ch) = self.cell;
        let (bw, bh) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        if cw <= 0. || ch <= 0. || bw <= 0. || bh <= 0. {
            return;
        }
        let scale = (bw / cw).min(bh / ch);
        let (sw, sh) = (cw * scale, ch * scale);
        let origin = point(bounds.origin.x + px((bw - sw) / 2.), bounds.origin.y + px(bh - sh));
        let shown = Bounds { origin, size: size(px(sw), px(sh)) };
        let data = if mirrored { &self.flipped } else { &self.img };
        let result = if self.sprite {
            let sheet =
                Bounds { origin: point(origin.x - px(sw * frame as f32), origin.y), size: size(px(sw * self.cells as f32), px(sh)) };
            window.paint_image(shown, sheet, Corners::default(), data.clone(), 0, false)
        } else {
            window.paint_image(shown, shown, Corners::default(), data.clone(), frame.min(self.cells - 1), false)
        };
        if let Err(e) = result {
            log::debug!("actors: can't paint: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use gpui_kit::RenderImage;
    use image::{Frame, RgbaImage};

    use super::{Pic, flip_cells, is_svg};

    fn pic(sprite: bool, cells: usize, delays: Vec<f32>) -> Pic {
        let frame = || Frame::new(RgbaImage::new(2, 2));
        Pic {
            img: Arc::new(RenderImage::new(vec![frame()])),
            flipped: Arc::new(RenderImage::new(vec![frame()])),
            cells,
            cell: (1., 1.),
            sprite,
            delays,
        }
    }

    #[test]
    fn a_sprite_plays_by_fps_and_a_gif_by_its_delays() {
        let s = pic(true, 4, vec![]);
        assert_eq!(s.frame_at(0.30, 10., true), 3);
        assert_eq!(s.frame_at(0.40, 10., true), 0);
        assert_eq!(s.frame_at(9., 10., false), 3);
        assert_eq!(s.interval(10.), Some(0.1));
        let g = pic(false, 3, vec![0.1, 0.3, 0.1]);
        assert_eq!(g.frame_at(0.05, 0., true), 0);
        assert_eq!(g.frame_at(0.2, 0., true), 1);
        assert_eq!(g.frame_at(0.45, 0., true), 2);
        assert_eq!(g.frame_at(0.55, 0., true), 0);
        assert_eq!(g.interval(0.), Some(0.1));
        assert_eq!(pic(false, 1, vec![]).interval(0.), None);
        assert_eq!(pic(false, 1, vec![]).frame_at(5., 0., true), 0);
    }

    #[test]
    fn flipping_mirrors_each_cell_alone() {
        let mut b = RgbaImage::new(4, 1);
        for x in 0..4 {
            b.put_pixel(x, 0, image::Rgba([x as u8, 0, 0, 255]));
        }
        let f = flip_cells(&b, 2);
        let reds: Vec<u8> = (0..4).map(|x| f.get_pixel(x, 0)[0]).collect();
        assert_eq!(reds, [1, 0, 3, 2]);
        let whole: Vec<u8> = (0..4).map(|x| flip_cells(&b, 1).get_pixel(x, 0)[0]).collect();
        assert_eq!(whole, [3, 2, 1, 0]);
    }

    #[test]
    fn svg_is_told_by_its_extension() {
        assert!(is_svg(Path::new("a/b.SVG")) && !is_svg(Path::new("b.png")));
    }
}
