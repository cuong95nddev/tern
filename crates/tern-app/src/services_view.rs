//! Services panel: the dev servers and other long-running commands agents
//! started through Tern's MCP server (`tern --mcp`, see [`crate::mcp`]) in
//! the workspace's folder, with their state and the ports they listen on.
//! Clicking a row opens its terminal in a tab; its buttons open the port in
//! the browser, restart it, and stop it.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui_kit::assets::IconName as AssetIcon;
use gpui_kit::component::ActiveTheme;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tern_mux::client::Connector;
use tern_mux::proto::ServiceSpec;

use crate::style;
use crate::template::{self, Framed, on_event};

/// The element a row is drawn as ([`style`]).
const ROW: &str = "services.row";
/// How often the daemon is asked for the services (and their ports).
const POLL: Duration = Duration::from_secs(2);

/// A service under the workspace's folder.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceRow {
    /// Its session in the daemon.
    pub id: u64,
    pub pid: u32,
    pub spec: ServiceSpec,
    pub exited: bool,
    pub exit_code: Option<i32>,
    /// The TCP ports its processes listen on.
    pub ports: Vec<u16>,
}

#[derive(Debug, Clone)]
pub enum ServicesEvent {
    /// Show its terminal in a tab.
    Open(ServiceRow),
    /// An agent started a service here: the panel shows.
    Started,
    /// The header's close button: hide the panel.
    Hide,
}

pub struct ServicesView {
    rows: Vec<ServiceRow>,
    /// What the agents panel calls the agent of each terminal, by session.
    agents: HashMap<u64, String>,
    /// The session last opened.
    selected: Option<u64>,
    /// The services seen, from the first poll on (`None` before it).
    seen: Option<HashSet<u64>>,
    mux: Arc<Connector>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    _poll: Task<()>,
}

impl EventEmitter<ServicesEvent> for ServicesView {}

impl Focusable for ServicesView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ServicesView {
    pub fn new(root: PathBuf, mux: Arc<Connector>, cx: &mut Context<Self>) -> Self {
        let poll = cx.spawn({
            let mux = mux.clone();
            async move |this, cx| {
                loop {
                    let (mux, root) = (mux.clone(), root.clone());
                    let rows = cx.background_spawn(async move { read_rows(&mux, &root) }).await;
                    if this.update(cx, |this, cx| this.set(rows, cx)).is_err() {
                        break;
                    }
                    cx.background_executor().timer(POLL).await;
                }
            }
        });
        Self {
            rows: Vec::new(),
            agents: HashMap::new(),
            selected: None,
            seen: None,
            mux,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _poll: poll,
        }
    }

    fn set(&mut self, rows: Vec<ServiceRow>, cx: &mut Context<Self>) {
        // Those running when the workspace opened aren't news.
        let seen = self.seen.get_or_insert_with(|| rows.iter().map(|r| r.id).collect());
        let mut started = false;
        for r in rows.iter().filter(|r| !r.exited) {
            started |= seen.insert(r.id);
        }
        if started {
            cx.emit(ServicesEvent::Started);
        }
        // The uptimes count seconds: redrawn every poll while one runs.
        if rows != self.rows || rows.iter().any(|r| !r.exited) {
            self.rows = rows;
            cx.notify();
        }
    }

    /// The names of the agents in the workspace's terminals, by session.
    pub fn set_agents(&mut self, agents: HashMap<u64, String>, cx: &mut Context<Self>) {
        if agents != self.agents {
            self.agents = agents;
            cx.notify();
        }
    }

    /// Whether session `id` runs a service (its tab only lets go of it).
    pub fn is_service(&self, id: u64) -> bool {
        self.rows.iter().any(|r| r.id == id)
    }

    fn click(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(ix) else { return };
        self.selected = Some(row.id);
        cx.emit(ServicesEvent::Open(row.clone()));
        cx.notify();
    }

    /// Stop it (a running one) or forget it (an ended one).
    fn stop(&mut self, id: u64, cx: &mut Context<Self>) {
        let mux = self.mux.clone();
        self.rows.retain(|r| r.id != id);
        cx.notify();
        cx.background_spawn(async move {
            if let Some(c) = mux.existing() {
                c.close(id);
            }
        })
        .detach();
    }

    fn restart(&mut self, row: ServiceRow, cx: &mut Context<Self>) {
        let task = cx.background_spawn(async move { crate::mcp::restart(row.id, row.spec) });
        cx.spawn(async move |_, cx| {
            if let Err(e) = task.await {
                log::warn!("services: restart: {e}");
                cx.update(|cx| crate::workspace::notify_system("Cannot restart the service", &e, cx));
            }
        })
        .detach();
    }

