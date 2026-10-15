# Background sessions (tern daemon) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Terminal tabs run in a per-user `tern --server` daemon and survive Tern quitting, crashing or updating.

**Architecture:** The daemon owns each PTY plus the authoritative alacritty `Term` (own PTY loop so snapshot and byte stream line up), the hook server and status tracking. The UI keeps a parse-only mirror `Term` per tab fed from one Unix-socket connection; attach sends a serialized grid + modes, then the live byte stream with in-band markers.

**Tech Stack:** Rust 2024, alacritty_terminal 0.26 (serde grids), polling 3, serde + rmp-serde, libc (SCM_RIGHTS, getpeereid, flock, setsid), objc2-user-notifications (daemon notifications), GPUI (unchanged).

**Spec:** `docs/superpowers/specs/2026-10-15-background-daemon-design.md`

## Global Constraints

- `cargo fmt --check`, `cargo clippy --workspace --all-targets` and `cargo build --release` stay warning-free (the `block v0.1.6` future-incompat note is accepted); `cargo test --workspace` green.
- Protocol major is `1`; socket file `mux-1.sock` in `~/Library/Caches/dev.tern/mux` (`mux-dev` for debug builds), dir mode 0700.
- Frame: `u32` LE length + msgpack; client→server ≤ 16 MiB, server→client ≤ 256 MiB.
- Daemon idles out after 10 s with no sessions and no client.
- Hook settings placeholder in spawn args: `{tern-settings}`.
- Commit as `cuong95nddev <cuong95nd@gmail.com>` (repo-local config). No tags/releases.

## Review Focus

1. Attach while the program is printing a lot (`yes`): the mirror must not show doubled or missing lines — the host sends bytes under the Term lock that attach also takes (test in Task 5: attach mid-stream, compare mirror text to daemon text).
2. Alternate-screen program (vim/Claude in alt screen) at reattach: both screens restored; leaving alt shows the old shell output (Task 2 test `alt_screen_round_trip`).
3. A UI that stops reading (window hidden, main thread busy) must not stall the PTYs of other tabs: bounded writer queue, connection dropped (Task 6 test `slow_client_is_dropped`).
4. Daemon killed (`kill -9`) while Tern runs: tabs show "[background server stopped]" and Enter restarts them through a new daemon (Task 8 manual check + client test `server_loss_exits_sessions`).
5. Old session file without mux ids, and a saved id the daemon doesn't know: tabs start fresh as before (Task 8 `window_state` tests + `NotFound` path).

---

### Task 1: Shared activity status

**Files:**
- Create: `crates/tern-term/src/status.rs`
- Modify: `crates/tern-term/src/lib.rs`, `crates/tern-app/src/terminal_view.rs:55-61, 428-452`

**Interfaces:**
- Produces: `tern_term::status::{ActivityStatus, next_status(prev: ActivityStatus, sig: &OscSignal) -> Option<ActivityStatus>}` (serde Serialize/Deserialize); `ActivityStatus` re-exported where the app used its own.

- [ ] Write tests: `working` → Working; `blocked` → Blocked; `clear` after Working → Done; `clear` after Blocked → None; `idle` → Done; Notify/Probe → None (no change).
- [ ] Move the enum and the mapping from `TerminalView::on_term_event`/`set_status` into `next_status`; the app calls it.
- [ ] `cargo test -p tern-term status` passes; `cargo build -p tern-app`.
- [ ] Commit "Share the activity status machine in tern-term".

### Task 2: Term snapshot capture/restore

**Files:**
- Create: `crates/tern-term/src/snapshot.rs`
- Modify: `crates/tern-term/Cargo.toml` (serde, rmp-serde dev for size test), `lib.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Serialize, Deserialize, Clone)]
  pub struct Snapshot {
      pub size: TermSize,
      pub grid: Grid<Cell>,              // active grid
      pub primary: Option<Grid<Cell>>,   // set when alt screen is active
      pub mode: u32,                     // TermMode bits
      pub cursor_style: Option<(u8, bool)>, // (shape as DECSCUSR param, blinking)
      pub colors: Vec<(usize, [u8; 3])>, // overridden palette entries
      pub title: Option<String>,
      pub status: ActivityStatus,
  }
  pub fn capture<L: EventListener>(term: &mut Term<L>, size: TermSize, title: Option<String>, status: ActivityStatus) -> Snapshot;
  pub fn restore<L: EventListener>(snap: &Snapshot, term: &mut Term<L>, parser: &mut vte::ansi::Processor);
  ```
  `restore` expects `term` freshly built at `snap.size`.

