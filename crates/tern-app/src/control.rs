//! Debug-build control socket: lets scripts drive a window (clicks, keys,
//! state dump) by injecting events into it, without touching the real mouse
//! or keyboard. Enabled with `TERN_CONTROL=<socket path>`.
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
//!   windows / ws     list windows / this window's workspaces
//!   ws-open PATH, ws-close [N], ws-activate N, ws-reorder FROM TO,
//!   ws-detach N, ws-move N WINDOW, ws-dropout N X Y, new-window
//!   restart-background   end the daemon now (Restart Background Terminals, no dialog)
//!   term-type TEXT   type into the active terminal (`\n` Enter, `\e` Esc, `\a` BEL)
//!   term-screen      the active terminal's visible text
//!   term-info        its size, focus reporting, exit; tabs that rang; what runs
//!   term-paste-path P1|P2   paste paths as a file drop does
//!   @N CMD           send CMD to window N instead of the active one

#[cfg(unix)]
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::net::UnixListener;
use std::path::PathBuf;

use gpui_kit::*;

pub type Request = (String, flume::Sender<String>);

/// No Unix sockets here: the control socket is a Unix-only debug aid.
#[cfg(not(unix))]
pub fn listen(_path: PathBuf) -> Option<flume::Receiver<Request>> {
    log::warn!("TERN_CONTROL needs a Unix socket; ignored");
    None
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
    let (tx, rx) = flume::unbounded::<Request>();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let Ok(mut out) = stream.try_clone() else { return };
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
    log::info!("control socket at {}", path.display());
    Some(rx)
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
        // targets that appear mid-drag are hit), release.
        "drag" => {
            let n: Vec<f32> = parts.filter_map(|s| s.parse().ok()).collect();
            let [x1, y1, x2, y2] = n[..] else { return Some("error: drag X1 Y1 X2 Y2".into()) };
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
            window.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { button, position: to, modifiers, click_count: 1 }), cx);
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
