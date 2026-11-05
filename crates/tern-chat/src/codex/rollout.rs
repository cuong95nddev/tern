//! Codex's saved conversations: `<home>/sessions/YYYY/MM/DD/rollout-<time>-<id>.jsonl`.

use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tern_bridge::context::Reading;

use crate::claude::fold::timestamp_ms;
use crate::model::{Entry, Tool, TurnEnd};
use crate::recent::{Recent, TAIL, recent};

/// Tokens Codex counts as always there, out of what it shows as used.
const BASELINE: u64 = 12_000;

/// The folder Codex keeps its config and conversations in.
pub fn codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|h| h.join(".codex")))
        .unwrap_or_else(|| ".codex".into())
}

/// The entries of `dir`, by name, last first.
fn entries_newest_first(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut paths: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    paths.sort_by(|a, b| b.cmp(a));
    paths
}

/// The rollouts under `home`, the latest first (their names start with
/// their time). Compressed ones aren't read.
fn rollouts(home: &Path) -> impl Iterator<Item = PathBuf> {
    entries_newest_first(&home.join("sessions"))
        .into_iter()
        .flat_map(|year| entries_newest_first(&year))
        .flat_map(|month| entries_newest_first(&month))
        .flat_map(|day| entries_newest_first(&day))
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
}

/// Where conversation `session` is saved under `home`, if it is.
pub fn rollout_path(home: &Path, session: &str) -> Option<PathBuf> {
    let end = format!("-{session}.jsonl");
    rollouts(home).find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().ends_with(&end)))
}

/// The end of the rollout at `path`, in whole lines.
fn tail(path: &Path) -> io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let from = f.metadata()?.len().saturating_sub(TAIL);
    f.seek(SeekFrom::Start(from))?;
    let mut buf = vec![];
    f.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    Ok(if from == 0 { text } else { text.split_once('\n').map_or_else(String::new, |(_, rest)| rest.to_owned()) })
}

/// The payloads of the `event_msg` lines with `needle` in `text`, last
/// first. A line still being written doesn't parse, and is left out.
fn events_back<'a>(text: &'a str, needle: &'a str) -> impl Iterator<Item = Value> + 'a {
    text.lines().rev().filter(move |l| l.contains(needle)).filter_map(|l| {
        let mut v: Value = serde_json::from_str(l).ok()?;
        (v.get("type")?.as_str()? == "event_msg").then(|| v["payload"].take())
    })
}

/// Percent of a window of `window` tokens that `total` fill, as Codex shows
/// it (`percent_of_context_window_remaining`).
pub fn context_percent(total: u64, window: u64) -> f32 {
    if window <= BASELINE {
        return 100.;
    }
    let size = window - BASELINE;
    let left = size.saturating_sub(total.saturating_sub(BASELINE));
    100. - (left as f64 / size as f64 * 100.).round() as f32
}

/// How full the conversation saved at `path` is, by its latest count.
pub fn context_reading(path: &Path) -> Option<Reading> {
    let text = tail(path).ok()?;
    events_back(&text, "token_count").find_map(|p| {
        let info = p.get("info")?;
        let total = info.pointer("/last_token_usage/total_tokens")?.as_u64()?;
        let size = info.get("model_context_window")?.as_u64().filter(|s| *s > 0)?;
        Some(Reading { percent: context_percent(total, size), size })
    })
}

/// The latest quota the conversation saved at `path` was told.
pub fn rate_limits(path: &Path) -> Option<Value> {
    let text = tail(path).ok()?;
    events_back(&text, "token_count").find_map(|p| p.get("rate_limits").filter(|r| r.is_object()).cloned())
}

/// The latest quota any conversation under `home` was told: the last few
/// are looked at, a new one has none before its first answer.
pub fn latest_rate_limits(home: &Path) -> Option<Value> {
    rollouts(home).take(8).find_map(|p| rate_limits(&p))
}

/// The words of item `item` (an `AgentMessage`, a `UserMessage`).
fn item_text(item: &Value) -> Option<String> {
    let parts = item.get("content")?.as_array()?;
    let text: Vec<_> = parts.iter().filter_map(|p| p.get("text")?.as_str()).collect();
    (!text.is_empty()).then(|| text.join("\n"))
}

