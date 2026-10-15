//! How a workspace's center area is split into panes, like VS Code's editor
//! groups: a tree of side-by-side (`Horizontal`) and stacked (`Vertical`)
//! splits whose leaves are panes. Each pane has its own tabs (see `workspace`).

use gpui_kit::Axis;

pub type PaneId = u64;

/// A side of a pane: where a tab dropped near it opens a new pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    #[cfg(debug_assertions)] // the debug control socket
    pub const ALL: [Edge; 4] = [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom];

    /// The split that puts a pane on this side.
    pub fn axis(self) -> Axis {
        match self {
            Edge::Left | Edge::Right => Axis::Horizontal,
            Edge::Top | Edge::Bottom => Axis::Vertical,
        }
    }

    /// Whether the new pane goes after (right of / below) the one split.
    pub fn after(self) -> bool {
        matches!(self, Edge::Right | Edge::Bottom)
    }

    #[cfg(debug_assertions)] // the debug control socket
    pub fn key(self) -> &'static str {
        match self {
            Edge::Left => "left",
            Edge::Right => "right",
            Edge::Top => "up",
            Edge::Bottom => "down",
        }
    }

    #[cfg(debug_assertions)] // the debug control socket
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.key() == s)
    }
}

/// Where in a pane a dragged tab is dropped: onto it (joins its tabs) or
/// near one of its sides (splits it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Center,
    Edge(Edge),
}

impl Zone {
    /// The zone at `(x, y)`, as fractions of the pane's width and height:
    /// the outer third along a side picks that side, as VS Code does.
    pub fn at(x: f32, y: f32) -> Self {
        let near = [(x, Edge::Left), (1. - x, Edge::Right), (y, Edge::Top), (1. - y, Edge::Bottom)];
        let (d, edge) = near.into_iter().fold((f32::MAX, Edge::Left), |best, n| if n.0 < best.0 { n } else { best });
        if d < 1. / 3. { Zone::Edge(edge) } else { Zone::Center }
    }

    #[cfg(debug_assertions)] // the debug control socket
    pub fn parse(s: &str) -> Option<Self> {
        if s == "center" { Some(Zone::Center) } else { Edge::parse(s).map(Zone::Edge) }
    }
}

/// A part of the unit square: x, y, width, height.
pub type Rect = [f32; 4];

#[derive(Debug, Clone, PartialEq)]
pub enum Layout {
    Pane(PaneId),
    Split(Axis, Vec<Layout>),
}

impl Layout {
    /// `cols` × `rows` panes (rows of columns), filled from `ids` in reading order.
    pub fn grid(cols: usize, rows: usize, ids: &[PaneId]) -> Self {
        let (cols, rows) = (cols.max(1), rows.max(1));
        let row = |r: usize| {
            let panes: Vec<Layout> = (0..cols).map(|c| Layout::Pane(ids[r * cols + c])).collect();
            if cols == 1 { panes.into_iter().next().unwrap() } else { Layout::Split(Axis::Horizontal, panes) }
        };
        if rows == 1 { row(0) } else { Layout::Split(Axis::Vertical, (0..rows).map(row).collect()) }
    }

