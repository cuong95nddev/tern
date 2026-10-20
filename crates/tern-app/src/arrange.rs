//! Where the movable parts (explorer, scratches, changes, ...) sit: one tree of parts per dock.
//! Pure data, no gpui.

use std::collections::{BTreeMap, BTreeSet};

/// Direction a split lays its children out in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Row,
    Column,
}

/// How much room a child of a split takes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Size {
    Px(f32),
    Frac(f32),
    Auto,
}

/// A side of a part or pane: where something dropped near it goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    pub const ALL: [Edge; 4] = [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom];

    /// The split that puts something on this side.
    pub fn dir(self) -> Dir {
        match self {
            Edge::Left | Edge::Right => Dir::Row,
            Edge::Top | Edge::Bottom => Dir::Column,
        }
    }

    /// Whether the new one goes after (right of / below) the one split.
    pub fn after(self) -> bool {
        matches!(self, Edge::Right | Edge::Bottom)
    }

    pub fn key(self) -> &'static str {
        match self {
            Edge::Left => "left",
            Edge::Right => "right",
            Edge::Top => "up",
            Edge::Bottom => "down",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|e| e.key() == s)
    }
}

/// Where in an area a dragged thing is dropped: onto it (joins it) or
/// near one of its sides (splits it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Center,
    Edge(Edge),
}

impl Zone {
    /// The zone at `(x, y)`, as fractions of the area's width and height:
    /// the outer third along a side picks that side, as VS Code does.
    pub fn at(x: f32, y: f32) -> Self {
        let (d, edge) = nearest(x, y);
        if d < 1. / 3. { Zone::Edge(edge) } else { Zone::Center }
    }

    /// `at`, kept to the zones an area takes: with no `center`, the nearest
    /// side; with no `edges`, the center. None when it takes neither.
    pub fn pick(x: f32, y: f32, edges: bool, center: bool) -> Option<Self> {
        match (edges, center) {
            (true, true) => Some(Self::at(x, y)),
            (true, false) => Some(Zone::Edge(nearest(x, y).1)),
            (false, true) => Some(Zone::Center),
            (false, false) => None,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        if s == "center" { Some(Zone::Center) } else { Edge::parse(s).map(Zone::Edge) }
    }
}

/// The side of the unit square closest to `(x, y)`, and how close.
fn nearest(x: f32, y: f32) -> (f32, Edge) {
    let near = [(x, Edge::Left), (1. - x, Edge::Right), (y, Edge::Top), (1. - y, Edge::Bottom)];
    near.into_iter().fold((f32::MAX, Edge::Left), |best, n| if n.0 < best.0 { n } else { best })
}

/// What a dock holds: a single part, parts stacked as tabs in one area (the
/// index of the shown one), or a split of subtrees.
#[derive(Debug, Clone, PartialEq)]
pub enum Tree {
    Part(String),
    Stack(Vec<String>, usize),
    Split(Dir, Vec<(Tree, Size)>),
}

impl Tree {
    /// The parts in the tree, in order.
    pub fn parts(&self) -> Vec<&str> {
        let mut out = Vec::new();
        collect_parts(self, &mut out);
        out
    }

    /// True when `part` is somewhere in the tree.
    pub fn has(&self, part: &str) -> bool {
        match self {
            Tree::Part(name) => name == part,
            Tree::Stack(names, _) => names.iter().any(|n| n == part),
            Tree::Split(_, items) => items.iter().any(|(t, _)| t.has(part)),
        }
    }

    /// True when this is the area that shows `part`: the part, or a stack holding it.
    fn is_leaf_of(&self, part: &str) -> bool {
        match self {
            Tree::Part(_) | Tree::Stack(..) => self.has(part),
            Tree::Split(..) => false,
        }
    }
}

/// Where a part is dropped, relative to a part already in the dock.
#[derive(Debug, Clone, PartialEq)]
pub enum Drop {
    Before(String),
    After(String),
    End,
    /// On that side of the part (of its stack, when it is in one).
    Edge(String, Edge),
    /// Onto the part: stacked with it, and shown.
    Stack(String),
}

impl Drop {
    /// The part the drop is next to or onto.
    fn target(&self) -> Option<&str> {
        match self {
            Drop::Before(t) | Drop::After(t) | Drop::Edge(t, _) | Drop::Stack(t) => Some(t),
            Drop::End => None,
        }
    }
}

/// How a panel group lays out the panels in it: as cells split any way
/// (`grid`, drops split on a panel's sides or stack onto it), as tabs, or one
/// under another (`stack`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GroupMode {
    #[default]
    Grid,
    Tabs,
    Stack,
}

impl GroupMode {
    pub fn key(self) -> &'static str {
        match self {
            GroupMode::Grid => "grid",
            GroupMode::Tabs => "tabs",
            GroupMode::Stack => "stack",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "grid" => Some(GroupMode::Grid),
            "tabs" => Some(GroupMode::Tabs),
            "stack" => Some(GroupMode::Stack),
            _ => None,
        }
    }

    /// The drop zones its panels show: `edges`/`stack` as a dock declares them.
    pub fn zones(self) -> &'static [&'static str] {
        match self {
            GroupMode::Grid => &["edges", "stack"],
            GroupMode::Tabs => &["stack"],
            GroupMode::Stack => &["before", "after"],
        }
    }
}

/// The kind of a panel group, as a part: it goes where panels go.
pub const GROUP_KIND: &str = "panel";

/// True when `name` names a panel group the user made (`group-<n>`).
pub fn is_group(name: &str) -> bool {
    name.strip_prefix("group-").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The dock a panel group is to the panels in it.
pub fn group_dock(name: &str) -> DockInfo {
    DockInfo {
        name: name.to_string(),
        dir: Dir::Column,
        accepts: vec![GROUP_KIND.to_string()],
        defaults: Tree::Split(Dir::Column, vec![]),
        grouped: false,
    }
}

/// A dock as the active template declares it.
#[derive(Debug, Clone, PartialEq)]
pub struct DockInfo {
    pub name: String,
    pub dir: Dir,
    /// Part names or kinds the dock takes.
    pub accepts: Vec<String>,
    pub defaults: Tree,
    /// Its panels are always in panel groups: one dropped in it, or placed in
    /// it by the template, gets a group of its own around it.
    pub grouped: bool,
}

impl DockInfo {
    /// True when the dock takes the part: `accepts` holds its name or its kind.
    pub fn takes(&self, part: &str, kind: &str) -> bool {
        self.accepts.iter().any(|a| a == part || a == kind)
    }
}

/// The parts of every dock, stored under a key that names the template and its docks.
#[derive(Debug, Clone, PartialEq)]
pub struct Arrangement {
    pub key: String,
    /// Dock name to its tree; `None` is an empty dock.
    pub docks: BTreeMap<String, Option<Tree>>,
    /// Stored dock widths, by dock name.
    pub widths: BTreeMap<String, f32>,
    /// The parts floating over the docks, by name.
    pub floating: BTreeMap<String, Float>,
    /// The parts among the panes (the center dock takes them), by name, with
    /// the dock each came from (its strip button stays there, and it docks
    /// back there). Where they sit among the panes is the pane layout's.
    pub center: BTreeMap<String, String>,
    /// The panel groups the user made, by name, with how each lays out its
    /// panels. A group is a part (it sits in a dock, floats or is among the
    /// panes) and a dock: its tree is in `docks` under its name.
    pub groups: BTreeMap<String, GroupMode>,
}

/// A part floating over the docks: the dock it came from (its strip button
/// stays there, and it docks back there), its top-left as fractions of the
/// area (a smaller window keeps it in view) and its size in pixels.
/// In a window of its own (`window`, its display's uuid or `-`), `x` and `y`
/// are the window's top-left in pixels on that display instead.
#[derive(Debug, Clone, PartialEq)]
pub struct Float {
    pub from: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub window: Option<String>,
}

/// A rectangle in pixels, from the area's top-left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// The sides of a floating part a resize drags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sides {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

impl Float {
    /// Where it sits in an area of `area` (width, height): no bigger than
    /// the area, and inside it.
    pub fn rect(&self, area: (f32, f32)) -> Rect {
        let (w, h) = (self.w.min(area.0).max(0.), self.h.min(area.1).max(0.));
        let x = (self.x * area.0).clamp(0., (area.0 - w).max(0.));
        let y = (self.y * area.1).clamp(0., (area.1 - h).max(0.));
        Rect { x, y, w, h }
    }

    /// Floating from dock `from` at `r` in an area of `area`.
    pub fn at(from: &str, r: Rect, area: (f32, f32)) -> Self {
        let frac = |v: f32, of: f32| if of > 0. { (v / of).clamp(0., 1.) } else { 0. };
        Float { from: from.to_string(), x: frac(r.x, area.0), y: frac(r.y, area.1), w: r.w.max(1.), h: r.h.max(1.), window: None }
    }

    /// In a window of its own at `r` (pixels) on display `display`.
    pub fn popped(from: &str, display: &str, r: Rect) -> Self {
        Float { from: from.to_string(), x: r.x, y: r.y, w: r.w.max(1.), h: r.h.max(1.), window: Some(display.to_string()) }
    }

    /// Its window's bounds, when it is in one.
    pub fn window_rect(&self) -> Option<Rect> {
        self.window.as_ref().map(|_| Rect { x: self.x, y: self.y, w: self.w, h: self.h })
    }
}

/// `r` moved by `(dx, dy)`, kept inside an area of `area`.
pub fn moved(r: Rect, dx: f32, dy: f32, area: (f32, f32)) -> Rect {
    let x = (r.x + dx).min(area.0 - r.w).max(0.);
    let y = (r.y + dy).min(area.1 - r.h).max(0.);
    Rect { x, y, ..r }
}

/// `r` with `sides` dragged by `(dx, dy)`: at least `min`, inside an area of `area`.
pub fn resized(r: Rect, sides: Sides, dx: f32, dy: f32, min: (f32, f32), area: (f32, f32)) -> Rect {
    let (mut l, mut t, mut rt, mut b) = (r.x, r.y, r.x + r.w, r.y + r.h);
    if sides.left {
        l = (l + dx).clamp(0., (rt - min.0).max(0.));
    }
    if sides.right {
        rt = (rt + dx).clamp(l + min.0, area.0.max(l + min.0));
    }
    if sides.top {
        t = (t + dy).clamp(0., (b - min.1).max(0.));
    }
    if sides.bottom {
        b = (b + dy).clamp(t + min.1, area.1.max(t + min.1));
    }
    Rect { x: l, y: t, w: rt - l, h: b - t }
}

/// The key an arrangement is stored under: template name, `/`, the dock names sorted and joined with `+`
/// (`workspace/left+right`).
pub fn key(template: &str, docks: &[DockInfo]) -> String {
    let mut names: Vec<&str> = docks.iter().map(|d| d.name.as_str()).collect();
    names.sort_unstable();
    format!("{template}/{}", names.join("+"))
}

/// `tree` with the parts `shown` rejects removed; a split left with one child becomes that child; None when empty.
pub fn prune(tree: &Tree, shown: &dyn Fn(&str) -> bool) -> Option<Tree> {
    prune_mut(tree, &mut |n| shown(n))
}

/// What a dock shows of its tree: the parts `shown` keeps, each split with
/// where it is in the stored tree, so a resize lands on the stored split.
#[derive(Debug, Clone, PartialEq)]
pub enum Shown {
    Part(String),
    /// A stack with more than one part shown.
    Stack {
        /// Child indices from the dock's root to the stack, in the stored tree.
        path: Vec<usize>,
        parts: Vec<String>,
        /// The index in `parts` of the one shown.
        active: usize,
    },
    Split {
        dir: Dir,
        /// Child indices from the dock's root to this split, in the stored tree.
        path: Vec<usize>,
        /// The stored sizes of all its children, the hidden ones' too.
        sizes: Vec<Size>,
        /// The kept children, each with its index among the stored ones.
        items: Vec<(usize, Shown)>,
    },
}

/// `tree` pruned as `prune` does, keeping the stored place of every split.
pub fn shown(tree: &Tree, shown: &dyn Fn(&str) -> bool) -> Option<Shown> {
    fn walk(tree: &Tree, path: &mut Vec<usize>, shown: &dyn Fn(&str) -> bool) -> Option<Shown> {
        match tree {
            Tree::Part(name) => shown(name).then(|| Shown::Part(name.clone())),
            Tree::Stack(names, active) => {
                let (mut parts, active) = kept_stack(names, *active, &mut |n| shown(n));
                match parts.len() {
                    0 => None,
                    1 => parts.pop().map(Shown::Part),
                    _ => Some(Shown::Stack { path: path.clone(), parts, active }),
                }
            },
            Tree::Split(dir, items) => {
                let mut kept = Vec::new();
                for (i, (t, _)) in items.iter().enumerate() {
                    path.push(i);
                    if let Some(s) = walk(t, path, shown) {
                        kept.push((i, s));
                    }
                    path.pop();
                }
                match kept.len() {
                    0 => None,
                    1 => kept.pop().map(|(_, s)| s),
                    _ => Some(Shown::Split { dir: *dir, path: path.clone(), sizes: items.iter().map(|(_, s)| *s).collect(), items: kept }),
                }
            },
        }
    }
    walk(tree, &mut Vec::new(), shown)
}

/// The sizes to keep for a split the user dragged to `shown` (`fixed`: which
/// of its children kept a size): every child keeps the size it was dragged
/// to but the largest of those without one, which takes up the rest.
pub fn dragged_sizes(shown: &[f32], fixed: &[bool]) -> Vec<Size> {
    let flex = (0..shown.len()).filter(|&i| !fixed.get(i).copied().unwrap_or(false)).max_by(|&a, &b| shown[a].total_cmp(&shown[b]).then(b.cmp(&a)));
    shown.iter().enumerate().map(|(i, s)| if Some(i) == flex { Size::Auto } else { Size::Px(*s) }).collect()
}

/// The stored sizes of a split with `new` put in for its kept children
/// (`kept`: their stored indices, in order).
pub fn merge_sizes(stored: &[Size], kept: &[usize], new: &[Size]) -> Vec<Size> {
    let mut out = stored.to_vec();
    for (i, size) in kept.iter().zip(new) {
        if let Some(slot) = out.get_mut(*i) {
            *slot = *size;
        }
    }
    out
}

/// `prune` with a stateful filter, asked once per part in tree order.
fn prune_mut(tree: &Tree, shown: &mut dyn FnMut(&str) -> bool) -> Option<Tree> {
    match tree {
        Tree::Part(name) => shown(name).then(|| tree.clone()),
        Tree::Stack(names, active) => {
            let (mut parts, active) = kept_stack(names, *active, shown);
            match parts.len() {
                0 => None,
                1 => parts.pop().map(Tree::Part),
                _ => Some(Tree::Stack(parts, active)),
            }
        },
        Tree::Split(dir, items) => {
            let mut kept: Vec<(Tree, Size)> = items.iter().filter_map(|(t, s)| prune_mut(t, shown).map(|t| (t, *s))).collect();
            match kept.len() {
                0 => None,
                1 => kept.pop().map(|(t, _)| t),
                _ => Some(Tree::Split(*dir, kept)),
            }
        },
    }
}

/// The parts of a stack `keep` keeps, and which of them is shown: the one
/// shown before, or the next kept after it (the last kept, past the end).
fn kept_stack(names: &[String], active: usize, keep: &mut dyn FnMut(&str) -> bool) -> (Vec<String>, usize) {
    let mut parts = Vec::new();
    let mut shown = None;
    for (i, n) in names.iter().enumerate() {
        if keep(n) {
            if shown.is_none() && i >= active {
                shown = Some(parts.len());
            }
            parts.push(n.clone());
        }
    }
    let shown = shown.unwrap_or(parts.len().saturating_sub(1));
    (parts, shown)
}

fn collect_parts<'a>(tree: &'a Tree, out: &mut Vec<&'a str>) {
    match tree {
        Tree::Part(name) => out.push(name),
        Tree::Stack(names, _) => out.extend(names.iter().map(String::as_str)),
        Tree::Split(_, items) => items.iter().for_each(|(t, _)| collect_parts(t, out)),
    }
}

