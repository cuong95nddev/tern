// The menu bar agent is macOS only (Unix socket paths under /tmp here).
#![cfg(unix)]

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
        #[cfg(unix)]
        successor: None,
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
