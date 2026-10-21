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
}

/// How a Claude tab keeps its conversation's context window in bounds (see
/// `tern_bridge::context`): when `on`, past `limit` percent Claude is asked
/// for a handoff, and with `auto_switch` the tab then goes on in a fresh
/// conversation. Set from the usage bar's context ring.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContextGuard {
    pub on: bool,
    pub limit: u8,
    pub auto_switch: bool,
}

impl Default for ContextGuard {
    fn default() -> Self {
        Self { on: false, limit: 50, auto_switch: true }
    }
}

impl ContextGuard {
    /// What the limit can be set to.
    pub const MIN: u8 = 5;
    pub const MAX: u8 = 100;

    /// From a saved tab's (limit, auto switch), on when there is one.
    pub fn from_saved(saved: Option<(u8, bool)>) -> Self {
        saved.map_or_else(Self::default, |(limit, auto_switch)| Self { on: true, limit: limit.clamp(Self::MIN, Self::MAX), auto_switch })
    }

    /// What a saved tab keeps of it.
    pub fn saved(&self) -> Option<(u8, bool)> {
        self.on.then_some((self.limit, self.auto_switch))
    }

    /// The limit the daemon is told: 0 when off.
    pub fn sent_limit(&self) -> u8 {
        if self.on { self.limit } else { 0 }
    }

    /// The limit typed as `text`, when it is one it can be.
    pub fn typed(text: &str) -> Option<u8> {
        text.trim().parse().ok().filter(|l| (Self::MIN..=Self::MAX).contains(l))
    }

    /// The limit nearest to what was typed as `text` (none for no number).
    pub fn nearest(text: &str) -> Option<u8> {
        text.trim().parse::<u64>().ok().map(|l| l.clamp(Self::MIN.into(), Self::MAX.into()) as u8)
    }
}

/// The size of a context window, short: `200k`, `1M`.
pub fn tokens_label(n: u64) -> String {
    match n {
        n if n >= 1_000_000 && n % 1_000_000 == 0 => format!("{}M", n / 1_000_000),
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n => format!("{}k", n / 1000),
    }
}

/// An agent row's state icon (template `agents.tab`), as Harp's sidebar
/// draws it: an arc turning on its track while it works (`working`), an amber
/// ring around a dot while it waits for you (`needs`), a filled disc with the
/// tick cut out and the title in bold once its turn is done (`done`), a quiet
/// tick once its tab has been seen (`seen`), an empty ring before its first
/// turn (`pending`), an amber dot after a bell rang in it unseen (`rang`); a
/// dot on its corner once its turn is done while it left shells or
/// sub-agents (of `children`) running in the background (`shell_dot`).
/// `time`, how long it has been working or since its turn ended; `tip`, all
/// of it in words, with how full its context window is and the finished
/// task that woke it.
pub fn ring_vars(
    scope: crate::template::Scope,
    view: &crate::terminal_view::TerminalView,
    rang: bool,
    context: Option<ContextUse>,
    children: Option<&crate::subagents::Children>,
    theme: &Theme,
) -> crate::template::Scope {
    let (status, shells) = (view.status(), view.shells().len());
    let agents = children.map_or(0, crate::subagents::Children::background);
    let seen = view.seen_done() && status == ActivityStatus::None;
    let since = view.status_since().elapsed();
    let (word, _, _) = status_look(status, rang, since, theme);
    let word = if seen && !rang { "Done".to_owned() } else { word };
    let time = match status {
        ActivityStatus::Working => working_label(since),
        _ => ago_label(since),
    };
    // Only a finished turn: while it works its shells are its work.
    let shell_dot = shells + agents > 0 && (status == ActivityStatus::Done || seen);
    let mut tip = match context {
        Some(c) => format!("{word} · context {:.0}% of {}", c.percent, tokens_label(c.size)),
        None => word,
    };
    if shell_dot {
        tip += &format!(" · {}", background_label(shells, agents));
    }
    if status == ActivityStatus::Working
        && let Some(woke) = children.and_then(|c| c.woke.as_deref())
    {
        tip += &format!(" · woken by: {woke}");
    }
    let idle = status == ActivityStatus::None;
    scope
        .var("time", time)
        .var("tip", tip)
        .var("working", status == ActivityStatus::Working)
        .var("needs", status == ActivityStatus::Blocked)
        .var("done", status == ActivityStatus::Done)
        .var("rang", idle && rang)
        .var("seen", idle && !rang && seen)
        .var("pending", idle && !rang && !seen)
        .var("shell_dot", shell_dot)
}

