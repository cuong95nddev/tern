//! OpenCode's chat: `opencode acp`, the Agent Client Protocol over stdio.
//! A conversation's history comes from `session/load`'s replay, so nothing
//! here reads OpenCode's database. A turn is one `session/prompt` request,
//! answered when it ends; there is no steering one that runs.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::time::Instant;

use serde_json::{Value, json};
use tern_bridge::context::Reading;

use crate::claude::launch;
use crate::codex::app_server::{id_text, wire_id};
use crate::driver::{ChatCaps, ChatDriver, ChatFollow, Said};
use crate::model::{Answer, Ask, Elicit, ElicitAnswer, Entry, PermissionMode, Tool, Transcript, TurnEnd};
use crate::summary;

/// The ids of the requests that start a chat. The server numbers its own
/// from 0, so ours are text.
const HELLO: &str = "hello";
const OPEN: &str = "open";
/// How much of a message titles a notification.
const TITLE_CHARS: usize = 60;

fn str_at<'a>(v: &'a Value, pointer: &str) -> &'a str {
    v.pointer(pointer).and_then(Value::as_str).unwrap_or_default()
}

fn request(id: &str, method: &str, params: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string()
}

/// A tool's input as Tern shows it: its file under `file_path`, without the
/// folder every call names.
fn tool_input(raw: Option<&Value>) -> Value {
    let mut input = raw.filter(|r| r.is_object()).cloned().unwrap_or_else(|| json!({}));
    if let Some(o) = input.as_object_mut() {
        o.remove("cwd");
        if let Some(path) = o.remove("filePath") {
            o.insert("file_path".into(), path);
        }
    }
    input
}

fn is_empty(input: &Value) -> bool {
    input.as_object().is_none_or(serde_json::Map::is_empty)
}

