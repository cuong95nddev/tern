# Agent Adapters Phase 2 (Codex) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Codex becomes a first-class Tern agent:
- a Codex tab launches with Tern's hooks wired in;
- its tab status comes from those hooks;
- its session, transcript, context ring, guard and handoff, usage, resume and sidebar preview all work;
- it can be opened as a chat (`codex app-server`).

**Architecture:**
- **Codex's hooks mirror Claude's protocol.** That covers the event names, the stdin JSON fields, `additionalContext`, and `Stop` blocking with `{"decision":"block","reason"}`. So Codex reuses Tern's existing hook endpoint, relay (`tern --hook <file>`), guard and tracker, with thin Codex entries in each per-capability table.
- **No overlay home.** Tern passes the hooks, together with their trust hashes, as one `-c hooks={…}` argument (Codex's session-flags layer). It never writes into `~/.codex` or a copy of it, so sessions stay in `~/.codex/sessions`.
- **Context.** The daemon reads the context meter from the rollout's `token_count` and feeds it to the guard and the UI as a synthesized `StatusLine` event. The UI and guard code stay unchanged.
- **Chat.** It speaks `codex app-server` JSON-RPC. Before that driver is built, the daemon's chat follower is generalised behind a per-agent table.

**Tech Stack:** Rust 2024 workspace, serde_json, sha2 (already a workspace dependency? check `Cargo.lock`; add `sha2` to `tern-bridge` if not), the rmp-serde daemon protocol (`crates/tern-mux/src/proto.rs`), templates (`crates/tern-app/src/template/`, `templates.json`).

**Spec:** `docs/superpowers/specs/2026-11-05-agent-adapters-design.md`, phase 2 of §6. The spike that settled the spec's "Open items" for Codex is recorded in this plan under "Facts".

## Facts (verified against Codex 0.166.0 on 2026-11-05)

Verified with `~/.local/bin/codex` and its source at tag `rust-v0.166.0`.

**Hooks**
- Hooks are `command` handlers. Codex writes the payload as JSON on stdin and reads the answer as JSON on stdout.
- Payload fields:
  - every event: `session_id`, `transcript_path`, `cwd`, `hook_event_name`, `model`;
  - most events also carry `turn_id` and `permission_mode`;
  - `SessionStart`: `source` (`startup|resume|clear|compact|fork`);
  - `UserPromptSubmit`: `prompt`;
  - tool events: `tool_name`, `tool_input`, `tool_use_id`, plus `tool_response` on `PostToolUse`;
  - `Stop`: `last_assistant_message` and `stop_hook_active`;
  - subagents: `agent_id` and `agent_type`;
  - `SubagentStop`: `agent_transcript_path` as well.
- Tool names: a shell command arrives as `tool_name:"Bash"`, `tool_input:{command}`. A patch arrives as `tool_name:"apply_patch"`, `tool_input:{command:"*** Begin Patch\n*** Add File: c.txt\n…"}`, with paths relative to `cwd`.
- What hook answers do (tried live):
  - `SessionStart` with `{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":…}}`: the model sees the text.
  - `Stop` with `{"decision":"block","reason":R}`: Codex keeps going with R as the prompt. The next `Stop` carries `stop_hook_active:true`.
  - `UserPromptSubmit`, `PostToolUse` and `PreToolUse` accept `hookSpecificOutput.additionalContext` (per the output schemas).
- Events Tern registers: `SessionStart, UserPromptSubmit, PreToolUse, PermissionRequest, PostToolUse, SubagentStart, SubagentStop, Stop, Interrupt`.
  - `Interrupt` is the only signal that the user pressed Esc.
  - Its timeout is clamped to 1–3 s (default 1). Every other event defaults to 600 s.

**Trust**
- Hooks that aren't trusted are skipped with no message.
- Trust lives in `hooks.state."<key>".trusted_hash`, read from the user layer and the session-flags layer.
- Hooks passed with `-c` belong to the session-flags layer. Their key is `/<session-flags>/config.toml:<label>:<group>:<handler>`, with the label in snake_case (`session_start`, `user_prompt_submit`, `pre_tool_use`, `permission_request`, `post_tool_use`, `subagent_start`, `subagent_stop`, `stop`, `interrupt`).
- `trusted_hash` is `"sha256:" + hex(sha256(canonical JSON))`:
  - the hashed value is `{"event_name":<label>,"hooks":[{"async":false,"command":<cmd>,"timeout":<t>,"type":"command"}]}`, plus `"matcher"` only when one is set;
  - canonical means keys sorted and no whitespace;
  - `timeout` is the normalized value, so Tern always writes it explicitly.
- Test vector: command `/private/tmp/claude-501/-Users-cuongpham-ws-tern-ws-tern/5f2e9412-232e-40cd-a0a5-dded8c428e40/scratchpad/spike/hook.sh PreToolUse`, label `pre_tool_use`, timeout 600 → `sha256:c62946eb7aa1c4048196c116ff1952c564e533113073e686049504fdd3ec1efc`.

**Launch**
- With any `-c` override the TUI does not attach to Codex's shared background daemon; it runs embedded. Hooks therefore run in the tab's process with the tab's environment.
- `codex resume <id>` takes `-c` too.

**Rollouts**
- Live rollouts are `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<ts>-<session>.jsonl`. `CODEX_HOME` defaults to `~/.codex`. Archived ones may be `.jsonl.zst`.
- Useful records:
  - `event_msg` / `token_count`, payload `{info:{last_token_usage:{total_tokens,…}, model_context_window}, rate_limits:{limit_id, primary:{used_percent, window_minutes, resets_at}, secondary, plan_type, credits}}`. `info` may be null.
  - `event_msg` / `item_completed`, with `item.type` = `UserMessage`, `AgentMessage` (`content:[{type:"Text",text}]`), `CommandExecution` (`command:[…]`) or `FileChange` (`changes:{<abs path>:…}`).
  - `event_msg` / `task_complete` `{last_agent_message}`.
- Context used, as Codex shows it: `BASELINE = 12000`, `eff = window − BASELINE`, `used = max(0, total − BASELINE)`, `remaining% = round(100·(eff − used)/eff)`. Tern's percent is `100 − remaining%`, and it is 100 when `window ≤ BASELINE`.

**app-server**
- Newline-delimited JSON-RPC on stdio. The handshake is `initialize {clientInfo:{name,version}}` then the `initialized` notification.
- `thread/start` takes no client-chosen id: the thread id comes in its response.
- `thread/resume {threadId}` works on a session another process started and has since ended. It refuses a session a running TUI still holds, with `-32600 … already has an active writer`.
- Approval requests (server→client):
  - `item/commandExecution/requestApproval`, `item/fileChange/requestApproval`, `item/permissions/requestApproval`;
  - `item/tool/requestUserInput`;
  - `mcpServer/elicitation/request`.
- Approval replies are `{decision: accept|acceptForSession|decline|cancel}`.
- Notifications: `turn/started|completed`, `item/started|completed`, `item/agentMessage/delta`, `thread/tokenUsage/updated` (`tokenUsage.last.totalTokens`, `tokenUsage.modelContextWindow`), `account/rateLimits/updated`, `thread/status/changed`.

## Global Constraints

- **No Claude regression.** Every Claude behaviour and test stays the same in substance.
- **Never edit the user's agent config.** Integration goes in per launch only (`-c`, private files under the daemon's run dir).
- **New or reworked UI goes through templates.** Agent copy comes from `{agent}` variables.
- **Daemon protocol changes follow the MINOR rules in `proto.rs`** (bump `MINOR` to 18):
  - a new UI on an old daemon launches Codex as today: basic tier, typed command;
  - an old UI on a new daemon keeps working.
