//! What an OpenCode tab's events mean. Tern's plugin forwards them as they
//! are (see `opencode_plugin.js`); [`OpenCodeFold`] turns them into the hook
//! events Claude Code's and Codex's tabs send, so the guard, the tracker and
//! the UI take them as they do theirs. A guard's answer goes back as a steer.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use crate::context::Reading;

/// The hook event name the plugin's posts come under.
pub const EVENT: &str = "OpenCode";
/// A permission or a form the agent waited on was settled: OpenCode tells, as
/// Claude's hooks don't.
pub const ANSWERED: &str = "PermissionAnswered";

fn text_at<'a>(v: &'a Value, pointer: &str) -> &'a str {
    v.pointer(pointer).and_then(Value::as_str).unwrap_or_default()
}

/// The name Tern's hook events know a tool by.
fn tool_name(name: &str) -> &str {
    match name {
        "shell" => "Bash",
        "edit" => "Edit",
        "write" => "Write",
        "read" => "Read",
        "glob" => "Glob",
        "grep" => "Grep",
        "webfetch" => "WebFetch",
        "websearch" => "WebSearch",
        "subagent" => "Task",
        other => other,
    }
}

/// A tool's input as Tern's hook events have it: its file under `file_path`.
fn tool_input(input: &Value) -> Value {
    let mut input = input.clone();
    if let Some(o) = input.as_object_mut()
        && let Some(path) = o.remove("filePath")
    {
        o.insert("file_path".into(), path);
    }
    input
}

/// How full the window is after a step: what went in, cached or not.
fn reading(raw: &Value) -> Option<Reading> {
    let tokens = raw.pointer("/data/tokens")?;
    let count = |p: &str| tokens.pointer(p).and_then(Value::as_u64).unwrap_or(0);
    let used = count("/input") + count("/cache/read") + count("/cache/write");
    let size = raw.get("window").and_then(Value::as_u64).filter(|s| *s > 0)?;
    Some(Reading { percent: (used as f64 * 100. / size as f64).clamp(0., 100.) as f32, size })
}

/// What one tab's events told so far. Not handed to a new daemon: it fills
/// again from the next events.
#[derive(Debug, Default)]
pub struct OpenCodeFold {
    /// The conversation the tab shows; others' events tell it nothing.
    root: Option<String>,
    /// The folder of the conversation.
    cwd: Option<String>,
    /// Child sessions at work: subagents.
    children: HashSet<String>,
    /// Tools at work by call id: their name, then their input.
    tools: HashMap<String, (String, Value)>,
    /// The latest text of the running turn.
    text: Option<String>,
    /// A steer kept the turn going: the next `Stop` says so.
    steered: bool,
}

impl OpenCodeFold {
    /// A hook payload `event` of the tab's conversation, with `fields`.
    fn payload(&self, event: &str, fields: Value) -> Value {
        let mut v = json!({ "hook_event_name": event, "session_id": self.root });
        if let Some(cwd) = &self.cwd {
            v["cwd"] = json!(cwd);
        }
        if let (Some(o), Some(fields)) = (v.as_object_mut(), fields.as_object()) {
            o.extend(fields.clone());
        }
        v
    }

    /// The tab goes on in conversation `session`, of `source`.
    fn switch(&mut self, session: &str, source: &str) -> Value {
        self.root = Some(session.to_owned());
        self.children.clear();
        self.tools.clear();
        (self.text, self.steered) = (None, false);
        self.payload("SessionStart", json!({ "source": source }))
    }

