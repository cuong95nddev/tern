//! The UI's copy of a terminal that runs in the daemon.
//!
//! It parses the bytes the daemon's terminal parsed, in the same order and
//! with the same resizes ([`Marker`]), so its grid matches the daemon's. It
//! never answers the program (device attributes, color and size queries, the
//! OSC 7501 probe): the daemon does, once. Title, bell and clipboard events
//! come from here, as they did from an in-process session; a clipboard read
//! (OSC 52 `?`) too, as only the UI has the clipboard.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Osc52, Term, TermMode};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use serde::{Deserialize, Serialize};

use crate::osc::{OscScanner, OscSignal};
use crate::session::{ClipboardReply, EventSink, TermEvent, TermSize};
use crate::snapshot::{self, Snapshot};

const MAX_CLIPBOARD_STORE: usize = 16 << 20;
/// The most bytes held back while the UI has the terminal locked, before
/// [`Mirror::feed`] waits for the lock.
const MAX_DEFERRED: usize = 1 << 20;

/// Something that happened in the daemon's terminal between two runs of
/// bytes, applied by the mirror at the same point of the stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Marker {
    /// The terminal and its PTY were resized.
    Resized(TermSize),
    /// A synchronized update (DEC 2026) ended on its timeout.
    SyncFlush,
    /// The program exited.
    Exited(Option<i32>),
}

pub type MirrorTerm = Term<MirrorListener>;

#[derive(Clone)]
pub struct MirrorListener {
    wake: Arc<AtomicBool>,
    sink: EventSink,
}

impl MirrorListener {
    fn wakeup(&self) {
        if !self.wake.swap(true, Ordering::AcqRel) {
            (self.sink)(TermEvent::Wakeup);
        }
    }
}

impl EventListener for MirrorListener {
    fn send_event(&self, event: Event) {
        match event {
            Event::Wakeup => self.wakeup(),
            Event::Title(t) => (self.sink)(TermEvent::Title(t)),
            Event::ResetTitle => (self.sink)(TermEvent::ResetTitle),
            Event::Bell => (self.sink)(TermEvent::Bell),
            Event::ClipboardStore(_, text) if text.len() <= MAX_CLIPBOARD_STORE => (self.sink)(TermEvent::ClipboardStore(text)),
            Event::ClipboardLoad(_, reply) => (self.sink)(TermEvent::ClipboardLoad(ClipboardReply(reply))),
            // Replies to the program come from the daemon.
            _ => {},
        }
    }
}

pub struct Mirror {
    term: Arc<FairMutex<MirrorTerm>>,
    parser: Processor<StdSyncHandler>,
    scanner: OscScanner,
    listener: MirrorListener,
    /// The term's mode bits, as of the last bytes parsed: the UI reads them
    /// per key and mouse move without waiting for the term lock.
    mode: Arc<AtomicU32>,
    size: TermSize,
    scrollback: usize,
    /// Bytes not parsed yet: the UI had the terminal locked (it draws).
    deferred: Vec<u8>,
}

fn config(scrollback: usize) -> Config {
    Config { scrolling_history: scrollback, kitty_keyboard: crate::KITTY_KEYBOARD, osc52: Osc52::CopyPaste, ..Config::default() }
}

impl Mirror {
    pub fn new(size: TermSize, scrollback: usize, sink: EventSink) -> Self {
        let listener = MirrorListener { wake: Arc::new(AtomicBool::new(false)), sink };
        let term = Term::new(config(scrollback), &size, listener.clone());
        let mode = Arc::new(AtomicU32::new(term.mode().bits()));
        Self {
            term: Arc::new(FairMutex::new(term)),
            parser: Processor::new(),
            scanner: OscScanner::new(),
            listener,
            mode,
            size,
            scrollback,
            deferred: Vec::new(),
        }
    }

    pub fn term(&self) -> &Arc<FairMutex<MirrorTerm>> {
        &self.term
    }

    /// Set while a redraw is pending; the UI clears it when it draws.
    pub fn wake_flag(&self) -> &Arc<AtomicBool> {
        &self.listener.wake
    }

    pub fn size(&self) -> TermSize {
        self.size
    }

    /// See the `mode` field; read it with [`mode_of`].
    pub fn mode_flag(&self) -> &Arc<AtomicU32> {
        &self.mode
    }

