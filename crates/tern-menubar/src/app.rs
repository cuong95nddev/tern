//! The status item and its menu. Everything here runs on the main thread;
//! the link thread hands events over through the main dispatch queue.

use std::cell::RefCell;
use std::path::PathBuf;

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSApplicationActivationPolicy, NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem,
};
use objc2_foundation::{NSString, ns_string};
use tern_mux::observer::Watcher;
use tern_mux::proto::SessionInfo;

use crate::link::{self, LinkEvent};
use crate::model;

struct Ui {
    item: Retained<NSStatusItem>,
    target: Retained<Target>,
    /// Set while connected.
    watcher: Option<Watcher>,
    list: Vec<SessionInfo>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TernMenuTarget"]
    struct Target;

    impl Target {
        #[unsafe(method(open:))]
        fn open(&self, sender: &NSMenuItem) {
            open_session(sender.tag() as u64);
        }

        #[unsafe(method(end:))]
        fn end(&self, sender: &NSMenuItem) {
            end_session(sender.tag() as u64);
        }

        #[unsafe(method(openTern:))]
        fn open_tern(&self, _sender: &NSMenuItem) {
            open_tern();
        }

        #[unsafe(method(hide:))]
        fn hide(&self, _sender: &NSMenuItem) {
            if let Err(e) = crate::setting::set_enabled(&crate::setting::file(), false) {
                log::warn!("can't save the setting: {e}");
            }
            std::process::exit(0);
        }
    }
);

impl Target {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's init.
        unsafe { msg_send![super(this), init] }
    }
}

/// Run the menu bar item until the process ends.
pub fn run(mux_dir: PathBuf) -> ! {
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    // No Dock icon, no menu bar of its own.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let item = NSStatusBar::systemStatusBar().statusItemWithLength(-1.0); // NSVariableStatusItemLength
    if let Some(button) = item.button(mtm) {
        let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(ns_string!("terminal"), Some(ns_string!("Tern")));
        if let Some(image) = image {
            image.setTemplate(true);
            button.setImage(Some(&image));
        }
    }
    let ui = Ui { item, target: Target::new(mtm), watcher: None, list: vec![] };
    UI.with(|u| *u.borrow_mut() = Some(ui));
    refresh(mtm);
    std::thread::Builder::new()
        .name("link".into())
        .spawn(move || link::run(&mux_dir, |ev| DispatchQueue::main().exec_async(move || apply(ev))))
        .expect("link thread");
    app.run();
    std::process::exit(0)
}

fn apply(ev: LinkEvent) {
    let mtm = MainThreadMarker::new().expect("main queue");
    UI.with(|u| {
        let mut u = u.borrow_mut();
        let Some(ui) = u.as_mut() else { return };
        match ev {
            LinkEvent::Connected(w) => ui.watcher = Some(w),
            LinkEvent::List(list) => ui.list = list,
            LinkEvent::Lost => {
                ui.watcher = None;
                ui.list.clear();
            },
        }
    });
    refresh(mtm);
}

fn menu_item(title: &str, action: Option<Sel>, tag: isize, target: &Target, mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    // SAFETY: the selectors are methods of `Target`.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &NSString::from_str(title), action, ns_string!(""))
    };
    // SAFETY: `target` lives as long as the UI (thread-local, never dropped).
    unsafe { item.setTarget(Some(target as &AnyObject)) };
    item.setTag(tag);
    item
}

/// Redraw the icon and rebuild the menu from the latest list. (Rebuilt on
/// every list, at most 4×/s; an open menu updates in place.)
fn refresh(mtm: MainThreadMarker) {
    UI.with(|u| {
        let u = u.borrow();
        let Some(ui) = u.as_ref() else { return };
        let state = model::icon(&ui.list);
        if let Some(button) = ui.item.button(mtm) {
            button.setTitle(&NSString::from_str(&model::icon_title(state)));
        }
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let rows = model::rows(&ui.list, home.as_deref());
        let menu = NSMenu::new(mtm);
        if rows.is_empty() {
            let empty = menu_item("No background terminals", None, 0, &ui.target, mtm);
            empty.setEnabled(false);
            menu.addItem(&empty);
        }
        for row in &rows {
            let tag = row.id as isize; // ids are < 2^63 (see the daemon's random_id)
            let item = menu_item(&row.menu_title(), None, tag, &ui.target, mtm);
            let sub = NSMenu::new(mtm);
            sub.addItem(&menu_item("Open", Some(sel!(open:)), tag, &ui.target, mtm));
            sub.addItem(&menu_item("End…", Some(sel!(end:)), tag, &ui.target, mtm));
            item.setSubmenu(Some(&sub));
            menu.addItem(&item);
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&menu_item("Open Tern", Some(sel!(openTern:)), 0, &ui.target, mtm));
        menu.addItem(&menu_item("Hide Menu Bar Icon", Some(sel!(hide:)), 0, &ui.target, mtm));
        ui.item.setMenu(Some(&menu));
    });
}

fn watcher() -> Option<Watcher> {
    UI.with(|u| u.borrow().as_ref().and_then(|ui| ui.watcher.clone()))
}

/// Ask the running Tern to show it; else start/activate Tern with it.
fn open_session(id: u64) {
    let w = watcher();
    // `reveal` waits for the daemon's answer: not on the main thread.
    std::thread::spawn(move || {
        if !w.is_some_and(|w| w.reveal(id)) {
            open_with_tern(id);
        }
    });
}

fn open_with_tern(id: u64) {
    let r = if cfg!(debug_assertions) {
        // No bundle to route a URL to.
        tern_mux::client::spawn_detached(&["--open-session", &id.to_string()], None)
    } else {
        std::process::Command::new("/usr/bin/open").arg(format!("tern://session/{id}")).status().map(drop)
    };
    if let Err(e) = r {
        log::warn!("can't open session {id} in Tern: {e}");
    }
}

fn open_tern() {
    let r = if cfg!(debug_assertions) {
        tern_mux::client::spawn_detached(&[], None)
    } else {
        std::process::Command::new("/usr/bin/open").args(["-b", "dev.tern"]).status().map(drop)
    };
    if let Err(e) = r {
        log::warn!("can't open Tern: {e}");
    }
}

fn end_session(id: u64) {
    let mtm = MainThreadMarker::new().expect("main thread");
    let label = UI.with(|u| {
        let u = u.borrow();
        let list = u.as_ref().map(|ui| ui.list.clone()).unwrap_or_default();
        model::rows(&list, None).into_iter().find(|r| r.id == id).map(|r| r.label)
    });
    let Some(label) = label else { return }; // ended meanwhile
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(&format!("End \"{label}\"?")));
    alert.setInformativeText(ns_string!("The program running in this terminal stops."));
    alert.addButtonWithTitle(ns_string!("End"));
    alert.addButtonWithTitle(ns_string!("Cancel"));
    // An accessory app's alert would open behind other apps.
    NSApplication::sharedApplication(mtm).activate();
    if alert.runModal() == NSAlertFirstButtonReturn
        && let Some(w) = watcher()
    {
        w.close(id);
    }
}