    /// The hook payloads post `raw` of the plugin's stands for.
    pub fn on_event(&mut self, raw: &Value) -> Vec<Value> {
        let kind = text_at(raw, "/type");
        let data = &raw["data"];
        let session = text_at(data, "/sessionID");
        if session.is_empty() {
            return Vec::new();
        }
        if raw.get("parent").is_some_and(|p| !p.is_null()) {
            return self.child(kind, session);
        }
        let mut out = Vec::new();
        let prompt = kind == "session.inbox.enqueued" && text_at(data, "/item/type") == "user";
        match self.root.as_deref() {
            Some(root) if root == session => {},
            // A new conversation, or one the user wrote in, is the tab's now.
            Some(_) if kind == "session.created" => {
                out.push(self.switch(session, "clear"));
                self.cwd = data.pointer("/location/directory").and_then(Value::as_str).map(str::to_owned).or(self.cwd.take());
                return out;
            },
            Some(_) if prompt => out.push(self.switch(session, "resume")),
            Some(_) => return out,
            None if kind == "session.created" => {
                self.cwd = data.pointer("/location/directory").and_then(Value::as_str).map(str::to_owned);
                out.push(self.switch(session, "startup"));
                return out;
            },
            None => out.push(self.switch(session, "resume")),
        }
        out.extend(self.root_event(kind, raw));
        out
    }

    /// What event `kind` of the tab's conversation stands for.
    fn root_event(&mut self, kind: &str, raw: &Value) -> Option<Value> {
        let data = &raw["data"];
        match kind {
            "session.inbox.enqueued" if text_at(data, "/item/type") == "user" => {
                Some(self.payload("UserPromptSubmit", json!({ "prompt": text_at(data, "/item/payload/text") })))
            },
            "session.execution.started" => {
                self.text = None;
                None
            },
            "session.tool.input.started" => {
                self.tools.insert(text_at(data, "/id").to_owned(), (text_at(data, "/name").to_owned(), Value::Null));
                None
            },
            "session.tool.called" => {
                let id = text_at(data, "/id");
                let tool = self.tools.entry(id.to_owned()).or_default();
                tool.1 = tool_input(&data["input"]);
                let fields = json!({ "tool_name": tool_name(&tool.0), "tool_input": tool.1, "tool_use_id": id });
                Some(self.payload("PreToolUse", fields))
            },
            "session.tool.success" | "session.tool.failed" => {
                let id = text_at(data, "/id");
                let (name, input) = self.tools.remove(id).unwrap_or_default();
                let event = if kind == "session.tool.success" { "PostToolUse" } else { "PostToolUseFailure" };
                Some(self.payload(event, json!({ "tool_name": tool_name(&name), "tool_input": input, "tool_use_id": id })))
            },
            "permission.asked" | "form.created" => Some(self.payload("PermissionRequest", json!({}))),
            "permission.replied" | "form.replied" | "form.cancelled" => Some(self.payload(ANSWERED, json!({}))),
            "session.text.ended" => {
                self.text = Some(text_at(data, "/text").to_owned());
                None
            },
            "session.execution.succeeded" => {
                let fields = json!({ "last_assistant_message": self.text, "stop_hook_active": std::mem::take(&mut self.steered) });
                Some(self.payload("Stop", fields))
            },
            "session.execution.failed" => Some(self.payload("StopFailure", json!({ "error": text_at(data, "/error/message") }))),
            "session.execution.interrupted" => Some(self.payload("Interrupt", json!({}))),
            "session.compaction.ended" => Some(self.payload("SessionStart", json!({ "source": "compact" }))),
            "session.step.ended" => {
                let reading = reading(raw)?;
                Some(crate::statusline::event_for(self.root.as_deref()?, reading))
            },
            _ => None,
        }
    }

    /// What event `kind` of child session `session` stands for: it shows as
    /// a subagent of the tab's conversation, and its asking waits on the tab.
    fn child(&mut self, kind: &str, session: &str) -> Vec<Value> {
        if self.root.is_none() {
            return Vec::new();
        }
        let fields = json!({ "agent_id": session, "agent_type": "subagent" });
        let mut out = Vec::new();
        let ended = matches!(kind, "session.execution.succeeded" | "session.execution.failed" | "session.execution.interrupted");
        if !ended && self.children.insert(session.to_owned()) {
            out.push(self.payload("SubagentStart", fields.clone()));
        }
        match kind {
            _ if ended => {
                if self.children.remove(session) {
                    out.push(self.payload("SubagentStop", fields));
                }
            },
            "permission.asked" | "form.created" => out.push(self.payload("PermissionRequest", json!({}))),
            "permission.replied" | "form.replied" | "form.cancelled" => out.push(self.payload(ANSWERED, json!({}))),
            _ => {},
        }
        out
    }