    fn sync_mode(&self, term: &MirrorTerm) {
        self.mode.store(term.mode().bits(), Ordering::Release);
    }

    /// Parse `bytes`, or hold them back while the UI has the terminal
    /// locked, so one tab drawing never stalls the reader that feeds every
    /// tab. Held bytes go in with the next feed, or with [`Self::flush`].
    pub fn feed(&mut self, bytes: &[u8]) {
        if !self.deferred.is_empty() {
            self.deferred.extend_from_slice(bytes);
            if self.deferred.len() >= MAX_DEFERRED {
                self.flush();
            }
            return;
        }
        let term = self.term.clone();
        let Some(mut term) = term.try_lock_unfair() else {
            self.deferred.extend_from_slice(bytes);
            return;
        };
        self.parser.advance(&mut *term, bytes);
        self.sync_mode(&term);
        drop(term);
        self.parsed(bytes);
    }

    /// Parse the bytes [`Self::feed`] held back, waiting for the lock.
    pub fn flush(&mut self) {
        if self.deferred.is_empty() {
            return;
        }
        let bytes = std::mem::take(&mut self.deferred);
        {
            let mut term = self.term.lock();
            self.parser.advance(&mut *term, &bytes);
            self.sync_mode(&term);
        }
        self.parsed(&bytes);
        // Keep the room for the next burst.
        self.deferred = bytes;
        self.deferred.clear();
    }

    /// Whether [`Self::feed`] holds bytes back.
    pub fn has_deferred(&self) -> bool {
        !self.deferred.is_empty()
    }

    /// Signals and the redraw for bytes just parsed.
    fn parsed(&mut self, bytes: &[u8]) {
        let sink = &self.listener.sink;
        self.scanner.feed(bytes, |sig| {
            if sig != OscSignal::ProgramStatusProbe {
                sink(TermEvent::Signal(sig));
            }
        });
        // Bytes held for a synchronized update show when it ends.
        if self.parser.sync_bytes_count() < bytes.len() {
            self.listener.wakeup();
        }
    }

    /// Have the UI draw again (something it draws besides the grid changed).
    pub fn redraw(&self) {
        self.listener.wakeup();
    }

    pub fn marker(&mut self, marker: &Marker) {
        self.flush();
        match *marker {
            Marker::Resized(size) => {
                self.size = size;
                self.term.lock().resize(size);
                self.listener.wakeup();
            },
            Marker::SyncFlush => {
                let mut term = self.term.lock();
                self.parser.stop_sync(&mut *term);
                self.sync_mode(&term);
                drop(term);
                self.listener.wakeup();
            },
            Marker::Exited(code) => (self.listener.sink)(TermEvent::Exit(code)),
        }
    }

    /// Replace the terminal's state with `snap`.
    pub fn install(&mut self, snap: Snapshot) {
        let (size, title) = (snap.size, snap.title.clone());
        let mut parser = Processor::new();
        let mut term = Term::new(config(self.scrollback), &size, self.listener.clone());
        snapshot::restore(snap, &mut term, &mut parser);
        self.sync_mode(&term);
        *self.term.lock() = term;
        // Held bytes came before the snapshot: it has them.
        self.deferred.clear();
        self.parser = parser;
        self.scanner = OscScanner::new();
        self.size = size;
        if let Some(t) = title {
            (self.listener.sink)(TermEvent::Title(t));
        }
        self.listener.wakeup();
    }
}

