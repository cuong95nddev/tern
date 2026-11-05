//! Codex's chat: `codex app-server`, JSON-RPC lines on stdin and stdout.
//!
//! [`CodexDriver`] folds what the server says for the UI, [`CodexFollow`] is
//! what the daemon reads of the same lines. Both leave out other threads'
//! notifications: a sub-agent's, or those of the thread a handoff left
//! behind. Its requests they take, unanswered they would hold its turn.

use std::collections::HashMap;
use std::io;
use std::path::Path;

use serde_json::{Value, json};
use tern_bridge::context::Reading;

use super::rollout;
use crate::claude::launch;
use crate::driver::{ChatCaps, ChatDriver, ChatFollow, Said};
use crate::model::{Answer, Ask, Elicit, ElicitAnswer, Entry, Field, PermissionMode, Tool, Transcript, TurnEnd};

/// The id of the request that opens the conversation.
const OPEN: u64 = 1;
/// The JSON-RPC error for a method the client doesn't have.
const NO_METHOD: i64 = -32601;
/// How many characters of the first message title a notification.
const TITLE_CHARS: usize = 60;

/// What a resume refused by the thread's writer shows as.
const STILL_OPEN: &str = "This conversation is still open in a terminal: close it there, then send a message here.";

fn str_at<'a>(v: &'a Value, pointer: &str) -> &'a str {
    v.pointer(pointer).and_then(Value::as_str).unwrap_or_default()
}

/// A JSON-RPC id as requests are kept by: its text.
fn id_text(id: &Value) -> String {
    id.as_str().map_or_else(|| id.to_string(), str::to_owned)
}

/// The id `text` (see [`id_text`]) on the wire.
fn wire_id(text: &str) -> Value {
    text.parse::<u64>().map_or_else(|_| json!(text), |n| json!(n))
}

/// `line` tells of a thread other than `thread`. A request of any thread is
/// the user's to answer, and its settling goes with it.
fn elsewhere(line: &Value, thread: Option<&str>) -> bool {
    let settles = line.get("method").and_then(Value::as_str) == Some("serverRequest/resolved");
    let of = line.pointer("/params/threadId").and_then(Value::as_str).filter(|_| line.get("id").is_none() && !settles);
    matches!((of, thread), (Some(of), Some(thread)) if of != thread)
}

/// How full the window is, from a `thread/tokenUsage/updated`.
fn reading(line: &Value) -> Option<Reading> {
    let usage = line.pointer("/params/tokenUsage")?;
    let total = usage.pointer("/last/totalTokens")?.as_u64()?;
    let size = usage.get("modelContextWindow")?.as_u64().filter(|s| *s > 0)?;
    Some(Reading { percent: rollout::context_percent(total, size), size })
}

