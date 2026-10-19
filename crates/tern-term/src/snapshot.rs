//! A terminal's state, captured in the daemon and rebuilt in the UI's mirror
//! when it attaches: the grids (with scrollback), modes, cursor style,
//! palette overrides and title. After the snapshot the UI parses the same
//! bytes the daemon does, so the two stay identical.
//!
//! Not captured (no public API): the scroll region, charsets, tab stops and a
//! half-parsed escape sequence. The daemon makes full-screen programs repaint
//! after an attach (see the host's "jiggle" resize), which hides these.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Cursor, Grid};
use alacritty_terminal::index::{Line, Point};
use alacritty_terminal::term::cell::Cell;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{CursorShape, NamedColor, Processor, StdSyncHandler};
use serde::{Deserialize, Serialize};

use crate::session::TermSize;
use crate::status::ActivityStatus;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub size: TermSize,
    /// The active grid.
    pub grid: GridState,
    /// The primary grid, when the alternate screen is active.
    pub primary: Option<GridState>,
    /// `TermMode` bits.
    pub mode: u32,
    /// DECSCUSR parameter, when not the default style.
    pub cursor_style: Option<u8>,
    /// Palette entries a program changed (OSC 4/10/11/12).
    pub colors: Vec<(usize, [u8; 3])>,
    pub title: Option<String>,
    pub status: ActivityStatus,
}

/// A grid with its cursors (alacritty doesn't serialize those).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GridState {
    pub grid: Grid<Cell>,
    pub cursor: CursorState,
    pub saved_cursor: CursorState,
}

/// A cursor without its charsets (not serializable; rarely not ASCII).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorState {
    pub point: Point,
    pub template: Cell,
    pub input_needs_wrap: bool,
}

impl CursorState {
    fn of(c: &Cursor<Cell>) -> Self {
        Self { point: c.point, template: c.template.clone(), input_needs_wrap: c.input_needs_wrap }
    }

    fn apply(&self, c: &mut Cursor<Cell>) {
        c.point = self.point;
        c.template = self.template.clone();
        c.input_needs_wrap = self.input_needs_wrap;
    }
}

impl GridState {
    fn of(grid: &Grid<Cell>) -> Self {
        Self { grid: grid.clone(), cursor: CursorState::of(&grid.cursor), saved_cursor: CursorState::of(&grid.saved_cursor) }
    }

    /// Put a copy back (the capture keeps it).
    fn put_back(&self, grid: &mut Grid<Cell>) {
        *grid = self.grid.clone();
        self.cursor.apply(&mut grid.cursor);
        self.saved_cursor.apply(&mut grid.saved_cursor);
    }

    /// Make it `grid`, moving its scrollback rather than copying it.
    fn install(self, grid: &mut Grid<Cell>) {
        *grid = self.grid;
        self.cursor.apply(&mut grid.cursor);
        self.saved_cursor.apply(&mut grid.saved_cursor);
    }
}

/// DECSET/DECRST numbers of the private modes a program can change.
const PRIVATE_MODES: &[(TermMode, u16)] = &[
    (TermMode::APP_CURSOR, 1),
    (TermMode::ORIGIN, 6),
    (TermMode::LINE_WRAP, 7),
    (TermMode::SHOW_CURSOR, 25),
    (TermMode::MOUSE_REPORT_CLICK, 1000),
    (TermMode::MOUSE_DRAG, 1002),
    (TermMode::MOUSE_MOTION, 1003),
    (TermMode::FOCUS_IN_OUT, 1004),
    (TermMode::UTF8_MOUSE, 1005),
    (TermMode::SGR_MOUSE, 1006),
    (TermMode::ALTERNATE_SCROLL, 1007),
    (TermMode::URGENCY_HINTS, 1042),
    (TermMode::BRACKETED_PASTE, 2004),
];

