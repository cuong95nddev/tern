//! Drawing compiled templates with gpui.

use std::collections::HashMap;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::resizable::ResizablePanel;
use gpui_kit::component::switch::Switch;
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
    Switch(Switch),
}

impl Framed {
    pub fn into_any_element(self) -> AnyElement {
        match self {
            Framed::Box(d) => d.into_any_element(),
            Framed::Button(b) => b.into_any_element(),
            Framed::Switch(s) => s.into_any_element(),
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

/// Draws part `name` by itself, as its node draws it in a dock, with
/// `scope`'s values and slots (a part in a window of its own).
pub fn render_part(name: &str, scope: Scope, cx: &App) -> AnyElement {
    let mut scope = scope;
    let mut frames = |_: &str, _: Option<usize>, f: Framed| f.into_any_element();
    let mut d = Drawer { frames: &mut frames, cx, rem: cx.theme_rem() };
    d.part_body(name, &mut scope, &[])
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
            What::Ring(parts) => {
                let full = fill(parts, &lookup(scope, outer)).trim().parse::<f32>().ok();
                kids.push(ring(full, &look).into_any_element());
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
        let holds = |c: &Option<Cond>| c.as_ref().is_some_and(|c| c.holds(&get));
        if b.switch {
            let mut switch = look.apply(Switch::new(id)).checked(holds(&b.selected)).disabled(holds(&b.disabled));
            switch = match b.size {
                ButtonSize::XSmall | ButtonSize::Small => switch.small(),
                ButtonSize::Medium => switch,
                ButtonSize::Large => switch.large(),
            };
            if let Some(tip) = &b.tooltip {
                switch = switch.tooltip(fill(tip, &get));
            }
            return (self.frames)(&b.frame, scope.index, Framed::Switch(switch));
        }
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
        if self.no_drag() || !self.holds(&n.drag, scope, outer) {
            return None;
        }
        let get = lookup(scope, outer);
        let part = get("part")?.text();
        let templates = self.cx.try_global::<Templates>()?;
        let kind = templates.kind_of(part.as_ref())?.to_owned();
        if self.cx.try_global::<PinnedParts>().is_some_and(|p| p.0.contains(part.as_ref())) {
            return None;
        }
        let title = get("title").map(|t| t.text()).unwrap_or_else(|| part.clone());
        let floats = self.cx.try_global::<Floats>();
        let floating = floats.is_some_and(|f| f.floating.contains(part.as_ref()));
        let popped = floats.is_some_and(|f| f.popped.contains(part.as_ref()));
        Some(DraggedPart { part, kind: kind.into(), title, floating, popped })
    }

    /// `part`, or the first panel in it when it is a panel group.
    fn first_panel(&self, part: &str, scope: &Scope) -> Option<String> {
        let mut name = part.to_owned();
        for _ in 0..32 {
            if !arrange::is_group(&name) {
                return Some(name);
            }
            let tree = scope.arrangement.as_ref().and_then(|a| a.docks.get(&name)).cloned().flatten()?;
            name = tree.parts().first()?.to_string();
        }
        None
    }

    /// Whether panels can't be dragged now: the layout is locked, or isn't
    /// being edited. Panes and their tabs aren't panels.
    fn no_drag(&self) -> bool {
        self.locked() || !crate::dock::editing(self.cx)
    }

    /// True when `part` shows: a panel group while it is empty or one of
    /// its panels shows, any other part while its `when` holds.
    fn shown(&self, part: &str, scope: &Scope, outer: &[&HashMap<&'static str, Value>], depth: usize) -> bool {
        let Some(templates) = self.cx.try_global::<Templates>() else { return false };
        if !arrange::is_group(part) {
            return templates.part(part).is_some_and(|p| self.holds(&p.when, scope, outer));
        }
        let tree = scope.arrangement.as_ref().and_then(|a| a.docks.get(part)).cloned().flatten();
        match tree {
            None => true,
            Some(t) => depth < 32 && t.parts().iter().any(|p| self.shown(p, scope, outer, depth + 1)),
        }
    }

    /// Whether moving and resizing panels is turned off.
    fn locked(&self) -> bool {
        self.cx.try_global::<LayoutLock>().is_some_and(|l| l.0)
    }

    /// The part being dragged, when dock `d` takes it, and where a drop goes.
    fn drop_for(&self, d: &DockNode, scope: &Scope) -> Option<(DraggedPart, OnChange)> {
        let p = self.cx.try_global::<ActiveDrag>()?.0.clone()?;
        let on_change = scope.on_change.clone()?;
        let own = scope.arrangement.as_ref().is_some_and(|a| a.groups.contains_key(p.part.as_ref()) && a.group_holds(&p.part, &d.name));
        (d.info.takes(&p.part, &p.kind) && !own).then_some((p, on_change))
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
        if self.no_drag() || self.cx.try_global::<PinnedParts>().is_some_and(|p| p.0.contains(part)) {
            return None;
        }
        let kind = self.cx.try_global::<Templates>()?.kind_of(part)?.to_owned();
        Some(DraggedPart {
            part: part.to_owned().into(),
            kind: kind.into(),
            title: crate::dock::part_title(part),
            floating: false,
            popped: false,
        })
    }

    /// Part `name` as its node draws it.
    fn part_body(&mut self, name: &str, scope: &mut Scope, outer: &[&HashMap<&'static str, Value>]) -> AnyElement {
        if arrange::is_group(name) {
            return self.group(name, scope, outer);
        }
        let cx = self.cx;
        let body = cx.try_global::<Templates>().and_then(|ts| ts.part(name)).and_then(|p| self.draw(p, scope, outer));
        let body = body.unwrap_or_else(|| div().into_any_element());
        // The panes hold docks of their own: their tabs and panes drag in them.
        if self.no_drag() || cx.try_global::<Templates>().is_none_or(|ts| !ts.docks(name).is_empty()) {
            return body;
        }
        // While the layout is edited, all of it drags it (a panel without a
        // title bar has nothing else to drag it by), and clicks don't reach it.
        let title = crate::dock::part_title(name);
        let edit = Scope::new(format!("{}:edit:{name}", scope.id)).var("part", name.to_owned()).var("title", title);
        let edit = div().absolute().inset_0().occlude().child(render("panel.edit", edit, &mut |_, _, f| f.into_any_element(), cx));
        div().relative().size_full().flex().flex_col().child(body).child(edit).into_any_element()
    }

    /// Panel group `name` (template `panel_group`): its title bar, with a
    /// button per layout and one that closes it, over the panels in it (or
    /// `panel_group.empty` while it has none). It is a dock to them.
    fn group(&mut self, name: &str, scope: &mut Scope, outer: &[&HashMap<&'static str, Value>]) -> AnyElement {
        let mode = scope.arrangement.as_ref().and_then(|a| a.groups.get(name)).copied().unwrap_or_default();
        let d = DockNode::group(name, mode);
        let tree = self.dock_tree(&d, scope, outer);
        let empty = tree.is_none();
        let content = match tree {
            Some(t) => self.tree(&d, &t, scope, outer),
            None => {
                let hint = render("panel_group.empty", Scope::new(format!("{name}:empty")), &mut |_, _, f| f.into_any_element(), self.cx);
                // All of it takes a dragged panel.
                let zone = self.end_zone(&d, scope).map(|z| z.absolute().inset_0());
                div().relative().size_full().flex().flex_col().child(hint).children(zone).into_any_element()
            },
        };
        let on_change = scope.on_change.clone();
        let group: SharedString = name.to_owned().into();
        let mut frames = |button: &str, _: Option<usize>, f: Framed| {
            let Framed::Button(b) = f else { return f.into_any_element() };
            let close = button == "close";
            let mode = arrange::GroupMode::parse(button);
            let Some(on_change) = on_change.clone().filter(|_| close || mode.is_some()) else { return b.into_any_element() };
            let group = group.clone();
            b.on_click(move |_, window, cx| {
                let change = match mode {
                    Some(mode) => Change::GroupMode { group: group.clone(), mode },
                    None => Change::CloseGroup { group: group.clone() },
                };
                on_change(change, window, cx);
            })
            .into_any_element()
        };
        let buttons = Scope::new(format!("{name}:buttons"))
            .var("grid", mode == arrange::GroupMode::Grid)
            .var("tabs", mode == arrange::GroupMode::Tabs)
            .var("stack", mode == arrange::GroupMode::Stack);
        let header = crate::dock::panel_header(group.clone(), "panel_group.header", "Panel", None, buttons, &mut frames, self.cx);
        let edit = !self.no_drag();
        let scope = Scope::new(format!("{}:{name}", scope.id))
            .var("empty", empty)
            .var("edit", edit)
            .slot("header", header)
            .slot("content", content);
        render("panel_group", scope, &mut |_, _, f| f.into_any_element(), self.cx)
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
        let dragging = self.cx.try_global::<ActiveDrag>().is_some_and(|a| a.0.is_some());
        if !dragging && !self.no_drag() {
            return self.seam(d, parts.get(active).or(parts.first()), body, scope);
        }
        let Some((p, on_change)) = self.drop_for(d, scope) else { return body };
        // Next to or onto the part shown, or another of its stack when that one is dragged.
        let shown = parts.get(active).or(parts.first());
        let Some(name) = shown.filter(|n| **n != *p.part).or_else(|| parts.iter().find(|n| **n != *p.part)).cloned() else {
            // Let go on its own place: nothing happens (it doesn't float).
            let stay = div().absolute().inset_0().on_drop(|_: &DraggedPart, _, _| {});
            return div().relative().size_full().flex().flex_col().child(body).child(stay).into_any_element();
        };
        // A panel group takes what is dropped in its middle: the dock only
        // has its sides.
        if arrange::is_group(&name) {
            let sides: &[arrange::Edge] = match (d.zones.contains(&"edges"), d.dir) {
                (true, _) => &[arrange::Edge::Left, arrange::Edge::Right, arrange::Edge::Top, arrange::Edge::Bottom],
                (false, arrange::Dir::Row) => &[arrange::Edge::Left, arrange::Edge::Right],
                (false, arrange::Dir::Column) => &[arrange::Edge::Top, arrange::Edge::Bottom],
            };
            let edges = d.zones.contains(&"edges");
            let bands = crate::dock::side_bands::<DraggedPart>(sides, self.cx).into_iter().map(|(edge, band)| {
                let (on_change, dock, name) = (on_change.clone(), d.name.clone(), name.clone());
                band.on_drop(move |p: &DraggedPart, window, cx| {
                    let at = match (edges, edge.after()) {
                        (true, _) => arrange::Drop::Edge(name.clone(), edge),
                        (false, false) => arrange::Drop::Before(name.clone()),
                        (false, true) => arrange::Drop::After(name.clone()),
                    };
                    on_change(Change::Move { part: p.part.clone(), kind: p.kind.clone(), dock: dock.clone(), at }, window, cx);
                })
            });
            return div().relative().size_full().flex().flex_col().child(body).children(bands.collect::<Vec<_>>()).into_any_element();
        }
        // A part that doesn't `attach` takes nothing onto it, unless stacking
        // is all the dock does (a group of tabs: the group takes it).
        let edges = d.zones.contains(&"edges");
        let attach =
            !edges || self.cx.try_global::<Templates>().and_then(|t| t.part(&name)).is_none_or(|n| self.holds(&n.attach, scope, &[]));
        let stack = d.zones.contains(&"stack") && attach;
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
                "after" => (arrange::Drop::After(name.clone()), false),
                _ => continue,
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

    /// `body`, the area of panel group `shown` in dock `d`, while the layout
    /// is edited: along its far side (its bottom in a column) a strip
    /// (`panel_group.seam`) whose button adds an empty group right after it,
    /// and another across (its right side in a column) whose button adds one
    /// beside it, splitting its place.
    fn seam(&self, d: &DockNode, shown: Option<&String>, body: AnyElement, scope: &Scope) -> AnyElement {
        let (Some(name), Some(on_change)) = (shown.filter(|n| arrange::is_group(n)), scope.on_change.clone()) else { return body };
        let size = style::el("panel_group.seam", self.cx).px("size");
        let (after, beside) = match d.dir {
            arrange::Dir::Column => (arrange::Edge::Bottom, arrange::Edge::Right),
            arrange::Dir::Row => (arrange::Edge::Right, arrange::Edge::Bottom),
        };
        let strips = [(after, arrange::Drop::After(name.clone())), (beside, arrange::Drop::Edge(name.clone(), beside))].map(|(edge, at)| {
            let (on_change, dock) = (on_change.clone(), d.name.clone());
            let mut frames = |button: &str, _: Option<usize>, f: Framed| match (button, f) {
                ("add", Framed::Button(b)) => {
                    let (on_change, dock, at) = (on_change.clone(), dock.clone(), at.clone());
                    b.on_click(move |_, window, cx| on_change(Change::AddGroup { dock: dock.clone(), at: at.clone() }, window, cx))
                        .into_any_element()
                },
                (_, f) => f.into_any_element(),
            };
            let id = format!("{}:seam:{edge:?}:{name}", scope.id);
            let strip = render("panel_group.seam", Scope::new(id), &mut frames, self.cx);
            let place = div().absolute();
            let place = match edge {
                arrange::Edge::Bottom => place.left_0().right_0().bottom_0().h(size),
                _ => place.top_0().bottom_0().right_0().w(size),
            };
            place.child(strip)
        });
        div().relative().size_full().flex().flex_col().child(body).children(strips).into_any_element()
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
        // Those in windows of their own are drawn there.
        for (name, f) in a.floating.iter().filter(|(_, f)| f.window.is_none()) {
            if !self.shown(name, scope, &[], 0) {
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
        let tree = match scope.arrangement.as_ref().and_then(|a| a.docks.get(&*d.name)) {
            Some(t) => t.as_ref()?,
            None => &d.info.defaults,
        };
        let shown = |part: &str| self.shown(part, scope, outer, 0);
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
            // A panel group sizes as the first panel in it.
            let part = part.and_then(|name| self.first_panel(name, scope));
            let part = part.as_ref().and_then(|name| Some((name, templates?.part(name)?)));
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
                let shown: Vec<f32> = state.read(cx).sizes().iter().map(|s| f32::from(*s)).collect();
                let shown = arrange::dragged_sizes(&shown, &fixed_by_kept);
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
        // One look per hover group (gpui takes one): states that hold for
        // the same group merge, the later ones over the earlier.
        let mut merged: Vec<(SharedString, HashMap<&'static str, Val>)> = vec![];
        for st in n.states.iter().filter(|s| s.hover.is_some() && holds(&s.when)) {
            let group = st.hover.clone().unwrap_or_default();
            let at = match merged.iter().position(|(g, _)| *g == group) {
                Some(i) => i,
                None => {
                    merged.push((group, HashMap::new()));
                    merged.len() - 1
                },
            };
            let m = &mut merged[at].1;
            m.extend(st.props.iter().map(|(k, v)| (*k, v.clone())));
            filled(&st.holes, m);
        }
        let hovers = merged.into_iter().map(|(group, m)| (group, style::el_with("", Arc::new(m), self.cx))).collect();
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
            // The children that don't fit wrap onto lines below the box's
            // height, out of sight.
            Layout::Fit => d.flex_row().flex_wrap().overflow_hidden(),
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
        let kids: Vec<AnyElement> = if n.layout == Layout::Fit {
            // Each as tall as the box, so a wrapped line starts right below
            // it; one alone on the line shrinks to the box, so it can fit
            // what it holds (a `fit` box in it hides the rest).
            kids.into_iter().map(|k| div().h_full().min_w_0().flex().items_center().child(k).into_any_element()).collect()
        } else {
            kids
        };
        let d = d.children(look.backdrop()).children(under).children(kids);
        let e = match (drag, &n.frame) {
            // gpui doesn't say what is dragged: the drag's start keeps it for the docks.
            (Some(p), _) => {
                let mut d = d.id(ElementId::Name(format!("handle:{}", p.part).into()));
                // gpui's drags stay in their window: one in a window of its own doesn't drag.
                if !p.popped {
                    d = d.on_drag(p.clone(), crate::dock::start_drag);
                }
                let floats = self.cx.try_global::<Floats>();
                let grounded = floats.is_some_and(|f| f.grounded.contains(p.part.as_ref()));
                let windowless = floats.is_some_and(|f| f.windowless.contains(p.part.as_ref()));
                if p.kind == "pane" || (grounded && windowless && !p.floating && !p.popped) {
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

/// A part's handle's right-click menu (`menu.part`): Float, Move to New
/// Window, or Dock when it floats or is in a window of its own.
fn part_menu(p: &DraggedPart, popup: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
    let part = p.part.clone();
    let items: MenuItems = Rc::new(move |name, _| {
        let action: Box<dyn Action> = match name {
            "float" => Box::new(crate::dock::FloatPart(part.clone())),
            "window" => Box::new(crate::dock::PopOutPart(part.clone())),
            "dock" => Box::new(crate::dock::DockPart(part.clone())),
            _ => return None,
        };
        let h: MenuHandler = Rc::new(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx));
        Some(h)
    });
    let floats = cx.try_global::<Floats>();
    let can_float = !floats.is_some_and(|f| f.grounded.contains(p.part.as_ref()));
    let can_window = !floats.is_some_and(|f| f.windowless.contains(p.part.as_ref()));
    let centered = floats.is_some_and(|f| f.centered.contains(p.part.as_ref()));
    let scope = Scope::new(format!("part-menu:{}", p.part))
        .var("floating", p.floating)
        .var("popped", p.popped)
        .var("centered", centered)
        .var("can_float", can_float)
        .var("can_window", can_window)
        .var("part", p.part.clone());
    menu("menu.part", scope, popup, items, window, cx)
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

/// A ring `full` percent full, as [`super::Spec::ring`] says, in the box
/// it is drawn in.
fn ring(full: Option<f32>, look: &style::El) -> impl IntoElement {
    let (arc, track) = (look.color("text"), look.color("track"));
    let width = look.px("stroke_width").max(px(1.));
    let turn = look.num("rotate");
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            use lyon_tessellation::{LineCap, StrokeOptions};
            let size = bounds.size.width.min(bounds.size.height);
            let r = (size - width) / 2.;
            let c = bounds.center();
            // Clockwise from the top, `turn` turns on.
            let at = |t: f32| {
                let a = (t + turn) * std::f32::consts::TAU;
                point(c.x + r * a.sin(), c.y - r * a.cos())
            };
            let points = |from: f32, to: f32| -> Vec<Point<Pixels>> {
                let n = ((to - from) * 64.).ceil().max(2.) as usize;
                (0..=n).map(|i| at(from + (to - from) * i as f32 / n as f32)).collect()
            };
            let mut stroke = |pts: &[Point<Pixels>], closed: bool, dash: bool, color: Hsla| {
                let opts = StrokeOptions::default().with_line_width(f32::from(width)).with_line_cap(LineCap::Round);
                let mut pb = PathBuilder::stroke(width).with_style(PathStyle::Stroke(opts));
                if dash {
                    // Twelve dashes around.
                    let step = r * std::f32::consts::TAU / 24.;
                    pb = pb.dash_array(&[step * 0.9, step * 1.1]);
                }
                pb.add_polygon(pts, closed);
                if let Ok(path) = pb.build() {
                    window.paint_path(path, color);
                }
            };
            let whole = points(0., 1.);
            match full.map(|f| (f / 100.).clamp(0., 1.)) {
                None => stroke(&whole, true, true, track),
                Some(f) => {
                    stroke(&whole, true, false, track);
                    if f >= 1. {
                        stroke(&whole, true, false, arc);
                    } else if f > 0. {
                        stroke(&points(0., f), false, false, arc);
                    }
                },
            }
        },
    )
    .size_full()
}
