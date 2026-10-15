# Kitty Graphics (Unicode Placeholders) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Tern shows images sent with the kitty graphics protocol's Unicode-placeholder form, so Claude Code draws pasted images inline; a Claude Code mod previews pasted images above the prompt.

**Architecture:** A new `tern_term::graphics` module taps the raw PTY stream (next to the existing OSC tap in `TapReader`), parses `ESC _ G … ESC \` commands, loads/decodes images into a per-session `ImageStore` and writes replies back to the PTY. `TerminalView::build_frame` finds U+10EEEE placeholder cells in the alacritty grid, decodes image id/row/column from colour and diacritics, and paints the matching part of the image with `window.paint_image`.

**Tech Stack:** Rust 2024, alacritty_terminal 0.26, GPUI (gpui-kit 0.7.3 / gpui-pre 0.3.9), `image` 0.25 (PNG), `base64` 0.22, libc (shm); Claude Code mod in TSX.

**Spec:** `docs/superpowers/specs/2026-10-15-kitty-graphics-design.md`

## Global Constraints

- Scope: transmission `a=t`/`a=T`, `a=q`, `a=d` (`d=a/A/i/I/n/N`), `a=p` with `U=1`; formats `f=24/32/100`; media `t=d/f/t/s`; direct placements answered `ENOTSUPPORTED`; no animation, z-index, relative placements, `o=z`, sixel, OSC 1337.
- Caps: one APC command ≤ 16 MiB buffered; one (chunked) transmission ≤ 64 MiB of base64; file/shm read ≤ 64 MiB; image side ≤ 10000 px; decoded pixels per session ≤ 256 MiB, LRU eviction.
- `t=f`/`t=t`: absolute path, regular file, canonical path not under `/dev`, `/proc`, `/sys`. `t=t` deleted only if under a temp dir and its name contains `tty-graphics-protocol`.
- Replies only when the command named the image (`i` or `I`); `q=1` drops OK replies, `q=2` drops all.
- Setting: `TerminalConfig.graphics`, default `true`; `TERN_GRAPHICS=0` at Tern launch turns it off; when off no tap is installed.
- When on, child env gets `CLAUDE_CODE_FORCE_TERMINAL_IMAGES=1` unless already set. Tern never impersonates another terminal.
- Public repo: code and commits cite only the kitty spec (https://sw.kovidgoyal.net/kitty/graphics-protocol/), never other programs' internals.
- Gates before each commit: `cargo fmt --check`, `cargo clippy --workspace --all-targets` (only the accepted `block v0.1.6` future-incompat note), `cargo test --workspace`.
- Commit identity (already in repo config): `cuong95nddev <cuong95nd@gmail.com>`; commit trailer `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. Commands split across `read()` calls at any byte → still parsed once (Task 2 test `split_at_every_byte`).
2. A program that never terminates an APC string or floods it → bounded memory, no panic (Task 2 `oversized_command_keeps_its_control_data`, Task 5 chunk overflow logic).
3. Malicious `t=f` paths (devices, directories, relative paths, missing files) → refused with an error code, never a hang (Task 4 `file_refusals`).
4. Image replaced under the same id while on screen → redraw uses the new pixels (generation key; Task 5 `replace_changes_generation`).
5. Placeholder cells with omitted diacritics (inheritance) → correct row/column (Task 1 `inherits_from_the_left`).

---

### Task 0: Worktree

The main tree holds another session's uncommitted work; build in a separate worktree.

- [ ] **Step 1: Create the worktree from `main`**

```bash
cd ~/ws/tern-ws/tern
git worktree add -b kitty-graphics ../tern-kitty main
cd ../tern-kitty && git log --oneline -2
```
Expected: branch `kitty-graphics`, HEAD is the spec commit.

- [ ] **Step 2: Baseline**

Run: `cargo test --workspace 2>&1 | tail -5`
Expected: all tests pass.

All later paths are relative to `~/ws/tern-ws/tern-kitty`.

---

### Task 1: Placeholder decoding and placement geometry

**Files:**
- Create: `crates/tern-term/src/graphics/mod.rs` (module skeleton, `Error`)
- Create: `crates/tern-term/src/graphics/placeholder.rs`
- Modify: `crates/tern-term/src/lib.rs` (add `pub mod graphics;`)

**Interfaces:**
- Produces: `graphics::Error { code: &'static str, msg: String }`, `Error::new(code, msg)`; `placeholder::PLACEHOLDER: char`; `placeholder::diacritic_value(char) -> Option<u32>`; `placeholder::PlaceholderCell { image_id: u32, placement_id: u32, row: u32, col: u32 }` with `PlaceholderCell::decode(fg: Color, underline: Option<Color>, marks: &[char], prev: Option<&PlaceholderCell>) -> Option<PlaceholderCell>`; `placeholder::box_cells(cols, rows, img_w, img_h, cell_w: f32, cell_h: f32) -> (u32, u32)`; `placeholder::fit(cols, rows, cell_w, cell_h, img_w, img_h) -> (f32, f32, f32, f32)`.

- [ ] **Step 1: Module skeleton**

`crates/tern-term/src/graphics/mod.rs`:
```rust
//! Kitty graphics protocol, Unicode-placeholder subset
//! (https://sw.kovidgoyal.net/kitty/graphics-protocol/).
//!
//! Programs transmit images with `ESC _ G <control> ; <payload> ESC \` and show
//! them by printing U+10EEEE placeholder cells (see [`placeholder`]). Placements
//! floating over the grid ("direct placements") are not supported.

pub mod placeholder;

/// A protocol error, answered as `<code>:<msg>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub code: &'static str,
    pub msg: String,
}

impl Error {
    pub(crate) fn new(code: &'static str, msg: impl Into<String>) -> Self {
        Self { code, msg: msg.into() }
    }
}
```
Add `pub mod graphics;` to `crates/tern-term/src/lib.rs` (alphabetical, before `pub mod keys;`) and update its doc line to `//! Terminal core: PTY session, VT state, key encoding, OSC and graphics taps. No UI deps.`

