//! Words for tool calls: one call's line, a group's summary, a turn's length.

use std::collections::HashSet;
use std::path::Path;

use serde_json::Value;

use crate::model::Tool;

/// What a tool does, for wording and grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Read,
    Edit,
    Command,
    Search,
    Web,
    Agent,
    Todo,
    Other,
}

pub fn kind(name: &str) -> Kind {
    match name {
        "Read" | "NotebookRead" => Kind::Read,
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => Kind::Edit,
        "Bash" | "BashOutput" | "KillShell" | "Monitor" => Kind::Command,
        "Grep" | "Glob" | "LS" | "LSP" => Kind::Search,
        "WebFetch" | "WebSearch" => Kind::Web,
        "Task" | "Agent" => Kind::Agent,
        "TodoWrite" => Kind::Todo,
        _ => Kind::Other,
    }
}

/// An icon for it (a Lucide name).
pub fn icon(name: &str) -> &'static str {
    match kind(name) {
        Kind::Read => "file-text",
        Kind::Edit => "file-pen",
        Kind::Command => "square-terminal",
        Kind::Search => "search",
        Kind::Web => "globe",
        Kind::Agent => "bot",
        Kind::Todo => "list-checks",
        Kind::Other => "wrench",
    }
}

fn str_of<'a>(input: &'a Value, key: &str) -> &'a str {
    input.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// `path` relative to `cwd` when it is under it.
pub fn short_path(path: &str, cwd: Option<&Path>) -> String {
    let p = Path::new(path);
    match cwd.and_then(|c| p.strip_prefix(c).ok()) {
        Some(rel) if !rel.as_os_str().is_empty() => rel.to_string_lossy().into_owned(),
        _ => path.to_owned(),
    }
}

/// The first line of `s`, cut to `max` characters.
pub fn first_line(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or_default().trim();
    let more = s.trim().lines().nth(1).is_some();
    if line.chars().count() > max {
        line.chars().take(max - 1).collect::<String>() + "…"
    } else if more {
        format!("{line} …")
    } else {
        line.to_owned()
    }
}

/// One call's line: "Read src/main.rs", "Ran cargo test", "Editing lib.rs".
pub fn label(tool: &Tool, cwd: Option<&Path>) -> String {
    let i = &tool.input;
    let done = tool.done();
    let path = || {
        let p =
            [str_of(i, "file_path"), str_of(i, "notebook_path"), str_of(i, "path")].into_iter().find(|p| !p.is_empty()).unwrap_or_default();
        short_path(p, cwd)
    };
    let pick = |now: &str, then: &str| if done { then.to_owned() } else { now.to_owned() };
    match tool.name.as_str() {
        "Read" | "NotebookRead" => format!("{} {}", pick("Reading", "Read"), path()),
        "Edit" | "MultiEdit" | "NotebookEdit" => format!("{} {}", pick("Editing", "Edited"), path()),
        "Write" => format!("{} {}", pick("Writing", "Wrote"), path()),
        "Bash" => {
            let what = Some(str_of(i, "description"))
                .filter(|d| !d.is_empty())
                .map_or_else(|| first_line(str_of(i, "command"), 80), str::to_owned);
            format!("{} {what}", pick("Running", "Ran"))
        },
        "Grep" => {
            let mut s = format!("{} {}", pick("Searching", "Searched"), str_of(i, "pattern"));
            let p = path();
            if !p.is_empty() {
                s.push_str(&format!(" in {p}"));
            }
            s
        },
        "Glob" => format!("{} files {}", pick("Finding", "Found"), str_of(i, "pattern")),
        "WebFetch" => format!("{} {}", pick("Fetching", "Fetched"), str_of(i, "url")),
        "WebSearch" => format!("{} the web for {}", pick("Searching", "Searched"), str_of(i, "query")),
        "Task" | "Agent" => format!("Agent: {}", first_line(str_of(i, "description"), 80)),
        "TodoWrite" => "Updated the plan".to_owned(),
        "ExitPlanMode" => "Proposed a plan".to_owned(),
        "AskUserQuestion" => "Asked a question".to_owned(),
        name => match name.strip_prefix("mcp__").and_then(|r| r.split_once("__")) {
            Some((server, tool)) => format!("{server}: {}", tool.replace('_', " ")),
            None => format!("{} {name}", pick("Using", "Used")),
        },
    }
}

/// What an opened call shows: its command or change, then its output, at
/// most `max_lines` lines of each.
pub fn detail(tool: &Tool, max_lines: usize) -> String {
    let i = &tool.input;
    let cut = |s: &str| {
        let lines: Vec<&str> = s.lines().collect();
        if lines.len() > max_lines {
            format!("{}\n… {} more lines", lines[..max_lines].join("\n"), lines.len() - max_lines)
        } else {
            s.to_owned()
        }
    };
    let head = match tool.name.as_str() {
        "Bash" => format!("$ {}", str_of(i, "command")),
        "Edit" => {
            let old: Vec<String> = str_of(i, "old_string").lines().map(|l| format!("- {l}")).collect();
            let new: Vec<String> = str_of(i, "new_string").lines().map(|l| format!("+ {l}")).collect();
            return cut(&[old, new].concat().join("\n"));
        },
        "Write" => return cut(str_of(i, "content")),
        "Task" | "Agent" => str_of(i, "prompt").to_owned(),
        _ => String::new(),
    };
    let out = tool.result.as_deref().map(cut).unwrap_or_default();
    match (head.is_empty(), out.is_empty()) {
        (true, _) => out,
        (false, true) => cut(&head),
        (false, false) => format!("{}\n\n{out}", cut(&head)),
    }
}

