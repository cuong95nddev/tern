# Handoff: performance pass (2026-10-18)

The user asked: "check Tern's performance, I want it as smooth as possible and using as little RAM as possible."
Done so far: an audit (three read-only agents: render path, mux/daemon, app idle work), real measurements, and a
first set of safe fixes. Next: the bigger items below, in the order the user picks.

State: branch `perf-2`, commit `161ad23` ("Less work per frame: row-wide text segments, quieter titles"), on
top of `1ab17b9` (Tern 0.4.0). **Not pushed, not merged.** `cargo test --workspace`, clippy (native and
cross linux/macOS for tern-term/tern-mux) and `cargo fmt` are all clean. (An older branch `perf` exists and is
already merged into main: don't reuse it.)

## What was measured

How: release build with `CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true` (so it uses `dev.lock` / `mux-dev` and runs
beside the installed Tern), launched with `APPDATA` / `LOCALAPPDATA` pointed at a fresh folder. The Unix
socket path limit means that folder must be short (`D:\tb\<name>`, not the scratchpad). The shell is a fake
`pwsh.exe` placed first on `PATH`: it runs `powershell -NoExit -File $TERN_BENCH` (`platform::shell()` prefers
`pwsh.exe` from `PATH`). Workloads: idle; a Claude-like spinner (OSC title + `\r` line every 80ms); a
`cmd /c type` of a 16MB / 200k-line colored file. The scripts were in the session scratchpad and are gone, so
rebuild them from this description if needed.

Medians:

| Scenario | UI CPU | UI RAM (working set) | daemon RAM |
|---|---|---|---|
| idle, 1 tab | ~0.3% | ~78MB | ~15MB |
| spinner 12Hz | 5–7% of a core | ~81–85MB | ~15MB |
| 16MB flood | 14.6s total (plain conhost: 14.3s) | ~103MB | ~39MB |

- **Flood throughput** is bound by ConPTY, not Tern.
- **Frame breakdown with spinner**, from temporary counters since removed: about 10.6 frames/s at ~2.7ms each.
  - MainWindow render 0.16ms, Workspace render 0.34ms.
  - TerminalView render 0.03ms, terminal prepaint (`build_frame`) 0.34ms, terminal paint 0.35ms.
  - The remaining ~1.5ms is GPUI layout/prepaint/paint of the rest of the window tree.
- **Why the whole window redraws:** GPUI's `mark_view_dirty` (gpui-pre `window.rs:2252`) marks every *ancestor*
  view dirty. So each terminal notify re-renders MainWindow and Workspace, whatever `.cached()` is used. The
  agent's suggestion to cache the workspace slot in `main_window.rs` doesn't help.
- **Fixes in `161ad23`:** measured CPU change is within noise; UI RAM went down ~3–5MB.
- **RAM per tab:** a full 10k-line scrollback costs ~25MB in the UI mirror plus ~25MB in the daemon. Both keep a
  `Term` with 10k lines (`workspace.rs:842`, `mirror.rs:82`, `host.rs:174`). This is the main RAM that grows
  with tab count.

## Done in `161ad23`

- `terminal_view.rs` `RowBuilder::pad`: blank cells between words become spaces in the segment (lines are not
  carried over blanks), so a row is one `shape_line` rather than one per word. Wide chars keep their own
  segment.
- Background color `rgb_to_hsla` is cached per run (`last_bg`), like the foreground.
- `raw_title` vs `title`: `title()` (the tab label) is `strip_spinner(raw_title)`. `TitleChanged` is emitted only
  when the label changes, and an unchanged OSC title is ignored. Agent status reading still uses `raw_title`.
- **Bug fix:** hidden tabs never render, so the mirror's wake flag stayed set and no further `Wakeup` came. That
  made `last_output` stale, so busy/Working was wrong. The per-tab 1s tick now calls `take_wakeup()`.
- `viewer.rs`: `language` field; `set_highlighter` only when the language changes, not on every reload.
- `proto.rs` `FrameReader`: shrinks the buffer back after a frame over 1MB.

## Next, ranked

1. **Scrollback RAM.** Either keep history only in the daemon and have the UI request pages when scrolled or
   searched (big change, ~25MB saved per heavy tab), or lower the default from 10k to 5k (a behavior change,
   ask the user).
   - Also, the attach snapshot clones the whole grid under the term lock (`snapshot.rs:282`, 3 clones on the
     alt screen) and packs it on the connection's writer thread, which stalls the other sessions' output.
     Unpacking happens on the single client reader thread (`client.rs:590`).
   - Fix: pack and unpack on workers, and send the screen first and history after.
2. **Window layout cost (~1.5ms/frame).** Make the tab bars, side strips and other parts of `Workspace::render`
   their own cached views, so a terminal frame only rebuilds a shallow tree. Measure again with counters around
   the render, prepaint and paint parts.
3. **Input jank.** Done (see below).
4. **Idle wakeups.**
   - One app-wide 1s ticker instead of one per tab (`terminal_view.rs` ~343), with one shared process scan.
   - Agent monitor: `agent_monitor.rs:24,171` notifies every 500ms while open; make it event-driven.
   - Daemon: an observer thread ticks every 250ms (`server.rs:463`), the idle thread every 1s (`server.rs:198`),
     and `check_scheme` polls every 300ms per 2031 session (`host.rs:392`). Use Condvar/events instead.
5. **Mux overhead per PTY read** (`server.rs:760-821`): about 3 copies and about 4 mutexes per chunk, and one
   `write_all` per frame. Batch frames in the writer and use a borrowed `&[u8]`.
   - Queue limits are too loose: `MAX_QUEUED_SESSION` is 64MB and `MAX_QUEUED` 256MB (`server.rs:38`).
   - `host.rs:161` and `host.rs:367` have unbounded queues.
6. **Screenshot editor:** unlimited undo of full RGBA images (`shot/editor.rs:108,152`, ~33MB per step at 4K).
   Old `RenderImage`s are never `drop_image`d, `EditorData.clip` is kept after close (`editor.rs:603`), and tray
   thumbnails decode full images (`tray.rs:386`).
7. **Smaller items:** all done (below).

## Done after the handoff: git status refresh and small items

- `watch.rs`: `ChangeBatch.git_repos` names the repos whose `HEAD`/`index`/`index.lock`/`refs` changed
  (was one `git_changed` flag).
- `changes_view.rs`: changes wait in `Dirty` until the disk is quiet for 250ms (at most 2s), and then only the
  repos owning a changed path run `status`. When only worktree files changed, one `git check-ignore -z --stdin`
  per repo first; if every path is ignored, no status. A repo with `.git/index.lock` is skipped. Its release is
  reported, which triggers the refresh again.
- Compared with Warp (an in-process `ignore` matcher, 500ms debounce + 5s throttle, `index.lock` watched) and
  T3 Code (no watcher, `check-ignore --stdin`, 250ms trailing, skip on `index.lock`).
- Fixes the diff pane bug: it reloads when `git_repos` holds its repo.
- Tested in the app with a logging `git.exe` wrapper first on `PATH`. The test folder was an outer repo
  plus `ext/a`, `ext/b`, `ext/c`.
  - 150 writes to ignored `out/` over 3s: 2 `check-ignore`, 0 `status`.
  - 40 edits in `ext/a`: 1 `check-ignore` + 1 `status`, only for `ext/a`.
  - A commit in `ext/b`: only `ext/b`.
  - 5s idle: nothing.
  - `QUIET` must exceed the explorer watcher's 400ms batch cap, or each batch starts a refresh.
  - Windows reports a repo's `.git` dir itself when files in it change. Repos are only looked for again
    when a `.git` really appeared or vanished, or a known repo's `.git` is gone (deleting a whole repo
    reports just its dir).
