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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use gpui_kit::*;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::arrange::{Arrangement, Dir, DockInfo, Size, Tree};
use crate::dock::OnChange;
use crate::style::{self, Props, Val};

mod draw;

#[allow(unused_imports)] // `Frames` is named by `render`'s callers only through inference.
pub use draw::{Framed, Frames, on_event, render};

/// The built-in templates.
const BUILTIN: &str = include_str!("../templates.json");

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
    /// the entry's values and slots (and `index`, its place); `children`
    /// come after the entries.
    pub each: Option<String>,
    pub item: Option<Box<Spec>>,
    /// An icon by name (`"plus"`, `"columns-2"`: the bundled Lucide names),
    /// or `"{name}"` for one the code gives. On a `button`: its icon.
    pub icon: Option<String>,
    /// A button; the code attaches what it does by this name, as for `frame`.
    /// It takes `icon`, `label`, `tooltip`, `variant`, `size`, `selected`,
    /// `disabled` and `loading`.
    pub button: Option<String>,
    /// A button's text; `{name}` puts in a value.
    pub label: Option<String>,
    /// What a button says under the pointer; `{name}` puts in a value.
    pub tooltip: Option<String>,
    /// A button's look: `ghost` (the default), `primary`, `secondary`,
    /// `danger`, `outline` or `text`.
    pub variant: Option<String>,
    /// A button drawn as on: `true`, `false` or a condition.
    #[schemars(with = "Option<serde_json::Value>")]
    pub selected: Option<serde_json::Value>,
    /// A button that can't be clicked: `true`, `false` or a condition.
    #[schemars(with = "Option<serde_json::Value>")]
    pub disabled: Option<serde_json::Value>,
    /// A button that shows a spinner for its icon: `true`, `false` or a
    /// condition.
    #[schemars(with = "Option<serde_json::Value>")]
    pub loading: Option<serde_json::Value>,
    /// Names this box for `"hover:<group>"` states of the boxes in it.
    pub group: Option<String>,
    /// A name the code attaches behavior to (clicks, drags, focus).
    pub frame: Option<String>,
    /// The element whose properties this box takes (see `reference/elements.md`).
    /// On a `use` site: the element the used template's root takes, and the
    /// one `$element` names in it (`"$element"`, `"$element.count_gap"`).
    pub element: Option<String>,
    /// Properties of the box, over its element's: any of an element's box
    /// properties (`padding`, `background`, `border_width`, …). `"{name}"`
    /// takes a value the code gives (a tab group's color).
    #[schemars(with = "BTreeMap<String, serde_json::Value>")]
    pub style: BTreeMap<String, serde_json::Value>,
    /// Properties over `style` while a condition holds:
    /// `{"focused": {"border": "$accent"}}`. `hover` holds under the pointer,
    /// `hover:<group>` while the box named `group` is (`"hover && !selected"`).
    /// `"element": "x"` among them takes element x's properties (`{"active":
    /// {"element": "pane_tab_bar.tab_active"}}`).
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
    /// As a split's panel: its starting size. On a `button`: `xsmall`,
    /// `small`, `medium` or `large`.
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
    /// A piece the user may move between docks; the code fills it like a slot
    /// of the same name (`explorer`, `scratches`, `changes`).
    pub part: Option<String>,
    /// What a dock matches a part on (default `panel`).
    pub kind: Option<String>,
    /// This box holds parts, named so a layout can refer to it; its children
    /// are its default parts, and only parts. It needs a `split` and a `strip`.
    pub dock: Option<String>,
    /// The part names or kinds a dock takes.
    pub accepts: Vec<String>,
    /// Where a part may be dropped in a dock: `before` and `after`.
    pub zones: Vec<String>,
    /// What a dock does when it has no parts: `hide` (the default) or `keep`.
    pub empty: Option<String>,
    /// `left` or `right`: the tool strip whose buttons show this dock's parts
    /// (a dock needs one).
    pub strip: Option<String>,
    /// Dragging this box drags the part it is drawn for.
    pub handle: bool,
    /// Whether the box can be dragged: `true`, `false` or a condition
    /// (default `true`).
    #[schemars(with = "Option<serde_json::Value>")]
    pub drag: Option<serde_json::Value>,
    /// The size a part gets when it arrives in this dock from elsewhere.
    #[schemars(with = "Option<serde_json::Value>")]
    pub part_size: Option<serde_json::Value>,
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
    /// For a `"{name}"` color in a style.
    Color(Hsla),
}

impl Value {
    fn truthy(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.,
            Value::Text(t) => !t.is_empty(),
            Value::Color(c) => c.a > 0.,
        }
    }

    fn num(&self) -> f64 {
        match self {
            Value::Bool(b) => f64::from(u8::from(*b)),
            Value::Num(n) => *n,
            Value::Text(t) => t.parse().unwrap_or(0.),
            Value::Color(_) => 0.,
        }
    }

    fn text(&self) -> SharedString {
        match self {
            Value::Bool(b) => b.to_string().into(),
            Value::Num(n) => n.to_string().into(),
            Value::Text(t) => t.clone(),
            Value::Color(_) => SharedString::default(),
        }
    }

    /// As a property of `kind`, when it can be one.
    fn val(&self, kind: style::Kind) -> Option<Val> {
        match (kind, self) {
            (style::Kind::Color, Value::Color(c)) => Some(Val::Color(*c)),
            (style::Kind::Len, Value::Num(n)) => Some(Val::Len(px(*n as f32).into())),
            (style::Kind::Num, v @ (Value::Num(_) | Value::Bool(_))) => Some(Val::Num(v.num() as f32)),
            (style::Kind::Text, Value::Text(t)) => Some(Val::Text(t.clone())),
            _ => None,
        }
    }
}

