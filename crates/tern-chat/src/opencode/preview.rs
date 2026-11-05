//! What an OpenCode tab is doing: its conversations live in a database Tern
//! doesn't read, so the sidebar reads the short file the daemon keeps per tab
//! from its events (`tern_bridge::opencode_fold`).

use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

use crate::model::{Entry, Tool};
use crate::recent::{Recent, TAIL, recent};

/// The whole lines at the end of the file at `path`.
fn tail(path: &Path) -> io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let from = f.metadata()?.len().saturating_sub(TAIL);
    f.seek(SeekFrom::Start(from))?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Ok(if from > 0 { text.split_once('\n').map_or_else(String::new, |(_, rest)| rest.to_owned()) } else { text })
}

/// What `line` shows as; `None` for one still being written, or that tells
/// of the tool before it.
fn entry(line: &str) -> Option<Entry> {
    let v: Value = serde_json::from_str(line).ok()?;
    if let Some(text) = v.get("text").and_then(Value::as_str) {
        return Some(Entry::Text { text: text.to_owned(), streaming: false });
    }
    let name = v.get("tool")?.as_str()?.to_owned();
    Some(Entry::Tool(Tool::new(name, v.get("input").cloned().unwrap_or(Value::Null))))
}

/// What the tab with preview file `path` is doing now.
pub fn read_recent(path: &Path, cwd: Option<&Path>) -> io::Result<Recent> {
    let mut entries = Vec::new();
    for line in tail(path)?.lines() {
        match entry(line) {
            Some(e) => entries.push(e),
            None if line.contains("\"done\"") => {
                if let Some(Entry::Tool(tool)) = entries.last_mut() {
                    tool.result = Some(String::new());
                }
            },
            None => {},
        }
    }
    Ok(recent(&entries, cwd))
}

/// The agent's latest text in preview file `path`.
pub fn last_text(path: &Path) -> Option<String> {
    tail(path).ok()?.lines().rev().find_map(|l| match entry(l)? {
        Entry::Text { text, .. } => Some(text),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preview.json");
        std::fs::write(&path, text).unwrap();
        (dir, path)
    }

    #[test]
    fn a_text_then_a_tool() {
        let (_dir, path) = written("{\"text\":\"Looking.\"}\n{\"tool\":\"Bash\",\"input\":{\"command\":\"cargo test\"}}\n");
        let recent = read_recent(&path, None).unwrap();
        assert_eq!(recent.text.as_deref(), Some("Looking."));
        assert_eq!(recent.tool.as_deref(), Some("Running cargo test"));
    }

    #[test]
    fn a_finished_tool_reads_as_done() {
        let (_dir, path) = written("{\"tool\":\"Bash\",\"input\":{\"command\":\"cargo test\"}}\n{\"done\":true}\n");
        assert_eq!(read_recent(&path, None).unwrap().tool.as_deref(), Some("Ran cargo test"));
    }

    #[test]
    fn a_half_written_last_line_is_skipped() {
        let (_dir, path) = written("{\"text\":\"Done.\"}\n{\"tool\":\"Ba");
        assert_eq!(read_recent(&path, None).unwrap().text.as_deref(), Some("Done."));
        assert_eq!(last_text(&path).as_deref(), Some("Done."));
    }

    #[test]
    fn the_preview_is_read_as_a_bounded_tail() {
        let old = "{\"text\":\"old\"}\n".repeat(40_000);
        let (_dir, path) = written(&format!("x{old}{{\"text\":\"new\"}}\n"));
        assert_eq!(last_text(&path).as_deref(), Some("new"));
        assert_eq!(read_recent(&path, None).unwrap().text.as_deref(), Some("new"));
    }

    #[test]
    fn no_file_is_no_preview() {
        assert_eq!(last_text(std::path::Path::new("/nonexistent/preview")), None);
    }
}
