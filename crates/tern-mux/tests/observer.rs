// The scripts are sh; `windows.rs` covers Windows.
#![cfg(unix)]

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
    let _ui = tern_mux::client::MuxClient::connect(
        &d.dir,
        false,
        std::sync::Arc::new(|_, _| {}),
        std::sync::Arc::new(move |id| g.lock().unwrap().push(id)),
    )
    .unwrap();
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