- **No `if agent == Claude` in a feature.** Codex code lives in Codex-named modules and table entries.
- **Builds and checks run in the repo's `target/`.** CI checks must pass at every commit:
  - `cargo fmt --all --check`
  - `cargo clippy --locked --workspace --all-targets -- -D warnings`
  - `cargo test --locked --workspace --no-fail-fast`
- **Before the final commit, also run Windows clippy** via `cargo xwin` (see memory "Check before push").
- **Minimum Codex.** Tern wires Codex in only when `codex --version` ≥ **0.166.0**, the version verified above. Below that, or when the version can't be told, the tab runs at basic tier.
- **Code and comments are in English.**

## Review Focus

1. **A Codex older than 0.166, a Codex that can't be found, or a shell shim that prints nothing.** The tab must still open and run Codex, at basic tier with no `-c`. It must never fail with `{tern-settings}` left in its command line. Pinned in Task 2.
2. **The trust hash drifting from what Codex computes**, through a changed timeout, the `Interrupt` clamp, or a path with spaces or quotes. The hooks would then be silently skipped, and the tab would quietly fall back to screen status. Pinned in Task 1, with the unit vector and an `#[ignore]` live check against `hooks/list`.
3. **Two Codex tabs in one folder at once, and a Codex tab resumed after a Tern restart.** Each tab's hooks must reach its own tab (its own token file), and the session must follow `/clear` and `resume`. Pinned in Tasks 2 and 3.
4. **A rollout that is big, still being written, or has `info: null`.**
   - A big one is read as a bounded tail, never whole.
   - With a half-written last line, or a `token_count` whose `info` is null, the reading is kept from the previous record, never dropped to 0.
   - A `.jsonl.zst` rollout gives no preview and no reading, without an error.

   Pinned in Task 4.
5. **Switching a Codex terminal tab to chat while its TUI still runs.** The switch ends the TUI before `thread/resume`. A `-32600 active writer` refusal shows as a chat notice, never as a hang. Pinned in Task 9.

---

### Task 1: Codex hook wiring in `tern-bridge`

This task builds the `-c hooks=…` argument, the trust hashes, the private hook file and relay, and the paths `apply_patch` edits.

**Files:**
- Create: `crates/tern-bridge/src/codex_hooks.rs`
- Modify: `crates/tern-bridge/src/lib.rs`: re-export.
- Modify: `crates/tern-bridge/src/statusline.rs:58`: `hook_url` also reads `/hook_url`.
- Modify: `crates/tern-bridge/src/relay.rs`: answer per event; the guide only on `SessionStart`.
- Modify: `crates/tern-bridge/src/hooks.rs`: `HookEvent::parse` handles `apply_patch` paths.
- Test: unit tests in those files.

