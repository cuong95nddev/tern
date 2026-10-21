// Handoff is Unix only; the scripts are sh.
#![cfg(unix)]
//! A daemon handing its sessions to another, within one process: the
//! "new daemon" is a thread running `adopt` on the other end of a socketpair.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tern_mux::client::{Open, RemoteSession};
use tern_mux::paths;
use tern_mux::proto::{self, HandoffMsg};
use tern_mux::server::{ServeOpts, Successor, adopt, serve};
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

fn opts(run_root: PathBuf, successor: Option<Successor>) -> ServeOpts {
    ServeOpts {
        idle_exit: Duration::from_secs(60),
        notifications: false,
        run_root,
        hooks: true,
        max_queued: 1 << 20,
        max_queued_session: usize::MAX,
        successor,
    }
}

struct Pair {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    old: std::thread::JoinHandle<std::io::Result<()>>,
    /// What the adopting daemon's `adopt` returned, once it did.
    new: Arc<Mutex<Option<std::thread::JoinHandle<std::io::Result<()>>>>>,
}

/// An old daemon whose handoff starts `adopter` on a thread.
fn daemon_with_adopter(adopter: impl Fn(&Path, UnixStream) -> std::io::Result<()> + Send + Sync + 'static) -> Pair {
    let tmp = short_tempdir();
    let dir = tmp.path().join("m");
    let new = Arc::new(Mutex::new(None));
    let adopter = Arc::new(adopter);
    let successor: Successor = {
        let (dir, new) = (dir.clone(), new.clone());
        Arc::new(move |_exe: &Path| {
            let (ours, theirs) = UnixStream::pair()?;
            let (dir, adopter) = (dir.clone(), adopter.clone());
            *new.lock().unwrap() = Some(std::thread::spawn(move || adopter(&dir, theirs)));
            Ok((ours, Box::new(|| {}) as Box<dyn FnOnce() + Send>))
        })
    };
    let o = opts(tmp.path().join("run"), Some(successor));
    let d = dir.clone();
    let old = std::thread::spawn(move || serve(&d, o));
    wait_until("socket", || paths::socket_path(&dir).exists());
    Pair { _tmp: tmp, dir, old, new }
}

/// An old daemon whose handoff goes to a real `adopt`.
fn daemon_pair() -> Pair {
    daemon_with_adopter(|dir, stream| {
        let run_root = dir.parent().unwrap().join("run");
        adopt(dir, opts(run_root, None), stream)
    })
}

fn hand_off(dir: &Path) -> std::io::Result<()> {
    client(dir).request_hand_off("/nonexistent/tern".into())
}

#[test]
fn a_running_program_moves_to_the_new_daemon() {
    let p = daemon_pair();
    let c = client(&p.dir);
    let (sink1, ev1) = sink();
    let s = c.open(Open::Spawn { cfg: sh("echo before; read x; echo after:$x; read y"), hook: false, seed: None }, SIZE, 1000, sink1);
    wait_until("ready", || ready_id(&ev1).is_some());
    wait_until("output", || screen(&s).contains("before"));
    let id = s.id().unwrap();
    let pid = c.list().unwrap()[0].pid;

    hand_off(&p.dir).unwrap();
    // The old daemon is done, and let go of every connection.
    p.old.join().unwrap().unwrap();
    wait_until("lost", || ev1.lock().unwrap().iter().any(|e| matches!(e, TermEvent::ServerLost)));

    let c = client(&p.dir);
    let list = c.list().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!((list[0].id, list[0].pid, list[0].exited), (id, pid, false));
    let (sink2, ev2) = sink();
    let s = c.open(Open::Attach { id }, SIZE, 1000, sink2);
    wait_until("reattached", || ready_id(&ev2) == Some(id));
    assert!(screen(&s).contains("before"), "{}", screen(&s));
    s.write(b"hi\r".to_vec());
    wait_until("after", || screen(&s).contains("after:hi"));
    // The new daemon sees it end.
    s.write(b"y\r".to_vec());
    wait_until("exit", || ev2.lock().unwrap().iter().any(|e| matches!(e, TermEvent::Exit(_))));
}

#[test]
fn ended_sessions_and_claude_tabs_carry_over() {
    let p = daemon_pair();
    let c = client(&p.dir);
    let (sink1, ev1) = sink();
    let ended = c.open(Open::Spawn { cfg: sh("echo done; exit 4"), hook: false, seed: None }, SIZE, 1000, sink1);
    wait_until("exit", || ev1.lock().unwrap().iter().any(|e| matches!(e, TermEvent::Exit(Some(4)))));
    let ended = ended.id().unwrap();
    let (sink2, ev2) = sink();
    let claude = c.open(Open::Spawn { cfg: sh("read x"), hook: true, seed: None }, SIZE, 1000, sink2);
    wait_until("ready", || ready_id(&ev2).is_some());
    let claude = claude.id().unwrap();

    hand_off(&p.dir).unwrap();
    p.old.join().unwrap().unwrap();

    let c = client(&p.dir);
    let list = c.list().unwrap();
    assert!(list.iter().any(|i| i.id == ended && i.exited), "{list:?}");
    assert!(list.iter().any(|i| i.id == claude && i.claude && !i.exited), "{list:?}");
    let (sink3, ev3) = sink();
    let s = c.open(Open::Attach { id: ended }, SIZE, 1000, sink3);
    wait_until("reattached", || ready_id(&ev3) == Some(ended));
    wait_until("exit replayed", || ev3.lock().unwrap().iter().any(|e| matches!(e, TermEvent::Exit(Some(4)))));
    assert!(screen(&s).contains("done"), "{}", screen(&s));
}

