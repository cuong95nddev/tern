//! A Claude chat tab: Claude Code run headless (see [`tern_chat`]), its
//! conversation drawn as a timeline instead of a terminal.
//!
//! The timeline follows T3 Code: the user's messages in bubbles, Claude's
//! replies as prose, tool calls as one-line rows (a run of them folded into
//! a summary line), a finished turn's work folded behind "Worked for 12s"
//! above its final reply, and permission requests as cards with buttons.
//!
//! Claude runs in Tern's daemon ([`tern_mux::client::RemoteChat`]), so it
//! goes on while Tern is closed; a tab that comes back attaches to it again
//! and replays what it said over the history Claude saved before it started.

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::diff::{Diff, DiffFile, DiffHunkSeparator, DiffState};
use gpui_kit::component::input::{
    Enter, Escape, IndentInline, Input, InputEvent, InputState, MoveDown, MoveUp, Paste, Position, Textarea, TextareaState,
};
use gpui_kit::component::menu::DropdownMenu as _;
use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _};
use gpui_kit::*;
use serde_json::{Value, json};
use tern_agent::AgentId;
use tern_chat::{Answer, Ask, ChatDriver, Elicit, ElicitAnswer, Entry, FieldKind, PermissionMode, Transcript, UserImage, summary};
use tern_mux::client::{ChatEvent as MuxChatEvent, ChatSink, Connector, OpenChat, RemoteChat};
use tern_mux::proto::{ChatExit, ChatSpec};

use crate::accounts;
use crate::agents::{ContextGuard, ContextUse, Handoff};
use crate::template::{self, Framed, on_event};
use crate::terminal_view::ActivityStatus;

/// The lines an opened tool call shows of its command and of its output.
const DETAIL_LINES: usize = 40;

/// The slash commands listed at once over the message box.
const SLASH_ROWS: usize = 8;

/// The largest image Claude takes in a message.
const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;

/// An image going with the next message.
struct Attachment {
    name: String,
    image: Arc<Image>,
}

/// What the tab tells the workspace.
#[derive(Debug, Clone)]
pub enum ChatEvent {
    /// The title changed (the first message gives it).
    Title,
    /// Its chat in the daemon changed (started, attached, ended): the
    /// session should be saved, to attach to it after a restart.
    Chat,
    /// Its status changed (working, waiting for the user, done).
    Status,
    /// How full its context window is changed.
    Context,
    /// Its handoff changed, or the conversation it is in: the session
    /// should be saved.
    Conversation,
}

/// What the user asked of a Claude that doesn't run yet: done once it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waiting {
    HandOff,
    Resume,
}

/// Something that opens and closes: by the entry it starts at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Open {
    Tool(usize),
    Group(usize),
    Thinking(usize),
    Raw(usize),
    /// A finished turn's work, by its user message.
    Turn(usize),
}

/// A file change drawn as a diff.
struct ShownDiff {
    patch: String,
    state: Entity<DiffState>,
    /// The lines it draws (changes, context), and its hunks.
    lines: usize,
    hunks: usize,
    /// Its line numbers are the file's (it ran), not counted from the replaced text.
    numbered: bool,
}

/// A line of the timeline.
#[derive(Debug, Clone, PartialEq)]
enum Row {
    User(usize),
    Text(usize),
    Thinking(usize),
    /// A tool call; `nested` inside an opened group.
    Tool {
        ix: usize,
        nested: bool,
    },
    /// A run of tool calls, as a summary.
    Group(Vec<usize>),
    Ask(usize),
    /// An MCP server's form, or how it was answered.
    Elicit(usize),
    Notice(usize),
    /// Something Claude sent that has no look of its own, as it came.
    Raw(usize),
    /// How a turn ended when it didn't finish (stopped, failed).
    Ended(usize),
    /// A finished turn's work, folded: by its user message.
    Worked {
        user: usize,
        ms: u64,
    },
    /// The turn running now.
    Working,
}

pub struct ChatView {
    root: PathBuf,
    account: String,
    /// The conversation's id: resumed with it after a restart.
    session: String,
    t: Transcript,
    /// The agent's chat protocol: folds its lines into `t`, makes the lines
    /// written to it.
    driver: Box<dyn ChatDriver>,
    mux: Arc<Connector>,
    /// Its chat in the daemon, while the tab holds it.
    chat: Option<RemoteChat>,
    /// That chat's id: attached to again after a restart.
    chat_id: Option<u64>,
    /// Claude was started or attached to and hasn't ended: what the user
    /// sends goes to it rather than starting another.
    live: bool,
    /// Lines written before the daemon was reached.
    outbox: Vec<String>,
    /// The chat being opened is attached to (not started): what it said is
    /// replayed over the saved history.
    replay: bool,
    rows: Vec<Row>,
    /// Each row's look when last drawn, to measure again only those that changed.
    sigs: Vec<u64>,
    open: HashSet<Open>,
    /// The picks made so far on a question card: entry, question, label.
    picks: Vec<(usize, usize, String)>,
    /// The images going with the next message.
    attachments: Vec<Attachment>,
    /// The text boxes of MCP forms (see [`Elicit`]): by entry and field.
    form_inputs: HashMap<(usize, usize), Entity<InputState>>,
    /// The picks on MCP forms' yes/no and choice fields: by entry and field.
    form_picks: HashMap<(usize, usize), Value>,
    /// The images of sent messages, ready to draw, by where their bytes are
    /// (see [`thumb_key`]).
    thumbs: HashMap<usize, (Arc<[u8]>, Arc<Image>)>,
    /// The diffs drawn: of opened file changes and of requests to make one, by entry.
    diffs: HashMap<usize, ShownDiff>,
    mode: PermissionMode,
    /// The user picked `mode`; else Claude starts in its own (the user's
    /// settings and arguments) and says which.
    mode_picked: bool,
    /// The model the user picked (`set_model`'s value): Claude runs it
    /// again when started anew.
    model_picked: Option<String>,
    /// The slash command picked in the list over the message box.
    slash: usize,
    /// The text the list was dismissed for (Escape); it shows again once the text changes.
    slash_dismissed: Option<String>,
    /// When the running turn started.
    started: Option<Instant>,
    /// What the agents panel shows of it, and since when.
    status: ActivityStatus,
    status_since: Instant,
    /// A turn ended, and the user hasn't been in the tab since.
    done_unseen: bool,
    /// A turn ended while it was open.
    turned: bool,
    /// The turn ran at the last look (to see one end).
    was_busy: bool,
    scroller: Entity<MessageScrollerState>,
    input: Entity<TextareaState>,
    title: String,
    _input_sub: Subscription,
    _events: Option<Task<()>>,
    _ticker: Option<Task<()>>,
    /// How full its context window is, as last told.
    context: Option<ContextUse>,
    /// The limit it keeps.
    guard: ContextGuard,
    /// The handoff it can go on from.
    handoff: Handoff,
    waiting_for_start: Option<Waiting>,
    /// The agent may start in its folder (see [`crate::folder_trust`]).
    trusted: bool,
}

impl EventEmitter<ChatEvent> for ChatView {}

