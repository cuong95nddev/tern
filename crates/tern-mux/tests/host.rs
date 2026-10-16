// The scripts are sh; `windows.rs` covers Windows.
#![cfg(unix)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tern_mux::host::{Host, Observer, Subscriber};
use tern_term::mirror::{Marker, Mirror};
use tern_term::status::ActivityStatus;
use tern_term::vt::grid::Dimensions;
use tern_term::vt::index::Line;
use tern_term::{SpawnConfig, TermSize};

const SIZE: TermSize = TermSize { cols: 60, rows: 10, cell_width: 8, cell_height: 16 };

struct NoObserver;
impl Observer for NoObserver {
    fn status(&self, _: u64, _: ActivityStatus, _: ActivityStatus) {}
    fn notify(&self, _: u64, _: Option<String>, _: String) {}
    fn exited(&self, _: u64, _: Option<i32>) {}
}

#[derive(Default)]
struct Recorder {
    bytes: Mutex<Vec<u8>>,
    markers: Mutex<Vec<Marker>>,
}

impl Subscriber for Recorder {
    fn output(&self, _: u64, bytes: &[u8]) {
        self.bytes.lock().unwrap().extend_from_slice(bytes);
    }
    fn marker(&self, _: u64, marker: &Marker) {
        self.markers.lock().unwrap().push(marker.clone());
    }
}

/// Feeds a mirror, as the UI does.
struct MirrorSub(Mutex<Mirror>);

