//! Off macOS (where `open -b dev.tern` does it), a second Tern hands its
//! folder to the running one over a loopback socket. The port and a random
//! token sit in a file in Tern's cache folder, which only the user can read.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

/// A request: open this folder (`None`: just come to the front).
pub type Request = Option<PathBuf>;

fn file() -> PathBuf {
    crate::dirs::cache().join(if cfg!(debug_assertions) { "dev-handoff" } else { "handoff" })
}

/// Accept requests from later Terns; they arrive on the receiver.
pub fn listen() -> Option<flume::Receiver<Request>> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| log::warn!("handoff: {e}")).ok()?;
    let port = listener.local_addr().ok()?.port();
    let token = tern_bridge::hooks::random_token();
    let path = file();
    let dir = path.parent()?;
    let _ = std::fs::create_dir_all(dir);
    #[cfg(unix)]
    {
        // Before the token is written: only the user may read it.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).ok()?;
    }
    crate::window_state::write_atomic(&path, format!("{port} {token}\n").as_bytes()).map_err(|e| log::warn!("handoff: {e}")).ok()?;
    let (tx, rx) = flume::unbounded();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            if let Some(req) = read_request(stream, &token) {
                let _ = tx.send(req);
            }
        }
    });
    Some(rx)
}

/// One line: `<token> <folder>` (the folder may be empty).
fn read_request(stream: TcpStream, token: &str) -> Option<Request> {
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let mut line = String::new();
    BufReader::new(stream.take(64 * 1024)).read_line(&mut line).ok()?;
    let (got, folder) = line.trim_end_matches(['\r', '\n']).split_once(' ')?;
    tern_bridge::hooks::constant_time_eq(got.as_bytes(), token.as_bytes()).then(|| (!folder.is_empty()).then(|| PathBuf::from(folder)))
}

/// Hand `folder` to the running Tern.
pub fn send(folder: Option<PathBuf>) -> std::io::Result<()> {
    let text = std::fs::read_to_string(file())?;
    let (port, token) = text.trim().split_once(' ').ok_or_else(|| std::io::Error::other("bad handoff file"))?;
    let port: u16 = port.parse().map_err(std::io::Error::other)?;
    let mut s = TcpStream::connect_timeout(&(Ipv4Addr::LOCALHOST, port).into(), Duration::from_secs(2))?;
    let folder = folder.map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    s.write_all(format!("{token} {folder}\n").as_bytes())
}
