//! Drawing compiled templates with gpui.

use std::collections::HashMap;

use gpui_kit::component::resizable::ResizablePanel;
use gpui_kit::component::{h_resizable, resizable_panel, v_resizable};
use gpui_kit::*;

use std::sync::Arc;

use super::{Align, Cond, DockNode, Justify, Layout, Node, Panel, Part, Scope, Templates, Value, What};
use crate::arrange::{self, Shown, Size};
use crate::dock::Change;
use crate::style::{self, Val};

/// Attaches behavior to a box named by `frame`: gets the frame's name, the
/// list entry it is in (if any) and the box; gives back what to draw.
pub type Frames<'a> = dyn FnMut(&str, Option<usize>, Div) -> AnyElement + 'a;

/// Draws template `name` with `scope`; `frames` attaches behavior to the
/// boxes that name a frame.
pub fn render(name: &str, scope: Scope, frames: &mut Frames, cx: &App) -> AnyElement {
    let Some(node) = cx.try_global::<Templates>().and_then(|t| t.nodes.get(name)).cloned() else {
        return div().into_any_element();
    };
    let mut scope = scope;
    let mut d = Drawer { frames, cx, rem: cx.theme_rem() };
    d.node(&node, &mut scope, &[]).unwrap_or_else(|| div().into_any_element())
}

trait Rem {
    fn theme_rem(&self) -> Pixels;
}

impl Rem for App {
    fn theme_rem(&self) -> Pixels {
        use gpui_kit::component::ActiveTheme as _;
        self.theme().font_size
    }
}

struct Drawer<'a, 'f> {
    frames: &'a mut Frames<'f>,
    cx: &'a App,
    rem: Pixels,
}

/// The values a node sees: its scope's, then those of the scopes around it.
fn lookup<'s>(scope: &'s Scope, outer: &'s [&'s HashMap<&'static str, Value>]) -> impl Fn(&str) -> Option<Value> + 's {
    move |name: &str| match name {
        "true" => Some(Value::Bool(true)),
        "false" => Some(Value::Bool(false)),
        "index" => scope.index.map(Value::from),
        _ => scope.vars.get(name).or_else(|| outer.iter().rev().find_map(|v| v.get(name))).cloned(),
    }
}

