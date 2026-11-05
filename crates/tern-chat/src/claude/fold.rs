//! Folding Claude Code's stream-json frames (and its saved `.jsonl`) into
//! a chat's [`Transcript`].

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::{Value, json};

use crate::model::*;

/// What folding Claude's frames keeps beside the transcript.
#[derive(Debug, Default)]
pub struct ClaudeFold {
    /// How full the context window is, from the frames' usage.
    pub(crate) meter: tern_bridge::context::Meter,
    /// The streamed message's blocks: block index to entry.
    blocks: HashMap<u64, usize>,
    /// The entries streamed for each message, and how many of them its
    /// whole `assistant` frames (one per block) have settled.
    streamed: HashMap<String, (Vec<usize>, usize)>,
    /// The message streaming now.
    stream_msg: Option<String>,
    /// Tool calls by id.
    tools: HashMap<String, usize>,
    /// Permission requests by request id.
    asks: HashMap<String, usize>,
    /// The ids of the messages sent from here, so their replays aren't shown twice.
    pub(crate) sent: HashSet<String>,
    /// The rate limit's last status (`allowed`, `allowed_warning`,
    /// `rejected`), to tell only a change.
    rate_limit: Option<String>,
}

impl ClaudeFold {
    /// Fold `v` into `t`; the reply a control request needs when the chat
    /// doesn't answer it itself.
    pub fn apply(&mut self, t: &mut Transcript, v: &Value) -> Option<Value> {
        Folder { t, f: self }.apply(v)
    }

    /// [`Self::apply`] for one line Claude wrote.
    pub fn apply_line(&mut self, t: &mut Transcript, line: &str) -> Option<Value> {
        Folder { t, f: self }.apply_line(line)
    }

    /// Nothing streams any more.
    pub fn settle(&mut self, t: &mut Transcript) {
        Folder { t, f: self }.settle();
    }

    /// A saved conversation, read from its `.jsonl` (see
    /// [`crate::transcript_path`]), up to the lines saved at or after
    /// `before` (ms since the Unix epoch: those of a program still running,
    /// which it replays), with `extra` lines (ms since the Unix epoch,
    /// frame) Claude doesn't save, e.g. MCP forms (see [`read_record`]),
    /// each where its time puts it.
    pub fn load(&mut self, path: &Path, before: Option<u64>, mut extra: Vec<(u64, Value)>) -> std::io::Result<Transcript> {
        let text = std::fs::read_to_string(path)?;
        let mut t = Transcript::default();
        let mut f = Folder { t: &mut t, f: self };
        extra.retain(|(ms, _)| before.is_none_or(|b| *ms < b));
        extra.sort_by_key(|(ms, _)| *ms);
        let mut extra = extra.into_iter().peekable();
        // The saved lines have no `result`: each turn ends where the next
        // user message starts, after as long as its lines' timestamps span.
        let mut turn: Option<(u64, u64)> = None;
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            let at = v.get("timestamp").and_then(Value::as_str).and_then(timestamp_ms);
            if let (Some(before), Some(at)) = (before, at)
                && at >= before
            {
                break;
            }
            if let Some(at) = at {
                while let Some((_, v)) = extra.next_if(|(ms, _)| *ms <= at) {
                    f.apply(&v);
                }
            }
            // The rest are Claude's own records (titles, file snapshots,
            // requests made), not the conversation.
            if !matches!(v.get("type").and_then(Value::as_str), Some("user" | "assistant" | "system")) {
                continue;
            }
            let users = f.t.entries.iter().filter(|e| matches!(e, Entry::User { .. })).count();
            let len = f.t.entries.len();
            f.apply(&v);
            let started = f.t.entries.iter().filter(|e| matches!(e, Entry::User { .. })).count() > users;
            if started {
                if let Some((from, to)) = turn.take() {
                    f.t.entries.insert(len, Entry::TurnEnd(TurnEnd { ms: to.saturating_sub(from), interrupted: false, error: None }));
                    f.reindex();
                }
                turn = at.map(|a| (a, a));
            } else if let (Some((_, to)), Some(a)) = (turn.as_mut(), at) {
                *to = a;
            }
        }
        for (_, v) in extra {
            f.apply(&v);
        }
        if let Some((from, to)) = turn {
            f.t.entries.push(Entry::TurnEnd(TurnEnd { ms: to.saturating_sub(from), interrupted: false, error: None }));
        }
        f.settle();
        Ok(t)
    }
}

/// A transcript and the fold's state, while a frame folds in.
struct Folder<'a> {
    t: &'a mut Transcript,
    f: &'a mut ClaudeFold,
}

