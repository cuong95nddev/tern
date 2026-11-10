//! Where captures go: a terminal tab of any window, or a new agent tab, and
//! how they are handed over (pasted, like a file drop, then submitted: see
//! `prompt_input`).

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::*;

use crate::main_window;
use crate::prompt_input::{plan, run, wait_ready};
use crate::workspace::Workspace;
use tern_agent::AgentId;

/// Agents offered for a new tab.
pub const NEW_AGENTS: [&str; 3] = [tern_agent::CLAUDE, tern_agent::CODEX, tern_agent::OPENCODE];

#[derive(Clone, PartialEq, Eq)]
pub enum Dest {
    Tab { window: AnyWindowHandle, ws: EntityId, tab: EntityId },
    New { window: AnyWindowHandle, ws: EntityId, agent: AgentId },
}

#[derive(Clone)]
pub struct Choice {
    pub label: String,
    /// The workspace's folder, the send menu's first level.
    pub folder: String,
    /// The label under the folder.
    pub item: String,
    pub agent: Option<AgentId>,
    pub dest: Dest,
}

/// For each workspace (frontmost window and workspace first), its terminal
/// tabs, then a new tab of each agent.
pub fn choices(cx: &mut App) -> Vec<Choice> {
    let all = main_window::workspaces(cx);
    let mut out = Vec::new();
    for (window, ws) in &all {
        let w = ws.read(cx);
        let name = w.name();
        for t in w.term_tabs(cx) {
            out.push(Choice {
                label: format!("{name} › {}", t.title),
                folder: name.to_string(),
                item: t.title.to_string(),
                agent: t.agent,
                dest: Dest::Tab { window: *window, ws: ws.entity_id(), tab: t.id },
            });
        }
        for agent in NEW_AGENTS.map(AgentId::new) {
            let agent_name = crate::agents::agent_name(&agent);
            out.push(Choice {
                label: format!("New {agent_name} tab in {name}"),
                folder: name.to_string(),
                item: format!("New {agent_name} tab"),
                agent: Some(agent.clone()),
                dest: Dest::New { window: *window, ws: ws.entity_id(), agent },
            });
        }
    }
    out
}

/// The choice to start with: the shown tab of the frontmost workspace when
/// an agent runs there, else the first agent tab, else a new Claude tab.
pub fn default_choice(choices: &[Choice], cx: &mut App) -> Option<usize> {
    let front = main_window::workspaces(cx).into_iter().next();
    let shown = front.and_then(|(_, ws)| {
        let tabs = ws.read(cx).term_tabs(cx);
        tabs.into_iter().find(|t| t.active && t.agent.is_some()).map(|t| t.id)
    });
    let is_tab = |c: &Choice, id| matches!(c.dest, Dest::Tab { tab, .. } if tab == id);
    shown
        .and_then(|id| choices.iter().position(|c| is_tab(c, id)))
        .or_else(|| choices.iter().position(|c| matches!(c.dest, Dest::Tab { .. }) && c.agent.is_some()))
        .or_else(|| choices.iter().position(|c| matches!(&c.dest, Dest::New { agent, .. } if agent.is(tern_agent::CLAUDE))))
}

fn find_ws(window: AnyWindowHandle, id: EntityId, cx: &mut App) -> Option<Entity<Workspace>> {
    main_window::workspaces(cx).into_iter().find(|(h, ws)| *h == window && ws.entity_id() == id).map(|(_, ws)| ws)
}

/// Show `dest` and hand it `paths` and `prompt`.
pub fn deliver(dest: Dest, paths: Vec<PathBuf>, prompt: String, cx: &mut App) -> Task<Result<(), String>> {
    let (window, ws_id) = match &dest {
        Dest::Tab { window, ws, .. } | Dest::New { window, ws, .. } => (*window, *ws),
    };
    let Some(ws) = find_ws(window, ws_id, cx) else { return Task::ready(Err("That workspace is closed".into())) };
    main_window::reveal(window, &ws, cx);
    let fresh = matches!(dest, Dest::New { .. });
    let view = window
        .update(cx, |_, w, cx| {
            ws.update(cx, |ws, cx| match dest {
                Dest::Tab { tab, .. } => {
                    ws.show_tab(tab, w, cx);
                    ws.term_tabs(cx).into_iter().find(|t| t.id == tab).map(|t| t.view)
                },
                Dest::New { agent, .. } => ws.new_agent_tab(agent, w, cx),
            })
        })
        .ok()
        .flatten();
    let Some(view) = view else { return Task::ready(Err("That tab is closed".into())) };
    cx.spawn(async move |cx| {
        if fresh {
            wait_ready(&view, Duration::from_secs(25), cx).await?;
        }
        run(&view, plan(&paths, &prompt), cx).await
    })
}
