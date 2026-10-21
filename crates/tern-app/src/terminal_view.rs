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
use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::*;
use tern_mux::client::{Connector, Foreground, Open, RemoteSession, ShownImage};
use tern_term::graphics::placeholder::{self, PLACEHOLDER, PlaceholderCell};
use tern_term::keys::{self, Key, KeyEventKind, LineEdit, ModifierKey, Mods};
use tern_term::mirror::MirrorTerm;
use tern_term::mouse::{self, MouseAction, MouseMods};
use tern_term::osc::OscSignal;
use tern_term::palette::Palette;
use tern_term::procs::AgentShell;
use tern_term::status::next_status;
use tern_term::vt::grid::{Dimensions, Scroll};
use tern_term::vt::index::{Column, Line, Point as GridPoint, Side};
use tern_term::vt::selection::{Selection, SelectionType};
use tern_term::vt::sync::FairMutex;
use tern_term::vt::term::TermMode;
use tern_term::vt::term::cell::Flags;
use tern_term::vt::vte::ansi::{ClearMode, CursorShape, Handler, Rgb};
use tern_term::{SpawnConfig, TermEvent, TermSize};

use gpui_kit::component::Sizable as _;
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};

use crate::agents::Agent;
use crate::links::{self, Link};
use crate::platform::bind;
use crate::terminal_find::{Matches, Query};

actions!(
    terminal,
    [
        Copy,
        Paste,
        Clear,
        ScrollPageUp,
        ScrollPageDown,
        SelectAll,
        CopyOrInterrupt,
        PasteOrQuote,
        LineStart,
        LineEnd,
        WordLeft,
        WordRight,
        DeleteWordLeft,
        DeleteToLineStart,
        DeleteToLineEnd,
        Find,
        FindNext,
        FindPrevious,
        CloseFind
    ]
);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        bind("cmd-c", Copy, Some("Terminal")),
        bind("cmd-v", Paste, Some("Terminal")),
        bind("cmd-k", Clear, Some("Terminal")),
        bind("cmd-a", SelectAll, Some("Terminal")),
        bind("cmd-f", Find, Some("Terminal")),
        bind("cmd-g", FindNext, Some("Terminal")),
        bind("cmd-shift-g", FindPrevious, Some("Terminal")),
        bind("shift-pageup", ScrollPageUp, Some("Terminal")),
        bind("shift-pagedown", ScrollPageDown, Some("Terminal")),
        // The root binds tab / shift-tab to move focus, and bindings run
        // before `key_down`: unbind them here so the shell gets the key
        // (completion, accepting a suggestion).
        bind("tab", NoAction, Some("Terminal")),
        bind("shift-tab", NoAction, Some("Terminal")),
    ]);
    // macOS text editing (⌘←, ⌥←, ⌘⌫, …) as readline keys. Off macOS
    // Ctrl+←/→ and Ctrl+Backspace already reach the program as keys.
    if cfg!(target_os = "macos") {
        let ctx = Some("Terminal");
        cx.bind_keys([
            KeyBinding::new("cmd-left", LineStart, ctx),
            KeyBinding::new("cmd-right", LineEnd, ctx),
            KeyBinding::new("alt-left", WordLeft, ctx),
            KeyBinding::new("alt-right", WordRight, ctx),
            KeyBinding::new("alt-backspace", DeleteWordLeft, ctx),
            KeyBinding::new("cmd-backspace", DeleteToLineStart, ctx),
            KeyBinding::new("cmd-delete", DeleteToLineEnd, ctx),
        ]);
    }
    if !cfg!(target_os = "macos") {
        cx.bind_keys([bind("ctrl-insert", Copy, Some("Terminal")), bind("shift-insert", Paste, Some("Terminal"))]);
    }
    // As in Windows Terminal: Ctrl+C copies a selection (else it is ^C) and
    // Ctrl+V pastes (else, with `terminal.ctrl_v_paste` off, it is ^V).
    if cfg!(target_os = "windows") {
        cx.bind_keys([bind("ctrl-c", CopyOrInterrupt, Some("Terminal")), bind("ctrl-v", PasteOrQuote, Some("Terminal"))]);
    }
}

pub use tern_term::status::ActivityStatus;

/// Find in the terminal (⌘F): see [`crate::terminal_find`].
struct FindBar {
    input: Entity<InputState>,
    query: Query,
    matches: Matches,
    /// Searching again after output, at most every [`FIND_REFRESH`].
    _refresh: Option<Task<()>>,
    _input: Subscription,
}

/// How long output waits before the open find bar searches again.
const FIND_REFRESH: Duration = Duration::from_millis(200);

/// What the grid marks for the find bar: its matches on screen.
struct FindPaint {
    matches: Vec<tern_term::vt::term::search::Match>,
    current: Option<tern_term::vt::term::search::Match>,
    background: Hsla,
    current_background: Hsla,
}

/// Decoded images not drawn for this long are released from the GPU atlas.
const IMAGE_CACHE_TTL: Duration = Duration::from_secs(10);
/// Most bytes of GPU-ready images kept per tab.
const IMAGE_CACHE_CAP: usize = 256 * 1024 * 1024;

/// The shortest time between two redraws for output.
const OUTPUT_FRAME: Duration = Duration::from_millis(16);

/// How many of an agent's bottom lines tell what it is doing.
const AGENT_SCREEN_LINES: usize = 15;

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
    /// The program starts again after its daemon was lost, in its last
    /// folder and below what it showed.
    Restored,
    /// A click on an inline image, written out as this PNG.
    PreviewImage(PathBuf),
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
    /// Ctrl+V pastes (Windows); off, the terminal gets ^V.
    pub ctrl_v_paste: bool,
    /// Programs may read the clipboard (OSC 52).
    pub clipboard_read: bool,
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
    /// The session the last start asked to attach to (its id isn't the
    /// session's until the daemon answers).
    attach_asked: Option<u64>,
    /// What the tab showed when its daemon was lost, and at what size: the
    /// next program starts below it.
    lost: Option<(Arc<FairMutex<MirrorTerm>>, TermSize)>,
    /// The folder the shell was last seen in.
    last_cwd: Option<PathBuf>,
    /// The folder the shell last told (OSC 7).
    reported_cwd: Option<PathBuf>,
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
    /// The title the program set, as it set it (spinner and all).
    raw_title: SharedString,
    /// `raw_title` without an agent's spinner: what the tab shows.
    title: SharedString,
    default_title: SharedString,
    status: ActivityStatus,
    /// The program has reported its status (OSC 7501): `status` is its word,
    /// not a guess.
    reports_status: bool,
    /// `status` follows Claude Code's session file, not its OSC 7501 reports.
    status_from_file: bool,
    /// When `status` last changed.
    status_since: Instant,
    /// Its last turn ended and its tab has been seen since: no longer
    /// `Done`, but done.
    seen_done: bool,
    /// Reads in a row that found a working agent not working.
    quiet_reads: u8,
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
    /// A phone viewing the session has it: the PTY takes its size and
    /// typing here is dropped (by the daemon) until taken back.
    remote: bool,
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
    /// Paths looked up for links and where they resolved (newest last), so
    /// moving over the same words doesn't search the disk again. Dropped
    /// when ⌘ goes down: files may have come or gone since.
    resolved: RefCell<Vec<(String, Option<PathBuf>)>>,
    /// A path hovered but not in `resolved`: `update_hover` looks it up in
    /// the background.
    unresolved: RefCell<Option<String>>,
    /// The path being looked up, and the task doing it.
    resolving: Option<(String, Task<()>)>,
    /// ⌘ is held (as of the last `update_hover`).
    cmd_held: bool,
    /// The modifiers down as of the last change: a lone press or release
    /// is a key event to a program that wants all keys (Kitty flag 8).
    mods_down: Modifiers,
    /// [`Ticker`] ticks since the program last wrote (after a quiet 10s).
    idle_ticks: u32,
    /// The coding agent running in the foreground, if any.
    agent: Option<Agent>,
    /// The shells `agent` runs commands in.
    shells: Vec<AgentShell>,
    /// When the program last wrote something.
    last_output: Instant,
    /// When the grid was last drawn, and the redraw held back to keep
    /// output at [`OUTPUT_FRAME`] (see `on_term_event`).
    last_draw: Instant,
    _redraw: Option<Task<()>>,
    /// The find bar, while it is open.
    find: Option<FindBar>,
    /// GPU-ready copies of the images on screen (kitty graphics).
    images: DrawnImages,
    /// Tells this tab's sessions apart: a new one reuses image ids.
    session_serial: u64,
    /// Where images show in the last frame, and which.
    image_hits: Vec<(Bounds<Pixels>, u32)>,
    /// The mouse is over an image a click would open.
    hover_image: bool,
    _pump: Option<Task<()>>,
    /// Connecting to the daemon, before the session opens.
    _connect: Option<Task<()>>,
    _watch: Task<()>,
}