/// The change `tool` makes to a file, as a unified patch: the one it
/// reported once it ran (with the file's line numbers), else one worked out
/// from its input (numbered from the start of the replaced text). None for
/// a tool that changes no file.
pub fn patch(tool: &Tool) -> Option<String> {
    if let Some(p) = &tool.patch {
        return Some(p.clone());
    }
    let i = &tool.input;
    let path = str_of(i, "file_path");
    let hunks = match tool.name.as_str() {
        "Edit" => text_hunks(str_of(i, "old_string"), str_of(i, "new_string")),
        "MultiEdit" => {
            i.get("edits").and_then(Value::as_array)?.iter().map(|e| text_hunks(str_of(e, "old_string"), str_of(e, "new_string"))).collect()
        },
        "Write" => return Some(created(path, str_of(i, "content"))),
        _ => return None,
    };
    (!hunks.is_empty()).then(|| format!("{}{hunks}", header(path)))
}

/// The patch in a tool result (`tool_use_result`): its `structuredPatch`,
/// or the whole content of a file it created.
pub fn result_patch(meta: &Value) -> Option<String> {
    let path = str_of(meta, "filePath");
    if path.is_empty() {
        return None;
    }
    let hunks = meta.get("structuredPatch").and_then(Value::as_array).filter(|h| !h.is_empty());
    let Some(hunks) = hunks else {
        return (meta.get("type").and_then(Value::as_str) == Some("create")).then(|| created(path, str_of(meta, "content")));
    };
    let mut out = header(path);
    for h in hunks {
        let n = |k: &str| h.get(k).and_then(Value::as_u64).unwrap_or(0);
        out.push_str(&format!("@@ -{},{} +{},{} @@\n", n("oldStart"), n("oldLines"), n("newStart"), n("newLines")));
        for line in h.get("lines").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
            out.push_str(line);
            out.push('\n');
        }
    }
    Some(out)
}

fn header(path: &str) -> String {
    format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n")
}

/// A new file's patch: all of `content` added.
fn created(path: &str, content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut out = format!("diff --git a/{path} b/{path}\nnew file mode 100644\n--- /dev/null\n+++ b/{path}\n");
    out.push_str(&format!("@@ -0,0 +1,{} @@\n", lines.len()));
    for l in lines {
        out.push('+');
        out.push_str(l);
        out.push('\n');
    }
    out
}

/// The hunks turning `old` into `new`, line by line.
fn text_hunks(old: &str, new: &str) -> String {
    let end = |s: &str| if s.is_empty() || s.ends_with('\n') { s.to_owned() } else { format!("{s}\n") };
    let (old, new) = (end(old), end(new));
    let diff = similar::TextDiff::from_lines(&old, &new);
    diff.unified_diff().context_radius(3).iter_hunks().map(|h| h.to_string()).collect()
}

/// A group of calls in a line: "Read 3 files, ran 2 commands and 1 other action".
pub fn group(tools: &[&Tool]) -> String {
    // Kind, count; edits count files.
    let mut counts: Vec<(Kind, usize)> = Vec::new();
    let mut edited = HashSet::new();
    for t in tools {
        let k = kind(&t.name);
        if k == Kind::Edit {
            edited.insert(str_of(&t.input, "file_path").to_owned());
        }
        match counts.iter_mut().find(|(c, _)| *c == k) {
            Some((_, n)) => *n += 1,
            None => counts.push((k, 1)),
        }
    }
    if let Some((_, n)) = counts.iter_mut().find(|(k, _)| *k == Kind::Edit) {
        *n = edited.len().max(1);
    }
    let rank = |k: Kind| match k {
        Kind::Command | Kind::Edit => 0,
        Kind::Other | Kind::Todo => 2,
        _ => 1,
    };
    counts.sort_by_key(|(k, _)| rank(*k));
    let plural = |n: usize, one: &str, many: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {many}") };
    let mut parts: Vec<String> = counts
        .iter()
        .take(2)
        .map(|&(k, n)| match k {
            Kind::Read => format!("read {}", plural(n, "file", "files")),
            Kind::Edit => format!("edited {}", plural(n, "file", "files")),
            Kind::Command => format!("ran {}", plural(n, "command", "commands")),
            Kind::Search => format!("searched {}", plural(n, "time", "times")),
            Kind::Web => format!("browsed {}", plural(n, "time", "times")),
            Kind::Agent => format!("ran {}", plural(n, "agent", "agents")),
            Kind::Todo => format!("updated the plan{}", if n > 1 { format!(" {n} times") } else { String::new() }),
            Kind::Other => format!("used {}", plural(n, "tool", "tools")),
        })
        .collect();
    let rest: usize = counts.iter().skip(2).map(|(_, n)| n).sum();
    if rest > 0 {
        parts.push(plural(rest, "other action", "other actions"));
    }
    let text = match parts.len() {
        0 => String::new(),
        1 => parts.remove(0),
        _ => {
            let last = parts.pop().unwrap_or_default();
            format!("{} and {last}", parts.join(", "))
        },
    };
    let mut c = text.chars();
    c.next().map_or_else(String::new, |f| f.to_uppercase().collect::<String>() + c.as_str())
}

