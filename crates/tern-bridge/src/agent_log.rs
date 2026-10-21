//! A Claude Code sub-agent's log, readable in a terminal. Claude Code keeps
//! each sub-agent's conversation as JSON lines (`<session>/subagents/
//! agent-<id>.jsonl`, beside the session's own); `tern --agent-log <file>`
//! prints what it says and what it does (its words, each tool it calls and
//! what that tool gave back), then follows the file as the sub-agent goes
//! on, as `tail -F` would.

use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

/// How many lines of a tool's output show; the rest is counted.
const OUTPUT_LINES: usize = 12;
/// How long a tool call's one-line summary may be.
const SUMMARY_CHARS: usize = 160;
/// The tool a sub-agent hands its report back with: the report is its last
/// word, not a tool call.
const HANDBACK: &str = "SubagentHandback";
/// How often the file is read again for what was added.
const POLL: Duration = Duration::from_millis(300);

const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const CYAN: &str = "\x1b[36m";
const RESET: &str = "\x1b[0m";

/// Sub-agent `id`'s log, from `transcript`, the session's own (any hook's
/// `transcript_path`).
pub fn path(transcript: &Path, id: &str) -> PathBuf {
    transcript.with_extension("").join("subagents").join(format!("agent-{id}.jsonl"))
}

/// `tern --agent-log <file>`: print the log at `file`, then what is added to
/// it, until ended. Waits for a file not there yet. Returns the exit code.
pub fn run(file: &Path) -> i32 {
    let mut out = std::io::stdout();
    if let Some(head) = header(file) {
        let _ = writeln!(out, "{head}\n");
    }
    let mut follow = Follower::new(file);
    loop {
        let text = follow.poll();
        if !text.is_empty() && out.write_all(text.as_bytes()).and_then(|()| out.flush()).is_err() {
            return 0;
        }
        std::thread::sleep(POLL);
    }
}

/// Reads a log as it grows.
struct Follower {
    file: PathBuf,
    /// How far the file has been read.
    at: u64,
    /// Read past the last whole line: the rest of it is still being written.
    pending: Vec<u8>,
    seen: Seen,
    finished: bool,
}

impl Follower {
    fn new(file: &Path) -> Self {
        Self { file: file.to_owned(), at: 0, pending: Vec::new(), seen: Seen::default(), finished: false }
    }

    /// What was added to the file since the last call, for a reader.
    fn poll(&mut self) -> String {
        if let Ok(mut f) = std::fs::File::open(&self.file) {
            let len = f.metadata().map(|m| m.len()).unwrap_or(0);
            // Written anew (shorter than read): start over.
            if len < self.at {
                self.at = 0;
                self.pending.clear();
                self.seen = Seen::default();
            }
            if len > self.at && f.seek(SeekFrom::Start(self.at)).is_ok() {
                let mut buf = Vec::new();
                if let Ok(n) = f.take(len - self.at).read_to_end(&mut buf) {
                    self.at += n as u64;
                    self.pending.extend_from_slice(&buf);
                }
            }
        }
        let mut text = String::new();
        if let Some(end) = self.pending.iter().rposition(|&b| b == b'\n') {
            for line in self.pending[..end].split(|&b| b == b'\n') {
                render(&String::from_utf8_lossy(line), &mut self.seen, &mut text);
            }
            self.pending.drain(..=end);
        }
        if !self.finished && meta(&self.file).and_then(|m| m.get("finished")?.as_bool()) == Some(true) {
            self.finished = true;
            text.push_str(&format!("{DIM}── finished ──{RESET}\n"));
        }
        text
    }
}

/// The file Claude Code keeps beside a sub-agent's log: its description and
/// type, and whether it finished.
fn meta(file: &Path) -> Option<Value> {
    let name = file.file_name()?.to_str()?.strip_suffix(".jsonl")?;
    serde_json::from_slice(&std::fs::read(file.with_file_name(format!("{name}.meta.json"))).ok()?).ok()
}

