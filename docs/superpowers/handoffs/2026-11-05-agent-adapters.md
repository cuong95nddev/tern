# Handoff: agent adapters, phase 2 (Codex) — written 2026-11-05

## Where things stand

Phase 1 is done and merged into `main` locally (fast-forward, HEAD `846cfed`), **not pushed**. The branch `agent-adapters` can be deleted.

Each Tern agent feature now goes through a registry of agents plus per-capability tables, with Claude filling every table:
- Tern-wide checks pass: tests 754/754, clippy on macOS and on Windows (`cargo xwin`), and fmt.
- Phase 1 had a fresh whole-branch review. Its 4 Important and 4 Minor findings are fixed.

Reply to the user in Vietnamese; write code and comments in English. The user wants this done properly ("làm chuẩn chỉ") and wants native, inline execution: `superpowers:writing-plans`, then `superpowers:executing-plans`.

Read first:
- The spec, `docs/superpowers/specs/2026-11-05-agent-adapters-design.md`, is the binding authority. Read §2 Capabilities, §4 Chat model, §6 Rollout order (phase 2 = Codex), "Calls made in this design" and "Open items for the builder to verify".
- The phase-1 plan, `docs/superpowers/plans/2026-11-05-agent-adapters-phase1.md`, shows the plan format the user accepted.

## Where phase 2 plugs in (all phase-1 code)

| Concern | Where | Today |
|---|---|---|
| Descriptor and capabilities | `crates/tern-agent/agents/*.json`, `tern-agent/src/registry.rs::builtin_capabilities` | Only Claude has capabilities. Codex: detect + screen |
| Hook lane | `tern_term::agent_lane::AgentLane::{Claude}`; host `AgentHooks` in tern-mux | A lane is created as Claude on the first hook. The agent should instead be chosen at spawn: add an `agent` field to `Spawn` (protocol MINOR bump) |
| Status | `tern_term::agent_status::AgentStatus` | Neutral |
| Chat driver | `tern-chat/src/driver.rs` (`ChatDriver`, `ChatCaps`, `chat_driver()` table); Claude's in `tern-chat/src/claude/` | Codex → `None` |
| Daemon chat follower | `tern-mux/src/chat.rs` `State::follow` (busy/asks/forms/meter/guard) | **Still Claude stream-json.** It must be generalised before a second driver; its guard message already goes through `chat_driver(agent).user_line` |
| Sessions / transcript | `tern-chat/src/sessions.rs` (`transcript_path`, `read_recent`, `last_text`) | Codex → none |
| Usage | `tern-app/src/usage.rs` (`UsageSource`, `usage_source()`, `ACCOUNT_AGENT`) | Cards are per Claude account |
| Resume | `agents::resume_line` (descriptor `resume`) + `spawn_config` | Only for agents with `Session` |
| Ring / guard gating | `agents::guard_shown` (Context + Steer + Transcript); `ContextShown.guards`; template var `can_guard` | — |
| Launch | `workspace::spawn_config`: Claude via `agents::start_line`; others run as the tab's program (the tab goes back to a shell on any exit, see `goes_on_as_shell`) | Codex with `Launch` needs its overlay home + hooks |
| Actions | `NewAgentTab(id)`, `SwitchAgentView` (alias `SwitchClaudeView`), `NewClaudeTab` = ⌘T | No `NewAgentChat` yet: add it with the Codex driver |

## Phase 2 scope (spec §6 item 2)

The work, in this order:
1. **Codex launch overlay + hooks.** Use a Tern-owned overlay `CODEX_HOME` per agent; never edit the user's `~/.codex`. Trust its hooks there (Orca `src/main/codex/trust/`).
2. Codex's **hook lane**.
3. **Sessions and rollouts:** `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-*-<id>.jsonl`.
4. **Context meter:** `event_msg/token_count` → `last_token_usage`, `model_context_window`.
5. **Usage** from the rollout's `rate_limits`. No network call.
6. **Steer:** a hook answer if Codex hooks can inject or block, else an idle bracketed paste.
7. **Resume** and the **recent preview**.
8. **Chat driver** (`codex app-server`, JSON-RPC) + `NewAgentChat`.

Generalise the daemon chat follower before step 8.

Verify these open items before planning; they decide the design:
- Can Codex hook outputs inject context or block `Stop`?
- Can `codex app-server` resume a session the TUI started?
- How does Codex hook trust work?

Codex 0.166.0 is installed at `~/.local/bin/codex`. Prior art: Orca (`codex-hook-definition.ts`, `providers/codex-state.ts`), herdr.

## Deferred / known gaps from phase 1

- Usage cards stay per Claude account; per-agent cards come with Codex's usage source.
- `new_agent_tab` returns the previously shown tab while Claude's account picker is open, so `agent-new claude` answers "ok" then.
- Never checked by hand:
  - restoring a window saved by the pre-branch `main` (unit tests only);
  - a guard auto-handoff on crossing the limit;
  - a subagent keeping a Claude tab working.

  On the guard: a conversation whose first reading is already over the limit is `born_over`, and the status line after `/clear` still reports the old reading. That is existing behaviour.
- Pre-existing: a Claude chat's "Trust This Folder" doesn't stop a terminal Claude in the same folder from asking again.

## Manual testing (debug build)

Build and launch:
- `cargo build -p tern-app`.
- Launch with `XDG_CONFIG_HOME=<scratch>/xdg scripts/dev-app-macos.sh <scratch>/work`. The daemon starts lazily, so the title says "Background: unavailable" until a tab opens.

Drive it with `printf 'cmd\n' | nc -U -w 3 target/dev.sock`. Useful commands:
- `state`, `chat-new`, `chat-trust`, `chat-send …`, `chat-state`, `chat-mode`, `chat-answer allow`;
- `agent-new <id>`, `agent-switch`;
- `context-state`, `context-guard N [manual]`, `context-hand-off`, `context-resume`;
- `key <keystroke>` (one key at a time: use `/`, `.`, `space`), `click X Y` (window points).

`agent-new claude` opens the account picker when several accounts are on; answer it with `key 1`.

Screenshots:
- Find the 1920-wide "Tern Dev" window with a CGWindowList Swift helper (`winid`).
- Capture with `screencapture -x -o -l <id>`.
- Never capture the whole screen.

Don't set `TERN_SESSION`: the debug build shares `~/Library/Application Support/dev.tern` with the installed Tern.

Shut down: `restart-background`, then `pkill -f "Tern Dev.app/Contents/MacOS/tern"`, then `kill -9` any leftover. Kill the `opencode serve --service` and Codex processes your own tests started.