impl From<Hsla> for Value {
    fn from(c: Hsla) -> Self {
        Value::Color(c)
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
    /// Where the parts sit, and who hears of changes to it; docks draw their
    /// default parts without it.
    arrangement: Option<Arc<Arrangement>>,
    on_change: Option<OnChange>,
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

    /// The arrangement the docks draw, and where their changes go.
    pub fn docks(mut self, arrangement: Arc<Arrangement>, on_change: OnChange) -> Self {
        self.arrangement = Some(arrangement);
        self.on_change = Some(on_change);
        self
    }

    /// List `name`'s entries, for `each`.
    pub fn list(mut self, name: &'static str, items: Vec<Scope>) -> Self {
        let items = items.into_iter().enumerate().map(|(i, s)| Scope { index: Some(i), ..s }).collect();
        self.lists.insert(name, items);
        self
    }
}

/// The templates in effect, ready to draw.
#[derive(Default)]
pub struct Templates {
    nodes: HashMap<String, Arc<Node>>,
    /// The docks each template holds, its used templates' included.
    docks: HashMap<String, Vec<DockInfo>>,
    /// Every part, by name: its kind and its node.
    parts: BTreeMap<&'static str, (String, Node)>,
    /// Which compile this is: a new one means the settings changed.
    generation: u64,
}

impl Templates {
    /// The docks `template` holds.
    pub fn docks(&self, template: &str) -> Vec<DockInfo> {
        self.docks.get(template).cloned().unwrap_or_default()
    }

    /// Every part's name and kind.
    pub fn parts(&self) -> Vec<(&'static str, String)> {
        self.parts.iter().map(|(n, (k, _))| (*n, k.clone())).collect()
    }

    /// The node a part is drawn with.
    pub(crate) fn part(&self, name: &str) -> Option<&Node> {
        self.parts.get(name).map(|(_, n)| n)
    }

    /// The tool strip (`left`, `right`) that shows the parts of `dock` in `template`.
    pub fn dock_strip(&self, template: &str, dock: &str) -> Option<SharedString> {
        self.nodes.get(template).and_then(|n| find_dock(n, dock)).and_then(|d| d.strip.clone())
    }

    /// Bumped on every compile, so a change of settings can be noticed.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The parts whose `drag` doesn't hold with `scope`'s values, the ones
    /// their `when` reads where the docks draw them.
    pub fn pinned(&self, scope: &Scope) -> BTreeSet<&'static str> {
        let get = draw::lookup(scope, &[]);
        self.parts.iter().filter(|(_, (_, n))| n.drag.as_ref().is_some_and(|c| !c.holds(&get))).map(|(name, _)| *name).collect()
    }

    /// Whether `template`'s root is a drag handle.
    #[cfg(test)]
    pub fn is_handle(&self, template: &str) -> bool {
        self.nodes.get(template).is_some_and(|n| n.handle)
    }
}

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
    /// A part the user may move; filled like the slot of the same name.
    Part(&'static str),
    Text(Vec<Part>),
    Each(SharedString, Arc<Node>),
    /// An icon's name.
    Icon(Vec<Part>),
    Button(Arc<ButtonNode>),
}

/// A button: the code attaches what it does by `frame`.
#[derive(Debug, Clone)]
struct ButtonNode {
    frame: SharedString,
    icon: Option<Vec<Part>>,
    label: Option<Vec<Part>>,
    tooltip: Option<Vec<Part>>,
    variant: Variant,
    size: ButtonSize,
    selected: Option<Cond>,
    disabled: Option<Cond>,
    loading: Option<Cond>,
}

#[derive(Debug, Clone, Copy)]
enum Variant {
    Ghost,
    Primary,
    Secondary,
    Danger,
    Outline,
    Text,
}

#[derive(Debug, Clone, Copy)]
enum ButtonSize {
    XSmall,
    Small,
    Medium,
    Large,
}

