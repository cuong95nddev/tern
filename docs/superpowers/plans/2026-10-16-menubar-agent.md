# Menu bar agent Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `tern --menubar` agent shows every terminal the background daemon runs in the macOS menu bar (also while Tern is closed), and opens or ends any of them.

**Architecture:** A stateless AppKit process (same binary, like `--server`) connects to the daemon as an *observer*: it gets pushed session lists (`Watch`) and asks the daemon to forward `Reveal { id }` to the Tern UI. When no UI is connected it opens `tern://session/<id>` (release) or runs `tern --open-session <id>` (debug); the UI then focuses the tab showing that session or adopts it into the active workspace.

**Tech Stack:** Rust 2024; `tern-mux` (protocol, daemon, clients); new crate `tern-menubar` (objc2 0.6, objc2-app-kit 0.3.2, objc2-foundation 0.3.2, dispatch2 0.3, semver 1, libc); GPUI app unchanged except wiring.

**Spec:** `docs/superpowers/specs/2026-10-16-menubar-agent-design.md`

## Global Constraints

- Branch `background-daemon` in `~/ws/tern-ws/tern`. Commit as `cuong95nddev <cuong95nd@gmail.com>` (repo-local config already set). No merge, push or tag.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets` (no warnings) and `cargo test --workspace` green after every task.
- Protocol stays `MAJOR = 1`; this work is `MINOR = 1`. New fields carry `#[serde(default)]`; new message kinds are skipped by older peers (`proto::decode` already does this).
- An observer never starts the daemon, never keeps it from idling out (10 s), never stops "unattended" notifications.
- Session list pushes: at most one per 250 ms, only when the list changed. Agent retries a lost/refused connection every 2 s; an old (MINOR 0) daemon is polled with `List` every 2 s.
- Agent lock: `~/Library/Caches/dev.tern/menubar.lock` (`menubar-dev.lock` in debug builds), contents `<pid> <version>`; newer agent SIGTERMs an older holder and waits ≤ 2 s; equal or newer holder → the new one exits.
- Setting file: `~/Library/Application Support/dev.tern/menubar` (next to `claude-args`); content `off` = hidden; missing = shown.
- Agent log: `~/Library/Logs/Tern/menubar.log` (`menubar-dev.log` in debug).
- URL scheme `tern`, form `tern://session/<decimal id>`.
- Code style: match the surrounding code — short doc comments that say *why*, `log::` for diagnostics, no `unwrap()` on I/O in non-test code.

## Review Focus

1. **Agent connected, daemon has nothing to do:** the daemon must still idle out after 10 s, and the agent must notice (empty state) and pick up the next daemon when Tern starts one. → Task 2 test `observer_does_not_keep_the_daemon_alive`; Task 5 test `link_reports_loss_and_reconnects`.
2. **The session ended between the menu showing it and the click:** Open must not create an empty/broken tab. → Task 6 test `reveal_target_unknown_session_does_nothing`.
3. **Tern restarted (old UI connection gone, new one connected):** `Reveal` must reach the live UI, never a dead connection, and report `delivered: false` with none. → Task 2 test `reveal_goes_to_the_live_ui`.
4. **Sessions stored in a `HashMap`:** list order must not flap, or the agent gets a push every 250 ms and rebuilds its menu forever. → Task 2 test `watch_pushes_only_changes`.
5. **Garbage in the URL / arguments** (`tern://session/abc`, `tern://session/1/x`, negative, overflow): ignored, no panic. → Task 6 test `parses_session_urls`.

---

### Task 1: Protocol MINOR 1

**Files:**
- Modify: `crates/tern-mux/src/proto.rs`
- Modify: `crates/tern-mux/src/host.rs:272-280` (`Host::info`)
- Modify: `crates/tern-mux/src/client.rs:145` (Hello gets `role`), `crates/tern-mux/tests/daemon.rs` (raw Hello sites)

**Interfaces:**
- Produces (all in `tern_mux::proto`):
  ```rust
  pub const MINOR: u32 = 1;
  #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
  pub enum Role { #[default] Ui, Observer }
  ClientMsg::Hello { major: u32, minor: u32, app_version: String, #[serde(default)] role: Role }
  ClientMsg::Watch
  ClientMsg::Reveal { req: u64, id: u64 }
  ServerMsg::SessionsChanged { list: Vec<SessionInfo> }
  ServerMsg::Reveal { id: u64 }
  ServerMsg::Revealed { req: u64, delivered: bool }
  #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
  pub struct SessionInfo {
      pub id: u64, pub pid: u32, pub exited: bool, pub cwd: Option<PathBuf>, pub title: Option<String>,
      #[serde(default)] pub status: ActivityStatus,
      /// Some UI connection shows it.
      #[serde(default)] pub attached: bool,
      /// The first foreground program (for a label when there is no title).
      #[serde(default)] pub program: Option<String>,
      /// Spawned with a hook token (a Claude tab).
      #[serde(default)] pub claude: bool,
      /// `session_id` of its latest Claude hook event.
      #[serde(default)] pub claude_session: Option<String>,
  }
  ```
- `Host::info()` fills `status`, `attached`, `program`; `claude`/`claude_session` stay default (the server fills them in Task 2).

- [ ] **Step 1: Write the failing tests** (append to `proto.rs` `mod tests`)

