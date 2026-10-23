//! `tern --web`: Tern's terminals in a browser on another device, to watch
//! and answer coding agents away from the desk. Meant for a private network
//! (Tailscale): it serves plain HTTP, by default on the machine's Tailscale
//! address, and lets in only paired browsers (see [`access`]).
//!
//! A separate process from the daemon: the daemon that owns the programs
//! never listens on the network. Each page gets a viewer connection of its
//! own (see [`tern_mux::viewer`]), so the desktop UI keeps its sessions.

pub mod access;
mod auth;
mod bridge;
mod http;
pub mod live;
mod service;
mod ws;

pub use service::{Serving, last_log_line, log_file, qr, replace_outdated, serving, start, stop, wanted};

use std::io::{self, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

pub const DEFAULT_PORT: u16 = 7317;
/// How often the server looks for devices the app revoked.
const WATCH: Duration = Duration::from_secs(1);

const USAGE: &str = "\
Usage: tern --web [--bind <address>] [--port <port>] [--no-pair]

Serves Tern's terminals to a browser on another device (over Tailscale).

  --bind <address>  Address to listen on. Default: this machine's Tailscale
                    address, else 127.0.0.1. 0.0.0.0 listens everywhere
                    (plain HTTP: only on a network you trust).
  --port <port>     Port to listen on (default 7317).
  --no-pair         Don't print a pairing link.

A device pairs once, with a link that works once, for 5 minutes. Tern's
Settings > Connections makes more links, lists the paired devices and
revokes them.";

struct Opts {
    bind: Option<IpAddr>,
    port: u16,
    no_pair: bool,
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut opts = Opts { bind: None, port: DEFAULT_PORT, no_pair: false };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--bind" => opts.bind = Some(it.next().and_then(|v| v.parse().ok()).ok_or("--bind takes an IP address")?),
            "--port" => opts.port = it.next().and_then(|v| v.parse().ok()).ok_or("--port takes a port number")?,
            "--no-pair" => opts.no_pair = true,
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(opts)
}

/// `tern --web <args>`: serve until killed.
pub fn run(args: &[String]) -> ! {
    #[cfg(windows)]
    attach_console();
    let code = match parse_args(args) {
        Ok(opts) => match serve(opts) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("tern --web: {e}");
                1
            },
        },
        Err(e) => {
            if !e.is_empty() {
                eprintln!("tern --web: {e}\n");
            }
            eprintln!("{USAGE}");
            2
        },
    };
    std::process::exit(code)
}

/// A release build on Windows has no console: print to the one it was
/// started from.
#[cfg(windows)]
fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // SAFETY: plain call; fails harmlessly when there is no parent console
    // or one is attached already.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

fn serve(opts: Opts) -> io::Result<()> {
    // Hash this binary now, before an update can replace it on disk.
    tern_mux::paths::build_id();
    let mux_dir = tern_mux::paths::mux_dir();
    tern_mux::paths::ensure_private_dir(&mux_dir)?;
    // Held while serving. An app looking at it holds it for a moment.
    let mut lock = service::lock(&mux_dir);
    for _ in 0..20 {
        if lock.is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
        lock = service::lock(&mux_dir);
    }
    let _lock = lock?;
    // The one secret link of old: its browsers pair again.
    let _ = std::fs::remove_file(mux_dir.join("web-token"));
    let tailnet = tailscale_self();
    let ip = opts.bind.or(tailnet.as_ref().map(|t| t.ip)).unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    let listener = TcpListener::bind(SocketAddr::new(ip, opts.port))?;
    let port = listener.local_addr()?.port();

    let base = |host: &str| format!("http://{host}:{port}/");
    let on_tailnet = tailnet.as_ref().filter(|t| t.ip == ip || ip.is_unspecified());
    let addr = match ip {
        IpAddr::V6(v6) => format!("[{v6}]"),
        ip if ip.is_unspecified() => on_tailnet.map(|t| t.ip).or_else(lan_ip).map_or_else(|| "127.0.0.1".into(), |ip| ip.to_string()),
        ip => ip.to_string(),
    };
    println!("Tern is serving its terminals on http://{}", SocketAddr::new(ip, port));
    if ip.is_loopback() {
        println!("Only this machine can reach it: no Tailscale address was found (pass --bind to choose one).");
    }
    if !opts.no_pair {
        let pairing = access::pair_in(&mux_dir)?;
        println!("\nTo pair your phone or another computer, open this link on it (it works once, for 5 minutes):\n");
        println!("  {}", pairing.link(&base(&addr)));
        if let Some(name) = on_tailnet.and_then(|t| t.name.as_deref()) {
            println!("  {}  (with MagicDNS)", pairing.link(&base(name)));
        }
        println!();
        if let Some(qr) = qr_text(&pairing.link(&base(&addr))) {
            println!("{qr}");
        }
        println!("Tern's Settings > Connections makes more links and lists the paired devices.");
    }
    println!("Press Ctrl+C to stop.");
    let _ = io::stdout().flush();
    let mut links = vec![base(&addr)];
    links.extend(on_tailnet.and_then(|t| t.name.as_deref()).map(base));
    let status = service::Serving {
        pid: std::process::id(),
        links,
        all_networks: ip.is_unspecified(),
        local_only: ip.is_loopback(),
        connected: Vec::new(),
        build: Some(tern_mux::paths::build_id().to_owned()),
    };
    service::write_status(&mux_dir, &status)?;

    let state = Arc::new(Shared {
        access: Mutex::new((access::read(&mux_dir), modified(&mux_dir))),
        mux_dir,
        cookie: format!("tern_web_{port}"),
        status: Mutex::new(status),
        pages: Mutex::default(),
        tries: auth::Tries::default(),
    });
    {
        let state = state.clone();
        std::thread::Builder::new().name("web access".into()).spawn(move || {
            loop {
                std::thread::sleep(WATCH);
                state.reload(false);
            }
        })?;
    }
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                log::warn!("accept: {e}");
                continue;
            },
        };
        let state = state.clone();
        let _ = std::thread::Builder::new().name("web client".into()).spawn(move || {
            if let Err(e) = handle(stream, &state) {
                log::info!("web client: {e}");
            }
        });
    }
    Ok(())
}

