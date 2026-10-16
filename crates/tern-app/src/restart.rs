//! Restart Background Terminals: end the daemon (and every program in it)
//! so a new one, of this version, runs them. An update otherwise keeps the
//! old daemon until its last terminal ends, and a daemon can also get stuck.

use std::time::{Duration, Instant};

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Sizable, WindowExt};
use gpui_kit::*;

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

/// How many terminals run in the background (in all windows).
pub fn running(cx: &App) -> usize {
    cx.global::<AppServices>().0.mux.existing().and_then(|c| c.list().ok()).map_or(0, |l| l.len())
}

/// Ask, then restart.
pub fn confirm(window: &mut Window, cx: &mut App) {
    let mux = cx.global::<AppServices>().0.mux.clone();
    let count = running(cx);
    let version = mux.server_version();
    let same_build = mux.server_same_build() != Some(false);
    window.open_dialog(cx, move |dialog, _, cx| {
        let what = match count {
            0 => "No terminals run in the background.".to_owned(),
            1 => "1 terminal runs in the background.".to_owned(),
            n => format!("{n} terminals run in the background."),
        };
        let server = match &version {
            Some(v) if v != env!("CARGO_PKG_VERSION") => {
                format!(" The background server is Tern {v}; a new one will be Tern {}.", env!("CARGO_PKG_VERSION"))
            },
            Some(_) if !same_build => " The background server is an older build of Tern; a new one will be this one.".to_owned(),
            _ => String::new(),
        };
        let text = format!(
            "{what}{server} Restarting ends every program in them, in all windows, and starts the tabs again \
             (a Claude Code conversation can be picked up with /resume)."
        );
        dialog
            .w(crate::style::dialog_width(true, cx))
            .title("Restart Background Terminals")
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
                    .child(Button::new("restart-cancel").small().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(Button::new("restart-ok").small().danger().label("Restart").on_click(|_, window, cx| {
                        window.close_dialog(cx);
                        restart(cx);
                    })),
            )
    });
}

/// End the daemon; the terminals that lose it start again on a new one.
pub fn restart(cx: &mut App) {
    let mux = cx.global::<AppServices>().0.mux.clone();
    cx.set_global(Restarted(Instant::now()));
    // Waits up to a couple of seconds for the daemon to go.
    cx.background_executor()
        .spawn(async move {
            if let Err(e) = mux.stop_server() {
                log::warn!("can't end the background server: {e}");
            }
        })
        .detach();
}
