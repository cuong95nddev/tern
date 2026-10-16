#![cfg(windows)]
//! The daemon on Windows: AF_UNIX socket, ConPTY, PowerShell scripts.

use std::time::Duration;

use tern_mux::client::{Open, RemoteSession};
use tern_mux::proto::MAJOR;
use tern_mux::server::{ServeOpts, serve};
use tern_term::TermEvent;
use tern_term::vt::grid::Dimensions;
use tern_term::vt::index::Line;

mod common;
use common::*;

fn screen(s: &RemoteSession) -> String {
    let term = s.term().lock();
    let g = term.grid();
    (0..g.screen_lines() as i32)
        .map(|l| g[Line(l)].into_iter().map(|c| c.c).collect::<String>().trim_end().to_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn spawn_detach_reattach() {
    let d = daemon(Duration::from_secs(60));
    let c = client(&d.dir);
    let (sink1, ev1) = sink();
    let s = c.open(
        Open::Spawn { cfg: ps("Write-Host before; $x = Read-Host; Write-Host after:$x; Read-Host"), hook: false },
        SIZE,
        1000,
        sink1,
    );
    wait_until("ready", || ready_id(&ev1).is_some());
    wait_until("output", || screen(&s).contains("before"));
    let id = s.id().unwrap();
    assert!(s.foreground_programs().iter().any(|p| p == "powershell"), "{:?}", s.foreground_programs());
    drop(s); // detach

    // A new UI process: new connection.
    drop(c);
    let c = client(&d.dir);
    let (sink2, ev2) = sink();
    let s = c.open(Open::Attach { id }, SIZE, 1000, sink2);
    wait_until("reattached", || ready_id(&ev2) == Some(id));
    assert!(screen(&s).contains("before"), "{}", screen(&s));
    s.write(b"x\r".to_vec());
    wait_until("after", || screen(&s).contains("after:x"));

    s.close();
    wait_until("closed", || c.list().unwrap().iter().all(|i| i.id != id));
}

#[test]
fn exit_code_is_reported() {
    let d = daemon(Duration::from_secs(60));
    let c = client(&d.dir);
    let (sink, ev) = sink();
    let s = c.open(Open::Spawn { cfg: ps("$null = Read-Host; exit 4"), hook: false }, SIZE, 1000, sink);
    wait_until("ready", || ready_id(&ev).is_some());
    s.write(b"\r".to_vec());
    wait_until("exit", || ev.lock().unwrap().iter().any(|e| matches!(e, TermEvent::Exit(Some(4)))));
}

#[test]
fn hook_settings_are_substituted() {
    let d = daemon(Duration::from_secs(60));
    let c = client(&d.dir);
    let (sink, ev) = sink();
    let s = c.open(Open::Spawn { cfg: ps("Get-Content -Raw {tern-settings}; Read-Host"), hook: true }, SIZE, 1000, sink);
    wait_until("ready", || ready_id(&ev).is_some());
    let token = ev.lock().unwrap().iter().find_map(|e| match e {
        TermEvent::Ready { token, .. } => token.clone(),
        _ => None,
    });
    assert!(token.is_some());
    wait_until("settings json", || screen(&s).contains("hooks"));
    s.close();
}

/// A program that prints nothing for longer than the hello's read timeout
/// keeps its connection: the UI used to drop it every 3 s, reattach, and
/// make the program repaint (a Claude Code tab flickered while idle).
#[test]
fn quiet_session_keeps_its_connection() {
    let d = daemon(Duration::from_secs(60));
    let c = client(&d.dir);
    let (sink, ev) = sink();
    let s = c.open(Open::Spawn { cfg: ps("Read-Host"), hook: false }, SIZE, 1000, sink);
    wait_until("ready", || ready_id(&ev).is_some());
    std::thread::sleep(Duration::from_secs(5));
    assert!(c.alive(), "the connection dropped while the program was quiet");
    assert!(!ev.lock().unwrap().iter().any(|e| matches!(e, TermEvent::ServerLost)));
    s.close();
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
    };
    assert_eq!(serve(&d.dir, opts).unwrap_err().kind(), std::io::ErrorKind::AddrInUse);
    assert!(std::fs::metadata(d.dir.join(format!("server-{MAJOR}.lock"))).is_ok());
}

#[test]
fn idle_daemon_exits() {
    let mut d = daemon(Duration::from_millis(300));
    let c = client(&d.dir);
    let (sink, ev) = sink();
    let s = c.open(Open::Spawn { cfg: ps("Read-Host"), hook: false }, SIZE, 1000, sink);
    wait_until("ready", || ready_id(&ev).is_some());
    s.close();
    drop(c);
    d.thread.take().unwrap().join().unwrap().unwrap();
}

#[test]
fn ended_programs_keep_the_daemon_a_while() {
    let mut d = daemon(Duration::from_millis(200));
    let c = client(&d.dir);
    let (sink, ev) = sink();
    let s = c.open(Open::Spawn { cfg: ps("exit 3"), hook: false }, SIZE, 1000, sink);
    wait_until("ready", || ready_id(&ev).is_some());
    let id = s.id().unwrap();
    wait_until("exited", || c.list().unwrap().iter().any(|i| i.id == id && i.exited));
    drop(s);
    drop(c);
    // Its tab may still come to show the end.
    std::thread::sleep(Duration::from_secs(1));
    assert!(!d.thread.as_ref().unwrap().is_finished());
    let c = client(&d.dir);
    c.close(id);
    wait_until("closed", || c.list().unwrap().is_empty());
    drop(c);
    d.thread.take().unwrap().join().unwrap().unwrap();
}

#[test]
fn pid_file_names_the_daemon_while_it_runs() {
    let mut d = daemon(Duration::from_millis(200));
    let pid = std::fs::read_to_string(tern_mux::paths::pid_path(&d.dir)).unwrap();
    assert_eq!(pid, std::process::id().to_string());
    d.thread.take().unwrap().join().unwrap().unwrap();
    assert!(!tern_mux::paths::pid_path(&d.dir).exists());
}
