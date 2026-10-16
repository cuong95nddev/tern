//! Terminal pane: a GPUI view over a terminal running in the background
//! daemon ([`tern_mux::client::RemoteSession`]).
//!
//! Rendering: on each frame we lock the VT state briefly and convert the
//! visible grid into background rects + text segments. Segments are shaped via
//! GPUI's line-layout cache (unchanged rows hit the cache) with `force_width`
//! snapping glyphs to the cell grid. Wide chars get their own segment so CJK /
//! emoji stay aligned.
//!
//! Input: named keys and Ctrl/Alt chords are encoded in `on_key_down`; plain
//! printable text goes through the platform input handler, so IMEs (Vietnamese
//! Telex, CJK) and dead keys work via marked text.

use std::cell::RefCell;
use std::ops::Range;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tern_mux::client::{Connector, Open, RemoteSession};
use tern_term::keys::{self, Key, KeyEventKind, Mods};
use tern_term::mouse::{self, MouseAction, MouseMods};
use tern_term::osc::OscSignal;
use tern_term::palette::Palette;
use tern_term::status::next_status;
use tern_term::vt::grid::{Dimensions, Scroll};
use tern_term::vt::index::{Column, Line, Point as GridPoint, Side};
use tern_term::vt::selection::{Selection, SelectionType};
use tern_term::vt::term::TermMode;
use tern_term::vt::term::cell::Flags;
use tern_term::vt::vte::ansi::{ClearMode, CursorShape, Handler, Rgb};
use tern_term::{SpawnConfig, TermEvent, TermSize};

use crate::agents::Agent;
use crate::links::{self, Link};
use crate::platform::bind;

actions!(terminal, [Copy, Paste, Clear, ScrollPageUp, ScrollPageDown, SelectAll]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        bind("cmd-c", Copy, Some("Terminal")),
        bind("cmd-v", Paste, Some("Terminal")),
        bind("cmd-k", Clear, Some("Terminal")),
        bind("cmd-a", SelectAll, Some("Terminal")),
        bind("shift-pageup", ScrollPageUp, Some("Terminal")),
        bind("shift-pagedown", ScrollPageDown, Some("Terminal")),
        // The root binds tab / shift-tab to move focus, and bindings run
        // before `key_down`: unbind them here so the shell gets the key
        // (completion, accepting a suggestion).
        bind("tab", NoAction, Some("Terminal")),
        bind("shift-tab", NoAction, Some("Terminal")),
    ]);
    if !cfg!(target_os = "macos") {
        cx.bind_keys([bind("ctrl-insert", Copy, Some("Terminal")), bind("shift-insert", Paste, Some("Terminal"))]);
    }
}

pub use tern_term::status::ActivityStatus;

#[derive(Debug, Clone)]
pub enum TerminalEvent {
    TitleChanged,
    StatusChanged(ActivityStatus),
    Notify {
        title: Option<String>,
        body: String,
    },
    Bell,
    /// The process ended; `quick` when it ran only a few seconds; `live`
    /// when it ended while shown (not an exit replayed on attaching to a
    /// session that ended while no one watched).
    Exited {
        code: Option<i32>,
        quick: bool,
        live: bool,
    },
    /// The background session is known (see `mux_id`), with its hook token;
    /// `attached`: it was already running (no new program started).
    Ready {
        token: Option<String>,
        attached: bool,
    },
    /// ⌘-click on a path that exists; absolute.
    OpenPath {
        path: PathBuf,
        line: Option<u32>,
        col: Option<u32>,
    },
}

/// What ⌘-click would open, and the cells (grid coordinates, inclusive)
/// underlined while ⌘ is held over it.
#[derive(Debug, Clone, PartialEq)]
enum Target {
    Url(String),
    Path { path: PathBuf, line: Option<u32>, col: Option<u32> },
}

#[derive(Debug, Clone, PartialEq)]
struct HoverLink {
    start: GridPoint,
    end: GridPoint,
    target: Target,
}

/// Which Option keys act as Meta (ESC prefix); the others compose
/// characters, so layouts that type `@ [ { |` with Option keep working.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum OptionAsMeta {
    None,
    Left,
    Right,
    Both,
}

#[derive(Debug, Clone)]
pub struct TerminalConfig {
    pub font_family: SharedString,
    pub font_size: Pixels,
    pub line_height: f32,
    pub option_as_meta: OptionAsMeta,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self { font_family: "Menlo".into(), font_size: px(13.), line_height: 1.25, option_as_meta: OptionAsMeta::Left }
    }
}

impl TerminalConfig {
    /// The defaults with the font of the settings in effect.
    pub fn from_look(cx: &App) -> Self {
        let mut c = Self::default();
        if let Some(look) = cx.try_global::<TerminalLook>() {
            (c.font_family, c.font_size, c.line_height) = (look.font_family.clone(), look.font_size, look.line_height);
        }
        c
    }
}

/// How terminals look, from the settings ([`crate::settings`]): every
/// terminal view follows it, so a saved change shows at once.
pub struct TerminalLook {
    pub font_family: SharedString,
    pub font_size: Pixels,
    pub line_height: f32,
    pub palette: Arc<Palette>,
}

impl Global for TerminalLook {}

/// Which Option keys are down now, `(left, right)`.
#[cfg(target_os = "macos")]
fn option_keys() -> (bool, bool) {
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventSourceFlagsState(state: i32) -> u64;
    }
    // kCGEventSourceStateHIDSystemState; NX_DEVICE{L,R}ALTKEYMASK.
    let flags = unsafe { CGEventSourceFlagsState(1) };
    (flags & 0x20 != 0, flags & 0x40 != 0)
}

#[cfg(not(target_os = "macos"))]
fn option_keys() -> (bool, bool) {
    (true, false)
}