```rust
    #[test]
    fn minor_1_messages_round_trip() {
        let msgs = vec![
            ClientMsg::Hello { major: MAJOR, minor: MINOR, app_version: "0.1.2".into(), role: Role::Observer },
            ClientMsg::Watch,
            ClientMsg::Reveal { req: 3, id: 9 },
        ];
        let mut buf = vec![];
        for m in &msgs {
            write_frame(&mut buf, m).unwrap();
        }
        let mut r = Cursor::new(buf);
        for m in &msgs {
            assert_eq!(&read_frame::<ClientMsg>(&mut r, MAX_CLIENT_FRAME).unwrap().unwrap(), m);
        }
        let info = SessionInfo { id: 1, status: ActivityStatus::Blocked, attached: true, claude: true, claude_session: Some("s".into()), ..Default::default() };
        let mut buf = vec![];
        write_frame(&mut buf, &ServerMsg::SessionsChanged { list: vec![info.clone()] }).unwrap();
        write_frame(&mut buf, &ServerMsg::Revealed { req: 3, delivered: true }).unwrap();
        let mut r = Cursor::new(buf);
        assert!(matches!(read_frame(&mut r, MAX_SERVER_FRAME).unwrap(), Some(ServerMsg::SessionsChanged { list }) if list == vec![info]));
        assert!(matches!(read_frame(&mut r, MAX_SERVER_FRAME).unwrap(), Some(ServerMsg::Revealed { req: 3, delivered: true })));
    }

    /// What a MINOR 0 peer sent and parsed.
    mod v0 {
        use serde::{Deserialize, Serialize};
        use std::path::PathBuf;
        #[derive(Serialize)]
        pub enum ClientMsg {
            Hello { major: u32, minor: u32, app_version: String },
        }
        #[derive(Serialize, Deserialize, Debug)]
        pub struct SessionInfo {
            pub id: u64,
            pub pid: u32,
            pub exited: bool,
            pub cwd: Option<PathBuf>,
            pub title: Option<String>,
        }
        #[derive(Deserialize, Debug)]
        pub enum ServerMsg {
            Sessions { req: u64, list: Vec<SessionInfo> },
        }
        /// `ServerMsg::Sessions` as a MINOR 0 daemon encoded it.
        #[derive(Serialize)]
        pub enum ServerMsgOut {
            Sessions { req: u64, list: Vec<SessionInfo> },
        }
    }

    #[test]
    fn minor_0_peers_still_talk() {
        // An old UI's Hello reads as a UI.
        let mut buf = vec![];
        write_frame(&mut buf, &v0::ClientMsg::Hello { major: 1, minor: 0, app_version: "0.1.1".into() }).unwrap();
        let got = read_frame::<ClientMsg>(&mut Cursor::new(buf), MAX_CLIENT_FRAME).unwrap().unwrap();
        assert!(matches!(got, ClientMsg::Hello { role: Role::Ui, .. }));
        // An old daemon's list reads with defaults.
        let mut buf = vec![];
        let old = v0::SessionInfo { id: 4, pid: 5, exited: false, cwd: None, title: Some("t".into()) };
        write_frame(&mut buf, &v0::ServerMsgOut::Sessions { req: 1, list: vec![old] }).unwrap();
        let got = read_frame::<ServerMsg>(&mut Cursor::new(buf), MAX_SERVER_FRAME).unwrap().unwrap();
        let ServerMsg::Sessions { list, .. } = got else { panic!("{got:?}") };
        assert_eq!(list[0].id, 4);
        assert_eq!(list[0].status, ActivityStatus::None);
        assert!(!list[0].claude);
        // A new list reads on an old peer (extra fields ignored).
        let mut buf = vec![];
        write_frame(&mut buf, &ServerMsg::Sessions { req: 2, list: vec![SessionInfo { id: 6, claude: true, ..Default::default() }] }).unwrap();
        let got = read_frame::<v0::ServerMsg>(&mut Cursor::new(buf), MAX_SERVER_FRAME).unwrap().unwrap();
        let v0::ServerMsg::Sessions { list, .. } = got;
        assert_eq!(list[0].id, 6);
        // New kinds are skipped by an old peer.
        let (a, b) = UnixStream::pair().unwrap();
        write_frame(&mut &a, &ServerMsg::Reveal { id: 1 }).unwrap();
        write_frame(&mut &a, &ServerMsg::Sessions { req: 3, list: vec![] }).unwrap();
        let mut r = FrameReader::new(b, MAX_SERVER_FRAME);
        assert!(matches!(r.read::<v0::ServerMsg>().unwrap(), Some(v0::ServerMsg::Sessions { req: 3, .. })));
    }

```

  Also add `use tern_term::status::ActivityStatus;` to the test module.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p tern-mux --lib proto`
Expected: compile errors (`Role`, `Watch`, `Reveal`, new `SessionInfo` fields missing).

- [ ] **Step 3: Implement**

  - In `proto.rs`: bump `MINOR` to `1`; add `Role`; add `#[serde(default)] role: Role` to `ClientMsg::Hello`; add `Watch` (doc: "From now on send `SessionsChanged` whenever the list changes (and once now)") and `Reveal { req, id }` (doc: "Ask the UI to show session `id`; answered by `Revealed`"); add `ServerMsg::SessionsChanged`, `ServerMsg::Reveal` (doc: "Show session `id` (asked by an observer)"), `ServerMsg::Revealed` (doc: "`delivered`: a UI got the `Reveal`"); extend `SessionInfo` as in Interfaces (add `Default` to its derives; `use tern_term::status::ActivityStatus;`). Update the module doc's first paragraph: "The first message each way is `Hello`; a client says whether it is a UI or an observer (the menu bar agent)."
  - Update the existing `client_messages_round_trip` Hello to include `role: Role::Ui`.
  - `host.rs` `Host::info`:
    ```rust
    pub fn info(&self) -> SessionInfo {
        SessionInfo {
            id: self.id,
            pid: self.pid,
            exited: self.exited().is_some(),
            cwd: self.cwd(),
            title: self.shared.title.lock().unwrap().clone(),
            status: self.status(),
            attached: self.attached(),
            program: self.foreground_programs().into_iter().next(),
            ..Default::default()
        }
    }
    ```
  - `client.rs:145`: `ClientMsg::Hello { major: MAJOR, minor: MINOR, app_version: env!("CARGO_PKG_VERSION").into(), role: Role::Ui }` (import `Role`).
  - `tests/daemon.rs`: every raw `ClientMsg::Hello { .. }` gets `role: proto::Role::Ui`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p tern-mux`
Expected: all pass (existing + 2 new).

- [ ] **Step 5: Commit**

```bash
git add crates/tern-mux
git commit -m "Protocol 1.1: observer role, Watch, Reveal, richer SessionInfo"
```

---

### Task 2: Daemon serves observers; UI gets `Reveal`

**Files:**
- Modify: `crates/tern-mux/src/server.rs`
- Modify: `crates/tern-mux/src/client.rs` (`RevealSink`, `MuxClient::connect`, `Connector::new`, `read_loop`)
- Modify: `crates/tern-app/src/workspace.rs:3326-3335` (`services()` passes a reveal sink; full wiring in Task 6)
- Test: `crates/tern-mux/tests/daemon.rs`

**Interfaces:**
- Consumes: Task 1 protocol.
- Produces:
  - `tern_mux::client::RevealSink = Arc<dyn Fn(u64) + Send + Sync>`
  - `MuxClient::connect(dir: &Path, spawn_daemon: bool, hooks: HookSink, reveal: RevealSink) -> io::Result<MuxClient>`
  - `Connector::new(dir: PathBuf, spawn_daemon: bool, hooks: HookSink, reveal: RevealSink) -> Connector`
  - Daemon behavior: `List` and `SessionsChanged` lists are sorted by `id`, carry `claude` (= has a hook token) and `claude_session`; observers (`Role::Observer`) don't count in `clients`; `Reveal` goes to the most recently connected live UI connection.
  - `pub const WATCH_INTERVAL: Duration = Duration::from_millis(250);` in `server.rs`.

- [ ] **Step 1: Update test helpers, write the failing tests** (`tests/daemon.rs`)

  Helpers (replace `client`, add `observer`):
  ```rust
  fn client(dir: &Path) -> MuxClient {
      MuxClient::connect(dir, false, Arc::new(|_, _| {}), Arc::new(|_| {})).unwrap()
  }

  /// A raw observer connection: Hello as an observer, read the daemon's Hello.
  fn observer(dir: &Path) -> (UnixStream, proto::FrameReader) {
      let mut raw = UnixStream::connect(paths::socket_path(dir)).unwrap();
      proto::write_frame(&mut raw, &ClientMsg::Hello { major: MAJOR, minor: MINOR, app_version: "t".into(), role: proto::Role::Observer }).unwrap();
      let mut r = proto::FrameReader::new(raw.try_clone().unwrap(), proto::MAX_SERVER_FRAME);
      assert!(matches!(r.read::<ServerMsg>().unwrap(), Some(ServerMsg::Hello { minor: 1, .. })));
      raw.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
      (raw, r)
  }

  /// The next `SessionsChanged` list.
  fn next_list(r: &mut proto::FrameReader) -> Vec<proto::SessionInfo> {
      loop {
          match r.read::<ServerMsg>().unwrap() {
              Some(ServerMsg::SessionsChanged { list }) => return list,
              Some(_) => continue,
              None => panic!("connection ended"),
          }
      }
  }
  ```
  Also update the two `Connector::new(..., Arc::new(|_, _| {}))` calls to add `, Arc::new(|_| {})`, and import `ServerMsg`.

  Tests:
  ```rust
  #[test]
  fn observer_does_not_keep_the_daemon_alive() {
      let mut d = daemon(Duration::from_millis(300));
      let (_raw, _r) = observer(&d.dir);
      let t = d.thread.take().unwrap();
      wait_until("idle exit with an observer connected", || t.is_finished());
      t.join().unwrap().unwrap();
  }

  #[test]
  fn watch_gets_the_list_and_its_changes() {
      let d = daemon(Duration::from_secs(60));
      let (mut raw, mut r) = observer(&d.dir);
      proto::write_frame(&mut raw, &ClientMsg::Watch).unwrap();
      assert!(next_list(&mut r).is_empty());
      let c = client(&d.dir);
      let (sink, ev) = sink();
      let s = c.open(Open::Spawn { cfg: sh("read x; exit 3"), hook: true }, SIZE, 1000, sink);
      wait_until("ready", || ready_id(&ev).is_some());
      let id = s.id().unwrap();
      let list = loop {
          let l = next_list(&mut r);
          if l.iter().any(|i| i.id == id && i.attached) {
              break l;
          }
      };
      assert!(list[0].claude, "spawned with a hook token");
      s.write(b"\n".to_vec());
      while !next_list(&mut r).iter().any(|i| i.id == id && i.exited) {}
      drop(s); // detach
      while next_list(&mut r).iter().any(|i| i.id == id && i.attached) {}
      c.close(id);
      while !next_list(&mut r).is_empty() {}
  }

  #[test]
  fn watch_pushes_only_changes() {
      let d = daemon(Duration::from_secs(60));
      let c = client(&d.dir);
      let mut held = vec![];
      for _ in 0..5 {
          let (sink, ev) = sink();
          held.push((c.open(Open::Spawn { cfg: sh("read x"), hook: false }, SIZE, 1000, sink), ev));
      }
      for (_, ev) in &held {
          wait_until("ready", || ready_id(ev).is_some());
      }
      let (mut raw, mut r) = observer(&d.dir);
      proto::write_frame(&mut raw, &ClientMsg::Watch).unwrap();
      let first = next_list(&mut r);
      assert_eq!(first.len(), 5);
      assert!(first.windows(2).all(|w| w[0].id < w[1].id), "sorted by id");
      // Nothing changes for > 4 intervals: no push.
      raw.set_read_timeout(Some(Duration::from_millis(1200))).unwrap();
      let err = r.read::<ServerMsg>().unwrap_err();
      assert!(matches!(err.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut), "{err}");
      for (s, _) in held {
          s.close();
      }
  }

  #[test]
  fn reveal_goes_to_the_live_ui() {
      let d = daemon(Duration::from_secs(60));
      let (mut raw, mut r) = observer(&d.dir);
      let revealed = |r: &mut proto::FrameReader| loop {
          if let Some(ServerMsg::Revealed { req: 1, delivered }) = r.read::<ServerMsg>().unwrap() {
              return delivered;
          }
      };
      // No UI.
      proto::write_frame(&mut raw, &ClientMsg::Reveal { req: 1, id: 42 }).unwrap();
      assert!(!revealed(&mut r));
      // An old UI, gone; a new one, connected.
      let old = client(&d.dir);
      drop(old);
      let got = Arc::new(Mutex::new(vec![]));
      let g = got.clone();
      let new = MuxClient::connect(&d.dir, false, Arc::new(|_, _| {}), Arc::new(move |id| g.lock().unwrap().push(id))).unwrap();
      std::thread::sleep(Duration::from_millis(100)); // the daemon saw the old one go
      proto::write_frame(&mut raw, &ClientMsg::Reveal { req: 1, id: 42 }).unwrap();
      assert!(revealed(&mut r));
      wait_until("reveal reached the UI", || *got.lock().unwrap() == [42]);
      drop(new);
      std::thread::sleep(Duration::from_millis(100));
      proto::write_frame(&mut raw, &ClientMsg::Reveal { req: 1, id: 42 }).unwrap();
      assert!(!revealed(&mut r));
  }

  #[test]
  fn claude_session_comes_from_hooks() {
      let d = daemon(Duration::from_secs(60));
      let c = client(&d.dir);
      let (sink, ev) = sink();
      // Post a hook like Claude Code does, using the URL in the settings file.
      let script = r#"url=$(sed -n 's/.*"url": *"\([^"]*\)".*/\1/p' {tern-settings} | head -1); curl -s -X POST -H 'content-type: application/json' -d '{"hook_event_name":"Stop","session_id":"abc-123"}' "$url" >/dev/null; read x"#;
      let s = c.open(Open::Spawn { cfg: sh(script), hook: true }, SIZE, 1000, sink);
      wait_until("ready", || ready_id(&ev).is_some());
      let id = s.id().unwrap();
      wait_until("claude_session", || {
          c.list().unwrap().iter().any(|i| i.id == id && i.claude && i.claude_session.as_deref() == Some("abc-123"))
      });
      s.close();
  }
  ```
  Before writing `claude_session_comes_from_hooks`, open `crates/tern-bridge/src` (`write_claude_settings`, `HookServer`) and check how the settings file names the hook URL and which JSON fields `HookEvent::parse` needs; adjust the `sed`/JSON so the request is accepted. If the hook server needs the token in the path/header, it is already in the URL from the settings file.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p tern-mux --test daemon`
Expected: compile error (`connect` arity), then after fixing helpers: the new tests fail (`Revealed` never comes, observer keeps daemon alive, no `SessionsChanged`).

