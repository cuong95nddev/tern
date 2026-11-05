# Handoff: agent adapters, phase 4 (basic-tier polish) — written 2026-11-05

## Task

Phase 3 made OpenCode a first-class Tern agent. Phase 4 per the spec: Gemini, Cursor, Grok and Pi descriptors, and user `agents/*.json` documented in Tern's config README. Reply to the user in Vietnamese; code and comments in English. The user wants it done properly ("làm chuẩn chỉ").

Read first:
- Spec: `docs/superpowers/specs/2026-11-05-agent-adapters-design.md`.
- Phase 3's plan and spike tools: `docs/superpowers/plans/2026-11-05-agent-adapters-phase3.md`, `docs/superpowers/spikes/2026-11-05-opencode/`.

## State

Phase 3 is on branch `agent-adapters-opencode`, off `main` c14b2e1. See `git log` and the user's word for whether it is merged. `main` was not pushed since phase 1.

What a new agent plugs into:
- `tern-agent`: registry capabilities, `agents/<id>.json`.
- `tern-bridge`: hook wiring per agent (`codex_hooks.rs`, `opencode_hooks.rs`), `opencode_fold.rs` for an agent whose events aren't Claude-shaped.
- `tern-mux`: `Server::hook_args` (`HookLaunch`: arguments and environment), `guard_for`, `fold_for`, `clear_command`, `agent_probe.rs`, `client::for_daemon`, `strip_chat_hooks`.
- `tern-term`: a lane in `AgentLane`.
- `tern-chat`: `sessions.rs` table, `chat_driver`, `chat_follower`. `codex/app_server.rs` and `opencode/acp.rs` are the models; `acp.rs` is the one to reuse for any ACP agent.
- `tern-app`: `start_line`, `spawn_config`, usage (`UsageStore::agent_usage`), `chat_agents` feeds the menus.

## Left from phase 3

- Window restore after a clean quit with an OpenCode tab came back with no tabs when the app was reopened with a folder argument; not looked into, and Codex's was never exercised either. `restart-background` does resume with `--session`.
- A chat's Stop button wasn't pressed live (the dev `key escape` isn't handled by the chat): `session/cancel` was measured with `acp.py`, and `interrupt_line` is unit tested.
- `synthetic` with `resume: false` during a turn wasn't measured, only between turns.
- Sub-agents (`parent`) and forms (`form.*`) come from Orca's code and unit tests: no live run had a child session or a form.
- An OpenCode chat has no model switch, images or MCP forms.
- A fresh OpenCode tab reads Working for its first seconds, then Done, from output activity alone.
- A rejected permission ends the turn in a chat as OpenCode does; the row shows `Ended`.
- A tool that was mid-turn when a chat ended keeps no result.

## Left from phase 2

- `ClaudeTracker` isn't renamed yet, though `CodexTracker` and `OpenCodeTracker` wrap it.
- Windows at runtime is unverified: the hook command is double-quoted for `cmd`, and the trust key `/<session-flags>/…` hasn't been seen there. OpenCode's plugin path on Windows is unverified too.
- A `.jsonl.zst` rollout gives no preview, no reading and no history.
- A Codex chat refuses requests it has no look for (`item/permissions/requestApproval`, `item/tool/requestUserInput`, `item/tool/call`) with -32601.
- A Codex approval doesn't show which thread it came from, and one from a thread a handoff left behind is still asked.
- A reloaded Codex chat doesn't show past MCP forms, and an interrupted command shows after its turn's end.
- Any error response ends the turn in a Codex chat's UI and daemon: a refused `turn/steer` shows idle until `turn/completed`.
- `read_context` may follow a sub-agent's `transcript_path`, so the ring could flip mid-turn (unverified).
- `set_tab_agent` is deferred on every render while an agent without `Usage` is shown.
- Claude's usage card has no Codex row; Codex's numbers show when a Codex tab or chat is shown. The user was told.

## Decisions to keep

- Never edit an agent's global config: everything goes in what a launch is given.
- **Never answer Codex's "Trust this folder" in the real home while testing**: that writes `~/.codex/config.toml`.
- **Plain `opencode` commands in the real home start its background service**: stop it afterwards (`opencode service stop`).
- Born-over: a conversation whose first reading is past the limit never hands off (by design). Test the guard on a resumed conversation.
- JSON-RPC ids overlap both ways: a follower must tell lines read from lines written (`ChatFollow::wrote`).

## Manual testing

`cargo build -p tern-app`, then `XDG_CONFIG_HOME=<scratch>/xdg XDG_DATA_HOME=… XDG_STATE_HOME=… XDG_CACHE_HOME=… scripts/dev-app-macos.sh <scratch>/work` (`CODEX_HOME=<home>` for Codex), drive with `printf 'cmd\n' | nc -U -w 3 target/dev.sock`: `state`, `agent-new <id>`, `agent-switch`, `context-state`, `context-guard N`, `term-type text\n`, `term-screen`, `term-info`, `key <k>`, `chat-new [agent]`, `chat-send …`, `chat-state`, `chat-answer allow`, `click X Y`, `move X Y`, `restart-background`.

For OpenCode: `mock.py` from the spike folder as a Tern service, and `<scratch>/xdg/opencode/opencode.json` naming it as the provider. Run the mock's service commands with your real `HOME`: a long one breaks Tern's socket path.

The app window says "Background: an older build" when the daemon predates the build: `restart-background` first, or the daemon's side of a change isn't what runs.

For a scratch Codex: `<home>/auth.json` a symlink to `~/.codex/auth.json`; `<home>/config.toml` with `[projects."<folder>"] trust_level = "trusted"`, for both the `/tmp` and `/private/tmp` spellings. Add `approval_policy = "on-request"` and `sandbox_mode = "read-only"` to see approvals.

Screenshots: a CGWindowList Swift helper finds the "Tern Dev" window, `screencapture -x -o -l <id>`. Never capture the whole screen. Shut down with `restart-background`, then `pkill -f "Tern Dev.app/Contents/MacOS/tern"`.

Use `/usr/bin/grep`: the shell's `grep` is ugrep and `\|` patterns silently find nothing.