impl EventEmitter<TerminalEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// One timer for every tab, rather than one each: a single wakeup per
/// second, and the tabs' foreground programs read together, so one scan of
/// every process serves them all. Runs while there are tabs.
#[derive(Default)]
struct Ticker {
    views: Vec<WeakEntity<TerminalView>>,
    running: bool,
}

impl Global for Ticker {}

impl Ticker {
    fn add(view: WeakEntity<TerminalView>, cx: &mut App) {
        let t = cx.default_global::<Ticker>();
        t.views.push(view);
        if std::mem::replace(&mut t.running, true) {
            return;
        }
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let probes = cx.update(|cx| {
                    let mut views = std::mem::take(&mut cx.global_mut::<Ticker>().views);
                    let probes: Vec<_> =
                        views.iter().filter_map(|v| Some((v.clone(), v.update(cx, |view, cx| view.tick(cx)).ok()??))).collect();
                    views.retain(|v| v.upgrade().is_some());
                    let t = cx.global_mut::<Ticker>();
                    // Tabs added meanwhile are in `t.views`.
                    views.append(&mut t.views);
                    t.views = views;
                    t.running = !t.views.is_empty();
                    t.running.then_some(probes)
                });
                let Some(probes) = probes else { break };
                if probes.is_empty() {
                    continue;
                }
                let (views, fgs): (Vec<_>, Vec<_>) = probes.into_iter().unzip();
                // A shell's folder too, for starting it there again (a Claude
                // tab keeps its own: `--resume` needs it). An agent's shells
                // too, while it has one, and Claude Code's status from its
                // session file, when it runs.
                let found = cx
                    .background_spawn(async move {
                        let sessions = tern_term::status::claude_sessions_dirs();
                        fgs.into_iter()
                            .map(|(f, shell, agent)| {
                                let Some(f) = f else { return Default::default() };
                                let programs = f.programs();
                                let shells = if agent { f.agent_shells() } else { Vec::new() };
                                let claude = (Agent::detect(&programs) == Some(Agent::Claude)).then(|| f.pid()).flatten().and_then(|pid| {
                                    let dir = sessions.iter().find(|d| d.join(format!("{pid}.json")).is_file())?;
                                    tern_term::status::claude_session_status(dir, pid)
                                });
                                (programs, shell.then(|| f.cwd()).flatten(), shells, claude)
                            })
                            .collect::<Vec<_>>()
                    })
                    .await;
                cx.update(|cx| {
                    for (v, (programs, cwd, shells, claude)) in views.into_iter().zip(found) {
                        let _ = v.update(cx, |view, cx| {
                            if cwd.is_some() {
                                view.last_cwd = cwd;
                            }
                            view.set_agent(Agent::detect(&programs), cx);
                            view.set_shells(shells, cx);
                            view.set_session_file_status(claude, cx);
                        });
                    }
                });
            }
        })
        .detach();
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
            attach_asked: None,
            lost: None,
            last_cwd: None,
            reported_cwd: None,
            reattaching: false,
            attaching: false,
            output_since_ready: false,
            config,
            focus: cx.focus_handle(),
            raw_title: title.clone(),
            title: title.clone(),
            default_title: title,
            status: ActivityStatus::None,
            reports_status: false,
            status_from_file: false,
            status_since: Instant::now(),
            seen_done: false,
            quiet_reads: 0,
            exited: None,
            spawn_error: None,
            started: Instant::now(),
            spawned: false,
            reported_focus: false,
            remote: false,
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
            resolved: RefCell::new(Vec::new()),
            unresolved: RefCell::new(None),
            resolving: None,
            cmd_held: false,
            mods_down: Modifiers::default(),
            idle_ticks: 0,
            agent: None,
            shells: Vec::new(),
            last_output: Instant::now(),
            last_draw: Instant::now(),
            _redraw: None,
            find: None,
            images: DrawnImages::new(IMAGE_CACHE_CAP),
            session_serial: 0,
            image_hits: Vec::new(),
            hover_image: false,
            _pump: None,
            _connect: None,
            _watch: Task::ready(()),
        };
        this._watch = cx.spawn(async move |this, cx| {
            // A tab that isn't shown never lays out: start it anyway.
            cx.background_executor().timer(Duration::from_millis(150)).await;
            let _ = this.update(cx, |view, cx| view.start_once(cx));
        });
        Ticker::add(cx.entity().downgrade(), cx);
        this
    }

    /// One tick of [`Ticker`]. `Some` when this tab's foreground programs
    /// should be read: each tick while the program writes (an agent starting
    /// or ending shows) or an agent runs (its shells come and go quietly),
    /// else every fifth.
    /// The foreground to read now, if any, whether this is a shell tab, and
    /// whether to read the agent's shells.
    fn tick(&mut self, cx: &mut Context<Self>) -> Option<(Option<Foreground>, bool, bool)> {
        // Rendering clears the wakeup flag; a tab not shown never renders,
        // so clear it here or its output stops waking it.
        if let Some(s) = &self.session {
            s.take_wakeup();
        }
        self.read_agent_status(cx);
        let busy = self.last_output.elapsed() < Duration::from_secs(10);
        self.idle_ticks = if busy { 0 } else { self.idle_ticks + 1 };
        let agent = self.agent.is_some();
        (busy || agent || self.idle_ticks.is_multiple_of(5))
            .then(|| (self.session.as_ref().map(RemoteSession::foreground), !self.hook, agent))
    }

    fn start_once(&mut self, cx: &mut Context<Self>) {
        if !self.spawned {
            self.start(cx);
        }
    }

    /// Start (or restart) the process: attach to the saved background
    /// session the first time, else start a new one (ending the old one).
    /// After the daemon was lost, a new one starts where the old one was.
    pub fn start(&mut self, cx: &mut Context<Self>) {
        self.spawned = true;
        self.reported_focus = false;
        self.remote = false;
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
        self.session_serial += 1;
        self.attaching = self.attach.is_some();
        self.attach_asked = self.attach;
        // A shell starts in its last folder, below what it showed; a Claude
        // tab resumes (its spawn config says so) and draws its own.
        let lost = if self.attach.is_none() { self.lost.take() } else { None };
        let seed_from = match &lost {
            Some((term, size)) if !self.hook => {
                cfg.size = *size;
                if let Some(cwd) = self.last_cwd.clone().filter(|d| d.is_dir()) {
                    cfg.cwd = Some(cwd);
                }
                Some((term.clone(), *size))
            },
            _ => None,
        };
        let hook = self.hook;
        let attach = self.attach;
        let spawn_cfg = cfg.clone();
        let open = move |seed| match attach {
            Some(id) => Open::Attach { id },
            None => Open::Spawn { cfg: spawn_cfg, hook, seed },
        };
        self.exited = None;
        self.spawn_error = None;
        match (self.mux.ready(), seed_from) {
            (Some(client), None) => self.open_session(Ok(client), open(None), &cfg, sink),
            // Connecting may start the daemon and wait for it, and packing
            // the old screen takes a while: off the UI thread. The saved
            // session id stays until it is open.
            (ready, seed_from) => {
                let mux = self.mux.clone();
                let task = cx.background_spawn(async move {
                    let seed = seed_from.and_then(|(term, size)| pack_screen(&term, size));
                    let client = match ready {
                        Some(c) => Ok(c),
                        None => mux.get(),
                    };
                    (client, seed)
                });
                self._connect = Some(cx.spawn(async move |this, cx| {
                    let (client, seed) = task.await;
                    let _ = this.update(cx, |view, cx| {
                        view.open_session(client, open(seed), &cfg, sink);
                        cx.notify();
                    });
                }));
            },
        }
        if lost.is_some() {
            cx.emit(TerminalEvent::Restored);
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

    fn open_session(
        &mut self,
        client: std::io::Result<tern_mux::client::MuxClient>,
        open: Open,
        cfg: &tern_term::SpawnConfig,
        sink: tern_term::EventSink,
    ) {
        self.attach = None;
        match client {
            Ok(client) => self.session = Some(client.open(open, cfg.size, cfg.scrollback, sink)),
            Err(err) => {
                log::error!("background server unavailable: {err}");
                self.exited = Some(None);
                self.spawn_error = Some(format!("background server unavailable: {err}"));
            },
        }
    }

    /// The title to show: an agent's spinner frames left out (its status
    /// badge shows it working), so they don't redraw the tab bar.
    pub fn title(&self) -> &SharedString {
        &self.title
    }

    fn set_title(&mut self, title: SharedString, cx: &mut Context<Self>) {
        let label = strip_spinner(&title);
        if *label != *self.title {
            self.title = label.to_owned().into();
            cx.emit(TerminalEvent::TitleChanged);
        }
        self.raw_title = title;
    }

    pub fn status(&self) -> ActivityStatus {
        self.status
    }

    /// When `status` last changed: how long a turn has been working.
    pub fn status_since(&self) -> Instant {
        self.status_since
    }

    /// Whether its last turn ended and has been seen (see
    /// [`Self::clear_done_status`]).
    pub fn seen_done(&self) -> bool {
        self.seen_done
    }

    pub fn agent(&self) -> Option<Agent> {
        self.agent
    }

    /// The shells the agent runs commands in, oldest first.
    pub fn shells(&self) -> &[AgentShell] {
        &self.shells
    }

    /// Whether the program has settled for input: it turned on bracketed
    /// paste and has been quiet for `quiet`.
    #[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))] // for the screenshot tray
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
            "size={size:?} focus_reported={} focus_mode={} exited={:?} spawn_error={:?} history={} status={:?} from_file={} find={:?} offset={}",
            self.reported_focus,
            self.mode().contains(TermMode::FOCUS_IN_OUT),
            self.exited,
            self.spawn_error,
            self.session.as_ref().map_or(0, |s| s.term().lock().grid().history_size()),
            self.status,
            self.status_from_file,
            self.find.as_ref().map(|f| (f.query.text.clone(), f.matches.count(), f.matches.current_match().cloned())),
            self.session.as_ref().map_or(0, |s| s.term().lock().grid().display_offset()),
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
        // Not opened yet: it never will be.
        self._connect = None;
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

    fn set_shells(&mut self, shells: Vec<AgentShell>, cx: &mut Context<Self>) {
        let shells = if self.agent.is_some() { shells } else { Vec::new() };
        if shells != self.shells {
            self.shells = shells;
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
                self.refresh_find_soon(cx);
                // As Warp: output redraws at most every OUTPUT_FRAME. The
                // first after a pause draws at once (an echoed key shows
                // with no delay); a flood draws at 60 fps, not at a 120 Hz
                // display's rate.
                let since = self.last_draw.elapsed();
                if since >= OUTPUT_FRAME {
                    cx.notify();
                } else if self._redraw.is_none() {
                    let wait = OUTPUT_FRAME - since;
                    self._redraw = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(wait).await;
                        let _ = this.update(cx, |view, cx| {
                            view._redraw = None;
                            cx.notify();
                        });
                    }));
                }
            },
            TermEvent::Title(t) => {
                if *t != *self.raw_title {
                    self.set_title(t.into(), cx);
                    self.read_agent_status(cx);
                }
            },
            TermEvent::ResetTitle => self.set_title(self.default_title.clone(), cx),
            TermEvent::Bell => cx.emit(TerminalEvent::Bell),
            TermEvent::ClipboardStore(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            // Off by default, as in Warp: what was copied (a password, a
            // token) is the program's only if the user said so. A refused
            // read gets no answer, as in xterm.
            TermEvent::ClipboardLoad(reply) => {
                if cx.try_global::<TerminalLook>().is_some_and(|l| l.clipboard_read)
                    && let Some(s) = &self.session
                {
                    let text = cx.read_from_clipboard().and_then(|item| item.text()).unwrap_or_default();
                    s.write((reply.0)(&text).into_bytes());
                }
            },
            // Ended by a restart on purpose: wait for the connection to go,
            // then start again on the new daemon (`ServerLost`).
            TermEvent::Exit(_) if crate::restart::ended_by_restart(self.started, cx) => {},
            TermEvent::Exit(code) => {
                self.exited = Some(code);
                self.set_status(ActivityStatus::None, cx);
                let quick = self.started.elapsed() < Duration::from_secs(5);
                let live = !self.attaching || self.output_since_ready;
                cx.emit(TerminalEvent::Exited { code, quick, live });
                cx.notify();
            },
            TermEvent::Signal(sig @ OscSignal::ProgramStatus(_)) => {
                self.reports_status = true;
                // Claude Code's session file knows better when a turn has
                // ended (it may still wait for a background shell); its
                // reports are only quicker to tell one started.
                if let Some(status) = next_status(self.status, &sig)
                    && (!self.status_from_file || matches!(status, ActivityStatus::Working | ActivityStatus::Blocked))
                {
                    self.set_status(status, cx);
                }
            },
            TermEvent::Signal(OscSignal::Notify { title, body }) => {
                cx.emit(TerminalEvent::Notify { title, body });
            },
            // The shell's folder as it tells it (OSC 7): known where the
            // process's can't be read (Windows), and where links resolve.
            TermEvent::Signal(OscSignal::Cwd(dir)) => {
                self.last_cwd = Some(dir.clone());
                self.reported_cwd = Some(dir);
            },
            TermEvent::Signal(
                OscSignal::ProgramStatusProbe | OscSignal::ColorSchemeReports(_) | OscSignal::ColorSchemeQuery | OscSignal::VersionQuery,
            ) => {},
            TermEvent::Ready { token, status, .. } => {
                // The status it had while no one watched (e.g. Done): shown,
                // not notified again.
                if self.status != status {
                    self.status = status;
                    self.status_since = Instant::now();
                    self.seen_done = false;
                }
                self.reports_status |= status != ActivityStatus::None;
                self.reattaching = false;
                self.lost = None;
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
            TermEvent::RemoteControl(on) => {
                self.remote = on;
                cx.notify();
            },
            TermEvent::ServerLost => {
                self.remote = false;
                // The connection went (the daemon dropped it, died, or handed
                // its sessions to a new one): try the same session again once.
                let session = self.session.take();
                let id = session.as_ref().and_then(RemoteSession::id);
                if self.exited.is_none()
                    && id.is_some()
                    && let Some(s) = &session
                {
                    // What it showed, should its program have to start again.
                    self.lost = Some((s.term().clone(), s.size()));
                }
                drop(session);
                // Restarted on purpose: its program went with the daemon.
                if self.exited.is_none() && crate::restart::just_restarted(cx) {
                    self.reattaching = false;
                    return self.start(cx);
                }
                if self.exited.is_none() && !self.reattaching {
                    self.reattaching = true;
                    // No id: lost before the daemon answered (as when it hands
                    // off): ask again for what was asked.
                    self.attach = id.or(self.attach_asked);
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
            self.status_since = Instant::now();
            self.seen_done = false;
            cx.emit(TerminalEvent::StatusChanged(status));
        }
    }

    /// The status Claude Code's session file gives (see
    /// `tern_term::status::claude_session_status`), `None` when it has
    /// none: then its OSC 7501 reports alone tell.
    fn set_session_file_status(&mut self, status: Option<ActivityStatus>, cx: &mut Context<Self>) {
        self.status_from_file = status.is_some() && self.exited.is_none();
        let Some(status) = status.filter(|_| self.status_from_file) else { return };
        self.reports_status = true;
        // Done stays until its tab is seen.
        if !(status == ActivityStatus::None && self.status == ActivityStatus::Done) {
            self.set_status(status, cx);
        }
    }

    /// For an agent that doesn't report its status, read it off its title
    /// and screen (`Agent::read_screen`), else off its output.
    fn read_agent_status(&mut self, cx: &mut Context<Self>) {
        use ActivityStatus::*;
        if self.reports_status || self.exited.is_some() {
            return;
        }
        let Some(agent) = self.agent else {
            // It ended mid-turn: done.
            if matches!(self.status, Working | Blocked) {
                self.set_status(None, cx);
            }
            return;
        };
        let bottom = self.bottom_text(AGENT_SCREEN_LINES);
        let read = agent
            .read_screen(&self.raw_title, &bottom)
            .unwrap_or_else(|| if self.last_output.elapsed() < Duration::from_millis(1500) { Working } else { None });
        // A turn's spinner may pause: it ends on a second read.
        if read == None && self.status == Working {
            self.quiet_reads += 1;
            if self.quiet_reads < 2 {
                return;
            }
        }
        self.quiet_reads = 0;
        // Done stays until its tab is seen.
        if !(read == None && self.status == Done) {
            self.set_status(read, cx);
        }
    }

    /// The last `n` non-blank lines of the screen, lowercased.
    fn bottom_text(&self, n: usize) -> String {
        let Some(s) = &self.session else { return String::new() };
        let term = s.term().lock();
        let grid = term.grid();
        let mut lines: Vec<String> = (0..grid.screen_lines() as i32)
            .rev()
            .map(|l| grid[Line(l)].into_iter().map(|c| if c.c == ' ' { ' ' } else { c.c }).collect::<String>())
            .filter(|l| !l.trim().is_empty())
            .take(n)
            .collect();
        lines.reverse();
        lines
            .join(
                "
",
            )
            .to_lowercase()
    }

    pub fn clear_done_status(&mut self, cx: &mut Context<Self>) {
        // Still done, since the turn ended: only no longer news.
        if self.status == ActivityStatus::Done {
            self.status = ActivityStatus::None;
            self.seen_done = true;
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
        self.session.as_ref().map_or(TermMode::empty(), RemoteSession::mode)
    }

    /// The program speaks the Kitty keyboard protocol: ⌘ chords no binding
    /// took are keys to it (with the Super bit), as in kitty and Warp.
    fn kitty_keys(&self) -> bool {
        self.mode().intersects(TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_ALL_KEYS_AS_ESC)
    }

    // ---- keyboard -------------------------------------------------------

    /// The key a keystroke encodes as, or `None` for text that goes
    /// through the input handler (IMEs, dead keys).
    fn map_key(ks: &Keystroke, meta: bool, all_keys: bool) -> Option<Key> {
        let m = ks.modifiers;
        let all_keys = all_keys || m.platform;
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

    fn key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Keys typed in the find bar are the bar's.
        if !self.focus.is_focused(window) {
            return;
        }
        let ks = &ev.keystroke;
        if (ks.modifiers.platform && !self.kitty_keys()) || self.marked_text.is_some() {
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

    fn key_up(&mut self, ev: &KeyUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) {
            return;
        }
        let ks = &ev.keystroke;
        if (ks.modifiers.platform && !self.kitty_keys()) || self.marked_text.is_some() || self.exited.is_some() {
            return;
        }
        if self.mode().contains(TermMode::REPORT_EVENT_TYPES) {
            self.send_key(ks, KeyEventKind::Release, cx);
        }
    }

    fn send_key(&mut self, ks: &Keystroke, kind: KeyEventKind, cx: &mut Context<Self>) -> bool {
        let m = ks.modifiers;
        let meta = m.alt && self.config.option_as_meta.applies();
        let mode = self.mode();
        let Some(key) = Self::map_key(ks, meta, mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC)) else { return false };
        // "Option as Meta" only decides what Option does to text keys (a
        // composed char or Meta); on the others (⌥↑, ⌥Enter, ⌥F1) Option is
        // always the Alt modifier, as in Warp and Ghostty.
        let alt = if matches!(key, Key::Char(_)) { meta } else { m.alt };
        let mods = Mods { shift: m.shift, alt, ctrl: m.control, meta: m.platform };
        let text = ks.key_char.as_deref().filter(|_| !meta && !m.platform);
        match keys::encode_event(key, mods, text, mode, kind) {
            Some(bytes) => {
                self.write_input(bytes, cx);
                true
            },
            None => false,
        }
    }

    // ---- find -----------------------------------------------------------

    /// ⌘F: open the find bar (with the selection, if one line, as the
    /// query), or go back to its field.
    fn open_find(&mut self, _: &Find, window: &mut Window, cx: &mut Context<Self>) {
        let selected =
            self.session.as_ref().and_then(|s| s.term().lock().selection_to_string()).filter(|t| !t.is_empty() && !t.contains('\n'));
        if self.find.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find"));
            let sub = cx.subscribe_in(&input, window, |this, input, ev: &InputEvent, _, cx| match ev {
                // Enter reports a change too: search again only for new text.
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    if this.find.as_ref().is_some_and(|f| f.query.text != text) {
                        this.search(|q| q.text = text, cx);
                    }
                },
                InputEvent::PressEnter { shift, .. } => this.find_step(!*shift, cx),
                _ => {},
            });
            self.find = Some(FindBar { input, query: Query::default(), matches: Matches::default(), _refresh: None, _input: sub });
        }
        let find = self.find.as_ref().unwrap();
        if let Some(text) = selected {
            find.input.update(cx, |i, cx| i.set_value(text, window, cx));
        }
        find.input.update(cx, |i, cx| i.focus(window, cx));
        cx.notify();
    }

    fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.take().is_some() {
            window.focus(&self.focus, cx);
            cx.notify();
        }
    }

    /// Change the query and search again, showing the match nearest the
    /// bottom of the screen.
    fn search(&mut self, change: impl FnOnce(&mut Query), cx: &mut Context<Self>) {
        let (Some(find), Some(s)) = (&mut self.find, &self.session) else { return };
        change(&mut find.query);
        let mut term = s.term().lock();
        let bottom = GridPoint::new(Line(term.screen_lines() as i32 - 1 - term.grid().display_offset() as i32), term.last_column());
        find.matches = Matches::find(&term, &find.query, bottom);
        if let Some(m) = find.matches.current_match() {
            term.scroll_to_point(*m.start());
        }
        cx.notify();
    }

    /// Enter / ⌘G go up (older output), as in iTerm2: a search starts at the
    /// bottom.
    fn find_step(&mut self, up: bool, cx: &mut Context<Self>) {
        let (Some(find), Some(s)) = (&mut self.find, &self.session) else { return };
        if let Some(m) = find.matches.step(up) {
            s.term().lock().scroll_to_point(*m.start());
            cx.notify();
        }
    }

    /// New output moves and adds text: search again soon, on the same match.
    fn refresh_find_soon(&mut self, cx: &mut Context<Self>) {
        let Some(find) = &mut self.find else { return };
        if find.query.text.is_empty() || find._refresh.is_some() {
            return;
        }
        find._refresh = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FIND_REFRESH).await;
            let _ = this.update(cx, |view, cx| {
                let (Some(find), Some(s)) = (&mut view.find, &view.session) else { return };
                find._refresh = None;
                let at = find.matches.current_match().map(|m| *m.start());
                let term = s.term().lock();
                let bottom = GridPoint::new(Line(term.screen_lines() as i32 - 1), term.last_column());
                find.matches = Matches::find(&term, &find.query, bottom);
                if let Some(at) = at {
                    find.matches.keep_near(at);
                }
                cx.notify();
            });
        }));
    }

    /// The find bar's matches on screen, for the grid to mark.
    fn find_paint(&self, cx: &App) -> Option<FindPaint> {
        let find = self.find.as_ref()?;
        let s = self.session.as_ref()?;
        let (top, bottom) = {
            let term = s.term().lock();
            let offset = term.grid().display_offset() as i32;
            (Line(-offset), Line(term.screen_lines() as i32 - 1 - offset))
        };
        let shown = |m: &&tern_term::vt::term::search::Match| m.end().line >= top && m.start().line <= bottom;
        let look = crate::style::el("terminal.find", cx);
        Some(FindPaint {
            matches: find.matches.all.iter().filter(shown).cloned().collect(),
            current: find.matches.current_match().cloned(),
            background: look.color("match_background"),
            current_background: look.color("current_background"),
        })
    }

    /// A macOS text-editing chord. While an IME composes, or with no live
    /// program, the key goes on as usual.
    fn line_edit(&mut self, edit: LineEdit, cx: &mut Context<Self>) {
        if self.session.is_none() || self.marked_text.is_some() || self.exited.is_some() {
            return cx.propagate();
        }
        self.write_input(edit.bytes().to_vec(), cx);
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = &self.session else { return };
        if let Some(text) = s.term().lock().selection_to_string() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// Ctrl+C on Windows: copy the selection and drop it, or with none let
    /// the key reach the terminal as ^C.
    fn copy_or_interrupt(&mut self, _: &CopyOrInterrupt, _: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = &self.session else { return cx.propagate() };
        let text = {
            let mut term = s.term().lock();
            let text = term.selection_to_string().filter(|t| !t.is_empty());
            if text.is_some() {
                term.selection = None;
            }
            text
        };
        match text {
            Some(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                cx.notify();
            },
            None => cx.propagate(),
        }
    }

    /// Ctrl+V on Windows: paste, or let the key reach the terminal as ^V
    /// when the settings say so (Vim's block selection).
    fn paste_or_quote(&mut self, _: &PasteOrQuote, window: &mut Window, cx: &mut Context<Self>) {
        if cx.try_global::<TerminalLook>().is_none_or(|l| l.ctrl_v_paste) {
            self.paste(&Paste, window, cx);
        } else {
            cx.propagate();
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

    fn report_mouse(&mut self, action: MouseAction, button: mouse::MouseButton, pos: Point<Pixels>, mods: &Modifiers) -> bool {
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
        true
    }

    fn mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if ev.button == MouseButton::Left
            && ev.click_count == 1
            && let Some(id) = self.clickable_image(ev.position, ev.modifiers.secondary())
        {
            self.open_image(id, cx);
            return;
        }
        if ev.button == MouseButton::Left
            && ev.modifiers.secondary()
            && let Some(link) = self.link_at(ev.position, true)
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
        if self.report_mouse(MouseAction::Press, button, ev.position, &ev.modifiers) {
            self.mouse_button_down = Some(button);
            self.last_motion = self.grid_point(ev.position).map(|(c, r, _)| (c, r));
            return;
        }
        if button != mouse::MouseButton::Left {
            return;
        }
        let Some((col, row, side)) = self.grid_point(ev.position) else { return };
        // As Warp: a double click on a URL or path takes all of it, not the
        // word between its slashes and colons.
        let link = if ev.click_count == 2 { self.link_text_at(ev.position) } else { None };
        let Some(s) = &self.session else { return };
        let mut term = s.term().lock();
        if let Some((start, end, _)) = link {
            let mut sel = Selection::new(SelectionType::Simple, start, Side::Left);
            sel.update(end, Side::Right);
            term.selection = Some(sel);
            self.selecting = false;
            return cx.notify();
        }
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
                self.report_mouse(MouseAction::Motion, button, ev.position, &ev.modifiers);
            }
            return;
        }
        if ev.pressed_button.is_none() && self.mode().contains(TermMode::MOUSE_MOTION) {
            if cell != self.last_motion {
                self.last_motion = cell;
                self.report_mouse(MouseAction::Motion, mouse::MouseButton::None, ev.position, &ev.modifiers);
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
            self.report_mouse(MouseAction::Release, button, ev.position, &ev.modifiers);
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

    fn modifiers_changed(&mut self, ev: &ModifiersChangedEvent, window: &mut Window, cx: &mut Context<Self>) {
        let (old, new) = (std::mem::replace(&mut self.mods_down, ev.modifiers), ev.modifiers);
        let mode = self.mode();
        if mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC)
            && self.marked_text.is_none()
            && self.exited.is_none()
            && self.focus.is_focused(window)
        {
            let mods = Mods { shift: new.shift, alt: new.alt, ctrl: new.control, meta: new.platform };
            for (key, was, is) in [
                (ModifierKey::Shift, old.shift, new.shift),
                (ModifierKey::Ctrl, old.control, new.control),
                (ModifierKey::Alt, old.alt, new.alt),
                (ModifierKey::Super, old.platform, new.platform),
            ] {
                let kind = match (was, is) {
                    (false, true) => KeyEventKind::Press,
                    (true, false) => KeyEventKind::Release,
                    _ => continue,
                };
                // Straight to the program: holding ⌘ to copy keeps the
                // selection and the scroll position.
                if let (Some(bytes), Some(s)) = (keys::encode_event(Key::Modifier(key), mods, None, mode, kind), &self.session) {
                    s.write(bytes);
                }
            }
        }
        if ev.modifiers.secondary() && self.hover_link.is_none() {
            self.resolved.borrow_mut().clear();
        }
        self.update_hover(ev.modifiers.secondary(), cx);
    }

    fn hover_changed(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if !*hovered {
            self.mouse_pos = None;
            self.update_hover(false, cx);
        }
    }

    /// The image at `pos` a click opens: on ⌘-click, or on a plain click
    /// when the program doesn't take the mouse.
    fn clickable_image(&self, pos: Point<Pixels>, cmd: bool) -> Option<u32> {
        if !cmd && self.mode().intersects(TermMode::MOUSE_MODE) {
            return None;
        }
        image_at(&self.image_hits, pos)
    }

    /// Shows image `id` full size in a popup, written out as a PNG first.
    fn open_image(&mut self, id: u32, cx: &mut Context<Self>) {
        let Some(s) = &self.session else { return };
        let Some(img) = s.images().get(&id).map(|i| (i.width, i.height, i.rgba.clone(), i.generation)) else { return };
        let (width, height, rgba, generation) = img;
        // The user's own cache, not a shared temp folder another user could
        // have taken the name in first.
        let Some(dir) = crate::dirs::private_cache().map(|d| d.join("images")) else {
            return log::warn!("no home folder to write image {id} in");
        };
        let name = format!("{}-{}-{id}-{generation}", std::process::id(), self.session_serial);
        let task = cx.background_executor().spawn(async move { save_png(width, height, &rgba, &dir, &name) });
        cx.spawn(async move |this, cx| match task.await {
            Ok(path) => {
                let _ = this.update(cx, |_, cx| cx.emit(TerminalEvent::PreviewImage(path)));
            },
            Err(e) => log::warn!("cannot write image {id}: {e}"),
        })
        .detach();
    }

    /// Underline the link under the mouse while `cmd` is held.
    fn update_hover(&mut self, cmd: bool, cx: &mut Context<Self>) {
        self.cmd_held = cmd;
        let image = self.mouse_pos.is_some_and(|p| self.clickable_image(p, cmd).is_some());
        if image != self.hover_image {
            self.hover_image = image;
            cx.notify();
        }
        let link = if cmd { self.mouse_pos.and_then(|p| self.link_at(p, false)) } else { None };
        if link != self.hover_link {
            self.hover_link = link;
            cx.notify();
        }
        let Some(path) = self.unresolved.take() else { return };
        if self.resolving.as_ref().is_some_and(|(p, _)| *p == path) {
            return;
        }
        let Some(s) = &self.session else { return };
        // The disk (`exists`, a few levels of `read_dir`) and the foreground
        // process's cwd are read off the UI thread; the link shows when done.
        let fg = s.foreground();
        let cwd = self.spawn_cfg.cwd.clone();
        let p = path.clone();
        let key = path.clone();
        let task = cx.spawn(async move |this, cx| {
            let found = cx
                .background_spawn(async move {
                    let bases: Vec<PathBuf> = fg.cwd().into_iter().chain(cwd).collect();
                    links::resolve(&p, &bases)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.resolving = None;
                this.remember(key, found);
                if this.cmd_held {
                    this.update_hover(true, cx);
                }
            });
        });
        self.resolving = Some((path, task));
    }

    fn remember(&self, path: String, found: Option<PathBuf>) {
        const MAX_RESOLVED: usize = 64;
        let mut cache = self.resolved.borrow_mut();
        cache.retain(|(p, _)| *p != path);
        if cache.len() >= MAX_RESOLVED {
            cache.remove(0);
        }
        cache.push((path, found));
    }

    /// The link at `pos`: an OSC 8 hyperlink, a URL, or the path of a file
    /// or folder that exists. Text wrapped over several rows counts as one
    /// line. A path not looked up yet is looked up now if `wait`, else left
    /// in `unresolved` (no link for now).
    /// The link written at `pos` (an OSC 8 hyperlink, a URL or a path),
    /// found or not: its first and last cells and what it says.
    fn link_text_at(&self, pos: Point<Pixels>) -> Option<(GridPoint, GridPoint, Link)> {
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
        Some((points[range.start], points[range.end - 1], link))
    }

    fn link_at(&self, pos: Point<Pixels>, wait: bool) -> Option<HoverLink> {
        let s = self.session.as_ref()?;
        let (start, end, link) = self.link_text_at(pos)?;
        let target = match link {
            Link::Url(url) => Target::Url(url),
            Link::Path { path, line, col } => {
                let cached = self.resolved.borrow().iter().find(|(p, _)| *p == path).map(|(_, f)| f.clone());
                let found = match cached {
                    Some(found) => found,
                    None if wait => {
                        let bases: Vec<PathBuf> =
                            s.foreground_cwd().into_iter().chain(self.reported_cwd.clone()).chain(self.spawn_cfg.cwd.clone()).collect();
                        let found = links::resolve(&path, &bases);
                        self.remember(path, found.clone());
                        found
                    },
                    None => {
                        *self.unresolved.borrow_mut() = Some(path);
                        return None;
                    },
                };
                Target::Path { path: found?, line, col }
            },
        };
        Some(HoverLink { start, end, target })
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
            // One write for every notch.
            if mouse::wants(mode, MouseAction::Press, button)
                && let Some((col, row, _)) = self.grid_point(ev.position)
                && let Some(one) = mouse::encode(mode, MouseAction::Press, button, Self::mouse_mods(&ev.modifiers), col, row)
                && let Some(s) = &self.session
            {
                s.write(one.repeat(lines.unsigned_abs().min(10) as usize));
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
        self.last_draw = Instant::now();
        self.watch_focus(window, cx);
        let look = crate::style::el("terminal", cx);
        let opacity = look.num("background_opacity").clamp(0., 1.) * crate::style::window_opacity(&crate::style::looks(cx));
        let bg = rgb_to_hsla(self.sync_look(cx).background).opacity(opacity);
        let mut scope = crate::template::Scope::new("terminal")
            .var("background", bg)
            .var("find", self.find.is_some())
            .var("remote", self.remote)
            .slot("grid", TerminalElement { view: cx.entity() });
        if let Some(find) = &self.find {
            let q = &find.query;
            let m = &find.matches;
            scope = scope
                .var("count", if m.error { "Invalid".to_owned() } else { m.count() })
                .var("error", m.error)
                .var("case", q.case_sensitive)
                .var("word", q.whole_word)
                .var("regex", q.regex)
                .slot("input", Input::new(&find.input).appearance(false).small());
        }
        let mut frames = |name: &str, _, f: crate::template::Framed| match (name, f) {
            ("terminal", crate::template::Framed::Box(d)) => d
                .id("terminal")
                .track_focus(&self.focus)
                .key_context("Terminal")
                .on_key_down(cx.listener(Self::key_down))
                .on_key_up(cx.listener(Self::key_up))
                .on_action(cx.listener(Self::copy))
                .on_action(cx.listener(Self::paste))
                .on_action(cx.listener(Self::copy_or_interrupt))
                .on_action(cx.listener(Self::paste_or_quote))
                .on_action(cx.listener(Self::clear))
                .on_action(cx.listener(Self::select_all))
                .on_action(cx.listener(|v, _: &ScrollPageUp, _, cx| v.scroll_page(true, cx)))
                .on_action(cx.listener(|v, _: &ScrollPageDown, _, cx| v.scroll_page(false, cx)))
                .on_action(cx.listener(Self::open_find))
                .on_action(cx.listener(|v, _: &FindNext, _, cx| v.find_step(true, cx)))
                .on_action(cx.listener(|v, _: &FindPrevious, _, cx| v.find_step(false, cx)))
                .on_action(cx.listener(|v, _: &CloseFind, window, cx| v.close_find(window, cx)))
                .on_action(cx.listener(|v, _: &LineStart, _, cx| v.line_edit(LineEdit::LineStart, cx)))
                .on_action(cx.listener(|v, _: &LineEnd, _, cx| v.line_edit(LineEdit::LineEnd, cx)))
                .on_action(cx.listener(|v, _: &WordLeft, _, cx| v.line_edit(LineEdit::WordLeft, cx)))
                .on_action(cx.listener(|v, _: &WordRight, _, cx| v.line_edit(LineEdit::WordRight, cx)))
                .on_action(cx.listener(|v, _: &DeleteWordLeft, _, cx| v.line_edit(LineEdit::DeleteWordLeft, cx)))
                .on_action(cx.listener(|v, _: &DeleteToLineStart, _, cx| v.line_edit(LineEdit::DeleteToLineStart, cx)))
                .on_action(cx.listener(|v, _: &DeleteToLineEnd, _, cx| v.line_edit(LineEdit::DeleteToLineEnd, cx)))
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
                .cursor(if self.hover_link.is_some() || self.hover_image { CursorStyle::PointingHand } else { CursorStyle::IBeam })
                .into_any_element(),
            // The field lets Escape through when it has nothing to undo.
            ("find", crate::template::Framed::Box(d)) => {
                d.on_action(cx.listener(|v, _: &Escape, window, cx| v.close_find(window, cx))).into_any_element()
            },
            ("case", crate::template::Framed::Button(b)) => {
                b.on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.search(|q| q.case_sensitive ^= true, cx))).into_any_element()
            },
            ("word", crate::template::Framed::Button(b)) => {
                b.on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.search(|q| q.whole_word ^= true, cx))).into_any_element()
            },
            ("regex", crate::template::Framed::Button(b)) => {
                b.on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.search(|q| q.regex ^= true, cx))).into_any_element()
            },
            ("take_back", crate::template::Framed::Button(b)) => b
                .on_click(cx.listener(|v, _: &ClickEvent, _, _| {
                    if let Some(s) = &v.session {
                        s.take_back();
                    }
                }))
                .into_any_element(),
            (_, f) => f.into_any_element(),
        };
        crate::template::render("terminal", scope, &mut frames, cx)
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

