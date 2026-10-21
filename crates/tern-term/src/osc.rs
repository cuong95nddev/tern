//! Streaming scanner that taps the raw PTY byte stream for OSC sequences that
//! `alacritty_terminal` ignores but Claude Code relies on:
//!
//! - OSC 7501 — "program status" (`state=working:app=claude-code:...`), plus the
//!   `OSC 7501 ; ?` capability probe which we must answer.
//! - OSC 9 / OSC 777 — desktop notifications.
//! - `CSI ? 2031 h` / `l` and `CSI ? 996 n` — the color scheme (light or
//!   dark) reports Claude Code's `auto` theme asks for.
//! - OSC 7 — the shell's working directory.
//! - `CSI > q` (XTVERSION) — the program asks which terminal this is.
//!
//! The scanner never modifies the stream; bytes still flow to the VT parser.

use std::path::PathBuf;

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
    /// `CSI ? 2031 h` / `l`: report color scheme changes (`CSI ? 997 ; n n`) or stop.
    ColorSchemeReports(bool),
    /// `CSI ? 996 n`: the program asks for the color scheme now.
    ColorSchemeQuery,
    /// `OSC 7 ; file://host/path`: the shell's working directory, when the
    /// host is this machine.
    Cwd(PathBuf),
    /// `CSI > q` / `CSI > 0 q`: XTVERSION.
    VersionQuery,
}

/// The XTVERSION reply: `DCS > | name ST`.
pub fn version_report(version: &str) -> Vec<u8> {
    format!("\x1bP>|Tern {version}\x1b\\").into_bytes()
}

/// The color scheme report: `CSI ? 997 ; 1 n` for dark, `; 2 n` for light.
pub fn color_scheme_report(dark: bool) -> &'static [u8] {
    if dark { b"\x1b[?997;1n" } else { b"\x1b[?997;2n" }
}

/// Max parameter bytes we keep for a CSI; longer ones aren't ours.
const MAX_CSI_LEN: usize = 32;

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
    /// Collecting a CSI's parameters.
    Csi,
}

#[derive(Debug)]
pub struct OscScanner {
    state: State,
    buf: Vec<u8>,
    overflow: bool,
    csi: Vec<u8>,
}

impl Default for OscScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl OscScanner {
    pub fn new() -> Self {
        Self { state: State::Ground, buf: Vec::with_capacity(256), overflow: false, csi: Vec::with_capacity(MAX_CSI_LEN) }
    }

