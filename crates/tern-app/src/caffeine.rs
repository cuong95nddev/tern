//! Keeps the machine and its display awake on request, like the Caffeine
//! menu-bar app (as in herdr-gpui). The title bar's cup turns it on and off;
//! a right click holds it only until no agent is working.
//!
//! The hold is a `keepawake` guard: IOKit power assertions on macOS, the
//! thread execution state on Windows, inhibitors over D-Bus on Linux. Each
//! platform drops the hold when the process exits, so a crash or quit
//! releases it without cleanup here. The state is app-wide, so every
//! window's cup shows the same.
//!
//! One dedicated thread owns the guard: Windows ties the execution state to
//! the thread that set it, and Linux makes blocking D-Bus calls that must
//! stay off the UI thread.

use std::sync::mpsc::{Receiver, SyncSender};

use gpui_kit::component::WindowExt as _;
use gpui_kit::*;

use crate::terminal_view::ActivityStatus;

/// How long a hold lasts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Until the cup is clicked again.
    Indefinite,
    /// Until no agent is working.
    UntilAgentsFinish,
}

/// What the cup shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cup {
    #[default]
    Off,
    /// A hold or release is in flight; clicks wait for it.
    Pending,
    On(Mode),
}

/// What the worker thread does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    Hold,
    Release,
}

struct Request {
    command: Command,
    reply: SyncSender<keepawake::Result<()>>,
}

#[derive(Default)]
struct Caffeine {
    cup: Cup,
    /// Dropping it ends the worker, which releases any hold on its own thread.
    worker: Option<SyncSender<Request>>,
}

impl Global for Caffeine {}

pub fn cup(cx: &App) -> Cup {
    cx.try_global::<Caffeine>().map_or(Cup::Off, |c| c.cup)
}

/// The cup's icon, tooltip and whether it is lit, for the title bar.
pub fn button(cx: &App) -> (&'static str, &'static str, bool) {
    match cup(cx) {
        Cup::On(Mode::Indefinite) => ("coffee-full", "Keeping the display awake · Click to stop", true),
        Cup::On(Mode::UntilAgentsFinish) => ("coffee-full", "Keeping the display awake until agents finish · Click to stop", true),
        Cup::Pending => ("coffee", "Keep the display awake", false),
        Cup::Off => ("coffee", "Keep the display awake · Right-click: until agents finish", false),
    }
}

/// Starts keeping the display awake until the cup is clicked again, or stops
/// any hold.
pub fn toggle(cx: &mut App) {
    match cup(cx) {
        Cup::Pending => {},
        Cup::Off => send(cx, Command::Hold, Mode::Indefinite),
        Cup::On(_) => send(cx, Command::Release, Mode::Indefinite),
    }
}

/// Starts keeping the display awake until every agent finishes, turns an
/// indefinite hold into one, or stops one. With no agent working there is
/// nothing to wait for: an error to show.
pub fn toggle_until_agents_finish(cx: &mut App) -> Result<(), &'static str> {
    match cup(cx) {
        Cup::Pending => {},
        Cup::On(Mode::UntilAgentsFinish) => send(cx, Command::Release, Mode::Indefinite),
        _ if !any_working(cx) => return Err("No agent is working, so there is nothing to wait for"),
        Cup::On(Mode::Indefinite) => {
            cx.global_mut::<Caffeine>().cup = Cup::On(Mode::UntilAgentsFinish);
            cx.refresh_windows();
        },
        Cup::Off => send(cx, Command::Hold, Mode::UntilAgentsFinish),
    }
    Ok(())
}

/// Ends a hold until agents finish once none is working. Called when an
/// agent's status changes and when tabs or windows close, often from inside
/// a workspace's update, so it looks at the agents once that is over.
pub fn settle(cx: &mut App) {
    if cup(cx) != Cup::On(Mode::UntilAgentsFinish) {
        return;
    }
    cx.defer(|cx| {
        if cup(cx) == Cup::On(Mode::UntilAgentsFinish) && !any_working(cx) {
            send(cx, Command::Release, Mode::Indefinite);
        }
    });
}

fn any_working(cx: &mut App) -> bool {
    crate::main_window::all_workspaces(cx)
        .into_iter()
        .any(|(_, ws)| ws.read(cx).term_tabs(cx).iter().any(|t| t.agent.is_some() && t.view.read(cx).status() == ActivityStatus::Working))
}

