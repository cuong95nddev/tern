//! A chat's conversation, as any agent's chat driver folds it (see
//! [`crate::driver`]): the entries a chat draws and the session's state.

use std::fmt;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use serde_json::Value;

/// How Claude asks before it acts (`--permission-mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    /// Asks before edits and commands.
    Default,
    /// Edits files without asking.
    AcceptEdits,
    /// Only plans, changes nothing.
    Plan,
    /// Decides itself what is safe to run.
    Auto,
    /// Never asks (`--dangerously-skip-permissions`).
    Bypass,
}

impl PermissionMode {
    /// The modes the user cycles through (Bypass only when Claude started in it).
    pub const ALL: [Self; 4] = [Self::Default, Self::AcceptEdits, Self::Plan, Self::Auto];

    /// Its name on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::AcceptEdits => "acceptEdits",
            Self::Plan => "plan",
            Self::Auto => "auto",
            Self::Bypass => "bypassPermissions",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().chain([Self::Bypass]).find(|m| m.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "Ask first",
            Self::AcceptEdits => "Accept edits",
            Self::Plan => "Plan",
            Self::Auto => "Auto",
            Self::Bypass => "Bypass",
        }
    }

    /// The one after it, as Shift+Tab cycles them in the terminal.
    pub fn next(self) -> Self {
        match Self::ALL.iter().position(|m| *m == self) {
            Some(i) => Self::ALL[(i + 1) % Self::ALL.len()],
            None => Self::Default,
        }
    }
}

/// An image sent with a user message.
#[derive(Clone, PartialEq)]
pub struct UserImage {
    /// Its media type, e.g. `image/png`.
    pub media_type: String,
    pub bytes: Arc<[u8]>,
}

impl UserImage {
    /// The image of a message's `image` block (base64 source), if it has one.
    pub(crate) fn from_block(b: &Value) -> Option<Self> {
        let src = b.get("source")?;
        let media_type = src.get("media_type").and_then(Value::as_str)?.to_owned();
        let bytes = STANDARD.decode(src.get("data").and_then(Value::as_str)?).ok()?;
        Some(Self { media_type, bytes: bytes.into() })
    }
}

// Rows are compared by their debug text: not the bytes.
impl fmt::Debug for UserImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "UserImage({}, {} bytes, {:p})", self.media_type, self.bytes.len(), self.bytes.as_ptr())
    }
}

/// One thing the chat shows, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    /// A message the user sent: its text, and the images that came with it.
    User {
        text: String,
        images: Vec<UserImage>,
    },
    /// Claude's reply text (Markdown); `streaming` while it still grows.
    Text {
        text: String,
        streaming: bool,
    },
    /// Claude's thinking, as the CLI shows it.
    Thinking {
        text: String,
        streaming: bool,
    },
    Tool(Tool),
    /// Claude asks before a tool runs (or asks the user questions).
    Ask(Ask),
    /// An MCP server asks the user for input (a form, or a page to visit).
    Elicit(Elicit),
    /// A line from the session itself: an error, an interruption, a compaction.
    Notice {
        text: String,
        error: bool,
    },
    /// The end of a turn.
    TurnEnd(TurnEnd),
    /// Something Claude sent that the chat has no look for, shown as it
    /// came so nothing is lost: what it is, and its text (JSON
    /// pretty-printed).
    Raw {
        title: String,
        body: String,
    },
}

/// A tool call and, once it ran, its result.
#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    pub id: String,
    pub name: String,
    /// Its input: `{}` until it has streamed in whole.
    pub input: Value,
    /// The input's JSON as it streams.
    pub(crate) partial: String,
    /// The result's text, once it ran.
    pub result: Option<String>,
    /// The result is an error (it failed, or the user denied it).
    pub error: bool,
    /// The change it made to a file, as a unified patch with the file's own
    /// line numbers (from the result's `structuredPatch`).
    pub patch: Option<String>,
    /// The images in its result (a screenshot, an image it read).
    pub images: Vec<UserImage>,
}

impl Tool {
    /// A call of `name` with `input` that hasn't run.
    pub fn new(name: String, input: Value) -> Self {
        Self { id: String::new(), name, input, partial: String::new(), result: None, error: false, patch: None, images: Vec::new() }
    }

    /// It ran (or the turn ended without it running).
    pub fn done(&self) -> bool {
        self.result.is_some()
    }
}

