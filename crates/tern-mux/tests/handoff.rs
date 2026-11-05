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
    let s = c.open(
        Open::Spawn { cfg: sh("echo before; read x; echo after:$x; read y"), hook: false, seed: None, agent: None },
        SIZE,
        1000,
        sink1,
    );
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
    let ended = c.open(Open::Spawn { cfg: sh("echo done; exit 4"), hook: false, seed: None, agent: None }, SIZE, 1000, sink1);
    wait_until("exit", || ev1.lock().unwrap().iter().any(|e| matches!(e, TermEvent::Exit(Some(4)))));
    let ended = ended.id().unwrap();
    let (sink2, ev2) = sink();
    let claude = c.open(Open::Spawn { cfg: sh("read x"), hook: true, seed: None, agent: None }, SIZE, 1000, sink2);
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

/// The agent statuses `ev` got from the daemon, in order.
fn statuses(ev: &Mutex<Vec<TermEvent>>) -> Vec<tern_term::agent_status::AgentStatus> {
    ev.lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            TermEvent::AgentStatus(s) => Some((**s).clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_claude_tabs_hook_state_carries_over() {
    use tern_term::agent_status::TurnState;
    let p = daemon_pair();
    let c = client(&p.dir);
    let (sink1, ev1) = sink();
    // A turn that ended with a background shell running; after the handoff
    // its inventory drops the shell, which the main agent is then owed.
    let post = |body: &str| format!(r#"curl -s -X POST -H 'content-type: application/json' -d '{body}' "$url" >/dev/null; "#);
    let mut script = r#"url=$(sed -n 's/.*"url": *"\([^"]*\)".*/\1/p' {tern-settings} | head -1); "#.to_owned();
    script += &post(r#"{"hook_event_name":"UserPromptSubmit","session_id":"s","prompt":"run it"}"#);
    script += &post(r#"{"hook_event_name":"PostToolUse","session_id":"s","tool_name":"Bash","tool_response":{"backgroundTaskId":"b1"}}"#);
    script += &post(r#"{"hook_event_name":"Stop","session_id":"s","background_tasks":[{"id":"b1","type":"shell","status":"running"}]}"#);
    // Posted until it reaches the new daemon: here the old one, in the same
    // process, still takes some (a real one has exited).
    script += "read x; while :; do ";
    script += &post(r#"{"hook_event_name":"Stop","session_id":"s","background_tasks":[]}"#);
    script += "sleep 0.1; done";
    let s = c.open(Open::Spawn { cfg: sh(&script), hook: true, seed: None, agent: None }, SIZE, 1000, sink1);
    wait_until("ready", || ready_id(&ev1).is_some());
    wait_until("monitoring", || statuses(&ev1).last().is_some_and(|s| s.monitoring));
    let id = s.id().unwrap();

    hand_off(&p.dir).unwrap();
    p.old.join().unwrap().unwrap();

    let c = client(&p.dir);
    let (sink2, ev2) = sink();
    let s = c.open(Open::Attach { id }, SIZE, 1000, sink2);
    wait_until("status after the handoff", || statuses(&ev2).last().is_some_and(|s| s.monitoring));
    s.write(b"\n".to_vec());
    // Only a tracker that knew of b1 owes its notification.
    wait_until("owed", || statuses(&ev2).last().is_some_and(|s| s.state == TurnState::Working && s.held.owed));
    s.close();
}

#[test]
fn a_codex_tabs_agent_and_lane_carry_over() {
    use tern_term::agent_status::TurnState;
    let p = daemon_pair();
    let c = client(&p.dir);
    let (sink1, ev1) = sink();
    let (_dir, path) = fake_codex("0.166.0");
    let post = |body: &str| format!(r#"curl -s -X POST -H 'content-type: application/json' -d '{body}' "$url" >/dev/null; "#);
    let mut script = CODEX_URL.to_owned();
    script += &post(r#"{"hook_event_name":"UserPromptSubmit","session_id":"s","prompt":"hi"}"#);
    // Posted until it reaches the new daemon: here the old one, in the same
    // process, still takes some (a real one has exited).
    script += "read x; while :; do ";
    script += &post(r#"{"hook_event_name":"Interrupt","session_id":"s"}"#);
    script += "sleep 0.1; done";
    let s = c.open(codex_tab(&path, &script), SIZE, 1000, sink1);
    wait_until("ready", || ready_id(&ev1).is_some());
    wait_until("working", || statuses(&ev1).last().is_some_and(|s| s.state == TurnState::Working));
    let id = s.id().unwrap();

    hand_off(&p.dir).unwrap();
    p.old.join().unwrap().unwrap();

    let c = client(&p.dir);
    assert!(c.list().unwrap().iter().any(|i| i.id == id && i.agent.as_deref() == Some("codex") && !i.claude));
    let (sink2, ev2) = sink();
    let s = c.open(Open::Attach { id }, SIZE, 1000, sink2);
    wait_until("status after the handoff", || statuses(&ev2).last().is_some_and(|s| s.state == TurnState::Working));
    s.write(b"\n".to_vec());
    // Only a Codex lane takes `Interrupt` for the turn's end.
    wait_until("interrupted", || statuses(&ev2).last().is_some_and(|s| s.state == TurnState::Done));
    s.close();
}

#[test]
fn kitty_images_carry_over() {
    let p = daemon_pair();
    let c = client(&p.dir);
    let (sink1, ev1) = sink();
    // A 1x1 RGB image with a virtual placement, then one known by number only.
    let script = r"printf '\033_Ga=T,U=1,i=7,f=24,s=1,v=1,c=2,r=1,q=2;AAAA\033\\\033_Ga=t,I=3,f=24,s=1,v=1,q=2;AAAA\033\\'; echo sent; read x; printf '\033_Ga=p,I=3,U=1,c=4,r=2\033\\'; echo placed; read y";
    let s = c.open(Open::Spawn { cfg: sh(script), hook: false, seed: None, agent: None }, SIZE, 1000, sink1);
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
    let s = c.open(Open::Spawn { cfg: sh("read x; echo got:$x; read y"), hook: false, seed: None, agent: None }, SIZE, 1000, sink1);
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
    let _s2 = c2.open(Open::Spawn { cfg: sh("read x"), hook: false, seed: None, agent: None }, SIZE, 1000, sink2);
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
    let s = c.open(Open::Spawn { cfg: sh("echo fresh; read x"), hook: false, seed: Some(seed), agent: None }, SIZE, 1000, sink);
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

#[test]
fn services_carry_over() {
    use tern_mux::proto::ServiceSpec;
    use tern_mux::services::Services;
    let p = daemon_pair();
    let spec = ServiceSpec { name: "web".into(), command: "dev".into(), cwd: "/tmp".into(), by: Some(3), started: 0 };
    let (id, pid) = Services::connect(&p.dir, false).unwrap().run(sh("echo up; read x"), spec).unwrap();

    hand_off(&p.dir).unwrap();
    p.old.join().unwrap().unwrap();

    let mut s = Services::connect(&p.dir, false).unwrap();
    let info = s.list().unwrap().into_iter().find(|i| i.id == id).unwrap();
    assert_eq!(info.pid, pid);
    assert_eq!(info.service.map(|s| (s.name, s.by)), Some(("web".into(), Some(3))));
    wait_until("output", || s.text(id, 5).unwrap().is_some_and(|t| t.contains("up")));
}

/// A chat's sink, and what it heard.
fn chat_sink() -> (tern_mux::client::ChatSink, Arc<Mutex<Vec<tern_mux::client::ChatEvent>>>) {
    let ev = Arc::new(Mutex::new(Vec::new()));
    let e = ev.clone();
    (Arc::new(move |x| e.lock().unwrap().push(x)), ev)
}

fn chat_spec(script: &str) -> proto::ChatSpec {
    proto::ChatSpec { program: "/bin/sh".into(), args: vec!["-c".into(), script.into()], cwd: "/".into(), ..Default::default() }
}

#[test]
fn a_running_chat_moves_to_the_new_daemon() {
    use tern_mux::client::{ChatEvent, OpenChat};
    let p = daemon_pair();
    let c = client(&p.dir);
    let (sink1, ev1) = chat_sink();
    let spec = proto::ChatSpec { session: "conv-1".into(), ..chat_spec(r#"echo '{"n":1}'; read a; echo "{\"got\":\"$a\"}"; read b"#) };
    let chat = c.open_chat(OpenChat::Spawn(Box::new(spec)), sink1);
    let attached = |ev: &Arc<Mutex<Vec<ChatEvent>>>| {
        ev.lock().unwrap().iter().find_map(|e| match e {
            ChatEvent::Attached { id, lines, .. } => Some((*id, lines.clone())),
            _ => None,
        })
    };
    wait_until("attached", || attached(&ev1).is_some());
    wait_until("first line", || {
        attached(&ev1).is_some_and(|(_, l)| !l.is_empty()) || ev1.lock().unwrap().iter().any(|e| matches!(e, ChatEvent::Line(_)))
    });
    let id = chat.id().unwrap();

    hand_off(&p.dir).unwrap();
    p.old.join().unwrap().unwrap();
    wait_until("lost", || ev1.lock().unwrap().iter().any(|e| matches!(e, ChatEvent::Lost)));

    let c = client(&p.dir);
    // Whose it is came along.
    assert_eq!(c.list_chats().unwrap().iter().map(|i| (i.id, i.session.as_str())).collect::<Vec<_>>(), [(id, "conv-1")]);
    let (sink2, ev2) = chat_sink();
    let chat = c.open_chat(OpenChat::Attach { id }, sink2);
    wait_until("reattached", || attached(&ev2).is_some());
    assert_eq!(attached(&ev2).unwrap().1, [r#"{"n":1}"#]);
    chat.send("hi".into());
    wait_until("answer", || ev2.lock().unwrap().iter().any(|e| *e == ChatEvent::Line(r#"{"got":"hi"}"#.into())));
    chat.close();
    let _ = ev1;
}