impl Folder<'_> {
    /// Find tools and requests again after entries moved.
    fn reindex(&mut self) {
        self.f.tools.clear();
        self.f.asks.clear();
        for (i, e) in self.t.entries.iter().enumerate() {
            match e {
                Entry::Tool(t) => {
                    self.f.tools.insert(t.id.clone(), i);
                },
                Entry::Ask(a) => {
                    self.f.asks.insert(a.request_id.clone(), i);
                },
                Entry::Elicit(e) => {
                    self.f.asks.insert(e.request_id.clone(), i);
                },
                _ => {},
            }
        }
    }

    /// Fold in one frame. Returns the reply a control request needs when
    /// the chat doesn't answer it itself (one it doesn't handle).
    pub(crate) fn apply(&mut self, v: &Value) -> Option<Value> {
        // A subagent's own messages: its Task call shows its result.
        if v.get("parent_tool_use_id").is_some_and(|p| !p.is_null()) || v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            return None;
        }
        self.t.version += 1;
        self.f.meter.read(v);
        self.t.context = self.f.meter.reading();
        let Some(kind) = v.get("type").and_then(Value::as_str) else {
            self.raw("A message without a type".into(), v);
            return None;
        };
        match kind {
            "system" => self.system(v),
            "stream_event" => self.stream(v.get("event")?),
            "assistant" => self.assistant(v),
            "user" => self.user(v),
            "result" => self.result(v),
            "control_request" => return self.control_request(v),
            "control_response" => self.control_response(v.get("response")?),
            "control_cancel_request" => {
                let id = v.get("request_id").and_then(Value::as_str)?;
                self.t.set_answer(id, Answer::Withdrawn);
            },
            "rate_limit_event" => self.rate_limit(v),
            "auth_status" => {
                let lines: Vec<&str> = v.get("output").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
                let error = v.get("error").and_then(Value::as_str).filter(|e| !e.is_empty());
                let text = error.map(str::to_owned).unwrap_or_else(|| lines.join("\n"));
                if !text.trim().is_empty() {
                    self.t.entries.push(Entry::Notice { text, error: error.is_some() });
                }
            },
            // How queued messages stand, and a running tool's heartbeat: the
            // chat shows the turn working its own way.
            "command_lifecycle" | "keep_alive" | "tool_progress" => {},
            // `/clear`: its echo shows, and a fresh `init` follows.
            "conversation_reset" => self.t.handoff = Some(None),
            other => self.raw(format!("A message of type “{other}”"), v),
        }
        None
    }

    /// [`Self::apply`] for one line Claude wrote: a line that isn't JSON is
    /// shown as it is (lines in a row go together).
    pub(crate) fn apply_line(&mut self, line: &str) -> Option<Value> {
        if line.trim().is_empty() {
            return None;
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            return self.apply(&v);
        }
        self.t.version += 1;
        match self.t.entries.last_mut() {
            Some(Entry::Raw { title, body }) if title == NOT_JSON => {
                body.push('\n');
                body.push_str(line);
            },
            _ => self.t.entries.push(Entry::Raw { title: NOT_JSON.into(), body: line.to_owned() }),
        }
        None
    }

    /// A rate limit near or reached: told once each time it changes.
    fn rate_limit(&mut self, v: &Value) {
        let info = v.get("rate_limit_info");
        let status = info.and_then(|i| i.get("status")).and_then(Value::as_str).unwrap_or("allowed").to_owned();
        if self.f.rate_limit.as_deref() == Some(status.as_str()) {
            return;
        }
        let first = self.f.rate_limit.replace(status.clone()).is_none();
        let kind = info.and_then(|i| i.get("rateLimitType")).and_then(Value::as_str).unwrap_or("usage").replace('_', " ");
        let text = match status.as_str() {
            "allowed" if first => return,
            "allowed" => format!("The {kind} limit is clear again"),
            "allowed_warning" => match info.and_then(|i| i.get("utilization")).and_then(Value::as_f64) {
                Some(u) => format!("Near the {kind} limit ({:.0}% used)", if u <= 1.0 { u * 100.0 } else { u }),
                None => format!("Near the {kind} limit"),
            },
            "rejected" => format!("The {kind} limit is reached"),
            other => format!("Rate limit: {other} ({kind})"),
        };
        self.t.entries.push(Entry::Notice { error: status == "rejected", text });
    }

    /// Show `v` as it came, called `title`.
    fn raw(&mut self, title: String, v: &Value) {
        self.t.entries.push(raw_entry(title, v));
    }

    fn system(&mut self, v: &Value) {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        match v.get("subtype").and_then(Value::as_str) {
            Some("init") => {
                self.t.session_id = s("session_id").or(self.t.session_id.take());
                self.t.model = s("model");
                self.t.permission_mode = s("permissionMode");
            },
            Some("compact_boundary") => {
                let how = v.pointer("/compact_metadata/trigger").and_then(Value::as_str);
                let before = v.pointer("/compact_metadata/pre_tokens").and_then(Value::as_u64);
                let text = match (how, before) {
                    (Some(how), Some(n)) => format!("Context compacted ({how}, from {}k tokens)", n / 1000),
                    (Some(how), None) => format!("Context compacted ({how})"),
                    _ => "Context compacted".to_owned(),
                };
                self.t.entries.push(Entry::Notice { text, error: false });
            },
            Some("commands_changed") => {
                if let Some(Value::Array(commands)) = v.get("commands") {
                    self.t.commands = command_list(commands);
                }
            },
            Some("hook_response") => {
                if v.get("outcome").and_then(Value::as_str).is_some_and(|o| o != "success") {
                    let name = v.get("hook_name").and_then(Value::as_str).unwrap_or("A hook");
                    let out = ["stderr", "stdout", "output"]
                        .iter()
                        .filter_map(|k| v.get(*k).and_then(Value::as_str))
                        .find(|t| !t.trim().is_empty());
                    let text = match out {
                        Some(out) => format!("{name} failed: {}", out.trim()),
                        None => format!("{name} failed"),
                    };
                    self.t.entries.push(Entry::Notice { text, error: true });
                }
            },
            Some("stop_hook_summary") => {
                for e in v.get("hookErrors").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
                    self.t.entries.push(Entry::Notice { text: format!("Stop hook: {e}"), error: false });
                }
            },
            Some("api_retry") => {
                let n = |k: &str| v.get(k).and_then(Value::as_u64).map_or("?".to_owned(), |n| n.to_string());
                let why = v.get("error").map(|e| e.as_str().map_or_else(|| e.to_string(), str::to_owned)).unwrap_or_default();
                let text = format!("Retrying after an API error (attempt {} of {}) {why}", n("attempt"), n("max_retries"));
                self.t.entries.push(Entry::Notice { text: text.trim().to_owned(), error: false });
            },
            Some("api_error") => {
                let why = v.get("error").map(|e| e.as_str().map_or_else(|| e.to_string(), str::to_owned)).unwrap_or_default();
                self.t.entries.push(Entry::Notice { text: format!("API error: {why}"), error: true });
            },
            Some("permission_denied") => {
                let tool = v.get("tool_name").and_then(Value::as_str).unwrap_or("A tool");
                let text = v
                    .get("message")
                    .and_then(Value::as_str)
                    .map_or_else(|| format!("{tool} was denied"), |m| format!("{tool} was denied: {m}"));
                self.t.entries.push(Entry::Notice { text, error: true });
            },
            Some("memory_saved") => {
                let paths: Vec<&str> =
                    v.get("writtenPaths").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
                self.t.entries.push(Entry::Notice { text: format!("Saved to memory: {}", paths.join(", ")), error: false });
            },
            // Progress the chat shows its own way (working, hooks and tasks
            // running, thinking), and the saved files' bookkeeping.
            Some(
                "status"
                | "hook_started"
                | "hook_progress"
                | "turn_duration"
                | "thinking_tokens"
                | "task_progress"
                | "task_updated"
                | "background_tasks_changed"
                | "session_state_changed"
                | "files_persisted"
                | "elicitation_complete",
            ) => {},
            subtype => match ["content", "text", "summary", "description"]
                .iter()
                .find_map(|k| v.get(*k).and_then(Value::as_str))
                .map(strip_command_tags)
            {
                // A slash command that printed nothing.
                Some(text) if text.is_empty() && subtype == Some("local_command") => {},
                Some(text) if !text.is_empty() => {
                    let error = v.get("level").and_then(Value::as_str) == Some("error");
                    self.t.entries.push(Entry::Notice { text, error });
                },
                _ => self.raw(format!("A system message “{}”", subtype.unwrap_or("?")), v),
            },
        }
    }

    fn stream(&mut self, ev: &Value) {
        let index = ev.get("index").and_then(Value::as_u64);
        match ev.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                self.f.stream_msg = ev.pointer("/message/id").and_then(Value::as_str).map(str::to_owned);
                self.f.blocks.clear();
            },
            Some("content_block_start") => {
                let (Some(index), Some(block)) = (index, ev.get("content_block")) else { return };
                let ix = self.push(new_entry(block, true));
                self.f.blocks.insert(index, ix);
                if let Some(msg) = &self.f.stream_msg {
                    self.f.streamed.entry(msg.clone()).or_default().0.push(ix);
                }
            },
            Some("content_block_delta") => {
                let (Some(&ix), Some(delta)) = (index.and_then(|i| self.f.blocks.get(&i)), ev.get("delta")) else { return };
                let piece = |k: &str| delta.get(k).and_then(Value::as_str).unwrap_or_default();
                match (self.t.entries.get_mut(ix), delta.get("type").and_then(Value::as_str)) {
                    (Some(Entry::Text { text, .. }), Some("text_delta")) => text.push_str(piece("text")),
                    (Some(Entry::Thinking { text, .. }), Some("thinking_delta")) => text.push_str(piece("thinking")),
                    (Some(Entry::Tool(t)), Some("input_json_delta")) => t.partial.push_str(piece("partial_json")),
                    _ => {},
                }
            },
            Some("content_block_stop") => {
                let Some(&ix) = index.and_then(|i| self.f.blocks.get(&i)) else { return };
                match self.t.entries.get_mut(ix) {
                    Some(Entry::Text { streaming, .. } | Entry::Thinking { streaming, .. }) => *streaming = false,
                    Some(Entry::Tool(t)) => {
                        if let Ok(input) = serde_json::from_str(&t.partial) {
                            t.input = input;
                        }
                    },
                    _ => {},
                }
            },
            // The whole message follows as an `assistant` frame.
            Some("message_delta" | "message_stop" | "ping") => {},
            Some("error") => {
                let text = ev.pointer("/error/message").and_then(Value::as_str).unwrap_or("The response stopped with an error");
                self.t.entries.push(Entry::Notice { text: text.to_owned(), error: true });
            },
            other => self.raw(format!("A stream event “{}”", other.unwrap_or("?")), ev),
        }
    }

    fn assistant(&mut self, v: &Value) {
        let msg = v.pointer("/message/id").and_then(Value::as_str).unwrap_or_default().to_owned();
        let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) else { return };
        for block in blocks {
            let whole = new_entry(block, false);
            // The entry its stream made, if it streamed.
            let streamed = self.f.streamed.get_mut(&msg).and_then(|(ixs, used)| {
                let ix = ixs.get(*used).copied()?;
                *used += 1;
                Some(ix)
            });
            match streamed {
                Some(ix) => {
                    if let (Some(Entry::Tool(t)), Entry::Tool(w)) = (self.t.entries.get_mut(ix), &whole) {
                        t.input = w.input.clone();
                    } else if let Some(e) = self.t.entries.get_mut(ix) {
                        *e = whole;
                    }
                },
                None => {
                    self.push(whole);
                },
            }
        }
    }

    fn user(&mut self, v: &Value) {
        let uuid = v.get("uuid").and_then(Value::as_str).unwrap_or_default();
        if self.f.sent.contains(uuid) || v.get("isMeta").and_then(Value::as_bool) == Some(true) {
            return;
        }
        // What the tool reported besides its text: `tool_use_result` in the
        // stream, `toolUseResult` in the saved lines.
        let meta = v.get("tool_use_result").or_else(|| v.get("toolUseResult"));
        match v.pointer("/message/content") {
            Some(Value::String(text)) => self.user_text(text, Vec::new()),
            Some(Value::Array(blocks)) => {
                let mut text = String::new();
                let mut images = Vec::new();
                // Parts it can't show go after the message they came with.
                let mut extra = Vec::new();
                for b in blocks {
                    match b.get("type").and_then(Value::as_str) {
                        Some("tool_result") => {
                            let id = b.get("tool_use_id").and_then(Value::as_str).unwrap_or_default();
                            let error = b.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                            let out = result_text(b.get("content"));
                            match self.f.tools.get(id).and_then(|&i| self.t.entries.get_mut(i)) {
                                Some(Entry::Tool(t)) => {
                                    t.result = Some(out);
                                    t.error = error;
                                    t.patch = meta.and_then(crate::summary::result_patch);
                                    t.images = result_images(b.get("content"));
                                },
                                _ => extra.push(raw_entry("The result of a tool call not shown here".into(), b)),
                            }
                        },
                        Some("text") => {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(b.get("text").and_then(Value::as_str).unwrap_or_default());
                        },
                        Some("image") => match UserImage::from_block(b) {
                            Some(image) => images.push(image),
                            None => extra.push(raw_entry("An image this chat can't draw".into(), b)),
                        },
                        other => extra.push(raw_entry(format!("Content of type “{}”", other.unwrap_or("?")), b)),
                    }
                }
                if !text.is_empty() || !images.is_empty() {
                    self.user_text(&text, images);
                }
                self.t.entries.extend(extra);
            },
            Some(other) => self.raw("A user message of another shape".into(), other),
            None => {},
        }
    }

    /// A user message from the CLI: a replay of one typed elsewhere, a saved
    /// one, or a line the CLI writes as the user (an interruption).
    fn user_text(&mut self, text: &str, images: Vec<UserImage>) {
        let t = text.trim();
        if t.starts_with("[Request interrupted") {
            self.t.entries.push(Entry::Notice { text: "Interrupted".into(), error: false });
            return;
        }
        // Notes the CLI writes for the model, not for the user.
        if ["<system-reminder", "<local-command-caveat", "Caveat:"].iter().any(|p| t.starts_with(p)) {
            return;
        }
        // A slash command the user ran, or a shell command (`!`): what was typed.
        if let Some(name) = tag(t, "command-name") {
            let args = tag(t, "command-args").unwrap_or_default();
            let typed = if args.is_empty() { name.to_owned() } else { format!("{name} {args}") };
            self.t.entries.push(Entry::User { text: typed, images });
            return;
        }
        if let Some(cmd) = tag(t, "bash-input") {
            self.t.entries.push(Entry::User { text: format!("! {cmd}"), images });
            return;
        }
        // What a command printed.
        if t.starts_with("<local-command-") || t.starts_with("<bash-") {
            let error = t.starts_with("<local-command-stderr") || t.starts_with("<bash-stderr");
            let out = strip_command_tags(t);
            if !out.is_empty() {
                self.t.entries.push(Entry::Notice { text: out, error });
            }
            return;
        }
        // A background task's news, as its summary.
        if t.starts_with("<task-notification") {
            let text = tag(t, "summary").map_or_else(|| t.to_owned(), str::to_owned);
            self.t.entries.push(Entry::Notice { text, error: false });
            return;
        }
        self.t.entries.push(Entry::User { text: text.to_owned(), images });
    }

    fn result(&mut self, v: &Value) {
        let subtype = v.get("subtype").and_then(Value::as_str).unwrap_or_default();
        let is_error = v.get("is_error").and_then(Value::as_bool).unwrap_or(false);
        let reason = v.get("terminal_reason").and_then(Value::as_str);
        let errors: Vec<&str> = v.get("errors").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
        // A stopped turn: the CLI says it was aborted (older ones only
        // that it ended during execution, with no errors).
        let interrupted = match reason {
            Some(r) => r.starts_with("aborted"),
            None => subtype == "error_during_execution" && errors.is_empty(),
        };
        let error = (!interrupted && (is_error || subtype != "success")).then(|| {
            let said = v.get("result").and_then(Value::as_str).filter(|r| !r.trim().is_empty());
            match (said, errors.is_empty()) {
                (Some(r), _) => r.to_owned(),
                (None, false) => errors.join("\n"),
                (None, true) => match subtype {
                    "error_max_turns" => "Stopped: the turn limit was reached".to_owned(),
                    "error_max_budget_usd" => "Stopped: the budget was reached".to_owned(),
                    _ => match reason {
                        Some(r) => format!("The turn ended with an error ({subtype}, {r})"),
                        None => format!("The turn ended with an error ({subtype})"),
                    },
                },
            }
        });
        if let Some(path) = v.get("result").and_then(Value::as_str).and_then(tern_bridge::context::handoff_path) {
            self.t.handoff = Some(Some(path));
        }
        let ms = v.get("duration_ms").and_then(Value::as_u64).unwrap_or(0);
        // The turn's end says it was stopped.
        if interrupted && matches!(self.t.entries.last(), Some(Entry::Notice { text, .. }) if text == "Interrupted") {
            self.t.entries.pop();
        }
        self.settle();
        self.t.entries.push(Entry::TurnEnd(TurnEnd { ms, interrupted, error }));
        self.t.busy = false;
    }

    /// Claude's answer to a request of the chat's (`initialize`), or a
    /// chat's answer to one of Claude's (replayed).
    fn control_response(&mut self, r: &Value) {
        let id = r.get("request_id").and_then(Value::as_str).unwrap_or_default();
        let body = r.get("response");
        if let Some(Value::Array(commands)) = body.and_then(|b| b.get("commands")) {
            let s = |c: &Value, k: &str| c.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
            self.t.commands = command_list(commands);
            if let Some(Value::Array(models)) = body.and_then(|b| b.get("models")) {
                self.t.models = models
                    .iter()
                    .map(|m| Model { value: s(m, "value"), name: s(m, "displayName"), description: s(m, "description") })
                    .filter(|m| !m.value.is_empty())
                    .collect();
            }
            if let Some(mode) = body.and_then(|b| b.get("current_permission_mode")).and_then(Value::as_str) {
                self.t.permission_mode = Some(mode.to_owned());
            }
            return;
        }
        // A replayed answer to a form.
        if let Some(Entry::Elicit(e)) = self.f.asks.get(id).and_then(|&i| self.t.entries.get(i))
            && e.answer.is_none()
        {
            let answer = match body.and_then(|b| b.get("action")).and_then(Value::as_str) {
                Some("accept") => ElicitAnswer::Accept(body.and_then(|b| b.get("content")).cloned().unwrap_or(Value::Null)),
                _ => ElicitAnswer::Decline,
            };
            self.t.set_elicit_answer(id, answer);
            return;
        }
        let answer = match body.and_then(|b| b.get("behavior")).and_then(Value::as_str) {
            Some("allow") => Answer::Allow,
            Some("deny") => Answer::Deny,
            _ => return,
        };
        if matches!(self.f.asks.get(id).and_then(|&i| self.t.entries.get(i)), Some(Entry::Ask(a)) if a.answer.is_none()) {
            self.t.set_answer(id, answer);
        }
    }

    fn control_request(&mut self, v: &Value) -> Option<Value> {
        let id = v.get("request_id").and_then(Value::as_str)?.to_owned();
        let req = v.get("request")?;
        match req.get("subtype").and_then(Value::as_str) {
            Some("can_use_tool") => {
                let s = |k: &str| req.get(k).and_then(Value::as_str).map(str::to_owned);
                let ask = Ask {
                    request_id: id.clone(),
                    tool_name: s("tool_name").unwrap_or_default(),
                    tool_use_id: s("tool_use_id"),
                    input: req.get("input").cloned().unwrap_or_else(|| json!({})),
                    suggestions: req.get("permission_suggestions").cloned().unwrap_or(Value::Null),
                    answer: None,
                };
                let ix = self.push(Entry::Ask(ask));
                self.f.asks.insert(id, ix);
                None
            },
            // An MCP server asks the user for input.
            Some("elicitation") => {
                let s = |k: &str| req.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
                let schema = req.get("requested_schema");
                let required: Vec<&str> = schema
                    .and_then(|s| s.get("required"))
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                let fields = schema
                    .and_then(|s| s.get("properties"))
                    .and_then(Value::as_object)
                    .map(|props| props.iter().map(|(name, p)| Field::parse(name, p, required.contains(&name.as_str()))).collect())
                    .unwrap_or_default();
                let url = (s("mode") == "url").then(|| s("url")).filter(|u| !u.is_empty());
                let e = Elicit { request_id: id.clone(), server: s("mcp_server_name"), message: s("message"), url, fields, answer: None };
                let ix = self.push(Entry::Elicit(e));
                self.f.asks.insert(id, ix);
                None
            },
            other => {
                self.raw(format!("A request this chat can't answer: “{}”", other.unwrap_or("?")), req);
                Some(json!({
                "type": "control_response",
                "response": { "subtype": "error", "request_id": id, "error": format!("unsupported request {}", other.unwrap_or("?")) },
                }))
            },
        }
    }

    fn push(&mut self, e: Entry) -> usize {
        if let Entry::Tool(t) = &e {
            self.f.tools.insert(t.id.clone(), self.t.entries.len());
        }
        self.t.entries.push(e);
        self.t.entries.len() - 1
    }

    /// Nothing streams any more (see [`Transcript::settle`]).
    pub(crate) fn settle(&mut self) {
        self.t.settle();
        self.f.blocks.clear();
        self.f.stream_msg = None;
    }
}