/// Properties a value the code gives fills in (`"{name}"`): the property,
/// what it holds, the value's name.
type Holes = Vec<(&'static str, style::Kind, String)>;

/// Properties over a node's style: when they apply, what they are.
#[derive(Debug, Clone)]
struct State {
    when: Option<Cond>,
    /// Under the pointer: over this box (`Some("")`) or the named group.
    hover: Option<SharedString>,
    props: HashMap<&'static str, Val>,
    holes: Holes,
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

/// A box that holds parts.
#[derive(Debug, Clone)]
pub(crate) struct DockNode {
    name: SharedString,
    dir: Dir,
    zones: Vec<&'static str>,
    empty_keep: bool,
    strip: Option<SharedString>,
    part_size: Option<AbsoluteLength>,
    info: DockInfo,
}

/// A node, its values read.
#[derive(Debug, Clone)]
pub struct Node {
    what: What,
    /// Unique within its template: the ids of splits.
    path: SharedString,
    element: &'static str,
    style: Arc<HashMap<&'static str, Val>>,
    /// Style properties the code's values fill in.
    holes: Holes,
    states: Vec<State>,
    /// Its name for the `hover:<group>` states inside it.
    group: Option<SharedString>,
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
    /// As a part: what a dock matches it on (`panel` when unset).
    kind: Option<String>,
    dock: Option<Arc<DockNode>>,
    /// Dragging this box drags the part it is drawn for.
    handle: bool,
    /// Whether it can be dragged; None is always.
    drag: Option<Cond>,
    children: Vec<Node>,
}

/// The slots every template may name. The code fills those it has; a slot
/// it leaves empty draws nothing.
const SLOTS: &[&str] = &[
    "menu_bar",
    "welcome",
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
    "buttons",
    "icon",
    "scrollbar",
];

/// The parts a template may name.
const PARTS: &[&str] = &["explorer", "scratches", "changes"];

/// The zones a dock may take a drop in.
const ZONES: &[&str] = &["before", "after"];

/// Reads `specs` (the built-in templates with the theme's and the
/// settings' over them) with `props`; what's wrong goes to `problems`.
pub fn compile(specs: &BTreeMap<String, Spec>, props: &Props, problems: &mut Vec<String>) -> Templates {
    let start = problems.len();
    let mut c = Compiler { specs, props, problems, done: HashMap::new(), stack: vec![], element: None };
    for name in specs.keys() {
        c.template(name);
    }
    // A template no other uses is a root: the parts and docks of its expanded
    // tree are the ones placed, so a repeat (also through `use`) is a problem.
    let mut used = BTreeSet::new();
    specs.values().for_each(|s| uses(s, &mut used));
    let mut parts = BTreeMap::new();
    let mut docks_seen = BTreeSet::new();
    for name in specs.keys().filter(|n| !used.contains(n.as_str())) {
        if let Some(root) = c.done.get(name) {
            let mut root = (**root).clone();
            let mut seen = Seen { parts: &mut parts, docks: &mut docks_seen, root: name, problems: c.problems };
            seen.walk(&mut root);
            c.done.insert(name.clone(), Arc::new(root));
        }
    }
    let docks = c
        .done
        .iter()
        .map(|(name, node)| {
            let mut docks = docks_of(node);
            let mut names = BTreeSet::new();
            docks.retain(|d| names.insert(d.name.clone()));
            (name.clone(), docks)
        })
        .collect();
    // A template used with an `element` is read once for each use: say each problem once.
    let nodes = c.done;
    let mut said = BTreeSet::new();
    let mut ix = 0;
    problems.retain(|p| {
        ix += 1;
        ix <= start || said.insert(p.clone())
    });
    static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Templates { nodes, docks, parts, generation }
}

/// The templates `spec` uses, below it too.
fn uses(spec: &Spec, out: &mut BTreeSet<String>) {
    out.extend(spec.use_.clone());
    spec.item.iter().for_each(|i| uses(i, out));
    spec.children.iter().for_each(|c| uses(c, out));
}

/// The parts and docks placed so far.
struct Seen<'a> {
    parts: &'a mut BTreeMap<&'static str, (String, Node)>,
    docks: &'a mut BTreeSet<String>,
    root: &'a str,
    problems: &'a mut Vec<String>,
}

impl Seen<'_> {
    /// Indexes the parts of `node` with their use-site settings; a part or dock
    /// seen before turns into a plain box, so it is drawn once.
    fn walk(&mut self, node: &mut Node) {
        if let What::Part(name) = node.what {
            if self.parts.contains_key(name) {
                self.problems.push(format!("templates.{}: part \"{name}\" placed twice", self.root));
                node.what = What::Box;
            } else {
                self.parts.insert(name, (node.kind.clone().unwrap_or_else(|| "panel".to_owned()), node.clone()));
            }
        }
        if let Some(d) = &node.dock
            && !self.docks.insert(d.info.name.clone())
        {
            self.problems.push(format!("templates.{}: dock \"{}\" placed twice", self.root, d.info.name));
            node.dock = None;
        }
        if let What::Each(_, item) = &mut node.what {
            self.walk(Arc::make_mut(item));
        }
        node.children.iter_mut().for_each(|c| self.walk(c));
        // A part placed before is no longer one of the dock's.
        if let Some(d) = &mut node.dock {
            let d = Arc::make_mut(d);
            d.info.defaults = dock_defaults(d.dir, &node.children);
        }
    }
}

/// The dock named `name` in `node`.
fn find_dock<'n>(node: &'n Node, name: &str) -> Option<&'n DockNode> {
    if let Some(d) = node.dock.as_deref().filter(|d| *d.name == *name) {
        return Some(d);
    }
    if let What::Each(_, item) = &node.what
        && let Some(d) = find_dock(item, name)
    {
        return Some(d);
    }
    node.children.iter().find_map(|c| find_dock(c, name))
}

/// The docks in `node`, in drawing order.
fn docks_of(node: &Node) -> Vec<DockInfo> {
    let mut out: Vec<DockInfo> = node.dock.iter().map(|d| d.info.clone()).collect();
    if let What::Each(_, item) = &node.what {
        out.extend(docks_of(item));
    }
    for c in &node.children {
        out.extend(docks_of(c));
    }
    out
}

struct Compiler<'a, 'b> {
    specs: &'a BTreeMap<String, Spec>,
    props: &'a Props<'b>,
    problems: &'a mut Vec<String>,
    done: HashMap<String, Arc<Node>>,
    /// The templates being read, to catch one that uses itself.
    stack: Vec<String>,
    /// What `$element` names: the element given where the template being read is used.
    element: Option<&'static str>,
}

