# Handoff: agent adapters, phase 2 (Codex), tasks 7-10 — written 2026-11-05

## Task

Make Codex a first-class Tern agent. Reply to the user in Vietnamese; code and comments in English. The user wants it done properly ("làm chuẩn chỉ") and chose native, inline execution: carry on with `superpowers:executing-plans`, one fresh whole-branch review at the end, then `superpowers:finishing-a-development-branch`.

Read first:
- Plan: `docs/superpowers/plans/2026-11-05-agent-adapters-phase2.md`. Its "Facts" section is what the spike verified on Codex 0.166.0; the user approved the plan, including dropping the overlay `CODEX_HOME`.
- Spec: `docs/superpowers/specs/2026-11-05-agent-adapters-design.md` (not yet updated, that is Task 10).
- Ledger: `.superpowers/sdd/2026-11-05-agent-adapters-phase2/progress.md` (untracked, do not commit). It has every `Ruling:` line and `Task N: complete`; the final message must list them all. Briefs are made by `scripts/task-start`.

## State

Branch `agent-adapters-codex`, off `main` 4be3336. Not merged, not pushed. HEAD is this handoff's commit. `main` itself is still not pushed since phase 1.

Tasks 1-6 are complete (run order 1, 3, 2, 4, 5, 6). Package tests pass and macOS clippy is clean at 18e15f3. Not run yet: the whole-workspace test command and Windows clippy (`cargo xwin`).

Done:
- `tern-bridge/src/codex_hooks.rs`: hooks + trust through `--no-daemon -c hooks={…}`, hook file `codex-hooks-<hash of token>.json`, `codex_wired` (≥ 0.166.0). `relay.rs` answers per event. `version.rs`. `HookEvent::parse` takes `apply_patch` paths.
- `tern-mux`: `Spawn.agent` (MINOR 18, `AGENT_SPAWN_MINOR`), `agent_probe.rs` (was `claude_probe`), `Server::hook_settings`/`hook_args`, `Session.agent`, `Host::set_agent`, `client::for_daemon` (old daemon: no hooks), `Server::read_context` (rollout reading comes in as a synthesized `StatusLine`), `guard_for`, handoff carries `codex` and `agent`.
- `tern-term/src/codex_status.rs`: `CodexTracker` wraps `ClaudeTracker`, `Interrupt` → `interrupted`. `AgentLane::Codex`.
- `tern-chat/src/codex/rollout.rs`: `codex_home`, `rollout_path`, `read_recent`, `last_text`, `context_reading`, `context_percent`, `rate_limits`, `latest_rate_limits`, `entries` (pub(crate), for Task 9's `load`). `sessions.rs` table, incl. `context_reading`.
- `tern-bridge/src/context.rs`: `Guard::starting_at_first_turn`, `Action::ClearPrompt`.
- `tern-app`: `start_line` per agent, `spawn_config` for agents with `Launch`, `TerminalView::set_hook_agent`, `CodexUsage`, `codex_report`, `UsageStore::agent_usage`, `UsageView::set_tab_agent`, `own_usage`, `UsageCard::render_own`, `has_accounts` in `templates.json`.
- Registry: codex has `Launch, Status, Session, Transcript, Context, Steer, Usage, Skills`. `Chat` comes with Task 9.

Checked live on Codex 0.166 (debug build, scratch `CODEX_HOME`): hook status, Esc ends the turn, ring, sidebar preview, resume after `restart-background`, full auto handoff (HANDOFF.md → `/clear` → continue prompt → reads it).

Not checked:
- Task 6 in the debug build: the bar and card with a Codex tab shown. Do this first.
- Window restore after a clean quit (`pkill` doesn't save the window).
- Windows at runtime: hook command is double-quoted for `cmd`, trust key `/<session-flags>/…` unverified there.

## Left

- **Task 7.** The preview already works through `sessions::read_recent`. Left: `file_tree_view.rs` badge says "Claude"; name the tab's agent. Read the brief.
- **Task 8.** `ChatFollow`/`Said` in `tern-chat`, Claude's moved from `tern-mux/src/chat.rs` `State::follow`/`form_line`. No behaviour change.
- **Task 9.** `codex app-server` driver + follower, `NewAgentChat`, switch. Extra decisions not in the plan:
  - The guard in a Codex chat: there is no `/clear` there, and `ChatHost::tell` maps `ClearPrompt` to the prompt only, as a user message. Chat state must use `Guard::starting_at_first_turn` for Codex, and going on in a fresh conversation needs a new `thread/start`, or auto switch off for Codex chats. Rule on it and ledger it.
  - `--no-daemon` and `-c` both work before `app-server`; `{tern-settings}` fills them.
  - The fixture needs recording: JSON-RPC over stdio, `initialize`, `initialized`, `thread/start`, `turn/start`; approvals with `approvalPolicy: "on-request"`.
- **Task 10.** Spec (`-c`, not overlay; open items settled; SessionStart comes at the first turn), config README, rewrite this handoff for phase 3 (OpenCode), CI checks + `cargo xwin clippy`.
- Then the whole-branch review (most capable model, plan's Review Focus verbatim, ledger rulings), one fix pass, finishing skill. Merging or pushing needs the user's word.

## Decisions to keep

- No overlay home: Codex sessions stay in `~/.codex/sessions`.
- Codex hooks never touch `~/.codex`. **Never answer Codex's "Trust this folder" in the real home while testing**: that writes `~/.codex/config.toml`.
- `ClaudeTracker` isn't renamed yet; note it for later.
- Claude's card has no Codex row; Codex's numbers show when a Codex tab is shown. The user was told.
- Born-over: a conversation whose first reading is past the limit never hands off (by design). Test the guard on a resumed conversation.

## Manual testing

Handoff of phase 1 applies: `cargo build -p tern-app`, then `XDG_CONFIG_HOME=<scratch>/xdg scripts/dev-app-macos.sh <scratch>/work`, drive with `printf 'cmd\n' | nc -U -w 3 target/dev.sock`: `state`, `agent-new codex`, `context-state`, `context-guard N`, `key <k>` (one key at a time, `space`, `enter`, `escape`), `chat-new`, `chat-send …`, `chat-answer allow`, `restart-background`.

For a scratch Codex:
- `<scratch>/xdg/tern/agents/codex.json` = `{"id":"codex","command":"env CODEX_HOME=<home> codex"}`;
- `<home>/auth.json` a symlink to `~/.codex/auth.json`;
- `<home>/config.toml` with `[projects."<folder>"] trust_level = "trusted"`, for both the `/tmp` and `/private/tmp` spellings.

Screenshots: a CGWindowList Swift helper finds the "Tern Dev" window, `screencapture -x -o -l <id>`. Never capture the whole screen. Shut down with `restart-background`, then `pkill -f "Tern Dev.app/Contents/MacOS/tern"`.

Codex's source at `rust-v0.166.0` answers protocol questions: shallow-clone `openai/codex` into a scratch folder (`codex-rs/app-server-protocol/src/protocol/v2/`, `codex-rs/hooks/`). Orca: `../refs/orca/src/main/codex/`. Use `/usr/bin/grep`: the shell's `grep` is ugrep and `\|` patterns silently find nothing.
