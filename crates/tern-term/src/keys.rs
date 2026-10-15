//! Keyboard encoding: UI-agnostic key events → bytes for the PTY.
//!
//! Supports legacy xterm encoding and the Kitty keyboard protocol
//! ("disambiguate" and "report all keys as escape codes" flags), which Claude
//! Code enables so that e.g. Shift+Enter is distinguishable from Enter.

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

/// Encode a key press. `text` is what the platform would insert (already
/// shifted/composed), if anything.
pub fn encode(key: Key, mods: Mods, text: Option<&str>, mode: TermMode) -> Option<Vec<u8>> {
    let kitty_all = mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC);
    let kitty = kitty_all || mode.contains(TermMode::DISAMBIGUATE_ESC_CODES);
    if kitty {
        if let Some(b) = encode_kitty(key, mods, text, kitty_all) {
            return Some(b);
        }
    }
    encode_legacy(key, mods, text, mode)
}

fn csi_u(code: u32, mods: Mods) -> Vec<u8> {
    if mods.any() {
        format!("\x1b[{code};{}u", mods.param()).into_bytes()
    } else {
        format!("\x1b[{code}u").into_bytes()
    }
}

fn encode_kitty(key: Key, mods: Mods, text: Option<&str>, all: bool) -> Option<Vec<u8>> {
    match key {
        Key::Escape => Some(csi_u(27, mods)),
        Key::Enter if all || mods.any() => Some(csi_u(13, mods)),
        Key::Tab if all || mods.any() => Some(csi_u(9, mods)),
        Key::Backspace if all || mods.any() => Some(csi_u(127, mods)),
        Key::Char(c) => {
            // Plain/shifted text stays text unless reporting all keys.
            let textual = !mods.ctrl && !mods.alt && !mods.meta;
            if textual && !all {
                return None;
            }
            let _ = text;
            Some(csi_u(c.to_ascii_lowercase() as u32, mods))
        },
        // Functional keys use the legacy CSI forms, which kitty keeps.
        _ => None,
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
    let tilde = |n: u8| -> Vec<u8> {
        if mods.any() { format!("\x1b[{n};{m}~").into_bytes() } else { format!("\x1b[{n}~").into_bytes() }
    };

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
        Key::F(n) => {
            let code = match n {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                12 => 24,
                _ => return None,
            };
            tilde(code)
        },
        Key::Char(c) => {
            if mods.ctrl {
                let ctl = ctrl_byte(c)?;
                with_alt(vec![ctl])
            } else if let Some(t) = text.filter(|t| !t.is_empty()) {
                with_alt(t.as_bytes().to_vec())
            } else if mods.alt {
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
}
