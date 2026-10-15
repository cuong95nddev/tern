# Menu bar agent — design

Date: 2026-10-16. Branch: `background-daemon` (builds on the background sessions daemon,
`2026-10-15-background-daemon-design.md`).

## Goal

The user can always see, from the macOS menu bar, every terminal the background daemon runs —
across all windows and workspaces, and while Tern is closed — and open or end any of them.
The icon is Tern's permanent place in the menu bar: more features will live there later.

What the user asked for:

- The list shows **all** running sessions (not only the ones no tab shows).
- It lives in the **macOS menu bar** and is there while Tern is closed.
- The icon is **always shown**, with a setting to turn it off.
- **Open** puts the session in the active workspace; with no window, a new window opens with
  that tab.

Assumed (not asked; cheap to change): ending a session from the menu asks first; the count
next to the icon is hidden at 0; "needs input" (a Claude/Codex waiting) gets a mark on the icon.

## Non-goals

- Starting at login (the agent starts with Tern; login item later).
- Any menu bar feature besides the session list, Open Tern, settings toggle.
- Linux/Windows (the daemon is Unix-only, the menu bar is macOS-only).

## Approach

A separate, stateless process: `tern --menubar` (same binary, like `--server`). It holds no
programs, so it can be restarted or updated at any time without touching a terminal, and a bug
in its AppKit code can't take terminals down. It talks to the daemon as an **observer** client.
(Rejected: the daemon hosting the icon — it could then never exit or update without killing
every program; the UI hosting it — gone when Tern quits.)

```
 Tern UI ──ui conn──┐                ┌── observer conn ── tern --menubar (NSStatusItem)
                    ├── tern --server┤
 (Reveal forwarded) ┘   (daemon)     └─ pushes session lists, forwards Reveal to the UI
```

## Protocol (MINOR 1, same MAJOR)

Fields are added with `#[serde(default)]`; new message kinds are skipped by older peers.

- `ClientMsg::Hello` gains `role: Role` (`Ui` default, `Observer`).
  Observers don't count in `clients`: they neither keep the daemon from idling out nor stop
  "unattended" notifications.
- `ClientMsg::Watch` — from now on, the daemon sends `ServerMsg::SessionsChanged { list }`
  (immediately, then on every change), coalesced to at most one per 250 ms.
  Changes: spawn, close, exit, status, title, attach/detach.
- `ClientMsg::Reveal { req, id }` — the daemon sends `ServerMsg::Reveal { id }` to the most
  recently connected UI connection and answers `ServerMsg::Revealed { req, delivered: bool }`.
- `SessionInfo` gains `status: ActivityStatus`, `attached: bool` (some UI shows it),
  `claude: bool` (spawned with a hook token) and `claude_session: Option<String>` (the
  `session_id` of its latest Claude hook event, for `--resume` after an adopted tab restarts).

Version note: an old (MINOR 0) daemon ignores `Watch`/`Reveal`; the agent then falls back to
`List` polling every 2 s and to the URL path for Open.

## The agent (`crates/tern-menubar`)

- `model.rs` (pure, tested): `rows(&[SessionInfo]) -> Vec<Row>` (order: needs input, working,
  done, idle, exited; then title) and `icon(&[SessionInfo]) -> IconState { count, attention }`.
  Row = status glyph, title (or the first foreground program, or "shell"), `~`-shortened cwd,
  "not open" when `!attached`.
- `app.rs` (AppKit via `objc2-app-kit`): `NSApplication` with the Accessory activation policy
  (no Dock icon), one `NSStatusItem` (template image from `assets/icon` + count text, a dot
  when `attention`). The menu is rebuilt in `menuNeedsUpdate:` from the latest list:
  - one item per session; clicking it = Open; its submenu: Open, End… (an `NSAlert` asks);
  - separator, "Open Tern", "Hide Menu Bar Icon".
  - With no daemon: a disabled "No background terminals" item.
- `link.rs`: a thread that connects to the daemon's socket as an observer (never starts it),
  sends `Watch`, and hands lists to the main thread (`dispatch` main queue). On a lost or
  refused connection it retries every 2 s and shows the empty state.
- Single instance: `flock` on `~/Library/Caches/dev.tern/menubar.lock` (`menubar-dev.lock`
  for debug builds); the file holds `<pid> <version>`. A newer agent finding an older holder
  SIGTERMs it and waits for the lock (≤ 2 s); an equal or newer holder makes it exit.

## Open (Reveal)

1. The agent sends `Reveal { id }`. Delivered → done.
2. Not delivered (no UI connected, or an old daemon): the agent opens
   `tern://session/<id>` (release: LaunchServices starts or activates Tern, which registers the
   scheme via `CFBundleURLTypes` in `scripts/bundle-macos.sh`); a debug agent instead starts its
   own executable with `--open-session <id>` (no bundle to route a URL to).
3. The UI (`main_window::reveal_session(id)`, from a `Reveal`, a `tern://session/` URL or
   `--open-session`), after the saved session is restored:
   - a tab already shows it → bring its window forward, activate its workspace and tab;
   - else the active window's active workspace adopts it (`adopt_session`, as a Claude tab when
     `claude`, with `claude_session` as its resume id);
   - no window → open a window at the session's cwd (else `$HOME`) and adopt it there.
   - unknown id (it ended meanwhile) → nothing.

## Settings

- A `menubar` file next to `claude-args`: `off` disables the icon; missing = on.
- Tern menu: "Hide Menu Bar Icon" / "Show Menu Bar Icon" (writes the file; hiding SIGTERMs the
  agent, showing starts it). The agent's own "Hide Menu Bar Icon" writes `off` and exits.

## Lifecycle

- Tern UI at launch (not scripted, setting on): start `tern --menubar` detached (`setsid`,
  log to `~/Library/Logs/Tern/menubar.log`); the lock rules above keep one, the newest.
  The agent outlives the UI.
- So after an update, the first launch of the new Tern replaces the agent. The daemon is
  untouched.

## Errors

- No daemon / refused / lost connection → empty state, retry every 2 s, never start the daemon.
- `Reveal` not delivered and URL open fails → log it; the menu stays.
- Ending a session: `Close { id }` as the UI does. If it already ended, nothing.
- Agent crash → no terminal affected; the next Tern launch starts it again.

## Testing

- `tern-mux` integration tests: observer doesn't count for idle exit or `unattended`;
  `Watch` gets an initial list and a push on spawn/status/exit/close (coalesced);
  `Reveal` reaches the UI connection and reports `delivered: false` without one;
  `SessionInfo` carries `attached`, `claude`, `claude_session`; MINOR-0 peers still parse.
- `tern-menubar` unit tests: `rows` ordering and labels, `icon` count/attention; lock takeover
  (older holder replaced, newer kept) with a test lock path.
- `tern-app`: `reveal_session` target choice as a pure function over (tabs with mux ids,
  active window?) → Focus / AdoptIntoActive / NewWindow.
- By hand on the installed `.app`: icon with Tern open and closed, count and attention mark,
  Open into the active workspace / with no window / when Tern is closed (URL path), End…,
  Hide/Show, update replacing the agent.
