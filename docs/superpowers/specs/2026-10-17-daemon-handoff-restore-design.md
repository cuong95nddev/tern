# Daemon handoff and tab restore

## Why

After an update the background daemon keeps running the old build. Today the
app offers to restart it at launch (`restart.rs`), which ends every program and
starts each tab from scratch: a shell opens in the workspace root with an
empty screen, and a Claude tab starts a new conversation (the dialog tells the
user to `/resume` by hand).

Two complementary fixes, modeled on what others do (herdr: live handoff on
Unix; cmux: restore after relaunch):

1. **Live handoff (macOS/Linux).** The old daemon hands its running terminals
   to a daemon of the new build. Programs never notice; tabs reattach.
2. **Restore (every platform; the fallback).** When terminals do have to be
   restarted (Windows, a daemon too old to hand off, a failed handoff, the
   manual "Restart Background Terminals"), each tab comes back as close to
   what it was as possible.

## Goals

- At launch, an outdated daemon (same protocol major, new enough to know the
  handoff) with running programs is handed off silently. No dialog.
- Any failure before the new daemon commits rolls back: the old daemon keeps
  serving as if nothing happened, and the launch dialog is shown as today.
- A restarted shell tab starts in the folder its shell was last in, below the
  screen and scrollback it showed (scrolled into history).
- A restarted Claude tab resumes its conversation (`claude --resume <id>`),
  falling back to a new one when the resume fails at once (as on launch).

## Non-goals

- Live handoff on Windows (ConPTY's `HPCON` can't move between processes;
  herdr doesn't do it either).
- Handoff from daemons released before this change, or across protocol
  majors: those get the restart dialog, which now restores.
- Rerunning the program a shell tab was running (cmux doesn't either).
- Restoring tabs after the whole machine restarted (the UI is gone then; the
  cwd and screen aren't saved to disk).

## Live handoff (Unix)

Request: the UI sends `ClientMsg::HandOff { req, exe }` (MINOR 2) with its own
executable. A daemon that can't (Windows) answers
`ServerMsg::HandOffFailed { req, message }`. The UI only asks a daemon whose
`Hello` says MINOR ≥ 2.

Old daemon:

1. Refuses new connections (as when it exits for idleness: clients retry) and
   holds `Spawn`s until the handoff ends (a tab whose request was lost with
   the connection asks the new daemon again).
2. Starts `<exe> --server --adopt` with one end of a socketpair as fd 3
   (in tests: a thread running `adopt` on the other end).
3. Pauses every session's driver thread (it deregisters its PTY and hands the
   driver back without dropping it) and captures each terminal's snapshot.
4. Sends `Begin` with the daemon lock fd and the hook server's listening
   socket fd, then one `Session` per session with its PTY master fd (none for
   ended ones): id, child pid, size, scrollback, title, status, exit, hook
   token, Claude session id, queued hook bodies, unwritten input, whether it
   asked for color scheme reports, and the packed snapshot.
5. Waits for `Restored`, sends `Commit`, waits for `Committed`.
6. On success closes every connection (UIs reattach to the new daemon by
   session id; observers reconnect), forgets the paused drivers (dropping
   alacritty's `Pty` would SIGHUP the program) and exits without removing
   the socket, pid file or run dir, which the new daemon now owns.
7. On any failure or timeout: kills the new daemon, resumes every driver,
   accepts connections again and answers `HandOffFailed`. If it had already
   sent `Commit`, it also binds the socket path again.

New daemon (`--adopt`):

1. Checks the handoff version and protocol major, takes over the lock (the
   passed fd holds the same `flock`) and the hook listener (same port, same
   tokens, so Claude's settings files stay valid), and keeps the old run dir.
2. Rebuilds each terminal from its snapshot; the PTY is "adopted": not its
   child, so it learns of the exit by polling the pid (`waitpid` when it is
   its child, as in tests, else `kill(pid, 0)`) and reading to EOF. Exit codes
   of adopted programs are unknown. Drivers start only after `Commit`.
3. On `Commit`: binds the socket at a temporary path and renames it over the
   public one (a client always finds a socket), writes the pid file, starts
   the drivers and serving, answers `Committed`.

The handoff messages carry a version; the new daemon reads every version up to
its own, so any newer build can adopt from this one on.

Not carried (lost in a short window): input typed and hook requests that
arrive while the drivers are paused, a half-parsed escape sequence (the
reattach "jiggle" makes full-screen programs repaint).

## Restore (all platforms)

- `ClientMsg::Spawn` gains `seed`: a packed snapshot. The daemon puts its
  primary grid (with scrollback) in the new terminal, default modes, and
  scrolls the visible lines into history before the program starts.
- `TerminalView` remembers its shell's cwd (read with the foreground program
  scan it already does) and, on losing the daemon, keeps the old mirror's
  terminal. Starting again after a loss (after a restart on purpose, or Enter
  once the server stopped) spawns in that cwd with the old screen as the seed.
  Claude tabs get no seed and keep their cwd: `--resume` redraws the
  conversation.
- The workspace keeps a Claude tab's spawn config on `--resume <session>` as
  soon as a hook reports the session id, and on a restore marks the tab as
  resuming, so a resume that fails at once starts a new conversation.
- The restart dialog's text says tabs come back in their folders and Claude
  conversations resume.

## App flow

`restart::check_at_launch`: outdated daemon with running programs, on Unix and
MINOR ≥ 2: hand off in the background; on success no dialog for it, on failure
the dialog as today. Daemons of older majors keep the dialog. The manual
"Restart Background Terminals" stays a restart (it also unsticks a daemon).

## Testing

- tern-mux integration tests (Unix): in-process handoff between two servers on
  one dir: a running program keeps its pid and output, a tab reattaches by id,
  input reaches it, an ended session keeps its exit, the hook token and Claude
  session carry over, an adopted program's exit is seen, and a failing adopter
  rolls back with the old daemon still serving.
- Spawn with a seed (every platform): the old lines are in the scrollback and
  the screen starts blank.
- Only `cargo check --target x86_64-unknown-linux-gnu` can run on the
  Windows dev machine; CI runs the Unix tests on macOS and Linux.