impl Focusable for ChatView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl ChatView {
    /// A chat with the agent `driver` talks to, in `root` running as
    /// `account`: conversation `session` when given (its history shown,
    /// resumed at the first message), else a new one; attached to the
    /// daemon's chat `chat_id` when that still runs.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        driver: Box<dyn ChatDriver>,
        root: PathBuf,
        account: String,
        session: Option<String>,
        chat_id: Option<u64>,
        mux: Arc<Connector>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let short = crate::agents::agent_short_name(&AgentId::new(driver.agent()));
        let placeholder = format!("Ask {short} anything, or / for commands");
        let input = cx.new(|cx| TextareaState::new(window, cx).auto_grow(1, 12).submit_on_enter(true).placeholder(placeholder));
        let input_sub = cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { secondary: false, shift: false } => this.send(window, cx),
            InputEvent::Change => {
                this.slash = 0;
                // Claude starts while the user types, so its commands and models are there.
                this.warm(cx);
                cx.notify();
            },
            InputEvent::Focus => {
                this.check_trust(cx);
                this.warm(cx);
                this.looked(cx);
            },
            _ => {},
        });
        let scroller = cx.new(|cx| MessageScrollerState::new(0, cx));
        let session = session.unwrap_or_else(tern_chat::new_session_id);
        // Folder trust is Claude Code's own record; other agents aren't asked here.
        let trusted = driver.agent() != tern_agent::CLAUDE || crate::folder_trust::trusted(&account, &root);
        let mut this = Self {
            root,
            account,
            session,
            t: Transcript::default(),
            driver,
            mux,
            chat: None,
            chat_id: None,
            live: false,
            outbox: Vec::new(),
            replay: false,
            rows: Vec::new(),
            sigs: Vec::new(),
            open: HashSet::new(),
            picks: Vec::new(),
            form_inputs: HashMap::new(),
            form_picks: HashMap::new(),
            attachments: Vec::new(),
            thumbs: HashMap::new(),
            diffs: HashMap::new(),
            mode: PermissionMode::Default,
            mode_picked: false,
            model_picked: None,
            slash: 0,
            slash_dismissed: None,
            started: None,
            status: ActivityStatus::None,
            status_since: Instant::now(),
            done_unseen: false,
            turned: false,
            was_busy: false,
            scroller,
            input,
            title: format!("{short} Chat"),
            _input_sub: input_sub,
            _events: None,
            _ticker: None,
            context: None,
            guard: ContextGuard::default(),
            handoff: Handoff::default(),
            waiting_for_start: None,
            trusted,
        };
        match chat_id {
            Some(id) => {
                this.chat_id = Some(id);
                this.connect(OpenChat::Attach { id }, cx);
            },
            None => this.load_history(None),
        }
        this.refresh(cx);
        this
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// The agent it talks to.
    pub fn agent(&self) -> AgentId {
        AgentId::new(self.driver.agent())
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    pub fn account(&self) -> &str {
        &self.account
    }

    /// Its chat in the daemon, to attach to after a restart.
    pub fn chat_id(&self) -> Option<u64> {
        self.chat_id
    }

    /// How full its context window is, once a turn ended.
    pub fn context(&self) -> Option<ContextUse> {
        self.context
    }

    pub fn guard(&self) -> ContextGuard {
        self.guard
    }

    pub fn set_guard(&mut self, guard: ContextGuard) {
        self.guard = guard;
    }

    pub fn handoff(&self) -> &Handoff {
        &self.handoff
    }

    pub fn set_handoff(&mut self, handoff: Handoff) {
        self.handoff = handoff;
    }

    /// Start Claude, to do `what` once it runs (see [`Self::take_waiting`]).
    pub fn start_for(&mut self, what: Waiting, cx: &mut Context<Self>) {
        self.waiting_for_start = Some(what);
        self.warm(cx);
    }

    pub fn take_waiting(&mut self) -> Option<Waiting> {
        self.waiting_for_start.take()
    }

    /// A turn runs, or a request waits for the user.
    pub fn busy(&self) -> bool {
        self.t.busy
    }

    /// What the agents panel shows: waiting for the user, working, done and
    /// not looked at, or idle.
    pub fn status(&self) -> ActivityStatus {
        self.status
    }

    /// What the conversation is doing now: Claude's latest sentence and
    /// tool call, for the agents panel.
    pub fn recent(&self) -> tern_chat::Recent {
        tern_chat::recent(&self.t.entries, Some(self.root.as_path()))
    }

    pub fn status_since(&self) -> Instant {
        self.status_since
    }

    /// A turn ended and the user has looked since.
    pub fn seen_done(&self) -> bool {
        self.turned && !self.done_unseen
    }

    /// Work out the status again; tell the workspace when it changed.
    fn update_status(&mut self, cx: &mut Context<Self>) {
        if self.was_busy && !self.t.busy {
            self.done_unseen = true;
            self.turned = true;
        }
        self.was_busy = self.t.busy;
        let status = if self.waiting() {
            ActivityStatus::Blocked
        } else if self.t.busy {
            ActivityStatus::Working
        } else if self.done_unseen {
            ActivityStatus::Done
        } else {
            ActivityStatus::None
        };
        if status != self.status {
            self.status = status;
            self.status_since = Instant::now();
            cx.emit(ChatEvent::Status);
        }
    }

    /// The user is in the tab: a finished turn is seen.
    fn looked(&mut self, cx: &mut Context<Self>) {
        if self.done_unseen {
            self.done_unseen = false;
            self.update_status(cx);
        }
    }

    pub fn waiting(&self) -> bool {
        self.t.waiting()
    }

    /// The tab closes: its Claude ends.
    pub fn close(&mut self) {
        if let Some(chat) = self.chat.take() {
            chat.close();
        }
        self.chat_id = None;
        self.live = false;
    }

    /// Show the conversation the agent saved, up to `before` (see
    /// [`ChatDriver::load`]).
    fn load_history(&mut self, before: Option<u64>) {
        let record = record_path(&self.session).map(|p| tern_chat::read_record(&p)).unwrap_or_default();
        // Whatever was folded before goes with the transcript it was folded into.
        self.driver.reset();
        let mut t = Transcript::default();
        if let Some(path) = saved_path(self.driver.agent(), &self.account, &self.root, &self.session)
            && let Err(e) = self.driver.load(&mut t, &path, before, record)
        {
            log::warn!("chat: read {}: {e}", path.display());
            t = Transcript::default();
        }
        self.t = t;
        self.open.clear();
        self.picks.clear();
        self.form_inputs.clear();
        self.form_picks.clear();
        self.title_from_history();
    }

    /// Title the tab after the conversation's first message; true when that changed it.
    fn title_from_history(&mut self) -> bool {
        let Some(Entry::User { text, .. }) = self.t.entries.iter().find(|e| matches!(e, Entry::User { .. })) else { return false };
        let title = summary::first_line(text, 40);
        let changed = title != self.title;
        self.title = title;
        changed
    }

    /// Start Claude in the daemon: resuming the conversation when Claude saved it.
    fn start(&mut self, cx: &mut Context<Self>) {
        let resume = saved_path(self.driver.agent(), &self.account, &self.root, &self.session).is_some();
        // The flags the user picked are Claude's.
        let user_args = if self.agent().is(tern_agent::CLAUDE) { crate::window_state::claude_args() } else { String::new() };
        let line = self.driver.command(
            &user_args,
            &self.session,
            resume,
            self.model_picked.as_deref(),
            self.mode_picked.then_some(self.mode),
            crate::workspace::shell_quote,
        );
        let (_, env) = crate::workspace::claude_env(&self.account, cx);
        let spec = ChatSpec {
            program: crate::platform::shell(),
            args: crate::platform::shell_command_args(&line),
            cwd: self.root.clone(),
            env: env.into_iter().collect(),
            session: self.session.clone(),
            account: self.account.clone(),
            record: record_path(&self.session),
            agent: Some(self.driver.agent().to_owned()),
        };
        // Before anything the user sends.
        self.outbox.splice(0..0, self.driver.start_lines(&self.session, resume));
        self.connect(OpenChat::Spawn(Box::new(spec)), cx);
    }

    /// Start Claude unless it runs: it then says which commands and models
    /// it has. Not in a folder the user hasn't trusted: it would run the
    /// folder's hooks.
    fn warm(&mut self, cx: &mut Context<Self>) {
        if !self.live && self.trusted {
            self.start(cx);
        }
    }

    /// See whether the folder was trusted meanwhile (in a terminal, another chat).
    fn check_trust(&mut self, cx: &mut Context<Self>) {
        if !self.trusted && crate::folder_trust::trusted(&self.account, &self.root) {
            self.trusted = true;
            cx.notify();
        }
    }

    /// The user trusts the folder: Claude starts.
    fn trust_folder(&mut self, cx: &mut Context<Self>) {
        if let Err(e) = crate::folder_trust::trust(&self.root) {
            // It holds for this tab all the same.
            log::warn!("chat: can't keep {} as trusted: {e}", self.root.display());
        }
        self.trusted = true;
        self.warm(cx);
        cx.notify();
    }

    /// Reach the daemon (off the UI thread: it may have to start) and open
    /// the chat there; then follow what it says.
    fn connect(&mut self, open: OpenChat, cx: &mut Context<Self>) {
        self.replay = matches!(open, OpenChat::Attach { .. });
        self.live = true;
        let mux = self.mux.clone();
        let (tx, rx) = flume::unbounded::<MuxChatEvent>();
        let sink: ChatSink = Arc::new(move |e| {
            let _ = tx.send(e);
        });
        self._events = Some(cx.spawn(async move |this, cx| {
            let client = cx
                .background_spawn(async move {
                    let client = mux.get()?;
                    if client.runs_chats() {
                        return Ok(client);
                    }
                    // A daemon of an older build: this build's takes its
                    // sessions over (their programs run on), then runs the chat.
                    drop(client);
                    mux.hand_off()?;
                    mux.get()
                })
                .await;
            let opened = this.update(cx, |this, cx| match client {
                Ok(client) => {
                    let chat = client.open_chat(open, sink);
                    for line in this.outbox.drain(..) {
                        chat.send(line);
                    }
                    this.chat = Some(chat);
                    true
                },
                Err(e) => {
                    this.outbox.clear();
                    this.live = false;
                    this.driver.ended(&mut this.t, Some(format!("Cannot reach Tern's background daemon: {e}")));
                    this.refresh(cx);
                    false
                },
            });
            if !matches!(opened, Ok(true)) {
                return;
            }
            while let Ok(first) = rx.recv_async().await {
                // A burst of deltas is drawn once.
                let mut batch = vec![first];
                batch.extend(rx.try_iter());
                if this.update(cx, |this, cx| this.on_events(batch, cx)).is_err() {
                    break;
                }
            }
        }));
    }

    /// Write `line` to Claude (kept until the daemon is reached).
    fn write(&mut self, line: String) {
        match &self.chat {
            Some(chat) => chat.send(line),
            None => self.outbox.push(line),
        }
    }

    fn on_events(&mut self, events: Vec<MuxChatEvent>, cx: &mut Context<Self>) {
        let chat_id = self.chat_id;
        // Attached again keeps the id, but maybe to a new daemon.
        let attached = events.iter().any(|e| matches!(e, MuxChatEvent::Attached { .. }));
        self.apply_events(events, cx);
        if attached || self.chat_id != chat_id {
            cx.emit(ChatEvent::Chat);
        }
    }

    fn apply_events(&mut self, events: Vec<MuxChatEvent>, cx: &mut Context<Self>) {
        for e in events {
            match e {
                MuxChatEvent::Attached { id, started, lines, exited } => {
                    self.chat_id = Some(id);
                    // It runs already: what asking would have kept from happening did.
                    self.trusted = true;
                    if self.replay {
                        // The history Claude saved before this program, then what it said.
                        self.load_history(Some(started));
                        for line in &lines {
                            self.driver.fold_line(&mut self.t, line);
                        }
                        self.t.resume_busy(exited.is_none());
                        // The first message may be among the replayed lines.
                        if self.title_from_history() {
                            cx.emit(ChatEvent::Title);
                        }
                        if self.t.busy {
                            self.start_ticker(cx);
                        }
                    }
                    self.follow_mode();
                    if let Some(exit) = exited {
                        self.ended(exit);
                    }
                },
                MuxChatEvent::Line(line) => {
                    if let Some(reply) = self.driver.fold_line(&mut self.t, &line) {
                        self.write(reply);
                    }
                    self.follow_mode();
                },
                MuxChatEvent::Exited(exit) => self.ended(exit),
                // The daemon was restarted: the next message resumes the conversation.
                MuxChatEvent::NotFound => {
                    self.chat = None;
                    self.chat_id = None;
                    self.live = false;
                    self.load_history(None);
                },
                MuxChatEvent::Error(e) => {
                    self.chat = None;
                    self.chat_id = None;
                    self.live = false;
                    self.driver.ended(&mut self.t, Some(e));
                },
                // The daemon handed off or quit: attach again.
                MuxChatEvent::Lost => {
                    self.chat = None;
                    if let Some(id) = self.chat_id {
                        self.connect(OpenChat::Attach { id }, cx);
                        return;
                    }
                },
            }
        }
        if !self.t.busy {
            self.started = None;
            self._ticker = None;
        }
        self.follow_conversation(cx);
        self.refresh(cx);
    }

    /// What Claude's lines said of the conversation: how full it is, its
    /// handoff, and the one `/clear` started in its place.
    fn follow_conversation(&mut self, cx: &mut Context<Self>) {
        let context = self.t.context.map(ContextUse::from);
        if context != self.context {
            self.context = context;
            cx.emit(ChatEvent::Context);
        }
        let handoff = self.t.handoff.take().map(|news| news.map_or(Handoff::None, Handoff::Written));
        let handoff = handoff.filter(|h| *h != self.handoff);
        let session = self.t.session_id.clone().filter(|s| *s != self.session);
        if handoff.is_some() || session.is_some() {
            self.handoff = handoff.unwrap_or_else(|| std::mem::take(&mut self.handoff));
            self.session = session.unwrap_or_else(|| std::mem::take(&mut self.session));
            cx.emit(ChatEvent::Conversation);
        }
    }

    /// The mode Claude says it is in, unless the user picked one.
    fn follow_mode(&mut self) {
        if let Some(m) = self.t.permission_mode.as_deref().and_then(PermissionMode::parse)
            && !self.mode_picked
        {
            self.mode = m;
        }
    }

    /// Its program ended: the daemon forgets it; the next message starts
    /// another that resumes the conversation.
    fn ended(&mut self, exit: ChatExit) {
        let why = match (exit.code, exit.stderr.is_empty()) {
            (Some(0), _) | (None, true) => None,
            (code, _) => {
                let code = code.map_or_else(String::new, |c| format!(" ({c})"));
                let sep = if exit.stderr.is_empty() { "" } else { ": " };
                let name = crate::agents::agent_short_name(&AgentId::new(self.driver.agent()));
                Some(format!("{name} exited{code}{sep}{}", exit.stderr))
            },
        };
        self.driver.ended(&mut self.t, why);
        if let Some(chat) = self.chat.take() {
            chat.close();
        }
        self.chat_id = None;
        self.live = false;
    }

    fn start_ticker(&mut self, cx: &mut Context<Self>) {
        if self.started.is_some() {
            return;
        }
        self.started = Some(Instant::now());
        self._ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        }));
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().trim().to_owned();
        if text.is_empty() && self.attachments.is_empty() {
            return;
        }
        self.check_trust(cx);
        if !self.trusted {
            // What was typed stays, for once it is.
            return;
        }
        self.warm(cx);
        let attachments = std::mem::take(&mut self.attachments);
        let images: Vec<(&str, &[u8])> = attachments.iter().map(|a| (a.image.format().mime_type(), a.image.bytes())).collect();
        let (_, line) = self.driver.user_line(&self.session, &text, &images);
        if !line.is_empty() {
            self.write(line);
        }
        if self.t.entries.iter().all(|e| !matches!(e, Entry::User { .. })) {
            let title = if text.is_empty() { attachments.first().map_or("", |a| a.name.as_str()) } else { text.as_str() };
            self.title = summary::first_line(title, 40);
            cx.emit(ChatEvent::Title);
        }
        let sent = attachments
            .iter()
            .map(|a| {
                let image = UserImage { media_type: a.image.format().mime_type().to_owned(), bytes: a.image.bytes().into() };
                self.thumbs.insert(thumb_key(&image), (image.bytes.clone(), a.image.clone()));
                image
            })
            .collect();
        self.t.push_user(&text, sent);
        self.input.update(cx, |i, cx| i.set_value("", window, cx));
        self.start_ticker(cx);
        self.refresh(cx);
        self.scroller.update(cx, |s, cx| s.scroll_to_end(cx));
    }

    /// Send `image` (called `name`) with the next message: one of the
    /// formats Claude reads, at most [`MAX_IMAGE_BYTES`].
    fn attach(&mut self, name: String, image: Image, window: &mut Window, cx: &mut Context<Self>) {
        let problem = if !matches!(image.format(), ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::Webp) {
            Some(format!("Claude can't read {name}: only PNG, JPEG, GIF and WebP images"))
        } else if image.bytes().len() > MAX_IMAGE_BYTES {
            Some(format!("{name} is over 5 MB, more than Claude takes"))
        } else {
            None
        };
        match problem {
            Some(p) => window.push_notification(p, cx),
            None => self.attachments.push(Attachment { name, image: Arc::new(image) }),
        }
        cx.notify();
    }

    /// Attach the image files among `paths`; the others are mentioned in the
    /// message (`@path`), as Claude Code does. False when none was taken.
    fn attach_paths(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) -> bool {
        let mut mentions = Vec::new();
        for p in paths {
            let name = p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned());
            match crate::viewer::image_format(p).filter(|_| p.is_file()) {
                Some(format) => match std::fs::read(p) {
                    Ok(bytes) => self.attach(name, Image::from_bytes(format, bytes), window, cx),
                    Err(e) => window.push_notification(format!("Could not read {name}: {e}"), cx),
                },
                None => {
                    let shown = p.strip_prefix(&self.root).unwrap_or(p).display().to_string();
                    mentions.push(format!("@{}", if shown.contains(' ') { format!("\"{shown}\"") } else { shown }));
                },
            }
        }
        if !mentions.is_empty() {
            let text = mentions.join(" ") + " ";
            self.input.update(cx, |i, cx| {
                i.insert(text, window, cx);
                i.focus(window, cx);
            });
        }
        !paths.is_empty()
    }

    /// Take the images on the clipboard (copied, or copied files); false
    /// when it holds none, for the text box to paste its text.
    fn paste_images(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(item) = cx.read_from_clipboard() else { return false };
        let mut took = false;
        let mut n = self.attachments.len();
        for entry in item.into_entries() {
            match entry {
                ClipboardEntry::Image(image) => {
                    n += 1;
                    let ext = image.format().mime_type().rsplit('/').next().unwrap_or("png").to_owned();
                    self.attach(format!("image-{n}.{ext}"), image, window, cx);
                    took = true;
                },
                ClipboardEntry::ExternalPaths(paths) => {
                    let images: Vec<PathBuf> = paths.paths().iter().filter(|p| crate::viewer::image_format(p).is_some()).cloned().collect();
                    took |= self.attach_paths(&images, window, cx);
                },
                ClipboardEntry::String(_) => {},
            }
        }
        took
    }

    /// Ask for image files to attach.
    fn pick_images(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some("Attach".into()) });
        cx.spawn_in(window, async move |this, cx| {
            let Some(paths) = rx.await.ok().and_then(Result::ok).flatten() else { return };
            let _ = this.update_in(cx, |this, window, cx| this.attach_paths(&paths, window, cx));
        })
        .detach();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if self.chat.is_some()
            && let Some(line) = self.driver.interrupt_line()
        {
            self.write(line);
        }
        cx.notify();
    }

    fn cycle_mode(&mut self, cx: &mut Context<Self>) {
        self.set_mode(self.mode.next(), cx);
    }

    fn set_mode(&mut self, mode: PermissionMode, cx: &mut Context<Self>) {
        self.mode = mode;
        self.mode_picked = true;
        if self.chat.is_some()
            && let Some(line) = self.driver.mode_line(mode)
        {
            self.write(line);
        }
        cx.notify();
    }

    /// Run `model` (a value of [`Transcript::models`]) from the next turn on.
    fn set_model(&mut self, model: String, cx: &mut Context<Self>) {
        if self.live
            && let Some(line) = self.driver.model_line(&model)
        {
            self.write(line);
        }
        self.model_picked = Some(model);
        cx.notify();
    }

    /// The model shown on its button: the one picked, else the one Claude
    /// says it runs, else (before its first turn) its default.
    fn model_label(&self) -> String {
        let name = |v: &str| self.t.models.iter().find(|m| m.value == v).map(|m| m.name.clone());
        match &self.model_picked {
            Some(v) => name(v).unwrap_or_else(|| v.clone()),
            None => self.t.model.clone().or_else(|| name("default")).unwrap_or_else(|| "Model".to_owned()),
        }
    }

    /// The slash commands matching the typed text, when it is a command
    /// being typed (`/` and no space yet): those starting with it first.
    /// None hides the list.
    fn slash_matches(&self, cx: &App) -> Option<Vec<(String, String)>> {
        let text = self.input.read(cx).value();
        let query = text.strip_prefix('/')?;
        if query.contains(char::is_whitespace) || self.slash_dismissed.as_deref() == Some(text.as_ref()) {
            return None;
        }
        Some(command_matches(&self.t.commands, query))
    }

    /// Move the pick in the slash command list by `by`; false when it isn't shown.
    fn slash_step(&mut self, by: isize, cx: &mut Context<Self>) -> bool {
        let Some(n) = self.slash_matches(cx).map(|m| m.len()).filter(|&n| n > 0) else { return false };
        self.slash = (self.slash as isize + by).rem_euclid(n as isize) as usize;
        cx.notify();
        true
    }

    /// Put the picked slash command in the message box; false when the list
    /// isn't shown. `send` sends it instead when it is typed out already.
    fn slash_complete(&mut self, send: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(matches) = self.slash_matches(cx) else { return false };
        let Some((name, _)) = matches.get(self.slash.min(matches.len().saturating_sub(1))) else { return false };
        let text = format!("/{name}");
        if send && self.input.read(cx).value().as_ref() == text {
            return false;
        }
        let text = format!("{text} ");
        let end = text.len() as u32;
        self.input.update(cx, |i, cx| {
            i.set_value(text, window, cx);
            i.set_cursor_position(Position::new(0, end), window, cx);
        });
        self.slash = 0;
        cx.notify();
        true
    }

    fn answer(&mut self, ix: usize, answer: Answer, cx: &mut Context<Self>) {
        let Some(Entry::Ask(ask)) = self.t.entries.get(ix).cloned() else { return };
        if let Some(line) = self.driver.answer_line(&ask, &answer) {
            self.write(line);
        }
        self.t.set_answer(&ask.request_id, answer);
        self.picks.retain(|p| p.0 != ix);
        self.refresh(cx);
    }

    /// The user picked option `label` of question `q` on card `ix`. On a
    /// question that takes several the pick toggles, and the card waits for
    /// Submit; else it is sent once every question has a pick.
    fn pick(&mut self, ix: usize, q: usize, label: String, cx: &mut Context<Self>) {
        let Some(Entry::Ask(ask)) = self.t.entries.get(ix) else { return };
        let questions = ask.questions();
        let Some((.., multi)) = questions.get(q) else { return };
        if *multi {
            let had = self.picks.len();
            self.picks.retain(|p| !(p.0 == ix && p.1 == q && p.2 == label));
            if self.picks.len() == had {
                self.picks.push((ix, q, label));
            }
        } else {
            self.picks.retain(|p| !(p.0 == ix && p.1 == q));
            self.picks.push((ix, q, label));
        }
        if questions.iter().any(|q| q.3) {
            cx.notify();
        } else {
            self.submit(ix, cx);
        }
    }

    /// Card `ix`'s answers, once every question has a pick: the labels of a
    /// question that takes several as a list, in the order offered.
    fn picked_answers(&self, ix: usize) -> Option<Value> {
        let Some(Entry::Ask(ask)) = self.t.entries.get(ix) else { return None };
        let mut answers = serde_json::Map::new();
        for (qi, (text, header, labels, multi)) in ask.questions().into_iter().enumerate() {
            let picked: Vec<&String> = labels.iter().filter(|l| self.picks.iter().any(|p| p.0 == ix && p.1 == qi && &p.2 == *l)).collect();
            let first = picked.first()?;
            let key = if text.is_empty() { header } else { text };
            answers.insert(key, if multi { json!(picked) } else { json!(first) });
        }
        Some(Value::Object(answers))
    }

    /// Send card `ix`'s answers if every question has one.
    fn submit(&mut self, ix: usize, cx: &mut Context<Self>) {
        match self.picked_answers(ix) {
            Some(answers) => self.answer(ix, Answer::Answered(answers), cx),
            None => cx.notify(),
        }
    }

    fn toggle(&mut self, what: Open, cx: &mut Context<Self>) {
        if !self.open.remove(&what) {
            self.open.insert(what);
        }
        self.refresh(cx);
    }

    /// Lay the timeline out again; rows from the first that changed on are
    /// measured again.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.update_thumbs();
        let rows = self.build();
        self.update_diffs(&rows, cx);
        let sigs: Vec<u64> = rows.iter().map(|r| self.sig(r)).collect();
        let same = self.sigs.iter().zip(&sigs).take_while(|(a, b)| a == b).count();
        let old = self.sigs.len();
        self.rows = rows;
        self.sigs = sigs;
        if same < old || same < self.sigs.len() {
            let new = self.sigs.len() - same;
            self.scroller.update(cx, |s, cx| {
                s.splice(same..old, new, cx);
            });
        }
        self.update_status(cx);
        cx.notify();
    }

    /// What a row's `images` list holds: a thumbnail each.
    fn thumbs_of(&self, images: &[UserImage]) -> Vec<template::Scope> {
        images
            .iter()
            .filter_map(|i| self.thumbs.get(&thumb_key(i)))
            .map(|(_, image)| {
                template::Scope::default().slot("thumb", img(ImageSource::Image(image.clone())).size_full().object_fit(ObjectFit::Cover))
            })
            .collect()
    }

    /// Keep an image to draw for each one sent or a tool gave, and only those.
    fn update_thumbs(&mut self) {
        let sent: Vec<&UserImage> = self
            .t
            .entries
            .iter()
            .filter_map(|e| match e {
                Entry::User { images, .. } => Some(images),
                Entry::Tool(t) => Some(&t.images),
                _ => None,
            })
            .flatten()
            .collect();
        if sent.len() == self.thumbs.len() && sent.iter().all(|i| self.thumbs.contains_key(&thumb_key(i))) {
            return;
        }
        let mut thumbs = HashMap::new();
        for i in sent {
            let key = thumb_key(i);
            let image = self.thumbs.remove(&key).unwrap_or_else(|| {
                let format = ImageFormat::from_mime_type(&i.media_type).unwrap_or(ImageFormat::Png);
                (i.bytes.clone(), Arc::new(Image::from_bytes(format, i.bytes.to_vec())))
            });
            thumbs.insert(key, image);
        }
        self.thumbs = thumbs;
    }

    /// Keep a diff for each of `rows` that shows one: an opened file change,
    /// a request to make one.
    fn update_diffs(&mut self, rows: &[Row], cx: &mut Context<Self>) {
        let mut want: HashMap<usize, (String, String, bool)> = HashMap::new();
        for row in rows {
            let (ix, tool) = match row {
                Row::Tool { ix, .. } if self.open.contains(&Open::Tool(*ix)) => match self.t.entries.get(*ix) {
                    Some(Entry::Tool(t)) => (*ix, t.clone()),
                    _ => continue,
                },
                Row::Ask(ix) => match self.t.entries.get(*ix) {
                    Some(Entry::Ask(a)) => (*ix, tool_for(a)),
                    _ => continue,
                },
                _ => continue,
            };
            if let Some(patch) = summary::patch(&tool) {
                let path = tool.input.get("file_path").and_then(Value::as_str).unwrap_or_default().to_owned();
                want.insert(ix, (patch, path, tool.patch.is_some()));
            }
        }
        self.diffs.retain(|ix, _| want.contains_key(ix));
        for (ix, (patch, path, numbered)) in want {
            if self.diffs.get(&ix).is_some_and(|d| d.patch == patch) {
                continue;
            }
            let lang = crate::diff_pane::language_for(std::path::Path::new(&path));
            let files: Vec<DiffFile> =
                DiffFile::parse(&patch).unwrap_or_default().into_iter().map(|f| f.with_language(lang.clone())).collect();
            if files.is_empty() {
                self.diffs.remove(&ix);
                continue;
            }
            let body: Vec<&str> = patch.lines().skip_while(|l| !l.starts_with("@@")).collect();
            let hunks = body.iter().filter(|l| l.starts_with("@@")).count();
            let lines = body.iter().filter(|l| l.starts_with([' ', '+', '-']) || l.is_empty()).count();
            let state = match self.diffs.remove(&ix) {
                Some(d) => {
                    d.state.update(cx, |s, cx| s.set_files(files, cx));
                    d.state
                },
                None => cx.new(|cx| DiffState::new(files, cx)),
            };
            self.diffs.insert(ix, ShownDiff { patch, state, lines, hunks, numbered });
        }
    }

    /// Diff `ix`'s element and its height, if it has one.
    fn diff_view(&self, ix: usize, cx: &App) -> Option<(Diff, f64)> {
        let d = self.diffs.get(&ix)?;
        let el = crate::style::el("chat.diff", cx);
        // As the diff draws them: a line is 1.6 of the code font, the line
        // between hunks 4px (a `Simple` separator); and a border around.
        let row = f32::from(cx.theme().mono_font_size) * 1.6;
        let lines = d.lines.min(el.num("max_rows").max(1.) as usize);
        let height = f64::from(lines as f32 * row + d.hunks as f32 * 4. + 2.);
        let diff = crate::diff_pane::styled(Diff::new(&d.state), cx)
            .line_number(d.numbered)
            .header_visible(false)
            .hunk_separator(DiffHunkSeparator::Simple);
        Some((diff, height))
    }

    /// What a row looks like, as a number that changes when it does.
    fn sig(&self, row: &Row) -> u64 {
        let mut h = DefaultHasher::new();
        format!("{row:?}").hash(&mut h);
        let e = |ix: usize| self.t.entries.get(ix);
        match row {
            Row::User(ix)
            | Row::Text(ix)
            | Row::Thinking(ix)
            | Row::Ask(ix)
            | Row::Elicit(ix)
            | Row::Notice(ix)
            | Row::Raw(ix)
            | Row::Ended(ix) => {
                format!("{:?}", e(*ix)).hash(&mut h);
            },
            Row::Tool { ix, .. } => {
                if let Some(Entry::Tool(t)) = e(*ix) {
                    (t.done(), t.error, t.input.to_string().len(), t.result.as_ref().map(String::len)).hash(&mut h);
                }
            },
            Row::Group(ixs) => {
                for ix in ixs {
                    if let Some(Entry::Tool(t)) = e(*ix) {
                        (t.done(), t.error).hash(&mut h);
                    }
                }
            },
            Row::Worked { .. } | Row::Working => {},
        }
        for o in &self.open {
            let start = match row {
                Row::Tool { ix, .. } | Row::Thinking(ix) | Row::Raw(ix) => Some(*ix),
                Row::Group(ixs) => ixs.first().copied(),
                Row::Worked { user, .. } => Some(*user),
                _ => None,
            };
            if Some(match o {
                Open::Tool(i) | Open::Group(i) | Open::Thinking(i) | Open::Raw(i) | Open::Turn(i) => *i,
            }) == start
            {
                format!("{o:?}").hash(&mut h);
            }
        }
        h.finish()
    }

    /// The timeline's rows, from the entries.
    fn build(&self) -> Vec<Row> {
        let entries = &self.t.entries;
        let mut rows = Vec::new();
        // Each turn starts at a user message.
        let mut starts: Vec<usize> = entries.iter().enumerate().filter(|(_, e)| matches!(e, Entry::User { .. })).map(|(i, _)| i).collect();
        if starts.first() != Some(&0) {
            starts.insert(0, 0);
        }
        for (n, &start) in starts.iter().enumerate() {
            let end = starts.get(n + 1).copied().unwrap_or(entries.len());
            let body = if matches!(entries.get(start), Some(Entry::User { .. })) {
                rows.push(Row::User(start));
                start + 1
            } else {
                start
            };
            let finished = match entries[body..end].last() {
                Some(Entry::TurnEnd(t)) if !t.interrupted && t.error.is_none() => Some(t.ms),
                _ => None,
            };
            let last_text = (body..end).rev().find(|&i| matches!(&entries[i], Entry::Text { text, .. } if !text.trim().is_empty()));
            match (finished, last_text) {
                // The work before the final reply, folded.
                (Some(ms), Some(reply)) if self.has_rows(body..reply) => {
                    rows.push(Row::Worked { user: start, ms });
                    if self.open.contains(&Open::Turn(start)) {
                        self.body_rows(body..reply, &mut rows);
                    }
                    self.body_rows(reply..end, &mut rows);
                },
                _ => self.body_rows(body..end, &mut rows),
            }
        }
        if self.t.busy {
            rows.push(Row::Working);
        }
        rows
    }

    fn has_rows(&self, range: std::ops::Range<usize>) -> bool {
        let mut rows = Vec::new();
        self.body_rows(range, &mut rows);
        !rows.is_empty()
    }

    /// The last of a closed group's `tools` while it runs: it shows under the
    /// summary, which tells only those before it.
    fn running_apart(&self, tools: &[usize]) -> Option<usize> {
        let last = *tools.last()?;
        let running = matches!(self.t.entries.get(last), Some(Entry::Tool(t)) if !t.done());
        (running && self.t.busy && !self.open.contains(&Open::Group(tools[0]))).then_some(last)
    }

    fn body_rows(&self, range: std::ops::Range<usize>, rows: &mut Vec<Row>) {
        let entries = &self.t.entries;
        let mut tools: Vec<usize> = Vec::new();
        let flush = |tools: &mut Vec<usize>, rows: &mut Vec<Row>| match std::mem::take(tools).as_slice() {
            [] => {},
            [one] => rows.push(Row::Tool { ix: *one, nested: false }),
            tools => {
                rows.push(Row::Group(tools.to_vec()));
                if self.open.contains(&Open::Group(tools[0])) {
                    rows.extend(tools.iter().map(|&ix| Row::Tool { ix, nested: true }));
                } else if let Some(last) = self.running_apart(tools) {
                    rows.push(Row::Tool { ix: last, nested: true });
                }
            },
        };
        for i in range {
            match &entries[i] {
                Entry::Tool(_) => tools.push(i),
                // A settled request leaves only its tool call.
                Entry::Ask(a) if a.answer.is_some() => {},
                Entry::TurnEnd(t) if !t.interrupted && t.error.is_none() => {},
                Entry::Text { text, .. } if text.trim().is_empty() => {},
                Entry::Thinking { text, streaming } if text.trim().is_empty() && !streaming => {},
                e => {
                    flush(&mut tools, rows);
                    rows.push(match e {
                        Entry::User { .. } => Row::User(i),
                        Entry::Text { .. } => Row::Text(i),
                        Entry::Thinking { .. } => Row::Thinking(i),
                        Entry::Ask(_) => Row::Ask(i),
                        Entry::Elicit(_) => Row::Elicit(i),
                        Entry::Notice { .. } => Row::Notice(i),
                        Entry::Raw { .. } => Row::Raw(i),
                        _ => Row::Ended(i),
                    });
                },
            }
        }
        flush(&mut tools, rows);
    }

    fn render_row(&self, ix: usize, me: &WeakEntity<Self>, cx: &App) -> AnyElement {
        let Some(row) = self.rows.get(ix) else { return div().into_any_element() };
        let id = me.entity_id();
        let mono = crate::platform::terminal_font(cx);
        let cwd = Some(self.root.as_path());
        let entry = |i: usize| self.t.entries.get(i);
        let toggle = |what: Open| -> Box<template::Frames> {
            let me = me.clone();
            Box::new(move |name, _, f| match (name, f) {
                ("toggle", Framed::Box(d)) => d
                    .id(SharedString::from(format!("chat-toggle-{what:?}")))
                    .cursor_pointer()
                    .on_click(on_event(&me, move |this: &mut Self, _: &ClickEvent, _, cx| this.toggle(what, cx)))
                    .into_any_element(),
                (_, f) => f.into_any_element(),
            })
        };
        match row {
            Row::User(i) => {
                let Some(Entry::User { text, images }) = entry(*i) else { return div().into_any_element() };
                let thumbs = self.thumbs_of(images);
                let scope = template::Scope::new("chat.user")
                    .var("text", text.as_str())
                    .var("has_text", !text.trim().is_empty())
                    .var("has_images", !images.is_empty())
                    .list("images", thumbs);
                template::render_plain("chat.user", scope, cx)
            },
            Row::Text(i) => {
                let Some(Entry::Text { text, streaming }) = entry(*i) else { return div().into_any_element() };
                let md = markdown(SharedString::from(format!("chat-{id}-{i}")), text.clone()).selectable(true).stream_fade(*streaming);
                template::render_plain("chat.text", template::Scope::new("chat.text").slot("markdown", md), cx)
            },
            Row::Thinking(i) => {
                let Some(Entry::Thinking { text, streaming }) = entry(*i) else { return div().into_any_element() };
                let open = self.open.contains(&Open::Thinking(*i));
                let label = if *streaming { "Thinking…" } else { "Thought" };
                let scope = template::Scope::new("chat.thinking")
                    .var("label", label)
                    .var("open", open)
                    .var("running", *streaming)
                    .var("text", text.trim());
                template::render("chat.thinking", scope, &mut *toggle(Open::Thinking(*i)), cx)
            },
            Row::Tool { ix: i, nested } => {
                let Some(Entry::Tool(t)) = entry(*i) else { return div().into_any_element() };
                let open = self.open.contains(&Open::Tool(*i));
                let running = !t.done() && self.t.busy;
                let diff = if open { self.diff_view(*i, cx) } else { None };
                // A diff stands for the change; a failed one still says why.
                let detail = match (&diff, open) {
                    (_, false) => String::new(),
                    (Some(_), _) if !t.error => String::new(),
                    (Some(_), _) => t.result.clone().unwrap_or_default(),
                    (None, _) => summary::detail(t, DETAIL_LINES),
                };
                let thumbs = if open { self.thumbs_of(&t.images) } else { Vec::new() };
                // The images drawn stand for their "[image]" lines.
                let detail = if !thumbs.is_empty() && detail.lines().all(|l| l.trim().is_empty() || l.trim() == "[image]") {
                    String::new()
                } else {
                    detail
                };
                let mut scope = template::Scope::new("chat.tool")
                    .var("has_images", !thumbs.is_empty())
                    .list("images", thumbs)
                    .var("icon", summary::icon(&t.name))
                    .var("label", summary::label(t, cwd))
                    .var("nested", *nested)
                    .var("open", open)
                    .var("running", running)
                    .var("error", t.error)
                    .var("mono", mono)
                    .var("detail", detail)
                    .var("has_diff", diff.is_some());
                if let Some((diff, height)) = diff {
                    scope = scope.var("diff_height", height).slot("diff", diff);
                }
                template::render("chat.tool", scope, &mut *toggle(Open::Tool(*i)), cx)
            },
            Row::Group(ixs) => {
                let apart = self.running_apart(ixs);
                let tools: Vec<&tern_chat::Tool> = ixs
                    .iter()
                    .filter(|&&i| Some(i) != apart)
                    .filter_map(|&i| match entry(i) {
                        Some(Entry::Tool(t)) => Some(t),
                        _ => None,
                    })
                    .collect();
                let first = ixs.first().copied().unwrap_or(0);
                let scope = template::Scope::new("chat.group")
                    .var("summary", summary::group(&tools))
                    .var("count", tools.len().to_string())
                    .var("open", self.open.contains(&Open::Group(first)))
                    .var("error", tools.iter().any(|t| t.error));
                template::render("chat.group", scope, &mut *toggle(Open::Group(first)), cx)
            },
            Row::Worked { user, ms } => {
                let scope = template::Scope::new("chat.worked")
                    .var("time", summary::duration(*ms))
                    .var("open", self.open.contains(&Open::Turn(*user)));
                template::render("chat.worked", scope, &mut *toggle(Open::Turn(*user)), cx)
            },
            Row::Working => {
                let secs = self.started.map_or(0, |s| s.elapsed().as_secs());
                let waiting = self.waiting();
                let label = if waiting {
                    "Waiting for you".to_owned()
                } else if secs == 0 {
                    "Working…".to_owned()
                } else {
                    format!("Working for {}", if secs < 60 { format!("{secs}s") } else { summary::duration(secs * 1000) })
                };
                template::render_plain("chat.working", template::Scope::new("chat.working").var("label", label).var("waiting", waiting), cx)
            },
            Row::Notice(i) => {
                let Some(Entry::Notice { text, error }) = entry(*i) else { return div().into_any_element() };
                let scope = template::Scope::new("chat.notice").var("text", text.as_str()).var("error", *error);
                template::render_plain("chat.notice", scope, cx)
            },
            Row::Raw(i) => {
                let Some(Entry::Raw { title, body }) = entry(*i) else { return div().into_any_element() };
                let scope = template::Scope::new("chat.raw")
                    .var("title", title.as_str())
                    .var("body", body.as_str())
                    .var("mono", mono.clone())
                    .var("open", self.open.contains(&Open::Raw(*i)));
                template::render("chat.raw", scope, &mut *toggle(Open::Raw(*i)), cx)
            },
            Row::Ended(i) => {
                let Some(Entry::TurnEnd(t)) = entry(*i) else { return div().into_any_element() };
                let (text, error) = match &t.error {
                    Some(e) => (e.clone(), true),
                    None => (format!("You stopped after {}", summary::duration(t.ms)), false),
                };
                let scope = template::Scope::new("chat.notice").var("text", text).var("error", error);
                template::render_plain("chat.notice", scope, cx)
            },
            Row::Ask(i) => {
                let Some(Entry::Ask(ask)) = entry(*i) else { return div().into_any_element() };
                self.render_ask(*i, ask, me, mono, cx)
            },
            Row::Elicit(i) => {
                let Some(Entry::Elicit(e)) = entry(*i) else { return div().into_any_element() };
                self.render_elicit(*i, e, me, mono, cx)
            },
        }
    }

    /// Make the text boxes of the forms waiting for the user.
    fn ensure_form_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut wanted = Vec::new();
        for (ix, e) in self.t.entries.iter().enumerate() {
            let Entry::Elicit(e @ Elicit { answer: None, url: None, .. }) = e else { continue };
            for (fi, f) in e.fields.iter().enumerate() {
                if matches!(f.kind, FieldKind::Text | FieldKind::Number { .. }) && !self.form_inputs.contains_key(&(ix, fi)) {
                    let default = f.default.as_ref().map(|d| d.as_str().map_or_else(|| d.to_string(), str::to_owned));
                    wanted.push(((ix, fi), f.description.clone(), default));
                }
            }
        }
        for (key, placeholder, default) in wanted {
            let input = cx.new(|cx| {
                let state = InputState::new(window, cx).placeholder(placeholder);
                match default {
                    Some(d) => state.default_value(d),
                    None => state,
                }
            });
            // Submit is enabled once the required fields hold something.
            cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify()).detach();
            self.form_inputs.insert(key, input);
        }
    }

    /// The values of form `ix` as its answer, or the fields still wanting
    /// one (empty, or not a number).
    fn form_content(&self, ix: usize, cx: &App) -> Result<Value, Vec<String>> {
        let Some(Entry::Elicit(e)) = self.t.entries.get(ix) else { return Err(Vec::new()) };
        let mut content = serde_json::Map::new();
        let mut wanting = Vec::new();
        for (fi, f) in e.fields.iter().enumerate() {
            let value = match &f.kind {
                FieldKind::Text | FieldKind::Number { .. } => {
                    let text = self.form_inputs.get(&(ix, fi)).map(|i| i.read(cx).value().trim().to_owned()).unwrap_or_default();
                    match (&f.kind, text.is_empty()) {
                        (_, true) => None,
                        (FieldKind::Number { integer: true }, _) => match text.parse::<i64>() {
                            Ok(n) => Some(json!(n)),
                            Err(_) => {
                                wanting.push(f.title.clone());
                                continue;
                            },
                        },
                        (FieldKind::Number { .. }, _) => match text.parse::<f64>() {
                            Ok(n) => Some(json!(n)),
                            Err(_) => {
                                wanting.push(f.title.clone());
                                continue;
                            },
                        },
                        _ => Some(Value::String(text)),
                    }
                },
                FieldKind::Bool => self.form_picks.get(&(ix, fi)).cloned().or_else(|| f.default.clone()),
                FieldKind::Choice { many, .. } => match self.form_picks.get(&(ix, fi)).cloned().or_else(|| f.default.clone()) {
                    Some(Value::Array(a)) if *many && a.is_empty() => None,
                    v => v,
                },
            };
            match value {
                Some(v) => {
                    content.insert(f.name.clone(), v);
                },
                None if f.required => wanting.push(f.title.clone()),
                None => {},
            }
        }
        if wanting.is_empty() { Ok(Value::Object(content)) } else { Err(wanting) }
    }

    /// Pick `value` on field `fi` of form `ix` (a choice that takes several
    /// toggles it).
    fn form_pick(&mut self, ix: usize, fi: usize, value: Value, cx: &mut Context<Self>) {
        let many = matches!(
            self.t.entries.get(ix),
            Some(Entry::Elicit(e)) if matches!(e.fields.get(fi).map(|f| &f.kind), Some(FieldKind::Choice { many: true, .. }))
        );
        if many {
            let slot = self.form_picks.entry((ix, fi)).or_insert_with(|| Value::Array(Vec::new()));
            if let Value::Array(list) = slot {
                match list.iter().position(|v| *v == value) {
                    Some(at) => {
                        list.remove(at);
                    },
                    None => list.push(value),
                }
            }
        } else {
            self.form_picks.insert((ix, fi), value);
        }
        cx.notify();
    }

    /// Answer form `ix`: send it (`accept`), or decline.
    fn answer_form(&mut self, ix: usize, accept: bool, cx: &mut Context<Self>) {
        let Some(Entry::Elicit(e)) = self.t.entries.get(ix).cloned() else { return };
        let answer = match (accept, &e.url) {
            (false, _) => ElicitAnswer::Decline,
            (true, Some(_)) => ElicitAnswer::Accept(json!({})),
            (true, None) => match self.form_content(ix, cx) {
                Ok(content) => ElicitAnswer::Accept(content),
                Err(_) => return,
            },
        };
        if let Some(line) = self.driver.elicit_line(&e, &answer) {
            self.write(line);
        }
        self.t.set_elicit_answer(&e.request_id, answer);
        self.form_inputs.retain(|k, _| k.0 != ix);
        self.form_picks.retain(|k, _| k.0 != ix);
        self.refresh(cx);
    }

    fn render_elicit(&self, ix: usize, e: &Elicit, me: &WeakEntity<Self>, mono: SharedString, cx: &App) -> AnyElement {
        let server = if e.server.is_empty() { "An MCP server" } else { e.server.as_str() };
        if let Some(answer) = &e.answer {
            let text = match answer {
                ElicitAnswer::Accept(_) if e.url.is_some() => format!("{server}: you opened the page"),
                ElicitAnswer::Accept(_) => format!("{server}: you sent the form"),
                ElicitAnswer::Decline => format!("{server}: you declined"),
                ElicitAnswer::Withdrawn => format!("{server}: the request was withdrawn"),
            };
            return template::render_plain("chat.notice", template::Scope::new("chat.notice").var("text", text).var("error", false), cx);
        }
        // Each field flat: its title and box, or its options (the first carries the title).
        let mut fields = Vec::new();
        // What each of `fields` picks when clicked: field and value.
        let mut options: Vec<Option<(usize, Value)>> = Vec::new();
        for (fi, f) in e.fields.iter().enumerate() {
            let title = if f.required { format!("{} *", f.title) } else { f.title.clone() };
            let head = |item: template::Scope| item.var("label", title.as_str()).var("description", f.description.as_str());
            let choices: Vec<(Value, String)> = match &f.kind {
                FieldKind::Bool => vec![(json!(true), "Yes".into()), (json!(false), "No".into())],
                FieldKind::Choice { options, .. } => options.clone(),
                FieldKind::Text | FieldKind::Number { .. } => {
                    let input = self.form_inputs.get(&(ix, fi)).map(|i| Input::new(i).small().into_any_element());
                    fields.push(head(template::Scope::default()).var("input", input.is_some()).slot_opt("input", input));
                    options.push(None);
                    continue;
                },
            };
            let picked = self.form_picks.get(&(ix, fi)).or(f.default.as_ref());
            for (oi, (value, label)) in choices.into_iter().enumerate() {
                let on = match picked {
                    Some(Value::Array(list)) => list.contains(&value),
                    Some(v) => *v == value,
                    None => false,
                };
                let mut item = template::Scope::default().var("option", label).var("picked", on);
                if oi == 0 {
                    item = head(item);
                }
                options.push(Some((fi, value)));
                fields.push(item);
            }
        }
        let wanting = match (&e.url, self.form_content(ix, cx)) {
            (Some(_), _) | (_, Ok(_)) => String::new(),
            (None, Err(w)) => format!("Still wanted: {}", w.join(", ")),
        };
        let scope = template::Scope::new(format!("chat-elicit-{ix}"))
            .var("title", format!("{server} asks for your input"))
            .var("message", e.message.as_str())
            .var("url", e.url.clone().unwrap_or_default())
            .var("openable", e.url.as_deref().is_some_and(web_url))
            .var("mono", mono)
            .var("wanting", wanting.clone())
            .var("complete", wanting.is_empty())
            .list("fields", fields);
        let me = me.clone();
        let url = e.url.clone();
        template::render(
            "chat.elicit",
            scope,
            &mut |name, item, f| match (name, f) {
                ("submit", Framed::Button(b)) => b
                    .on_click(on_event(&me, move |this: &mut Self, _: &ClickEvent, _, cx| this.answer_form(ix, true, cx)))
                    .into_any_element(),
                ("decline", Framed::Button(b)) => b
                    .on_click(on_event(&me, move |this: &mut Self, _: &ClickEvent, _, cx| this.answer_form(ix, false, cx)))
                    .into_any_element(),
                // Only a web page: the server chose the URL.
                ("open_url", Framed::Button(b)) => match url.clone().filter(|u| web_url(u)) {
                    Some(url) => b.on_click(move |_, _, cx| cx.open_url(&url)).into_any_element(),
                    None => b.into_any_element(),
                },
                ("option", Framed::Button(b)) => {
                    let Some((fi, value)) = item.and_then(|i| options.get(i)).cloned().flatten() else { return b.into_any_element() };
                    b.on_click(on_event(&me, move |this: &mut Self, _: &ClickEvent, _, cx| this.form_pick(ix, fi, value.clone(), cx)))
                        .into_any_element()
                },
                (_, f) => f.into_any_element(),
            },
            cx,
        )
    }

    fn render_ask(&self, ix: usize, ask: &Ask, me: &WeakEntity<Self>, mono: SharedString, cx: &App) -> AnyElement {
        let id = me.entity_id();
        let input = &ask.input;
        let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or_default();
        let path = || {
            summary::short_path(
                [s("file_path"), s("notebook_path"), s("path")].into_iter().find(|p| !p.is_empty()).unwrap_or_default(),
                Some(&self.root),
            )
        };
        let questions = ask.questions();
        let mut plan = None;
        let (title, detail) = match ask.tool_name.as_str() {
            "Bash" => ("Run this command?".to_owned(), format!("$ {}", s("command"))),
            "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => {
                let detail = if self.diffs.contains_key(&ix) { String::new() } else { summary::detail(&tool_for(ask), DETAIL_LINES) };
                (format!("Change {}?", path()), detail)
            },
            "ExitPlanMode" => {
                plan = Some(s("plan").to_owned());
                ("Approve this plan?".to_owned(), String::new())
            },
            "AskUserQuestion" => (format!("{} has a question", crate::agents::agent_short_name(&self.agent())), String::new()),
            "WebFetch" => ("Fetch this page?".to_owned(), s("url").to_owned()),
            name => {
                let detail = serde_json::to_string_pretty(input).unwrap_or_default();
                let lines: Vec<&str> = detail.lines().take(DETAIL_LINES).collect();
                let name = match name.strip_prefix("mcp__").and_then(|r| r.split_once("__")) {
                    Some((server, tool)) => format!("{server}: {}", tool.replace('_', " ")),
                    None => name.to_owned(),
                };
                (format!("Allow {name}?"), lines.join("\n"))
            },
        };
        // Each question's options, flat: the first of each carries the question.
        let mut options = Vec::new();
        let mut flat = Vec::new();
        for (qi, (text, header, labels, multi)) in questions.iter().enumerate() {
            for (oi, label) in labels.iter().enumerate() {
                let picked = self.picks.iter().any(|p| p.0 == ix && p.1 == qi && &p.2 == label);
                let mut item =
                    template::Scope::new("chat.ask.option").var("label", label.as_str()).var("picked", picked).var("any", *multi);
                if oi == 0 {
                    item = item.var("question", if text.is_empty() { header.as_str() } else { text.as_str() });
                }
                options.push(item);
                flat.push((qi, label.clone()));
            }
        }
        let session = ask.suggestions.as_array().is_some_and(|s| !s.is_empty()) && ask.tool_name != "ExitPlanMode";
        let mut scope = template::Scope::new(format!("chat-ask-{ix}"))
            .var("title", title)
            .var("detail", detail)
            .var("mono", mono)
            .var("session", session)
            .var("questions", !questions.is_empty())
            .var("plan", plan.is_some())
            .var("multi", questions.iter().any(|q| q.3))
            .var("complete", self.picked_answers(ix).is_some())
            .var("allow_label", if ask.tool_name == "ExitPlanMode" { "Approve" } else { "Allow" })
            .var(
                "deny_label",
                match ask.tool_name.as_str() {
                    "ExitPlanMode" => "Keep planning",
                    "AskUserQuestion" => "Skip",
                    _ => "Deny",
                },
            )
            .list("options", options);
        if let Some((diff, height)) = self.diff_view(ix, cx) {
            scope = scope.var("has_diff", true).var("diff_height", height).slot("diff", diff);
        }
        if let Some(plan) = plan {
            scope = scope.slot("markdown", markdown(SharedString::from(format!("chat-plan-{id}-{ix}")), plan).selectable(true));
        }
        let me = me.clone();
        template::render(
            "chat.ask",
            scope,
            &mut |name, item, f| match (name, f) {
                ("allow", Framed::Button(b)) => b
                    .on_click(on_event(&me, move |this: &mut Self, _: &ClickEvent, _, cx| this.answer(ix, Answer::Allow, cx)))
                    .into_any_element(),
                ("allow_session", Framed::Button(b)) => b
                    .on_click(on_event(&me, move |this: &mut Self, _: &ClickEvent, _, cx| this.answer(ix, Answer::AllowSession, cx)))
                    .into_any_element(),
                ("submit", Framed::Button(b)) => {
                    b.on_click(on_event(&me, move |this: &mut Self, _: &ClickEvent, _, cx| this.submit(ix, cx))).into_any_element()
                },
                ("deny", Framed::Button(b)) => b
                    .on_click(on_event(&me, move |this: &mut Self, _: &ClickEvent, _, cx| this.answer(ix, Answer::Deny, cx)))
                    .into_any_element(),
                ("option", Framed::Button(b)) => {
                    let Some((q, label)) = item.and_then(|i| flat.get(i)).cloned() else { return b.into_any_element() };
                    b.on_click(on_event(&me, move |this: &mut Self, _: &ClickEvent, _, cx| this.pick(ix, q, label.clone(), cx)))
                        .into_any_element()
                },
                (_, f) => f.into_any_element(),
            },
            cx,
        )
    }
}