#[test]
fn kitty_images_carry_over() {
    let p = daemon_pair();
    let c = client(&p.dir);
    let (sink1, ev1) = sink();
    // A 1x1 RGB image with a virtual placement, then one known by number only.
    let script = r"printf '\033_Ga=T,U=1,i=7,f=24,s=1,v=1,c=2,r=1,q=2;AAAA\033\\\033_Ga=t,I=3,f=24,s=1,v=1,q=2;AAAA\033\\'; echo sent; read x; printf '\033_Ga=p,I=3,U=1,c=4,r=2\033\\'; echo placed; read y";
    let s = c.open(Open::Spawn { cfg: sh(script), hook: false, seed: None }, SIZE, 1000, sink1);
    wait_until("ready", || ready_id(&ev1).is_some());
    wait_until("images", || s.images().len() == 2);
    let id = s.id().unwrap();

    hand_off(&p.dir).unwrap();
    p.old.join().unwrap().unwrap();

    let c = client(&p.dir);
    let (sink2, ev2) = sink();
    let s = c.open(Open::Attach { id }, SIZE, 1000, sink2);
    wait_until("reattached", || ready_id(&ev2) == Some(id));
    wait_until("images again", || s.images().len() == 2);
    {
        let images = s.images();
        let img = &images[&7];
        assert_eq!((img.width, img.height, img.rgba.len()), (1, 1, 4));
        assert_eq!(img.placements, [(0, 2, 1)]);
    }
    // The new daemon still knows image number 3.
    s.write(b"\r".to_vec());
    wait_until("placed", || screen(&s).contains("placed"));
    wait_until("placement", || s.images().values().any(|i| i.placements == [(0, 4, 2)]));
}

#[test]
fn a_failed_adoption_leaves_the_old_daemon_serving() {
    // The new daemon reads the first message, then dies.
    let p = daemon_with_adopter(|_, stream| {
        let mut r = proto::FrameReader::new(stream, proto::MAX_SERVER_FRAME);
        let _ = r.read::<HandoffMsg>();
        Err(std::io::Error::other("crashed"))
    });
    let c = client(&p.dir);
    let (sink1, ev) = sink();
    let s = c.open(Open::Spawn { cfg: sh("read x; echo got:$x; read y"), hook: false, seed: None }, SIZE, 1000, sink1);
    wait_until("ready", || ready_id(&ev).is_some());

    assert!(hand_off(&p.dir).is_err());
    let _ = p.new.lock().unwrap().take().unwrap().join();
    // Its program is driven again, its connection never dropped.
    s.write(b"a\r".to_vec());
    wait_until("output", || screen(&s).contains("got:a"));
    assert!(!ev.lock().unwrap().iter().any(|e| matches!(e, TermEvent::ServerLost)));
    // And it takes new connections and programs.
    let c2 = client(&p.dir);
    let (sink2, ev2) = sink();
    let _s2 = c2.open(Open::Spawn { cfg: sh("read x"), hook: false, seed: None }, SIZE, 1000, sink2);
    wait_until("ready", || ready_id(&ev2).is_some());
    assert!(!p.old.is_finished());
}

#[test]
fn a_seeded_terminal_starts_below_the_old_screen() {
    use tern_term::vt::term::{Config, Term};
    use tern_term::vt::vte::ansi::{Processor, StdSyncHandler};
    let mut old = Term::new(Config::default(), &SIZE, tern_term::vt::event::VoidListener);
    Processor::<StdSyncHandler>::new().advance(&mut old, b"old line one\r\nold line two\r\n$ ");
    let snap = tern_term::snapshot::capture(&mut old, SIZE, None, Default::default());
    let seed = proto::pack_snapshot(&snap).unwrap();

    let d = daemon(Duration::from_secs(60));
    let c = client(&d.dir);
    let (sink, ev) = sink();
    let s = c.open(Open::Spawn { cfg: sh("echo fresh; read x"), hook: false, seed: Some(seed) }, SIZE, 1000, sink);
    wait_until("ready", || ready_id(&ev).is_some());
    wait_until("output", || screen(&s).contains("fresh"));
    let term = s.term().lock();
    let g = term.grid();
    let history: Vec<String> = (1..=g.history_size() as i32)
        .rev()
        .map(|l| g[Line(-l)].into_iter().map(|c| c.c).collect::<String>().trim_end().to_owned())
        .collect();
    assert_eq!(history, ["old line one", "old line two", "$"]);
    assert!(g[Line(0)].into_iter().map(|c| c.c).collect::<String>().starts_with("fresh"));
}