- [ ] Tests (helper `fn feed(term, bytes)` via `Processor::advance`):
  - `plain_round_trip`: 300 lines of `line N\r\n` into a 20×80 term with 1000 scrollback → capture → restore into a fresh term → same visible text, same `history_size`, same cursor point.
  - `alt_screen_round_trip`: shell text, `\x1b[?1049h`, alt text → round trip → restored term has ALT_SCREEN, alt text visible; feeding `\x1b[?1049l` shows the shell text.
  - `modes_round_trip`: `\x1b[?2004h\x1b[?1006h\x1b[?1002h\x1b[?1004h\x1b[?1h\x1b=\x1b[=5;1u` → restored `mode()` equals source `mode()`.
  - `colors_and_title`: `\x1b]4;1;rgb:12/34/56\x07` and title "t" → restored `colors()[1]` and snapshot title.
  - `capture_keeps_source_intact`: capture with alt screen active leaves source text/mode unchanged.
- [ ] Implement capture: alt → clone alt grid, `swap_alt()`, clone primary, `swap_alt()`, `*grid_mut() = alt_clone`; history capped by the term's config. Restore: primary first (or the only grid), `CSI ?1049h` via the parser, alt grid; then mode sequences (DECSET/DECRST list: 1 APP_CURSOR, 6 ORIGIN, 7 LINE_WRAP, 25 SHOW_CURSOR, 1000/1002/1003/1005/1006, 1004, 1007 ALTERNATE_SCROLL, 1042 URGENCY_HINTS, 2004; `ESC =`; `CSI 4 h` INSERT; `CSI 20 h` LNM; kitty flags `CSI = n ; 1 u`), DECSCUSR, OSC 4.
- [ ] Tests pass; commit "Snapshot and restore a terminal's state".

### Task 3: Mirror terminal

**Files:**
- Create: `crates/tern-term/src/mirror.rs`
- Modify: `lib.rs`, `session.rs` (`TermEvent` gains variants)

**Interfaces:**
- Produces:
  ```rust
  pub enum Marker { Resized(TermSize), SyncFlush, Exited(Option<i32>) }   // serde
  pub struct Mirror { /* term: Arc<FairMutex<Term<MirrorListener>>>, parser, scanner, size */ }
  impl Mirror {
      pub fn new(size: TermSize, scrollback: usize, palette: Palette, sink: EventSink) -> Self;
      pub fn term(&self) -> &Arc<FairMutex<Term<MirrorListener>>>;
      pub fn feed(&mut self, bytes: &[u8]);          // parse + OSC scan (probe ignored) + Wakeup
      pub fn marker(&mut self, m: &Marker);          // resize mirror / stop_sync / Exit event
      pub fn install(&mut self, snap: &Snapshot);    // rebuild term from a snapshot
      pub fn size(&self) -> TermSize;
  }
  ```
  `MirrorListener` forwards `Title`, `ResetTitle`, `Bell`, `ClipboardStore` (≤16 MiB) and coalesced `Wakeup` to the sink; drops `PtyWrite`, `ColorRequest`, `TextAreaSizeRequest`, `ClipboardLoad`. `TermEvent` keeps its variants and gains `Ready { id: u64, token: Option<String>, status: ActivityStatus }`, `SpawnError(String)`, `NotFound`, `ServerLost`.
  Wakeup coalescing (`take_wakeup`) moves to a shared `Arc<AtomicBool>` exposed by the mirror.

- [ ] Tests: feeding bytes produces the same visible text as a plain Term; a DA query (`\x1b[c`) produces no outgoing write (listener records nothing); `Marker::Resized` changes `term.columns()`; `install` after a snapshot shows the snapshot text and emits Wakeup.
- [ ] Commit "Parse-only mirror terminal".

### Task 4: tern-mux protocol

**Files:**
- Create: `crates/tern-mux/{Cargo.toml, src/lib.rs, src/proto.rs, src/fdpass.rs}`

