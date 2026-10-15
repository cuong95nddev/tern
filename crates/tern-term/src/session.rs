//! A running terminal session: PTY + child process + VT state.
//!
//! I/O runs on alacritty's dedicated PTY thread (non-blocking, handles DEC 2026
//! synchronized updates). We wrap the PTY so every byte read is also fed to the
//! [`OscScanner`] for Claude Code's status/notification sequences.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener, OnResize, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Osc52, Term};
use alacritty_terminal::tty::{self, ChildEvent, EventedPty, EventedReadWrite, Options, Pty, Shell};
use polling::{Event as PollEvent, PollMode, Poller};

use crate::osc::{OscScanner, OscSignal, PROGRAM_STATUS_PROBE_REPLY};
use crate::palette::Palette;
use crate::status::ActivityStatus;

/// Events delivered to the UI. `Wakeup` is coalesced: at most one is pending
/// until the consumer calls [`Session::take_wakeup`].
#[derive(Debug, Clone)]
pub enum TermEvent {
    Wakeup,
    Title(String),
    ResetTitle,
    Bell,
    ClipboardStore(String),
    Exit(Option<i32>),
    Signal(OscSignal),
    /// A daemon session is running (spawned or attached): its id, its hook
    /// token and the status it had.
    Ready { id: u64, token: Option<String>, status: ActivityStatus },
    /// The daemon couldn't start the program.
    SpawnError(String),
    /// The daemon has no session with that id.
    NotFound,
    /// The connection to the daemon is gone.
    ServerLost,
}

const MAX_CLIPBOARD_STORE: usize = 16 << 20;

pub type EventSink = Arc<dyn Fn(TermEvent) + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TermSize {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

impl TermSize {
    fn window_size(self) -> WindowSize {
        WindowSize { num_lines: self.rows, num_cols: self.cols, cell_width: self.cell_width, cell_height: self.cell_height }
    }
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }

    fn screen_lines(&self) -> usize {
        self.rows as usize
    }

    fn columns(&self) -> usize {
        self.cols as usize
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SpawnConfig {
    /// `None` = user's login shell.
    pub program: Option<String>,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: HashMap<String, String>,
    pub size: TermSize,
    pub scrollback: usize,
}

pub type SessionTerm = Term<Listener>;

/// State shared between the PTY thread (via [`Listener`]) and the UI.
struct Shared {
    wake_pending: AtomicBool,
    sender: OnceLock<EventLoopSender>,
    size: Mutex<TermSize>,
    palette: Palette,
    sink: EventSink,
}

impl Shared {
    fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        if let Some(s) = self.sender.get() {
            let _ = s.send(Msg::Input(bytes.into()));
        }
    }
}

#[derive(Clone)]
pub struct Listener {
    shared: Arc<Shared>,
    /// Weak-ish back reference for color queries (set after Term is built).
    term: Arc<OnceLock<Arc<FairMutex<SessionTerm>>>>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let sh = &self.shared;
        match event {
            Event::Wakeup => {
                if !sh.wake_pending.swap(true, Ordering::AcqRel) {
                    (sh.sink)(TermEvent::Wakeup);
                }
            },
            Event::PtyWrite(text) => sh.write(text.into_bytes()),
            Event::Title(t) => (sh.sink)(TermEvent::Title(t)),
            Event::ResetTitle => (sh.sink)(TermEvent::ResetTitle),
            Event::Bell => (sh.sink)(TermEvent::Bell),
            // A program can't flood the clipboard (OSC 52).
            Event::ClipboardStore(_, text) if text.len() <= MAX_CLIPBOARD_STORE => (sh.sink)(TermEvent::ClipboardStore(text)),
            Event::ClipboardStore(..) => {},
            Event::ColorRequest(idx, fmt) => {
                // Called from inside the parser while the term lock is held, so
                // read overrides through a try_lock and fall back to the palette.
                let rgb = self
                    .term
                    .get()
                    .and_then(|t| t.try_lock_unfair().map(|t| sh.palette.indexed(t.colors(), idx)))
                    .unwrap_or_else(|| sh.palette.indexed(&Default::default(), idx));
                sh.write(fmt(rgb).into_bytes());
            },
            Event::TextAreaSizeRequest(fmt) => {
                let size = *sh.size.lock().unwrap();
                sh.write(fmt(size.window_size()).into_bytes());
            },
            Event::ChildExit(status) => (sh.sink)(TermEvent::Exit(status.code())),
            Event::Exit => {},
            Event::ClipboardLoad(..) | Event::MouseCursorDirty | Event::CursorBlinkingChange => {},
        }
    }
}

/// PTY wrapper that taps reads through the OSC scanner.
struct TapPty {
    inner: Pty,
    reader: TapReader,
}

struct TapReader {
    file: File,
    scanner: OscScanner,
    shared: Arc<Shared>,
    /// Rate limit for probe replies: if the tty echoes our reply back as output
    /// (echoctl off), an unthrottled reply would loop forever.
    last_probe_reply: Option<Instant>,
}

const PROBE_REPLY_MIN_INTERVAL: Duration = Duration::from_millis(500);

