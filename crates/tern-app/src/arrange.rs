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

/// What a dock holds: a single part, or a split of subtrees.
#[derive(Debug, Clone, PartialEq)]
pub enum Tree {
    Part(String),
    Split(Dir, Vec<(Tree, Size)>),
}

/// Where a part is dropped, relative to a part already in the dock.
#[derive(Debug, Clone, PartialEq)]
pub enum Drop {
    Before(String),
    After(String),
    End,
}

/// A dock as the active template declares it.
#[derive(Debug, Clone, PartialEq)]
pub struct DockInfo {
    pub name: String,
    pub dir: Dir,
    /// Part kinds the dock takes.
    pub accepts: Vec<String>,
    pub defaults: Tree,
}

/// The parts of every dock, stored under a key that names the template and its docks.
#[derive(Debug, Clone, PartialEq)]
pub struct Arrangement {
    pub key: String,
    /// Dock name to its tree; `None` is an empty dock.
    pub docks: BTreeMap<String, Option<Tree>>,
    /// Stored dock widths, by dock name.
    pub widths: BTreeMap<String, f32>,
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
    match tree {
        Tree::Part(name) => shown(name).then(|| tree.clone()),
        Tree::Split(dir, items) => {
            let mut kept: Vec<(Tree, Size)> = items.iter().filter_map(|(t, s)| prune(t, shown).map(|t| (t, *s))).collect();
            match kept.len() {
                0 => None,
                1 => kept.pop().map(|(t, _)| t),
                _ => Some(Tree::Split(*dir, kept)),
            }
        },
    }
}

fn collect_parts<'a>(tree: &'a Tree, out: &mut Vec<&'a str>) {
    match tree {
        Tree::Part(name) => out.push(name),
        Tree::Split(_, items) => items.iter().for_each(|(t, _)| collect_parts(t, out)),
    }
}

/// Inserts `part` next to `target` in the split that holds it; false when `target` isn't in `tree`.
fn insert_next_to(tree: &mut Tree, target: &str, part: &str, after: bool) -> bool {
    let Tree::Split(_, items) = tree else {
        return false;
    };
    if let Some(i) = items.iter().position(|(t, _)| matches!(t, Tree::Part(n) if n == target)) {
        let at = if after { i + 1 } else { i };
        items.insert(at, (Tree::Part(part.to_string()), Size::Auto));
        return true;
    }
    items.iter_mut().any(|(t, _)| insert_next_to(t, target, part, after))
}

