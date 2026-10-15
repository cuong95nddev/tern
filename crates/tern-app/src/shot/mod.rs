//! Screenshots for agents: capture part of the screen, mark it up, collect
//! a few, and send them with a prompt to an agent tab.

pub mod grab;
mod icons;
pub mod model;
mod overlay;
pub mod raster;
pub mod targets;
mod tray;

use std::time::Duration;

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use gpui_kit::*;

use overlay::Overlay;

actions!(tern, [CaptureScreenshot]);

/// Captures are kept this long.
const KEEP: Duration = Duration::from_secs(7 * 24 * 3600);

/// The overlays of the capture on screen, if one is.
#[derive(Default)]
struct Capture {
    on: bool,
    overlays: Vec<AnyWindowHandle>,
    /// Keeps the hotkey registered.
    _hotkey: Option<GlobalHotKeyManager>,
}

impl Global for Capture {}

/// ⌘⇧2 anywhere, and the menu action.
pub fn init(cx: &mut App) {
    cx.on_action(|_: &CaptureScreenshot, cx| start(cx));
    cx.background_executor().spawn(async { grab::cleanup(&grab::dir(), KEEP) }).detach();
    let manager = match GlobalHotKeyManager::new() {
        Ok(m) => m,
        Err(e) => return log::warn!("no global hotkey: {e}"),
    };
    let hotkey = HotKey::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::Digit2);
    if let Err(e) = manager.register(hotkey) {
        log::warn!("can't register ⌘⇧2: {e}");
    }
    let (tx, rx) = flume::unbounded();
    GlobalHotKeyEvent::set_event_handler(Some(move |ev: GlobalHotKeyEvent| {
        if ev.id == hotkey.id() && ev.state == HotKeyState::Pressed {
            let _ = tx.send(());
        }
    }));
    cx.spawn(async move |cx| {
        while rx.recv_async().await.is_ok() {
            cx.update(start);
        }
    })
    .detach();
    cx.default_global::<Capture>()._hotkey = Some(manager);
}

/// Freeze every display and put the capture overlay on it.
pub fn start(cx: &mut App) {
    if cx.default_global::<Capture>().on {
        return;
    }
    if !grab::has_permission() {
        grab::request_permission();
        notify(
            "Tern needs Screen Recording access",
            "Allow Tern in System Settings ▸ Privacy & Security ▸ Screen Recording, then try again.",
            cx,
        );
        grab::open_permission_settings();
        return;
    }
    cx.default_global::<Capture>().on = true;
    tray::hide(cx);
    let displays: Vec<_> = cx.displays().iter().map(|d| (d.id(), d.bounds().size)).collect();
    cx.spawn(async move |cx| {
        // The tray's window needs a moment to leave the screen.
        cx.background_executor().timer(Duration::from_millis(60)).await;
        let grabs = cx
            .background_executor()
            .spawn(async move {
                std::thread::scope(|s| {
                    let jobs: Vec<_> = displays
                        .into_iter()
                        .map(|(id, size)| {
                            s.spawn(move || {
                                let (x, y, w, h) = grab::display_frame(u64::from(id));
                                (id, size, grab::grab(x, y, w, h))
                            })
                        })
                        .collect();
                    jobs.into_iter().filter_map(|j| j.join().ok()).collect::<Vec<_>>()
                })
            })
            .await;
        cx.update(|cx| open_overlays(grabs, cx));
    })
    .detach();
}

fn open_overlays(grabs: Vec<(DisplayId, Size<Pixels>, Result<image::RgbaImage, String>)>, cx: &mut App) {
    let mut overlays = Vec::new();
    for (id, size, grabbed) in grabs {
        let img = match grabbed {
            Ok(img) => img,
            Err(e) => {
                log::error!("screen capture failed: {e}");
                continue;
            },
        };
        let opened = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(point(px(0.), px(0.)), size))),
                titlebar: None,
                focus: true,
                show: true,
                kind: WindowKind::PopUp,
                is_movable: false,
                is_resizable: false,
                is_minimizable: false,
                display_id: Some(id),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| Overlay::new(img, size.width.as_f32(), window, cx)),
        );
        match opened {
            Ok((handle, _)) => overlays.push(handle),
            Err(e) => log::error!("failed to open the capture overlay: {e}"),
        }
    }
    if overlays.is_empty() {
        cx.default_global::<Capture>().on = false;
        notify("Couldn't capture the screen", "screencapture failed; see the log.", cx);
        if tray::has_shots(cx) {
            tray::show(cx);
        }
        return;
    }
    if let Some(first) = overlays.first() {
        let _ = first.update(cx, |_, w, _| w.activate_window());
    }
    cx.default_global::<Capture>().overlays = overlays;
}

/// Close the overlays (after the current update, which may be one of
/// theirs); the tray comes back if it holds captures.
pub fn cancel(cx: &mut App) {
    cx.defer(|cx| {
        let capture = cx.default_global::<Capture>();
        capture.on = false;
        for w in std::mem::take(&mut capture.overlays) {
            let _ = w.update(cx, |_, w, _| w.remove_window());
        }
        if tray::has_shots(cx) {
            tray::show(cx);
        }
    });
}

fn notify(title: &str, body: &str, cx: &mut App) {
    cx.show_system_notification(SystemNotification {
        tag: "tern-shot".into(),
        title: title.to_owned().into(),
        body: body.to_owned().into(),
        actions: vec![],
    });
}
