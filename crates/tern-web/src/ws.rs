//! Just enough WebSocket (RFC 6455) for one browser page: the handshake,
//! and frames read and written on separate threads (no extensions).

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::Mutex;

use base64::Engine;

/// The most a browser may send in one message (a big paste fits).
pub const MAX_MESSAGE: usize = 4 << 20;

/// `Sec-WebSocket-Accept` for a client's `Sec-WebSocket-Key`.
pub fn accept_key(key: &str) -> String {
    let mut sha = sha1_smol::Sha1::new();
    sha.update(key.trim().as_bytes());
    sha.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    base64::engine::general_purpose::STANDARD.encode(sha.digest().bytes())
}

#[derive(Debug, PartialEq, Eq)]
pub enum Message {
    Text(String),
    Binary(Vec<u8>),
    Close,
}

const OP_CONT: u8 = 0x0;
const OP_TEXT: u8 = 0x1;
const OP_BINARY: u8 = 0x2;
const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
const OP_PONG: u8 = 0xA;

/// The sending half, shared by the threads that send.
pub struct Sender {
    stream: Mutex<TcpStream>,
}

impl Sender {
    pub fn new(stream: TcpStream) -> Self {
        Self { stream: Mutex::new(stream) }
    }

    pub fn text(&self, text: &str) -> io::Result<()> {
        self.frame(OP_TEXT, text.as_bytes())
    }

    pub fn binary(&self, bytes: &[u8]) -> io::Result<()> {
        self.frame(OP_BINARY, bytes)
    }

    pub fn close(&self) {
        let _ = self.frame(OP_CLOSE, &[]);
        let _ = self.stream.lock().unwrap().shutdown(std::net::Shutdown::Both);
    }

    fn frame(&self, op: u8, payload: &[u8]) -> io::Result<()> {
        let mut out = Vec::with_capacity(payload.len() + 10);
        encode_frame(&mut out, op, payload);
        self.stream.lock().unwrap().write_all(&out)
    }
}

/// A final, unmasked (server to client) frame.
fn encode_frame(out: &mut Vec<u8>, op: u8, payload: &[u8]) {
    out.push(0x80 | op);
    match payload.len() {
        n if n < 126 => out.push(n as u8),
        n if n <= 0xFFFF => {
            out.push(126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        },
        n => {
            out.push(127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        },
    }
    out.extend_from_slice(payload);
}

/// The next message from the browser; pings are answered on `sender`.
/// `Ok(Message::Close)` when it closes or the stream ends.
pub fn read_message(r: &mut impl Read, sender: &Sender) -> io::Result<Message> {
    let mut data = Vec::new();
    let mut kind = None;
    loop {
        let mut head = [0u8; 2];
        if let Err(e) = r.read_exact(&mut head) {
            return if e.kind() == io::ErrorKind::UnexpectedEof { Ok(Message::Close) } else { Err(e) };
        }
        let fin = head[0] & 0x80 != 0;
        let op = head[0] & 0x0F;
        if head[1] & 0x80 == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "unmasked client frame"));
        }
        let len = match head[1] & 0x7F {
            126 => {
                let mut b = [0u8; 2];
                r.read_exact(&mut b)?;
                u16::from_be_bytes(b) as usize
            },
            127 => {
                let mut b = [0u8; 8];
                r.read_exact(&mut b)?;
                usize::try_from(u64::from_be_bytes(b)).unwrap_or(usize::MAX)
            },
            n => n as usize,
        };
        if data.len().saturating_add(len) > MAX_MESSAGE {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "message too large"));
        }
        let mut mask = [0u8; 4];
        r.read_exact(&mut mask)?;
        let mut payload = vec![0u8; len];
        r.read_exact(&mut payload)?;
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= mask[i % 4];
        }
        match op {
            OP_PING => sender.frame(OP_PONG, &payload)?,
            OP_PONG => {},
            OP_CLOSE => return Ok(Message::Close),
            OP_TEXT | OP_BINARY if kind.is_none() => {
                kind = Some(op);
                data = payload;
            },
            OP_CONT if kind.is_some() => data.extend_from_slice(&payload),
            _ => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("unexpected opcode {op}"))),
        }
        if fin && let Some(op) = kind {
            return Ok(if op == OP_TEXT {
                Message::Text(String::from_utf8(data).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?)
            } else {
                Message::Binary(data)
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::net::TcpListener;

    /// A browser's frame: masked.
    fn client_frame(fin: bool, op: u8, payload: &[u8]) -> Vec<u8> {
        let mask = [1u8, 2, 3, 4];
        let mut out = vec![if fin { 0x80 } else { 0 } | op];
        match payload.len() {
            n if n < 126 => out.push(0x80 | n as u8),
            n => {
                out.push(0x80 | 126);
                out.extend_from_slice(&(n as u16).to_be_bytes());
            },
        }
        out.extend_from_slice(&mask);
        out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        out
    }

    fn sender() -> (Sender, TcpStream) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let a = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let (b, _) = l.accept().unwrap();
        (Sender::new(a), b)
    }

    #[test]
    fn accept_key_matches_the_rfc() {
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn reads_text_fragments_and_pings() {
        let (s, mut peer) = sender();
        let mut bytes = client_frame(true, OP_PING, b"p");
        bytes.extend(client_frame(false, OP_TEXT, "hé".as_bytes()));
        bytes.extend(client_frame(true, OP_CONT, &[b'x'; 300]));
        bytes.extend(client_frame(true, OP_CLOSE, &[]));
        let mut r = Cursor::new(bytes);
        assert_eq!(read_message(&mut r, &s).unwrap(), Message::Text(format!("hé{}", "x".repeat(300))));
        assert_eq!(read_message(&mut r, &s).unwrap(), Message::Close);
        // The ping was answered with a pong.
        let mut pong = [0u8; 3];
        peer.read_exact(&mut pong).unwrap();
        assert_eq!(pong, [0x80 | OP_PONG, 1, b'p']);
    }

    #[test]
    fn refuses_unmasked_and_huge_frames() {
        let (s, _peer) = sender();
        let mut r = Cursor::new(vec![0x81, 0x01, b'a']);
        assert!(read_message(&mut r, &s).is_err());
        let mut huge = vec![0x82, 0x80 | 127];
        huge.extend_from_slice(&(MAX_MESSAGE as u64 + 1).to_be_bytes());
        assert!(read_message(&mut Cursor::new(huge), &s).is_err());
    }

    #[test]
    fn writes_each_length_form() {
        for n in [0usize, 125, 126, 70000] {
            let mut out = vec![];
            encode_frame(&mut out, OP_BINARY, &vec![7; n]);
            let header = match n {
                n if n < 126 => 2,
                n if n <= 0xFFFF => 4,
                _ => 10,
            };
            assert_eq!(out.len(), header + n);
            assert_eq!(out[0], 0x80 | OP_BINARY);
        }
    }
}