- [ ] **Step 3: Implement the daemon side** (`server.rs`)

  - `use crate::proto::Role;` and `pub const WATCH_INTERVAL: Duration = Duration::from_millis(250);`
  - `Session` gains `claude_session: Option<String>` (init `None` in `spawn`).
  - `State` gains `uis: Vec<Weak<Conn>>` (doc: "UI connections, oldest first (`Reveal` goes to the newest)"); `clients` doc: "UI connections (observers don't count)". Init `uis: Vec::new()`.
  - `Conn` gains `watching: AtomicBool` (init `false`).
  - `handle`: take the role from Hello, count only UIs:
    ```rust
    let role = match reader.read::<ClientMsg>()? {
        Some(ClientMsg::Hello { major, role, .. }) if major == MAJOR => role,
        other => { /* unchanged */ }
    };
    if role == Role::Ui {
        let mut st = self.state.lock().unwrap();
        st.clients += 1;
        st.uis.push(Arc::downgrade(&conn));
        Self::update_idle(&mut st);
    }
    ```
    and at the end:
    ```rust
    let mut st = self.state.lock().unwrap();
    if role == Role::Ui {
        st.clients -= 1;
        st.uis.retain(|w| w.upgrade().is_some_and(|c| !Arc::ptr_eq(&c, &conn)));
    }
    // (the owner loop stays as is)
    ```
  - `list()`:
    ```rust
    /// Every session, by id (a stable order: watchers compare lists).
    /// Built outside the state lock: `info` asks the OS about processes.
    fn list(&self) -> Vec<SessionInfo> {
        let sessions: Vec<_> = self.state.lock().unwrap().sessions.values().map(|s| (s.host.clone(), s.token.is_some(), s.claude_session.clone())).collect();
        let mut list: Vec<_> = sessions.into_iter().map(|(h, claude, claude_session)| SessionInfo { claude, claude_session, ..h.info() }).collect();
        list.sort_by_key(|i| i.id);
        list
    }
    ```
    `ClientMsg::List` uses it.
  - `on_msg` new arms:
    ```rust
    ClientMsg::Watch => self.watch(conn),
    ClientMsg::Reveal { req, id } => {
        let ui = self.state.lock().unwrap().uis.iter().rev().filter_map(Weak::upgrade).find(|c| !c.dead.load(Ordering::Acquire));
        if let Some(ui) = &ui {
            ui.send(ServerMsg::Reveal { id });
        }
        conn.send(ServerMsg::Revealed { req, delivered: ui.is_some() });
    },
    ```
  - `watch`:
    ```rust
    /// Send `conn` the session list now and whenever it changes (checked
    /// every `WATCH_INTERVAL`, which also coalesces bursts).
    fn watch(self: &Arc<Self>, conn: &Arc<Conn>) {
        if conn.watching.swap(true, Ordering::AcqRel) {
            return;
        }
        let (server, conn) = (Arc::downgrade(self), Arc::downgrade(conn));
        let _ = std::thread::Builder::new().name("watch".into()).spawn(move || {
            let mut last = None;
            loop {
                let (Some(server), Some(conn)) = (server.upgrade(), conn.upgrade()) else { return };
                if conn.dead.load(Ordering::Acquire) {
                    return;
                }
                let list = server.list();
                if last.as_ref() != Some(&list) {
                    conn.send(ServerMsg::SessionsChanged { list: list.clone() });
                    last = Some(list);
                }
                drop((server, conn));
                std::thread::sleep(WATCH_INTERVAL);
            }
        });
    }
    ```
  - `on_hook`: after finding the session, `if let Some(id) = &ev.session_id { s.claude_session = Some(id.clone()); }` (before the `match` on owner; `ev.session_id` is read before `ev.token` is moved).
  - Module doc: add a sentence: "Observers (the menu bar agent) watch the session list and ask for a session to be shown; they don't keep the daemon alive."

- [ ] **Step 4: Implement the UI side** (`client.rs`)

  - `/// Gets each request to show a session (from the menu bar agent).` `pub type RevealSink = Arc<dyn Fn(u64) + Send + Sync>;`
  - `Inner` gains `reveal: RevealSink`; `MuxClient::connect` and `Connector` take and pass it (`Connector` stores it beside `hooks`).
  - `read_loop`:
    ```rust
    ServerMsg::Reveal { id } => (inner.reveal)(id),
    // For observers.
    ServerMsg::SessionsChanged { .. } | ServerMsg::Revealed { .. } => {},
    ```
  - `crates/tern-app/src/workspace.rs` `services()`: pass `Arc::new(|_| {})` for now (Task 6 wires it).

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p tern-mux && cargo build -p tern-app`
Expected: all pass, app builds.

- [ ] **Step 6: Commit**

```bash
git add crates/tern-mux crates/tern-app/src/workspace.rs
git commit -m "Daemon: observers watch the session list, Reveal reaches the UI"
```

---

### Task 3: Observer client (`Watcher`)

**Files:**
- Create: `crates/tern-mux/src/observer.rs`
- Modify: `crates/tern-mux/src/lib.rs` (`pub mod observer;`)
- Test: `crates/tern-mux/tests/observer.rs`
- Create: `crates/tern-mux/tests/common/mod.rs` — move `SIZE`, `Daemon`, `daemon`, `daemon_with`, `client`, `wait_until`, `Events`, `sink`, `sh`, `ready_id` there from `tests/daemon.rs` (all `pub`, file starts with `#![allow(dead_code)]`); `tests/daemon.rs` gets `mod common; use common::*;`

**Interfaces:**
- Consumes: Task 1 protocol; daemon behavior from Task 2.
- Produces (`tern_mux::observer`):
  ```rust
  /// Requests to the daemon; clones share the connection.
  #[derive(Clone)] pub struct Watcher { .. }
  /// The session lists the daemon sends, until the connection ends.
  pub struct WatchStream { .. }
  impl Watcher {
      /// Connect to the daemon in `dir` as an observer (never starts it).
      pub fn connect(dir: &Path) -> io::Result<(Watcher, WatchStream)>;
      /// Ask the UI to show session `id`. False when no UI got it (none
      /// connected, an old daemon, the connection lost, no answer in 2 s).
      pub fn reveal(&self, id: u64) -> bool;
      /// End session `id`.
      pub fn close(&self, id: u64);
  }
  impl WatchStream {
      /// The next list; `None` once the connection is gone.
      pub fn next(&mut self) -> Option<Vec<SessionInfo>>;
  }
  pub const POLL_INTERVAL: Duration = Duration::from_secs(2); // MINOR 0 daemons
  ```

- [ ] **Step 1: Write the failing tests** (`tests/observer.rs`)

```rust
mod common;
use common::*;

use std::os::unix::net::UnixListener;
use std::time::{Duration, Instant};

use tern_mux::client::Open;
use tern_mux::observer::Watcher;
use tern_mux::paths;
use tern_mux::proto::{self, ClientMsg, ServerMsg, SessionInfo};

#[test]
fn watcher_follows_the_list() {
    let d = daemon(Duration::from_secs(60));
    let (w, mut stream) = Watcher::connect(&d.dir).unwrap();
    assert_eq!(stream.next(), Some(vec![]));
    let c = client(&d.dir);
    let (sink, ev) = sink();
    let s = c.open(Open::Spawn { cfg: sh("read x"), hook: false }, SIZE, 1000, sink);
    wait_until("ready", || ready_id(&ev).is_some());
    let id = s.id().unwrap();
    while !stream.next().unwrap().iter().any(|i| i.id == id) {}
    drop(s);
    w.close(id);
    while !stream.next().unwrap().is_empty() {}
}

#[test]
fn watcher_reveal_reports_delivery() {
    let d = daemon(Duration::from_secs(60));
    let (w, mut stream) = Watcher::connect(&d.dir).unwrap();
    // `reveal` gets its answer through the stream: keep reading it.
    std::thread::spawn(move || while stream.next().is_some() {});
    assert!(!w.reveal(5));
    let got = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
    let g = got.clone();
    let _ui = tern_mux::client::MuxClient::connect(&d.dir, false, std::sync::Arc::new(|_, _| {}), std::sync::Arc::new(move |id| g.lock().unwrap().push(id))).unwrap();
    assert!(w.reveal(5));
    wait_until("ui got it", || *got.lock().unwrap() == [5]);
}

#[test]
fn watcher_ends_when_the_daemon_goes() {
    let d = daemon(Duration::from_millis(300));
    let (w, mut stream) = Watcher::connect(&d.dir).unwrap();
    let start = Instant::now();
    while stream.next().is_some() {
        assert!(start.elapsed() < Duration::from_secs(10), "daemon never idled out");
    }
    assert!(!w.reveal(1), "a lost connection reveals nothing");
}

#[test]
fn watcher_never_starts_a_daemon() {
    let tmp = tempfile::Builder::new().prefix("tm").tempdir_in("/tmp").unwrap();
    assert!(Watcher::connect(tmp.path()).is_err());
    assert!(!paths::socket_path(tmp.path()).exists());
}

#[test]
fn minor_0_daemon_is_polled() {
    let tmp = tempfile::Builder::new().prefix("tm").tempdir_in("/tmp").unwrap();
    let listener = UnixListener::bind(paths::socket_path(tmp.path())).unwrap();
    std::thread::spawn(move || {
        let (sock, _) = listener.accept().unwrap();
        let mut w = sock.try_clone().unwrap();
        let mut r = proto::FrameReader::new(sock, proto::MAX_CLIENT_FRAME);
        assert!(matches!(r.read::<ClientMsg>().unwrap(), Some(ClientMsg::Hello { .. })));
        proto::write_frame(&mut w, &ServerMsg::Hello { major: proto::MAJOR, minor: 0, app_version: "0.1.1".into(), pid: 1 }).unwrap();
        while let Ok(Some(msg)) = r.read::<ClientMsg>() {
            match msg {
                ClientMsg::List { req } => {
                    let list = vec![SessionInfo { id: 7, ..Default::default() }];
                    proto::write_frame(&mut w, &ServerMsg::Sessions { req, list }).unwrap();
                },
                ClientMsg::Watch | ClientMsg::Reveal { .. } => panic!("sent {msg:?} to a MINOR 0 daemon"),
                _ => {},
            }
        }
    });
    let (w, mut stream) = Watcher::connect(tmp.path()).unwrap();
    assert_eq!(stream.next().unwrap()[0].id, 7);
    let t = Instant::now();
    assert!(!w.reveal(7));
    assert!(t.elapsed() < Duration::from_millis(100), "no waiting on an old daemon");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p tern-mux --test observer`
Expected: compile error (`tern_mux::observer` missing).

- [ ] **Step 3: Implement** (`observer.rs`)