**Interfaces:**
- **Produces:**
  - `pub const CODEX_EVENTS: [(&str, &str); 9]`: (PascalCase event, snake label) in the order listed under Facts.
  - `pub fn codex_trusted_hash(label: &str, command: &str, timeout: u64) -> String`.
  - `pub fn codex_hook_timeout(event: &str) -> u64`: 3 for `Interrupt`, 10 otherwise.
  - `pub fn codex_hooks_arg(command: &str) -> String`. It returns the TOML value text after `hooks=`: `{SessionStart=[{hooks=[{type="command",command=…,timeout=10}]}],…,state={"/<session-flags>/config.toml:session_start:0:0"={trusted_hash="sha256:…"},…}}`. String values are TOML basic strings, escaped by a small `toml_str()` helper (`\\`, `\"`, control chars).
  - `pub fn write_codex_hook_file(dir: &Path, token: &str, hook_url: &str) -> io::Result<PathBuf>`. It writes `codex-hooks-<token>.json` = `{"hook_url":…}`, with the same privacy checks as `write_claude_settings`. Factor that check into a shared `create_private(dir, name)`, and give `pub fn remove_codex_hook_file(dir, token)`.
  - `pub fn codex_launch_args(exe: &Path, file: &Path) -> String`. It returns `-c ` + shell-quoted `hooks=<codex_hooks_arg(relay::command(exe, file))>`; the daemon substitutes this for `{tern-settings}`.
  - `sweep_run_dirs` also ages out stray `codex-hooks-*.json` files, as it does `claude-settings-*`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn trusted_hash_matches_codex_0_166() {
    let cmd = "/private/tmp/claude-501/-Users-cuongpham-ws-tern-ws-tern/5f2e9412-232e-40cd-a0a5-dded8c428e40/scratchpad/spike/hook.sh PreToolUse";
    assert_eq!(codex_trusted_hash("pre_tool_use", cmd, 600), "sha256:c62946eb7aa1c4048196c116ff1952c564e533113073e686049504fdd3ec1efc");
}
#[test] fn hooks_arg_names_every_event_with_its_trust() {
    let v: toml::Value = toml::from_str(&format!("hooks={}", codex_hooks_arg("'/A p/tern' --hook '/r/f.json'"))).unwrap();
    for (event, label) in CODEX_EVENTS {
        let h = &v["hooks"][event][0]["hooks"][0];
        assert_eq!(h["command"].as_str(), Some("'/A p/tern' --hook '/r/f.json'"));
        let t = h["timeout"].as_integer().unwrap() as u64;
        let key = format!("/<session-flags>/config.toml:{label}:0:0");
        assert_eq!(v["hooks"]["state"][&key]["trusted_hash"].as_str().unwrap(), codex_trusted_hash(label, h["command"].as_str().unwrap(), t));
    }
    assert_eq!(v["hooks"]["Interrupt"][0]["hooks"][0]["timeout"].as_integer(), Some(3));
}
#[test] fn hook_file_is_private_and_read_back() { /* write_codex_hook_file then statusline::hook_url(&read(path)) == Some(url); a second write with the same token errs */ }
#[test] fn relay_adds_the_guide_only_on_session_start() {
    // with_guide(Some(json!({"decision":"block","reason":"r"})), "Stop", "g") is returned unchanged
    // with_guide(None, "SessionStart", "g") == {"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"g"}}
}
#[test] fn apply_patch_paths_are_the_files_it_touched() {
    let raw = json!({"hook_event_name":"PostToolUse","tool_name":"apply_patch","cwd":"/w",
        "tool_input":{"command":"*** Begin Patch\n*** Add File: c.txt\n+y\n*** Update File: /abs/d.rs\n*** Move to: e.rs\n*** Delete File: f.txt\n*** End Patch"}});
    let ev = HookEvent::parse("t".into(), raw.to_string().as_bytes()).unwrap();
    assert_eq!(ev.paths, ["/w/c.txt", "/abs/d.rs", "/w/e.rs", "/w/f.txt"].map(PathBuf::from));
}
```

`toml` is a dev-dependency here; add it to `tern-bridge` `[dev-dependencies]` if missing. Keep the existing Claude relay test `adds_the_guide_to_the_answer` passing: it now passes the event name `"SessionStart"`.

- [ ] **Step 2: Run them and see them fail**
  - Run: `cargo test -p tern-bridge codex relay apply_patch`
  - Expected: compile errors, because the functions don't exist yet.

- [ ] **Step 3: Implement**
  - `codex_trusted_hash`: build a `serde_json::Value`, serialize it with sorted keys (serde_json's `Map` is a BTreeMap unless the `preserve_order` feature is on; check `Cargo.lock`, and sort explicitly if it is), then sha256 it.
  - `relay::run`: read `hook_event_name` from the input, and call `with_guide` only when it is `SessionStart`.
  - `HookEvent::parse`: for `tool_name == "apply_patch"`, take the paths from the `*** (Add|Update|Delete) File: ` and `*** Move to: ` lines, joined to `cwd` when relative.

- [ ] **Step 4: Run them and see them pass**
  - Run: `cargo test -p tern-bridge`
  - Expected: all pass.

- [ ] **Step 5: Add the `#[ignore]` live check**
  - Add `#[test] #[ignore] fn codex_trusts_tern_hooks()`. It:
    - writes a `CODEX_HOME` tempdir with `auth.json` absent and an empty `config.toml`;
    - runs `codex -c <hooks arg> app-server`;
    - sends `initialize`, then `initialized`, then `hooks/list {cwds:[tmp]}`;
    - asserts that every hook whose `sourcePath` ends in `<session-flags>/config.toml` has `trustStatus == "trusted"`.
  - Run: `cargo test -p tern-bridge codex_trusts_tern_hooks -- --ignored`
  - Expected: PASS with Codex 0.166 installed.

- [ ] **Step 6: Commit**
  - Message: `Codex hooks: -c hooks with trust hashes, private hook file, relay per event, apply_patch paths`

---

### Task 2: The daemon launches agents by id: `Spawn.agent`, Codex version probe, lane at spawn

**Files:**
- Modify: `crates/tern-mux/src/proto.rs`:
  - `ClientMsg::Spawn` gets `#[serde(default)] agent: Option<String>`;
  - `MINOR = 18`;
  - `pub const AGENT_SPAWN_MINOR: u32 = 18`.
- Modify: `crates/tern-mux/src/server.rs`: `hook_settings` dispatches on the agent; `Session` keeps `agent: Option<AgentId>`; `list` reports it.
- Rename: `crates/tern-mux/src/claude_probe.rs` → `agent_probe.rs`. It becomes generic over the version command, with one cache per (agent, shell).
- Modify: `crates/tern-mux/src/host.rs:494-505`: `agent_hook` no longer creates a Claude lane; the lane comes from `Host::set_agent`.
- Modify: `crates/tern-term/src/agent_lane.rs`: `for_agent("codex")` (the lane type comes in Task 3; this task only threads the id through).
- Modify: `crates/tern-app/src/` daemon client (where `Spawn` is sent): it sends `agent`.
- Test: `crates/tern-mux/tests/daemon.rs`, plus unit tests in `proto.rs` and `agent_probe.rs`.