**Interfaces:**
- Produces:
  ```rust
  pub const MAJOR: u32 = 1; pub const MINOR: u32 = 0;
  #[derive(Serialize, Deserialize)] pub enum ClientMsg {
      Hello { major: u32, minor: u32, app_version: String },
      Spawn { req: u64, cfg: SpawnCfg, hook: bool, size: TermSize },
      Attach { req: u64, id: u64, size: TermSize },
      Detach { id: u64 }, Input { id: u64, bytes: Vec<u8> }, Resize { id: u64, size: TermSize },
      Respawn { id: u64, cfg: SpawnCfg }, Close { id: u64 }, List { req: u64 },
  }
  #[derive(Serialize, Deserialize)] pub enum ServerMsg {
      Hello { major: u32, minor: u32, app_version: String, pid: u32 },
      Attached { req: u64, id: u64, token: Option<String>, snapshot: Box<Snapshot> }, // + fd via SCM_RIGHTS
      NotFound { req: u64, id: u64 }, Error { req: u64, message: String },
      Output { id: u64, bytes: Vec<u8> }, Marker { id: u64, marker: Marker },
      Hook { token: String, body: Vec<u8> }, Detached { id: u64 },
      Sessions { req: u64, list: Vec<SessionInfo> },
  }
  pub struct SessionInfo { id: u64, pid: u32, exited: bool, cwd: Option<PathBuf>, title: Option<String> }
  pub struct SpawnCfg { program: Option<String>, args: Vec<String>, cwd: Option<PathBuf>, env: HashMap<String,String>, scrollback: usize }
  pub fn write_frame(w: &mut impl Write, msg: &impl Serialize) -> io::Result<()>;
  pub fn read_frame<T: DeserializeOwned>(r: &mut impl Read, max: usize) -> io::Result<Option<T>>; // Ok(None) = unknown variant, skipped
  // fdpass.rs
  pub fn send_with_fd(sock: &UnixStream, bytes: &[u8], fd: RawFd) -> io::Result<()>;
  pub fn recv_fd(sock: &UnixStream) -> io::Result<Option<OwnedFd>>; // reads one 1-byte message carrying an fd
  ```
  Fd passing rule: right before an `Attached` frame the server sends a single byte `b'F'` with the fd as ancillary data (or `b'N'` with none); the client reader, on reading an `Attached`, first... — simpler and race-free: the reader thread always reads frames with `recvmsg`, collecting any fd that arrives into a queue; `Attached` takes the next queued fd. Implement `FrameReader` over the socket using `recvmsg` into a buffer + fd queue.
- [ ] Tests: round-trip every message; oversize frame → `InvalidData`; an unknown variant (hand-built msgpack map `{"Future": 1}`) → `Ok(None)` and the next frame still reads; fd passing over `UnixStream::pair()` (pass a pipe fd, write on the received fd, read on the original).
- [ ] Commit "tern-mux: wire protocol and fd passing".

### Task 5: Host (daemon-side PTY driver)

**Files:**
- Create: `crates/tern-mux/src/host.rs`

**Interfaces:**
- Produces:
  ```rust
  pub trait Subscriber: Send { fn output(&self, id: u64, bytes: &[u8]); fn marker(&self, id: u64, m: &Marker); }
  pub struct Host { id: u64, /* term, tx, poller, pid, master fd, status, title, exited */ }
  impl Host {
      pub fn spawn(id: u64, cfg: &SpawnCfg, size: TermSize, palette: Palette, events: HostEvents) -> io::Result<Arc<Host>>;
      pub fn attach(&self, sub: Arc<dyn Subscriber>) -> Snapshot;   // under the Term lock
      pub fn detach(&self);
      pub fn input(&self, bytes: Vec<u8>);
      pub fn resize(&self, size: TermSize);
      pub fn jiggle(&self, size: TermSize);    // rows+1, then size after 50 ms
      pub fn master_fd(&self) -> RawFd; pub fn pid(&self) -> u32;
      pub fn info(&self) -> SessionInfo;
      pub fn kill(&self);                       // SIGHUP + stop thread
  }
  pub struct HostEvents { pub status: Box<dyn Fn(u64, ActivityStatus, ActivityStatus) + Send + Sync>, pub notify: Box<dyn Fn(u64, Option<String>, String) + Send + Sync> }
  ```
  Loop: `polling::Poller`, `tty::new` + `EventedReadWrite::register`, read 64 KiB → lock term → `parser.advance` → OSC scan (probe → write reply, rate limited 500 ms as today) → `sub.output` → unlock; writes drained from an mpsc queue (woken by `poller.notify()`); `PTY_CHILD_EVENT_TOKEN` → drain read, `Marker::Exited`; sync timeout → `stop_sync` + `Marker::SyncFlush`; resize → `pty.on_resize` + `term.resize` + `Marker::Resized` all under the lock. Term listener: `PtyWrite`/`ColorRequest`/`TextAreaSizeRequest` → input queue; `Title` stored; others ignored.