impl OptionAsMeta {
    /// Whether the Option key held for this keystroke acts as Meta.
    fn applies(self) -> bool {
        let (left, right) = match option_keys() {
            // Unknown side: treat as the left key.
            (false, false) => (true, false),
            sides => sides,
        };
        match self {
            Self::None => false,
            Self::Left => left,
            Self::Right => right,
            Self::Both => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct CellMetrics {
    width: Pixels,
    height: Pixels,
}

pub struct TerminalView {
    session: Option<RemoteSession>,
    spawn_cfg: SpawnConfig,
    mux: Arc<Connector>,
    /// Spawn with a Claude hook token.
    hook: bool,
    /// The background session to attach to at the next start.
    attach: Option<u64>,
    /// Reattaching after the connection to the daemon was lost: if the
    /// session is gone, say so rather than start a new program.
    reattaching: bool,
    /// The last start attached to a running session (vs. spawned one).
    attaching: bool,
    /// Output arrived since the session was ready: an exit after it was
    /// seen, not one the daemon replays on attach.
    output_since_ready: bool,
    config: TerminalConfig,
    focus: FocusHandle,
    title: SharedString,
    default_title: SharedString,
    status: ActivityStatus,
    exited: Option<Option<i32>>,
    /// Why the process couldn't start.
    spawn_error: Option<String>,
    /// When the process started.
    started: Instant,
    /// The process starts at the first layout, with the pane's real size,
    /// or shortly after when the tab isn't shown.
    spawned: bool,
    /// Whether the program was last told it has focus (DECSET 1004).
    reported_focus: bool,
    /// The window the focus listeners are registered with.
    focus_window: Option<AnyWindowHandle>,
    _focus_subs: Vec<Subscription>,
    /// Pending PTY resize while a split is dragged.
    _resize: Option<Task<()>>,
    /// The cell the last mouse motion was reported for.
    last_motion: Option<(usize, usize)>,
    marked_text: Option<String>,
    metrics: Option<CellMetrics>,
    /// Content origin + cursor rect from the last prepaint (for mouse + IME).
    last_origin: Point<Pixels>,
    last_cursor: Option<Bounds<Pixels>>,
    scroll_px: f32,
    selecting: bool,
    mouse_button_down: Option<mouse::MouseButton>,
    /// Last mouse position over the view, to find the link under it when ⌘
    /// goes down or the text changes.
    mouse_pos: Option<Point<Pixels>>,
    hover_link: Option<HoverLink>,
    /// The last path looked up for a link and where it resolved, so moving
    /// over the same word doesn't search the disk again. Dropped when ⌘ goes
    /// down.
    resolved: RefCell<Option<(String, Option<PathBuf>)>>,
    /// The coding agent running in the foreground, if any.
    agent: Option<Agent>,
    /// When the program last wrote something.
    last_output: Instant,
    _pump: Option<Task<()>>,
    _watch: Task<()>,
}

impl EventEmitter<TerminalEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TerminalView {
    /// A terminal that attaches to background session `attach` when it still
    /// runs, else starts `spawn_cfg` (with a hook token when `hook`).
    pub fn new(
        spawn_cfg: SpawnConfig,
        hook: bool,
        attach: Option<u64>,
        title: impl Into<SharedString>,
        config: TerminalConfig,
        mux: Arc<Connector>,
        cx: &mut Context<Self>,
    ) -> Self {
        let title = title.into();
        let mut this = Self {
            session: None,
            spawn_cfg,
            mux,
            hook,
            attach,
            reattaching: false,
            attaching: false,
            output_since_ready: false,
            config,
            focus: cx.focus_handle(),
            title: title.clone(),
            default_title: title,
            status: ActivityStatus::None,
            exited: None,
            spawn_error: None,
            started: Instant::now(),
            spawned: false,
            reported_focus: false,
            focus_window: None,
            _focus_subs: vec![],
            _resize: None,
            last_motion: None,
            marked_text: None,
            metrics: None,
            last_origin: Point::default(),
            last_cursor: None,
            scroll_px: 0.,
            selecting: false,
            mouse_button_down: None,
            mouse_pos: None,
            hover_link: None,
            resolved: RefCell::new(None),
            agent: None,
            last_output: Instant::now(),
            _pump: None,
            _watch: Task::ready(()),
        };
        this._watch = cx.spawn(async move |this, cx| {
            // A tab that isn't shown never lays out: start it anyway.
            cx.background_executor().timer(Duration::from_millis(150)).await;
            if this.update(cx, |view, cx| view.start_once(cx)).is_err() {
                return;
            }
            // Each second while the program writes (an agent starting or
            // ending shows), else every few: a scan reads every process.
            let mut idle_ticks = 0;
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let Ok(probe) = this.update(cx, |view, _| {
                    let busy = view.last_output.elapsed() < Duration::from_secs(10);
                    idle_ticks = if busy { 0 } else { idle_ticks + 1 };
                    (busy || idle_ticks % 5 == 0).then(|| view.session.as_ref().map(RemoteSession::foreground))
                }) else {
                    break;
                };
                let Some(probe) = probe else { continue };
                let programs = match probe {
                    Some(p) => cx.background_spawn(async move { p.programs() }).await,
                    None => Vec::new(),
                };
                if this.update(cx, |view, cx| view.set_agent(Agent::detect(&programs), cx)).is_err() {
                    break;
                }
            }
        });
        this
    }

    fn start_once(&mut self, cx: &mut Context<Self>) {
        if !self.spawned {
            self.start(cx);
        }
    }

    /// Start (or restart) the process: attach to the saved background
    /// session the first time, else start a new one (ending the old one).
    pub fn start(&mut self, cx: &mut Context<Self>) {
        self.spawned = true;
        self.reported_focus = false;
        let (tx, rx) = flume::unbounded::<TermEvent>();
        let sink = Arc::new(move |e| {
            let _ = tx.send(e);
        });
        let mut cfg = self.spawn_cfg.clone();
        if let Some(s) = self.session.take() {
            cfg.size = s.size();
            s.close();
        }
        self.started = Instant::now();
        self.attaching = self.attach.is_some();
        let open = match self.attach.take() {
            Some(id) => Open::Attach { id },
            None => Open::Spawn { cfg: cfg.clone(), hook: self.hook },
        };
        match self.mux.get() {
            Ok(client) => {
                self.session = Some(client.open(open, cfg.size, cfg.scrollback, sink));
                self.exited = None;
                self.spawn_error = None;
            },
            Err(err) => {
                log::error!("background server unavailable: {err}");
                self.exited = Some(None);
                self.spawn_error = Some(format!("background server unavailable: {err}"));
            },
        }
        cx.notify();
        self._pump = Some(cx.spawn(async move |this, cx| {
            while let Ok(first) = rx.recv_async().await {
                let mut batch = vec![first];
                batch.extend(rx.try_iter());
                let alive = this.update(cx, |view, cx| {
                    for ev in batch {
                        view.on_term_event(ev, cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        }));
        cx.notify();
    }

    pub fn title(&self) -> &SharedString {
        &self.title
    }

    pub fn status(&self) -> ActivityStatus {
        self.status
    }

    pub fn agent(&self) -> Option<Agent> {
        self.agent
    }

    /// Whether the program has settled for input: it turned on bracketed
    /// paste and has been quiet for `quiet`.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))] // for the screenshot tray
    pub fn ready_for_paste(&self, quiet: Duration) -> bool {
        self.mode().contains(TermMode::BRACKETED_PASTE) && self.last_output.elapsed() >= quiet
    }

    /// The visible screen's text, trailing blanks trimmed (control socket).
    #[cfg(debug_assertions)]
    pub fn debug_screen(&self) -> String {
        let Some(s) = &self.session else { return String::new() };
        let term = s.term().lock();
        let grid = term.grid();
        let mut lines: Vec<String> = (0..grid.screen_lines() as i32)
            .map(|l| grid[Line(l)].into_iter().map(|c| if c.c == '\0' { ' ' } else { c.c }).collect::<String>().trim_end().to_owned())
            .collect();
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        lines.join("\\n")
    }

    /// Size, whether the program was told it has focus, and how it exited.
    #[cfg(debug_assertions)]
    pub fn debug_info(&self) -> String {
        let size = self.session.as_ref().map(|s| s.size());
        format!(
            "size={size:?} focus_reported={} focus_mode={} exited={:?} spawn_error={:?} history={}",
            self.reported_focus,
            self.mode().contains(TermMode::FOCUS_IN_OUT),
            self.exited,
            self.spawn_error,
            self.session.as_ref().map_or(0, |s| s.term().lock().grid().history_size()),
        )
    }

    /// Paste `paths` as a drop would (control socket).
    #[cfg(debug_assertions)]
    pub fn debug_paste_paths(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        self.paste_paths(paths, cx);
    }

    /// The background session's id, once known.
    pub fn mux_id(&self) -> Option<u64> {
        self.session.as_ref().and_then(RemoteSession::id).or(self.attach)
    }

    /// End the program (the tab is closing). Dropping the view instead
    /// leaves it running in the background.
    pub fn close(&mut self) {
        self.attach = None;
        if let Some(s) = self.session.take() {
            s.close();
        }
    }

    /// Run `cfg` from the next (re)start on.
    pub fn set_spawn_config(&mut self, cfg: SpawnConfig) {
        self.spawn_cfg = cfg;
    }

    /// Whether the next start gets a Claude hook token.
    pub fn set_hook(&mut self, hook: bool) {
        self.hook = hook;
    }

    /// The program running in the foreground when it isn't the shell.
    pub fn foreground_job(&self) -> Option<String> {
        let s = self.session.as_ref().filter(|_| self.exited.is_none())?;
        let shell = self.spawn_cfg.program.as_deref().and_then(|p| Path::new(p).file_name()).and_then(|n| n.to_str()).unwrap_or("");
        s.foreground_programs().into_iter().find(|p| {
            let p = p.trim_start_matches('-');
            p != shell
                && !matches!(
                    p,
                    "login"
                        | "zsh"
                        | "bash"
                        | "fish"
                        | "sh"
                        | "dash"
                        | "tcsh"
                        | "csh"
                        | "ksh"
                        | "nu"
                        | "pwsh"
                        | "powershell"
                        | "cmd"
                        | "wsl"
                )
        })
    }

    fn set_agent(&mut self, agent: Option<Agent>, cx: &mut Context<Self>) {
        // The session may have ended while its programs were read.
        let agent = agent.filter(|_| self.session.is_some());
        if agent != self.agent {
            self.agent = agent;
            cx.emit(TerminalEvent::TitleChanged);
        }
    }

    pub fn write_input(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        let Some(s) = &self.session else { return };
        {
            let mut term = s.term().lock();
            if term.grid().display_offset() != 0 {
                term.scroll_display(Scroll::Bottom);
                cx.notify();
            }
            if term.selection.take().is_some() {
                cx.notify();
            }
        }
        s.write(bytes);
    }

    fn on_term_event(&mut self, ev: TermEvent, cx: &mut Context<Self>) {
        match ev {
            TermEvent::Wakeup => {
                self.last_output = Instant::now();
                self.output_since_ready = true;
                if self.hover_link.is_some() {
                    self.update_hover(true, cx);
                }
                cx.notify()
            },
            TermEvent::Title(t) => {
                self.title = t.into();
                cx.emit(TerminalEvent::TitleChanged);
            },
            TermEvent::ResetTitle => {
                self.title = self.default_title.clone();
                cx.emit(TerminalEvent::TitleChanged);
            },
            TermEvent::Bell => cx.emit(TerminalEvent::Bell),
            TermEvent::ClipboardStore(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            TermEvent::Exit(code) => {
                self.exited = Some(code);
                self.set_status(ActivityStatus::None, cx);
                let quick = self.started.elapsed() < Duration::from_secs(5);
                let live = !self.attaching || self.output_since_ready;
                cx.emit(TerminalEvent::Exited { code, quick, live });
                cx.notify();
            },
            TermEvent::Signal(sig @ OscSignal::ProgramStatus(_)) => {
                if let Some(status) = next_status(self.status, &sig) {
                    self.set_status(status, cx);
                }
            },
            TermEvent::Signal(OscSignal::Notify { title, body }) => {
                cx.emit(TerminalEvent::Notify { title, body });
            },
            TermEvent::Signal(OscSignal::ProgramStatusProbe | OscSignal::ColorSchemeReports(_) | OscSignal::ColorSchemeQuery) => {},
            TermEvent::Ready { token, status, .. } => {
                // The status it had while no one watched (e.g. Done): shown,
                // not notified again.
                self.status = status;
                self.reattaching = false;
                self.output_since_ready = false;
                cx.emit(TerminalEvent::Ready { token, attached: self.attaching });
                cx.emit(TerminalEvent::TitleChanged);
                cx.notify();
            },
            // The session is gone: start the program afresh, unless this was
            // a reconnect (the daemon died with it; Enter restarts).
            // After a restart on purpose, start afresh on the new daemon.
            TermEvent::NotFound if self.reattaching && crate::restart::just_restarted(cx) => {
                self.reattaching = false;
                self.start(cx)
            },
            TermEvent::NotFound if self.reattaching => self.server_stopped(cx),
            TermEvent::NotFound => self.start(cx),
            TermEvent::SpawnError(err) => {
                log::error!("failed to spawn terminal: {err}");
                self.session = None;
                self.exited = Some(None);
                self.spawn_error = Some(err);
                cx.notify();
            },
            TermEvent::ServerLost => {
                // The connection went (the daemon dropped it or died): try the
                // same session again once.
                let id = self.session.take().and_then(|s| s.id());
                // Restarted on purpose: its program went with the daemon.
                if self.exited.is_none() && crate::restart::just_restarted(cx) {
                    self.reattaching = false;
                    return self.start(cx);
                }
                if self.exited.is_none()
                    && !self.reattaching
                    && let Some(id) = id
                {
                    self.reattaching = true;
                    self.attach = Some(id);
                    return self.start(cx);
                }
                self.server_stopped(cx);
            },
        }
    }

    fn server_stopped(&mut self, cx: &mut Context<Self>) {
        self.reattaching = false;
        if self.exited.is_none() {
            self.exited = Some(None);
            self.spawn_error = Some("background server stopped".into());
            self.set_status(ActivityStatus::None, cx);
            cx.emit(TerminalEvent::Exited { code: None, quick: false, live: false });
        }
        cx.notify();
    }

    fn set_status(&mut self, status: ActivityStatus, cx: &mut Context<Self>) {
        // A program that ends mid-turn ("working") reads as done.
        let status = if status == ActivityStatus::None && self.status == ActivityStatus::Working { ActivityStatus::Done } else { status };
        if self.status != status {
            self.status = status;
            cx.emit(TerminalEvent::StatusChanged(status));
        }
    }

    pub fn clear_done_status(&mut self, cx: &mut Context<Self>) {
        if self.status == ActivityStatus::Done {
            self.status = ActivityStatus::None;
            cx.emit(TerminalEvent::StatusChanged(ActivityStatus::None));
        }
    }

    /// Tell the program whether it has focus (DECSET 1004): this pane is
    /// focused, shown and the window is active.
    fn sync_focus(&mut self, window: &mut Window, _: &mut Context<Self>) {
        let focused = self.focus.is_focused(window) && window.is_window_active();
        if focused == self.reported_focus {
            return;
        }
        self.reported_focus = focused;
        if self.mode().contains(TermMode::FOCUS_IN_OUT)
            && let Some(s) = &self.session
        {
            s.write(if focused { &b"\x1b[I"[..] } else { &b"\x1b[O"[..] });
        }
    }

    fn watch_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = window.window_handle();
        if self.focus_window == Some(handle) {
            return;
        }
        self.focus_window = Some(handle);
        self._focus_subs = vec![
            cx.on_focus(&self.focus, window, Self::sync_focus),
            cx.on_blur(&self.focus, window, Self::sync_focus),
            cx.observe_window_activation(window, Self::sync_focus),
        ];
    }

    fn mode(&self) -> TermMode {
        self.session.as_ref().map(|s| *s.term().lock().mode()).unwrap_or(TermMode::empty())
    }

    // ---- keyboard -------------------------------------------------------

    /// The key a keystroke encodes as, or `None` for text that goes
    /// through the input handler (IMEs, dead keys).
    fn map_key(ks: &Keystroke, meta: bool, all_keys: bool) -> Option<Key> {
        let m = ks.modifiers;
        match ks.key.as_str() {
            "enter" => Some(Key::Enter),
            "tab" => Some(Key::Tab),
            "backspace" => Some(Key::Backspace),
            "escape" => Some(Key::Escape),
            "up" => Some(Key::Up),
            "down" => Some(Key::Down),
            "left" => Some(Key::Left),
            "right" => Some(Key::Right),
            "home" => Some(Key::Home),
            "end" => Some(Key::End),
            "pageup" => Some(Key::PageUp),
            "pagedown" => Some(Key::PageDown),
            "insert" => Some(Key::Insert),
            "delete" => Some(Key::Delete),
            "space" if m.control || meta || all_keys => Some(Key::Char(' ')),
            k if k.len() > 1 && k.starts_with('f') => k[1..].parse().ok().map(Key::F),
            // With "report all keys" (Kitty flag 8) text keys are key events too.
            k if (m.control || meta || all_keys) && k.chars().count() == 1 => k.chars().next().map(Key::Char),
            _ => None,
        }
    }

    fn key_down(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        if ks.modifiers.platform || self.marked_text.is_some() {
            return;
        }
        // AltGr (Ctrl+Alt on Windows) or a dead key makes a character: let
        // it arrive as text input.
        if cfg!(windows) && ev.prefer_character_input {
            return;
        }
        if self.exited.is_some() && ks.key == "enter" {
            self.start(cx);
            cx.stop_propagation();
            return;
        }
        let kind = if ev.is_held { KeyEventKind::Repeat } else { KeyEventKind::Press };
        if self.send_key(ks, kind, cx) {
            cx.stop_propagation();
        }
    }

    fn key_up(&mut self, ev: &KeyUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        if ks.modifiers.platform || self.marked_text.is_some() || self.exited.is_some() {
            return;
        }
        if self.mode().contains(TermMode::REPORT_EVENT_TYPES) {
            self.send_key(ks, KeyEventKind::Release, cx);
        }
    }

    fn send_key(&mut self, ks: &Keystroke, kind: KeyEventKind, cx: &mut Context<Self>) -> bool {
        let m = ks.modifiers;
        let meta = m.alt && self.config.option_as_meta.applies();
        let mods = Mods { shift: m.shift, alt: meta, ctrl: m.control, meta: false };
        let mode = self.mode();
        let Some(key) = Self::map_key(ks, meta, mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC)) else { return false };
        let text = ks.key_char.as_deref().filter(|_| !meta);
        match keys::encode_event(key, mods, text, mode, kind) {
            Some(bytes) => {
                self.write_input(bytes, cx);
                true
            },
            None => false,
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = &self.session else { return };
        if let Some(text) = s.term().lock().selection_to_string() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else { return };
        // Files copied in Finder: their paths, not the name Finder also
        // puts on the clipboard as text.
        let paths: Vec<PathBuf> = item
            .entries()
            .iter()
            .filter_map(|e| match e {
                ClipboardEntry::ExternalPaths(p) => Some(p.paths().to_vec()),
                _ => None,
            })
            .flatten()
            .collect();
        if !paths.is_empty() {
            return self.paste_paths(&paths, cx);
        }
        match item.text() {
            Some(text) => self.paste_text(&text, cx),
            // No text (e.g. an image): an empty bracketed paste, as iTerm2
            // sends, tells Claude Code to read the image from the clipboard.
            None if self.mode().contains(TermMode::BRACKETED_PASTE) => self.write_input(b"\x1b[200~\x1b[201~".to_vec(), cx),
            None => {},
        }
    }

    /// Paste shell-escaped paths, space separated (a file drop or Finder
    /// copy); Claude Code attaches the images among them.
    fn paste_paths(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        let text = paths.iter().map(|p| escape_path(p)).collect::<Vec<_>>().join(" ");
        self.paste_text(&format!("{text} "), cx);
    }

    fn drop_paths(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.paste_paths(paths.paths(), cx);
    }

    pub fn paste_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let bracketed = self.mode().contains(TermMode::BRACKETED_PASTE);
        let clean = sanitize_paste(text);
        let bytes = if bracketed {
            format!("\x1b[200~{clean}\x1b[201~").into_bytes()
        } else {
            clean.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
        };
        self.write_input(bytes, cx);
    }

    /// ⌘K: drop the scrollback and the screen, then a form feed so the
    /// shell (or TUI) redraws its prompt.
    fn clear(&mut self, _: &Clear, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(s) = &self.session {
            let mut term = s.term().lock();
            if !term.mode().contains(TermMode::ALT_SCREEN) {
                // Clearing the screen scrolls it into the scrollback: drop
                // that after.
                term.clear_screen(ClearMode::All);
                term.clear_screen(ClearMode::Saved);
                term.goto(0, 0);
            }
        }
        self.write_input(vec![0x0c], cx);
        cx.notify();
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = &self.session else { return };
        let mut term = s.term().lock();
        let top = term.grid().topmost_line();
        let bottom = term.grid().bottommost_line();
        let last_col = term.grid().last_column();
        let mut sel = Selection::new(SelectionType::Simple, GridPoint::new(top, Column(0)), Side::Left);
        sel.update(GridPoint::new(bottom, last_col), Side::Right);
        term.selection = Some(sel);
        cx.notify();
    }

    fn scroll_page(&mut self, up: bool, cx: &mut Context<Self>) {
        if let Some(s) = &self.session {
            s.term().lock().scroll_display(if up { Scroll::PageUp } else { Scroll::PageDown });
            cx.notify();
        }
    }

    // ---- mouse ----------------------------------------------------------

    fn grid_point(&self, pos: Point<Pixels>) -> Option<(usize, usize, Side)> {
        let m = self.metrics?;
        let s = self.session.as_ref()?;
        let size = s.size();
        let local = pos - self.last_origin;
        let x = (local.x / m.width).max(0.);
        let col = (x as usize).min(size.cols.saturating_sub(1) as usize);
        let row = ((local.y / m.height).max(0.) as usize).min(size.rows.saturating_sub(1) as usize);
        let side = if x.fract() < 0.5 { Side::Left } else { Side::Right };
        Some((col, row, side))
    }

    fn mouse_mods(m: &Modifiers) -> MouseMods {
        MouseMods { shift: m.shift, alt: m.alt, ctrl: m.control }
    }

    fn report_mouse(
        &mut self,
        action: MouseAction,
        button: mouse::MouseButton,
        pos: Point<Pixels>,
        mods: &Modifiers,
        cx: &mut Context<Self>,
    ) -> bool {
        let mode = self.mode();
        // Shift bypasses mouse reporting so users can still select text.
        if mods.shift || !mouse::wants(mode, action, button) {
            return false;
        }
        let Some((col, row, _)) = self.grid_point(pos) else { return false };
        if let Some(bytes) = mouse::encode(mode, action, button, Self::mouse_mods(mods), col, row)
            && let Some(s) = &self.session
        {
            s.write(bytes);
        }
        cx.notify();
        true
    }

    fn mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if ev.button == MouseButton::Left
            && ev.modifiers.secondary()
            && let Some(link) = self.link_at(ev.position)
        {
            self.open_link(link.target, cx);
            return;
        }
        let button = match ev.button {
            MouseButton::Left => mouse::MouseButton::Left,
            MouseButton::Middle => mouse::MouseButton::Middle,
            MouseButton::Right => mouse::MouseButton::Right,
            _ => return,
        };
        if self.report_mouse(MouseAction::Press, button, ev.position, &ev.modifiers, cx) {
            self.mouse_button_down = Some(button);
            self.last_motion = self.grid_point(ev.position).map(|(c, r, _)| (c, r));
            return;
        }
        if button != mouse::MouseButton::Left {
            return;
        }
        let Some((col, row, side)) = self.grid_point(ev.position) else { return };
        let Some(s) = &self.session else { return };
        let mut term = s.term().lock();
        let offset = term.grid().display_offset() as i32;
        let point = GridPoint::new(Line(row as i32 - offset), Column(col));
        let ty = match ev.click_count {
            2 => SelectionType::Semantic,
            n if n >= 3 => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        term.selection = Some(Selection::new(ty, point, side));
        self.selecting = true;
        cx.notify();
    }

    fn mouse_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.mouse_pos = Some(ev.position);
        self.update_hover(ev.modifiers.secondary() && ev.pressed_button.is_none(), cx);
        // Motion is reported once per cell: with a button held (1002/1003),
        // or with none (1003).
        let cell = self.grid_point(ev.position).map(|(c, r, _)| (c, r));
        if let Some(button) = self.mouse_button_down {
            if ev.pressed_button.is_some() && cell != self.last_motion {
                self.last_motion = cell;
                self.report_mouse(MouseAction::Motion, button, ev.position, &ev.modifiers, cx);
            }
            return;
        }
        if ev.pressed_button.is_none() && self.mode().contains(TermMode::MOUSE_MOTION) {
            if cell != self.last_motion {
                self.last_motion = cell;
                self.report_mouse(MouseAction::Motion, mouse::MouseButton::None, ev.position, &ev.modifiers, cx);
            }
            return;
        }
        if !self.selecting || ev.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let Some((col, row, side)) = self.grid_point(ev.position) else { return };
        let Some(s) = &self.session else { return };
        let mut term = s.term().lock();
        let offset = term.grid().display_offset() as i32;
        if let Some(sel) = term.selection.as_mut() {
            sel.update(GridPoint::new(Line(row as i32 - offset), Column(col)), side);
        }
        cx.notify();
    }

    fn mouse_up(&mut self, ev: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(button) = self.mouse_button_down.take() {
            // Released outside the pane: report it at the nearest cell, so
            // the program doesn't think the button is still down.
            self.report_mouse(MouseAction::Release, button, ev.position, &ev.modifiers, cx);
            return;
        }
        self.selecting = false;
        // Drop empty (click-without-drag) selections.
        if let Some(s) = &self.session {
            let mut term = s.term().lock();
            if term.selection.as_ref().is_some_and(|sel| sel.is_empty()) {
                term.selection = None;
                cx.notify();
            }
        }
    }

    fn modifiers_changed(&mut self, ev: &ModifiersChangedEvent, _: &mut Window, cx: &mut Context<Self>) {
        if ev.modifiers.secondary() && self.hover_link.is_none() {
            self.resolved.take();
        }
        self.update_hover(ev.modifiers.secondary(), cx);
    }

    fn hover_changed(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if !*hovered {
            self.mouse_pos = None;
            self.update_hover(false, cx);
        }
    }

    /// Underline the link under the mouse while `cmd` is held.
    fn update_hover(&mut self, cmd: bool, cx: &mut Context<Self>) {
        let link = if cmd { self.mouse_pos.and_then(|p| self.link_at(p)) } else { None };
        if link != self.hover_link {
            self.hover_link = link;
            cx.notify();
        }
    }

    /// The link at `pos`: an OSC 8 hyperlink, a URL, or the path of a file
    /// or folder that exists. Text wrapped over several rows counts as one
    /// line.
    fn link_at(&self, pos: Point<Pixels>) -> Option<HoverLink> {
        let (col, row, _) = self.grid_point(pos)?;
        let s = self.session.as_ref()?;
        let (text, points, hyperlinks, ix) = {
            let term = s.term().lock();
            let grid = term.grid();
            let at = GridPoint::new(Line(row as i32 - grid.display_offset() as i32), Column(col));
            let last = grid.last_column();
            let wraps = |l: Line| grid[l][last].flags.contains(Flags::WRAPLINE);
            let (mut first, mut end) = (at.line, at.line);
            while first > grid.topmost_line() && wraps(first - 1) && at.line.0 - first.0 < 20 {
                first -= 1;
            }
            while end < grid.bottommost_line() && wraps(end) && end.0 - at.line.0 < 20 {
                end += 1;
            }
            let (mut text, mut points, mut hyperlinks) = (vec![], vec![], vec![]);
            for l in first.0..=end.0 {
                for (c, cell) in grid[Line(l)].into_iter().enumerate() {
                    if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                        continue;
                    }
                    text.push(if cell.c == '\0' { ' ' } else { cell.c });
                    points.push(GridPoint::new(Line(l), Column(c)));
                    hyperlinks.push(cell.hyperlink());
                }
            }
            // The char at the mouse, or the wide char whose spacer it is on.
            let ix = points.iter().rposition(|p| *p <= at)?;
            (text, points, hyperlinks, ix)
        };
        let (range, link) = match &hyperlinks[ix] {
            Some(h) => {
                let same = |i: &usize| hyperlinks[*i].as_ref() == Some(h);
                let start = (0..ix).rev().take_while(same).last().unwrap_or(ix);
                let end = (ix..text.len()).take_while(same).last().unwrap_or(ix);
                (start..end + 1, links::from_uri(h.uri())?)
            },
            None => links::find_at(&text, ix)?,
        };
        let target = match link {
            Link::Url(url) => Target::Url(url),
            Link::Path { path, line, col } => {
                let mut cache = self.resolved.borrow_mut();
                let found = match &*cache {
                    Some((p, found)) if *p == path => found.clone(),
                    _ => {
                        let bases: Vec<PathBuf> = s.foreground_cwd().into_iter().chain(self.spawn_cfg.cwd.clone()).collect();
                        let found = links::resolve(&path, &bases);
                        *cache = Some((path, found.clone()));
                        found
                    },
                };
                Target::Path { path: found?, line, col }
            },
        };
        Some(HoverLink { start: points[range.start], end: points[range.end - 1], target })
    }

    fn open_link(&mut self, target: Target, cx: &mut Context<Self>) {
        self.hover_link = None;
        match target {
            Target::Url(url) => cx.open_url(&url),
            Target::Path { path, line, col } => cx.emit(TerminalEvent::OpenPath { path, line, col }),
        }
        cx.notify();
    }

    fn scroll_wheel(&mut self, ev: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(m) = self.metrics else { return };
        let dy = match ev.delta {
            ScrollDelta::Pixels(p) => f32::from(p.y),
            ScrollDelta::Lines(l) => l.y * f32::from(m.height),
        };
        self.scroll_px += dy;
        let lines = (self.scroll_px / f32::from(m.height)) as i32;
        if lines == 0 {
            return;
        }
        self.scroll_px -= lines as f32 * f32::from(m.height);
        let mode = self.mode();
        let button = if lines > 0 { mouse::MouseButton::WheelUp } else { mouse::MouseButton::WheelDown };
        if mode.intersects(TermMode::MOUSE_MODE) && !ev.modifiers.shift {
            for _ in 0..lines.unsigned_abs().min(10) {
                self.report_mouse(MouseAction::Press, button, ev.position, &ev.modifiers, cx);
            }
            return;
        }
        if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
            let seq: &[u8] = match (lines > 0, mode.contains(TermMode::APP_CURSOR)) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1b[A",
                (false, true) => b"\x1bOB",
                (false, false) => b"\x1b[B",
            };
            if let Some(s) = &self.session {
                s.write(seq.repeat(lines.unsigned_abs() as usize));
            }
            return;
        }
        if let Some(s) = &self.session {
            s.term().lock().scroll_display(Scroll::Delta(lines));
            cx.notify();
        }
    }

    /// Fit the terminal to `size`. The first layout starts the process at
    /// that size; later changes wait for 30 ms without another one, so
    /// dragging a split doesn't reflow the program on every frame.
    fn resize(&mut self, size: TermSize, cx: &mut Context<Self>) {
        if !self.spawned {
            self.spawn_cfg.size = size;
            self.start(cx);
            return;
        }
        let Some(s) = &self.session else { return };
        if s.size() == size {
            self._resize = None;
            return;
        }
        self._resize = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(30)).await;
            let _ = this.update(cx, |view, cx| {
                if let Some(s) = &view.session {
                    s.resize(size);
                    cx.notify();
                }
            });
        }));
    }

    /// Takes the font of the settings in effect (the cell size follows) and
    /// returns their palette.
    fn sync_look(&mut self, cx: &App) -> Arc<Palette> {
        let Some(look) = cx.try_global::<TerminalLook>() else {
            return Arc::new(Palette::default());
        };
        let c = &mut self.config;
        if (&c.font_family, c.font_size, c.line_height) != (&look.font_family, look.font_size, look.line_height) {
            (c.font_family, c.font_size, c.line_height) = (look.font_family.clone(), look.font_size, look.line_height);
            self.metrics = None;
        }
        look.palette.clone()
    }

    fn ensure_metrics(&mut self, window: &mut Window) -> CellMetrics {
        if let Some(m) = self.metrics {
            return m;
        }
        let ts = window.text_system();
        let font_id = ts.resolve_font(&font(self.config.font_family.clone()));
        let width = ts.em_advance(font_id, self.config.font_size).unwrap_or(px(8.));
        let height = (self.config.font_size * self.config.line_height).round();
        let m = CellMetrics { width, height };
        self.metrics = Some(m);
        m
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(s) = &self.session {
            s.take_wakeup();
        }
        self.watch_focus(window, cx);
        let look = crate::style::el("terminal", cx);
        let bg = rgb_to_hsla(self.sync_look(cx).background).opacity(look.num("background_opacity").clamp(0., 1.));
        let backdrop = look.backdrop();
        let (padding_x, padding_top) = (look.len("padding_x"), look.len("padding_top"));
        div()
            .id("terminal")
            .track_focus(&self.focus)
            .key_context("Terminal")
            .on_key_down(cx.listener(Self::key_down))
            .on_key_up(cx.listener(Self::key_up))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::clear))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(|v, _: &ScrollPageUp, _, cx| v.scroll_page(true, cx)))
            .on_action(cx.listener(|v, _: &ScrollPageDown, _, cx| v.scroll_page(false, cx)))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_modifiers_changed(cx.listener(Self::modifiers_changed))
            .on_hover(cx.listener(Self::hover_changed))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Right, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Middle, cx.listener(Self::mouse_up))
            .on_drop(cx.listener(Self::drop_paths))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .cursor(if self.hover_link.is_some() { CursorStyle::PointingHand } else { CursorStyle::IBeam })
            .size_full()
            .bg(bg)
            .px(padding_x)
            .pt(padding_top)
            .map(|d| look.apply(d))
            .relative()
            .children(backdrop)
            .child(TerminalElement { view: cx.entity() })
    }
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let text = self.marked_text.as_deref().unwrap_or("");
        let utf16: Vec<u16> = text.encode_utf16().collect();
        let r = range.start.min(utf16.len())..range.end.min(utf16.len());
        *actual = Some(r.clone());
        Some(String::from_utf16_lossy(&utf16[r]))
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        let len = self.marked_text.as_deref().map_or(0, |t| t.encode_utf16().count());
        Some(UTF16Selection { range: len..len, reversed: false })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_text.as_deref().map(|t| 0..t.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked_text = None;
        cx.notify();
    }

    fn replace_text_in_range(&mut self, _: Option<Range<usize>>, text: &str, _: &mut Window, cx: &mut Context<Self>) {
        self.marked_text = None;
        if !text.is_empty() {
            self.write_input(text.as_bytes().to_vec(), cx);
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = (!text.is_empty()).then(|| text.to_owned());
        cx.notify();
    }

    fn bounds_for_range(&mut self, _: Range<usize>, _: Bounds<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        self.last_cursor
    }

    fn character_index_for_point(&mut self, _: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        None
    }

    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        self.exited.is_none()
    }
}