/// The words of a `userMessage` item.
fn user_text(item: &Value) -> String {
    let parts = item.get("content").and_then(Value::as_array).into_iter().flatten();
    parts.filter_map(|p| p.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n")
}

/// The line a `commandExecution` ran, without the shell around it:
/// `/bin/zsh -lc 'ls -a'` is `ls -a`.
fn command_line(item: &Value) -> String {
    let command = str_at(item, "/command");
    let line = command.split_once(" -lc ").map_or(command, |(_, line)| line);
    line.strip_prefix('\'').and_then(|l| l.strip_suffix('\'')).unwrap_or(line).to_owned()
}

/// The tools a started item shows as: a change to each file, or one call.
fn item_tools(item: &Value) -> Vec<Tool> {
    let id = str_at(item, "/id");
    let tool = |name: &str, input: Value| Tool { id: id.to_owned(), ..Tool::new(name.to_owned(), input) };
    match str_at(item, "/type") {
        "commandExecution" => vec![tool("Bash", json!({ "command": command_line(item) }))],
        "fileChange" => {
            let changes = item.get("changes").and_then(Value::as_array).into_iter().flatten();
            changes
                .enumerate()
                .map(|(i, c)| {
                    let (path, diff) = (str_at(c, "/path"), str_at(c, "/diff"));
                    let added = str_at(c, "/kind/type") == "add";
                    let mut t = tool(if added { "Write" } else { "Edit" }, json!({ "file_path": path }));
                    t.id = format!("{id}:{i}");
                    if added {
                        t.input["content"] = json!(diff);
                    } else {
                        t.patch = Some(diff.to_owned());
                    }
                    t
                })
                .collect()
        },
        "mcpToolCall" => {
            let name = format!("mcp__{}__{}", str_at(item, "/server"), str_at(item, "/tool"));
            vec![tool(&name, item.get("arguments").cloned().unwrap_or_else(|| json!({})))]
        },
        "webSearch" => vec![tool("WebSearch", json!({ "query": str_at(item, "/query") }))],
        _ => Vec::new(),
    }
}

/// What a finished item's tools show as their result, and whether it failed.
fn item_result(item: &Value) -> (String, bool) {
    let failed = !matches!(str_at(item, "/status"), "" | "completed");
    match str_at(item, "/type") {
        "commandExecution" => {
            let code = item.get("exitCode").and_then(Value::as_i64).unwrap_or(0);
            (str_at(item, "/aggregatedOutput").to_owned(), failed || code != 0)
        },
        "mcpToolCall" => {
            let result = item.get("result").filter(|r| !r.is_null()).map(Value::to_string);
            (result.unwrap_or_else(|| str_at(item, "/error/message").to_owned()), failed)
        },
        _ => (String::new(), failed),
    }
}

/// The line that opens conversation `thread`.
fn resume_line(thread: &str) -> String {
    json!({ "id": OPEN, "method": "thread/resume", "params": { "threadId": thread } }).to_string()
}

fn request(id: &str, method: &str, params: Value) -> String {
    json!({ "id": id, "method": method, "params": params }).to_string()
}

/// `codex app-server`.
#[derive(Debug, Default)]
pub struct CodexDriver {
    /// The conversation, once the server said.
    thread: Option<String>,
    /// The running turn.
    turn: Option<String>,
    /// The conversation was asked for and isn't open yet.
    opening: bool,
    /// The conversation to resume, until it has opened: a refused resume is
    /// asked again with the next message.
    resuming: Option<String>,
    /// What the user sent meanwhile.
    queued: Vec<String>,
    /// Messages sent that the server is yet to echo: already shown.
    sent: Vec<String>,
    /// The entries of the running turn's items, by item id.
    items: HashMap<String, Vec<usize>>,
    /// The tools of other threads' running items, by item id: shown once one
    /// asks for approval, for the user to see what they are allowing.
    others: HashMap<String, Vec<Tool>>,
}

impl CodexDriver {
    /// What `text` goes in as: it steers a running turn, or starts one.
    fn turn_line(&self, thread: &str, text: &str) -> (String, String) {
        let id = crate::new_session_id();
        let method = if self.turn.is_some() { "turn/steer" } else { "turn/start" };
        let line = request(&id, method, json!({ "threadId": thread, "input": [{ "type": "text", "text": text }] }));
        (id, line)
    }

    fn item_started(&mut self, t: &mut Transcript, item: &Value) {
        let id = str_at(item, "/id");
        if self.items.contains_key(id) {
            return;
        }
        let tools = item_tools(item);
        if tools.is_empty() {
            return;
        }
        let first = t.entries.len();
        t.entries.extend(tools.into_iter().map(Entry::Tool));
        self.items.insert(id.to_owned(), (first..t.entries.len()).collect());
    }

    /// A line of another thread: only what its requests will need is kept.
    fn other_thread(&mut self, t: &mut Transcript, v: &Value) {
        let item = &v["params"]["item"];
        let id = str_at(item, "/id");
        match v.get("method").and_then(Value::as_str) {
            Some("item/started") => {
                let tools = item_tools(item);
                if !tools.is_empty() {
                    self.others.insert(id.to_owned(), tools);
                }
            },
            Some("item/completed") if self.others.remove(id).is_none() && self.items.contains_key(id) => self.item_completed(t, item),
            _ => {},
        }
    }

    /// Show the tools of another thread's item, now that it asks.
    fn show_other(&mut self, t: &mut Transcript, item: &str) {
        if let Some(tools) = self.others.remove(item) {
            let first = t.entries.len();
            t.entries.extend(tools.into_iter().map(Entry::Tool));
            self.items.insert(item.to_owned(), (first..t.entries.len()).collect());
        }
    }

    fn item_completed(&mut self, t: &mut Transcript, item: &Value) {
        let id = str_at(item, "/id");
        match str_at(item, "/type") {
            "agentMessage" => {
                let text = str_at(item, "/text").to_owned();
                match self.items.remove(id).and_then(|ix| ix.first().copied()).and_then(|i| t.entries.get_mut(i)) {
                    Some(Entry::Text { text: shown, streaming }) => (*shown, *streaming) = (text, false),
                    _ => t.entries.push(Entry::Text { text, streaming: false }),
                }
            },
            "userMessage" => {
                let text = user_text(item);
                match self.sent.iter().position(|s| *s == text) {
                    Some(i) => drop(self.sent.remove(i)),
                    None => t.entries.push(Entry::User { text, images: Vec::new() }),
                }
            },
            "contextCompaction" => t.entries.push(Entry::Notice { text: "Conversation compacted".into(), error: false }),
            _ => {
                self.item_started(t, item);
                let (result, error) = item_result(item);
                for i in self.items.remove(id).into_iter().flatten() {
                    if let Some(Entry::Tool(tool)) = t.entries.get_mut(i) {
                        (tool.result, tool.error) = (Some(result.clone()), error);
                    }
                }
            },
        }
    }

    fn delta(&mut self, t: &mut Transcript, params: &Value) {
        let (id, piece) = (str_at(params, "/itemId"), str_at(params, "/delta"));
        match self.items.get(id).and_then(|ix| ix.first()).and_then(|&i| t.entries.get_mut(i)) {
            Some(Entry::Text { text, .. }) => text.push_str(piece),
            _ => {
                t.entries.push(Entry::Text { text: piece.to_owned(), streaming: true });
                self.items.insert(id.to_owned(), vec![t.entries.len() - 1]);
            },
        }
    }

    fn turn_completed(&mut self, t: &mut Transcript, turn: &Value) {
        let status = str_at(turn, "/status");
        // An item the turn left behind, as an interrupted command is.
        for i in std::mem::take(&mut self.items).into_values().flatten() {
            if let Some(Entry::Tool(tool)) = t.entries.get_mut(i)
                && tool.result.is_none()
            {
                (tool.result, tool.error) = (Some(String::new()), true);
            }
        }
        t.settle();
        let error = (status == "failed").then(|| str_at(turn, "/error/message").to_owned());
        let ms = turn.get("durationMs").and_then(Value::as_u64).unwrap_or(0);
        t.entries.push(Entry::TurnEnd(TurnEnd { ms, interrupted: status == "interrupted", error }));
        t.busy = false;
        self.turn = None;
    }

    fn notification(&mut self, t: &mut Transcript, v: &Value, method: &str) {
        let params = &v["params"];
        match method {
            "turn/started" => {
                t.busy = true;
                self.turn = Some(str_at(params, "/turn/id").to_owned());
            },
            "turn/completed" => self.turn_completed(t, &params["turn"]),
            "item/started" => self.item_started(t, &params["item"]),
            "item/completed" => self.item_completed(t, &params["item"]),
            "item/agentMessage/delta" => self.delta(t, params),
            "thread/tokenUsage/updated" => t.context = reading(v).or(t.context),
            "serverRequest/resolved" => {
                let id = params.get("requestId").map(id_text).unwrap_or_default();
                // Unanswered here: the server took it back.
                if t.waiting() {
                    t.set_answer(&id, Answer::Withdrawn);
                }
            },
            "error" if params.get("willRetry").and_then(Value::as_bool) != Some(true) => {
                t.entries.push(Entry::Notice { text: str_at(params, "/error/message").to_owned(), error: true });
            },
            _ => {},
        }
    }

    /// A request of the server's: what to write back, when it isn't the user's to answer.
    fn server_request(&mut self, t: &mut Transcript, v: &Value, method: &str) -> Option<String> {
        let (id, params) = (id_text(&v["id"]), &v["params"]);
        match method {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                let item = str_at(params, "/itemId");
                self.show_other(t, item);
                let tool = self.items.get(item).and_then(|ix| ix.first()).and_then(|&i| t.entries.get(i));
                let (name, input) = match tool {
                    Some(Entry::Tool(tool)) => (tool.name.clone(), tool.input.clone()),
                    _ if method.contains("commandExecution") => ("Bash".to_owned(), json!({ "command": str_at(params, "/command") })),
                    // Nothing says what would change: not for the user to allow blind.
                    _ => {
                        t.entries.push(Entry::Notice { text: "Declined a file change that came without its details".into(), error: true });
                        return Some(json!({ "id": v["id"], "result": { "decision": "decline" } }).to_string());
                    },
                };
                let ask = Ask {
                    request_id: id,
                    tool_name: name,
                    tool_use_id: Some(item.to_owned()),
                    input,
                    suggestions: Value::Null,
                    answer: None,
                };
                t.entries.push(Entry::Ask(ask));
                None
            },
            "mcpServer/elicitation/request" => {
                let schema = params.get("requestedSchema");
                let required: Vec<&str> = schema
                    .and_then(|s| s.get("required"))
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                let props = schema.and_then(|s| s.get("properties")).and_then(Value::as_object);
                let fields = props.into_iter().flatten().map(|(n, p)| Field::parse(n, p, required.contains(&n.as_str()))).collect();
                let url = (str_at(params, "/mode") == "url").then(|| str_at(params, "/url").to_owned()).filter(|u| !u.is_empty());
                let (server, message) = (str_at(params, "/serverName").to_owned(), str_at(params, "/message").to_owned());
                t.entries.push(Entry::Elicit(Elicit { request_id: id, server, message, url, fields, answer: None }));
                None
            },
            other => {
                t.entries.push(Entry::Notice { text: format!("A request this chat can't answer: “{other}”"), error: false });
                Some(
                    json!({ "id": v["id"], "error": { "code": NO_METHOD, "message": format!("unsupported request {other}") } }).to_string(),
                )
            },
        }
    }

    /// An answer: to a request of ours, or one of the user's, replayed.
    fn response(&mut self, t: &mut Transcript, v: &Value) -> Option<String> {
        if let Some(error) = v.get("error") {
            let opening = std::mem::take(&mut self.opening);
            let message = str_at(error, "/message");
            let refused = opening && message.contains("active writer");
            t.entries.push(Entry::Notice { text: if refused { STILL_OPEN.to_owned() } else { message.to_owned() }, error: true });
            t.busy = false;
            return None;
        }
        let result = v.get("result")?;
        if let Some(thread) = result.pointer("/thread/id").and_then(Value::as_str) {
            // A later one is the fresh conversation a handoff goes on in.
            if self.thread.as_deref().is_some_and(|old| old != thread) {
                t.handoff = Some(None);
                t.context = None;
            }
            self.thread = Some(thread.to_owned());
            t.session_id = Some(thread.to_owned());
            self.opening = false;
            self.resuming = None;
            let queued = std::mem::take(&mut self.queued);
            return (!queued.is_empty()).then(|| self.turn_line(thread, &queued.join("\n\n")).1);
        }
        let id = id_text(&v["id"]);
        match (str_at(result, "/decision"), str_at(result, "/action")) {
            ("accept", _) => t.set_answer(&id, Answer::Allow),
            ("acceptForSession", _) => t.set_answer(&id, Answer::AllowSession),
            ("decline", _) => t.set_answer(&id, Answer::Deny),
            (_, "accept") => t.set_elicit_answer(&id, ElicitAnswer::Accept(result.get("content").cloned().unwrap_or(Value::Null))),
            (_, "decline") => t.set_elicit_answer(&id, ElicitAnswer::Decline),
            _ => {},
        }
        None
    }
}