/// What a finished item shows as.
fn item_entries(item: &Value) -> Vec<Entry> {
    let done = |name: &str, input: Value| {
        let mut tool = Tool::new(name.to_owned(), input);
        tool.result = Some(String::new());
        Entry::Tool(tool)
    };
    match item.get("type").and_then(Value::as_str) {
        Some("UserMessage") => item_text(item).map(|text| Entry::User { text, images: Vec::new() }).into_iter().collect(),
        Some("AgentMessage") => item_text(item).map(|text| Entry::Text { text, streaming: false }).into_iter().collect(),
        // `[shell, -lc, line]`.
        Some("CommandExecution") => {
            let line = item.get("command").and_then(Value::as_array).and_then(|c| c.last()).and_then(Value::as_str);
            line.map(|l| done("Bash", json!({ "command": l }))).into_iter().collect()
        },
        Some("FileChange") => {
            let changes = item.get("changes").and_then(Value::as_object);
            changes.into_iter().flatten().map(|(path, _)| done("Edit", json!({ "file_path": path }))).collect()
        },
        _ => Vec::new(),
    }
}

/// What the rollout lines `text` show as, in order.
pub(crate) fn entries(text: &str) -> Vec<Entry> {
    let mut items: Vec<_> = events_back(text, "item_completed").collect();
    items.reverse();
    items.iter().filter_map(|p| p.get("item")).flat_map(item_entries).collect()
}

/// What the rollout lines `text` show as in a chat, with the ends of their
/// turns: those saved before `before` (ms since the Unix epoch).
pub(crate) fn history(text: &str, before: Option<u64>) -> Vec<Entry> {
    let mut shown = Vec::new();
    let wanted = ["item_completed", "task_complete", "turn_aborted"];
    for line in text.lines().filter(|l| wanted.iter().any(|w| l.contains(w))) {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let at = v.get("timestamp").and_then(Value::as_str).and_then(timestamp_ms);
        if before.is_some_and(|b| at.is_none_or(|at| at >= b)) {
            break;
        }
        if v.get("type").and_then(Value::as_str) != Some("event_msg") {
            continue;
        }
        let p = &v["payload"];
        let ms = p.get("duration_ms").and_then(Value::as_u64).unwrap_or(0);
        match p.get("type").and_then(Value::as_str) {
            Some("item_completed") => shown.extend(p.get("item").into_iter().flat_map(item_entries)),
            Some("task_complete") => shown.push(Entry::TurnEnd(TurnEnd { ms, interrupted: false, error: None })),
            Some("turn_aborted") => shown.push(Entry::TurnEnd(TurnEnd { ms, interrupted: true, error: None })),
            _ => {},
        }
    }
    shown
}

/// [`Recent`] of the conversation saved at `path`.
pub fn read_recent(path: &Path, cwd: Option<&Path>) -> io::Result<Recent> {
    Ok(recent(&entries(&tail(path)?), cwd))
}

