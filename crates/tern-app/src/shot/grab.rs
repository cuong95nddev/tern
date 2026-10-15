//! Grabbing the screen (with `screencapture`, which every macOS has) and
//! where captures are kept.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use image::RgbaImage;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
    fn CGDisplayBounds(display: u32) -> CGRect;
}

/// Display `id`'s frame in global points (top left origin): x, y, w, h.
pub fn display_frame(id: u64) -> (f32, f32, f32, f32) {
    let r = unsafe { CGDisplayBounds(id as u32) };
    (r.x as f32, r.y as f32, r.w as f32, r.h as f32)
}

/// Whether Tern may record the screen (other apps' windows show up).
pub fn has_permission() -> bool {
    unsafe { CGPreflightScreenCaptureAccess() }
}

/// Ask for screen recording access; macOS shows its prompt once.
pub fn request_permission() {
    unsafe { CGRequestScreenCaptureAccess() };
}

/// System Settings at Privacy & Security ▸ Screen Recording.
pub fn open_permission_settings() {
    let _ = Command::new("/usr/bin/open").arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture").spawn();
}

/// Where captures are written.
pub fn dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/tmp".into());
    home.join("Library/Caches/dev.tern/shots")
}

fn unique_name(prefix: &str) -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
    format!("{prefix}-{ms}-{}.png", N.fetch_add(1, Ordering::Relaxed))
}

/// The screen inside `(x, y, w, h)` (global points, top left origin), at
/// the display's pixel resolution. Blocks for a few hundred ms.
pub fn grab(x: f32, y: f32, w: f32, h: f32) -> Result<RgbaImage, String> {
    let dir = dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join(unique_name(".grab"));
    let rect = format!("{},{},{},{}", x.round() as i64, y.round() as i64, w.round() as i64, h.round() as i64);
    let status = Command::new("/usr/sbin/screencapture").args(["-x", "-R", &rect]).arg(&file).status().map_err(|e| e.to_string())?;
    let img = if status.success() {
        image::open(&file).map(|i| i.to_rgba8()).map_err(|e| format!("can't read the capture: {e}"))
    } else {
        Err(format!("screencapture failed ({status})"))
    };
    let _ = std::fs::remove_file(&file);
    img
}

/// Write `img` as a new PNG in `dir`.
pub fn save(img: &RgbaImage, dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = dir.join(unique_name("shot"));
    img.save_with_format(&path, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Delete captures in `dir` older than `max_age`.
pub fn cleanup(dir: &Path, max_age: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let now = SystemTime::now();
    for e in entries.flatten() {
        let old = e.metadata().and_then(|m| m.modified()).is_ok_and(|t| now.duration_since(t).is_ok_and(|age| age > max_age));
        if old && e.path().extension().is_some_and(|x| x == "png") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_cleans_up() {
        let dir = std::env::temp_dir().join(format!("tern-shots-test-{}", std::process::id()));
        let img = RgbaImage::from_pixel(3, 2, image::Rgba([1, 2, 3, 255]));
        let a = save(&img, &dir).unwrap();
        let b = save(&img, &dir).unwrap();
        assert_ne!(a, b);
        assert_eq!(image::open(&a).unwrap().to_rgba8(), img);
        cleanup(&dir, Duration::from_secs(3600));
        assert!(a.exists(), "new files stay");
        std::thread::sleep(Duration::from_millis(20));
        cleanup(&dir, Duration::from_millis(1));
        assert!(!a.exists() && !b.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