```rust
//! The menu bar agent's side: an observer connection that follows the
//! daemon's session list and asks it to show or end a session. It never
//! starts the daemon and doesn't keep it alive.

use std::collections::HashMap;
use std::io::{self, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use crate::paths;
use crate::proto::{self, ClientMsg, FrameReader, MAJOR, MINOR, Role, ServerMsg, SessionInfo};

/// How often a MINOR 0 daemon (no `Watch`) is asked for its list.
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);
const REVEAL_TIMEOUT: Duration = Duration::from_secs(2);
const HELLO_TIMEOUT: Duration = Duration::from_secs(3);

struct Inner {
    writer: Mutex<UnixStream>,
    alive: AtomicBool,
    /// The daemon's protocol minor: 0 has no `Watch`/`Reveal`.
    minor: u32,
    next_req: AtomicU64,
    reveals: Mutex<HashMap<u64, mpsc::Sender<bool>>>,
}

impl Inner {
    fn send(&self, msg: &ClientMsg) -> io::Result<()> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(io::ErrorKind::NotConnected.into());
        }
        let frame = proto::encode(msg)?;
        self.writer.lock().unwrap().write_all(&frame)
    }
}

#[derive(Clone)]
pub struct Watcher {
    inner: Arc<Inner>,
}

pub struct WatchStream {
    inner: Arc<Inner>,
    reader: FrameReader,
}

impl Watcher {
    pub fn connect(dir: &Path) -> io::Result<(Watcher, WatchStream)> {
        let stream = UnixStream::connect(paths::socket_path(dir))?;
        if !paths::peer_uid_ok(&stream) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "daemon runs as another user"));
        }
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        stream.set_read_timeout(Some(HELLO_TIMEOUT))?;
        let mut writer = stream.try_clone()?;
        proto::write_frame(&mut writer, &ClientMsg::Hello { major: MAJOR, minor: MINOR, app_version: env!("CARGO_PKG_VERSION").into(), role: Role::Observer })?;
        let mut reader = FrameReader::new(stream, proto::MAX_SERVER_FRAME);
        let minor = match reader.read::<ServerMsg>()? {
            Some(ServerMsg::Hello { major, minor, .. }) if major == MAJOR => minor,
            other => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("bad hello: {other:?}"))),
        };
        writer.set_read_timeout(None)?;
        let inner = Arc::new(Inner { writer: Mutex::new(writer), alive: AtomicBool::new(true), minor, next_req: AtomicU64::new(1), reveals: Mutex::default() });
        if minor >= 1 {
            inner.send(&ClientMsg::Watch)?;
        } else {
            let weak = Arc::downgrade(&inner);
            std::thread::Builder::new().name("watch poll".into()).spawn(move || {
                while let Some(i) = weak.upgrade() {
                    if i.send(&ClientMsg::List { req: 0 }).is_err() {
                        return;
                    }
                    drop(i);
                    std::thread::sleep(POLL_INTERVAL);
                }
            })?;
        }
        Ok((Watcher { inner: inner.clone() }, WatchStream { inner, reader }))
    }

    pub fn reveal(&self, id: u64) -> bool {
        if self.inner.minor < 1 {
            return false;
        }
        let req = self.inner.next_req.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.inner.reveals.lock().unwrap().insert(req, tx);
        let ok = self.inner.send(&ClientMsg::Reveal { req, id }).is_ok() && rx.recv_timeout(REVEAL_TIMEOUT).unwrap_or(false);
        self.inner.reveals.lock().unwrap().remove(&req);
        ok
    }

    pub fn close(&self, id: u64) {
        let _ = self.inner.send(&ClientMsg::Close { id });
    }
}

impl WatchStream {
    pub fn next(&mut self) -> Option<Vec<SessionInfo>> {
        loop {
            match self.reader.read::<ServerMsg>() {
                Ok(Some(ServerMsg::SessionsChanged { list } | ServerMsg::Sessions { list, .. })) => return Some(list),
                Ok(Some(ServerMsg::Revealed { req, delivered })) => {
                    if let Some(tx) = self.inner.reveals.lock().unwrap().remove(&req) {
                        let _ = tx.send(delivered);
                    }
                },
                Ok(Some(_)) => {},
                Ok(None) | Err(_) => {
                    self.inner.alive.store(false, Ordering::Release);
                    // Waiting reveals get `false` now rather than at their timeout.
                    self.inner.reveals.lock().unwrap().clear();
                    return None;
                },
            }
        }
    }
}

impl Drop for WatchStream {
    fn drop(&mut self) {
        self.inner.alive.store(false, Ordering::Release);
        let _ = self.inner.writer.lock().unwrap().shutdown(std::net::Shutdown::Both);
    }
}
```
  Note: `SessionInfo` must implement `PartialEq` for `assert_eq!(stream.next(), Some(vec![]))` — it does (Task 1).

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p tern-mux`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/tern-mux
git commit -m "tern-mux: observer client for the menu bar agent"
```

---

### Task 4: `tern-menubar` crate — model, single instance, setting, detached spawn

**Files:**
- Create: `crates/tern-menubar/Cargo.toml`, `crates/tern-menubar/src/lib.rs`, `src/model.rs`, `src/instance.rs`, `src/setting.rs`
- Modify: `crates/tern-mux/src/client.rs` (extract `spawn_detached`, generalize `log_file`)

**Interfaces:**
- Consumes: `tern_mux::proto::SessionInfo`, `tern_term::status::ActivityStatus`.
- Produces:
  ```rust
  // tern_mux::client
  /// Run this executable with `args`, detached (own session, output to `log`).
  pub fn spawn_detached(args: &[&str], log: Option<&Path>) -> io::Result<()>;
  /// `~/Library/Logs/Tern/<name>.log` (`<name>-dev.log` for debug builds).
  pub fn log_path(name: &str) -> Option<PathBuf>;
  pub fn log_file() -> Option<PathBuf>; // = log_path("daemon"), kept for callers

  // tern_menubar::model
  pub struct Row { pub id: u64, pub glyph: &'static str, pub label: String, pub cwd: Option<String>, pub open: bool, pub exited: bool }
  pub fn rows(list: &[SessionInfo], home: Option<&Path>) -> Vec<Row>;
  impl Row { pub fn menu_title(&self) -> String }
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)] pub struct IconState { pub count: usize, pub attention: bool }
  pub fn icon(list: &[SessionInfo]) -> IconState;
  pub fn icon_title(s: IconState) -> String;

  // tern_menubar::instance
  pub fn lock_path() -> PathBuf;
  pub enum Taken { Ours(std::fs::File), Busy }
  pub fn take(path: &Path, version: &str, stop: impl Fn(u32)) -> io::Result<Taken>;
  pub fn holder(path: &Path) -> Option<u32>; // pid of a live holder

  // tern_menubar::setting
  pub fn file() -> PathBuf;
  pub fn enabled(file: &Path) -> bool;
  pub fn set_enabled(file: &Path, on: bool) -> io::Result<()>;
  ```

- [ ] **Step 1: Scaffold the crate**

`crates/tern-menubar/Cargo.toml`:
```toml
[package]
name = "tern-menubar"
version.workspace = true
edition.workspace = true

[dependencies]
libc = "0.2.190"
log = "0.4.34"
semver = "1"
tern-mux = { version = "0.1.0", path = "../tern-mux" }
tern-term = { version = "0.1.0", path = "../tern-term" }

[dev-dependencies]
tempfile = "3.27.0"
```
(`version.workspace = true` makes the agent's version Tern's version — the lock compares it.)

`src/lib.rs`:
```rust
//! The menu bar agent: `tern --menubar`. A small separate process that
//! lists the background daemon's terminals in the macOS menu bar, also
//! while Tern is closed. It holds no programs, so it can be replaced at any
//! time.

pub mod instance;
pub mod model;
pub mod setting;
```

- [ ] **Step 2: Write the failing tests**

`src/model.rs` tests:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tern_term::status::ActivityStatus;

    fn s(id: u64, status: ActivityStatus, title: Option<&str>) -> SessionInfo {
        SessionInfo { id, status, title: title.map(Into::into), attached: true, ..Default::default() }
    }

    #[test]
    fn rows_put_waiting_first() {
        let list = vec![
            s(1, ActivityStatus::None, Some("b idle")),
            s(2, ActivityStatus::Done, Some("done")),
            SessionInfo { exited: true, ..s(3, ActivityStatus::None, Some("a exited")) },
            s(4, ActivityStatus::Working, Some("working")),
            s(5, ActivityStatus::Blocked, Some("needs you")),
            s(6, ActivityStatus::None, Some("a idle")),
        ];
        let ids: Vec<_> = rows(&list, None).iter().map(|r| r.id).collect();
        assert_eq!(ids, [5, 4, 2, 6, 1, 3]);
    }

    #[test]
    fn row_labels() {
        let home = Path::new("/Users/me");
        let list = vec![
            SessionInfo { cwd: Some("/Users/me/src/tern".into()), attached: false, ..s(1, ActivityStatus::None, Some("✳ Fix bug")) },
            SessionInfo { program: Some("vim".into()), cwd: Some("/tmp".into()), ..s(2, ActivityStatus::None, None) },
            SessionInfo { title: Some(String::new()), ..s(3, ActivityStatus::None, None) },
        ];
        let r = rows(&list, Some(home));
        let by = |id| r.iter().find(|r| r.id == id).unwrap();
        assert_eq!(by(1).label, "✳ Fix bug");
        assert_eq!(by(1).cwd.as_deref(), Some("~/src/tern"));
        assert!(!by(1).open);
        assert_eq!(by(1).menu_title(), "○  ✳ Fix bug — ~/src/tern  (not open)");
        assert_eq!(by(2).label, "vim");
        assert_eq!(by(2).menu_title(), "○  vim — /tmp");
        assert_eq!(by(3).label, "shell");
    }

    #[test]
    fn icon_counts_running_and_flags_waiting() {
        assert_eq!(icon(&[]), IconState { count: 0, attention: false });
        let list = vec![s(1, ActivityStatus::Working, None), SessionInfo { exited: true, ..s(2, ActivityStatus::None, None) }];
        assert_eq!(icon(&list), IconState { count: 1, attention: false });
        let list = vec![s(1, ActivityStatus::Blocked, None), s(2, ActivityStatus::None, None)];
        assert_eq!(icon(&list), IconState { count: 2, attention: true });
        assert_eq!(icon_title(IconState { count: 0, attention: false }), "");
        assert_eq!(icon_title(IconState { count: 3, attention: false }), "3");
        assert_eq!(icon_title(IconState { count: 2, attention: true }), "2 •");
    }
}
```

`src/instance.rs` tests:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Hold `path` as another agent would: locked, with `<pid> <version>`.
    fn hold(path: &Path, pid: u32, version: &str) -> std::fs::File {
        let f = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(path).unwrap();
        assert_eq!(unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
        write_holder(&f, pid, version).unwrap();
        f
    }

    #[test]
    fn free_lock_is_taken_and_names_us() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("menubar.lock");
        let Taken::Ours(_f) = take(&path, "0.1.2", |_| panic!("nothing to stop")).unwrap() else { panic!("busy") };
        assert_eq!(std::fs::read_to_string(&path).unwrap(), format!("{} 0.1.2\n", std::process::id()));
        assert_eq!(holder(&path), Some(std::process::id()));
    }

    #[test]
    fn older_holder_is_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("menubar.lock");
        let old = Mutex::new(Some(hold(&path, 4242, "0.1.1")));
        let stopped = Mutex::new(vec![]);
        // Stopping it releases its lock, as its exit would.
        let got = take(&path, "0.1.2", |pid| {
            stopped.lock().unwrap().push(pid);
            old.lock().unwrap().take();
        })
        .unwrap();
        assert!(matches!(got, Taken::Ours(_)));
        assert_eq!(*stopped.lock().unwrap(), [4242]);
    }

    #[test]
    fn equal_or_newer_holder_stays() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("menubar.lock");
        let _same = hold(&path, 4242, "0.1.2");
        assert!(matches!(take(&path, "0.1.2", |_| panic!("stopped an equal agent")).unwrap(), Taken::Busy));
        drop(_same);
        let _newer = hold(&path, 4242, "0.2.0");
        assert!(matches!(take(&path, "0.1.2", |_| panic!("stopped a newer agent")).unwrap(), Taken::Busy));
    }

    #[test]
    fn holder_that_wont_stop_keeps_it() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("menubar.lock");
        let _old = hold(&path, 4242, "0.1.1");
        let t = std::time::Instant::now();
        assert!(matches!(take(&path, "0.1.2", |_| {}).unwrap(), Taken::Busy));
        assert!(t.elapsed() >= TAKEOVER_WAIT);
    }

    #[test]
    fn no_holder_without_a_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("menubar.lock");
        std::fs::write(&path, "4242 0.1.1\n").unwrap(); // stale: nobody holds it
        assert_eq!(holder(&path), None);
    }
}
```

