//! The agent's connection to the daemon, on its own thread.

use std::path::Path;
use std::time::Duration;

use tern_mux::observer::Watcher;
use tern_mux::proto::SessionInfo;

/// How often a lost or refused connection is tried again.
pub const RETRY: Duration = Duration::from_secs(2);

pub enum LinkEvent {
    /// Requests (Open, End) go through this until `Lost`.
    Connected(Watcher),
    List(Vec<SessionInfo>),
    /// No daemon (or it went away): show the empty state.
    Lost,
}

/// One connection's worth of `run`: returns once it is lost or refused.
pub fn follow_once(dir: &Path, post: &impl Fn(LinkEvent)) {
    match Watcher::connect(dir) {
        Ok((w, mut stream)) => {
            post(LinkEvent::Connected(w));
            while let Some(list) = stream.next() {
                post(LinkEvent::List(list));
            }
            log::info!("daemon connection lost");
        },
        Err(e) => log::debug!("no daemon: {e}"),
    }
    post(LinkEvent::Lost);
}

/// Follow the daemon in `dir` forever: connect, forward lists, report the
/// loss, retry every `RETRY`. Never starts the daemon.
pub fn run(dir: &Path, post: impl Fn(LinkEvent)) -> ! {
    loop {
        follow_once(dir, &post);
        std::thread::sleep(RETRY);
    }
}
