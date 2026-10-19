//! Live Server: a folder served over `http://127.0.0.1`, with a script put
//! into every HTML page that reloads it when a file under the folder
//! changes. The app feeds changes in from its fs watcher ([`changed`]).
//!
//! One server per folder, kept until the app quits. Loopback only, and
//! nothing outside the folder is served.

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::thread;
use std::time::Duration;

use crate::http;

/// Ports tried first, so a page keeps its address across runs (VS Code's
/// Live Server starts at 5500 too).
const PORTS: std::ops::Range<u16> = 5500..5600;
/// The event stream the injected script listens to.
const EVENTS: &str = "/__tern_live";
/// A comment on the event stream this often, so a closed tab ends its thread.
const KEEPALIVE: Duration = Duration::from_secs(15);

const SCRIPT: &str = "<script>(()=>{const s=new EventSource('/__tern_live');\
s.onmessage=()=>location.reload();})();</script>";

/// Bumped on every change under the folder; event streams wait on it.
struct Shared {
    port: u16,
    root: PathBuf,
    version: Mutex<u64>,
    bumped: Condvar,
}

/// Servers by their canonical folder.
static SERVERS: LazyLock<Mutex<HashMap<PathBuf, Arc<Shared>>>> = LazyLock::new(Default::default);

/// The port serving `root`, starting a server for it if there is none.
pub fn open(root: &Path) -> io::Result<u16> {
    let root = fs::canonicalize(root)?;
    let mut servers = SERVERS.lock().unwrap();
    if let Some(s) = servers.get(&root) {
        return Ok(s.port);
    }
    let listener = bind()?;
    let port = listener.local_addr()?.port();
    let shared = Arc::new(Shared { port, root: root.clone(), version: Mutex::new(0), bumped: Condvar::new() });
    let s = shared.clone();
    thread::Builder::new().name("live-server".into()).spawn(move || accept_loop(listener, s))?;
    log::info!("live server for {} on port {port}", root.display());
    servers.insert(root, shared);
    Ok(port)
}

/// The page address for `file` under `root`, starting the server if needed.
pub fn url_for(root: &Path, file: &Path) -> io::Result<String> {
    let port = open(root)?;
    let root = fs::canonicalize(root)?;
    let file = fs::canonicalize(file)?;
    let rel = file.strip_prefix(&root).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "file is outside the folder"))?;
    let path: Vec<String> = rel.components().map(|c| encode(&c.as_os_str().to_string_lossy())).collect();
    Ok(format!("http://127.0.0.1:{port}/{}", path.join("/")))
}

/// Paths changed on disk: reload the pages of every server they fall under.
pub fn changed(paths: &[PathBuf]) {
    let servers = SERVERS.lock().unwrap();
    for (root, shared) in servers.iter() {
        if paths.iter().any(|p| under(p, root)) {
            *shared.version.lock().unwrap() += 1;
            shared.bumped.notify_all();
        }
    }
}

/// Whether `path` is in `root`. Watcher paths are not canonical (a `\\?\`
/// prefix or a different drive letter case on Windows), so compare both.
fn under(path: &Path, root: &Path) -> bool {
    path.starts_with(root) || fs::canonicalize(path).is_ok_and(|p| p.starts_with(root)) || {
        // A deleted file no longer canonicalizes: try its parent.
        path.parent().and_then(|d| fs::canonicalize(d).ok()).is_some_and(|d| d.starts_with(root))
    }
}

fn bind() -> io::Result<TcpListener> {
    let at = |port| TcpListener::bind(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port));
    PORTS.clone().find_map(|p| at(p).ok()).map_or_else(|| at(0), Ok)
}

fn accept_loop(listener: TcpListener, shared: Arc<Shared>) {
    for stream in listener.incoming().flatten() {
        let s = shared.clone();
        let _ = thread::Builder::new().name("live-conn".into()).spawn(move || {
            if let Err(e) = handle(stream, &s) {
                log::debug!("live server: {e}");
            }
        });
    }
}

fn handle(stream: TcpStream, shared: &Shared) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let Some(req) = http::read_request(&mut reader)? else { return Ok(()) };
    let mut w = stream;
    if req.method != "GET" && req.method != "HEAD" {
        return send(&mut w, "405 Method Not Allowed", "text/plain", b"method not allowed", false);
    }
    let head = req.method == "HEAD";
    if req.path == EVENTS {
        return events(w, shared);
    }
    let Some(path) = resolve(&shared.root, &req.path) else {
        return send(&mut w, "404 Not Found", "text/plain", b"not found", head);
    };
    if path.is_dir() {
        // A folder without a trailing slash: redirect, so its relative links work.
        if !req.path.ends_with('/') {
            let to = format!("{}/", req.path);
            let out = format!("HTTP/1.1 301 Moved Permanently\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            return w.write_all(out.as_bytes());
        }
        let index = ["index.html", "index.htm"].iter().map(|n| path.join(n)).find(|p| p.is_file());
        return match index {
            Some(index) => file(&mut w, &index, head),
            None => send(&mut w, "200 OK", "text/html; charset=utf-8", listing(&path, &req.path).as_bytes(), head),
        };
    }
    file(&mut w, &path, head)
}

fn file(w: &mut TcpStream, path: &Path, head: bool) -> io::Result<()> {
    let Ok(mut body) = fs::read(path) else {
        return send(w, "404 Not Found", "text/plain", b"not found", head);
    };
    let mime = mime(path);
    if mime.starts_with("text/html") {
        body = inject(body);
    }
    send(w, "200 OK", mime, &body, head)
}

