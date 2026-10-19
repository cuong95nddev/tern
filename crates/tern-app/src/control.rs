//! Debug-build control socket: lets scripts drive a window (clicks, keys,
//! state dump) by injecting events into it, without touching the real mouse
//! or keyboard. Enabled with `TERN_CONTROL=<socket path>` (on Windows, a
//! loopback address: `127.0.0.1:7777`).
//!
//! One command per line, one reply line each:
//!   click X Y        left click at window coordinates (logical px)
//!   rclick X Y       right click there
//!   drag X1 Y1 X2 Y2 press, move and release the left button (a drag and drop)
//!   key KEYSTROKE    e.g. `escape`, `cmd-w`, `cmd-shift-g`
//!   state            tabs, active tab, whether a dialog is open
//!   scratch-new EXT  create and open a scratch file (as the + menu does)
//!   scratch-open NAME   open an existing scratch file by file name
//!   scratch-text T   replace the active scratch tab's text (`\n` = newline)
//!   file-open PATH   open a file (relative to the workspace root) in a tab
//!   tab-move FROM TO move center tab FROM to slot TO (as a drag does)
//!   tab-layout scroll|wrap|vertical   how tabs are laid out
//!   group-new TAB, group-add TAB GROUP, group-remove TAB, group-move GROUP TAB,
//!   group-toggle|ungroup|close GROUP, group-name GROUP NAME,
//!   group-color GROUP COLOR   tab groups (indices as `state` lists them)
//!   tab-scroll       the tab strip's scroll offset and maximum
//!   pane-drop TAB PANE ZONE (center|left|right|up|down), pane-split TAB EDGE,
//!   pane-layout COLS ROWS, pane-tile, pane-zoom, pane-focus PANE|EDGE,
//!   pane-close PANE   split panes (indices as `state` lists them)
//!   layout-reset                                    put the side panels back (View > Reset Layout)
//!   part-move PART DOCK before|after|end [TARGET]   drop a side panel in a dock
//!                    (as dragging its header does); `state` ends with `arrange …`
//!   part-center PART PANE|PART left|right|up|down   drop a side panel beside a pane or a
//!                    part among the panes (when the center dock takes it)
//!   windows / ws     list windows / this window's workspaces
//!   ws-open PATH, ws-close [N], ws-activate N, ws-reorder FROM TO,
//!   ws-detach N, ws-move N WINDOW, ws-dropout N X Y, new-window
//!   settings SECTION open the Settings window at a section (`general`, `connections`, …)
//!   settings-input CMD   `click X Y`, `key K`, … in the Settings window
//!   restart-background   end the daemon now (Restart Background Terminals, no dialog)
//!   term-type TEXT   type into the active terminal (`\n` Enter, `\e` Esc, `\a` BEL)
//!   term-screen      the active terminal's visible text
//!   term-info        its size, focus reporting, exit; tabs that rang; what runs
//!   term-paste-path P1|P2   paste paths as a file drop does
//!   @N CMD           send CMD to window N instead of the active one

use std::io::{BufRead, BufReader, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use gpui_kit::*;

pub type Request = (String, flume::Sender<String>);

/// No Unix sockets in std here: `TERN_CONTROL` is a loopback TCP address
/// (`127.0.0.1:7777`) instead.
#[cfg(not(unix))]
pub fn listen(path: PathBuf) -> Option<flume::Receiver<Request>> {
    use std::net::{SocketAddr, TcpListener, TcpStream};
    let addr: SocketAddr = path.to_str()?.parse().map_err(|_| log::warn!("TERN_CONTROL: not an address like 127.0.0.1:7777")).ok()?;
    if !addr.ip().is_loopback() {
        log::warn!("TERN_CONTROL {addr} is not a loopback address");
        return None;
    }
    let listener = TcpListener::bind(addr).map_err(|e| log::warn!("control socket: {e}")).ok()?;
    log::info!("control socket at {addr}");
    let incoming = std::iter::repeat_with(move || listener.accept()).filter_map(|r| r.ok().map(|(s, _)| s));
    Some(serve(incoming, TcpStream::try_clone))
}

/// Answers each line of each connection `incoming` gives with the reply to
/// its request; `clone` gives a connection's writing half.
fn serve<S: Read + Write + Send + 'static>(
    incoming: impl Iterator<Item = S> + Send + 'static,
    clone: fn(&S) -> std::io::Result<S>,
) -> flume::Receiver<Request> {
    let (tx, rx) = flume::unbounded::<Request>();
    std::thread::spawn(move || {
        for stream in incoming {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let Ok(mut out) = clone(&stream) else { return };
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    let (reply_tx, reply_rx) = flume::bounded(1);
                    if tx.send((line, reply_tx)).is_err() {
                        return;
                    }
                    let reply = reply_rx.recv().unwrap_or_else(|_| "error: window closed".into());
                    if writeln!(out, "{reply}").is_err() {
                        return;
                    }
                }
            });
        }
    });
    rx
}

