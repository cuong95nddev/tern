# Agent Adapters Phase 3 (OpenCode) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** OpenCode 2 becomes a first-class Tern agent: a tab launches with Tern's plugin, and its status, session, context ring, guard and handoff, resume and sidebar preview all work; it opens as a chat over `opencode acp`, with the terminal ⇄ chat switch.

**Architecture:**
- **Launch.** `opencode --standalone`, with `OPENCODE_CONFIG_CONTENT` naming Tern's plugin directory. No overlay, nothing written to the user's OpenCode folders, their own plugins keep loading.
- **A thin plugin, the logic in Rust.** The plugin forwards OpenCode's events to the tab's hook endpoint and carries out one answer, `steer`. `OpenCodeFold` in `tern-bridge` turns the forwarded events into Claude-shaped hook events, so the guard, the tracker and the UI's hook handling are reused as they are. This keeps the logic in unit-tested Rust: CI has no JS runner.
- **Chat.** A new `opencode::acp` driver and follower in `tern-chat`, on the `ChatDriver`/`ChatFollow` tables phase 2 built. A chat's history comes from `session/load`'s replay, so no database reader is needed.
- **No SQLite in Tern.** The sidebar preview reads a small per-tab file `OpenCodeFold` writes.

**Tech Stack:** Rust 2024 workspace, serde_json, the rmp-serde daemon protocol (`crates/tern-mux/src/proto.rs`), one JS asset (`include_str!`). Plain JS with no dependencies: the plugin runs in OpenCode's Bun.

**Spec:** `docs/superpowers/specs/2026-11-05-agent-adapters-design.md`, phase 3 of §6 and "Settled for OpenCode". Read that section first: it holds the measured facts.

**Spike tools:** `docs/superpowers/spikes/2026-11-05-opencode/`:
- `mock.py PORT LOGFILE`: a local OpenAI-compatible provider. A prompt with `TOOL: <cmd>` makes it call `shell`; `SLOW` waits 6 s.
- `acp.py`: an ACP client. `drive2.py`: runs a TUI under a PTY and sends timed keys.
- `events-tui.jsonl`: a real TUI session's events (a tool with a permission, then an interrupted turn).

A scratch setup: `HOME=<scratch>/home`, the `XDG_*` variables unset, and `OPENCODE_CONFIG_CONTENT='{"provider":{"mock":{"npm":"@ai-sdk/openai-compatible","name":"Mock","options":{"baseURL":"http://127.0.0.1:18991/v1","apiKey":"x"},"models":{"m":{"name":"m","limit":{"context":100000,"output":1000}}}}},"model":"mock/m"}'`. Close stdin on `opencode run` (`</dev/null`) or it hangs. Run the mock as a Tern service. **Plain `opencode` commands in the real home start the background service: stop it afterwards.**

## Facts beyond the spec (measured on 2.0.30)

**Events** (`{type, data}` from `ctx.event.subscribe`; every session event has `data.sessionID`):
- Turn: `session.execution.started`, then `.succeeded`, `.failed` (`error.type`, `error.message`) or `.interrupted` (`reason`).
- Prompt: `session.inbox.enqueued` with `item.type` `user` or `synthetic`, `item.payload.text`.
- Tool: `session.tool.input.started` has `{id, name}`; `session.tool.called` has `{id, input}`; `session.tool.success` has `{id, content, metadata}`. The tool's name comes only from the first. Shell's name is `shell`.
- Text: `session.text.ended` has `{text}`.
- Tokens: `session.step.ended` has `tokens {input, output, reasoning, cache {read, write}}` for that step. `session.usage.updated` is the running total: don't use it for the window.
- Permission: `permission.asked` `{id, sessionID, action, resources}`, `permission.replied` `{requestID, reply}`.
- Forms (questions): `form.created` `{form}`, `form.replied`, `form.cancelled`, per Orca; not measured.
- Compaction: `session.compaction.started/ended`; names from the binary, the payload not seen.
- `session.created` has no parent field for a root session. A child's parent comes from `ctx.session.get`'s `parentID` (per Orca; not measured).
- A plugin's window size: `ctx.model.list()` gives `{data: [{modelID, providerID, limit: {context}}]}`; match `session.step.started`'s `model`.