    /// Feed bytes, invoking `emit` for every recognised signal.
    pub fn feed(&mut self, bytes: &[u8], mut emit: impl FnMut(OscSignal)) {
        let mut i = 0;
        while i < bytes.len() {
            match self.state {
                State::Ground => {
                    // Fast path: skip to the next ESC.
                    match memchr::memchr(0x1b, &bytes[i..]) {
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
                    } else if b == b'[' {
                        self.csi.clear();
                        State::Csi
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
                State::Csi => {
                    let b = bytes[i];
                    i += 1;
                    match b {
                        // Parameters and intermediates.
                        0x20..=0x3f => {
                            if self.csi.len() < MAX_CSI_LEN {
                                self.csi.push(b);
                            } else {
                                self.state = State::Ground;
                            }
                        },
                        0x40..=0x7e => {
                            self.state = State::Ground;
                            if let Some(sig) = parse_csi(&self.csi, b) {
                                emit(sig);
                            }
                        },
                        0x1b => self.state = State::Esc,
                        _ => self.state = State::Ground,
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

/// The CSIs we act on, by parameters and final byte.
fn parse_csi(params: &[u8], fin: u8) -> Option<OscSignal> {
    if fin == b'q' && matches!(params, b">" | b">0") {
        return Some(OscSignal::VersionQuery);
    }
    let modes = params.strip_prefix(b"?")?;
    let has = |n: &[u8]| modes.split(|&b| b == b';').any(|m| m == n);
    match fin {
        b'h' if has(b"2031") => Some(OscSignal::ColorSchemeReports(true)),
        b'l' if has(b"2031") => Some(OscSignal::ColorSchemeReports(false)),
        b'n' if modes == b"996" => Some(OscSignal::ColorSchemeQuery),
        _ => None,
    }
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
        // OSC 9 ; body  (iTerm2 growl). OSC 9 ; <n> ; ... are ConEmu
        // commands (4 = progress, 9 = cwd, ...) — ignore.
        "9" => {
            let head = rest.split(';').next().unwrap_or_default();
            let conemu = !head.is_empty() && head.len() <= 2 && head.bytes().all(|b| b.is_ascii_digit());
            if rest.is_empty() || conemu {
                return None;
            }
            Some(OscSignal::Notify { title: None, body: rest.to_owned() })
        },
        "7" => parse_cwd(rest).map(OscSignal::Cwd),
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

/// `file://host/path` (percent-encoded), when `host` is this machine.
fn parse_cwd(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let (host, path) = rest.split_at(rest.find('/')?);
    if !is_local_host(host) {
        return None;
    }
    let path = String::from_utf8(percent_decode(path.as_bytes())?).ok()?;
    // `/C:/Users/me` on Windows.
    #[cfg(windows)]
    let path = match path.as_bytes() {
        [b'/', d, b':', ..] if d.is_ascii_alphabetic() => path[1..].to_owned(),
        _ => path,
    };
    Some(PathBuf::from(path))
}

fn percent_decode(s: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' {
            let hex = std::str::from_utf8(s.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    Some(out)
}

fn is_local_host(host: &str) -> bool {
    if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let Some(name) = host_name() else { return false };
    // `mac.local` and `mac` name the same machine.
    let short = |h: &str| h.split('.').next().unwrap_or(h).to_ascii_lowercase();
    short(host) == short(&name)
}

#[cfg(unix)]
fn host_name() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for its length.
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0)?;
    String::from_utf8(buf[..end].to_vec()).ok()
}

#[cfg(windows)]
fn host_name() -> Option<String> {
    std::env::var("COMPUTERNAME").ok()
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
    fn color_scheme_modes_and_query() {
        let out = scan(&[b"\x1b[?1004;20", b"31h\x1b[31mx\x1b[?996n\x1b[?2031l\x1b[?99", b"6n"]);
        assert_eq!(
            out,
            [
                OscSignal::ColorSchemeReports(true),
                OscSignal::ColorSchemeQuery,
                OscSignal::ColorSchemeReports(false),
                OscSignal::ColorSchemeQuery
            ]
        );
        // Not ours: other modes, a plain CSI 2031 h, an OSC still read after a CSI.
        let out = scan(&[b"\x1b[?20310h\x1b[2031h\x1b[1;2H\x1b]7501;?\x07"]);
        assert_eq!(out, [OscSignal::ProgramStatusProbe]);
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
        let out = scan(&[b"\x1b]9;Done!\x07\x1b]777;notify;Claude;Need input\x07\x1b]9;4;1;50\x07\x1b]9;9;/tmp\x07\x1b]9;1\x07"]);
        assert_eq!(
            out,
            vec![
                OscSignal::Notify { title: None, body: "Done!".into() },
                OscSignal::Notify { title: Some("Claude".into()), body: "Need input".into() },
            ]
        );
    }

    #[test]
    fn working_directory() {
        let out = scan(&[b"\x1b]7;file:///Users/me/my%20dir\x07\x1b]7;file://localhost/tmp\x1b\\"]);
        assert_eq!(out, [OscSignal::Cwd("/Users/me/my dir".into()), OscSignal::Cwd("/tmp".into())]);
        let here = format!("\x1b]7;file://{}/srv\x07", host_name().unwrap());
        assert_eq!(scan(&[here.as_bytes()]), [OscSignal::Cwd("/srv".into())]);
        // Another machine's folder, a bad escape, not a file URI.
        assert!(scan(&[b"\x1b]7;file://elsewhere.example/x\x07\x1b]7;file:///a%zz\x07\x1b]7;/plain\x07"]).is_empty());
    }

    #[test]
    fn version_query() {
        assert_eq!(scan(&[b"\x1b[>q\x1b[>0q\x1b[>1q\x1b[q"]), [OscSignal::VersionQuery, OscSignal::VersionQuery]);
        assert_eq!(version_report("1.2"), b"\x1bP>|Tern 1.2\x1b\\");
    }

    #[test]
    fn ignores_other_sequences() {
        assert!(scan(&[b"\x1b[31mred\x1b]0;title\x07\x1b]8;;http://x\x1b\\"]).is_empty());
    }
}