impl ChatDriver for CodexDriver {
    fn agent(&self) -> &'static str {
        tern_agent::CODEX
    }

    fn caps(&self) -> ChatCaps {
        ChatCaps { permission_modes: false, models: false, elicitation: true, images: false, interrupt: true }
    }

    fn command(&self, user_args: &str, _: &str, _: bool, _: Option<&str>, _: Option<PermissionMode>, quote: fn(&str) -> String) -> String {
        // Its descriptor's command and args: a user file may change them.
        let registry = tern_agent::registry();
        let mut line = registry.get(tern_agent::CODEX).map_or_else(|| tern_agent::CODEX.to_owned(), |d| launch::start(d, quote));
        if !user_args.is_empty() {
            line.push(' ');
            line.push_str(user_args);
        }
        line.push_str(" {tern-settings} app-server");
        line
    }

    fn start_lines(&mut self, session: &str, resume: bool) -> Vec<String> {
        self.opening = true;
        let open = if resume {
            self.resuming = Some(session.to_owned());
            resume_line(session)
        } else {
            // It runs in the chat's folder.
            json!({ "id": OPEN, "method": "thread/start", "params": {} }).to_string()
        };
        let hello = json!({ "id": 0, "method": "initialize", "params": { "clientInfo": { "name": "tern", "version": env!("CARGO_PKG_VERSION") } } });
        vec![hello.to_string(), json!({ "method": "initialized" }).to_string(), open]
    }

    fn fold_line(&mut self, t: &mut Transcript, line: &str) -> Option<String> {
        let v: Value = serde_json::from_str(line).ok()?;
        if elsewhere(&v, self.thread.as_deref()) {
            self.other_thread(t, &v);
            return None;
        }
        let reply = match (v.get("method").and_then(Value::as_str), v.get("id")) {
            (Some(method), Some(_)) => self.server_request(t, &v, method),
            (Some(method), None) => {
                self.notification(t, &v, method);
                None
            },
            (None, _) => self.response(t, &v),
        };
        t.version += 1;
        reply
    }

    fn user_line(&mut self, session: &str, text: &str, _: &[(&str, &[u8])]) -> (String, String) {
        self.sent.push(text.to_owned());
        if self.opening || self.resuming.is_some() {
            self.queued.push(text.to_owned());
            let line = self.resuming.as_deref().filter(|_| !self.opening).map(resume_line).unwrap_or_default();
            self.opening = true;
            return (crate::new_session_id(), line);
        }
        self.turn_line(self.thread.as_deref().unwrap_or(session), text)
    }

    fn clear_line(&mut self, _: &str) -> String {
        request(&crate::new_session_id(), "thread/start", json!({ "sessionStartSource": "clear" }))
    }

    fn answer_line(&self, ask: &Ask, answer: &Answer) -> Option<String> {
        let decision = match answer {
            Answer::Allow => "accept",
            Answer::AllowSession => "acceptForSession",
            Answer::Deny => "decline",
            Answer::Answered(_) | Answer::Withdrawn => return None,
        };
        Some(json!({ "id": wire_id(&ask.request_id), "result": { "decision": decision } }).to_string())
    }

    fn elicit_line(&self, e: &Elicit, answer: &ElicitAnswer) -> Option<String> {
        let result = match answer {
            ElicitAnswer::Accept(content) => json!({ "action": "accept", "content": content }),
            ElicitAnswer::Decline | ElicitAnswer::Withdrawn => json!({ "action": "decline" }),
        };
        Some(json!({ "id": wire_id(&e.request_id), "result": result }).to_string())
    }

    fn interrupt_line(&self) -> Option<String> {
        let (thread, turn) = (self.thread.as_ref()?, self.turn.as_ref()?);
        Some(request(&crate::new_session_id(), "turn/interrupt", json!({ "threadId": thread, "turnId": turn })))
    }

    fn mode_line(&self, _: PermissionMode) -> Option<String> {
        None
    }

    fn model_line(&self, _: &str) -> Option<String> {
        None
    }

    fn reset(&mut self) {
        *self = Self::default();
    }

    fn load(&mut self, t: &mut Transcript, path: &Path, before: Option<u64>, _: Vec<(u64, Value)>) -> io::Result<()> {
        let text = std::fs::read_to_string(path)?;
        *t = Transcript { entries: rollout::history(&text, before), context: rollout::context_reading(path), ..Default::default() };
        Ok(())
    }

    fn ended(&mut self, t: &mut Transcript, why: Option<String>) {
        self.turn = None;
        t.ended(why);
    }
}

