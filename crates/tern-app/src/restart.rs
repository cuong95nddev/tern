//! Restart Background Terminals: end the daemon (and every program in it)
//! so a new one, of this version, runs them. An update otherwise keeps the
//! old daemon until its last terminal ends, and a daemon can also get stuck.
//! At launch an outdated daemon that still runs programs hands them to a new
//! one where it can (Unix); otherwise it, or one of an older protocol (whose
//! terminals no window can show), is offered for a restart. Restarted tabs
//! come back in their folders, below what they showed (see `TerminalView`).

use std::time::{Duration, Instant};

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Sizable, WindowExt};
use gpui_kit::*;

use tern_mux::client::{Connector, OtherDaemon};

use crate::AppServices;

actions!(tern, [RestartBackground]);

/// When the daemon was last ended on purpose. Terminals that lose it soon
/// after start their programs afresh instead of asking for Enter.
pub struct Restarted(Instant);

impl Global for Restarted {}

/// How long after a restart a lost terminal counts as part of it.
const WINDOW: Duration = Duration::from_secs(15);

/// Whether the daemon was just restarted on purpose.
pub fn just_restarted(cx: &App) -> bool {
    cx.try_global::<Restarted>().is_some_and(|r| r.0.elapsed() < WINDOW)
}

/// Whether a program started at `started` just ended because the daemon
/// was restarted on purpose (the old daemon reports its programs' exits as
/// it goes): its terminal starts it again on the new daemon.
pub fn ended_by_restart(started: Instant, cx: &App) -> bool {
    cx.try_global::<Restarted>().is_some_and(|r| killed_by(r.0, started))
}

fn killed_by(restarted: Instant, started: Instant) -> bool {
    started < restarted && restarted.elapsed() < WINDOW
}

/// How many terminals run in the background (in all windows).
pub fn running(cx: &App) -> usize {
    cx.global::<AppServices>().0.mux.existing().and_then(|c| c.list().ok()).map_or(0, |l| l.len())
}

/// What a restart would end, worked out off the UI thread.
#[derive(Clone)]
struct Outlook {
    /// Terminals of this protocol's daemon (ended ones too).
    count: usize,
    /// Of those, still running.
    live: usize,
    /// The daemon's version, when it is another build than this app.
    outdated: Option<String>,
    others: Vec<OtherDaemon>,
}

fn outlook(mux: &Connector) -> Outlook {
    let client = mux.existing();
    let list = client.as_ref().and_then(|c| c.list().ok()).unwrap_or_default();
    Outlook {
        count: list.len(),
        live: list.iter().filter(|s| !s.exited).count(),
        outdated: client.filter(|c| c.server_outdated()).map(|c| c.server_version().to_owned()),
        others: mux.other_daemons(),
    }
}

/// Ask, then restart.
pub fn confirm(window: &mut Window, cx: &mut App) {
    let mux = cx.global::<AppServices>().0.mux.clone();
    let task = cx.background_spawn(async move { outlook(&mux) });
    let window = window.window_handle();
    cx.spawn(async move |cx| {
        let o = task.await;
        let _ = window.update(cx, |_, window, cx| open(&o, false, window, cx));
    })
    .detach();
}

/// At launch: move the terminals of a background server that runs an older
/// Tern to one of this build (macOS and Linux, from servers that know how);
/// else offer a restart, as its terminals would otherwise keep it for good.
pub fn check_at_launch(cx: &mut App) {
    let mux = cx.global::<AppServices>().0.mux.clone();
    let task = cx.background_spawn(async move {
        #[allow(unused_mut)]
        let mut o = outlook(&mux);
        // No program to lose: the next connection replaces it quietly.
        if o.outdated.is_some() && o.live == 0 {
            let _ = mux.get();
        }
        // Their programs keep running; the tabs reattach.
        #[cfg(unix)]
        if o.outdated.is_some() && o.live > 0 {
            match mux.hand_off() {
                Ok(()) => o.outdated = None,
                Err(e) => log::warn!("can't move the background terminals to this build: {e}"),
            }
        }
        o
    });
    cx.spawn(async move |cx| {
        let o = task.await;
        if (o.outdated.is_none() || o.live == 0) && o.others.is_empty() {
            return;
        }
        cx.update(|cx| {
            let Some(window) = cx.active_window().or_else(|| cx.windows().first().copied()) else { return };
            let _ = window.update(cx, |_, window, cx| open(&o, true, window, cx));
        });
    })
    .detach();
}

