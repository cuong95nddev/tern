//! Coding-agent CLIs a terminal may be running, told apart by their
//! foreground programs, and their logos (from t3code, MIT: see
//! `assets/agents/LICENSE-t3code`).

use gpui_kit::component::Theme;
use gpui_kit::*;
use tern_term::status::ActivityStatus;

/// What an agent's row says of its state, as T3 Code's sidebar does: its
/// label, icon (empty for none) and color. `since` is how long it has been
/// in `status`; `rang` is a bell rung while not shown.
pub fn status_look(status: ActivityStatus, rang: bool, since: std::time::Duration, theme: &Theme) -> (String, &'static str, Hsla) {
    match (status, rang) {
        (ActivityStatus::Working, _) => (format!("Working {}", working_label(since)), "circle-dashed", theme.info),
        (ActivityStatus::Blocked, _) => ("Needs input".to_owned(), "message-circle-question-mark", theme.warning),
        (ActivityStatus::Done, _) => ("Done".to_owned(), "circle-check", theme.success),
        (ActivityStatus::None, true) => ("Bell".to_owned(), "bell", theme.warning),
        (ActivityStatus::None, false) => (ago_label(since), "", theme.muted_foreground),
    }
}

/// How full a Claude conversation's context window is: what its status
/// line's input says (see `tern_bridge::statusline`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContextUse {
    /// Percent of the window used, 0 to 100.
    pub percent: f32,
    /// The window's size, in tokens.
    pub size: u64,
}

impl ContextUse {
    /// From a status line's input. Before the first answer the window is
    /// empty: Claude Code then gives no percentage.
    pub fn read(line: &serde_json::Value) -> Option<Self> {
        let w = line.get("context_window")?;
        let size = w.get("context_window_size")?.as_u64().filter(|s| *s > 0)?;
        let percent = w.get("used_percentage").and_then(serde_json::Value::as_f64).unwrap_or(0.);
        Some(Self { percent: percent.clamp(0., 100.) as f32, size })
    }

    /// From this percent on, the ring warns that the window is filling up.
    const HIGH: f32 = 80.;
}

/// The size of a context window, short: `200k`, `1M`.
pub fn tokens_label(n: u64) -> String {
    match n {
        n if n >= 1_000_000 && n % 1_000_000 == 0 => format!("{}M", n / 1_000_000),
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n => format!("{}k", n / 1000),
    }
}

/// An agent row's ring (template `agents.tab`), as Harp's sidebar draws its
/// states: how full its context window is (`ring`, empty when unknown),
/// turning a step each second while it works (`spin`), in amber with a dot
/// while it waits for you, with a tick once its turn is done; `time`, how
/// long it has been working or since its turn ended; `tip`, all of it in
/// words.
pub fn ring_vars(
    scope: crate::template::Scope,
    status: ActivityStatus,
    rang: bool,
    since: std::time::Duration,
    context: Option<ContextUse>,
    theme: &Theme,
) -> crate::template::Scope {
    let (word, _, _) = status_look(status, rang, since, theme);
    let (color, glyph) = match (status, rang) {
        (ActivityStatus::Working, _) => (theme.foreground, None),
        (ActivityStatus::Blocked, _) => (theme.warning, Some(("agent-dot", theme.warning))),
        (ActivityStatus::Done, _) => (theme.foreground, Some(("agent-tick", theme.foreground))),
        (ActivityStatus::None, true) => (theme.muted_foreground, Some(("agent-dot", theme.warning))),
        (ActivityStatus::None, false) => (theme.muted_foreground, None),
    };
    let high = context.is_some_and(|c| c.percent >= ContextUse::HIGH);
    let time = match status {
        ActivityStatus::Working => working_label(since),
        _ => ago_label(since),
    };
    let tip = match context {
        Some(c) => format!("{word} · context {:.0}% of {}", c.percent, tokens_label(c.size)),
        None => word,
    };
    let spin = match status {
        ActivityStatus::Working => (since.as_secs() % 12) as f64 / 12.,
        _ => 0.,
    };
    let mut scope = scope
        .var("ring", context.map(|c| format!("{:.1}", c.percent)).unwrap_or_default())
        .var("ring_color", if high { theme.danger } else { color })
        .var("spin", spin)
        .var("time", time)
        .var("tip", tip)
        .var("working", status == ActivityStatus::Working);
    if let Some((icon, color)) = glyph {
        scope = scope.var("ring_icon", icon).var("ring_icon_color", color);
    }
    scope
}

