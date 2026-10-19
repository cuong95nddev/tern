//! One browser page: its WebSocket, and a viewer connection to the daemon
//! of its own. The page views one session at a time.
//!
//! To the page: `hello` (the theme), `sessions` (the list, when it
//! changes), `view` (a view began: the session's size; the bytes that paint
//! its snapshot follow as one binary message), binary messages (the
//! session's output), `resize`, `exited`, `gone` and `error`. From the page:
//! `view`, `input`, `resize` (make the PTY this size: the page always fits
//! it to its screen), `unfit` (give it back its size). Session ids go as strings: they don't fit a JavaScript
//! number.

use std::io::BufReader;
use std::net::TcpStream;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use serde_json::json;
use tern_mux::viewer::{ViewEvent, Viewer};
use tern_term::TermSize;
use tern_term::mirror::Marker;
use tern_term::palette::{Palette, PaletteFile};
use tern_term::vt::vte::ansi::Rgb;

use crate::ws::{self, Message, Sender};

#[derive(Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
enum FromPage {
    View {
        #[serde(with = "id_string")]
        id: u64,
    },
    Input {
        d: String,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
    Unfit,
}

/// The page's view.
#[derive(Default)]
struct State {
    id: Option<u64>,
    /// Its size, as last heard from the daemon.
    size: Option<TermSize>,
    /// The size the page gave it, and the one it had before.
    fitted: Option<(TermSize, TermSize)>,
}

impl State {
    /// Stop viewing, giving the session back the size it had if the page
    /// changed it (and nobody did since).
    fn leave(&mut self, viewer: &Viewer) {
        if let Some(id) = self.id.take() {
            let _ = viewer.unview(id);
            self.unfit(viewer, id);
        }
        self.size = None;
    }