/// The children of the split that directly holds `part`.
fn parent_of<'a>(tree: &'a Tree, part: &str) -> Option<&'a [(Tree, Size)]> {
    let Tree::Split(_, items) = tree else {
        return None;
    };
    if items.iter().any(|(t, _)| t.is_leaf_of(part)) {
        return Some(items);
    }
    items.iter().find_map(|(t, _)| parent_of(t, part))
}

/// True when dropping `part` at `at` would put it where it already is.
fn already_there(root: Option<&Tree>, part: &str, at: &Drop) -> bool {
    let Some(root) = root else {
        return false;
    };
    let index_of = |items: &[(Tree, Size)], name: &str| items.iter().position(|(t, _)| t.is_leaf_of(name));
    match at {
        Drop::End => match root {
            Tree::Part(n) => n == part,
            Tree::Stack(..) => false,
            Tree::Split(_, items) => items.last().is_some_and(|(t, _)| matches!(t, Tree::Part(n) if n == part)),
        },
        // Already in the target's stack.
        Drop::Stack(target) => parent_stack(root, target).is_some_and(|names| names.iter().any(|n| n == part)),
        Drop::Edge(..) => false,
        Drop::Before(target) | Drop::After(target) => {
            let Some(items) = parent_of(root, part) else {
                return false;
            };
            let (Some(i), Some(j)) = (index_of(items, part), index_of(items, target)) else {
                return false;
            };
            if matches!(at, Drop::Before(_)) { i + 1 == j } else { j + 1 == i }
        },
    }
}

/// The stack that holds `part`, when it is in one.
fn parent_stack<'a>(tree: &'a Tree, part: &str) -> Option<&'a [String]> {
    match tree {
        Tree::Part(_) => None,
        Tree::Stack(names, _) => names.iter().any(|n| n == part).then_some(names.as_slice()),
        Tree::Split(_, items) => items.iter().find_map(|(t, _)| parent_stack(t, part)),
    }
}

/// Inserts `part` next to `target` in the split that holds it; false when `target` isn't in `tree`.
fn insert_next_to(tree: &mut Tree, target: &str, part: &str, after: bool) -> bool {
    let Tree::Split(_, items) = tree else {
        return false;
    };
    if let Some(i) = items.iter().position(|(t, _)| t.is_leaf_of(target)) {
        let at = if after { i + 1 } else { i };
        items.insert(at, (Tree::Part(part.to_string()), Size::Auto));
        return true;
    }
    items.iter_mut().any(|(t, _)| insert_next_to(t, target, part, after))
}

/// Puts `part` on side `edge` of the area showing `target`: a sibling when the
/// split there runs along that side's axis, else a new split in its place.
fn split_at(tree: &mut Tree, target: &str, part: &str, edge: Edge) -> bool {
    let pair = |old: Tree| {
        let new = (Tree::Part(part.to_string()), Size::Auto);
        let old = (old, Size::Auto);
        Tree::Split(edge.dir(), if edge.after() { vec![old, new] } else { vec![new, old] })
    };
    if tree.is_leaf_of(target) {
        *tree = pair(tree.clone());
        return true;
    }
    let Tree::Split(dir, items) = tree else {
        return false;
    };
    if let Some(i) = items.iter().position(|(t, _)| t.is_leaf_of(target)) {
        if *dir == edge.dir() {
            items.insert(if edge.after() { i + 1 } else { i }, (Tree::Part(part.to_string()), Size::Auto));
        } else {
            items[i].0 = pair(items[i].0.clone());
        }
        return true;
    }
    items.iter_mut().any(|(t, _)| split_at(t, target, part, edge))
}

/// Stacks `part` with `target`, shown.
fn stack_on(tree: &mut Tree, target: &str, part: &str) -> bool {
    match tree {
        Tree::Part(n) if n == target => {
            *tree = Tree::Stack(vec![n.clone(), part.to_string()], 1);
            true
        },
        Tree::Part(_) => false,
        Tree::Stack(names, active) if names.iter().any(|n| n == target) => {
            names.push(part.to_string());
            *active = names.len() - 1;
            true
        },
        Tree::Stack(..) => false,
        Tree::Split(_, items) => items.iter_mut().any(|(t, _)| stack_on(t, target, part)),
    }
}

/// Puts `with` where `tree` holds part `name` on its own (not in a stack); false when it doesn't.
fn replace_part(tree: &mut Tree, name: &str, with: &Tree) -> bool {
    match tree {
        Tree::Part(n) if n == name => {
            *tree = with.clone();
            true
        },
        Tree::Split(_, items) => items.iter_mut().any(|(t, _)| replace_part(t, name, with)),
        _ => false,
    }
}

/// Puts `part` into `root` at `at`; false when the drop target isn't there.
fn place(root: &mut Option<Tree>, dir: Dir, part: &str, at: &Drop) -> bool {
    let new = Tree::Part(part.to_string());
    match at {
        Drop::Edge(target, edge) => root.as_mut().is_some_and(|t| split_at(t, target, part, *edge)),
        Drop::Stack(target) => root.as_mut().is_some_and(|t| stack_on(t, target, part)),
        Drop::End => {
            *root = Some(match root.take() {
                None => new,
                Some(Tree::Split(d, mut items)) => {
                    items.push((new, Size::Auto));
                    Tree::Split(d, items)
                },
                Some(old) => Tree::Split(dir, vec![(old, Size::Auto), (new, Size::Auto)]),
            });
            true
        },
        Drop::Before(target) | Drop::After(target) => {
            let after = matches!(at, Drop::After(_));
            match root {
                Some(only) if only.is_leaf_of(target) => {
                    let old = only.clone();
                    let items = if after { vec![(old, Size::Auto), (new, Size::Auto)] } else { vec![(new, Size::Auto), (old, Size::Auto)] };
                    *root = Some(Tree::Split(dir, items));
                    true
                },
                Some(tree) => insert_next_to(tree, target, part, after),
                None => false,
            }
        },
    }
}

