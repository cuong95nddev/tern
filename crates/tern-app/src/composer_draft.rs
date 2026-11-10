//! Whether an agent's prompt holds a draft the user typed and hasn't sent:
//! text typed into that terminal would join it and be sent with it. Ported
//! from Orca's `detectTerminalComposerDraft`
//! (`src/shared/terminal/input/terminal-composer-draft.ts`), which holds its
//! mailbox pointer back while one shows: the cursor's row, or a row above it,
//! starts with the prompt glyph (`❯` Claude, `›` Codex, `»`), and the text
//! after it, with dim placeholder or suggestion cells left out and rows below
//! the cursor up to the prompt's bottom frame joined on, is not a stock
//! placeholder.

/// A screen row: all its text, and its text with dim cells blanked (what
/// was typed, not a placeholder).
pub struct Row {
    pub text: String,
    pub typed: String,
}

const GLYPHS: [char; 3] = ['\u{276F}', '\u{203A}', '\u{00BB}'];

/// Codex's placeholders before 0.152 (Orca's `STOCK_PLACEHOLDERS`), shown
/// undimmed.
const STOCK_PLACEHOLDERS: [&str; 13] = [
    "Ask Codex to do anything",
    "Ask a follow-up question",
    "Explain this codebase",
    "Summarize recent commits",
    "Implement {feature}",
    "Find and fix a bug in @filename",
    "Write tests for @filename",
    "Improve documentation in @filename",
    "Run /review on my current changes",
    "Use /skills to list available skills",
    "Check recently modified functions for compatibility",
    "How many files have been modified?",
    "Will this algorithm scale well?",
];

/// A row of the prompt's frame (`────────`).
fn frame(row: &str) -> bool {
    let t = row.trim_end();
    t.chars().count() >= 8 && t.chars().all(|c| matches!(c, '\u{2500}' | '\u{2501}' | '-'))
}

/// `row` without its leading prompt glyph (and the space after it), when it
/// has one.
fn after_glyph(row: &str) -> Option<&str> {
    let t = row.trim_start();
    let c = t.chars().next().filter(|c| GLYPHS.contains(c))?;
    let rest = &t[c.len_utf8()..];
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

fn placeholder(text: &str) -> bool {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    text.starts_with("Try \"") || text.starts_with("Try \u{201C}") || STOCK_PLACEHOLDERS.contains(&text.as_str())
}

/// The draft in the prompt: `upto` holds the rows above the cursor's, then
/// the cursor's last; `below` the rows under it.
pub fn draft(upto: &[Row], below: &[Row]) -> Option<String> {
    let cursor = upto.len().checked_sub(1)?;
    for i in (0..=cursor).rev() {
        let row = &upto[i];
        if after_glyph(&row.text).is_some() {
            // Claude's prompt sits in a frame: a `❯` row without one above
            // is some other program's.
            if row.text.trim_start().starts_with('\u{276F}') && !upto.get(i.wrapping_sub(1)).is_some_and(|r| frame(&r.text)) {
                return None;
            }
            let mut lines = vec![after_glyph(&row.typed).unwrap_or(row.typed.trim_start()).to_owned()];
            lines.extend(upto[i + 1..].iter().map(|r| r.typed.clone()));
            for r in below {
                if frame(&r.text) || r.typed.trim().is_empty() {
                    break;
                }
                lines.push(r.typed.clone());
            }
            let text = lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
            return (!text.is_empty() && !placeholder(&text)).then_some(text);
        }
        // A row that starts at the left edge and isn't the prompt's ends it.
        if !row.text.is_empty() && !row.text.starts_with(' ') && i != cursor {
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{Row, draft};

    fn row(text: &str) -> Row {
        Row { text: text.into(), typed: text.into() }
    }

    /// A row whose `dim` part is a placeholder.
    fn dim(text: &str, typed: &str) -> Row {
        Row { text: text.into(), typed: typed.into() }
    }

    const FRAME: &str = "────────────────────────────────";

    #[test]
    fn claudes_framed_prompt() {
        assert_eq!(draft(&[row(FRAME), row("❯ fix the build")], &[row(FRAME)]), Some("fix the build".into()));
        // Its dim suggestion is no draft.
        assert_eq!(draft(&[row(FRAME), dim("❯ Try \"refactor x\"", "❯")], &[row(FRAME)]), None);
        assert_eq!(draft(&[row(FRAME), row("❯ ")], &[row(FRAME)]), None);
        // A `❯` with no frame above is not Claude's prompt.
        assert_eq!(draft(&[row("some output"), row("❯ ls")], &[]), None);
        // Rows of the draft below the cursor.
        assert_eq!(draft(&[row(FRAME), row("❯ one")], &[row("  two"), row(FRAME)]), Some("one\ntwo".into()));
    }

    #[test]
    fn codexs_prompt() {
        assert_eq!(draft(&[row("› review the diff")], &[row(""), row("  gpt-6 · ~/w")]), Some("review the diff".into()));
        assert_eq!(draft(&[dim("› Ask Codex to do anything", "›")], &[]), None);
        // An undimmed stock placeholder is not the user's either.
        assert_eq!(draft(&[row("› Ask Codex to do anything")], &[]), None);
    }

    #[test]
    fn no_prompt_no_draft() {
        assert_eq!(draft(&[row("$ cargo build")], &[]), None);
        assert_eq!(draft(&[], &[]), None);
    }
}
