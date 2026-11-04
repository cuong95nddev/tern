// The scripts are sh; `windows.rs` covers Windows.
#![cfg(unix)]

mod common;
use common::*;

use std::os::unix::net::UnixListener;
use std::time::{Duration, Instant};

use tern_mux::client::Open;
use tern_mux::observer::{WatchStream, Watched, Watcher};
use tern_mux::paths;
use tern_mux::proto::{self, ClientMsg, ServerMsg, SessionInfo};

/// The next session list `stream` gets, past chat lists.
fn sessions(stream: &mut WatchStream) -> Option<Vec<SessionInfo>> {
    loop {
        match stream.next()? {
            Watched::Sessions(list) => return Some(list),
            Watched::Chats(_) => {},
        }
    }
}

#[test]
fn watcher_follows_the_list() {
    let d = daemon(Duration::from_secs(60));
    let (w, mut stream) = Watcher::connect(&d.dir).unwrap();
    assert_eq!(sessions(&mut stream), Some(vec![]));
    let c = client(&d.dir);
    let (sink, ev) = sink();
    let s = c.open(Open::Spawn { cfg: sh("read x"), hook: false, seed: None }, SIZE, 1000, sink);
    wait_until("ready", || ready_id(&ev).is_some());
    let id = s.id().unwrap();
    while !sessions(&mut stream).unwrap().iter().any(|i| i.id == id) {}
    drop(s);
    w.close(id);
    while !sessions(&mut stream).unwrap().is_empty() {}
}

#[test]
fn watcher_follows_the_chats() {
    let d = daemon(Duration::from_secs(60));
    let (w, mut stream) = Watcher::connect(&d.dir).unwrap();
    let c = client(&d.dir);
    let spec = proto::ChatSpec {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), r#"echo '{"type":"user","message":{"content":"Fix the bug"}}'; read x"#.into()],
        cwd: "/".into(),
        ..Default::default()
    };
    let chat = c.open_chat(tern_mux::client::OpenChat::Spawn(spec), std::sync::Arc::new(|_| {}));
    let working = |list: &[proto::ChatInfo]| {
        list.iter()
            .any(|i| i.attached && i.status == tern_term::status::ActivityStatus::Working && i.title.as_deref() == Some("Fix the bug"))
    };
    while !matches!(stream.next().unwrap(), Watched::Chats(l) if working(&l)) {}
    let id = chat.id().unwrap();
    drop(chat);
    w.close_chat(id);
    while !matches!(stream.next().unwrap(), Watched::Chats(l) if l.is_empty()) {}
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
    let ui = tern_mux::client::MuxClient::connect(
        &d.dir,
        false,
        std::sync::Arc::new(|_, _| {}),
        std::sync::Arc::new(move |r| g.lock().unwrap().push(r)),
    )
    .unwrap();
    // `connect` returns once its Hello is sent; an answer on that connection
    // means the daemon took the Hello, so it knows the UI.
    ui.list().unwrap();
    assert!(w.reveal(5));
    wait_until("ui got it", || *got.lock().unwrap() == [tern_mux::client::UiRequest::Reveal(5)]);
}

#[test]
fn an_agent_asks_the_ui() {
    let d = daemon(Duration::from_secs(60));
    let mut agent = tern_mux::services::Services::connect(&d.dir, false).unwrap();
    // No UI: it says so.
    let err = agent.ask_ui("{}".into()).unwrap_err();
    assert!(err.to_string().contains("isn't open"), "{err}");

    let (tx, rx) = std::sync::mpsc::channel();
    let ui = tern_mux::client::MuxClient::connect(
        &d.dir,
        false,
        std::sync::Arc::new(|_, _| {}),
        std::sync::Arc::new(move |r| {
            let _ = tx.send(r);
        }),
    )
    .unwrap();
    ui.list().unwrap();
    let answering = ui.clone();
    let answerer = std::thread::spawn(move || {
        let tern_mux::client::UiRequest::Ask { ask, request } = rx.recv().unwrap() else { panic!("not an ask") };
        answering.answer_ui(ask, format!("{{\"got\":{request}}}"));
        // The next one goes unanswered: the window goes away.
        let tern_mux::client::UiRequest::Ask { .. } = rx.recv().unwrap() else { panic!("not an ask") };
        drop(answering);
    });
    assert_eq!(agent.ask_ui(r#"{"op":"status"}"#.into()).unwrap(), r#"{"got":{"op":"status"}}"#);
    let asking = std::thread::spawn(move || agent.ask_ui("{}".into()));
    answerer.join().unwrap();
    drop(ui);
    let err = asking.join().unwrap().unwrap_err();
    assert!(err.to_string().contains("went away"), "{err}");
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
        proto::write_frame(
            &mut w,
            &ServerMsg::Hello { major: proto::MAJOR, minor: 0, app_version: "0.1.1".into(), pid: 1, build: String::new() },
        )
        .unwrap();
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
    assert_eq!(sessions(&mut stream).unwrap()[0].id, 7);
    let t = Instant::now();
    assert!(!w.reveal(7));
    assert!(t.elapsed() < Duration::from_millis(100), "no waiting on an old daemon");
}
