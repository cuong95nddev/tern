//! Turns a transmission into RGBA pixels: reads the medium (inline data, a
//! file, a temporary file or a shared-memory object), then decodes the format.

use std::ffi::CStr;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::FromRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

use super::Error;
use super::command::{Command, Format, Medium};

/// Most bytes read from a file or shared-memory object.
const MAX_READ: u64 = 64 * 1024 * 1024;
/// Largest width or height, in pixels.
const MAX_SIDE: u32 = 10_000;
/// Most memory one PNG may take while decoding.
const MAX_DECODED: u64 = 256 * 1024 * 1024;

/// Chunks are split on 4-byte boundaries; the last may or may not be padded.
const BASE64: GeneralPurpose =
    GeneralPurpose::new(&base64::alphabet::STANDARD, GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    /// 4 bytes a pixel, row by row.
    pub rgba: Arc<[u8]>,
}

/// Loads the image a transmission (or query) describes; `payload` is its
/// base64 data, all chunks joined.
pub fn load(cmd: &Command, payload: &[u8]) -> Result<Image, Error> {
    if cmd.compressed {
        return Err(Error::new("ENOTSUPPORTED", "compressed data"));
    }
    let data = BASE64.decode(payload).map_err(|_| Error::new("EINVAL", "bad base64 data"))?;
    let bytes = match cmd.medium {
        Medium::Direct => return decode(cmd, &data),
        Medium::File => read_file(&path_of(&data)?, cmd),
        Medium::TempFile => {
            let path = path_of(&data)?;
            let bytes = read_file(&path, cmd);
            remove_temp_file(&path);
            bytes
        },
        Medium::SharedMemory => read_shm(&data, cmd),
    };
    // The program may be on another machine (ssh): say nothing about whether
    // the file exists, what it is or how big it is.
    let image = bytes.and_then(|b| decode(cmd, &b)).map_err(|_| Error::new("EBADF", "cannot read an image from it"))?;
    // The protocol hands the object over to us, but only one that held the
    // image: anything else may be another program's.
    if cmd.medium == Medium::SharedMemory
        && let Ok(cname) = std::ffi::CString::new(data)
    {
        // SAFETY: `cname` is a valid C string.
        unsafe { libc::shm_unlink(cname.as_ptr()) };
    }
    Ok(image)
}

fn path_of(data: &[u8]) -> Result<PathBuf, Error> {
    let s = std::str::from_utf8(data).map_err(|_| Error::new("EINVAL", "path is not UTF-8"))?;
    let path = PathBuf::from(s);
    if !path.is_absolute() {
        return Err(Error::new("EINVAL", "path is not absolute"));
    }
    Ok(path)
}

/// How many bytes of `len` the command asks for: from `offset`, `size` of
/// them (0 = to the end).
fn window(cmd: &Command, len: u64) -> Result<u64, Error> {
    let avail = len.saturating_sub(cmd.offset);
    let want = if cmd.size == 0 { avail } else { cmd.size.min(avail) };
    if want > MAX_READ {
        return Err(Error::new("EFBIG", "image data too large"));
    }
    Ok(want)
}

fn read_file(path: &Path, cmd: &Command) -> Result<Vec<u8>, Error> {
    let real = path.canonicalize().map_err(io_error)?;
    // Any program's output can name a file, so only temporary files are read.
    if !temp_dirs().iter().any(|d| real.starts_with(d)) {
        return Err(Error::new("EPERM", "not a temporary file"));
    }
    // Look before opening: opening a FIFO would block. O_NONBLOCK covers a
    // file swapped for one in between.
    if !std::fs::metadata(&real).map_err(io_error)?.is_file() {
        return Err(Error::new("EINVAL", "not a regular file"));
    }
    let mut file = OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(&real).map_err(io_error)?;
    let meta = file.metadata().map_err(io_error)?;
    if !meta.is_file() {
        return Err(Error::new("EINVAL", "not a regular file"));
    }
    let want = window(cmd, meta.len())?;
    file.seek(SeekFrom::Start(cmd.offset)).map_err(io_error)?;
    let mut buf = Vec::with_capacity(want as usize);
    file.take(want).read_to_end(&mut buf).map_err(io_error)?;
    Ok(buf)
}