/// What the daemon reads of a Codex chat.
#[derive(Debug, Default)]
pub struct CodexFollow {
    thread: Option<String>,
}

impl ChatFollow for CodexFollow {
    fn follow(&mut self, v: &Value, out: &mut Vec<Said>) {
        if elsewhere(v, self.thread.as_deref()) {
            return;
        }
        let Some(method) = v.get("method").and_then(Value::as_str) else {
            if v.get("error").is_some() {
                out.push(Said::TurnEnded);
            } else if let Some(thread) = v.pointer("/result/thread/id").and_then(Value::as_str) {
                self.thread = Some(thread.to_owned());
                out.push(Said::Session(thread.to_owned()));
            }
            return;
        };
        match method {
            "turn/started" => out.push(Said::TurnStarted),
            "turn/completed" => out.push(Said::TurnEnded),
            "thread/tokenUsage/updated" => out.extend(reading(v).map(Said::Reading)),
            "serverRequest/resolved" => out.push(Said::Settled(v.pointer("/params/requestId").map(id_text).unwrap_or_default())),
            "item/completed" if str_at(v, "/params/item/type") == "userMessage" => {
                let text = user_text(&v["params"]["item"]);
                let line = text.lines().next().unwrap_or_default().trim();
                if !line.is_empty() {
                    out.push(Said::Title(line.chars().take(TITLE_CHARS).collect()));
                }
            },
            m if m.ends_with("Delta") || m.ends_with("/delta") => out.push(Said::Streamed),
            m if v.get("id").is_some() && (m.ends_with("/requestApproval") || m == "mcpServer/elicitation/request") => {
                out.push(Said::Ask(id_text(&v["id"])));
            },
            _ => {},
        }
    }

    fn wrote(&mut self, v: &Value, out: &mut Vec<Said>) {
        match v.get("method").and_then(Value::as_str) {
            Some("turn/start") => out.push(Said::TurnStarted),
            None if v.get("result").is_some() => out.push(Said::Settled(v.get("id").map(id_text).unwrap_or_default())),
            _ => {},
        }
    }

