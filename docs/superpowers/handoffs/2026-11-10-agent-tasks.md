# Handoff: agent tasks (`tern --task`) — 2026-11-10

One agent gives another a task in a new Tern tab and gets its answer back
(Claude → Codex, etc.). Uncommitted on `main`; not installed (Tern.app runs an
earlier build of it without wake/answer). The user's own pre-existing edits
(`claude.md`, `settings.rs` parts, `style.rs`, `vendor/*`) are also uncommitted:
don't mix them into this feature's commit without asking.

## Status (2026-11-10, end of day)

Reviewed, fixed, installed and tested live: wake of a giver started without
`--wait`, Stop-hook answer, approvals refused, `--task answer` (`--option N` and
text) for Claude's AskUserQuestion and Codex's `request_user_input_async`.
Review fixes: only the giver's read counts as heard; `send` refuses while a
dialog shows; news looked for only on tabs with a giver; only the giver closes;
no repeat wake right after `answer`. Live-test finds: the daemon never forwarded
`PreToolUse`/`PermissionRequest` to the UI (`asks_user` in `tern-mux/src/server.rs`
now does, live only); newer Claude sends `PermissionRequest` for AskUserQuestion
(kept as a question); Codex 0.167 in Default mode asks by `request_user_input_async`
(returns at once, turn sleeps: blocked is read off the screen, answer opens it
with Shift+Left first).

## Design (follows refs, user insisted: "coi thật kỹ herdr orca, đừng tự chế")

- CLI `tern --task start|wait|read|send|answer|ls|close` (`agent_cli.rs`) → `AskUi`
  ops `task_*` → `agent_tasks.rs`. Guide text in `agent_cli::guide_for` reaches
  agents through the SessionStart hook.
- Task = new agent tab behind the shown one (`Workspace::start_task`); id = the
  terminal's entity id. Prompt pasted like a user (`prompt_input.rs`, moved out of
  `shot/targets.rs`) once the agent takes input; `wait_ready` refuses to paste
  while `TerminalView::screen_asks()` (a startup dialog: Codex update/trust, Claude
  trust). A pasted Enter once chose Codex "Update now" and Claude "No, exit".
- Answer = Stop hook `last_assistant_message` (as t3code); agents without hooks:
  screen. State in `Track` (agent_tasks.rs); "starting" reads the screen live.
- Wake = Orca mailbox pointer: `tell_givers` every 1s types
  `[Tern] Task N (Agent) is done… Run '…tern' --task read N`, Enter 500 ms later;
  held while the giver shows a dialog or has a draft (`composer_draft.rs`, port of
  Orca's detectTerminalComposerDraft, simplified) typed within 10 min; never into a
  shell; not while the giver polls (`read_at` 3 s); done only after 1.5 s quiet.
- Answering: questions (option or text) and, since the user asked for it, approvals
  (`--option N` only), only by the giving tab. `by` is vouched for by the daemon
  (`stamp_caller` in tern-mux server.rs: the socket peer's pid, walked up to a
  session's program), not taken from TERN_SESSION_ID. Startup and unknown dialogs
  stay the user's.
- Sandbox: Codex's sandbox blocks the socket → error says "run again with
  escalated permissions" (Orca's guide).
- Daemon fix `tern-mux/src/host.rs`: after PTY EOF, `poll_exit` (waitpid) and
  stop watching readable — was 100% CPU + zombie when a SIGCHLD byte was missed.
- `codex.json` blocked rule `enter continue · esc` (startup screens).

## Known gaps to check in review

- Task ↔ giver links live only in app memory: lost on Tern restart.
- `task_news` runs each second over all tabs (screen reads): cost with many tabs.
- `composer_draft` lacks Orca's hidden-cursor / Codex footer checks.
- Codex 0.167 sends no SessionStart until the first turn; ignores
  `-c projects.<p>.trust_level`.
- Claude interrupted by Esc sends no Stop: a hooked task stays "working".

## Files

crates/tern-app/src/{agent_tasks.rs (new), prompt_input.rs (new), composer_draft.rs (new),
agent_cli.rs, agent_ui.rs, workspace.rs, terminal_view.rs, main.rs, shot/targets.rs},
crates/tern-agent/{agents/codex.json, src/screen.rs}, crates/tern-mux/src/host.rs.
Refs studied: refs/orca/skill-guides/orchestration.md, refs/orca/src/main/runtime/orchestration/*,
refs/herdr/skills/herdr/SKILL.md, t3code clone in the old scratchpad.
