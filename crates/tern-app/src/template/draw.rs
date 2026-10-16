//! Drawing compiled templates with gpui.

use std::collections::HashMap;

use gpui_kit::component::{h_resizable, resizable_panel, v_resizable};
use gpui_kit::*;

use std::sync::Arc;

use super::{Align, Cond, Justify, Layout, Node, Part, Scope, Templates, Value, What};
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
        if !self.holds(&n.when, scope, outer) {
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
        if let Some(axis) = n.split {
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
        for c in &n.children {
            let shown = self.holds(&c.when, scope, outer);
            let fixed = c.panel.fixed.as_ref().is_some_and(|f| f.holds(&lookup(scope, outer)));
            let body = if shown { self.draw(c, scope, outer) } else { None };
            let px = |l: Option<AbsoluteLength>| l.map(|l| l.to_pixels(self.rem));
            let mut panel = resizable_panel().visible(shown);
            if fixed {
                panel = panel.flex_none();
            }
            if let Some(size) = px(c.panel.size) {
                panel = panel.size(size);
            }
            if c.panel.min.is_some() || c.panel.max.is_some() {
                panel = panel.size_range(px(c.panel.min).unwrap_or(px_(20.))..px(c.panel.max).unwrap_or(px_(100_000.)));
            }
            group = group.child(match body {
                Some(b) => panel.child(b),
                None => panel,
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