    fn notice(&self, v: &Value, agent: &str) -> Option<String> {
        match v.get("method")?.as_str()? {
            "item/commandExecution/requestApproval" => Some(format!("{agent} asks to run a command")),
            "item/fileChange/requestApproval" => Some(format!("{agent} asks to change files")),
            "mcpServer/elicitation/request" => Some(format!("{} asks for your input", str_at(v, "/params/serverName"))),
            "turn/completed" => match str_at(v, "/params/turn/status") {
                "interrupted" => None,
                "completed" => Some(format!("{agent} finished")),
                _ => Some(format!("{agent} stopped with an error")),
            },
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::driver::{ChatDriver, ChatFollow, Said};
    use crate::model::{Answer, Entry, Transcript};

    const THREAD: &str = "01a1aaef-8567-79c1-bb1a-67c648ca8aff";
    const SECOND_TURN: &str = "01a1aaef-a8f8-7dd3-9e8d-37b785833b90";

    /// The recording: `{"c": line}` the client wrote, `{"s": line}` the server did.
    fn recording() -> Vec<(bool, Value)> {
        include_str!("fixtures/app_server.jsonl")
            .lines()
            .map(|l| {
                let v: Value = serde_json::from_str(l).unwrap();
                match (v.get("c"), v.get("s")) {
                    (Some(c), _) => (true, c.clone()),
                    (_, Some(s)) => (false, s.clone()),
                    _ => panic!("{l}"),
                }
            })
            .collect()
    }

    /// The server's lines, up to the `n`th `turn/completed` (all with 0).
    fn server_lines(turns: usize) -> Vec<String> {
        let mut seen = 0;
        let mut lines = Vec::new();
        for (client, v) in recording() {
            if client {
                continue;
            }
            lines.push(v.to_string());
            seen += usize::from(v["method"] == "turn/completed");
            if turns > 0 && seen == turns {
                break;
            }
        }
        lines
    }

    fn fed(turns: usize) -> (CodexDriver, Transcript) {
        let (mut d, mut t) = (CodexDriver::default(), Transcript::default());
        for line in server_lines(turns) {
            d.fold_line(&mut t, &line);
        }
        (d, t)
    }

    fn tools(t: &Transcript) -> Vec<&crate::Tool> {
        t.entries.iter().filter_map(|e| if let Entry::Tool(t) = e { Some(t) } else { None }).collect()
    }

    #[test]
    fn a_new_codex_chat_learns_its_thread() {
        let (d, t) = fed(1);
        assert_eq!(t.session_id.as_deref(), Some(THREAD));
        assert_eq!(d.thread.as_deref(), Some(THREAD));
    }

    #[test]
    fn a_message_during_a_turn_steers_it() {
        let (mut d, mut t) = (CodexDriver::default(), Transcript::default());
        let lines = server_lines(1);
        let started = lines.iter().position(|l| l.contains("turn/started")).unwrap();
        for line in &lines[..=started] {
            d.fold_line(&mut t, line);
        }
        let (_, line) = d.user_line(THREAD, "also this", &[]);
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["method"], "turn/steer");
    }

    #[test]
    fn a_line_of_another_thread_is_left_out() {
        let (mut d, mut t) = fed(1);
        let turn = json!({"method": "turn/started", "params": {"threadId": "sub", "turn": {"id": "x"}}}).to_string();
        d.fold_line(&mut t, &turn);
        assert!(!t.busy);
        let mut f = CodexFollow::default();
        let mut out = Vec::new();
        f.follow(&json!({"id": 1, "result": {"thread": {"id": "main"}}}), &mut out);
        out.clear();
        f.follow(&serde_json::from_str(&turn).unwrap(), &mut out);
        assert_eq!(out, []);
    }

    #[test]
    fn a_new_chat_starts_a_thread_and_an_old_one_resumes_it() {
        let mut d = CodexDriver::default();
        let start: Vec<Value> = d.start_lines("x", false).iter().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(start[0]["method"], "initialize");
        assert_eq!(start[1]["method"], "initialized");
        assert_eq!(start[2]["method"], "thread/start");
        let mut d = CodexDriver::default();
        let resume: Value = serde_json::from_str(&d.start_lines("old", true)[2]).unwrap();
        assert_eq!((&resume["method"], &resume["params"]["threadId"]), (&json!("thread/resume"), &json!("old")));
    }

    #[test]
    fn a_message_sent_before_the_thread_opens_goes_in_once_it_has() {
        let (mut d, mut t) = (CodexDriver::default(), Transcript::default());
        d.start_lines("x", false);
        let (_, line) = d.user_line("x", "hi", &[]);
        assert_eq!(line, "", "nothing to write yet");
        let lines = server_lines(1);
        let opened = lines.iter().find(|l| l.contains("\"thread\"")).unwrap();
        let reply: Value = serde_json::from_str(&d.fold_line(&mut t, opened).unwrap()).unwrap();
        assert_eq!(reply["method"], "turn/start");
        assert_eq!(reply["params"]["threadId"], THREAD);
        assert_eq!(reply["params"]["input"][0]["text"], "hi");
    }

    #[test]
    fn codex_text_streams_then_settles() {
        let (mut d, mut t) = (CodexDriver::default(), Transcript::default());
        let lines = server_lines(1);
        let first = lines.iter().position(|l| l.contains("item/agentMessage/delta")).unwrap();
        for line in &lines[..=first] {
            d.fold_line(&mut t, line);
        }
        assert!(matches!(t.entries.last(), Some(Entry::Text { streaming: true, .. })), "{:?}", t.entries.last());
        for line in &lines[first + 1..] {
            d.fold_line(&mut t, line);
        }
        let texts: Vec<_> = t
            .entries
            .iter()
            .filter_map(|e| if let Entry::Text { text, streaming } = e { Some((text.as_str(), *streaming)) } else { None })
            .collect();
        assert_eq!(texts.last(), Some(&("Done.", false)));
        assert!(texts.iter().all(|(_, streaming)| !streaming));
    }

    #[test]
    fn codex_command_is_a_tool_with_its_output() {
        let (_, t) = fed(1);
        let tools = tools(&t);
        assert_eq!((tools[0].name.as_str(), &tools[0].input["command"]), ("Bash", &json!("ls")));
        assert_eq!(tools[0].result.as_deref(), Some("a.txt\n"));
        assert!(!tools[0].error);
    }

    #[test]
    fn codex_file_change_is_a_tool_per_file() {
        let (_, t) = fed(1);
        let tools = tools(&t);
        assert_eq!((tools[1].name.as_str(), &tools[1].input["file_path"]), ("Write", &json!("/w/b.txt")));
        assert!(tools[1].done());
    }

    #[test]
    fn codex_approval_is_an_ask_and_answers_by_id() {
        let rec = recording();
        let (mut d, mut t) = (CodexDriver::default(), Transcript::default());
        for (client, v) in &rec {
            if !client {
                d.fold_line(&mut t, &v.to_string());
            }
            if v["method"] == "item/fileChange/requestApproval" {
                break;
            }
        }
        let Some(Entry::Ask(ask)) = t.entries.last().cloned() else { panic!("{:?}", t.entries.last()) };
        assert_eq!((ask.request_id.as_str(), ask.tool_name.as_str()), ("0", "Write"));
        assert_eq!(ask.input["file_path"], "/w/b.txt");
        let recorded = rec.iter().find(|(client, v)| *client && v.get("result").is_some()).unwrap();
        let line: Value = serde_json::from_str(&d.answer_line(&ask, &Answer::Allow).unwrap()).unwrap();
        assert_eq!(line, recorded.1);
        let session: Value = serde_json::from_str(&d.answer_line(&ask, &Answer::AllowSession).unwrap()).unwrap();
        assert_eq!(session["result"]["decision"], "acceptForSession");
        let deny: Value = serde_json::from_str(&d.answer_line(&ask, &Answer::Deny).unwrap()).unwrap();
        assert_eq!(deny["result"]["decision"], "decline");
    }

    #[test]
    fn an_answer_replayed_settles_its_ask() {
        let (mut d, mut t) = (CodexDriver::default(), Transcript::default());
        for (client, v) in recording() {
            if v["id"] == json!(0) && client && v.get("result").is_some() {
                d.fold_line(&mut t, &v.to_string());
                break;
            }
            if !client {
                d.fold_line(&mut t, &v.to_string());
            }
        }
        assert!(matches!(t.entries.last(), Some(Entry::Ask(a)) if a.answer == Some(Answer::Allow)), "{:?}", t.entries.last());
    }

    #[test]
    fn a_turn_ends_with_how_it_ended() {
        let (_, t) = fed(2);
        let ends: Vec<_> =
            t.entries.iter().filter_map(|e| if let Entry::TurnEnd(e) = e { Some((e.ms, e.interrupted)) } else { None }).collect();
        assert_eq!(ends, [(9038, false), (3828, true)]);
        assert!(!t.busy);
    }

    #[test]
    fn an_interrupted_command_doesnt_run_on() {
        let (_, t) = fed(2);
        assert!(tools(&t).iter().all(|t| t.done()), "{:?}", tools(&t));
    }

    #[test]
    fn codex_interrupt_names_the_running_turn() {
        let (mut d, mut t) = (CodexDriver::default(), Transcript::default());
        assert_eq!(d.interrupt_line(), None);
        let lines = server_lines(2);
        let second = lines.iter().position(|l| l.contains("turn/started") && l.contains(SECOND_TURN)).unwrap();
        for line in &lines[..=second] {
            d.fold_line(&mut t, line);
        }
        assert!(t.busy);
        let v: Value = serde_json::from_str(&d.interrupt_line().unwrap()).unwrap();
        assert_eq!(v["method"], "turn/interrupt");
        assert_eq!(v["params"], json!({"threadId": THREAD, "turnId": SECOND_TURN}));
    }

    #[test]
    fn codex_context_reading_from_token_usage() {
        let (_, t) = fed(1);
        let reading = t.context.unwrap();
        assert_eq!(reading.size, 258_400);
        assert!(reading.percent > 0. && reading.percent < 100.);
    }

    #[test]
    fn resume_refused_is_a_notice() {
        let (mut d, mut t) = (CodexDriver::default(), Transcript::default());
        d.start_lines(THREAD, true);
        t.busy = true;
        let refused = server_lines(0).pop().unwrap();
        assert!(refused.contains("-32600"), "{refused}");
        assert_eq!(d.fold_line(&mut t, &refused), None);
        assert!(matches!(t.entries.last(), Some(Entry::Notice { text, error: true }) if text.contains("still open")), "{:?}", t.entries);
        assert!(!t.busy);
    }

    #[test]
    fn a_message_after_a_refused_resume_resumes_first() {
        let (mut d, mut t) = (CodexDriver::default(), Transcript::default());
        d.start_lines(THREAD, true);
        d.fold_line(&mut t, &server_lines(0).pop().unwrap());
        let (_, line) = d.user_line(THREAD, "hi", &[]);
        let again: Value = serde_json::from_str(&line).unwrap();
        assert_eq!((&again["method"], &again["params"]["threadId"]), (&json!("thread/resume"), &json!(THREAD)));
        let opened = json!({"id": 1, "result": {"thread": {"id": THREAD}}}).to_string();
        let reply: Value = serde_json::from_str(&d.fold_line(&mut t, &opened).unwrap()).unwrap();
        assert_eq!((&reply["method"], &reply["params"]["input"][0]["text"]), (&json!("turn/start"), &json!("hi")));
    }

    #[test]
    fn a_request_of_another_thread_is_still_asked() {
        let (mut d, mut t) = fed(1);
        let line = json!({"id": 9, "method": "item/commandExecution/requestApproval",
            "params": {"threadId": "sub", "itemId": "i", "command": "ls"}});
        d.fold_line(&mut t, &line.to_string());
        assert!(matches!(t.entries.last(), Some(Entry::Ask(a)) if a.request_id == "9"), "{:?}", t.entries.last());
        let (mut f, mut out) = (CodexFollow::default(), Vec::new());
        f.follow(&json!({"id": 1, "result": {"thread": {"id": "main"}}}), &mut out);
        out.clear();
        f.follow(&line, &mut out);
        assert_eq!(out, [Said::Ask("9".into())]);
    }

    fn sub_change(id: &str) -> Value {
        json!({"id": id, "type": "fileChange", "changes": [
            {"path": "/w/a.txt", "kind": {"type": "update"}, "diff": "-a\n+b\n"},
            {"path": "/w/b.txt", "kind": {"type": "add"}, "diff": "new\n"},
        ]})
    }

    #[test]
    fn a_change_another_thread_asks_for_shows_every_file() {
        let (mut d, mut t) = fed(1);
        let started = json!({"method": "item/started", "params": {"threadId": "sub", "item": sub_change("c")}});
        d.fold_line(&mut t, &started.to_string());
        assert!(tools(&t).iter().all(|tool| !tool.id.starts_with("c:")), "not shown until it asks");
        let ask = json!({"id": 9, "method": "item/fileChange/requestApproval", "params": {"threadId": "sub", "itemId": "c"}});
        assert_eq!(d.fold_line(&mut t, &ask.to_string()), None);
        let files: Vec<_> = tools(&t).iter().filter(|x| x.id.starts_with("c:")).map(|x| x.input["file_path"].clone()).collect();
        assert_eq!(files, [json!("/w/a.txt"), json!("/w/b.txt")]);
        assert!(matches!(t.entries.last(), Some(Entry::Ask(a)) if a.input["file_path"] == "/w/a.txt"), "{:?}", t.entries.last());
        let done = json!({"method": "item/completed", "params": {"threadId": "sub", "item": sub_change("c")}});
        d.fold_line(&mut t, &done.to_string());
        assert!(tools(&t).iter().filter(|x| x.id.starts_with("c:")).all(|x| x.result.is_some() && !x.error));
    }

    #[test]
    fn a_change_nothing_is_known_of_is_declined_not_asked() {
        let (mut d, mut t) = fed(1);
        let ask = json!({"id": 9, "method": "item/fileChange/requestApproval", "params": {"threadId": "sub", "itemId": "gone"}});
        let reply: Value = serde_json::from_str(&d.fold_line(&mut t, &ask.to_string()).expect("a reply")).unwrap();
        assert_eq!(reply, json!({"id": 9, "result": {"decision": "decline"}}));
        assert!(!t.entries.iter().any(|e| matches!(e, Entry::Ask(a) if a.request_id == "9")));
        assert!(matches!(t.entries.last(), Some(Entry::Notice { .. })), "{:?}", t.entries.last());
    }

    #[test]
    fn a_request_another_thread_takes_back_is_settled() {
        let (mut d, mut t) = fed(1);
        let ask = json!({"id": 9, "method": "item/commandExecution/requestApproval",
            "params": {"threadId": "sub", "itemId": "i", "command": "ls"}});
        d.fold_line(&mut t, &ask.to_string());
        let resolved = json!({"method": "serverRequest/resolved", "params": {"threadId": "sub", "requestId": 9}});
        d.fold_line(&mut t, &resolved.to_string());
        assert!(matches!(t.entries.last(), Some(Entry::Ask(a)) if a.answer == Some(Answer::Withdrawn)), "{:?}", t.entries.last());
        let (mut f, mut out) = (CodexFollow::default(), Vec::new());
        f.follow(&json!({"id": 1, "result": {"thread": {"id": "main"}}}), &mut out);
        out.clear();
        f.follow(&resolved, &mut out);
        assert_eq!(out, [Said::Settled("9".into())]);
    }

    #[test]
    fn a_message_the_user_sent_is_not_shown_twice() {
        let (mut d, mut t) = (CodexDriver::default(), Transcript::default());
        let text = "Run ls. Then create b.txt containing hi with apply_patch. Then say Done.";
        d.user_line(THREAD, text, &[]);
        t.push_user(text, Vec::new());
        for line in server_lines(1) {
            d.fold_line(&mut t, &line);
        }
        assert_eq!(t.entries.iter().filter(|e| matches!(e, Entry::User { .. })).count(), 1);
    }

    #[test]
    fn a_message_someone_else_sent_is_shown() {
        let (_, t) = fed(1);
        assert!(matches!(&t.entries[0], Entry::User { text, .. } if text.starts_with("Run ls.")), "{:?}", t.entries[0]);
    }

    #[test]
    fn a_request_it_cant_answer_is_refused() {
        let (mut d, mut t) = (CodexDriver::default(), Transcript::default());
        let line = json!({"id": 7, "method": "item/tool/call", "params": {}}).to_string();
        let reply: Value = serde_json::from_str(&d.fold_line(&mut t, &line).unwrap()).unwrap();
        assert_eq!(reply["id"], 7);
        assert_eq!(reply["error"]["code"], -32601);
    }

    #[test]
    fn an_mcp_form_is_an_elicit_and_answers_by_id() {
        let (mut d, mut t) = (CodexDriver::default(), Transcript::default());
        let line = json!({"id": 3, "method": "mcpServer/elicitation/request", "params": {
            "threadId": "t", "serverName": "deploy", "mode": "form", "message": "Which?",
            "requestedSchema": {"type": "object", "properties": {"env": {"type": "string"}}, "required": ["env"]},
        }});
        assert_eq!(d.fold_line(&mut t, &line.to_string()), None);
        let Some(Entry::Elicit(e)) = t.entries.last().cloned() else { panic!("{:?}", t.entries) };
        assert_eq!((e.server.as_str(), e.message.as_str(), e.fields.len()), ("deploy", "Which?", 1));
        let answer = crate::ElicitAnswer::Accept(json!({"env": "prod"}));
        let v: Value = serde_json::from_str(&d.elicit_line(&e, &answer).unwrap()).unwrap();
        assert_eq!(v, json!({"id": 3, "result": {"action": "accept", "content": {"env": "prod"}}}));
    }

    #[test]
    fn a_fresh_thread_tells_the_chat_it_was_cleared() {
        let (mut d, mut t) = fed(1);
        t.handoff = None;
        let line = json!({"id": "x", "result": {"thread": {"id": "fresh"}}});
        d.fold_line(&mut t, &line.to_string());
        assert_eq!(t.session_id.as_deref(), Some("fresh"));
        assert_eq!(t.handoff, Some(None));
    }

    #[test]
    fn load_shows_what_the_rollout_saved_with_its_turns_ended() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.jsonl");
        std::fs::write(&path, include_str!("fixtures/rollout.jsonl")).unwrap();
        let (mut d, mut t) = (CodexDriver::default(), Transcript::default());
        d.load(&mut t, &path, None, Vec::new()).unwrap();
        assert!(t.entries.iter().any(|e| matches!(e, Entry::Text { text, .. } if text == "Done.")), "{:?}", t.entries);
        assert!(matches!(t.entries.last(), Some(Entry::TurnEnd(_))), "{:?}", t.entries.last());
    }