impl Arrangement {
    /// The arrangement a template's docks start with: a dock without parts is
    /// empty, a split of one part is that part.
    pub fn defaults(key: &str, docks: &[DockInfo]) -> Self {
        let mut a = Arrangement {
            key: key.to_string(),
            docks: docks.iter().map(|d| (d.name.clone(), prune(&d.defaults, &|_| true))).collect(),
            widths: BTreeMap::new(),
            floating: BTreeMap::new(),
            center: BTreeMap::new(),
            groups: BTreeMap::new(),
        };
        a.wrap_loose(docks);
        a
    }

    /// A fresh group's name.
    fn next_group(&self) -> String {
        let n = (1..).find(|n| !self.groups.contains_key(&format!("group-{n}"))).unwrap_or(1);
        format!("group-{n}")
    }

    /// Puts each part sitting in a `grouped` dock outside a group, or
    /// floating from one, in a group of its own (parts stacked as tabs go
    /// together, in a group of tabs).
    pub fn wrap_loose(&mut self, docks: &[DockInfo]) {
        fn walk(tree: &mut Tree, a: &mut Arrangement) {
            let loose = match tree {
                Tree::Part(p) => !a.groups.contains_key(p),
                Tree::Stack(..) => true,
                Tree::Split(_, items) => {
                    for (t, _) in items.iter_mut() {
                        walk(t, a);
                    }
                    false
                },
            };
            if loose {
                let g = a.next_group();
                let mode = if matches!(tree, Tree::Stack(..)) { GroupMode::Tabs } else { GroupMode::Grid };
                a.groups.insert(g.clone(), mode);
                let inner = std::mem::replace(tree, Tree::Part(g.clone()));
                a.docks.insert(g, Some(inner));
            }
        }
        for d in docks.iter().filter(|d| d.grouped) {
            let Some(Some(mut tree)) = self.docks.get(&d.name).cloned() else { continue };
            walk(&mut tree, self);
            self.docks.insert(d.name.clone(), Some(tree));
        }
        let grouped = |from: &str| docks.iter().any(|d| d.grouped && d.name == from);
        let loose: Vec<String> =
            self.floating.iter().filter(|(p, f)| !self.groups.contains_key(*p) && grouped(&f.from)).map(|(p, _)| p.clone()).collect();
        for p in loose {
            let Some(f) = self.floating.remove(&p) else { continue };
            let g = self.next_group();
            self.groups.insert(g.clone(), GroupMode::Grid);
            self.docks.insert(g.clone(), Some(Tree::Part(p)));
            self.floating.insert(g, f);
        }
    }

    /// Puts a new, empty panel group at `at` in `dock` (one of the
    /// template's, or a group); its name, or None when it can't go there.
    pub fn add_group(&mut self, dock: &str, at: Drop, docks: &[DockInfo]) -> Option<String> {
        let info = self.dock_info(dock, docks)?;
        if !info.takes("", GROUP_KIND) {
            return None;
        }
        let g = self.next_group();
        let mut next = self.clone();
        next.groups.insert(g.clone(), GroupMode::default());
        next.docks.insert(g.clone(), None);
        let root = next.docks.entry(dock.to_string()).or_insert(None);
        if !place(root, info.dir, &g, &at) {
            return None;
        }
        *self = next;
        Some(g)
    }

    /// The dock named `name`: one of the template's, or a panel group.
    fn dock_info(&self, name: &str, docks: &[DockInfo]) -> Option<DockInfo> {
        docks.iter().find(|d| d.name == name).cloned().or_else(|| self.groups.contains_key(name).then(|| group_dock(name)))
    }

    /// True when `inner` is group `outer`, or is somewhere inside it.
    pub fn group_holds(&self, outer: &str, inner: &str) -> bool {
        let mut todo = vec![outer.to_string()];
        let mut seen = BTreeSet::new();
        while let Some(g) = todo.pop() {
            if g == inner {
                return true;
            }
            if !seen.insert(g.clone()) {
                continue;
            }
            if let Some(Some(tree)) = self.docks.get(&g) {
                todo.extend(tree.parts().into_iter().filter(|p| self.groups.contains_key(*p)).map(str::to_owned));
            }
        }
        false
    }

    /// `parts` with the panels of each panel group after it (those in it,
    /// floating from it or among the panes from it), at any depth.
    pub fn with_group_parts(&self, parts: Vec<String>) -> Vec<String> {
        let mut out = Vec::new();
        let mut todo: Vec<String> = parts.into_iter().rev().collect();
        while let Some(p) = todo.pop() {
            if out.contains(&p) {
                continue;
            }
            if self.groups.contains_key(&p) {
                let mut inner: Vec<String> =
                    self.docks.get(&p).cloned().flatten().iter().flat_map(|t| t.parts()).map(str::to_owned).collect();
                inner.extend(self.floating.iter().filter(|(_, f)| f.from == p).map(|(n, _)| n.clone()));
                inner.extend(self.center.iter().filter(|(_, from)| **from == p).map(|(n, _)| n.clone()));
                todo.extend(inner.into_iter().rev());
            }
            out.push(p);
        }
        out
    }

    /// Removes panel group `name`. Its panels take its place when it sits
    /// alone in a dock's split (or is a dock's only part); else each goes back
    /// to its default dock (`parts`: every part's name and kind). False when
    /// there is no such group.
    pub fn close_group(&mut self, name: &str, docks: &[DockInfo], parts: &[(&str, &str)]) -> bool {
        if self.groups.remove(name).is_none() {
            return false;
        }
        let inner = self.docks.remove(name).flatten();
        let mut placed = false;
        if let Some(t) = &inner {
            for tree in self.docks.values_mut() {
                if let Some(root) = tree
                    && replace_part(root, name, t)
                {
                    placed = true;
                    break;
                }
            }
        }
        self.take_out(name);
        if !placed && let Some(t) = inner {
            for part in t.parts() {
                let kind = parts
                    .iter()
                    .find(|(n, _)| *n == part)
                    .map(|(_, k)| *k)
                    .or_else(|| self.groups.contains_key(part).then_some(GROUP_KIND));
                self.send_to_default(part, kind, docks);
            }
        }
        // Parts floating from it dock back to their defaults.
        for f in self.floating.values_mut().filter(|f| f.from == name) {
            if let Some(d) = docks.iter().find(|d| d.takes("", GROUP_KIND)) {
                f.from = d.name.clone();
            }
        }
        for from in self.center.values_mut().filter(|f| *f == name) {
            if let Some(d) = docks.iter().find(|d| d.takes("", GROUP_KIND)) {
                *from = d.name.clone();
            }
        }
        self.wrap_loose(docks);
        true
    }

    /// Lays group `name`'s panels out as `mode` does: tabs stack them all,
    /// `stack` puts them one under another, `grid` keeps them where they are.
    /// False when there is no such group or nothing changes.
    pub fn set_group_mode(&mut self, name: &str, mode: GroupMode) -> bool {
        let Some(m) = self.groups.get_mut(name) else {
            return false;
        };
        let was = *m;
        *m = mode;
        let tree = self.docks.get_mut(name).and_then(|t| t.as_mut());
        let mut changed = was != mode;
        if let Some(tree) = tree {
            let parts: Vec<String> = tree.parts().into_iter().map(str::to_owned).collect();
            let next = match mode {
                _ if parts.len() < 2 => None,
                GroupMode::Tabs if !matches!(tree, Tree::Stack(..)) => Some(Tree::Stack(parts, 0)),
                GroupMode::Stack => Some(Tree::Split(Dir::Column, parts.into_iter().map(|p| (Tree::Part(p), Size::Auto)).collect())),
                _ => None,
            };
            if let Some(next) = next.filter(|n| n != tree) {
                *tree = next;
                changed = true;
            }
        }
        changed
    }

    /// The dock `part` sits in.
    pub fn dock_of(&self, part: &str) -> Option<&str> {
        self.docks.iter().find_map(|(name, tree)| {
            let mut parts = Vec::new();
            if let Some(tree) = tree {
                collect_parts(tree, &mut parts);
            }
            parts.contains(&part).then_some(name.as_str())
        })
    }

    /// Moves `part` into `dock` at `at`; false (and no change) when the dock doesn't accept
    /// `kind`, the dock is unknown, or the move would leave everything as it is.
    pub fn move_part(&mut self, part: &str, kind: &str, dock: &str, at: Drop, docks: &[DockInfo]) -> bool {
        let Some(info) = self.dock_info(dock, docks) else {
            return false;
        };
        if !info.takes(part, kind) {
            return false;
        }
        // A group doesn't go into itself.
        if self.groups.contains_key(part) && self.group_holds(part, dock) {
            return false;
        }
        if at.target() == Some(part) {
            return false;
        }
        if already_there(self.docks.get(dock).and_then(|t| t.as_ref()), part, &at) {
            return false;
        }
        // Onto a group: into it.
        if let Drop::Stack(target) = &at
            && self.groups.contains_key(target)
            && target != part
        {
            let target = target.clone();
            return self.move_part(part, kind, &target, Drop::End, docks);
        }
        let mut next = self.clone();
        next.take_out(part);
        // A panel in a grouped dock goes in a group of its own, where it was dropped.
        let wrap = info.grouped && !self.groups.contains_key(part);
        let placed = if wrap { next.next_group() } else { part.to_string() };
        let root = next.docks.entry(dock.to_string()).or_insert(None);
        if !place(root, info.dir, &placed, &at) {
            return false;
        }
        if wrap {
            next.groups.insert(placed.clone(), GroupMode::default());
            next.docks.insert(placed, Some(Tree::Part(part.to_string())));
        }
        if next == *self {
            return false;
        }
        *self = next;
        true
    }

    /// Takes `part` out of every dock, the floating parts and the center.
    fn take_out(&mut self, part: &str) {
        let not_part = |name: &str| name != part;
        for tree in self.docks.values_mut() {
            *tree = tree.as_ref().and_then(|t| prune(t, &not_part));
        }
        self.floating.remove(part);
        self.center.remove(part);
    }

    /// The dock `part` sits in, or came from when it floats or is in the center.
    fn home_of(&self, part: &str) -> Option<String> {
        let away = self.floating.get(part).map(|f| &f.from).or_else(|| self.center.get(part));
        away.cloned().or_else(|| self.dock_of(part).map(str::to_owned))
    }

    /// Moves `part` among the panes (the pane layout says where); false when
    /// it is in no dock, or already there.
    pub fn center_part(&mut self, part: &str) -> bool {
        if self.center.contains_key(part) {
            return false;
        }
        let Some(from) = self.home_of(part) else {
            return false;
        };
        self.take_out(part);
        self.center.insert(part.to_string(), from);
        true
    }

    /// Floats `part` at `r` in an area of `area`, out of its dock (a floating
    /// one just moves); false when it is in no dock and doesn't float, or
    /// nothing changes.
    pub fn float_part(&mut self, part: &str, r: Rect, area: (f32, f32)) -> bool {
        let Some(from) = self.home_of(part) else {
            return false;
        };
        let mut next = self.clone();
        next.take_out(part);
        next.floating.insert(part.to_string(), Float::at(&from, r, area));
        if next == *self {
            return false;
        }
        *self = next;
        true
    }

