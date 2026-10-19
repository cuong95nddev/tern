//! Where captures go: a terminal tab of any window, or a new agent tab, and
//! how they are handed over (pasted, like a file drop, then submitted).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui_kit::*;

use crate::agents::Agent;
use crate::main_window;
use crate::terminal_view::{TerminalView, escape_path};
use crate::workspace::Workspace;

/// Agents offered for a new tab.
pub const NEW_AGENTS: [Agent; 3] = [Agent::Claude, Agent::Codex, Agent::OpenCode];

#[derive(Clone, PartialEq, Eq)]
pub enum Dest {
    Tab { window: AnyWindowHandle, ws: EntityId, tab: EntityId },
    New { window: AnyWindowHandle, ws: EntityId, agent: Agent },
}

#[derive(Clone)]
pub struct Choice {
    pub label: String,
    pub agent: Option<Agent>,
    pub dest: Dest,
}

/// Every terminal tab (frontmost window and workspace first), then a new
/// tab of each agent in the frontmost workspace.
pub fn choices(cx: &mut App) -> Vec<Choice> {
    let all = main_window::workspaces(cx);
    let mut out = Vec::new();
    for (window, ws) in &all {
        let w = ws.read(cx);
        for t in w.term_tabs(cx) {
            out.push(Choice {
                label: format!("{} › {}", t.folder, t.title),
                agent: t.agent,
                dest: Dest::Tab { window: *window, ws: ws.entity_id(), tab: t.id },
            });
        }
    }
    if let Some((window, ws)) = all.first() {
        for agent in NEW_AGENTS {
            out.push(Choice {
                label: format!("New {} tab", agent.name()),
                agent: Some(agent),
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
        .or_else(|| choices.iter().position(|c| matches!(c.dest, Dest::New { agent: Agent::Claude, .. })))
}

/// One step of handing captures to a terminal.
#[derive(Debug, PartialEq)]
pub enum Step {
    /// Paste text (bracketed when the program asked for it).
    Paste(String),
    /// Type raw bytes.
    Type(Vec<u8>),
    Wait(Duration),
}

/// Each image as its own pasted path (agents attach a pasted image path),
/// then the prompt, then Enter.
pub fn plan(paths: &[PathBuf], prompt: &str) -> Vec<Step> {
    let mut steps = Vec::new();
    for p in paths {
        steps.push(Step::Paste(format!("{} ", escape_path(p))));
        steps.push(Step::Wait(Duration::from_millis(150)));
    }
    let prompt = prompt.trim();
    if !prompt.is_empty() {
        steps.push(Step::Paste(prompt.to_owned()));
    }
    steps.push(Step::Wait(Duration::from_millis(300)));
    steps.push(Step::Type(b"\r".to_vec()));
    steps
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
    let fresh = matches!(dest, Dest::New { .. });
    cx.spawn(async move |cx| {
        if fresh {
            wait_ready(&view, cx).await?;
        }
        run(&view, plan(&paths, &prompt), cx).await
    })
}

/// Wait for a new tab's agent to take input: it runs (or a while has
/// passed, for agents not told apart by their process name), has turned on
/// bracketed paste and has gone quiet.
async fn wait_ready(view: &Entity<TerminalView>, cx: &mut AsyncApp) -> Result<(), String> {
    let start = Instant::now();
    loop {
        cx.background_executor().timer(Duration::from_millis(100)).await;
        let ready = cx.update(|cx| {
            let v = view.read(cx);
            (v.agent().is_some() || start.elapsed() > Duration::from_secs(8)) && v.ready_for_paste(Duration::from_millis(600))
        });
        if ready {
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(25) {
            return Err("The agent didn't start in time".into());
        }
    }
}

async fn run(view: &Entity<TerminalView>, steps: Vec<Step>, cx: &mut AsyncApp) -> Result<(), String> {
    for step in steps {
        match step {
            Step::Wait(d) => cx.background_executor().timer(d).await,
            Step::Paste(text) => view.update(cx, |v, cx| v.paste_text(&text, cx)),
            Step::Type(bytes) => view.update(cx, |v, cx| v.write_input(bytes, cx)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Step, plan};

    #[test]
    fn plan_pastes_each_image_then_the_prompt_then_enter() {
        let steps = plan(&["/a b.png".into(), "/c.png".into()], "  fix #1\n");
        // As each platform's shell takes a path with a space.
        let spaced = if cfg!(windows) { "\"/a b.png\" " } else { "/a\\ b.png " };
        assert_eq!(
            steps,
            vec![
                Step::Paste(spaced.into()),
                Step::Wait(Duration::from_millis(150)),
                Step::Paste("/c.png ".into()),
                Step::Wait(Duration::from_millis(150)),
                Step::Paste("fix #1".into()),
                Step::Wait(Duration::from_millis(300)),
                Step::Type(b"\r".to_vec()),
            ]
        );
        assert_eq!(plan(&[], "").len(), 2, "no prompt: just Enter");
    }
}
