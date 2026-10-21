//! Keeps the machine and its display awake, like the Caffeine menu-bar app
//! (as in herdr-gpui) and Orca's status-bar cup. Clicking the title bar's
//! cup picks a mode: On holds it awake, Agents holds it only while an agent
//! is working, Off lets it sleep. The mode is kept for the next launch.
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

use std::rc::Rc;
use std::sync::mpsc::{Receiver, SyncSender};

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::*;

use crate::template;
use crate::terminal_view::ActivityStatus;
use crate::window_state;

/// When the computer is kept awake.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Always.
    On,
    /// While an agent is working.
    Agents,
    #[default]
    Off,
}

impl Mode {
    const ALL: [Mode; 3] = [Mode::On, Mode::Agents, Mode::Off];

    fn key(self) -> &'static str {
        match self {
            Mode::On => "on",
            Mode::Agents => "agents",
            Mode::Off => "off",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.key() == s)
    }

    fn label(self) -> &'static str {
        match self {
            Mode::On => "On",
            Mode::Agents => "Agents",
            Mode::Off => "Off",
        }
    }

    fn detail(self) -> &'static str {
        match self {
            Mode::On => "Keep this computer awake continuously",
            Mode::Agents => "Stay awake while an agent is working",
            Mode::Off => "Allow normal system sleep",
        }
    }
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
    mode: Mode,
    /// Whether the worker holds the computer awake.
    held: bool,
    /// A hold or release is in flight; the next waits for it.
    pending: bool,
    /// Dropping it ends the worker, which releases any hold on its own thread.
    worker: Option<SyncSender<Request>>,
}

impl Global for Caffeine {}

fn state(cx: &App) -> (Mode, bool) {
    cx.try_global::<Caffeine>().map_or((Mode::Off, false), |c| (c.mode, c.held))
}

/// Takes up the mode chosen last time.
pub fn init(cx: &mut App) {
    let mode = Mode::parse(&window_state::caffeine_mode()).unwrap_or_default();
    cx.default_global::<Caffeine>().mode = mode;
    reconcile(cx);
}

/// The cup's icon, tooltip and whether it is lit (a mode other than Off), for
/// the title bar. The cup is full while it holds.
pub fn button(cx: &App) -> (&'static str, String, bool) {
    let (mode, held) = state(cx);
    let icon = if held { "coffee-full" } else { "coffee" };
    (icon, format!("Keep computer awake · {}", status(mode, held)), mode != Mode::Off)
}

fn status(mode: Mode, held: bool) -> String {
    format!("{} · {}", mode.label(), if held { "Active" } else { "Inactive" })
}

/// Fills the cup's menu: the modes to pick from.
pub fn menu(menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
    let (mode, held) = state(cx);
    let modes = Mode::ALL
        .iter()
        .map(|m| template::Scope::default().var("label", m.label()).var("detail", m.detail()).var("on", *m == mode))
        .collect();
    let scope = template::Scope::new("caffeine").var("status", status(mode, held)).list("modes", modes);
    let items: template::MenuItems = Rc::new(|name, entry| {
        let mode = *Mode::ALL.get(entry.filter(|_| name == "mode")?)?;
        Some(Rc::new(move |_, _, cx| set_mode(mode, cx)))
    });
    template::menu("menu.caffeine", scope, menu, items, window, cx)
}

/// Switches to `mode`, kept for the next launch.
pub fn set_mode(mode: Mode, cx: &mut App) {
    window_state::set_caffeine_mode(mode.key());
    cx.default_global::<Caffeine>().mode = mode;
    cx.refresh_windows();
    reconcile(cx);
}

/// Holds or releases as the agents now need, in Agents mode. Called when an
/// agent's status changes and when tabs or windows close, often from inside
/// a workspace's update, so it looks at the agents once that is over.
pub fn settle(cx: &mut App) {
    if state(cx).0 == Mode::Agents {
        cx.defer(reconcile);
    }
}

/// Starts or ends the hold to match the mode, unless one is in flight:
/// `finish` comes back here.
fn reconcile(cx: &mut App) {
    let Some(&Caffeine { mode, held, pending, .. }) = cx.try_global::<Caffeine>() else { return };
    if pending {
        return;
    }
    let want = match mode {
        Mode::On => true,
        Mode::Agents => any_working(cx),
        Mode::Off => false,
    };
    if want != held {
        send(cx, if want { Command::Hold } else { Command::Release });
    }
}

fn any_working(cx: &mut App) -> bool {
    crate::main_window::all_workspaces(cx)
        .into_iter()
        .any(|(_, ws)| ws.read(cx).term_tabs(cx).iter().any(|t| t.agent.is_some() && t.view.read(cx).status() == ActivityStatus::Working))
}

/// Asks the worker for `command`, spawning it on the first hold.
fn send(cx: &mut App, command: Command) {
    let caffeine = cx.default_global::<Caffeine>();
    let worker = match caffeine.worker.take() {
        Some(worker) => Ok(worker),
        None if command == Command::Release => {
            // No worker holds nothing.
            caffeine.held = false;
            cx.refresh_windows();
            return;
        },
        None => spawn(),
    };
    let (reply, replies) = std::sync::mpsc::sync_channel(1);
    // Never blocks: `pending` keeps at most one request in the slot.
    let sent = worker.and_then(|w| w.send(Request { command, reply }).map(|()| w).map_err(|_| "the keep-awake thread stopped".to_owned()));
    let caffeine = cx.global_mut::<Caffeine>();
    match sent {
        Ok(worker) => caffeine.worker = Some(worker),
        Err(e) => return fail(&e, cx),
    }
    caffeine.pending = true;
    let wait = cx.background_executor().spawn(async move { replies.recv() });
    cx.spawn(async move |cx| {
        // A dropped reply means the worker died.
        let result = match wait.await {
            Ok(result) => result.map_err(|e| e.to_string()),
            Err(_) => Err("the keep-awake thread stopped".to_owned()),
        };
        cx.update(|cx| finish(cx, command, result));
    })
    .detach();
}

fn finish(cx: &mut App, command: Command, result: Result<(), String>) {
    let caffeine = cx.global_mut::<Caffeine>();
    caffeine.pending = false;
    match result {
        Ok(()) => {
            caffeine.held = command == Command::Hold;
            cx.refresh_windows();
            // The mode or the agents may have changed while it was in flight.
            reconcile(cx);
        },
        Err(e) => fail(&e, cx),
    }
}

/// A failed hold or release holds nothing: back to Off for this run (the
/// mode chosen stays for the next launch), and the failure shown.
fn fail(error: &str, cx: &mut App) {
    let caffeine = cx.global_mut::<Caffeine>();
    caffeine.worker = None;
    caffeine.held = false;
    caffeine.mode = Mode::Off;
    cx.refresh_windows();
    report(error, cx);
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