    fn unfit(&mut self, viewer: &Viewer, id: u64) {
        if let Some((set, orig)) = self.fitted.take()
            && self.size == Some(set)
        {
            let _ = viewer.resize(id, orig);
        }
    }
}

pub fn serve(mut reader: BufReader<TcpStream>, sender: Arc<Sender>, mux_dir: &Path) {
    let palette = PaletteFile::new(tern_mux::paths::palette_path()).get();
    if sender.text(&json!({ "t": "hello", "theme": theme(&palette) }).to_string()).is_err() {
        return;
    }
    let (viewer, mut stream) = match Viewer::connect(mux_dir) {
        Ok(v) => v,
        Err(e) => {
            let message = match e.kind() {
                std::io::ErrorKind::Unsupported => "Tern's background server is older than this page: quit Tern and start it again".into(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => "Tern isn't running".into(),
                _ => format!("Can't reach Tern's background server: {e}"),
            };
            let _ = sender.text(&json!({ "t": "error", "message": message }).to_string());
            return sender.close();
        },
    };
    let state = Arc::new(Mutex::new(State::default()));
    let daemon = {
        let (viewer, sender, state) = (viewer.clone(), sender.clone(), state.clone());
        std::thread::Builder::new().name("web daemon".into()).spawn(move || {
            while let Some(event) = stream.next() {
                if on_event(event, &viewer, &sender, &state).is_err() {
                    break;
                }
            }
            let _ = sender.text(&json!({ "t": "error", "message": "Tern's background server stopped" }).to_string());
            sender.close();
        })
    };
    if daemon.is_err() {
        return sender.close();
    }
    loop {
        let msg = match ws::read_message(&mut reader, &sender) {
            Ok(Message::Text(t)) => t,
            Ok(Message::Binary(_)) => continue,
            Ok(Message::Close) => break,
            Err(e) => {
                log::info!("page: {e}");
                break;
            },
        };
        let msg = match serde_json::from_str::<FromPage>(&msg) {
            Ok(m) => m,
            Err(e) => {
                log::info!("page sent {msg:?}: {e}");
                continue;
            },
        };
        let mut st = state.lock().unwrap();
        match msg {
            FromPage::View { id } => {
                st.leave(&viewer);
                st.id = Some(id);
                let _ = viewer.view(id);
            },
            FromPage::Input { d } => {
                if let Some(id) = st.id {
                    let _ = viewer.input(id, d.into_bytes());
                }
            },
            FromPage::Resize { cols, rows } => {
                if let (Some(id), Some(size)) = (st.id, st.size) {
                    let want = TermSize { cols: cols.clamp(10, 500), rows: rows.clamp(3, 300), ..size };
                    if want != size {
                        // The size from before this page, unless another view
                        // set one since.
                        let orig = match st.fitted {
                            Some((set, orig)) if set == size => orig,
                            _ => size,
                        };
                        st.fitted = Some((want, orig));
                        let _ = viewer.resize(id, want);
                    }
                }
            },
            FromPage::Unfit => {
                if let Some(id) = st.id {
                    st.unfit(&viewer, id);
                }
            },
        }
    }
    state.lock().unwrap().leave(&viewer);
    viewer.shutdown();
    sender.close();
}

/// Pass one daemon event on to the page. Errs when the page is gone.
fn on_event(event: ViewEvent, viewer: &Viewer, sender: &Sender, state: &Mutex<State>) -> std::io::Result<()> {
    let mut st = state.lock().unwrap();
    match event {
        ViewEvent::Sessions(list) => {
            drop(st);
            let list: Vec<_> = list
                .iter()
                .map(|s| {
                    let mut v = serde_json::to_value(s).unwrap_or_default();
                    v["id"] = s.id.to_string().into();
                    v
                })
                .collect();
            sender.text(&json!({ "t": "sessions", "list": list }).to_string())
        },
        ViewEvent::Viewing { id, snapshot } if st.id == Some(id) => {
            st.size = Some(snapshot.size);
            drop(st);
            let s = snapshot.size;
            sender
                .text(&json!({ "t": "view", "id": id.to_string(), "cols": s.cols, "rows": s.rows, "title": snapshot.title }).to_string())?;
            sender.binary(&tern_term::replay::to_vt(&snapshot))
        },
        ViewEvent::Output { id, bytes } if st.id == Some(id) => {
            drop(st);
            sender.binary(&bytes)
        },
        ViewEvent::Marker { id, marker } if st.id == Some(id) => match marker {
            Marker::Resized(s) => {
                st.size = Some(s);
                drop(st);
                sender.text(&json!({ "t": "resize", "cols": s.cols, "rows": s.rows }).to_string())
            },
            Marker::SyncFlush => Ok(()),
            Marker::Exited(code) => {
                drop(st);
                sender.text(&json!({ "t": "exited", "code": code }).to_string())
            },
        },
        // Fell behind: start again from a fresh snapshot.
        ViewEvent::Lagged { id } if st.id == Some(id) => {
            let _ = viewer.view(id);
            Ok(())
        },
        ViewEvent::NotFound { id } if st.id == Some(id) => {
            st.id = None;
            st.size = None;
            st.fitted = None;
            drop(st);
            sender.text(&json!({ "t": "gone", "id": id.to_string() }).to_string())
        },
        // For a session the page no longer views.
        _ => Ok(()),
    }
}

mod id_string {
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        String::deserialize(d)?.parse().map_err(serde::de::Error::custom)
    }
}

fn hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

/// The app's palette as an xterm.js theme.
fn theme(p: &Palette) -> serde_json::Value {
    const NAMES: [&str; 16] = [
        "black",
        "red",
        "green",
        "yellow",
        "blue",
        "magenta",
        "cyan",
        "white",
        "brightBlack",
        "brightRed",
        "brightGreen",
        "brightYellow",
        "brightBlue",
        "brightMagenta",
        "brightCyan",
        "brightWhite",
    ];
    let mut t = serde_json::Map::new();
    for (name, c) in NAMES.iter().zip(p.ansi()) {
        t.insert((*name).into(), hex(c).into());
    }
    t.insert("foreground".into(), hex(p.foreground).into());
    t.insert("background".into(), hex(p.background).into());
    t.insert("cursor".into(), hex(p.cursor).into());
    t.insert("cursorAccent".into(), hex(p.background).into());
    t.insert("selectionBackground".into(), hex(p.selection).into());
    t.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_the_page_sends() {
        let m: FromPage = serde_json::from_str(r#"{"t":"view","id":"9223372036854775807"}"#).unwrap();
        assert!(matches!(m, FromPage::View { id: 9223372036854775807 }));
        let m: FromPage = serde_json::from_str(r#"{"t":"input","d":"ls\r"}"#).unwrap();
        assert!(matches!(m, FromPage::Input { d } if d == "ls\r"));
        let m: FromPage = serde_json::from_str(r#"{"t":"resize","cols":80,"rows":24}"#).unwrap();
        assert!(matches!(m, FromPage::Resize { cols: 80, rows: 24 }));
        assert!(serde_json::from_str::<FromPage>(r#"{"t":"view","id":12}"#).is_err());
    }

    #[test]
    fn theme_has_every_color() {
        let t = theme(&Palette::default());
        for k in ["black", "brightWhite", "foreground", "background", "cursor", "selectionBackground"] {
            assert!(t[k].as_str().is_some_and(|c| c.starts_with('#') && c.len() == 7), "{k}");
        }
    }
}