struct DrawnImage {
    generation: u64,
    data: Arc<RenderImage>,
    bytes: usize,
    used: Instant,
}

/// GPU-ready images by session and image id: one generation each (a
/// replaced image's old copy goes at once), released when unused for a
/// while or over the cap.
struct DrawnImages {
    entries: HashMap<(u64, u32), DrawnImage>,
    bytes: usize,
    cap: usize,
}

impl DrawnImages {
    fn new(cap: usize) -> Self {
        Self { entries: HashMap::new(), bytes: 0, cap }
    }

    /// The copy of image `id` at `img`'s generation; released copies go
    /// to `dropped`.
    fn get(&mut self, id: (u64, u32), img: &ShownImage, now: Instant, dropped: &mut Vec<Arc<RenderImage>>) -> Option<Arc<RenderImage>> {
        if let Some(e) = self.entries.get_mut(&id)
            && e.generation == img.generation
        {
            e.used = now;
            return Some(e.data.clone());
        }
        self.release(id, dropped);
        let data = render_image(img)?;
        self.bytes += img.rgba.len();
        self.entries.insert(id, DrawnImage { generation: img.generation, data: data.clone(), bytes: img.rgba.len(), used: now });
        Some(data)
    }

    /// Releases copies unused for [`IMAGE_CACHE_TTL`], then the least
    /// recently used while over the cap.
    fn sweep(&mut self, now: Instant, dropped: &mut Vec<Arc<RenderImage>>) {
        let expired: Vec<_> =
            self.entries.iter().filter(|(_, e)| now.duration_since(e.used) >= IMAGE_CACHE_TTL).map(|(id, _)| *id).collect();
        for id in expired {
            self.release(id, dropped);
        }
        while self.bytes > self.cap {
            let Some(id) = self.entries.iter().min_by_key(|(_, e)| e.used).map(|(id, _)| *id) else { break };
            self.release(id, dropped);
        }
    }