/// What runs in the background, in words: "1 background shell, 2 background
/// agents".
pub fn background_label(shells: usize, agents: usize) -> String {
    let count = |n: usize, what: &str| (n > 0).then(|| format!("{n} background {what}{}", if n == 1 { "" } else { "s" }));
    [count(shells, "shell"), count(agents, "agent")].into_iter().flatten().collect::<Vec<_>>().join(", ")
}

/// What a workspace's line says of its agents that wait for you, in place
/// of its dot: "1 needs you".
pub fn needs_label(n: usize) -> String {
    format!("{n} need{} you", if n == 1 { "s" } else { "" })
}

/// The glyphs of an agent's state (Harp's sidebar icons, 16×16, drawn in
/// the text color).
const GLYPHS: &[(&str, &str)] = &[
    // A disc with the tick cut out of it; `-cut` also cuts room for the
    // background-shell dot on its corner.
    (
        "icons/agent-done.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><mask id="m"><rect width="16" height="16" fill="white"/><path d="M5.1 8.2l2 2 3.8-4.1" fill="none" stroke="black" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/></mask><circle cx="8" cy="8" r="7.2" fill="black" mask="url(#m)"/></svg>"#,
    ),
    (
        "icons/agent-done-cut.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><mask id="m"><rect width="16" height="16" fill="white"/><path d="M5.1 8.2l2 2 3.8-4.1" fill="none" stroke="black" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/><circle cx="15.5" cy="0.5" r="4" fill="black"/></mask><circle cx="8" cy="8" r="7.2" fill="black" mask="url(#m)"/></svg>"#,
    ),
    (
        "icons/agent-tick.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none"><path d="M4.2 8.4l2.6 2.6 5-5.4" stroke="black" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/></svg>"#,
    ),
    (
        "icons/agent-ring.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none"><circle cx="8" cy="8" r="6.6" stroke="black" stroke-width="1.3"/></svg>"#,
    ),
    (
        "icons/agent-needs.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none"><circle cx="8" cy="8" r="6.2" stroke="black" stroke-width="1.8"/><circle cx="8" cy="8" r="3.2" fill="black"/></svg>"#,
    ),
    (
        "icons/agent-dot.svg",
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><circle cx="8" cy="8" r="3.2" fill="black"/></svg>"#,
    ),
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
    fn context_guard_reads_and_saves() {
        use super::ContextGuard;
        let g = ContextGuard::default();
        assert_eq!((g.sent_limit(), g.saved()), (0, Option::None));
        let on = ContextGuard { on: true, ..g };
        assert_eq!((on.sent_limit(), on.saved()), (50, Some((50, true))));
        assert_eq!(ContextGuard::from_saved(on.saved()), on);
        assert_eq!(ContextGuard::from_saved(Option::None), g);
        assert_eq!(
            (ContextGuard::typed(" 60 "), ContextGuard::typed("3"), ContextGuard::typed("101")),
            (Some(60), Option::None, Option::None)
        );
        assert_eq!(
            (ContextGuard::nearest("3"), ContextGuard::nearest("250"), ContextGuard::nearest("")),
            (Some(5), Some(100), Option::None)
        );
    }

    #[test]
    fn others_tell_only_prompts() {
        assert_eq!(Agent::Gemini.read_screen("gemini", "allow? [y/n]"), Some(Blocked));
        assert_eq!(Agent::Gemini.read_screen("gemini", "> "), Option::None);
    }
}