**Interfaces:**
- **Consumes:** Task 1's `write_codex_hook_file` and `codex_launch_args`.
- **Produces, protocol:** `ClientMsg::Spawn { req, cfg, hook, seed, agent: Option<String> }`.
  - An old client sends no `agent`. With `hook: true` it means Claude, as today.
- **Produces, the version probe:** `pub fn agent_probe::version(agent: &str, cfg: &SpawnConfig, placeholder: &str) -> Option<Version>`.
  - `Version` moves to `tern_bridge::version::Version`: a `(u32, u32, u32)` parse of the first `x.y.z` in the output. `claude_version` keeps its event tables.
  - `pub fn codex_wired(v: Option<Version>) -> bool` is `v >= 0.166.0`.
- **Produces, the integration args:** `Server::hook_settings(&self, agent: &AgentId, probe, args, hook) -> Option<String>`.
  - claude: today's code, unchanged.
  - codex, when `codex_wired`: write the hook file, then replace `{tern-settings}` with `codex_launch_args(exe, file)`.
  - Otherwise: strip the placeholder. That covers ` --settings {tern-settings}` (Claude) and ` {tern-settings}` (any agent).
  - Closing the tab removes the Codex file, as Claude's settings file is removed (find `remove_claude_settings` callers).
- **Produces, the host:** `Host::set_agent(&self, agent: &str)`. It creates `AgentLane::for_agent(agent)` when there is one.
  - `agent_hook` folds into the existing lane.
  - With no lane (old client, Claude), it creates a Claude lane, as today.
  - `reads_session_file` stays Claude-only.
- **Produces, the session list:** `SessionInfo::with_agent(id, agent, session)` gets the tab's real agent, not `claude.then_some(CLAUDE)`.
- **Produces, names and chats:**
  - `Server::agent_name` names the session's own agent first, and only then the detected one; notifications then say "Codex".
  - `ChatHost` spawn passes `spec.agent_id()` to `hook_settings`, so a Codex chat's `{tern-settings}` is filled the same way (Task 9).
- **Produces, the UI side:** UI code calls `spawn(..., agent: Some(id))` only when the daemon's MINOR ≥ `AGENT_SPAWN_MINOR`, and otherwise launches Codex at basic tier (Task 5 uses this).

- [ ] **Step 1: Write the failing tests**
  - `proto.rs`: round-trip `Spawn { agent: Some("codex".into()), .. }`. A `Spawn` encoded without `agent`, as a MINOR 17 client sends it, decodes with `agent: None`.
  - `agent_probe.rs`:
    - `parse("codex-cli 0.166.0") == Some((0,166,0))`;
    - `codex_wired(Some((0,165,9))) == false`;
    - `codex_wired(None) == false`.
  - `tests/daemon.rs`, `codex_tab_gets_its_hook_args`:
    - spawn with `agent: Some("codex")`, `hook: true`, and a script `echo {tern-settings}; read x`;
    - put a fake `codex` script printing `codex-cli 0.166.0` first on `PATH`;
    - assert the screen shows `-c 'hooks={` and the file path, and no token.
  - `tests/daemon.rs`, `old_codex_tab_runs_without_hooks`: the fake prints `0.100.0`, and the screen shows no `{tern-settings}` and no `-c`.
  - `tests/daemon.rs`, `codex_hook_reaches_its_own_tab`:
    - two Codex sessions;
    - a hook POSTed with session A's file URL changes only A's status;
    - `List` reports `agent: Some("codex")` and `agent_session` for A.

- [ ] **Step 2: Run them and see them fail**
  - Run: `cargo test -p tern-mux codex`
  - Expected: FAIL.

- [ ] **Step 3: Implement as in the Interfaces block.** A Codex tab's hook endpoint, token and URL are the same as Claude's: one `HookServer`.

- [ ] **Step 4: Run the tests**
  - Run: `cargo test -p tern-mux` and `cargo test -p tern-term`
  - Expected: all pass, Claude tests included.

- [ ] **Step 5: Commit**
  - Message: `Daemon: Spawn names its agent (MINOR 18); Codex tabs get hooks when codex ≥ 0.166`

---

### Task 3: Codex hook lane

**Files:**
- Create: `crates/tern-term/src/codex_status.rs`, and `crates/tern-term/src/codex_status/fixtures/*.jsonl` (recorded payloads).
- Modify: `crates/tern-term/src/agent_lane.rs`: the `AgentLane::Codex(CodexTracker)` variant, plus `for_agent("codex")` and `agent()`.
- Modify: `crates/tern-mux/src/server.rs:1535`, wherever `AgentLane::Claude(t)` is matched for a handoff to an older daemon. A Codex lane is not handed to a daemon older than MINOR 18; the new daemon gets the lane, and an older one starts without it.

**Interfaces:**
- **Produces:** `pub struct CodexTracker(ClaudeTracker)`. It derives Serialize, Deserialize, Clone, PartialEq and Debug. Codex's hooks are Claude's protocol, so the tracker folds them as Claude's does, except:
  - `Interrupt` → `self.0.interrupted(now)`;
  - Codex has no `StopFailure`, `TeammateIdle`, `PostToolUseFailure`, `SessionEnd`, `PostCompact` or Bash background tasks, so those arms simply never fire.
- **Produces:** each `AgentLane` method gets a `Codex(t)` arm.
- **Note:** `ClaudeTracker` is reused, not renamed. Renaming it to a neutral `HookTracker` is the spec's §3 wording, but it would touch every Claude test; leave it for a later cleanup and record that in the handoff.