    /// The panes, left to right and top to bottom as the tree goes.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = vec![];
        self.each(&mut |p| out.push(p));
        out
    }

    fn each(&self, f: &mut impl FnMut(PaneId)) {
        match self {
            Layout::Pane(p) => f(*p),
            Layout::Split(_, children) => children.iter().for_each(|c| c.each(f)),
        }
    }

    pub fn contains(&self, pane: PaneId) -> bool {
        match self {
            Layout::Pane(p) => *p == pane,
            Layout::Split(_, children) => children.iter().any(|c| c.contains(pane)),
        }
    }

    /// Put pane `new` on side `edge` of pane `target`.
    pub fn split(&mut self, target: PaneId, new: PaneId, edge: Edge) {
        let pair = |old: Layout| {
            let new = Layout::Pane(new);
            Layout::Split(edge.axis(), if edge.after() { vec![old, new] } else { vec![new, old] })
        };
        match self {
            Layout::Pane(p) if *p == target => *self = pair(Layout::Pane(target)),
            Layout::Pane(_) => {},
            Layout::Split(axis, children) => {
                let Some(k) = children.iter().position(|c| *c == Layout::Pane(target)) else {
                    return children.iter_mut().for_each(|c| c.split(target, new, edge));
                };
                if *axis == edge.axis() {
                    // A sibling in the same row (or column), not a nested split.
                    children.insert(if edge.after() { k + 1 } else { k }, Layout::Pane(new));
                } else {
                    children[k] = pair(Layout::Pane(target));
                }
            },
        }
    }

    /// Take pane `pane` out, closing up the space. The last pane stays.
    pub fn remove(&mut self, pane: PaneId) -> bool {
        if !self.contains(pane) || *self == Layout::Pane(pane) {
            return false;
        }
        self.drop_pane(pane);
        self.normalize();
        true
    }

    fn drop_pane(&mut self, pane: PaneId) {
        if let Layout::Split(_, children) = self {
            children.retain(|c| *c != Layout::Pane(pane));
            children.iter_mut().for_each(|c| c.drop_pane(pane));
        }
    }

    /// No split of one pane, and no split inside another along the same axis.
    fn normalize(&mut self) {
        let Layout::Split(axis, children) = self else { return };
        children.iter_mut().for_each(Layout::normalize);
        children.retain(|c| !matches!(c, Layout::Split(_, cs) if cs.is_empty()));
        let axis = *axis;
        let mut flat = vec![];
        for c in std::mem::take(children) {
            match c {
                Layout::Split(a, cs) if a == axis => flat.extend(cs),
                c => flat.push(c),
            }
        }
        *children = flat;
        if children.len() == 1 {
            *self = children.pop().unwrap();
        }
    }

    /// Each pane's part of the unit square, splits sharing space evenly.
    pub fn rects(&self) -> Vec<(PaneId, Rect)> {
        let mut out = vec![];
        self.place([0., 0., 1., 1.], &mut out);
        out
    }

    fn place(&self, r: Rect, out: &mut Vec<(PaneId, Rect)>) {
        match self {
            Layout::Pane(p) => out.push((*p, r)),
            Layout::Split(axis, children) => {
                let n = children.len() as f32;
                for (k, c) in children.iter().enumerate() {
                    let k = k as f32;
                    let part = match axis {
                        Axis::Horizontal => [r[0] + r[2] * k / n, r[1], r[2] / n, r[3]],
                        Axis::Vertical => [r[0], r[1] + r[3] * k / n, r[2], r[3] / n],
                    };
                    c.place(part, out);
                }
            },
        }
    }

    /// The pane next to `from` on side `edge`: the closest one that faces
    /// it, the one facing it most when several are as close.
    pub fn neighbor(&self, from: PaneId, edge: Edge) -> Option<PaneId> {
        let rects = self.rects();
        let a = rects.iter().find(|(p, _)| *p == from)?.1;
        let eps = 1e-4;
        let overlap = |a0: f32, a1: f32, b0: f32, b1: f32| a1.min(b1) - a0.max(b0);
        rects
            .iter()
            .filter(|(p, _)| *p != from)
            .filter_map(|(p, b)| {
                let (gap, facing) = match edge {
                    Edge::Left => (a[0] - (b[0] + b[2]), overlap(a[1], a[1] + a[3], b[1], b[1] + b[3])),
                    Edge::Right => (b[0] - (a[0] + a[2]), overlap(a[1], a[1] + a[3], b[1], b[1] + b[3])),
                    Edge::Top => (a[1] - (b[1] + b[3]), overlap(a[0], a[0] + a[2], b[0], b[0] + b[2])),
                    Edge::Bottom => (b[1] - (a[1] + a[3]), overlap(a[0], a[0] + a[2], b[0], b[0] + b[2])),
                };
                (gap > -eps && facing > eps).then_some((*p, gap, facing))
            })
            .min_by(|x, y| x.1.total_cmp(&y.1).then(y.2.total_cmp(&x.2)))
            .map(|(p, _, _)| p)
    }

    /// The pane at the top right, where the window-wide buttons go.
    pub fn top_right(&self) -> PaneId {
        let rects = self.rects();
        rects
            .iter()
            .min_by(|x, y| x.1[1].total_cmp(&y.1[1]).then((y.1[0] + y.1[2]).total_cmp(&(x.1[0] + x.1[2]))))
            .map_or(0, |(p, _)| *p)
    }

    /// `h(0,v(1,2))`: splits by axis, panes by their place in `order`.
    pub fn encode(&self, order: &[PaneId]) -> String {
        match self {
            Layout::Pane(p) => order.iter().position(|x| x == p).unwrap_or(0).to_string(),
            Layout::Split(axis, children) => {
                let parts: Vec<String> = children.iter().map(|c| c.encode(order)).collect();
                let a = if *axis == Axis::Horizontal { 'h' } else { 'v' };
                format!("{a}({})", parts.join(","))
            },
        }
    }

    /// What `encode` wrote, panes numbered as written.
    pub fn decode(s: &str) -> Option<Self> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        let (layout, rest) = Self::parse(&s)?;
        let mut seen = layout.panes();
        seen.sort_unstable();
        // Each of 0..n once.
        (rest.is_empty() && seen.iter().enumerate().all(|(k, p)| *p == k as PaneId)).then_some(layout)
    }

    /// This layout with pane `k` (as `decode` numbers them) as `ids[k]`.
    pub fn renumber(&self, ids: &[PaneId]) -> Self {
        match self {
            Layout::Pane(k) => Layout::Pane(ids[*k as usize]),
            Layout::Split(axis, children) => Layout::Split(*axis, children.iter().map(|c| c.renumber(ids)).collect()),
        }
    }

    fn parse(s: &str) -> Option<(Self, &str)> {
        let axis = match s.chars().next()? {
            'h' => Axis::Horizontal,
            'v' => Axis::Vertical,
            _ => {
                let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
                return Some((Layout::Pane(s[..end].parse().ok()?), &s[end..]));
            },
        };
        let mut rest = s[1..].strip_prefix('(')?;
        let mut children = vec![];
        loop {
            let (c, r) = Self::parse(rest)?;
            children.push(c);
            match r.chars().next()? {
                ',' => rest = &r[1..],
                ')' => {
                    rest = &r[1..];
                    break;
                },
                _ => return None,
            }
        }
        let mut layout = Layout::Split(axis, children);
        layout.normalize();
        Some((layout, rest))
    }
}