struct Shared {
    mux_dir: PathBuf,
    /// Cookies aren't kept apart by port: the name is.
    cookie: String,
    /// Who has access, and when its file last changed.
    access: Mutex<(access::Access, Option<SystemTime>)>,
    status: Mutex<service::Serving>,
    /// The open pages, by device.
    pages: Mutex<Vec<(String, Arc<ws::Sender>)>>,
    tries: auth::Tries,
}

impl Shared {
    /// The paired device a request comes from: its id.
    fn device(&self, req: &http::Request) -> Option<String> {
        let cookie = req.cookie(&self.cookie)?;
        access::check(&self.access.lock().unwrap().0, cookie).map(|d| d.id.clone())
    }

    /// Read who has access again (when its file changed, or `now`), and end
    /// the pages of devices that lost it.
    fn reload(&self, now: bool) {
        let stamp = modified(&self.mux_dir);
        let ended: Vec<Arc<ws::Sender>> = {
            let mut a = self.access.lock().unwrap();
            let t = access::now();
            // Sessions also run out.
            if !now && stamp == a.1 && a.0.devices.iter().all(|d| d.expires() > t) {
                return;
            }
            *a = (access::read(&self.mux_dir), stamp);
            a.0.devices.retain(|d| d.expires() > t);
            let alive = |id: &str| a.0.devices.iter().any(|d| d.id == id);
            self.pages.lock().unwrap().iter().filter(|(id, _)| !alive(id)).map(|(_, s)| s.clone()).collect()
        };
        for s in ended {
            let _ = s.text(r#"{"t":"revoked"}"#);
            s.close();
        }
    }

    fn opened(&self, device: &str, sender: &Arc<ws::Sender>) {
        self.pages.lock().unwrap().push((device.to_owned(), sender.clone()));
        self.write_connected();
    }

    fn closed(&self, sender: &Arc<ws::Sender>) {
        self.pages.lock().unwrap().retain(|(_, s)| !Arc::ptr_eq(s, sender));
        self.write_connected();
    }

    /// Say in the status which devices have a page open, for Settings.
    fn write_connected(&self) {
        let mut ids: Vec<String> = self.pages.lock().unwrap().iter().map(|(id, _)| id.clone()).collect();
        ids.sort();
        ids.dedup();
        let mut status = self.status.lock().unwrap();
        if status.connected != ids {
            status.connected = ids;
            if let Err(e) = service::write_status(&self.mux_dir, &status) {
                log::warn!("can't write the status: {e}");
            }
        }
    }

    /// Note that a device was just used: it stays paired for another while.
    fn touch(&self, device: &str) {
        if let Err(e) = access::touch(&self.mux_dir, device) {
            log::warn!("can't note a device was used: {e}");
        }
        self.reload(true);
    }

    fn set_cookie(&self, value: &str) -> String {
        format!("{}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}", self.cookie, access::SESSION_TTL)
    }
}

fn modified(dir: &Path) -> Option<SystemTime> {
    std::fs::metadata(access::file(dir)).and_then(|m| m.modified()).ok()
}

/// The page and the files it loads.
fn asset(path: &str) -> Option<(&'static [u8], &'static str)> {
    Some(match path {
        "/" | "/index.html" => (include_bytes!("../assets/index.html"), "text/html; charset=utf-8"),
        "/app.js" => (include_bytes!("../assets/app.js"), "text/javascript; charset=utf-8"),
        "/style.css" => (include_bytes!("../assets/style.css"), "text/css; charset=utf-8"),
        "/vendor/xterm.js" => (include_bytes!("../assets/vendor/xterm.js"), "text/javascript; charset=utf-8"),
        "/vendor/xterm.css" => (include_bytes!("../assets/vendor/xterm.css"), "text/css; charset=utf-8"),
        "/vendor/addon-unicode11.js" => (include_bytes!("../assets/vendor/addon-unicode11.js"), "text/javascript; charset=utf-8"),
        "/vendor/addon-webgl.js" => (include_bytes!("../assets/vendor/addon-webgl.js"), "text/javascript; charset=utf-8"),
        "/manifest.json" => (include_bytes!("../assets/manifest.json"), "application/manifest+json"),
        // The app's own logo, as its agents panel draws it.
        "/agents/claude.svg" => (include_bytes!("../../tern-app/assets/agents/claude.svg"), "image/svg+xml"),
        _ => return None,
    })
}

const CSP: &str = "default-src 'self'; connect-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'";

fn handle(stream: TcpStream, shared: &Shared) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let _ = stream.set_nodelay(true);
    let peer = stream.peer_addr()?.ip();
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut w = stream;
    let Some(req) = http::read_request(&mut reader)? else { return Ok(()) };
    let device = shared.device(&req);
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/ws") => {
            let key = req.header("sec-websocket-key");
            let upgrade = req.header("upgrade").is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
            let Some(device) = device.filter(|_| auth::same_origin(req.header("origin"), req.header("host"))) else {
                return http::respond(&mut w, "403 Forbidden", &[], b"");
            };
            let (true, Some(key)) = (upgrade, key) else { return http::respond(&mut w, "400 Bad Request", &[], b"") };
            let head = format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
                ws::accept_key(key)
            );
            w.write_all(head.as_bytes())?;
            // A page may sit idle for hours; dead peers show up as write errors.
            w.set_read_timeout(None)?;
            let sender = Arc::new(ws::Sender::new(w));
            shared.touch(&device);
            shared.opened(&device, &sender);
            bridge::serve(reader, sender.clone(), &shared.mux_dir);
            shared.closed(&sender);
            Ok(())
        },
        ("POST", "/pair") => {
            if !auth::same_origin(req.header("origin"), req.header("host")) {
                return http::respond(&mut w, "403 Forbidden", &[], b"");
            }
            if !shared.tries.allowed(peer) {
                return http::respond(&mut w, "429 Too Many Requests", &[], b"");
            }
            let ip = match peer {
                IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(peer, IpAddr::V4),
                ip => ip,
            };
            let code = String::from_utf8_lossy(&req.body);
            match access::redeem(&shared.mux_dir, &code, req.header("user-agent").unwrap_or_default(), &ip.to_string())? {
                Some((value, d)) => {
                    log::info!("paired {} ({ip})", d.name);
                    shared.reload(true);
                    let cookie = shared.set_cookie(&value);
                    http::respond(&mut w, "204 No Content", &[("Set-Cookie", &cookie), ("Cache-Control", "no-store")], b"")
                },
                None => {
                    shared.tries.failed(peer);
                    // Codes are long; this only keeps a typo from looking like a hang.
                    std::thread::sleep(Duration::from_millis(500));
                    http::respond(&mut w, "403 Forbidden", &[], b"")
                },
            }
        },
        ("GET", "/session") => match (device, req.cookie(&shared.cookie)) {
            (Some(device), Some(value)) => {
                shared.touch(&device);
                let cookie = shared.set_cookie(value);
                let headers = [("Content-Type", "application/json"), ("Cache-Control", "no-store"), ("Set-Cookie", cookie.as_str())];
                http::respond(&mut w, "200 OK", &headers, b"true")
            },
            _ => http::respond(&mut w, "200 OK", &[("Content-Type", "application/json"), ("Cache-Control", "no-store")], b"false"),
        },
        ("GET", path) => match asset(path) {
            Some((body, kind)) => http::respond(
                &mut w,
                "200 OK",
                &[("Content-Type", kind), ("Cache-Control", "no-cache"), ("Content-Security-Policy", CSP)],
                body,
            ),
            None => http::respond(&mut w, "404 Not Found", &[], b""),
        },
        _ => http::respond(&mut w, "405 Method Not Allowed", &[], b""),
    }
}