/// The log's first line: what the sub-agent does, and its type.
fn header(file: &Path) -> Option<String> {
    let meta = meta(file)?;
    let text = |k: &str| meta.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
    let what = text("description").or(text("agentType"))?;
    Some(match text("agentType") {
        Some(kind) if kind != what => format!("{BOLD}{what}{RESET} {DIM}· {kind}{RESET}"),
        _ => format!("{BOLD}{what}{RESET}"),
    })
}

/// What earlier lines of a log said that later ones need.
#[derive(Default)]
pub struct Seen {
    /// The tool calls whose results say nothing (a handed back report's).
    quiet: HashSet<String>,
}

/// Append what log line `line` says, for a reader, to `out`: nothing for
/// what Claude Code keeps for itself (requests, attachments, its own
/// reminders, the model's thinking). `seen` carries what the lines before
/// it said.
pub fn render(line: &str, seen: &mut Seen, out: &mut String) {
    let Ok(record) = serde_json::from_str::<Value>(line.trim()) else { return };
    let kind = record.get("type").and_then(Value::as_str);
    if !matches!(kind, Some("user" | "assistant")) || record.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return;
    }
    let Some(content) = record.pointer("/message/content") else { return };
    let blocks: Vec<Value> = match content {
        Value::String(s) => vec![serde_json::json!({ "type": "text", "text": s })],
        Value::Array(a) => a.clone(),
        _ => return,
    };
    for block in &blocks {
        let field = |k: &str| block.get(k).and_then(Value::as_str);
        match (kind, field("type")) {
            (_, Some("text")) => {
                let text = field("text").unwrap_or_default().trim();
                // Claude Code's own notes in the conversation.
                if text.is_empty() || text.starts_with("<system-reminder>") {
                    continue;
                }
                if kind == Some("user") {
                    out.push_str(&format!("{CYAN}❯{RESET} {text}\n\n"));
                } else {
                    out.push_str(&format!("{text}\n\n"));
                }
            },
            (Some("assistant"), Some("tool_use")) => {
                let name = field("name").unwrap_or("tool");
                let report = block.pointer("/input/message").and_then(Value::as_str).filter(|_| name == HANDBACK);
                if let Some(report) = report {
                    if let Some(id) = field("id") {
                        seen.quiet.insert(id.to_owned());
                    }
                    out.push_str(&format!("{}\n\n", report.trim()));
                    continue;
                }
                let summary = block.get("input").map(summary).unwrap_or_default();
                out.push_str(&format!("{BOLD}⏺ {name}{RESET}{}\n", if summary.is_empty() { String::new() } else { format!(" {summary}") }));
            },
            (Some("user"), Some("tool_result")) => {
                if field("tool_use_id").is_some_and(|id| seen.quiet.remove(id)) {
                    continue;
                }
                let error = block.get("is_error").and_then(Value::as_bool) == Some(true);
                let text = block.get("content").map(result_text).unwrap_or_default();
                out.push_str(&output(&text, error));
                out.push('\n');
            },
            _ => {},
        }
    }
}

/// A tool call's input in one line: the command, file or pattern it names,
/// else its JSON.
fn summary(input: &Value) -> String {
    let named = ["command", "file_path", "notebook_path", "pattern", "url", "query", "description", "prompt"]
        .iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str));
    let text = match named {
        Some(s) => s.to_owned(),
        None if input.as_object().is_some_and(|o| o.is_empty()) => String::new(),
        None => input.to_string(),
    };
    let line = text.lines().next().unwrap_or_default();
    let mut short: String = line.chars().take(SUMMARY_CHARS).collect();
    if short.len() < line.len() || text.lines().nth(1).is_some() {
        short.push('…');
    }
    short
}

