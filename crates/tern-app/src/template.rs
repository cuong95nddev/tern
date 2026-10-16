//! Templates: the window's parts as trees of boxes, so a theme or
//! `settings.json` can rearrange them (`templates`), not just restyle them.
//!
//! A template is a node; a node is a box with children, a piece the code
//! provides (`slot`: a terminal, the explorer, a tab bar), text with values
//! put in (`"{title}"`), a list the code provides drawn once per entry
//! (`each` + `item`), or another template (`use`). Boxes take an element's
//! properties (`element`) and their own (`style`, the same values as
//! `elements` in `settings.json`), lay out their children (`layout`, `align`,
//! `justify`, `grow`, `fill`), sit over their parent (`position`, `z`,
//! `overlay`), show only when a condition holds (`when`), change while one
//! does (`states`), or split into panels the user resizes (`split`).
//!
//! The code still owns behavior: what a click or a drag does, focus,
//! scrolling. It attaches that to boxes by name (`frame`) and fills the
//! slots; what a template doesn't use is simply not drawn.
//!
//! The built-in templates (`templates.json`) draw Tern as it looks out of the
//! box; one given in a theme or `settings.json` replaces the one of that name.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use gpui_kit::component::{h_resizable, resizable_panel, v_resizable};
use gpui_kit::*;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::style::{self, Props, Val};

/// The built-in templates.
const BUILTIN: &str = include_str!("templates.json");

/// A node as written.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Spec {
    /// A piece the code provides here (see `reference/templates.md` for each
    /// template's slots).
    pub slot: Option<String>,
    /// Text; `{name}` puts in the template's value of that name.
    pub text: Option<String>,
    /// Another template, drawn here. This node's `when`, `position`, `z`,
    /// `grow`, `fill` and panel settings go over its own.
    #[serde(rename = "use")]
    pub use_: Option<String>,
    /// A list the code provides: `item` is drawn once for each entry, with
    /// the entry's values and slots.
    pub each: Option<String>,
    pub item: Option<Box<Spec>>,
    /// A name the code attaches behavior to (clicks, drags, focus).
    pub frame: Option<String>,
    /// The element whose properties this box takes (see `reference/elements.md`).
    pub element: Option<String>,
    /// Properties of the box, over its element's: any of an element's box
    /// properties (`padding`, `background`, `border_width`, …).
    #[schemars(with = "BTreeMap<String, serde_json::Value>")]
    pub style: BTreeMap<String, serde_json::Value>,
    /// Properties over `style` while a condition holds:
    /// `{"focused": {"border": "$accent"}}`.
    #[schemars(with = "BTreeMap<String, BTreeMap<String, serde_json::Value>>")]
    pub states: BTreeMap<String, BTreeMap<String, serde_json::Value>>,
    /// How the children line up: `column` (the default), `row`, or `wrap`
    /// (a row that wraps).
    pub layout: Option<String>,
    /// Across the layout: `start`, `center`, `end`, `stretch` (the default).
    pub align: Option<String>,
    /// Along the layout: `start` (the default), `center`, `end`, `between`, `around`.
    pub justify: Option<String>,
    /// Takes its parent's whole size.
    pub fill: bool,
    /// Its share of the space its parent has left along its layout.
    pub grow: Option<f32>,
    /// `0` keeps it from getting smaller than its content.
    pub shrink: Option<f32>,
    /// Placed over its parent at these edges (sizes), out of the layout.
    pub position: Option<Edges>,
    /// Paint order among its siblings: higher is on top.
    pub z: i32,
    /// Painted over everything else in the window.
    pub overlay: bool,
    /// Shown only while this holds: `focused`, `!vertical`, `tabs > 1`,
    /// `a && b`, `a || b`.
    pub when: Option<String>,
    /// One line, cut with an ellipsis when it doesn't fit.
    pub truncate: bool,
    /// Hides what spills out of it.
    pub clip: bool,
    /// Paints its border under its children, so a child can sit on it (a
    /// box normally paints its border over them).
    pub border_under: bool,
    /// Its children are panels the user resizes, side by side (`row`) or
    /// one above another (`column`).
    pub split: Option<String>,
    /// As a split's panel: its starting size.
    #[schemars(with = "Option<serde_json::Value>")]
    pub size: Option<serde_json::Value>,
    /// As a split's panel: how small and how large it may get.
    #[schemars(with = "Option<serde_json::Value>")]
    pub min: Option<serde_json::Value>,
    #[schemars(with = "Option<serde_json::Value>")]
    pub max: Option<serde_json::Value>,
    /// As a split's panel: keeps its size when the split grows (`true`, or
    /// a condition).
    #[schemars(with = "Option<serde_json::Value>")]
    pub fixed: Option<serde_json::Value>,
    pub children: Vec<Spec>,
}

