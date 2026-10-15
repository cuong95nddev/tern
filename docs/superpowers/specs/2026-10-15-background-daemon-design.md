# Background sessions (tern daemon) — design

Date: 2026-10-15
Status: approved in chat ("làm theo bạn recommend đi không cần hỏi")

## Goal

Every terminal tab (Claude and shell) keeps running when Tern quits, crashes, or updates
itself, and comes back exactly where it was when Tern starts again. While no Tern window
is open, Claude Code still reaches Tern's hooks, and a turn that finishes or needs
approval posts a macOS notification; the tab shows its badge when Tern comes back.

Success:
- Open a Claude tab, start a long turn, ⌘Q. The `claude` process keeps running
  (`pgrep claude`). The notification "Claude finished" appears when the turn ends. Start
  Tern: the same process is on screen (same pid), with its scrollback, and the tab shows
  the Done badge.
- A shell tab running `top` survives ⌘Q and is still `top` after relaunch, with the right
  size.
- Closing a tab (or a window) still ends its processes.
- Auto-update relaunches Tern and every tab survives.

Out (YAGNI for this spec): surviving logout/reboot (Claude tabs already resume with
`claude --resume`), more than one UI attached to one tab at a time, remote machines, live
handoff of PTYs to a newer daemon (SCM_RIGHTS, as herdr does), Windows.

## References studied

Notes in the session scratchpad (`refs/`): shpool (daemon autostart, peer uid check,
serialized-grid restore + input modes, reattach "jiggle" resize), tmux (0700 socket dir,
strict protocol version, `window-size latest`), WezTerm mux (length-prefixed PDUs,
version handshake, local mirror of remote panes), zellij (protocol major in the socket
path), herdr (headless terminal per pane, agent state kept server-side, SCM_RIGHTS
handoff as a later opt-in), dtach/abduco (what not to do: redraw by `^L`).

## Architecture

```
Tern.app (UI process)                          tern --server (daemon, one per user+flavor)
┌──────────────────────────────┐   Unix socket  ┌────────────────────────────────────────┐
│ TerminalView                 │   (frames)     │ Server: accept, peer-uid check, routing │
│   └ Session (tern-term)      │◀══════════════▶│ Host × N   (one thread each)            │
│       Remote: mirror Term    │                │   PTY + child, authoritative Term,      │
│       parse-only, no replies │                │   vte Processor, OscScanner,            │
│ MuxClient (one connection,   │                │   status tracker, recent-signal ring    │
│   reader thread, routing)    │                │ HookServer (127.0.0.1, lives as long    │
└──────────────────────────────┘                │   as the daemon) + per-token hook ring  │
                                                │ Notifier (UNUserNotificationCenter,     │
                                                │   only while no UI is connected)        │
                                                └────────────────────────────────────────┘
```

### Crates

- `tern-term` (existing): gains `snapshot` (capture/restore a `Term`), the `ActivityStatus`
  state machine moved from the app so both processes share it, and `Session` becomes a
  facade over a remote backend (the UI never owns a PTY any more). The PTY driver used by
  the daemon lives here too (`host.rs`), replacing alacritty's `EventLoop`.
- `tern-mux` (new): wire protocol, `server` (daemon main), `client` (`MuxClient`), socket
  dir/peer checks, daemon spawn, notifier.
- `tern-bridge`: unchanged API; the daemon owns the `HookServer`.
- `tern-app`: `main()` runs `tern_mux::server::run()` when started with `--server`, before
  anything GPUI. Tabs remember their daemon session id.

### Why our own PTY loop

Attaching must take a snapshot of the Term and start the byte stream at exactly the same
point. alacritty's `EventLoop` reads bytes before it gets the Term lock and parses them
later, so a snapshot can miss or duplicate bytes. The daemon's host thread does
read → lock Term → parse → send bytes to the attached client → unlock, and attach takes
the same lock, so nothing is lost or doubled. The loop is small (modeled on alacritty's
`event_loop.rs`): `polling` on the PTY fd and alacritty's child-exit signal, an input
queue woken through `Poller::notify`, resize messages, and the DEC 2026 sync timeout.

### Mirror Term in the UI

The UI keeps an alacritty `Term` per tab and feeds it the same bytes, so rendering,
selection, scrollback, search and links keep working unchanged (`terminal_view.rs` reads
`session.term().lock()` as today). Rules that keep the mirror identical to the daemon's:

- Only the daemon answers the program: `PtyWrite`, color and size queries, the OSC 7501
  probe. The mirror's listener drops them. Title, bell and clipboard store events come
  from the mirror (same bytes, same events as today).