/// The mode in a [`Mirror::mode_flag`].
pub fn mode_of(flag: &AtomicU32) -> TermMode {
    TermMode::from_bits_truncate(flag.load(Ordering::Acquire))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::status::ActivityStatus;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::Line;

    const SIZE: TermSize = TermSize { cols: 30, rows: 5, cell_width: 8, cell_height: 16 };

    fn mirror() -> (Mirror, Arc<Mutex<Vec<TermEvent>>>) {
        let events = Arc::new(Mutex::new(vec![]));
        let ev = events.clone();
        (Mirror::new(SIZE, 100, Arc::new(move |e| ev.lock().unwrap().push(e))), events)
    }

    fn text<L: EventListener>(term: &Term<L>) -> Vec<String> {
        let g = term.grid();
        (0..g.screen_lines() as i32).map(|l| g[Line(l)].into_iter().map(|c| c.c).collect::<String>().trim_end().to_owned()).collect()
    }

    #[test]
    fn parses_like_a_plain_term() {
        let (mut m, events) = mirror();
        let bytes = b"hello\r\n\x1b[1mbold\x1b[0m\r\n\x1b]2;title\x07";
        m.feed(bytes);
        let mut plain = Term::new(Config::default(), &SIZE, VoidListener);
        Processor::<StdSyncHandler>::new().advance(&mut plain, bytes);
        assert_eq!(text(&*m.term().lock()), text(&plain));
        let ev = events.lock().unwrap();
        assert!(ev.iter().any(|e| matches!(e, TermEvent::Wakeup)));
        assert!(ev.iter().any(|e| matches!(e, TermEvent::Title(t) if t == "title")));
    }

    #[test]
    fn never_answers_the_program() {
        let (mut m, events) = mirror();
        // Device attributes, cursor position, color query, OSC 7501 probe.
        m.feed(b"\x1b[c\x1b[6n\x1b]11;?\x07\x1b]7501;?\x07");
        let ev = events.lock().unwrap();
        assert!(ev.iter().all(|e| matches!(e, TermEvent::Wakeup)), "{ev:?}");
    }

    #[test]
    fn markers_apply_in_order() {
        let (mut m, events) = mirror();
        m.marker(&Marker::Resized(TermSize { cols: 50, ..SIZE }));
        assert_eq!(m.term().lock().columns(), 50);
        assert_eq!(m.size().cols, 50);
        m.marker(&Marker::Exited(Some(3)));
        assert!(events.lock().unwrap().iter().any(|e| matches!(e, TermEvent::Exit(Some(3)))));
    }

    #[test]
    fn install_shows_the_snapshot() {
        let mut src = Term::new(Config::default(), &SIZE, VoidListener);
        Processor::<StdSyncHandler>::new().advance(&mut src, b"from the daemon");
        let snap = snapshot::capture(&mut src, SIZE, Some("t".into()), ActivityStatus::None);
        let (mut m, events) = mirror();
        m.feed(b"old");
        m.install(snap);
        assert_eq!(text(&*m.term().lock())[0], "from the daemon");
        assert!(events.lock().unwrap().iter().any(|e| matches!(e, TermEvent::Title(t) if t == "t")));
    }

    #[test]
    fn holds_bytes_while_the_ui_draws() {
        let (mut m, _) = mirror();
        let term = m.term().clone();
        let guard = term.lock();
        m.feed(b"held ");
        m.feed(b"back");
        assert!(m.has_deferred());
        drop(guard);
        m.feed(b"!");
        assert!(m.has_deferred());
        m.flush();
        assert!(!m.has_deferred());
        assert_eq!(text(&*m.term().lock())[0], "held back!");
    }

    #[test]
    fn markers_and_snapshots_take_held_bytes_first() {
        let (mut m, _) = mirror();
        let term = m.term().clone();
        let guard = term.lock();
        m.feed(b"x");
        drop(guard);
        m.marker(&Marker::Resized(TermSize { cols: 50, ..SIZE }));
        assert_eq!(text(&*m.term().lock())[0], "x");
        let guard = term.lock();
        m.feed(b"stale");
        drop(guard);
        let mut src = Term::new(Config::default(), &SIZE, VoidListener);
        m.install(snapshot::capture(&mut src, SIZE, None, ActivityStatus::None));
        assert!(!m.has_deferred());
        assert_eq!(text(&*m.term().lock())[0], "");
    }

    #[test]
    fn mode_flag_follows_the_bytes() {
        let (mut m, _) = mirror();
        assert!(!mode_of(m.mode_flag()).contains(TermMode::BRACKETED_PASTE));
        m.feed(b"[?2004h");
        assert!(mode_of(m.mode_flag()).contains(TermMode::BRACKETED_PASTE));
        assert_eq!(mode_of(m.mode_flag()), *m.term().lock().mode());
        m.feed(b"[?2004l");
        assert!(!mode_of(m.mode_flag()).contains(TermMode::BRACKETED_PASTE));
    }
}
