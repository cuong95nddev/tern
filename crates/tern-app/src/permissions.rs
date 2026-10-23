//! What macOS lets Tern do: post notifications (an agent finished or needs
//! input) and record the screen (the screenshot tool). Settings ▸ Permissions
//! shows each, asks for it, or opens System Settings at it.
//!
//! macOS keys notification permission to the app bundle, so outside one
//! (`cargo run`) there is nothing to ask: the framework would abort.

use std::sync::mpsc;
use std::time::Duration;

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_foundation::{NSBundle, NSError};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNNotificationSetting, UNNotificationSettings, UNUserNotificationCenter,
};

/// The screenshot tool's Screen Recording access.
pub use crate::shot::grab::{
    has_permission as screen_recording, open_permission_settings as open_screen_recording_settings,
    request_permission as ask_screen_recording,
};

/// How long to wait for the notification center's answer.
const WAIT: Duration = Duration::from_secs(3);

/// Whether macOS shows Tern's notifications.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Notifications {
    /// Not asked yet: asking shows macOS's prompt.
    NotAsked,
    Allowed,
    /// Allowed, but set to show no banners or alerts.
    Hidden,
    Denied,
    /// Not running from the app bundle.
    Unavailable,
}

fn in_bundle() -> bool {
    NSBundle::mainBundle().bundleIdentifier().is_some()
}

/// The notification permission as macOS has it. Blocks until it answers.
pub fn notifications() -> Notifications {
    if !in_bundle() {
        return Notifications::Unavailable;
    }
    let (tx, rx) = mpsc::channel();
    let done = RcBlock::new(move |settings: std::ptr::NonNull<UNNotificationSettings>| {
        // SAFETY: the center hands a valid settings object for the call.
        let settings = unsafe { settings.as_ref() };
        let _ = tx.send(match settings.authorizationStatus() {
            UNAuthorizationStatus::NotDetermined => Notifications::NotAsked,
            UNAuthorizationStatus::Denied => Notifications::Denied,
            _ if settings.alertSetting() == UNNotificationSetting::Disabled => Notifications::Hidden,
            _ => Notifications::Allowed,
        });
    });
    UNUserNotificationCenter::currentNotificationCenter().getNotificationSettingsWithCompletionHandler(&done);
    rx.recv_timeout(WAIT).unwrap_or(Notifications::Unavailable)
}

/// Ask for notifications: macOS prompts while not asked yet, else does
/// nothing. The answer shows in [`notifications`] once given.
pub fn ask_notifications() {
    if !in_bundle() {
        return;
    }
    let done = RcBlock::new(|_granted: Bool, err: *mut NSError| {
        // SAFETY: when non-null, `err` is a valid NSError for the call.
        if let Some(err) = unsafe { err.as_ref() } {
            log::warn!("notification permission: {}", err.localizedDescription());
        }
    });
    UNUserNotificationCenter::currentNotificationCenter()
        .requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound, &done);
}

/// System Settings at Notifications ▸ Tern.
pub fn open_notification_settings() {
    let mut url = "x-apple.systempreferences:com.apple.Notifications-Settings.extension".to_owned();
    if let Some(id) = NSBundle::mainBundle().bundleIdentifier() {
        url.push_str(&format!("?id={id}"));
    }
    let _ = std::process::Command::new("/usr/bin/open").arg(url).spawn();
}