- Stream markers are sent in order with the bytes: `Resized{cols,rows,cell_w,cell_h}` (the
  daemon resized its Term and PTY here), `SyncFlush` (the daemon ended a DEC 2026 sync on
  timeout), `Exited{code}`. The UI applies each at the same point of the stream, so reflow
  and sync handling match.
- The UI asks for a resize; it resizes its mirror only when the `Resized` marker comes
  back.
- The OSC scanner runs in both: the daemon for the probe reply, status tracking and
  notifications; the UI for live status and notifications as today (it ignores the probe).

### Snapshot (attach)

Under the host's Term lock the daemon captures:
- the active grid (`Grid<Cell>`, serde, already supported by alacritty 0.26), with
  history capped to the configured scrollback;
- when the alternate screen is active, the primary grid too: clone the alt grid,
  `swap_alt()` (non-destructive when leaving alt), clone primary, `swap_alt()` back, put
  the alt clone back with `grid_mut()`;
- `TermMode` bits, kitty keyboard flags, cursor style, color overrides
  (`term.colors()`), the last title, the size and cell size.

The UI builds a fresh Term of that size, installs the grids (primary, then `CSI ?1049h`,
then the alt grid), and replays the modes as VT sequences (DECSET/DECRST for each mode
bit, `ESC =`/`ESC >`, `CSI = flags ; 1 u`, DECSCUSR, OSC 4/10/11/12, OSC 2). Then it
subscribes to the stream that continues exactly after the snapshot.

Not captured (no public API, or mid-parse): scroll region, charsets, tab stops, a
half-read escape sequence, bytes buffered inside an open DEC 2026 sync. To make full-screen
programs repaint over any such difference, the daemon does shpool's "jiggle" on attach:
resize the PTY to rows+1 and immediately back to the client's size ~50 ms later
(both through `Resized` markers).

The snapshot also carries the tab's recent OSC signals (last 32 status/notify signals since
the previous detach) and its queued hook bodies; the UI replays them with
`replayed: true` so the status badge (e.g. Done) is computed as if it had been attached,
without notifying again (the daemon already did).

### Protocol

- Frames: `u32` little-endian length, then a msgpack (`rmp-serde`) message. Client→server
  frames are capped at 16 MiB, server→client at 256 MiB (snapshots), and a peer that sends
  a bigger frame is dropped.
- First frame each way is `Hello { major: 1, minor, app_version }`. The socket path holds
  the major (`mux-1.sock`), so incompatible majors never meet (zellij). Messages are
  externally tagged enums; a variant the receiver doesn't know is logged and skipped, so a
  minor bump can add messages and optional fields (`#[serde(default)]`).
- Client → server: `Spawn { cfg, hook: bool }` → `Spawned { id, token: Option }` (or `Error`),
  `Attach { id, size }` → `Attached { id, snapshot }` (the PTY master fd travels with it
  over SCM_RIGHTS, see below) or `NotFound { id }`, `Detach { id }`, `Input { id, bytes }`,
  `Resize { id, size }`, `Respawn { id, cfg }` (restart the process in the same tab, same
  id and hook token), `Close { id }` (kill), `List` → `Sessions [...]`.
- Server → client: `Output { id, bytes }`, `Marker { id, Resized | SyncFlush | Exited }`,
  `Hook { token, body }`, `Detached { id }` (taken over by another client), and replies.
- One connection per UI process, multiplexing every tab. The daemon writes to it from a
  writer thread through a bounded queue (64 MiB); if the UI stops reading, the daemon
  drops the connection rather than block hosts. The UI then reconnects and re-attaches
  every tab (fresh snapshots).
- Spawn requests carry the full `SpawnConfig` built by the UI (env, cwd, args), so the
  UI's environment fixes (locale, cleaned variables) keep applying.
- Hooks: with `hook: true` the daemon registers a token with its `HookServer`, writes
  the Claude settings file in its own run dir (`…/run/<daemon pid>/`), and replaces the
  placeholder `{tern-settings}` in the config's args with the shell-quoted settings path
  before spawning. The UI builds `claude … --settings {tern-settings}`. `Spawned` returns
  the token so the UI can match `Hook` events to the tab. Token and settings file live
  until the session is closed; `Respawn` keeps them.

### Foreground program queries

