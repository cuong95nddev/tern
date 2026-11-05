# Handoff: agent adapters, phase 3 (OpenCode) — written 2026-11-05

## Task

Make OpenCode a first-class Tern agent, as phase 2 did for Codex. Reply to the user in Vietnamese; code and comments in English. The user wants it done properly ("làm chuẩn chỉ").

Start with a spike that settles OpenCode's open items in the spec, then a plan (`superpowers:writing-plans`) the user approves.

Read first:
- Spec: `docs/superpowers/specs/2026-11-05-agent-adapters-design.md` (Open items, phase 3).
- Phase 2's plan, as the model: `docs/superpowers/plans/2026-11-05-agent-adapters-phase2.md`.

## State

Phase 2 is on branch `agent-adapters-codex`, off `main` 4be3336. See `git log` and the user's word for whether it is merged. `main` itself was not pushed since phase 1.

What a new agent plugs into:
- `tern-agent`: registry capabilities, `agents/<id>.json`.
- `tern-bridge`: hook wiring per agent (`codex_hooks.rs` is the model), `relay.rs`.
- `tern-mux`: `Server::hook_args`, `guard_for`, `agent_probe.rs`, `client::for_daemon`, `strip_chat_hooks`.
- `tern-term`: a lane in `AgentLane` (`codex_status.rs`).
- `tern-chat`: `sessions.rs` table, `chat_driver`, `chat_follower` (`ChatDriver`, `ChatFollow`, `Said`). `codex/app_server.rs` is the model of a JSON-RPC driver.
- `tern-app`: `start_line`, `spawn_config`, usage (`CodexUsage`, `UsageStore::agent_usage`), `chat_agents` feeds the menus.

## Left from phase 2

- `ClaudeTracker` isn't renamed yet, though `CodexTracker` wraps it.
- Windows at runtime is unverified: the hook command is double-quoted for `cmd`, and the trust key `/<session-flags>/…` hasn't been seen there.
- A `.jsonl.zst` rollout gives no preview, no reading and no history.
- Window restore after a clean quit with a Codex tab wasn't exercised (`pkill` doesn't save the window).
- A Codex chat refuses requests it has no look for (`item/permissions/requestApproval`, `item/tool/requestUserInput`, `item/tool/call`) with -32601.
- A reloaded Codex chat doesn't show past MCP forms, and an interrupted command shows after its turn's end.
- Any error response ends the turn in a Codex chat's UI and daemon: a refused `turn/steer` shows idle until `turn/completed`.
- `read_context` may follow a sub-agent's `transcript_path`, so the ring could flip mid-turn (unverified).
- `set_tab_agent` is deferred on every render while an agent without `Usage` is shown.
- Claude's usage card has no Codex row; Codex's numbers show when a Codex tab or chat is shown. The user was told.

## Decisions to keep

- Never edit an agent's global config: everything goes in what a launch is given.
- **Never answer Codex's "Trust this folder" in the real home while testing**: that writes `~/.codex/config.toml`.
- Born-over: a conversation whose first reading is past the limit never hands off (by design). Test the guard on a resumed conversation.
- JSON-RPC ids overlap both ways: a follower must tell lines read from lines written (`ChatFollow::wrote`).

## Manual testing

`cargo build -p tern-app`, then `CODEX_HOME=<home> XDG_CONFIG_HOME=<scratch>/xdg scripts/dev-app-macos.sh <scratch>/work`, drive with `printf 'cmd\n' | nc -U -w 3 target/dev.sock`: `state`, `agent-new codex`, `agent-switch`, `context-state`, `context-guard N`, `key <k>` (one key at a time, `space`, `enter`, `escape`), `chat-new [agent]`, `chat-send …`, `chat-state`, `chat-answer allow`, `click X Y`, `move X Y`, `restart-background`.

The app window says "Background: an older build" when the daemon predates the build: `restart-background` first, or the daemon's side of a change isn't what runs.

For a scratch Codex: `<home>/auth.json` a symlink to `~/.codex/auth.json`; `<home>/config.toml` with `[projects."<folder>"] trust_level = "trusted"`, for both the `/tmp` and `/private/tmp` spellings. Add `approval_policy = "on-request"` and `sandbox_mode = "read-only"` to see approvals.

Screenshots: a CGWindowList Swift helper finds the "Tern Dev" window, `screencapture -x -o -l <id>`. Never capture the whole screen. Shut down with `restart-background`, then `pkill -f "Tern Dev.app/Contents/MacOS/tern"`.

Use `/usr/bin/grep`: the shell's `grep` is ugrep and `\|` patterns silently find nothing.