`src/setting.rs` tests:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_means_on_and_off_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("sub/menubar");
        assert!(enabled(&f));
        set_enabled(&f, false).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "off\n");
        assert!(!enabled(&f));
        set_enabled(&f, true).unwrap();
        assert!(enabled(&f));
        std::fs::write(&f, "  off  \n").unwrap();
        assert!(!enabled(&f));
        std::fs::write(&f, "garbage").unwrap();
        assert!(enabled(&f));
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p tern-menubar`
Expected: compile errors (functions missing).

- [ ] **Step 4: Implement**

`src/model.rs`:
```rust
//! What the menu shows, from the daemon's session list.

use std::path::Path;

use tern_mux::proto::SessionInfo;
use tern_term::status::ActivityStatus;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: u64,
    pub glyph: &'static str,
    pub label: String,
    /// The working directory, `~`-shortened.
    pub cwd: Option<String>,
    /// Some Tern tab shows it.
    pub open: bool,
    pub exited: bool,
}

impl Row {
    pub fn menu_title(&self) -> String {
        let mut t = format!("{}  {}", self.glyph, self.label);
        if let Some(cwd) = &self.cwd {
            t.push_str(" — ");
            t.push_str(cwd);
        }
        if !self.open {
            t.push_str("  (not open)");
        }
        t
    }
}

/// Needs input, working, done, idle, exited; then by label.
fn rank(s: &SessionInfo) -> u8 {
    match (s.exited, s.status) {
        (true, _) => 4,
        (false, ActivityStatus::Blocked) => 0,
        (false, ActivityStatus::Working) => 1,
        (false, ActivityStatus::Done) => 2,
        (false, ActivityStatus::None) => 3,
    }
}

fn glyph(s: &SessionInfo) -> &'static str {
    match (s.exited, s.status) {
        (true, _) => "×",
        (false, ActivityStatus::Blocked) => "●",
        (false, ActivityStatus::Working) => "◐",
        (false, ActivityStatus::Done) => "✓",
        (false, ActivityStatus::None) => "○",
    }
}

fn label(s: &SessionInfo) -> String {
    s.title.clone().filter(|t| !t.trim().is_empty()).or_else(|| s.program.clone()).unwrap_or_else(|| "shell".into())
}

fn short(cwd: &Path, home: Option<&Path>) -> String {
    match home.and_then(|h| cwd.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => cwd.display().to_string(),
    }
}

pub fn rows(list: &[SessionInfo], home: Option<&Path>) -> Vec<Row> {
    let mut rows: Vec<_> = list
        .iter()
        .map(|s| (rank(s), Row { id: s.id, glyph: glyph(s), label: label(s), cwd: s.cwd.as_deref().map(|c| short(c, home)), open: s.attached, exited: s.exited }))
        .collect();
    rows.sort_by(|(ra, a), (rb, b)| ra.cmp(rb).then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase())).then(a.id.cmp(&b.id)));
    rows.into_iter().map(|(_, r)| r).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IconState {
    /// Running sessions.
    pub count: usize,
    /// One waits for the user.
    pub attention: bool,
}

pub fn icon(list: &[SessionInfo]) -> IconState {
    let running = list.iter().filter(|s| !s.exited);
    IconState { count: running.clone().count(), attention: running.into_iter().any(|s| s.status == ActivityStatus::Blocked) }
}

/// The text beside the icon: the count (none at 0), and a dot when
/// something waits for the user.
pub fn icon_title(s: IconState) -> String {
    match (s.count, s.attention) {
        (0, false) => String::new(),
        (0, true) => "•".into(),
        (n, false) => n.to_string(),
        (n, true) => format!("{n} •"),
    }
}
```

`src/instance.rs`:
```rust
//! One agent at a time, the newest: the lock file holds `<pid> <version>`
//! of the agent that has it. A newer agent stops an older one (after an
//! update); otherwise the one starting exits.

use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a newer agent waits for an older one to let go.
pub const TAKEOVER_WAIT: Duration = Duration::from_secs(2);

pub fn lock_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/tmp".into());
    home.join("Library/Caches/dev.tern").join(if cfg!(debug_assertions) { "menubar-dev.lock" } else { "menubar.lock" })
}

pub enum Taken {
    /// Ours while the file stays open.
    Ours(File),
    /// An agent as new as this one, or newer, runs.
    Busy,
}

fn open(path: &Path) -> io::Result<File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(path)
}

fn try_lock(f: &File) -> io::Result<bool> {
    // SAFETY: flock on an fd we own.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let e = io::Error::last_os_error();
    if e.raw_os_error() == Some(libc::EWOULDBLOCK) { Ok(false) } else { Err(e) }
}

fn write_holder(mut f: &File, pid: u32, version: &str) -> io::Result<()> {
    f.set_len(0)?;
    f.rewind()?;
    write!(f, "{pid} {version}\n")?;
    f.sync_all()
}

fn read_holder(path: &Path) -> Option<(u32, semver::Version)> {
    let mut s = String::new();
    File::open(path).ok()?.read_to_string(&mut s).ok()?;
    let mut it = s.split_whitespace();
    let pid = it.next()?.parse().ok()?;
    let version = semver::Version::parse(it.next()?).ok()?;
    Some((pid, version))
}

/// Take the lock at `path` for an agent of `version`; an older holder is
/// asked to go with `stop(pid)` (SIGTERM in the real agent).
pub fn take(path: &Path, version: &str, stop: impl Fn(u32)) -> io::Result<Taken> {
    let f = open(path)?;
    if !try_lock(&f)? {
        let ours = semver::Version::parse(version).map_err(io::Error::other)?;
        match read_holder(path) {
            Some((pid, theirs)) if theirs < ours => {
                log::info!("replacing menu bar agent {pid} (Tern {theirs})");
                stop(pid);
                let deadline = Instant::now() + TAKEOVER_WAIT;
                loop {
                    if try_lock(&f)? {
                        break;
                    }
                    if Instant::now() >= deadline {
                        return Ok(Taken::Busy);
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            },
            _ => return Ok(Taken::Busy),
        }
    }
    write_holder(&f, std::process::id(), version)?;
    Ok(Taken::Ours(f))
}

/// The pid of the agent holding `path`, if one does.
pub fn holder(path: &Path) -> Option<u32> {
    let f = File::open(path).ok()?;
    // Free (we got it) = nobody holds it; let go at once.
    if try_lock(&f).ok()? {
        return None;
    }
    read_holder(path).map(|(pid, _)| pid)
}
```
  Note: `holder` on a file we hold ourselves (the `free_lock_is_taken_and_names_us` test) — a second `open` is a separate open-file description, so its `flock` fails while ours is held: correct. Also make `write_holder` `pub(crate)` or keep private (the test module sees it).

`src/setting.rs`:
```rust
//! Whether the menu bar icon is shown: a file next to `claude-args`,
//! `off` to hide it (missing = shown).

use std::io;
use std::path::{Path, PathBuf};

pub fn file() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join("Library/Application Support/dev.tern/menubar")
}

pub fn enabled(file: &Path) -> bool {
    std::fs::read_to_string(file).map_or(true, |s| s.trim() != "off")
}

pub fn set_enabled(file: &Path, on: bool) -> io::Result<()> {
    if on {
        return match std::fs::remove_file(file) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(file, "off\n")
}
```

`tern-mux/src/client.rs`: replace `fn spawn()` and `log_file()` with:
```rust
/// Start the daemon from this executable, detached from us.
fn spawn() -> io::Result<()> {
    spawn_detached(&["--server"], log_file().as_deref())
}

/// Run this executable with `args` in its own session (it outlives us;
/// nobody waits for it), output appended to `log`.
pub fn spawn_detached(args: &[&str], log: Option<&Path>) -> io::Result<()> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe()?;
    let (out, err) = match log.and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok()) {
        Some(f) => (std::process::Stdio::from(f.try_clone()?), std::process::Stdio::from(f)),
        None => (std::process::Stdio::null(), std::process::Stdio::null()),
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args).stdin(std::process::Stdio::null()).stdout(out).stderr(err);
    if std::env::var_os("RUST_LOG").is_none() {
        cmd.env("RUST_LOG", "info");
    }
    // SAFETY: setsid is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().map(drop)
}

/// `~/Library/Logs/Tern/<name>.log` (`<name>-dev.log` for debug builds).
pub fn log_path(name: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let dir = PathBuf::from(home).join("Library/Logs/Tern");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(if cfg!(debug_assertions) { format!("{name}-dev.log") } else { format!("{name}.log") }))
}

/// The daemon's log.
pub fn log_file() -> Option<PathBuf> {
    log_path("daemon")
}
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p tern-menubar && cargo test -p tern-mux && cargo clippy --workspace --all-targets`
Expected: pass, no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/tern-menubar crates/tern-mux Cargo.lock
git commit -m "tern-menubar: rows, icon state, single instance, setting"
```

---

### Task 5: The agent — daemon link and the menu bar item

**Files:**
- Create: `crates/tern-menubar/src/link.rs`, `crates/tern-menubar/src/app.rs`
- Modify: `crates/tern-menubar/Cargo.toml`, `src/lib.rs` (add `run`, `launch`, `stop`)
- Modify: `crates/tern-app/Cargo.toml` (dep `tern-menubar`), `crates/tern-app/src/main.rs` (`--menubar` entry)
- Test: `crates/tern-menubar/tests/link.rs`

**Interfaces:**
- Consumes: `tern_mux::observer::{Watcher, WatchStream}` (Task 3); `model`, `instance`, `setting` (Task 4); `tern_mux::client::{spawn_detached, log_path}`.
- Produces:
  ```rust
  // tern_menubar::link
  pub enum LinkEvent { Connected(Watcher), List(Vec<SessionInfo>), Lost }
  pub const RETRY: Duration = Duration::from_secs(2);
  /// Follow the daemon in `dir` forever: connect, forward lists, report the
  /// loss, retry every `RETRY`. Never starts the daemon.
  pub fn run(dir: &Path, post: impl Fn(LinkEvent)) -> !;
  /// One connection's worth of `run` (tests): returns once it is lost or refused.
  pub fn follow_once(dir: &Path, post: &impl Fn(LinkEvent));

  // tern_menubar (lib.rs)
  /// `tern --menubar`: run the agent until stopped. macOS only.
  pub fn run() -> !;
  /// Start the agent detached (Tern UI at launch / "Show Menu Bar Icon").
  pub fn launch();
  /// Stop the running agent ("Hide Menu Bar Icon" in Tern).
  pub fn stop();
  ```

