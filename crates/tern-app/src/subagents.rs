//! The sub-agents a Claude tab runs, and the finished task that last woke
//! it, from its hooks: Claude Code tells of a sub-agent as it starts
//! (`SubagentStart`, or an `Agent` call it sends to the background) and
//! stops (`SubagentStop`), lists the background ones still running on every
//! turn end (`background_tasks`), and wakes its main agent with a
//! `<task-notification>` prompt once one of them (or a background shell)
//! is done. Background shells themselves are processes, read from the
//! terminal (see `tern_term::procs::agent_shells`).

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::Value;

/// What a background task's `status` says once it is over; any other one
/// (or none) is still running.
const OVER: [&str; 19] = [
    "idle",
    "done",
    "success",
    "succeeded",
    "complete",
    "completed",
    "finished",
    "failed",
    "error",
    "terminated",
    "exited",
    "aborted",
    "expired",
    "skipped",
    "crashed",
    "killed",
    "cancelled",
    "canceled",
    "timed_out",
];

/// A sub-agent at work.
#[derive(Debug, Clone, PartialEq)]
pub struct Subagent {
    /// Claude Code's id for it (`agent_id`, the task id of a background one).
    pub id: String,
    /// What it does: its description, else its type.
    pub label: String,
    pub started: Instant,
    /// Sent to the background: it may outlive the turn that started it.
    pub background: bool,
    /// Its log (see `tern_bridge::agent_log`), when the hook named the
    /// session's.
    pub log: Option<PathBuf>,
}

/// A Claude tab's sub-agents at work, oldest first, and what woke its main
/// agent last.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Children {
    pub agents: Vec<Subagent>,
    /// The summary of the finished task whose notification started the
    /// turn now under way; `None` once a prompt of the user's starts one.
    pub woke: Option<String>,
}

impl Children {
    /// Take hook `raw` (its JSON body), at `now`; whether anything changed.
    pub fn on_hook(&mut self, raw: &Value, now: Instant) -> bool {
        let before = self.clone();
        let text = |key: &str| raw.get(key).and_then(Value::as_str).filter(|s| !s.is_empty());
        let child = text("agent_id");
        // Every hook names the main agent's log; a sub-agent's is beside it.
        let transcript = text("transcript_path").map(Path::new);
        match text("hook_event_name").unwrap_or_default() {
            "SubagentStart" => {
                if let Some(id) = child {
                    self.start(id, text("agent_type").unwrap_or("agent"), false, transcript, now);
                }
            },
            // The main agent sent a sub-agent to the background: its
            // description names it better than its type.
            "PostToolUse" if child.is_none() && text("tool_name") == Some("Agent") => {
                let response = raw.get("tool_response");
                let field = |key: &str| response.and_then(|r| r.get(key)).and_then(Value::as_str).filter(|s| !s.is_empty());
                let launched = response.and_then(|r| r.get("isAsync")).and_then(Value::as_bool) == Some(true);
                if launched && let Some(id) = field("agentId") {
                    let label = field("description").unwrap_or("agent");
                    match self.agents.iter_mut().find(|a| a.id == id) {
                        Some(a) => {
                            a.label = label.to_owned();
                            a.background = true;
                        },
                        None => self.start(id, label, true, transcript, now),
                    }
                }
            },
            "SubagentStop" => {
                self.reconcile(raw, false, transcript, now);
                if let Some(id) = child {
                    self.agents.retain(|a| a.id != id);
                }
            },
            // The main agent's turn ended: what it started in the
            // foreground ended with it.
            "Stop" | "StopFailure" if child.is_none() => self.reconcile(raw, true, transcript, now),
            "UserPromptSubmit" if child.is_none() => {
                let prompt = text("prompt").unwrap_or_default();
                match notification(prompt) {
                    Some((id, summary)) => {
                        if let Some(id) = id {
                            self.agents.retain(|a| a.id != id);
                        }
                        self.woke = Some(summary.unwrap_or("A background task finished").to_owned());
                    },
                    None => self.woke = None,
                }
            },
            _ => {},
        }
        *self != before
    }

    /// The background sub-agents still running, from the `background_tasks`
    /// a turn end lists (when it lists them): those missing are added, and
    /// with `ended` (the main agent's own turn end) those not listed go.
    fn reconcile(&mut self, raw: &Value, ended: bool, transcript: Option<&Path>, now: Instant) {
        let Some(tasks) = raw.get("background_tasks").and_then(Value::as_array) else { return };
        let running: Vec<(&str, &str)> = tasks
            .iter()
            .filter_map(|t| {
                let field = |key: &str| t.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
                let status = field("status").map(str::to_ascii_lowercase);
                if field("type") != Some("subagent") || status.is_some_and(|s| OVER.contains(&s.as_str())) {
                    return None;
                }
                Some((field("id")?, field("description").or(field("agent_type")).unwrap_or("agent")))
            })
            .collect();
        if ended {
            self.agents.retain(|a| running.iter().any(|(id, _)| *id == a.id));
        }
        for (id, label) in running {
            if !self.agents.iter().any(|a| a.id == id) {
                self.start(id, label, true, transcript, now);
            }
        }
    }

    fn start(&mut self, id: &str, label: &str, background: bool, transcript: Option<&Path>, now: Instant) {
        if !self.agents.iter().any(|a| a.id == id) {
            let log = transcript.map(|t| tern_bridge::agent_log::path(t, id));
            self.agents.push(Subagent { id: id.to_owned(), label: label.to_owned(), started: now, background, log });
        }
    }

