//! What is drawn on a capture: shapes in image pixels, their style, the
//! undo history, and the geometry both the editor and the export use so
//! they draw the same thing.

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct P {
    pub x: f32,
    pub y: f32,
}

pub const fn p(x: f32, y: f32) -> P {
    P { x, y }
}

impl std::ops::Sub for P {
    type Output = P;
    fn sub(self, o: P) -> P {
        p(self.x - o.x, self.y - o.y)
    }
}

impl std::ops::Add for P {
    type Output = P;
    fn add(self, o: P) -> P {
        p(self.x + o.x, self.y + o.y)
    }
}

impl P {
    pub fn scale(self, k: f32) -> P {
        p(self.x * k, self.y * k)
    }

    pub fn len(self) -> f32 {
        self.x.hypot(self.y)
    }
}

/// RGBA, not premultiplied.
pub type Rgba = [u8; 4];

/// The colour swatches, in toolbar order.
pub const COLORS: [Rgba; 8] = [
    [0xE5, 0x39, 0x35, 0xFF],
    [0x2F, 0x6B, 0xFF, 0xFF],
    [0x3C, 0xB8, 0x4A, 0xFF],
    [0xE0, 0xA4, 0x2B, 0xFF],
    [0xEC, 0x4C, 0x8B, 0xFF],
    [0x1A, 0x1A, 0x1A, 0xFF],
    [0x7A, 0x7F, 0x87, 0xFF],
    [0xFF, 0xFF, 0xFF, 0xFF],
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Rect,
    Ellipse,
    Line,
    Arrow,
    Pen,
    Text,
    Marker,
    Mosaic,
}

impl Tool {
    pub const ALL: [Tool; 8] =
        [Tool::Rect, Tool::Ellipse, Tool::Line, Tool::Arrow, Tool::Pen, Tool::Text, Tool::Marker, Tool::Mosaic];
}

/// Colour and one of three sizes; the sizes are in points, times the
/// capture's backing scale in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    pub color: Rgba,
    /// 0 (thin), 1, 2 (thick).
    pub size: u8,
}

impl Default for Style {
    fn default() -> Self {
        Style { color: COLORS[0], size: 1 }
    }
}

impl Style {
    fn pick(self, sizes: [f32; 3]) -> f32 {
        sizes[usize::from(self.size.min(2))]
    }

    /// Line width in points.
    pub fn stroke(self) -> f32 {
        self.pick([2., 4., 6.])
    }

    /// Text size in points.
    pub fn font(self) -> f32 {
        self.pick([16., 22., 30.])
    }

    /// Numbered marker radius in points.
    pub fn marker_radius(self) -> f32 {
        self.pick([11., 14., 18.])
    }

    /// The colour text on a filled marker of this colour is drawn in.
    pub fn marker_text(self) -> Rgba {
        let [r, g, b, _] = self.color;
        let luma = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b);
        if luma > 170. { [0, 0, 0, 0xFF] } else { [0xFF; 4] }
    }
}

/// Mosaic block edge in points.
pub const MOSAIC_BLOCK: f32 = 8.;

#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    Rect { a: P, b: P, style: Style },
    Ellipse { a: P, b: P, style: Style },
    Line { a: P, b: P, style: Style },
    Arrow { a: P, b: P, style: Style },
    Pen { points: Vec<P>, style: Style },
    /// `at` is the top left of the first line.
    Text { at: P, text: String, style: Style },
    Marker { at: P, n: u32, style: Style },
    Mosaic { a: P, b: P },
}

/// The shapes drawn on one capture, oldest first, with undo/redo. Points
/// are image pixels; `scale` is pixels per point.
#[derive(Debug, Clone, Default)]
pub struct Doc {
    pub shapes: Vec<Shape>,
    redo: Vec<Shape>,
    pub scale: f32,
}

impl Doc {
    pub fn new(scale: f32) -> Self {
        Doc { shapes: Vec::new(), redo: Vec::new(), scale }
    }

    pub fn push(&mut self, shape: Shape) {
        self.shapes.push(shape);
        self.redo.clear();
    }

    pub fn undo(&mut self) -> bool {
        self.shapes.pop().map(|s| self.redo.push(s)).is_some()
    }

    pub fn redo(&mut self) -> bool {
        self.redo.pop().map(|s| self.shapes.push(s)).is_some()
    }

