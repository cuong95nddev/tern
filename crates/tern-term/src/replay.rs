//! A snapshot as the bytes that paint it on a fresh terminal: what a remote
//! viewer (xterm.js in a browser) parses before the live output that follows
//! the snapshot. Scrollback, cells and their attributes, both screens, the
//! cursors, modes, palette overrides and the title come through; what the
//! snapshot lacks (see [`crate::snapshot`]) doesn't.

use std::fmt::Write as _;

use alacritty_terminal::grid::{Dimensions, Row};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::{Cell, Flags, Hyperlink};
use alacritty_terminal::vte::ansi::{Color, NamedColor};

use crate::snapshot::{CursorState, GridState, Snapshot, mode_sequences, style_sequences};

/// The bytes that turn a fresh terminal of `snap.size` into `snap`.
pub fn to_vt(snap: &Snapshot) -> Vec<u8> {
    let mut out = String::new();
    match &snap.primary {
        Some(primary) => {
            paint(&mut out, primary);
            cursors(&mut out, primary);
            // Saves the primary's cursor and clears; the cursor stays put.
            out += "\x1b[?1049h\x1b[H";
            paint(&mut out, &snap.grid);
        },
        None => paint(&mut out, &snap.grid),
    }
    out += "\x1b[0m";
    // After the cells (no wrap would cut them) and before the cursor (origin
    // mode homes it).
    let mode = TermMode::from_bits_truncate(snap.mode);
    out += &String::from_utf8_lossy(&mode_sequences(mode));
    cursors(&mut out, &snap.grid);
    out += &String::from_utf8_lossy(&style_sequences(snap.cursor_style, &snap.colors));
    if let Some(title) = &snap.title {
        let _ = write!(out, "\x1b]2;{}\x07", title.replace(['\x07', '\x1b'], ""));
    }
    out.into_bytes()
}

/// The grid's lines, scrollback first, so the last screenful ends up on the
/// screen.
fn paint(out: &mut String, gs: &GridState) {
    let grid = &gs.grid;
    let first = -(grid.history_size() as i32);
    let last = grid.screen_lines() as i32 - 1;
    let mut pen = Pen::default();
    for l in first..=last {
        let row = &grid[Line(l)];
        let wraps = row[Column(grid.columns() - 1)].flags.contains(Flags::WRAPLINE);
        paint_row(out, row, grid.columns(), wraps, &mut pen);
        if l != last && !wraps {
            out.push_str("\r\n");
        }
    }
    pen.finish(out);
}

fn paint_row(out: &mut String, row: &Row<Cell>, cols: usize, wraps: bool, pen: &mut Pen) {
    // Default blanks at the end of a line that doesn't wrap are left out.
    let end = if wraps { cols } else { (0..cols).rev().find(|&c| row[Column(c)] != Cell::default()).map_or(0, |c| c + 1) };
    for c in 0..end {
        let cell = &row[Column(c)];
        // A wide char's second half, and the blank a wide char left at the
        // end of the line it didn't fit on: the terminal makes both itself.
        if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        pen.set(out, cell);
        out.push(cell.c);
        if let Some(zw) = cell.zerowidth() {
            out.extend(zw);
        }
    }
}

/// The saved cursor, then the cursor with its pen.
fn cursors(out: &mut String, gs: &GridState) {
    place(out, &gs.saved_cursor, gs);
    out.push_str("\x1b7");
    place(out, &gs.cursor, gs);
}

fn place(out: &mut String, cur: &CursorState, gs: &GridState) {
    let (line, col) = (cur.point.line.0.max(0) as usize, cur.point.column.0);
    let _ = write!(out, "\x1b[{};{}H", line + 1, col + 1);
    if cur.input_needs_wrap {
        // Only writing the last column leaves the cursor waiting to wrap:
        // write it again.
        let cell = &gs.grid[cur.point.line][cur.point.column];
        if !cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER | Flags::WIDE_CHAR) {
            let mut pen = Pen::default();
            pen.set(out, cell);
            out.push(cell.c);
            pen.finish(out);
        }
    }
    out.push_str(&sgr(&cur.template));
}