/// This machine's address on its local network: the one it would reach
/// the internet from (a UDP socket only picks a route; nothing is sent).
fn lan_ip() -> Option<IpAddr> {
    let s = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    s.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    Some(s.local_addr().ok()?.ip()).filter(|ip| !ip.is_loopback() && !ip.is_unspecified())
}

/// This machine on the tailnet.
struct Tailnet {
    ip: IpAddr,
    /// Its MagicDNS name.
    name: Option<String>,
}

/// Ask the Tailscale CLI for this machine's address; `None` without it.
fn tailscale_self() -> Option<Tailnet> {
    let candidates: &[&str] = if cfg!(windows) {
        &["tailscale", r"C:\Program Files\Tailscale\tailscale.exe"]
    } else if cfg!(target_os = "macos") {
        &["tailscale", "/Applications/Tailscale.app/Contents/MacOS/Tailscale"]
    } else {
        &["tailscale"]
    };
    let out = candidates.iter().find_map(|cmd| {
        let mut c = std::process::Command::new(cmd);
        c.args(["status", "--json"]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            c.creation_flags(CREATE_NO_WINDOW);
        }
        c.output().ok().filter(|o| o.status.success())
    })?;
    parse_tailscale_status(&out.stdout)
}

fn parse_tailscale_status(json: &[u8]) -> Option<Tailnet> {
    let v: serde_json::Value = serde_json::from_slice(json).ok()?;
    let me = v.get("Self")?;
    let ips: Vec<IpAddr> = me.get("TailscaleIPs")?.as_array()?.iter().filter_map(|i| i.as_str()?.parse().ok()).collect();
    let ip = ips.iter().find(|i| i.is_ipv4()).or(ips.first()).copied()?;
    let name = me.get("DNSName").and_then(|n| n.as_str()).map(|n| n.trim_end_matches('.').to_owned()).filter(|n| !n.is_empty());
    Some(Tailnet { ip, name })
}