fn open(o: &Outlook, at_launch: bool, window: &mut Window, cx: &mut App) {
    let what = match o.count {
        0 => "No terminals run in the background.".to_owned(),
        1 => "1 terminal runs in the background.".to_owned(),
        n => format!("{n} terminals run in the background."),
    };
    let ours = env!("CARGO_PKG_VERSION");
    let server = match &o.outdated {
        Some(v) if v != ours => format!(" They run on Tern {v}; a restart moves them to Tern {ours}."),
        Some(_) => " They run on an older build of Tern; a restart moves them to this one.".to_owned(),
        None => String::new(),
    };
    let others = match o.others.as_slice() {
        [] => String::new(),
        [d] => format!(" An older Tern ({}) also still runs terminals that no window can show; they end too.", d.version),
        ds => format!(" {} older Tern background servers also still run terminals that no window can show; they end too.", ds.len()),
    };
    let text = format!(
        "{what}{server}{others} Restarting ends every program in them, in all windows. The tabs start again \
         in the folders they were in, and Claude Code conversations resume."
    );
    let (title, later) =
        if at_launch { ("Background Terminals Run an Older Tern", "Later") } else { ("Restart Background Terminals", "Cancel") };
    window.open_dialog(cx, move |dialog, _, cx| {
        let text = text.clone();
        dialog
            .w(crate::style::dialog_width(true, cx))
            .title(title)
            .content(move |c, _, cx| {
                c.child(div().whitespace_normal().child(text.clone())).child(
                    div().pt_2().text_xs().text_color(cx.theme().muted_foreground).child("Unsaved work in terminal programs is lost."),
                )
            })
            .footer(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(Button::new("restart-cancel").small().label(later).on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(Button::new("restart-ok").small().danger().label("Restart").on_click(|_, window, cx| {
                        window.close_dialog(cx);
                        restart(cx);
                    })),
            )
    });
}

/// End the daemon and those of older protocols, with every program in them.
pub fn stop_all(mux: &Connector) -> std::io::Result<()> {
    for d in mux.other_daemons() {
        if let Err(e) = d.stop() {
            log::warn!("can't end the Tern {} background server: {e}", d.version);
        }
    }
    mux.stop_server()
}

/// End the daemon; the terminals that lose it start again on a new one.
pub fn restart(cx: &mut App) {
    let mux = cx.global::<AppServices>().0.mux.clone();
    cx.set_global(Restarted(Instant::now()));
    // Waits up to a couple of seconds for the daemon to go.
    cx.background_executor()
        .spawn(async move {
            if let Err(e) = stop_all(&mux) {
                log::warn!("can't end the background server: {e}");
            }
        })
        .detach();
}

#[cfg(test)]
mod tests {
    use super::{Duration, Instant, WINDOW, killed_by};

    #[test]
    fn programs_started_before_a_restart_were_killed_by_it() {
        let restarted = Instant::now();
        let Some(before) = restarted.checked_sub(Duration::from_secs(1)) else { return };
        assert!(killed_by(restarted, before));
        // Started on the new daemon: its exit is its own.
        assert!(!killed_by(restarted, restarted + Duration::from_millis(1)));
        // A restart long ago.
        let Some(long_ago) = restarted.checked_sub(WINDOW * 2) else { return };
        let Some(older) = long_ago.checked_sub(Duration::from_secs(1)) else { return };
        assert!(!killed_by(long_ago, older));
    }
}
