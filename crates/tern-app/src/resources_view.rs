//! The Resources card (a click on the system panel opens it): what each
//! workspace's terminals use, the background sessions no tab shows, and
//! Tern itself, as template `resources.card` with a row per line
//! (`resources.row`). Rows sort by name, CPU or memory; a terminal's row
//! shows its tab, closes it or ends its session. The numbers come from the
//! app's one store ([`crate::resources`]).

use std::collections::HashSet;
use std::ops::Range;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::resources::{self, Item, Owner, ResourceStore, Usage};
use crate::style;
use crate::template::{self, Framed};
use crate::workspace::Workspace;

const ROW: &str = "resources.row";

/// The card is done: it showed something elsewhere.
pub struct Done;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Sort {
    Name,
    Cpu,
    Mem,
}

/// A row that opens and closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Fold {
    Workspace(EntityId),
    Background,
    Tern,
}

impl Fold {
    /// Tern's own processes interest few: closed until opened.
    fn starts_open(self) -> bool {
        self != Fold::Tern
    }
}

#[derive(Debug, Clone)]
enum Kind {
    Fold(Fold),
    Term(Owner),
    /// A part of Tern: no actions.
    Part,
}

#[derive(Debug, Clone)]
struct Row {
    depth: usize,
    kind: Kind,
    icon: &'static str,
    label: String,
    /// How many rows it holds (a folder's).
    count: Option<usize>,
    usage: Option<Usage>,
    /// Memory over the last samples (a folder's).
    history: Vec<u64>,
    /// A workspace's row: where it is, to show it.
    workspace: Option<(AnyWindowHandle, WeakEntity<Workspace>)>,
}

impl EventEmitter<Done> for ResourcesView {}

pub struct ResourcesView {
    store: Entity<ResourceStore>,
    sort: Sort,
    /// Name sorts A to Z unless flipped; CPU and memory, most first.
    flipped: bool,
    /// Folds whose open state is the other one than [`Fold::starts_open`].
    toggled: HashSet<Fold>,
    rows: Vec<Row>,
    scroll: UniformListScrollHandle,
    _sub: Subscription,
}