/// The attributes written so far, so they change only between cells that
/// differ.
#[derive(Default)]
struct Pen {
    sgr: String,
    link: Option<Hyperlink>,
}

impl Pen {
    fn set(&mut self, out: &mut String, cell: &Cell) {
        let sgr = sgr(cell);
        if sgr != self.sgr {
            out.push_str(if sgr.is_empty() { "\x1b[0m" } else { &sgr });
            self.sgr = sgr;
        }
        let link = cell.hyperlink();
        if link != self.link {
            match &link {
                Some(l) => {
                    let _ = write!(out, "\x1b]8;id={};{}\x1b\\", l.id(), l.uri());
                },
                None => out.push_str("\x1b]8;;\x1b\\"),
            }
            self.link = link;
        }
    }

    fn finish(self, out: &mut String) {
        if self.link.is_some() {
            out.push_str("\x1b]8;;\x1b\\");
        }
        if !self.sgr.is_empty() {
            out.push_str("\x1b[0m");
        }
    }
}

/// The SGR sequence that sets a fresh pen to `cell`'s attributes.
fn sgr(cell: &Cell) -> String {
    let mut s = String::from("\x1b[0");
    let f = cell.flags;
    for (flag, code) in
        [(Flags::BOLD, "1"), (Flags::DIM, "2"), (Flags::ITALIC, "3"), (Flags::INVERSE, "7"), (Flags::HIDDEN, "8"), (Flags::STRIKEOUT, "9")]
    {
        if f.contains(flag) {
            s.push(';');
            s.push_str(code);
        }
    }
    let underline = [
        (Flags::UNDERLINE, "4"),
        (Flags::DOUBLE_UNDERLINE, "4:2"),
        (Flags::UNDERCURL, "4:3"),
        (Flags::DOTTED_UNDERLINE, "4:4"),
        (Flags::DASHED_UNDERLINE, "4:5"),
    ];
    if let Some((_, code)) = underline.iter().find(|(flag, _)| f.contains(*flag)) {
        s.push(';');
        s.push_str(code);
    }
    color(&mut s, cell.fg, 30);
    color(&mut s, cell.bg, 40);
    if let Some(c) = cell.underline_color() {
        color(&mut s, c, 50);
    }
    s.push('m');
    if s == "\x1b[0m" { String::new() } else { s }
}

