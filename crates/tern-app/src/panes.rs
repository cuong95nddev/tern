//! How a workspace's center area is split into panes, like VS Code's editor
//! groups: a tree of side-by-side (`Horizontal`) and stacked (`Vertical`)
//! splits whose leaves are panes. Each pane has its own tabs (see `workspace`).
//! A template whose center dock takes panels puts those panels among the
//! panes too (`Part` leaves).

use gpui_kit::Axis;

pub type PaneId = u64;

pub use crate::arrange::{Edge, Zone};

/// The split that puts a pane on side `edge`.
fn axis(edge: Edge) -> Axis {
    match edge.dir() {
        crate::arrange::Dir::Row => Axis::Horizontal,
        crate::arrange::Dir::Column => Axis::Vertical,
    }
}

/// A part of the unit square: x, y, width, height.
pub type Rect = [f32; 4];

#[derive(Debug, Clone, PartialEq)]
pub enum Layout {
    Pane(PaneId),
    /// A panel (a template's part) in the center, by name.
    Part(String),
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
            Layout::Part(_) => {},
            Layout::Split(_, children) => children.iter().for_each(|c| c.each(f)),
        }
    }

    /// The panels among the panes, in the same order.
    pub fn parts(&self) -> Vec<&str> {
        match self {
            Layout::Pane(_) => vec![],
            Layout::Part(name) => vec![name],
            Layout::Split(_, children) => children.iter().flat_map(Layout::parts).collect(),
        }
    }

    pub fn contains(&self, pane: PaneId) -> bool {
        self.has(&Layout::Pane(pane))
    }

    /// Whether leaf `leaf` (a pane or a part) is in it.
    pub fn has(&self, leaf: &Layout) -> bool {
        match self {
            Layout::Split(_, children) => children.iter().any(|c| c.has(leaf)),
            l => l == leaf,
        }
    }

    /// Put pane `new` on side `edge` of pane `target`.
    pub fn split(&mut self, target: PaneId, new: PaneId, edge: Edge) {
        self.split_leaf(&Layout::Pane(target), Layout::Pane(new), edge);
    }

    /// Put leaf `new` on side `edge` of leaf `target`.
    pub fn split_leaf(&mut self, target: &Layout, new: Layout, edge: Edge) {
        let pair = |old: Layout, new: Layout| Layout::Split(axis(edge), if edge.after() { vec![old, new] } else { vec![new, old] });
        match self {
            Layout::Split(axis, children) => {
                let Some(k) = children.iter().position(|c| c == target) else {
                    return children.iter_mut().for_each(|c| c.split_leaf(target, new.clone(), edge));
                };
                if *axis == self::axis(edge) {
                    // A sibling in the same row (or column), not a nested split.
                    children.insert(if edge.after() { k + 1 } else { k }, new);
                } else {
                    children[k] = pair(target.clone(), new);
                }
            },
            _ if self == target => *self = pair(target.clone(), new),
            _ => {},
        }
    }

    /// Take pane `pane` out, closing up the space. The last pane stays.
    pub fn remove(&mut self, pane: PaneId) -> bool {
        if !self.contains(pane) || self.panes().len() < 2 {
            return false;
        }
        self.drop_leaf(&Layout::Pane(pane));
        self.normalize();
        true
    }

    /// Take part `part` out, closing up the space.
    pub fn remove_part(&mut self, part: &str) -> bool {
        let leaf = Layout::Part(part.to_owned());
        if !self.has(&leaf) || *self == leaf {
            return false;
        }
        self.drop_leaf(&leaf);
        self.normalize();
        true
    }

    fn drop_leaf(&mut self, leaf: &Layout) {
        if let Layout::Split(_, children) = self {
            children.retain(|c| c != leaf);
            children.iter_mut().for_each(|c| c.drop_leaf(leaf));
        }
    }

    /// Holds exactly the parts in `parts`: drops the others, and puts those
    /// it lacks at its right. True when it changed.
    pub fn fit_parts(&mut self, parts: &[&str]) -> bool {
        let before = self.clone();
        let gone: Vec<String> = self.parts().into_iter().filter(|p| !parts.contains(p)).map(str::to_owned).collect();
        for p in gone {
            self.remove_part(&p);
        }
        for p in parts {
            let leaf = Layout::Part((*p).to_owned());
            if !self.has(&leaf) {
                let old = std::mem::replace(self, Layout::Pane(0));
                *self = Layout::Split(Axis::Horizontal, vec![old, leaf]);
                self.normalize();
            }
        }
        *self != before
    }

    /// What is drawn when the parts `shown` says no to are hidden: they
    /// leave no room. None when nothing is left.
    pub fn shown(&self, shown: &dyn Fn(&str) -> bool) -> Option<Layout> {
        let mut l = match self {
            Layout::Part(p) if !shown(p) => return None,
            Layout::Split(axis, children) => {
                let kept: Vec<Layout> = children.iter().filter_map(|c| c.shown(shown)).collect();
                if kept.is_empty() {
                    return None;
                }
                Layout::Split(*axis, kept)
            },
            l => l.clone(),
        };
        l.normalize();
        Some(l)
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
            Layout::Part(_) => {},
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
        rects.iter().min_by(|x, y| x.1[1].total_cmp(&y.1[1]).then((y.1[0] + y.1[2]).total_cmp(&(x.1[0] + x.1[2])))).map_or(0, |(p, _)| *p)
    }

    /// `h(0,v(1,2))`: splits by axis, panes by their place in `order`,
    /// parts by name (`h(0,explorer)`).
    pub fn encode(&self, order: &[PaneId]) -> String {
        match self {
            Layout::Pane(p) => order.iter().position(|x| x == p).unwrap_or(0).to_string(),
            Layout::Part(name) => name.clone(),
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
        let mut parts = layout.parts();
        let n = parts.len();
        parts.sort_unstable();
        parts.dedup();
        // Each of 0..n once, at least one pane, each part once.
        let ok = rest.is_empty() && !seen.is_empty() && seen.iter().enumerate().all(|(k, p)| *p == k as PaneId) && parts.len() == n;
        ok.then_some(layout)
    }

    /// This layout with pane `k` (as `decode` numbers them) as `ids[k]`.
    pub fn renumber(&self, ids: &[PaneId]) -> Self {
        match self {
            Layout::Pane(k) => Layout::Pane(ids[*k as usize]),
            Layout::Part(name) => Layout::Part(name.clone()),
            Layout::Split(axis, children) => Layout::Split(*axis, children.iter().map(|c| c.renumber(ids)).collect()),
        }
    }

    fn parse(s: &str) -> Option<(Self, &str)> {
        let axis = match s.chars().next()? {
            'h' if s[1..].starts_with('(') => Axis::Horizontal,
            'v' if s[1..].starts_with('(') => Axis::Vertical,
            c if c.is_ascii_digit() => {
                let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
                return Some((Layout::Pane(s[..end].parse().ok()?), &s[end..]));
            },
            _ => {
                let end = s.find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))).unwrap_or(s.len());
                return (end > 0).then(|| (Layout::Part(s[..end].to_owned()), &s[end..]));
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
    fn parts_among_the_panes() {
        let part = |n: &str| Layout::Part(n.to_owned());
        let mut l = P(0);
        l.split_leaf(&P(0), part("explorer"), Edge::Left);
        assert_eq!(l, h(vec![part("explorer"), P(0)]));
        assert_eq!(l.panes(), vec![0]);
        assert_eq!(l.parts(), vec!["explorer"]);
        // The last pane stays, even beside a part.
        assert!(!l.remove(0));
        l.split_leaf(&part("explorer"), P(1), Edge::Bottom);
        assert_eq!(l, h(vec![v(vec![part("explorer"), P(1)]), P(0)]));
        assert!(l.remove(0));
        assert_eq!(l, v(vec![part("explorer"), P(1)]));
        assert_eq!(l.rects(), vec![(1, [0., 0.5, 1., 0.5])]);
        assert!(l.remove_part("explorer"));
        assert_eq!(l, P(1));
        assert!(!l.remove_part("explorer"));
        // Fitting adds what is missing at the right and drops the rest.
        let mut l = h(vec![part("changes"), P(0)]);
        assert!(l.fit_parts(&["explorer"]));
        assert_eq!(l, h(vec![P(0), part("explorer")]));
        assert!(!l.fit_parts(&["explorer"]));
        // Hidden parts leave no room; nothing left is None.
        let l = h(vec![part("explorer"), v(vec![P(0), part("changes")])]);
        assert_eq!(l.shown(&|p| p != "changes"), Some(h(vec![part("explorer"), P(0)])));
        assert_eq!(part("x").shown(&|_| false), None);
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
        // Parts by name; a pane is needed, and a part once.
        let l = h(vec![Layout::Part("explorer".into()), v(vec![P(5), Layout::Part("h2".into())])]);
        let s = l.encode(&l.panes());
        assert_eq!(s, "h(explorer,v(0,h2))");
        assert_eq!(Layout::decode(&s).map(|l| l.renumber(&[5])), Some(l));
        assert_eq!(Layout::decode("h(explorer,changes)"), None);
        assert_eq!(Layout::decode("h(0,explorer,explorer)"), None);
    }
}