/// A permission request (`can_use_tool`): answered with
/// [`crate::Claude::answer`] under its `request_id`.
#[derive(Debug, Clone, PartialEq)]
pub struct Ask {
    pub request_id: String,
    pub tool_name: String,
    pub tool_use_id: Option<String>,
    pub input: Value,
    /// What "allow for this session" would add (the CLI's own suggestions),
    /// sent back as they came.
    pub suggestions: Value,
    pub answer: Option<Answer>,
}

impl Ask {
    /// The questions of an `AskUserQuestion` call: each one's text, header,
    /// option labels and whether several can be picked.
    pub fn questions(&self) -> Vec<(String, String, Vec<String>, bool)> {
        if self.tool_name != "AskUserQuestion" {
            return Vec::new();
        }
        let Some(qs) = self.input.get("questions").and_then(Value::as_array) else { return Vec::new() };
        qs.iter()
            .map(|q| {
                let text = q.get("question").and_then(Value::as_str).unwrap_or_default().to_owned();
                let header = q.get("header").and_then(Value::as_str).unwrap_or_default().to_owned();
                let options = q
                    .get("options")
                    .and_then(Value::as_array)
                    .map(|o| o.iter().filter_map(|o| o.get("label").and_then(Value::as_str)).map(str::to_owned).collect())
                    .unwrap_or_default();
                let multi = q.get("multiSelect").and_then(Value::as_bool).unwrap_or(false);
                (text, header, options, multi)
            })
            .collect()
    }
}

/// An MCP server's request for input (`elicitation`), answered with
/// [`crate::wire::elicit`].
#[derive(Debug, Clone, PartialEq)]
pub struct Elicit {
    pub request_id: String,
    pub server: String,
    pub message: String,
    /// A page to open instead of a form (`mode: "url"`).
    pub url: Option<String>,
    pub fields: Vec<Field>,
    pub answer: Option<ElicitAnswer>,
}

/// A field of an elicitation's form (a property of its `requested_schema`).
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    /// The property's name: the key of its value in the answer.
    pub name: String,
    /// What the form shows: its `title`, else its name.
    pub title: String,
    pub description: String,
    pub kind: FieldKind,
    pub required: bool,
    pub default: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    Text,
    Number {
        integer: bool,
    },
    Bool,
    /// One of these values (value, label); `many` takes any number of them.
    Choice {
        options: Vec<(Value, String)>,
        many: bool,
    },
}

impl Field {
    pub(crate) fn parse(name: &str, p: &Value, required: bool) -> Self {
        let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
        let options = |p: &Value| -> Option<Vec<(Value, String)>> {
            if let Some(Value::Array(values)) = p.get("enum") {
                let names = p.get("enumNames").and_then(Value::as_array);
                return Some(
                    values
                        .iter()
                        .enumerate()
                        .map(|(i, v)| {
                            let label = names.and_then(|n| n.get(i)).and_then(Value::as_str).map(str::to_owned);
                            (v.clone(), label.unwrap_or_else(|| v.as_str().map_or_else(|| v.to_string(), str::to_owned)))
                        })
                        .collect(),
                );
            }
            let one_of = p.get("oneOf").or_else(|| p.get("anyOf"))?.as_array()?;
            Some(
                one_of
                    .iter()
                    .filter_map(|o| {
                        let v = o.get("const")?.clone();
                        let label = o
                            .get("title")
                            .and_then(Value::as_str)
                            .map_or_else(|| v.as_str().map_or_else(|| v.to_string(), str::to_owned), str::to_owned);
                        Some((v, label))
                    })
                    .collect(),
            )
        };
        let kind = match (p.get("type").and_then(Value::as_str), options(p)) {
            (Some("array"), _) => match p.get("items").and_then(options) {
                Some(options) => FieldKind::Choice { options, many: true },
                None => FieldKind::Text,
            },
            (_, Some(options)) => FieldKind::Choice { options, many: false },
            (Some("boolean"), _) => FieldKind::Bool,
            (Some("number"), _) => FieldKind::Number { integer: false },
            (Some("integer"), _) => FieldKind::Number { integer: true },
            _ => FieldKind::Text,
        };
        let title = Some(s("title")).filter(|t| !t.is_empty()).unwrap_or_else(|| name.to_owned());
        Field { name: name.to_owned(), title, description: s("description"), kind, required, default: p.get("default").cloned() }
    }
}

/// How an elicitation was settled.
#[derive(Debug, Clone, PartialEq)]
pub enum ElicitAnswer {
    /// Sent with the form's values.
    Accept(Value),
    Decline,
    /// Claude took the request back.
    Withdrawn,
}