impl Drawer<'_, '_> {
    fn holds(&self, cond: &Option<Cond>, scope: &Scope, outer: &[&HashMap<&'static str, Value>]) -> bool {
        cond.as_ref().is_none_or(|c| c.holds(&lookup(scope, outer)))
    }

    fn node(&mut self, n: &Node, scope: &mut Scope, outer: &[&HashMap<&'static str, Value>]) -> Option<AnyElement> {
        if !self.holds(&n.when, scope, outer) || self.dock_hidden(n, scope, outer) {
            return None;
        }
        self.draw(n, scope, outer)
    }

    /// `n`, its `when` aside (a split's panel hides rather than goes).
    fn draw(&mut self, n: &Node, scope: &mut Scope, outer: &[&HashMap<&'static str, Value>]) -> Option<AnyElement> {
        let look = self.look(n, scope, outer);
        let mut kids: Vec<AnyElement> = vec![];
        match &n.what {
            What::Slot(name) | What::Part(name) => {
                let e = scope.slots.remove(name)?;
                if n.is_bare() {
                    return Some(e);
                }
                kids.push(e);
            },
            What::Text(parts) => {
                let get = lookup(scope, outer);
                let text: String = parts
                    .iter()
                    .map(|p| match p {
                        Part::Lit(t) => t.to_string(),
                        Part::Var(v) => get(v).map(|v| v.text().to_string()).unwrap_or_default(),
                    })
                    .collect();
                kids.push(look.label(text).into_any_element());
            },
            What::Each(list, item) => {
                let items = scope.lists.remove(list.as_ref()).unwrap_or_default();
                let mut chain = outer.to_vec();
                chain.push(&scope.vars);
                for mut entry in items {
                    kids.extend(self.node(item, &mut entry, &chain));
                }
            },
            What::Box => {},
        }
        if let Some(d) = &n.dock {
            kids.extend(self.dock_tree(d, scope, outer).map(|t| self.tree(d, &t, scope, outer)));
        } else if let Some(axis) = n.split {
            kids.push(self.split(n, axis, scope, outer));
        } else {
            for c in &n.children {
                if let Some(e) = self.node(c, scope, outer) {
                    kids.push(e);
                }
            }
        }
        Some(self.boxed(n, &look, kids, scope.index))
    }

    fn split(&mut self, n: &Node, axis: Axis, scope: &mut Scope, outer: &[&HashMap<&'static str, Value>]) -> AnyElement {
        let id = SharedString::from(format!("{}:{}", scope.id, n.path));
        let mut group = match axis {
            Axis::Horizontal => h_resizable(id),
            Axis::Vertical => v_resizable(id),
        };
        // The docks shown here, by panel, with the width they start the drag at:
        // a resize stores the widths that changed.
        let mut docks: Vec<(usize, SharedString, Option<Pixels>)> = vec![];
        for (i, c) in n.children.iter().enumerate() {
            let shown = self.holds(&c.when, scope, outer) && !self.dock_hidden(c, scope, outer);
            let fixed = c.panel.fixed.as_ref().is_some_and(|f| f.holds(&lookup(scope, outer)));
            let mut size = c.panel.size.map(|l| l.to_pixels(self.rem));
            if let Some(d) = &c.dock {
                if let Some(w) = scope.arrangement.as_ref().and_then(|a| a.widths.get(&*d.name)) {
                    size = Some(px(*w));
                }
                if shown {
                    docks.push((i, d.name.clone(), size));
                }
            }
            let body = if shown { self.draw(c, scope, outer) } else { None };
            let panel = self.panel(&c.panel, fixed, size).visible(shown);
            group = group.child(match body {
                Some(b) => panel.child(b),
                None => panel,
            });
        }
        if let Some(on_change) = scope.on_change.clone().filter(|_| !docks.is_empty()) {
            group = group.on_resize(move |state, window, cx| {
                let sizes = state.read(cx).sizes().clone();
                for (i, dock, was) in &docks {
                    let Some(w) = sizes.get(*i) else { continue };
                    if was.is_none_or(|was| (f32::from(was) - f32::from(*w)).abs() >= 0.5) {
                        on_change(Change::Width { dock: dock.clone(), width: f32::from(*w) }, window, cx);
                    }
                }
            });
        }
        group.into_any_element()
    }

    /// A split's panel with `p`'s range, `size` and, when `fixed`, kept at its size.
    fn panel(&self, p: &Panel, fixed: bool, size: Option<Pixels>) -> ResizablePanel {
        let px = |l: Option<AbsoluteLength>| l.map(|l| l.to_pixels(self.rem));
        let mut panel = resizable_panel();
        if fixed {
            panel = panel.flex_none();
        }
        if let Some(size) = size {
            panel = panel.size(size);
        }
        if p.min.is_some() || p.max.is_some() {
            panel = panel.size_range(px(p.min).unwrap_or(px_(20.))..px(p.max).unwrap_or(px_(100_000.)));
        }
        panel
    }

    /// True when `n` is a dock with no part shown that hides when empty.
    fn dock_hidden(&self, n: &Node, scope: &Scope, outer: &[&HashMap<&'static str, Value>]) -> bool {
        n.dock.as_ref().is_some_and(|d| !d.empty_keep && self.dock_tree(d, scope, outer).is_none())
    }

    /// The parts of dock `d` to draw: the arrangement's (the template's
    /// defaults without one), less those whose `when` doesn't hold.
    fn dock_tree(&self, d: &DockNode, scope: &Scope, outer: &[&HashMap<&'static str, Value>]) -> Option<Shown> {
        let templates = self.cx.try_global::<Templates>()?;
        let tree = match scope.arrangement.as_ref().and_then(|a| a.docks.get(&*d.name)) {
            Some(t) => t.as_ref()?,
            None => &d.info.defaults,
        };
        let shown = |part: &str| templates.part(part).is_some_and(|p| self.holds(&p.when, scope, outer));
        arrange::shown(tree, &shown)
    }

    /// What dock `d` shows of its tree: ids and resizes go by the stored tree's places.
    fn tree(&mut self, d: &DockNode, t: &Shown, scope: &mut Scope, outer: &[&HashMap<&'static str, Value>]) -> AnyElement {
        let cx = self.cx;
        let templates = cx.try_global::<Templates>();
        let (dir, path, sizes, items) = match t {
            Shown::Part(name) => {
                let body = templates.and_then(|ts| ts.part(name)).and_then(|p| self.draw(p, scope, outer));
                return body.unwrap_or_else(|| div().into_any_element());
            },
            Shown::Split { dir, path, sizes, items } => (dir, path, sizes, items),
        };
        let at: Vec<String> = path.iter().map(usize::to_string).collect();
        let id = SharedString::from(format!("{}:dock:{}:{}", scope.id, d.name, at.join(".")));
        let mut group = match dir {
            arrange::Dir::Row => h_resizable(id),
            arrange::Dir::Column => v_resizable(id),
        };
        let mut fixed_by_kept = vec![];
        for (ix, child) in items {
            let part = match child {
                Shown::Part(name) => templates.and_then(|ts| ts.part(name)).map(|p| (name, p)),
                Shown::Split { .. } => None,
            };
            let own = part.map(|(_, p)| p.panel.clone()).unwrap_or_default();
            let (fixed, size) = match sizes.get(*ix).copied().unwrap_or(Size::Auto) {
                Size::Px(v) => (true, Some(px(v))),
                Size::Auto | Size::Frac(_) => match part {
                    Some((name, p)) if d.info.defaults.has(name) => {
                        let fixed = p.panel.fixed.as_ref().is_some_and(|f| f.holds(&lookup(scope, outer)));
                        (fixed, p.panel.size.map(|l| l.to_pixels(self.rem)))
                    },
                    // A part from another dock takes the dock's size for newcomers, or flexes.
                    Some(_) => match d.part_size {
                        Some(l) => (true, Some(l.to_pixels(self.rem))),
                        None => (false, None),
                    },
                    None => (false, None),
                },
            };
            fixed_by_kept.push(fixed);
            let body = self.tree(d, child, scope, outer);
            group = group.child(self.panel(&own, fixed, size).child(body));
        }
        if let Some(on_change) = scope.on_change.clone() {
            let (dock, path, stored) = (d.name.clone(), path.clone(), sizes.clone());
            let kept: Vec<usize> = items.iter().map(|(i, _)| *i).collect();
            group = group.on_resize(move |state, window, cx| {
                let shown: Vec<Size> = state
                    .read(cx)
                    .sizes()
                    .iter()
                    .zip(&fixed_by_kept)
                    .map(|(s, f)| if *f { Size::Px(f32::from(*s)) } else { Size::Auto })
                    .collect();
                let sizes = arrange::merge_sizes(&stored, &kept, &shown);
                on_change(Change::Resize { dock: dock.clone(), path: path.clone(), sizes }, window, cx);
            });
        }
        group.into_any_element()
    }

    /// The node's look: its element and style, with the states that hold.
    fn look(&self, n: &Node, scope: &Scope, outer: &[&HashMap<&'static str, Value>]) -> style::El {
        let on: Vec<&HashMap<&'static str, Val>> =
            n.states.iter().filter(|(c, _)| c.holds(&lookup(scope, outer))).map(|(_, p)| p).collect();
        let inline = if on.is_empty() {
            n.style.clone()
        } else {
            let mut m = (*n.style).clone();
            for p in on {
                m.extend(p.iter().map(|(k, v)| (*k, v.clone())));
            }
            Arc::new(m)
        };
        style::el_with(n.element, inline, self.cx)
    }

    fn boxed(&mut self, n: &Node, look: &style::El, kids: Vec<AnyElement>, index: Option<usize>) -> AnyElement {
        let mut d = div().flex();
        d = match n.layout {
            Layout::Column => d.flex_col(),
            Layout::Row => d.flex_row(),
            Layout::Wrap => d.flex_row().flex_wrap(),
        };
        d = match n.align {
            Some(Align::Start) => d.items_start(),
            Some(Align::Center) => d.items_center(),
            Some(Align::End) => d.items_end(),
            Some(Align::Stretch) | None => d,
        };
        d = match n.justify {
            Some(Justify::Start) | None => d,
            Some(Justify::Center) => d.justify_center(),
            Some(Justify::End) => d.justify_end(),
            Some(Justify::Between) => d.justify_between(),
            Some(Justify::Around) => d.justify_around(),
        };
        if n.fill {
            d = d.size_full();
        }
        if let Some(g) = n.grow {
            let st = d.style();
            st.flex_grow = Some(g);
            st.flex_shrink = Some(1.);
            st.flex_basis = Some(relative(0.).into());
            d = d.min_w_0().min_h_0();
        }
        if let Some(s) = n.shrink {
            d.style().flex_shrink = Some(s);
        }
        d = match n.position {
            Some([t, r, b, l]) => {
                let st = d.style();
                st.position = Some(Position::Absolute);
                let len = |v: Option<AbsoluteLength>| v.map(|v| Length::Definite(v.into()));
                if let Some(v) = len(t) {
                    st.inset.top = Some(v);
                }
                if let Some(v) = len(r) {
                    st.inset.right = Some(v);
                }
                if let Some(v) = len(b) {
                    st.inset.bottom = Some(v);
                }
                if let Some(v) = len(l) {
                    st.inset.left = Some(v);
                }
                d
            },
            None => d.relative(),
        };
        if n.clip || n.truncate {
            d = d.overflow_hidden();
        }
        if n.truncate {
            d = d.min_w_0().whitespace_nowrap().text_ellipsis();
        }
        d = look.apply(d);
        let mut under = None;
        if n.border_under {
            // The box keeps its border's room; the layer under its children draws it.
            let st = d.style();
            let widths = st.border_widths.clone();
            let color = st.border_color.replace(hsla(0., 0., 0., 0.));
            let radii = st.corner_radii.clone();
            // Out to the box's edge: an absolute child starts inside the border.
            let mut b = div().absolute();
            let bs = b.style();
            let out = |w: Option<AbsoluteLength>| Some(Length::Definite((-w.map_or(px(0.), |w| w.to_pixels(self.rem))).into()));
            bs.inset.top = out(widths.top);
            bs.inset.right = out(widths.right);
            bs.inset.bottom = out(widths.bottom);
            bs.inset.left = out(widths.left);
            bs.border_widths = widths;
            bs.border_color = color;
            bs.corner_radii = radii;
            under = Some(b);
        }
        let d = d.children(look.backdrop()).children(under).children(kids);
        let e = match &n.frame {
            Some(f) => (self.frames)(f, index, d),
            None => d.into_any_element(),
        };
        if n.overlay { deferred(e).with_priority(n.z.max(0) as usize).into_any_element() } else { e }
    }
}

fn px_(v: f32) -> Pixels {
    px(v)
}

impl Node {
    /// A slot with nothing of its own: drawn as the code gives it.
    fn is_bare(&self) -> bool {
        self.element.is_empty()
            && self.style.is_empty()
            && self.states.is_empty()
            && !self.fill
            && self.grow.is_none()
            && self.shrink.is_none()
            && self.position.is_none()
            && !self.overlay
            && !self.truncate
            && !self.clip
            && self.frame.is_none()
            && self.align.is_none()
            && self.justify.is_none()
    }
}
