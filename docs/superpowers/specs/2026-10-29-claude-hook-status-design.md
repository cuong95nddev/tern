# Claude tab status from hooks (Orca parity)

## Why

A Claude tab's status came from Claude Code's OSC 7501 reports and its
session file (`sessions/<pid>.json`). Neither knows what Claude still owes
the conversation: a background shell, Monitor or sub-agent that outlives the
main agent's turn ends with a `<task-notification>` that wakes the main agent
again, so "the turn ended" is not "finished". The session file's `shell`
status (idle with a background shell) read as done, so Tern announced
"finished" and Claude carried on a minute later.

Orca reads Claude's hooks instead. This ports its Claude hook lane
(`refs/orca/src/shared/agent-hook-listener/providers/claude-*.ts`,
`claude-owed-task-notifications.ts`, `claude-subagent-roster.ts`,
`claude-background-task-inventory.ts`, `agent-lead-status-fold.ts`) whole,
not only the background-task part.

## Goals

- The status of a Claude tab Tern launched (one with a hook token) comes from
  its hooks: working, waiting for you (permission, AskUserQuestion, a child's
  permission), done, and **monitoring**: the main agent's turn is over but a
  background shell, Monitor or session cron still runs.
- A pane stays working while a sub-agent works, and while a task that ended
  still owes the main agent its notification, for at most a 60 s lease after
  the main agent went idle (Orca's `CLAUDE_OWED_TASK_NOTIFICATION_LEASE_MS`).
- The turn a notification opens is a wake-up turn, not a prompt you typed.
- "Finished" is announced once per turn: at the turn's end when running child
  work holds the pane (Orca's `turnCompletedAt`), not again when that work
  drains; not at a turn end the main agent is ~100 ms from resuming (only an
  owed notification holds it).
- A cancel (Esc, Ctrl+C: Claude sends no hook) and an answered
  AskUserQuestion clear the main agent's state.
- Sub-agent / teammate roster as Orca keeps it (one-shot vs teammate ids,
  TeammateIdle parking, inventory folds), `/compact`, `StopFailure`,
  `SessionStart` and replaced-session voiding.
- The status survives a UI restart (the daemon holds it) and a daemon handoff
  (it goes along with the session).

## Non-goals

- Orca's restore-from-disk machinery (`restoredFromSnapshot`, persisted rows):
  Tern's daemon outlives the UI and hands its state on, so nothing is restored.
- Tool previews, prompt labels, dashboard child rows.
- Other agents (Codex, OpenCode…) keep their OSC / screen reading.

## How

- **Hooks** (`tern-bridge::hooks`): the tab's settings register every event
  Orca does (`PreToolUse`, `PostToolUse`, `PostToolUseFailure`,
  `PermissionRequest`, `UserPromptSubmit`, `Stop`, `StopFailure`,
  `SubagentStart`, `SubagentStop`, `TeammateIdle`, `PostCompact`, `SessionEnd`
  from 2.1.261) as HTTP hooks; `SessionStart` stays a command relayed by
  `tern --hook`. Claude 1.0.23–2.1.100 throws away a settings file naming an
  event it doesn't know, so each event is written only for a Claude that
  knows it (`tern-bridge::claude_version`, Orca's table); the daemon probes
  `claude --version` the way it will start Claude (the tab's shell), caches
  it 5 minutes, and an unresolved version gets what 1.0.64 knows.
- **Tracker** (`tern-bridge::claude_status`): a pure, serializable state
  machine per tab, fed every hook payload; it yields a `ClaudeStatus` (state,
  monitoring, wake-up pending, main agent state and outcome, turn completed
  stamp, sub-agents) and the instant its lease next expires.
- **Daemon** (`tern-mux`): each Claude session owns a tracker. Its status is
  the session's status (it notifies when no UI is attached), sent to the UI
  as `ServerMsg::AgentStatus`. A Stop Tern's own context guard blocks is not
  a turn end. The host's loop wakes at the lease deadline.
  Cancel (Orca infers it from the keystroke): while the main agent is in a
  turn, the host reads Claude's session file every second; at its prompt
  twice in a row with no hook between marks the turn cancelled (an OSC 7501
  report that the turn is over only reads it sooner). The file saying busy,
  or an OSC "working", while the tab waits on AskUserQuestion clears that
  wait.

## Verified

- Unit tests port Orca's (`tern_term::claude_status::tests`), replaying its
  captured hook payloads (`fixtures/`, MIT); daemon and handoff tests post
  hooks over HTTP.
- Live, Claude Code 2.1.312 in the debug build: a `run_in_background` sleep
  read monitoring (pulse icon) after the turn, then the wake-up turn, then
  done, never done early; Esc on a foreground command read done
  (cancellation) about 2 s later.
- **UI** (`tern-app`): a tab with a hook status follows it, over its OSC
  reports and session file. Monitoring shows as working in its own look
  (Orca's "Monitoring background tasks"); the tip says what holds it.

## Left to the builder

Glyph for monitoring, wording of tips.
