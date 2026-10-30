# Claude chat tabs

## Why

A terminal tab shows Claude Code's TUI. That works, but the TUI isn't a good
fit for some things: long replies, tool output you want to fold, images,
diffs, permission prompts with buttons. Orca has a "native chat" next to its
terminals. Tern gets the same: a tab that draws the conversation as a
timeline (rendered after T3 Code) and still runs the real `claude` CLI, so
settings, hooks, MCP servers, skills, login and CLAUDE.md are the same as in a
terminal tab. The Agent SDK isn't used.

## What

- **New Claude Chat** (palette, File menu, + menu) opens `CenterTab::Chat`.
- Claude runs headless: `claude <user's claude-args> --output-format
  stream-json --input-format stream-json --verbose --include-partial-messages
  --permission-prompt-tool stdio --replay-user-messages --session-id|--resume
  <id> [--model M] [--permission-mode P] --mcp-config <tern MCP>`, started
  through the login shell like terminal tabs.
- It starts when the message box takes focus or the user types, so its slash
  commands and models (from the `initialize` answer) are there before the
  first message.

### Timeline

- The user's messages in bubbles (thumbnails of their images over them); Claude's replies as
  Markdown; thinking, folded.
- Tool calls as one-line rows ("Read src/a.rs", "Ran cargo test"); a run of
  them folded into a summary ("Edited 2 files and ran 1 command"). An opened
  row shows the command and output. A file change shows a real diff
  (gpui-kit's `Diff`): from the result's `structuredPatch` (the file's line
  numbers) once it ran, else worked out from the input with `similar`.
- A finished turn's work folds behind "Worked for 12s" above its final reply;
  the running turn shows "Working for 5s".
- Permission requests (`can_use_tool`) are cards: Allow / Allow for this
  session / Deny, a diff for file changes, the plan for ExitPlanMode, the
  options for AskUserQuestion. A `multiSelect` question's options toggle and
  the card waits for Submit; its answer is a list of labels (a string for a
  single-select one), as `claude` validates.

### Composer

- `Textarea`, Enter sends (Shift+Enter a new line); sent during a turn, a
  message is queued by Claude.
- Typing `/` lists the matching slash commands over the text (those starting
  with the query first). Up/Down move, Tab fills `/name `, Enter fills it (or
  sends one typed out), Escape hides the list until the text changes.
- The permission mode button cycles Default → Accept edits → Plan → Auto
  (`set_permission_mode`), as Shift+Tab does in the terminal. Unpicked, the
  mode shown is Claude's own (Bypass when the user's `claude-args` say so).
- The model button is a dropdown of Claude's models (`set_model`); the pick is
  passed as `--model` when Claude has to start again.
- Images: pasted (⌘V), dropped, or picked with the image button; shown as
  thumbnails with a remove button; PNG, JPEG, GIF and WebP up to 5 MB, sent
  as base64 image blocks before the text. Other dropped files are mentioned
  as `@path`.

### In the daemon

- A chat runs in Tern's daemon (`tern-mux/src/chat.rs`, `ChatHost`): its
  stdio piped, so it goes on while Tern is closed. The daemon keeps its lines
  for replay (stream deltas dropped once their turn's `result` arrives;
  the chat's own answers to requests kept), follows its status (a user
  message starts a turn, `result` ends it, an unanswered `can_use_tool`
  waits for the user), and posts notifications while no UI is connected.
- Protocol (MINOR 12): `SpawnChat`, `AttachChat`, `ChatInput`, `DetachChat`,
  `CloseChat`; `ChatAttached { started, lines, exited }`, `ChatLine`,
  `ChatExited`. MINOR 13: `ListChats` → `Chats`, and `ChatsChanged` to
  watchers. A `ChatSpec` carries the conversation and account, so a UI that
  finds a chat without a tab can open one. Handoff (version 5) passes the
  chats' stdin, stdout and stderr and their metadata to the new daemon.
- A tab is saved as `SavedTab::Chat { session, account, mux }`; the session is
  saved again whenever its chat id changes. Reopened, the tab attaches to the
  chat and shows the history Claude saved before the chat started, then the
  daemon's lines. A chat that ended is resumed (`--resume`) at the next
  message. Closing the tab ends Claude; quitting Tern leaves it running.
- Chats no saved tab knows (a crash before the session was saved) are adopted
  at launch, into the workspace of their folder; ended ones are closed.

### Elsewhere

- The agents panel, the agent monitor, the Dock badge and Keep Awake count chat
  tabs as agents: working, waiting for you, done (until the tab is looked at),
  idle.
- The menu bar lists chats with the background terminals; Open reveals (or
  adopts) the tab, End closes the chat (the conversation stays).
- Debug control socket: `chat-new`, `chat-open ID`, `chat-send TEXT`,
  `chat-state`, `chat-answer allow|session|deny`, `chat-mode MODE`.

## Code

- `crates/tern-chat`: `transcript.rs` (frames folded into entries; saved
  `.jsonl` history), `launch.rs` (arguments, `wire::*` lines written to
  stdin), `summary.rs` (tool wording, patches).
- `crates/tern-app/src/chat_view.rs`, templates `chat*` and `menu.chat_model`
  in `templates.json`, elements `chat.*` in `style.rs`.
- `crates/tern-mux/src/chat.rs`, `server.rs`, `client.rs` (`RemoteChat`,
  `list_chats`), `observer.rs` (`Watched::Chats`).