// Debug-build control (see `crate::control`).
impl ChatView {
    pub fn debug_send(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |i, cx| i.set_value(text.to_owned(), window, cx));
        self.send(window, cx);
    }

    pub fn debug_trust(&mut self, cx: &mut Context<Self>) {
        self.trust_folder(cx);
    }

    pub fn debug_state(&self, cx: &App) -> String {
        let rows: Vec<String> = self.rows.iter().map(|r| format!("{r:?}")).collect();
        let slash = self.slash_matches(cx).map(|m| m.iter().take(SLASH_ROWS).map(|c| c.0.clone()).collect::<Vec<_>>());
        format!(
            "trusted={} busy={} live={} chat={:?} mode={} model={:?} picked={:?} commands={} models={} slash={:?}@{} session={} entries={} open={:?} rows=[{}]",
            self.trusted,
            self.t.busy,
            self.live,
            self.chat_id,
            self.mode.as_str(),
            self.t.model,
            self.model_picked,
            self.t.commands.len(),
            self.t.models.len(),
            slash,
            self.slash,
            self.session,
            self.t.entries.len(),
            self.open,
            rows.join(", ")
        )
    }

    pub fn debug_mode(&mut self, mode: &str, cx: &mut Context<Self>) -> String {
        match PermissionMode::parse(mode.trim()) {
            Some(m) => {
                self.set_mode(m, cx);
                "ok".into()
            },
            None => format!("error: unknown mode {mode:?}"),
        }
    }

    pub fn debug_answer(&mut self, how: &str, cx: &mut Context<Self>) -> String {
        let Some(ix) = self.t.entries.iter().rposition(|e| matches!(e, Entry::Ask(a) if a.answer.is_none())) else {
            return "error: no open request".into();
        };
        let answer = match how.trim() {
            "allow" => Answer::Allow,
            "session" => Answer::AllowSession,
            "deny" => Answer::Deny,
            other => return format!("error: unknown answer {other:?}"),
        };
        self.answer(ix, answer, cx);
        "ok".into()
    }
}

