//! Which hook events a Claude Code version knows (Orca's
//! `claude-hook-event-versions.ts`).
//!
//! Claude Code 1.0.23 through 2.1.100 check `hooks` against a closed list of
//! events and throw away the whole settings file (every hook, the status
//! line) over one name they don't know, so a tab's settings name only the
//! events its Claude knows.

pub use crate::version::{Version, parse};

/// The first version that knows each hook event.
const FIRST_VERSIONS: &[(&str, Version)] = &[
    ("PreToolUse", (1, 0, 23)),
    ("PostToolUse", (1, 0, 23)),
    ("Stop", (1, 0, 31)),
    ("SubagentStop", (1, 0, 41)),
    ("UserPromptSubmit", (1, 0, 53)),
    ("SessionStart", (1, 0, 62)),
    ("SessionEnd", (1, 0, 85)),
    ("SubagentStart", (2, 0, 43)),
    ("PermissionRequest", (2, 0, 45)),
    ("PostToolUseFailure", (2, 0, 56)),
    ("TeammateIdle", (2, 1, 33)),
    ("PostCompact", (2, 1, 76)),
    ("StopFailure", (2, 1, 78)),
];

/// `statusLine` joined the settings schema in 1.0.64 (1.0.49 to 1.0.66 also
/// throw the file away over an unknown top-level key).
const STATUS_LINE: Version = (1, 0, 64);

/// What a Claude whose version isn't known gets: what the first release that
/// knows the status line and the core lifecycle events knows, nothing newer.
const UNRESOLVED: Version = STATUS_LINE;

/// `SessionEnd` is known from 1.0.85, but its delivery was measured only on
/// 2.1.261 (Orca writes it from there).
const SESSION_END_FROM: Version = (2, 1, 261);

/// Whether a Claude of `version` (`None`: not known) knows hook `event`.
pub fn knows_event(version: Option<Version>, event: &str) -> bool {
    let Some(&(_, first)) = FIRST_VERSIONS.iter().find(|(e, _)| *e == event) else { return false };
    if event == "SessionEnd" {
        // Written only for a version known to deliver it.
        return version.is_some_and(|v| v >= SESSION_END_FROM);
    }
    version.unwrap_or(UNRESOLVED) >= first
}

/// Whether a Claude of `version` knows the `statusLine` setting.
pub fn knows_status_line(version: Option<Version>) -> bool {
    version.unwrap_or(UNRESOLVED) >= STATUS_LINE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_version_line() {
        assert_eq!(parse("2.1.312 (Claude Code)"), Some((2, 1, 312)));
        assert_eq!(parse("claude 1.0.64\n"), Some((1, 0, 64)));
        assert_eq!(parse("2.1.3-beta.1"), Some((2, 1, 3)));
        assert_eq!(parse("command not found: claude"), None);
        assert_eq!(parse("1.2"), None);
    }

    #[test]
    fn gates_events_by_version() {
        assert!(knows_event(Some((2, 1, 312)), "StopFailure"));
        assert!(!knows_event(Some((2, 1, 77)), "StopFailure"));
        assert!(knows_event(Some((2, 0, 45)), "PermissionRequest"));
        assert!(!knows_event(Some((2, 0, 44)), "PermissionRequest"));
        assert!(!knows_event(Some((2, 1, 312)), "NoSuchEvent"));
        // Unknown: what 1.0.64 knows.
        assert!(knows_event(None, "SessionStart"));
        assert!(knows_event(None, "Stop"));
        assert!(!knows_event(None, "SubagentStart"));
        assert!(!knows_event(None, "SessionEnd"));
        assert!(knows_event(Some((2, 1, 261)), "SessionEnd"));
        assert!(!knows_event(Some((2, 1, 260)), "SessionEnd"));
        assert!(knows_status_line(None));
        assert!(!knows_status_line(Some((1, 0, 63))));
    }
}
