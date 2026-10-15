use std::collections::HashMap;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tern_mux::client::{MuxClient, Open, RemoteSession};
use tern_mux::paths;
use tern_mux::proto::{self, ClientMsg, MAJOR, MINOR};
use tern_mux::server::{ServeOpts, serve};
use tern_term::vt::grid::Dimensions;
use tern_term::vt::index::Line;
use tern_term::{SpawnConfig, TermEvent, TermSize};

const SIZE: TermSize = TermSize { cols: 60, rows: 10, cell_width: 8, cell_height: 16 };

struct Daemon {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    thread: Option<std::thread::JoinHandle<std::io::Result<()>>>,
}

fn daemon(idle: Duration) -> Daemon {
    // Short path: Unix socket paths are limited to 104 bytes.
    let tmp = tempfile::Builder::new().prefix("tm").tempdir_in("/tmp").unwrap();
    let dir = tmp.path().join("m");
    let opts = ServeOpts { idle_exit: idle, notifications: false, run_root: tmp.path().join("run"), hooks: true, max_queued: 1 << 20 };
    let d = dir.clone();
    let thread = std::thread::spawn(move || serve(&d, opts));
    wait_until("socket", || paths::socket_path(&dir).exists());
    Daemon { _tmp: tmp, dir, thread: Some(thread) }
}

fn client(dir: &Path) -> MuxClient {
    MuxClient::connect(dir, false, Arc::new(|_, _| {})).unwrap()
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

type Events = Arc<Mutex<Vec<TermEvent>>>;

fn sink() -> (tern_term::EventSink, Events) {
    let ev: Events = Arc::default();
    let e = ev.clone();
    (Arc::new(move |x| e.lock().unwrap().push(x)), ev)
}

fn sh(script: &str) -> SpawnConfig {
    SpawnConfig {
        program: Some("/bin/sh".into()),
        args: vec!["-c".into(), script.into()],
        cwd: Some("/tmp".into()),
        env: HashMap::new(),
        size: SIZE,
        scrollback: 1000,
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

fn ready_id(ev: &Events) -> Option<u64> {
    ev.lock().unwrap().iter().find_map(|e| match e {
        TermEvent::Ready { id, .. } => Some(*id),
        _ => None,
    })
}

fn alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[test]
fn spawn_detach_reattach() {
    let d = daemon(Duration::from_secs(60));
    let c = client(&d.dir);
    let (sink1, ev1) = sink();
    let s = c.open(Open::Spawn { cfg: sh("echo before; read x; echo after:$x; read y"), hook: false }, SIZE, 1000, sink1);
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
    let _s = c.open(Open::Spawn { cfg, hook: false }, SIZE, 1000, sink);
    wait_until("error", || ev.lock().unwrap().iter().any(|e| matches!(e, TermEvent::SpawnError(_) | TermEvent::Ready { .. })));
}

#[test]
fn hook_settings_are_substituted() {
    let d = daemon(Duration::from_secs(60));
    let c = client(&d.dir);
    let (sink, ev) = sink();
    let s = c.open(Open::Spawn { cfg: sh("cat {tern-settings}; read x"), hook: true }, SIZE, 1000, sink);
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
    let s = c.open(Open::Spawn { cfg: sh("read x; exit 4"), hook: false }, SIZE, 1000, sink1);
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
    let s = c.open(Open::Spawn { cfg: sh("read x"), hook: false }, SIZE, 1000, sink);
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
    };
    assert_eq!(serve(&d.dir, opts).unwrap_err().kind(), std::io::ErrorKind::AddrInUse);
}

#[test]
fn slow_client_is_dropped() {
    let d = daemon(Duration::from_secs(60));
    // A client that attaches to a flood and never reads.
    let mut raw = UnixStream::connect(paths::socket_path(&d.dir)).unwrap();
    proto::write_frame(&mut raw, &ClientMsg::Hello { major: MAJOR, minor: MINOR, app_version: "t".into() }).unwrap();
    proto::write_frame(&mut raw, &ClientMsg::Spawn { req: 1, cfg: sh("yes 0123456789012345678901234567890123456789"), hook: false })
        .unwrap();
    raw.flush().unwrap();
    // A well-behaved client keeps working meanwhile.
    let c = client(&d.dir);
    wait_until("flood session", || !c.list().unwrap().is_empty());
    let (sink, ev) = sink();
    let s = c.open(Open::Spawn { cfg: sh("echo fine; read x"), hook: false }, SIZE, 1000, sink);
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
    let conn = tern_mux::client::Connector::new(d.dir.clone(), false, Arc::new(|_, _| {}));
    assert!(!conn.connected());
    let (sink, ev) = sink();
    let s = conn.get().unwrap().open(Open::Spawn { cfg: sh("read x"), hook: false }, SIZE, 1000, sink);
    wait_until("ready", || ready_id(&ev).is_some());
    assert!(conn.connected());
    // Same connection: the session is visible to the next `get`.
    assert!(conn.get().unwrap().list().unwrap().iter().any(|i| Some(i.id) == s.id()));
    s.close();
}

#[test]
fn connector_without_daemon_fails() {
    let tmp = tempfile::Builder::new().prefix("tm").tempdir_in("/tmp").unwrap();
    let conn = tern_mux::client::Connector::new(tmp.path().to_owned(), false, Arc::new(|_, _| {}));
    assert!(conn.get().is_err());
}

#[test]
fn attach_to_an_exited_session_reports_the_exit() {
    let d = daemon(Duration::from_secs(60));
    let c = client(&d.dir);
    let (sink1, ev1) = sink();
    let s = c.open(Open::Spawn { cfg: sh("echo bye; sleep 0.3; exit 5"), hook: false }, SIZE, 1000, sink1);
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