**ACP** (`opencode acp`, newline JSON-RPC):
- `initialize` with `{protocolVersion: 1, clientCapabilities, clientInfo}`.
- `session/new {cwd, mcpServers: []}` gives `sessionId`. `session/load {sessionId, cwd, mcpServers: []}` replays, as `session/update` notifications, then answers.
- `session/prompt {sessionId, prompt: [{type: "text", text}]}` answers when the turn ends, with `stopReason` (`end_turn`, `cancelled`) and `usage`.
- `session/cancel {sessionId}` is a notification.
- `session/update`'s `update.sessionUpdate`: `user_message_chunk`, `agent_message_chunk`, `tool_call`, `tool_call_update`, `usage_update {used, size}`, `available_commands_update`.
- `tool_call {toolCallId, name, title, kind, status, rawInput}`; the first one's `rawInput` may lack the command: `tool_call_update` fills `title` and `rawInput`, and at the end `status` `completed`/`failed`, `content`.
- `session/request_permission` is a request with **id starting at 0**: `{toolCall, options: [{optionId: "once", kind: "allow_once"}, {optionId: "always", kind: "allow_always"}, {optionId: "reject", kind: "reject_once"}]}`. The answer: `{"result": {"outcome": {"outcome": "selected", "optionId": …}}}`.
- A provider error is the prompt's JSON-RPC error: `-32603`, `data.errorName` (`provider.quota`).

**Cost:** at idle in an empty folder, `opencode serve --stdio` took 374 MB and its TUI 206 MB. `--standalone` adds the server's share per tab.

## Global Constraints

- **No Claude or Codex regression.** Their behaviour and tests stay the same in substance.
- **Never edit the user's agent config.** Nothing is written to `~/.config/opencode`, `~/.local/share/opencode` or the user's `OPENCODE_CONFIG_DIR`. Tern's files live under the daemon's run dir.
- **A user's own `OPENCODE_CONFIG_CONTENT` is kept**, merged with Tern's `plugins` entry, never replaced.
- **The hook URL's token stays out of the environment.** The tab's environment names a file (mode 0600) that holds it, as Codex's hook file does.
- **New or reworked UI goes through templates.** Agent copy comes from `{agent}` variables.
- **Daemon protocol changes follow the MINOR rules in `proto.rs`** (bump `MINOR` to 19).
- **No `if agent == Claude` in a feature.** OpenCode code lives in OpenCode-named modules and table entries.
- **Minimum OpenCode: 2.0.30**, the version verified. Below that, or when the version can't be told, the tab runs at basic tier: the typed command, no `--standalone`, no plugin.
- **Builds and checks run in the repo's `target/`.** CI checks must pass at every commit: `cargo fmt --all --check`; `cargo clippy --locked --workspace --all-targets -- -D warnings`; `cargo test --locked --workspace --no-fail-fast`. Before the final commit also run Windows clippy via `cargo xwin`.
- **Code and comments are in English.**

## Review Focus

1. **An OpenCode 1.x, one that can't be found, or a shim that prints nothing.** The tab still opens and runs OpenCode at basic tier, with no `--standalone` (1.x doesn't have it) and no `{tern-settings}` left. Pinned in Task 2.
2. **The user has `OPENCODE_CONFIG_CONTENT` set, malformed or with its own `plugins`.** Theirs stays and Tern's is added; when it isn't valid JSON, Tern leaves it alone and the tab runs without Tern's plugin, at basic status. Pinned in Task 1.
3. **Two sessions in one tab, and sub-agents.** The user switches session in the TUI, or a `subagent` tool opens a child session. Only the tab's current root session moves the tab's status; a child shows as a subagent, and its `session.execution.succeeded` never reads as the turn's end. Pinned in Task 3.
4. **OpenCode compacts by itself while the guard is at work.** The window reading falls and the guard must not stay over the limit or ask for a handoff from a stale percentage. Pinned in Task 3.
5. **Tern's daemon is gone or slow while the plugin posts.** OpenCode must not hang or fail: a post that fails or takes over 2 s is dropped, and a turn goes on. Pinned in Task 2's live check. A permission waiting at the switch to chat: the switch ends the TUI first, and a `session/load` refusal shows as a chat notice, not a hang (Task 7).

