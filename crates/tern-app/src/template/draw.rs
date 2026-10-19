//! Drawing compiled templates with gpui.

use std::collections::HashMap;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::resizable::ResizablePanel;
use gpui_kit::component::{Disableable, Icon, Selectable, Sizable, h_resizable, resizable_panel, v_resizable};
use gpui_kit::*;

use std::rc::Rc;
use std::sync::Arc;

use super::fill_parts as fill;
use super::menu::Entry;
use super::{Align, ButtonNode, ButtonSize, Cond, DockNode, Edge, Justify, Layout, Node, Panel, Scope, Templates, Value, Variant, What};
use crate::arrange::{self, Shown, Size};
use crate::dock::{ActiveDrag, Change, DraggedPart, FloatResize, Floats, LayoutLock, OnChange, PinnedParts};
use crate::style::{self, Val};

/// What a `frame` or a `button` names, for the code to attach behavior to.
// gpui's builders are large and move by value everywhere; these do too.
#[allow(clippy::large_enum_variant)]
pub enum Framed {
    Box(Div),
    Button(Button),
}

impl Framed {
    pub fn into_any_element(self) -> AnyElement {
        match self {
            Framed::Box(d) => d.into_any_element(),
            Framed::Button(b) => b.into_any_element(),
        }
    }
}

/// Attaches behavior to a box named by `frame` or a `button`: gets the
/// name, the list entry it is in (if any) and the box or button; gives back
/// what to draw.
pub type Frames<'a> = dyn FnMut(&str, Option<usize>, Framed) -> AnyElement + 'a;

/// `f` on view `v` as an event handler that holds it weakly, for frames,
/// which can't borrow a `Context` to make a listener.
pub fn on_event<V: 'static, E: ?Sized + 'static>(
    v: &WeakEntity<V>,
    f: impl Fn(&mut V, &E, &mut Window, &mut Context<V>) + 'static,
) -> impl Fn(&E, &mut Window, &mut App) + 'static {
    let v = v.clone();
    move |e, window, cx| {
        let _ = v.update(cx, |this, cx| f(this, e, window, cx));
    }
}

/// Draws template `name` with `scope`; `frames` attaches behavior to the
/// boxes that name a frame.
pub fn render(name: &str, scope: Scope, frames: &mut Frames, cx: &App) -> AnyElement {
    let Some(node) = cx.try_global::<Templates>().and_then(|t| t.nodes.get(name)).cloned() else {
        return div().into_any_element();
    };
    let mut scope = scope;
    let mut d = Drawer { frames, cx, rem: cx.theme_rem() };
    let root = d.node(&node, &mut scope, &[]).unwrap_or_else(|| div().into_any_element());
    if scope.arrangement.is_none() {
        return root;
    }
    // The template that holds the docks: its floating parts over it.
    let layer = d.floating(&mut scope);
    div().relative().size_full().child(root).child(layer).into_any_element()
}

/// Draws template `name` with `scope`, with no behavior attached.
pub fn render_plain(name: &str, scope: Scope, cx: &App) -> AnyElement {
    render(name, scope, &mut |_, _, f| f.into_any_element(), cx)
}

/// What clicking a menu item does.
pub type MenuHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// Gives what a menu's `button` item does: by its name and the list entry
/// it is in (if any). None leaves it doing nothing.
pub type MenuItems = Rc<dyn Fn(&str, Option<usize>) -> Option<MenuHandler>>;

/// Fills `menu` with menu template `name`'s entries for `scope`; `items`
/// attaches what its buttons do.
pub fn menu(name: &str, scope: Scope, menu: PopupMenu, items: MenuItems, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
    let entries = cx.try_global::<Templates>().map(|t| t.menu(name, scope)).unwrap_or_default();
    fill_menu(&entries, menu, &items, window, cx)
}