/// Where a positioned node sits: distances from its parent's edges.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Edges {
    #[schemars(with = "Option<serde_json::Value>")]
    pub top: Option<serde_json::Value>,
    #[schemars(with = "Option<serde_json::Value>")]
    pub right: Option<serde_json::Value>,
    #[schemars(with = "Option<serde_json::Value>")]
    pub bottom: Option<serde_json::Value>,
    #[schemars(with = "Option<serde_json::Value>")]
    pub left: Option<serde_json::Value>,
}

/// The built-in templates, by name.
pub fn builtin() -> BTreeMap<String, Spec> {
    serde_json::from_str(BUILTIN).expect("built-in templates")
}

/// The built-in templates as written, for the reference folder.
pub fn builtin_text() -> &'static str {
    BUILTIN
}

/// A value a template reads: in text, in conditions.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Num(f64),
    Text(SharedString),
}

impl Value {
    fn truthy(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.,
            Value::Text(t) => !t.is_empty(),
        }
    }

    fn num(&self) -> f64 {
        match self {
            Value::Bool(b) => f64::from(u8::from(*b)),
            Value::Num(n) => *n,
            Value::Text(t) => t.parse().unwrap_or(0.),
        }
    }

    fn text(&self) -> SharedString {
        match self {
            Value::Bool(b) => b.to_string().into(),
            Value::Num(n) => n.to_string().into(),
            Value::Text(t) => t.clone(),
        }
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

impl From<usize> for Value {
    fn from(n: usize) -> Self {
        Value::Num(n as f64)
    }
}

impl From<String> for Value {
    fn from(t: String) -> Self {
        Value::Text(t.into())
    }
}

impl From<&str> for Value {
    fn from(t: &str) -> Self {
        Value::Text(t.to_owned().into())
    }
}

impl From<SharedString> for Value {
    fn from(t: SharedString) -> Self {
        Value::Text(t)
    }
}

/// What the code gives a template to draw: values, slots, lists.
#[derive(Default)]
pub struct Scope {
    /// Makes the ids of resizable splits unique to this drawing (a pane's).
    id: SharedString,
    vars: HashMap<&'static str, Value>,
    slots: HashMap<&'static str, AnyElement>,
    lists: HashMap<&'static str, Vec<Scope>>,
    /// The entry's place in its list.
    index: Option<usize>,
}

impl Scope {
    pub fn new(id: impl Into<SharedString>) -> Self {
        Scope { id: id.into(), ..Default::default() }
    }

    pub fn var(mut self, name: &'static str, v: impl Into<Value>) -> Self {
        self.vars.insert(name, v.into());
        self
    }

    pub fn slot(mut self, name: &'static str, e: impl IntoElement) -> Self {
        self.slots.insert(name, e.into_any_element());
        self
    }

    pub fn slot_opt(self, name: &'static str, e: Option<impl IntoElement>) -> Self {
        match e {
            Some(e) => self.slot(name, e),
            None => self,
        }
    }

    // Lists come with the tab and row templates.
    #[allow(dead_code)]
    pub fn list(mut self, name: &'static str, items: Vec<Scope>) -> Self {
        let items = items.into_iter().enumerate().map(|(i, s)| Scope { index: Some(i), ..s }).collect();
        self.lists.insert(name, items);
        self
    }
}

/// Attaches behavior to a box named by `frame`: gets the frame's name, the
/// list entry it is in (if any) and the box; gives back what to draw.
pub type Frames<'a> = dyn FnMut(&str, Option<usize>, Div) -> AnyElement + 'a;

/// The templates in effect, ready to draw.
#[derive(Default)]
pub struct Templates(HashMap<String, Arc<Node>>);

impl Global for Templates {}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Layout {
    Column,
    Row,
    Wrap,
}

#[derive(Debug, Clone, Copy)]
enum Align {
    Start,
    Center,
    End,
    Stretch,
}

#[derive(Debug, Clone, Copy)]
enum Justify {
    Start,
    Center,
    End,
    Between,
    Around,
}

#[derive(Debug, Clone)]
enum What {
    Box,
    Slot(&'static str),
    Text(Vec<Part>),
    Each(SharedString, Arc<Node>),
}

#[derive(Debug, Clone)]
enum Part {
    Lit(SharedString),
    Var(String),
}

/// A condition: alternatives (`||`) of conjunctions (`&&`) of tests.
#[derive(Debug, Clone, PartialEq)]
pub struct Cond(Vec<Vec<Test>>);

#[derive(Debug, Clone, PartialEq)]
enum Test {
    Is(String, bool),
    Cmp(String, Cmp, f64),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Cmp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Cond {
    pub fn parse(text: &str) -> Result<Cond, String> {
        let mut any = vec![];
        for alt in text.split("||") {
            let mut all = vec![];
            for test in alt.split("&&") {
                all.push(Test::parse(test.trim()).map_err(|e| format!("\"{text}\": {e}"))?);
            }
            any.push(all);
        }
        Ok(Cond(any))
    }

    fn holds(&self, get: &dyn Fn(&str) -> Option<Value>) -> bool {
        self.0.iter().any(|all| all.iter().all(|t| t.holds(get)))
    }
}

impl Test {
    fn parse(t: &str) -> Result<Test, String> {
        for (op, cmp) in [("==", Cmp::Eq), ("!=", Cmp::Ne), ("<=", Cmp::Le), (">=", Cmp::Ge), ("<", Cmp::Lt), (">", Cmp::Gt)] {
            if let Some((name, n)) = t.split_once(op) {
                let n: f64 = n.trim().parse().map_err(|_| format!("\"{}\" is not a number", n.trim()))?;
                return Ok(Test::Cmp(ident(name.trim())?, cmp, n));
            }
        }
        match t.strip_prefix('!') {
            Some(name) => Ok(Test::Is(ident(name.trim())?, false)),
            None => Ok(Test::Is(ident(t)?, true)),
        }
    }

    fn holds(&self, get: &dyn Fn(&str) -> Option<Value>) -> bool {
        match self {
            Test::Is(name, want) => get(name).is_some_and(|v| v.truthy()) == *want,
            Test::Cmp(name, cmp, n) => {
                let v = get(name).map_or(0., |v| v.num());
                match cmp {
                    Cmp::Eq => v == *n,
                    Cmp::Ne => v != *n,
                    Cmp::Lt => v < *n,
                    Cmp::Le => v <= *n,
                    Cmp::Gt => v > *n,
                    Cmp::Ge => v >= *n,
                }
            },
        }
    }
}

fn ident(t: &str) -> Result<String, String> {
    if t == "true" || t == "false" || (!t.is_empty() && t.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.')) {
        Ok(t.to_owned())
    } else {
        Err(format!("\"{t}\" is not a name"))
    }
}

/// A split's panel settings.
#[derive(Debug, Clone, Default)]
struct Panel {
    size: Option<AbsoluteLength>,
    min: Option<AbsoluteLength>,
    max: Option<AbsoluteLength>,
    fixed: Option<Cond>,
}

/// A node, its values read.
#[derive(Debug, Clone)]
pub struct Node {
    what: What,
    /// Unique within its template: the ids of splits.
    path: SharedString,
    element: &'static str,
    style: Arc<HashMap<&'static str, Val>>,
    states: Vec<(Cond, HashMap<&'static str, Val>)>,
    layout: Layout,
    align: Option<Align>,
    justify: Option<Justify>,
    fill: bool,
    grow: Option<f32>,
    shrink: Option<f32>,
    position: Option<[Option<AbsoluteLength>; 4]>,
    z: i32,
    overlay: bool,
    when: Option<Cond>,
    truncate: bool,
    clip: bool,
    border_under: bool,
    split: Option<Axis>,
    panel: Panel,
    frame: Option<SharedString>,
    children: Vec<Node>,
}

/// The slots every template may name. The code fills those it has; a slot
/// it leaves empty draws nothing.
const SLOTS: &[&str] = &[
    "menu_bar",
    "welcome",
    "workspace_tabs",
    "workspace",
    "left_strip",
    "right_strip",
    "explorer",
    "scratches",
    "center",
    "changes",
    "tab_bar",
    "vertical_tabs",
    "content",
    "drop_overlay",
];

/// Reads `specs` (the built-in templates with the theme's and the
/// settings' over them) with `props`; what's wrong goes to `problems`.
pub fn compile(specs: &BTreeMap<String, Spec>, props: &Props, problems: &mut Vec<String>) -> Templates {
    let mut c = Compiler { specs, props, problems, done: HashMap::new(), stack: vec![] };
    for name in specs.keys() {
        c.template(name);
    }
    Templates(c.done)
}

struct Compiler<'a, 'b> {
    specs: &'a BTreeMap<String, Spec>,
    props: &'a Props<'b>,
    problems: &'a mut Vec<String>,
    done: HashMap<String, Arc<Node>>,
    /// The templates being read, to catch one that uses itself.
    stack: Vec<String>,
}

impl Compiler<'_, '_> {
    fn template(&mut self, name: &str) -> Option<Arc<Node>> {
        if let Some(n) = self.done.get(name) {
            return Some(n.clone());
        }
        let spec = self.specs.get(name)?;
        if self.stack.iter().any(|s| s == name) {
            self.problems.push(format!("templates.{name}: uses itself"));
            return None;
        }
        self.stack.push(name.to_owned());
        let node = Arc::new(self.node(spec, &format!("templates.{name}")));
        self.stack.pop();
        self.done.insert(name.to_owned(), node.clone());
        Some(node)
    }

    fn len(&mut self, v: &Option<serde_json::Value>, at: &str) -> Option<AbsoluteLength> {
        self.props.len(v.as_ref()?, at, self.problems)
    }

    fn cond(&mut self, text: &str, at: &str) -> Option<Cond> {
        Cond::parse(text).map_err(|e| self.problems.push(format!("{at}: {e}"))).ok()
    }

    fn node(&mut self, s: &Spec, at: &str) -> Node {
        let mut node = match &s.use_ {
            Some(name) => match self.template(name) {
                Some(n) => (*n).clone(),
                None => {
                    if !self.stack.iter().any(|t| t == name) {
                        self.problems.push(format!("{at}.use: no template \"{name}\""));
                    }
                    self.own(&Spec::default(), at)
                },
            },
            None => self.own(s, at),
        };
        if s.use_.is_some() {
            // The place it's used in decides where it goes.
            if let Some(w) = &s.when {
                node.when = self.cond(w, &format!("{at}.when"));
            }
            if s.position.is_some() {
                node.position = self.position(s, at);
            }
            node.z = if s.z != 0 { s.z } else { node.z };
            node.overlay |= s.overlay;
            node.fill |= s.fill;
            node.grow = s.grow.or(node.grow);
            node.shrink = s.shrink.or(node.shrink);
            let panel = self.panel(s, at);
            node.panel = Panel {
                size: panel.size.or(node.panel.size),
                min: panel.min.or(node.panel.min),
                max: panel.max.or(node.panel.max),
                fixed: panel.fixed.or(node.panel.fixed),
            };
            node.frame = s.frame.clone().map(Into::into).or(node.frame);
        }
        node.path = at.into();
        node
    }

    fn position(&mut self, s: &Spec, at: &str) -> Option<[Option<AbsoluteLength>; 4]> {
        let p = s.position.as_ref()?;
        Some([
            self.len(&p.top, &format!("{at}.position.top")),
            self.len(&p.right, &format!("{at}.position.right")),
            self.len(&p.bottom, &format!("{at}.position.bottom")),
            self.len(&p.left, &format!("{at}.position.left")),
        ])
    }

    fn panel(&mut self, s: &Spec, at: &str) -> Panel {
        let fixed = match &s.fixed {
            None | Some(serde_json::Value::Bool(false)) => None,
            Some(serde_json::Value::Bool(true)) => Some(Cond(vec![vec![]])),
            Some(serde_json::Value::String(c)) => self.cond(c, &format!("{at}.fixed")),
            Some(_) => {
                self.problems.push(format!("{at}.fixed: true, false or a condition"));
                None
            },
        };
        Panel {
            size: self.len(&s.size, &format!("{at}.size")),
            min: self.len(&s.min, &format!("{at}.min")),
            max: self.len(&s.max, &format!("{at}.max")),
            fixed,
        }
    }

    fn own(&mut self, s: &Spec, at: &str) -> Node {
        let kinds = [s.slot.is_some(), s.text.is_some(), s.each.is_some(), s.use_.is_some()];
        if kinds.iter().filter(|k| **k).count() > 1 {
            self.problems.push(format!("{at}: a node is one of slot, text, each and use"));
        }
        let element = match &s.element {
            Some(name) => match style::element(name) {
                Some(e) => Some(e),
                None => {
                    self.problems.push(format!("{at}.element: no element \"{name}\""));
                    None
                },
            },
            None => None,
        };
        let what = if let Some(slot) = &s.slot {
            match SLOTS.iter().find(|n| **n == slot) {
                Some(n) => What::Slot(n),
                None => {
                    self.problems.push(format!("{at}.slot: no slot \"{slot}\" (there are: {})", SLOTS.join(", ")));
                    What::Box
                },
            }
        } else if let Some(text) = &s.text {
            What::Text(parts(text))
        } else if let Some(list) = &s.each {
            match &s.item {
                Some(item) => What::Each(list.clone().into(), Arc::new(self.node(item, &format!("{at}.item")))),
                None => {
                    self.problems.push(format!("{at}: \"each\" needs an \"item\""));
                    What::Box
                },
            }
        } else {
            What::Box
        };
        let style = self.props.compile(element, &s.style, &format!("{at}.style"), self.problems);
        let mut states = vec![];
        for (cond, props) in &s.states {
            let Some(c) = self.cond(cond, &format!("{at}.states")) else { continue };
            states.push((c, self.props.compile(element, props, &format!("{at}.states.{cond}"), self.problems)));
        }
        let pick = |v: &Option<String>, what: &str, problems: &mut Vec<String>, opts: &[&str]| -> Option<usize> {
            let v = v.as_deref()?;
            let found = opts.iter().position(|o| *o == v);
            if found.is_none() {
                problems.push(format!("{at}.{what}: \"{v}\" is not one of {}", opts.join(", ")));
            }
            found
        };
        let layout = match pick(&s.layout, "layout", self.problems, &["column", "row", "wrap"]) {
            Some(1) => Layout::Row,
            Some(2) => Layout::Wrap,
            _ => Layout::Column,
        };
        let align = pick(&s.align, "align", self.problems, &["start", "center", "end", "stretch"])
            .map(|i| [Align::Start, Align::Center, Align::End, Align::Stretch][i]);
        let justify = pick(&s.justify, "justify", self.problems, &["start", "center", "end", "between", "around"])
            .map(|i| [Justify::Start, Justify::Center, Justify::End, Justify::Between, Justify::Around][i]);
        let split = pick(&s.split, "split", self.problems, &["row", "column"]).map(|i| [Axis::Horizontal, Axis::Vertical][i]);
        let when = s.when.as_deref().and_then(|w| self.cond(w, &format!("{at}.when")));
        let mut children: Vec<Node> = s.children.iter().enumerate().map(|(i, c)| self.node(c, &format!("{at}.children.{i}"))).collect();
        // Later children paint over earlier ones; `z` reorders them.
        children.sort_by_key(|c| c.z);
        Node {
            what,
            path: at.into(),
            element: element.map_or("", |e| e.name),
            style: Arc::new(style),
            states,
            layout,
            align,
            justify,
            fill: s.fill,
            grow: s.grow,
            shrink: s.shrink,
            position: self.position(s, at),
            z: s.z,
            overlay: s.overlay,
            when,
            truncate: s.truncate,
            clip: s.clip,
            border_under: s.border_under,
            split,
            panel: self.panel(s, at),
            frame: s.frame.clone().map(Into::into),
            children,
        }
    }
}

fn parts(text: &str) -> Vec<Part> {
    let mut out = vec![];
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        let Some(len) = rest[start..].find('}') else { break };
        if start > 0 {
            out.push(Part::Lit(rest[..start].to_owned().into()));
        }
        out.push(Part::Var(rest[start + 1..start + len].trim().to_owned()));
        rest = &rest[start + len + 1..];
    }
    if !rest.is_empty() {
        out.push(Part::Lit(rest.to_owned().into()));
    }
    out
}

/// Draws template `name` with `scope`; `frames` attaches behavior to the
/// boxes that name a frame.
pub fn render(name: &str, scope: Scope, frames: &mut Frames, cx: &App) -> AnyElement {
    let Some(node) = cx.try_global::<Templates>().and_then(|t| t.0.get(name)).cloned() else {
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
            What::Slot(name) => {
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

#[cfg(test)]
mod tests {
    use super::{BTreeMap, Cond, HashMap, Part, Props, Spec, Value, builtin, compile, parts, style};

    #[test]
    fn conditions() {
        let vars: HashMap<&str, Value> =
            [("focused", Value::Bool(true)), ("tabs", Value::Num(3.)), ("title", Value::Text("".into()))].into();
        let get = |n: &str| vars.get(n).cloned();
        let holds = |c: &str| Cond::parse(c).unwrap().holds(&get);
        assert!(holds("focused"));
        assert!(!holds("!focused"));
        assert!(holds("tabs > 1 && focused"));
        assert!(!holds("tabs > 3"));
        assert!(holds("title || tabs >= 3"));
        assert!(!holds("title"));
        assert!(!holds("missing"));
        assert!(Cond::parse("a b").is_err());
    }

    #[test]
    fn text_parts() {
        let p = parts("Settings: {error}!");
        assert!(matches!(&p[..], [Part::Lit(a), Part::Var(v), Part::Lit(b)] if a == "Settings: " && v == "error" && b == "!"));
    }

    #[test]
    fn builtin_templates_read_cleanly() {
        let over = style::Overrides::default();
        let theme = gpui_kit::component::ThemeColor::default();
        let props = Props::new(&over, &theme, std::path::Path::new(""));
        let mut problems = vec![];
        let t = compile(&builtin(), &props, &mut problems);
        assert_eq!(problems, Vec::<String>::new());
        assert!(t.0.contains_key("window") && t.0.contains_key("pane"));
    }

    #[test]
    fn mistakes_are_reported() {
        let over = style::Overrides::default();
        let theme = gpui_kit::component::ThemeColor::default();
        let props = Props::new(&over, &theme, std::path::Path::new(""));
        let specs: BTreeMap<String, Spec> = serde_json::from_str(
            r#"{"a": {"slot": "nope", "children": [{"use": "b"}, {"use": "a"}, {"layout": "diagonal"}, {"element": "x"}]}}"#,
        )
        .unwrap();
        let mut problems = vec![];
        compile(&specs, &props, &mut problems);
        assert_eq!(problems.len(), 5, "{problems:?}");
    }
}