/// Start listening on `path` (from `TERN_CONTROL`); requests arrive on the
/// receiver.
#[cfg(unix)]
pub fn listen(path: PathBuf) -> Option<flume::Receiver<Request>> {
    // Replace a stale socket, never another kind of file.
    if let Ok(m) = std::fs::symlink_metadata(&path) {
        if !m.file_type().is_socket() {
            log::warn!("TERN_CONTROL {} exists and is not a socket", path.display());
            return None;
        }
        let _ = std::fs::remove_file(&path);
    }
    let listener = UnixListener::bind(&path).map_err(|e| log::warn!("control socket: {e}")).ok()?;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    log::info!("control socket at {}", path.display());
    let incoming = std::iter::repeat_with(move || listener.accept()).filter_map(|r| r.ok().map(|(s, _)| s));
    Some(serve(incoming, UnixStream::try_clone))
}

/// Run an input command (`click`, `key`) against `window`. `None` if `line`
/// is not one of those.
pub fn input(line: &str, window: &mut Window, cx: &mut App) -> Option<String> {
    let mut parts = line.split_whitespace();
    match parts.next()? {
        cmd @ ("click" | "rclick") => {
            let button = if cmd == "rclick" { MouseButton::Right } else { MouseButton::Left };
            let mut num = || parts.next().and_then(|s| s.parse::<f32>().ok());
            let (Some(x), Some(y)) = (num(), num()) else { return Some("error: click X Y".into()) };
            // An occluded window stops drawing, leaving hit-testing on a stale
            // frame; lay out the current state first.
            window.draw(cx).clear(cx);
            let position = point(px(x), px(y));
            let modifiers = Modifiers::default();
            window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers }), cx);
            window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent { button, position, modifiers, click_count: 1, first_mouse: false }),
                cx,
            );
            window.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { button, position, modifiers, click_count: 1 }), cx);
            Some("ok".into())
        },
        // Press at X1 Y1, move there to X2 Y2 in steps (redrawing, so drop
        // targets that appear mid-drag are hit), release; with `hold`, stay
        // pressed until `release`.
        "drag" => {
            let words: Vec<&str> = parts.collect();
            let hold = words.last() == Some(&"hold");
            let n: Vec<f32> = words.iter().filter_map(|s| s.parse().ok()).collect();
            let [x1, y1, x2, y2] = n[..] else { return Some("error: drag X1 Y1 X2 Y2 [hold]".into()) };
            window.draw(cx).clear(cx);
            let (button, modifiers) = (MouseButton::Left, Modifiers::default());
            let from = point(px(x1), px(y1));
            window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position: from, pressed_button: None, modifiers }), cx);
            window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent { button, position: from, modifiers, click_count: 1, first_mouse: false }),
                cx,
            );
            const STEPS: usize = 8;
            for k in 1..=STEPS {
                let t = k as f32 / STEPS as f32;
                let position = point(px(x1 + (x2 - x1) * t), px(y1 + (y2 - y1) * t));
                window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: Some(button), modifiers }), cx);
                window.draw(cx).clear(cx);
            }
            let to = point(px(x2), px(y2));
            window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position: to, pressed_button: Some(button), modifiers }), cx);
            window.draw(cx).clear(cx);
            if !hold {
                window.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { button, position: to, modifiers, click_count: 1 }), cx);
            }
            Some("ok".into())
        },
        // Release a `drag … hold` at X Y.
        "release" => {
            let n: Vec<f32> = parts.filter_map(|s| s.parse().ok()).collect();
            let [x, y] = n[..] else { return Some("error: release X Y".into()) };
            let position = point(px(x), px(y));
            window.draw(cx).clear(cx);
            let modifiers = Modifiers::default();
            window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Left, position, modifiers, click_count: 1 }),
                cx,
            );
            Some("ok".into())
        },
        // Move the pointer to X Y (hover), or scroll there by DX DY pixels.
        cmd @ ("move" | "scroll") => {
            let n: Vec<f32> = parts.filter_map(|s| s.parse().ok()).collect();
            let (position, modifiers) = match n[..] {
                [x, y, ..] => (point(px(x), px(y)), Modifiers::default()),
                _ => return Some(format!("error: {cmd} X Y{}", if cmd == "scroll" { " DX DY" } else { "" })),
            };
            window.draw(cx).clear(cx);
            window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers }), cx);
            if let [_, _, dx, dy] = n[..] {
                let delta = ScrollDelta::Pixels(point(px(dx), px(dy)));
                window
                    .dispatch_event(PlatformInput::ScrollWheel(ScrollWheelEvent { position, delta, modifiers, ..Default::default() }), cx);
            } else if cmd == "scroll" {
                return Some("error: scroll X Y DX DY".into());
            }
            window.draw(cx).clear(cx);
            Some("ok".into())
        },
        "key" => {
            let Some(k) = parts.next() else { return Some("error: key KEYSTROKE".into()) };
            match Keystroke::parse(k) {
                Ok(ks) => {
                    // Dispatch on the current focus, not a stale frame's.
                    window.draw(cx).clear(cx);
                    let handled = window.dispatch_keystroke(ks, cx);
                    Some(format!("ok handled={handled}"))
                },
                Err(e) => Some(format!("error: {e}")),
            }
        },
        _ => None,
    }
}