impl Read for TapReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.file.read(buf)?;
        let shared = &self.shared;
        let file = &self.file;
        let last = &mut self.last_probe_reply;
        self.scanner.feed(&buf[..n], |sig| {
            if sig == OscSignal::ProgramStatusProbe {
                let now = Instant::now();
                if last.is_none_or(|t| now.duration_since(t) >= PROBE_REPLY_MIN_INTERVAL) {
                    *last = Some(now);
                    // Fixed bytes, tiny write on a non-blocking fd, ordering preserved.
                    let mut f = file;
                    let _ = f.write_all(PROGRAM_STATUS_PROBE_REPLY);
                }
            }
            (shared.sink)(TermEvent::Signal(sig));
        });
        Ok(n)
    }
}

impl EventedReadWrite for TapPty {
    type Reader = TapReader;
    type Writer = File;

    unsafe fn register(&mut self, poll: &Arc<Poller>, ev: PollEvent, mode: PollMode) -> io::Result<()> {
        unsafe { self.inner.register(poll, ev, mode) }
    }

    fn reregister(&mut self, poll: &Arc<Poller>, ev: PollEvent, mode: PollMode) -> io::Result<()> {
        self.inner.reregister(poll, ev, mode)
    }

    fn deregister(&mut self, poll: &Arc<Poller>) -> io::Result<()> {
        self.inner.deregister(poll)
    }

    fn reader(&mut self) -> &mut TapReader {
        &mut self.reader
    }

    fn writer(&mut self) -> &mut File {
        self.inner.writer()
    }
}

impl EventedPty for TapPty {
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        self.inner.next_child_event()
    }
}

impl OnResize for TapPty {
    fn on_resize(&mut self, size: WindowSize) {
        self.inner.on_resize(size)
    }
}

pub struct Session {
    term: Arc<FairMutex<SessionTerm>>,
    shared: Arc<Shared>,
    pid: u32,
    /// The PTY, to ask who runs in its foreground.
    tty: File,
}

impl Session {
    pub fn spawn(cfg: SpawnConfig, palette: Palette, sink: EventSink) -> io::Result<Self> {
        let shared =
            Arc::new(Shared { wake_pending: AtomicBool::new(false), sender: OnceLock::new(), size: Mutex::new(cfg.size), palette, sink });
        let term_cell = Arc::new(OnceLock::new());
        let listener = Listener { shared: shared.clone(), term: term_cell.clone() };

        let config = Config {
            scrolling_history: cfg.scrollback,
            kitty_keyboard: true,
            // Programs may set the clipboard (Claude Code's copy), never read it.
            osc52: Osc52::OnlyCopy,
            ..Config::default()
        };
        let term = Arc::new(FairMutex::new(Term::new(config, &cfg.size, listener.clone())));
        let _ = term_cell.set(term.clone());

        let mut env = cfg.env.clone();
        env.entry("TERM".into()).or_insert_with(|| "xterm-256color".into());
        env.entry("COLORTERM".into()).or_insert_with(|| "truecolor".into());
        env.entry("TERM_PROGRAM".into()).or_insert_with(|| "Tern".into());
        let opts = Options {
            shell: cfg.program.clone().map(|p| Shell::new(p, cfg.args.clone())),
            working_directory: cfg.cwd.clone(),
            drain_on_exit: true,
            env,
        };
        let pty = tty::new(&opts, cfg.size.window_size(), 0)?;
        let pid = pty.child().id();
        let tty = pty.file().try_clone()?;
        let reader =
            TapReader { file: pty.file().try_clone()?, scanner: OscScanner::new(), shared: shared.clone(), last_probe_reply: None };
        let event_loop = EventLoop::new(term.clone(), listener, TapPty { inner: pty, reader }, true, false)?;
        let _ = shared.sender.set(event_loop.channel());
        event_loop.spawn();

        Ok(Self { term, shared, pid, tty })
    }

    /// The VT state. Lock briefly (render prep only).
    pub fn term(&self) -> &Arc<FairMutex<SessionTerm>> {
        &self.term
    }

    pub fn palette(&self) -> &Palette {
        &self.shared.palette
    }

    /// Clears the pending-wakeup flag; returns whether one was pending.
    pub fn take_wakeup(&self) -> bool {
        self.shared.wake_pending.swap(false, Ordering::AcqRel)
    }

    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        self.shared.write(bytes);
    }

    pub fn resize(&self, size: TermSize) {
        {
            let mut cur = self.shared.size.lock().unwrap();
            if *cur == size {
                return;
            }
            *cur = size;
        }
        self.term.lock().resize(size);
        if let Some(s) = self.shared.sender.get() {
            let _ = s.send(Msg::Resize(size.window_size()));
        }
    }

    pub fn size(&self) -> TermSize {
        *self.shared.size.lock().unwrap()
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// See [`crate::procs::foreground_programs`].
    pub fn foreground_programs(&self) -> Vec<String> {
        crate::procs::foreground_programs(&self.tty)
    }

    /// See [`crate::procs::foreground_cwd`].
    pub fn foreground_cwd(&self) -> Option<PathBuf> {
        crate::procs::foreground_cwd(&self.tty)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(s) = self.shared.sender.get() {
            let _ = s.send(Msg::Shutdown);
        }
    }
}
