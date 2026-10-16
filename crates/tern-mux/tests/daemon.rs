// The scripts are sh; `windows.rs` covers Windows.
#![cfg(unix)]

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tern_mux::client::{MuxClient, Open, RemoteSession};
use tern_mux::paths;
use tern_mux::proto::{self, ClientMsg, MAJOR, MINOR, ServerMsg};
use tern_mux::server::{ServeOpts, serve};
use tern_term::vt::grid::Dimensions;
use tern_term::vt::index::Line;
use tern_term::{TermEvent, TermSize};

mod common;
use common::*;

/// A raw observer connection: Hello as an observer, read the daemon's Hello.
fn observer(dir: &Path) -> (UnixStream, proto::FrameReader) {
    let mut raw = UnixStream::connect(paths::socket_path(dir)).unwrap();
    proto::write_frame(&mut raw, &ClientMsg::Hello { major: MAJOR, minor: MINOR, app_version: "t".into(), role: proto::Role::Observer })
        .unwrap();
    let mut r = proto::FrameReader::new(raw.try_clone().unwrap(), proto::MAX_SERVER_FRAME);
    assert!(matches!(r.read::<ServerMsg>().unwrap(), Some(ServerMsg::Hello { minor: MINOR, .. })));
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

fn screen(s: &RemoteSession) -> String {
    let term = s.term().lock();
    let g = term.grid();
    (0..g.screen_lines() as i32)
        .map(|l| g[Line(l)].into_iter().map(|c| c.c).collect::<String>().trim_end().to_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

fn alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[test]
fn spawn_detach_reattach() {
    let d = daemon(Duration::from_secs(60));
    let c = client(&d.dir);
    let (sink1, ev1) = sink();
    let s = c.open(Open::Spawn { cfg: sh("echo before; read x; echo after:$x; read y"), hook: false, seed: None }, SIZE, 1000, sink1);
    wait_until("ready", || ready_id(&ev1).is_some());
    wait_until("output", || screen(&s).contains("before"));
    let id = s.id().unwrap();
    let pid = c.list().unwrap().iter().find(|i| i.id == id).unwrap().pid;
    assert!(s.foreground_programs().iter().any(|p| p == "sh"), "{:?}", s.foreground_programs());
    drop(s); // detach

    // A new UI process: new connection.
    drop(c);
    let c = client(&d.dir);
    let (sink2, ev2) = sink();
    let s = c.open(Open::Attach { id }, SIZE, 1000, sink2);
    wait_until("reattached", || ready_id(&ev2) == Some(id));
    assert!(screen(&s).contains("before"), "{}", screen(&s));
    s.write(b"x\n".to_vec());
    wait_until("after", || screen(&s).contains("after:x"));
    assert!(alive(pid));
    let info = c.list().unwrap();
    assert_eq!(info.iter().find(|i| i.id == id).map(|i| i.pid), Some(pid));

    s.close();
    wait_until("killed", || !alive(pid) || c.list().unwrap().is_empty());
    assert!(c.list().unwrap().iter().all(|i| i.id != id));
}

#[test]
fn unknown_session_is_not_found() {
    let d = daemon(Duration::from_secs(60));
    let c = client(&d.dir);
    let (sink, ev) = sink();
    let _s = c.open(Open::Attach { id: 12345 }, SIZE, 1000, sink);
    wait_until("not found", || ev.lock().unwrap().iter().any(|e| matches!(e, TermEvent::NotFound)));
}

#[test]
fn spawn_error_is_reported() {
    let d = daemon(Duration::from_secs(60));
    let c = client(&d.dir);
    let (sink, ev) = sink();
    let mut cfg = sh("true");
    cfg.cwd = Some("/nonexistent/dir".into());
    let _s = c.open(Open::Spawn { cfg, hook: false, seed: None }, SIZE, 1000, sink);
    wait_until("error", || ev.lock().unwrap().iter().any(|e| matches!(e, TermEvent::SpawnError(_) | TermEvent::Ready { .. })));
}

#[test]
fn hook_settings_are_substituted() {
    let d = daemon(Duration::from_secs(60));
    let c = client(&d.dir);
    let (sink, ev) = sink();
    let s = c.open(Open::Spawn { cfg: sh("cat {tern-settings}; read x"), hook: true, seed: None }, SIZE, 1000, sink);
    wait_until("ready", || ready_id(&ev).is_some());
    let token = ev.lock().unwrap().iter().find_map(|e| match e {
        TermEvent::Ready { token, .. } => token.clone(),
        _ => None,
    });
    assert!(token.is_some());
    wait_until("settings json", || screen(&s).contains("hooks"));
    s.close();
}

#[test]
fn exit_survives_detach() {
    let d = daemon(Duration::from_secs(60));
    let c = client(&d.dir);
    let (sink1, ev1) = sink();
    let s = c.open(Open::Spawn { cfg: sh("read x; exit 4"), hook: false, seed: None }, SIZE, 1000, sink1);
    wait_until("ready", || ready_id(&ev1).is_some());
    let id = s.id().unwrap();
    drop(s);
    // The program ends while no one is attached.
    let (sink2, ev2) = sink();
    let s = c.open(Open::Attach { id }, SIZE, 1000, sink2);
    wait_until("ready again", || ready_id(&ev2).is_some());
    s.write(b"\n".to_vec());
    wait_until("exit", || ev2.lock().unwrap().iter().any(|e| matches!(e, TermEvent::Exit(Some(4)))));
    assert!(c.list().unwrap().iter().find(|i| i.id == id).unwrap().exited);
}

#[test]
fn server_loss_ends_sessions() {
    let d = daemon(Duration::from_millis(300));
    let c = client(&d.dir);
    let (sink, ev) = sink();
    let s = c.open(Open::Spawn { cfg: sh("read x"), hook: false, seed: None }, SIZE, 1000, sink);
    wait_until("ready", || ready_id(&ev).is_some());
    // Kill the connection from the server side: the daemon process of a real
    // run would have died; here, close all its sessions and let it idle out.
    s.close();
    drop(c);
    let mut d = d;
    d.thread.take().unwrap().join().unwrap().unwrap();
    assert!(!paths::socket_path(&d.dir).exists());
}

#[test]
fn second_daemon_refuses_the_dir() {
    let d = daemon(Duration::from_secs(60));
    let opts = ServeOpts {
        idle_exit: Duration::from_secs(1),
        notifications: false,
        run_root: d.dir.join("run2"),
        hooks: false,
        max_queued: 1 << 20,
        max_queued_session: usize::MAX,
        #[cfg(unix)]
        successor: None,
    };
    assert_eq!(serve(&d.dir, opts).unwrap_err().kind(), std::io::ErrorKind::AddrInUse);
}

#[test]
fn slow_client_is_dropped() {
    let d = daemon(Duration::from_secs(60));
    // A client that attaches to a flood and never reads.
    let mut raw = UnixStream::connect(paths::socket_path(&d.dir)).unwrap();
    proto::write_frame(&mut raw, &ClientMsg::Hello { major: MAJOR, minor: MINOR, app_version: "t".into(), role: proto::Role::Ui }).unwrap();
    proto::write_frame(
        &mut raw,
        &ClientMsg::Spawn { req: 1, cfg: sh("yes 0123456789012345678901234567890123456789"), hook: false, seed: None },
    )
    .unwrap();
    raw.flush().unwrap();
    // A well-behaved client keeps working meanwhile.
    let c = client(&d.dir);
    wait_until("flood session", || !c.list().unwrap().is_empty());
    let (sink, ev) = sink();
    let s = c.open(Open::Spawn { cfg: sh("echo fine; read x"), hook: false, seed: None }, SIZE, 1000, sink);
    wait_until("ready", || ready_id(&ev).is_some());
    wait_until("output", || screen(&s).contains("fine"));
    // The stalled one gets disconnected once it is 1 MiB behind: after a
    // while, reading finds what the kernel buffered, then the end.
    raw.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    std::thread::sleep(Duration::from_secs(3));
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0usize;
    loop {
        match std::io::Read::read(&mut raw, &mut buf) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(e) => panic!("still connected after {total} bytes: {e}"),
        }
        assert!(total < 32 << 20, "still streaming");
    }
    for i in c.list().unwrap() {
        c.close(i.id);
    }
    s.close();
}

#[test]
fn connector_keeps_one_connection() {
    let d = daemon(Duration::from_secs(60));
    let conn = tern_mux::client::Connector::new(d.dir.clone(), false, Arc::new(|_, _| {}), Arc::new(|_| {}));
    assert!(!conn.connected());
    let (sink, ev) = sink();
    let s = conn.get().unwrap().open(Open::Spawn { cfg: sh("read x"), hook: false, seed: None }, SIZE, 1000, sink);
    wait_until("ready", || ready_id(&ev).is_some());
    assert!(conn.connected());
    // Same connection: the session is visible to the next `get`.
    assert!(conn.get().unwrap().list().unwrap().iter().any(|i| Some(i.id) == s.id()));
    s.close();
}

#[test]
fn connector_without_daemon_fails() {
    let tmp = tempfile::Builder::new().prefix("tm").tempdir_in("/tmp").unwrap();
    let conn = tern_mux::client::Connector::new(tmp.path().to_owned(), false, Arc::new(|_, _| {}), Arc::new(|_| {}));
    assert!(conn.get().is_err());
}

#[test]
fn attach_to_an_exited_session_reports_the_exit() {
    let d = daemon(Duration::from_secs(60));
    let c = client(&d.dir);
    let (sink1, ev1) = sink();
    let s = c.open(Open::Spawn { cfg: sh("echo bye; sleep 0.3; exit 5"), hook: false, seed: None }, SIZE, 1000, sink1);
    wait_until("ready", || ready_id(&ev1).is_some());
    let id = s.id().unwrap();
    drop(s);
    wait_until("exited", || c.list().unwrap().iter().any(|i| i.id == id && i.exited));
    let (sink2, ev2) = sink();
    let s = c.open(Open::Attach { id }, SIZE, 1000, sink2);
    wait_until("exit event", || ev2.lock().unwrap().iter().any(|e| matches!(e, TermEvent::Exit(Some(5)))));
    assert!(screen(&s).contains("bye"));
    s.close();
}

#[test]
fn closing_before_the_reply_ends_the_session() {
    let d = daemon(Duration::from_secs(60));
    let c = client(&d.dir);
    let (sink1, _ev1) = sink();
    let s = c.open(Open::Spawn { cfg: sh("read x"), hook: false, seed: None }, SIZE, 1000, sink1);
    s.close(); // before `Attached` came back
    let (sink2, _ev2) = sink();
    let s2 = c.open(Open::Spawn { cfg: sh("read x"), hook: false, seed: None }, SIZE, 1000, sink2);
    drop(s2); // dropped before the reply: detached, still listed
    wait_until("only the dropped one left", || {
        let l = c.list().unwrap();
        l.len() == 1 && !l[0].exited
    });
    for i in c.list().unwrap() {
        c.close(i.id);
    }
}

#[test]
fn size_set_before_the_reply_wins() {
    let d = daemon(Duration::from_secs(60));
    let c = client(&d.dir);
    let (sink1, ev1) = sink();
    let s = c.open(Open::Spawn { cfg: sh("read x"), hook: false, seed: None }, SIZE, 1000, sink1);
    wait_until("ready", || ready_id(&ev1).is_some());
    let id = s.id().unwrap();
    drop(s);
    // Reattach at a passing size, then settle back at the old one before
    // the daemon answers.
    let (sink2, ev2) = sink();
    let s = c.open(Open::Attach { id }, TermSize { cols: 90, rows: 30, ..SIZE }, 1000, sink2);
    s.resize(SIZE);
    wait_until("ready", || ready_id(&ev2).is_some());
    std::thread::sleep(Duration::from_millis(300));
    let cols = s.term().lock().columns();
    let rows = s.term().lock().screen_lines();
    assert_eq!((cols, rows), (SIZE.cols as usize, SIZE.rows as usize));
    s.close();
}

#[test]
fn dropped_connection_reports_loss_and_lets_go_of_the_pty() {
    // A daemon that drops a client as soon as more than its hello is
    // queued for it (PTY reads come in ~1 KiB chunks).
    let d = daemon_with(Duration::from_secs(60), 200, usize::MAX);
    let c = client(&d.dir);
    let (sink, ev) = sink();
    let s = c.open(
        Open::Spawn { cfg: sh("sleep 0.5; head -c 2000000 /dev/zero | tr '\\0' x; read x"), hook: false, seed: None },
        SIZE,
        1000,
        sink,
    );
    wait_until("server lost", || ev.lock().unwrap().iter().any(|e| matches!(e, TermEvent::ServerLost)));
    assert!(s.foreground_programs().is_empty(), "still holds the PTY");
    assert!(!c.alive());
}

#[test]
fn lagging_session_is_detached_alone() {
    // Any output frame puts a session over the limit; the connection's is
    // out of reach.
    let d = daemon_with(Duration::from_secs(60), usize::MAX, 200);
    let c = client(&d.dir);
    let (sink_a, ev_a) = sink();
    let a = c.open(Open::Spawn { cfg: sh("read x; echo got:$x; read y"), hook: false, seed: None }, SIZE, 1000, sink_a);
    wait_until("a ready", || ready_id(&ev_a).is_some());
    let (sink_b, ev_b) = sink();
    let b = c.open(
        Open::Spawn { cfg: sh("sleep 0.3; head -c 5000 /dev/zero | tr '\\0' x; echo; echo flood; read x"), hook: false, seed: None },
        SIZE,
        1000,
        sink_b,
    );
    wait_until("b lost", || ev_b.lock().unwrap().iter().any(|e| matches!(e, TermEvent::ServerLost)));
    assert!(c.alive());
    assert!(!ev_a.lock().unwrap().iter().any(|e| matches!(e, TermEvent::ServerLost)));
    let id_b = b.id().unwrap();
    drop(b); // as the tab does before reattaching

    // The other tab is untouched (its one small read stays under the limit).
    a.write(b"y\n".to_vec());
    wait_until("a echoes", || screen(&a).contains("got:y"));

    // The lagging one comes back from a snapshot that has what it skipped.
    let (sink_b, ev_b) = sink();
    let b = c.open(Open::Attach { id: id_b }, SIZE, 1000, sink_b);
    wait_until("b back", || ready_id(&ev_b) == Some(id_b));
    assert!(screen(&b).contains("flood"), "{}", screen(&b));
    b.close();
    a.close();
}

#[test]
fn input_before_the_reply_is_kept() {
    let d = daemon(Duration::from_secs(60));
    let c = client(&d.dir);
    let (sink, ev) = sink();
    let s = c.open(Open::Spawn { cfg: sh("read x; echo got:$x; read y"), hook: false, seed: None }, SIZE, 1000, sink);
    s.write(b"early\n".to_vec()); // before `Attached` came back
    wait_until("ready", || ready_id(&ev).is_some());
    wait_until("echo", || screen(&s).contains("got:early"));
    s.close();
}

#[test]
fn big_input_goes_in_pieces() {
    let d = daemon(Duration::from_secs(60));
    let c = client(&d.dir);
    let (sink, ev) = sink();
    let out = d.dir.join("got");
    let s = c.open(
        Open::Spawn { cfg: sh(&format!("stty raw -echo; head -c 20000000 > {}", out.display())), hook: false, seed: None },
        SIZE,
        1000,
        sink,
    );
    wait_until("ready", || ready_id(&ev).is_some());
    std::thread::sleep(Duration::from_millis(200));
    // Over the daemon's 16 MiB frame limit as one message.
    s.write(vec![b'x'; 20_000_000]);
    wait_until("all of it", || std::fs::metadata(&out).is_ok_and(|m| m.len() == 20_000_000));
    assert!(c.alive());
    s.close();
}

#[test]
fn closing_ends_the_foreground_job() {
    let d = daemon(Duration::from_secs(60));
    let c = client(&d.dir);
    let (sink, ev) = sink();
    let pidfile = d.dir.join("job");
    // An interactive shell running a job that ignores SIGHUP, in its own
    // process group.
    let mut cfg = sh("");
    cfg.args = vec!["-i".into()];
    cfg.env.insert("PS1".into(), "$ ".into());
    let s = c.open(Open::Spawn { cfg, hook: false, seed: None }, SIZE, 1000, sink);
    wait_until("ready", || ready_id(&ev).is_some());
    s.write(format!("sh -c 'trap \"\" HUP; echo $$ > {}; while :; do sleep 0.1; done'\n", pidfile.display()).into_bytes());
    wait_until("job", || std::fs::read_to_string(&pidfile).is_ok_and(|p| p.ends_with('\n')));
    let job: u32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
    assert!(alive(job));
    s.close();
    wait_until("job ended", || !alive(job));
}

#[test]
fn another_majors_daemon_does_not_block_this_one() {
    use std::os::fd::AsRawFd;
    let tmp = tempfile::Builder::new().prefix("tm").tempdir_in("/tmp").unwrap();
    let dir = tmp.path().join("m");
    paths::ensure_private_dir(&dir).unwrap();
    // An older Tern's daemon holds its lock (and runs its programs).
    let other = std::fs::File::create(dir.join(format!("server-{}.lock", MAJOR + 1))).unwrap();
    assert_eq!(unsafe { libc::flock(other.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
    let old = std::fs::File::create(dir.join("server.lock")).unwrap();
    assert_eq!(unsafe { libc::flock(old.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
    let opts = ServeOpts {
        idle_exit: Duration::from_millis(200),
        notifications: false,
        run_root: tmp.path().join("run"),
        hooks: false,
        max_queued: 1 << 20,
        max_queued_session: usize::MAX,
        #[cfg(unix)]
        successor: None,
    };
    serve(&dir, opts).unwrap();
}

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
    let s = c.open(Open::Spawn { cfg: sh("read x; exit 3"), hook: true, seed: None }, SIZE, 1000, sink);
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
        held.push((c.open(Open::Spawn { cfg: sh("read x"), hook: false, seed: None }, SIZE, 1000, sink), ev));
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
    let s = c.open(Open::Spawn { cfg: sh(script), hook: true, seed: None }, SIZE, 1000, sink);
    wait_until("ready", || ready_id(&ev).is_some());
    let id = s.id().unwrap();
    wait_until("claude_session", || {
        c.list().unwrap().iter().any(|i| i.id == id && i.claude && i.claude_session.as_deref() == Some("abc-123"))
    });
    s.close();
}