/// Kitty keyboard protocol flags, in `CSI = flags u` bit order.
const KITTY_FLAGS: &[TermMode] = &[
    TermMode::DISAMBIGUATE_ESC_CODES,
    TermMode::REPORT_EVENT_TYPES,
    TermMode::REPORT_ALTERNATE_KEYS,
    TermMode::REPORT_ALL_KEYS_AS_ESC,
    TermMode::REPORT_ASSOCIATED_TEXT,
];

/// Capture `term`. Leaves it as it was (the alternate screen is left and
/// re-entered to read the primary grid).
pub fn capture<L: EventListener>(term: &mut Term<L>, size: TermSize, title: Option<String>, status: ActivityStatus) -> Snapshot {
    let mode = *term.mode();
    let (grid, primary) = if mode.contains(TermMode::ALT_SCREEN) {
        let alt = GridState::of(term.grid());
        // Leaving the alternate screen keeps both grids; entering it again
        // clears it and moves the primary's saved cursor, so both are put
        // back afterwards.
        term.swap_alt();
        let primary = GridState::of(term.grid());
        term.swap_alt();
        alt.put_back(term.grid_mut());
        (alt, Some(primary))
    } else {
        (GridState::of(term.grid()), None)
    };
    let style = term.cursor_style();
    let cursor_style = match (style.shape, style.blinking) {
        (CursorShape::Block, false) => None,
        (CursorShape::Block, true) => Some(1),
        (CursorShape::Underline, true) => Some(3),
        (CursorShape::Underline, false) => Some(4),
        (CursorShape::Beam, true) => Some(5),
        (CursorShape::Beam, false) => Some(6),
        _ => None,
    };
    let colors = (0..=NamedColor::Cursor as usize).filter_map(|i| term.colors()[i].map(|c| (i, [c.r, c.g, c.b]))).collect();
    Snapshot { size, grid, primary, mode: mode.bits(), cursor_style, colors, title, status }
}

/// Rebuild `snap` into `term`, which must be new and of `snap.size`. Its
/// grids move into `term`.
pub fn restore<L: EventListener>(snap: Snapshot, term: &mut Term<L>, parser: &mut Processor<StdSyncHandler>) {
    let Snapshot { grid, primary, mode, cursor_style, colors, .. } = snap;
    let mode = TermMode::from_bits_truncate(mode);
    match primary {
        Some(primary) => {
            primary.install(term.grid_mut());
            parser.advance(term, b"\x1b[?1049h");
            grid.install(term.grid_mut());
        },
        None => grid.install(term.grid_mut()),
    }
    parser.advance(term, &mode_sequences(mode));
    parser.advance(term, &style_sequences(cursor_style, &colors));
}

/// The sequences that set a fresh terminal's cursor style and palette
/// overrides.
pub(crate) fn style_sequences(cursor_style: Option<u8>, colors: &[(usize, [u8; 3])]) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(n) = cursor_style {
        out.extend_from_slice(format!("\x1b[{n} q").as_bytes());
    }
    for &(i, [r, g, b]) in colors {
        let rgb = format!("rgb:{r:02x}/{g:02x}/{b:02x}");
        let seq = match i {
            i if i < 256 => format!("\x1b]4;{i};{rgb}\x07"),
            i if i == NamedColor::Foreground as usize => format!("\x1b]10;{rgb}\x07"),
            i if i == NamedColor::Background as usize => format!("\x1b]11;{rgb}\x07"),
            i if i == NamedColor::Cursor as usize => format!("\x1b]12;{rgb}\x07"),
            _ => continue,
        };
        out.extend_from_slice(seq.as_bytes());
    }
    out
}