- [ ] **Step 1: Write the failing link test** (`tests/link.rs`)

```rust
use std::sync::mpsc;
use std::time::Duration;

use tern_menubar::link::{LinkEvent, follow_once};
use tern_mux::server::{ServeOpts, serve};

#[test]
fn link_reports_loss_and_reconnects() {
    let tmp = tempfile::Builder::new().prefix("tm").tempdir_in("/tmp").unwrap();
    let dir = tmp.path().join("m");
    let opts = || ServeOpts {
        idle_exit: Duration::from_millis(300),
        notifications: false,
        run_root: tmp.path().join("run"),
        hooks: false,
        max_queued: 1 << 20,
        max_queued_session: usize::MAX,
    };
    // No daemon: refused at once, nothing posted but `Lost`.
    let (tx, rx) = mpsc::channel();
    follow_once(&dir, &|e| tx.send(e).unwrap());
    assert!(matches!(rx.try_recv(), Ok(LinkEvent::Lost)));

    for _round in 0..2 {
        let d = dir.clone();
        let o = opts();
        let daemon = std::thread::spawn(move || serve(&d, o));
        while !tern_mux::paths::socket_path(&dir).exists() {
            std::thread::sleep(Duration::from_millis(10));
        }
        let (tx, rx) = mpsc::channel();
        // Returns once the idle daemon exits.
        follow_once(&dir, &|e| tx.send(e).unwrap());
        let events: Vec<_> = rx.try_iter().collect();
        assert!(matches!(events.first(), Some(LinkEvent::Connected(_))), "{}", events.len());
        assert!(events.iter().any(|e| matches!(e, LinkEvent::List(l) if l.is_empty())));
        assert!(matches!(events.last(), Some(LinkEvent::Lost)));
        daemon.join().unwrap().unwrap();
    }
}
```
  (`LinkEvent` needs no `Debug`; if you add `#[derive(Debug)]`, `Watcher` needs a manual `Debug` — skip it.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p tern-menubar --test link`
Expected: compile error (`link` missing).

- [ ] **Step 3: Implement `link.rs`**

```rust
//! The agent's connection to the daemon, on its own thread.

use std::path::Path;
use std::time::Duration;

use tern_mux::observer::Watcher;
use tern_mux::proto::SessionInfo;

pub const RETRY: Duration = Duration::from_secs(2);

pub enum LinkEvent {
    /// Requests (Open, End) go through this until `Lost`.
    Connected(Watcher),
    List(Vec<SessionInfo>),
    /// No daemon (or it went away): show the empty state.
    Lost,
}

pub fn follow_once(dir: &Path, post: &impl Fn(LinkEvent)) {
    match Watcher::connect(dir) {
        Ok((w, mut stream)) => {
            post(LinkEvent::Connected(w));
            while let Some(list) = stream.next() {
                post(LinkEvent::List(list));
            }
            log::info!("daemon connection lost");
        },
        Err(e) => log::debug!("no daemon: {e}"),
    }
    post(LinkEvent::Lost);
}

pub fn run(dir: &Path, post: impl Fn(LinkEvent)) -> ! {
    loop {
        follow_once(dir, &post);
        std::thread::sleep(RETRY);
    }
}
```
  Add `pub mod link;` to `lib.rs`.

- [ ] **Step 4: Run the link test**

Run: `cargo test -p tern-menubar --test link`
Expected: PASS.

- [ ] **Step 5: Add the AppKit dependencies** (`crates/tern-menubar/Cargo.toml`)

```toml
[target.'cfg(target_os = "macos")'.dependencies]
dispatch2 = "0.3"
objc2 = "0.6"
objc2-app-kit = { version = "0.3.2", default-features = false, features = ["std", "NSAlert", "NSApplication", "NSButton", "NSButtonCell", "NSCell", "NSControl", "NSImage", "NSMenu", "NSMenuItem", "NSResponder", "NSRunningApplication", "NSStatusBar", "NSStatusBarButton", "NSStatusItem", "NSView"] }
objc2-foundation = { version = "0.3.2", features = ["NSString"] }
```
  These versions are already in `Cargo.lock` (via gpui). If a method below is gated behind a feature not listed, the compiler names it — add that feature.

- [ ] **Step 6: Implement `app.rs`** (macOS only: `#[cfg(target_os = "macos")] mod app;` in `lib.rs`)

```rust
//! The status item and its menu. Everything here runs on the main thread;
//! the link thread hands events over through the main dispatch queue.

use std::cell::RefCell;
use std::path::PathBuf;

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSApplicationActivationPolicy, NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem,
};
use objc2_foundation::{NSString, ns_string};
use tern_mux::observer::Watcher;
use tern_mux::proto::SessionInfo;

use crate::link::{self, LinkEvent};
use crate::model;

struct Ui {
    item: Retained<NSStatusItem>,
    target: Retained<Target>,
    /// Set while connected.
    watcher: Option<Watcher>,
    list: Vec<SessionInfo>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TernMenuTarget"]
    struct Target;

    impl Target {
        #[unsafe(method(open:))]
        fn open(&self, sender: &NSMenuItem) {
            open_session(sender.tag() as u64);
        }

        #[unsafe(method(end:))]
        fn end(&self, sender: &NSMenuItem) {
            end_session(sender.tag() as u64);
        }

        #[unsafe(method(openTern:))]
        fn open_tern(&self, _sender: &NSMenuItem) {
            open_tern();
        }

        #[unsafe(method(hide:))]
        fn hide(&self, _sender: &NSMenuItem) {
            if let Err(e) = crate::setting::set_enabled(&crate::setting::file(), false) {
                log::warn!("can't save the setting: {e}");
            }
            std::process::exit(0);
        }
    }
);

impl Target {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's init.
        unsafe { msg_send![super(this), init] }
    }
}

/// Run the menu bar item until the process ends.
pub fn run(mux_dir: PathBuf) -> ! {
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    // No Dock icon, no menu bar of its own.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let item = NSStatusBar::systemStatusBar().statusItemWithLength(-1.0); // NSVariableStatusItemLength
    if let Some(button) = item.button(mtm) {
        let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(ns_string!("terminal"), Some(ns_string!("Tern")));
        if let Some(image) = image {
            image.setTemplate(true);
            button.setImage(Some(&image));
        }
    }
    let ui = Ui { item, target: Target::new(mtm), watcher: None, list: vec![] };
    UI.with(|u| *u.borrow_mut() = Some(ui));
    refresh(mtm);
    std::thread::Builder::new()
        .name("link".into())
        .spawn(move || link::run(&mux_dir, |ev| DispatchQueue::main().exec_async(move || apply(ev))))
        .expect("link thread");
    app.run();
    std::process::exit(0)
}

fn apply(ev: LinkEvent) {
    let mtm = MainThreadMarker::new().expect("main queue");
    UI.with(|u| {
        let mut u = u.borrow_mut();
        let Some(ui) = u.as_mut() else { return };
        match ev {
            LinkEvent::Connected(w) => ui.watcher = Some(w),
            LinkEvent::List(list) => ui.list = list,
            LinkEvent::Lost => {
                ui.watcher = None;
                ui.list.clear();
            },
        }
    });
    refresh(mtm);
}

fn menu_item(title: &str, action: Option<Sel>, tag: isize, target: &Target, mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    // SAFETY: the selectors are methods of `Target`.
    let item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &NSString::from_str(title), action, ns_string!("")) };
    // SAFETY: `target` lives as long as the UI (thread-local, never dropped).
    unsafe { item.setTarget(Some(target as &AnyObject)) };
    item.setTag(tag);
    item
}

/// Redraw the icon and rebuild the menu from the latest list. (Rebuilt on
/// every list, at most 4×/s; an open menu updates in place.)
fn refresh(mtm: MainThreadMarker) {
    UI.with(|u| {
        let u = u.borrow();
        let Some(ui) = u.as_ref() else { return };
        let state = model::icon(&ui.list);
        if let Some(button) = ui.item.button(mtm) {
            button.setTitle(&NSString::from_str(&model::icon_title(state)));
        }
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let rows = model::rows(&ui.list, home.as_deref());
        let menu = NSMenu::new(mtm);
        if rows.is_empty() {
            let empty = menu_item("No background terminals", None, 0, &ui.target, mtm);
            empty.setEnabled(false);
            menu.addItem(&empty);
        }
        for row in &rows {
            let tag = row.id as isize; // ids are < 2^63 (see the daemon's random_id)
            let item = menu_item(&row.menu_title(), None, tag, &ui.target, mtm);
            let sub = NSMenu::new(mtm);
            sub.addItem(&menu_item("Open", Some(sel!(open:)), tag, &ui.target, mtm));
            sub.addItem(&menu_item("End…", Some(sel!(end:)), tag, &ui.target, mtm));
            item.setSubmenu(Some(&sub));
            menu.addItem(&item);
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&menu_item("Open Tern", Some(sel!(openTern:)), 0, &ui.target, mtm));
        menu.addItem(&menu_item("Hide Menu Bar Icon", Some(sel!(hide:)), 0, &ui.target, mtm));
        ui.item.setMenu(Some(&menu));
    });
}

fn watcher() -> Option<Watcher> {
    UI.with(|u| u.borrow().as_ref().and_then(|ui| ui.watcher.clone()))
}

/// Ask the running Tern to show it; else start/activate Tern with it.
fn open_session(id: u64) {
    let w = watcher();
    // `reveal` waits for the daemon's answer: not on the main thread.
    std::thread::spawn(move || {
        if !w.is_some_and(|w| w.reveal(id)) {
            open_with_tern(id);
        }
    });
}

fn open_with_tern(id: u64) {
    let r = if cfg!(debug_assertions) {
        // No bundle to route a URL to.
        tern_mux::client::spawn_detached(&["--open-session", &id.to_string()], None)
    } else {
        std::process::Command::new("/usr/bin/open").arg(format!("tern://session/{id}")).status().map(drop)
    };
    if let Err(e) = r {
        log::warn!("can't open session {id} in Tern: {e}");
    }
}

fn open_tern() {
    let r = if cfg!(debug_assertions) {
        tern_mux::client::spawn_detached(&[], None)
    } else {
        std::process::Command::new("/usr/bin/open").args(["-b", "dev.tern"]).status().map(drop)
    };
    if let Err(e) = r {
        log::warn!("can't open Tern: {e}");
    }
}

fn end_session(id: u64) {
    let mtm = MainThreadMarker::new().expect("main thread");
    let label = UI.with(|u| {
        let u = u.borrow();
        let list = u.as_ref().map(|ui| ui.list.clone()).unwrap_or_default();
        model::rows(&list, None).into_iter().find(|r| r.id == id).map(|r| r.label)
    });
    let Some(label) = label else { return }; // ended meanwhile
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(&format!("End \"{label}\"?")));
    alert.setInformativeText(ns_string!("The program running in this terminal stops."));
    alert.addButtonWithTitle(ns_string!("End"));
    alert.addButtonWithTitle(ns_string!("Cancel"));
    // An accessory app's alert would open behind other apps.
    NSApplication::sharedApplication(mtm).activate();
    if alert.runModal() == NSAlertFirstButtonReturn
        && let Some(w) = watcher()
    {
        w.close(id);
    }
}
```
  Compile-fix notes for the implementer: if `Self::alloc(mtm).set_ivars(())` doesn't compile for a class without `#[ivars]`, use `unsafe { msg_send![Self::alloc(mtm), init] }`. If `NSApplication::activate` is unavailable on the SDK, use `activateIgnoringOtherApps(true)`. `target as &AnyObject` may need `&**target` / `.as_super()` chains — use whatever coerces `&Target` to `&AnyObject`.

- [ ] **Step 7: `run`/`launch`/`stop` in `lib.rs`**

```rust
pub mod link;
#[cfg(target_os = "macos")]
mod app;

/// `tern --menubar`: one agent at a time, the newest.
#[cfg(target_os = "macos")]
pub fn run() -> ! {
    let version = env!("CARGO_PKG_VERSION");
    let stop = |pid: u32| {
        // SAFETY: plain kill.
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    };
    let _lock = match instance::take(&instance::lock_path(), version, stop) {
        Ok(instance::Taken::Ours(f)) => f,
        Ok(instance::Taken::Busy) => {
            log::info!("a menu bar agent as new runs already");
            std::process::exit(0)
        },
        Err(e) => {
            log::error!("menu bar lock: {e}");
            std::process::exit(1)
        },
    };
    log::info!("menu bar agent {} (Tern {version})", std::process::id());
    app::run(tern_mux::paths::mux_dir())
}

/// Start the agent, detached (it outlives the UI). It sorts out by itself
/// whether another one stays.
pub fn launch() {
    let log = tern_mux::client::log_path("menubar");
    if let Err(e) = tern_mux::client::spawn_detached(&["--menubar"], log.as_deref()) {
        log::warn!("can't start the menu bar agent: {e}");
    }
}

/// Stop the running agent, if any.
pub fn stop() {
    if let Some(pid) = instance::holder(&instance::lock_path()) {
        // SAFETY: plain kill.
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    }
}
```

- [ ] **Step 8: Wire `--menubar` into the binary** (`crates/tern-app/Cargo.toml`: `tern-menubar = { version = "0.1.2", path = "../tern-menubar" }`; `main.rs` right after the `--server` branch):

```rust
    // The menu bar agent (started by the UI): lists background terminals.
    if std::env::args().nth(1).as_deref() == Some("--menubar") {
        tern_menubar::run();
    }
```

- [ ] **Step 9: Build, lint, smoke-test by hand**

Run: `cargo clippy --workspace --all-targets && cargo test --workspace`
Expected: clean, all pass.

Smoke (debug build): `cargo build -p tern-app && RUST_LOG=info target/debug/tern-app --menubar &`
Expected: a terminal-symbol icon in the menu bar; the menu shows "No background terminals", "Open Tern", "Hide Menu Bar Icon". Start `scripts/dev.sh`, open a shell tab: within ~0.5 s the icon shows `1` and the menu lists it. Starting a second `target/debug/tern-app --menubar` exits at once ("as new runs already" in its output). `kill %1` afterwards. Record what you saw in the task report; if the icon doesn't appear, say so — don't claim it works.

- [ ] **Step 10: Commit**

```bash
git add crates/tern-menubar crates/tern-app/Cargo.toml crates/tern-app/src/main.rs Cargo.lock
git commit -m "Menu bar agent: status item listing background terminals"
```

---

### Task 6: Tern shows a session on request (Reveal, URL, `--open-session`)

**Files:**
- Modify: `crates/tern-app/src/main_window.rs` (`reveal_target`, `reveal_session`, `adopt_orphans`/`adopt_into_first` use the new `adopt_session`)
- Modify: `crates/tern-app/src/workspace.rs` (`adopt_session(&SessionInfo, ..)`, `mux_ids`, `show_tab`, `Services.reveal_events`, `services()`)
- Modify: `crates/tern-app/src/main.rs` (`--open-session`, `tern://session/` URLs, reveal events loop, `session_url_id`)
- Modify: `scripts/bundle-macos.sh` (`CFBundleURLTypes`)

**Interfaces:**
- Consumes: `RevealSink`/`Connector::new` (Task 2), `SessionInfo.{claude, claude_session}` (Task 1).
- Produces:
  ```rust
  // main_window.rs
  #[derive(Debug, PartialEq, Eq)]
  pub enum RevealTarget { Focus { window: usize, ws: usize, tab: usize }, AdoptIntoActive, NewWindow, Nothing }
  /// `tabs[w][ws]` = the mux ids of window `w`'s workspace `ws`'s tabs
  /// (`None` for non-terminal tabs); `known`: the daemon has the session.
  pub fn reveal_target(tabs: &[Vec<Vec<Option<u64>>>], id: u64, known: bool) -> RevealTarget;
  pub fn reveal_session(id: u64, cx: &mut App);
  // workspace.rs
  pub fn adopt_session(&mut self, s: &tern_mux::proto::SessionInfo, window: &mut Window, cx: &mut Context<Self>);
  pub fn mux_ids(&self, cx: &App) -> Vec<Option<u64>>;
  pub fn show_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>);
  Services { .., pub reveal_events: flume::Receiver<u64> }
  // main.rs
  fn session_url_id(url: &str) -> Option<u64>;
  ```

- [ ] **Step 1: Write the failing tests**

`main_window.rs` (add `#[cfg(test)] mod tests` at the end):
```rust
#[cfg(test)]
mod tests {
    use super::{RevealTarget, reveal_target};

    #[test]
    fn reveal_target_focuses_the_tab_showing_it() {
        let tabs = vec![vec![vec![None, Some(1)]], vec![vec![Some(2)], vec![None, None, Some(3)]]];
        assert_eq!(reveal_target(&tabs, 3, true), RevealTarget::Focus { window: 1, ws: 1, tab: 2 });
        assert_eq!(reveal_target(&tabs, 1, true), RevealTarget::Focus { window: 0, ws: 0, tab: 1 });
    }

    #[test]
    fn reveal_target_adopts_what_no_tab_shows() {
        let tabs = vec![vec![vec![Some(1)]]];
        assert_eq!(reveal_target(&tabs, 9, true), RevealTarget::AdoptIntoActive);
        assert_eq!(reveal_target(&[], 9, true), RevealTarget::NewWindow);
    }

    #[test]
    fn reveal_target_unknown_session_does_nothing() {
        // Ended between the menu and the click; a stale tab may still name it.
        assert_eq!(reveal_target(&[vec![vec![Some(9)]]], 9, false), RevealTarget::Nothing);
        assert_eq!(reveal_target(&[], 9, false), RevealTarget::Nothing);
    }
}
```

`main.rs` tests module:
```rust
    #[test]
    fn parses_session_urls() {
        use super::session_url_id;
        assert_eq!(session_url_id("tern://session/42"), Some(42));
        assert_eq!(session_url_id("tern://session/9223372036854775807"), Some(9223372036854775807));
        assert_eq!(session_url_id("tern://session/abc"), None);
        assert_eq!(session_url_id("tern://session/1/x"), None);
        assert_eq!(session_url_id("tern://session/-1"), None);
        assert_eq!(session_url_id("tern://session/99999999999999999999999"), None);
        assert_eq!(session_url_id("tern://session/"), None);
        assert_eq!(session_url_id("file:///tmp"), None);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p tern-app reveal_target parses_session_urls`
Expected: compile errors (functions missing).

- [ ] **Step 3: Implement the pure parts**

`main_window.rs`:
```rust
/// Where Open (from the menu bar) puts session `id`.
#[derive(Debug, PartialEq, Eq)]
pub enum RevealTarget {
    /// A tab shows it already.
    Focus { window: usize, ws: usize, tab: usize },
    /// A new tab in the active window's active workspace.
    AdoptIntoActive,
    /// No window: one opens, with it.
    NewWindow,
    /// It ended meanwhile.
    Nothing,
}

pub fn reveal_target(tabs: &[Vec<Vec<Option<u64>>>], id: u64, known: bool) -> RevealTarget {
    if !known {
        return RevealTarget::Nothing;
    }
    for (window, wss) in tabs.iter().enumerate() {
        for (ws, ids) in wss.iter().enumerate() {
            if let Some(tab) = ids.iter().position(|t| *t == Some(id)) {
                return RevealTarget::Focus { window, ws, tab };
            }
        }
    }
    if tabs.is_empty() { RevealTarget::NewWindow } else { RevealTarget::AdoptIntoActive }
}
```

`main.rs`:
```rust
/// The session id in a `tern://session/<id>` URL (from the menu bar agent).
fn session_url_id(url: &str) -> Option<u64> {
    let id = url.strip_prefix("tern://session/")?;
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    id.parse().ok()
}
```

- [ ] **Step 4: Run the pure tests**

Run: `cargo test -p tern-app reveal_target parses_session_urls`
Expected: PASS.

- [ ] **Step 5: Workspace helpers** (`workspace.rs`)

```rust
    /// A tab for background session `s` that no tab shows. A Claude one
    /// resumes its conversation if it has to start again.
    pub fn adopt_session(&mut self, s: &tern_mux::proto::SessionInfo, window: &mut Window, cx: &mut Context<Self>) {
        if s.claude {
            // Claude names its tab itself.
            self.add_term(TabKind::Claude, None, s.claude_session.clone(), Some(s.id), window, cx);
        } else {
            self.add_term(TabKind::Shell, s.title.clone(), None, Some(s.id), window, cx);
        }
    }

    /// Each tab's background session id (`None` for other tabs), by index.
    pub fn mux_ids(&self, cx: &App) -> Vec<Option<u64>> {
        self.tabs.iter().map(|t| t.term().and_then(|t| t.view.read(cx).mux_id())).collect()
    }

    /// Make tab `ix` the active one.
    pub fn show_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.activate(ix, window, cx);
    }
