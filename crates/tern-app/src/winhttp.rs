//! HTTPS through WinHTTP, Windows' own client. An unsigned GUI app that
//! starts hidden `curl.exe` children (one with a sign-in on its stdin) is
//! what antivirus software takes for malware, so on Windows Tern asks in
//! process instead.

use std::ffi::c_void;
use std::io::Write;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use windows_sys::Win32::Networking::WinHttp::*;

/// One HTTPS request.
pub struct Request<'a> {
    pub method: &'a str,
    /// An `https://` URL; any other scheme is refused.
    pub url: &'a str,
    pub agent: &'a str,
    pub headers: &'a [(&'a str, &'a str)],
    /// Follow redirects (never from HTTPS to HTTP), else answer with them.
    pub follow_redirects: bool,
    /// Give up once the whole exchange takes longer.
    pub max_time: Duration,
}

/// The answer's status and `Location` header; its body went to `body`.
pub struct Response {
    pub status: u16,
    pub location: Option<String>,
}

/// Send `req`, writing the answer's body to `body`. A connection that sends
/// nothing for 30 s is given up.
pub fn send(req: &Request, body: &mut dyn Write) -> Result<Response, String> {
    let rest = req.url.strip_prefix("https://").ok_or_else(|| format!("not an https URL: {}", req.url))?;
    let (authority, path) = rest.find('/').map_or((rest, "/"), |i| (&rest[..i], &rest[i..]));
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().map_err(|_| format!("bad port in {}", req.url))?),
        None => (authority, INTERNET_DEFAULT_HTTPS_PORT),
    };
    let deadline = Instant::now() + req.max_time;
    // SAFETY: every pointer passed is to a live, NUL-terminated buffer or a
    // local of the size given; handles are closed once, by `Handle`.
    unsafe {
        let session = Handle::new(
            WinHttpOpen(wide(req.agent).as_ptr(), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, null(), null(), 0),
            "couldn't start WinHTTP",
        )?;
        WinHttpSetTimeouts(session.0, 15_000, 15_000, 30_000, 30_000);
        let connect = Handle::new(WinHttpConnect(session.0, wide(host).as_ptr(), port, 0), "couldn't connect")?;
        let request = Handle::new(
            WinHttpOpenRequest(connect.0, wide(req.method).as_ptr(), wide(path).as_ptr(), null(), null(), null(), WINHTTP_FLAG_SECURE),
            "couldn't open the request",
        )?;
        let policy = match req.follow_redirects {
            true => WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP,
            false => WINHTTP_OPTION_REDIRECT_POLICY_NEVER,
        };
        WinHttpSetOption(request.0, WINHTTP_OPTION_REDIRECT_POLICY, (&raw const policy).cast(), size_of::<u32>() as u32);
        let headers: String = req.headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
        let headers = wide(&headers);
        let (ptr, len) = match headers.len() > 1 {
            true => (headers.as_ptr(), (headers.len() - 1) as u32),
            false => (null(), 0),
        };
        if WinHttpSendRequest(request.0, ptr, len, null(), 0, 0, 0) == 0 || WinHttpReceiveResponse(request.0, null_mut()) == 0 {
            return Err(error(host));
        }
        let mut status = 0u32;
        let mut size = size_of::<u32>() as u32;
        let flags = WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER;
        if WinHttpQueryHeaders(request.0, flags, null(), (&raw mut status).cast(), &mut size, null_mut()) == 0 {
            return Err(error(host));
        }
        let location = header(request.0, WINHTTP_QUERY_LOCATION);
        let mut buf = vec![];
        loop {
            let mut available = 0u32;
            if WinHttpQueryDataAvailable(request.0, &mut available) == 0 {
                return Err(error(host));
            }
            if available == 0 {
                break;
            }
            buf.resize(available as usize, 0);
            let mut read = 0u32;
            if WinHttpReadData(request.0, buf.as_mut_ptr().cast(), available, &mut read) == 0 {
                return Err(error(host));
            }
            body.write_all(&buf[..read as usize]).map_err(|e| e.to_string())?;
            if Instant::now() > deadline {
                return Err(format!("{host}: took too long"));
            }
        }
        Ok(Response { status: status as u16, location })
    }
}

/// Header `query` of `request`'s answer, if it has one.
unsafe fn header(request: *mut c_void, query: u32) -> Option<String> {
    let mut size = 0u32;
    // SAFETY: the first call only reports the size the second one needs.
    unsafe {
        WinHttpQueryHeaders(request, query, null(), null_mut(), &mut size, null_mut());
        if size == 0 {
            return None;
        }
        let mut buf = vec![0u16; size as usize / 2 + 1];
        if WinHttpQueryHeaders(request, query, null(), buf.as_mut_ptr().cast(), &mut size, null_mut()) == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..size as usize / 2]))
    }
}

struct Handle(*mut c_void);

impl Handle {
    fn new(h: *mut c_void, what: &str) -> Result<Self, String> {
        if h.is_null() { Err(error(what)) } else { Ok(Self(h)) }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: a handle WinHTTP gave, closed only here.
        unsafe { WinHttpCloseHandle(self.0) };
    }
}

/// The last WinHTTP error, after `what`.
fn error(what: &str) -> String {
    let why = match std::io::Error::last_os_error().raw_os_error().unwrap_or(0) {
        12002 => "timed out".to_owned(),
        12007 => "the server's name can't be resolved".to_owned(),
        12029 | 12030 => "the connection failed".to_owned(),
        12037 | 12038 | 12044 | 12045 | 12057 | 12175 => "its certificate isn't trusted".to_owned(),
        c => format!("WinHTTP error {c}"),
    };
    format!("{what}: {why}")
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
