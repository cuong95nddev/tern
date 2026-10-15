//! Keyboard encoding: UI-agnostic key events → bytes for the PTY.
//!
//! Supports legacy xterm encoding and the Kitty keyboard protocol (all five
//! flags: disambiguate, event types, alternate keys, all keys as escape codes,
//! associated text), which Claude Code enables so that e.g. Shift+Enter is
//! distinguishable from Enter.

use alacritty_terminal::term::TermMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A printable key; the char is the unshifted base key (e.g. 'a', '1', '/').
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F(u8),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub meta: bool,
}

impl Mods {
    fn any(self) -> bool {
        self.shift || self.alt || self.ctrl || self.meta
    }

    /// xterm / kitty modifier parameter (1 = none).
    fn param(self) -> u8 {
        1 + self.shift as u8 + (self.alt as u8) * 2 + (self.ctrl as u8) * 4 + (self.meta as u8) * 8
    }
}

/// Press, auto-repeat or release (the last two only matter to the Kitty
/// protocol's "report event types" flag).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum KeyEventKind {
    #[default]
    Press,
    Repeat,
    Release,
}

/// Encode a key press. `text` is what the platform would insert (already
/// shifted/composed), if anything.
pub fn encode(key: Key, mods: Mods, text: Option<&str>, mode: TermMode) -> Option<Vec<u8>> {
    encode_event(key, mods, text, mode, KeyEventKind::Press)
}

/// Encode a key event. Releases are only reported when the program asked
/// for them (Kitty flag 2); without the Kitty protocol a repeat is a press.
pub fn encode_event(key: Key, mods: Mods, text: Option<&str>, mode: TermMode, kind: KeyEventKind) -> Option<Vec<u8>> {
    let all = mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC);
    let kitty = all || mode.contains(TermMode::DISAMBIGUATE_ESC_CODES);
    let events = kitty && mode.contains(TermMode::REPORT_EVENT_TYPES);
    if kind == KeyEventKind::Release && !events {
        return None;
    }
    if !kitty {
        return encode_legacy(key, mods, text, mode);
    }
    // Plain text and unmodified Enter/Tab/Backspace stay legacy unless all
    // keys are reported; those never report a release.
    let textual = matches!(key, Key::Char(_)) && !mods.ctrl && !mods.alt && !mods.meta;
    let plain = matches!(key, Key::Enter | Key::Tab | Key::Backspace) && !mods.any();
    if !all && (textual || plain) {
        return if kind == KeyEventKind::Release { None } else { encode_legacy(key, mods, text, mode) };
    }
    Some(encode_kitty(key, mods, text, mode, kind))
}

/// `CSI number[:alternate] ; mods[:event] ; text terminator`, leaving out
/// trailing fields that hold defaults.
fn encode_kitty(key: Key, mods: Mods, text: Option<&str>, mode: TermMode, kind: KeyEventKind) -> Vec<u8> {
    let (number, term) = match key {
        Key::Escape => (27, 'u'),
        Key::Enter => (13, 'u'),
        Key::Tab => (9, 'u'),
        Key::Backspace => (127, 'u'),
        Key::Char(c) => (c.to_lowercase().next().unwrap_or(c) as u32, 'u'),
        Key::Up => (1, 'A'),
        Key::Down => (1, 'B'),
        Key::Right => (1, 'C'),
        Key::Left => (1, 'D'),
        Key::Home => (1, 'H'),
        Key::End => (1, 'F'),
        Key::Insert => (2, '~'),
        Key::Delete => (3, '~'),
        Key::PageUp => (5, '~'),
        Key::PageDown => (6, '~'),
        Key::F(1) => (1, 'P'),
        Key::F(2) => (1, 'Q'),
        // CSI R would read as a cursor position report.
        Key::F(3) => (13, '~'),
        Key::F(4) => (1, 'S'),
        Key::F(n @ 5..=12) => (f_tilde(n), '~'),
        Key::F(n) => (57376 + n.saturating_sub(13) as u32, 'u'),
    };
    let mut first = number.to_string();
    if let Key::Char(c) = key
        && mode.contains(TermMode::REPORT_ALTERNATE_KEYS)
        && mods.shift
        && c.is_lowercase()
    {
        first.push_str(&format!(":{}", c.to_uppercase().next().unwrap_or(c) as u32));
    }
    let event = match kind {
        KeyEventKind::Press => 1,
        KeyEventKind::Repeat => 2,
        KeyEventKind::Release => 3,
    };
    let event = if mode.contains(TermMode::REPORT_EVENT_TYPES) { event } else { 1 };
    let text: Option<String> = text
        .filter(|_| mode.contains(TermMode::REPORT_ASSOCIATED_TEXT) && kind != KeyEventKind::Release && term == 'u')
        .filter(|t| !t.is_empty() && !t.chars().any(char::is_control))
        .map(|t| t.chars().map(|c| (c as u32).to_string()).collect::<Vec<_>>().join(":"));
    let mut fields = vec![first];
    if mods.any() || event != 1 || text.is_some() {
        fields.push(if event == 1 { mods.param().to_string() } else { format!("{}:{event}", mods.param()) });
    }
    if let Some(t) = text {
        fields.push(t);
    }
    // `CSI 1 A` is just `CSI A`.
    if fields.len() == 1 && term != 'u' && term != '~' {
        fields.clear();
    }
    format!("\x1b[{}{term}", fields.join(";")).into_bytes()
}