```
  `Services` gains `pub reveal_events: flume::Receiver<u64>` (doc: "Sessions the menu bar agent asked to show."). `services()`:
  ```rust
  let (reveal_tx, reveal_rx) = flume::unbounded();
  let reveal: tern_mux::client::RevealSink = Arc::new(move |id| {
      let _ = reveal_tx.send(id);
  });
  let mux = Arc::new(Connector::new(tern_mux::paths::mux_dir(), true, hooks, reveal));
  Services { mux, hook_events: rx, reveal_events: reveal_rx, terminal }
  ```

- [ ] **Step 6: `reveal_session`** (`main_window.rs`), and update `adopt_into_first` to call `w.adopt_session(s, window, cx)`:

```rust
/// Show background session `id` (asked from the menu bar): focus the tab
/// showing it, else adopt it into the active workspace, else into a new
/// window at its folder. Nothing if it has ended.
pub fn reveal_session(id: u64, cx: &mut App) {
    let info = cx.global::<AppServices>().0.mux.existing().and_then(|c| c.list().ok()).and_then(|l| l.into_iter().find(|s| s.id == id));
    let wins = windows(cx);
    let tabs: Vec<Vec<Vec<Option<u64>>>> =
        wins.iter().map(|(_, m)| m.read(cx).slots.iter().map(|s| s.ws.read(cx).mux_ids(cx)).collect()).collect();
    match reveal_target(&tabs, id, info.is_some()) {
        RevealTarget::Nothing => log::info!("session {id} is gone; nothing to show"),
        RevealTarget::Focus { window: w, ws, tab } => {
            let (handle, main) = wins[w].clone();
            let _ = handle.update(cx, |_, window, cx| {
                main.update(cx, |m, cx| {
                    m.activate(ws, window, cx);
                    if let Some(ws) = m.active_ws().cloned() {
                        ws.update(cx, |w, cx| w.show_tab(tab, window, cx));
                    }
                });
                window.activate_window();
            });
        },
        RevealTarget::AdoptIntoActive | RevealTarget::NewWindow => {
            let Some(info) = info else { return };
            let target = cx.active_window().and_then(|a| wins.iter().find(|(h, _)| *h == a).cloned()).or_else(|| wins.last().cloned());
            let has_ws = target.as_ref().is_some_and(|(_, m)| m.read(cx).active_ws().is_some());
            if !has_ws {
                // No window, or a welcome page: open its folder there first.
                let root = info.cwd.clone().unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default());
                open_folder(root, cx);
            }
            let wins = windows(cx);
            let target = cx.active_window().and_then(|a| wins.iter().find(|(h, _)| *h == a).cloned()).or_else(|| wins.last().cloned());
            let Some((handle, main)) = target else { return };
            let _ = handle.update(cx, |_, window, cx| {
                if let Some(ws) = main.read(cx).active_ws().cloned() {
                    ws.update(cx, |w, cx| w.adopt_session(&info, window, cx));
                }
                window.activate_window();
            });
        },
    }
    cx.activate(true);
}
```
  If the borrow checker objects to `wins[w].clone()` or reading entities inside `handle.update`, restructure in the same shape as `adopt_into_first`. `open_folder` on a new window: the window is in `Registry` right after `open_window`, so `wins.last()` finds it.

- [ ] **Step 7: Entry points** (`main.rs`)

  - Arguments: before `let arg: Option<PathBuf> = …`, read `--open-session`:
    ```rust
    // `--open-session <id>`: the menu bar agent's Open in debug builds
    // (release ones get a `tern://session/<id>` URL).
    let open_session = match std::env::args().nth(1).as_deref() {
        Some("--open-session") => std::env::args().nth(2).and_then(|s| s.parse::<u64>().ok()),
        _ => None,
    };
    let arg: Option<PathBuf> = if open_session.is_some() { None } else { std::env::args().nth(1).map(PathBuf::from) };
    ```
  - After the `match arg { … }` startup block: `if let Some(id) = open_session { main_window::reveal_session(id, cx); }`
  - URL loop: handle session URLs before folders:
    ```rust
    for url in &urls {
        if let Some(id) = session_url_id(url) {
            main_window::reveal_session(id, cx);
        }
    }
    for path in urls.iter().filter_map(|u| file_url_path(u)).filter(|p| p.is_dir()) { … }
    ```
  - Reveal events from the daemon (next to the hook events loop):
    ```rust
    let reveal_events = services.reveal_events.clone();
    cx.spawn(async move |cx| {
        while let Ok(id) = reveal_events.recv_async().await {
            cx.update(|cx| main_window::reveal_session(id, cx));
        }
    })
    .detach();
    ```
  - `scripts/bundle-macos.sh` Info.plist, after `CFBundleDocumentTypes`'s `</array>`:
    ```xml
    <key>CFBundleURLTypes</key>
    <array>
        <dict>
            <key>CFBundleURLName</key><string>dev.tern.session</string>
            <key>CFBundleURLSchemes</key><array><string>tern</string></array>
        </dict>
    </array>
    ```
  - In `forward_to_running` nothing changes: a release `open tern://…` reaches the running app through LaunchServices, not this path.