- [ ] Tests: `sh -c 'printf hello; read x; printf "got:$x"'` → subscriber sees `hello`; `input(b"abc\n")` → sees `got:abc`; then `Marker::Exited(Some(0))`. `resize` → `Marker::Resized` with the new size and `stty size` inside reports it. Attach mid-stream: run `sh -c 'i=0; while [ $i -lt 20000 ]; do echo line$i; i=$((i+1)); done'`, attach after ~5 ms with a Mirror subscriber, wait for Exited → mirror's last visible lines equal the host term's.
- [ ] Commit "tern-mux: PTY host with ordered snapshot + stream".

### Task 6: Server and client

**Files:**
- Create: `crates/tern-mux/src/{paths.rs, server.rs, client.rs, notify.rs}`

**Interfaces:**
- Produces:
  ```rust
  // paths.rs
  pub fn mux_dir() -> PathBuf; pub fn socket_path(dir: &Path) -> PathBuf; pub fn ensure_private_dir(dir: &Path) -> io::Result<()>;
  pub fn peer_uid_ok(s: &UnixStream) -> bool;
  // server.rs
  pub fn run() -> !;                              // `tern --server`
  pub fn serve(dir: &Path, opts: ServeOpts) -> io::Result<()>; // testable; returns on idle exit
  pub struct ServeOpts { pub idle_exit: Duration, pub notify: bool }
  // client.rs
  pub struct MuxClient;  // Arc, global per process
  impl MuxClient {
      pub fn connect(dir: &Path, spawn_daemon: bool, hooks: HookSink) -> io::Result<Arc<Self>>;
      pub fn open(self: &Arc<Self>, open: Open, size: TermSize, scrollback: usize, sink: EventSink) -> RemoteSession;
      pub fn list(&self) -> io::Result<Vec<SessionInfo>>;   // blocking, 2 s timeout
  }
  pub enum Open { Spawn { cfg: SpawnCfg, hook: bool }, Attach { id: u64 } }
  pub struct RemoteSession;  // same surface the UI used on tern_term::Session
  impl RemoteSession {
      pub fn term(&self) -> &Arc<FairMutex<Term<MirrorListener>>>; pub fn take_wakeup(&self) -> bool;
      pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>); pub fn resize(&self, size: TermSize); pub fn size(&self) -> TermSize;
      pub fn id(&self) -> Option<u64>; pub fn foreground_programs(&self) -> Vec<String>; pub fn foreground_cwd(&self) -> Option<PathBuf>;
      pub fn respawn(&self, cfg: SpawnCfg); pub fn close(self);   // Drop = detach
  }
  ```
  Server: `flock(server.lock)`, unlink stale socket after refused connect, bind, accept loop thread; per connection a reader thread + writer thread (bounded `sync_channel` sized by bytes: drop connection when > 64 MiB queued); `HookServer` started once, tokens per host; settings files in `run/<pid>`; `{tern-settings}` substitution; idle timer; SIGTERM handler via `signal-hook`-free self-pipe (libc `sigaction` writing to a pipe polled by the accept loop). Notifier called by host status/notify when no client is connected.
  Client: reader thread routes `Output`/`Marker`/`Attached` to each session's `Mirror` (locks term only to feed), `Hook` to the hook sink; on EOF every session gets `TermEvent::ServerLost` + `Exit(None)` and the client marks itself dead (next `connect` makes a new one).