fn fill_menu(entries: &[Entry], mut menu: PopupMenu, items: &MenuItems, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
    for e in entries {
        menu = match e {
            Entry::Separator => menu.separator(),
            Entry::Label(text) => menu.item(PopupMenuItem::label(text.clone())),
            Entry::Item(it) => {
                let mut item = match &it.content {
                    Some((node, content)) => {
                        let (node, content) = (node.clone(), content.clone());
                        PopupMenuItem::element(move |_, cx| content_of(&node, &content, cx))
                    },
                    None => PopupMenuItem::new(it.label.clone()),
                };
                item = item.checked(it.checked).disabled(it.disabled);
                if let Some(icon) = &it.icon {
                    item = item.icon(Icon::default().path(super::icon_path(icon)));
                }
                if let Some(action) = it.action.as_ref().and_then(|a| cx.build_action(a, None).ok()) {
                    item = item.action(action);
                }
                if let Some(h) = items(&it.frame, it.index) {
                    item = item.on_click(move |e, window, cx| h(e, window, cx));
                }
                menu.item(item)
            },
            Entry::Submenu { label, entries } => {
                let (entries, items) = (entries.clone(), items.clone());
                menu.submenu(label.clone(), window, cx, move |m, window, cx| fill_menu(&entries, m, &items, window, cx))
            },
        };
    }
    menu
}