/// `base` 30 for the foreground, 40 for the background, 50 for underlines
/// (which have no short codes).
fn color(s: &mut String, color: Color, base: u8) {
    let _ = match color {
        Color::Spec(rgb) => write!(s, ";{};2;{};{};{}", base + 8, rgb.r, rgb.g, rgb.b),
        Color::Indexed(i) => write!(s, ";{};5;{i}", base + 8),
        Color::Named(n) => {
            let idx = n as usize;
            let ansi = match n {
                _ if idx < 16 => Some(idx),
                NamedColor::DimBlack
                | NamedColor::DimRed
                | NamedColor::DimGreen
                | NamedColor::DimYellow
                | NamedColor::DimBlue
                | NamedColor::DimMagenta
                | NamedColor::DimCyan
                | NamedColor::DimWhite => Some(idx - NamedColor::DimBlack as usize),
                _ => None,
            };
            match ansi {
                Some(i) if base == 50 => write!(s, ";58;5;{i}"),
                Some(i) if i < 8 => write!(s, ";{}", base as usize + i),
                Some(i) => write!(s, ";{}", base as usize + 60 + i - 8),
                // The default color: a fresh pen has it.
                None => Ok(()),
            }
        },
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TermSize;
    use crate::snapshot::capture;
    use crate::status::ActivityStatus;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::{Config, Term};
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

    const SIZE: TermSize = TermSize { cols: 20, rows: 6, cell_width: 8, cell_height: 16 };

    fn term() -> Term<VoidListener> {
        Term::new(Config { scrolling_history: 1000, kitty_keyboard: true, ..Config::default() }, &SIZE, VoidListener)
    }

    fn feed(term: &mut Term<VoidListener>, bytes: &[u8]) {
        Processor::<StdSyncHandler>::new().advance(term, bytes);
    }

    /// Every line, scrollback too, with each cell's char and attributes.
    fn cells(term: &Term<VoidListener>) -> Vec<Vec<(char, Color, Color, Flags)>> {
        let g = term.grid();
        (-(g.history_size() as i32)..g.screen_lines() as i32)
            .map(|l| g[Line(l)].into_iter().map(|c| (c.c, c.fg, c.bg, c.flags)).collect())
            .collect()
    }

    /// `src` painted on a fresh terminal from its snapshot.
    fn replayed(src: &mut Term<VoidListener>) -> Term<VoidListener> {
        let snap = capture(src, SIZE, Some("t".into()), ActivityStatus::None);
        let mut dst = term();
        feed(&mut dst, &to_vt(&snap));
        dst
    }

    #[test]
    fn scrollback_cells_and_cursor() {
        let mut src = term();
        let text: String = (0..30).map(|i| format!("\x1b[3{}mline {i}\x1b[0m\r\n", i % 8)).collect();
        feed(&mut src, text.as_bytes());
        feed(&mut src, b"\x1b[1;4;48;2;1;2;3mbold\x1b[0m \x1b[38;5;200mx\x1b[7my\x1b[0m $ ");
        let dst = replayed(&mut src);
        assert_eq!(cells(&dst), cells(&src));
        assert_eq!(dst.grid().cursor.point, src.grid().cursor.point);
    }

    #[test]
    fn wrapped_and_wide_lines() {
        let mut src = term();
        // 25 chars on 20 columns wraps; a wide char that doesn't fit at the
        // end moves to the next line.
        feed(&mut src, "abcdefghijklmnopqrstuvwxy\r\n".as_bytes());
        feed(&mut src, "0123456789012345678界z\r\n".as_bytes());
        feed(&mut src, "e\u{301} 世界".as_bytes());
        let dst = replayed(&mut src);
        assert_eq!(cells(&dst), cells(&src));
        assert_eq!(dst.grid().cursor.point, src.grid().cursor.point);
    }

    #[test]
    fn pending_wrap_is_kept() {
        let mut src = term();
        feed(&mut src, b"\x1b[6;1H12345678901234567890");
        assert!(src.grid().cursor.input_needs_wrap);
        let mut dst = replayed(&mut src);
        assert!(dst.grid().cursor.input_needs_wrap);
        feed(&mut src, b"z");
        feed(&mut dst, b"z");
        assert_eq!(cells(&dst), cells(&src));
    }

    #[test]
    fn alternate_screen_and_modes() {
        let mut src = term();
        feed(&mut src, b"shell\r\n$ vim\r\n\x1b[?1049h\x1b[H\x1b[2J\x1b[2;3Hedit\x1b[?2004h\x1b[?1006h\x1b[?1002h\x1b[?25l");
        let mut dst = replayed(&mut src);
        assert_eq!(*dst.mode(), *src.mode());
        assert_eq!(cells(&dst), cells(&src));
        assert_eq!(dst.grid().cursor.point, src.grid().cursor.point);
        feed(&mut src, b"\x1b[?1049l");
        feed(&mut dst, b"\x1b[?1049l");
        assert_eq!(cells(&dst), cells(&src));
    }

    #[test]
    fn saved_cursor_and_pen() {
        let mut src = term();
        feed(&mut src, b"\x1b[3;5H\x1b[31m\x1b7\x1b[1;1H\x1b[32mgreen");
        let mut dst = replayed(&mut src);
        feed(&mut src, b"more\x1b8red");
        feed(&mut dst, b"more\x1b8red");
        assert_eq!(cells(&dst), cells(&src));
    }
}