impl Compiler<'_, '_> {
    fn template(&mut self, name: &str) -> Option<Arc<Node>> {
        if let Some(n) = self.done.get(name) {
            return Some(n.clone());
        }
        let node = Arc::new(self.read(name, None)?);
        self.done.insert(name.to_owned(), node.clone());
        Some(node)
    }

    /// Template `name` read with `$element` naming `element`.
    fn read(&mut self, name: &str, element: Option<&'static str>) -> Option<Node> {
        let spec = self.specs.get(name)?;
        if self.stack.iter().any(|s| s == name) {
            self.problems.push(format!("templates.{name}: uses itself"));
            return None;
        }
        self.stack.push(name.to_owned());
        let outer = std::mem::replace(&mut self.element, element);
        let node = self.node(spec, &format!("templates.{name}"));
        self.element = outer;
        self.stack.pop();
        Some(node)
    }

    /// `raw` with `$element.` naming the element in effect; without one, those values go.
    fn bind(&self, raw: &BTreeMap<String, serde_json::Value>) -> BTreeMap<String, serde_json::Value> {
        raw.iter()
            .filter_map(|(k, v)| match v.as_str().and_then(|t| t.strip_prefix("$element.")) {
                Some(prop) => self.element.map(|e| (k.clone(), serde_json::Value::String(format!("${e}.{prop}")))),
                None => Some((k.clone(), v.clone())),
            })
            .collect()
    }