/// The `commands` (name, description) whose name holds `query`, ignoring
/// case: those it starts first.
fn command_matches(commands: &[(String, String)], query: &str) -> Vec<(String, String)> {
    let query = query.to_lowercase();
    let (mut first, mut then): (Vec<_>, Vec<_>) = (Vec::new(), Vec::new());
    for c in commands {
        let name = c.0.to_lowercase();
        if name.starts_with(&query) {
            first.push(c.clone());
        } else if name.contains(&query) {
            then.push(c.clone());
        }
    }
    first.extend(then);
    first
}

/// A tool call made of a permission request, to word it.
fn tool_for(ask: &Ask) -> tern_chat::Tool {
    tern_chat::Tool::new(ask.tool_name.clone(), ask.input.clone())
}

/// Where `account`'s Claude saved conversation `id` started in `root`.
/// Claude's Markdown, whose links open only when they are web pages (a
/// reply can carry links from pages Claude read).
fn markdown(id: SharedString, text: impl Into<SharedString>) -> TextView {
    TextView::markdown(id, text.into()).on_link_click(|url, _, _, cx| {
        if web_url(url) {
            cx.open_url(url);
        }
    })
}

/// `url` is a web page (`http` or `https`): what a chat may open from what
/// Claude or an MCP server sent, not a file or another app's scheme.
fn web_url(url: &str) -> bool {
    let scheme = url.split_once(':').map(|(s, _)| s.to_ascii_lowercase());
    matches!(scheme.as_deref(), Some("http" | "https")) && url[url.find(':').unwrap_or(0)..].starts_with("://")
}