// ---- element ------------------------------------------------------------

fn rgb_to_hsla(c: Rgb) -> Hsla {
    Rgba { r: c.r as f32 / 255., g: c.g as f32 / 255., b: c.b as f32 / 255., a: 1. }.into()
}

struct TerminalElement {
    view: Entity<TerminalView>,
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

struct TextSegment {
    origin: Point<Pixels>,
    line: ShapedLine,
}

struct Frame {
    rects: Vec<PaintQuad>,
    text: Vec<TextSegment>,
    cursor: Option<PaintQuad>,
    marked: Option<(PaintQuad, TextSegment)>,
    overlay: Option<TextSegment>,
    line_height: Pixels,
}

#[derive(Clone, Copy, PartialEq)]
struct Style {
    fg: Hsla,
    bold: bool,
    italic: bool,
    /// Underline color, and whether it is curly.
    underline: Option<(Hsla, bool)>,
    strike: bool,
}

/// Accumulates one row's text into force-width segments.
struct RowBuilder<'a> {
    font: &'a Font,
    font_size: Pixels,
    cell: CellMetrics,
    y: Pixels,
    origin_x: Pixels,
    text: String,
    runs: Vec<TextRun>,
    start_col: usize,
    next_col: usize,
}

impl<'a> RowBuilder<'a> {
    fn push(&mut self, col: usize, s: &str, style: Style, out: &mut Vec<TextSegment>, ts: &WindowTextSystem) {
        if !self.text.is_empty() && col != self.next_col {
            self.flush(out, ts);
        }
        if self.text.is_empty() {
            self.start_col = col;
        }
        let mut f = self.font.clone();
        if style.bold {
            f.weight = FontWeight::BOLD;
        }
        if style.italic {
            f.style = FontStyle::Italic;
        }
        let run = TextRun {
            len: s.len(),
            font: f,
            color: style.fg,
            background_color: None,
            underline: style.underline.map(|(color, wavy)| UnderlineStyle { thickness: px(1.), color: Some(color), wavy }),
            strikethrough: style.strike.then(|| StrikethroughStyle { thickness: px(1.), color: Some(style.fg) }),
        };
        match self.runs.last_mut() {
            Some(last)
                if last.font == run.font
                    && last.color == run.color
                    && last.underline == run.underline
                    && last.strikethrough == run.strikethrough =>
            {
                last.len += run.len
            },
            _ => self.runs.push(run),
        }
        self.text.push_str(s);
        self.next_col = col + 1;
    }

