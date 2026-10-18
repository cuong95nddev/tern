//! Coding-agent CLIs a terminal may be running, told apart by their
//! foreground programs, and their logos (from t3code, MIT: see
//! `assets/agents/LICENSE-t3code`).

use gpui_kit::component::Theme;
use gpui_kit::*;
use tern_term::status::ActivityStatus;

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

/// The logo at `path`, for the app's asset source.
pub fn asset(path: &str) -> Option<&'static [u8]> {
    LOGOS.iter().find(|(p, _)| *p == path).map(|(_, b)| *b)
}

pub fn asset_paths(prefix: &str) -> impl Iterator<Item = SharedString> + '_ {
    LOGOS.iter().map(|(p, _)| *p).filter(move |p| p.starts_with(prefix)).map(SharedString::from)
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
    fn others_tell_only_prompts() {
        assert_eq!(Agent::Gemini.read_screen("gemini", "allow? [y/n]"), Some(Blocked));
        assert_eq!(Agent::Gemini.read_screen("gemini", "> "), Option::None);
    }
}
