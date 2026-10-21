//! Viewers: watching and typing into a session beside the UI that owns it.

use std::sync::mpsc;
use std::time::Duration;

use tern_mux::client::{Open, RemoteSession};
use tern_mux::proto::Controller;
use tern_mux::viewer::{ViewEvent, Viewer};
use tern_term::snapshot::Snapshot;
use tern_term::vt::grid::Dimensions;
use tern_term::vt::index::Line;
use tern_term::{SpawnConfig, TermSize};

mod common;
use common::*;

/// Prints "before", then echoes one line as "after:<line>" and waits.
#[cfg(unix)]
fn echo_once() -> SpawnConfig {
    sh("echo before; read x; echo after:$x; read y")
}

#[cfg(windows)]
fn echo_once() -> SpawnConfig {
    ps("Write-Host before; $x = Read-Host; Write-Host after:$x; Read-Host")
}

fn grid_text(lines: impl Iterator<Item = String>) -> String {
    lines.map(|l| l.trim_end().to_owned()).collect::<Vec<_>>().join("\n")
}

fn screen(s: &RemoteSession) -> String {
    let term = s.term().lock();
    let g = term.grid();
    grid_text((0..g.screen_lines() as i32).map(|l| g[Line(l)].into_iter().map(|c| c.c).collect()))
}

fn snapshot_text(snap: &Snapshot) -> String {
    let g = &snap.grid.grid;
    grid_text((0..g.screen_lines() as i32).map(|l| g[Line(l)].into_iter().map(|c| c.c).collect()))
}

/// The viewer's events, read on a thread.
fn events(mut stream: tern_mux::viewer::ViewStream) -> mpsc::Receiver<ViewEvent> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        while let Some(e) = stream.next() {
            if tx.send(e).is_err() {
                return;
            }
        }
    });
    rx
}

fn next_until<T>(rx: &mpsc::Receiver<ViewEvent>, what: &str, mut f: impl FnMut(ViewEvent) -> Option<T>) -> T {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let e = rx.recv_timeout(left).unwrap_or_else(|_| panic!("timed out waiting for {what}"));
        if let Some(t) = f(e) {
            return t;
        }
    }
}

#[test]
fn viewer_watches_and_types_beside_the_owner() {
    let d = daemon(Duration::from_secs(60));
    let c = client(&d.dir);
    let (sink, ev) = sink();
    let s = c.open(Open::Spawn { cfg: echo_once(), hook: false, seed: None }, SIZE, 1000, sink);
    wait_until("ready", || ready_id(&ev).is_some());
    wait_until("output", || screen(&s).contains("before"));
    let id = s.id().unwrap();

    let (viewer, stream) = Viewer::connect(&d.dir).unwrap();
    let rx = events(stream);
    next_until(&rx, "the list", |e| matches!(e, ViewEvent::Sessions(l) if l.iter().any(|i| i.id == id && i.attached)).then_some(()));
    viewer.view(id).unwrap();
    let snap = next_until(&rx, "the snapshot", |e| match e {
        ViewEvent::Viewing { id: v, snapshot } if v == id => Some(snapshot),
        _ => None,
    });
    assert!(snapshot_text(&snap).contains("before"), "{}", snapshot_text(&snap));

    // Typed by the viewer: both see the program's answer.
    viewer.input(id, b"x\r".to_vec()).unwrap();
    wait_until("after on the owner", || screen(&s).contains("after:x"));
    let mut seen = Vec::new();
    next_until(&rx, "after on the viewer", |e| {
        if let ViewEvent::Output { id: v, bytes } = e
            && v == id
        {
            seen.extend_from_slice(&bytes);
        }
        String::from_utf8_lossy(&seen).contains("after:x").then_some(())
    });
    // The owner kept the session.
    assert!(c.list().unwrap().iter().any(|i| i.id == id && i.attached));
    assert!(!ev.lock().unwrap().iter().any(|e| matches!(e, tern_term::TermEvent::Exit(_))));

    // A viewer going away leaves the owner alone.
    // Its typing took the session: the owner types again once it is back.
    viewer.shutdown();
    drop(rx);
    wait_until("the owner told", || remote(&ev) == Some(false));
    s.write(b"\r".to_vec());
    wait_until("exit", || ev.lock().unwrap().iter().any(|e| matches!(e, tern_term::TermEvent::Exit(_))));
}