/// A length of time as a turn shows it: "820ms", "4.2s", "42s", "3m 5s", "1h 2m 3s".
pub fn duration(ms: u64) -> String {
    match ms {
        0..1_000 => format!("{ms}ms"),
        1_000..10_000 => format!("{:.1}s", ms as f64 / 1000.0),
        10_000..60_000 => format!("{}s", ms / 1000),
        _ => {
            let s = ms / 1000;
            let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
            if h > 0 { format!("{h}h {m}m {s}s") } else { format!("{m}m {s}s") }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str, input: Value, result: Option<&str>) -> Tool {
        let mut t = Tool::new(name.to_owned(), input);
        t.result = result.map(str::to_owned);
        t
    }

    #[test]
    fn patches_from_input_and_from_results() {
        let edit = tool("Edit", json!({"file_path":"/w/a.rs","old_string":"a\nb\nc","new_string":"a\nB\nc"}), None);
        let p = patch(&edit).unwrap();
        assert!(p.starts_with("diff --git a//w/a.rs b//w/a.rs\n--- a//w/a.rs\n+++ b//w/a.rs\n@@ -1,3 +1,3 @@\n"), "{p}");
        assert!(p.ends_with(" a\n-b\n+B\n c\n"), "{p}");
        let write = tool("Write", json!({"file_path":"n.txt","content":"x\ny\n"}), None);
        assert!(patch(&write).unwrap().ends_with("--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1,2 @@\n+x\n+y\n"));
        assert_eq!(patch(&tool("Read", json!({"file_path":"a"}), None)), None);

        let meta =
            json!({"filePath":"x.txt","structuredPatch":[{"oldStart":3,"oldLines":2,"newStart":3,"newLines":2,"lines":[" c","-d","+D"]}]});
        assert!(result_patch(&meta).unwrap().ends_with("@@ -3,2 +3,2 @@\n c\n-d\n+D\n"));
        assert!(result_patch(&json!({"type":"create","filePath":"n","content":"z","structuredPatch":[]})).unwrap().ends_with("+z\n"));
        assert_eq!(result_patch(&json!({"type":"text","file":{}})), None);
    }

    #[test]
    fn labels() {
        let cwd = Path::new("/w");
        assert_eq!(label(&tool("Read", json!({"file_path":"/w/src/a.rs"}), Some("x")), Some(cwd)), "Read src/a.rs");
        assert_eq!(label(&tool("Edit", json!({"file_path":"/w/a.rs"}), None), Some(cwd)), "Editing a.rs");
        assert_eq!(label(&tool("Bash", json!({"command":"cargo test\n--all"}), Some("")), None), "Ran cargo test …");
        assert_eq!(label(&tool("Bash", json!({"command":"ls","description":"List files"}), Some("")), None), "Ran List files");
        assert_eq!(label(&tool("mcp__tern__run_service", json!({}), Some("")), None), "tern: run service");
    }

    #[test]
    fn groups() {
        let a = tool("Read", json!({}), Some(""));
        let b = tool("Bash", json!({}), Some(""));
        let c = tool("Edit", json!({"file_path":"/a"}), Some(""));
        let d = tool("Edit", json!({"file_path":"/a"}), Some(""));
        let e = tool("Grep", json!({}), Some(""));
        assert_eq!(group(&[&a, &a]), "Read 2 files");
        assert_eq!(group(&[&a, &b, &c, &d]), "Ran 1 command, edited 1 file and 1 other action");
        assert_eq!(group(&[&a, &b, &c, &e]), "Ran 1 command, edited 1 file and 2 other actions");
    }

    #[test]
    fn durations() {
        assert_eq!(duration(820), "820ms");
        assert_eq!(duration(4210), "4.2s");
        assert_eq!(duration(42_000), "42s");
        assert_eq!(duration(185_000), "3m 5s");
        assert_eq!(duration(3_723_000), "1h 2m 3s");
    }

    #[test]
    fn details_cut_long_output() {
        let t = tool("Bash", json!({"command":"seq 5"}), Some("1\n2\n3\n4\n5"));
        assert_eq!(detail(&t, 3), "$ seq 5\n\n1\n2\n3\n… 2 more lines");
    }
}