`foreground_programs()` / `foreground_cwd()` (agent detection every second, quit/close
confirmation, link resolution) need the PTY. `Attached` passes a duplicate of the PTY
master fd with `SCM_RIGHTS`; the UI keeps it only for `tcgetpgrp`-style queries
(`tern_term::procs`), never reads or writes it. These calls stay synchronous.

### Daemon lifetime

- Socket dir: `~/Library/Caches/dev.tern/mux` (`mux-dev` for debug builds), created 0700;
  refused if not owned by us or group/other-accessible (tmux). Every accepted connection
  is checked with `getpeereid` (shpool).
- Start: the UI connects; on `ENOENT`/`ECONNREFUSED` it spawns `current_exe() --server`
  with `setsid` (stdin null, stdout/stderr appended to `~/Library/Logs/Tern/daemon.log`)
  and retries with backoff for up to 3 s (shpool). The daemon takes an exclusive `flock`
  on `server.lock` before binding; if it can't, another daemon is running and it exits.
  It removes a stale socket file only after a refused connect.
- The daemon ignores SIGHUP/SIGINT, exits on SIGTERM (hanging up every child), and exits
  by itself when it has no sessions and no client for 10 s.
- It never execs anything but the configs the UI sends.

### UI behaviour changes

- A tab's daemon session id is saved with the tab (`claude\t<conv>\t<name>\t<mux id>`,
  `shell\t<name>\t<mux id>`; older files without the field still load).
- Restoring a tab: `Attach(id)`; on `NotFound` (daemon restarted, process gone) spawn as
  today (`claude --resume <conv>` for Claude tabs, a new shell otherwise).
- After restoring windows, live daemon sessions no saved tab refers to (session file not
  written before a crash) are opened as tabs in the first window, so nothing keeps running
  unseen. Exited orphans are closed.
- ⌘Q / Dock quit / update relaunch: detach only. The quit confirmation no longer lists
  running programs (nothing ends); closing a tab or a window still asks and sends `Close`.
- The UI losing the daemon (crash): every tab shows `[background server stopped]` as an
  exited tab; Enter restarts it (Claude tabs with `--resume`), using a new daemon.
- Exited processes stay in the daemon (final screen, exit code) until the tab is closed or
  restarted, so a tab that ended while Tern was closed shows "[process exited]" on return.

### Notifications while no UI is connected

The daemon tracks each host's `ActivityStatus` with the shared state machine. With no
client connected, a transition to Blocked or Done, or an OSC 9/777 notification, posts a
macOS notification through `UNUserNotificationCenter` (the daemon runs the app bundle's
executable, so it has the bundle identity; same guard as GPUI: skipped when
`bundleIdentifier` is nil, e.g. `cargo run`). Title is the agent's name, as in the UI.
Clicking it opens Tern (the system launches the bundle).

### Updates

The updater replaces the bundle and relaunches the UI; the daemon keeps running from the
old (unlinked) binary and the new UI talks to it as long as the major matches. New fields
are additive (minor). A major bump must ship either a live handoff or an explicit "stop
the old daemon" flow; neither is needed for major 1.

## Error handling

- Spawn failure in the daemon → `Error { message }`; the tab shows the message (as today
  for local spawn errors).
- Malformed frame, oversize frame, wrong uid → connection dropped, logged.
- Host thread panics are caught per host: that session becomes Exited(None) with the panic
  logged; the daemon stays up.
- Daemon unreachable after spawning it → tabs open with the error text and a "Retry"
  hint (Enter restarts).

## Testing

Unit (no UI):
- protocol: frame round-trip, size limits, unknown variant skipped;
- snapshot: feed byte sequences to Term A, capture, restore into Term B, compare visible
  text, history length, cursor, modes; cases: plain shell output with scrollback, alt
  screen (vim-like), bracketed paste + mouse modes, kitty keyboard flags, color overrides;
- status state machine (moved code keeps its tests);
- host: spawn `/bin/sh -c 'printf hello; read x'`, bytes reach a subscriber, input reaches
  the child, `Resized` and `Exited` markers arrive in order.

Integration (`tern-mux` tests, temp socket dir):
- server + client in-process: spawn, attach, output, detach, reattach (snapshot contains
  earlier output, same pid), close kills the child, `NotFound` for unknown ids;
- peer check rejects a socket dir with mode 0777;
- fd passing: `foreground_programs` through the passed fd sees `sh`.

Manual (dev build + control socket `scripts/dev.sh`): Claude tab survives ⌘Q and relaunch
(same pid), `top` survives with the right size, closing tab kills, notification after quit
when a turn ends (installed build only — needs the bundle).