/// The text of the last answer in the conversation saved at `path`.
pub fn last_text(path: &Path) -> Option<String> {
    let text = tail(path).ok()?;
    text.lines().rev().find_map(|l| {
        if !l.contains("task_complete") && !l.contains("AgentMessage") {
            return None;
        }
        let v: Value = serde_json::from_str(l).ok()?;
        let p = v.get("payload")?;
        match p.get("type")?.as_str()? {
            "task_complete" => p.get("last_agent_message")?.as_str().map(str::to_owned),
            "item_completed" if p.pointer("/item/type")?.as_str()? == "AgentMessage" => item_text(p.get("item")?),
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    const FIXTURE: &str = include_str!("fixtures/rollout.jsonl");

    fn written(text: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        std::fs::write(&path, text).unwrap();
        (dir, path)
    }

    #[test]
    fn percent_matches_what_codex_shows() {
        assert_eq!(context_percent(14_653, 258_400), 1.);
        assert_eq!(context_percent(135_200, 258_400), 50.);
        assert_eq!(context_percent(258_400, 258_400), 100.);
        assert_eq!(context_percent(5, 258_400), 0.);
        assert_eq!(context_percent(5, 10_000), 100.);
    }

    #[test]
    fn reading_is_the_latest_count_with_a_window() {
        let (_d, path) = written(FIXTURE);
        let r = context_reading(&path).unwrap();
        assert_eq!((r.percent, r.size), (context_percent(15_567, 258_400), 258_400));
    }

    #[test]
    fn a_count_without_info_keeps_the_one_before() {
        let (_d, path) = written(FIXTURE);
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, r#"{{"type":"event_msg","payload":{{"type":"token_count","info":null,"rate_limits":{{"limit_id":"codex"}}}}}}"#)
            .unwrap();
        assert_eq!(context_reading(&path).map(|r| r.size), Some(258_400));
    }

    #[test]
    fn a_half_written_last_line_is_skipped() {
        let (_d, path) = written(&format!("{FIXTURE}{}", r#"{"type":"event_msg","payload":{"type":"token_cou"#));
        assert_eq!(context_reading(&path).map(|r| r.size), Some(258_400));
        assert_eq!(last_text(&path).as_deref(), Some("Done."));
    }

    #[test]
    fn rate_limits_are_the_latest() {
        let (_d, path) = written(FIXTURE);
        assert_eq!(rate_limits(&path).unwrap()["limit_id"], "codex");
    }

    #[test]
    fn recent_says_the_latest_words() {
        let (_d, path) = written(FIXTURE);
        let r = read_recent(&path, Some(Path::new("/w"))).unwrap();
        assert_eq!((r.text.as_deref(), r.tool), (Some("Done."), None));
    }

    #[test]
    fn recent_says_the_tool_after_the_words() {
        let mid: String = FIXTURE.lines().take(7).map(|l| format!("{l}\n")).collect();
        let (_d, path) = written(&mid);
        let r = read_recent(&path, Some(Path::new("/w"))).unwrap();
        assert!(r.text.unwrap().starts_with("I’ll list"));
        assert_eq!(r.tool.as_deref(), Some("Edited b.txt"));
    }

    #[test]
    fn a_shell_command_is_what_ran_in_the_shell() {
        let lines: String = FIXTURE.lines().take(4).map(|l| format!("{l}\n")).collect();
        let (_d, path) = written(&lines);
        assert_eq!(read_recent(&path, None).unwrap().tool.as_deref(), Some("Ran ls"));
    }

    #[test]
    fn last_text_is_the_turns_answer() {
        let (_d, path) = written(FIXTURE);
        assert_eq!(last_text(&path).as_deref(), Some("Done."));
    }

    #[test]
    fn last_text_without_a_finished_turn_is_the_latest_message() {
        let mid: String = FIXTURE.lines().take(3).map(|l| format!("{l}\n")).collect();
        let (_d, path) = written(&mid);
        assert!(last_text(&path).unwrap().starts_with("I’ll list"));
    }

    #[test]
    fn a_big_rollout_is_read_from_its_tail() {
        let filler = format!("{{\"type\":\"response_item\",\"payload\":{{\"x\":\"{}\"}}}}\n", "y".repeat(1024));
        let (_d, path) = written(&format!("{}{FIXTURE}", filler.repeat(2048)));
        assert_eq!(context_reading(&path).map(|r| r.size), Some(258_400));
        assert_eq!(read_recent(&path, None).unwrap().text.as_deref(), Some("Done."));
    }

    #[test]
    fn rollout_path_finds_a_session_newest_first() {
        let home = tempfile::tempdir().unwrap();
        let day = |d: &str| {
            let p = home.path().join("sessions/2026/11").join(d);
            std::fs::create_dir_all(&p).unwrap();
            p
        };
        let file = day("05").join("rollout-2026-11-05T15-10-31-ID.jsonl");
        std::fs::write(&file, "").unwrap();
        std::fs::write(day("04").join("rollout-2026-11-04T15-10-31-OTHER.jsonl"), "").unwrap();
        assert_eq!(rollout_path(home.path(), "ID"), Some(file));
        assert_eq!(rollout_path(home.path(), "NONE"), None);
    }

    #[test]
    fn a_compressed_rollout_is_not_read() {
        let home = tempfile::tempdir().unwrap();
        let day = home.path().join("sessions/2026/11/05");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(day.join("rollout-x-ID.jsonl.zst"), "").unwrap();
        assert_eq!(rollout_path(home.path(), "ID"), None);
    }

    #[test]
    fn the_latest_quota_is_the_newest_conversations_that_has_one() {
        let home = tempfile::tempdir().unwrap();
        let day = home.path().join("sessions/2026/11/05");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(day.join("rollout-a-1.jsonl"), FIXTURE).unwrap();
        std::fs::write(day.join("rollout-b-2.jsonl"), "").unwrap();
        assert_eq!(latest_rate_limits(home.path()).unwrap()["limit_id"], "codex");
        assert_eq!(latest_rate_limits(Path::new("/nonexistent")), None);
    }
}
