//! Menus as data: a `menu.<name>` template's entries worked out with the
//! code's values, apart from drawing them (`draw::menu` fills a gpui popup).

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::SharedString;

use super::draw::lookup;
use super::{Node, Scope, Templates, Value, What, fill_parts};

/// One entry of a menu.
#[derive(Clone)]
pub enum Entry {
    Separator,
    /// Text that can't be clicked.
    Label(SharedString),
    Item(Item),
    Submenu {
        label: SharedString,
        entries: Vec<Entry>,
    },
}

/// An entry that does something: the action it dispatches, or what the code
/// attaches to its name.
#[derive(Clone)]
pub struct Item {
    pub frame: SharedString,
    /// The list entry it was drawn for, if any.
    pub index: Option<usize>,
    pub action: Option<SharedString>,
    pub label: SharedString,
    pub icon: Option<SharedString>,
    pub checked: bool,
    pub disabled: bool,
    /// Drawn instead of the label: the item's node, and the values it sees.
    pub content: Option<(Arc<Node>, Content)>,
}

/// The values an item's content is drawn with.
#[derive(Clone)]
pub struct Content {
    pub vars: HashMap<&'static str, Value>,
    pub index: Option<usize>,
}

impl Templates {
    /// The entries of menu template `name` with `scope`'s values and lists.
    pub fn menu(&self, name: &str, mut scope: Scope) -> Vec<Entry> {
        let Some(root) = self.nodes.get(name) else { return vec![] };
        let mut out = vec![];
        for c in &root.children {
            entries(c, &mut scope, &[], &mut out);
        }
        tidy(out)
    }
}

/// `n`'s entries, onto `out`.
fn entries(n: &Node, scope: &mut Scope, outer: &[&HashMap<&'static str, Value>], out: &mut Vec<Entry>) {
    if n.when.as_ref().is_some_and(|c| !c.holds(&lookup(scope, outer))) {
        return;
    }
    match &n.what {
        What::Separator => out.push(Entry::Separator),
        What::Text(parts) => out.push(Entry::Label(fill_parts(parts, &lookup(scope, outer)).into())),
        What::Button(b) => {
            let get = lookup(scope, outer);
            let holds = |c: &Option<super::Cond>| c.as_ref().is_some_and(|c| c.holds(&get));
            let content = (!n.children.is_empty()).then(|| {
                let mut vars: HashMap<&'static str, Value> = HashMap::new();
                for o in outer {
                    vars.extend(o.iter().map(|(k, v)| (*k, v.clone())));
                }
                vars.extend(scope.vars.iter().map(|(k, v)| (*k, v.clone())));
                (Arc::new(n.clone()), Content { vars, index: scope.index })
            });
            out.push(Entry::Item(Item {
                frame: b.frame.clone(),
                index: scope.index,
                action: b.action.clone(),
                label: b.label.as_ref().map(|l| fill_parts(l, &get).into()).unwrap_or_default(),
                icon: b.icon.as_ref().map(|i| fill_parts(i, &get).into()).filter(|i: &SharedString| !i.is_empty()),
                checked: holds(&b.selected),
                disabled: holds(&b.disabled),
                content,
            }));
        },
        What::Each(list, item) => {
            let items = scope.lists.remove(list.as_ref()).unwrap_or_default();
            let mut chain = outer.to_vec();
            chain.push(&scope.vars);
            for mut entry in items {
                entries(item, &mut entry, &chain, out);
            }
        },
        What::Box => {
            let label = n.label.as_ref().map(|l| fill_parts(l, &lookup(scope, outer)));
            let mut kids = vec![];
            for c in &n.children {
                entries(c, scope, outer, &mut kids);
            }
            match label {
                Some(label) => out.push(Entry::Submenu { label: label.into(), entries: tidy(kids) }),
                // A `use`d menu, or a group: its entries in place.
                None => out.extend(kids),
            }
        },
        What::Slot(_) | What::Part(_) | What::Icon(_) => {},
    }
}

/// `entries` without separators at the ends or next to each other.
fn tidy(entries: Vec<Entry>) -> Vec<Entry> {
    let mut out: Vec<Entry> = vec![];
    for e in entries {
        if matches!(e, Entry::Separator) && out.last().is_none_or(|l| matches!(l, Entry::Separator)) {
            continue;
        }
        out.push(e);
    }
    if matches!(out.last(), Some(Entry::Separator)) {
        out.pop();
    }
    out
}