    #[test]
    fn load_stops_at_lines_saved_after_the_program_started() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.jsonl");
        std::fs::write(&path, include_str!("fixtures/rollout.jsonl")).unwrap();
        let (mut d, mut t) = (CodexDriver::default(), Transcript::default());
        d.load(&mut t, &path, Some(0), Vec::new()).unwrap();
        assert!(t.entries.is_empty(), "{:?}", t.entries);
    }

    fn said(lines: &[(bool, Value)]) -> Vec<Said> {
        let (mut f, mut out) = (CodexFollow::default(), Vec::new());
        for (client, v) in lines {
            if *client {
                f.wrote(v, &mut out);
            } else {
                f.follow(v, &mut out);
            }
        }
        out
    }

    #[test]
    fn codex_follower_says_turns_asks_and_session() {
        let out = said(&recording());
        let wanted: Vec<_> = out.iter().filter(|s| !matches!(s, Said::Reading(_) | Said::Streamed | Said::Title(_))).collect();
        assert_eq!(
            wanted,
            [
                &Said::Session(THREAD.into()),
                &Said::TurnStarted,
                &Said::TurnStarted,
                &Said::Ask("0".into()),
                &Said::Settled("0".into()),
                &Said::Settled("0".into()),
                &Said::TurnEnded,
                &Said::TurnStarted,
                &Said::TurnStarted,
                &Said::TurnEnded,
                &Said::TurnEnded,
            ]
        );
        assert!(out.iter().any(|s| matches!(s, Said::Reading(r) if r.size == 258_400)));
        assert!(out.contains(&Said::Streamed));
    }

    #[test]
    fn a_servers_answer_doesnt_settle_an_ask_of_its_number() {
        let lines = [
            (false, json!({"id": 5, "method": "item/fileChange/requestApproval", "params": {}})),
            (false, json!({"id": 5, "result": {"turn": {"id": "t"}}})),
        ];
        assert_eq!(said(&lines), [Said::Ask("5".into())]);
    }

    #[test]
    fn a_turn_the_server_refused_ends() {
        assert_eq!(said(&[(false, json!({"id": 2, "error": {"code": -32600, "message": "no"}}))]), [Said::TurnEnded]);
    }

    #[test]
    fn the_first_user_message_titles_the_chat() {
        assert!(
            said(&recording()).contains(&Said::Title(
                "Run ls. Then create b.txt containing hi with apply_patch. Then say Done.".chars().take(60).collect()
            ))
        );
    }

    #[test]
    fn notifications_say_what_codex_waits_for() {
        let f = CodexFollow::default();
        let ask = |m: &str| f.notice(&json!({"id": 1, "method": m, "params": {"serverName": "deploy"}}), "Codex");
        assert_eq!(ask("item/commandExecution/requestApproval").as_deref(), Some("Codex asks to run a command"));
        assert_eq!(ask("item/fileChange/requestApproval").as_deref(), Some("Codex asks to change files"));
        assert_eq!(ask("mcpServer/elicitation/request").as_deref(), Some("deploy asks for your input"));
        let ended = |s: &str| f.notice(&json!({"method": "turn/completed", "params": {"turn": {"status": s}}}), "Codex");
        assert_eq!(ended("completed").as_deref(), Some("Codex finished"));
        assert_eq!(ended("interrupted"), None);
        assert_eq!(ended("failed").as_deref(), Some("Codex stopped with an error"));
    }
}