    /// Moves `part` to a window of its own at `r` on display `display` (a
    /// popped one just moves); false when it is in no dock and doesn't float,
    /// or nothing changes.
    pub fn pop_out(&mut self, part: &str, display: &str, r: Rect) -> bool {
        let Some(from) = self.home_of(part) else {
            return false;
        };
        let display = if valid_name(display) { display } else { "-" };
        let mut next = self.clone();
        next.take_out(part);
        next.floating.insert(part.to_string(), Float::popped(&from, display, r));
        if next == *self {
            return false;
        }
        *self = next;
        true
    }

    /// Docks `part`, floating or in the center, again: at the end of the dock
    /// it came from, or of its default dock when that one is gone or doesn't
    /// take it.
    pub fn dock_part(&mut self, part: &str, kind: &str, docks: &[DockInfo]) -> bool {
        let Some(from) = self.floating.get(part).map(|f| f.from.clone()).or_else(|| self.center.get(part).cloned()) else {
            return false;
        };
        let mut next = self.clone();
        next.take_out(part);
        if next.dock_back(part, kind, &from, docks).is_none() {
            return false;
        }
        next.wrap_loose(docks);
        *self = next;
        true
    }

    /// Appends `part` to dock `from`, or to its default dock when that one is
    /// gone or doesn't take it; the dock's name.
    fn dock_back(&mut self, part: &str, kind: &str, from: &str, docks: &[DockInfo]) -> Option<String> {
        match self
            .dock_info(from, docks)
            .filter(|d| d.takes(part, kind) && !(self.groups.contains_key(part) && self.group_holds(part, from)))
        {
            Some(info) => {
                let root = self.docks.entry(info.name.clone()).or_insert(None);
                place(root, info.dir, part, &Drop::End);
                Some(info.name.clone())
            },
            None => self.send_to_default(part, Some(kind), docks),
        }
    }

    /// Sets the sizes of the split at `path` (child indices from the dock's root) in `dock`.
    pub fn set_sizes(&mut self, dock: &str, path: &[usize], sizes: &[Size]) {
        let Some(Some(mut node)) = self.docks.get_mut(dock).map(|t| t.as_mut()) else {
            return;
        };
        for &i in path {
            let Tree::Split(_, items) = node else {
                return;
            };
            let Some((child, _)) = items.get_mut(i) else {
                return;
            };
            node = child;
        }
        if let Tree::Split(_, items) = node
            && items.len() == sizes.len()
        {
            for ((_, size), new) in items.iter_mut().zip(sizes) {
                *size = *new;
            }
        }
    }

    /// Shows `part` in the stack at `path` in `dock`; false when nothing changes.
    pub fn set_active(&mut self, dock: &str, path: &[usize], part: &str) -> bool {
        let Some(Some(mut node)) = self.docks.get_mut(dock).map(|t| t.as_mut()) else {
            return false;
        };
        for &i in path {
            let Tree::Split(_, items) = node else {
                return false;
            };
            let Some((child, _)) = items.get_mut(i) else {
                return false;
            };
            node = child;
        }
        let Tree::Stack(names, active) = node else {
            return false;
        };
        match names.iter().position(|n| n == part) {
            Some(i) if i != *active => {
                *active = i;
                true
            },
            _ => false,
        }
    }

    /// Fits a stored arrangement to the templates in effect (`center`: the
    /// part names and kinds the center dock takes); returns the problems.
    pub fn reconcile(&mut self, docks: &[DockInfo], parts: &[(&str, &str)], center: &[String]) -> Vec<String> {
        let groups = self.groups.clone();
        let kind_of = |part: &str| {
            parts.iter().find(|(n, _)| *n == part).map(|(_, k)| *k).or_else(|| groups.contains_key(part).then_some(GROUP_KIND))
        };
        let info_of =
            |name: &str| docks.iter().find(|d| d.name == name).cloned().or_else(|| groups.contains_key(name).then(|| group_dock(name)));
        let mut problems = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut moved_back: Vec<String> = Vec::new();

        let stored = std::mem::take(&mut self.docks);
        for (name, tree) in stored {
            let info = info_of(&name);
            let mut keep = |part: &str| {
                let Some(kind) = kind_of(part) else {
                    problems.push(format!("layout: \"{part}\" is not a part any more"));
                    return false;
                };
                if !seen.insert(part.to_string()) {
                    problems.push(format!("layout: \"{part}\" is placed twice"));
                    return false;
                }
                match &info {
                    Some(i) if i.takes(part, kind) => true,
                    _ => {
                        moved_back.push(part.to_string());
                        false
                    },
                }
            };
            let tree = tree.and_then(|t| prune_mut(&t, &mut keep));
            if let Some(info) = &info {
                self.docks.insert(info.name.clone(), tree);
            }
        }
        for info in docks {
            self.docks.entry(info.name.clone()).or_insert(None);
        }
        for g in groups.keys() {
            self.docks.entry(g.clone()).or_insert(None);
        }

        // Floating parts keep floating; one whose dock is gone docks back to its default.
        let floating = std::mem::take(&mut self.floating);
        for (part, mut f) in floating {
            let Some(kind) = kind_of(&part) else {
                problems.push(format!("layout: \"{part}\" is not a part any more"));
                continue;
            };
            if !seen.insert(part.clone()) {
                problems.push(format!("layout: \"{part}\" is placed twice"));
                continue;
            }
            if !info_of(&f.from).is_some_and(|i| i.takes(&part, kind)) || groups.contains_key(&part) && f.from == part {
                match default_dock(&part, kind, docks) {
                    Some(info) => f.from = info.name.clone(),
                    None => {
                        problems.push(format!("layout: \"{part}\" has no dock"));
                        continue;
                    },
                }
            }
            self.floating.insert(part, f);
        }

        // Parts in the center stay while it takes them; else they dock back.
        let in_center = std::mem::take(&mut self.center);
        for (part, mut from) in in_center {
            let Some(kind) = kind_of(&part) else {
                problems.push(format!("layout: \"{part}\" is not a part any more"));
                continue;
            };
            if !seen.insert(part.clone()) {
                problems.push(format!("layout: \"{part}\" is placed twice"));
                continue;
            }
            let home = info_of(&from).filter(|i| i.takes(&part, kind)).or_else(|| default_dock(&part, kind, docks).cloned());
            if !center.iter().any(|a| *a == part || a == kind) {
                match self.dock_back(&part, kind, &from, docks) {
                    Some(dock) => problems.push(format!("layout: \"{part}\" moved back to dock \"{dock}\"")),
                    None => problems.push(format!("layout: \"{part}\" has no dock")),
                }
                continue;
            }
            match home {
                Some(info) => from = info.name.clone(),
                None => {
                    problems.push(format!("layout: \"{part}\" has no dock"));
                    continue;
                },
            }
            self.center.insert(part, from);
        }

        for part in moved_back {
            match self.send_to_default(&part, kind_of(&part), docks) {
                Some(dock) => problems.push(format!("layout: \"{part}\" moved back to dock \"{dock}\"")),
                None => problems.push(format!("layout: \"{part}\" has no dock")),
            }
        }
        let all = parts.iter().copied().chain(groups.keys().map(|g| (g.as_str(), GROUP_KIND)));
        for (part, kind) in all {
            if seen.contains(part) {
                continue;
            }
            if self.send_to_default(part, Some(kind), docks).is_none() {
                problems.push(format!("layout: \"{part}\" has no dock"));
            }
        }
        self.free_groups(docks, &mut problems);
        self.wrap_loose(docks);
        problems
    }

    /// Groups placed only inside each other (a stored arrangement can say so)
    /// go back to a template's dock, so every group is drawn once.
    fn free_groups(&mut self, docks: &[DockInfo], problems: &mut Vec<String>) {
        loop {
            let mut reached: BTreeSet<String> = BTreeSet::new();
            let mut todo: Vec<String> = self.floating.keys().chain(self.center.keys()).cloned().collect();
            for d in docks {
                if let Some(Some(t)) = self.docks.get(&d.name) {
                    todo.extend(t.parts().into_iter().map(str::to_owned));
                }
            }
            while let Some(p) = todo.pop() {
                if !reached.insert(p.clone()) {
                    continue;
                }
                if self.groups.contains_key(&p)
                    && let Some(Some(t)) = self.docks.get(&p)
                {
                    todo.extend(t.parts().into_iter().map(str::to_owned));
                }
            }
            let Some(lost) = self.groups.keys().find(|g| !reached.contains(*g)).cloned() else {
                return;
            };
            self.take_out(&lost);
            match self.send_to_default(&lost, Some(GROUP_KIND), docks) {
                Some(dock) => problems.push(format!("layout: \"{lost}\" moved back to dock \"{dock}\"")),
                None => {
                    problems.push(format!("layout: \"{lost}\" has no dock"));
                    self.groups.remove(&lost);
                    self.docks.remove(&lost);
                },
            }
        }
    }

    /// Appends `part` to its default dock (or the first dock that takes its kind); the dock's name.
    fn send_to_default(&mut self, part: &str, kind: Option<&str>, docks: &[DockInfo]) -> Option<String> {
        let info = default_dock(part, kind?, docks)?;
        let root = self.docks.entry(info.name.clone()).or_insert(None);
        place(root, info.dir, part, &Drop::End);
        Some(info.name.clone())
    }

    /// One line: `name=tree` per dock joined by `;`, then `;f:part=dock@x,y,w,h` per floating part
    /// (`;p:part=dock@display@x,y,w,h` for one in its own window), `;c:part=dock` per part in the
    /// center, `;g:group=mode` per panel group (its tree is among the docks'), then `;w:name=px,...`
    /// when widths are stored.
    pub fn encode(&self) -> String {
        let mut out: Vec<String> = self
            .docks
            .iter()
            .map(|(name, tree)| {
                let mut s = format!("{name}=");
                if let Some(tree) = tree {
                    encode_tree(tree, &mut s);
                }
                s
            })
            .collect();
        for (part, f) in &self.floating {
            match &f.window {
                Some(d) => out.push(format!("p:{part}={}@{d}@{},{},{},{}", f.from, f.x, f.y, f.w, f.h)),
                None => out.push(format!("f:{part}={}@{},{},{},{}", f.from, f.x, f.y, f.w, f.h)),
            }
        }
        out.extend(self.center.iter().map(|(part, from)| format!("c:{part}={from}")));
        out.extend(self.groups.iter().map(|(g, mode)| format!("g:{g}={}", mode.key())));
        if !self.widths.is_empty() {
            let widths: Vec<String> = self.widths.iter().map(|(n, w)| format!("{n}={w}")).collect();
            out.push(format!("w:{}", widths.join(",")));
        }
        out.join(";")
    }

