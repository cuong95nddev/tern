//! macOS notifications from the daemon, while no Tern window is open.
//!
//! The daemon runs the app bundle's executable, so the notification center
//! knows it as Tern. Outside a bundle (`cargo run`) the framework would abort
//! the process, so nothing is posted there.

#[cfg(target_os = "macos")]
pub fn post(title: &str, body: &str) {
    use std::sync::Once;

    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSBundle, NSError, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest, UNNotificationSound, UNUserNotificationCenter,
    };

    if NSBundle::mainBundle().bundleIdentifier().is_none() {
        log::info!("notification not posted (not in an app bundle): {title}: {body}");
        return;
    }
    let center = UNUserNotificationCenter::currentNotificationCenter();
    static AUTH: Once = Once::new();
    AUTH.call_once(|| {
        let done = RcBlock::new(|granted: Bool, _err: *mut NSError| {
            if !granted.as_bool() {
                log::info!("notifications not allowed");
            }
        });
        center.requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound, &done);
    });
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str(body));
    content.setSound(Some(&UNNotificationSound::defaultSound()));
    let id = NSString::from_str(&format!(
        "tern-daemon-{}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()
    ));
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&id, &content, None);
    let done = RcBlock::new(|err: *mut NSError| {
        // SAFETY: when non-null, `err` is a valid NSError for the call.
        if let Some(err) = unsafe { err.as_ref() } {
            log::warn!("notification failed: {}", err.localizedDescription());
        }
    });
    center.addNotificationRequest_withCompletionHandler(&request, Some(&done));
}

#[cfg(not(target_os = "macos"))]
pub fn post(title: &str, body: &str) {
    log::info!("notification: {title}: {body}");
}