/// The grid that shows `n` panes on a wide screen: columns first.
pub fn grid_for(n: usize) -> (usize, usize) {
    match n {
        0 | 1 => (1, 1),
        2 => (2, 1),
        3 => (3, 1),
        4 => (2, 2),
        5 | 6 => (3, 2),
        7 | 8 => (4, 2),
        9 => (3, 3),
        _ => (4, 3),
    }
}

/// The layouts offered in the menus: columns, rows, label.
pub const PRESETS: [(usize, usize, &str); 10] = [
    (1, 1, "Single"),
    (2, 1, "Two Columns"),
    (3, 1, "Three Columns"),
    (4, 1, "Four Columns"),
    (1, 2, "Two Rows"),
    (1, 3, "Three Rows"),
    (2, 2, "Grid 2 × 2"),
    (3, 2, "Grid 3 × 2"),
    (4, 2, "Grid 4 × 2"),
    (3, 3, "Grid 3 × 3"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn h(c: Vec<Layout>) -> Layout {
        Layout::Split(Axis::Horizontal, c)
    }
    fn v(c: Vec<Layout>) -> Layout {
        Layout::Split(Axis::Vertical, c)
    }
    use Layout::Pane as P;

    #[test]
    fn split_and_remove() {
        let mut l = P(0);
        l.split(0, 1, Edge::Right);
        assert_eq!(l, h(vec![P(0), P(1)]));
        // Same axis: a sibling, not a nested split.
        l.split(0, 2, Edge::Left);
        assert_eq!(l, h(vec![P(2), P(0), P(1)]));
        l.split(1, 3, Edge::Bottom);
        assert_eq!(l, h(vec![P(2), P(0), v(vec![P(1), P(3)])]));
        assert!(l.remove(1));
        assert_eq!(l, h(vec![P(2), P(0), P(3)]));
        assert!(l.remove(0) && l.remove(2));
        assert_eq!(l, P(3));
        assert!(!l.remove(3));
    }

    #[test]
    fn removing_flattens_same_axis() {
        // h(0, v(1, h(2, 3))) minus 1 → h(0, 2, 3)
        let mut l = h(vec![P(0), v(vec![P(1), h(vec![P(2), P(3)])])]);
        assert!(l.remove(1));
        assert_eq!(l, h(vec![P(0), P(2), P(3)]));
    }

    #[test]
    fn grids() {
        let l = Layout::grid(3, 2, &[0, 1, 2, 3, 4, 5]);
        assert_eq!(l, v(vec![h(vec![P(0), P(1), P(2)]), h(vec![P(3), P(4), P(5)])]));
        assert_eq!(l.panes(), vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(Layout::grid(1, 1, &[7]), P(7));
        assert_eq!(Layout::grid(1, 2, &[0, 1]), v(vec![P(0), P(1)]));
        assert_eq!(grid_for(6), (3, 2));
        assert_eq!(grid_for(4), (2, 2));
    }

    #[test]
    fn neighbors() {
        let l = Layout::grid(3, 2, &[0, 1, 2, 3, 4, 5]);
        assert_eq!(l.neighbor(0, Edge::Right), Some(1));
        assert_eq!(l.neighbor(0, Edge::Bottom), Some(3));
        assert_eq!(l.neighbor(4, Edge::Top), Some(1));
        assert_eq!(l.neighbor(5, Edge::Left), Some(4));
        assert_eq!(l.neighbor(2, Edge::Right), None);
        assert_eq!(l.top_right(), 2);
        // A tall pane beside two stacked ones: up/down from it finds nothing.
        let l = h(vec![P(0), v(vec![P(1), P(2)])]);
        assert_eq!(l.neighbor(0, Edge::Right), Some(1));
        assert_eq!(l.neighbor(2, Edge::Left), Some(0));
        assert_eq!(l.neighbor(0, Edge::Bottom), None);
    }

    #[test]
    fn zones() {
        assert_eq!(Zone::at(0.5, 0.5), Zone::Center);
        assert_eq!(Zone::at(0.1, 0.5), Zone::Edge(Edge::Left));
        assert_eq!(Zone::at(0.9, 0.4), Zone::Edge(Edge::Right));
        assert_eq!(Zone::at(0.5, 0.05), Zone::Edge(Edge::Top));
        assert_eq!(Zone::at(0.45, 0.8), Zone::Edge(Edge::Bottom));
    }

    #[test]
    fn encode_round_trip() {
        let l = h(vec![P(10), v(vec![P(11), P(12)])]);
        let s = l.encode(&l.panes());
        assert_eq!(s, "h(0,v(1,2))");
        assert_eq!(Layout::decode(&s), Some(h(vec![P(0), v(vec![P(1), P(2)])])));
        assert_eq!(Layout::decode("0"), Some(P(0)));
        assert_eq!(Layout::decode("h(0,0)"), None);
        assert_eq!(Layout::decode("h(0,2)"), None);
        assert_eq!(Layout::decode("h(1,0)").map(|l| l.renumber(&[7, 8])), Some(h(vec![P(8), P(7)])));
        assert_eq!(Layout::decode("h(0,"), None);
        assert_eq!(Layout::decode("x"), None);
    }
}
