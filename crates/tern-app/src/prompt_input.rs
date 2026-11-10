//! Handing a prompt to the agent in a terminal tab as a user would: wait for
//! a new tab's agent to take input, paste (bracketed when it asked for it),
//! then Enter. Screenshots sent to a tab (`shot::targets`) and tasks one
//! agent gives another (`agent_tasks`) both go this way.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui_kit::*;

use crate::terminal_view::{TerminalView, escape_path};

/// One step of handing input to a terminal.
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

/// Wait for a new tab's agent to take input: it runs (or a while has
/// passed, for agents not told apart by their process name), has turned on
/// bracketed paste, has gone quiet, and shows no question of its own (its
/// screen rules tell one, such as an update offer or a folder trust
/// question, which a pasted Enter would answer: the user answers it).
/// `limit` is how long to wait.
pub async fn wait_ready(view: &Entity<TerminalView>, limit: Duration, cx: &mut AsyncApp) -> Result<(), String> {
    let start = Instant::now();
    loop {
        cx.background_executor().timer(Duration::from_millis(100)).await;
        let (settled, asks) = cx.update(|cx| {
            let v = view.read(cx);
            let settled =
                (v.agent().is_some() || start.elapsed() > Duration::from_secs(8)) && v.ready_for_paste(Duration::from_millis(600));
            // Its screen read now: its status, read every so often, could
            // still say it asks just after the user answered.
            (settled, v.screen_asks())
        });
        if settled && !asks {
            return Ok(());
        }
        if start.elapsed() > limit {
            return Err(if asks {
                "The agent still asks the user something in its tab".into()
            } else {
                "The agent didn't start in time".into()
            });
        }
    }
}

/// Carry out `steps` on `view`.
pub async fn run(view: &Entity<TerminalView>, steps: Vec<Step>, cx: &mut AsyncApp) -> Result<(), String> {
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