impl Subscriber for MirrorSub {
    fn output(&self, _: u64, bytes: &[u8]) {
        self.0.lock().unwrap().feed(bytes);
    }
    fn marker(&self, _: u64, marker: &Marker) {
        self.0.lock().unwrap().marker(marker);
    }
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

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn text(rec: &Recorder) -> String {
    String::from_utf8_lossy(&rec.bytes.lock().unwrap()).into_owned()
}

#[test]
fn output_input_and_exit() {
    let host = Host::spawn(1, &sh("printf hello; read x; printf \"got:$x\"; exit 3"), Arc::new(NoObserver)).unwrap();
    let rec = Arc::new(Recorder::default());
    let snap = host.attach(rec.clone());
    assert_eq!(snap.size, SIZE);
    // "hello" may already be in the snapshot; it's in exactly one of them.
    wait_until("hello", || {
        let row: String = snap.grid.grid[Line(0)].into_iter().map(|c| c.c).collect();
        row.contains("hello") || text(&rec).contains("hello")
    });
    host.input(b"abc\n".to_vec());
    wait_until("got:abc", || text(&rec).contains("got:abc"));
    wait_until("exit marker", || rec.markers.lock().unwrap().contains(&Marker::Exited(Some(3))));
    assert_eq!(host.exited(), Some(Some(3)));
}

#[test]
fn resize_reaches_the_program_in_order() {
    let host = Host::spawn(2, &sh("read x; stty size; read y"), Arc::new(NoObserver)).unwrap();
    let rec = Arc::new(Recorder::default());
    host.attach(rec.clone());
    host.resize(TermSize { cols: 70, rows: 20, ..SIZE });
    wait_until("resize marker", || !rec.markers.lock().unwrap().is_empty());
    assert_eq!(rec.markers.lock().unwrap()[0], Marker::Resized(TermSize { cols: 70, rows: 20, ..SIZE }));
    host.input(b"\n".to_vec());
    wait_until("stty output", || text(&rec).contains("20 70"));
    host.kill();
    wait_until("killed", || host.exited().is_some());
}

#[test]
fn jiggle_resizes_twice() {
    let host = Host::spawn(3, &sh("read x"), Arc::new(NoObserver)).unwrap();
    let rec = Arc::new(Recorder::default());
    host.attach(rec.clone());
    host.jiggle(SIZE);
    wait_until("two resizes", || rec.markers.lock().unwrap().len() >= 2);
    let m = rec.markers.lock().unwrap().clone();
    assert_eq!(m[0], Marker::Resized(TermSize { rows: SIZE.rows + 1, ..SIZE }));
    assert_eq!(m[1], Marker::Resized(SIZE));
    host.kill();
}

#[test]
fn kill_ends_a_program_that_ignores_sighup() {
    let host = Host::spawn(4, &sh("trap '' HUP; while :; do sleep 1; done"), Arc::new(NoObserver)).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let pid = host.pid() as i32;
    host.kill();
    wait_until("killed", || host.exited().is_some());
    assert_ne!(unsafe { libc::kill(pid, 0) }, 0, "process still alive");
}

#[test]
fn kill_ends_a_program_that_writes_on_its_way_out() {
    // On a hangup it writes more than the tty holds, so it (and, once
    // killed, its exit) waits for the output to be read.
    let script = "trap 'yes | head -c 4000000; exit' HUP; while :; do sleep 1; done";
    let host = Host::spawn(9, &sh(script), Arc::new(NoObserver)).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let pid = host.pid() as i32;
    let start = Instant::now();
    host.kill();
    wait_until("killed", || host.exited().is_some());
    assert!(start.elapsed() < Duration::from_secs(8), "took {:?}", start.elapsed());
    assert_ne!(unsafe { libc::kill(pid, 0) }, 0, "process still alive");
}

#[test]
fn attach_mid_stream_loses_and_doubles_nothing() {
    let host = Host::spawn(5, &sh("i=0; while [ $i -lt 3000 ]; do echo line$i; i=$((i+1)); done; read x"), Arc::new(NoObserver)).unwrap();
    std::thread::sleep(Duration::from_millis(5));
    let events: Arc<Mutex<Vec<tern_term::TermEvent>>> = Arc::default();
    let ev = events.clone();
    let mirror = Mirror::new(SIZE, 1000, Arc::new(move |e| ev.lock().unwrap().push(e)));
    let sub = Arc::new(MirrorSub(Mutex::new(mirror)));
    let snap = host.attach(sub.clone());
    sub.0.lock().unwrap().install(snap);
    wait_until("all lines", || {
        let m = sub.0.lock().unwrap();
        let term = m.term().lock();
        let g = term.grid();
        (0..g.screen_lines() as i32).any(|l| g[Line(l)].into_iter().map(|c| c.c).collect::<String>().contains("line2999"))
    });
    // Every line in the mirror's history follows the one before it.
    let m = sub.0.lock().unwrap();
    let term = m.term().lock();
    let g = term.grid();
    let mut prev: Option<u32> = None;
    for l in -(g.history_size() as i32)..g.screen_lines() as i32 {
        let row: String = g[Line(l)].into_iter().map(|c| c.c).collect();
        if let Some(n) = row.trim().strip_prefix("line").and_then(|n| n.parse::<u32>().ok()) {
            if let Some(p) = prev {
                assert_eq!(n, p + 1, "line{n} after line{p}");
            }
            prev = Some(n);
        }
    }
    assert_eq!(prev, Some(2999));
    drop(term);
    drop(m);
    host.kill();
}

#[test]
fn attach_inside_a_synchronized_update() {
    // Starts a synchronized update, waits, then finishes it.
    let host = Host::spawn(6, &sh("printf '\\033[?2026hAAA'; read x; printf 'BBB\\033[?2026l'; read y"), Arc::new(NoObserver)).unwrap();
    // Inside the update (it times out after 150 ms).
    std::thread::sleep(Duration::from_millis(60));
    let events: Arc<Mutex<Vec<tern_term::TermEvent>>> = Arc::default();
    let ev = events.clone();
    let mirror = Mirror::new(SIZE, 1000, Arc::new(move |e| ev.lock().unwrap().push(e)));
    let sub = Arc::new(MirrorSub(Mutex::new(mirror)));
    let snap = host.attach(sub.clone());
    sub.0.lock().unwrap().install(snap);
    host.input(b"\n".to_vec());
    wait_until("both halves", || {
        let m = sub.0.lock().unwrap();
        let term = m.term().lock();
        let rows: String = (0..2).flat_map(|l| term.grid()[Line(l)].into_iter().map(|c| c.c).collect::<Vec<_>>()).collect();
        rows.contains("AAA") && rows.contains("BBB")
    });
    host.kill();
}

#[test]
fn shell_in_foreground_tells_a_prompt_from_a_program() {
    let host = Host::spawn(1, &sh("read x; exec sleep 5"), Arc::new(NoObserver)).unwrap();
    wait_until("shell", || host.shell_in_foreground());
    host.input(b"\n".to_vec());
    wait_until("program", || !host.shell_in_foreground());
    host.kill();
}