/// Asks the worker for `command` (`mode` is what a hold becomes), spawning
/// the worker on the first hold.
fn send(cx: &mut App, command: Command, mode: Mode) {
    let caffeine = cx.default_global::<Caffeine>();
    let worker = match caffeine.worker.take() {
        Some(worker) => Ok(worker),
        None if command == Command::Release => {
            // No worker holds nothing.
            caffeine.cup = Cup::Off;
            cx.refresh_windows();
            return;
        },
        None => spawn(),
    };
    let (reply, replies) = std::sync::mpsc::sync_channel(1);
    // Never blocks: `Pending` keeps at most one request in the slot.
    let sent = worker.and_then(|w| w.send(Request { command, reply }).map(|()| w).map_err(|_| "the keep-awake thread stopped".to_owned()));
    let caffeine = cx.global_mut::<Caffeine>();
    match sent {
        Ok(worker) => caffeine.worker = Some(worker),
        Err(e) => return report(&e, cx),
    }
    caffeine.cup = Cup::Pending;
    cx.refresh_windows();
    let wait = cx.background_executor().spawn(async move { replies.recv() });
    cx.spawn(async move |cx| {
        // A dropped reply means the worker died.
        let result = match wait.await {
            Ok(result) => result.map_err(|e| e.to_string()),
            Err(_) => Err("the keep-awake thread stopped".to_owned()),
        };
        cx.update(|cx| finish(cx, command, mode, result));
    })
    .detach();
}

fn finish(cx: &mut App, command: Command, mode: Mode, result: Result<(), String>) {
    let caffeine = cx.global_mut::<Caffeine>();
    caffeine.cup = match (command, &result) {
        (Command::Hold, Ok(())) => Cup::On(mode),
        // A failed hold holds nothing.
        (Command::Hold, Err(_)) | (Command::Release, _) => Cup::Off,
    };
    if result.is_err() {
        caffeine.worker = None;
    }
    cx.refresh_windows();
    match result {
        Err(e) => report(&e, cx),
        // The agents may have finished while the hold was being made.
        Ok(()) => settle(cx),
    }
}

/// Shows a failure in the window in front.
fn report(error: &str, cx: &mut App) {
    log::warn!("caffeine: {error}");
    let message = format!("Could not keep the display awake: {error}");
    if let Some(window) = cx.active_window().or_else(|| cx.windows().first().copied()) {
        let _ = window.update(cx, |_, window, cx| window.push_notification(message, cx));
    }
}

fn spawn() -> Result<SyncSender<Request>, String> {
    let (worker, requests) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("tern-keepawake".into())
        .spawn(move || run(&requests))
        .map_err(|e| format!("could not start its thread: {e}"))?;
    Ok(worker)
}

/// Serves requests until the app drops its sender, then drops any hold here.
fn run(requests: &Receiver<Request>) {
    let mut guard = None;
    for Request { command, reply } in requests {
        let result = match command {
            Command::Hold if guard.is_some() => Ok(()),
            Command::Hold => hold().map(|held| guard = Some(held)),
            Command::Release => {
                guard = None;
                Ok(())
            },
        };
        // The waiting task outlives the request unless the app is quitting.
        let _ = reply.send(result);
    }
}

fn hold() -> keepawake::Result<keepawake::KeepAwake> {
    // Idle, not `sleep`: that one needs AC power, and closing the lid
    // should still sleep.
    keepawake::Builder::default()
        .display(true)
        .idle(true)
        .reason("Tern keeps the display awake")
        .app_name("Tern")
        .app_reverse_domain("dev.tern")
        .create()
}

// The cup (Lucide's `coffee`, and one filled while it holds), drawn as masks
// in the text colour.
const COFFEE: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M10 2v2"/><path d="M14 2v2"/><path d="M16 8a1 1 0 0 1 1 1v8a4 4 0 0 1-4 4H7a4 4 0 0 1-4-4V9a1 1 0 0 1 1-1h14a4 4 0 1 1 0 8h-1"/><path d="M6 2v2"/></svg>"#;
const COFFEE_FULL: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M10 2v2"/><path d="M14 2v2"/><path d="M6 2v2"/><path d="M4 8h12a1 1 0 0 1 1 1v8a4 4 0 0 1-4 4H7a4 4 0 0 1-4-4V9a1 1 0 0 1 1-1z" fill="black"/><path d="M17 9h1a4 4 0 1 1 0 8h-1"/></svg>"#;

const NAMED: &[(&str, &str)] = &[("coffee", COFFEE), ("coffee-full", COFFEE_FULL)];

/// The SVG at asset `path`, when it is one of the cup's icons.
pub fn asset(path: &str) -> Option<&'static [u8]> {
    let name = path.strip_prefix("icons/")?.strip_suffix(".svg")?;
    NAMED.iter().find(|(n, _)| *n == name).map(|(_, svg)| svg.as_bytes())
}

pub fn asset_paths(prefix: &str) -> impl Iterator<Item = SharedString> + '_ {
    NAMED.iter().map(|(n, _)| format!("icons/{n}.svg")).filter(move |p| p.starts_with(prefix)).map(SharedString::from)
}