- The `repo_defines_commands` answer is cached per repo (`ChangesView.defines_commands`). It is dropped
  when that repo's `.git` (now including `config`) changes and on a full refresh.
- `FsWatcher` debounce is trailing: a batch goes out once the disk is quiet for the window, at most 10×
  the window after the first change. Only kept paths extend the wait.
- The welcome screen reads the recent folders once per render.

## Done after the handoff: input jank

- `client.rs`: an `Outbox` and a "mux writer" thread. The UI only queues messages, and they are written in
  batches. A failed write ends the connection, which shows up as `ServerLost`. Queued input over 64MB makes
  the sender wait. `Closer` waits up to 1s for queued `Close`/`Detach` before the socket shuts.
- `mirror.rs`: mode bits are kept in an `AtomicU32` after each feed, sync flush and install.
  `RemoteSession::mode()` and `TerminalView::mode()` read them without the term lock.
- `terminal_view.rs`: ⌘-hover path lookup (`links::resolve` + the foreground cwd) runs in the background,
  with a cache of 64 entries cleared when ⌘ goes down. A ⌘-click still resolves inline when the path isn't
  cached.

## Notes

- **Disk:** D: was 100% full during the work. Separate target dirs cost ~2.2GB each, so delete them afterwards.
  rustc crashed with `STATUS_ACCESS_VIOLATION` under memory/disk pressure; `-j 4` helped.
- **Line endings:** Python edits on Windows write CRLF and the repo is LF (`.gitattributes`), so run
  `sed -i 's/\r$//'` on any edited file.
- **Left for the user:** `D:\tb` (bench env folders; the safety check blocked deleting it). An old dev daemon
  `tern-server-0.3.0-p2-50059e…` (from 2026-10-17, `mux-dev`, with 5 PowerShells) was still running.