    /// What the plugin does for guard answer `reply` to conversation `session`.
    pub fn answer(&mut self, session: &str, reply: &Value) -> Option<Value> {
        if reply.get("decision").and_then(Value::as_str) == Some("block") {
            self.steered = true;
            return Some(json!({ "steer": { "sessionID": session, "text": text_at(reply, "/reason"), "resume": true } }));
        }
        let context = reply.pointer("/hookSpecificOutput/additionalContext")?.as_str()?;
        // A new conversation's comes with its first prompt, too late for that
        // turn: only a turn of its own is sure to carry it.
        let resume = text_at(reply, "/hookSpecificOutput/hookEventName") == "SessionStart";
        Some(json!({ "steer": { "sessionID": session, "text": context, "resume": resume } }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HookEvent;
    use crate::context::{Guard, Policy};

    fn ev(kind: &str, data: Value) -> Value {
        json!({"hook_event_name": EVENT, "type": kind, "data": data})
    }

    fn child(kind: &str, data: Value, parent: &str) -> Value {
        let mut e = ev(kind, data);
        e["parent"] = json!(parent);
        e
    }

    fn fold_all(text: &str) -> Vec<Value> {
        let mut f = OpenCodeFold::default();
        text.lines().flat_map(|l| f.on_event(&serde_json::from_str(l).unwrap())).collect()
    }

    fn names(events: &[Value]) -> Vec<&str> {
        events.iter().map(|v| v["hook_event_name"].as_str().unwrap()).collect()
    }

    fn started(f: &mut OpenCodeFold, session: &str) {
        f.on_event(&ev("session.created", json!({"sessionID": session})));
        f.on_event(&ev("session.inbox.enqueued", json!({"sessionID": session, "item": {"type": "user", "payload": {"text": "hi"}}})));
    }

    #[test]
    fn a_real_session_folds_to_hook_events() {
        let events = fold_all(include_str!("opencode_fold/fixtures/turn.jsonl"));
        let names = names(&events);
        assert_eq!(names.first(), Some(&"SessionStart"));
        for name in ["UserPromptSubmit", "PreToolUse", "PermissionRequest", "PermissionAnswered", "PostToolUse", "Stop", "Interrupt"] {
            assert!(names.contains(&name), "{name}: {names:?}");
        }
        assert!(events.iter().all(|e| e["session_id"] == "ses_ternspike0000000000000006"), "{events:?}");
    }

    #[test]
    fn a_new_session_starts_a_resumed_one_goes_on() {
        let fresh = OpenCodeFold::default().on_event(&ev("session.created", json!({"sessionID": "a"})));
        assert_eq!(fresh[0]["source"], "startup");
        let opened = OpenCodeFold::default().on_event(&ev("session.viewed", json!({"sessionID": "a"})));
        assert_eq!((opened[0]["hook_event_name"].as_str(), opened[0]["source"].as_str()), (Some("SessionStart"), Some("resume")));
    }

    #[test]
    fn a_session_after_another_is_a_clear() {
        let mut f = OpenCodeFold::default();
        started(&mut f, "a");
        assert_eq!(f.on_event(&ev("session.created", json!({"sessionID": "b"})))[0]["source"], "clear");
    }

    #[test]
    fn a_shell_tool_reads_as_bash_with_its_name_from_the_input_event() {
        let mut f = OpenCodeFold::default();
        started(&mut f, "s");
        f.on_event(&ev("session.tool.input.started", json!({"sessionID": "s", "id": "c", "name": "shell"})));
        let pre = f.on_event(&ev("session.tool.called", json!({"sessionID": "s", "id": "c", "input": {"command": "ls"}})));
        assert_eq!((&pre[0]["hook_event_name"], &pre[0]["tool_name"]), (&json!("PreToolUse"), &json!("Bash")));
        assert_eq!((&pre[0]["tool_input"]["command"], &pre[0]["tool_use_id"]), (&json!("ls"), &json!("c")));
    }

    #[test]
    fn an_edit_names_its_file() {
        let mut f = OpenCodeFold::default();
        started(&mut f, "s");
        f.on_event(&ev("session.tool.input.started", json!({"sessionID": "s", "id": "c", "name": "edit"})));
        f.on_event(&ev("session.tool.called", json!({"sessionID": "s", "id": "c", "input": {"filePath": "/w/a.rs"}})));
        let post = f.on_event(&ev("session.tool.success", json!({"sessionID": "s", "id": "c"})));
        let hook = HookEvent::parse("t".into(), post[0].to_string().as_bytes()).unwrap();
        assert_eq!((hook.event.as_str(), hook.paths), ("PostToolUse", vec![std::path::PathBuf::from("/w/a.rs")]));
    }

    #[test]
    fn a_childs_end_is_not_the_turns_end() {
        let mut f = OpenCodeFold::default();
        started(&mut f, "r");
        let first = f.on_event(&child("session.execution.started", json!({"sessionID": "c"}), "r"));
        assert_eq!(names(&first), ["SubagentStart"]);
        assert_eq!((&first[0]["session_id"], &first[0]["agent_id"]), (&json!("r"), &json!("c")));
        let tool = child("session.tool.called", json!({"sessionID": "c", "id": "x", "input": {}}), "r");
        assert!(f.on_event(&tool).is_empty());
        assert_eq!(names(&f.on_event(&child("session.execution.succeeded", json!({"sessionID": "c"}), "r"))), ["SubagentStop"]);
        assert_eq!(names(&f.on_event(&ev("session.execution.succeeded", json!({"sessionID": "r"})))), ["Stop"]);
    }

    #[test]
    fn a_childs_permission_waits_on_the_tab() {
        let mut f = OpenCodeFold::default();
        started(&mut f, "r");
        let asked = f.on_event(&child("permission.asked", json!({"sessionID": "c", "id": "p"}), "r"));
        assert!(names(&asked).contains(&"PermissionRequest"), "{asked:?}");
        assert_eq!(asked.last().unwrap()["session_id"], "r");
    }

    #[test]
    fn another_root_session_takes_the_tab_over() {
        let mut f = OpenCodeFold::default();
        started(&mut f, "a");
        let prompt = json!({"sessionID": "b", "item": {"type": "user", "payload": {"text": "go"}}});
        let got = f.on_event(&ev("session.inbox.enqueued", prompt));
        assert_eq!(names(&got), ["SessionStart", "UserPromptSubmit"]);
        assert_eq!(got[1]["session_id"], "b");
        assert!(f.on_event(&ev("session.execution.succeeded", json!({"sessionID": "a"}))).is_empty());
    }

    #[test]
    fn a_session_only_looked_at_doesnt_take_the_tab() {
        let mut f = OpenCodeFold::default();
        started(&mut f, "a");
        assert!(f.on_event(&ev("session.viewed", json!({"sessionID": "b"}))).is_empty());
    }

    #[test]
    fn a_synthetic_message_is_no_prompt_of_the_users() {
        let mut f = OpenCodeFold::default();
        started(&mut f, "s");
        let item = json!({"sessionID": "s", "item": {"type": "synthetic", "payload": {"text": "x"}}});
        assert!(f.on_event(&ev("session.inbox.enqueued", item)).is_empty());
    }

    #[test]
    fn the_stop_tells_the_last_text_of_its_turn() {
        let mut f = OpenCodeFold::default();
        started(&mut f, "s");
        f.on_event(&ev("session.execution.started", json!({"sessionID": "s"})));
        f.on_event(&ev("session.text.ended", json!({"sessionID": "s", "text": "first"})));
        f.on_event(&ev("session.text.ended", json!({"sessionID": "s", "text": "Handoff: a.md"})));
        let stop = f.on_event(&ev("session.execution.succeeded", json!({"sessionID": "s"})));
        assert_eq!(stop[0]["last_assistant_message"], "Handoff: a.md");
    }

    #[test]
    fn a_failed_turn_ends_as_a_failure() {
        let mut f = OpenCodeFold::default();
        started(&mut f, "s");
        assert_eq!(names(&f.on_event(&ev("session.execution.failed", json!({"sessionID": "s"})))), ["StopFailure"]);
    }

    #[test]
    fn the_reading_is_of_the_last_step_not_the_running_total() {
        let mut f = OpenCodeFold::default();
        started(&mut f, "s");
        let tokens = json!({"input": 600, "output": 20, "cache": {"read": 300, "write": 100}});
        let mut step = ev("session.step.ended", json!({"sessionID": "s", "tokens": tokens}));
        step["window"] = json!(10000);
        let line = f.on_event(&step);
        assert_eq!(line[0]["hook_event_name"], crate::statusline::EVENT);
        assert_eq!(line[0]["context_window"]["used_percentage"].as_f64(), Some(10.));
        assert_eq!(line[0]["context_window"]["context_window_size"], 10000);
        assert!(f.on_event(&ev("session.usage.updated", json!({"sessionID": "s", "tokens": tokens}))).is_empty());
    }

    #[test]
    fn a_step_with_no_window_tells_nothing() {
        let mut f = OpenCodeFold::default();
        started(&mut f, "s");
        assert!(f.on_event(&ev("session.step.ended", json!({"sessionID": "s", "tokens": {"input": 9}}))).is_empty());
    }

    #[test]
    fn compaction_resets_the_reading() {
        let mut f = OpenCodeFold::default();
        started(&mut f, "s");
        let policy = Policy { limit: 50, ..Policy::default() };
        let mut guard = Guard::default();
        guard.reading(80., policy);
        let line = &f.on_event(&ev("session.compaction.ended", json!({"sessionID": "s"})))[0];
        assert_eq!((&line["hook_event_name"], &line["source"]), (&json!("SessionStart"), &json!("compact")));
        let hook = HookEvent::parse("t".into(), line.to_string().as_bytes()).unwrap();
        guard.on_event(&hook, policy);
        assert_eq!(guard.percent(), 0.);
        let stop = &f.on_event(&ev("session.execution.succeeded", json!({"sessionID": "s"})))[0];
        let (reply, _) = guard.on_event(&HookEvent::parse("t".into(), stop.to_string().as_bytes()).unwrap(), policy);
        assert_eq!(reply, None);
    }

    #[test]
    fn a_block_answer_steers_and_the_next_stop_says_so() {
        let mut f = OpenCodeFold::default();
        started(&mut f, "s");
        let steer = f.answer("s", &json!({"decision": "block", "reason": "write a handoff"})).unwrap();
        assert_eq!(steer, json!({"steer": {"sessionID": "s", "text": "write a handoff", "resume": true}}));
        let stop = json!({"sessionID": "s"});
        assert_eq!(f.on_event(&ev("session.execution.succeeded", stop.clone()))[0]["stop_hook_active"], true);
        assert_eq!(f.on_event(&ev("session.execution.succeeded", stop))[0]["stop_hook_active"], false);
    }

    #[test]
    fn added_context_steers_without_opening_a_turn() {
        let mut f = OpenCodeFold::default();
        let reply = json!({"hookSpecificOutput": {"hookEventName": "PostToolUse", "additionalContext": "finish up"}});
        assert_eq!(f.answer("s", &reply).unwrap(), json!({"steer": {"sessionID": "s", "text": "finish up", "resume": false}}));
        assert_eq!(f.answer("s", &json!({})), None);
    }

    #[test]
    fn a_new_conversations_context_is_sure_to_be_read() {
        // It comes with the first prompt: only a turn of its own is sure to carry it.
        let mut f = OpenCodeFold::default();
        let reply = json!({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": "read the handoff"}});
        assert_eq!(f.answer("s", &reply).unwrap(), json!({"steer": {"sessionID": "s", "text": "read the handoff", "resume": true}}));
    }
}
