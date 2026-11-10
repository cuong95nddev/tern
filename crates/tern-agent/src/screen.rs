//! What an agent is doing, read off its terminal title and the bottom lines
//! of its screen: for agents that don't report it themselves (no hooks, no
//! OSC 7501). The cues are those herdr's detection matches.

use serde::Deserialize;

/// What a screen says an agent does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenState {
    Working,
    /// Waits for you: a permission, a question.
    Blocked,
    Idle,
}

/// One cue: every field it has must match.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// The title holds this text (or each of these).
    #[serde(default, deserialize_with = "one_or_many")]
    pub title_contains: Vec<String>,
    /// The bottom lines, lowercased, hold this text (or each of these).
    #[serde(default, deserialize_with = "one_or_many")]
    pub bottom_contains: Vec<String>,
    /// The title holds one of these characters.
    pub title_any_char: Option<String>,
    /// The title starts with a character in one of these inclusive
    /// code-point ranges, then a space (a spinner before the task's name).
    pub title_lead: Option<Vec<(u32, u32)>>,
}

impl Rule {
    fn matches(&self, title: &str, bottom: &str) -> bool {
        // A rule that asks for nothing matches nothing.
        if self.title_contains.is_empty() && self.bottom_contains.is_empty() && self.title_any_char.is_none() && self.title_lead.is_none() {
            return false;
        }
        let contains = |hay: &str, needles: &[String]| needles.iter().all(|n| hay.contains(n.as_str()));
        let any_char = self.title_any_char.as_ref().is_none_or(|set| title.chars().any(|c| set.contains(c)));
        let lead = self.title_lead.as_ref().is_none_or(|ranges| {
            let mut chars = title.chars();
            let first = chars.next().map(u32::from);
            first.is_some_and(|c| ranges.iter().any(|(lo, hi)| (*lo..=*hi).contains(&c))) && chars.next() == Some(' ')
        });
        contains(title, &self.title_contains) && contains(bottom, &self.bottom_contains) && any_char && lead
    }
}

/// A string, or a list of them.
fn one_or_many<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(d)? {
        OneOrMany::One(s) => vec![s],
        OneOrMany::Many(v) => v,
    })
}

/// The cues an agent's screen shows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenRules {
    #[serde(default)]
    pub blocked: Vec<Rule>,
    /// None: its screen can't tell working from idle.
    #[serde(default)]
    pub working: Vec<Rule>,
}

impl ScreenRules {
    /// What `title` and the screen's bottom lines (`bottom`, lowercased)
    /// say; `None` when they can't tell.
    pub fn read(&self, title: &str, bottom: &str) -> Option<ScreenState> {
        if self.blocked.iter().any(|r| r.matches(title, bottom)) {
            return Some(ScreenState::Blocked);
        }
        if self.working.is_empty() {
            return None;
        }
        Some(if self.working.iter().any(|r| r.matches(title, bottom)) { ScreenState::Working } else { ScreenState::Idle })
    }
}

#[cfg(test)]
mod tests {
    use super::ScreenState::*;
    use crate::Registry;

    fn read(id: &str, title: &str, bottom: &str) -> Option<super::ScreenState> {
        Registry::builtin().get(id).unwrap().screen.read(title, bottom)
    }

    #[test]
    fn claude_from_title_and_screen() {
        assert_eq!(read("claude", "\u{25D0} Long poem", ""), Some(Working));
        assert_eq!(read("claude", "\u{2733} Long poem", ""), Some(Idle));
        assert_eq!(read("claude", "\u{2733} Long poem", "* thinking… (3s · esc to interrupt)"), Some(Working));
        // Waiting for an answer: the spinner has stopped.
        let prompt = "bash command\n  ls\ndo you want to proceed?\n❯ 1. yes\n  2. no\nesc to cancel";
        assert_eq!(read("claude", "\u{2733} Long poem", prompt), Some(Blocked));
        let question = "which one?\n❯ 1. a\n  2. b\nenter to select · ↑/↓ to navigate · esc to cancel";
        assert_eq!(read("claude", "\u{2733} x", question), Some(Blocked));
    }

    #[test]
    fn codex_from_title_and_screen() {
        assert_eq!(read("codex", "⠋ codex", ""), Some(Working));
        assert_eq!(read("codex", "codex", "• working (12s • esc to interrupt)"), Some(Working));
        assert_eq!(read("codex", "Action Required", ""), Some(Blocked));
        // The questions it asks as it starts: an update, a folder's trust.
        assert_eq!(read("codex", "codex", "› 1. update now\n  2. skip\n  enter continue · esc skip"), Some(Blocked));
        assert_eq!(read("codex", "codex", "› 1. trust and continue\n  enter continue · esc back"), Some(Blocked));
        assert_eq!(read("codex", "codex", "› "), Some(Idle));
    }

    #[test]
    fn gemini_from_its_screen() {
        assert_eq!(read("gemini", "gemini", "[y/n]"), Some(Blocked));
        assert_eq!(read("gemini", "gemini", "│ apply this change?\n│ esc to cancel"), Some(Blocked));
        assert_eq!(read("gemini", "gemini", "│ allow execution of [shell]?"), Some(Blocked));
        assert_eq!(read("gemini", "gemini", "│ do you trust the files in this folder?\n│ ● 1. trust folder (work)"), Some(Blocked));
        assert_eq!(read("gemini", "gemini", "⠏ thinking (esc to cancel, 3s)"), Some(Working));
        assert_eq!(read("gemini", "gemini", "> type your message"), Some(Idle));
    }

    #[test]
    fn cursor_from_its_screen() {
        assert_eq!(read("cursor", "cursor", "run this command?\nrun (once) (y)\nskip (esc or n)"), Some(Blocked));
        assert_eq!(read("cursor", "cursor", "write to this file?\nproceed (y)\nkeep (n)"), Some(Blocked));
        assert_eq!(read("cursor", "cursor", "⬡ generating\nctrl+c to stop"), Some(Working));
        assert_eq!(read("cursor", "cursor", "→ add a follow-up"), Some(Idle));
    }

    #[test]
    fn grok_from_title_and_screen() {
        assert_eq!(read("grok", "⚠ Action Required - grok", ""), Some(Blocked));
        assert_eq!(read("grok", "⠋ fixing - grok", "┃  2 (○) yes, proceed"), Some(Blocked));
        assert_eq!(read("grok", "grok", "1/3:select │ ctrl+o:yolo │ ctrl+c:cancel"), Some(Blocked));
        assert_eq!(read("grok", "grok", "esc:unselect │ tab:scrollback │ shift+x:dismiss"), Some(Blocked));
        assert_eq!(read("grok", "⠋ fixing - grok", ""), Some(Working));
        assert_eq!(read("grok", "grok", "⠧ waiting on subagent… 2.8s   13s ⇣29.7k [stop]"), Some(Working));
        assert_eq!(read("grok", "grok", "esc:cancel │ ctrl+.:shortcuts"), Some(Working));
        assert_eq!(read("grok", "grok", "ctrl+.:shortcuts"), Some(Idle));
    }

    #[test]
    fn pi_from_its_screen() {
        assert_eq!(read("pi", "pi", "[y/n]"), Some(Blocked));
        assert_eq!(read("pi", "pi", "working..."), Some(Working));
        assert_eq!(read("pi", "pi", "── ⠹ working ──────"), Some(Working));
        assert_eq!(read("pi", "pi", "the tests are working"), Some(Idle));
    }

    #[test]
    fn a_spinner_lead_needs_a_space_after_it() {
        assert_eq!(read("claude", "\u{25D0}x", ""), Some(Idle));
    }
}