---

### Task 1: OpenCode wiring in `tern-bridge`

**Files:**
- Create: `crates/tern-bridge/src/opencode_hooks.rs`, `crates/tern-bridge/src/opencode_plugin.js`
- Modify: `crates/tern-bridge/src/lib.rs`

**Interfaces:**
- Produces:
  - `pub fn opencode_wired(version: Option<Version>) -> bool`: true from 2.0.30.
  - `pub fn write_opencode_plugin(dir: &Path) -> io::Result<PathBuf>`: writes `<dir>/opencode-plugin/{index.js,package.json}`, gives that directory.
  - `pub fn write_opencode_hook_file(dir: &Path, token: &str, hook_url: &str) -> io::Result<PathBuf>`, `pub fn remove_opencode_hook_file(dir: &Path, token: &str)`: as Codex's, also removing the preview file of Task 4.
  - `pub fn opencode_config_content(plugin_dir: &Path, existing: Option<&str>) -> Option<String>`.
  - `pub const OPENCODE_HOOK_FILE_ENV: &str = "TERN_OPENCODE_HOOK_FILE"`.

- [ ] **Step 1: Write the failing tests** in `opencode_hooks.rs`:

```rust
#[test]
fn wired_from_the_verified_version() {
    assert!(opencode_wired(version::parse("2.0.30")));
    assert!(!opencode_wired(version::parse("2.0.29")));
    assert!(!opencode_wired(version::parse("1.14.0")));
    assert!(!opencode_wired(None));
}

#[test]
fn config_content_names_the_plugin() {
    let c: Value = serde_json::from_str(&opencode_config_content(Path::new("/run/p"), None).unwrap()).unwrap();
    assert_eq!(c, json!({"plugins": ["/run/p"]}));
}

#[test]
fn the_users_config_content_is_kept() {
    let user = r#"{"model":"a/b","plugins":["/mine"]}"#;
    let c: Value = serde_json::from_str(&opencode_config_content(Path::new("/run/p"), Some(user)).unwrap()).unwrap();
    assert_eq!(c, json!({"model": "a/b", "plugins": ["/mine", "/run/p"]}));
}

#[test]
fn config_content_that_is_not_an_object_is_left_alone() {
    assert_eq!(opencode_config_content(Path::new("/run/p"), Some("{oops")), None);
    assert_eq!(opencode_config_content(Path::new("/run/p"), Some("[1]")), None);
}
```

