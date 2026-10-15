//! Coding-agent CLIs a terminal may be running, told apart by their
//! foreground programs, and their logos (from t3code, MIT: see
//! `assets/agents/LICENSE-t3code`).

use gpui_kit::component::Theme;
use gpui_kit::*;

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