/// Where the daemon keeps conversation `session`'s MCP forms, which Claude
/// doesn't save (see `ChatSpec::record`).
fn record_path(session: &str) -> Option<PathBuf> {
    let safe = !session.is_empty() && session.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    safe.then(|| crate::dirs::data().join("chats").join(format!("{session}.jsonl")))
}

fn saved_path(agent: &str, account: &str, root: &std::path::Path, id: &str) -> Option<PathBuf> {
    let config = if agent == tern_agent::CODEX { tern_chat::codex::rollout::codex_home() } else { accounts::claude_dir(account)? };
    tern_chat::sessions::transcript_path(agent, &config, root, id)
}

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_form_inputs(window, cx);
        // Shown with its box focused when a turn ends: seen.
        if self.done_unseen && self.input.focus_handle(cx).is_focused(window) {
            cx.defer_in(window, |this, _, cx| this.looked(cx));
        }
        let me = cx.entity().downgrade();
        let empty = self.rows.is_empty();
        let list = (!empty).then(|| {
            let me = me.clone();
            MessageScroller::new("chat-timeline", self.scroller.clone(), move |ix, _, cx| {
                me.upgrade().map_or_else(|| div().into_any_element(), |v| v.read(cx).render_row(ix, &me, cx))
            })
            // The rows' templates space them (`chat.row`, `chat.work`); the
            // scroller's own gap and inset would add to it.
            .with_row_style(StyleRefinement::default().px_0().pb_0())
            .size_full()
        });
        let typed = !self.input.read(cx).value().trim().is_empty() || !self.attachments.is_empty();
        let attachments = self
            .attachments
            .iter()
            .map(|a| {
                let thumb = img(ImageSource::Image(a.image.clone())).size_full().object_fit(ObjectFit::Cover);
                template::Scope::default().var("name", a.name.as_str()).slot("thumb", thumb)
            })
            .collect();
        let input = Textarea::new(&self.input).appearance(false);
        let slash = self.slash_matches(cx);
        // The window of matches around the picked one.
        let (commands, first) = match &slash {
            Some(m) => {
                let pick = self.slash.min(m.len().saturating_sub(1));
                let first = (pick + 1).saturating_sub(SLASH_ROWS);
                let rows = m
                    .iter()
                    .enumerate()
                    .skip(first)
                    .take(SLASH_ROWS)
                    .map(|(i, (name, about))| {
                        template::Scope::default().var("name", name.as_str()).var("description", about.as_str()).var("on", i == pick)
                    })
                    .collect();
                (rows, first)
            },
            None => (Vec::new(), 0),
        };
        let agent = AgentId::new(self.driver.agent());
        let caps = self.driver.caps();
        let scope = template::Scope::new("chat")
            .var("empty", empty)
            .var("busy", self.t.busy)
            .var("typed", typed)
            .var("agent", crate::agents::agent_name(&agent))
            .var("agent_short", crate::agents::agent_short_name(&agent))
            .var("can_mode", caps.permission_modes)
            .var("can_model", caps.models)
            .var("can_attach", caps.images)
            .var("can_stop", caps.interrupt)
            .var("mode", self.mode.label())
            .var("model", self.model_label())
            .var("slash", slash.is_some())
            .var("slash_loading", self.t.commands.is_empty())
            .var("matched", slash.as_ref().is_some_and(|m| !m.is_empty()))
            .list("commands", commands)
            .var("attached", !self.attachments.is_empty())
            .list("attachments", attachments)
            .slot_opt("list", list)
            .var("untrusted", !self.trusted && self.chat_id.is_none())
            .var("folder", crate::dirs::tilde(&self.root))
            .slot("input", input);
        template::render(
            "chat",
            scope,
            &mut |name, item, f| match (name, f) {
                // Over the message box, the slash command list takes the keys
                // that move and pick in it while it shows.
                ("chat", Framed::Box(d)) => d
                    .on_drop(on_event(&me, |this, paths: &ExternalPaths, window, cx| {
                        this.attach_paths(paths.paths(), window, cx);
                    }))
                    // Images on the clipboard are attached; text goes on to the box.
                    .capture_action(on_event(&me, |this, _: &Paste, window, cx| {
                        if this.paste_images(window, cx) {
                            cx.stop_propagation();
                        }
                    }))
                    .capture_action(on_event(&me, |this, _: &MoveUp, _, cx| {
                        if this.slash_step(-1, cx) {
                            cx.stop_propagation();
                        }
                    }))
                    .capture_action(on_event(&me, |this, _: &MoveDown, _, cx| {
                        if this.slash_step(1, cx) {
                            cx.stop_propagation();
                        }
                    }))
                    .capture_action(on_event(&me, |this, _: &IndentInline, window, cx| {
                        if this.slash_complete(false, window, cx) {
                            cx.stop_propagation();
                        }
                    }))
                    .capture_action(on_event(&me, |this, e: &Enter, window, cx| {
                        if !e.shift && this.slash_complete(true, window, cx) {
                            cx.stop_propagation();
                        }
                    }))
                    .capture_action(on_event(&me, |this, _: &Escape, _, cx| {
                        if this.slash_matches(cx).is_some() {
                            this.slash_dismissed = Some(this.input.read(cx).value().to_string());
                            cx.stop_propagation();
                            cx.notify();
                        }
                    }))
                    .into_any_element(),
                ("attach", Framed::Button(b)) => {
                    b.on_click(on_event(&me, |this: &mut Self, _: &ClickEvent, window, cx| this.pick_images(window, cx))).into_any_element()
                },
                ("unattach", Framed::Button(b)) => {
                    let at = item.unwrap_or(0);
                    b.on_click(on_event(&me, move |this: &mut Self, _: &ClickEvent, _, cx| {
                        if at < this.attachments.len() {
                            this.attachments.remove(at);
                        }
                        cx.notify();
                    }))
                    .into_any_element()
                },
                ("command", Framed::Box(d)) => {
                    let at = first + item.unwrap_or(0);
                    d.id(SharedString::from(format!("chat-command-{at}")))
                        .cursor_pointer()
                        .on_click(on_event(&me, move |this: &mut Self, _: &ClickEvent, window, cx| {
                            this.slash = at;
                            this.slash_complete(false, window, cx);
                            let focus = this.input.read(cx).focus_handle(cx);
                            focus.focus(window, cx);
                        }))
                        .into_any_element()
                },
                ("model", Framed::Button(b)) => {
                    let me = me.clone();
                    let models: Vec<(String, String, String)> =
                        self.t.models.iter().map(|m| (m.value.clone(), m.name.clone(), m.description.clone())).collect();
                    let on = self.model_picked.clone().unwrap_or_else(|| "default".to_owned());
                    let short = crate::agents::agent_short_name(&self.agent());
                    b.dropdown_caret(true)
                        .dropdown_menu(move |m, window, cx| {
                            let entries = models
                                .iter()
                                .map(|(value, name, about)| {
                                    template::Scope::default()
                                        .var("label", name.as_str())
                                        .var("detail", about.as_str())
                                        .var("on", *value == on)
                                })
                                .collect();
                            let values: Vec<String> = models.iter().map(|m| m.0.clone()).collect();
                            let me = me.clone();
                            let items: template::MenuItems = Rc::new(move |name, entry| {
                                let value = values.get(entry.filter(|_| name == "pick")?)?.clone();
                                let me = me.clone();
                                Some(Rc::new(move |_, _, cx| {
                                    let _ = me.update(cx, |this, cx| this.set_model(value.clone(), cx));
                                }))
                            });
                            let scope = template::Scope::new("chat-model")
                                .var("loaded", !models.is_empty())
                                .var("agent_short", short.as_str())
                                .list("models", entries);
                            template::menu("menu.chat_model", scope, m.scrollable(true), items, window, cx)
                        })
                        .into_any_element()
                },
                ("send", Framed::Button(b)) => {
                    b.on_click(on_event(&me, |this: &mut Self, _: &ClickEvent, window, cx| this.send(window, cx))).into_any_element()
                },
                ("trust", Framed::Button(b)) => {
                    b.on_click(on_event(&me, |this: &mut Self, _: &ClickEvent, _, cx| this.trust_folder(cx))).into_any_element()
                },
                ("stop", Framed::Button(b)) => {
                    b.on_click(on_event(&me, |this: &mut Self, _: &ClickEvent, _, cx| this.stop(cx))).into_any_element()
                },
                ("mode", Framed::Button(b)) => {
                    b.on_click(on_event(&me, |this: &mut Self, _: &ClickEvent, _, cx| this.cycle_mode(cx))).into_any_element()
                },
                (_, f) => f.into_any_element(),
            },
            cx,
        )
    }
}