Plus: the plugin directory has a `package.json` with `"type":"module"`, `"main":"index.js"`, `"exports":{".":"./index.js"}` (OpenCode refuses a plugin path that isn't a directory); the hook file's mode is 0600 on Unix; `remove_opencode_hook_file` removes it.

- [ ] **Step 2:** Run `cargo test -p tern-bridge opencode`. Expected: FAIL, nothing defined.

- [ ] **Step 3: Implement the module and `opencode_plugin.js`.** The plugin is `export default { id: "tern.opencode", setup }`, about 60 lines, no imports beyond `node:fs`. `setup(ctx)`:
  - Returns a no-op disposer when `process.env.TERN_OPENCODE_HOOK_FILE` is unset or `ctx.event?.subscribe` is missing (an `opencode` started by something else in the tab).
  - Subscribes to `ctx.event.subscribe({signal})`, and for each event of the prefixes `session.`, `permission.`, `form.` (not `session.text.delta`, `session.step.streamed`, `session.tool.progress`) POSTs `{hook_event_name: "OpenCode", type, data}` to the URL read from the file, in order, one at a time, with `AbortSignal.timeout(2000)`; a failure is dropped.
  - Adds two facts Rust can't get: `window` (`limit.context` from `ctx.model.list()`, cached by model) on `session.step.ended`, and `parent` (`parentID` from `ctx.session.get`, cached by session) on the first event of a session it hasn't seen.
  - For an answer `{"steer": {"sessionID", "text", "resume"}}`, calls `ctx.session.synthetic({sessionID, text, resume})`.

- [ ] **Step 4:** Run `cargo test -p tern-bridge opencode`. Expected: PASS.

- [ ] **Step 5: Commit** `OpenCode: launch wiring and Tern's plugin`.

### Task 2: The daemon launches OpenCode with its plugin

**Files:**
- Modify: `crates/tern-mux/src/server.rs` (`hook_settings`, `hook_args`, the token cleanup near line 1395), `crates/tern-agent/src/registry.rs`, `crates/tern-mux/tests/daemon.rs`, `crates/tern-app/src/agents.rs` (test only)

**Interfaces:**
- Consumes: Task 1.
- Produces: `struct HookLaunch { args: String, env: Vec<(String, String)> }` (private to `server.rs`), returned by `hook_args`; `hook_settings` gains `env: &mut HashMap<String, String>`. Claude and Codex give an empty `env`. `Launch` joins OpenCode's capabilities, so `start_line` gives `opencode {tern-settings}`.

- [ ] **Step 1: Write the failing tests:**
  - `agents.rs`: `start_line(&opencode, "")` is `opencode {tern-settings}`; `resume_line("opencode {tern-settings}", &id, "ses_1")` is `opencode {tern-settings} --session ses_1`.
  - `daemon.rs`: `an_opencode_tab_runs_standalone_with_the_plugin`, with a fake `opencode` printing `2.0.30` for `--version` and dumping argv and env otherwise. It sees `--standalone`, `OPENCODE_CONFIG_CONTENT` naming a run-dir directory, and `TERN_OPENCODE_HOOK_FILE`.
  - `daemon.rs`: `an_old_opencode_runs_plain`, the fake printing `1.14.0`: no `--standalone`, no `{tern-settings}`, neither variable. The same with a fake that prints nothing.
  - `daemon.rs`: `a_users_broken_config_content_runs_plain`: `{oops` in the tab's env stays, no `--standalone`.

- [ ] **Step 2:** Run `cargo test -p tern-mux opencode`. Expected: FAIL.

- [ ] **Step 3: Implement.** `tern_agent::OPENCODE` in `hook_args`: `Ok(None)` unless `opencode_wired(version)` and `opencode_config_content` gives `Some`, reading `existing` from the spawn config's env, then the process env. Args are `--standalone`.

- [ ] **Step 4:** Run `cargo test -p tern-mux opencode` and `cargo test -p tern-app agents`. Expected: PASS.

- [ ] **Step 5: Live check.** In a scratch home with the mock, a fake hook endpoint (`nc -l` is enough) and a real `opencode --standalone`: the events arrive. Then kill the endpoint mid-turn: the turn still ends in the TUI.

- [ ] **Step 6: Commit** `OpenCode tabs start standalone, with Tern's plugin`.

### Task 3: `OpenCodeFold`, the lane and the guard

**Files:**
- Create: `crates/tern-bridge/src/opencode_fold.rs`, `crates/tern-bridge/src/opencode_fold/fixtures/turn.jsonl` (from `events-tui.jsonl`), `crates/tern-term/src/opencode_status.rs`
- Modify: `crates/tern-term/src/agent_lane.rs`, `crates/tern-term/src/lib.rs`, `crates/tern-mux/src/server.rs` (`on_hook`, `guard_for`, `type_after_turn`), `crates/tern-mux/src/proto.rs`, `crates/tern-bridge/src/lib.rs`

**Interfaces:**
- Produces:
  - `#[derive(Default)] pub struct OpenCodeFold` with `pub fn on_event(&mut self, raw: &Value) -> Vec<Value>` (Claude-shaped hook payloads) and `pub fn answer(&mut self, session: &str, reply: &Value) -> Option<Value>` (a guard reply as the plugin's `steer`).
  - `OpenCodeTracker`, wrapping `ClaudeTracker` as `CodexTracker` does; `AgentLane::OpenCode(OpenCodeTracker)`.
  - `proto.rs`: `opencode: Option<OpenCodeTracker>` beside `codex` (line 573), `MINOR = 19`.
  - `Session` gains `fold: Option<OpenCodeFold>`, set for an OpenCode tab. It isn't saved over a handover: it fills again from the next events.

The mapping, every payload with `session_id`:

| OpenCode | Hook payload |
|---|---|
| `session.created`, root, or the first event of a new root | `SessionStart`, `source` `startup`; `resume` when the session already had a turn (`session.viewed` first) |
| `session.inbox.enqueued`, `item.type` `user` | `UserPromptSubmit`, `prompt` |
| `session.tool.called` | `PreToolUse`, `tool_name`, `tool_input`, `tool_use_id` (name from `session.tool.input.started`, `shell` as `Bash`, `edit`/`write` as `Edit`/`Write`, `filePath` as `file_path`) |
| `session.tool.success`/`.failed` | `PostToolUse`/`PostToolUseFailure` |
| `permission.asked`, `form.created` | `PermissionRequest` |
| `permission.replied`, `form.replied/cancelled` | none: `question_answered` on the lane |
| `session.execution.succeeded` | `Stop`, `last_assistant_message` (last `session.text.ended`), `stop_hook_active` |
| `session.execution.failed` | `StopFailure` |
| `session.execution.interrupted` | `Interrupt` |
| `session.compaction.ended` | `SessionStart`, `source` `compact` |
| `session.step.ended` | `StatusLine` by `statusline::event_for`, `(input + cache.read + cache.write) / window` |
| a child session's events | `SubagentStart` on its first, `SubagentStop` when it ends; nothing else |

- [ ] **Step 1: Write the failing tests** in `opencode_fold.rs`:

```rust
#[test]
fn a_real_session_folds_to_hook_events() {
    let names: Vec<_> = fold_all(include_str!("opencode_fold/fixtures/turn.jsonl"))
        .iter().map(|v| v["hook_event_name"].as_str().unwrap().to_owned()).collect();
    assert_eq!(names.first().unwrap(), "SessionStart");
    for name in ["UserPromptSubmit", "PreToolUse", "PermissionRequest", "PostToolUse", "Stop", "Interrupt"] {
        assert!(names.iter().any(|n| n == name), "{name}: {names:?}");
    }
}

#[test]
fn a_shell_tool_reads_as_bash_with_its_name_from_the_input_event() { /* tool_name "Bash", tool_input.command */ }

#[test]
fn a_childs_end_is_not_the_turns_end() {
    // child `c` with parent `r`: its succeeded gives SubagentStop, never Stop; r's own gives Stop.
}

#[test]
fn another_root_session_takes_the_tab_over() {
    // events of root `a`, then a prompt in root `b`: a new SessionStart for `b`; `a`'s later events give nothing.
}

#[test]
fn the_reading_is_of_the_last_step_not_the_running_total() {
    // step.ended, tokens input 600, cache.read 400, window 10000 => used_percentage 10;
    // session.usage.updated gives nothing.
}

#[test]
fn compaction_resets_the_reading() {
    // a guard fed over the limit, then session.compaction.ended's SessionStart: percent() == 0, no handoff asked.
}

#[test]
fn a_block_answer_steers_and_the_next_stop_says_so() {
    let mut f = OpenCodeFold::default();
    let steer = f.answer("s", &json!({"decision": "block", "reason": "write a handoff"})).unwrap();
    assert_eq!(steer, json!({"steer": {"sessionID": "s", "text": "write a handoff", "resume": true}}));
    // the next session.execution.succeeded of `s` has stop_hook_active true, the one after false.
}
```

In `opencode_status.rs`: `interrupt_ends_the_turn`, `a_permission_waits`, `a_failed_turn_ends` (`StopFailure`). In `proto.rs`: an `OpenCode` lane round-trips, and a session saved by MINOR 18 reads with `opencode: None`.

- [ ] **Step 2:** Run `cargo test -p tern-bridge opencode_fold` and `cargo test -p tern-term opencode`. Expected: FAIL.

- [ ] **Step 3: Implement the fold, the tracker and the lane.** `additionalContext` answers become `steer` with `resume: false`.

- [ ] **Step 4: Wire `on_hook`.** At its top, an event named `OpenCode` goes through the session's fold: each payload through `on_hook` itself, the last non-empty reply through `answer`, given back as the HTTP answer. `permission.replied` and `form.*` ends call the host's `question_answered`.

- [ ] **Step 5: Measure two things in a live scratch TUI, then settle them.**
  - Does a `synthetic` with `resume: false` reach the model on its next request, in the turn and between turns? If not, `additionalContext` during a turn goes with `delivery: "steer"`; record which in the spec.
  - In a bare TUI with no `--prompt`, does `session.created` come at open or at the first prompt, and is `/new` the clear command? `guard_for` gives `Guard::starting_at_first_turn()` when it comes at the first prompt, as Codex's. `type_after_turn` takes the agent and types `/new` for OpenCode, `/clear` for the rest, pinned by a unit test.

- [ ] **Step 6:** Run `cargo test -p tern-bridge -p tern-term -p tern-mux`. Expected: PASS.

- [ ] **Step 7: Commit** `OpenCode: events fold to hooks, the lane, the guard steers through the plugin`.

### Task 4: Sidebar preview, edited badges, the rest of the terminal tab

**Files:**
- Create: `crates/tern-chat/src/opencode/mod.rs`, `crates/tern-chat/src/opencode/preview.rs`
- Modify: `crates/tern-bridge/src/opencode_fold.rs`, `crates/tern-chat/src/sessions.rs`, `crates/tern-chat/src/lib.rs`, `crates/tern-agent/src/registry.rs`

**Interfaces:**
- Consumes: Task 3.
- Produces:
  - `OpenCodeFold::with_preview(path: PathBuf) -> Self`: appends `{"text": …}` per `session.text.ended` and `{"tool": <name>, "input": …}` per `session.tool.called` of the root, and sets `transcript_path` to it on every payload, which `workspace.rs:2948` already reads. The daemon places it at `<run_dir>/<token>.opencode.jsonl`.
  - `preview::read_recent(path, cwd) -> io::Result<Recent>`, `preview::last_text(path) -> Option<String>`; both in `sessions.rs`'s tables. `context_reading` stays `None`: the reading comes as a status line.
  - OpenCode's capabilities: `Launch, Status, Session, Transcript, Context, Steer, Skills`. No `Usage`: it brings its own providers, so the usage view shows no quota.

- [ ] **Step 1: Write the failing tests:** `opencode_reads_its_preview` in `sessions.rs` (a text, then a tool: `text` and `tool` of `Recent`, the tool labelled "Ran …" by `summary.rs`'s Bash entry); `a_half_written_last_line_is_skipped`; `the_preview_is_read_as_a_bounded_tail`; `an_edit_names_its_file` (a `PostToolUse` of `edit` gives `ev.paths` with the file, so the badge lights); in `registry.rs`, the capability list above.

- [ ] **Step 2:** Run `cargo test -p tern-chat opencode`. Expected: FAIL.

- [ ] **Step 3: Implement.** The preview file is bounded by rewriting it from its last 64 KB once past 1 MB.

- [ ] **Step 4:** Run `cargo test --locked --workspace --no-fail-fast`. Expected: PASS.

- [ ] **Step 5: Live check in the dev app** (see the handoff's "Manual testing"), with `XDG_CONFIG_HOME` and `HOME` scratch and the mock: `agent-new opencode`, a turn, the ring and preview show; `context-guard 1` on a resumed session asks for a handoff through the plugin; the agent's reply (a mock prompt can echo `Handoff: x.md`) clears to a fresh conversation; `restart-background` keeps the tab and its lane; a quit and reopen resumes with `--session`. `~/.config/opencode` and `~/.local/share/opencode` have unchanged mtimes.

- [ ] **Step 6: Commit** `OpenCode tabs: preview, edited files, capabilities`.

### Task 5: ACP driver

**Files:**
- Create: `crates/tern-chat/src/opencode/acp.rs`, `crates/tern-chat/src/opencode/fixtures/acp.jsonl` (recorded with `acp.py`: a load, a prompt, a permission, a cancel)
- Modify: `crates/tern-chat/src/driver.rs`, `crates/tern-chat/src/summary.rs`

**Interfaces:**
- Consumes: `ChatDriver`, `Transcript`, `Entry`, `Ask`, `Tool` as `codex/app_server.rs` uses them.
- Produces: `#[derive(Default)] pub struct OpenCodeDriver`, `#[derive(Default)] pub struct OpenCodeFollow`, in `chat_driver` and `chat_follower`. Caps: `ChatCaps { interrupt: true, ..Default::default() }`; model switch and images are left for later (not measured).

Choices the tests leave open:
- `command`: the descriptor's start (`launch::start`), the user's args, then `acp`. No plugin, no `--standalone`, no hook file: a chat's status comes from its driver.
- `start_lines`: `initialize`, then `session/load` (resume) or `session/new`, ids from a counter of Tern's. Server request ids overlap them, starting at 0: tell lines apart with `wrote`, as `CodexFollow` does.
- A message sent before the conversation is open queues, as `CodexDriver` does. A prompt while a turn runs also queues: ACP's prompt answers at the turn's end and has no steer.
- `session/prompt`'s answer is the turn's end: `TurnEnd` with `interrupted` for `cancelled`, `error` from a JSON-RPC error's message.
- `reset`, then `session/load`'s replay rebuilds the transcript, so `load` does nothing and `sessions::transcript_path` stays `None` for OpenCode.
- `clear_line`: `session/new`.
- `summary.rs`: ACP `kind` `execute` as Bash, `edit` as Edit, `read` as Read, `search` as Grep.

- [ ] **Step 1: Write the failing tests** in `acp.rs`, following `app_server.rs`'s `fed`/`server_lines` style:

```rust
#[test] fn a_new_chat_learns_its_session() { /* session/new's answer sets t.session_id */ }
#[test] fn a_resumed_chat_loads_then_shows_the_replay() { /* user + agent chunks become entries; not busy */ }
#[test] fn chunks_of_one_message_join() { /* same messageId: one Entry::Text */ }
#[test] fn a_tool_fills_in_from_its_updates() { /* command from tool_call_update; result and error at the end */ }
#[test] fn a_permission_asks_with_the_tools_input() {
    // request id 0: Entry::Ask, input.command set, even when it precedes any tool_call.
}
#[test] fn answers_pick_the_option() {
    // Allow => "once", AllowSession => "always", Deny => "reject";
    // {"id":0,"result":{"outcome":{"outcome":"selected","optionId":…}}}
}
#[test] fn an_ask_with_no_details_is_rejected_not_shown() {
    // no rawInput and no title: reply optionId "reject" and a notice, as Codex's file changes are.
}
#[test] fn a_cancelled_turn_ends_interrupted() {}
#[test] fn a_provider_error_ends_the_turn_with_its_message() {
    // -32603 "Free usage limit reached": TurnEnd.error, not busy, the next message goes.
}
#[test] fn a_refused_load_is_a_notice_and_asked_again() { /* as a refused Codex resume */ }
#[test] fn a_message_during_a_turn_waits_for_its_end() {}
#[test] fn usage_update_gives_the_reading() { /* used 620, size 100000 => Reading { size: 100000, .. } */ }
#[test] fn a_child_sessions_update_is_left_out() { /* sessionId other than ours */ }
#[test] fn follow_tells_turns_asks_and_session() { /* Said::TurnStarted, Ask, Settled, Reading, Session, TurnEnded */ }
#[test] fn ids_overlap_both_ways() { /* our id 0 answer vs. the server's request 0 */ }
```

- [ ] **Step 2:** Run `cargo test -p tern-chat opencode::acp`. Expected: FAIL.

- [ ] **Step 3: Implement.**

- [ ] **Step 4:** Run `cargo test -p tern-chat`. Expected: PASS.

- [ ] **Step 5: Commit** `OpenCode chat: ACP driver and follower`.

### Task 6: Chats and the guard in the daemon

**Files:**
- Modify: `crates/tern-mux/src/chat.rs`, `crates/tern-agent/src/registry.rs`, `crates/tern-mux/tests/handoff.rs`

**Interfaces:**
- Consumes: Task 5. `Chat` joins OpenCode's capabilities, so `chat_agents` puts New OpenCode Chat in the menus.

- [ ] **Step 1: Write the failing tests** in `chat.rs`, on a scripted ACP server as `codex_server()` is: `opencode_chat_runs_against_a_scripted_server`, `an_opencode_chat_goes_on_in_a_fresh_session`, `a_reattach_replays_an_opencode_chat` (a waiting permission is still asked), `an_unattended_opencode_ask_notifies`. In `handoff.rs`: an OpenCode chat over the limit is asked for a handoff with a user message, as Codex's.

- [ ] **Step 2:** Run `cargo test -p tern-mux opencode_chat`. Expected: FAIL.

- [ ] **Step 3: Implement** what the tests need; `ChatHost` should need only the table entries. Anything `if`-ed on Codex in `chat.rs` becomes a `ChatFollow` or `ChatDriver` method.

- [ ] **Step 4:** Run `cargo test -p tern-mux`. Expected: PASS.

- [ ] **Step 5: Commit** `OpenCode chats run in the daemon`.

### Task 7: Terminal ⇄ chat switch, docs, final checks

**Files:**
- Modify: `crates/tern-app/src/workspace.rs` (test), `crates/tern-app/src/settings.rs` (Tern's config README), `docs/superpowers/specs/2026-11-05-agent-adapters-design.md`, `docs/superpowers/handoffs/2026-11-05-agent-adapters.md`

- [ ] **Step 1: Write the failing tests:** `chats(&TabKind::Agent(opencode))` is true; the agent list for chats is `["claude", "codex", "opencode"]`.

- [ ] **Step 2:** Run `cargo test -p tern-app chats`. Expected: FAIL until the switch's table covers OpenCode; then implement that entry.

- [ ] **Step 3: Live check in the dev app** with the mock: terminal → chat ends the TUI before `session/load` and shows the conversation; a permission waiting at the switch doesn't hang the chat; chat → terminal runs `--session` and shows the chat's turns; an Allow in a chat runs the tool, a Reject doesn't, a stop ends the turn. Take a screenshot of each per the handoff.

- [ ] **Step 4: Docs.** In the config README: OpenCode needs 2.0.30 or later, an older one runs with its screen status; Tern writes nothing to OpenCode's folders; each tab runs its own server. In the spec: phase 3 done and what Task 3's Step 5 settled. Rewrite the handoff for phase 4, with what this phase left.

- [ ] **Step 5: Final checks.** `cargo fmt --all --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo test --locked --workspace --no-fail-fast`, and Windows clippy via `cargo xwin`. Expected: all clean.

- [ ] **Step 6: Commit** `Agents phase 3: docs, spec settled for OpenCode, handoff for phase 4`.