- [ ] **Step 2: Write the failing tests** (bottom of `placeholder.rs`, file otherwise empty but for `use`s)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::vte::ansi::{NamedColor, Rgb};

    const R0: char = '\u{0305}';
    const R1: char = '\u{030D}';
    const R2: char = '\u{030E}';

    #[test]
    fn diacritic_table_is_sorted_and_complete() {
        assert_eq!(DIACRITICS.len(), 297);
        assert!(DIACRITICS.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(diacritic_value(R0), Some(0));
        assert_eq!(diacritic_value(R2), Some(2));
        assert_eq!(diacritic_value('\u{1D244}'), Some(296));
        assert_eq!(diacritic_value('a'), None);
    }

    #[test]
    fn decodes_id_row_and_column() {
        let fg = Color::Spec(Rgb { r: 0, g: 1, b: 2 });
        let ul = Some(Color::Indexed(9));
        let c = PlaceholderCell::decode(fg, ul, &[R1, R2], None).unwrap();
        assert_eq!(c, PlaceholderCell { image_id: 0x0102, placement_id: 9, row: 1, col: 2 });
    }

    #[test]
    fn third_diacritic_is_the_high_byte() {
        let c = PlaceholderCell::decode(Color::Indexed(5), None, &[R0, R0, R1], None).unwrap();
        assert_eq!(c.image_id, (1 << 24) | 5);
    }

    #[test]
    fn inherits_from_the_left() {
        let fg = Color::Indexed(7);
        let first = PlaceholderCell::decode(fg, None, &[R1, R2, R1], None).unwrap();
        let bare = PlaceholderCell::decode(fg, None, &[], Some(&first)).unwrap();
        assert_eq!(bare, PlaceholderCell { image_id: (1 << 24) | 7, placement_id: 0, row: 1, col: 3 });
        let row_only = PlaceholderCell::decode(fg, None, &[R1], Some(&bare)).unwrap();
        assert_eq!((row_only.row, row_only.col), (1, 4));
        // Another image to the left: nothing is inherited.
        let other = PlaceholderCell::decode(Color::Indexed(8), None, &[], Some(&first)).unwrap();
        assert_eq!((other.image_id, other.row, other.col), (8, 0, 0));
    }

    #[test]
    fn default_colour_or_zero_id_is_not_an_image() {
        assert_eq!(PlaceholderCell::decode(Color::Named(NamedColor::Foreground), None, &[R0, R0], None), None);
        assert_eq!(PlaceholderCell::decode(Color::Indexed(0), None, &[R0, R0], None), None);
    }

    #[test]
    fn box_size_from_the_placement_or_the_image() {
        assert_eq!(box_cells(10, 4, 100, 50, 8., 16.), (10, 4));
        // Only columns: rows follow the aspect ratio (80 px wide -> 40 px tall -> 3 rows).
        assert_eq!(box_cells(10, 0, 100, 50, 8., 16.), (10, 3));
        // Only rows: 4 rows = 64 px tall -> 128 px wide -> 16 columns.
        assert_eq!(box_cells(0, 4, 100, 50, 8., 16.), (16, 4));
        // Neither: the image's own size, rounded up.
        assert_eq!(box_cells(0, 0, 100, 50, 8., 16.), (13, 4));
    }

    #[test]
    fn fit_keeps_aspect_and_centres() {
        // Box 80x64, image 100x50 -> scale 0.8 -> 80x40, centred vertically.
        assert_eq!(fit(10, 4, 8., 16., 100, 50), (0., 12., 80., 40.));
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p tern-term graphics::placeholder`
Expected: compile errors (`DIACRITICS`, `PlaceholderCell`, … not found).

- [ ] **Step 4: Implement** (top of `placeholder.rs`)

```rust
//! Unicode placeholders: a program shows an image by printing U+10EEEE cells
//! whose foreground colour names the image, whose underline colour names the
//! placement, and whose combining diacritics give the row, the column and the
//! image id's high byte of the piece drawn there.

use alacritty_terminal::vte::ansi::Color;

/// The placeholder character.
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// Row/column diacritics from the protocol's table: the n-th one means n.
const DIACRITICS: [u32; 297] = [
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A, 0x034B, 0x034C, 0x0350, 0x0351, 0x0352,
    0x0357, 0x035B, 0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369, 0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F,
    0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592, 0x0593, 0x0594, 0x0595, 0x0597, 0x0598, 0x0599, 0x059C, 0x059D, 0x059E,
    0x059F, 0x05A0, 0x05A1, 0x05A8, 0x05A9, 0x05AB, 0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065A, 0x065B, 0x065D, 0x065E, 0x06D6, 0x06D7, 0x06D8, 0x06D9, 0x06DA, 0x06DB,
    0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2, 0x06E4, 0x06E7, 0x06E8, 0x06EB, 0x06EC, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736,
    0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743, 0x0745, 0x0747, 0x0749, 0x074A, 0x07EB, 0x07EC, 0x07ED, 0x07EE, 0x07EF,
    0x07F0, 0x07F1, 0x07F3, 0x0816, 0x0817, 0x0818, 0x0819, 0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A, 0x082B, 0x082C, 0x082D, 0x0951, 0x0953, 0x0954, 0x0F82, 0x0F83, 0x0F86,
    0x0F87, 0x135D, 0x135E, 0x135F, 0x17DD, 0x193A, 0x1A17, 0x1A75, 0x1A76, 0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C,
    0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F, 0x1B70, 0x1B71, 0x1B72, 0x1B73, 0x1CD0, 0x1CD1, 0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0,
    0x1DC1, 0x1DC3, 0x1DC4, 0x1DC5, 0x1DC6, 0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5,
    0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF, 0x1DE0, 0x1DE1, 0x1DE2, 0x1DE3, 0x1DE4,
    0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1, 0x20D4, 0x20D5, 0x20D6, 0x20D7, 0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0,
    0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2, 0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6, 0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB,
    0x2DEC, 0x2DED, 0x2DEE, 0x2DEF, 0x2DF0, 0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8, 0x2DF9, 0x2DFA,
    0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1, 0xA8E0, 0xA8E1, 0xA8E2, 0xA8E3, 0xA8E4,
    0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9, 0xA8EA, 0xA8EB, 0xA8EC, 0xA8ED, 0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2,
    0xAAB3, 0xAAB7, 0xAAB8, 0xAABE, 0xAABF, 0xAAC1, 0xFE20, 0xFE21, 0xFE22, 0xFE23, 0xFE24, 0xFE25, 0xFE26, 0x10A0F,
    0x10A38, 0x1D185, 0x1D186, 0x1D187, 0x1D188, 0x1D189, 0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242, 0x1D243, 0x1D244,
];

/// The number a row/column diacritic stands for.
pub fn diacritic_value(c: char) -> Option<u32> {
    DIACRITICS.binary_search(&(c as u32)).ok().map(|i| i as u32)
}

/// One decoded placeholder cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaceholderCell {
    pub image_id: u32,
    /// 0 when the cell names no placement.
    pub placement_id: u32,
    pub row: u32,
    pub col: u32,
}

/// An id carried by a colour: 24-bit colours give `r<<16|g<<8|b`, 256-colour
/// ones their index. The default colours carry none.
fn color_id(c: Color) -> Option<u32> {
    match c {
        Color::Spec(rgb) => Some(((rgb.r as u32) << 16) | ((rgb.g as u32) << 8) | rgb.b as u32),
        Color::Indexed(i) => Some(i as u32),
        Color::Named(_) => None,
    }
}

impl PlaceholderCell {
    /// Decodes a placeholder cell from its colours and combining marks. `prev`
    /// is the decoded cell just to its left on the same row, if that was a
    /// placeholder: a cell that leaves out its column (or row, or high byte)
    /// continues it.
    pub fn decode(fg: Color, underline: Option<Color>, marks: &[char], prev: Option<&PlaceholderCell>) -> Option<Self> {
        let low = color_id(fg)?;
        let placement_id = underline.and_then(color_id).unwrap_or(0);
        let mut values = marks.iter().filter_map(|&c| diacritic_value(c));
        let (row, col, high) = (values.next(), values.next(), values.next());
        let prev = prev.filter(|p| p.image_id & 0x00FF_FFFF == low && p.placement_id == placement_id);
        let row = row.or(prev.map(|p| p.row)).unwrap_or(0);
        let prev = prev.filter(|p| p.row == row);
        let col = col.or(prev.map(|p| p.col + 1)).unwrap_or(0);
        let high = high.or(prev.filter(|p| p.col + 1 == col).map(|p| p.image_id >> 24)).unwrap_or(0);
        let image_id = (high << 24) | low;
        (image_id != 0).then_some(Self { image_id, placement_id, row, col })
    }
}

/// The placement box in cells. A size of 0 is derived: from the other side
/// and the image's aspect ratio, or from the image's own pixel size.
pub fn box_cells(cols: u32, rows: u32, img_w: u32, img_h: u32, cell_w: f32, cell_h: f32) -> (u32, u32) {
    let (w, h) = (img_w.max(1) as f32, img_h.max(1) as f32);
    let up = |v: f32| (v.ceil() as u32).max(1);
    match (cols, rows) {
        (0, 0) => (up(w / cell_w), up(h / cell_h)),
        (c, 0) => (c, up(c as f32 * cell_w * h / w / cell_h)),
        (0, r) => (up(r as f32 * cell_h * w / h / cell_w), r),
        (c, r) => (c, r),
    }
}

/// Where the image goes inside a `cols`×`rows` box: scaled to fit with its
/// aspect ratio kept, centred. `(x, y, width, height)` from the box's corner.
pub fn fit(cols: u32, rows: u32, cell_w: f32, cell_h: f32, img_w: u32, img_h: u32) -> (f32, f32, f32, f32) {
    let (bw, bh) = (cols as f32 * cell_w, rows as f32 * cell_h);
    let (iw, ih) = (img_w.max(1) as f32, img_h.max(1) as f32);
    let scale = (bw / iw).min(bh / ih);
    let (w, h) = (iw * scale, ih * scale);
    ((bw - w) / 2., (bh - h) / 2., w, h)
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p tern-term graphics::placeholder`
Expected: 7 passed.

- [ ] **Step 6: Commit**

```bash
cargo fmt && cargo clippy -p tern-term --all-targets
git add crates/tern-term/src/lib.rs crates/tern-term/src/graphics
git commit -m "Decode kitty graphics Unicode placeholder cells"
```

---

### Task 2: APC parser

**Files:**
- Create: `crates/tern-term/src/graphics/apc.rs`
- Modify: `crates/tern-term/src/graphics/mod.rs` (add `mod apc;`)

**Interfaces:**
- Produces: `apc::Apc::{Command { control: Vec<u8>, payload: Vec<u8> }, TooLarge { control: Vec<u8> }}`, `apc::ApcParser::new()`, `ApcParser::feed(&mut self, &[u8], impl FnMut(Apc))`, `apc::MAX_COMMAND`.

- [ ] **Step 1: Write the failing tests** (bottom of `apc.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn collect(chunks: &[&[u8]]) -> Vec<Apc> {
        let mut p = ApcParser::new();
        let mut out = vec![];
        for c in chunks {
            p.feed(c, |a| out.push(a));
        }
        out
    }

    fn cmd(control: &str, payload: &str) -> Apc {
        Apc::Command { control: control.into(), payload: payload.into() }
    }

    #[test]
    fn whole_command() {
        assert_eq!(collect(&[b"ab\x1b_Ga=q,i=1;AAAA\x1b\\cd"]), vec![cmd("a=q,i=1", "AAAA")]);
    }

    #[test]
    fn split_at_every_byte() {
        let s: &[u8] = b"x\x1b_Gi=1;QUJD\x1b\\y\x1b_Ga=d\x1b\\";
        for cut in 0..=s.len() {
            assert_eq!(collect(&[&s[..cut], &s[cut..]]), vec![cmd("i=1", "QUJD"), cmd("a=d", "")], "cut at {cut}");
        }
    }

    #[test]
    fn other_strings_are_skipped() {
        assert_eq!(collect(&[b"\x1b_Xhello\x1b\\\x1b]0;t\x07\x1b[1m\x1b_Gi=2\x1b\\"]), vec![cmd("i=2", "")]);
    }

    #[test]
    fn esc_cancels_the_string() {
        assert_eq!(collect(&[b"\x1b_Gi=1;AA\x1b[0m\x1b_Gi=3\x1b\\"]), vec![cmd("i=3", "")]);
    }

    #[test]
    fn oversized_command_keeps_its_control_data() {
        let mut big = b"\x1b_Gi=5,m=0;".to_vec();
        big.resize(big.len() + MAX_COMMAND + 10, b'A');
        big.extend_from_slice(b"\x1b\\\x1b_Gi=6\x1b\\");
        assert_eq!(collect(&[&big]), vec![Apc::TooLarge { control: b"i=5,m=0".to_vec() }, cmd("i=6", "")]);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p tern-term graphics::apc`
Expected: compile errors (`ApcParser` not found).

- [ ] **Step 3: Implement** (top of `apc.rs`)

```rust
//! Pulls graphics commands (`ESC _ G <control> ; <payload> ESC \`) out of the
//! raw PTY byte stream. The VT parser sees the same bytes and ignores APC
//! strings, so nothing is taken out of the stream.

/// Largest command (control data and payload) buffered.
pub const MAX_COMMAND: usize = 16 * 1024 * 1024;
/// Control data kept from an oversized command, so it can still be answered.
const MAX_CONTROL: usize = 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum Apc {
    Command { control: Vec<u8>, payload: Vec<u8> },
    /// Longer than [`MAX_COMMAND`]; the payload was dropped.
    TooLarge { control: Vec<u8> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Esc,
    /// Saw `ESC _`, waiting for `G`.
    Intro,
    Body,
    BodyEsc,
    /// Another APC string: skipped to its end.
    Skip,
    SkipEsc,
}

#[derive(Debug)]
pub struct ApcParser {
    state: State,
    buf: Vec<u8>,
    overflow: bool,
}

impl ApcParser {
    pub fn new() -> Self {
        Self { state: State::Ground, buf: Vec::new(), overflow: false }
    }

    /// Feeds bytes, calling `emit` for every complete graphics command.
    pub fn feed(&mut self, bytes: &[u8], mut emit: impl FnMut(Apc)) {
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            match self.state {
                State::Ground => match memchr_esc(&bytes[i..]) {
                    Some(off) => {
                        i += off + 1;
                        self.state = State::Esc;
                        continue;
                    },
                    None => return,
                },
                State::Esc => {
                    self.state = match b {
                        b'_' => State::Intro,
                        0x1b => State::Esc,
                        _ => State::Ground,
                    }
                },
                State::Intro => {
                    self.state = match b {
                        b'G' => {
                            self.buf.clear();
                            self.overflow = false;
                            State::Body
                        },
                        0x1b => State::SkipEsc,
                        _ => State::Skip,
                    }
                },
                State::Body => {
                    let end = memchr_esc(&bytes[i..]).map_or(bytes.len(), |o| i + o);
                    self.push(&bytes[i..end]);
                    if end < bytes.len() {
                        self.state = State::BodyEsc;
                    }
                    i = end + 1;
                    continue;
                },
                State::BodyEsc => {
                    if b == b'\\' {
                        emit(self.take());
                        self.state = State::Ground;
                    } else {
                        // An ESC inside the string cancels it and starts a new sequence.
                        self.buf.clear();
                        self.state = State::Esc;
                        continue;
                    }
                },
                State::Skip => {
                    if b == 0x1b {
                        self.state = State::SkipEsc;
                    }
                },
                State::SkipEsc => {
                    if b == b'\\' {
                        self.state = State::Ground;
                    } else {
                        self.state = State::Esc;
                        continue;
                    }
                },
            }
            i += 1;
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        if self.overflow {
            return;
        }
        if self.buf.len() + chunk.len() <= MAX_COMMAND {
            self.buf.extend_from_slice(chunk);
            return;
        }
        // Keep only the control data, up to the first `;`.
        let all = || self.buf.iter().chain(chunk).take(MAX_CONTROL);
        let len = all().position(|&c| c == b';').unwrap_or_else(|| all().count());
        self.buf = all().take(len).copied().collect();
        self.overflow = true;
    }

    fn take(&mut self) -> Apc {
        let mut control = std::mem::take(&mut self.buf);
        if std::mem::take(&mut self.overflow) {
            return Apc::TooLarge { control };
        }
        match control.iter().position(|&c| c == b';') {
            Some(p) => {
                let payload = control.split_off(p + 1);
                control.pop();
                Apc::Command { control, payload }
            },
            None => Apc::Command { control, payload: Vec::new() },
        }
    }
}

impl Default for ApcParser {
    fn default() -> Self {
        Self::new()
    }
}

fn memchr_esc(hay: &[u8]) -> Option<usize> {
    hay.iter().position(|&c| c == 0x1b)
}
```
Add `mod apc;` to `graphics/mod.rs` (above `pub mod placeholder;`). If clippy flags `dead_code` before Task 5 wires it, add `#[allow(dead_code)]` on `mod apc;` and remove it in Task 5.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p tern-term graphics::apc`
Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy -p tern-term --all-targets
git add crates/tern-term/src/graphics
git commit -m "Extract kitty graphics commands from the PTY stream"
```

---

### Task 3: Command parsing

**Files:**
- Create: `crates/tern-term/src/graphics/command.rs`
- Modify: `crates/tern-term/src/graphics/mod.rs` (add `mod command;`)

**Interfaces:**
- Consumes: `graphics::Error`.
- Produces: `command::{Action, Format, Medium, Command}`, `command::parse(&[u8]) -> (Command, Option<Error>)`. `Command` fields: `action, quiet: u8, id: u32, number: u32, placement: u32, format, medium, width: u32, height: u32, size: u64, offset: u64, more: bool, compressed: bool, unicode: bool, cols: u32, rows: u32, delete: u8, answer: bool`.

- [ ] **Step 1: Write the failing tests** (bottom of `command.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let (c, e) = parse(b"");
        assert!(e.is_none());
        assert_eq!((c.action, c.format, c.medium, c.answer), (Action::Transmit, Format::Rgba, Medium::Direct, false));
    }

    #[test]
    fn every_key() {
        let (c, e) = parse(b"a=T,q=2,i=31,I=4,p=7,f=100,t=s,s=3,v=2,S=10,O=5,m=1,U=1,c=40,r=12,d=I,X=9,zz=1");
        assert!(e.is_none());
        assert_eq!(
            c,
            Command {
                action: Action::TransmitAndPlace,
                quiet: 2,
                id: 31,
                number: 4,
                placement: 7,
                format: Format::Png,
                medium: Medium::SharedMemory,
                width: 3,
                height: 2,
                size: 10,
                offset: 5,
                more: true,
                compressed: false,
                unicode: true,
                cols: 40,
                rows: 12,
                delete: b'I',
                answer: true,
            }
        );
    }

    #[test]
    fn bad_value_keeps_the_ids() {
        let (c, e) = parse(b"i=9,f=7");
        assert_eq!(c.id, 9);
        assert_eq!(e.unwrap().code, "EINVAL");
        assert_eq!(parse(b"i=x").1.unwrap().code, "EINVAL");
    }

    #[test]
    fn actions() {
        assert_eq!(parse(b"a=q").0.action, Action::Query);
        assert_eq!(parse(b"a=p").0.action, Action::Place);
        assert_eq!(parse(b"a=d").0.action, Action::Delete);
        assert_eq!(parse(b"a=f").0.action, Action::Other(b'f'));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p tern-term graphics::command`
Expected: compile errors.

- [ ] **Step 3: Implement** (top of `command.rs`)

```rust
//! A command's control data: `key=value` pairs separated by commas.

use super::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Transmit,
    TransmitAndPlace,
    Query,
    Place,
    Delete,
    Other(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Rgb,
    Rgba,
    Png,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Medium {
    Direct,
    File,
    TempFile,
    SharedMemory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub action: Action,
    /// 1: no OK replies; 2: no replies at all.
    pub quiet: u8,
    pub id: u32,
    pub number: u32,
    pub placement: u32,
    pub format: Format,
    pub medium: Medium,
    pub width: u32,
    pub height: u32,
    pub size: u64,
    pub offset: u64,
    pub more: bool,
    pub compressed: bool,
    pub unicode: bool,
    pub cols: u32,
    pub rows: u32,
    pub delete: u8,
    /// The program named the image (`i` or `I`), so it wants replies.
    pub answer: bool,
}

impl Default for Command {
    fn default() -> Self {
        Self {
            action: Action::Transmit,
            quiet: 0,
            id: 0,
            number: 0,
            placement: 0,
            format: Format::Rgba,
            medium: Medium::Direct,
            width: 0,
            height: 0,
            size: 0,
            offset: 0,
            more: false,
            compressed: false,
            unicode: false,
            cols: 0,
            rows: 0,
            delete: 0,
            answer: false,
        }
    }
}

/// Parses control data. A bad value doesn't stop parsing, so the reply can
/// still name the image; the first error is returned beside the command.
pub fn parse(control: &[u8]) -> (Command, Option<Error>) {
    let mut cmd = Command::default();
    let mut err = None;
    for pair in control.split(|&b| b == b',').filter(|p| !p.is_empty()) {
        let Some(eq) = pair.iter().position(|&b| b == b'=') else {
            err.get_or_insert(Error::new("EINVAL", "malformed control data"));
            continue;
        };
        if let [key] = pair[..eq]
            && let Err(e) = apply(&mut cmd, key, &pair[eq + 1..])
        {
            err.get_or_insert(e);
        }
    }
    cmd.answer = cmd.id != 0 || cmd.number != 0;
    (cmd, err)
}

fn apply(cmd: &mut Command, key: u8, value: &[u8]) -> Result<(), Error> {
    let bad = || Error::new("EINVAL", format!("bad value for {}", key as char));
    let ch = || match value {
        [c] => Ok(*c),
        _ => Err(bad()),
    };
    let num = || std::str::from_utf8(value).ok().and_then(|s| s.parse::<u64>().ok()).ok_or_else(bad);
    let num32 = || num().and_then(|n| u32::try_from(n).map_err(|_| bad()));
    match key {
        b'a' => {
            cmd.action = match ch()? {
                b't' => Action::Transmit,
                b'T' => Action::TransmitAndPlace,
                b'q' => Action::Query,
                b'p' => Action::Place,
                b'd' => Action::Delete,
                c => Action::Other(c),
            }
        },
        b'q' => cmd.quiet = num32()?.min(2) as u8,
        b'i' => cmd.id = num32()?,
        b'I' => cmd.number = num32()?,
        b'p' => cmd.placement = num32()?,
        b'f' => {
            cmd.format = match num32()? {
                24 => Format::Rgb,
                32 => Format::Rgba,
                100 => Format::Png,
                _ => return Err(bad()),
            }
        },
        b't' => {
            cmd.medium = match ch()? {
                b'd' => Medium::Direct,
                b'f' => Medium::File,
                b't' => Medium::TempFile,
                b's' => Medium::SharedMemory,
                _ => return Err(bad()),
            }
        },
        b's' => cmd.width = num32()?,
        b'v' => cmd.height = num32()?,
        b'S' => cmd.size = num()?,
        b'O' => cmd.offset = num()?,
        b'm' => cmd.more = num()? == 1,
        b'o' => cmd.compressed = ch()? == b'z',
        b'U' => cmd.unicode = num()? == 1,
        b'c' => cmd.cols = num32()?,
        b'r' => cmd.rows = num32()?,
        b'd' => cmd.delete = ch()?,
        _ => {},
    }
    Ok(())
}
```
Add `mod command;` to `graphics/mod.rs` (with `#[allow(dead_code)]` until Task 5 if clippy complains).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p tern-term graphics::command`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy -p tern-term --all-targets
git add crates/tern-term/src/graphics
git commit -m "Parse kitty graphics control data"
```

---

### Task 4: Loading and decoding images

**Files:**
- Create: `crates/tern-term/src/graphics/load.rs`
- Modify: `crates/tern-term/Cargo.toml` (deps), `crates/tern-term/src/graphics/mod.rs` (`mod load; pub use load::Image;`)

**Interfaces:**
- Consumes: `command::{Command, Format, Medium}`, `command::parse`, `Error`.
- Produces: `load::Image { width: u32, height: u32, rgba: Arc<[u8]> }` (`Clone`, `PartialEq`), `load::load(&Command, payload_base64: &[u8]) -> Result<Image, Error>`.

- [ ] **Step 1: Dependencies**

In `crates/tern-term/Cargo.toml` `[dependencies]` add:
```toml
base64 = "0.22"
image = { version = "0.25", default-features = false, features = ["png"] }
```

- [ ] **Step 2: Write the failing tests** (bottom of `load.rs`)

```rust
#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use base64::engine::general_purpose::STANDARD;

    use super::*;
    use crate::graphics::command::parse;

    fn cmd(control: &str) -> Command {
        parse(control.as_bytes()).0
    }

    fn b64(bytes: &[u8]) -> Vec<u8> {
        STANDARD.encode(bytes).into_bytes()
    }

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("{name}-{}", std::process::id()))
    }

    #[test]
    fn rgb_direct() {
        let img = load(&cmd("f=24,s=2,v=1"), &b64(&[1, 2, 3, 4, 5, 6])).unwrap();
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(&*img.rgba, &[1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[test]
    fn short_data_and_missing_size_are_errors() {
        assert_eq!(load(&cmd("f=32,s=2,v=2"), &b64(&[0; 8])).unwrap_err().code, "ENODATA");
        assert_eq!(load(&cmd("f=32"), &b64(&[0; 4])).unwrap_err().code, "EINVAL");
        assert_eq!(load(&cmd("f=24,s=1,v=1"), b"!!!!").unwrap_err().code, "EINVAL");
    }

    #[test]
    fn the_capability_probe_loads() {
        load(&cmd("i=31,s=1,v=1,a=q,t=d,f=24"), b"AAAA").unwrap();
    }

    #[test]
    fn png_direct() {
        let mut png = Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(3, 2, image::Rgba([9, 8, 7, 255])).write_to(&mut png, image::ImageFormat::Png).unwrap();
        let img = load(&cmd("f=100"), &b64(png.get_ref())).unwrap();
        assert_eq!((img.width, img.height), (3, 2));
        assert_eq!(&img.rgba[..4], &[9, 8, 7, 255]);
    }

    #[test]
    fn file_with_offset_and_size() {
        let path = temp("tern-gfx-file");
        std::fs::write(&path, [0, 0, 1, 2, 3, 0]).unwrap();
        let img = load(&cmd("t=f,f=24,s=1,v=1,O=2,S=3"), &b64(path.to_str().unwrap().as_bytes())).unwrap();
        assert_eq!(&*img.rgba, &[1, 2, 3, 255]);
        assert!(path.exists(), "t=f leaves the file");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn file_refusals() {
        let code = |p: &str| load(&cmd("t=f,f=24,s=1,v=1"), &b64(p.as_bytes())).unwrap_err().code;
        assert_eq!(code("/dev/zero"), "EPERM");
        assert_eq!(code(std::env::temp_dir().to_str().unwrap()), "EINVAL");
        assert_eq!(code("relative.png"), "EINVAL");
        assert_eq!(code("/nonexistent/x.png"), "ENOENT");
    }

    #[test]
    fn temp_file_is_deleted_only_when_named_for_us() {
        let ours = temp("tty-graphics-protocol-tern");
        let other = temp("tern-keep");
        for p in [&ours, &other] {
            std::fs::write(p, [1, 2, 3]).unwrap();
            load(&cmd("t=t,f=24,s=1,v=1"), &b64(p.to_str().unwrap().as_bytes())).unwrap();
        }
        assert!(!ours.exists());
        assert!(other.exists());
        std::fs::remove_file(other).unwrap();
    }

    #[test]
    fn shared_memory_round_trip() {
        let name = format!("/tern-t{}", std::process::id());
        let cname = std::ffi::CString::new(name.clone()).unwrap();
        // SAFETY: test-only shm object of 3 bytes, mapped and unmapped here.
        unsafe {
            let fd = shm_open(&cname, libc::O_CREAT | libc::O_RDWR);
            assert!(fd >= 0);
            assert_eq!(libc::ftruncate(fd, 3), 0);
            let p = libc::mmap(std::ptr::null_mut(), 3, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0);
            assert_ne!(p, libc::MAP_FAILED);
            std::ptr::copy_nonoverlapping([4u8, 5, 6].as_ptr(), p as *mut u8, 3);
            libc::munmap(p, 3);
            libc::close(fd);
        }
        let img = load(&cmd("t=s,f=24,s=1,v=1"), &b64(name.as_bytes())).unwrap();
        assert_eq!(&*img.rgba, &[4, 5, 6, 255]);
        // SAFETY: probing that the object was unlinked.
        assert!(unsafe { shm_open(&cname, libc::O_RDONLY) } < 0);
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p tern-term graphics::load`
Expected: compile errors.

- [ ] **Step 4: Implement** (top of `load.rs`)

```rust
//! Turns a transmission into RGBA pixels: reads the medium (inline data, a
//! file, a temporary file or a shared-memory object), then decodes the format.

use std::ffi::CStr;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::FromRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

use super::Error;
use super::command::{Command, Format, Medium};

/// Most bytes read from a file or shared-memory object.
const MAX_READ: u64 = 64 * 1024 * 1024;
/// Largest width or height, in pixels.
const MAX_SIDE: u32 = 10_000;

/// Chunks are split on 4-byte boundaries; the last may or may not be padded.
const BASE64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    /// 4 bytes a pixel, row by row.
    pub rgba: Arc<[u8]>,
}

/// Loads the image a transmission (or query) describes; `payload` is its
/// base64 data, all chunks joined.
pub fn load(cmd: &Command, payload: &[u8]) -> Result<Image, Error> {
    if cmd.compressed {
        return Err(Error::new("ENOTSUPPORTED", "compressed data"));
    }
    let data = BASE64.decode(payload).map_err(|_| Error::new("EINVAL", "bad base64 data"))?;
    let bytes = match cmd.medium {
        Medium::Direct => data,
        Medium::File => read_file(&path_of(&data)?, cmd)?,
        Medium::TempFile => {
            let path = path_of(&data)?;
            let bytes = read_file(&path, cmd);
            remove_temp_file(&path);
            bytes?
        },
        Medium::SharedMemory => read_shm(&data, cmd)?,
    };
    decode(cmd, &bytes)
}

fn path_of(data: &[u8]) -> Result<PathBuf, Error> {
    let s = std::str::from_utf8(data).map_err(|_| Error::new("EINVAL", "path is not UTF-8"))?;
    let path = PathBuf::from(s);
    if !path.is_absolute() {
        return Err(Error::new("EINVAL", "path is not absolute"));
    }
    Ok(path)
}

/// The bytes `cmd.offset .. cmd.offset + cmd.size` (0 = to the end) of `len`.
fn window(cmd: &Command, len: u64) -> Result<u64, Error> {
    let avail = len.saturating_sub(cmd.offset);
    let want = if cmd.size == 0 { avail } else { cmd.size.min(avail) };
    if want > MAX_READ {
        return Err(Error::new("EFBIG", "image data too large"));
    }
    Ok(want)
}

fn read_file(path: &Path, cmd: &Command) -> Result<Vec<u8>, Error> {
    let real = path.canonicalize().map_err(io_error)?;
    if ["/dev", "/proc", "/sys"].iter().any(|d| real.starts_with(d)) {
        return Err(Error::new("EPERM", "refusing a device or kernel file"));
    }
    let mut file = File::open(&real).map_err(io_error)?;
    let meta = file.metadata().map_err(io_error)?;
    if !meta.is_file() {
        return Err(Error::new("EINVAL", "not a regular file"));
    }
    let want = window(cmd, meta.len())?;
    file.seek(SeekFrom::Start(cmd.offset)).map_err(io_error)?;
    let mut buf = Vec::with_capacity(want as usize);
    file.take(want).read_to_end(&mut buf).map_err(io_error)?;
    Ok(buf)
}

/// Deletes a `t=t` file, but only one plainly meant for this protocol.
fn remove_temp_file(path: &Path) {
    let named = path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.contains("tty-graphics-protocol"));
    let parent = path.parent().and_then(|p| p.canonicalize().ok());
    let in_tmp = [std::env::temp_dir(), PathBuf::from("/tmp"), PathBuf::from("/dev/shm")]
        .iter()
        .filter_map(|d| d.canonicalize().ok())
        .any(|d| parent.as_ref().is_some_and(|p| p.starts_with(&d)));
    if named && in_tmp {
        let _ = std::fs::remove_file(path);
    }
}

fn read_shm(name: &[u8], cmd: &Command) -> Result<Vec<u8>, Error> {
    let cname = std::ffi::CString::new(name).map_err(|_| Error::new("EINVAL", "bad shared memory name"))?;
    // SAFETY: the fd is owned by `file` (closed on drop); the mapping covers
    // `map_len` bytes of an object at least that long and is unmapped below.
    unsafe {
        let fd = shm_open(&cname, libc::O_RDONLY);
        if fd < 0 {
            return Err(io_error(std::io::Error::last_os_error()));
        }
        libc::shm_unlink(cname.as_ptr());
        let file = File::from_raw_fd(fd);
        let want = window(cmd, file.metadata().map_err(io_error)?.len())?;
        if want == 0 {
            return Ok(Vec::new());
        }
        let map_len = (cmd.offset + want) as usize;
        let p = libc::mmap(std::ptr::null_mut(), map_len, libc::PROT_READ, libc::MAP_SHARED, fd, 0);
        if p == libc::MAP_FAILED {
            return Err(io_error(std::io::Error::last_os_error()));
        }
        let bytes = std::slice::from_raw_parts((p as *const u8).add(cmd.offset as usize), want as usize).to_vec();
        libc::munmap(p, map_len);
        Ok(bytes)
    }
}

/// `shm_open` with mode 0600 (variadic on macOS, where the mode is passed as `c_uint`).
unsafe fn shm_open(name: &CStr, flags: libc::c_int) -> libc::c_int {
    #[cfg(target_os = "macos")]
    let mode = 0o600 as libc::c_uint;
    #[cfg(not(target_os = "macos"))]
    let mode = 0o600 as libc::mode_t;
    // SAFETY: `name` is a valid C string; the rest is forwarded from the caller.
    unsafe { libc::shm_open(name.as_ptr(), flags, mode) }
}

fn io_error(e: std::io::Error) -> Error {
    let code = match e.kind() {
        std::io::ErrorKind::NotFound => "ENOENT",
        std::io::ErrorKind::PermissionDenied => "EPERM",
        _ => "EIO",
    };
    Error::new(code, e.to_string())
}

fn decode(cmd: &Command, bytes: &[u8]) -> Result<Image, Error> {
    let (width, height, rgba) = match cmd.format {
        Format::Png => {
            let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
                .map_err(|e| Error::new("EBADPNG", e.to_string()))?
                .into_rgba8();
            (img.width(), img.height(), img.into_raw())
        },
        Format::Rgb | Format::Rgba => {
            let bpp = if cmd.format == Format::Rgb { 3 } else { 4 };
            let (w, h) = (cmd.width, cmd.height);
            if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
                return Err(Error::new("EINVAL", "bad image size"));
            }
            let need = w as usize * h as usize * bpp;
            if bytes.len() < need {
                return Err(Error::new("ENODATA", format!("need {need} bytes, got {}", bytes.len())));
            }
            let px = &bytes[..need];
            let rgba = if bpp == 4 { px.to_vec() } else { px.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect() };
            (w, h, rgba)
        },
    };
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(Error::new("EINVAL", "bad image size"));
    }
    Ok(Image { width, height, rgba: rgba.into() })
}
```
In the test module, `shm_open` refers to this helper (via `use super::*`).
Add to `graphics/mod.rs`: `mod load;` and `pub use load::Image;`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p tern-term graphics::load`
Expected: 8 passed.

- [ ] **Step 6: Commit**

```bash
cargo fmt && cargo clippy -p tern-term --all-targets
git add Cargo.lock crates/tern-term
git commit -m "Load kitty graphics images from inline data, files and shared memory"
```

---

### Task 5: Image store, replies and the `Graphics` engine

**Files:**
- Create: `crates/tern-term/src/graphics/store.rs`, `crates/tern-term/src/graphics/reply.rs`
- Modify: `crates/tern-term/src/graphics/mod.rs` (engine + tests; drop any `allow(dead_code)`)

**Interfaces:**
- Consumes: `apc::{Apc, ApcParser}`, `command::{parse, Action, Command}`, `load::{load, Image}`, `Error`.
- Produces: `graphics::{Graphics, ImageStore, StoredImage, VirtualPlacement, Image, STORE_CAP}`; `Graphics::new(Arc<Mutex<ImageStore>>)`, `Graphics::feed(&mut self, &[u8], reply: &mut Vec<u8>)`; `ImageStore::new(cap: usize)`, `get(&mut self, id) -> Option<&StoredImage>`, `bytes()`, plus `allocate_id`, `set_number`, `resolve`, `insert`, `place`, `delete`, `clear`; `StoredImage { image: Image, generation: u64, placements: Vec<VirtualPlacement> }`, `StoredImage::placement(&self, id: u32) -> Option<VirtualPlacement>`; `VirtualPlacement { id: u32, cols: u32, rows: u32 }`.

- [ ] **Step 1: Store — failing tests** (bottom of `store.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn img(px: usize) -> Image {
        Image { width: 1, height: px as u32, rgba: vec![0; 4 * px].into() }
    }

    #[test]
    fn replace_changes_generation_and_drops_placements() {
        let mut s = ImageStore::new(1 << 20);
        s.insert(1, img(1));
        s.place(1, VirtualPlacement { id: 0, cols: 2, rows: 1 }).unwrap();
        let first = s.get(1).unwrap().generation;
        s.insert(1, img(2));
        let again = s.get(1).unwrap();
        assert_ne!(again.generation, first);
        assert!(again.placements.is_empty());
        assert_eq!(s.bytes(), 8);
    }

    #[test]
    fn eviction_drops_the_least_recently_used() {
        let mut s = ImageStore::new(40);
        s.insert(1, img(4));
        s.insert(2, img(4));
        s.get(1);
        s.insert(3, img(4));
        assert!(s.get(2).is_none());
        assert!(s.get(1).is_some() && s.get(3).is_some());
        assert_eq!(s.bytes(), 32);
    }

    #[test]
    fn placements_and_numbers() {
        let mut s = ImageStore::new(1 << 20);
        assert_eq!(s.place(9, VirtualPlacement { id: 0, cols: 1, rows: 1 }).unwrap_err().code, "ENOENT");
        let id = s.allocate_id();
        s.insert(id, img(1));
        s.set_number(4, id);
        assert_eq!(s.resolve(0, 4), Some(id));
        s.place(id, VirtualPlacement { id: 3, cols: 5, rows: 2 }).unwrap();
        assert_eq!(s.get(id).unwrap().placement(3), Some(VirtualPlacement { id: 3, cols: 5, rows: 2 }));
        assert_eq!(s.get(id).unwrap().placement(0), Some(VirtualPlacement { id: 3, cols: 5, rows: 2 }));
        s.delete(id);
        assert_eq!(s.resolve(0, 4), None);
        assert_eq!(s.bytes(), 0);
    }
}
```

- [ ] **Step 2: Store — implement** (top of `store.rs`)

```rust
//! A session's transmitted images and their virtual placements.

use std::collections::HashMap;

use super::Error;
use super::load::Image;

/// A placement shown through placeholder cells: the box, in cells, the image is fitted into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualPlacement {
    pub id: u32,
    pub cols: u32,
    pub rows: u32,
}

#[derive(Debug)]
pub struct StoredImage {
    pub image: Image,
    /// Unique in the store; a replaced image gets a new one.
    pub generation: u64,
    pub placements: Vec<VirtualPlacement>,
    last_used: u64,
}

impl StoredImage {
    /// The placement a placeholder names, else the image's first one.
    pub fn placement(&self, id: u32) -> Option<VirtualPlacement> {
        self.placements.iter().find(|p| p.id == id).or_else(|| self.placements.first()).copied()
    }
}

#[derive(Debug)]
pub struct ImageStore {
    images: HashMap<u32, StoredImage>,
    /// Image numbers (`I=`) to the ids they were given.
    numbers: HashMap<u32, u32>,
    bytes: usize,
    cap: usize,
    clock: u64,
    next_id: u32,
}

impl ImageStore {
    /// A store holding at most `cap` bytes of decoded pixels.
    pub fn new(cap: usize) -> Self {
        Self { images: HashMap::new(), numbers: HashMap::new(), bytes: 0, cap, clock: 0, next_id: 0x7F00_0000 }
    }

    /// Decoded bytes held.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// An unused id for an image transmitted without one.
    pub fn allocate_id(&mut self) -> u32 {
        loop {
            self.next_id = self.next_id.wrapping_add(1).max(1);
            if !self.images.contains_key(&self.next_id) {
                return self.next_id;
            }
        }
    }

    pub fn set_number(&mut self, number: u32, id: u32) {
        self.numbers.insert(number, id);
    }

    /// The stored image named by `id`, or by `number` when `id` is 0.
    pub fn resolve(&self, id: u32, number: u32) -> Option<u32> {
        let id = if id != 0 { id } else { *self.numbers.get(&number)? };
        self.images.contains_key(&id).then_some(id)
    }

    /// Stores `image` under `id`, replacing an earlier one and its placements.
    pub fn insert(&mut self, id: u32, image: Image) {
        self.remove(id);
        self.clock += 1;
        self.bytes += image.rgba.len();
        self.images.insert(id, StoredImage { image, generation: self.clock, placements: Vec::new(), last_used: self.clock });
        self.evict(id);
    }

    pub fn place(&mut self, id: u32, placement: VirtualPlacement) -> Result<(), Error> {
        let img = self.images.get_mut(&id).ok_or_else(|| Error::new("ENOENT", "no such image"))?;
        img.placements.retain(|p| p.id != placement.id);
        img.placements.push(placement);
        Ok(())
    }

    /// The image `id`, for drawing; marks it recently used.
    pub fn get(&mut self, id: u32) -> Option<&StoredImage> {
        self.clock += 1;
        let img = self.images.get_mut(&id)?;
        img.last_used = self.clock;
        Some(img)
    }

    pub fn delete(&mut self, id: u32) {
        self.remove(id);
        self.numbers.retain(|_, v| *v != id);
    }

    pub fn clear(&mut self) {
        self.images.clear();
        self.numbers.clear();
        self.bytes = 0;
    }

    fn remove(&mut self, id: u32) {
        if let Some(old) = self.images.remove(&id) {
            self.bytes -= old.image.rgba.len();
        }
    }

    fn evict(&mut self, keep: u32) {
        while self.bytes > self.cap {
            let Some(victim) =
                self.images.iter().filter(|(id, _)| **id != keep).min_by_key(|(_, img)| img.last_used).map(|(id, _)| *id)
            else {
                break;
            };
            self.delete(victim);
        }
    }
}
```

- [ ] **Step 3: Replies** (`reply.rs`, with its tests)

```rust
//! Answers: `ESC _ G i=<id>[,I=<n>][,p=<p>];OK ESC \` or `…;<CODE>:<message> ESC \`.

use super::Error;
use super::command::Command;

pub fn ok(cmd: &Command, out: &mut Vec<u8>) {
    if cmd.quiet == 0 {
        write(cmd, "OK", out);
    }
}

pub fn error(cmd: &Command, e: &Error, out: &mut Vec<u8>) {
    if cmd.quiet < 2 {
        write(cmd, &format!("{}:{}", e.code, e.msg), out);
    }
}

fn write(cmd: &Command, msg: &str, out: &mut Vec<u8>) {
    if !cmd.answer {
        return;
    }
    let mut keys = Vec::new();
    if cmd.id != 0 {
        keys.push(format!("i={}", cmd.id));
    }
    if cmd.number != 0 {
        keys.push(format!("I={}", cmd.number));
    }
    if cmd.placement != 0 {
        keys.push(format!("p={}", cmd.placement));
    }
    out.extend_from_slice(b"\x1b_G");
    out.extend_from_slice(keys.join(",").as_bytes());
    out.push(b';');
    out.extend(msg.chars().filter(|c| !c.is_control()).collect::<String>().bytes());
    out.extend_from_slice(b"\x1b\\");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::command::parse;

    fn reply(control: &str, err: Option<Error>) -> Vec<u8> {
        let cmd = parse(control.as_bytes()).0;
        let mut out = vec![];
        match err {
            Some(e) => error(&cmd, &e, &mut out),
            None => ok(&cmd, &mut out),
        }
        out
    }

    #[test]
    fn ok_and_quiet_levels() {
        assert_eq!(reply("i=31", None), b"\x1b_Gi=31;OK\x1b\\");
        assert_eq!(reply("i=31,I=2,p=5", None), b"\x1b_Gi=31,I=2,p=5;OK\x1b\\");
        assert!(reply("i=31,q=1", None).is_empty());
        assert!(reply("f=24", None).is_empty(), "no id, no reply");
        let e = || Some(Error::new("ENOENT", "gone\x1b]x"));
        assert_eq!(reply("i=3,q=1", e()), b"\x1b_Gi=3;ENOENT:gone]x\x1b\\");
        assert!(reply("i=3,q=2", e()).is_empty());
    }
}
```

- [ ] **Step 4: Engine — failing tests** (append to `graphics/mod.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> (Graphics, Arc<Mutex<ImageStore>>) {
        let store = Arc::new(Mutex::new(ImageStore::new(STORE_CAP)));
        (Graphics::new(store.clone()), store)
    }

    fn run(g: &mut Graphics, bytes: &[u8]) -> Vec<u8> {
        let mut out = vec![];
        g.feed(bytes, &mut out);
        out
    }

    #[test]
    fn capability_probe_is_answered_and_not_stored() {
        let (mut g, store) = engine();
        assert_eq!(run(&mut g, b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\"), b"\x1b_Gi=31;OK\x1b\\");
        assert_eq!(store.lock().unwrap().bytes(), 0);
    }

    #[test]
    fn chunked_transmit_and_virtual_place() {
        let (mut g, store) = engine();
        // RGBA 2x1 = bytes 1..=8 = "AQIDBAUGBwg=", sent as two chunks.
        let out = run(&mut g, b"\x1b_Ga=T,U=1,i=5,f=32,s=2,v=1,c=3,r=2,q=1,m=1;AQID\x1b\\\x1b_Gm=0;BAUGBwg=\x1b\\");
        assert!(out.is_empty());
        let mut s = store.lock().unwrap();
        let img = s.get(5).unwrap();
        assert_eq!(&*img.image.rgba, &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(img.placement(0), Some(VirtualPlacement { id: 0, cols: 3, rows: 2 }));
    }

    #[test]
    fn direct_placement_is_refused() {
        let (mut g, store) = engine();
        assert!(run(&mut g, b"\x1b_Ga=T,i=6,f=24,s=1,v=1;AAAA\x1b\\").starts_with(b"\x1b_Gi=6;ENOTSUPPORTED:"));
        assert!(store.lock().unwrap().get(6).is_none());
    }

    #[test]
    fn delete_by_id() {
        let (mut g, store) = engine();
        run(&mut g, b"\x1b_Ga=t,i=7,f=24,s=1,v=1,q=2;AAAA\x1b\\");
        assert!(store.lock().unwrap().get(7).is_some());
        run(&mut g, b"\x1b_Ga=d,d=I,i=7,q=2\x1b\\");
        assert!(store.lock().unwrap().get(7).is_none());
    }

    #[test]
    fn number_gets_an_id() {
        let (mut g, _) = engine();
        let out = run(&mut g, b"\x1b_Ga=t,I=3,f=24,s=1,v=1;AAAA\x1b\\");
        assert!(out.starts_with(b"\x1b_Gi=") && out.ends_with(b",I=3;OK\x1b\\"), "{:?}", String::from_utf8_lossy(&out));
    }

    #[test]
    fn errors_are_answered() {
        let (mut g, _) = engine();
        assert!(run(&mut g, b"\x1b_Gi=4,f=7;AAAA\x1b\\").starts_with(b"\x1b_Gi=4;EINVAL:"));
        assert!(run(&mut g, b"\x1b_Ga=p,U=1,i=99\x1b\\").starts_with(b"\x1b_Gi=99;ENOENT:"));
        assert!(run(&mut g, b"\x1b_Ga=f,i=8\x1b\\").starts_with(b"\x1b_Gi=8;EINVAL:"));
    }
}
```

- [ ] **Step 5: Run to verify failure**

Run: `cargo test -p tern-term graphics`
Expected: compile errors (`Graphics`, `STORE_CAP` not found).

- [ ] **Step 6: Engine — implement** (`graphics/mod.rs` becomes)

```rust
//! Kitty graphics protocol, Unicode-placeholder subset
//! (https://sw.kovidgoyal.net/kitty/graphics-protocol/).
//!
//! [`Graphics`] taps the raw PTY output for `ESC _ G <control> ; <payload> ESC \`
//! commands, keeps transmitted images in an [`ImageStore`] and produces the
//! replies. Programs show an image by printing U+10EEEE placeholder cells (see
//! [`placeholder`]). Placements floating over the grid ("direct placements")
//! are not supported and answered with `ENOTSUPPORTED`.

mod apc;
mod command;
mod load;
pub mod placeholder;
mod reply;
mod store;

use std::sync::{Arc, Mutex};

pub use load::Image;
pub use store::{ImageStore, StoredImage, VirtualPlacement};

use apc::{Apc, ApcParser};
use command::{Action, Command};

/// Most base64 bytes one transmission may carry, all chunks together.
const MAX_TRANSMISSION: usize = 64 * 1024 * 1024;
/// Decoded pixels kept per session before the least recently used images go.
pub const STORE_CAP: usize = 256 * 1024 * 1024;

/// A protocol error, answered as `<code>:<msg>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub code: &'static str,
    pub msg: String,
}

impl Error {
    pub(crate) fn new(code: &'static str, msg: impl Into<String>) -> Self {
        Self { code, msg: msg.into() }
    }
}

/// A chunked transmission waiting for its last chunk (`m=0`).
struct Pending {
    cmd: Command,
    data: Vec<u8>,
    overflow: bool,
}

pub struct Graphics {
    parser: ApcParser,
    store: Arc<Mutex<ImageStore>>,
    pending: Option<Pending>,
}

impl Graphics {
    pub fn new(store: Arc<Mutex<ImageStore>>) -> Self {
        Self { parser: ApcParser::new(), store, pending: None }
    }

    /// Feeds raw program output; appends what must be written back to the
    /// program to `reply`.
    pub fn feed(&mut self, bytes: &[u8], reply: &mut Vec<u8>) {
        let mut found = Vec::new();
        self.parser.feed(bytes, |apc| found.push(apc));
        for apc in found {
            self.handle(apc, reply);
        }
    }

    fn handle(&mut self, apc: Apc, reply: &mut Vec<u8>) {
        let (control, payload) = match apc {
            Apc::TooLarge { control } => {
                self.pending = None;
                reply::error(&command::parse(&control).0, &Error::new("EFBIG", "command too large"), reply);
                return;
            },
            Apc::Command { control, payload } => (control, payload),
        };
        if let Some(mut p) = self.pending.take() {
            // A continuation chunk: only `m` matters.
            let more = command::parse(&control).0.more;
            if !p.overflow && p.data.len() + payload.len() > MAX_TRANSMISSION {
                p.overflow = true;
                p.data = Vec::new();
            }
            if !p.overflow {
                p.data.extend_from_slice(&payload);
            }
            if more {
                self.pending = Some(p);
            } else if p.overflow {
                reply::error(&p.cmd, &Error::new("EFBIG", "transmission too large"), reply);
            } else {
                self.execute(p.cmd, &p.data, reply);
            }
            return;
        }
        let (cmd, err) = command::parse(&control);
        if let Some(e) = err {
            reply::error(&cmd, &e, reply);
        } else if cmd.more && matches!(cmd.action, Action::Transmit | Action::TransmitAndPlace | Action::Query) {
            self.pending = Some(Pending { cmd, data: payload, overflow: false });
        } else {
            self.execute(cmd, &payload, reply);
        }
    }

    fn execute(&mut self, mut cmd: Command, payload: &[u8], reply: &mut Vec<u8>) {
        let result = match cmd.action {
            Action::Delete => {
                self.delete(&cmd);
                return;
            },
            Action::Query => load::load(&cmd, payload).map(drop),
            Action::Transmit | Action::TransmitAndPlace => self.transmit(&mut cmd, payload),
            Action::Place => self.place(&cmd),
            Action::Other(a) => Err(Error::new("EINVAL", format!("unsupported action {}", a as char))),
        };
        match result {
            Ok(()) => reply::ok(&cmd, reply),
            Err(e) => reply::error(&cmd, &e, reply),
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, ImageStore>, Error> {
        self.store.lock().map_err(|_| Error::new("EIO", "image store unavailable"))
    }

    fn transmit(&mut self, cmd: &mut Command, payload: &[u8]) -> Result<(), Error> {
        if cmd.action == Action::TransmitAndPlace && !cmd.unicode {
            return Err(Error::new("ENOTSUPPORTED", "only Unicode placeholder placements are supported"));
        }
        let image = load::load(cmd, payload)?;
        let mut store = self.lock()?;
        if cmd.id == 0 {
            cmd.id = store.allocate_id();
        }
        if cmd.number != 0 {
            store.set_number(cmd.number, cmd.id);
        }
        store.insert(cmd.id, image);
        if cmd.action == Action::TransmitAndPlace {
            store.place(cmd.id, VirtualPlacement { id: cmd.placement, cols: cmd.cols, rows: cmd.rows })?;
        }
        Ok(())
    }

    fn place(&mut self, cmd: &Command) -> Result<(), Error> {
        if !cmd.unicode {
            return Err(Error::new("ENOTSUPPORTED", "only Unicode placeholder placements are supported"));
        }
        let mut store = self.lock()?;
        let id = store.resolve(cmd.id, cmd.number).ok_or_else(|| Error::new("ENOENT", "no such image"))?;
        store.place(id, VirtualPlacement { id: cmd.placement, cols: cmd.cols, rows: cmd.rows })
    }

    fn delete(&mut self, cmd: &Command) {
        let Ok(mut store) = self.lock() else { return };
        match cmd.delete {
            0 | b'a' | b'A' => store.clear(),
            b'i' | b'I' | b'n' | b'N' => {
                if let Some(id) = store.resolve(cmd.id, cmd.number) {
                    store.delete(id);
                }
            },
            // Deleting by position or z-index means nothing without direct placements.
            _ => {},
        }
    }
}
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test -p tern-term graphics`
Expected: all graphics tests pass (placeholder 7, apc 5, command 4, load 8, store 3, reply 1, engine 6).

- [ ] **Step 8: Commit**

```bash
cargo fmt && cargo clippy -p tern-term --all-targets
git add crates/tern-term/src/graphics
git commit -m "Store kitty graphics images and answer commands"
```

---

### Task 6: Wire graphics into the PTY session

**Files:**
- Modify: `crates/tern-term/src/session.rs` (`SpawnConfig`, `TapReader`, `Session`)
- Modify: `crates/tern-term/tests/session.rs` (helper + 2 tests)
- Modify: `crates/tern-app/src/workspace.rs:~731` (`SpawnConfig { … graphics }`)
- Modify: `crates/tern-app/src/terminal_view.rs:~96-107` (`TerminalConfig.graphics`)

**Interfaces:**
- Consumes: `graphics::{Graphics, ImageStore, STORE_CAP}`, `placeholder::{PlaceholderCell, PLACEHOLDER}`.
- Produces: `SpawnConfig.graphics: bool`; `Session::images(&self) -> Option<&Arc<Mutex<ImageStore>>>`; `TerminalConfig.graphics: bool`.

- [ ] **Step 1: Failing integration tests** (`crates/tern-term/tests/session.rs`)

Add `graphics: true,` to the `SpawnConfig` in `spawn()`, the imports
`use tern_term::graphics::placeholder::{PLACEHOLDER, PlaceholderCell};`, and:
```rust
#[test]
fn graphics_query_is_answered() {
    let (s, rx) = spawn(
        "/bin/sh",
        &["-c", r"stty raw -echo; printf '\033_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\033\\'; dd bs=1 count=12 2>/dev/null | tr '\033' E"],
    );
    wait_exit(&rx, |_| {});
    assert!(row_text(&s, 0).contains(r"E_Gi=31;OKE\"), "{:?}", row_text(&s, 0));
}

#[test]
fn placeholder_cells_reach_the_grid() {
    // A red 1x1 RGB image (base64 "/wAA"), then one U+10EEEE cell in colour 7 with row 0 / column 0 diacritics.
    let (s, rx) = spawn(
        "/bin/sh",
        &["-c", r"printf '\033_Ga=T,U=1,i=7,f=24,s=1,v=1,c=1,r=1,q=2;/wAA\033\\\033[38;5;7m\364\216\273\256\314\205\314\205\033[0m'"],
    );
    wait_exit(&rx, |_| {});
    {
        let mut store = s.images().unwrap().lock().unwrap();
        assert_eq!(&*store.get(7).expect("image stored").image.rgba, &[255, 0, 0, 255]);
    }
    let term = s.term().lock();
    let cell = &term.grid()[Line(0)][Column(0)];
    assert_eq!(cell.c, PLACEHOLDER);
    let ph = PlaceholderCell::decode(cell.fg, cell.underline_color(), cell.zerowidth().unwrap_or(&[]), None);
    assert_eq!(ph, Some(PlaceholderCell { image_id: 7, placement_id: 0, row: 0, col: 0 }));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p tern-term --test session`
Expected: compile error (no field `graphics`, no method `images`).

- [ ] **Step 3: Implement in `session.rs`**

1. Imports: `use crate::graphics::{Graphics, ImageStore, STORE_CAP};`; update the module doc's last sentence to: "We wrap the PTY so every byte read is also fed to the [`OscScanner`] for Claude Code's status/notification sequences and, when enabled, to [`Graphics`] for kitty graphics commands."
2. `SpawnConfig` gains, after `scrollback`:
```rust
    /// Answer kitty graphics commands and keep their images (shown through
    /// Unicode placeholder cells).
    pub graphics: bool,
```
3. `TapReader` gains fields:
```rust
    graphics: Option<Graphics>,
    /// Scratch buffer for graphics replies.
    replies: Vec<u8>,
```
and at the end of `read()` before `Ok(n)`:
```rust
        if let Some(g) = &mut self.graphics {
            self.replies.clear();
            g.feed(&buf[..n], &mut self.replies);
            if !self.replies.is_empty() {
                let mut f = &self.file;
                let _ = f.write_all(&self.replies);
            }
        }
```
4. `Session` gains field `images: Option<Arc<Mutex<ImageStore>>>,` and method:
```rust
    /// The images kitty graphics commands transmitted, when graphics are on.
    pub fn images(&self) -> Option<&Arc<Mutex<ImageStore>>> {
        self.images.as_ref()
    }
```
5. In `spawn`, after the `TERM_PROGRAM` line:
```rust
        let images = cfg.graphics.then(|| Arc::new(Mutex::new(ImageStore::new(STORE_CAP))));
        if cfg.graphics {
            // Claude Code draws images only on terminals it recognises by name
            // unless told to trust the graphics probe.
            env.entry("CLAUDE_CODE_FORCE_TERMINAL_IMAGES".into()).or_insert_with(|| "1".into());
        }
```
and build the reader with
```rust
        let reader = TapReader {
            file: pty.file().try_clone()?,
            scanner: OscScanner::new(),
            shared: shared.clone(),
            last_probe_reply: None,
            graphics: images.clone().map(Graphics::new),
            replies: Vec::new(),
        };
```
and return `Ok(Self { term, shared, pid, tty, images })`.

- [ ] **Step 4: Run integration tests**

Run: `cargo test -p tern-term --test session`
Expected: all pass, including the two new ones.

- [ ] **Step 5: tern-app config**

`terminal_view.rs` `TerminalConfig`: add field
```rust
    /// Show kitty graphics images (off with `TERN_GRAPHICS=0`).
    pub graphics: bool,
```
and in `Default`: `graphics: std::env::var("TERN_GRAPHICS").map_or(true, |v| v != "0"),`.
`workspace.rs` `spawn_config`: add `graphics: self.services.terminal.graphics,` after `scrollback: 10_000,`.

- [ ] **Step 6: Gates and commit**

```bash
cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace
git add crates
git commit -m "Answer kitty graphics commands in terminal sessions"
```

---

### Task 7: Draw placeholder cells

**Files:**
- Modify: `crates/tern-app/Cargo.toml` (`image` dep)
- Modify: `crates/tern-app/src/terminal_view.rs` (cache field, `Frame`, `build_frame`, `paint`, `render_image`)

**Interfaces:**
- Consumes: `Session::images()`, `ImageStore::get`, `StoredImage::{image, generation, placement}`, `placeholder::{PLACEHOLDER, PlaceholderCell, box_cells, fit}`, `graphics::Image`.

- [ ] **Step 1: Dependency**

`crates/tern-app/Cargo.toml` `[dependencies]`: `image = { version = "0.25", default-features = false }` (same major as GPUI's, so `image::Frame` is GPUI's type).

- [ ] **Step 2: Types and cache**

In `terminal_view.rs`:
- imports: `use std::collections::HashMap;`, `use std::time::Instant;` (beside `Duration`), `use tern_term::graphics::placeholder::{self, PLACEHOLDER, PlaceholderCell};`.
- constant under the imports: 
```rust
/// Decoded images not drawn for this long are released from the GPU atlas.
const IMAGE_CACHE_TTL: Duration = Duration::from_secs(10);
```
- `TerminalView` field (after `agent`):
```rust
    /// GPU-ready images by (image id, generation), with when each was last drawn.
    images: HashMap<(u32, u64), (Arc<RenderImage>, Instant)>,
```
initialised `images: HashMap::new(),` in `new`.
- New structs near `Frame`:
```rust
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
```
- `Frame` gains `images: Vec<FrameImage>, dropped: Vec<Arc<RenderImage>>,` (initialise both `vec![]` in `build_frame`).
- Helper (near `rgb_to_hsla`):
```rust
/// GPUI wants BGRA.
fn render_image(img: &tern_term::graphics::Image) -> Arc<RenderImage> {
    let mut bgra = img.rgba.to_vec();
    for px in bgra.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let buf = image::RgbaImage::from_raw(img.width, img.height, bgra).expect("buffer matches the image size");
    Arc::new(RenderImage::new(vec![image::Frame::new(buf)]))
}
```

- [ ] **Step 3: Collect runs in `build_frame`**

Before the cell loop: `let mut placeholders: Vec<PlaceholderRun> = Vec::new();`
In the loop, right after `if is_cursor { … }` and before the `HIDDEN`/space check:
```rust
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
```

- [ ] **Step 4: Resolve runs to images** (after the loop's trailing `flush` calls, still holding `term`)

```rust
    // Placeholder runs -> the part of each image they show.
    let mut wanted = Vec::new();
    if !placeholders.is_empty()
        && let Some(store) = session.images()
        && let Ok(mut store) = store.lock()
    {
        let (cw, ch) = (f32::from(m.width), f32::from(m.height));
        for run in &placeholders {
            let Some(img) = store.get(run.first.image_id) else { continue };
            let p = img.placement(run.first.placement_id);
            let (iw, ih) = (img.image.width, img.image.height);
            let (cols, rows) = placeholder::box_cells(p.map_or(0, |p| p.cols), p.map_or(0, |p| p.rows), iw, ih, cw, ch);
            let (x, y, w, h) = placeholder::fit(cols, rows, cw, ch, iw, ih);
            let run_origin = point(bounds.origin.x + m.width * run.start as f32, bounds.origin.y + m.height * run.row as f32);
            let box_x = run_origin.x - m.width * run.first.col as f32;
            let box_y = run_origin.y - m.height * run.first.row as f32;
            let clip = Bounds::new(run_origin, size(m.width * (run.end - run.start) as f32, m.height));
            let at = Bounds::new(point(box_x + px(x), box_y + px(y)), size(px(w), px(h)));
            wanted.push(((run.first.image_id, img.generation), img.image.clone(), clip, at));
        }
    }
```
After `drop(term);` (the `session` borrow is no longer used):
```rust
    let now = Instant::now();
    for (key, img, clip, at) in wanted {
        let entry = view.images.entry(key).or_insert_with(|| (render_image(&img), now));
        entry.1 = now;
        frame.images.push(FrameImage { clip, bounds: at, data: entry.0.clone() });
    }
    view.images.retain(|_, (data, used)| {
        let keep = now.duration_since(*used) < IMAGE_CACHE_TTL;
        if !keep {
            frame.dropped.push(data.clone());
        }
        keep
    });
```

- [ ] **Step 5: Paint**

In `TerminalElement::paint`, before `window.with_content_mask(…)`:
```rust
        for data in frame.dropped.drain(..) {
            let _ = window.drop_image(data);
        }
```
Inside the content mask, right after the `rects` loop:
```rust
            for img in &frame.images {
                let _ = window.paint_image(img.clip, img.bounds, Corners::default(), img.data.clone(), 0, false);
            }
```

- [ ] **Step 6: Build and gates**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace && cargo build --release -p tern-app`
Expected: clean (only the accepted `block v0.1.6` note).

- [ ] **Step 7: Manual check with a hand-made placeholder image**

Write `/private/tmp/claude-501/-Users-cuongpham-ws-tern-ws/2c7be33c-9406-4751-99bc-5373d58d2a34/scratchpad/kitty/show.py` (outside the repo):
```python
#!/usr/bin/env python3
"""Show a PNG through kitty graphics Unicode placeholders: show.py IMAGE COLS ROWS"""
import base64, sys
path, cols, rows = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
D = [int(l.split(';')[0], 16) for l in open(sys.argv[0].rsplit('/', 1)[0] + '/diacritics.txt') if l[0] != '#' and l.strip()]
img_id = 42
sys.stdout.write(f"\033_Ga=T,U=1,i={img_id},f=100,t=f,c={cols},r={rows},q=2;{base64.b64encode(path.encode()).decode()}\033\\")
for r in range(rows):
    sys.stdout.write(f"\033[38;5;{img_id}m" + "".join("\U0010EEEE" + chr(D[r]) + chr(D[c]) for c in range(cols)) + "\033[39m\n")
sys.stdout.flush()
```
Run `scripts/dev.sh` in the worktree (background), open a shell tab in the dev Tern, run `python3 <scratchpad>/kitty/show.py <some.png> 30 10`, then `screencapture -x <scratchpad>/kitty/shot.png` and look at it. Check: image visible, scrolls with output (`seq 100`), survives a pane resize, `clear` removes it. Then relaunch with `TERN_GRAPHICS=0` and confirm nothing is drawn (the placeholder cells are blank-ish glyphs, no image).

- [ ] **Step 8: Commit**

```bash
git add Cargo.lock crates/tern-app
git commit -m "Draw kitty graphics Unicode placeholder images"
```

---

### Task 8: End-to-end with Claude Code

- [ ] **Step 1:** In the dev Tern, open a Claude tab; in it check `echo $CLAUDE_CODE_FORCE_TERMINAL_IMAGES` from a shell tab prints `1`.
- [ ] **Step 2:** In Claude Code, paste a screenshot (⌘V). Expected: `[Image #1]` in the prompt (needs the empty-bracketed-paste fix from the 0.1.1 work; if it is not merged yet, use `@/path/to/file.png` instead).
- [ ] **Step 3:** Ask the person to look at images Claude Code draws (e.g. the mod from Task 9, or `/status`'s Terminal capabilities if shown). Record what works and what doesn't; fix within this plan's scope or note follow-ups.

---

### Task 9: Claude Code mod — pasted image preview

**Files** (outside the repo): `~/.claude/dev-mods/<session id>/paste-preview/`
- `.claude-plugin/plugin.json`: `{ "name": "paste-preview", "version": "0.1.0", "description": "Shows pasted images above the prompt", "types": "./types/index.d.ts" }`
- `hooks/hooks.json`: `{ "modules": ["./register.tsx"] }`
- `types/index.d.ts`
- `hooks/register.tsx`
- `hooks/register.test.ts`

- [ ] **Step 1: Contract** (`types/index.d.ts`)

```ts
export type Pasted = { n: number; file: string }

declare module 'claude-code' {
  interface PluginState {
    'paste-preview': { pasted: Pasted[] }
  }
}
```

- [ ] **Step 2: Module** (`hooks/register.tsx`)

```tsx
import { atom, read, update } from 'claude-code'
import type { Register } from 'claude-code'

import type { Pasted } from '../types'

const pasted = atom({ plugin: 'paste-preview', key: 'pasted' } as const, [] as Pasted[])

const TOKEN = /\[Image #(\d+)\]/g
const TMP_ROOTS = ['/private/tmp', '/tmp']
const MAX_SHOWN = 3

const same = (a: Pasted[], b: Pasted[]) => a.length === b.length && a.every((p, i) => p.file === b[i].file)

export const register: Register = on => {
  let imagesDir: string | undefined

  // Pasted images are kept under <tmp>/claude-<uid>/<project>/<session id>/images/<n>.png.
  const findImagesDir = async ($: Parameters<Parameters<typeof on>[1]>[0]) => {
    if (imagesDir) return imagesDir
    const session = await $.session.id()
    for (const root of TMP_ROOTS) {
      for (const user of await $.fs.list(root).catch(() => [])) {
        if (!user.name.startsWith('claude-')) continue
        for (const project of await $.fs.list(`${root}/${user.name}`).catch(() => [])) {
          const dir = `${root}/${user.name}/${project.name}/${session}/images`
          if (await $.fs.exists(dir)) return (imagesDir = dir)
        }
      }
    }
    return undefined
  }

  on('prompt.change', async ($, e, next) => {
    const result = await next(e)
    const numbers = [...new Set([...result.text.matchAll(TOKEN)].map(m => Number(m[1])))].slice(-MAX_SHOWN)
    const found: Pasted[] = []
    const dir = numbers.length > 0 ? await findImagesDir($) : undefined
    for (const n of dir ? numbers : []) {
      const file = `${dir}/${n}.png`
      if (await $.fs.exists(file)) found.push({ n, file })
    }
    await update($, pasted, prev => (same(prev, found) ? prev : found))
    return result
  })

  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    const list = await read($, pasted)
    if (list.length === 0 || e.surface !== 'terminal') return next(e)
    const { Box, Image, Text } = $.ui.resolve(e)
    return (
      <Box flexDirection="row" gap={2}>
        {list.map(p => (
          <Box key={p.file} flexDirection="column">
            <Image source={{ file: p.file, format: 'png' }} columns={24} rows={8} alt={`Image #${p.n}`} />
            <Text dimColor>Image #{p.n}</Text>
          </Box>
        ))}
      </Box>
    )
  })
}
```
The exact `$` type for `findImagesDir`, `FsEntry` field names, and `Box` props are checked against `claude-code.d.ts` in Step 4; adjust names there, not the behaviour.

- [ ] **Step 3: Test** (`hooks/register.test.ts`) — follow the test shape in `plugin-authoring/reference.md` ("what a test holds"): a draft containing `[Image #1]` with a fake fs where `<tmp>/claude-501/proj/<session>/images/1.png` exists → state `pasted` is `[{ n: 1, file: … }]`; a draft without tokens → `[]`.

- [ ] **Step 4: Validate and type-check**

Run: `claude plugin validate <mod folder>`, `tsc -p <mod folder>` (or the skill's out-of-folder tsconfig on first write), `claude plugin test <mod folder>`.
Expected: no errors.

- [ ] **Step 5: Try it** — with hot reloading enabled, paste an image in the dev Tern's Claude tab: a thumbnail appears above the prompt, goes away after submit; in a non-graphics terminal the alt text `Image #1` shows instead.

---

### Task 10: Finish

- [ ] **Step 1:** `cargo fmt --check && cargo clippy --workspace --all-targets && cargo test --workspace && cargo build --release`
- [ ] **Step 2:** Use superpowers:finishing-a-development-branch (merge `kitty-graphics` into `main` or open a PR — ask the person).