/// Puts `part` into `root` at `at`; false when the drop target isn't there.
fn place(root: &mut Option<Tree>, dir: Dir, part: &str, at: &Drop) -> bool {
    let new = Tree::Part(part.to_string());
    match at {
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
                Some(Tree::Part(only)) if only == target => {
                    let old = Tree::Part(only.clone());
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
    /// The arrangement a template's docks start with.
    pub fn defaults(key: &str, docks: &[DockInfo]) -> Self {
        Arrangement {
            key: key.to_string(),
            docks: docks.iter().map(|d| (d.name.clone(), Some(d.defaults.clone()))).collect(),
            widths: BTreeMap::new(),
        }
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
        let Some(info) = docks.iter().find(|d| d.name == dock) else {
            return false;
        };
        if !info.accepts.iter().any(|k| k == kind) {
            return false;
        }
        if matches!(&at, Drop::Before(t) | Drop::After(t) if t == part) {
            return false;
        }
        let mut next = self.clone();
        let not_part = |name: &str| name != part;
        for tree in next.docks.values_mut() {
            *tree = tree.as_ref().and_then(|t| prune(t, &not_part));
        }
        let root = next.docks.entry(dock.to_string()).or_insert(None);
        if !place(root, info.dir, part, &at) || next == *self {
            return false;
        }
        *self = next;
        true
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

    /// Fits a stored arrangement to the templates in effect; returns the problems.
    pub fn reconcile(&mut self, docks: &[DockInfo], parts: &[(&str, &str)]) -> Vec<String> {
        let kind_of = |part: &str| parts.iter().find(|(n, _)| *n == part).map(|(_, k)| *k);
        let info_of = |name: &str| docks.iter().find(|d| d.name == name);
        let mut problems = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut moved_back: Vec<String> = Vec::new();

        let stored = std::mem::take(&mut self.docks);
        for (name, tree) in stored {
            let info = info_of(&name);
            let mut names = Vec::new();
            if let Some(tree) = &tree {
                collect_parts(tree, &mut names);
            }
            let mut keep: BTreeSet<String> = BTreeSet::new();
            for part in names {
                let Some(kind) = kind_of(part) else {
                    problems.push(format!("layout: \"{part}\" is not a part any more"));
                    continue;
                };
                if !seen.insert(part.to_string()) {
                    problems.push(format!("layout: \"{part}\" is placed twice"));
                    continue;
                }
                match info {
                    Some(i) if i.accepts.iter().any(|k| k == kind) => {
                        keep.insert(part.to_string());
                    },
                    _ => moved_back.push(part.to_string()),
                }
            }
            if let Some(info) = info {
                let tree = tree.and_then(|t| prune(&t, &|n| keep.contains(n)));
                self.docks.insert(info.name.clone(), tree);
            }
        }
        for info in docks {
            self.docks.entry(info.name.clone()).or_insert(None);
        }

        for part in moved_back {
            if let Some(dock) = self.send_to_default(&part, kind_of(&part), docks) {
                problems.push(format!("layout: \"{part}\" moved back to dock \"{dock}\""));
            }
        }
        for (part, kind) in parts {
            if seen.contains(*part) {
                continue;
            }
            self.send_to_default(part, Some(kind), docks);
        }
        problems
    }

    /// Appends `part` to its default dock (or the first dock that takes its kind); the dock's name.
    fn send_to_default(&mut self, part: &str, kind: Option<&str>, docks: &[DockInfo]) -> Option<String> {
        let kind = kind?;
        let takes = |d: &&DockInfo| d.accepts.iter().any(|k| k == kind);
        let in_defaults = |d: &&DockInfo| {
            let mut names = Vec::new();
            collect_parts(&d.defaults, &mut names);
            names.contains(&part)
        };
        let info = docks.iter().find(|d| takes(d) && in_defaults(d)).or_else(|| docks.iter().find(takes))?;
        let root = self.docks.entry(info.name.clone()).or_insert(None);
        place(root, info.dir, part, &Drop::End);
        Some(info.name.clone())
    }

    /// One line: `name=tree` per dock joined by `;`, then `;w:name=px,...` when widths are stored.
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
        let segments: Vec<&str> = text.split(';').collect();
        for (i, seg) in segments.iter().enumerate() {
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
            if !valid_name(name) {
                return None;
            }
            let tree = if tree.is_empty() {
                None
            } else {
                let mut p = Parser { s: tree.as_bytes(), i: 0 };
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
        Some(Arrangement { key: key.to_string(), docks, widths })
    }
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

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
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
        if self.peek() != Some(b'[') {
            return self.word(is_name_byte).map(|n| Tree::Part(n.to_string()));
        }
        self.i += 1;
        let dir = match self.peek()? {
            b'r' => Dir::Row,
            b'c' => Dir::Column,
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
            },
            DockInfo { name: "right".into(), dir: Dir::Column, accepts: vec!["panel".into()], defaults: part("changes") },
        ]
    }

    const DEFAULT: &str = "left=[c explorer@- scratches@0.3];right=changes";

    fn fresh() -> Arrangement {
        Arrangement::defaults("workspace/left+right", &docks())
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
        let problems = a.reconcile(&docks(), &parts);
        assert_eq!(a.encode(), "left=[c explorer@- scratches@-];right=changes");
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("ghost"));

        let mut a = Arrangement::decode("k", "left=[c explorer@- scratches@-];right=[c changes@- explorer@-]").unwrap();
        let problems = a.reconcile(&docks(), &parts);
        assert_eq!(a.encode(), "left=[c explorer@- scratches@-];right=changes");
        assert_eq!(problems, vec!["layout: \"explorer\" is placed twice".to_string()]);
    }

    #[test]
    fn reconcile_moves_back_parts_that_no_longer_fit() {
        let parts = [("explorer", "panel"), ("scratches", "panel"), ("changes", "pane")];
        let mut a = Arrangement::decode("k", "gone=explorer;left=scratches;right=changes").unwrap();
        let mut d = docks();
        d[1].accepts = vec!["other".into()];
        d[1].defaults = part("changes");
        let problems = a.reconcile(&d, &parts);
        assert_eq!(a.encode(), "left=[c scratches@- explorer@-];right=");
        assert!(problems.iter().any(|p| p.contains("\"explorer\" moved back to dock \"left\"")));
    }

    #[test]
    fn arrangements_are_kept_per_key() {
        let d = docks();
        assert_eq!(key("workspace", &d), "workspace/left+right");
        assert_ne!(key("workspace", &d), key("workspace", &d[..1]));
    }
}