/// The entry a content block starts.
fn new_entry(block: &Value, streaming: bool) -> Entry {
    let s = |k: &str| block.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
    match block.get("type").and_then(Value::as_str).unwrap_or_default() {
        "text" => Entry::Text { text: with_citations(s("text"), block), streaming },
        "thinking" => Entry::Thinking { text: s("thinking"), streaming },
        "tool_use" => Entry::Tool(Tool {
            id: s("id"),
            name: s("name"),
            input: block.get("input").cloned().unwrap_or_else(|| json!({})),
            partial: String::new(),
            result: None,
            error: false,
            patch: None,
            images: Vec::new(),
        }),
        "redacted_thinking" => raw_entry("Thinking, redacted".into(), block),
        other => raw_entry(format!("Content of type “{other}”"), block),
    }
}

/// The lines a chat's record holds (`{"ms": …, "line": …}` each, as the
/// daemon writes them): for [`ClaudeFold::load`]. None when there is
/// no record.
pub fn read_record(path: &Path) -> Vec<(u64, Value)> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| Some((v.get("ms")?.as_u64()?, v.get("line")?.clone())))
        .collect()
}

/// The title of the lines Claude wrote that aren't JSON.
const NOT_JSON: &str = "Output that isn't JSON";

fn raw_entry(title: String, v: &Value) -> Entry {
    Entry::Raw { title, body: serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string()) }
}

