//! The Dock icon's badge: how many agents finished or wait for you while
//! Tern was in the background. Coming back to Tern clears it.

use gpui_kit::*;

use crate::terminal_view::{ActivityStatus, TerminalView};

/// The agents that finished or began waiting while Tern was in the
/// background.
#[derive(Default)]
struct Pending(Vec<WeakEntity<TerminalView>>);

impl Global for Pending {}

/// An agent's status changed: badge it (and bounce the Dock icon once) when
/// it finished or waits while Tern is in the background; else unbadge it.
pub fn status_changed(view: &Entity<TerminalView>, status: ActivityStatus, window: &mut Window, cx: &mut App) {
    let active = app_active(cx);
    let pending = &mut cx.default_global::<Pending>().0;
    pending.retain(|v| v.upgrade().is_some());
    let at = pending.iter().position(|v| v.entity_id() == view.entity_id());
    let wants = matches!(status, ActivityStatus::Blocked | ActivityStatus::Done);
    match at {
        None if wants && !active => {
            pending.push(view.downgrade());
            window.request_attention();
        },
        Some(ix) if !wants => {
            pending.remove(ix);
        },
        _ => return,
    }
    platform::set_badge(pending.len());
}

/// Tern came to the front: what it badged is in front of you now.
pub fn clear(cx: &mut App) {
    let pending = &mut cx.default_global::<Pending>().0;
    if !pending.is_empty() {
        pending.clear();
        platform::set_badge(0);
    }
}

fn app_active(cx: &App) -> bool {
    #[cfg(target_os = "macos")]
    {
        let _ = cx;
        platform::app_active()
    }
    #[cfg(not(target_os = "macos"))]
    cx.active_window().is_some()
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{CStr, CString, c_char, c_void};

    type Id = *mut c_void;
    type Sel = *const c_void;

    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> Sel;
        fn objc_msgSend();
    }

    pub fn app_active() -> bool {
        unsafe {
            // `BOOL` is a `signed char` on x86_64, a `bool` on arm64.
            let send: unsafe extern "C" fn(Id, Sel) -> i8 = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(app(), sel(c"isActive")) != 0
        }
    }

    /// Show `n` on the Dock icon; nothing for 0.
    pub fn set_badge(n: usize) {
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let tile = send(app(), sel(c"dockTile"));
            let label = if n == 0 {
                std::ptr::null_mut()
            } else {
                let s = CString::new(n.to_string()).unwrap_or_default();
                let send: unsafe extern "C" fn(Id, Sel, *const c_char) -> Id = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
                send(objc_getClass(c"NSString".as_ptr()), sel(c"stringWithUTF8String:"), s.as_ptr())
            };
            let send: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(tile, sel(c"setBadgeLabel:"), label);
        }
    }

    unsafe fn app() -> Id {
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(objc_getClass(c"NSApplication".as_ptr()), sel(c"sharedApplication"))
        }
    }

    fn sel(name: &CStr) -> Sel {
        unsafe { sel_registerName(name.as_ptr()) }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    pub fn set_badge(_: usize) {}
}
