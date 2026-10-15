//! Streaming scanner that taps the raw PTY byte stream for OSC sequences that
//! `alacritty_terminal` ignores but Claude Code relies on:
//!
//! - OSC 7501 — "program status" (`state=working:app=claude-code:...`), plus the
//!   `OSC 7501 ; ?` capability probe which we must answer.
//! - OSC 9 / OSC 777 — desktop notifications.
//!
//! The scanner never modifies the stream; bytes still flow to the VT parser.

/// Max payload we buffer for a single OSC; longer sequences are dropped.
const MAX_OSC_LEN: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OscSignal {
    /// `OSC 7501 ; ?` — the program asks whether we speak the protocol.
    ProgramStatusProbe,
    ProgramStatus(ProgramStatus),
    Notify {
        title: Option<String>,
        body: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProgramStatus {
    /// `working`, `blocked`, `idle`, `clear`, ... (kept as-is for forward compat).
    pub state: String,
    pub app: Option<String>,
    pub id: Option<String>,
    pub kind: Option<String>,
    pub progress: Option<u8>,
    pub title: Option<String>,
    pub msg: Option<String>,
}

/// Reply that tells the program we support OSC 7501.
pub const PROGRAM_STATUS_PROBE_REPLY: &[u8] = b"\x1b]7501;?\x1b\\";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Esc,
    /// Collecting OSC payload.
    Osc,
    /// Saw ESC inside OSC; expecting `\` to terminate.
    OscEsc,
}

#[derive(Debug)]
pub struct OscScanner {
    state: State,
    buf: Vec<u8>,
    overflow: bool,
}

impl Default for OscScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl OscScanner {
    pub fn new() -> Self {
        Self { state: State::Ground, buf: Vec::with_capacity(256), overflow: false }
    }

    /// Feed bytes, invoking `emit` for every recognised signal.
    pub fn feed(&mut self, bytes: &[u8], mut emit: impl FnMut(OscSignal)) {
        let mut i = 0;
        while i < bytes.len() {
            match self.state {
                State::Ground => {
                    // Fast path: skip to the next ESC.
                    match memchr(0x1b, &bytes[i..]) {
                        Some(off) => {
                            i += off + 1;
                            self.state = State::Esc;
                        },
                        None => return,
                    }
                },
                State::Esc => {
                    let b = bytes[i];
                    i += 1;
                    self.state = if b == b']' {
                        self.buf.clear();
                        self.overflow = false;
                        State::Osc
                    } else if b == 0x1b {
                        State::Esc
                    } else {
                        State::Ground
                    };
                },
                State::Osc => {
                    let b = bytes[i];
                    i += 1;
                    match b {
                        0x07 => self.finish(&mut emit),
                        0x1b => self.state = State::OscEsc,
                        // CAN / SUB abort the sequence.
                        0x18 | 0x1a => self.state = State::Ground,
                        _ => {
                            if self.buf.len() < MAX_OSC_LEN {
                                self.buf.push(b);
                            } else {
                                self.overflow = true;
                            }
                        },
                    }
                },
                State::OscEsc => {
                    let b = bytes[i];
                    if b == b'\\' {
                        i += 1;
                        self.finish(&mut emit);
                    } else {
                        // ESC not followed by `\` terminates the OSC and starts a new escape.
                        self.finish(&mut emit);
                        self.state = State::Esc;
                    }
                },
            }
        }
    }

    fn finish(&mut self, emit: &mut impl FnMut(OscSignal)) {
        self.state = State::Ground;
        if self.overflow {
            return;
        }
        if let Some(sig) = parse_osc(&self.buf) {
            emit(sig);
        }
    }
}

fn memchr(needle: u8, hay: &[u8]) -> Option<usize> {
    hay.iter().position(|&b| b == needle)
}

fn parse_osc(payload: &[u8]) -> Option<OscSignal> {
    let s = std::str::from_utf8(payload).ok()?;
    let (code, rest) = match s.find(';') {
        Some(idx) => (&s[..idx], &s[idx + 1..]),
        None => (s, ""),
    };
    match code {
        "7501" => {
            if rest.starts_with('?') {
                return Some(OscSignal::ProgramStatusProbe);
            }
            Some(OscSignal::ProgramStatus(parse_program_status(rest)?))
        },
        // OSC 9 ; body  (iTerm2 growl). OSC 9 ; 4 ; ... is ConEmu progress — ignore.
        "9" => {
            if rest.starts_with("4;") || rest.is_empty() {
                return None;
            }
            Some(OscSignal::Notify { title: None, body: rest.to_owned() })
        },
        // OSC 777 ; notify ; title ; body
        "777" => {
            let mut parts = rest.splitn(3, ';');
            if parts.next()? != "notify" {
                return None;
            }
            let title = parts.next().map(str::to_owned);
            let body = parts.next().unwrap_or_default().to_owned();
            Some(OscSignal::Notify { title, body })
        },
        _ => None,
    }
}

fn parse_program_status(rest: &str) -> Option<ProgramStatus> {
    let mut st = ProgramStatus::default();
    for field in rest.split(':') {
        let Some((k, v)) = field.split_once('=') else { continue };
        match k {
            "state" => st.state = v.to_owned(),
            "app" => st.app = Some(v.to_owned()),
            "id" => st.id = Some(v.to_owned()),
            "kind" => st.kind = Some(v.to_owned()),
            "progress" => st.progress = v.parse().ok(),
            "title" => st.title = Some(v.to_owned()),
            "msg" => st.msg = Some(v.to_owned()),
            _ => {},
        }
    }
    (!st.state.is_empty()).then_some(st)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(chunks: &[&[u8]]) -> Vec<OscSignal> {
        let mut s = OscScanner::new();
        let mut out = vec![];
        for c in chunks {
            s.feed(c, |sig| out.push(sig));
        }
        out
    }

    #[test]
    fn program_status_bel_terminated() {
        let out = scan(&[b"hello\x1b]7501;state=working:app=claude-code:progress=40\x07world"]);
        assert_eq!(out.len(), 1);
        let OscSignal::ProgramStatus(st) = &out[0] else { panic!() };
        assert_eq!(st.state, "working");
        assert_eq!(st.app.as_deref(), Some("claude-code"));
        assert_eq!(st.progress, Some(40));
    }

    #[test]
    fn program_status_st_terminated_across_chunks() {
        let out = scan(&[b"\x1b]75", b"01;state=blocked:kind=permi", b"ssion\x1b", b"\\"]);
        let OscSignal::ProgramStatus(st) = &out[0] else { panic!("{out:?}") };
        assert_eq!(st.state, "blocked");
        assert_eq!(st.kind.as_deref(), Some("permission"));
    }

    #[test]
    fn probe() {
        assert_eq!(scan(&[b"\x1b]7501;?\x1b\\"]), vec![OscSignal::ProgramStatusProbe]);
    }

    #[test]
    fn notifications() {
        let out = scan(&[b"\x1b]9;Done!\x07\x1b]777;notify;Claude;Need input\x07\x1b]9;4;1;50\x07"]);
        assert_eq!(
            out,
            vec![
                OscSignal::Notify { title: None, body: "Done!".into() },
                OscSignal::Notify { title: Some("Claude".into()), body: "Need input".into() },
            ]
        );
    }

    #[test]
    fn ignores_other_sequences() {
        assert!(scan(&[b"\x1b[31mred\x1b]0;title\x07\x1b]8;;http://x\x1b\\"]).is_empty());
    }
}
