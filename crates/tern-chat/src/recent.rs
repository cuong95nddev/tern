//! What a conversation is doing now: Claude's latest words and the tool it
//! called after them. The agents panel shows it under
//! each agent so you can follow one without opening its tab.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

use crate::model::{Entry, Transcript};
use crate::summary;

/// How much of a saved conversation's end is read: enough for the latest
/// message and tool calls, not the whole history.
pub(crate) const TAIL: u64 = 256 * 1024;
/// The longest line kept; the panel cuts it shorter.
const MAX: usize = 600;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recent {
    /// The last paragraph of Claude's latest reply, as one line.
    pub text: Option<String>,
    /// The latest tool call after that reply ("Ran cargo test").
    pub tool: Option<String>,
}

impl Recent {
    pub fn is_empty(&self) -> bool {
        self.text.is_none() && self.tool.is_none()
    }
}

/// [`Recent`] of a chat's entries: Claude's latest text, also from an
/// earlier turn (a new message shouldn't blank the line while Claude
/// thinks), and the tool call after it.
pub fn recent(entries: &[Entry], cwd: Option<&Path>) -> Recent {
    let mut tool = None;
    for e in entries.iter().rev() {
        match e {
            Entry::Text { text, .. } => {
                if let Some(line) = last_line(text) {
                    return Recent { text: Some(line), tool };
                }
            },
            Entry::Tool(t) if tool.is_none() => tool = Some(summary::label(t, cwd)),
            _ => {},
        }
    }
    Recent { text: None, tool }
}

/// [`Recent`] of the conversation saved at `path` (a `.jsonl`), read from
/// its end.
pub fn read_recent(path: &Path, cwd: Option<&Path>) -> std::io::Result<Recent> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let from = len.saturating_sub(TAIL);
    f.seek(SeekFrom::Start(from))?;
    let mut buf = vec![];
    f.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.lines();
    // Read from the middle of the file, the first line is cut.
    if from > 0 {
        lines.next();
    }
    let mut t = Transcript::default();
    let mut fold = crate::claude::ClaudeFold::default();
    for line in lines {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        // As `ClaudeFold::load`: the rest are Claude's own records.
        if matches!(v.get("type").and_then(Value::as_str), Some("user" | "assistant" | "system")) {
            fold.apply(&mut t, &v);
        }
    }
    Ok(recent(&t.entries, cwd))
}

/// The last paragraph of `text` with words in it, without its Markdown
/// marks, as one line (the panel wraps and cuts it).
fn last_line(text: &str) -> Option<String> {
    let words = |l: &String| l.chars().any(char::is_alphanumeric);
    let mut lines = text.lines().rev().map(plain).skip_while(|l| !words(l));
    let mut para: Vec<String> = lines.by_ref().take_while(words).collect();
    if para.is_empty() {
        return None;
    }
    para.reverse();
    let l = para.join(" ");
    Some(if l.chars().count() > MAX { l.chars().take(MAX - 1).collect::<String>() + "…" } else { l })
}

/// `line` without its list, quote and heading marks, emphasis and code ticks.
fn plain(line: &str) -> String {
    let mut l = line.trim();
    loop {
        let before = l;
        l = l.trim_start_matches(['#', '>', '-', '*', '+', '|']).trim_start();
        // "1. ", "12) "
        let digits = l.len() - l.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits > 0 && l[digits..].starts_with(['.', ')']) {
            l = l[digits + 1..].trim_start();
        }
        if l == before {
            break;
        }
    }
    l.trim_end_matches(['|', ' ']).replace("**", "").replace("__", "").replace('`', "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Tool;

    fn text(s: &str) -> Entry {
        Entry::Text { text: s.into(), streaming: false }
    }

    fn tool(name: &str, input: Value) -> Entry {
        Entry::Tool(Tool::new(name.into(), input))
    }

    #[test]
    fn last_text_line_and_the_tool_after_it() {
        let e = vec![
            Entry::User { text: "fix it".into(), images: vec![] },
            text("Looking at it.\n\nNow let me check **the** `parser`:"),
            tool("Read", serde_json::json!({"file_path": "/p/src/a.rs"})),
            tool("Bash", serde_json::json!({"command": "cargo test", "description": "Run the tests"})),
        ];
        let r = recent(&e, Some(Path::new("/p")));
        assert_eq!(r.text.as_deref(), Some("Now let me check the parser:"));
        assert_eq!(r.tool.as_deref(), Some("Running Run the tests"));
    }

    #[test]
    fn a_new_message_keeps_the_last_text() {
        let e = vec![text("Done."), Entry::User { text: "next".into(), images: vec![] }];
        assert_eq!(recent(&e, None).text.as_deref(), Some("Done."));
    }

    #[test]
    fn markdown_marks_go() {
        assert_eq!(plain("## 1. **Heading**"), "Heading");
        assert_eq!(plain("- item `x`"), "item x");
        assert_eq!(plain("> quoted"), "quoted");
        assert_eq!(last_line("Text\n\n---\n"), Some("Text".into()));
        assert_eq!(last_line("Intro.\n\n- one\n- two\n"), Some("one two".into()));
    }

    #[test]
    fn reads_a_saved_conversations_end() {
        let dir = std::env::temp_dir().join(format!("tern-recent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let lines = [
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#,
            r#"{"type":"assistant","message":{"id":"m1","role":"assistant","content":[{"type":"text","text":"Checking the build."}]}}"#,
            r#"{"type":"assistant","message":{"id":"m1","role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Grep","input":{"pattern":"foo"}}]}}"#,
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();
        let r = read_recent(&path, None).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(r.text.as_deref(), Some("Checking the build."));
        assert_eq!(r.tool.as_deref(), Some("Searching foo"));
    }
}