    fn flush(&mut self, out: &mut Vec<TextSegment>, ts: &WindowTextSystem) {
        if self.text.is_empty() {
            return;
        }
        let text: SharedString = std::mem::take(&mut self.text).into();
        let line = ts.shape_line(text, self.font_size, &self.runs, Some(self.cell.width));
        self.runs.clear();
        out.push(TextSegment { origin: point(self.origin_x + self.cell.width * self.start_col as f32, self.y), line });
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Frame;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = gpui_kit::Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Frame {
        self.view.update(cx, |view, cx| {
            let palette = view.sync_look(cx);
            let m = view.ensure_metrics(window);
            view.last_origin = bounds.origin;
            let cols = ((bounds.size.width / m.width) as u16).max(2);
            let rows = ((bounds.size.height / m.height) as u16).max(1);
            // Cell size in device pixels, for programs that draw images.
            let scale = window.scale_factor();
            let cell = |p: Pixels| (f32::from(p) * scale).round() as u16;
            let size = TermSize { cols, rows, cell_width: cell(m.width), cell_height: cell(m.height) };
            view.resize(size, cx);
            build_frame(view, &palette, bounds, m, window)
        })
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        frame: &mut Frame,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.view.read(cx).focus.clone();
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.view.clone()), cx);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for q in frame.rects.drain(..) {
                window.paint_quad(q);
            }
            if let Some(c) = frame.cursor.take() {
                window.paint_quad(c);
            }
            let lh = frame.line_height;
            for seg in &frame.text {
                let _ = seg.line.paint(seg.origin, lh, TextAlign::Left, None, window, cx);
            }
            if let Some((bg, seg)) = &frame.marked {
                window.paint_quad(bg.clone());
                let _ = seg.line.paint(seg.origin, lh, TextAlign::Left, None, window, cx);
            }
            if let Some(seg) = &frame.overlay {
                let _ = seg.line.paint(seg.origin, lh, TextAlign::Left, None, window, cx);
            }
        });
    }
}