fn f_tilde(n: u8) -> u32 {
    match n {
        5 => 15,
        6 => 17,
        7 => 18,
        8 => 19,
        9 => 20,
        10 => 21,
        11 => 23,
        12 => 24,
        // VT220 / xterm codes for F13–F20.
        13 => 25,
        14 => 26,
        15 => 28,
        16 => 29,
        17 => 31,
        18 => 32,
        19 => 33,
        20 => 34,
        _ => 0,
    }
}

fn encode_legacy(key: Key, mods: Mods, text: Option<&str>, mode: TermMode) -> Option<Vec<u8>> {
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let m = mods.param();
    let with_alt = |mut b: Vec<u8>| {
        if mods.alt {
            b.insert(0, 0x1b);
        }
        b
    };

    let cursor = |c: u8| -> Vec<u8> {
        if mods.any() {
            format!("\x1b[1;{m}{}", c as char).into_bytes()
        } else if app_cursor {
            vec![0x1b, b'O', c]
        } else {
            vec![0x1b, b'[', c]
        }
    };
    let tilde = |n: u8| -> Vec<u8> { if mods.any() { format!("\x1b[{n};{m}~").into_bytes() } else { format!("\x1b[{n}~").into_bytes() } };

    Some(match key {
        Key::Enter => with_alt(vec![b'\r']),
        Key::Tab if mods.shift => b"\x1b[Z".to_vec(),
        Key::Tab => with_alt(vec![b'\t']),
        Key::Backspace if mods.ctrl => with_alt(vec![0x08]),
        Key::Backspace => with_alt(vec![0x7f]),
        Key::Escape => with_alt(vec![0x1b]),
        Key::Up => cursor(b'A'),
        Key::Down => cursor(b'B'),
        Key::Right => cursor(b'C'),
        Key::Left => cursor(b'D'),
        Key::Home => cursor(b'H'),
        Key::End => cursor(b'F'),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::F(n @ 1..=4) => {
            let c = b"PQRS"[(n - 1) as usize];
            if mods.any() { format!("\x1b[1;{m}{}", c as char).into_bytes() } else { vec![0x1b, b'O', c] }
        },
        Key::F(n) => match f_tilde(n) {
            0 => return None,
            code => tilde(code as u8),
        },
        Key::Char(c) => {
            if mods.ctrl {
                let ctl = ctrl_byte(c)?;
                with_alt(vec![ctl])
            } else if let Some(t) = text.filter(|t| !t.is_empty()) {
                with_alt(t.as_bytes().to_vec())
            } else if mods.alt {
                // Option as Meta: the platform text is the Option-composed
                // char, so shift the base key ourselves (Alt+Shift+a = ESC A).
                let c = if mods.shift { c.to_uppercase().next().unwrap_or(c) } else { c };
                let mut buf = [0u8; 4];
                with_alt(c.encode_utf8(&mut buf).as_bytes().to_vec())
            } else {
                return None;
            }
        },
    })
}