    /// Reads what `encode` wrote; None on anything malformed.
    pub fn decode(key: &str, text: &str) -> Option<Self> {
        let mut docks = BTreeMap::new();
        let mut widths = BTreeMap::new();
        let mut floating = BTreeMap::new();
        let mut center = BTreeMap::new();
        let mut groups = BTreeMap::new();
        let segments: Vec<&str> = text.split(';').collect();
        // Floating parts and those in the center come after the docks, before the widths.
        let mut docks_done = false;
        for (i, seg) in segments.iter().enumerate() {
            if let Some(entry) = seg.strip_prefix("g:") {
                docks_done = true;
                let (group, mode) = entry.split_once('=')?;
                if !is_group(group) || groups.insert(group.to_string(), GroupMode::parse(mode)?).is_some() {
                    return None;
                }
                continue;
            }
            if let Some(entry) = seg.strip_prefix("c:") {
                docks_done = true;
                let (part, from) = entry.split_once('=')?;
                if !valid_name(part) || !valid_name(from) || center.insert(part.to_string(), from.to_string()).is_some() {
                    return None;
                }
                continue;
            }
            let float = seg.strip_prefix("f:").map(|e| (e, false)).or_else(|| seg.strip_prefix("p:").map(|e| (e, true)));
            if let Some((entry, popped)) = float {
                docks_done = true;
                let (part, rest) = entry.split_once('=')?;
                let (from, rest) = rest.split_once('@')?;
                let (window, rect) = if popped {
                    let (display, rect) = rest.split_once('@')?;
                    if !valid_name(display) {
                        return None;
                    }
                    (Some(display.to_string()), rect)
                } else {
                    (None, rest)
                };
                let n: Vec<f32> = rect.split(',').map(number).collect::<Option<_>>()?;
                let [x, y, w, h] = n[..] else { return None };
                let ok = valid_name(part) && valid_name(from) && w > 0. && h > 0.;
                if !ok || floating.insert(part.to_string(), Float { from: from.to_string(), x, y, w, h, window }).is_some() {
                    return None;
                }
                continue;
            }
            if let Some(list) = seg.strip_prefix("w:") {
                if i != segments.len() - 1 {
                    return None;
                }
                for entry in list.split(',') {
                    let (name, w) = entry.split_once('=')?;
                    if !valid_name(name) || widths.insert(name.to_string(), number(w)?).is_some() {
                        return None;
                    }
                }
                continue;
            }
            let (name, tree) = seg.split_once('=')?;
            if !valid_name(name) || docks_done {
                return None;
            }
            let tree = if tree.is_empty() {
                None
            } else {
                let mut p = Parser { s: tree.as_bytes(), i: 0, depth: 0 };
                let t = p.tree()?;
                if p.i != p.s.len() {
                    return None;
                }
                Some(t)
            };
            if docks.insert(name.to_string(), tree).is_some() {
                return None;
            }
        }
        // A group's tree is where its docks are; a tree without its group isn't one.
        for g in groups.keys() {
            docks.entry(g.clone()).or_insert(None);
        }
        docks.retain(|name, _| !is_group(name) || groups.contains_key(name));
        Some(Arrangement { key: key.to_string(), docks, widths, floating, center, groups })
    }
}

/// The dock `part` starts in, or else the first that takes its kind.
fn default_dock<'d>(part: &str, kind: &str, docks: &'d [DockInfo]) -> Option<&'d DockInfo> {
    let takes = |d: &&DockInfo| d.takes(part, kind);
    let in_defaults = |d: &&DockInfo| d.defaults.has(part);
    docks.iter().find(|d| takes(d) && in_defaults(d)).or_else(|| docks.iter().find(takes))
}

fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(is_name_byte)
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-')
}

fn number(s: &str) -> Option<f32> {
    s.parse::<f32>().ok().filter(|n| n.is_finite())
}

fn encode_tree(tree: &Tree, out: &mut String) {
    match tree {
        Tree::Part(name) => out.push_str(name),
        Tree::Stack(names, active) => {
            out.push_str("[s");
            for (i, n) in names.iter().enumerate() {
                out.push_str(if i == *active { " *" } else { " " });
                out.push_str(n);
            }
            out.push(']');
        },
        Tree::Split(dir, items) => {
            out.push_str(if *dir == Dir::Row { "[r" } else { "[c" });
            for (t, size) in items {
                out.push(' ');
                encode_tree(t, out);
                match size {
                    Size::Px(px) => out.push_str(&format!("@{px}px")),
                    Size::Frac(f) => out.push_str(&format!("@{f}")),
                    Size::Auto => out.push_str("@-"),
                }
            }
            out.push(']');
        },
    }
}

/// Deepest nesting `decode` accepts.
const MAX_DEPTH: usize = 32;

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    depth: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn word(&mut self, accept: impl Fn(u8) -> bool) -> Option<&str> {
        let start = self.i;
        while self.peek().is_some_and(&accept) {
            self.i += 1;
        }
        std::str::from_utf8(&self.s[start..self.i]).ok().filter(|w| !w.is_empty())
    }

    fn tree(&mut self) -> Option<Tree> {
        self.depth += 1;
        let tree = if self.depth > MAX_DEPTH { None } else { self.tree_inner() };
        self.depth -= 1;
        tree
    }

    fn tree_inner(&mut self) -> Option<Tree> {
        if self.peek() != Some(b'[') {
            return self.word(is_name_byte).map(|n| Tree::Part(n.to_string()));
        }
        self.i += 1;
        let dir = match self.peek()? {
            b'r' => Dir::Row,
            b'c' => Dir::Column,
            b's' => {
                self.i += 1;
                return self.stack();
            },
            _ => return None,
        };
        self.i += 1;
        let mut items = Vec::new();
        while self.peek()? != b']' {
            if self.peek()? != b' ' {
                return None;
            }
            self.i += 1;
            let tree = self.tree()?;
            if self.peek()? != b'@' {
                return None;
            }
            self.i += 1;
            let size = self.word(|b| b != b' ' && b != b']')?;
            let size = match size {
                "-" => Size::Auto,
                s => match s.strip_suffix("px") {
                    Some(px) => Size::Px(number(px)?),
                    None => Size::Frac(number(s)?),
                },
            };
            items.push((tree, size));
        }
        self.i += 1;
        if items.is_empty() {
            return None;
        }
        Some(Tree::Split(dir, items))
    }

    /// A stack's parts after `[s`: ` name` each, `*` before the shown one.
    fn stack(&mut self) -> Option<Tree> {
        let mut names = Vec::new();
        let mut active = 0;
        while self.peek()? != b']' {
            if self.peek()? != b' ' {
                return None;
            }
            self.i += 1;
            if self.peek()? == b'*' {
                self.i += 1;
                active = names.len();
            }
            names.push(self.word(is_name_byte)?.to_string());
        }
        self.i += 1;
        match names.len() {
            0 => None,
            1 => names.pop().map(Tree::Part),
            _ => Some(Tree::Stack(names, active)),
        }
    }
}

/// The arrangements known to a workspace, by key, and which of them the user
/// changed. Only those are saved; the others follow the templates' defaults.
#[derive(Debug, Default)]
pub struct Kept {
    arrangements: BTreeMap<String, Arrangement>,
    touched: BTreeSet<String>,
}