/// Hold the connection open and send an event when the folder changes.
fn events(mut w: TcpStream, shared: &Shared) -> io::Result<()> {
    w.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n: hi\n\n")?;
    let mut seen = *shared.version.lock().unwrap();
    loop {
        let version = shared.version.lock().unwrap();
        let (version, _) = shared.bumped.wait_timeout_while(version, KEEPALIVE, |v| *v == seen).unwrap();
        let now = *version;
        drop(version);
        if now == seen {
            w.write_all(b": keepalive\n\n")?;
            continue;
        }
        // Let a burst of saves land before the page asks for its files again.
        thread::sleep(Duration::from_millis(60));
        seen = *shared.version.lock().unwrap();
        w.write_all(b"data: reload\n\n")?;
        w.flush()?;
    }
}

fn send(w: &mut impl Write, status: &str, mime: &str, body: &[u8], head: bool) -> io::Result<()> {
    let out = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    w.write_all(out.as_bytes())?;
    if !head {
        w.write_all(body)?;
    }
    w.flush()
}

/// The file a request path names under `root`, if it is there and inside it.
fn resolve(root: &Path, path: &str) -> Option<PathBuf> {
    let decoded = decode(path)?;
    let mut out = root.to_path_buf();
    for part in decoded.split('/').filter(|p| !p.is_empty()) {
        let part = Path::new(part);
        // Only plain names: no `..`, no drive or root.
        match part.components().collect::<Vec<_>>().as_slice() {
            [Component::Normal(_)] => out.push(part),
            _ => return None,
        }
    }
    // Symlinks may point anywhere: check where it really is.
    let real = fs::canonicalize(&out).ok()?;
    real.starts_with(root).then_some(real)
}

/// The reload script, before `</body>` (or at the end if there is none).
fn inject(body: Vec<u8>) -> Vec<u8> {
    let lower = body.to_ascii_lowercase();
    let at = lower.windows(7).rposition(|w| w == b"</body>").unwrap_or(body.len());
    let mut out = Vec::with_capacity(body.len() + SCRIPT.len());
    out.extend_from_slice(&body[..at]);
    out.extend_from_slice(SCRIPT.as_bytes());
    out.extend_from_slice(&body[at..]);
    out
}

fn listing(dir: &Path, url: &str) -> String {
    let mut names: Vec<(bool, String)> = fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| (e.path().is_dir(), e.file_name().to_string_lossy().into_owned())).collect())
        .unwrap_or_default();
    names.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())));
    let title = escape(&decode(url).unwrap_or_default());
    let mut html = format!("<!doctype html><meta charset=utf-8><title>{title}</title><h1>{title}</h1><ul>");
    if url != "/" {
        html += "<li><a href=\"../\">../</a></li>";
    }
    for (is_dir, name) in names {
        let slash = if is_dir { "/" } else { "" };
        html += &format!("<li><a href=\"{}{slash}\">{}{slash}</a></li>", encode(&name), escape(&name));
    }
    html += "</ul>";
    String::from_utf8(inject(html.into_bytes())).unwrap_or_default()
}

fn mime(path: &Path) -> &'static str {
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json",
        "txt" | "md" => "text/plain; charset=utf-8",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "wasm" => "application/wasm",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// Percent-decode a URL path. `None` if it is not UTF-8 or holds a NUL.
fn decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(byte) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok().filter(|s| !s.contains('\0'))
}

/// Percent-encode one path segment.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, Read};

    fn get(port: u16, path: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(s, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    #[test]
    fn injects_before_body_end() {
        let out = inject(b"<html><BODY>hi</BODY></html>".to_vec());
        assert_eq!(String::from_utf8(out).unwrap(), format!("<html><BODY>hi{SCRIPT}</BODY></html>"));
        let out = inject(b"<p>bare".to_vec());
        assert_eq!(String::from_utf8(out).unwrap(), format!("<p>bare{SCRIPT}"));
    }

    #[test]
    fn decodes_and_encodes() {
        assert_eq!(decode("/a%20b/%C3%A9").as_deref(), Some("/a b/é"));
        assert_eq!(decode("/x%00"), None);
        assert_eq!(encode("a b/é"), "a%20b%2F%C3%A9");
    }

    #[test]
    fn serves_files_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        fs::write(root.join("index.html"), "<body>home</body>").unwrap();
        fs::create_dir(root.join("sub dir")).unwrap();
        fs::write(root.join("sub dir/style.css"), "p{}").unwrap();

        let url = url_for(&root, &root.join("sub dir/style.css")).unwrap();
        assert!(url.ends_with("/sub%20dir/style.css"), "{url}");
        let port = open(&root).unwrap();

        let home = get(port, "/");
        assert!(home.contains("home<script>"), "{home}");
        assert!(get(port, "/sub%20dir/style.css").ends_with("\r\n\r\np{}"));
        assert!(get(port, "/sub%20dir").starts_with("HTTP/1.1 301"));
        assert!(get(port, "/sub%20dir/").contains("style.css"));
        assert!(get(port, "/../secret").starts_with("HTTP/1.1 404"));
        assert!(get(port, "/%2e%2e/secret").starts_with("HTTP/1.1 404"));
        assert!(get(port, "/missing.html").starts_with("HTTP/1.1 404"));

        let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(&s, "GET {EVENTS} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut r = BufReader::new(s);
        let mut line = String::new();
        while line != ": hi\n" {
            line.clear();
            r.read_line(&mut line).unwrap();
        }
        changed(&[root.join("index.html")]);
        loop {
            line.clear();
            r.read_line(&mut line).unwrap();
            if line.starts_with("data:") {
                break;
            }
        }
        assert_eq!(line, "data: reload\n");
    }
}