impl ResourcesView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let store = resources::store(cx);
        let sub = cx.observe(&store, |_, _, cx| cx.notify());
        Self {
            store,
            sort: Sort::Mem,
            flipped: false,
            toggled: HashSet::new(),
            rows: Vec::new(),
            scroll: UniformListScrollHandle::new(),
            _sub: sub,
        }
    }

    fn open(&self, f: Fold) -> bool {
        f.starts_open() != self.toggled.contains(&f)
    }

    fn sort_by(&mut self, sort: Sort, cx: &mut Context<Self>) {
        if self.sort == sort {
            self.flipped = !self.flipped;
        } else {
            (self.sort, self.flipped) = (sort, false);
        }
        cx.notify();
    }

    /// `a` before `b` in the order picked; rows with no numbers go last.
    fn order(&self, a: (&str, Option<Usage>), b: (&str, Option<Usage>)) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        let by = match self.sort {
            Sort::Name => {
                let o = natural(a.0, b.0);
                return if self.flipped { o.reverse() } else { o };
            },
            Sort::Cpu => (a.1.map(|u| u.cpu), b.1.map(|u| u.cpu)),
            Sort::Mem => (a.1.map(|u| u.mem as f32), b.1.map(|u| u.mem as f32)),
        };
        match by {
            (Some(x), Some(y)) => {
                let o = y.partial_cmp(&x).unwrap_or(Ordering::Equal);
                if self.flipped { o.reverse() } else { o }
            },
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
        .then_with(|| natural(a.0, b.0))
    }

    /// The rows to draw, from the last sample.
    fn build(&self, cx: &App) -> Vec<Row> {
        let store = self.store.read(cx);
        let Some(snap) = &store.snapshot else { return Vec::new() };
        let mut rows = Vec::new();
        let items = |rows: &mut Vec<Row>, list: &[Item]| {
            let mut list: Vec<&Item> = list.iter().collect();
            list.sort_by(|a, b| self.order((&a.label, a.usage), (&b.label, b.usage)));
            rows.extend(list.into_iter().map(|i| Row {
                depth: 1,
                kind: Kind::Term(i.owner.clone()),
                icon: if i.agent { "bot" } else { "square-terminal" },
                label: i.label.clone(),
                count: None,
                usage: i.usage,
                history: Vec::new(),
                workspace: None,
            }));
        };
        let mut groups: Vec<_> = snap.groups.iter().collect();
        groups.sort_by(|a, b| self.order((&a.name, Some(a.usage)), (&b.name, Some(b.usage))));
        for g in groups {
            let fold = Fold::Workspace(g.id);
            rows.push(Row {
                depth: 0,
                kind: Kind::Fold(fold),
                icon: "folder",
                label: g.name.clone(),
                count: Some(g.items.len()),
                usage: Some(g.usage),
                history: store.history(Some(g.id)).to_vec(),
                workspace: Some((g.window, g.ws.clone())),
            });
            if self.open(fold) {
                items(&mut rows, &g.items);
            }
        }
        if !snap.background.is_empty() {
            let mut usage = Usage::default();
            for i in &snap.background {
                usage += i.usage.unwrap_or_default();
            }
            rows.push(Row {
                depth: 0,
                kind: Kind::Fold(Fold::Background),
                icon: "server",
                label: "Background Sessions".into(),
                count: Some(snap.background.len()),
                usage: Some(usage),
                history: Vec::new(),
                workspace: None,
            });
            if self.open(Fold::Background) {
                items(&mut rows, &snap.background);
            }
        }
        let mut tern = snap.app;
        tern += snap.daemon;
        rows.push(Row {
            depth: 0,
            kind: Kind::Fold(Fold::Tern),
            icon: "app-window",
            label: "Tern".into(),
            count: None,
            usage: Some(tern),
            history: store.history(None).to_vec(),
            workspace: None,
        });
        if self.open(Fold::Tern) {
            for (label, usage) in [("App", snap.app), ("Background server", snap.daemon)] {
                rows.push(Row {
                    depth: 1,
                    kind: Kind::Part,
                    icon: "app-window",
                    label: label.into(),
                    count: None,
                    usage: Some(usage),
                    history: Vec::new(),
                    workspace: None,
                });
            }
        }
        rows
    }

    fn click(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(ix) else { return };
        match row.kind.clone() {
            Kind::Fold(f) => {
                if !self.toggled.remove(&f) {
                    self.toggled.insert(f);
                }
                cx.notify();
            },
            Kind::Term(owner) => {
                cx.emit(Done);
                show(owner, cx);
            },
            Kind::Part => {},
        }
    }

    fn render_rows(&mut self, range: Range<usize>, _: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        range
            .filter_map(|ix| {
                let row = self.rows.get(ix)?;
                let (folder, open) = match row.kind {
                    Kind::Fold(f) => (true, self.open(f)),
                    _ => (false, false),
                };
                let (tab, session) = match &row.kind {
                    Kind::Term(Owner::Tab { .. }) => (true, false),
                    Kind::Term(Owner::Session(_)) => (false, true),
                    _ => (false, false),
                };
                let max = row.history.iter().copied().max().unwrap_or(0);
                let spark = row
                    .history
                    .iter()
                    .enumerate()
                    .map(|(i, &m)| {
                        let used = if max == 0 { 0. } else { m as f64 / max as f64 * 100. };
                        template::Scope::new(format!("resources-spark-{ix}-{i}")).var("used", used).var("rest", 100. - used)
                    })
                    .collect();
                let scope = template::Scope::new(format!("resources-row-{ix}"))
                    .var("indent", style::row_indent(ROW, row.depth, cx))
                    .var("folder", folder)
                    .var("open", open)
                    .var("heading", row.depth == 0)
                    .var("icon", row.icon)
                    .var("label", row.label.clone())
                    .var("count", row.count.filter(|&c| c > 0).map(|c| c.to_string()).unwrap_or_default())
                    .var("spark", row.history.len() > 1 && max > 0)
                    .list("spark", spark)
                    .var("cpu", row.usage.map_or_else(|| "—".to_owned(), |u| resources::percent(u.cpu)))
                    .var("mem", row.usage.map_or_else(|| "—".to_owned(), |u| resources::bytes(u.mem)))
                    .var("busy", row.usage.is_some_and(|u| u.cpu >= 80.))
                    .var("show", session || row.workspace.is_some())
                    .var("show_tip", if session { "Show in a Tab" } else { "Show Workspace" })
                    .var("close", tab)
                    .var("end", session);
                let owner = match &row.kind {
                    Kind::Term(o) => Some(o.clone()),
                    _ => None,
                };
                let label = row.label.clone();
                let workspace = row.workspace.clone();
                Some(template::render(
                    ROW,
                    scope,
                    &mut |name, _, f| match (name, f) {
                        ("row", Framed::Box(d)) => {
                            d.id(ix).on_click(cx.listener(move |this, _, _, cx| this.click(ix, cx))).into_any_element()
                        },
                        ("show", Framed::Button(b)) => {
                            let (owner, workspace) = (owner.clone(), workspace.clone());
                            b.on_click(cx.listener(move |_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(Done);
                                if let Some(o) = owner.clone() {
                                    show(o, cx);
                                } else if let Some((handle, ws)) = workspace.clone().and_then(|(h, w)| Some((h, w.upgrade()?))) {
                                    cx.defer(move |cx| crate::main_window::reveal(handle, &ws, cx));
                                }
                            }))
                            .into_any_element()
                        },
                        ("close", Framed::Button(b)) => {
                            let owner = owner.clone();
                            b.on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                if let Some(Owner::Tab { window, ws, tab }) = owner.clone() {
                                    close_tab(window, ws, tab, cx);
                                }
                            })
                            .into_any_element()
                        },
                        ("end", Framed::Button(b)) => {
                            let (owner, label) = (owner.clone(), label.clone());
                            b.on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                if let Some(Owner::Session(id)) = owner {
                                    this.end_sessions(vec![id], &format!("End \u{201c}{label}\u{201d}?"), window, cx);
                                }
                            }))
                            .into_any_element()
                        },
                        (_, f) => f.into_any_element(),
                    },
                    cx,
                ))
            })
            .collect()
    }

    /// End sessions `ids`, once the user says so.
    fn end_sessions(&mut self, ids: Vec<u64>, question: &str, window: &mut Window, cx: &mut Context<Self>) {
        let detail = "Every program in them ends. Unsaved work in them is lost.";
        let answer = window.prompt(PromptLevel::Warning, question, Some(detail), &["End", "Cancel"], cx);
        cx.spawn(async move |_, cx| {
            if answer.await != Ok(0) {
                return;
            }
            cx.update(|cx| {
                let mux = cx.global::<crate::AppServices>().0.mux.clone();
                cx.background_spawn(async move {
                    if let Some(c) = mux.existing() {
                        for id in ids {
                            c.close(id);
                        }
                    }
                })
                .detach();
            });
        })
        .detach();
    }
}