/// The glyphs drawn in a ring (16×16, in the text color).
const GLYPHS: &[(&str, &str)] = &[
    (
        "icons/agent-tick.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="black" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><path d="M5.5 8.2l1.7 1.7 3.3-3.6"/></svg>"#,
    ),
    ("icons/agent-dot.svg", r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><circle cx="8" cy="8" r="2.8" fill="black"/></svg>"#),
];

/// A state's color: blue while working, amber while waiting for you,
/// green once done (as t3code's sidebar has them).
pub fn status_color(status: ActivityStatus, theme: &Theme) -> Hsla {
    match status {
        ActivityStatus::Working => theme.info,
        ActivityStatus::Blocked => theme.warning,
        ActivityStatus::Done => theme.success,
        ActivityStatus::None => theme.muted_foreground,
    }
}

/// Which state a workspace shows for its agents: the one that most wants you.
pub fn priority(status: ActivityStatus) -> u8 {
    match status {
        ActivityStatus::Blocked => 3,
        ActivityStatus::Working => 2,
        ActivityStatus::Done => 1,
        ActivityStatus::None => 0,
    }
}

/// What a row calls an agent: its tab's title without the status glyph
/// some agents put first (Claude Code's "✳"), else the agent's name.
pub fn display_name(title: &str, agent: Agent) -> String {
    let bare = title.trim_start_matches(|c: char| !c.is_alphanumeric()).trim();
    if bare.is_empty() { agent.name().to_owned() } else { bare.to_owned() }
}

/// How long a turn has run, as T3 Code's sidebar says it: `12s`, `3m`, `1h 5m`.
pub fn working_label(d: std::time::Duration) -> String {
    let s = d.as_secs();
    match s / 60 {
        0 => format!("{s}s"),
        m if m < 60 => format!("{m}m"),
        m => format!("{}h {}m", m / 60, m % 60),
    }
}

/// How long ago something happened, compact: `now`, `5m`, `3h`, `2d`.
pub fn ago_label(d: std::time::Duration) -> String {
    let m = d.as_secs() / 60;
    match m {
        0 => "now".to_owned(),
        m if m < 60 => format!("{m}m"),
        m if m < 24 * 60 => format!("{}h", m / 60),
        m => format!("{}d", m / (24 * 60)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
    Gemini,
    OpenCode,
    Cursor,
    Grok,
    Pi,
}

macro_rules! logos {
    ($($name:literal),* $(,)?) => {
        &[$((concat!("agents/", $name, ".svg"), include_bytes!(concat!("../assets/agents/", $name, ".svg")))),*]
    };
}

const LOGOS: &[(&str, &[u8])] = logos![
    "claude",
    "gemini",
    "openai-light",
    "openai-dark",
    "opencode-light",
    "opencode-dark",
    "cursor-light",
    "cursor-dark",
    "grok-light",
    "grok-dark",
    "pi-light",
    "pi-dark",
];

impl Agent {
    /// The agent among a terminal's foreground programs (see
    /// `tern_term::procs::foreground_programs`).
    pub fn detect(programs: &[String]) -> Option<Self> {
        programs.iter().find_map(|p| match p.as_str() {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "gemini" => Some(Self::Gemini),
            "opencode" => Some(Self::OpenCode),
            "cursor-agent" => Some(Self::Cursor),
            "grok" => Some(Self::Grok),
            "pi" => Some(Self::Pi),
            _ => None,
        })
    }

    /// The command that starts it in a shell.
    pub fn command(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::OpenCode => "opencode",
            Self::Cursor => "cursor-agent",
            Self::Grok => "grok",
            Self::Pi => "pi",
        }
    }

    /// The product name, for notification titles.
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Gemini => "Gemini CLI",
            Self::OpenCode => "OpenCode",
            Self::Cursor => "Cursor Agent",
            Self::Grok => "Grok",
            Self::Pi => "Pi",
        }
    }

    /// What the agent is doing, read off its terminal title and the bottom
    /// lines of its screen (lowercased): for when it doesn't report it with
    /// OSC 7501, as Claude Code doesn't on Windows (ConPTY answers its probe's
    /// closing DA1 before the terminal can). `None` when its screen can't
    /// tell. The cues are those herdr's detection matches.
    pub fn read_screen(self, title: &str, bottom: &str) -> Option<ActivityStatus> {
        let has = |s: &str| bottom.contains(s);
        let blocked = match self {
            // Permission prompts, questions and other forms.
            Self::Claude => {
                (has("esc to cancel") && (has("enter to confirm") || has("enter to select")))
                    || has("do you want to proceed?")
                    || has("waiting for permission")
                    || has("do you want to allow this connection?")
            },
            Self::Codex => {
                title.contains("Action Required")
                    || has("press enter to confirm or esc to cancel")
                    || has("allow command?")
                    || has("enter to submit answer")
                    || has("[y/n]")
            },
            _ => has("[y/n]") || has("do you want to proceed?") || has("waiting for user confirmation"),
        };
        if blocked {
            return Some(ActivityStatus::Blocked);
        }
        let working = match self {
            // A spinner leads the title while busy ("✳" when not), and the
            // footer offers to interrupt.
            Self::Claude => {
                let mut chars = title.chars();
                let spinner = chars.next().is_some_and(|c| matches!(c, '\u{25D0}'..='\u{25D3}' | '\u{2800}'..='\u{28FF}'));
                (spinner && chars.next() == Some(' ')) || has("esc to interrupt")
            },
            Self::Codex => title.chars().any(|c| "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".contains(c)) || has("to interrupt)"),
            _ => return None,
        };
        Some(if working { ActivityStatus::Working } else { ActivityStatus::None })
    }

    /// The logo's asset path. Black-and-white logos come in a variant per
    /// theme; the others keep their brand colors in both.
    fn logo(self, dark: bool) -> &'static str {
        let (light_path, dark_path) = match self {
            Self::Claude => return "agents/claude.svg",
            Self::Gemini => return "agents/gemini.svg",
            Self::Codex => ("agents/openai-light.svg", "agents/openai-dark.svg"),
            Self::OpenCode => ("agents/opencode-light.svg", "agents/opencode-dark.svg"),
            Self::Cursor => ("agents/cursor-light.svg", "agents/cursor-dark.svg"),
            Self::Grok => ("agents/grok-light.svg", "agents/grok-dark.svg"),
            Self::Pi => ("agents/pi-light.svg", "agents/pi-dark.svg"),
        };
        if dark { dark_path } else { light_path }
    }

    /// The logo in full color, icon-sized. An image rather than an `Icon`,
    /// which tints every path one color.
    pub fn icon(self, theme: &Theme) -> Img {
        img(self.logo(theme.is_dark())).size_3p5().flex_shrink_0()
    }
}

