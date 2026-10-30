//! Services: commands agents ask Tern to run (`tern --mcp`), each in a
//! daemon session of its own, so the user sees and stops them. [`Services`]
//! is a plain observer connection that asks the daemon one thing at a time.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use tern_term::SpawnConfig;

use crate::UnixStream;
use crate::paths;
use crate::proto::{self, ClientMsg, FrameReader, MAJOR, MINOR, Role, SERVICES_MINOR, ServerMsg, ServiceSpec, SessionInfo};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// How long a request may wait for its answer.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

/// A connection to the daemon for services.
pub struct Services {
    writer: UnixStream,
    reader: FrameReader,
    next_req: u64,
}

impl Services {
    /// Connect to the daemon in `dir`; with `start`, start one when none
    /// runs (a service keeps it running).
    pub fn connect(dir: &Path, start: bool) -> io::Result<Self> {
        let sock = paths::socket_path(dir);
        let stream = match UnixStream::connect(&sock) {
            Ok(s) => s,
            Err(e) if !start => return Err(e),
            Err(_) => {
                crate::client::spawn_daemon()?;
                let deadline = Instant::now() + CONNECT_TIMEOUT;
                loop {
                    match UnixStream::connect(&sock) {
                        Ok(s) => break s,
                        Err(e) if Instant::now() >= deadline => return Err(e),
                        Err(_) => std::thread::sleep(Duration::from_millis(50)),
                    }
                }
            },
        };
        if !paths::peer_uid_ok(&stream) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "daemon runs as another user"));
        }
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        stream.set_read_timeout(Some(ANSWER_TIMEOUT))?;
        let mut writer = stream.try_clone()?;
        proto::write_frame(
            &mut writer,
            &ClientMsg::Hello { major: MAJOR, minor: MINOR, app_version: env!("CARGO_PKG_VERSION").into(), role: Role::Observer },
        )?;
        let mut reader = FrameReader::new(stream, proto::MAX_SERVER_FRAME);
        match reader.read::<ServerMsg>()? {
            Some(ServerMsg::Hello { major, minor, app_version, .. }) if major == MAJOR => {
                if minor < SERVICES_MINOR {
                    let message = format!("the Tern daemon running (Tern {app_version}) is too old for services; restart Tern");
                    return Err(io::Error::new(io::ErrorKind::Unsupported, message));
                }
            },
            other => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("bad hello: {other:?}"))),
        }
        Ok(Self { writer, reader, next_req: 1 })
    }

    fn ask(&mut self, msg: impl FnOnce(u64) -> ClientMsg) -> io::Result<ServerMsg> {
        let req = self.next_req;
        self.next_req += 1;
        proto::write_frame(&mut self.writer, &msg(req))?;
        loop {
            let Some(answer) = self.reader.read::<ServerMsg>()? else {
                return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "the daemon closed the connection"));
            };
            match &answer {
                ServerMsg::Sessions { req: r, .. }
                | ServerMsg::ServiceStarted { req: r, .. }
                | ServerMsg::Text { req: r, .. }
                | ServerMsg::NotFound { req: r, .. }
                | ServerMsg::Error { req: r, .. }
                    if *r == req =>
                {
                    return Ok(answer);
                },
                _ => {},
            }
        }
    }

    /// Every session the daemon runs (services have `service` set).
    pub fn list(&mut self) -> io::Result<Vec<SessionInfo>> {
        match self.ask(|req| ClientMsg::List { req })? {
            ServerMsg::Sessions { list, .. } => Ok(list),
            other => Err(unexpected(other)),
        }
    }

    /// Start `service` running `cfg`: its session id and pid.
    pub fn run(&mut self, cfg: SpawnConfig, service: ServiceSpec) -> io::Result<(u64, u32)> {
        match self.ask(|req| ClientMsg::RunService { req, cfg, service })? {
            ServerMsg::ServiceStarted { id, pid, .. } => Ok((id, pid)),
            ServerMsg::Error { message, .. } => Err(io::Error::other(message)),
            other => Err(unexpected(other)),
        }
    }

    /// The last `lines` lines session `id` shows; `None` once it is gone.
    pub fn text(&mut self, id: u64, lines: u32) -> io::Result<Option<String>> {
        match self.ask(|req| ClientMsg::ReadText { req, id, lines })? {
            ServerMsg::Text { text, .. } => Ok(Some(text)),
            ServerMsg::NotFound { .. } => Ok(None),
            other => Err(unexpected(other)),
        }
    }

    /// End session `id` and forget it.
    pub fn close(&mut self, id: u64) -> io::Result<()> {
        proto::write_frame(&mut self.writer, &ClientMsg::Close { id })
    }
}

fn unexpected(msg: ServerMsg) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("unexpected answer: {msg:?}"))
}

/// The TCP ports each of `roots`' process trees listens on, sorted. Asks
/// `ps` and `lsof`; nothing when they fail (or on Windows, for now).
#[cfg(unix)]
pub fn listening_ports(roots: &[u32]) -> HashMap<u32, Vec<u16>> {
    use std::process::Command;
    let mut out = HashMap::new();
    if roots.is_empty() {
        return out;
    }
    let run = |cmd: &mut Command| cmd.output().ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    let Some(ps) = run(Command::new("ps").args(["-axo", "pid=,ppid="])) else { return out };
    let Some(lsof) = run(Command::new("lsof").args(["-nP", "-iTCP", "-sTCP:LISTEN", "-Fpn"])) else { return out };
    let parents = parse_ps(&ps);
    let listens = parse_lsof(&lsof);
    for &root in roots {
        let mut ports: Vec<u16> = listens.iter().filter(|(pid, _)| descends(*pid, root, &parents)).map(|e| e.1).collect();
        ports.sort_unstable();
        ports.dedup();
        out.insert(root, ports);
    }
    out
}

#[cfg(windows)]
pub fn listening_ports(_roots: &[u32]) -> HashMap<u32, Vec<u16>> {
    HashMap::new()
}

/// `ps -o pid=,ppid=` lines: each pid's parent.
fn parse_ps(text: &str) -> HashMap<u32, u32> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace().map(str::parse::<u32>);
            Some((it.next()?.ok()?, it.next()?.ok()?))
        })
        .collect()
}

/// `lsof -F pn` output: (pid, port) of each listening socket.
fn parse_lsof(text: &str) -> Vec<(u32, u16)> {
    let mut pid = None;
    let mut out = Vec::new();
    for line in text.lines() {
        if let Some(p) = line.strip_prefix('p') {
            pid = p.parse().ok();
        } else if let (Some(name), Some(pid)) = (line.strip_prefix('n'), pid)
            && let Some(port) = name.rsplit(':').next().and_then(|p| p.parse().ok())
        {
            out.push((pid, port));
        }
    }
    out
}

/// `pid` is `root` or runs under it.
fn descends(mut pid: u32, root: u32, parents: &HashMap<u32, u32>) -> bool {
    for _ in 0..64 {
        if pid == root {
            return true;
        }
        match parents.get(&pid) {
            Some(&p) if p != pid && p != 0 => pid = p,
            _ => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_of_a_tree() {
        let parents = parse_ps("  1     0\n 10     1\n 11    10\n 12    11\n 20     1\n");
        let listens = parse_lsof("p12\nf20\nn*:5173\nf21\nn[::1]:5173\np20\nn127.0.0.1:8080\n");
        assert_eq!(listens, vec![(12, 5173), (12, 5173), (20, 8080)]);
        assert!(descends(12, 10, &parents));
        assert!(!descends(20, 10, &parents));
        assert!(descends(10, 10, &parents));
    }
}