- [ ] **Step 1: Record the fixtures**
  - Put the spike's hook payloads in `codex_status/fixtures/`, one JSON per line:
    - `turn.jsonl`: SessionStart, UserPromptSubmit, Stop;
    - `tools.jsonl`: Bash Pre/Post, apply_patch Pre/Post, Stop;
    - `blocked.jsonl`: Stop with `stop_hook_active:false`, then Stop with `true`.
  - The source is `/private/tmp/claude-501/-Users-cuongpham-ws-tern-ws-tern/5f2e9412-232e-40cd-a0a5-dded8c428e40/scratchpad/spike/hooks.log`. Rewrite the paths to `/w`.
  - Also record one live interrupted turn (`Interrupt`), with one-off hooks as in the spike, as `interrupt.jsonl`.

- [ ] **Step 2: Write the failing tests**
  - `codex_turn_goes_working_then_done`: fold `turn.jsonl`; the states go `Working`, then `Done`.
  - `codex_tools_keep_it_working`
  - `a_blocked_stop_keeps_the_turn_going`: the first Stop with `stop_blocked=true` stays `Working`, and the second goes `Done`.
  - `interrupt_ends_the_turn`: `Interrupt` gives the same state as `ClaudeTracker::interrupted`.
  - `permission_request_waits`: a `PermissionRequest` state is `Waiting`.
  - `codex_subagents_count`: `SubagentStart{agent_id:"a"}` puts `a` in subagents, and `SubagentStop` removes it.
  - `a_codex_lane_serializes`
  - In `agent_lane.rs`, `unknown_agents_have_no_lane` now checks `"gemini"`.

- [ ] **Step 3: Run them and see them fail**
  - Run: `cargo test -p tern-term codex`
  - Expected: FAIL.

- [ ] **Step 4: Implement `CodexTracker` as in the Interfaces block**

- [ ] **Step 5: Run the tests**
  - Run: `cargo test -p tern-term && cargo test -p tern-mux`
  - Expected: PASS.

- [ ] **Step 6: Commit**
  - Message: `Codex hook lane: Codex hooks fold like Claude's, Interrupt ends the turn`

---

### Task 4: Codex rollouts: path, recent preview, last text, context reading, rate limits

**Files:**
- Create: `crates/tern-chat/src/codex/mod.rs` and `crates/tern-chat/src/codex/rollout.rs`
- Modify: `crates/tern-chat/src/sessions.rs`: Codex entries in `transcript_path`, `read_recent` and `last_text`.
- Modify: `crates/tern-chat/src/lib.rs`
- Test: `crates/tern-chat/src/codex/rollout.rs` unit tests, plus `crates/tern-chat/src/codex/fixtures/rollout.jsonl`. The fixture is trimmed from the spike's tool-call rollout; keep `session_meta`, `item_completed` ×5, `token_count` ×2 (one with `info: null`), and `task_complete`.

**Interfaces:**
- **Produces:**
  - `pub fn codex_home() -> PathBuf`: `$CODEX_HOME`, or `~/.codex`.
  - `pub fn rollout_path(home: &Path, session: &str) -> Option<PathBuf>`. It searches `home/sessions/*/*/*/rollout-*-<session>.jsonl`, newest date folders first, and stops at the first match. `.jsonl.zst` doesn't count.
  - `pub fn read_recent(path: &Path, cwd: Option<&Path>) -> io::Result<Recent>`. It uses the same `Recent` as Claude's: the last `AgentMessage`'s last sentence, and the last tool as `Bash <command>` or `Edit <file>` from `CommandExecution` / `FileChange` items. It reads only a tail of at most 256 KiB, starting at a line boundary.
  - `pub fn last_text(path: &Path) -> Option<String>`: the last `task_complete.last_agent_message`, or failing that the last `AgentMessage` text.
  - `pub fn context_reading(path: &Path) -> Option<tern_bridge::context::Reading>`: from the last `token_count` with a non-null `info`, using the formula under Facts.
  - `pub fn rate_limits(path: &Path) -> Option<Value>`: the last `token_count.rate_limits`.
  - `pub fn context_percent(total: u64, window: u64) -> f32`: the pure formula, shared with Task 9.