/// How a permission request was settled.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    Allow,
    /// Allowed, with the CLI's suggested rules for the rest of the session.
    AllowSession,
    Deny,
    /// The user's answers to `AskUserQuestion`, by question text.
    Answered(Value),
    /// Claude took the request back (the turn was interrupted).
    Withdrawn,
}

/// The end of a turn, from its `result` frame.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnEnd {
    pub ms: u64,
    /// The user stopped it.
    pub interrupted: bool,
    /// What went wrong, when it failed otherwise.
    pub error: Option<String>,
}

/// A model Claude offers (from its `initialize` answer).
#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    /// What `set_model` takes (`default`, `opus`, ...).
    pub value: String,
    pub name: String,
    pub description: String,
}

/// The conversation so far, and the session's state.
#[derive(Debug, Default)]
pub struct Transcript {
    pub entries: Vec<Entry>,
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    /// The slash commands the agent takes: name, description.
    pub commands: Vec<(String, String)>,
    pub models: Vec<Model>,
    /// A turn runs: from a sent message to its end.
    pub busy: bool,
    /// Bumped on every change.
    pub version: u64,
    /// How full the conversation's context window is, once the agent said.
    pub context: Option<tern_bridge::context::Reading>,
    /// News of its handoff, for the chat to take: one was written (at this
    /// path), or the conversation was cleared (`None`).
    pub handoff: Option<Option<String>>,
}

impl Transcript {
    /// Show a message the user sends, and start a turn.
    pub fn push_user(&mut self, text: &str, images: Vec<UserImage>) {
        self.entries.push(Entry::User { text: text.to_owned(), images });
        self.busy = true;
        self.version += 1;
    }

    /// Record the answer given to request `request_id`.
    pub fn set_answer(&mut self, request_id: &str, answer: Answer) {
        let entry = self.entries.iter_mut().find(|e| match e {
            Entry::Ask(a) => a.request_id == request_id,
            Entry::Elicit(e) => e.request_id == request_id,
            _ => false,
        });
        match entry {
            Some(Entry::Ask(a)) => a.answer = Some(answer),
            // The agent took it back.
            Some(Entry::Elicit(e)) if answer == Answer::Withdrawn => e.answer = Some(ElicitAnswer::Withdrawn),
            _ => return,
        }
        self.version += 1;
    }

    /// Record the answer given to elicitation `request_id`.
    pub fn set_elicit_answer(&mut self, request_id: &str, answer: ElicitAnswer) {
        let entry = self.entries.iter_mut().find(|e| matches!(e, Entry::Elicit(e) if e.request_id == request_id));
        if let Some(Entry::Elicit(e)) = entry {
            e.answer = Some(answer);
            self.version += 1;
        }
    }

    /// A request waits for the user: a permission, a question, a form.
    pub fn waiting(&self) -> bool {
        self.entries.iter().any(|e| matches!(e, Entry::Ask(Ask { answer: None, .. }) | Entry::Elicit(Elicit { answer: None, .. })))
    }

    /// The permission requests still waiting for the user.
    pub fn open_asks(&self) -> impl Iterator<Item = &Ask> {
        self.entries.iter().filter_map(|e| match e {
            Entry::Ask(a) if a.answer.is_none() => Some(a),
            _ => None,
        })
    }

    /// After a replay: a turn runs when the program does and its last user
    /// message has no end yet.
    pub fn resume_busy(&mut self, running: bool) {
        let user = self.entries.iter().rposition(|e| matches!(e, Entry::User { .. }));
        let end = self.entries.iter().rposition(|e| matches!(e, Entry::TurnEnd(_)));
        self.busy = running && user.is_some() && user > end;
        self.version += 1;
    }

    /// The process ended: nothing streams or waits any more.
    pub fn ended(&mut self, why: Option<String>) {
        if let Some(text) = why {
            self.entries.push(Entry::Notice { text, error: true });
        }
        self.settle();
        self.busy = false;
        self.version += 1;
    }

    /// Nothing streams any more; requests no one answered are withdrawn;
    /// tools that never answered are left as they are.
    pub fn settle(&mut self) {
        for e in &mut self.entries {
            match e {
                Entry::Text { streaming, .. } | Entry::Thinking { streaming, .. } => *streaming = false,
                Entry::Ask(a) if a.answer.is_none() => a.answer = Some(Answer::Withdrawn),
                _ => {},
            }
        }
    }
}