impl Kept {
    /// What a session saved, as `(key, encoded)`; a key stored twice: the last wins.
    pub fn new<'a>(saved: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let mut kept = Kept::default();
        for (key, text) in saved {
            if let Some(a) = Arrangement::decode(key, text) {
                kept.arrangements.insert(key.to_string(), a);
                kept.touched.insert(key.to_string());
            }
        }
        kept
    }

    pub fn get(&self, key: &str) -> Option<&Arrangement> {
        self.arrangements.get(key)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Arrangement> {
        self.arrangements.get_mut(key)
    }

    /// The user changed `key`'s arrangement: keep it.
    pub fn touch(&mut self, key: &str) {
        self.touched.insert(key.to_string());
    }

    /// Forgets `key`'s arrangement; the next `fit` starts from the defaults.
    pub fn reset(&mut self, key: &str) {
        self.arrangements.remove(key);
        self.touched.remove(key);
    }

    /// Fits `key`'s arrangement to the templates now in effect: a touched one is
    /// reconciled, any other is rebuilt from the defaults. A kept arrangement that
    /// ends up equal to the defaults is no longer the user's, so start sizes apply.
    pub fn fit(&mut self, key: &str, docks: &[DockInfo], parts: &[(&str, &str)], center: &[String]) -> Vec<String> {
        if self.touched.contains(key)
            && let Some(a) = self.arrangements.get_mut(key)
        {
            let problems = a.reconcile(docks, parts, center);
            if *a == Arrangement::defaults(key, docks) {
                self.touched.remove(key);
            }
            return problems;
        }
        self.arrangements.insert(key.to_string(), Arrangement::defaults(key, docks));
        self.touched.remove(key);
        vec![]
    }

    /// The arrangements to keep, by key, sorted.
    pub fn saved(&self) -> Vec<(String, String)> {
        self.arrangements.iter().filter(|(k, _)| self.touched.contains(*k)).map(|(k, a)| (k.clone(), a.encode())).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(n: &str) -> Tree {
        Tree::Part(n.to_string())
    }

    fn docks() -> Vec<DockInfo> {
        vec![
            DockInfo {
                name: "left".into(),
                dir: Dir::Column,
                accepts: vec!["panel".into()],
                defaults: Tree::Split(Dir::Column, vec![(part("explorer"), Size::Auto), (part("scratches"), Size::Frac(0.3))]),
                grouped: false,
            },
            DockInfo { name: "right".into(), dir: Dir::Column, accepts: vec!["panel".into()], defaults: part("changes"), grouped: false },
        ]
    }

    const DEFAULT: &str = "left=[c explorer@- scratches@0.3];right=changes";

    fn fresh() -> Arrangement {
        Arrangement::defaults("workspace/left+right", &docks())
    }

    #[test]
    fn dragged_sizes_keep_all_but_the_largest_flexible_one() {
        // Two groups side by side, neither with a size: the smaller keeps its own.
        assert_eq!(dragged_sizes(&[80.0, 160.0], &[false, false]), [Size::Px(80.0), Size::Auto]);
        // Equal: the first takes up the rest.
        assert_eq!(dragged_sizes(&[100.0, 100.0], &[false, false]), [Size::Auto, Size::Px(100.0)]);
        // The panes and a group under them: the panes take up the rest.
        assert_eq!(dragged_sizes(&[600.0, 120.0, 40.0], &[false, false, true]), [Size::Auto, Size::Px(120.0), Size::Px(40.0)]);
        // All sized: all keep it.
        assert_eq!(dragged_sizes(&[60.0, 40.0], &[true, true]), [Size::Px(60.0), Size::Px(40.0)]);
    }

    #[test]
    fn encode_decode_round_trip() {
        let mut a = fresh();
        assert_eq!(a.encode(), DEFAULT);
        assert_eq!(Arrangement::decode("workspace/left+right", DEFAULT), Some(a.clone()));
        assert_eq!(Arrangement::decode("k", "left=[x"), None);
        assert_eq!(Arrangement::decode("k", "left=[c a b@-]"), None);
        assert_eq!(Arrangement::decode("k", "left=[c]"), None);

        a.widths.insert("left".into(), 240.0);
        a.widths.insert("right".into(), 300.0);
        a.docks.insert("right".into(), None);
        a.set_sizes("left", &[], &[Size::Px(120.5), Size::Auto]);
        let text = a.encode();
        assert_eq!(text, "left=[c explorer@120.5px scratches@-];right=;w:left=240,right=300");
        assert_eq!(Arrangement::decode(&a.key, &text), Some(a));
    }

    #[test]
    fn move_before_and_after() {
        let d = docks();
        let mut a = fresh();
        assert!(a.move_part("changes", "panel", "left", Drop::Before("explorer".into()), &d));
        assert_eq!(a.encode(), "left=[c changes@- explorer@- scratches@0.3];right=");
        assert!(a.move_part("changes", "panel", "right", Drop::End, &d));
        assert_eq!(a.encode(), DEFAULT);
        assert!(a.move_part("changes", "panel", "left", Drop::After("explorer".into()), &d));
        assert_eq!(a.encode(), "left=[c explorer@- changes@- scratches@0.3];right=");
        assert_eq!(a.dock_of("changes"), Some("left"));
        assert_eq!(a.dock_of("nope"), None);
    }

    #[test]
    fn move_next_to_itself_is_a_no_op() {
        let d = docks();
        let mut a = fresh();
        assert!(!a.move_part("explorer", "panel", "left", Drop::Before("explorer".into()), &d));
        assert!(!a.move_part("explorer", "panel", "left", Drop::After("explorer".into()), &d));
        assert!(!a.move_part("explorer", "panel", "left", Drop::Before("scratches".into()), &d));
        assert!(!a.move_part("scratches", "panel", "left", Drop::After("explorer".into()), &d));
        assert!(!a.move_part("scratches", "panel", "left", Drop::End, &d));
        assert!(!a.move_part("changes", "panel", "right", Drop::End, &d));
        assert!(!a.move_part("changes", "panel", "nowhere", Drop::End, &d));
        assert_eq!(a.encode(), DEFAULT);
    }

    #[test]
    fn move_refuses_kinds_not_accepted() {
        let mut a = fresh();
        assert!(!a.move_part("explorer", "pane", "left", Drop::End, &docks()));
        assert_eq!(a.encode(), DEFAULT);
    }

    #[test]
    fn shown_keeps_stored_places() {
        let a = Arrangement::decode("k", "left=[c a@- b@100px c@-]").unwrap();
        let tree = a.docks["left"].clone().unwrap();
        let s = shown(&tree, &|n| n != "b").unwrap();
        let Shown::Split { path, items, sizes, .. } = &s else { panic!("{s:?}") };
        assert!(path.is_empty());
        assert_eq!(items.iter().map(|(i, _)| *i).collect::<Vec<_>>(), [0, 2]);
        assert_eq!(sizes.len(), 3);

        let a = Arrangement::decode("k", "left=[c a@- [r b@- c@-]@-]").unwrap();
        let tree = a.docks["left"].clone().unwrap();
        let s = shown(&tree, &|n| n != "a").unwrap();
        let Shown::Split { dir, path, items, .. } = &s else { panic!("{s:?}") };
        assert_eq!((*dir, path.as_slice()), (Dir::Row, &[1][..]));
        assert_eq!(items.iter().map(|(i, _)| *i).collect::<Vec<_>>(), [0, 1]);
        assert_eq!(shown(&tree, &|n| n == "c"), Some(Shown::Part("c".into())));
        assert_eq!(shown(&tree, &|_| false), None);
    }

    #[test]
    fn resize_after_hiding_lands_on_the_stored_split() {
        // `b` hidden: the two shown sizes go to `a` and `c`, `b` keeps its own.
        let mut a = Arrangement::decode("k", "left=[c a@- b@100px c@-]").unwrap();
        let tree = a.docks["left"].clone().unwrap();
        let Some(Shown::Split { path, sizes, items, .. }) = shown(&tree, &|n| n != "b") else { panic!() };
        let kept: Vec<usize> = items.iter().map(|(i, _)| *i).collect();
        a.set_sizes("left", &path, &merge_sizes(&sizes, &kept, &[Size::Auto, Size::Px(50.)]));
        assert_eq!(a.encode(), "left=[c a@- b@100px c@50px]");

        // `a` hidden: the shown row is the stored one at [1], not the root.
        let mut a = Arrangement::decode("k", "left=[c a@- [r b@- c@-]@-]").unwrap();
        let tree = a.docks["left"].clone().unwrap();
        let Some(Shown::Split { path, sizes, items, .. }) = shown(&tree, &|n| n != "a") else { panic!() };
        let kept: Vec<usize> = items.iter().map(|(i, _)| *i).collect();
        a.set_sizes("left", &path, &merge_sizes(&sizes, &kept, &[Size::Px(70.), Size::Auto]));
        assert_eq!(a.encode(), "left=[c a@- [r b@70px c@-]@-]");
    }

    #[test]
    fn prune_hides_and_collapses() {
        let a = fresh();
        let tree = a.docks["left"].clone().unwrap();
        assert_eq!(prune(&tree, &|n| n == "scratches"), Some(part("scratches")));
        assert_eq!(prune(&tree, &|_| false), None);
        assert_eq!(prune(&tree, &|_| true), Some(tree.clone()));
        assert_eq!(a.encode(), DEFAULT);
    }

    #[test]
    fn reconcile_drops_and_adds_parts() {
        let parts = [("explorer", "panel"), ("scratches", "panel"), ("changes", "panel")];
        let mut a = Arrangement::decode("k", "left=[c explorer@- ghost@-];right=changes").unwrap();
        let problems = a.reconcile(&docks(), &parts, &[]);
        assert_eq!(a.encode(), "left=[c explorer@- scratches@-];right=changes");
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("ghost"));

        let mut a = Arrangement::decode("k", "left=[c explorer@- scratches@-];right=[c changes@- explorer@-]").unwrap();
        let problems = a.reconcile(&docks(), &parts, &[]);
        assert_eq!(a.encode(), "left=[c explorer@- scratches@-];right=changes");
        assert_eq!(problems, vec!["layout: \"explorer\" is placed twice".to_string()]);
    }

    #[test]
    fn reconcile_keeps_a_part_once_within_a_dock() {
        let parts = [("explorer", "panel"), ("scratches", "panel"), ("changes", "panel")];
        let mut a = Arrangement::decode("k", "left=[c explorer@- explorer@-];right=changes").unwrap();
        let problems = a.reconcile(&docks(), &parts, &[]);
        assert_eq!(a.encode(), "left=[c explorer@- scratches@-];right=changes");
        assert_eq!(problems, vec!["layout: \"explorer\" is placed twice".to_string()]);
    }

    #[test]
    fn reconcile_reports_parts_no_dock_takes() {
        let parts = [("explorer", "panel"), ("odd", "weird")];
        let mut a = Arrangement::decode("k", "left=explorer;right=odd").unwrap();
        let problems = a.reconcile(&docks(), &parts, &[]);
        assert_eq!(a.encode(), "left=explorer;right=");
        assert_eq!(problems, vec!["layout: \"odd\" has no dock".to_string()]);
    }

    #[test]
    fn decode_caps_nesting() {
        let deep = |n: usize| format!("left={}x{}", "[c ".repeat(n), "@-]".repeat(n));
        assert!(Arrangement::decode("k", &deep(10)).is_some());
        assert_eq!(Arrangement::decode("k", &deep(10_000)), None);
    }

    #[test]
    fn reconcile_moves_back_parts_that_no_longer_fit() {
        let parts = [("explorer", "panel"), ("scratches", "panel"), ("changes", "pane")];
        let mut a = Arrangement::decode("k", "gone=explorer;left=scratches;right=changes").unwrap();
        let mut d = docks();
        d[1].accepts = vec!["other".into()];
        d[1].defaults = part("changes");
        let problems = a.reconcile(&d, &parts, &[]);
        assert_eq!(a.encode(), "left=[c scratches@- explorer@-];right=");
        assert!(problems.iter().any(|p| p.contains("\"explorer\" moved back to dock \"left\"")));
    }

    #[test]
    fn arrangements_are_kept_per_key() {
        let d = docks();
        assert_eq!(key("workspace", &d), "workspace/left+right");
        assert_ne!(key("workspace", &d), key("workspace", &d[..1]));
    }

    #[test]
    fn a_dock_takes_a_part_by_name_or_kind() {
        let mut d = docks();
        d[1].accepts = vec!["explorer".into()];
        assert!(d[1].takes("explorer", "tool"));
        assert!(!d[1].takes("scratches", "panel"));
        assert!(d[0].takes("scratches", "panel"));
        let mut a = fresh();
        assert!(!a.move_part("scratches", "panel", "right", Drop::End, &d));
        assert!(a.move_part("explorer", "tool", "right", Drop::End, &d));
        assert_eq!(a.dock_of("explorer"), Some("right"));
    }

    const PARTS: [(&str, &str); 3] = [("explorer", "panel"), ("scratches", "panel"), ("changes", "panel")];

    #[test]
    fn reset_gives_defaults() {
        let d = docks();
        let mut a = fresh();
        assert!(a.move_part("changes", "panel", "left", Drop::Before("explorer".into()), &d));
        a.widths.insert("left".into(), 300.0);
        assert_ne!(a.encode(), DEFAULT);
        let a = Arrangement::defaults(&a.key, &d);
        assert_eq!(a.encode(), DEFAULT);
    }

    #[test]
    fn untouched_keys_follow_the_defaults() {
        let mut kept = Kept::default();
        assert!(kept.fit("k", &docks(), &PARTS, &[]).is_empty());
        assert_eq!(kept.get("k").unwrap().encode(), DEFAULT);
        // The templates change: an untouched key is rebuilt from the new defaults.
        let mut d = docks();
        d[0].defaults = part("explorer");
        d[1].defaults = Tree::Split(Dir::Column, vec![(part("changes"), Size::Auto), (part("scratches"), Size::Auto)]);
        kept.fit("k", &d, &PARTS, &[]);
        assert_eq!(kept.get("k").unwrap().encode(), "left=explorer;right=[c changes@- scratches@-]");
        assert!(kept.saved().is_empty());
    }

    #[test]
    fn touched_keys_reconcile_and_are_saved() {
        let mut kept = Kept::new([("k", "left=[c explorer@- ghost@-];right=changes")]);
        let problems = kept.fit("k", &docks(), &PARTS, &[]);
        assert_eq!(problems.len(), 1);
        assert_eq!(kept.get("k").unwrap().encode(), "left=[c explorer@- scratches@-];right=changes");
        assert_eq!(kept.saved(), vec![("k".to_string(), "left=[c explorer@- scratches@-];right=changes".to_string())]);
    }

    #[test]
    fn only_touched_keys_are_saved() {
        let mut kept = Kept::default();
        kept.fit("a", &docks(), &PARTS, &[]);
        kept.fit("b", &docks(), &PARTS, &[]);
        kept.touch("b");
        let saved = kept.saved();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].0, "b");
    }

    #[test]
    fn restored_defaults_are_untouched() {
        let mut kept = Kept::new([("k", DEFAULT)]);
        assert_eq!(kept.saved().len(), 1);
        kept.fit("k", &docks(), &PARTS, &[]);
        assert!(kept.saved().is_empty());
    }

    #[test]
    fn an_empty_dock_takes_a_part_at_its_end() {
        let mut d = docks();
        d[1].defaults = Tree::Split(Dir::Column, vec![]);
        let mut a = Arrangement::defaults("k", &d);
        assert_eq!(a.docks["right"], None);
        assert_eq!(Arrangement::decode("k", &a.encode()), Some(a.clone()));
        assert!(a.move_part("explorer", "panel", "right", Drop::End, &d));
        assert_eq!(a.encode(), "left=scratches;right=explorer");
        assert_eq!(Arrangement::decode("k", &a.encode()), Some(a));
        // An empty split, however it got there, is no panic either.
        let mut a = fresh();
        a.docks.insert("right".into(), Some(Tree::Split(Dir::Column, vec![])));
        assert!(a.move_part("changes", "panel", "right", Drop::End, &docks()));
    }

    #[test]
    fn defaults_round_trip() {
        let mut d = docks();
        d[0].defaults = Tree::Split(Dir::Column, vec![(part("explorer"), Size::Auto)]);
        let a = Arrangement::defaults("k", &d);
        assert_eq!(a.encode(), "left=explorer;right=changes");
        assert_eq!(Arrangement::decode("k", &a.encode()), Some(a));
    }

    #[test]
    fn switching_keys_and_back_restores_the_first() {
        let d = docks();
        let other = &d[..1];
        let mut kept = Kept::default();
        kept.fit("a", &d, &PARTS, &[]);
        assert!(kept.get_mut("a").unwrap().move_part("changes", "panel", "left", Drop::End, &d));
        kept.touch("a");
        let moved = kept.get("a").unwrap().encode();
        kept.fit("b", other, &PARTS, &[]);
        kept.fit("a", &d, &PARTS, &[]);
        assert_eq!(kept.get("a").unwrap().encode(), moved);
        assert_eq!(kept.saved(), vec![("a".to_string(), moved)]);
    }

    #[test]
    fn zones() {
        assert_eq!(Zone::at(0.5, 0.5), Zone::Center);
        assert_eq!(Zone::at(0.1, 0.5), Zone::Edge(Edge::Left));
        assert_eq!(Zone::at(0.9, 0.4), Zone::Edge(Edge::Right));
        assert_eq!(Zone::at(0.5, 0.05), Zone::Edge(Edge::Top));
        assert_eq!(Zone::at(0.45, 0.8), Zone::Edge(Edge::Bottom));
        // Kept to what the area takes.
        assert_eq!(Zone::pick(0.5, 0.5, true, false), Some(Zone::Edge(Edge::Left)));
        assert_eq!(Zone::pick(0.5, 0.9, true, false), Some(Zone::Edge(Edge::Bottom)));
        assert_eq!(Zone::pick(0.1, 0.5, false, true), Some(Zone::Center));
        assert_eq!(Zone::pick(0.1, 0.5, false, false), None);
    }

    #[test]
    fn edges_split_across_the_dock() {
        let d = docks();
        let mut a = fresh();
        // Across the column: a row in explorer's place, keeping its size.
        assert!(a.move_part("changes", "panel", "left", Drop::Edge("scratches".into(), Edge::Right), &d));
        assert_eq!(a.encode(), "left=[c explorer@- [r scratches@- changes@-]@0.3];right=");
        // Along the row: a sibling, not another split.
        assert!(a.move_part("explorer", "panel", "left", Drop::Edge("scratches".into(), Edge::Left), &d));
        assert_eq!(a.encode(), "left=[r explorer@- scratches@- changes@-];right=");
        // Along the column of a lone part: the root splits.
        assert!(a.move_part("changes", "panel", "right", Drop::End, &d));
        assert!(a.move_part("explorer", "panel", "right", Drop::Edge("changes".into(), Edge::Top), &d));
        assert_eq!(a.encode(), "left=scratches;right=[c explorer@- changes@-]");
        assert!(!a.move_part("changes", "panel", "right", Drop::Edge("changes".into(), Edge::Top), &d));
    }

    #[test]
    fn stacks_hold_parts_as_tabs() {
        let d = docks();
        let mut a = fresh();
        assert!(a.move_part("changes", "panel", "left", Drop::Stack("explorer".into()), &d));
        assert_eq!(a.encode(), "left=[c [s explorer *changes]@- scratches@0.3];right=");
        assert_eq!(Arrangement::decode(&a.key, &a.encode()), Some(a.clone()));
        // Already there.
        assert!(!a.move_part("changes", "panel", "left", Drop::Stack("explorer".into()), &d));
        assert!(a.set_active("left", &[0], "explorer"));
        assert!(!a.set_active("left", &[0], "explorer"));
        assert!(!a.set_active("left", &[1], "explorer"));
        assert_eq!(a.encode(), "left=[c [s *explorer changes]@- scratches@0.3];right=");
        // A third joins and shows; on an edge of a stacked part, the whole stack splits.
        assert!(a.move_part("scratches", "panel", "left", Drop::Stack("changes".into()), &d));
        assert_eq!(a.encode(), "left=[s explorer changes *scratches];right=");
        assert!(a.move_part("changes", "panel", "left", Drop::Edge("scratches".into(), Edge::Bottom), &d));
        assert_eq!(a.encode(), "left=[c [s explorer *scratches]@- changes@-];right=");
        // Before a stacked part: next to its stack.
        assert!(a.move_part("changes", "panel", "left", Drop::Before("explorer".into()), &d));
        assert_eq!(a.encode(), "left=[c changes@- [s explorer *scratches]@-];right=");
        // Leaving a stack of two leaves the other part alone.
        assert!(a.move_part("scratches", "panel", "right", Drop::End, &d));
        assert_eq!(a.encode(), "left=[c changes@- explorer@-];right=scratches");
    }

    #[test]
    fn hidden_parts_of_a_stack() {
        let a = Arrangement::decode("k", "left=[c x@- [s a *b c]@-]").unwrap();
        let tree = a.docks["left"].clone().unwrap();
        let Some(Shown::Split { items, .. }) = shown(&tree, &|_| true) else { panic!() };
        assert_eq!(items[1].1, Shown::Stack { path: vec![1], parts: vec!["a".into(), "b".into(), "c".into()], active: 1 });
        // The shown one hidden: the next one shows; past the end, the last.
        let Some(Shown::Split { items, .. }) = shown(&tree, &|n| n != "b") else { panic!() };
        assert_eq!(items[1].1, Shown::Stack { path: vec![1], parts: vec!["a".into(), "c".into()], active: 1 });
        let tree = Tree::Stack(vec!["a".into(), "b".into(), "c".into()], 2);
        assert_eq!(shown(&tree, &|n| n != "c"), Some(Shown::Stack { path: vec![], parts: vec!["a".into(), "b".into()], active: 1 }));
        // One left: drawn as that part.
        assert_eq!(shown(&tree, &|n| n == "a"), Some(Shown::Part("a".into())));
        assert_eq!(prune(&tree, &|n| n != "a"), Some(Tree::Stack(vec!["b".into(), "c".into()], 1)));
        assert_eq!(Arrangement::decode("k", "left=[s]"), None);
        assert_eq!(Arrangement::decode("k", "left=[s *a]"), Some(Arrangement::decode("k", "left=a").unwrap()));
        assert_eq!(Arrangement::decode("k", "left=[s a@- b]"), None);
    }

    #[test]
    fn reset_clears_touched_and_rebuilds() {
        let mut kept = Kept::new([("k", "left=explorer;right=[c changes@- scratches@-]")]);
        kept.fit("k", &docks(), &PARTS, &[]);
        kept.reset("k");
        assert!(kept.get("k").is_none());
        assert!(kept.saved().is_empty());
        kept.fit("k", &docks(), &PARTS, &[]);
        assert_eq!(kept.get("k").unwrap().encode(), DEFAULT);
    }
    const AREA: (f32, f32) = (1000., 800.);

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn floating_round_trips() {
        let d = docks();
        let mut a = fresh();
        assert!(a.float_part("scratches", rect(100., 200., 300., 400.), AREA));
        a.widths.insert("left".into(), 250.);
        let text = a.encode();
        assert_eq!(text, "left=explorer;right=changes;f:scratches=left@0.1,0.25,300,400;w:left=250");
        assert_eq!(Arrangement::decode("k", &text).map(|b| b.encode()), Some(text.clone()));
        // Docks after the floating parts, a bad rect, a part twice: malformed.
        assert_eq!(Arrangement::decode("k", "f:a=left@0,0,1,1;left=x"), None);
        assert_eq!(Arrangement::decode("k", "left=x;f:a=left@0,0,1"), None);
        assert_eq!(Arrangement::decode("k", "left=x;f:a=left@0,0,0,1"), None);
        assert_eq!(Arrangement::decode("k", "left=x;f:a=left@0,0,1,1;f:a=left@0,0,1,1"), None);
        assert!(a.dock_part("scratches", "panel", &d));
        assert_eq!(a.encode(), "left=[c explorer@- scratches@-];right=changes;w:left=250");
    }

    #[test]
    fn floating_moves_and_docks() {
        let d = docks();
        let mut a = fresh();
        // Not a part anywhere: nothing to float.
        assert!(!a.float_part("ghost", rect(0., 0., 10., 10.), AREA));
        assert!(a.float_part("changes", rect(0., 0., 300., 400.), AREA));
        // Moving it keeps the dock it came from.
        assert!(a.float_part("changes", rect(500., 400., 300., 400.), AREA));
        assert!(!a.float_part("changes", rect(500., 400., 300., 400.), AREA));
        assert_eq!(a.floating["changes"].from, "right");
        // Dropped on a zone: docked there, floating no more.
        assert!(a.move_part("changes", "panel", "left", Drop::End, &d));
        assert!(a.floating.is_empty());
        assert_eq!(a.dock_of("changes"), Some("left"));
        assert!(!a.dock_part("changes", "panel", &d));
    }

    #[test]
    fn reconcile_keeps_floating_parts() {
        let parts = [("explorer", "panel"), ("scratches", "panel"), ("changes", "panel")];
        let mut a = Arrangement::decode("k", "left=explorer;right=changes;f:scratches=gone@0,0,10,10;f:ghost=left@0,0,10,10").unwrap();
        let problems = a.reconcile(&docks(), &parts, &[]);
        assert_eq!(a.encode(), "left=explorer;right=changes;f:scratches=left@0,0,10,10");
        assert!(problems.iter().any(|p| p.contains("\"ghost\" is not a part")));
        // Floating and docked both: the dock wins.
        let mut a = Arrangement::decode("k", "left=explorer;right=changes;f:explorer=left@0,0,10,10").unwrap();
        let problems = a.reconcile(&docks(), &parts, &[]);
        assert!(a.floating.is_empty());
        assert!(problems.iter().any(|p| p.contains("placed twice")));
    }

    #[test]
    fn parts_in_the_center() {
        let d = docks();
        let parts = [("explorer", "panel"), ("scratches", "panel"), ("changes", "panel")];
        let mut a = fresh();
        // From a dock: its button stays with the dock.
        assert!(a.center_part("changes"));
        assert!(!a.center_part("changes"));
        assert!(!a.center_part("ghost"));
        assert_eq!(a.center["changes"], "right");
        assert_eq!(a.dock_of("changes"), None);
        let s = a.encode();
        assert!(s.ends_with(";c:changes=right"), "{s}");
        assert_eq!(Arrangement::decode("k", &s), Some(Arrangement { key: "k".into(), ..a.clone() }));
        assert_eq!(Arrangement::decode("k", "left=;c:changes=right;c:changes=left"), None);
        assert_eq!(Arrangement::decode("k", "c:changes=right;left="), None);
        // Floated from there, it still came from its dock; Dock sends it back.
        assert!(a.float_part("changes", rect(0., 0., 100., 100.), AREA));
        assert!(a.center.is_empty());
        assert_eq!(a.floating["changes"].from, "right");
        assert!(a.center_part("changes"));
        assert!(a.floating.is_empty());
        assert!(a.dock_part("changes", "panel", &d));
        assert!(a.center.is_empty());
        assert_eq!(a.dock_of("changes"), Some("right"));
        // Moved to a dock from the center.
        assert!(a.center_part("explorer"));
        assert!(a.move_part("explorer", "panel", "right", Drop::End, &d));
        assert!(a.center.is_empty());
        // Kept while the center takes it; else it docks back where it came from.
        assert!(a.center_part("explorer"));
        let mut kept = a.clone();
        assert_eq!(kept.reconcile(&d, &parts, &["panel".into()]), Vec::<String>::new());
        assert_eq!(kept.center["explorer"], "right");
        let problems = a.reconcile(&d, &parts, &[]);
        assert!(a.center.is_empty());
        assert_eq!(a.dock_of("explorer"), Some("right"));
        assert!(problems.iter().any(|p| p.contains("moved back")), "{problems:?}");
    }

    #[test]
    fn popped_parts_round_trip_and_come_back() {
        let d = docks();
        let mut a = fresh();
        // From a dock: its window at those bounds, its button with the dock.
        assert!(a.pop_out("changes", "ab-12", rect(-20., 40., 300., 500.)));
        assert!(!a.pop_out("changes", "ab-12", rect(-20., 40., 300., 500.)));
        assert!(!a.pop_out("ghost", "ab-12", rect(0., 0., 10., 10.)));
        assert_eq!(a.floating["changes"].from, "right");
        assert_eq!(a.floating["changes"].window_rect(), Some(rect(-20., 40., 300., 500.)));
        let text = a.encode();
        assert_eq!(text, "left=[c explorer@- scratches@0.3];right=;p:changes=right@ab-12@-20,40,300,500");
        assert_eq!(Arrangement::decode("k", &text).map(|b| b.encode()), Some(text.clone()));
        // A bad display, a missing rect, a window between the docks: malformed.
        assert_eq!(Arrangement::decode("k", "left=x;p:a=left@a b@0,0,1,1"), None);
        assert_eq!(Arrangement::decode("k", "left=x;p:a=left@0,0,1,1"), None);
        assert_eq!(Arrangement::decode("k", "p:a=left@d@0,0,1,1;left=x"), None);
        // An unknown display is stored as `-`.
        assert!(a.pop_out("changes", "", rect(0., 0., 300., 500.)));
        assert_eq!(a.floating["changes"].window.as_deref(), Some("-"));
        // Floated back into the main window: same dock to go back to.
        assert!(a.float_part("changes", rect(100., 200., 300., 400.), AREA));
        assert_eq!(a.floating["changes"].window, None);
        assert_eq!(a.floating["changes"].from, "right");
        // From floating to a window, and docked from there.
        assert!(a.pop_out("changes", "ab-12", rect(0., 0., 300., 500.)));
        assert!(a.dock_part("changes", "panel", &d));
        assert_eq!(a.encode(), DEFAULT);
    }

    #[test]
    fn floating_rects_stay_in_the_area() {
        let f = Float::at("left", rect(900., 700., 300., 400.), AREA);
        // A smaller area: no bigger than it, and inside it.
        assert_eq!(f.rect((200., 300.)), rect(0., 0., 200., 300.));
        assert_eq!(f.rect(AREA), rect(700., 400., 300., 400.));
        assert_eq!(moved(rect(10., 10., 100., 100.), -50., 2000., AREA), rect(0., 700., 100., 100.));
        let r = rect(100., 100., 300., 200.);
        let left_top = Sides { left: true, top: true, ..Default::default() };
        assert_eq!(resized(r, left_top, -500., 50., (120., 80.), AREA), rect(0., 150., 400., 150.));
        let right = Sides { right: true, ..Default::default() };
        assert_eq!(resized(r, right, -1000., 0., (120., 80.), AREA), rect(100., 100., 120., 200.));
        assert_eq!(resized(r, right, 1000., 0., (120., 80.), AREA), rect(100., 100., 900., 200.));
    }

    #[test]
    fn a_new_group_is_empty_and_takes_panels() {
        let d = docks();
        let mut a = fresh();
        let g = a.add_group("right", Drop::End, &d).unwrap();
        assert_eq!(g, "group-1");
        assert_eq!(a.add_group("right", Drop::End, &d).as_deref(), Some("group-2"));
        assert!(a.move_part("explorer", "panel", &g, Drop::End, &d));
        assert!(a.move_part("changes", "panel", &g, Drop::Edge("explorer".into(), Edge::Right), &d));
        assert_eq!(a.docks[&g], Some(Tree::Split(Dir::Row, vec![(part("explorer"), Size::Auto), (part("changes"), Size::Auto)])));
        assert_eq!(a.dock_of("changes"), Some("group-1"));
        assert_eq!(a.docks["right"].as_ref().unwrap().parts(), ["group-1", "group-2"]);
    }

    #[test]
    fn a_group_never_goes_into_itself() {
        let d = docks();
        let mut a = fresh();
        let g1 = a.add_group("right", Drop::End, &d).unwrap();
        let g2 = a.add_group("right", Drop::End, &d).unwrap();
        assert!(!a.move_part(&g1, "panel", &g1, Drop::End, &d));
        assert!(a.move_part(&g2, "panel", &g1, Drop::End, &d));
        assert!(!a.move_part(&g1, "panel", &g2, Drop::End, &d));
        assert!(a.move_part(&g1, "panel", "left", Drop::End, &d));
        assert!(a.group_holds(&g1, &g2));
    }

    #[test]
    fn group_modes_lay_out_its_panels() {
        let d = docks();
        let mut a = fresh();
        let g = a.add_group("right", Drop::End, &d).unwrap();
        assert!(a.move_part("explorer", "panel", &g, Drop::End, &d));
        assert!(a.move_part("changes", "panel", &g, Drop::Edge("explorer".into(), Edge::Right), &d));
        assert!(a.set_group_mode(&g, GroupMode::Tabs));
        assert_eq!(a.docks[&g], Some(Tree::Stack(vec!["explorer".into(), "changes".into()], 0)));
        assert!(a.set_group_mode(&g, GroupMode::Stack));
        assert_eq!(a.docks[&g], Some(Tree::Split(Dir::Column, vec![(part("explorer"), Size::Auto), (part("changes"), Size::Auto)])));
        assert!(!a.set_group_mode(&g, GroupMode::Stack));
        assert_eq!(GroupMode::Stack.zones(), ["before", "after"]);
    }

    #[test]
    fn groups_round_trip_and_survive_reconcile() {
        let d = docks();
        let mut a = fresh();
        let g = a.add_group("right", Drop::End, &d).unwrap();
        assert!(a.move_part("changes", "panel", &g, Drop::End, &d));
        assert!(a.move_part(&g, "panel", "left", Drop::End, &d));
        a.set_group_mode(&g, GroupMode::Tabs);
        let text = a.encode();
        assert_eq!(text, "group-1=changes;left=[c explorer@- scratches@0.3 group-1@-];right=;g:group-1=tabs");
        let mut back = Arrangement::decode(&a.key, &text).unwrap();
        assert_eq!(back, a);
        assert_eq!(back.reconcile(&d, &PARTS, &[]), Vec::<String>::new());
        assert_eq!(back, a);
        // A group's tree without the group isn't kept.
        assert_eq!(Arrangement::decode("k", "group-3=changes;left=").unwrap().docks.get("group-3"), None);
    }

    #[test]
    fn groups_inside_only_each_other_go_back_to_a_dock() {
        let d = docks();
        let mut a = Arrangement::decode(
            "k",
            "group-1=group-2;group-2=group-1;left=[c explorer@- scratches@-];right=changes;g:group-1=grid;g:group-2=grid",
        )
        .unwrap();
        let problems = a.reconcile(&d, &PARTS, &[]);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(a.docks["left"].as_ref().unwrap().has("group-1"));
        assert!(a.docks["group-1"].as_ref().unwrap().has("group-2"));
    }

    #[test]
    fn closing_a_group_puts_its_panels_back() {
        let d = docks();
        let mut a = fresh();
        // In a dock's split: its panels take its place.
        let g = a.add_group("right", Drop::End, &d).unwrap();
        assert!(a.move_part("scratches", "panel", &g, Drop::End, &d));
        assert!(a.close_group(&g, &d, &PARTS));
        assert!(a.groups.is_empty() && !a.docks.contains_key(&g));
        assert_eq!(a.docks["right"].as_ref().unwrap().parts(), ["changes", "scratches"]);
        // In a stack: each goes back to its default dock.
        let g = a.add_group("right", Drop::Stack("changes".into()), &d).unwrap();
        assert!(a.move_part("explorer", "panel", &g, Drop::End, &d));
        assert!(a.close_group(&g, &d, &PARTS));
        assert!(a.docks["left"].as_ref().unwrap().has("explorer"));
        assert!(!a.docks["right"].as_ref().unwrap().has(&g));
    }

    fn grouped_docks() -> Vec<DockInfo> {
        docks().into_iter().map(|d| DockInfo { grouped: true, ..d }).collect()
    }

    #[test]
    fn grouped_docks_hold_only_groups() {
        let d = grouped_docks();
        let mut a = Arrangement::defaults("k", &d);
        assert_eq!(
            a.docks["left"],
            Some(Tree::Split(Dir::Column, vec![(part("group-1"), Size::Auto), (part("group-2"), Size::Frac(0.3))]))
        );
        assert_eq!(a.docks["group-1"], Some(part("explorer")));
        // Dropped between groups: in a new group of its own, there.
        assert!(a.move_part("changes", "panel", "left", Drop::After("group-1".into()), &d));
        assert_eq!(a.docks["left"].as_ref().unwrap().parts(), ["group-1", "group-4", "group-2"]);
        assert_eq!(a.docks["group-4"], Some(part("changes")));
        // The group it left stays, empty.
        assert_eq!(a.docks["group-3"], None);
        assert!(a.groups.contains_key("group-3"));
        // Onto a group: into it.
        assert!(a.move_part("scratches", "panel", "left", Drop::Stack("group-1".into()), &d));
        assert!(a.docks["group-1"].as_ref().unwrap().has("scratches"));
        // An empty group added below another.
        assert_eq!(a.add_group("left", Drop::After("group-4".into()), &d), Some("group-5".into()));
        assert_eq!(a.docks["left"].as_ref().unwrap().parts(), ["group-1", "group-4", "group-5", "group-2"]);
        // And one beside another: the two split its place in a row.
        assert_eq!(a.add_group("left", Drop::Edge("group-5".into(), Edge::Right), &d), Some("group-6".into()));
        assert_eq!(a.docks["left"].as_ref().unwrap().parts(), ["group-1", "group-4", "group-5", "group-6", "group-2"]);
        assert!(a.encode().contains("[r group-5@- group-6@-]"), "{}", a.encode());
    }
}
