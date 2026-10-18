//! Grabbing the screen (macOS: `screencapture`, which every macOS has;
//! Windows: GDI) and where captures are kept.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use image::RgbaImage;

#[cfg(target_os = "macos")]
pub use macos::*;
#[cfg(windows)]
pub use windows::*;

/// Where captures are written.
pub fn dir() -> PathBuf {
    crate::dirs::cache().join("shots")
}

fn unique_name(prefix: &str) -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
    format!("{prefix}-{ms}-{}.png", N.fetch_add(1, Ordering::Relaxed))
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

#[cfg(target_os = "macos")]
mod macos {
    use std::process::Command;

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

    /// Display `id` (a `CGDirectDisplayID`) at its pixel resolution. Blocks
    /// for a few hundred ms.
    pub fn grab_display(id: u64) -> Result<RgbaImage, String> {
        let r = unsafe { CGDisplayBounds(id as u32) };
        grab(r.x, r.y, r.w, r.h)
    }

    /// The screen inside `(x, y, w, h)` (global points, top left origin).
    fn grab(x: f64, y: f64, w: f64, h: f64) -> Result<RgbaImage, String> {
        let dir = super::dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        // Not a dot file: screencapture won't write those (and still exits 0).
        let file = dir.join(super::unique_name("grab"));
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
}

#[cfg(windows)]
mod windows {
    use image::RgbaImage;
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
        DeleteObject, GetDC, GetDIBits, GetMonitorInfoW, HMONITOR, MONITORINFO, ReleaseDC, SRCCOPY, SelectObject,
    };

    /// Windows needs no permission to read the screen.
    pub fn has_permission() -> bool {
        true
    }

    pub fn request_permission() {}

    pub fn open_permission_settings() {}

    /// Display `id` (GPUI's id is the `HMONITOR`) at its pixel resolution.
    pub fn grab_display(id: u64) -> Result<RgbaImage, String> {
        let empty = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, rcMonitor: empty, rcWork: empty, dwFlags: 0 };
        // SAFETY: `info.cbSize` is set; a stale handle only makes the call fail.
        if unsafe { GetMonitorInfoW(id as HMONITOR, &mut info) } == 0 {
            return Err(format!("no monitor {id:#x}: {}", std::io::Error::last_os_error()));
        }
        let r = info.rcMonitor;
        grab(r.left, r.top, r.right - r.left, r.bottom - r.top)
    }

    /// The screen inside `(x, y, w, h)`, in virtual screen pixels (Tern is
    /// per-monitor DPI aware, so these are the physical ones).
    fn grab(x: i32, y: i32, w: i32, h: i32) -> Result<RgbaImage, String> {
        if w <= 0 || h <= 0 {
            return Err(format!("empty display ({w}x{h})"));
        }
        let err = |what: &str| format!("{what} failed: {}", std::io::Error::last_os_error());
        // SAFETY: plain GDI calls; every object made here is released before
        // returning, and `buf` holds exactly `w * h` 32-bit pixels.
        unsafe {
            let screen = GetDC(std::ptr::null_mut());
            if screen.is_null() {
                return Err(err("GetDC"));
            }
            let mem = CreateCompatibleDC(screen);
            let bitmap = CreateCompatibleBitmap(screen, w, h);
            let result = if mem.is_null() || bitmap.is_null() {
                Err(err("CreateCompatibleBitmap"))
            } else {
                let old = SelectObject(mem, bitmap);
                // CAPTUREBLT: layered (translucent) windows too.
                let copied = BitBlt(mem, 0, 0, w, h, screen, x, y, SRCCOPY | CAPTUREBLT) != 0;
                SelectObject(mem, old);
                if copied {
                    let mut bmi: BITMAPINFO = std::mem::zeroed();
                    bmi.bmiHeader = BITMAPINFOHEADER {
                        biSize: size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: w,
                        // Negative: rows top down.
                        biHeight: -h,
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB,
                        ..std::mem::zeroed()
                    };
                    let mut buf = vec![0u8; w as usize * h as usize * 4];
                    let rows = GetDIBits(mem, bitmap, 0, h as u32, buf.as_mut_ptr().cast(), &mut bmi, DIB_RGB_COLORS);
                    if rows == h { Ok(bgrx_to_rgba(buf, w as u32, h as u32)) } else { Err(err("GetDIBits")) }
                } else {
                    Err(err("BitBlt"))
                }
            };
            if !bitmap.is_null() {
                DeleteObject(bitmap);
            }
            if !mem.is_null() {
                DeleteDC(mem);
            }
            ReleaseDC(std::ptr::null_mut(), screen);
            result
        }
    }

    /// GDI's 32-bit pixels are B, G, R and an unused byte.
    pub(super) fn bgrx_to_rgba(mut buf: Vec<u8>, w: u32, h: u32) -> RgbaImage {
        for px in buf.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
            px[3] = 255;
        }
        RgbaImage::from_raw(w, h, buf).expect("w * h * 4 bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_not_hidden() {
        assert!(!unique_name("grab").starts_with('.'));
        assert!(unique_name("grab").ends_with(".png"));
    }

    #[test]
    #[cfg(windows)]
    fn converts_gdi_pixels() {
        let img = super::windows::bgrx_to_rgba(vec![1, 2, 3, 0, 4, 5, 6, 0], 2, 1);
        assert_eq!(img.as_raw(), &[3, 2, 1, 255, 6, 5, 4, 255]);
    }

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