/// `text` with the sources it cites (a text block's `citations`) after it.
fn with_citations(text: String, block: &Value) -> String {
    let Some(Value::Array(cites)) = block.get("citations") else { return text };
    let mut seen = HashSet::new();
    let sources: Vec<String> = cites
        .iter()
        .filter_map(|c| {
            let url = c.get("url").and_then(Value::as_str);
            let title = c.get("title").and_then(Value::as_str).or(url).or_else(|| c.get("document_title").and_then(Value::as_str))?;
            seen.insert((title, url)).then(|| match url {
                Some(url) => format!("- [{title}]({url})"),
                None => format!("- {title}"),
            })
        })
        .collect();
    if sources.is_empty() { text } else { format!("{text}\n\nSources:\n{}", sources.join("\n")) }
}

/// The slash commands of an `initialize` answer or a `commands_changed`.
fn command_list(commands: &[Value]) -> Vec<(String, String)> {
    let s = |c: &Value, k: &str| c.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
    commands.iter().map(|c| (s(c, "name"), s(c, "description"))).filter(|c| !c.0.is_empty()).collect()
}

/// The text inside `<name>…</name>` in `text`, trimmed.
fn tag<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&format!("</{name}>"))? + start;
    Some(text[start..end].trim())
}

/// `text` without the CLI's command output tags (`<local-command-stdout>`,
/// `<bash-stderr>`, ...), trimmed.
fn strip_command_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        let name = after.trim_start_matches(['<', '/']);
        let is_tag = ["local-command-", "bash-"].iter().any(|p| name.starts_with(p));
        match after.find('>') {
            Some(j) if is_tag => rest = &after[j + 1..],
            _ => {
                out.push('<');
                rest = &after[1..];
            },
        }
    }
    out.push_str(rest);
    out.trim().to_owned()
}