fn build_frame(view: &mut TerminalView, palette: &Palette, bounds: Bounds<Pixels>, m: CellMetrics, window: &mut Window) -> Frame {
    let base_font = font(view.config.font_family.clone());
    let font_size = view.config.font_size;
    let focused = view.focus.is_focused(window);
    let ts = window.text_system().clone();
    let mut frame = Frame { rects: vec![], text: vec![], cursor: None, marked: None, overlay: None, line_height: m.height };

    let Some(session) = &view.session else {
        view.last_cursor = None;
        return frame;
    };
    let term = session.term().lock();
    let content = term.renderable_content();
    let colors = content.colors;
    let offset = content.display_offset as i32;
    let selection = content.selection;
    let cursor = content.cursor;
    let default_bg = palette.resolve(colors, tern_term::vt::vte::ansi::Color::Named(tern_term::vt::vte::ansi::NamedColor::Background));
    let sel_bg = rgb_to_hsla(palette.selection);
    let cursor_rgb = palette.cursor;
    let cursor_vp_line = cursor.point.line.0 + offset;
    let link = view.hover_link.as_ref().map(|h| h.start..=h.end);
    let block_cursor = focused && cursor.shape == CursorShape::Block;

    let mut row: i32 = -1;
    let mut builder: Option<RowBuilder> = None;
    let mut bg_run: Option<(i32, usize, usize, Hsla)> = None; // row, start, end(excl), color
    let flush_bg = |run: &mut Option<(i32, usize, usize, Hsla)>, rects: &mut Vec<PaintQuad>| {
        if let Some((r, s, e, c)) = run.take() {
            rects.push(fill(
                Bounds::new(
                    point(bounds.origin.x + m.width * s as f32, bounds.origin.y + m.height * r as f32),
                    size(m.width * (e - s) as f32, m.height),
                ),
                c,
            ));
        }
    };

    let mut buf = String::new();
    for indexed in content.display_iter {
        let vp_line = indexed.point.line.0 + offset;
        let col = indexed.point.column.0;
        if vp_line != row {
            if let Some(b) = builder.as_mut() {
                b.flush(&mut frame.text, &ts);
            }
            row = vp_line;
            builder = Some(RowBuilder {
                font: &base_font,
                font_size,
                cell: m,
                y: bounds.origin.y + m.height * row as f32,
                origin_x: bounds.origin.x,
                text: String::new(),
                runs: vec![],
                start_col: 0,
                next_col: 0,
            });
        }
        let cell = &indexed.cell;
        let flags = cell.flags;
        if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        let wide = flags.contains(Flags::WIDE_CHAR);
        let mut fg = palette.resolve(colors, cell.fg);
        let mut bg = palette.resolve(colors, cell.bg);
        if flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        let mut fg_h = rgb_to_hsla(fg);
        if flags.contains(Flags::DIM) {
            fg_h.a *= 0.66;
        }
        let selected = selection.is_some_and(|s| s.contains(indexed.point));
        let is_cursor = block_cursor && vp_line == cursor_vp_line && col == cursor.point.column.0;

        // Backgrounds.
        let cell_bg = if selected {
            Some(sel_bg)
        } else if bg != default_bg {
            Some(rgb_to_hsla(bg))
        } else {
            None
        };
        let span = if wide { 2 } else { 1 };
        match (&mut bg_run, cell_bg) {
            (Some((r, _, e, c)), Some(nc)) if *r == vp_line && *e == col && *c == nc => *e = col + span,
            (_, Some(nc)) => {
                flush_bg(&mut bg_run, &mut frame.rects);
                bg_run = Some((vp_line, col, col + span, nc));
            },
            (_, None) => flush_bg(&mut bg_run, &mut frame.rects),
        }

        if is_cursor {
            fg_h = rgb_to_hsla(default_bg);
        }
        let linked = link.as_ref().is_some_and(|r| r.contains(&indexed.point));
        let lined = flags.intersects(Flags::ALL_UNDERLINES | Flags::STRIKEOUT) || linked;
        // Blank cells only matter when they carry a line.
        if flags.contains(Flags::HIDDEN) || ((cell.c == ' ' || cell.c == '\0') && !lined) {
            continue;
        }
        buf.clear();
        buf.push(if cell.c == '\0' { ' ' } else { cell.c });
        if let Some(zw) = cell.zerowidth() {
            buf.extend(zw.iter());
        }
        let style = Style {
            fg: fg_h,
            bold: flags.contains(Flags::BOLD),
            italic: flags.contains(Flags::ITALIC),
            underline: (flags.intersects(Flags::ALL_UNDERLINES) || linked).then(|| {
                let color = cell.underline_color().map_or(fg_h, |c| rgb_to_hsla(palette.resolve(colors, c)));
                (color, flags.contains(Flags::UNDERCURL))
            }),
            strike: flags.contains(Flags::STRIKEOUT),
        };
        let b = builder.as_mut().unwrap();
        if wide {
            // Own segment so following cells stay on the grid.
            b.flush(&mut frame.text, &ts);
            b.push(col, &buf, style, &mut frame.text, &ts);
            b.flush(&mut frame.text, &ts);
        } else {
            b.push(col, &buf, style, &mut frame.text, &ts);
        }
    }
    flush_bg(&mut bg_run, &mut frame.rects);
    if let Some(b) = builder.as_mut() {
        b.flush(&mut frame.text, &ts);
    }

    // Cursor.
    let cursor_origin = point(bounds.origin.x + m.width * cursor.point.column.0 as f32, bounds.origin.y + m.height * cursor_vp_line as f32);
    let cursor_cell = Bounds::new(cursor_origin, size(m.width, m.height));
    view.last_cursor = Some(cursor_cell);
    let cursor_color = rgb_to_hsla(cursor_rgb);
    if view.exited.is_none() && cursor_vp_line >= 0 && cursor.shape != CursorShape::Hidden {
        frame.cursor = Some(match (focused, cursor.shape) {
            (true, CursorShape::Block) => fill(cursor_cell, cursor_color),
            (_, CursorShape::Beam) => fill(Bounds::new(cursor_origin, size(px(2.), m.height)), cursor_color),
            (_, CursorShape::Underline) => {
                fill(Bounds::new(point(cursor_origin.x, cursor_origin.y + m.height - px(2.)), size(m.width, px(2.))), cursor_color)
            },
            _ => outline(cursor_cell, cursor_color, BorderStyle::Solid),
        });
    }
    drop(term);

    // IME marked text, drawn over the cursor.
    if let Some(marked) = view.marked_text.clone() {
        let fg = rgb_to_hsla(palette.foreground);
        let run = TextRun {
            len: marked.len(),
            font: base_font.clone(),
            color: fg,
            background_color: None,
            underline: Some(UnderlineStyle { thickness: px(1.), color: Some(fg), wavy: false }),
            strikethrough: None,
        };
        let line = ts.shape_line(marked.into(), font_size, &[run], None);
        let bg = fill(Bounds::new(cursor_origin, size(line.width.max(m.width), m.height)), rgb_to_hsla(default_bg));
        frame.cursor = None;
        frame.marked = Some((bg, TextSegment { origin: cursor_origin, line }));
    }

    if let Some(code) = view.exited {
        let msg = match (code, &view.spawn_error) {
            (None, Some(err)) if err == "background server stopped" => format!("[{err}] — press Enter to restart"),
            (_, Some(err)) => {
                format!("[could not start {}: {err}] — press Enter to retry", view.spawn_cfg.program.as_deref().unwrap_or("shell"))
            },
            (Some(c), None) => format!("[process exited with code {c}] — press Enter to restart"),
            (None, None) => "[process exited] — press Enter to restart".to_owned(),
        };
        let color = rgb_to_hsla(palette.dim_foreground);
        let run = TextRun { len: msg.len(), font: base_font.clone(), color, background_color: None, underline: None, strikethrough: None };
        let line = ts.shape_line(msg.into(), font_size, &[run], None);
        let y = bounds.origin.y + m.height * (cursor_vp_line.max(0) + 1) as f32;
        frame.overlay = Some(TextSegment { origin: point(bounds.origin.x, y.min(bounds.bottom() - m.height)), line });
    }
    frame
}