    fn release(&mut self, id: (u64, u32), dropped: &mut Vec<Arc<RenderImage>>) {
        if let Some(e) = self.entries.remove(&id) {
            self.bytes -= e.bytes;
            dropped.push(e.data);
        }
    }
}

/// GPUI wants BGRA.
fn render_image(img: &ShownImage) -> Option<Arc<RenderImage>> {
    let mut bgra = img.rgba.to_vec();
    for px in bgra.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    let buf = image::RgbaImage::from_raw(img.width, img.height, bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buf)])))
}

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

struct FrameImage {
    /// The placeholder cells' area: what of the image shows.
    clip: Bounds<Pixels>,
    /// Where the whole fitted image would be.
    bounds: Bounds<Pixels>,
    data: Arc<RenderImage>,
}

/// Consecutive placeholder cells of one image on one row.
struct PlaceholderRun {
    row: i32,
    start: usize,
    end: usize,
    first: PlaceholderCell,
    last: PlaceholderCell,
}

struct Frame {
    rects: Vec<PaintQuad>,
    images: Vec<FrameImage>,
    /// Cached images to release from the GPU atlas.
    dropped: Vec<Arc<RenderImage>>,
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

/// A segment's text and runs, shaped once the terminal is unlocked.
struct PendingText {
    origin: Point<Pixels>,
    text: String,
    runs: Vec<TextRun>,
}

/// Accumulates one row's text into force-width segments.
struct RowBuilder<'a> {
    /// The font, then bold, italic, and bold italic (see [`face`]).
    fonts: &'a [Font; 4],
    cell: CellMetrics,
    y: Pixels,
    origin_x: Pixels,
    text: String,
    runs: Vec<TextRun>,
    /// The face of the last run.
    face: usize,
    start_col: usize,
    next_col: usize,
}

