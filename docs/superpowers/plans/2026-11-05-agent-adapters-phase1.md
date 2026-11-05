# Agent Adapters Phase 1 (Foundation + Claude port) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every Tern agent feature talks to a registered agent (id + descriptor + declared capabilities) instead of to Claude Code, with Claude ported onto that contract and no visible change for Claude users.

**Architecture:**
- **Registry crate.** A new leaf crate `tern-agent` holds:
  - agent ids;
  - JSON descriptors, built-in ones plus user overrides from `~/.config/tern/agents/*.json`;
  - screen-rule evaluation;
  - the single detector;
  - each built-in's capability set.
- **Capabilities.** Each capability is a per-concern table keyed by agent id, in the crate that owns that concern:
  - the hook lane in `tern-term`;
  - the chat driver in `tern-chat`;
  - sessions and transcripts in `tern-chat`;
  - usage in `tern-app`.
- **Who implements them.** Claude is the only agent that implements them in this phase. Every other agent keeps today's behaviour: detect, launch by typed command, and screen-read status.

**Tech Stack:** Rust 2024 workspace, gpui (`gpui-kit`), serde/serde_json, rmp-serde daemon protocol, the template engine (`crates/tern-app/src/template/`, `templates.json`).

**Spec:** `docs/superpowers/specs/2026-11-05-agent-adapters-design.md`. Read sections 1–6 plus "Calls made". This plan is phase 1 of §6.

## Global Constraints

- **No Claude regression.** Every Claude behaviour stays identical, and the existing tests keep passing with their substance unchanged. That covers:
  - the hook status lane: subagents, monitoring, owed notifications;
  - chat;
  - the terminal ⇄ chat switch;
  - the context guard and handoff;
  - accounts;
  - usage;
  - restore.
- **Never edit the user's global agent config.** Claude keeps its private `--settings` file for each tab.
- **New or reworked UI goes through templates.** Do not hand-build UI with `div()`. Agent-specific copy becomes template variables (`{agent}`), never a hard-coded "Claude".
- **Daemon protocol compatibility uses the existing MINOR rules** (`crates/tern-mux/src/proto.rs`):
  - old clients, old daemons and old saved windows → the new Tern must keep working;
  - a downgrade reading new saves does not need to work;
  - an old client may see non-Claude agent tabs as plain terminals.
- **Nothing is left behind as `if agent == Claude` in a feature.** Claude-only internals (accounts, `claude attach <job>`, Claude session files, Bash-tool shells) stay, but inside Claude-named modules or table entries.
- **Builds and checks run in the repo's `target/`.** Never set `CARGO_TARGET_DIR`, and never `cargo clean`.
- **Code and comments are in English.** Use `/usr/bin/grep` if `grep` misbehaves.
- **CI checks, which must pass at every commit:**
  - `cargo fmt --all --check`
  - `cargo clippy --locked --workspace --all-targets -- -D warnings`
  - `cargo test --locked --workspace --no-fail-fast`

## Review Focus

1. **A window saved before this change**, with `claude\t…` tab lines and `chat\t…` lines, restores Claude tabs and chats with their session, mux, account, guard and handoff intact. Pinned in Task 5.
2. **A new client attached to an old daemon** (`SessionInfo` without `agent`, `claude: true`) still shows and adopts Claude tabs. An old daemon handing a `ClaudeTracker` to a new daemon keeps the tab's hook state. Pinned in Tasks 3 and 4.
3. **User descriptor files that are wrong.** For example:
   - `~/.config/tern/agents/claude.json` is broken JSON;
   - it overrides only `command`;
   - it tries to grant capabilities.

   In every case Claude keeps its capabilities, a broken file is reported and never disables a built-in, and a user file never grants capabilities. Pinned in Task 1.