/// A tool result's text: a string, or text blocks (an image says so).
fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .map(|b| match b.get("type").and_then(Value::as_str) {
                Some("text") => b.get("text").and_then(Value::as_str).unwrap_or_default().to_owned(),
                Some(other) => format!("[{other}]"),
                None => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// A tool's output, indented under its call: its first lines, and how many
/// more there are.
fn output(text: &str, error: bool) -> String {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    let color = if error { RED } else { DIM };
    let mut out = String::new();
    for (i, line) in lines.iter().take(OUTPUT_LINES).enumerate() {
        let lead = if i == 0 { "  ⎿ " } else { "    " };
        out.push_str(&format!("{color}{lead}{line}{RESET}\n"));
    }
    if lines.len() > OUTPUT_LINES {
        out.push_str(&format!("{DIM}    … {} more lines{RESET}\n", lines.len() - OUTPUT_LINES));
    }
    if lines.is_empty() {
        out.push_str(&format!("{color}  ⎿ (no output){RESET}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text of `out`, without its colors.
    fn plain(out: &str) -> String {
        let mut s = String::new();
        let mut esc = false;
        for c in out.chars() {
            match (esc, c) {
                (false, '\x1b') => esc = true,
                (true, 'm') => esc = false,
                (true, _) => {},
                (false, c) => s.push(c),
            }
        }
        s
    }

    fn rendered(lines: &[&str]) -> String {
        let (mut out, mut seen) = (String::new(), Seen::default());
        for l in lines {
            render(l, &mut seen, &mut out);
        }
        plain(&out)
    }

    #[test]
    fn finds_a_sub_agents_log_beside_its_session() {
        let p = path(Path::new("/p/proj/a463.jsonl"), "a79");
        assert_eq!(p, Path::new("/p/proj/a463/subagents/agent-a79.jsonl"));
    }

    /// Records as Claude Code 2.1 wrote them for a sub-agent.
    #[test]
    fn shows_words_tools_and_their_output() {
        let out = rendered(&[
            r#"{"type":"user","isSidechain":true,"message":{"role":"user","content":"Run sleep 30 with Bash, then reply ok"}}"#,
            r#"{"type":"attachment","attachment":{}}"#,
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"<system-reminder>x"}}"#,
            r#"{"type":"api-request-blob","message":{"role":"user","content":[{"type":"text","text":"hidden"}]}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hmm"}]}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"sleep 30","description":"Sleep"}}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"<tool_use_error>Blocked</tool_use_error>","is_error":true}]}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/a.rs"}}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"fn a() {}"}]}]}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t2","name":"SubagentHandback","input":{"message":"All done."}}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t2","content":[{"type":"text","text":"{\"success\":true}"}]}]}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"ok"}]}}"#,
        ]);
        assert_eq!(
            out,
            "❯ Run sleep 30 with Bash, then reply ok\n\n\
             ⏺ Bash sleep 30\n  ⎿ <tool_use_error>Blocked</tool_use_error>\n\n\
             ⏺ Read /a.rs\n  ⎿ fn a() {}\n\n\
             All done.\n\n\
             ok\n\n"
        );
    }

    #[test]
    fn long_output_is_cut() {
        let text: Vec<String> = (1..=20).map(|i| format!("line {i}")).collect();
        let out = plain(&output(&text.join("\n"), false));
        assert!(out.contains("line 12\n") && !out.contains("line 13"));
        assert!(out.ends_with("… 8 more lines\n"));
    }

    #[test]
    fn a_half_written_line_says_nothing() {
        assert_eq!(rendered(&[r#"{"type":"assistant","message":{"role":"#]), "");
    }

    #[test]
    fn follows_the_file_as_it_grows() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("agent-x.jsonl");
        let mut follow = Follower::new(&file);
        // Not there yet.
        assert_eq!(follow.poll(), "");
        let mut f = std::fs::File::create(&file).unwrap();
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#;
        let (head, tail) = line.split_at(20);
        write!(f, "{head}").unwrap();
        assert_eq!(follow.poll(), "");
        writeln!(f, "{tail}").unwrap();
        assert_eq!(plain(&follow.poll()), "hi\n\n");
        assert_eq!(follow.poll(), "");
        // Written anew, shorter: read again from its start.
        std::fs::write(&file, format!("{}\n", line.replace("hi", "y"))).unwrap();
        assert_eq!(plain(&follow.poll()), "y\n\n");
        std::fs::write(dir.path().join("agent-x.meta.json"), r#"{"description":"d","finished":true}"#).unwrap();
        assert_eq!(plain(&follow.poll()), "── finished ──\n");
        assert_eq!(follow.poll(), "");
    }
}