- [ ] **Step 8: Verify**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets && sh -n scripts/bundle-macos.sh`
Expected: pass, clean.

By hand (debug): `scripts/dev.sh`; open two shell tabs in workspace A, switch to workspace B; with the Task 5 agent running (`target/debug/tern-app --menubar &`), Open the first tab's session → workspace A and that tab become active. Quit Tern (⌘Q), Open a session from the menu → a debug Tern starts (`--open-session`) and shows it (restored as a tab already → focused). Record in the task report exactly which of these you checked and what you saw.

- [ ] **Step 9: Commit**

```bash
git add crates/tern-app scripts/bundle-macos.sh
git commit -m "App: show a background session on request (Reveal, tern:// URL, --open-session)"
```

---

### Task 7: Agent lifecycle, Show/Hide, docs

**Files:**
- Modify: `crates/tern-app/src/main.rs` (launch at startup, Tern-menu toggle, `ToggleMenuBarIcon` action)
- Modify: `README.md` (background section)
- Modify: `~/ws/tern-ws/HANDOFF-background-daemon.md` (state of the menu bar work; outside the repo, not committed)

**Interfaces:**
- Consumes: `tern_menubar::{launch, stop, setting}` (Tasks 4–5).
- Produces: action `tern::ToggleMenuBarIcon`; `fn set_app_menus(cx: &mut App)` in `main.rs`.

- [ ] **Step 1: Write the failing test** (`main.rs` tests) — the menu label follows the setting:

```rust
    #[test]
    fn menu_bar_toggle_label_follows_the_setting() {
        assert_eq!(super::menu_bar_toggle_label(true), "Hide Menu Bar Icon");
        assert_eq!(super::menu_bar_toggle_label(false), "Show Menu Bar Icon");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p tern-app menu_bar_toggle_label`
Expected: compile error.

- [ ] **Step 3: Implement** (`main.rs`)

```rust
actions!(tern, [Quit, OpenFolder, ToggleMenuBarIcon]);

fn menu_bar_toggle_label(shown: bool) -> &'static str {
    if shown { "Hide Menu Bar Icon" } else { "Show Menu Bar Icon" }
}

/// The Tern menu (rebuilt when the menu bar setting changes).
fn set_app_menus(cx: &mut App) {
    let shown = tern_menubar::setting::enabled(&tern_menubar::setting::file());
    cx.set_menus([Menu::new("Tern").items([
        MenuItem::action("Check for Updates…", update::CheckForUpdates),
        MenuItem::action(menu_bar_toggle_label(shown), ToggleMenuBarIcon),
        MenuItem::separator(),
        MenuItem::os_submenu("Services", SystemMenuType::Services),
        MenuItem::separator(),
        MenuItem::action("Quit Tern", Quit),
    ])]);
}

fn toggle_menu_bar_icon(cx: &mut App) {
    let file = tern_menubar::setting::file();
    let show = !tern_menubar::setting::enabled(&file);
    if let Err(e) = tern_menubar::setting::set_enabled(&file, show) {
        log::warn!("can't save the menu bar setting: {e}");
        return;
    }
    if show { tern_menubar::launch() } else { tern_menubar::stop() }
    set_app_menus(cx);
}
```
  In `app.run`: replace the `cx.set_menus([...])` block with `set_app_menus(cx);`, add `cx.on_action(|_: &ToggleMenuBarIcon, cx| toggle_menu_bar_icon(cx));`. After `cx.set_global(main_window::Session { … })`:
  ```rust
  // The menu bar agent outlives us; a newer one replaces an older one.
  if !scripted && tern_menubar::setting::enabled(&tern_menubar::setting::file()) {
      tern_menubar::launch();
  }
  ```
  The agent's own "Hide Menu Bar Icon" writes the same file, so the Tern menu label is right the next time it is rebuilt; also call `set_app_menus(cx)` when the app becomes active if gpui offers a hook cheaply — otherwise accept a stale label until the next toggle/launch and note it in the handoff.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets && cargo fmt --check`
Expected: pass, clean.

- [ ] **Step 5: README** — after the background-process paragraph (line ~66) add:

```markdown
A menu bar icon lists every terminal running in the background, also while Tern is closed: the count beside it, a dot when one waits for you. Pick one to open it in Tern (in the active workspace, or a new window) or end it. Hide it from its own menu or with Tern ▸ Hide Menu Bar Icon. Its log is `~/Library/Logs/Tern/menubar.log`.
```
  And in the crates line add `tern-menubar` (menu bar agent).

- [ ] **Step 6: By-hand check on the installed app** (record results in the handoff, each as seen / not checked):
  `scripts/install-macos.sh`, then: icon present with Tern open and after ⌘Q; count and dot (start a Claude tab waiting for permission); Open into the active workspace; Open with no window; Open while Tern is closed (URL path starts Tern); End… asks and ends; Hide from the agent and from the Tern menu, Show again; agent shows no Dock icon; a second launch doesn't add a second icon.

- [ ] **Step 7: Commit**

```bash
git add crates/tern-app/src/main.rs README.md
git commit -m "Menu bar agent starts with Tern; Show/Hide in the Tern menu"
```
  Update `~/ws/tern-ws/HANDOFF-background-daemon.md` (what was built, what was checked by hand, deviations below).

---

## Deviations from the spec (decided while planning)

- **Icon:** the SF Symbol `terminal` as a template image, not an image from `assets/icon` (there is no monochrome menu bar asset).
- **Session items:** an item with a submenu can't also act on click in AppKit, so each session opens a submenu `Open` / `End…` (the spec had "click = Open" plus a submenu).
- **Menu rebuild:** on every pushed list (≤ 4×/s) instead of in `menuNeedsUpdate:` — no delegate needed; an open menu updates live.
- **Change detection:** the daemon compares the list every 250 ms (also catches title/cwd changes, which have no event) rather than reacting to individual events; same observable contract.
- **`SessionInfo.program`** added (first foreground program) for the row label the spec describes.