fn ctrl_byte(c: char) -> Option<u8> {
    let c = c.to_ascii_lowercase();
    Some(match c {
        'a'..='z' => c as u8 - b'a' + 1,
        '@' | ' ' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '-' | '7' => 0x1f,
        '/' => 0x1f,
        '8' => 0x7f,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Mods = Mods { shift: false, alt: false, ctrl: false, meta: false };
    const SHIFT: Mods = Mods { shift: true, ..NONE };
    const CTRL: Mods = Mods { ctrl: true, ..NONE };
    const ALT: Mods = Mods { alt: true, ..NONE };

    fn legacy(k: Key, m: Mods, t: Option<&str>) -> Vec<u8> {
        encode(k, m, t, TermMode::empty()).unwrap()
    }

    #[test]
    fn legacy_basics() {
        assert_eq!(legacy(Key::Char('a'), NONE, Some("a")), b"a");
        assert_eq!(legacy(Key::Char('a'), SHIFT, Some("A")), b"A");
        assert_eq!(legacy(Key::Char('c'), CTRL, None), [3]);
        assert_eq!(legacy(Key::Char('b'), ALT, None), b"\x1bb");
        assert_eq!(legacy(Key::Enter, NONE, None), b"\r");
        assert_eq!(legacy(Key::Tab, SHIFT, None), b"\x1b[Z");
        assert_eq!(legacy(Key::Up, NONE, None), b"\x1b[A");
        assert_eq!(legacy(Key::Left, CTRL, None), b"\x1b[1;5D");
        assert_eq!(legacy(Key::Delete, NONE, None), b"\x1b[3~");
        assert_eq!(legacy(Key::F(1), NONE, None), b"\x1bOP");
        assert_eq!(legacy(Key::F(5), SHIFT, None), b"\x1b[15;2~");
    }

    #[test]
    fn app_cursor_mode() {
        let b = encode(Key::Up, NONE, None, TermMode::APP_CURSOR).unwrap();
        assert_eq!(b, b"\x1bOA");
    }

    #[test]
    fn unicode_text_passthrough() {
        assert_eq!(legacy(Key::Char('a'), NONE, Some("ầ")), "ầ".as_bytes());
    }

    #[test]
    fn kitty_disambiguate() {
        let m = TermMode::DISAMBIGUATE_ESC_CODES;
        assert_eq!(encode(Key::Enter, SHIFT, None, m).unwrap(), b"\x1b[13;2u");
        assert_eq!(encode(Key::Enter, NONE, None, m).unwrap(), b"\r");
        assert_eq!(encode(Key::Escape, NONE, None, m).unwrap(), b"\x1b[27u");
        assert_eq!(encode(Key::Char('c'), CTRL, None, m).unwrap(), b"\x1b[99;5u");
        assert_eq!(encode(Key::Char('a'), NONE, Some("a"), m).unwrap(), b"a");
        assert_eq!(encode(Key::Up, NONE, None, m).unwrap(), b"\x1b[A");
    }

    #[test]
    fn kitty_report_all() {
        let m = TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_ALL_KEYS_AS_ESC;
        assert_eq!(encode(Key::Enter, NONE, None, m).unwrap(), b"\x1b[13u");
        assert_eq!(encode(Key::Char('a'), NONE, Some("a"), m).unwrap(), b"\x1b[97u");
    }

    #[test]
    fn alt_shift_letter_keeps_shift() {
        let alt_shift = Mods { alt: true, shift: true, ..NONE };
        assert_eq!(legacy(Key::Char('a'), alt_shift, None), b"\x1bA");
        // Shifted punctuation arrives as the shifted key already.
        assert_eq!(legacy(Key::Char('!'), ALT, None), b"\x1b!");
    }

    #[test]
    fn legacy_f13_to_f20() {
        assert_eq!(legacy(Key::F(13), NONE, None), b"\x1b[25~");
        assert_eq!(legacy(Key::F(20), CTRL, None), b"\x1b[34;5~");
        assert!(encode(Key::F(21), NONE, None, TermMode::empty()).is_none());
    }

    #[test]
    fn release_needs_event_types() {
        let m = TermMode::DISAMBIGUATE_ESC_CODES;
        assert!(encode_event(Key::Char('c'), CTRL, None, m, KeyEventKind::Release).is_none());
        assert!(encode_event(Key::Up, NONE, None, TermMode::empty(), KeyEventKind::Release).is_none());
        assert_eq!(encode_event(Key::Up, NONE, None, TermMode::empty(), KeyEventKind::Repeat).unwrap(), b"\x1b[A");
    }

    #[test]
    fn kitty_event_types() {
        let m = TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_EVENT_TYPES;
        assert_eq!(encode_event(Key::Char('c'), CTRL, None, m, KeyEventKind::Release).unwrap(), b"\x1b[99;5:3u");
        assert_eq!(encode_event(Key::Escape, NONE, None, m, KeyEventKind::Repeat).unwrap(), b"\x1b[27;1:2u");
        assert_eq!(encode_event(Key::Up, NONE, None, m, KeyEventKind::Release).unwrap(), b"\x1b[1;1:3A");
        assert_eq!(encode_event(Key::Up, NONE, None, m, KeyEventKind::Press).unwrap(), b"\x1b[A");
        // Text and plain Enter stay legacy and report no release.
        assert!(encode_event(Key::Char('a'), NONE, Some("a"), m, KeyEventKind::Release).is_none());
        assert!(encode_event(Key::Enter, NONE, None, m, KeyEventKind::Release).is_none());
    }

    #[test]
    fn kitty_functional_keys() {
        let m = TermMode::DISAMBIGUATE_ESC_CODES;
        assert_eq!(encode(Key::Left, CTRL, None, m).unwrap(), b"\x1b[1;5D");
        assert_eq!(encode(Key::Delete, SHIFT, None, m).unwrap(), b"\x1b[3;2~");
        assert_eq!(encode(Key::F(3), NONE, None, m | TermMode::REPORT_ALL_KEYS_AS_ESC).unwrap(), b"\x1b[13~");
        assert_eq!(encode(Key::F(13), NONE, None, m | TermMode::REPORT_ALL_KEYS_AS_ESC).unwrap(), b"\x1b[57376u");
    }

    #[test]
    fn kitty_alternate_keys_and_text() {
        let m = TermMode::DISAMBIGUATE_ESC_CODES
            | TermMode::REPORT_ALL_KEYS_AS_ESC
            | TermMode::REPORT_ALTERNATE_KEYS
            | TermMode::REPORT_ASSOCIATED_TEXT;
        assert_eq!(encode(Key::Char('a'), SHIFT, Some("A"), m).unwrap(), b"\x1b[97:65;2;65u");
        assert_eq!(encode(Key::Char('a'), NONE, Some("a"), m).unwrap(), b"\x1b[97;1;97u");
        assert_eq!(encode(Key::Enter, NONE, Some("\n"), m).unwrap(), b"\x1b[13u");
    }
}
