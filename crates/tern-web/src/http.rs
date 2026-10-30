//! Just enough HTTP/1.1 for the page, its files, the login and the
//! WebSocket upgrade: one request per connection.

use std::io::{self, BufRead, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const MAX_HEAD: usize = 16 << 10;
const MAX_BODY: usize = 4 << 10;

#[derive(Debug, Default)]
pub struct Request {
    pub method: String,
    /// Without the query.
    pub path: String,
    /// Names lowercased.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    /// The value of cookie `name`.
    pub fn cookie(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .filter(|(n, _)| n == "cookie")
            .flat_map(|(_, v)| v.split(';'))
            .filter_map(|kv| kv.trim().split_once('='))
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v)
    }
}

/// A connection read with one deadline for the whole request, so a client
/// sending a byte now and then can't keep it (and its thread) forever.
/// [`Deadline::lift`] it once the request is in (a WebSocket stays open).
pub struct Deadline {
    stream: TcpStream,
    until: Option<Instant>,
}

impl Deadline {
    pub fn new(stream: TcpStream, within: Duration) -> Self {
        Self { stream, until: Some(Instant::now() + within) }
    }

    /// No deadline from now on, nor a timeout on each read.
    pub fn lift(&mut self) -> io::Result<()> {
        self.until = None;
        self.stream.set_read_timeout(None)
    }
}

impl Read for Deadline {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if let Some(until) = self.until {
            let left = until
                .checked_duration_since(Instant::now())
                .filter(|d| !d.is_zero())
                .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "request deadline"))?;
            self.stream.set_read_timeout(Some(left))?;
        }
        self.stream.read(buf).map_err(|e| match e.kind() {
            // How a timed out read shows on Unix.
            io::ErrorKind::WouldBlock => io::Error::new(io::ErrorKind::TimedOut, "request deadline"),
            _ => e,
        })
    }
}

/// So many connections at once, at most, each holding a [`Slot`].
pub struct Slots(Arc<AtomicUsize>, usize);

/// One of [`Slots`], given back when dropped.
pub struct Slot(Arc<AtomicUsize>);

impl Slots {
    pub fn new(max: usize) -> Self {
        Self(Arc::default(), max)
    }

    /// A slot, or `None` when all are taken.
    pub fn take(&self) -> Option<Slot> {
        let taken = self.0.fetch_add(1, Ordering::AcqRel);
        if taken >= self.1 {
            self.0.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Slot(self.0.clone()))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Read one request (its body too, for a small `POST`). `Ok(None)` when the
/// connection closed before one came.
pub fn read_request(r: &mut impl BufRead) -> io::Result<Option<Request>> {
    let mut head = Vec::new();
    let mut lines = Vec::new();
    loop {
        let mut line = Vec::new();
        let n = io::Read::take(&mut *r, (MAX_HEAD - head.len()) as u64).read_until(b'\n', &mut line)?;
        if n == 0 {
            return if lines.is_empty() { Ok(None) } else { Err(io::ErrorKind::UnexpectedEof.into()) };
        }
        head.extend_from_slice(&line);
        if head.len() >= MAX_HEAD {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "request head too large"));
        }
        let line = String::from_utf8_lossy(&line).trim_end_matches(['\r', '\n']).to_owned();
        if line.is_empty() {
            break;
        }
        lines.push(line);
    }
    let mut it = lines.into_iter();
    let first = it.next().unwrap_or_default();
    let mut parts = first.split(' ');
    let method = parts.next().unwrap_or_default().to_owned();
    let target = parts.next().unwrap_or_default();
    let path = target.split(['?', '#']).next().unwrap_or_default().to_owned();
    let headers: Vec<(String, String)> =
        it.filter_map(|l| l.split_once(':').map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned()))).collect();
    let mut req = Request { method, path, headers, body: Vec::new() };
    let len: usize = req.header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    if len > MAX_BODY {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "body too large"));
    }
    req.body = vec![0; len];
    r.read_exact(&mut req.body)?;
    Ok(Some(req))
}

/// Write a whole response and its body.
pub fn respond(w: &mut impl Write, status: &str, headers: &[(&str, &str)], body: &[u8]) -> io::Result<()> {
    let mut out = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    // Nothing here is for other sites to frame or sniff.
    out += "X-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\n";
    for (n, v) in headers {
        out += &format!("{n}: {v}\r\n");
    }
    out += "\r\n";
    w.write_all(out.as_bytes())?;
    w.write_all(body)?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn parses_head_body_and_cookies() {
        let raw = "POST /login?x=1 HTTP/1.1\r\nHost: a:1\r\nCookie: a=1; tern=xyz\r\nContent-Length: 3\r\n\r\nabcEXTRA";
        let mut r = Cursor::new(raw.as_bytes().to_vec());
        let req = read_request(&mut r).unwrap().unwrap();
        assert_eq!((req.method.as_str(), req.path.as_str()), ("POST", "/login"));
        assert_eq!(req.header("host"), Some("a:1"));
        assert_eq!(req.cookie("tern"), Some("xyz"));
        assert_eq!(req.cookie("b"), None);
        assert_eq!(req.body, b"abc");
        // What follows stays in the reader (a WebSocket's first frames).
        let mut rest = String::new();
        std::io::Read::read_to_string(&mut r, &mut rest).unwrap();
        assert_eq!(rest, "EXTRA");
    }

    #[test]
    fn closed_connection_is_none() {
        assert!(read_request(&mut Cursor::new(Vec::new())).unwrap().is_none());
    }

    #[test]
    fn a_slow_request_runs_out_of_time() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut peer = std::net::TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let (s, _) = l.accept().unwrap();
        // A byte now and then: each read is quick, the whole head never ends.
        std::thread::spawn(move || {
            for _ in 0..40 {
                if peer.write_all(b"a").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let start = Instant::now();
        let mut r = io::BufReader::new(Deadline::new(s, Duration::from_millis(300)));
        let err = read_request(&mut r).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn slots_run_out_and_come_back() {
        let slots = Slots::new(2);
        let a = slots.take().unwrap();
        let _b = slots.take().unwrap();
        assert!(slots.take().is_none());
        drop(a);
        assert!(slots.take().is_some());
    }

    #[test]
    fn refuses_huge_heads() {
        let raw = format!("GET / HTTP/1.1\r\nX: {}\r\n\r\n", "a".repeat(MAX_HEAD));
        assert!(read_request(&mut Cursor::new(raw.into_bytes())).is_err());
    }
}