/// Where a sent image's bytes are: the same image (`thumbs` holds the bytes, so
/// the address isn't reused while it is kept).
fn thumb_key(image: &UserImage) -> usize {
    image.bytes.as_ptr() as usize
}

#[cfg(test)]
mod tests {
    use super::{command_matches, web_url};

    #[test]
    fn a_form_opens_only_web_pages() {
        assert!(web_url("https://example.com/login") && web_url("HTTP://example.com"));
        for url in ["file:///etc/passwd", "tern://session/1", "x-apple.systempreferences:com.apple", "javascript:alert(1)", "https:/x", ""]
        {
            assert!(!web_url(url), "{url}");
        }
    }

    #[test]
    fn every_tool_icon_is_bundled() {
        use gpui_kit::AssetSource as _;
        for tool in ["Read", "Edit", "Bash", "Grep", "WebFetch", "Task", "TodoWrite", "mcp__x__y", "AskUserQuestion"] {
            let icon = tern_chat::summary::icon(tool);
            let found = crate::file_icons::AppAssets.load(&format!("icons/{icon}.svg")).ok().flatten().is_some();
            assert!(found, "{tool}'s icon {icon} isn't bundled");
        }
    }

    #[test]
    fn commands_starting_with_the_query_come_first() {
        let commands: Vec<(String, String)> =
            ["review", "compact", "code-review", "Context"].iter().map(|n| ((*n).to_owned(), String::new())).collect();
        let names = |q: &str| command_matches(&commands, q).into_iter().map(|c| c.0).collect::<Vec<_>>();
        assert_eq!(names("co"), ["compact", "code-review", "Context"]);
        assert_eq!(names("rev"), ["review", "code-review"]);
        assert_eq!(names(""), ["review", "compact", "code-review", "Context"]);
        assert!(names("zz").is_empty());
    }
}