/// Show what `owner` is: its tab (in its window and workspace), or a tab
/// for a background session. Once the click is handled: the window it
/// came from may be the tab's.
fn show(owner: Owner, cx: &mut App) {
    cx.defer(move |cx| match owner {
        Owner::Tab { window: handle, ws, tab } => {
            let Some(ws) = ws.upgrade() else { return };
            crate::main_window::reveal(handle, &ws, cx);
            let _ = handle.update(cx, |_, window, cx| ws.update(cx, |w, cx| w.show_tab(tab, window, cx)));
        },
        Owner::Session(id) => crate::main_window::reveal_session(id, cx),
    });
}

/// Close tab `tab` of `ws`, asking first when that ends a running program
/// (once the click is handled, as [`show`]).
fn close_tab(handle: AnyWindowHandle, ws: WeakEntity<Workspace>, tab: EntityId, cx: &mut App) {
    cx.defer(move |cx| {
        let Some(ws) = ws.upgrade() else { return };
        let _ = handle.update(cx, |_, window, cx| ws.update(cx, |w, cx| w.request_close_id(tab, window, cx)));
    });
}

/// Names in the order people read them: case aside, `tern 2` before `tern 10`.
fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    let key = |s: &str| {
        let mut out: Vec<(String, u64)> = Vec::new();
        let mut text = String::new();
        let mut num: Option<u64> = None;
        for c in s.chars() {
            match (c.to_digit(10), num) {
                (Some(d), n) => num = Some(n.unwrap_or(0).saturating_mul(10).saturating_add(u64::from(d))),
                (None, Some(n)) => {
                    out.push((std::mem::take(&mut text), n));
                    num = None;
                    text.extend(c.to_lowercase());
                },
                (None, None) => text.extend(c.to_lowercase()),
            }
        }
        out.push((text, num.unwrap_or(0)));
        out
    };
    key(a).cmp(&key(b))
}