- [ ] Integration tests (`crates/tern-mux/tests/daemon.rs`, temp dir, `serve` on a thread, `spawn_daemon: false`): spawn/attach/output/detach/reattach with same pid and earlier output in the snapshot; `Close` kills (pid gone within 1 s); `NotFound`; `list`; dir mode 0777 refused; fd passing: `foreground_programs()` contains `sh`; `slow_client_is_dropped` (raw client that never reads + `yes` output → server closes it, host keeps running); `server_loss_exits_sessions` (drop server thread's listener / kill connection → sink gets ServerLost); idle exit after the configured duration.
- [ ] Commit "tern-mux: daemon server and client".

### Task 7: Daemon process plumbing

**Files:**
- Modify: `crates/tern-mux/src/{client.rs, notify.rs}`, `crates/tern-app/src/main.rs`, `crates/tern-app/Cargo.toml`

- [ ] `client::spawn_daemon()`: `Command::new(current_exe()).arg("--server")`, stdin null, stdout/stderr appended to `~/Library/Logs/Tern/daemon.log`, `pre_exec(setsid)`; `connect` retries with backoff (10 ms doubling, 3 s total).
- [ ] `main()`: `if std::env::args().nth(1).as_deref() == Some("--server") { tern_mux::server::run() }` before `lock_instance`; env_logger to the log file.
- [ ] `notify.rs` (macOS): guarded `UNUserNotificationCenter` post (no delegate), request authorization once; no-op when `bundleIdentifier` is nil.
- [ ] Manual: `cargo run -p tern-app -- --server` in one shell creates the socket; `lsof -U | grep mux-dev`.
- [ ] Commit "Run the daemon from the tern binary".

### Task 8: UI on the daemon

**Files:**
- Modify: `crates/tern-app/src/{terminal_view.rs, workspace.rs, window_state.rs, main_window.rs, main.rs, control.rs}`. Keep `crates/tern-term/src/session.rs` (in-process sessions; the Windows/Linux port on branch `windows` builds on it).

- [ ] `window_state`: `SavedTab::Claude { name, session, mux: Option<u64> }`, `Shell { name, mux }`; encode as an extra trailing field; tests: old lines decode with `mux: None`; round trip with ids.
- [ ] `Services` holds `mux: Option<Arc<MuxClient>>` (connect at startup; on failure tabs show the error), hook events from the client's hook sink; drop the in-process `HookServer` and `run_dir` sweeping of settings files (the daemon owns them).
- [ ] `TerminalView`: `session: Option<RemoteSession>`; `start()` opens `Open::Attach{id}` when the tab has a mux id and hasn't started yet, else `Open::Spawn`; `TermEvent::Ready{id, token, status}` stores id/token (emit `TerminalEvent::Ready`) and sets status without notifying; `NotFound` → spawn fresh (`Open::Spawn` with the tab's cold config: Claude `--resume <conv>`); `ServerLost` → exited with text `[background server stopped]`; restart (Enter) reconnects.
- [ ] `workspace.rs`: `spawn_config` builds `claude … --settings {tern-settings}` and `hook: true` for Claude tabs; `TermTab.token` filled from `Ready`; `close_tab` and window close (not quitting) call `close()`; `Drop` no longer unregisters hooks; save mux ids in `SavedTab`.
- [ ] `main.rs`: quit no longer asks about running programs (they keep running); `confirm_quit` only for closing windows (existing close-window confirmation path unchanged).
- [ ] `main_window::restore_session`: after restoring, `mux.list()`; live sessions not referenced by any restored tab open as shell-kind tabs in the first window (attach by id); exited orphans get `Close`.
- [ ] `cargo test --workspace`, clippy, fmt.
- [ ] Manual with `scripts/dev.sh` + control socket: open Claude/shell tabs, run `top`, quit (control `quit`), relaunch: same pids (`pgrep`), screen restored, `top` correct size; close tab → process gone; `kill -9` the daemon → tabs show stopped, Enter restarts.
- [ ] Commit "Tabs run in the background daemon".

### Task 9: Docs

- [ ] README: "Background sessions" section (tabs keep running after quit; daemon log path; `pkill -f 'tern --server'` stops everything).
- [ ] Commit "README: background sessions".