/// The directories image files may be read from, resolved.
fn temp_dirs() -> Vec<PathBuf> {
    [std::env::temp_dir(), PathBuf::from("/tmp"), PathBuf::from("/dev/shm")].iter().filter_map(|d| d.canonicalize().ok()).collect()
}

/// Deletes a `t=t` file, but only one plainly meant for this protocol.
fn remove_temp_file(path: &Path) {
    let named = path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.contains("tty-graphics-protocol"));
    let parent = path.parent().and_then(|p| p.canonicalize().ok());
    let in_tmp = temp_dirs().iter().any(|d| parent.as_ref().is_some_and(|p| p.starts_with(d)));
    if named && in_tmp {
        let _ = std::fs::remove_file(path);
    }
}

fn read_shm(name: &[u8], cmd: &Command) -> Result<Vec<u8>, Error> {
    let cname = std::ffi::CString::new(name).map_err(|_| Error::new("EINVAL", "bad shared memory name"))?;
    // SAFETY: the fd is owned by `file` (closed on drop); the mapping covers
    // `map_len` bytes of an object at least that long and is unmapped below.
    unsafe {
        let fd = shm_open(&cname, libc::O_RDONLY);
        if fd < 0 {
            return Err(io_error(std::io::Error::last_os_error()));
        }
        let file = File::from_raw_fd(fd);
        let want = window(cmd, file.metadata().map_err(io_error)?.len())?;
        if want == 0 {
            return Ok(Vec::new());
        }
        let map_len = (cmd.offset + want) as usize;
        let p = libc::mmap(std::ptr::null_mut(), map_len, libc::PROT_READ, libc::MAP_SHARED, fd, 0);
        if p == libc::MAP_FAILED {
            return Err(io_error(std::io::Error::last_os_error()));
        }
        let bytes = std::slice::from_raw_parts((p as *const u8).add(cmd.offset as usize), want as usize).to_vec();
        libc::munmap(p, map_len);
        Ok(bytes)
    }
}

/// `shm_open` with mode 0600 (variadic on macOS, where the mode is passed as `c_uint`).
unsafe fn shm_open(name: &CStr, flags: libc::c_int) -> libc::c_int {
    #[cfg(target_os = "macos")]
    let mode = 0o600 as libc::c_uint;
    #[cfg(not(target_os = "macos"))]
    let mode = 0o600 as libc::mode_t;
    // SAFETY: `name` is a valid C string; the rest is forwarded from the caller.
    unsafe { libc::shm_open(name.as_ptr(), flags, mode) }
}

fn io_error(e: std::io::Error) -> Error {
    let code = match e.kind() {
        std::io::ErrorKind::NotFound => "ENOENT",
        std::io::ErrorKind::PermissionDenied => "EPERM",
        _ => "EIO",
    };
    Error::new(code, e.to_string())
}

