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

## Basic-tier screen rules, as tested

Run live in the dev app on 2026-11-05: Pi 1.5.0 (Working) and Gemini CLI 0.67.0 (Working, the tool approval and the folder trust question as Blocked), both against local mocks. Grok 1.0.70 and Cursor Agent 2026.11.05 reached only their login screens: their Working and approval rules come from herdr's manifests and have not met a real turn. Scratch setup: `PI_CODING_AGENT_DIR` with `models.json` for Pi; `GEMINI_CLI_HOME`, `GEMINI_API_KEY`, `GOOGLE_GEMINI_BASE_URL` for Gemini, whose mock must also answer the router's `generateContent` with JSON.

## Left from phase 3

- `synthetic` with `resume: false` during a turn wasn't measured, only between turns.
- Forms (`form.*`) never ran live: their shapes come from the OpenCode binary and Orca's code, and `form.created`'s session is in `data.form`. A real child session did run (the mock's `SUB:`).
- An OpenCode chat has no model switch, images or MCP forms.
- A fresh OpenCode tab reads Working for its first seconds, then Done, from output activity alone.
- A rejected permission ends the turn in a chat as OpenCode does; the row shows `Ended`.
- A tool that was mid-turn when a chat ended keeps no result.
- A chat with no hooks is asked for a handoff at its turn's end only: no warning mid-turn. Its guard goes by `ChatFollow::hookless`, and a fresh conversation gets the context and the go-on prompt as one message.
- In a folder `folder_trust` doesn't know, a non-Claude chat starts only when asked for something: a sent message, Hand Off, Continue (`may_start_asked`), not on restore, focus or typing. So its commands and models show after the first message there. Neither Orca nor herdr has a question of its own for this (Orca writes each agent's trust record for it, which Tern won't do).
- A daemon handoff with a chat (`MuxChatEvent::Lost`) is covered by `parks_and_resumes_without_losing_a_line` and `kept_lines_of_a_handed_over_chat_ask_nothing_again`, not run live with OpenCode.
- Open minors from the final review (`opencode_fold.rs`, `opencode_plugin.js`, `acp.rs`, `chat.rs`):
  - The lane is wiped by the first event after a daemon handoff: seed the fold's `root`.
  - An adopted chat loses what only written lines told (`busy`, an answered ask).
  - Compaction leaves a stale warning and ring.
  - `OPENCODE_CONFIG_CONTENT` set in a shell rc file overrides Tern's, and an empty one counts as invalid.
  - A nested `opencode` reports into the tab.
  - A lost steer leaves the tab Working.
  - `write_if_changed`'s temp name is per process, not per call.
  - Preview file I/O runs under the state lock.
  - A refused open has no way out: offer a fresh conversation after "not found".
  - Any error ends a turn, whoever it answers.
  - `reset` keeps `session`, so a replay can raise a false `t.handoff`.
  - With two asks open, the first answer reads Working.

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

For OpenCode: `mock.py` from the spike folder as a Tern service, and `<scratch>/xdg/opencode/opencode.json` naming it as the provider. Run the mock's service commands with your real `HOME`: a long one breaks Tern's socket path. The mock takes `TOOL: <command>` (a shell call), `SUB:` (a real child session, slow), `SLOW` (6 s), and `FULL` (60% of a 100k window); it answers a handoff request with a `Handoff:` line.

The app window says "Background: an older build" when the daemon predates the build: `restart-background` first, or the daemon's side of a change isn't what runs.

`TERN_CONTROL` without `TERN_SESSION` is scripted: it neither saves, restores nor adopts, so a reopen shows no tabs. Set `TERN_SESSION=1` to test those. A dev app's `--server` outlives `pkill` of the app: `pkill -f 'MacOS/tern <folder>'` quits just the app.

For a scratch Codex: `<home>/auth.json` a symlink to `~/.codex/auth.json`; `<home>/config.toml` with `[projects."<folder>"] trust_level = "trusted"`, for both the `/tmp` and `/private/tmp` spellings. Add `approval_policy = "on-request"` and `sandbox_mode = "read-only"` to see approvals.

Screenshots: a CGWindowList Swift helper finds the "Tern Dev" window, `screencapture -x -o -l <id>`. Never capture the whole screen. Shut down with `restart-background`, then `pkill -f "Tern Dev.app/Contents/MacOS/tern"`.

Use `/usr/bin/grep`: the shell's `grep` is ugrep and `\|` patterns silently find nothing.