impl Render for ResourcesView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Drawn: the dialog is open, so keep sampling.
        self.store.update(cx, |s, _| s.seen());
        self.rows = self.build(cx);
        let snap = self.store.read(cx).snapshot.as_ref();
        let ready = snap.is_some();
        let total = snap.map(|s| s.total).unwrap_or_default();
        let background: Vec<u64> = snap
            .into_iter()
            .flat_map(|s| &s.background)
            .filter_map(|i| match i.owner {
                Owner::Session(id) => Some(id),
                Owner::Tab { .. } => None,
            })
            .collect();
        let sys = crate::system::store(cx);
        let machine = {
            let s = sys.read(cx);
            match (s.cpu.back(), s.mem_total) {
                (Some(c), t) if t > 0 => {
                    format!("Machine {}% · {} / {}", c.round(), crate::system::gigabytes(s.mem_used), crate::system::gigabytes(t))
                },
                _ => String::new(),
            }
        };
        // The machine's numbers sample while the system store is looked at.
        sys.update(cx, |s, _| s.seen());
        let arrow = |sort: Sort| {
            let down = match sort {
                Sort::Name => self.flipped,
                _ => !self.flipped,
            };
            if self.sort != sort {
                ""
            } else if down {
                " ↓"
            } else {
                " ↑"
            }
        };
        let list = uniform_list("resources", self.rows.len(), cx.processor(Self::render_rows)).track_scroll(&self.scroll).size_full();
        // As tall as its rows; past `max_height` it scrolls.
        let height = style::el(ROW, cx).px("height") * self.rows.len() as f32;
        let list = div().id("resources-list").w_full().h(height).child(list).map(|d| style::el("resources.list", cx).apply(d));
        let n = background.len();
        let scope = template::Scope::new("resources")
            .var("ready", ready)
            .var("cpu", resources::percent(total.cpu))
            .var("mem", resources::bytes(total.mem))
            .var("machine", machine)
            .var("name_label", format!("Name{}", arrow(Sort::Name)))
            .var("cpu_label", format!("CPU{}", arrow(Sort::Cpu)))
            .var("mem_label", format!("Memory{}", arrow(Sort::Mem)))
            .var("by_name", self.sort == Sort::Name)
            .var("by_cpu", self.sort == Sort::Cpu)
            .var("by_mem", self.sort == Sort::Mem)
            .var("background", n as f64)
            .var("end_background_label", if n == 1 { "End Background Session".to_owned() } else { format!("End {n} Background Sessions") })
            .slot("list", list);
        template::render(
            "resources.card",
            scope,
            &mut |name, _, f| match (name, f) {
                ("sort_name", Framed::Button(b)) => {
                    b.on_click(cx.listener(|this, _, _, cx| this.sort_by(Sort::Name, cx))).into_any_element()
                },
                ("sort_cpu", Framed::Button(b)) => b.on_click(cx.listener(|this, _, _, cx| this.sort_by(Sort::Cpu, cx))).into_any_element(),
                ("sort_mem", Framed::Button(b)) => b.on_click(cx.listener(|this, _, _, cx| this.sort_by(Sort::Mem, cx))).into_any_element(),
                ("end_background", Framed::Button(b)) => {
                    let ids = background.clone();
                    b.on_click(cx.listener(move |this, _, window, cx| {
                        let q = if ids.len() == 1 {
                            "End the background session?".to_owned()
                        } else {
                            format!("End {} background sessions?", ids.len())
                        };
                        this.end_sessions(ids.clone(), &q, window, cx);
                    }))
                    .into_any_element()
                },
                (_, f) => f.into_any_element(),
            },
            cx,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::natural;
    use std::cmp::Ordering;

    #[test]
    fn names_sort_as_read() {
        assert_eq!(natural("tern 2", "tern 10"), Ordering::Less);
        assert_eq!(natural("Alpha", "beta"), Ordering::Less);
        assert_eq!(natural("b", "B"), Ordering::Equal);
    }
}