/// The text of a `tool_call_update`'s `content`.
fn content_text(update: &Value) -> String {
    let parts = update.get("content").and_then(Value::as_array).into_iter().flatten();
    parts.filter_map(|p| p.pointer("/content/text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n")
}

/// How full the window is, from a `usage_update`.
fn reading(update: &Value) -> Option<Reading> {
    let used = update.get("used")?.as_u64()?;
    let size = update.get("size")?.as_u64().filter(|s| *s > 0)?;
    Some(Reading { percent: (used as f64 * 100. / size as f64).clamp(0., 100.) as f32, size })
}

/// `opencode acp`.
#[derive(Debug, Default)]
pub struct OpenCodeDriver {
    /// The conversation, once the server said.
    session: Option<String>,
    /// The chat's folder.
    cwd: String,
    /// The conversation was asked for and isn't open yet.
    opening: bool,
    /// The conversation to load, until it has opened: a refused load is asked
    /// again with the next message.
    resuming: Option<String>,
    /// A turn runs, of the chat's or the daemon's.
    turn: bool,
    /// When this chat's turn started.
    started: Option<Instant>,
    /// What the user sent meanwhile.
    queued: Vec<String>,
    /// The latest message: its id and entry, which its next chunk adds to.
    message: Option<(String, usize)>,
    /// The entries of the tools, by call id.
    tools: HashMap<String, usize>,
}

impl OpenCodeDriver {
    fn load_line(&self, session: &str) -> String {
        request(OPEN, "session/load", json!({ "sessionId": session, "cwd": self.cwd, "mcpServers": [] }))
    }

    /// The line that sends `text` in conversation `session`, a turn starting.
    fn prompt_line(&mut self, session: &str, text: &str) -> (String, String) {
        (self.turn, self.started) = (true, Some(Instant::now()));
        let id = crate::new_session_id();
        let line = request(&id, "session/prompt", json!({ "sessionId": session, "prompt": [{ "type": "text", "text": text }] }));
        (id, line)
    }

    /// What was held back goes in, now that it can.
    fn flush(&mut self, t: &mut Transcript, session: &str) -> Option<String> {
        let queued = std::mem::take(&mut self.queued);
        if queued.is_empty() {
            return None;
        }
        t.busy = true;
        Some(self.prompt_line(session, &queued.join("\n\n")).1)
    }

    /// Conversation `session` is open.
    fn opened(&mut self, t: &mut Transcript, session: &str) -> Option<String> {
        // A later one is the fresh conversation a handoff goes on in.
        if self.session.as_deref().is_some_and(|old| old != session) {
            t.handoff = Some(None);
            t.context = None;
        }
        self.session = Some(session.to_owned());
        t.session_id = Some(session.to_owned());
        (self.opening, self.resuming, self.message) = (false, None, None);
        // A replayed load's lines looked like a turn at work.
        (self.turn, t.busy) = (false, false);
        t.history = t.entries.len();
        t.settle();
        self.flush(t, session)
    }

    fn turn_ended(&mut self, t: &mut Transcript, interrupted: bool, error: Option<String>) -> Option<String> {
        let ms = self.started.take().map_or(0, |s| s.elapsed().as_millis() as u64);
        if let Some(Entry::Text { text, .. }) = self.message.as_ref().and_then(|(_, i)| t.entries.get(*i))
            && let Some(path) = tern_bridge::context::handoff_path(text)
        {
            t.handoff = Some(Some(path));
        }
        t.settle();
        t.entries.push(Entry::TurnEnd(TurnEnd { ms, interrupted, error }));
        (t.busy, self.turn, self.message) = (false, false, None);
        let session = self.session.clone()?;
        self.flush(t, &session)
    }

    /// Add chunk `piece` of message `id`: to its entry, or as `entry`.
    fn chunk(&mut self, t: &mut Transcript, id: &str, piece: &str, entry: impl FnOnce(String) -> Entry) {
        let shown = self.message.as_ref().filter(|(m, _)| m == id).and_then(|(_, i)| t.entries.get_mut(*i));
        match shown {
            Some(Entry::Text { text, .. } | Entry::User { text, .. }) => text.push_str(piece),
            _ => {
                t.entries.push(entry(piece.to_owned()));
                self.message = Some((id.to_owned(), t.entries.len() - 1));
            },
        }
    }

    /// A prompt the daemon kept, replayed: the server tells of none it was
    /// sent live.
    fn prompted(&mut self, t: &mut Transcript, v: &Value) {
        let blocks = v.pointer("/params/prompt").and_then(Value::as_array).into_iter().flatten();
        let text = blocks.filter_map(|b| b.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n");
        t.entries.push(Entry::User { text, images: Vec::new() });
        self.working(t);
    }

    /// The agent is at work, on a turn this chat may not have started.
    fn working(&mut self, t: &mut Transcript) {
        if !self.opening {
            (self.turn, t.busy) = (true, true);
        }
    }

    fn tool_call(&mut self, t: &mut Transcript, u: &Value) {
        let name = summary::acp_name(str_at(u, "/kind"), str_at(u, "/name")).to_owned();
        let id = str_at(u, "/toolCallId").to_owned();
        let tool = Tool { id: id.clone(), ..Tool::new(name, tool_input(u.get("rawInput"))) };
        t.entries.push(Entry::Tool(tool));
        self.tools.insert(id, t.entries.len() - 1);
    }

    fn tool_update(&mut self, t: &mut Transcript, u: &Value) {
        let Some(Entry::Tool(tool)) = self.tools.get(str_at(u, "/toolCallId")).and_then(|&i| t.entries.get_mut(i)) else { return };
        let input = tool_input(u.get("rawInput"));
        if !is_empty(&input) {
            tool.input = input;
        }
        match str_at(u, "/status") {
            "completed" => (tool.result, tool.error) = (Some(content_text(u)), false),
            "failed" => (tool.result, tool.error) = (Some(content_text(u)), true),
            _ => {},
        }
    }

    fn update(&mut self, t: &mut Transcript, params: &Value) {
        let session = str_at(params, "/sessionId");
        match self.session.as_deref() {
            Some(ours) if ours != session => return,
            Some(_) => {},
            None => {
                self.session = Some(session.to_owned());
                t.session_id = Some(session.to_owned());
            },
        }
        let u = &params["update"];
        let (id, piece) = (str_at(u, "/messageId"), str_at(u, "/content/text"));
        match str_at(u, "/sessionUpdate") {
            "user_message_chunk" => self.chunk(t, id, piece, |text| Entry::User { text, images: Vec::new() }),
            "agent_message_chunk" => {
                self.working(t);
                self.chunk(t, id, piece, |text| Entry::Text { text, streaming: true });
            },
            "tool_call" => {
                self.working(t);
                self.tool_call(t, u);
            },
            "tool_call_update" => self.tool_update(t, u),
            "usage_update" => t.context = reading(u).or(t.context),
            _ => {},
        }
    }

    /// `session/request_permission`: asked of the user, unless it says too
    /// little for them to tell what they would allow.
    fn permission(&mut self, t: &mut Transcript, v: &Value) -> Option<String> {
        let call = &v["params"]["toolCall"];
        let id = str_at(call, "/toolCallId");
        let (kind, name, title) = (str_at(call, "/kind"), str_at(call, "/name"), str_at(call, "/title"));
        let mut input = tool_input(call.get("rawInput"));
        let shown = self.tools.get(id).and_then(|&i| t.entries.get(i));
        if let (true, Some(Entry::Tool(tool))) = (is_empty(&input), shown) {
            input = tool.input.clone();
        }
        if is_empty(&input) && !title.is_empty() && title != name {
            input = if kind == "execute" { json!({ "command": title }) } else { json!({ "description": title }) };
        }
        if is_empty(&input) {
            t.entries.push(Entry::Notice { text: "Rejected a request that came without its details".into(), error: true });
            return Some(answer(&v["id"], "reject"));
        }
        t.entries.push(Entry::Ask(Ask {
            request_id: id_text(&v["id"]),
            tool_name: summary::acp_name(kind, name).to_owned(),
            tool_use_id: Some(id.to_owned()),
            input,
            suggestions: Value::Null,
            answer: None,
        }));
        None
    }

    /// An answer: to a request of ours, or one of the user's, replayed.
    fn response(&mut self, t: &mut Transcript, v: &Value) -> Option<String> {
        let id = v.get("id").map(id_text).unwrap_or_default();
        if let Some(error) = v.get("error") {
            let message = str_at(error, "/message").to_owned();
            if id == OPEN || id == HELLO {
                self.opening = false;
                t.entries.push(Entry::Notice { text: message, error: true });
                t.busy = false;
                return None;
            }
            return self.turn_ended(t, false, Some(message));
        }
        let result = v.get("result")?;
        if let Some(option) = result.pointer("/outcome/optionId").and_then(Value::as_str) {
            let answer = match option {
                "once" => Answer::Allow,
                "always" => Answer::AllowSession,
                _ => Answer::Deny,
            };
            t.set_answer(&id, answer);
            return None;
        }
        if let Some(session) = result.get("sessionId").and_then(Value::as_str) {
            return self.opened(t, session);
        }
        if let Some(stop) = result.get("stopReason").and_then(Value::as_str) {
            return self.turn_ended(t, stop == "cancelled", None);
        }
        if id == OPEN {
            let session = self.resuming.clone().or_else(|| self.session.clone())?;
            return self.opened(t, &session);
        }
        None
    }
}

/// The answer to permission request `id`.
fn answer(id: &Value, option: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": { "outcome": { "outcome": "selected", "optionId": option } } }).to_string()
}

impl ChatDriver for OpenCodeDriver {
    fn agent(&self) -> &'static str {
        tern_agent::OPENCODE
    }

    fn caps(&self) -> ChatCaps {
        ChatCaps { interrupt: true, ..ChatCaps::default() }
    }

    fn command(&self, user_args: &str, _: &str, _: bool, _: Option<&str>, _: Option<PermissionMode>, quote: fn(&str) -> String) -> String {
        // Its descriptor's command and args: a user file may change them.
        let registry = tern_agent::registry();
        let mut line = registry.get(tern_agent::OPENCODE).map_or_else(|| tern_agent::OPENCODE.to_owned(), |d| launch::start(d, quote));
        if !user_args.is_empty() {
            line.push(' ');
            line.push_str(user_args);
        }
        line.push_str(" acp");
        line
    }

    fn start_lines(&mut self, session: &str, resume: bool, cwd: &Path) -> Vec<String> {
        self.cwd = cwd.to_string_lossy().into_owned();
        self.opening = true;
        let open = if resume {
            self.resuming = Some(session.to_owned());
            self.session = Some(session.to_owned());
            self.load_line(session)
        } else {
            request(OPEN, "session/new", json!({ "cwd": self.cwd, "mcpServers": [] }))
        };
        let hello = json!({
            "protocolVersion": 1,
            "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false } },
            "clientInfo": { "name": "tern", "version": env!("CARGO_PKG_VERSION") },
        });
        vec![request(HELLO, "initialize", hello), open]
    }

    fn resumes(&self, session: &str) -> bool {
        session.starts_with("ses_")
    }

    fn fold_line(&mut self, t: &mut Transcript, line: &str) -> Option<String> {
        let v: Value = serde_json::from_str(line).ok()?;
        let reply = match (v.get("method").and_then(Value::as_str), v.get("id")) {
            (Some("session/request_permission"), Some(_)) => self.permission(t, &v),
            (Some("session/prompt"), Some(_)) => {
                self.prompted(t, &v);
                None
            },
            (Some(other), Some(id)) => {
                t.entries.push(Entry::Notice { text: format!("A request this chat can't answer: “{other}”"), error: false });
                let error = json!({ "code": -32601, "message": format!("unsupported request {other}") });
                Some(json!({ "jsonrpc": "2.0", "id": id, "error": error }).to_string())
            },
            (Some("session/update"), None) => {
                self.update(t, &v["params"]);
                None
            },
            (Some(_), None) => None,
            (None, _) => self.response(t, &v),
        };
        t.version += 1;
        reply
    }

    fn user_line(&mut self, session: &str, text: &str, _: &[(&str, &[u8])]) -> (String, String) {
        if self.opening || self.resuming.is_some() {
            self.queued.push(text.to_owned());
            let line = self.resuming.as_deref().filter(|_| !self.opening).map(|s| self.load_line(s)).unwrap_or_default();
            self.opening = true;
            return (crate::new_session_id(), line);
        }
        if self.turn {
            self.queued.push(text.to_owned());
            return (crate::new_session_id(), String::new());
        }
        let session = self.session.clone().unwrap_or_else(|| session.to_owned());
        self.prompt_line(&session, text)
    }

    fn clear_line(&mut self, _: &str, cwd: &Path) -> String {
        request(&crate::new_session_id(), "session/new", json!({ "cwd": cwd, "mcpServers": [] }))
    }

    fn answer_line(&self, ask: &Ask, answer_to: &Answer) -> Option<String> {
        let option = match answer_to {
            Answer::Allow => "once",
            Answer::AllowSession => "always",
            Answer::Deny => "reject",
            Answer::Answered(_) | Answer::Withdrawn => return None,
        };
        Some(answer(&wire_id(&ask.request_id), option))
    }

    fn elicit_line(&self, _: &Elicit, _: &ElicitAnswer) -> Option<String> {
        None
    }

    fn interrupt_line(&self) -> Option<String> {
        let session = self.session.as_deref()?;
        Some(json!({ "jsonrpc": "2.0", "method": "session/cancel", "params": { "sessionId": session } }).to_string())
    }

    fn mode_line(&self, _: PermissionMode) -> Option<String> {
        None
    }

    fn model_line(&self, _: &str) -> Option<String> {
        None
    }

    fn reset(&mut self) {
        *self = Self { session: self.session.take(), cwd: std::mem::take(&mut self.cwd), ..Self::default() };
    }

    fn load(&mut self, _: &mut Transcript, _: &Path, _: Option<u64>, _: Vec<(u64, Value)>) -> io::Result<()> {
        // `session/load`'s replay is the history.
        Ok(())
    }

    fn ended(&mut self, t: &mut Transcript, why: Option<String>) {
        self.turn = false;
        t.ended(why);
    }
}

/// What the daemon reads of an OpenCode chat.
#[derive(Debug, Default)]
pub struct OpenCodeFollow {
    session: Option<String>,
    /// The turn's latest message: its id and what came of it so far.
    message: String,
    text: String,
}

impl ChatFollow for OpenCodeFollow {
    fn follow(&mut self, v: &Value, out: &mut Vec<Said>) {
        let Some(method) = v.get("method").and_then(Value::as_str) else {
            if v.get("error").is_some() {
                out.push(Said::TurnEnded);
            } else if let Some(stop) = v.pointer("/result/stopReason").and_then(Value::as_str) {
                if stop != "cancelled" {
                    out.push(Said::LastText(std::mem::take(&mut self.text)));
                }
                out.push(Said::TurnEnded);
            } else if let Some(session) = v.pointer("/result/sessionId").and_then(Value::as_str) {
                self.session = Some(session.to_owned());
                out.push(Said::Session(session.to_owned()));
            }
            return;
        };
        match method {
            "session/request_permission" if v.get("id").is_some() => out.push(Said::Ask(id_text(&v["id"]))),
            "session/update" if self.session.as_deref().is_none_or(|s| s == str_at(v, "/params/sessionId")) => {
                let u = &v["params"]["update"];
                match str_at(u, "/sessionUpdate") {
                    // The only copy of a message: it stays for replay.
                    "agent_message_chunk" => {
                        let id = str_at(u, "/messageId");
                        if self.message != id {
                            id.clone_into(&mut self.message);
                            self.text.clear();
                        }
                        self.text.push_str(str_at(u, "/content/text"));
                    },
                    "usage_update" => out.extend(reading(u).map(Said::Reading)),
                    _ => {},
                }
            },
            _ => {},
        }
    }

    fn wrote(&mut self, v: &Value, out: &mut Vec<Said>) {
        match v.get("method").and_then(Value::as_str) {
            Some("session/prompt") => {
                self.text.clear();
                out.extend([Said::TurnStarted, Said::Keep]);
                let line = str_at(v, "/params/prompt/0/text").lines().next().unwrap_or_default().trim();
                if !line.is_empty() {
                    out.push(Said::Title(line.chars().take(TITLE_CHARS).collect()));
                }
            },
            Some("session/load") => {
                let session = str_at(v, "/params/sessionId").to_owned();
                self.session = Some(session.clone());
                out.push(Said::Session(session));
            },
            None if v.get("result").is_some() || v.get("error").is_some() => {
                out.push(Said::Settled(v.get("id").map(id_text).unwrap_or_default()));
            },
            _ => {},
        }
    }

    fn notice(&self, v: &Value, agent: &str) -> Option<String> {
        if v.get("method").and_then(Value::as_str) == Some("session/request_permission") {
            let run = str_at(v, "/params/toolCall/kind") == "execute";
            return Some(if run { format!("{agent} asks to run a command") } else { format!("{agent} asks for permission") });
        }
        if v.get("error").is_some() {
            return Some(format!("{agent} stopped with an error"));
        }
        (str_at(v, "/result/stopReason") == "end_turn").then(|| format!("{agent} finished"))
    }

    fn hookless(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::*;
    use crate::model::{Answer, Entry, Transcript};

    const FIXTURE: &str = include_str!("fixtures/acp.jsonl");
    const SESSION: &str = "ses_ternspike0000000000000004";

    fn opened() -> (OpenCodeDriver, Transcript) {
        let (mut d, mut t) = (OpenCodeDriver::default(), Transcript::default());
        d.start_lines("new", false, Path::new("/w"));
        d.fold_line(&mut t, &json!({"id": "open", "result": {"sessionId": "s"}}).to_string());
        (d, t)
    }

    fn update(u: Value) -> String {
        json!({"method": "session/update", "params": {"sessionId": "s", "update": u}}).to_string()
    }

    fn text(entry: &Entry) -> &str {
        match entry {
            Entry::Text { text, .. } | Entry::User { text, .. } => text,
            _ => "",
        }
    }

    fn tools(t: &Transcript) -> Vec<&crate::Tool> {
        t.entries.iter().filter_map(|e| if let Entry::Tool(t) = e { Some(t) } else { None }).collect()
    }

    fn ask(id: u64, input: Value, title: &str) -> String {
        json!({"id": id, "method": "session/request_permission", "params": {"sessionId": "s",
            "toolCall": {"toolCallId": "c", "name": "shell", "title": title, "kind": "execute", "rawInput": input},
            "options": [{"optionId": "once", "kind": "allow_once"}, {"optionId": "always", "kind": "allow_always"}, {"optionId": "reject", "kind": "reject_once"}]}})
        .to_string()
    }

    #[test]
    fn it_opens_and_goes_on() {
        let mut d = OpenCodeDriver::default();
        let new: Vec<Value> = d.start_lines("x", false, Path::new("/w")).iter().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(new[0]["method"], "initialize");
        assert_eq!((&new[1]["method"], &new[1]["params"]["cwd"]), (&json!("session/new"), &json!("/w")));
        let mut d = OpenCodeDriver::default();
        let load: Vec<Value> = d.start_lines("ses_1", true, Path::new("/w")).iter().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!((&load[1]["method"], &load[1]["params"]["sessionId"]), (&json!("session/load"), &json!("ses_1")));
    }

    #[test]
    fn its_sessions_are_saved_where_tern_cant_tell() {
        let d = OpenCodeDriver::default();
        assert!(d.resumes("ses_e54e70200ffeElMBFmIu2vYoqo"));
        assert!(!d.resumes("0f-1"));
    }

    #[test]
    fn a_new_chat_learns_its_session() {
        let (d, t) = opened();
        assert_eq!(t.session_id.as_deref(), Some("s"));
        assert_eq!(d.session.as_deref(), Some("s"));
    }

    #[test]
    fn a_recorded_chat_folds() {
        let (mut d, mut t) = (OpenCodeDriver::default(), Transcript::default());
        d.start_lines(SESSION, true, Path::new("/w"));
        for line in FIXTURE.replace("\"id\":\"r2\"", "\"id\":\"open\"").lines() {
            if let Some(reply) = d.fold_line(&mut t, line) {
                panic!("nothing to write back: {reply}");
            }
        }
        assert_eq!(t.session_id.as_deref(), Some(SESSION));
        let users: Vec<_> = t.entries.iter().filter(|e| matches!(e, Entry::User { .. })).map(text).collect();
        assert_eq!(users.first(), Some(&"Say the word pong and nothing else."));
        assert!(t.entries.iter().any(|e| matches!(e, Entry::Text { text, .. } if text.starts_with("mock says: hello again"))));
        assert!(tools(&t).iter().any(|x| x.name == "Bash" && x.input["command"] == "echo tern-tool"), "{:?}", tools(&t));
        assert_eq!(t.entries.iter().filter(|e| matches!(e, Entry::Ask(_))).count(), 1);
        assert_eq!(t.entries.iter().filter(|e| matches!(e, Entry::TurnEnd(_))).count(), 3);
        assert!(matches!(t.entries.last(), Some(Entry::TurnEnd(e)) if e.interrupted));
        assert_eq!(t.context.map(|c| c.size), Some(100_000));
        assert!(!t.busy);
    }

    #[test]
    fn chunks_of_one_message_join() {
        let (mut d, mut t) = opened();
        let chunk = |id: &str, text: &str| {
            update(json!({"sessionUpdate": "agent_message_chunk", "messageId": id, "content": {"type": "text", "text": text}}))
        };
        d.fold_line(&mut t, &chunk("m", "he"));
        d.fold_line(&mut t, &chunk("m", "llo"));
        d.fold_line(&mut t, &chunk("n", "again"));
        assert_eq!(t.entries.iter().map(text).collect::<Vec<_>>(), ["hello", "again"]);
    }

    #[test]
    fn a_tool_fills_in_from_its_updates() {
        let (mut d, mut t) = opened();
        d.fold_line(&mut t, &update(json!({"sessionUpdate": "tool_call", "toolCallId": "c", "name": "shell", "title": "shell", "kind": "execute", "status": "pending", "rawInput": {"cwd": "/w"}})));
        d.fold_line(&mut t, &update(json!({"sessionUpdate": "tool_call_update", "toolCallId": "c", "status": "in_progress", "rawInput": {"command": "ls", "cwd": "/w"}})));
        d.fold_line(&mut t, &update(json!({"sessionUpdate": "tool_call_update", "toolCallId": "c", "status": "failed", "content": [{"type": "content", "content": {"type": "text", "text": "boom"}}]})));
        let tool = tools(&t)[0];
        assert_eq!((tool.name.as_str(), &tool.input["command"]), ("Bash", &json!("ls")));
        assert_eq!((tool.result.as_deref(), tool.error), (Some("boom"), true));
    }

    #[test]
    fn a_file_tool_names_its_file_as_tern_does() {
        let (mut d, mut t) = opened();
        d.fold_line(
            &mut t,
            &update(
                json!({"sessionUpdate": "tool_call", "toolCallId": "c", "name": "edit", "kind": "edit", "rawInput": {"filePath": "/w/a"}}),
            ),
        );
        assert_eq!((tools(&t)[0].name.as_str(), &tools(&t)[0].input["file_path"]), ("Edit", &json!("/w/a")));
    }

    #[test]
    fn a_permission_asks_with_the_tools_input() {
        let (mut d, mut t) = opened();
        assert_eq!(d.fold_line(&mut t, &ask(0, json!({"command": "rm x", "cwd": "/w"}), "rm x")), None);
        let Some(Entry::Ask(a)) = t.entries.last() else { panic!("{:?}", t.entries) };
        assert_eq!((a.request_id.as_str(), a.tool_name.as_str(), &a.input["command"]), ("0", "Bash", &json!("rm x")));
    }

    #[test]
    fn a_command_known_only_by_its_title_is_asked_with_it() {
        let (mut d, mut t) = opened();
        d.fold_line(&mut t, &ask(0, json!({"cwd": "/w"}), "rm x"));
        assert!(matches!(t.entries.last(), Some(Entry::Ask(a)) if a.input["command"] == "rm x"), "{:?}", t.entries);
    }

    #[test]
    fn answers_pick_the_option() {
        let (mut d, mut t) = opened();
        d.fold_line(&mut t, &ask(0, json!({"command": "ls"}), "ls"));
        let Some(Entry::Ask(a)) = t.entries.last() else { panic!() };
        for (answer, option) in [(Answer::Allow, "once"), (Answer::AllowSession, "always"), (Answer::Deny, "reject")] {
            let line: Value = serde_json::from_str(&d.answer_line(a, &answer).unwrap()).unwrap();
            assert_eq!(line, json!({"jsonrpc": "2.0", "id": 0, "result": {"outcome": {"outcome": "selected", "optionId": option}}}));
        }
    }

    #[test]
    fn a_replayed_answer_shows() {
        let (mut d, mut t) = opened();
        d.fold_line(&mut t, &ask(0, json!({"command": "ls"}), "ls"));
        d.fold_line(&mut t, &json!({"id": 0, "result": {"outcome": {"outcome": "selected", "optionId": "reject"}}}).to_string());
        assert!(matches!(t.entries.last(), Some(Entry::Ask(a)) if a.answer == Some(Answer::Deny)));
    }

    #[test]
    fn an_ask_with_no_details_is_rejected_not_shown() {
        let (mut d, mut t) = opened();
        let reply: Value = serde_json::from_str(&d.fold_line(&mut t, &ask(3, json!({"cwd": "/w"}), "")).expect("a reply")).unwrap();
        assert_eq!(reply["result"]["outcome"]["optionId"], "reject");
        assert_eq!(reply["id"], 3);
        assert!(!t.entries.iter().any(|e| matches!(e, Entry::Ask(_))));
        assert!(matches!(t.entries.last(), Some(Entry::Notice { error: true, .. })));
    }

    #[test]
    fn a_cancelled_turn_ends_interrupted() {
        let (mut d, mut t) = opened();
        t.push_user("x", Vec::new());
        let (id, _) = d.user_line("s", "x", &[]);
        d.fold_line(&mut t, &json!({"id": id, "result": {"stopReason": "cancelled"}}).to_string());
        assert!(matches!(t.entries.last(), Some(Entry::TurnEnd(e)) if e.interrupted && e.error.is_none()));
        assert!(!t.busy);
    }

    #[test]
    fn a_provider_error_ends_the_turn_with_its_message() {
        let (mut d, mut t) = opened();
        t.push_user("x", Vec::new());
        let (id, _) = d.user_line("s", "x", &[]);
        d.fold_line(&mut t, &json!({"id": id, "error": {"code": -32603, "message": "Free usage limit reached"}}).to_string());
        assert!(matches!(t.entries.last(), Some(Entry::TurnEnd(e)) if e.error.as_deref() == Some("Free usage limit reached")));
        assert!(!t.busy);
        let (_, line) = d.user_line("s", "next", &[]);
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["method"], "session/prompt");
    }

    #[test]
    fn a_refused_load_is_a_notice_and_asked_again() {
        let (mut d, mut t) = (OpenCodeDriver::default(), Transcript::default());
        d.start_lines("ses_1", true, Path::new("/w"));
        d.fold_line(&mut t, &json!({"id": "open", "error": {"code": -32603, "message": "Session not found"}}).to_string());
        assert!(matches!(t.entries.last(), Some(Entry::Notice { error: true, .. })));
        assert!(!t.busy);
        let (_, line) = d.user_line("ses_1", "hi", &[]);
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["method"], "session/load");
        let reply = d.fold_line(&mut t, &json!({"id": "open", "result": {}}).to_string()).expect("the message goes in");
        assert_eq!(serde_json::from_str::<Value>(&reply).unwrap()["params"]["prompt"][0]["text"], "hi");
    }

    #[test]
    fn a_message_before_the_session_opens_waits() {
        let (mut d, mut t) = (OpenCodeDriver::default(), Transcript::default());
        d.start_lines("x", false, Path::new("/w"));
        assert_eq!(d.user_line("x", "hi", &[]).1, "");
        let reply = d.fold_line(&mut t, &json!({"id": "open", "result": {"sessionId": "s"}}).to_string()).expect("the message goes in");
        let v: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!((&v["method"], &v["params"]["sessionId"]), (&json!("session/prompt"), &json!("s")));
    }

    #[test]
    fn a_message_during_a_turn_waits_for_its_end() {
        let (mut d, mut t) = opened();
        t.push_user("one", Vec::new());
        let (id, _) = d.user_line("s", "one", &[]);
        t.push_user("two", Vec::new());
        assert_eq!(d.user_line("s", "two", &[]).1, "");
        let reply = d.fold_line(&mut t, &json!({"id": id, "result": {"stopReason": "end_turn"}}).to_string()).expect("it goes in");
        assert_eq!(serde_json::from_str::<Value>(&reply).unwrap()["params"]["prompt"][0]["text"], "two");
        assert!(t.busy);
    }

    #[test]
    fn a_turn_someone_else_started_counts() {
        // The daemon sends the guard's messages.
        let (mut d, mut t) = opened();
        d.fold_line(
            &mut t,
            &update(json!({"sessionUpdate": "agent_message_chunk", "messageId": "m", "content": {"type": "text", "text": "hi"}})),
        );
        assert_eq!(d.user_line("s", "wait", &[]).1, "");
        let reply = d.fold_line(&mut t, &json!({"id": "other", "result": {"stopReason": "end_turn"}}).to_string());
        assert!(reply.is_some());
    }

    #[test]
    fn usage_update_gives_the_reading() {
        let (mut d, mut t) = opened();
        d.fold_line(&mut t, &update(json!({"sessionUpdate": "usage_update", "used": 620, "size": 100000})));
        assert_eq!(t.context.map(|c| c.size), Some(100_000));
        assert!((t.context.unwrap().percent - 0.62).abs() < 0.001);
    }

    #[test]
    fn a_child_sessions_update_is_left_out() {
        let (mut d, mut t) = opened();
        let line = json!({"method": "session/update", "params": {"sessionId": "child", "update": {"sessionUpdate": "agent_message_chunk", "messageId": "m", "content": {"type": "text", "text": "x"}}}});
        d.fold_line(&mut t, &line.to_string());
        assert!(t.entries.is_empty());
    }

    #[test]
    fn a_fresh_session_is_a_handoffs_end() {
        let (mut d, mut t) = opened();
        let line: Value = serde_json::from_str(&d.clear_line("s", Path::new("/w"))).unwrap();
        assert_eq!((&line["method"], &line["params"]["cwd"]), (&json!("session/new"), &json!("/w")));
        d.fold_line(&mut t, &json!({"id": line["id"], "result": {"sessionId": "s2"}}).to_string());
        assert_eq!(t.handoff, Some(None));
        assert_eq!(t.session_id.as_deref(), Some("s2"));
    }

    #[test]
    fn interrupting_cancels_its_session() {
        let (d, _) = opened();
        let line: Value = serde_json::from_str(&d.interrupt_line().unwrap()).unwrap();
        assert_eq!((&line["method"], &line["params"]["sessionId"]), (&json!("session/cancel"), &json!("s")));
        assert!(line.get("id").is_none());
    }

    #[test]
    fn follow_tells_turns_asks_and_session() {
        let (mut f, mut out) = (OpenCodeFollow::default(), Vec::new());
        f.follow(&json!({"id": "open", "result": {"sessionId": "s"}}), &mut out);
        f.wrote(&json!({"id": "p", "method": "session/prompt", "params": {"sessionId": "s", "prompt": [{"type": "text", "text": "Fix it\nnow"}]}}), &mut out);
        f.follow(
            &serde_json::from_str(&update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "x"}})))
                .unwrap(),
            &mut out,
        );
        f.follow(&serde_json::from_str(&update(json!({"sessionUpdate": "usage_update", "used": 50, "size": 100}))).unwrap(), &mut out);
        f.follow(&serde_json::from_str(&ask(0, json!({"command": "ls"}), "ls")).unwrap(), &mut out);
        f.wrote(&json!({"id": 0, "result": {"outcome": {"outcome": "selected", "optionId": "once"}}}), &mut out);
        f.follow(&json!({"id": "p", "result": {"stopReason": "end_turn"}}), &mut out);
        let reading = tern_bridge::context::Reading { percent: 50., size: 100 };
        assert_eq!(
            out,
            [
                Said::Session("s".into()),
                Said::TurnStarted,
                Said::Keep,
                Said::Title("Fix it".into()),
                Said::Reading(reading),
                Said::Ask("0".into()),
                Said::Settled("0".into()),
                Said::LastText("x".into()),
                Said::TurnEnded,
            ]
        );
    }

    #[test]
    fn follow_learns_a_loaded_session_from_what_it_wrote() {
        let (mut f, mut out) = (OpenCodeFollow::default(), Vec::new());
        f.wrote(&json!({"id": "open", "method": "session/load", "params": {"sessionId": "ses_1"}}), &mut out);
        assert_eq!(out, [Said::Session("ses_1".into())]);
    }

    #[test]
    fn ids_overlap_both_ways() {
        // An error answer to id 0 is the server's to ours; ours to its request 0 is written.
        let (mut f, mut out) = (OpenCodeFollow::default(), Vec::new());
        f.wrote(&json!({"id": 0, "result": {"outcome": {"outcome": "cancelled"}}}), &mut out);
        assert_eq!(out, [Said::Settled("0".into())]);
    }

    #[test]
    fn a_child_session_tells_the_follower_nothing() {
        let (mut f, mut out) = (OpenCodeFollow::default(), Vec::new());
        f.follow(&json!({"id": "open", "result": {"sessionId": "s"}}), &mut out);
        out.clear();
        let line = json!({"method": "session/update", "params": {"sessionId": "child", "update": {"sessionUpdate": "usage_update", "used": 1, "size": 2}}});
        f.follow(&line, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn notices_say_what_waits() {
        let f = OpenCodeFollow::default();
        let say = |v: Value| f.notice(&v, "OpenCode");
        assert_eq!(say(serde_json::from_str(&ask(0, json!({}), "ls")).unwrap()).as_deref(), Some("OpenCode asks to run a command"));
        assert_eq!(say(json!({"id": "p", "result": {"stopReason": "end_turn"}})).as_deref(), Some("OpenCode finished"));
        assert_eq!(say(json!({"id": "p", "result": {"stopReason": "cancelled"}})), None);
        assert_eq!(say(json!({"id": "p", "error": {"message": "x"}})).as_deref(), Some("OpenCode stopped with an error"));
    }

    fn chunk(id: &str, text: &str) -> String {
        update(json!({"sessionUpdate": "agent_message_chunk", "messageId": id, "content": {"type": "text", "text": text}}))
    }

    fn prompt(text: &str) -> String {
        json!({"id": "p", "method": "session/prompt", "params": {"sessionId": "s", "prompt": [{"type": "text", "text": text}]}}).to_string()
    }

    fn end_turn() -> String {
        json!({"id": "p", "result": {"stopReason": "end_turn"}}).to_string()
    }

    /// `t`'s entries, with what a replay can't know left out.
    fn shown(t: &Transcript) -> Vec<Entry> {
        let mut entries = t.entries.clone();
        for e in &mut entries {
            if let Entry::TurnEnd(end) = e {
                end.ms = 0;
            }
        }
        entries
    }

    #[test]
    fn what_the_daemon_keeps_replays_as_the_chat_went() {
        let opened_line = json!({"id": "open", "result": {"sessionId": "s"}}).to_string();
        let tool = update(json!({"sessionUpdate": "tool_call", "toolCallId": "c", "name": "shell", "kind": "execute", "rawInput": {"command": "ls"}}));
        let (mut live, mut t) = (OpenCodeDriver::default(), Transcript::default());
        live.start_lines("new", false, Path::new("/w"));
        let mut kept = vec![opened_line.clone()];
        live.fold_line(&mut t, &opened_line);
        t.push_user("hi", Vec::new());
        let (_, written) = live.user_line("s", "hi", &[]);
        kept.push(written);
        for line in [chunk("m", "hel"), chunk("m", "lo"), tool, end_turn()] {
            live.fold_line(&mut t, &line);
            kept.push(line);
        }
        let (mut fresh, mut again) = (OpenCodeDriver::default(), Transcript::default());
        for line in &kept {
            assert_eq!(fresh.fold_line(&mut again, line), None, "{line}");
        }
        again.resume_busy(true);
        assert_eq!(shown(&again), shown(&t));
        assert!(!again.busy);
    }

    #[test]
    fn a_turn_replayed_unended_still_runs() {
        let (mut d, mut t) = opened();
        d.fold_line(&mut t, &prompt("hi"));
        d.fold_line(&mut t, &chunk("m", "hel"));
        t.resume_busy(true);
        assert!(t.busy);
        assert_eq!(d.user_line("s", "more", &[]).1, "", "it waits for the turn's end");
    }

    /// A chat replaying the load it opened with.
    fn loaded(lines: &[String]) -> (OpenCodeDriver, Transcript) {
        let (mut d, mut t) = (OpenCodeDriver::default(), Transcript::default());
        let user = update(json!({"sessionUpdate": "user_message_chunk", "messageId": "u", "content": {"type": "text", "text": "old"}}));
        d.fold_line(&mut t, &user);
        d.fold_line(&mut t, &chunk("m", "older answer"));
        d.fold_line(&mut t, &json!({"id": "open", "result": {}}).to_string());
        for line in lines {
            d.fold_line(&mut t, line);
        }
        t.resume_busy(true);
        (d, t)
    }

    #[test]
    fn a_loaded_chat_reattached_is_idle() {
        let (mut d, t) = loaded(&[]);
        assert!(!t.busy);
        let (_, line) = d.user_line("s", "go on", &[]);
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["method"], "session/prompt");
    }

    #[test]
    fn a_turn_after_a_load_reattached_still_runs() {
        let (_, t) = loaded(&[prompt("go on"), chunk("n", "working")]);
        assert!(t.busy);
    }

    #[test]
    fn a_turn_ending_on_a_handoff_line_tells_its_path() {
        let (mut d, mut t) = opened();
        d.fold_line(&mut t, &prompt("write one"));
        d.fold_line(&mut t, &chunk("m", "Done.\nHandoff: docs/h.md"));
        d.fold_line(&mut t, &end_turn());
        assert_eq!(t.handoff, Some(Some("docs/h.md".to_owned())));
    }

    #[test]
    fn follow_tells_the_last_message_of_a_turn() {
        let (mut f, mut out) = (OpenCodeFollow::default(), Vec::new());
        let value = |l: String| serde_json::from_str::<Value>(&l).unwrap();
        f.wrote(&value(prompt("go")), &mut out);
        f.follow(&value(chunk("m", "first")), &mut out);
        f.follow(&value(chunk("n", "sec")), &mut out);
        f.follow(&value(chunk("n", "ond")), &mut out);
        out.clear();
        f.follow(&value(end_turn()), &mut out);
        assert_eq!(out, [Said::LastText("second".into()), Said::TurnEnded]);
    }

    #[test]
    fn follow_starts_each_turn_with_no_text() {
        let (mut f, mut out) = (OpenCodeFollow::default(), Vec::new());
        let value = |l: String| serde_json::from_str::<Value>(&l).unwrap();
        f.wrote(&value(prompt("go")), &mut out);
        f.follow(&value(chunk("m", "first")), &mut out);
        f.follow(&value(end_turn()), &mut out);
        f.wrote(&value(prompt("again")), &mut out);
        out.clear();
        f.follow(&value(end_turn()), &mut out);
        assert_eq!(out, [Said::LastText(String::new()), Said::TurnEnded]);
    }

    #[test]
    fn a_stopped_turn_isnt_a_finished_one() {
        let (mut f, mut out) = (OpenCodeFollow::default(), Vec::new());
        f.follow(&json!({"id": "p", "result": {"stopReason": "cancelled"}}), &mut out);
        assert_eq!(out, [Said::TurnEnded]);
    }

    #[test]
    fn a_written_error_settles_like_an_answer() {
        // The reply to a request this chat can't answer, kept so it isn't sent again.
        let (mut f, mut out) = (OpenCodeFollow::default(), Vec::new());
        f.wrote(&json!({"id": 4, "error": {"code": -32601, "message": "x"}}), &mut out);
        assert_eq!(out, [Said::Settled("4".into())]);
    }
}