/// A menu item's own content: its node's children, in a row by default.
fn content_of(n: &Node, content: &super::menu::Content, cx: &App) -> AnyElement {
    let mut scope = Scope { vars: content.vars.clone(), index: content.index, ..Default::default() };
    let mut frames = |_: &str, _: Option<usize>, f: Framed| f.into_any_element();
    let mut d = Drawer { frames: &mut frames, cx, rem: cx.theme_rem() };
    let (look, hovers) = d.look(n, &scope, &[]);
    let kids: Vec<AnyElement> = n.children.iter().filter_map(|c| d.node(c, &mut scope, &[])).collect();
    let place = place(n, &lookup(&scope, &[]));
    d.boxed(n, &look, hovers, kids, place, None, None)
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
pub(super) fn lookup<'s>(scope: &'s Scope, outer: &'s [&'s HashMap<&'static str, Value>]) -> impl Fn(&str) -> Option<Value> + 's {
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
        if self.dock_hidden(n, scope, outer) {
            // Out of a split, an empty dock takes a dragged part on a strip of its own.
            let d = n.dock.as_ref()?;
            let size = style::el("drop_zone", self.cx).px("size");
            return self.end_zone(d, scope).map(|z| z.flex_none().w(size).into_any_element());
        }
        self.draw(n, scope, outer)
    }

    /// `n`, its `when` aside (a split's panel hides rather than goes).
    fn draw(&mut self, n: &Node, scope: &mut Scope, outer: &[&HashMap<&'static str, Value>]) -> Option<AnyElement> {
        let (look, hovers) = self.look(n, scope, outer);
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
                let text = fill(parts, &lookup(scope, outer));
                kids.push(look.label(text).into_any_element());
            },
            What::Icon(parts) => {
                let name = fill(parts, &lookup(scope, outer));
                if name.is_empty() {
                    return None;
                }
                let icon = look.apply(Icon::default().path(super::icon_path(&name)));
                if n.is_bare_icon() {
                    return Some(icon.into_any_element());
                }
                kids.push(icon.into_any_element());
                // The box takes the place; the icon keeps the look.
                let place = place(n, &lookup(scope, outer));
                return Some(self.boxed(n, &style::el_with("", Default::default(), self.cx), hovers, kids, place, scope.index, None));
            },
            What::Button(b) => return Some(self.button(n, b, &look, scope, outer)),
            What::Each(list, item) => {
                let items = scope.lists.remove(list.as_ref()).unwrap_or_default();
                let mut chain = outer.to_vec();
                chain.push(&scope.vars);
                for mut entry in items {
                    kids.extend(self.node(item, &mut entry, &chain));
                }
            },
            What::Box => {},
            // Only a menu draws it.
            What::Separator => return None,
        }
        if let Some(d) = &n.dock {
            match self.dock_tree(d, scope, outer) {
                Some(t) => kids.push(self.tree(d, &t, scope, outer)),
                // An empty dock that stays: all of it takes a dragged part.
                None => kids.extend(self.end_zone(d, scope).map(|z| z.absolute().inset_0().into_any_element())),
            }
        } else if let Some(axis) = n.split {
            kids.push(self.split(n, axis, scope, outer));
            // The split's empty docks take a dragged part on a strip along their edge.
            let size = style::el("drop_zone", self.cx).px("size");
            for (i, c) in n.children.iter().enumerate() {
                let Some(d) = c.dock.as_ref().filter(|_| self.holds(&c.when, scope, outer) && self.dock_hidden(c, scope, outer)) else {
                    continue;
                };
                let Some(z) = self.end_zone(d, scope) else { continue };
                let start = i * 2 < n.children.len();
                let z = match (axis, start) {
                    (Axis::Horizontal, true) => z.absolute().top_0().bottom_0().left_0().w(size),
                    (Axis::Horizontal, false) => z.absolute().top_0().bottom_0().right_0().w(size),
                    (Axis::Vertical, true) => z.absolute().left_0().right_0().top_0().h(size),
                    (Axis::Vertical, false) => z.absolute().left_0().right_0().bottom_0().h(size),
                };
                kids.push(z.into_any_element());
            }
        } else {
            for c in &n.children {
                if let Some(e) = self.node(c, scope, outer) {
                    kids.push(e);
                }
            }
        }
        let drag = if n.handle { self.dragged_part(n, scope, outer) } else { None };
        let place = place(n, &lookup(scope, outer));
        Some(self.boxed(n, &look, hovers, kids, place, scope.index, drag))
    }

    /// Button `b`, drawn as node `n` says, with the code's behavior.
    fn button(&mut self, n: &Node, b: &ButtonNode, look: &style::El, scope: &Scope, outer: &[&HashMap<&'static str, Value>]) -> AnyElement {
        let get = lookup(scope, outer);
        let id = ElementId::Name(format!("{}:{}:{}", scope.id, n.path, scope.index.unwrap_or(0)).into());
        let mut button = look.apply(Button::new(id));
        button = match b.variant {
            Variant::Ghost => button.ghost(),
            Variant::Primary => button.primary(),
            // Secondary is the default.
            Variant::Secondary => button,
            Variant::Danger => button.danger(),
            Variant::Outline => button.outline(),
            Variant::Text => button.text(),
        };
        button = match b.size {
            ButtonSize::XSmall => button.xsmall(),
            ButtonSize::Small => button.small(),
            // Medium is the default.
            ButtonSize::Medium => button,
            ButtonSize::Large => button.large(),
        };
        if let Some(icon) = &b.icon {
            let name = fill(icon, &get);
            if !name.is_empty() {
                button = button.icon(Icon::default().path(super::icon_path(&name)));
            }
        }
        if let Some(label) = &b.label {
            button = button.label(look.label(fill(label, &get)));
        }
        if let Some(tip) = &b.tooltip {
            button = button.tooltip(fill(tip, &get));
        }
        if b.selected.as_ref().is_some_and(|c| c.holds(&get)) {
            button = button.selected(true);
        }
        if b.disabled.as_ref().is_some_and(|c| c.holds(&get)) {
            button = button.disabled(true);
        }
        if b.loading.as_ref().is_some_and(|c| c.holds(&get)) {
            button = button.loading(true);
        }
        if let Some(action) = b.action.as_ref().and_then(|a| self.cx.build_action(a, None).ok()) {
            button = button.on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx));
        }
        (self.frames)(&b.frame, scope.index, Framed::Button(button))
    }

    /// What dragging handle `n` drags: the part its scope names, when it may
    /// be dragged (the part's own `drag` was read where its dock draws it).
    fn dragged_part(&self, n: &Node, scope: &Scope, outer: &[&HashMap<&'static str, Value>]) -> Option<DraggedPart> {
        if self.locked() || !self.holds(&n.drag, scope, outer) {
            return None;
        }
        let get = lookup(scope, outer);
        let part = get("part")?.text();
        let templates = self.cx.try_global::<Templates>()?;
        let (kind, _) = templates.parts.get(part.as_ref())?;
        if self.cx.try_global::<PinnedParts>().is_some_and(|p| p.0.contains(part.as_ref())) {
            return None;
        }
        let title = get("title").map(|t| t.text()).unwrap_or_else(|| part.clone());
        let floating = self.cx.try_global::<Floats>().is_some_and(|f| f.floating.contains(part.as_ref()));
        Some(DraggedPart { part, kind: kind.clone().into(), title, floating })
    }

    /// Whether moving and resizing panels is turned off.
    fn locked(&self) -> bool {
        self.cx.try_global::<LayoutLock>().is_some_and(|l| l.0)
    }

    /// The part being dragged, when dock `d` takes it, and where a drop goes.
    fn drop_for(&self, d: &DockNode, scope: &Scope) -> Option<(DraggedPart, OnChange)> {
        let p = self.cx.try_global::<ActiveDrag>()?.0.clone()?;
        let on_change = scope.on_change.clone()?;
        d.info.takes(&p.part, &p.kind).then_some((p, on_change))
    }

    /// A box that drops the dragged part at `at` in dock `d`.
    fn zone(&self, d: &DockNode, at: arrange::Drop, on_change: OnChange) -> Div {
        let hover = style::el("drop_zone.hover", self.cx);
        let dock = d.name.clone();
        style::el("drop_zone", self.cx).apply(div()).drag_over::<DraggedPart>(move |s, _, _, _| hover.apply(s)).on_drop(
            move |p: &DraggedPart, window, cx| {
                let change = Change::Move { part: p.part.clone(), kind: p.kind.clone(), dock: dock.clone(), at: at.clone() };
                on_change(change, window, cx);
            },
        )
    }

    /// Where a dragged part goes to the end of dock `d`, while one it takes is dragged.
    fn end_zone(&self, d: &DockNode, scope: &Scope) -> Option<Div> {
        let (_, on_change) = self.drop_for(d, scope)?;
        Some(self.zone(d, arrange::Drop::End, on_change))
    }

    /// What dragging part `part` by a stack's tab drags, when it may be dragged.
    fn part_drag(&self, part: &str) -> Option<DraggedPart> {
        if self.locked() || self.cx.try_global::<PinnedParts>().is_some_and(|p| p.0.contains(part)) {
            return None;
        }
        let (kind, _) = self.cx.try_global::<Templates>()?.parts.get(part)?;
        Some(DraggedPart { part: part.to_owned().into(), kind: kind.clone().into(), title: crate::dock::part_title(part), floating: false })
    }

    /// Part `name` as its node draws it.
    fn part_body(&mut self, name: &str, scope: &mut Scope, outer: &[&HashMap<&'static str, Value>]) -> AnyElement {
        let cx = self.cx;
        let body = cx.try_global::<Templates>().and_then(|ts| ts.part(name)).and_then(|p| self.draw(p, scope, outer));
        body.unwrap_or_else(|| div().into_any_element())
    }

    /// The stack at `path` in dock `d`: a row of tabs (`dock_tabs`) over the
    /// part it shows. A tab shows its part, and drags it.
    #[allow(clippy::too_many_arguments)]
    fn stack(
        &mut self,
        d: &DockNode,
        path: &[usize],
        parts: &[String],
        active: usize,
        scope: &mut Scope,
        outer: &[&HashMap<&'static str, Value>],
    ) -> AnyElement {
        let active = active.min(parts.len().saturating_sub(1));
        let body = self.part_body(&parts[active], scope, outer);
        let tabs = parts
            .iter()
            .enumerate()
            .map(|(i, p)| Scope::default().var("title", crate::dock::part_title(p)).var("part", p.clone()).var("on", i == active))
            .collect();
        let drags: Vec<Option<DraggedPart>> = parts.iter().map(|p| self.part_drag(p)).collect();
        let (id, on_change, dock, at) = (scope.id.clone(), scope.on_change.clone(), d.name.clone(), path.to_vec());
        let names = parts.to_vec();
        let mut frames = |name: &str, ix: Option<usize>, f: Framed| match (name, f, ix.filter(|i| *i < names.len())) {
            ("tab", Framed::Box(b), Some(i)) => {
                let mut b = b.id(ElementId::Name(format!("{id}:dock_tab:{}", names[i]).into()));
                if let Some(on_change) = on_change.clone() {
                    let (dock, path, part) = (dock.clone(), at.clone(), SharedString::from(names[i].clone()));
                    b = b.on_click(move |_, window, cx| {
                        on_change(Change::Activate { dock: dock.clone(), path: path.clone(), part: part.clone() }, window, cx);
                    });
                }
                if let Some(p) = drags[i].clone() {
                    b = b.on_drag(p, crate::dock::start_drag);
                }
                b.into_any_element()
            },
            (_, f, _) => f.into_any_element(),
        };
        let at: Vec<String> = path.iter().map(usize::to_string).collect();
        let tabs = Scope::new(format!("{}:stack:{}:{}", scope.id, d.name, at.join("."))).list("tabs", tabs).slot("content", body);
        let e = render("dock_tabs", tabs, &mut frames, self.cx);
        self.leaf(d, parts, active, e, scope)
    }

    /// The area showing `parts[active]` (one part, or a stack) in dock `d`,
    /// with its drop zones over it while a part the dock takes is dragged:
    /// with `edges` or `stack`, the zone under the pointer; else the
    /// `before`/`after` halves.
    fn leaf(&self, d: &DockNode, parts: &[String], active: usize, body: AnyElement, scope: &Scope) -> AnyElement {
        let Some((p, on_change)) = self.drop_for(d, scope) else { return body };
        // Next to or onto the part shown, or another of its stack when that one is dragged.
        let shown = parts.get(active).or(parts.first());
        let Some(name) = shown.filter(|n| **n != *p.part).or_else(|| parts.iter().find(|n| **n != *p.part)).cloned() else {
            // Let go on its own place: nothing happens (it doesn't float).
            let stay = div().absolute().inset_0().on_drop(|_: &DraggedPart, _, _| {});
            return div().relative().size_full().flex().flex_col().child(body).child(stay).into_any_element();
        };
        let (edges, stack) = (d.zones.contains(&"edges"), d.zones.contains(&"stack"));
        if edges || stack {
            let area = format!("{}:{}:{name}", scope.id, d.name);
            let dock = d.name.clone();
            let overlay = crate::dock::zone_overlay(&area, self.cx);
            let overlay = crate::dock::on_zones(overlay, area.into(), edges, stack, move |p: &DraggedPart, zone, window, cx| {
                let at = match zone {
                    arrange::Zone::Center => arrange::Drop::Stack(name.clone()),
                    arrange::Zone::Edge(e) => arrange::Drop::Edge(name.clone(), e),
                };
                on_change(Change::Move { part: p.part.clone(), kind: p.kind.clone(), dock: dock.clone(), at }, window, cx);
            });
            return div().relative().size_full().flex().flex_col().child(body).child(overlay).into_any_element();
        }
        let mut zones = vec![];
        for zone in &d.zones {
            let (at, start) = match *zone {
                "before" => (arrange::Drop::Before(name.clone()), true),
                _ => (arrange::Drop::After(name.clone()), false),
            };
            let z = self.zone(d, at, on_change.clone()).absolute();
            zones.push(match (d.dir, start) {
                (arrange::Dir::Row, true) => z.top_0().bottom_0().left_0().w(relative(0.5)),
                (arrange::Dir::Row, false) => z.top_0().bottom_0().right_0().w(relative(0.5)),
                (arrange::Dir::Column, true) => z.left_0().right_0().top_0().h(relative(0.5)),
                (arrange::Dir::Column, false) => z.left_0().right_0().bottom_0().h(relative(0.5)),
            });
        }
        div().relative().size_full().flex().flex_col().child(body).children(zones).into_any_element()
    }

    /// The parts floating over the docks, each in a `floating` box where the
    /// arrangement keeps it (following the pointer while it moves or
    /// resizes), with grips on its sides and corners to resize it.
    fn floating(&mut self, scope: &mut Scope) -> AnyElement {
        let Some(a) = scope.arrangement.clone() else { return div().into_any_element() };
        let cx = self.cx;
        let look = style::el("floating", cx);
        let grip = look.px("grip");
        let resizing = cx.try_global::<crate::dock::Resizing>().is_some_and(|r| r.0.is_some());
        let mut boxes = vec![];
        for (name, f) in &a.floating {
            let Some(node) = cx.try_global::<Templates>().and_then(|t| t.part(name)) else { continue };
            if !self.holds(&node.when, scope, &[]) {
                continue;
            }
            let body = self.part_body(name, scope, &[]);
            let live = crate::dock::live_rect(name, f, cx);
            let mut b = look.apply(div()).absolute().flex().flex_col().overflow_hidden();
            b = match live {
                Some(r) => b.left(px(r.x)).top(px(r.y)).w(px(r.w)).h(px(r.h)),
                None => b.left(relative(f.x)).top(relative(f.y)).w(px(f.w)).h(px(f.h)).max_w_full().max_h_full(),
            };
            // It keeps clicks from what is under it, but not while it moves:
            // the zones under it take it then.
            if live.is_none() || resizing {
                b = b.occlude();
            }
            b = b.child(div().flex().flex_col().size_full().min_h_0().child(body));
            if !self.locked() {
                b = b.children(grips(name, grip, &scope.id));
            }
            boxes.push(b);
        }
        div().absolute().inset_0().overflow_hidden().children(boxes).into_any_element()
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
            let locked = c.dock.is_some() && self.locked();
            let panel = self.panel(&c.panel, fixed, size, locked).visible(shown);
            group = group.child(match body {
                Some(b) => panel.child(b),
                None => panel,
            });
        }
        if let Some(on_change) = scope.on_change.clone().filter(|_| !docks.is_empty() && !self.locked()) {
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

    /// A split's panel with `p`'s range, `size` and, when `fixed`, kept at
    /// its size; `locked` keeps it at `size` whatever the user drags.
    fn panel(&self, p: &Panel, fixed: bool, size: Option<Pixels>, locked: bool) -> ResizablePanel {
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
        if let Some(size) = size.filter(|_| locked) {
            panel = panel.size_range(size..size);
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
                let body = self.part_body(name, scope, outer);
                return self.leaf(d, std::slice::from_ref(name), 0, body, scope);
            },
            Shown::Stack { path, parts, active } => return self.stack(d, path, parts, *active, scope, outer),
            Shown::Split { dir, path, sizes, items } => (dir, path, sizes, items),
        };
        let mut cells = vec![];
        for (ix, child) in items {
            // A stack sizes as the part it shows.
            let part = match child {
                Shown::Part(name) => Some(name),
                Shown::Stack { parts, active, .. } => parts.get(*active),
                Shown::Split { .. } => None,
            };
            let part = part.and_then(|name| templates.and_then(|ts| ts.part(name)).map(|p| (name, p)));
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
            let body = self.tree(d, child, scope, outer);
            cells.push((own, fixed, size, body));
        }
        if self.locked() {
            return self.locked_tree(*dir, cells);
        }
        let at: Vec<String> = path.iter().map(usize::to_string).collect();
        let id = SharedString::from(format!("{}:dock:{}:{}", scope.id, d.name, at.join(".")));
        let mut group = match dir {
            arrange::Dir::Row => h_resizable(id),
            arrange::Dir::Column => v_resizable(id),
        };
        let fixed_by_kept: Vec<bool> = cells.iter().map(|c| c.1).collect();
        for (own, fixed, size, body) in cells {
            group = group.child(self.panel(&own, fixed, size, false).child(body));
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

    /// A dock's split while the layout is locked: the same parts in a plain
    /// row or column, with nothing to drag. Parts with a size keep it.
    fn locked_tree(&self, dir: arrange::Dir, cells: Vec<(Panel, bool, Option<Pixels>, AnyElement)>) -> AnyElement {
        use gpui_kit::component::ActiveTheme as _;
        let line = self.cx.theme().border;
        let row = dir == arrange::Dir::Row;
        let mut group = div().flex().size_full();
        group = if row { group.flex_row() } else { group.flex_col() };
        for (i, (_, fixed, size, body)) in cells.into_iter().enumerate() {
            let mut cell = div().flex().relative().size_full().min_w_0().min_h_0();
            cell = match size.filter(|_| fixed) {
                Some(size) => cell.flex_none().flex_basis(size),
                None => cell.flex_1(),
            };
            if i > 0 {
                cell = if row { cell.border_l_1() } else { cell.border_t_1() }.border_color(line);
            }
            group = group.child(cell.child(body));
        }
        group.into_any_element()
    }

    /// The node's look: its element and style, with the states that hold;
    /// and the looks it takes under the pointer, by group (`""`: its own).
    fn look(&self, n: &Node, scope: &Scope, outer: &[&HashMap<&'static str, Value>]) -> (style::El, Vec<(SharedString, style::El)>) {
        let get = lookup(scope, outer);
        let holds = |c: &Option<Cond>| c.as_ref().is_none_or(|c| c.holds(&get));
        let filled = |holes: &super::Holes, m: &mut HashMap<&'static str, Val>| {
            for (prop, kind, var) in holes {
                if let Some(v) = get(var).and_then(|v| v.val(*kind)) {
                    m.insert(*prop, v);
                }
            }
        };
        let on: Vec<&super::State> = n.states.iter().filter(|s| s.hover.is_none() && holds(&s.when)).collect();
        let inline = if on.is_empty() && n.holes.is_empty() {
            n.style.clone()
        } else {
            let mut m = (*n.style).clone();
            filled(&n.holes, &mut m);
            for s in on {
                m.extend(s.props.iter().map(|(k, v)| (*k, v.clone())));
                filled(&s.holes, &mut m);
            }
            Arc::new(m)
        };
        let hovers = n
            .states
            .iter()
            .filter_map(|s| {
                let group = s.hover.clone()?;
                if !holds(&s.when) {
                    return None;
                }
                let mut m = s.props.clone();
                filled(&s.holes, &mut m);
                Some((group, style::el_with("", Arc::new(m), self.cx)))
            })
            .collect();
        (style::el_with(n.element, inline, self.cx), hovers)
    }

    #[allow(clippy::too_many_arguments)]
    fn boxed(
        &mut self,
        n: &Node,
        look: &style::El,
        hovers: Vec<(SharedString, style::El)>,
        kids: Vec<AnyElement>,
        place: Option<[Option<AbsoluteLength>; 4]>,
        index: Option<usize>,
        drag: Option<DraggedPart>,
    ) -> AnyElement {
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
        d = match place {
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
        if n.wrap {
            d = d.whitespace_normal();
        }
        if n.nowrap {
            // At least as wide as its text, even when it grows from nothing.
            d = d.whitespace_nowrap();
            d.style().min_size.width = Some(Length::Auto);
        }
        d = look.apply(d);
        if let Some(g) = &n.group {
            d = d.group(g.clone());
        }
        for (group, h) in hovers {
            d = if group.is_empty() { d.hover(move |s| h.apply(s)) } else { d.group_hover(group, move |s| h.apply(s)) };
        }
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
        let e = match (drag, &n.frame) {
            // gpui doesn't say what is dragged: the drag's start keeps it for the docks.
            (Some(p), _) => {
                let d = d.id(ElementId::Name(format!("handle:{}", p.part).into())).on_drag(p.clone(), crate::dock::start_drag);
                let grounded = self.cx.try_global::<Floats>().is_some_and(|f| f.grounded.contains(p.part.as_ref()));
                if p.kind == "pane" || (grounded && !p.floating) {
                    d.into_any_element()
                } else {
                    d.context_menu(move |menu, window, cx| part_menu(&p, menu, window, cx)).into_any_element()
                }
            },
            (None, Some(f)) => (self.frames)(f, index, Framed::Box(d)),
            (None, None) => d.into_any_element(),
        };
        if n.overlay { deferred(e).with_priority(n.z.max(0) as usize).into_any_element() } else { e }
    }
}

/// Grips on the sides and corners of floating part `part`, `g` wide, that resize it.
fn grips(part: &str, g: Pixels, id: &str) -> Vec<Stateful<Div>> {
    let side = |left, right, top, bottom| arrange::Sides { left, right, top, bottom };
    let all = [
        (side(true, false, false, false), CursorStyle::ResizeLeftRight),
        (side(false, true, false, false), CursorStyle::ResizeLeftRight),
        (side(false, false, true, false), CursorStyle::ResizeUpDown),
        (side(false, false, false, true), CursorStyle::ResizeUpDown),
        (side(true, false, true, false), CursorStyle::ResizeUpLeftDownRight),
        (side(false, true, false, true), CursorStyle::ResizeUpLeftDownRight),
        (side(false, true, true, false), CursorStyle::ResizeUpRightDownLeft),
        (side(true, false, false, true), CursorStyle::ResizeUpRightDownLeft),
    ];
    all.into_iter()
        .enumerate()
        .map(|(i, (sides, cursor))| {
            let mut d = div().absolute().cursor(cursor);
            d = if sides.left {
                d.left_0().w(g)
            } else if sides.right {
                d.right_0().w(g)
            } else {
                d.left(g).right(g)
            };
            d = if sides.top {
                d.top_0().h(g)
            } else if sides.bottom {
                d.bottom_0().h(g)
            } else {
                d.top(g).bottom(g)
            };
            let r = FloatResize { part: part.to_owned().into(), sides };
            d.id(ElementId::Name(format!("{id}:float_grip:{part}:{i}").into())).on_drag(r, crate::dock::start_resize)
        })
        .collect()
}

/// A part's handle's right-click menu (`menu.part`): Float, or Dock when it floats.
fn part_menu(p: &DraggedPart, popup: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
    let part = p.part.clone();
    let items: MenuItems = Rc::new(move |name, _| {
        let action: Box<dyn Action> = match name {
            "float" => Box::new(crate::dock::FloatPart(part.clone())),
            "dock" => Box::new(crate::dock::DockPart(part.clone())),
            _ => return None,
        };
        let h: MenuHandler = Rc::new(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx));
        Some(h)
    });
    menu(
        "menu.part",
        Scope::new(format!("part-menu:{}", p.part)).var("floating", p.floating).var("part", p.part.clone()),
        popup,
        items,
        window,
        cx,
    )
}

/// Where positioned node `n` sits, its `"{name}"` edges read from `get`.
pub(super) fn place(n: &Node, get: &dyn Fn(&str) -> Option<Value>) -> Option<[Option<AbsoluteLength>; 4]> {
    let edges = n.position.as_ref()?;
    Some(edges.clone().map(|e| match e? {
        Edge::Len(l) => Some(l),
        Edge::Var(v) => get(&v).map(|v| px(v.num() as f32).into()),
    }))
}

fn px_(v: f32) -> Pixels {
    px(v)
}

impl Node {
    /// An icon with nothing a box would do: drawn as the icon alone.
    fn is_bare_icon(&self) -> bool {
        !self.fill
            && self.grow.is_none()
            && self.shrink.is_none()
            && self.position.is_none()
            && !self.overlay
            && !self.clip
            && self.frame.is_none()
            && self.group.is_none()
            && self.states.iter().all(|s| s.hover.is_none())
    }

    /// A slot with nothing of its own: drawn as the code gives it.
    pub(super) fn is_bare(&self) -> bool {
        self.element.is_empty()
            && self.style.is_empty()
            && self.states.is_empty()
            && self.holes.is_empty()
            && self.group.is_none()
            && !self.fill
            && self.grow.is_none()
            && self.shrink.is_none()
            && self.position.is_none()
            && !self.overlay
            && !self.truncate
            && !self.nowrap
            && !self.wrap
            && !self.clip
            && self.frame.is_none()
            && self.align.is_none()
            && self.justify.is_none()
    }
}