4. **An agent run through a wrapper** (`node …/codex.js`, a Claude Bash tool's shell under `claude`) is detected exactly as before. Pinned in Task 2.
5. **⌘T still opens a Claude tab**, and the old control commands (`chat-new`, `claude-switch`, `context-state`) and the old action names in `templates.json` still work. Pinned in Task 6.

---

### Task 1: `tern-agent` crate: ids, descriptors, screen rules, registry

**Files:**
- Create: `crates/tern-agent/Cargo.toml`
  - deps: `serde` with derive, `serde_json`, `log`;
  - version `0.1.0`;
  - edition from the workspace.
- Create: `crates/tern-agent/src/lib.rs`: the crate root and docs.
- Create: `crates/tern-agent/src/id.rs`
- Create: `crates/tern-agent/src/descriptor.rs`
- Create: `crates/tern-agent/src/screen.rs`
- Create: `crates/tern-agent/src/registry.rs`
- Create: `crates/tern-agent/agents/{claude,codex,gemini,opencode,cursor,grok,pi}.json`: built-ins, embedded with `include_str!`.
- Test: unit tests in `screen.rs` and `registry.rs`.

**Interfaces:**
- **Produces, ids:**
  - `pub struct AgentId(String)`. It derives Clone, PartialEq, Eq, Hash, Debug, Serialize and Deserialize (`#[serde(transparent)]`) and implements Display.
  - `AgentId::new(&str)`, `as_str()`, `is(&self, id: &str) -> bool`.
  - Built-in id constants: `pub const CLAUDE: &str = "claude"`, and likewise `CODEX`, `GEMINI`, `OPENCODE`, `CURSOR`, `GROK`, `PI`.
- **Produces, capabilities:** `pub enum Capability { Launch, Status, Session, Transcript, Context, Steer, Usage, Chat, Skills }`, which derives Copy, Eq and Hash.
- **Produces, descriptors:**
  - `pub struct Descriptor` with fields `id: AgentId`, `name: String`, `short_name: String` (what menus say, "New Claude Tab"; defaults to `name`; claude = "Claude"), `command: String`, `args: Vec<String>`, `processes: Vec<String>`, `logo: Option<Logo>`, `resume: Vec<String>` (with a `{session}` placeholder), `skills_dir: Option<String>` and `screen: ScreenRules`.
  - `pub struct Logo { light: String, dark: String }`. Both are asset stems such as `"openai-light"`. A brand-colored logo has the same stem twice.
  - `Descriptor::resume_argv(&self, session: &str) -> Option<Vec<String>>`. It returns `None` when `resume` is empty.
- **Produces, screen rules:**
  - `pub struct ScreenRules { blocked: Vec<Rule>, working: Vec<Rule> }`.
  - `pub struct Rule`. All of its fields are optional, and every field that is present must match:
    - `title_contains: String`
    - `bottom_contains: String`
    - `title_any_char: String`
    - `title_lead: Vec<(u32, u32)>`: the title's first char falls in one of these inclusive code-point ranges and is followed by a space.
  - `ScreenRules::read(&self, title: &str, bottom_lowercased: &str) -> Option<ScreenState>`, where `pub enum ScreenState { Working, Blocked, Idle }`.
  - The semantics are exactly those of today's `Agent::read_screen` (`tern-app/src/agents.rs:415`):
    - if any blocked rule matches → `Blocked`;
    - else if `working` is empty → `None`;
    - else if any working rule matches → `Working`;
    - else `Idle`.
- **Produces, registry:**
  - `pub struct Registry`
  - `Registry::builtin() -> Registry`
  - `Registry::load(user_dir: Option<&Path>) -> (Registry, Vec<String /* problems */>)`
  - `get(&self, id: &str) -> Option<&Descriptor>`
  - `all(&self) -> impl Iterator<Item = &Descriptor>`, in built-in order then user ids sorted
  - `detect(&self, programs: &[String]) -> Option<&Descriptor>`
  - `has(&self, id: &str, cap: Capability) -> bool`
  - `builtin_ids() -> &'static [&'static str]`
- **Produces, global:**
  - `pub fn init(user_dir: Option<&Path>) -> Vec<String>`: loads the registry, stores it, and returns the problems.
  - `pub fn registry() -> Arc<Registry>`: built-in only if `init` was never called.

- [ ] **Step 1: Write the failing tests**

  `screen.rs` tests port today's `claude_from_title_and_screen` and `codex_from_title_and_screen` (`tern-app/src/agents.rs:489-509`) verbatim against `Registry::builtin().get("claude").unwrap().screen.read(..)`, with `Some(Idle)` where they had `Some(None)`. Add:
  - `gemini_blocks_on_a_yes_no_prompt_and_cannot_tell_working`. It asserts that `read("gemini", "[y/n]")` gives `Some(Blocked)` and `read("gemini", "")` gives `None`.

  `registry.rs` tests:
  - `detect_matches_exact_program_names`
    - `["zsh","claude"]` → claude
    - `["cursor-agent"]` → cursor
    - `["codex"]` → codex (the stem `procs::foreground_programs` produces for `node …/codex.js`)
    - `["claudette"]` → none
  - `user_file_overrides_fields_and_keeps_capabilities`
    - The dir has `claude.json` = `{"id":"claude","command":"claude-beta"}`.
    - `command == "claude-beta"`, `name == "Claude Code"`, and `has("claude", Chat)`.
  - `user_file_adds_a_basic_agent`
    - `{"id":"amp","name":"Amp","command":"amp","processes":["amp"]}` → `get("amp")` is some.
    - `has("amp", Status)` is false.
    - It is detected from `["amp"]`.
  - `user_file_cannot_grant_capabilities`
    - A `"capabilities":["chat"]` field on a user file is reported as a problem.
    - `has("amp", Chat)` is false.
  - `broken_user_file_is_reported_and_builtins_survive`
    - `claude.json` = `{oops` → one problem naming the file.
    - `get("claude").unwrap().command == "claude"`.
  - `short_name_defaults_to_name`: claude's is "Claude"; a user `amp` with no `short_name` gets "Amp".
  - `resume_argv_fills_the_session`
    - codex `resume_argv("abc") == Some(["codex","resume","abc"])`.

- [ ] **Step 2: Run them to make sure they fail**

  Run: `cargo test -p tern-agent`
  Expected: compile errors (the crate is empty).

- [ ] **Step 3: Implement**
  - **Built-in JSON values:**
    - Names, commands and processes come from `Agent::name`, `command` and `detect` (`tern-app/src/agents.rs:371-412`).
    - Logo stems come from `Agent::logo`.
    - Resume templates follow the spec §2:
      - claude `["claude","--resume","{session}"]`
      - codex `["codex","resume","{session}"]`
      - opencode `["opencode","--session","{session}"]`
      - the others empty
    - `skills_dir`: claude `.claude/skills`, codex and opencode `.agents/skills`.
    - Screen rules are a 1:1 transcription of today's `read_screen` cues. Claude's `title_lead` is `[[0x25D0,0x25D3],[0x2800,0x28FF]]`. The `_` arm's cues go to gemini, opencode, cursor, grok and pi as `blocked`, with no `working`.
  - **Capabilities:** a `match` in `registry.rs` on the built-in id, not a field in the JSON. Claude gets all nine; the others get `[Launch]`.
  - **Overrides:** a field present in the user JSON replaces the built-in's field. Use a `PartialDescriptor` with all fields `Option`, `#[serde(deny_unknown_fields)]`. An unknown field is reported as a problem and the file is skipped.
  - **Global storage:** a `static` `RwLock<Option<Arc<Registry>>>`.

- [ ] **Step 4: Run the tests**

  Run: `cargo test -p tern-agent`
  Expected: PASS.

- [ ] **Step 5: Commit**

  `git add crates/tern-agent Cargo.lock && git commit -m "tern-agent: agent registry, descriptors and screen rules"`

---

### Task 2: One detector, names and logos from the registry; `tern --agents`

**Files:**
- Modify: `crates/tern-app/Cargo.toml` and `crates/tern-mux/Cargo.toml`: add the `tern-agent` path dep.
- Modify: `crates/tern-app/src/agents.rs`
  - Remove `enum Agent`, `LOGOS` and `read_screen`.
  - Keep `status_look`, `ContextUse`, `Handoff`, `ContextGuard`, the labels and `recent_vars`.
  - Move the screen tests out, since Task 1 owns them.
- Modify: `crates/tern-app/src/terminal_view.rs:469-498,481,496,758,884,1136-1162`. These are `set_agent`, `agent()` and `read_agent_status`.
- Modify: `crates/tern-app/src/workspace.rs:2876,2920,2984,3044,3065,3095,3097`, `agent_monitor.rs:181`, `usage_view.rs:277,438`, `shot/targets.rs:15,65-76`.
- Modify: `crates/tern-mux/src/server.rs:1399-1411`. Change `agent_name()` to use `tern_agent::registry().detect(&programs)`. The fallback name for a hooked tab with no match is the claude descriptor's name, which keeps today's "Claude" fallback.
- Modify: `crates/tern-app/src/main.rs`. Call `tern_agent::init(Some(&dirs::config().join("agents")))` before the UI, daemon and agent-command branches; use the existing config-dir helper in `dirs.rs`. Log the problems.
- Modify: `crates/tern-app/src/agent_cli.rs`
  - Add `"--agents"` to the flag list and a `Command::Agents` variant.
  - It prints one line per agent (`id  name  command  capabilities`), then each problem prefixed with `error: `, and exits 0.
  - Add the command to `guide_for`'s list.
- Modify: `crates/tern-web/src/lib.rs:306,495`. Serve `/agents/<stem>.svg` for every built-in logo stem, not just claude.

**Interfaces:**
- Consumes: Task 1's `AgentId`, `Registry::detect`, `Descriptor`, `ScreenRules::read` and `registry()`.
- Produces, in `crates/tern-app/src/agents.rs`:
  - `pub fn agent_icon(id: &AgentId, theme: &Theme) -> AnyElement`. It is the logo image (`agents/<stem>.svg`) when the descriptor has one, else `Icon::new("terminal")`.
  - `pub fn agent_name(id: &AgentId) -> String`.
  - `pub fn asset(path)` keeps serving every built-in stem.
- Produces, `TerminalView`:
  - `agent(&self) -> Option<AgentId>`
  - `set_agent(&mut self, Option<AgentId>)`

- [ ] **Step 1: Write the failing test**

  In `agent_cli.rs` tests:
  - `parse("--agents", &[]) == Ok(Command::Agents)`
  - `agents_listing_names_every_builtin_and_its_problems`: call the pure `agents_text(&Registry, &[String]) -> String`. It contains `claude`, `Claude Code` and `chat`, and one `error: ` line per problem.

- [ ] **Step 2: Run it**

  Run: `cargo test -p tern-app agent_cli`
  Expected: FAIL to compile.

- [ ] **Step 3: Implement**

  Replace every `Agent::X` use with an `AgentId` and registry lookups:
  - `read_agent_status` maps `ScreenState` to `ActivityStatus` (`Working→Working`, `Blocked→Blocked`, `Idle→None`);
  - `NEW_AGENTS` becomes `[CLAUDE, CODEX, OPENCODE]` ids.

  Neither `Agent::` nor the substring checks in `server.rs` may remain.

- [ ] **Step 4: Verify**

  Run:
  - `cargo test -p tern-app -p tern-mux -p tern-web`
  - `cargo clippy --locked --workspace --all-targets -- -D warnings`

  Expected: PASS. `/usr/bin/grep -rn "Agent::" crates/tern-app/src` has no hits.

- [ ] **Step 5: Commit**

  `git commit -am "One agent detector: the registry names, detects and draws every agent"`

---

### Task 3: `AgentStatus` model and the hook-lane table (`tern-term`, `tern-mux` host)

**Files:**
- Create: `crates/tern-term/src/agent_status.rs`
  - Move `TurnState`, `Outcome`, `Wakeup`, `MainAgent`, `Subagent`, `Held` and `ClaudeStatus` here.
  - Rename `ClaudeStatus` to `AgentStatus`. Fields and serde shape stay unchanged, so the wire stays compatible.
- Create: `crates/tern-term/src/agent_lane.rs`.
- Modify: `crates/tern-term/src/claude_status.rs`
  - Keep `ClaudeTracker` here.
  - It now produces `AgentStatus`, imported from `agent_status`.
  - Re-exports for the moved types are not kept; update callers.
- Modify: `crates/tern-term/src/lib.rs`, `session.rs`, `claude_status/tests.rs`.
- Modify: `crates/tern-mux/src/host.rs:109-209,472-503`
  - `ClaudeHooks` becomes `AgentHooks { lane: Option<AgentLane>, .. }`.
  - Claude's session-file reads (`CLAUDE_CHECK`, `idle_reads`, `next_read`) run only when the lane is `AgentLane::Claude`.
- Modify: `crates/tern-mux/src/server.rs:1347-1390,1796`, `tern-mux/tests/handoff.rs`, `tern-mux/tests/daemon.rs`.
- Modify: `crates/tern-app/src/agents.rs:204-226` (`hook_detail`) and `terminal_view.rs:328-332,763,819`.

**Interfaces:**
- Produces, `tern_term::agent_status`: `AgentStatus` with the same fields and methods as the old `ClaudeStatus`, including `held_open()`.
- Produces, `tern_term::agent_lane`:
  - `pub enum AgentLane { Claude(ClaudeTracker) }`, Serialize and Deserialize.
  - `AgentLane::for_agent(id: &str) -> Option<AgentLane>`. It returns `Some(Claude(default))` for `"claude"` and `None` otherwise. This is the table.
  - Methods that delegate to the tracker:
    - `on_event(&mut self, raw: &Value, now: u64, stop_blocked: bool) -> Option<AgentStatus>`
    - `interrupted(&mut self, now) -> Option<AgentStatus>`
    - `question_answered(&mut self, now) -> Option<AgentStatus>`
    - `main_agent_busy() -> bool`
    - `asking() -> bool`
    - `current(&mut self, now) -> Option<AgentStatus>`
    - `deadline() -> Option<u64>`
    - `knows() -> bool`
  - `agent(&self) -> &'static str`
- Produces, `Host`:
  - `agent_hook(&self, raw, stop_blocked)` replaces `claude_hook`.
  - `agent_status() -> Option<AgentStatus>` replaces `claude_status`.
  - `lane() -> Option<AgentLane>` replaces `claude_tracker`.
  - `adopt_lane(AgentLane)` replaces `adopt_claude`.
- Protocol: `proto.rs` `ServerMsg::AgentStatus { id, status: Box<AgentStatus> }` keeps the same variant name. `HandedSession.claude: Option<ClaudeTracker>` stays on the wire under that name. The server maps it to and from `AgentLane::Claude`.

- [ ] **Step 1: Write the failing tests**
  - `tern-term/src/agent_lane.rs`:
    - `claude_lane_folds_hooks_like_the_tracker`. Feed a `UserPromptSubmit` then a `Stop` fixture from `claude_status/` through `AgentLane::for_agent("claude")`. It yields `Working` then `Done`, identical to `ClaudeTracker::on_event` on the same input.
    - `unknown_agents_have_no_lane`: `for_agent("codex").is_none()`.
  - `tern-mux/tests/handoff.rs`:
    - Extend the existing Claude handoff test to `a_claude_trackers_hook_state_survives_a_handoff`, if it isn't there under another name.
    - A `HandedSession { claude: Some(tracker), .. }` serialized with today's field names is adopted, and `agent_status()` equals the status before the handoff.

- [ ] **Step 2: Run them**

  Run: `cargo test -p tern-term agent_lane && cargo test -p tern-mux --test handoff`
  Expected: FAIL to compile.

- [ ] **Step 3: Implement** the move, rename and enum dispatch. `claude_status/tests.rs` changes only its type names.

- [ ] **Step 4: Verify**

  Run: `cargo test -p tern-term -p tern-mux -p tern-app`
  Expected: PASS, with every existing `claude_status` test unchanged in substance.

- [ ] **Step 5: Commit**

  `git commit -am "AgentStatus and the hook-lane table; Claude's tracker is its first lane"`

---

### Task 4: Protocol: sessions say which agent they run

**Files:**
- Modify: `crates/tern-mux/src/proto.rs`
  - `SessionInfo`: add `agent: Option<String>` and `agent_session: Option<String>`, both `#[serde(default)]`.
  - Keep `claude` and `claude_session`, and fill both pairs.
  - Add `// MINOR 16: SessionInfo.agent / agent_session` and `pub const AGENT_INFO_MINOR: u32 = 16`, and bump the current MINOR.
- Modify: the session spawn request at `proto.rs:88` and around it.
  - Add `agent: Option<String>` (`#[serde(default)]`).
  - The daemon records it. An old client sends none, so a spawn with a hook token counts as `"claude"`.
- Modify: `crates/tern-mux/src/server.rs:173-174` (`claude_session` → `agent_session`, internal), the session listing, and the host's agent field.
- Modify: `crates/tern-menubar/src/model.rs:18,71,180`, `tern-menubar/src/app.rs:213`.
  - The labels come from the agent's name: "Claude chat" becomes `"{name} chat"`.
  - With no agent known, the name is "Claude Code".
- Modify: `crates/tern-web/src/lib.rs`, `tern-web/assets/app.js:201,209,265,393,426`.
  - Filter on `s.agent`, falling back to `s.claude`.
  - The logo is `/agents/<stem>.svg` from the agent id. The server sends `logo` per session so the JS needs no table.
- Modify: `crates/tern-app/src/resources.rs:131,322`: `agent: s.agent_id().is_some()`.

**Interfaces:**
- Produces: `SessionInfo::agent_id(&self) -> Option<AgentId>`. It returns `agent` if set, else `Some(claude)` when `claude`, else `None`. Every client reads through this and never reads `claude` directly.
- Produces: `SessionInfo::agent_session_id(&self) -> Option<&str>`. It returns `agent_session`, falling back to `claude_session`.

- [ ] **Step 1: Write the failing tests**

  In `proto.rs` tests, beside the existing `v0::SessionInfo` compatibility test at `:962-1023`:
  - `old_daemon_session_reads_as_claude`. Decode a `SessionInfo` written by a struct without `agent` (`claude: true`, `claude_session: Some("s")`). `agent_id() == Some("claude")` and `agent_session_id() == Some("s")`.
  - `new_daemon_session_still_tells_old_clients_claude`. Encode `SessionInfo { agent: Some("claude"), agent_session: Some("s"), claude: true, claude_session: Some("s"), .. }`, then decode it with the old field set. `claude` is true.
  - `a_codex_session_is_not_claude`: `agent: Some("codex")` gives `claude == false` and `agent_id() == Some("codex")`.

- [ ] **Step 2: Run them**

  Run: `cargo test -p tern-mux proto`
  Expected: FAIL.

- [ ] **Step 3: Implement.** The daemon sets `claude = agent.as_deref() == Some("claude")`.

- [ ] **Step 4: Verify**

  Run: `cargo test -p tern-mux -p tern-menubar -p tern-web -p tern-app`
  Expected: PASS.

- [ ] **Step 5: Commit**

  `git commit -am "Daemon sessions name their agent; old peers still see claude"`

---

### Task 5: Agent tabs and their saved form

**Files:**
- Modify: `crates/tern-app/src/workspace.rs`
  - `TabKind` (`:155`) becomes `enum TabKind { Agent(AgentId), Shell }`, derived Clone and PartialEq; it is no longer `Copy`.
  - Update all 34 `TabKind::Claude` uses.
  - Code that is Claude-only becomes `TabKind::Agent(id) if registry().has(id, Capability::X)`, or an explicit `id.is(CLAUDE)` only where the behaviour is Claude-internal. The internal cases are:
    - accounts;
    - `claude_env`;
    - `CLAUDE_FLAGS`;
    - `spawn_config`'s `--settings` hook file;
    - `claude attach`.
  - `add_agent` (`:2984`) launches an `Agent` tab for an id with `Capability::Launch`, else a Shell tab with the typed command, as today.
  - Rename `claude_env` to `agent_env(id, account)` and `claude_view_switch` to `agent_view_switch`.
- Modify: `crates/tern-app/src/window_state.rs`
  - `SavedTab::Claude {..}` becomes `SavedTab::Agent { agent: AgentId, name, session, mux, account, guard, handoff }`.
  - `SavedTab::Chat` gains `agent: AgentId`.
  - **Encode:**
    - `agent\t<id>\t<session>\t<name>\t<mux>[\t\t<account>\t<guard>\t<handoff>]`, with the same tail rules as today's `claude` line.
    - `chat\t…` keeps today's fields and appends `\t<agent>` only when the agent is not claude.
  - **Decode:** `claude\t…` (today's format) → `Agent { agent: claude, .. }`, plus the new `agent\t…`. A `chat` line with no agent field is claude.
  - `tab_cwd` handles `agent` at index 5.
- Modify: `crates/tern-app/src/main_window.rs:152,341-346,472`. Adopt by `s.agent_id()`.
- Modify: `restart.rs:140`, `update.rs:141`. Wording comes from the agent's name.

**Interfaces:**
- Consumes: `AgentId`, `registry().has`, `SessionInfo::agent_id` and `agent_session_id`.
- Produces: `SavedTab::Agent { agent, .. }`, and `SavedTab::Chat { agent, .. }` (later tasks open chats by agent).

- [ ] **Step 1: Write the failing tests**

  In `window_state.rs` tests, next to the existing round-trip tests at `:760-940`:
  - `a_window_saved_before_agents_restores_claude_tabs`
    - Decode the literal line `claude\ts-1\tWork\t42\t\tacct\t60s\t/r/h.md`.
    - Result: `SavedTab::Agent { agent: claude, session: Some("s-1"), name: Some("Work"), mux: Some(42), account: Some("acct"), guard: Some((60,true)), handoff: Some("/r/h.md") }`.
  - `an_old_chat_line_is_a_claude_chat`: `chat\tsess\t\t7` → `Chat { agent: claude, session: "sess", mux: Some(7), .. }`.
  - `agent_tabs_round_trip`: encode then decode for claude and codex agent tabs, each with and without the optional tail.
  - Update the existing tests (`:771-807,862,928`) to construct `SavedTab::Agent`.

- [ ] **Step 2: Run them**

  Run: `cargo test -p tern-app window_state`
  Expected: FAIL.

- [ ] **Step 3: Implement**, as the files list above.

- [ ] **Step 4: Verify**
  - Run `cargo test -p tern-app`; expect PASS.
  - Manually, using the [[debug-app-screenshots]] flow (`screencapture -l` and `TERN_CONTROL`):
    1. Start the debug build with a saved window containing a Claude tab.
    2. Confirm it resumes.
    3. ⌘T opens a Claude tab.
    4. "New Codex Tab" from the new-agent picker still types `codex` into a shell.

- [ ] **Step 5: Commit**

  `git commit -am "Agent tabs: TabKind::Agent and saved agent tabs; old saves restore as Claude"`

---

### Task 6: Actions, palette, menus, control and templates by agent

**Files:**
- Modify: `crates/tern-app/src/workspace.rs:63-65,117,3298,3350,3737,5655-5657`
  - The actions become `NewAgentTab { agent: String }` and `NewAgentChat { agent: String }`, both gpui actions with data (`#[derive(Action)]` + Deserialize), plus `SwitchAgentView`.
  - Keep `NewClaudeTab` and `NewClaudeChat` as unit actions that dispatch `NewAgentTab { agent: "claude" }` and `NewAgentChat { agent: "claude" }`. This keeps ⌘T, templates and user muscle memory.
  - `"new_claude"` group button → `"new_agent"`, which takes the agent from the template scope.
- Modify: `crates/tern-app/src/palette.rs:96-98,776`
  - Commands are generated per agent: "New {name} Tab" for each `Capability::Launch` agent, and "New {name} Chat" for each `Capability::Chat` agent.
  - "Switch {name} Tab: Terminal ⇄ Chat" appears when the active tab's agent has `Chat`.
  - ⌘T stays on Claude's entry.
- Modify: `crates/tern-app/src/menus.rs:38-39`: the same list as the palette, built from the registry.
- Modify: `crates/tern-app/src/control.rs:41-49`
  - Add `agent-new <id>`, `chat-new [<id>]` (default claude) and `agent-switch`.
  - Keep `claude-switch` and `context-state` as accepted aliases.
- Modify: `crates/tern-app/src/templates.json:270,525,2158,2335,3193-3202,3274,3330,3352,3388-3389,3427`
  - Each "Claude" in copy becomes `{agent}`, filled from the scope.
  - The `when: claude` condition becomes `agent_edited`.
  - "Claude Accounts" and "Claude Options" stay as they are, because they are Claude-internal by design.
  - The menu entries use the new action names.
- Modify: `crates/tern-app/src/file_tree_view.rs:135,269-271,940` and `workspace.rs:2828`
  - The "edited by" badge carries the agent's name: tooltip "Edited by {agent}".
  - Its data comes from the tab's agent.

**Interfaces:**
- Consumes: `registry().all()`, `has()`, `agent_name()` and `agent_icon()` (Task 2), and `TabKind::Agent` (Task 5).
- Produces:
  - actions `NewAgentTab { agent }`, `NewAgentChat { agent }`, `SwitchAgentView`;
  - the template variable `agent` (display name) in every agent-related template scope.

- [ ] **Step 1: Write the failing tests**
  - `palette.rs`:
    - `:776` stays `titles("tab new")[..2] == ["New Claude Tab", "New Shell Tab"]`: labels use the descriptor's `short_name`, so the copy is identical.
    - `palette_offers_a_chat_only_for_agents_with_chat`: "New Claude Chat" is present, and "New Codex Chat" is absent.
  - `control.rs`: `claude_switch_is_still_accepted`, so `parse("claude-switch")` gives the same command as `parse("agent-switch")`.

- [ ] **Step 2: Run them**

  Run: `cargo test -p tern-app palette control`
  Expected: FAIL.

- [ ] **Step 3: Implement**, as above.

- [ ] **Step 4: Verify**
  - Run `cargo test --workspace`; expect PASS.
  - Manual: the palette lists "New Claude Tab", "New Codex Tab", "New OpenCode Tab"…, "New Claude Chat"; ⌘T opens Claude; the menubar menu matches.

- [ ] **Step 5: Commit**

  `git commit -am "Agent actions, palette, menus and templates come from the registry"`

---

### Task 7: Chat model split, chat-driver table, Claude driver

**Files:**
- Create: `crates/tern-chat/src/model.rs`
  - Move the neutral types here: `Entry`, `Tool`, `Ask`, `Elicit`, `Field`, `FieldKind`, `Answer`, `ElicitAnswer`, `TurnEnd`, `Model` and `UserImage`.
  - Also `Transcript`, with only the neutral state: `entries`, `session_id`, `model`, `permission_mode`, `commands`, `models`, `busy`, `version`, `context: Option<tern_bridge::context::Reading>`, `handoff`.
  - Keep the neutral methods: `push_user`, `set_answer`, `set_elicit_answer`, `waiting`, `open_asks`, `resume_busy`, `ended`.
- Create: `crates/tern-chat/src/driver.rs`: the trait and the table.
- Create: `crates/tern-chat/src/claude/mod.rs`, `claude/fold.rs`, `claude/launch.rs`
  - `fold.rs` takes the Claude stream-json folding now in `transcript.rs`, along with its private state: `blocks`, `streamed`, `stream_msg`, `tools`, `asks`, `sent`, `rate_limit` and the `Meter`.
  - `launch.rs` takes today's `launch.rs`: `Launch`, `PermissionMode`, `wire`, `new_session_id`, `transcript_path`.
- Move: `transcript.rs` tests go to `claude/fold.rs`; `recent.rs` stays and becomes Claude's transcript reader (Task 8).
- Modify: `crates/tern-chat/src/lib.rs`. Update the crate docs to say the chat model is neutral and drivers are per agent.
- Modify: `crates/tern-mux/src/chat.rs:98,103,324-360,359,594,633-662`
  - The daemon's meter and guard messages go through the driver: `driver.context()` and `driver.user_line(..)`.
  - Notification text takes the agent name.
  - `ChatSpec` and `HandedChat` gain `agent: String` (`#[serde(default = "claude")]`).
- Modify: `crates/tern-app/src/chat_view.rs:30,227,245-280,422-425,444-462,634,825,1963-1975`
  - The view holds `driver: Box<dyn ChatDriver>`, created from its agent.
  - The strings "Ask Claude anything" and "Claude Chat" take the agent's name through the template scope.
  - `claude_line` moves into `claude/launch.rs`.
  - Composer controls (mode, model) render only when `driver.caps()` has them.
- Modify: `crates/tern-app/src/folder_trust.rs`. It is Claude-internal; call it only when the agent is claude.

**Interfaces:**
- Produces: `pub trait ChatDriver: Send`, with these methods:
  - `fn agent(&self) -> &'static str;`
  - `fn caps(&self) -> ChatCaps;` where `pub struct ChatCaps { permission_modes: bool, models: bool, elicitation: bool, images: bool, interrupt: bool }`.
  - `fn command(&self, user_args: &str, session: &str, resume: bool, model: Option<&str>, mode: Option<PermissionMode>) -> String;`: the shell line the daemon runs. Claude's is today's `claude_line` plus `Launch::args`.
  - `fn start_lines(&self) -> Vec<String>;`: Claude's is `[wire::initialize()]`.
  - `fn fold_line(&mut self, t: &mut Transcript, line: &str) -> Option<String>;`: returns a reply line the chat must write, as `apply_line`'s `Value` serialized today.
  - `fn user_line(&self, session: &str, text: &str, images: &[(&str, &[u8])]) -> (String /*uuid*/, String /*line*/);`
  - `fn answer_line(&self, ask: &Ask, answer: &Answer) -> Option<String>;`
  - `fn elicit_line(&self, e: &Elicit, answer: &ElicitAnswer) -> Option<String>;`
  - `fn interrupt_line(&self) -> Option<String>;`
  - `fn mode_line(&self, mode: PermissionMode) -> Option<String>;`
  - `fn model_line(&self, model: &str) -> Option<String>;`
  - `fn load(&mut self, t: &mut Transcript, path: &Path, before: Option<u64>, extra: Vec<(u64, Value)>) -> io::Result<()>;`: Claude's is today's `Transcript::load_with`.
- Produces, in `tern-chat`:
  - `pub fn chat_driver(agent: &str) -> Option<Box<dyn ChatDriver>>`: claude → `ClaudeDriver`, otherwise `None`. This is the table.
  - `PermissionMode` stays in `model.rs` as the neutral mode set (Claude's values).

- [ ] **Step 1: Make the moved tests the failing tests**
  - Move every test in `transcript.rs:1120-1450` to `claude/fold.rs`.
  - Change their `feed` helper to `ClaudeDriver::default().fold_line(&mut t, &line)`. Assertions stay byte-identical.
  - Add `codex_has_no_chat_driver_yet`: `chat_driver("codex").is_none()`.
  - Add `claude_driver_caps_are_all_on`.

- [ ] **Step 2: Run them**

  Run: `cargo test -p tern-chat`
  Expected: FAIL to compile.

- [ ] **Step 3: Implement** the split. Then wire `tern-mux/src/chat.rs` and `chat_view.rs` through `chat_driver(agent)`.

- [ ] **Step 4: Verify**
  - Run `cargo test --workspace`; expect PASS, with all moved fold tests unchanged.
  - Manual on the debug build:
    1. Open a Claude chat.
    2. Send a message, approve a permission, switch the model and the mode, interrupt.
    3. Close and reopen Tern; the chat history reloads.
    4. Switch terminal ⇄ chat both ways.
    5. With the guard on at a low limit, the chat is asked for a handoff.

- [ ] **Step 5: Commit**

  `git commit -am "tern-chat: neutral chat model and a chat-driver table; Claude stream-json is the first driver"`

---

### Task 8: Sessions, transcript, context, steer and usage tables (Claude entries)

**Files:**
- Create: `crates/tern-chat/src/sessions.rs`
  - `pub fn transcript_path(agent: &str, config: &Path, cwd: &Path, session: &str) -> Option<PathBuf>`. The claude entry is today's `claude::launch::transcript_path`; others return `None`.
  - `pub fn read_recent(agent: &str, path: &Path, cwd: Option<&Path>) -> Recent`. The claude entry is today's `recent::read_recent`; others return `Recent::default()`.
  - `pub fn last_text(agent: &str, path: &Path) -> Option<String>`. The claude entry is `tern_bridge::context::last_transcript_text`.
- Modify: `crates/tern-app/src/workspace.rs:1565-1580,2476-2560,2620-2700,2816-2860`
  - Resume uses `descriptor.resume_argv(session)` for the restart-after-`/exit` and failed-`--resume` paths. Claude's argv equals today's.
  - The recent preview and handoff reads go through `sessions::*` with the tab's agent.
  - The context ring, guard and handoff UI show only for tabs whose agent has `Capability::Context` (ring) or `Context`+`Steer`+`Transcript` (guard).
- Modify: `crates/tern-bridge/src/context.rs`
  - Its hook answers stay Claude's.
  - Add the module doc line: "Claude's steer: hook answers. Other agents' steer is added per agent (spec §2 Steer)."
- Modify: `crates/tern-app/src/usage.rs`, `usage_view.rs`
  - Add `pub trait UsageSource { fn agent(&self) -> &'static str; fn read(&self, account: &str) -> Result<UsageReading, UsageError>; }`. `UsageReading` and `UsageError` are today's types, renamed if they are Claude-named.
  - Add `pub fn usage_source(agent: &str) -> Option<Box<dyn UsageSource>>`, where claude → today's OAuth reader.
  - `usage_view` draws a card for each agent that has a usage source, with that agent's icon.
  - Claude accounts stay as they are.
- Modify: `crates/tern-app/src/agent_monitor.rs:181`: a chat's agent comes from its `SavedTab::Chat.agent` / `ChatSpec.agent`.

**Interfaces:**
- Consumes: Tasks 1–7.
- Produces: the `sessions::{transcript_path, read_recent, last_text}`, `usage_source` and `UsageSource` tables. Phase 2 (Codex) adds entries here and in `AgentLane` / `chat_driver`.

- [ ] **Step 1: Write the failing tests**
  - `tern-chat/src/sessions.rs`:
    - `claude_transcript_path_is_unchanged`: equal to `claude::launch::transcript_path` for the same input.
    - `other_agents_have_no_transcript_yet`: `transcript_path("codex", ..) == None`.
  - `usage.rs`: `only_claude_has_a_usage_source`.
  - `workspace` unit, if a pure helper exists; otherwise a pure fn `guard_shown(agent: &str) -> bool` in `agents.rs` with the test `guard_is_offered_only_where_context_steer_and_transcript_exist`. claude → true, codex → false.

- [ ] **Step 2: Run them**

  Run: `cargo test -p tern-chat sessions && cargo test -p tern-app usage agents`
  Expected: FAIL.

- [ ] **Step 3: Implement**, as above.

- [ ] **Step 4: Verify**
  - Run `cargo test --workspace`; expect PASS.
  - Manual on the debug build, Claude tab:
    1. The context ring shows.
    2. Guard on at a low limit → handoff asked → auto switch to a fresh conversation.
    3. The usage card shows the Claude logo and limits.
    4. The recent preview updates in the sidebar.
  - In a Codex tab: no ring, no guard. The sidebar shows the Codex logo and screen-read status as before.

- [ ] **Step 5: Commit**

  `git commit -am "Per-agent tables for sessions, transcripts, context and usage; Claude fills them"`

---

### Task 9: Docs and full verification

**Files:**
- Modify: the Tern config README text (`crates/tern-app/src/settings.rs`, around `:1638`, which writes `~/.config/tern/README.md`). Add a section, "Agents (`agents/*.json`)", covering:
  - the descriptor fields;
  - that a file with a built-in's id overrides fields;
  - that a new id adds a basic agent (detect, launch, screen status, resume);
  - that `tern --agents` lists the agents and what is wrong;
  - a Codex example override (`args`).
- Modify: `README.md:22,34` if its agent claims need adjusting. Keep it factual.
- Modify: `docs/superpowers/specs/2026-11-05-agent-adapters-design.md`. Add a "Phase 1 done" note with the commit range.

- [ ] **Step 1: Run the full CI set** in the repo `target/`, after `rustc --version` matches CI's stable:
  - `cargo fmt --all --check`
  - `cargo clippy --locked --workspace --all-targets -- -D warnings`
  - `cargo test --locked --workspace --no-fail-fast`
  - Windows clippy via `cargo xwin clippy --target x86_64-pc-windows-msvc --workspace --all-targets -- -D warnings` (per the [[check-before-push]] memory)

  Expected: all clean.

- [ ] **Step 2: Grep for leftovers**

  Run: `/usr/bin/grep -rnE "TabKind::Claude|SavedTab::Claude|ClaudeStatus|Agent::(Claude|Codex)|NewClaudeTab\b" crates --include='*.rs'`

  Expected: hits only in the `NewClaudeTab` alias definition and its dispatch.

- [ ] **Step 3: Manual end-to-end** on the debug build (screenshots per [[debug-app-screenshots]]):
  1. A saved window from `main` (before this branch) restores.
  2. Claude tab status: working → needs input → done, with a subagent keeping it working.
  3. Claude chat, guard and handoff work.
  4. Codex and OpenCode tabs are detected with logos and screen status.
  5. A user `~/.config/tern/agents/amp.json` shows "New Amp Tab".
  6. A broken JSON shows in `tern --agents`.

- [ ] **Step 4: Commit**

  `git commit -am "Agents: config README section; phase 1 done"`