/// Start `term` (new, of `snap.size`) with what an earlier program left on
/// screen: the primary grid with its scrollback, the visible lines scrolled
/// into history, and nothing else (modes, colors and the cursor style were
/// that program's).
pub fn seed<L: EventListener>(snap: Snapshot, term: &mut Term<L>, parser: &mut Processor<StdSyncHandler>) {
    let rows = snap.size.rows as usize;
    let grid = snap.primary.unwrap_or(snap.grid);
    // The lines in use: up to the cursor, or the last one with text below it.
    let used = (0..rows)
        .rev()
        .find(|&l| grid.grid[Line(l as i32)].into_iter().any(|c| c.c != ' ' && c.c != '\0'))
        .map_or(0, |l| l + 1)
        .max(grid.cursor.point.line.0.max(0) as usize + 1)
        .min(rows);
    grid.install(term.grid_mut());
    // From the bottom line each newline scrolls one line into history.
    let mut out = format!("\x1b[0m\x1b[{rows};1H").into_bytes();
    out.extend(std::iter::repeat_n(b'\n', used));
    out.extend_from_slice(b"\x1b[H");
    parser.advance(term, &out);
}

/// The sequences that set a fresh terminal's modes to `mode`.
pub(crate) fn mode_sequences(mode: TermMode) -> Vec<u8> {
    let mut out = String::new();
    let fresh = TermMode::default();
    for &(flag, n) in PRIVATE_MODES {
        if mode.contains(flag) != fresh.contains(flag) {
            out += &format!("\x1b[?{n}{}", if mode.contains(flag) { 'h' } else { 'l' });
        }
    }
    if mode.contains(TermMode::APP_KEYPAD) {
        out += "\x1b=";
    }
    if mode.contains(TermMode::INSERT) {
        out += "\x1b[4h";
    }
    if mode.contains(TermMode::LINE_FEED_NEW_LINE) {
        out += "\x1b[20h";
    }
    let kitty = KITTY_FLAGS.iter().enumerate().filter(|(_, f)| mode.contains(**f)).fold(0u8, |acc, (i, _)| acc | 1 << i);
    if kitty != 0 {
        out += &format!("\x1b[={kitty};1u");
    }
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::Line;
    use alacritty_terminal::term::Config;

    const SIZE: TermSize = TermSize { cols: 40, rows: 10, cell_width: 8, cell_height: 16 };

    fn term() -> Term<VoidListener> {
        Term::new(Config { scrolling_history: 1000, kitty_keyboard: true, ..Config::default() }, &SIZE, VoidListener)
    }

    fn feed(term: &mut Term<VoidListener>, bytes: &[u8]) {
        Processor::<StdSyncHandler>::new().advance(term, bytes);
    }

    fn screen(term: &Term<VoidListener>) -> Vec<String> {
        let g = term.grid();
        (0..g.screen_lines() as i32).map(|l| g[Line(l)].into_iter().map(|c| c.c).collect::<String>().trim_end().to_owned()).collect()
    }

    fn round_trip(src: &mut Term<VoidListener>) -> Term<VoidListener> {
        let snap = capture(src, SIZE, Some("t".into()), ActivityStatus::Working);
        // Through the wire format, as the daemon sends it.
        let bytes = serde_json::to_vec(&snap).unwrap();
        let snap: Snapshot = serde_json::from_slice(&bytes).unwrap();
        let mut dst = term();
        restore(snap, &mut dst, &mut Processor::new());
        dst
    }

    #[test]
    fn plain_round_trip() {
        let mut src = term();
        let text: String = (0..300).map(|i| format!("line {i}\r\n")).collect();
        feed(&mut src, text.as_bytes());
        feed(&mut src, b"\x1b[31mred\x1b[0m $ ");
        let dst = round_trip(&mut src);
        assert_eq!(screen(&dst), screen(&src));
        assert_eq!(dst.grid().history_size(), src.grid().history_size());
        assert_eq!(dst.grid().cursor.point, src.grid().cursor.point);
        assert_eq!(dst.grid()[Line(9)][alacritty_terminal::index::Column(0)], src.grid()[Line(9)][alacritty_terminal::index::Column(0)]);
    }

    #[test]
    fn alt_screen_round_trip() {
        let mut src = term();
        feed(&mut src, b"shell output\r\n$ vim\r\n");
        feed(&mut src, b"\x1b[?1049h\x1b[H\x1b[2Jeditor text");
        let mut dst = round_trip(&mut src);
        assert!(dst.mode().contains(TermMode::ALT_SCREEN));
        assert_eq!(screen(&dst), screen(&src));
        feed(&mut dst, b"\x1b[?1049l");
        feed(&mut src, b"\x1b[?1049l");
        assert_eq!(screen(&dst)[0], "shell output");
        assert_eq!(screen(&dst), screen(&src));
    }

    #[test]
    fn capture_keeps_source_intact() {
        let mut src = term();
        feed(&mut src, b"primary\r\n\x1b[?1049h\x1b[Halt");
        let before = (screen(&src), *src.mode(), src.grid().cursor.point);
        let _ = capture(&mut src, SIZE, None, ActivityStatus::None);
        assert_eq!((screen(&src), *src.mode(), src.grid().cursor.point), before);
        feed(&mut src, b"\x1b[?1049l");
        assert_eq!(screen(&src)[0], "primary");
    }

    #[test]
    fn modes_round_trip() {
        let mut src = term();
        feed(&mut src, b"\x1b[?2004h\x1b[?1006h\x1b[?1002h\x1b[?1004h\x1b[?1h\x1b=\x1b[?25l\x1b[=5;1u");
        let dst = round_trip(&mut src);
        assert_eq!(*dst.mode(), *src.mode());
    }

    #[test]
    fn seed_scrolls_the_old_screen_into_history() {
        let mut src = term();
        let text: String = (0..30).map(|i| format!("line {i}\r\n")).collect();
        feed(&mut src, text.as_bytes());
        feed(&mut src, b"$ \x1b[?2004h\x1b[31m");
        let snap = capture(&mut src, SIZE, None, ActivityStatus::None);
        let history = src.grid().history_size();
        let mut dst = term();
        seed(snap, &mut dst, &mut Processor::new());
        assert!(screen(&dst).iter().all(String::is_empty), "{:?}", screen(&dst));
        assert_eq!(dst.grid().cursor.point, Point::new(Line(0), alacritty_terminal::index::Column(0)));
        assert_eq!(dst.grid().history_size(), history + SIZE.rows as usize);
        assert_eq!(*dst.mode(), TermMode::default());
        // The prompt line is the last one in history.
        let last = &dst.grid()[Line(-1)];
        assert_eq!(last.into_iter().map(|c| c.c).collect::<String>().trim_end(), "$");
        // Its color was the old program's: new text is plain.
        feed(&mut dst, b"x");
        assert_eq!(dst.grid()[Line(0)][alacritty_terminal::index::Column(0)].fg, Cell::default().fg);
    }

    #[test]
    fn seed_leaves_the_alt_screen_and_skips_blank_lines() {
        let mut src = term();
        feed(&mut src, b"one\r\ntwo\r\n$ vim\r\n\x1b[?1049h\x1b[Heditor");
        let snap = capture(&mut src, SIZE, None, ActivityStatus::None);
        let mut dst = term();
        seed(snap, &mut dst, &mut Processor::new());
        assert!(!dst.mode().contains(TermMode::ALT_SCREEN));
        // Three lines and the cursor's (empty) line, not the whole screen.
        assert_eq!(dst.grid().history_size(), 4);
        assert_eq!(dst.grid()[Line(-4)].into_iter().map(|c| c.c).collect::<String>().trim_end(), "one");
    }

    #[test]
    fn colors_cursor_and_title() {
        let mut src = term();
        feed(&mut src, b"\x1b]4;1;rgb:12/34/56\x07\x1b]11;rgb:01/02/03\x07\x1b[5 q");
        let snap = capture(&mut src, SIZE, Some("t".into()), ActivityStatus::Done);
        assert_eq!(snap.title.as_deref(), Some("t"));
        assert_eq!(snap.status, ActivityStatus::Done);
        let dst = round_trip(&mut src);
        assert_eq!(dst.colors()[1], src.colors()[1]);
        assert_eq!(dst.colors()[NamedColor::Background], src.colors()[NamedColor::Background]);
        assert_eq!(dst.cursor_style(), src.cursor_style());
    }
}