    /// The element `name` names (`$element`: the one in effect); an unknown one is a problem.
    fn element(&mut self, name: &str, at: &str) -> Option<&'static style::Element> {
        if name == "$element" {
            return self.element.and_then(style::element);
        }
        let e = style::element(name);
        if e.is_none() {
            self.problems.push(format!("{at}.element: no element \"{name}\""));
        }
        e
    }

    fn len(&mut self, v: &Option<serde_json::Value>, at: &str) -> Option<AbsoluteLength> {
        self.props.len(v.as_ref()?, at, self.problems)
    }

    fn cond(&mut self, text: &str, at: &str) -> Option<Cond> {
        Cond::parse(text).map_err(|e| self.problems.push(format!("{at}: {e}"))).ok()
    }

    /// `raw` (a `style` or a state's properties) read for `element`: the
    /// values, and the properties a `"{name}"` leaves to the code.
    fn style(
        &mut self,
        element: Option<&'static style::Element>,
        raw: &BTreeMap<String, serde_json::Value>,
        at: &str,
    ) -> (HashMap<&'static str, Val>, Holes) {
        let mut fixed = BTreeMap::new();
        let mut holes = vec![];
        let mut raw = self.bind(raw);
        // `"element": "x"`: x's properties, under those given here.
        if let Some(v) = raw.remove("element") {
            match v.as_str().and_then(style::element) {
                Some(e) => {
                    for &(prop, _, _) in e.props() {
                        let takes = element.is_some_and(|own| own.prop(prop).is_some()) || style::BOX.iter().any(|p| p.0 == prop);
                        if takes && !raw.contains_key(prop) {
                            raw.insert(prop.to_owned(), serde_json::Value::String(format!("${}.{prop}", e.name)));
                        }
                    }
                },
                None => self.problems.push(format!("{at}.element: no element {v}")),
            }
        }
        for (name, v) in raw {
            let var = v.as_str().and_then(|t| t.strip_prefix('{')).and_then(|t| t.strip_suffix('}')).map(str::trim);
            let Some(var) = var else {
                fixed.insert(name, v);
                continue;
            };
            match element.and_then(|e| e.prop(&name)).or_else(|| style::box_prop(&name)) {
                Some(&(prop, kind, _)) if ident(var).is_ok() => holes.push((prop, kind, var.to_owned())),
                Some(_) => self.problems.push(format!("{at}.{name}: \"{{{var}}}\" is not a name")),
                None => self.problems.push(format!("{at}: no property \"{name}\"")),
            }
        }
        (self.props.compile(element, &fixed, at, self.problems), holes)
    }

    /// A state's key: its condition, and whether it holds under the pointer
    /// (`hover`, or `hover:<group>`), as one of its `&&` terms.
    fn state_key(&mut self, key: &str, at: &str) -> Option<(Option<Cond>, Option<SharedString>)> {
        let is_hover = |t: &str| t == "hover" || t.starts_with("hover:");
        if key.contains("||") && key.split("||").flat_map(|a| a.split("&&")).any(|t| is_hover(t.trim())) {
            self.problems.push(format!("{at}: \"{key}\": hover can't be in an ||"));
            return None;
        }
        let mut hover = None;
        let mut rest = vec![];
        for term in key.split("&&").map(str::trim) {
            let group = match term {
                "hover" => Some(""),
                t => t.strip_prefix("hover:").map(str::trim),
            };
            match group {
                Some(_) if hover.is_some() => {
                    self.problems.push(format!("{at}: \"{key}\": hover twice"));
                    return None;
                },
                Some(g) => hover = Some(SharedString::from(g.to_owned())),
                None => rest.push(term),
            }
        }
        let when = if rest.is_empty() { None } else { Some(self.cond(&rest.join(" && "), at)?) };
        Some((when, hover))
    }

    /// An icon's name: a bundled one, or `{name}`s the code fills in.
    fn icon(&mut self, name: &str, at: &str) -> Vec<Part> {
        let p = parts(name);
        if let [Part::Lit(n)] = &p[..]
            && !icon_exists(n)
        {
            self.problems.push(format!("{at}: no icon \"{n}\""));
        }
        p
    }

    fn button(&mut self, frame: &str, s: &Spec, at: &str) -> ButtonNode {
        let variant = match s.variant.as_deref() {
            None | Some("ghost") => Variant::Ghost,
            Some("primary") => Variant::Primary,
            Some("secondary") => Variant::Secondary,
            Some("danger") => Variant::Danger,
            Some("outline") => Variant::Outline,
            Some("text") => Variant::Text,
            Some(v) => {
                self.problems.push(format!("{at}.variant: \"{v}\" is not one of ghost, primary, secondary, danger, outline, text"));
                Variant::Ghost
            },
        };
        let size = match s.size.as_ref() {
            None => ButtonSize::Small,
            Some(v) => match v.as_str() {
                Some("xsmall") => ButtonSize::XSmall,
                Some("small") => ButtonSize::Small,
                Some("medium") => ButtonSize::Medium,
                Some("large") => ButtonSize::Large,
                _ => {
                    self.problems.push(format!("{at}.size: a button's size is one of xsmall, small, medium, large"));
                    ButtonSize::Small
                },
            },
        };
        ButtonNode {
            frame: frame.to_owned().into(),
            icon: s.icon.as_deref().map(|i| self.icon(i, &format!("{at}.icon"))),
            label: s.label.as_deref().map(parts),
            tooltip: s.tooltip.as_deref().map(parts),
            variant,
            size,
            selected: self.flag(&s.selected, &format!("{at}.selected")),
            disabled: self.flag(&s.disabled, &format!("{at}.disabled")),
            loading: self.flag(&s.loading, &format!("{at}.loading")),
        }
    }

    fn node(&mut self, s: &Spec, at: &str) -> Node {
        let mut node = match &s.use_ {
            Some(name) => match match s.element.as_deref() {
                Some(e) => match self.element(e, at) {
                    Some(e) => self.read(name, Some(e.name)).map(|mut n| {
                        n.element = e.name;
                        Arc::new(n)
                    }),
                    None => self.template(name),
                },
                None => self.template(name),
            } {
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
            node.kind = s.kind.clone().or(node.kind);
            node.handle |= s.handle;
            if s.drag.is_some() {
                node.drag = self.flag(&s.drag, &format!("{at}.drag"));
            }
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

    /// `true`, `false` or a condition; `false` is a condition that never holds.
    fn flag(&mut self, v: &Option<serde_json::Value>, at: &str) -> Option<Cond> {
        match v {
            None => None,
            Some(serde_json::Value::Bool(b)) => Some(Cond(if *b { vec![vec![]] } else { vec![] })),
            Some(serde_json::Value::String(c)) => self.cond(c, at),
            Some(_) => {
                self.problems.push(format!("{at}: true, false or a condition"));
                None
            },
        }
    }

    fn panel(&mut self, s: &Spec, at: &str) -> Panel {
        let fixed = self.flag(&s.fixed, &format!("{at}.fixed")).filter(|c| !c.0.is_empty());
        // A button's `size` is its own.
        let size = if s.button.is_some() { None } else { self.len(&s.size, &format!("{at}.size")) };
        Panel { size, min: self.len(&s.min, &format!("{at}.min")), max: self.len(&s.max, &format!("{at}.max")), fixed }
    }

    fn own(&mut self, s: &Spec, at: &str) -> Node {
        let icon = s.icon.is_some() && s.button.is_none();
        let kinds = [s.slot.is_some(), s.part.is_some(), s.text.is_some(), s.each.is_some(), s.use_.is_some(), icon, s.button.is_some()];
        if kinds.iter().filter(|k| **k).count() > 1 {
            self.problems.push(format!("{at}: a node is one of slot, part, text, each, use, icon and button"));
        }
        if s.button.is_none() {
            for (field, set) in [
                ("label", s.label.is_some()),
                ("tooltip", s.tooltip.is_some()),
                ("variant", s.variant.is_some()),
                ("selected", s.selected.is_some()),
                ("disabled", s.disabled.is_some()),
                ("loading", s.loading.is_some()),
            ] {
                if set {
                    self.problems.push(format!("{at}.{field}: only a button takes it"));
                }
            }
        }
        let element = match &s.element {
            Some(name) => self.element(name, at),
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
        } else if let Some(part) = &s.part {
            match PARTS.iter().find(|n| **n == part) {
                Some(n) => What::Part(n),
                None => {
                    self.problems.push(format!("{at}.part: no part \"{part}\" (there are: {})", PARTS.join(", ")));
                    What::Box
                },
            }
        } else if let Some(text) = &s.text {
            What::Text(parts(text))
        } else if let Some(frame) = &s.button {
            What::Button(Arc::new(self.button(frame, s, at)))
        } else if let Some(name) = &s.icon {
            What::Icon(self.icon(name, &format!("{at}.icon")))
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
        let (style, holes) = self.style(element, &s.style, &format!("{at}.style"));
        let mut states = vec![];
        for (key, props) in &s.states {
            let Some((when, hover)) = self.state_key(key, &format!("{at}.states")) else { continue };
            let (props, holes) = self.style(element, props, &format!("{at}.states.{key}"));
            states.push(State { when, hover, props, holes });
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
        let dock = self.dock(s, at, split, &children);
        Node {
            what,
            path: at.into(),
            element: element.map_or("", |e| e.name),
            style: Arc::new(style),
            holes,
            states,
            group: s.group.clone().map(Into::into),
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
            dock,
            handle: s.handle,
            drag: self.flag(&s.drag, &format!("{at}.drag")),
            kind: s.kind.clone(),
            children,
        }
    }

    /// The dock `s` declares, if it does; its default parts are its children.
    fn dock(&mut self, s: &Spec, at: &str, split: Option<Axis>, children: &[Node]) -> Option<Arc<DockNode>> {
        let name = s.dock.as_ref()?;
        let dir = match split {
            Some(Axis::Horizontal) => Dir::Row,
            Some(Axis::Vertical) => Dir::Column,
            None => {
                if s.split.is_none() {
                    self.problems.push(format!("{at}.dock: a dock needs a \"split\""));
                }
                Dir::Column
            },
        };
        if s.accepts.is_empty() {
            self.problems.push(format!("{at}.accepts: a dock needs at least one part name or kind"));
        }
        let mut zones = vec![];
        for z in &s.zones {
            match ZONES.iter().find(|n| **n == z) {
                Some(n) => zones.push(*n),
                None => self.problems.push(format!("{at}.zones: \"{z}\" is not one of {}", ZONES.join(", "))),
            }
        }
        let empty_keep = match s.empty.as_deref() {
            None | Some("hide") => false,
            Some("keep") => true,
            Some(e) => {
                self.problems.push(format!("{at}.empty: \"{e}\" is not one of hide, keep"));
                false
            },
        };
        let strip = match s.strip.as_deref() {
            None => {
                self.problems.push(format!("{at}: dock \"{name}\" has no strip"));
                None
            },
            Some(v @ ("left" | "right")) => Some(v.to_owned().into()),
            Some(v) => {
                self.problems.push(format!("{at}.strip: \"{v}\" is not one of left, right"));
                None
            },
        };
        for c in children.iter().filter(|c| !matches!(c.what, What::Part(_))) {
            self.problems.push(format!("{}: a dock holds only parts", c.path));
        }
        let defaults = dock_defaults(dir, children);
        let info = DockInfo { name: name.clone(), dir, accepts: s.accepts.clone(), defaults };
        Some(Arc::new(DockNode {
            name: name.clone().into(),
            dir,
            zones,
            empty_keep,
            strip,
            part_size: self.len(&s.part_size, &format!("{at}.part_size")),
            info,
        }))
    }
}

/// What a dock starts with: its `part` children, each once. A part's own
/// `fixed` and `size` give its default size; a size that isn't in pixels
/// leaves it to share what's left. A dock without parts has an empty split,
/// which `prune` reads as nothing.
fn dock_defaults(dir: Dir, children: &[Node]) -> Tree {
    let mut names = BTreeSet::new();
    let mut items: Vec<(Tree, Size)> = children
        .iter()
        .filter_map(|c| {
            let What::Part(p) = c.what else { return None };
            if !names.insert(p) {
                return None;
            }
            let fixed = c.panel.fixed.is_some();
            let size = match c.panel.size {
                Some(AbsoluteLength::Pixels(px)) if fixed => Size::Px(f32::from(px)),
                _ => Size::Auto,
            };
            Some((Tree::Part(p.to_owned()), size))
        })
        .collect();
    match items.len() {
        1 => items.remove(0).0,
        _ => Tree::Split(dir, items),
    }
}

/// Where the bundled icon `name` is.
fn icon_path(name: &str) -> SharedString {
    format!("icons/{name}.svg").into()
}

/// Whether `name` is a bundled icon.
fn icon_exists(name: &str) -> bool {
    use gpui_kit::assets::IconName;
    static PATHS: std::sync::LazyLock<BTreeSet<SharedString>> =
        std::sync::LazyLock::new(|| IconName::ALL.iter().map(|i| i.path()).collect());
    PATHS.contains(&icon_path(name))
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
        assert!(t.nodes.contains_key("window") && t.nodes.contains_key("pane"));
    }

    #[test]
    fn builtin_has_side_docks() {
        let (t, problems) = compiled(super::BUILTIN);
        assert_eq!(problems, Vec::<String>::new());
        let docks = t.docks("workspace");
        let names: Vec<&str> = docks.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["left", "right"]);
        let a = crate::arrange::Arrangement::defaults("workspace/left+right", &docks);
        assert_eq!(a.encode(), "left=[c explorer@- scratches@220px];right=changes");
        assert_eq!(crate::arrange::Arrangement::decode(&a.key, &a.encode()), Some(a));
        assert_eq!(t.dock_strip("workspace", "right").as_deref(), Some("right"));
    }

    #[test]
    fn panel_header_template_compiles() {
        let (t, problems) = compiled(super::BUILTIN);
        assert_eq!(problems, Vec::<String>::new());
        assert!(t.is_handle("panel_header"));
        // Each panel's use site gives the header its element, and `$element` in its style.
        for name in ["explorer.header", "changes.header", "scratches.header"] {
            let n = &t.nodes[name];
            assert!(t.is_handle(name), "{name}");
            assert_eq!(n.element, name);
            assert!(n.children[0].style.contains_key("gap"), "{name}");
        }
    }

    #[test]
    fn mistakes_are_reported() {
        let over = style::Overrides::default();
        let theme = gpui_kit::component::ThemeColor::default();
        let props = Props::new(&over, &theme, std::path::Path::new(""));
        let specs: BTreeMap<String, Spec> = serde_json::from_str(
            r#"{"a": {"slot": "nope", "children": [{"use": "b"}, {"use": "a"}, {"layout": "diagonal"}, {"element": "x"},
                {"part": "nope"}, {"dock": "d", "split": "row", "zones": ["middle"], "strip": "left"}, {"dock": "d2", "strip": "right"},
                {"dock": "d3", "split": "row", "accepts": ["panel"], "strip": "left",
                    "children": [{"children": [{"part": "explorer"}]}, {"slot": "center"}, {"part": "changes"}]}]}}"#,
        )
        .unwrap();
        let mut problems = vec![];
        compile(&specs, &props, &mut problems);
        assert_eq!(problems.len(), 12, "{problems:?}");
        assert!(problems.iter().any(|p| p.ends_with("children.7.children.0: a dock holds only parts")), "{problems:?}");
        assert!(problems.iter().any(|p| p.ends_with("children.7.children.1: a dock holds only parts")), "{problems:?}");
    }

    #[test]
    fn drag_reads_the_docks_values() {
        let (t, problems) = compiled(
            r#"{"w": {"dock": "d", "split": "row", "accepts": ["panel"], "strip": "left",
                "children": [{"part": "explorer", "drag": "explorer && !busy"}, {"part": "changes"}]}}"#,
        );
        assert_eq!(problems, Vec::<String>::new());
        let scope = || super::Scope::new("w").var("explorer", true);
        assert!(t.pinned(&scope()).is_empty());
        assert_eq!(t.pinned(&scope().var("busy", true)), ["explorer"].into());
        // The header's own values (`part`, `title`) don't count.
        assert_eq!(t.pinned(&super::Scope::new("h").var("part", "explorer")), ["explorer"].into());
    }

    #[test]
    fn dock_without_strip_is_reported() {
        let (_, problems) = compiled(r#"{"w": {"dock": "d", "split": "row", "accepts": ["panel"], "children": [{"part": "explorer"}]}}"#);
        assert_eq!(problems, ["templates.w: dock \"d\" has no strip".to_string()]);
    }

    #[test]
    fn an_empty_dock_round_trips() {
        let (t, problems) = compiled(r#"{"w": {"dock": "d", "split": "row", "accepts": ["panel"], "strip": "left"}}"#);
        assert_eq!(problems, Vec::<String>::new());
        let a = crate::arrange::Arrangement::defaults("w/d", &t.docks("w"));
        assert_eq!(a.encode(), "d=");
        assert_eq!(crate::arrange::Arrangement::decode(&a.key, &a.encode()), Some(a));
    }

    fn compiled(json: &str) -> (super::Templates, Vec<String>) {
        let over = style::Overrides::default();
        let theme = gpui_kit::component::ThemeColor::default();
        let props = Props::new(&over, &theme, std::path::Path::new(""));
        let specs: BTreeMap<String, Spec> = serde_json::from_str(json).unwrap();
        let mut problems = vec![];
        let t = compile(&specs, &props, &mut problems);
        (t, problems)
    }

    #[test]
    fn docks_and_parts_compile() {
        let (t, problems) = compiled(
            r#"{"w": {"split": "row", "children": [{"dock": "left", "split": "column", "accepts": ["panel"],
                "zones": ["before", "after"], "strip": "left",
                "children": [{"part": "explorer"}, {"part": "scratches"}]}]}}"#,
        );
        assert_eq!(problems, Vec::<String>::new());
        let docks = t.docks("w");
        assert_eq!(docks.len(), 1);
        assert_eq!(docks[0].name, "left");
        assert_eq!(docks[0].accepts, ["panel"]);
        let a = crate::arrange::Arrangement::defaults("w/left", &docks);
        assert_eq!(a.encode(), "left=[c explorer@- scratches@-]");
        assert!(t.parts().contains(&("explorer", "panel".to_string())));
        assert!(t.part("scratches").is_some() && t.part("changes").is_none());
    }

    #[test]
    fn fixed_part_size_is_a_default() {
        let (t, problems) = compiled(
            r#"{"w": {"dock": "left", "split": "column", "accepts": ["panel"], "strip": "left",
                "children": [{"part": "explorer"}, {"part": "scratches", "fixed": true, "size": 220}]}}"#,
        );
        assert_eq!(problems, Vec::<String>::new());
        let a = crate::arrange::Arrangement::defaults("w/left", &t.docks("w"));
        assert_eq!(a.encode(), "left=[c explorer@- scratches@220px]");
    }

    #[test]
    fn part_placed_twice_is_reported() {
        let (t, problems) = compiled(
            r#"{"w": {"children": [
                {"dock": "a", "split": "column", "accepts": ["panel"], "strip": "left", "children": [{"part": "explorer"}]},
                {"dock": "b", "split": "column", "accepts": ["panel"], "strip": "right", "children": [{"part": "explorer"}]}]}}"#,
        );
        assert_eq!(problems.iter().filter(|p| p.contains("placed twice")).count(), 1, "{problems:?}");
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(t.parts().iter().filter(|(n, _)| *n == "explorer").count(), 1);
        let docks = t.docks("w");
        assert_eq!(crate::arrange::Arrangement::defaults("w/a+b", &docks).encode(), "a=explorer;b=");
        assert!(!docks[1].defaults.has("explorer"));

        // Twice in one dock: the dock starts with it once.
        let (t, problems) = compiled(
            r#"{"w": {"dock": "a", "split": "column", "accepts": ["panel"], "strip": "left",
                "children": [{"part": "explorer"}, {"part": "changes"}, {"part": "explorer"}]}}"#,
        );
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(crate::arrange::Arrangement::defaults("w/a", &t.docks("w")).encode(), "a=[c explorer@- changes@-]");
    }

    #[test]
    fn part_and_dock_used_twice_are_reported() {
        let (t, problems) = compiled(
            r#"{"w": {"children": [{"use": "p"}, {"use": "p"}]},
                "p": {"children": [{"part": "explorer"},
                    {"dock": "d", "split": "column", "accepts": ["panel"], "strip": "left", "children": [{"part": "changes"}]}]}}"#,
        );
        assert_eq!(problems.iter().filter(|p| p.contains("part \"explorer\" placed twice")).count(), 1, "{problems:?}");
        assert_eq!(problems.iter().filter(|p| p.contains("part \"changes\" placed twice")).count(), 1, "{problems:?}");
        assert_eq!(problems.iter().filter(|p| p.contains("dock \"d\" placed twice")).count(), 1, "{problems:?}");
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert_eq!(t.docks("w").len(), 1);
        assert_eq!(t.parts().len(), 2);
    }

    #[test]
    fn part_keeps_its_use_site_settings() {
        let (t, problems) = compiled(
            r#"{"w": {"children": [{"use": "sc", "fixed": true, "size": 220, "min": 90, "kind": "tool"}]},
                "sc": {"part": "scratches"}}"#,
        );
        assert_eq!(problems, Vec::<String>::new());
        let n = t.part("scratches").unwrap();
        assert!(n.panel.fixed.is_some() && n.panel.size.is_some() && n.panel.min.is_some());
        assert_eq!(t.parts(), [("scratches", "tool".to_string())]);
    }

    #[test]
    fn icons_and_buttons_compile() {
        let (t, problems) = compiled(
            r#"{"w": {"children": [
                {"icon": "plus"}, {"icon": "{icon}"},
                {"button": "go", "icon": "close", "label": "Go {n}", "tooltip": "Go", "size": "xsmall", "variant": "primary",
                    "selected": "on", "disabled": false, "loading": "busy"}]}}"#,
        );
        assert_eq!(problems, Vec::<String>::new());
        let kids = &t.nodes["w"].children;
        assert!(matches!(kids[0].what, super::What::Icon(_)));
        let super::What::Button(b) = &kids[2].what else { panic!("not a button") };
        assert_eq!(b.frame, "go");
        assert!(matches!(b.size, super::ButtonSize::XSmall) && matches!(b.variant, super::Variant::Primary));
        assert!(b.loading.is_some());
        // A button's size is no panel size.
        assert!(kids[2].panel.size.is_none());
    }

    #[test]
    fn any_box_takes_a_background_image() {
        let (_, problems) = compiled(
            r#"{"w": {"style": {"background_image": "$welcome.background_image", "background_image_fit": "contain",
                "background_image_opacity": 0.5}}}"#,
        );
        assert_eq!(problems, Vec::<String>::new());
    }

    #[test]
    fn icon_and_button_mistakes_are_reported() {
        let (_, problems) = compiled(
            r#"{"w": {"children": [{"icon": "no-such-icon"}, {"label": "x"}, {"button": "b", "size": 12, "variant": "loud"},
                {"icon": "plus", "text": "x"}]}}"#,
        );
        assert_eq!(problems.len(), 5, "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("no icon \"no-such-icon\"")), "{problems:?}");
        assert!(problems.iter().any(|p| p.contains("only a button takes it")), "{problems:?}");
    }

    #[test]
    fn values_fill_style_holes() {
        let (t, problems) = compiled(
            r#"{"w": {"element": "pane_tab_bar.tab", "style": {"background": "{color}", "height": "{h}"},
                "states": {"active": {"element": "pane_tab_bar.tab_active"}, "hover && !active": {"text": "{fg}"},
                    "hover:row": {"opacity": 1}}}}"#,
        );
        assert_eq!(problems, Vec::<String>::new());
        let n = &t.nodes["w"];
        assert_eq!(n.holes.len(), 2);
        assert!(!n.style.contains_key("background"));
        let active = n.states.iter().find(|s| s.hover.is_none()).unwrap();
        // The element's set properties, as values.
        assert!(active.props.contains_key("background") && active.props.contains_key("border"), "{:?}", active.props);
        let hover = n.states.iter().find(|s| s.hover.as_deref() == Some("")).unwrap();
        assert!(hover.when.is_some() && hover.holes.len() == 1);
        assert!(n.states.iter().any(|s| s.hover.as_deref() == Some("row") && s.when.is_none()));
        assert_eq!(super::Value::Num(3.).val(style::Kind::Len), Some(style::Val::Len(gpui_kit::px(3.).into())));
        assert_eq!(super::Value::Text("x".into()).val(style::Kind::Color), None);
    }

    #[test]
    fn hover_in_an_or_is_reported() {
        let (_, problems) = compiled(r#"{"w": {"states": {"hover || a": {"opacity": 1}, "hover && hover:g": {"opacity": 1}}}}"#);
        assert_eq!(problems.len(), 2, "{problems:?}");
    }

    #[test]
    fn dock_without_accepts_is_reported() {
        let (_, problems) = compiled(r#"{"w": {"dock": "d", "split": "row", "strip": "left"}}"#);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("accepts"));
    }
}