#[test]
fn viewing_an_unknown_session_is_not_found() {
    let d = daemon(Duration::from_secs(60));
    let (viewer, stream) = Viewer::connect(&d.dir).unwrap();
    let rx = events(stream);
    viewer.view(12345).unwrap();
    next_until(&rx, "not found", |e| matches!(e, ViewEvent::NotFound { id: 12345 }).then_some(()));
}

#[test]
fn viewer_does_not_keep_the_daemon_alive() {
    let mut d = daemon(Duration::from_millis(300));
    let (_viewer, _stream) = Viewer::connect(&d.dir).unwrap();
    let t = d.thread.take().unwrap();
    wait_until("idle exit with a viewer connected", || t.is_finished());
    t.join().unwrap().unwrap();
}

/// Who has session `id`, as the viewer was told last (once it changes).
fn control_until(rx: &mpsc::Receiver<ViewEvent>, id: u64, want: Controller) {
    next_until(rx, &format!("control {want:?}"), |e| matches!(e, ViewEvent::Control { id: v, by } if v == id && by == want).then_some(()));
}

/// Whether the owner was told last that a viewer has it.
fn remote(ev: &Events) -> Option<bool> {
    ev.lock().unwrap().iter().rev().find_map(|e| match e {
        tern_term::TermEvent::RemoteControl(on) => Some(*on),
        _ => None,
    })
}

#[test]
fn a_viewer_has_the_session_until_the_owner_takes_it_back() {
    let d = daemon(Duration::from_secs(60));
    let c = client(&d.dir);
    let (sink, ev) = sink();
    let s = c.open(Open::Spawn { cfg: echo_once(), hook: false, seed: None }, SIZE, 1000, sink);
    wait_until("ready", || ready_id(&ev).is_some());
    wait_until("output", || screen(&s).contains("before"));
    let id = s.id().unwrap();
    let cols = |s: &RemoteSession| s.term().lock().grid().columns();
    wait_until("the owner's size", || cols(&s) == SIZE.cols as usize);

    let (viewer, stream) = Viewer::connect(&d.dir).unwrap();
    let rx = events(stream);
    viewer.view(id).unwrap();
    control_until(&rx, id, Controller::Desktop);
    let phone = TermSize { cols: 40, rows: 20, ..SIZE };
    viewer.resize(id, phone).unwrap();
    wait_until("the viewer's size", || cols(&s) == 40);
    control_until(&rx, id, Controller::You);
    wait_until("the owner told", || remote(&ev) == Some(true));

    // The owner's window comes and goes, it resizes, it types: all dropped.
    s.write(b"\x1b[I".to_vec());
    let wide = TermSize { cols: 100, ..SIZE };
    s.resize(wide);
    s.write(b"x\r".to_vec());
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(cols(&s), 40);
    assert!(!screen(&s).contains("after:"), "{}", screen(&s));

    // Taken back: the owner's size (the one it asked for last), its typing.
    s.take_back();
    wait_until("the owner's size back", || cols(&s) == 100);
    control_until(&rx, id, Controller::Desktop);
    wait_until("the owner told", || remote(&ev) == Some(false));

    // The viewer refits (back to the front) and its window comes and goes:
    // the owner keeps it.
    viewer.resize(id, TermSize { rows: 21, ..phone }).unwrap();
    viewer.input(id, b"\x1b[O\x1b[I".to_vec()).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(cols(&s), 100);

    // The viewer types: it has it again, at its size.
    viewer.input(id, b"y".to_vec()).unwrap();
    wait_until("the viewer's size again", || cols(&s) == 40);
    control_until(&rx, id, Controller::You);

    // And asked for, after the owner took it back.
    s.take_back();
    control_until(&rx, id, Controller::Desktop);
    viewer.drive(id).unwrap();
    control_until(&rx, id, Controller::You);
    wait_until("the viewer's size on asking", || cols(&s) == 40);

    // The viewer leaves: the owner has it.
    viewer.unview(id).unwrap();
    wait_until("the owner's size once the viewer left", || cols(&s) == 100);
    wait_until("the owner told", || remote(&ev) == Some(false));

    // And when it goes away while it has it.
    viewer.view(id).unwrap();
    viewer.resize(id, phone).unwrap();
    wait_until("the viewer's size on coming back", || cols(&s) == 40);
    viewer.shutdown();
    wait_until("the owner's size once the viewer is gone", || cols(&s) == 100);
}