    pub fn can_undo(&self) -> bool {
        !self.shapes.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// The number the next marker gets.
    pub fn next_marker(&self) -> u32 {
        self.shapes.iter().filter_map(|s| if let Shape::Marker { n, .. } = s { Some(*n) } else { None }).max().unwrap_or(0) + 1
    }

    /// Points to pixels.
    pub fn px(&self, pt: f32) -> f32 {
        pt * self.scale
    }
}

/// The corners of the rectangle spanned by `a` and `b`: (top left, bottom right).
pub fn norm(a: P, b: P) -> (P, P) {
    (p(a.x.min(b.x), a.y.min(b.y)), p(a.x.max(b.x), a.y.max(b.y)))
}

/// An arrow from `a` to `b` drawn `width` wide: where its shaft stops, and
/// its head (tip, left, right). Short arrows get a smaller head.
pub fn arrow(a: P, b: P, width: f32) -> (P, [P; 3]) {
    let d = b - a;
    let len = d.len();
    if len < 0.01 {
        return (a, [b, b, b]);
    }
    let u = d.scale(1. / len);
    let n = p(-u.y, u.x);
    let head = (width * 3. + 8.).min(len * 0.6);
    let half = head * 0.55;
    let base = b - u.scale(head);
    // The shaft stops inside the head, so its round cap doesn't poke out.
    let shaft = b - u.scale(head * 0.6);
    (shaft, [b, base + n.scale(half), base - n.scale(half)])
}

/// The ellipse inscribed in the rectangle spanned by `a` and `b`, as a
/// closed polyline of `n` points.
pub fn ellipse(a: P, b: P, n: usize) -> Vec<P> {
    let (lo, hi) = norm(a, b);
    let c = (lo + hi).scale(0.5);
    let (rx, ry) = ((hi.x - lo.x) / 2., (hi.y - lo.y) / 2.);
    (0..n)
        .map(|i| {
            let t = i as f32 / n as f32 * std::f32::consts::TAU;
            p(c.x + rx * t.cos(), c.y + ry * t.sin())
        })
        .collect()
}

/// Whether a drag from `a` to `b` is too small to be a shape.
pub fn too_small(a: P, b: P, scale: f32) -> bool {
    (b - a).len() < 3. * scale
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> Shape {
        Shape::Rect { a: p(0., 0.), b: p(10., 10.), style: Style::default() }
    }

    #[test]
    fn undo_redo() {
        let mut d = Doc::new(2.);
        d.push(rect());
        d.push(rect());
        assert!(d.undo());
        assert_eq!(d.shapes.len(), 1);
        assert!(d.redo());
        assert_eq!(d.shapes.len(), 2);
        assert!(!d.redo());
        d.undo();
        d.push(rect());
        assert!(!d.can_redo(), "a new shape drops the redo history");
        d.undo();
        d.undo();
        assert!(!d.undo());
    }

    #[test]
    fn markers_count_up_past_the_largest() {
        let mut d = Doc::new(1.);
        assert_eq!(d.next_marker(), 1);
        d.push(Shape::Marker { at: P::default(), n: 1, style: Style::default() });
        d.push(Shape::Marker { at: P::default(), n: 2, style: Style::default() });
        assert_eq!(d.next_marker(), 3);
        d.undo();
        assert_eq!(d.next_marker(), 2);
    }

    #[test]
    fn norm_orders_corners() {
        assert_eq!(norm(p(5., 1.), p(2., 8.)), (p(2., 1.), p(5., 8.)));
    }

    #[test]
    fn arrow_head_points_at_the_end() {
        let (shaft, [tip, l, r]) = arrow(p(0., 0.), p(100., 0.), 4.);
        assert_eq!(tip, p(100., 0.));
        assert!(shaft.x < 100. && shaft.x > 80.);
        assert!((l.x - r.x).abs() < 1e-4 && l.x < 100.);
        assert!((l.y + r.y).abs() < 1e-4, "head is symmetric");
        // A tiny arrow keeps its head within its length.
        let (_, [_, l, _]) = arrow(p(0., 0.), p(10., 0.), 6.);
        assert!(l.x >= 3.9);
    }

    #[test]
    fn ellipse_fits_its_box() {
        let pts = ellipse(p(10., 20.), p(30., 60.), 64);
        assert_eq!(pts.len(), 64);
        for q in pts {
            assert!(q.x >= 9.99 && q.x <= 30.01 && q.y >= 19.99 && q.y <= 60.01);
        }
    }

    #[test]
    fn marker_text_contrasts() {
        assert_eq!(Style { color: COLORS[7], size: 0 }.marker_text(), [0, 0, 0, 0xFF]);
        assert_eq!(Style { color: COLORS[0], size: 0 }.marker_text(), [0xFF; 4]);
    }
}