/// Index of a style's font in [`RowBuilder::fonts`].
fn face(style: &Style) -> usize {
    usize::from(style.bold) | usize::from(style.italic) << 1
}

impl<'a> RowBuilder<'a> {
    fn push(&mut self, col: usize, s: &str, style: Style, out: &mut Vec<PendingText>) {
        if !self.text.is_empty() && col > self.next_col {
            self.pad(col - self.next_col);
        } else if !self.text.is_empty() && col != self.next_col {
            self.flush(out);
        }
        if self.text.is_empty() {
            self.start_col = col;
        }
        let face = face(&style);
        let underline = style.underline.map(|(color, wavy)| UnderlineStyle { thickness: px(1.), color: Some(color), wavy });
        let strikethrough = style.strike.then(|| StrikethroughStyle { thickness: px(1.), color: Some(style.fg) });
        match self.runs.last_mut() {
            Some(last)
                if self.face == face && last.color == style.fg && last.underline == underline && last.strikethrough == strikethrough =>
            {
                last.len += s.len()
            },
            _ => {
                self.runs.push(TextRun {
                    len: s.len(),
                    font: self.fonts[face].clone(),
                    color: style.fg,
                    background_color: None,
                    underline,
                    strikethrough,
                });
                self.face = face;
            },
        }
        self.text.push_str(s);
        self.next_col = col + 1;
    }