    fn render_rows(&mut self, range: Range<usize>, _: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let indent = style::row_indent(ROW, 0, cx);
        let theme = cx.theme();
        let (running, failed, muted) = (theme.success, theme.danger, theme.muted_foreground);
        range
            .filter_map(|ix| {
                let row = self.rows.get(ix)?.clone();
                let color = match (row.exited, row.exit_code) {
                    (false, _) => running,
                    (true, Some(0)) => muted,
                    (true, _) => failed,
                };
                let state = match (row.exited, row.exit_code) {
                    (false, _) => crate::agents::working_label(uptime(row.spec.started)),
                    (true, Some(code)) => format!("exit {code}"),
                    (true, None) => "ended".to_owned(),
                };
                let port = row.ports.first().copied();
                let mut scope = template::Scope::new(ROW)
                    .var("indent", indent)
                    .var("selected", self.selected == Some(row.id))
                    .var("name", row.spec.name.as_str())
                    .var("command", crate::shells_view::first_line(&row.spec.command))
                    .var("state", state)
                    .var("exited", row.exited)
                    .var("ports", row.ports.iter().map(|p| format!(":{p}")).collect::<Vec<_>>().join(" "))
                    .var("port", port.map_or(String::new(), |p| format!("http://localhost:{p}")))
                    .slot("icon", style::row_icon(ROW, AssetIcon::Server, color, cx));
                if let Some(agent) = row.spec.by.and_then(|by| self.agents.get(&by)) {
                    scope = scope.var("agent", agent.as_str());
                }
                let id = row.id;
                Some(template::render(
                    ROW,
                    scope,
                    &mut |name, _, f| match (name, f) {
                        ("row", Framed::Box(d)) => {
                            d.id(ix).on_click(cx.listener(move |this, _, _, cx| this.click(ix, cx))).into_any_element()
                        },
                        ("open", Framed::Button(b)) => b
                            .on_click(move |_, _, cx| {
                                // The row under it would take the click too.
                                cx.stop_propagation();
                                if let Some(p) = port {
                                    cx.open_url(&format!("http://localhost:{p}"));
                                }
                            })
                            .into_any_element(),
                        ("restart", Framed::Button(b)) => {
                            let row = row.clone();
                            b.on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.restart(row.clone(), cx);
                            }))
                            .into_any_element()
                        },
                        ("stop", Framed::Button(b)) => b
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.stop(id, cx);
                            }))
                            .into_any_element(),
                        (_, f) => f.into_any_element(),
                    },
                    cx,
                ))
            })
            .collect()
    }
}

/// The services under `root`, oldest first, with their ports.
fn read_rows(mux: &Connector, root: &std::path::Path) -> Vec<ServiceRow> {
    let Some(list) = mux.existing().and_then(|c| c.list().ok()) else { return Vec::new() };
    let mut rows: Vec<ServiceRow> = list
        .into_iter()
        .filter_map(|i| {
            let spec = i.service.filter(|s| s.cwd.starts_with(root))?;
            Some(ServiceRow { id: i.id, pid: i.pid, spec, exited: i.exited, exit_code: i.exit_code, ports: Vec::new() })
        })
        .collect();
    let pids: Vec<u32> = rows.iter().filter(|r| !r.exited).map(|r| r.pid).collect();
    let mut ports = tern_mux::services::listening_ports(&pids);
    for r in rows.iter_mut().filter(|r| !r.exited) {
        r.ports = ports.remove(&r.pid).unwrap_or_default();
    }
    rows.sort_by_key(|r| (r.spec.started, r.id));
    rows
}

fn uptime(started: u64) -> Duration {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    Duration::from_secs(now.saturating_sub(started))
}

impl Render for ServicesView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let me = cx.entity().downgrade();
        let running = self.rows.iter().filter(|r| !r.exited).count();
        let header = crate::dock::panel_header(
            "services",
            "services.header",
            "Services",
            Some(running),
            template::Scope::new("services.header.buttons"),
            &mut |name, _, f| match (name, f) {
                ("hide", Framed::Button(b)) => {
                    b.on_click(on_event(&me, |_, _: &ClickEvent, _, cx| cx.emit(ServicesEvent::Hide))).into_any_element()
                },
                (_, f) => f.into_any_element(),
            },
            cx,
        );
        let empty = self.rows.is_empty();
        let list = (!empty).then(|| {
            div()
                .id("service-list")
                .flex_1()
                .min_h_0()
                .child(uniform_list("services", self.rows.len(), cx.processor(Self::render_rows)).track_scroll(&self.scroll).size_full())
                .map(|d| style::el("services.list", cx).apply(d))
        });
        let scope = template::Scope::new("services").var("empty", empty).slot("header", header).slot_opt("list", list);
        let focus = &self.focus;
        template::render(
            "services",
            scope,
            &mut |name, _, f| match (name, f) {
                ("panel", Framed::Box(d)) => d.track_focus(focus).into_any_element(),
                (_, f) => f.into_any_element(),
            },
            cx,
        )
    }
}