- **Produces, the sessions table:** for `"codex"`, `transcript_path(agent, config, cwd, session)` ignores `cwd` and treats `config` as the Codex home.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn percent_matches_codex() { assert_eq!(context_percent(14_653, 258_400).round(), 1.); assert_eq!(context_percent(258_400, 258_400), 100.); assert_eq!(context_percent(5, 10_000), 100.); }
#[test] fn reading_skips_null_info_and_keeps_the_last() { /* fixture: size 258400, percent from last_token_usage.total_tokens 14653 */ }
#[test] fn recent_says_the_last_sentence_and_tool() { /* fixture: text "Done.", tool label names "c.txt" */ }
#[test] fn last_text_is_the_turns_answer() { assert_eq!(last_text(&fixture()).as_deref(), Some("Done.")); }
#[test] fn rollout_path_finds_the_session_and_ignores_zst() { /* tempdir sessions/2026/11/05/rollout-x-ID.jsonl → found; only .jsonl.zst → None */ }
#[test] fn a_half_written_last_line_is_skipped() { /* append `{"type":"event_msg","pay` → same reading */ }
#[test] fn a_big_rollout_is_read_from_its_tail() { /* 2 MiB of filler lines then the fixture → same results */ }
#[test] fn sessions_table_reads_codex() { /* sessions::last_text("codex", path) == Some("Done.") */ }
```

- [ ] **Step 2: Run them and see them fail**
  - Run: `cargo test -p tern-chat codex`
  - Expected: FAIL.

- [ ] **Step 3: Implement as in the Interfaces block.** Reuse `recent.rs`'s sentence-trimming helper; don't copy it.

- [ ] **Step 4: Run the tests**
  - Run: `cargo test -p tern-chat`
  - Expected: PASS.

- [ ] **Step 5: Commit**
  - Message: `Codex rollouts: transcript path, recent preview, last text, context reading, rate limits`

---

### Task 5: Codex terminal tabs end to end: launch, meter, guard, resume, capabilities

**Files:**
- Modify: `crates/tern-agent/src/registry.rs::builtin_capabilities`: codex gets `Launch, Status, Session, Transcript, Context, Steer, Usage, Skills` (`Chat` comes in Task 9).
- Modify: `crates/tern-app/src/agents.rs`: `start_line` per agent. Claude: unchanged. Codex: `<command> <args> <user args> {tern-settings}`. `launch_line` stays for basic-tier agents.
- Modify: `crates/tern-app/src/workspace.rs:1158-1209`, `spawn_config`:
  - an agent with `Launch` runs `start_line`, with resume through `resume_line`;
  - it is spawned with `agent` when the daemon is ≥ `AGENT_SPAWN_MINOR`;
  - otherwise it runs as basic tier.
  - Claude's `claude attach <job>` stays in its own arm.
- Modify: `crates/tern-mux/src/server.rs::on_hook`: for a Codex session, before the guard sees the hook, `codex::rollout::context_reading(transcript_path)` is turned into a `StatusLine` event, which goes through the same path a Claude status line takes (guard reading, UI forward, queue).
- Modify: `crates/tern-bridge/src/statusline.rs`: `pub fn event_for(session: &str, r: Reading) -> Value`, the synthesized line in the shape `ContextUse::read` and `context::used_percent` already parse.
- Modify: `crates/tern-app/src/workspace.rs`, where the UI learns a tab's transcript (`transcript_path` callers): pass the Codex home for codex.
- Modify: `crates/tern-mux/src/server.rs` / `tern-bridge/src/relay.rs`: the agent guide goes in at `SessionStart` for every agent with `Steer`, naming that agent's `skills_dir`.
- Test: `crates/tern-mux/tests/daemon.rs`, `crates/tern-app/src/agents.rs` unit tests.

**Interfaces:**
- **Consumes:** Tasks 1–4.
- **Produces:**
  - `pub fn start_line(d: &Descriptor, user_args: &str) -> String`, now per agent.
  - The registry test `only_claude_launches_with_tern_wired_in_so_far` becomes `claude_and_codex_launch_with_tern_wired_in`.

- [ ] **Step 1: Write the failing tests**
  - `agents.rs`:
    - `start_line(codex, "--foo") == "codex --foo {tern-settings}"`;
    - `resume_line("codex --foo {tern-settings}", codex, "id")` gives `codex resume id --foo {tern-settings}`, the `resume` argv from the descriptor followed by the rest of the start line's arguments. Fix `resume_line` per descriptor if it only appends today.
  - `tests/daemon.rs`, `codex_guard_asks_for_a_handoff_past_its_limit`:
    - the fake codex script POSTs `UserPromptSubmit`, then `Stop`, with `transcript_path` set to a fixture rollout at 60%;
    - the guard limit is 50;
    - the `Stop` answer is `{"decision":"block",…}` (the handoff request);
    - the UI receives a `StatusLine` hook whose reading is 60%.
  - `tests/daemon.rs`, `codex_meter_reads_on_each_hook`: the rollout grows between two hooks, and the second `StatusLine` has the new percent.
  - `tests/daemon.rs`, `old_daemon_codex_is_basic_tier`: spawning against a daemon that reports MINOR 17 sends `cfg` with no `{tern-settings}`. Unit-test the decision function `fn codex_wired_spawn(daemon_minor: u32) -> bool` in `workspace.rs` if a daemon double is too heavy.

- [ ] **Step 2: Run them and see them fail**
  - Run: `cargo test -p tern-mux codex_guard codex_meter && cargo test -p tern-app agents`
  - Expected: FAIL.

- [ ] **Step 3: Implement as in the Files list**
  - The guard is not changed. Its `Action::Clear` types `/clear`, which Codex has; its `SessionStart` `source` is `clear`.

- [ ] **Step 4: Run all tests**
  - Run: `cargo test --workspace`
  - Expected: PASS.

- [ ] **Step 5: Check by hand in the debug build** (handoff "Manual testing")
  - `agent-new codex`, then send a prompt with `key`.
  - `state` shows the tab Working, then Done, from hooks.
  - `context-state` shows a percent.
  - `context-guard 1` with a prompt: Codex writes a handoff and the tab clears and goes on.
  - Restart Tern: the tab resumes the same session (`codex resume <id>`).
  - Take a `screencapture` of the ring.

- [ ] **Step 6: Commit**
  - Message: `Codex tabs: launched with Tern's hooks, context meter from rollouts, guard and handoff, resume`

---

### Task 6: Codex usage card

**Files:**
- Modify: `crates/tern-app/src/usage.rs`:
  - `struct CodexUsage`, a `UsageSource` that reads `rate_limits` from the newest rollout under `codex_home()/sessions` with no network call;
  - `usage_source("codex")`;
  - `Report` from `primary`/`secondary`: `window_minutes` 300 → `Session`/`5h`, 10080 → `Weekly`/`W`, anything else → `<n>h`; `resets_at` in Unix seconds; `plan_type` → `plan`.
- Modify: `crates/tern-app/src/usage_view.rs` and `templates.json`:
  - the usage popover lists a card per agent with `Usage`: Claude's accounts as today, then a Codex card once it has a report;
  - the bar shows the shown tab's agent's quota when that agent has `Usage`, and Claude's account otherwise.
  - Template variables are `{agent}` and the icon slot. No new `div()` UI.
- Test: unit tests in `usage.rs` and `usage_view.rs`.

**Interfaces:**
- **Consumes:** Task 4's `rate_limits(path)` and `codex_home()`.
- **Produces:**
  - `fn codex_report(rate_limits: &Value) -> Option<Report>`.
  - `UsageStore` keeps `codex: Option<AccountUsage>`, refreshed when a Codex tab's hook comes and when the panel is drawn, at most every 60 s. It is read from disk only.