/// The images in a tool result's content.
fn result_images(content: Option<&Value>) -> Vec<UserImage> {
    match content {
        Some(Value::Array(parts)) => {
            parts.iter().filter(|p| p.get("type").and_then(Value::as_str) == Some("image")).filter_map(UserImage::from_block).collect()
        },
        _ => Vec::new(),
    }
}

/// A tool result's text: its content's text blocks (or string).
fn result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| match p.get("type").and_then(Value::as_str) {
                Some("text") => p.get("text").and_then(Value::as_str).map(str::to_owned),
                Some("image") => Some("[image]".to_owned()),
                Some("tool_reference") => Some(format!("[tool {}]", p.get("tool_name").and_then(Value::as_str).unwrap_or("?"))),
                _ => Some(serde_json::to_string_pretty(p).unwrap_or_else(|_| p.to_string())),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Milliseconds since 1970 of an RFC 3339 UTC time (`2026-10-30T07:49:25.177Z`).
fn timestamp_ms(s: &str) -> Option<u64> {
    let (date, time) = s.strip_suffix('Z')?.split_once('T')?;
    let mut d = date.splitn(3, '-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let (hms, frac) = time.split_once('.').unwrap_or((time, "0"));
    let mut t = hms.splitn(3, ':').map(|p| p.parse::<i64>().ok());
    let (h, min, sec) = (t.next()??, t.next()??, t.next()??);
    let ms: i64 = format!("{frac:0<3}")[..3].parse().ok()?;
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(((days * 24 + h) * 60 + min) * 60_000 + sec * 1000 + ms).ok()
}

/// A `control_response` answering request `id` with `response`.
pub(crate) fn success(id: &str, response: Value) -> Value {
    json!({ "type": "control_response", "response": { "subtype": "success", "request_id": id, "response": response } })
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;

    use super::*;

    /// A transcript with Claude's fold beside it, read as the transcript.
    #[derive(Default)]
    struct Transcript {
        t: crate::model::Transcript,
        fold: ClaudeFold,
    }

    impl Transcript {
        fn apply(&mut self, v: &Value) -> Option<Value> {
            self.fold.apply(&mut self.t, v)
        }

        fn apply_line(&mut self, line: &str) -> Option<Value> {
            self.fold.apply_line(&mut self.t, line)
        }

        fn load_with(path: &Path, before: Option<u64>, extra: Vec<(u64, Value)>) -> std::io::Result<Self> {
            let mut fold = ClaudeFold::default();
            Ok(Self { t: fold.load(path, before, extra)?, fold })
        }

        fn load_before(path: &Path, before: Option<u64>) -> std::io::Result<Self> {
            Self::load_with(path, before, Vec::new())
        }

        fn load(path: &Path) -> std::io::Result<Self> {
            Self::load_with(path, None, Vec::new())
        }

        /// As the driver's `user_line` then the chat's `push_user`.
        fn push_user(&mut self, text: &str, images: Vec<UserImage>, uuid: &str) {
            self.fold.sent.insert(uuid.to_owned());
            self.t.push_user(text, images);
        }
    }

    impl std::ops::Deref for Transcript {
        type Target = crate::model::Transcript;
        fn deref(&self) -> &Self::Target {
            &self.t
        }
    }

    impl std::ops::DerefMut for Transcript {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.t
        }
    }

    fn feed(t: &mut Transcript, frames: &[Value]) {
        for f in frames {
            t.apply(f);
        }
    }

    #[test]
    fn a_block_it_doesnt_know_shows_as_it_came_without_doubling_the_reply() {
        let mut t = Transcript::default();
        feed(
            &mut t,
            &[
                json!({"type":"stream_event","event":{"type":"message_start","message":{"id":"m1"}}}),
                json!({"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"redacted_thinking","data":"x"}}}),
                json!({"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}}),
                json!({"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hi"}}}),
                json!({"type":"assistant","message":{"id":"m1","content":[{"type":"redacted_thinking","data":"x"},{"type":"text","text":"Hi"}]}}),
                json!({"type":"some_future_frame","anything":1}),
            ],
        );
        let [Entry::Raw { title, body }, Entry::Text { text, streaming: false }, Entry::Raw { title: frame, .. }] = &t.entries[..] else {
            panic!("{:?}", t.entries)
        };
        assert_eq!((title.as_str(), text.as_str()), ("Thinking, redacted", "Hi"));
        assert!(body.contains("redacted_thinking"));
        assert!(frame.contains("some_future_frame"));
    }

    #[test]
    fn bookkeeping_is_skipped_and_the_rest_is_told() {
        let mut t = Transcript::default();
        feed(
            &mut t,
            &[
                json!({"type":"command_lifecycle","state":"queued"}),
                json!({"type":"rate_limit_event","rate_limit_info":{}}),
                json!({"type":"system","subtype":"status","status":"requesting"}),
                json!({"type":"system","subtype":"hook_started","hook_name":"SessionStart"}),
                json!({"type":"system","subtype":"hook_response","hook_name":"Stop","outcome":"success","stdout":"{}"}),
                json!({"type":"system","subtype":"commands_changed","commands":[{"name":"review","description":"Review"}]}),
                json!({"type":"stream_event","event":{"type":"ping"}}),
            ],
        );
        assert!(t.entries.is_empty(), "{:?}", t.entries);
        assert_eq!(t.commands, [("review".to_owned(), "Review".to_owned())]);
        feed(
            &mut t,
            &[
                json!({"type":"system","subtype":"hook_response","hook_name":"Stop","outcome":"error","stderr":"boom\n"}),
                json!({"type":"system","subtype":"informational","content":"Safeguards stopped the response","level":"notice"}),
                json!({"type":"system","subtype":"local_command","content":"<local-command-stdout></local-command-stdout>"}),
                json!({"type":"system","subtype":"brand_new","detail":1}),
                json!({"type":"stream_event","event":{"type":"error","error":{"message":"Overloaded"}}}),
            ],
        );
        let shown: Vec<String> = t
            .entries
            .iter()
            .map(|e| match e {
                Entry::Notice { text, error } => format!("{text} {error}"),
                Entry::Raw { title, .. } => title.clone(),
                e => format!("{e:?}"),
            })
            .collect();
        assert_eq!(
            shown,
            ["Stop failed: boom true", "Safeguards stopped the response false", "A system message “brand_new”", "Overloaded true"]
        );
    }

    #[test]
    fn what_the_cli_reports_is_told_once_and_heartbeats_are_not() {
        let mut t = Transcript::default();
        let limit = |status: &str| json!({"type":"rate_limit_event","rate_limit_info":{"status":status,"rateLimitType":"five_hour","utilization":0.91}});
        feed(
            &mut t,
            &[
                limit("allowed"),
                json!({"type":"tool_progress","tool_use_id":"t1","elapsed_time_seconds":3}),
                json!({"type":"system","subtype":"thinking_tokens","estimated_tokens":50}),
                json!({"type":"system","subtype":"task_progress","description":"step"}),
                limit("allowed_warning"),
                limit("allowed_warning"),
                limit("rejected"),
                limit("allowed"),
                json!({"type":"system","subtype":"notification","text":"Update available","key":"k"}),
                json!({"type":"system","subtype":"api_retry","attempt":2,"max_retries":10,"error":"overloaded"}),
                json!({"type":"system","subtype":"stop_hook_summary","hookErrors":["write a handoff"],"hookCount":1}),
                json!({"type":"system","subtype":"compact_boundary","compact_metadata":{"trigger":"auto","pre_tokens":154000}}),
            ],
        );
        let shown: Vec<String> = t
            .entries
            .iter()
            .map(|e| match e {
                Entry::Notice { text, error } => format!("{text} {error}"),
                e => format!("{e:?}"),
            })
            .collect();
        assert_eq!(
            shown,
            [
                "Near the five hour limit (91% used) false",
                "The five hour limit is reached true",
                "The five hour limit is clear again false",
                "Update available false",
                "Retrying after an API error (attempt 2 of 10) overloaded false",
                "Stop hook: write a handoff false",
                "Context compacted (auto, from 154k tokens) false",
            ]
        );
    }

    #[test]
    fn a_turn_ends_stopped_or_failed_as_the_cli_says() {
        let end = |v: Value| {
            let mut t = Transcript::default();
            t.apply(&v);
            let Some(Entry::TurnEnd(e)) = t.entries.last().cloned() else { panic!("{:?}", t.entries) };
            (e.interrupted, e.error)
        };
        let stopped = json!({"type":"result","subtype":"error_during_execution","is_error":true,"terminal_reason":"aborted_streaming"});
        assert_eq!(end(stopped), (true, None));
        let failed = json!({"type":"result","subtype":"error_during_execution","is_error":true,"terminal_reason":"model_error","errors":["API Error: 500"]});
        assert_eq!(end(failed), (false, Some("API Error: 500".into())));
        let turns = json!({"type":"result","subtype":"error_max_turns","is_error":true,"terminal_reason":"max_turns"});
        assert_eq!(end(turns), (false, Some("Stopped: the turn limit was reached".into())));
        // An older CLI: no reason, no errors.
        assert_eq!(end(json!({"type":"result","subtype":"error_during_execution","is_error":true})), (true, None));
    }

    #[test]
    fn a_turn_ending_with_a_handoff_line_names_it() {
        let mut t = Transcript::default();
        t.apply(&json!({"type":"result","subtype":"success","result":"Nothing to hand off"}));
        assert_eq!(t.handoff.take(), None);
        t.apply(&json!({"type":"result","subtype":"success","result":"Done.
Handoff: `docs/h.md`"}));
        assert_eq!(t.handoff.take(), Some(Some("docs/h.md".to_owned())));
    }

    #[test]
    fn a_cleared_conversation_starts_over_quietly() {
        let mut t = Transcript::default();
        t.apply(&json!({"type":"assistant","message":{"id":"m","model":"m","content":[],"usage":{"input_tokens":10}}}));
        t.apply(&json!({"type":"result","subtype":"success","modelUsage":{"m":{"contextWindow":100}}}));
        assert_eq!(t.context.map(|r| r.percent), Some(10.));
        let before = t.entries.len();
        t.apply(&json!({"type":"conversation_reset"}));
        assert_eq!(t.entries.len(), before);
        assert_eq!(t.context, None);
        assert_eq!(t.handoff.take(), Some(None));
    }

    #[test]
    fn a_tool_result_keeps_its_images() {
        let mut t = Transcript::default();
        let data = STANDARD.encode([9u8, 9]);
        feed(
            &mut t,
            &[
                json!({"type":"assistant","message":{"id":"m","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/a.png"}}]}}),
                json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":[
                    {"type":"image","source":{"type":"base64","media_type":"image/png","data":data}}]}]}}),
            ],
        );
        let Entry::Tool(tool) = &t.entries[0] else { panic!() };
        assert_eq!((tool.result.as_deref(), tool.images.len()), (Some("[image]"), 1));
    }

    #[test]
    fn commands_and_their_output_show() {
        let mut t = Transcript::default();
        let user = |c: &str| json!({"type":"user","message":{"role":"user","content":c}});
        feed(
            &mut t,
            &[
                user("<command-name>/usage</command-name>\n<command-message>usage</command-message>\n<command-args></command-args>"),
                user("<local-command-stdout>22% used</local-command-stdout>"),
                user("<bash-input>ls</bash-input>"),
                user("<bash-stdout>a.txt</bash-stdout><bash-stderr></bash-stderr>"),
                user("<local-command-caveat>Caveat: the messages below…</local-command-caveat>"),
                user("<task-notification>\n<task-id>1</task-id>\n<summary>Build finished</summary>\n</task-notification>"),
            ],
        );
        let shown: Vec<String> = t
            .entries
            .iter()
            .map(|e| match e {
                Entry::User { text, .. } => format!("user {text}"),
                Entry::Notice { text, .. } => format!("notice {text}"),
                e => format!("{e:?}"),
            })
            .collect();
        assert_eq!(shown, ["user /usage", "notice 22% used", "user ! ls", "notice a.txt", "notice Build finished"]);
    }

    #[test]
    fn lines_that_arent_json_show_together() {
        let mut t = Transcript::default();
        t.apply_line("warning: something");
        t.apply_line("  more");
        t.apply_line("");
        t.apply_line(r#"{"type":"system","subtype":"compact_boundary"}"#);
        t.apply_line("again");
        let [Entry::Raw { body, .. }, Entry::Notice { .. }, Entry::Raw { body: again, .. }] = &t.entries[..] else {
            panic!("{:?}", t.entries)
        };
        assert_eq!((body.as_str(), again.as_str()), ("warning: something\n  more", "again"));
    }

    #[test]
    fn an_elicitation_waits_for_the_form() {
        let mut t = Transcript::default();
        let reply = t.apply(&json!({"type":"control_request","request_id":"e1","request":{
            "subtype":"elicitation","mcp_server_name":"deploy","message":"Where to?","mode":"form",
            "requested_schema":{"type":"object","required":["env"],"properties":{
                "env":{"type":"string","title":"Environment","enum":["dev","prod"],"enumNames":["Development","Production"]},
                "count":{"type":"integer","default":1},
                "force":{"type":"boolean","description":"Skip checks"},
                "tags":{"type":"array","items":{"anyOf":[{"const":"a","title":"A"},{"const":"b","title":"B"}]}},
                "note":{"type":"string"}}}}}));
        assert!(reply.is_none(), "it waits for the user");
        assert!(t.waiting());
        let Entry::Elicit(e) = &t.entries[0] else { panic!("{:?}", t.entries) };
        let kinds: Vec<(&str, &str, bool, &FieldKind)> =
            e.fields.iter().map(|f| (f.name.as_str(), f.title.as_str(), f.required, &f.kind)).collect();
        let choice =
            |o: &[(&str, &str)], many| FieldKind::Choice { options: o.iter().map(|(v, l)| (json!(v), (*l).to_owned())).collect(), many };
        assert_eq!(
            kinds,
            [
                // As the server ordered them.
                ("env", "Environment", true, &choice(&[("dev", "Development"), ("prod", "Production")], false)),
                ("count", "count", false, &FieldKind::Number { integer: true }),
                ("force", "force", false, &FieldKind::Bool),
                ("tags", "tags", false, &choice(&[("a", "A"), ("b", "B")], true)),
                ("note", "note", false, &FieldKind::Text),
            ]
        );
        let line = crate::wire::elicit(e, &ElicitAnswer::Accept(json!({"env":"dev"})));
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v.pointer("/response/request_id"), Some(&json!("e1")));
        assert_eq!(v.pointer("/response/response"), Some(&json!({"action":"accept","content":{"env":"dev"}})));
        // Its answer replayed settles it; so does Claude taking it back.
        t.apply(&v);
        assert!(!t.waiting());
        t.apply(&json!({"type":"control_request","request_id":"e2","request":{"subtype":"elicitation","mode":"url","url":"https://x","message":"Log in"}}));
        assert!(matches!(t.entries.last(), Some(Entry::Elicit(Elicit { url: Some(u), .. })) if u == "https://x"));
        t.apply(&json!({"type":"control_cancel_request","request_id":"e2"}));
        assert!(matches!(t.entries.last(), Some(Entry::Elicit(Elicit { answer: Some(ElicitAnswer::Withdrawn), .. }))));
    }

    #[test]
    fn a_request_it_cant_answer_is_refused_and_shown() {
        let mut t = Transcript::default();
        let reply = t.apply(&json!({"type":"control_request","request_id":"r1","request":{"subtype":"brand_new"}})).unwrap();
        assert_eq!(reply.pointer("/response/subtype"), Some(&json!("error")));
        assert!(matches!(&t.entries[..], [Entry::Raw { title, .. }] if title.contains("brand_new")));
    }

    #[test]
    fn a_form_kept_by_the_daemon_shows_where_it_happened() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let lines = [
            json!({"type":"user","timestamp":"2026-10-30T10:00:00.000Z","message":{"role":"user","content":"deploy"}}),
            json!({"type":"assistant","timestamp":"2026-10-30T10:00:05.000Z","message":{"id":"m","content":[{"type":"text","text":"Deployed."}]}}),
        ];
        std::fs::write(&path, lines.iter().map(Value::to_string).collect::<Vec<_>>().join("\n")).unwrap();
        let at = timestamp_ms("2026-10-30T10:00:02.000Z").unwrap();
        let record = dir.path().join("record.jsonl");
        let kept = [
            json!({"ms": at, "line": {"type":"control_request","request_id":"e1","request":{"subtype":"elicitation","mcp_server_name":"deploy","message":"Where?"}}}),
            json!({"ms": at + 1, "line": {"type":"control_response","response":{"subtype":"success","request_id":"e1","response":{"action":"decline"}}}}),
        ];
        std::fs::write(&record, kept.iter().map(Value::to_string).collect::<Vec<_>>().join("\n")).unwrap();
        let t = Transcript::load_with(&path, None, read_record(&record)).unwrap();
        assert!(
            matches!(
                &t.entries[..],
                [
                    Entry::User { .. },
                    Entry::Elicit(Elicit { answer: Some(ElicitAnswer::Decline), .. }),
                    Entry::Text { .. },
                    Entry::TurnEnd(_)
                ]
            ),
            "{:?}",
            t.entries
        );
        // Kept after `before`: the running program replays it instead.
        let t = Transcript::load_with(&path, Some(at), read_record(&record)).unwrap();
        assert!(!t.entries.iter().any(|e| matches!(e, Entry::Elicit(_))));
    }

    #[test]
    fn a_saved_file_shows_only_its_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let lines = [
            json!({"type":"ai-title","title":"x"}),
            json!({"type":"file-history-snapshot","snapshot":{}}),
            json!({"type":"user","timestamp":"2026-10-30T10:00:00.000Z","message":{"role":"user","content":"hi"}}),
            json!({"type":"system","subtype":"turn_duration","durationMs":5}),
            json!({"type":"system","subtype":"away_summary","content":"You were fixing the build"}),
        ];
        std::fs::write(&path, lines.iter().map(Value::to_string).collect::<Vec<_>>().join("\n")).unwrap();
        let t = Transcript::load(&path).unwrap();
        assert!(
            matches!(&t.entries[..], [Entry::User { .. }, Entry::Notice { text, .. }, Entry::TurnEnd(_)] if text == "You were fixing the build"),
            "{:?}",
            t.entries
        );
    }

    #[test]
    fn a_saved_message_keeps_its_images() {
        let mut t = Transcript::default();
        let data = STANDARD.encode([1u8, 2, 3]);
        t.apply(&json!({"type":"user","message":{"role":"user","content":[
            {"type":"image","source":{"type":"base64","media_type":"image/png","data":data}},
            {"type":"image","source":{"type":"url","url":"https://x"}},
            {"type":"text","text":"what is this"},
        ]}}));
        let [Entry::User { text, images }, Entry::Raw { title, .. }] = &t.entries[..] else { panic!("{:?}", t.entries) };
        assert_eq!(text, "what is this");
        // Images it can draw: a base64 one; a link shows as it came, after.
        assert_eq!(images.len(), 1);
        assert_eq!(title, "An image this chat can't draw");
        assert_eq!((images[0].media_type.as_str(), &*images[0].bytes), ("image/png", &[1u8, 2, 3][..]));
    }

    #[test]
    fn streams_then_settles_a_reply() {
        let mut t = Transcript::default();
        t.push_user("hi", Vec::new(), "u1");
        feed(
            &mut t,
            &[
                json!({"type":"system","subtype":"init","session_id":"s1","model":"m","permissionMode":"default"}),
                json!({"type":"user","uuid":"u1","isReplay":true,"message":{"role":"user","content":[{"type":"text","text":"hi"}]}}),
                json!({"type":"stream_event","event":{"type":"message_start","message":{"id":"msg1"}}}),
                json!({"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}),
                json!({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}}),
            ],
        );
        assert_eq!(t.entries[1], Entry::Text { text: "Hel".into(), streaming: true });
        feed(
            &mut t,
            &[
                json!({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}}),
                json!({"type":"stream_event","event":{"type":"content_block_stop","index":0}}),
                json!({"type":"assistant","message":{"id":"msg1","content":[{"type":"text","text":"Hello"}]}}),
                json!({"type":"result","subtype":"success","is_error":false,"duration_ms":1200,"result":"Hello"}),
            ],
        );
        assert_eq!(t.session_id.as_deref(), Some("s1"));
        assert_eq!(t.entries.len(), 3, "the replay isn't shown twice: {:?}", t.entries);
        assert_eq!(t.entries[1], Entry::Text { text: "Hello".into(), streaming: false });
        assert_eq!(t.entries[2], Entry::TurnEnd(TurnEnd { ms: 1200, interrupted: false, error: None }));
        assert!(!t.busy);
    }

    #[test]
    fn a_tool_call_gets_its_input_and_result() {
        let mut t = Transcript::default();
        feed(
            &mut t,
            &[
                json!({"type":"stream_event","event":{"type":"message_start","message":{"id":"m"}}}),
                json!({"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t1","name":"Bash","input":{}}}}),
                json!({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"command\":"}}}),
                json!({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\"ls\"}"}}}),
                json!({"type":"stream_event","event":{"type":"content_block_stop","index":0}}),
            ],
        );
        let Entry::Tool(tool) = &t.entries[0] else { panic!() };
        assert_eq!(tool.input, json!({"command":"ls"}));
        assert!(!tool.done());
        feed(
            &mut t,
            &[
                json!({"type":"assistant","message":{"id":"m","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}),
                json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"a.txt","is_error":false}]}}),
            ],
        );
        assert_eq!(t.entries.len(), 1);
        let Entry::Tool(tool) = &t.entries[0] else { panic!() };
        assert_eq!(tool.result.as_deref(), Some("a.txt"));
    }

    #[test]
    fn unstreamed_messages_are_added_whole() {
        let mut t = Transcript::default();
        feed(
            &mut t,
            &[
                json!({"type":"user","message":{"role":"user","content":"Read a.txt"}}),
                json!({"type":"assistant","message":{"id":"m","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/a.txt"}}]}}),
                json!({"type":"user","message":{"role":"user","content":[{"tool_use_id":"t1","type":"tool_result","content":"1\thello"}]}}),
                json!({"type":"assistant","message":{"id":"m2","content":[{"type":"text","text":"hello"}]}}),
                json!({"type":"user","isMeta":true,"message":{"role":"user","content":"meta"}}),
                json!({"type":"user","message":{"role":"user","content":"<command-name>/clear</command-name>"}}),
                json!({"type":"assistant","parent_tool_use_id":"t9","message":{"id":"m3","content":[{"type":"text","text":"subagent"}]}}),
            ],
        );
        assert_eq!(t.entries.len(), 4, "{:?}", t.entries);
        assert!(matches!(&t.entries[0], Entry::User { text, .. } if text == "Read a.txt"));
        assert!(matches!(&t.entries[3], Entry::User { text, .. } if text == "/clear"));
        assert!(matches!(&t.entries[1], Entry::Tool(tool) if tool.done()));
    }

    #[test]
    fn permission_requests_wait_for_an_answer() {
        let mut t = Transcript::default();
        let reply = t.apply(&json!({"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"touch x"},"tool_use_id":"t1","permission_suggestions":[{"type":"setMode","mode":"acceptEdits"}]}}));
        assert!(reply.is_none());
        assert_eq!(t.open_asks().count(), 1);
        t.set_answer("r1", Answer::Allow);
        assert_eq!(t.open_asks().count(), 0);
        // One the chat can't handle gets an error reply.
        let reply = t.apply(&json!({"type":"control_request","request_id":"r2","request":{"subtype":"hook_callback"}})).unwrap();
        assert_eq!(reply["response"]["subtype"], "error");
        assert_eq!(reply["response"]["request_id"], "r2");
    }

    #[test]
    fn an_interrupted_turn_withdraws_its_requests() {
        let mut t = Transcript::default();
        t.apply(&json!({"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Edit","input":{}}}));
        t.apply(&json!({"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user]"}]}}));
        t.apply(&json!({"type":"result","subtype":"error_during_execution","is_error":true,"duration_ms":5}));
        assert!(!t.entries.iter().any(|e| matches!(e, Entry::Notice { .. })), "{:?}", t.entries);
        assert_eq!(t.open_asks().count(), 0);
        assert!(matches!(t.entries.last(), Some(Entry::TurnEnd(TurnEnd { interrupted: true, error: None, .. }))));
    }

    #[test]
    fn a_replayed_answer_settles_its_request() {
        let mut t = Transcript::default();
        t.apply(&json!({"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{}}}));
        t.apply(&json!({"type":"control_response","response":{"subtype":"success","request_id":"r1","response":{"behavior":"deny"}}}));
        assert_eq!(t.open_asks().count(), 0);
    }

    #[test]
    fn reads_what_initialize_answers() {
        let mut t = Transcript::default();
        t.apply(&json!({"type":"control_response","response":{"subtype":"success","request_id":"i","response":{
            "commands":[{"name":"compact","description":"Compact the conversation"}],
            "models":[{"value":"opus","displayName":"Opus","description":"Most capable"}],
            "current_permission_mode":"auto"}}}));
        assert_eq!(t.commands, [("compact".to_owned(), "Compact the conversation".to_owned())]);
        assert_eq!(t.models[0].value, "opus");
        assert_eq!(t.permission_mode.as_deref(), Some("auto"));
    }

    #[test]
    fn reads_ask_user_question() {
        let ask = Ask {
            request_id: "r".into(),
            tool_name: "AskUserQuestion".into(),
            tool_use_id: None,
            input: json!({"questions":[{"question":"Which?","header":"Pick","options":[{"label":"A"},{"label":"B"}],"multiSelect":false}]}),
            suggestions: Value::Null,
            answer: None,
        };
        assert_eq!(ask.questions(), vec![("Which?".into(), "Pick".into(), vec!["A".into(), "B".into()], false)]);
    }

    #[test]
    fn loads_a_saved_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let lines = [
            json!({"type":"user","timestamp":"2026-10-30T07:49:25.000Z","message":{"role":"user","content":"hi"}}),
            json!({"type":"assistant","timestamp":"2026-10-30T07:49:27.500Z","message":{"id":"m","content":[{"type":"text","text":"hello"}]}}),
            json!({"type":"user","timestamp":"2026-10-30T07:50:00.000Z","message":{"role":"user","content":"again"}}),
            json!({"type":"assistant","timestamp":"2026-10-30T07:50:01.000Z","message":{"id":"m2","content":[{"type":"text","text":"ok"}]}}),
        ];
        std::fs::write(&path, lines.iter().map(|l| l.to_string() + "\n").collect::<String>()).unwrap();
        // A program started at the second message replays it: left out.
        let early = Transcript::load_before(&path, Some(1_793_346_600_000)).unwrap();
        assert_eq!(early.entries.len(), 3, "{:?}", early.entries);
        let t = Transcript::load(&path).unwrap();
        let end = |ms| Entry::TurnEnd(TurnEnd { ms, interrupted: false, error: None });
        assert_eq!(t.entries.len(), 6, "{:?}", t.entries);
        assert_eq!(t.entries[2], end(2500));
        assert_eq!(t.entries[5], end(1000));
        assert!(!t.busy);
    }

    #[test]
    fn reads_timestamps() {
        assert_eq!(timestamp_ms("1970-01-01T00:00:01.5Z"), Some(1500));
        assert_eq!(timestamp_ms("2026-10-30T07:49:25.177Z"), Some(1_793_346_565_177));
        assert_eq!(timestamp_ms("nope"), None);
    }
}
