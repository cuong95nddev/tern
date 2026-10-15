//! Quit requests that bypass the Quit action (Dock menu, `osascript -e 'quit
//! app "Tern"'`, logout): GPUI's app delegate has no
//! `applicationShouldTerminate:`, so AppKit would terminate at once. We add
//! it, answer "later" and let `quit` ask before running programs end.

use std::ffi::{CStr, c_char, c_void};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

type Id = *mut c_void;
type Sel = *const c_void;

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn class_addMethod(cls: Id, sel: Sel, imp: *const c_void, types: *const c_char) -> bool;
    fn objc_msgSend();
}

const TERMINATE_NOW: usize = 1;
const TERMINATE_LATER: usize = 2;

/// Set once the user (or `quit` with nothing running) agreed to quit.
static APPROVED: AtomicBool = AtomicBool::new(false);
static ASK: OnceLock<flume::Sender<()>> = OnceLock::new();

/// Route AppKit's terminate requests to `ask`, which must end with `reply`.
pub fn install(ask: flume::Sender<()>) {
    let _ = ASK.set(ask);
    unsafe {
        let cls = objc_getClass(c"GPUIApplicationDelegate".as_ptr());
        if cls.is_null() {
            log::warn!("no GPUIApplicationDelegate; quitting from the Dock won't ask first");
            return;
        }
        let imp = should_terminate as extern "C" fn(Id, Sel, Id) -> usize;
        class_addMethod(cls, sel(c"applicationShouldTerminate:"), imp as *const c_void, c"Q@:@".as_ptr());
    }
}

/// Let the next terminate through (`cx.quit()` goes through AppKit too).
pub fn approve() {
    APPROVED.store(true, Ordering::SeqCst);
}

/// Answer a pending terminate request.
pub fn reply(quit: bool) {
    if quit {
        approve();
    }
    unsafe {
        let send: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let app = send(objc_getClass(c"NSApplication".as_ptr()), sel(c"sharedApplication"));
        let send: unsafe extern "C" fn(Id, Sel, bool) = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        send(app, sel(c"replyToApplicationShouldTerminate:"), quit);
    }
}

extern "C" fn should_terminate(_: Id, _: Sel, _: Id) -> usize {
    if APPROVED.load(Ordering::SeqCst) {
        return TERMINATE_NOW;
    }
    match ASK.get().map(|tx| tx.send(())) {
        Some(Ok(())) => TERMINATE_LATER,
        _ => TERMINATE_NOW,
    }
}

fn sel(name: &CStr) -> Sel {
    unsafe { sel_registerName(name.as_ptr()) }
}