/// Drop C0/C1 control characters (keeping tab and newlines) so clipboard
/// content can't inject escape sequences, end a bracketed paste early, or send
/// signals like Ctrl-C/Ctrl-D.
fn sanitize_paste(text: &str) -> String {
    text.chars().filter(|&c| matches!(c, '\t' | '\n' | '\r') || !(c.is_control())).collect()
}

/// A path as a shell word, for PowerShell (the default shell) and cmd
/// alike: plain when it holds only safe characters, else double-quoted, as
/// cmd reads quotes, so `& | < > ^` stay literal there. In PowerShell `$`,
/// the backtick and the curly double quotes (which it also takes as quotes)
/// get a backtick, so nothing in a file name expands or ends the string.
#[cfg(windows)]
pub(crate) fn escape_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "\\/:._-".contains(c)) {
        return s.into_owned();
    }
    let mut out = String::from("\"");
    for c in s.chars() {
        if matches!(c, '$' | '`' | '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}') {
            out.push('`');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// A path as a shell word: backslash before every char a shell treats
/// specially, as Ghostty and iTerm2 do for dropped files.
#[cfg(not(windows))]
pub(crate) fn escape_path(path: &Path) -> String {
    let mut out = String::new();
    for c in path.to_string_lossy().chars() {
        if c.is_whitespace() || "\\'\"()[]{}<>|&;$!?*#`".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{escape_path, sanitize_paste};
    use std::path::Path;

    #[test]
    #[cfg(windows)]
    fn escapes_dropped_paths() {
        assert_eq!(escape_path(Path::new(r"C:\Users\a\x.png")), r"C:\Users\a\x.png");
        assert_eq!(escape_path(Path::new(r"C:\Users\a\Screen Shot (1).png")), r#""C:\Users\a\Screen Shot (1).png""#);
        assert_eq!(escape_path(Path::new(r"C:\it's $HOME & x")), r#""C:\it's `$HOME & x""#);
        assert_eq!(escape_path(Path::new("C:\\a\u{201d};calc;`.png")), "\"C:\\a`\u{201d};calc;``.png\"");
    }

    #[test]
    #[cfg(not(windows))]
    fn escapes_dropped_paths() {
        assert_eq!(escape_path(Path::new("/Users/a/Screen Shot (1).png")), r"/Users/a/Screen\ Shot\ \(1\).png");
        assert_eq!(escape_path(Path::new("/tmp/it's $HOME")), r"/tmp/it\'s\ \$HOME");
        assert_eq!(escape_path(Path::new("/tmp/Tiếng Việt.txt")), r"/tmp/Tiếng\ Việt.txt");
    }

    #[test]
    fn paste_strips_controls() {
        assert_eq!(sanitize_paste("a\x1b[201~b\x03c\u{9b}d\te\nf"), "a[201~bcd\te\nf");
        assert_eq!(sanitize_paste("Tiếng Việt 中文"), "Tiếng Việt 中文");
    }
}