/// `url` as a QR code drawn with half blocks, for a phone's camera.
fn qr_text(url: &str) -> Option<String> {
    let code = qrcode::QrCode::new(url.as_bytes()).ok()?;
    let width = code.width();
    let dark = |x: i64, y: i64| {
        x >= 0 && y >= 0 && (x as usize) < width && (y as usize) < width && code[(x as usize, y as usize)] == qrcode::Color::Dark
    };
    // A quiet zone of two modules; light on dark terminals reads either way
    // as long as the colors are inverted together.
    let mut out = String::new();
    for y in (-2..width as i64 + 2).step_by(2) {
        out.push_str("  ");
        for x in -2..width as i64 + 2 {
            out.push(match (dark(x, y), dark(x, y + 1)) {
                (false, false) => '█',
                (false, true) => '▀',
                (true, false) => '▄',
                (true, true) => ' ',
            });
        }
        out.push('\n');
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_options() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let o = parse_args(&args(&["--bind", "0.0.0.0", "--port", "9000", "--no-pair"])).unwrap();
        assert_eq!((o.bind, o.port, o.no_pair), (Some("0.0.0.0".parse().unwrap()), 9000, true));
        let o = parse_args(&[]).unwrap();
        assert_eq!((o.bind, o.port), (None, DEFAULT_PORT));
        assert!(parse_args(&args(&["--bind", "nope"])).is_err());
        assert!(parse_args(&args(&["--what"])).is_err());
    }

    #[test]
    fn reads_tailscale_status() {
        let json = br#"{"Self":{"DNSName":"box.tail1234.ts.net.","TailscaleIPs":["fd7a:115c::1","100.101.102.103"]}}"#;
        let t = parse_tailscale_status(json).unwrap();
        assert_eq!(t.ip, "100.101.102.103".parse::<IpAddr>().unwrap());
        assert_eq!(t.name.as_deref(), Some("box.tail1234.ts.net"));
        assert!(parse_tailscale_status(b"{}").is_none());
    }

    #[test]
    fn every_asset_is_served() {
        for p in ["/", "/app.js", "/style.css", "/vendor/xterm.js", "/vendor/xterm.css", "/manifest.json", "/agents/claude.svg"] {
            assert!(asset(p).is_some_and(|(b, _)| !b.is_empty()), "{p}");
        }
        assert!(asset("/../Cargo.toml").is_none());
    }

    #[test]
    fn draws_a_qr_code() {
        let qr = qr_text("http://box:7317/#p=ABCDEFGHJKMN").unwrap();
        assert!(qr.lines().count() > 10);
    }
}