    /// Fill `n` skipped blank cells with spaces, so a row is one segment
    /// (one shaped line) rather than one per word.
    fn pad(&mut self, n: usize) {
        let Some(last) = self.runs.last_mut() else { return };
        if last.underline.is_none() && last.strikethrough.is_none() {
            last.len += n;
        } else {
            // The blanks carry no line.
            let plain = TextRun { len: n, underline: None, strikethrough: None, ..last.clone() };
            self.runs.push(plain);
        }
        self.text.extend(std::iter::repeat_n(' ', n));
        self.next_col += n;
    }

    fn flush(&mut self, out: &mut Vec<PendingText>) {
        if self.text.is_empty() {
            return;
        }
        out.push(PendingText {
            origin: point(self.origin_x + self.cell.width * self.start_col as f32, self.y),
            text: std::mem::take(&mut self.text),
            runs: std::mem::take(&mut self.runs),
        });
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
            let find = view.find_paint(cx);
            build_frame(view, &palette, bounds, m, find.as_ref(), window)
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
        for data in frame.dropped.drain(..) {
            let _ = window.drop_image(data);
        }
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for q in frame.rects.drain(..) {
                window.paint_quad(q);
            }
            for img in &frame.images {
                let _ = window.paint_image(img.clip, img.bounds, Corners::default(), img.data.clone(), 0, false);
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

fn build_frame(
    view: &mut TerminalView,
    palette: &Palette,
    bounds: Bounds<Pixels>,
    m: CellMetrics,
    find: Option<&FindPaint>,
    window: &mut Window,
) -> Frame {
    let base_font = font(view.config.font_family.clone());
    let fonts: [Font; 4] = std::array::from_fn(|i| {
        let mut f = base_font.clone();
        if i & 1 != 0 {
            f.weight = FontWeight::BOLD;
        }
        if i & 2 != 0 {
            f.style = FontStyle::Italic;
        }
        f
    });
    let font_size = view.config.font_size;
    let focused = view.focus.is_focused(window);
    let ts = window.text_system().clone();
    let mut frame = Frame {
        rects: vec![],
        images: vec![],
        dropped: vec![],
        text: vec![],
        cursor: None,
        marked: None,
        overlay: None,
        line_height: m.height,
    };

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

    // Shaping waits until the terminal is unlocked: its reader stays free.
    let mut pending: Vec<PendingText> = Vec::new();
    // Neighboring cells mostly share a color.
    let mut last_fg: Option<(Rgb, Hsla)> = None;
    let mut last_bg: Option<(Rgb, Hsla)> = None;
    let mut buf = String::new();
    let mut placeholders: Vec<PlaceholderRun> = Vec::new();
    for indexed in content.display_iter {
        let vp_line = indexed.point.line.0 + offset;
        let col = indexed.point.column.0;
        if vp_line != row {
            if let Some(b) = builder.as_mut() {
                b.flush(&mut pending);
            }
            row = vp_line;
            builder = Some(RowBuilder {
                fonts: &fonts,
                cell: m,
                y: bounds.origin.y + m.height * row as f32,
                origin_x: bounds.origin.x,
                text: String::new(),
                runs: vec![],
                face: 0,
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
        let mut fg_h = match last_fg {
            Some((rgb, h)) if rgb == fg => h,
            _ => {
                let h = rgb_to_hsla(fg);
                last_fg = Some((fg, h));
                h
            },
        };
        if flags.contains(Flags::DIM) {
            fg_h.a *= 0.66;
        }
        let selected = selection.is_some_and(|s| s.contains(indexed.point));
        let is_cursor = block_cursor && vp_line == cursor_vp_line && col == cursor.point.column.0;

        // Backgrounds.
        let found = find.and_then(|f| {
            if f.current.as_ref().is_some_and(|c| c.contains(&indexed.point)) {
                Some(f.current_background)
            } else {
                f.matches.iter().any(|r| r.contains(&indexed.point)).then_some(f.background)
            }
        });
        let cell_bg = if selected {
            Some(sel_bg)
        } else if found.is_some() {
            found
        } else if bg != default_bg {
            Some(match last_bg {
                Some((rgb, h)) if rgb == bg => h,
                _ => {
                    let h = rgb_to_hsla(bg);
                    last_bg = Some((bg, h));
                    h
                },
            })
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
        // A kitty graphics placeholder: part of an image, not text.
        if cell.c == PLACEHOLDER {
            let prev = placeholders.last().filter(|r| r.row == vp_line && r.end == col).map(|r| r.last);
            let marks = cell.zerowidth().unwrap_or(&[]);
            if let Some(ph) = PlaceholderCell::decode(cell.fg, cell.underline_color(), marks, prev.as_ref()) {
                match placeholders.last_mut() {
                    Some(r)
                        if r.row == vp_line
                            && r.end == col
                            && r.last.image_id == ph.image_id
                            && r.last.placement_id == ph.placement_id
                            && r.last.row == ph.row
                            && r.last.col + 1 == ph.col =>
                    {
                        r.end = col + 1;
                        r.last = ph;
                    },
                    _ => placeholders.push(PlaceholderRun { row: vp_line, start: col, end: col + 1, first: ph, last: ph }),
                }
            }
            continue;
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
            b.flush(&mut pending);
            b.push(col, &buf, style, &mut pending);
            b.flush(&mut pending);
        } else {
            b.push(col, &buf, style, &mut pending);
        }
    }
    flush_bg(&mut bg_run, &mut frame.rects);
    if let Some(b) = builder.as_mut() {
        b.flush(&mut pending);
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

    // Placeholder runs: the part of each image they show.
    view.image_hits.clear();
    if !placeholders.is_empty() {
        let shown = session.images();
        let (cw, ch) = (f32::from(m.width), f32::from(m.height));
        let now = Instant::now();
        for run in &placeholders {
            let Some(img) = shown.get(&run.first.image_id) else { continue };
            let (cols, rows) = img.placement(run.first.placement_id);
            let (cols, rows) = placeholder::box_cells(cols, rows, img.width, img.height, cw, ch);
            let (x, y, w, h) = placeholder::fit(cols, rows, cw, ch, img.width, img.height);
            let run_origin = point(bounds.origin.x + m.width * run.start as f32, bounds.origin.y + m.height * run.row as f32);
            let box_x = run_origin.x - m.width * run.first.col as f32;
            let box_y = run_origin.y - m.height * run.first.row as f32;
            let clip = Bounds::new(run_origin, size(m.width * (run.end - run.start) as f32, m.height));
            let at = Bounds::new(point(box_x + px(x), box_y + px(y)), size(px(w), px(h)));
            if let Some(data) = view.images.get((view.session_serial, run.first.image_id), img, now, &mut frame.dropped) {
                frame.images.push(FrameImage { clip, bounds: at, data });
                view.image_hits.push((clip, run.first.image_id));
            }
        }
    }
    view.images.sweep(Instant::now(), &mut frame.dropped);

    frame.text = pending
        .into_iter()
        .map(|p| TextSegment { origin: p.origin, line: ts.shape_line(p.text.into(), font_size, &p.runs, Some(m.width)) })
        .collect();

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

/// `term`'s screen and scrollback, packed as `Open::Spawn`'s seed.
fn pack_screen(term: &FairMutex<MirrorTerm>, size: TermSize) -> Option<Vec<u8>> {
    let snap = tern_term::snapshot::capture(&mut term.lock(), size, None, ActivityStatus::None);
    tern_mux::proto::pack_snapshot(&snap).map_err(|e| log::warn!("can't keep the old screen: {e}")).ok()
}

/// Drop C0/C1 control characters (keeping tab and newlines) so clipboard
/// content can't inject escape sequences, end a bracketed paste early, or send
/// signals like Ctrl-C/Ctrl-D.
fn sanitize_paste(text: &str) -> String {
    text.chars().filter(|&c| matches!(c, '\t' | '\n' | '\r') || !(c.is_control())).collect()
}

/// `title` without the spinner an agent leads it with: Claude's half
/// circles or braille dots while working and its asterisk when idle, or
/// Codex's dots.
fn strip_spinner(title: &str) -> &str {
    let mut chars = title.chars();
    match (chars.next(), chars.next()) {
        (
            Some('\u{25D0}'..='\u{25D3}' | '\u{2800}'..='\u{28FF}' | '\u{2722}' | '\u{2733}' | '\u{2736}' | '\u{273B}' | '\u{273D}'),
            Some(' '),
        ) => chars.as_str(),
        _ => title,
    }
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

/// The image shown at `pos`, from a frame's image areas.
fn image_at(hits: &[(Bounds<Pixels>, u32)], pos: Point<Pixels>) -> Option<u32> {
    hits.iter().find(|(b, _)| b.contains(&pos)).map(|(_, id)| *id)
}

/// Images written out for viewing are deleted after this long.
const SAVED_IMAGE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Writes RGBA pixels to `<dir>/<name>.png` unless that file exists, and
/// returns its path; images written there over a day ago go. The directory
/// and file are private: the image may show anything the program displayed.
fn save_png(width: u32, height: u32, rgba: &[u8], dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    use std::io;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)?;
    // A folder that is ours, not a link to another one.
    let meta = std::fs::symlink_metadata(dir)?;
    #[cfg(unix)]
    // SAFETY: `geteuid` has no preconditions.
    let ours = std::os::unix::fs::MetadataExt::uid(&meta) == unsafe { libc::geteuid() };
    #[cfg(not(unix))]
    let ours = true;
    if !meta.is_dir() || !ours {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "the images folder isn't a folder of ours"));
    }
    #[cfg(unix)]
    std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    // Only images written here go.
    for entry in std::fs::read_dir(dir)?.flatten() {
        let named = entry.file_name().to_str().is_some_and(is_saved_image_name);
        let old = entry.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|age| age > SAVED_IMAGE_TTL));
        if named && old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    let path = dir.join(format!("{name}.png"));
    if path.exists() {
        return Ok(path);
    }
    let buf = image::RgbaImage::from_raw(width, height, rgba.to_vec())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "pixel buffer does not match the size"))?;
    let mut png = io::Cursor::new(Vec::new());
    buf.write_to(&mut png, image::ImageFormat::Png).map_err(io::Error::other)?;
    // Written under a temporary name and renamed, so a half-written file is never opened.
    let tmp = dir.join(format!(".{name}.png.part"));
    // One left by a crash goes; a new one is created, never opened as found.
    let _ = std::fs::remove_file(&tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    io::Write::write_all(&mut opts.open(&tmp)?, png.get_ref())?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// A file name [`TerminalView::open_image`] gives: `<pid>-<serial>-<id>-<generation>.png`.
fn is_saved_image_name(name: &str) -> bool {
    name.strip_suffix(".png")
        .is_some_and(|stem| stem.split('-').count() == 4 && stem.split('-').all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())))
}

#[cfg(test)]
mod tests {
    use super::{escape_path, image_at, is_saved_image_name, sanitize_paste, save_png, strip_spinner};
    use gpui_kit::{Bounds, point, px, size};
    use std::path::Path;

    #[test]
    fn saved_image_names() {
        assert!(is_saved_image_name("123-1-42-7.png"));
        for other in ["notes.png", "1-2-3.png", "1-2-3-x.png", "1-2-3-4.jpg", "1--3-4.png"] {
            assert!(!is_saved_image_name(other), "{other}");
        }
    }

    #[test]
    fn image_at_finds_the_run_under_the_pointer() {
        let hits = [
            (Bounds::new(point(px(10.), px(0.)), size(px(20.), px(10.))), 7),
            (Bounds::new(point(px(10.), px(10.)), size(px(20.), px(10.))), 9),
        ];
        assert_eq!(image_at(&hits, point(px(12.), px(15.))), Some(9));
        assert_eq!(image_at(&hits, point(px(12.), px(2.))), Some(7));
        assert_eq!(image_at(&hits, point(px(5.), px(2.))), None);
    }

    #[test]
    fn writes_a_private_png_once() {
        let base = std::env::temp_dir().join(format!("tern-save-png-{}", std::process::id()));
        let dir = base.join("images");
        let rgba = [1, 2, 3, 255, 4, 5, 6, 128];
        let path = save_png(2, 1, &rgba, &dir, "1-2-7-1").unwrap();
        assert_eq!(path, dir.join("1-2-7-1.png"));
        let back = image::open(&path).unwrap().into_rgba8();
        assert_eq!((back.width(), back.height(), back.into_raw()), (2, 1, rgba.to_vec()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // Already written: kept as is.
        std::fs::write(&path, b"marker").unwrap();
        save_png(2, 1, &rgba, &dir, "1-2-7-1").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"marker");
        assert!(save_png(3, 1, &rgba, &dir, "bad").is_err(), "too few pixels");
        // One written over a day ago goes at the next write; other files stay.
        let other = dir.join("notes.png");
        std::fs::write(&other, b"x").unwrap();
        let a_day_ago = std::time::SystemTime::now() - super::SAVED_IMAGE_TTL * 2;
        for p in [&path, &other] {
            std::fs::File::options().write(true).open(p).unwrap().set_modified(a_day_ago).unwrap();
        }
        save_png(2, 1, &rgba, &dir, "1-2-8-1").unwrap();
        assert!(!path.exists() && other.exists() && dir.join("1-2-8-1.png").exists());
        #[cfg(unix)]
        {
            // A link in place of the folder is refused.
            let link = base.join("link");
            std::os::unix::fs::symlink(&dir, &link).unwrap();
            assert!(save_png(2, 1, &rgba, &link, "1-2-9-1").is_err());
            assert!(!dir.join("1-2-9-1.png").exists());
        }
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn strips_agent_spinners() {
        assert_eq!(strip_spinner("\u{280b} Fix the build"), "Fix the build");
        assert_eq!(strip_spinner("\u{2733} Fix the build"), "Fix the build");
        assert_eq!(strip_spinner("\u{25d0} x"), "x");
        assert_eq!(strip_spinner("\u{2733}"), "\u{2733}");
        assert_eq!(strip_spinner("vim main.rs"), "vim main.rs");
        assert_eq!(strip_spinner(""), "");
    }

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
