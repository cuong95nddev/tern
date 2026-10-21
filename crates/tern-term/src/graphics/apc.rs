//! Pulls graphics commands (`ESC _ G <control> ; <payload> ESC \`) out of the
//! raw PTY byte stream. The VT parser sees the same bytes and ignores APC
//! strings, so nothing is taken out of the stream.

/// Largest command (control data and payload) buffered.
pub const MAX_COMMAND: usize = 16 * 1024 * 1024;
/// Control data kept from an oversized command, so it can still be answered.
const MAX_CONTROL: usize = 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum Apc {
    Command {
        control: Vec<u8>,
        payload: Vec<u8>,
    },
    /// Longer than [`MAX_COMMAND`]; the payload was dropped.
    TooLarge {
        control: Vec<u8>,
    },
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
                State::Ground => match find_esc(&bytes[i..]) {
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
                    let end = find_esc(&bytes[i..]).map_or(bytes.len(), |o| i + o);
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

fn find_esc(hay: &[u8]) -> Option<usize> {
    hay.iter().position(|&c| c == 0x1b)
}

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