    /// How many sub-agents run in the background.
    pub fn background(&self) -> usize {
        self.agents.iter().filter(|a| a.background).count()
    }
}

/// The task id and summary of a `<task-notification>` prompt, which Claude
/// Code sends its main agent when a background task is done; `None` for any
/// other prompt.
fn notification(prompt: &str) -> Option<(Option<&str>, Option<&str>)> {
    let body = prompt.trim_start().strip_prefix("<task-notification>")?;
    let tag = |name: &str| {
        let open = format!("<{name}>");
        let start = body.find(&open)? + open.len();
        let end = start + body[start..].find(&format!("</{name}>"))?;
        Some(body[start..end].trim()).filter(|s| !s.is_empty())
    };
    Some((tag("task-id"), tag("summary")))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn ids(c: &Children) -> Vec<&str> {
        c.agents.iter().map(|a| a.id.as_str()).collect()
    }

    /// What Claude Code 2.1 sent for a turn that sent a sub-agent and a
    /// shell to the background, then was woken by each as it finished.
    #[test]
    fn follows_a_background_sub_agent_to_its_notification() {
        let (mut c, now) = (Children::default(), Instant::now());
        let start = json!({"hook_event_name": "SubagentStart", "agent_id": "a7", "agent_type": "general-purpose",
            "transcript_path": "/p/proj/s1.jsonl"});
        assert!(c.on_hook(&start, now));
        assert_eq!(c.agents[0].label, "general-purpose");
        assert_eq!(c.agents[0].log.as_deref(), Some(Path::new("/p/proj/s1/subagents/agent-a7.jsonl")));
        let launched = json!({
            "hook_event_name": "PostToolUse", "tool_name": "Agent",
            "tool_response": {"isAsync": true, "status": "async_launched", "agentId": "a7", "description": "Run sleep 5"}
        });
        assert!(c.on_hook(&launched, now));
        assert_eq!((c.agents[0].label.as_str(), c.background()), ("Run sleep 5", 1));
        let tasks = json!([
            {"id": "a7", "type": "subagent", "status": "running", "description": "Run sleep 5", "agent_type": "general-purpose"},
            {"id": "b1", "type": "shell", "status": "running", "description": "Sleep 20", "command": "sleep 20"}
        ]);
        // The main agent's turn ends; the sub-agent runs on.
        assert!(!c.on_hook(&json!({"hook_event_name": "Stop", "background_tasks": tasks}), now));
        assert_eq!(ids(&c), ["a7"]);
        // Its own stop still lists it as running.
        assert!(c.on_hook(&json!({"hook_event_name": "SubagentStop", "agent_id": "a7", "background_tasks": tasks}), now));
        assert!(c.agents.is_empty());
        let woke = json!({
            "hook_event_name": "UserPromptSubmit",
            "prompt": "<task-notification>\n<task-id>b1</task-id>\n<status>completed</status>\n<summary>Background command \"Sleep 20\" completed (exit code 0)</summary>\n</task-notification>"
        });
        assert!(c.on_hook(&woke, now));
        assert_eq!(c.woke.as_deref(), Some("Background command \"Sleep 20\" completed (exit code 0)"));
        assert!(c.on_hook(&json!({"hook_event_name": "UserPromptSubmit", "prompt": "next"}), now));
        assert_eq!(c.woke, None);
    }

    #[test]
    fn a_turn_end_settles_what_it_lists() {
        let (mut c, now) = (Children::default(), Instant::now());
        c.on_hook(&json!({"hook_event_name": "SubagentStart", "agent_id": "fg", "agent_type": "Explore"}), now);
        // Started while no UI listened: the turn end names it.
        let tasks = json!([
            {"id": "bg", "type": "subagent", "status": "running", "description": "Review"},
            {"id": "old", "type": "subagent", "status": "completed"},
            {"id": "mate", "type": "teammate", "status": "running"}
        ]);
        c.on_hook(&json!({"hook_event_name": "Stop", "background_tasks": tasks}), now);
        assert_eq!(ids(&c), ["bg"]);
        assert_eq!((c.agents[0].label.as_str(), c.agents[0].background), ("Review", true));
        // A turn end that lists nothing (older Claude Code) changes nothing.
        assert!(!c.on_hook(&json!({"hook_event_name": "Stop"}), now));
        assert!(c.on_hook(&json!({"hook_event_name": "Stop", "background_tasks": []}), now));
        assert!(c.agents.is_empty());
    }

    #[test]
    fn a_sub_agents_own_tools_change_nothing() {
        let (mut c, now) = (Children::default(), Instant::now());
        let call = json!({"hook_event_name": "PostToolUse", "agent_id": "a1", "tool_name": "Agent",
            "tool_response": {"isAsync": true, "agentId": "a2"}});
        assert!(!c.on_hook(&call, now));
        assert!(!c.on_hook(&json!({"hook_event_name": "UserPromptSubmit", "agent_id": "a1", "prompt": "<task-notification>"}), now));
    }

    #[test]
    fn reads_notifications() {
        assert_eq!(notification("hello"), None);
        assert_eq!(notification("<task-notification><task-id>x</task-id></task-notification>"), Some((Some("x"), None)));
    }
}