- [ ] **Step 1: Write the failing tests**
  - `codex_report_names_its_windows`: `{primary:{used_percent:12.5,window_minutes:300,resets_at:1795000565},secondary:{used_percent:40,window_minutes:10080,resets_at:…},plan_type:"plus"}` gives limits `["Session","Weekly"]`, used `[12.5,40]` and plan `Some("Plus")`.
  - `codex_report_without_limits_is_none`
  - `the_bar_follows_the_shown_agent`: a shown codex tab gives the codex summary, and a shown shell gives the Claude account.

- [ ] **Step 2: Run them and see them fail**
  - Run: `cargo test -p tern-app usage`
  - Expected: FAIL.

- [ ] **Step 3: Implement as in the Files list**

- [ ] **Step 4: Run the tests and look at it in the debug build**
  - Run: `cargo test -p tern-app`
  - Check: with a Codex tab shown, the bar shows Codex's limits; screenshot the popover.

- [ ] **Step 5: Commit**
  - Message: `Usage: Codex's limits from its rollouts, a card per agent, the bar follows the shown tab`

---

### Task 7: Recent preview and edited badges for Codex tabs

**Files:**
- Modify: `crates/tern-app/src/workspace.rs` (~2816-2860), where the sidebar preview reads `sessions::read_recent`: pass the tab's agent, so Codex uses Task 4's reader.
- Modify: `crates/tern-app/src/file_tree_view.rs`: the "edited by" badge names the tab's agent (`short_name`), not Claude.
- Test: unit tests where these pure helpers live.

- [ ] **Step 1: Write the failing tests**
  - `codex_tab_preview_reads_its_rollout`: the helper that maps (agent, session) to a preview gives "Done." for the fixture.
  - `edited_badge_names_the_agent`: a badge for a Codex edit reads `Edited by Codex`.

- [ ] **Step 2: Run them and see them fail**
  - Run: `cargo test -p tern-app preview badge`
  - Expected: FAIL.

- [ ] **Step 3: Implement.** Thread the tab's `AgentId` through; there must be no `CLAUDE` constant at the call sites.

- [ ] **Step 4: Run the tests and look at it in the debug build**
  - Run: `cargo test -p tern-app`
  - Check: the agent sidebar shows Codex's latest sentence and tool.

- [ ] **Step 5: Commit**
  - Message: `Codex tabs: sidebar preview and edited badges`

---

### Task 8: Generalise the daemon chat follower

There is no behaviour change in this task.

**Files:**
- Modify: `crates/tern-chat/src/driver.rs`: add `ChatFollow` and `chat_follower(agent)`.
- Create: `crates/tern-chat/src/claude/follow.rs`: today's `State::follow`, `State::form_line` and `Meter` logic from `crates/tern-mux/src/chat.rs:79-127`, moved.
- Modify: `crates/tern-mux/src/chat.rs`: `State` holds a `Box<dyn ChatFollow>` from `ChatSpec::agent_id()` and acts on what it says.

**Interfaces:**
- **Produces:**

```rust
/// What a chat line (read from the agent, or written to it) says, for the daemon.
pub enum Said {
    TurnStarted,
    TurnEnded,
    Session(String),
    Ask(String),
    Settled(String),
    Form(String),
    FormSettled(String),
    Reading(tern_bridge::context::Reading),
}
pub trait ChatFollow: Send {
    fn follow(&mut self, line: &Value, out: &mut Vec<Said>);
}
pub fn chat_follower(agent: &str) -> Option<Box<dyn ChatFollow>>;
```

- **Produces, the daemon side:** the daemon keeps `busy`, `asks`, `forms`, `session`, `guard` and `policy`. `Said::Form`/`FormSettled` decide which lines are recorded, as `form_line` does today.

- [ ] **Step 1: Write the test**
  - `claude_follower_says_what_state_followed`: feed it the lines the existing `tern-mux` chat tests use (user, `system/init`, `control_request can_use_tool`, `control_response`, `result`) and assert the `Said` sequence.
  - The existing `tern-mux/src/chat.rs` tests stay unchanged and must still pass.

- [ ] **Step 2: Run it and see it fail**
  - Run: `cargo test -p tern-chat follower`
  - Expected: FAIL.

- [ ] **Step 3: Move the code, then make `State` use the follower**

- [ ] **Step 4: Run the tests**
  - Run: `cargo test -p tern-chat && cargo test -p tern-mux`
  - Expected: every existing chat test passes unchanged.

- [ ] **Step 5: Commit**
  - Message: `Chats: the daemon follows a chat through its agent's follower`

---

### Task 9: Codex chat: `codex app-server` driver, follower, `NewAgentChat`, switch