fn decode(cmd: &Command, bytes: &[u8]) -> Result<Image, Error> {
    let (width, height, rgba) = match cmd.format {
        Format::Png => {
            let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Png);
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(MAX_SIDE);
            limits.max_image_height = Some(MAX_SIDE);
            limits.max_alloc = Some(MAX_DECODED);
            reader.limits(limits);
            let img = reader.decode().map_err(|e| match e {
                image::ImageError::Limits(_) => Error::new("EFBIG", "image too large"),
                e => Error::new("EBADPNG", e.to_string()),
            })?;
            // Decoding stayed within the limit; the RGBA copy must too.
            if img.width() as u64 * img.height() as u64 * 4 > MAX_DECODED {
                return Err(Error::new("EFBIG", "image too large"));
            }
            let img = img.into_rgba8();
            (img.width(), img.height(), img.into_raw())
        },
        Format::Rgb | Format::Rgba => {
            let bpp = if cmd.format == Format::Rgb { 3 } else { 4 };
            let (w, h) = (cmd.width, cmd.height);
            if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
                return Err(Error::new("EINVAL", "bad image size"));
            }
            let need = w as usize * h as usize * bpp;
            if bytes.len() < need {
                return Err(Error::new("ENODATA", format!("need {need} bytes, got {}", bytes.len())));
            }
            let px = &bytes[..need];
            let rgba = if bpp == 4 { px.to_vec() } else { px.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect() };
            (w, h, rgba)
        },
    };
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(Error::new("EINVAL", "bad image size"));
    }
    Ok(Image { width, height, rgba: rgba.into() })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;

    use super::*;
    use crate::graphics::command::parse;

    fn cmd(control: &str) -> Command {
        parse(control.as_bytes()).0
    }

    fn b64(bytes: &[u8]) -> Vec<u8> {
        STANDARD.encode(bytes).into_bytes()
    }

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("{name}-{}", std::process::id()))
    }

    #[test]
    fn rgb_direct() {
        let img = load(&cmd("f=24,s=2,v=1"), &b64(&[1, 2, 3, 4, 5, 6])).unwrap();
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(&*img.rgba, &[1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[test]
    fn short_data_and_missing_size_are_errors() {
        assert_eq!(load(&cmd("f=32,s=2,v=2"), &b64(&[0; 8])).unwrap_err().code, "ENODATA");
        assert_eq!(load(&cmd("f=32"), &b64(&[0; 4])).unwrap_err().code, "EINVAL");
        assert_eq!(load(&cmd("f=24,s=1,v=1"), b"!!!!").unwrap_err().code, "EINVAL");
    }

    #[test]
    fn the_capability_probe_loads() {
        load(&cmd("i=31,s=1,v=1,a=q,t=d,f=24"), b"AAAA").unwrap();
    }

    #[test]
    fn png_direct() {
        let mut png = Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(3, 2, image::Rgba([9, 8, 7, 255])).write_to(&mut png, image::ImageFormat::Png).unwrap();
        let img = load(&cmd("f=100"), &b64(png.get_ref())).unwrap();
        assert_eq!((img.width, img.height), (3, 2));
        assert_eq!(&img.rgba[..4], &[9, 8, 7, 255]);
    }

    #[test]
    fn file_with_offset_and_size() {
        let path = temp("tern-gfx-file");
        std::fs::write(&path, [0, 0, 1, 2, 3, 0]).unwrap();
        let img = load(&cmd("t=f,f=24,s=1,v=1,O=2,S=3"), &b64(path.to_str().unwrap().as_bytes())).unwrap();
        assert_eq!(&*img.rgba, &[1, 2, 3, 255]);
        assert!(path.exists(), "t=f leaves the file");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn file_errors_reveal_nothing_about_the_file() {
        let code = |p: &str| load(&cmd("t=f,f=24,s=1,v=1"), &b64(p.as_bytes())).unwrap_err().code;
        assert_eq!(code("relative.png"), "EINVAL");
        for p in ["/dev/zero", std::env::temp_dir().to_str().unwrap(), "/nonexistent/x.png"] {
            assert_eq!(code(p), "EBADF", "{p}");
        }
        let short = temp("tern-gfx-short");
        std::fs::write(&short, [7; 5]).unwrap();
        let e = load(&cmd("t=f,f=24,s=2,v=2"), &b64(short.to_str().unwrap().as_bytes())).unwrap_err();
        std::fs::remove_file(&short).unwrap();
        assert_eq!(e.code, "EBADF");
        assert!(!e.msg.contains('5'), "{e:?}");
    }

    #[test]
    fn files_outside_temp_dirs_are_refused() {
        // The crate directory, not a temp dir.
        let path = std::env::current_dir().unwrap().join(format!("tern-gfx-outside-{}", std::process::id()));
        std::fs::write(&path, [1, 2, 3]).unwrap();
        let res = load(&cmd("t=f,f=24,s=1,v=1"), &b64(path.to_str().unwrap().as_bytes()));
        std::fs::remove_file(&path).unwrap();
        assert_eq!(res.unwrap_err().code, "EBADF");
    }

    #[test]
    fn shared_memory_that_is_not_an_image_is_left_alone() {
        let name = format!("/tern-k{}", std::process::id());
        let cname = std::ffi::CString::new(name.clone()).unwrap();
        // SAFETY: test-only shm object of 3 bytes.
        unsafe {
            let fd = shm_open(&cname, libc::O_CREAT | libc::O_RDWR);
            assert!(fd >= 0);
            assert_eq!(libc::ftruncate(fd, 3), 0);
            libc::close(fd);
        }
        // Too small for a 100x100 image (macOS rounds the object up to a page).
        assert!(load(&cmd("t=s,f=32,s=100,v=100"), &b64(name.as_bytes())).is_err());
        // SAFETY: the object is still there; open and remove it.
        unsafe {
            let fd = shm_open(&cname, libc::O_RDONLY);
            assert!(fd >= 0, "a failed load unlinked someone else's object");
            libc::close(fd);
            libc::shm_unlink(cname.as_ptr());
        }
    }

    #[test]
    fn a_fifo_does_not_block() {
        let path = temp("tern-gfx-fifo");
        let c = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        // SAFETY: creates a test FIFO.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let (tx, rx) = std::sync::mpsc::channel();
        let arg = b64(path.to_str().unwrap().as_bytes());
        std::thread::spawn(move || {
            let _ = tx.send(load(&cmd("t=f,f=24,s=1,v=1"), &arg).map(drop));
        });
        let res = rx.recv_timeout(std::time::Duration::from_secs(2));
        std::fs::remove_file(&path).unwrap();
        assert_eq!(res.expect("load blocked on a FIFO").unwrap_err().code, "EBADF");
    }

    #[test]
    fn a_png_too_big_once_expanded_is_refused() {
        // 72 M grey pixels decode within the limit, but take 288 MiB as RGBA.
        let mut png = Cursor::new(Vec::new());
        image::GrayImage::new(9000, 8000).write_to(&mut png, image::ImageFormat::Png).unwrap();
        assert_eq!(load(&cmd("f=100"), &b64(png.get_ref())).unwrap_err().code, "EFBIG");
    }

    #[test]
    fn oversized_png_is_refused_while_decoding() {
        let mut png = Cursor::new(Vec::new());
        image::GrayImage::new(MAX_SIDE + 1, 1).write_to(&mut png, image::ImageFormat::Png).unwrap();
        assert_eq!(load(&cmd("f=100"), &b64(png.get_ref())).unwrap_err().code, "EFBIG");
    }

    #[test]
    fn temp_file_is_deleted_only_when_named_for_us() {
        let ours = temp("tty-graphics-protocol-tern");
        let other = temp("tern-keep");
        for p in [&ours, &other] {
            std::fs::write(p, [1, 2, 3]).unwrap();
            load(&cmd("t=t,f=24,s=1,v=1"), &b64(p.to_str().unwrap().as_bytes())).unwrap();
        }
        assert!(!ours.exists());
        assert!(other.exists());
        std::fs::remove_file(other).unwrap();
    }

    #[test]
    fn shared_memory_round_trip() {
        let name = format!("/tern-t{}", std::process::id());
        let cname = std::ffi::CString::new(name.clone()).unwrap();
        // SAFETY: test-only shm object of 3 bytes, mapped and unmapped here.
        unsafe {
            let fd = shm_open(&cname, libc::O_CREAT | libc::O_RDWR);
            assert!(fd >= 0);
            assert_eq!(libc::ftruncate(fd, 3), 0);
            let p = libc::mmap(std::ptr::null_mut(), 3, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0);
            assert_ne!(p, libc::MAP_FAILED);
            std::ptr::copy_nonoverlapping([4u8, 5, 6].as_ptr(), p as *mut u8, 3);
            libc::munmap(p, 3);
            libc::close(fd);
        }
        let img = load(&cmd("t=s,f=24,s=1,v=1"), &b64(name.as_bytes())).unwrap();
        assert_eq!(&*img.rgba, &[4, 5, 6, 255]);
        // SAFETY: probing that the object was unlinked.
        assert!(unsafe { shm_open(&cname, libc::O_RDONLY) } < 0);
    }
}
