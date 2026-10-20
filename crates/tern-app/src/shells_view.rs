//! Shells panel: the shells the coding agents in the workspace's terminals
//! run commands in, oldest first (see `tern_term::procs::agent_shells`).
//! Clicking a row opens a tab that follows its output.

use std::ops::Range;
use std::time::Duration;

use gpui_kit::assets::IconName as AssetIcon;
use gpui_kit::component::ActiveTheme;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tern_term::procs::AgentShell;

use crate::style;
use crate::template::{self, Framed, on_event};

/// The element a row is drawn as ([`style`]).
const ROW: &str = "shells.row";

/// A shell of the agent in tab `tab`.
#[derive(Debug, Clone)]
pub struct ShellRow {
    pub tab: EntityId,
    /// What the agents panel calls the agent.
    pub agent: String,
    pub shell: AgentShell,
}

#[derive(Debug, Clone)]
pub enum ShellsEvent {
    Open(ShellRow),
    /// The header's close button: hide the panel.
    Hide,
}

pub struct ShellsView {
    /// The shells as of the last [`ShellsView::set`].
    rows: Vec<ShellRow>,
    /// The pid of the shell last opened.
    selected: Option<u32>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    _tick: Task<()>,
}

impl EventEmitter<ShellsEvent> for ShellsView {}

impl Focusable for ShellsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ShellsView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        // The rows' times count seconds.
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let Ok(any) = this.read_with(cx, |this, _| !this.rows.is_empty()) else { break };
                if any && this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        Self { rows: Vec::new(), selected: None, scroll: UniformListScrollHandle::new(), focus: cx.focus_handle(), _tick: tick }
    }

    /// The shells to list now.
    pub fn set(&mut self, rows: Vec<ShellRow>, cx: &mut Context<Self>) {
        let same = rows.len() == self.rows.len() && rows.iter().zip(&self.rows).all(|(a, b)| a.tab == b.tab && a.shell == b.shell);
        if !same {
            self.rows = rows;
            cx.notify();
        }
    }

    fn click(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(ix) else { return };
        self.selected = Some(row.shell.pid);
        cx.emit(ShellsEvent::Open(row.clone()));
        cx.notify();
    }

    fn render_rows(&mut self, range: Range<usize>, _: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let indent = style::row_indent(ROW, 0, cx);
        let muted = cx.theme().muted_foreground;
        // The agent's name tells rows apart when there are several.
        let agents = self.rows.iter().map(|r| r.tab).collect::<std::collections::HashSet<_>>().len();
        range
            .filter_map(|ix| {
                let row = self.rows.get(ix)?;
                let age = crate::agents::working_label(row.shell.started.elapsed().unwrap_or_default());
                let mut scope = template::Scope::new(ROW)
                    .var("indent", indent)
                    .var("selected", self.selected == Some(row.shell.pid))
                    .var("command", first_line(&row.shell.command))
                    .var("age", age)
                    .slot("icon", style::row_icon(ROW, AssetIcon::SquareTerminal, muted, cx));
                if agents > 1 {
                    scope = scope.var("agent", row.agent.as_str());
                }
                Some(template::render(
                    ROW,
                    scope,
                    &mut |name, _, f| match (name, f) {
                        ("row", Framed::Box(d)) => {
                            d.id(ix).on_click(cx.listener(move |this, _, _, cx| this.click(ix, cx))).into_any_element()
                        },
                        (_, f) => f.into_any_element(),
                    },
                    cx,
                ))
            })
            .collect()
    }
}

/// `command`'s first line, marked when it has more.
pub fn first_line(command: &str) -> String {
    let mut lines = command.lines();
    let mut line = lines.next().unwrap_or_default().to_owned();
    if lines.next().is_some() {
        line.push_str(" …");
    }
    line
}

impl Render for ShellsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let me = cx.entity().downgrade();
        let header = crate::dock::panel_header(
            "shells",
            "shells.header",
            "Shells",
            Some(self.rows.len()),
            template::Scope::new("shells.header.buttons"),
            &mut |name, _, f| match (name, f) {
                ("hide", Framed::Button(b)) => {
                    b.on_click(on_event(&me, |_, _: &ClickEvent, _, cx| cx.emit(ShellsEvent::Hide))).into_any_element()
                },
                (_, f) => f.into_any_element(),
            },
            cx,
        );
        let empty = self.rows.is_empty();
        let list = (!empty).then(|| {
            div()
                .id("shell-list")
                .flex_1()
                .min_h_0()
                .child(uniform_list("shells", self.rows.len(), cx.processor(Self::render_rows)).track_scroll(&self.scroll).size_full())
                .map(|d| style::el("shells.list", cx).apply(d))
        });
        let scope = template::Scope::new("shells").var("empty", empty).slot("header", header).slot_opt("list", list);
        let focus = &self.focus;
        template::render(
            "shells",
            scope,
            &mut |name, _, f| match (name, f) {
                ("panel", Framed::Box(d)) => d.track_focus(focus).into_any_element(),
                (_, f) => f.into_any_element(),
            },
            cx,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::first_line;

    #[test]
    fn first_lines() {
        assert_eq!(first_line("npm run dev"), "npm run dev");
        assert_eq!(first_line("cat <<EOF\nx\nEOF"), "cat <<EOF …");
        assert_eq!(first_line(""), "");
    }
}