**Files:**
- Create: `crates/tern-chat/src/codex/app_server.rs`: `CodexDriver` (a `ChatDriver`) and `CodexFollow` (a `ChatFollow`).
- Create: `crates/tern-chat/src/codex/fixtures/app_server.jsonl`. Record it with a variant of the spike's `resume.py` that runs one turn with an `ls`, one `apply_patch` behind an approval (`approvalPolicy:"on-request"`, `sandbox:"read-only"`), and an interrupt. Keep both directions.
- Modify: `crates/tern-chat/src/driver.rs`: `chat_driver("codex")`, `chat_follower("codex")`.
- Modify: `crates/tern-chat/src/summary.rs`: tool labels for `commandExecution`, `fileChange`, `mcpToolCall` and `webSearch`.
- Modify: `crates/tern-agent/src/registry.rs`: codex gets `Chat`.
- Modify: `crates/tern-app/src/workspace.rs`, actions, palette and menus: `NewAgentChat(id)` for every agent with `Chat`; "New Codex Chat" comes from `{agent}` in `templates.json`. `NewClaudeChat` stays as an alias.
- Modify: `crates/tern-app/src/workspace.rs` (the switch, ~596-640 and 1711-1765): Codex terminal ⇄ chat. The terminal's process is ended before the chat starts, then the chat runs `thread/resume`. Chat → terminal runs `codex resume <id>` (Task 5).
- Test: unit tests against the fixture, and a `tern-mux` chat test with a scripted fake app-server (`sh` printing the fixture's server lines).

**Interfaces:**
- **Consumes:** Task 8's `ChatFollow`/`Said`, and Task 4's `context_percent`.
- **Produces, `CodexDriver` behaviour:**
  - `caps()`: `{permission_modes: false, models: false, elicitation: true, images: false, interrupt: true}`.
  - `command(user_args, session, resume, …)` gives `codex <descriptor args> <user_args> {tern-settings} app-server`. The daemon fills `{tern-settings}` as in Task 2, so the guard's hooks reach a Codex chat too.
  - `start_lines()` gives `initialize` (id 0), `initialized`, and then either `thread/start {cwd}` (id 1) or, when resuming, `thread/resume {threadId: session}` (id 1).
  - `fold_line`:
    - the `thread/start`/`thread/resume` response sets `t.session_id`;
    - `turn/started` sets `busy`;
    - `item/agentMessage/delta` streams text, and `item/completed AgentMessage` makes it whole;
    - `item/started|completed commandExecution|fileChange|mcpToolCall` become `Entry::Tool`;
    - `*/requestApproval` becomes `Entry::Ask` with `request_id` = the JSON-RPC id, as a string;
    - `mcpServer/elicitation/request` becomes `Entry::Elicit`;
    - `thread/tokenUsage/updated` sets `t.context` via `context_percent`;
    - `turn/completed` becomes `Entry::TurnEnd`, with `interrupted` when its status is `interrupted`;
    - an error response to `thread/resume` becomes `Entry::Notice` (e.g. "This conversation is still open in a terminal").
  - `user_line(session, text, [])` gives `turn/start {threadId, input:[{type:"text",text}]}` with a fresh id.
  - `answer_line(ask, Allow|AllowSession|Deny)` gives `{"id":<ask.request_id>,"result":{"decision":"accept"|"acceptForSession"|"decline"}}`.
  - `interrupt_line()` gives `turn/interrupt {threadId, turnId}`, using the last `turn/started` turn id.
  - `load(t, path, …)`: replay a rollout through Task 4's `item_completed` reader into entries.
- **Produces, `CodexFollow`:** the response `result.thread.id` gives `Session`; `turn/started` gives `TurnStarted`; `turn/completed` gives `TurnEnded`; a request with `requestApproval` gives `Ask(id)`; `serverRequest/resolved`, or a client response with that id, gives `Settled(id)`; `thread/tokenUsage/updated` gives `Reading`.

- [ ] **Step 1: Record the fixture**
  - Script: `/private/tmp/claude-501/-Users-cuongpham-ws-tern-ws-tern/5f2e9412-232e-40cd-a0a5-dded8c428e40/scratchpad/spike/resume.py`, extended.
  - Rewrite the paths to `/w`.

- [ ] **Step 2: Write the failing tests**
  - `a_new_codex_chat_learns_its_thread`
  - `codex_text_streams_then_settles`
  - `codex_command_is_a_tool_with_its_output`
  - `codex_approval_is_an_ask_and_answers_by_id`: `answer_line(Allow)` equals the recorded client reply.
  - `codex_interrupt_names_the_running_turn`
  - `codex_context_reading_from_token_usage`
  - `resume_refused_is_a_notice`: an error `-32600 … active writer` gives a `Notice`, with `busy` false.
  - `codex_follower_says_turns_asks_and_session`
  - A `tern-mux` test, `codex_chat_runs_against_a_scripted_server`: the guard's message goes in as `turn/start`.
  - A `tern-app` test: `new_agent_chat_lists_agents_with_chat` gives `["claude","codex"]`.

- [ ] **Step 3: Run them and see them fail**
  - Run: `cargo test -p tern-chat codex && cargo test -p tern-mux codex_chat`
  - Expected: FAIL.

- [ ] **Step 4: Implement as in the Interfaces block**

- [ ] **Step 5: Run all tests**
  - Run: `cargo test --workspace`
  - Expected: PASS.

- [ ] **Step 6: Check by hand in the debug build**
  - `chat-new` for codex: send `ls` and the chat shows the tool, then the answer.
  - Approve an edit with `chat-answer allow`.
  - Interrupt a turn.
  - Switch to terminal and back: the conversation goes on.
  - Take screenshots.

- [ ] **Step 7: Commit**
  - Message: `Codex chat: app-server driver, follower, New Codex Chat, terminal ⇄ chat switch`

---

### Task 10: Docs, spec, and the final checks

**Files:**
- Modify: `docs/superpowers/specs/2026-11-05-agent-adapters-design.md`:
  - In §2 `Launch` and "Calls made", say Codex uses `-c` hooks and trust, not an overlay home, and why (sessions stay in `~/.codex`; there is no config mirroring).
  - Mark the Codex open items as settled, with the facts from this plan.
- Modify: Tern's config README (`~/.config/tern/README.md` source in the repo, wherever phase 1 added the agents section): Codex's integration, the 0.166 minimum, and that hooks come in with `-c` (nothing is written to `~/.codex`).
- Modify: `docs/superpowers/handoffs/2026-11-05-agent-adapters.md`: rewrite it for phase 3 (OpenCode), with what phase 2 left (the `ClaudeTracker` rename, Windows runtime unverified, `.jsonl.zst`).

- [ ] **Step 1: Write the docs**

- [ ] **Step 2: Run CI's checks and Windows clippy**
  - `cargo fmt --all --check`
  - `cargo clippy --locked --workspace --all-targets -- -D warnings`
  - `cargo test --locked --workspace --no-fail-fast`
  - `cargo xwin clippy --locked --workspace --all-targets --target x86_64-pc-windows-msvc -- -D warnings`
  - Expected: all clean.

- [ ] **Step 3: Commit**
  - Message: `Agents phase 2: docs, spec settled for Codex, handoff for OpenCode`