/// The logo or ring glyph at `path`, for the app's asset source.
pub fn asset(path: &str) -> Option<&'static [u8]> {
    LOGOS.iter().find(|(p, _)| *p == path).map(|(_, b)| *b).or_else(|| GLYPHS.iter().find(|(p, _)| *p == path).map(|(_, s)| s.as_bytes()))
}

pub fn asset_paths(prefix: &str) -> impl Iterator<Item = SharedString> + '_ {
    let glyphs = GLYPHS.iter().map(|(p, _)| *p);
    LOGOS.iter().map(|(p, _)| *p).chain(glyphs).filter(move |p| p.starts_with(prefix)).map(SharedString::from)
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's `test` attribute would shadow the std one.
    use super::Agent;
    use tern_term::status::ActivityStatus::*;

    #[test]
    fn claude_from_title_and_screen() {
        let c = Agent::Claude;
        assert_eq!(c.read_screen("\u{25D0} Long poem", ""), Some(Working));
        assert_eq!(c.read_screen("\u{2733} Long poem", ""), Some(None));
        assert_eq!(c.read_screen("\u{2733} Long poem", "* thinking… (3s · esc to interrupt)"), Some(Working));
        // Waiting for an answer: the spinner has stopped.
        let prompt = "bash command\n  ls\ndo you want to proceed?\n❯ 1. yes\n  2. no\nesc to cancel";
        assert_eq!(c.read_screen("\u{2733} Long poem", prompt), Some(Blocked));
        let question = "which one?\n❯ 1. a\n  2. b\nenter to select · ↑/↓ to navigate · esc to cancel";
        assert_eq!(c.read_screen("\u{2733} x", question), Some(Blocked));
    }

    #[test]
    fn codex_from_title_and_screen() {
        let c = Agent::Codex;
        assert_eq!(c.read_screen("⠋ codex", ""), Some(Working));
        assert_eq!(c.read_screen("codex", "• working (12s • esc to interrupt)"), Some(Working));
        assert_eq!(c.read_screen("Action Required", ""), Some(Blocked));
        assert_eq!(c.read_screen("codex", "› "), Some(None));
    }

    #[test]
    fn context_from_the_status_line() {
        use super::ContextUse;
        let line = serde_json::json!({"context_window": {"context_window_size": 1000000, "used_percentage": 42.5}});
        assert_eq!(ContextUse::read(&line), Some(ContextUse { percent: 42.5, size: 1_000_000 }));
        let fresh = serde_json::json!({"context_window": {"context_window_size": 200000, "used_percentage": null}});
        assert_eq!(ContextUse::read(&fresh), Some(ContextUse { percent: 0., size: 200_000 }));
        assert_eq!(ContextUse::read(&serde_json::json!({"model": {}})), Option::None);
        assert_eq!((super::tokens_label(200_000), super::tokens_label(1_000_000)), ("200k".to_owned(), "1M".to_owned()));
    }

    #[test]
    fn others_tell_only_prompts() {
        assert_eq!(Agent::Gemini.read_screen("gemini", "allow? [y/n]"), Some(Blocked));
        assert_eq!(Agent::Gemini.read_screen("gemini", "> "), Option::None);
    }
}
