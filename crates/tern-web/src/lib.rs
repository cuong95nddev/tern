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
/// Connections still sending their request, at most: the rest wait to be
/// accepted. Open pages don't count.
const MAX_PENDING: usize = 64;
/// How long a client has to send its whole request.
const REQUEST_TIME: Duration = Duration::from_secs(10);

const USAGE: &str = "\
Usage: tern --web [--port <port>] [--no-pair]

Serves Tern's terminals to a browser on another device, over Tailscale: on
this machine's Tailscale address, else only to this machine (127.0.0.1).
Never on other networks: it serves plain HTTP.

  --port <port>     Port to listen on (default 7317).
  --no-pair         Don't print a pairing link.

A device pairs once, with a link that works once, for 5 minutes, and then
only watches. Tern's Settings > Connections lets it type, makes more links,
lists the paired devices and revokes them.";

struct Opts {
    port: u16,
    no_pair: bool,
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut opts = Opts { port: DEFAULT_PORT, no_pair: false };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
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
    let ip = tailnet.as_ref().map_or(IpAddr::V4(Ipv4Addr::LOCALHOST), |t| t.ip);
    let listener = TcpListener::bind(SocketAddr::new(ip, opts.port))?;
    let port = listener.local_addr()?.port();

    let base = |host: &str| format!("http://{host}:{port}/");
    let on_tailnet = tailnet.as_ref();
    let addr = match ip {
        IpAddr::V6(v6) => format!("[{v6}]"),
        ip => ip.to_string(),
    };
    println!("Tern is serving its terminals on http://{}", SocketAddr::new(ip, port));
    if ip.is_loopback() {
        println!("Only this machine can reach it: no Tailscale address was found (start Tailscale, then this again).");
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
        pending: http::Slots::new(MAX_PENDING),
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
        // A flood of connections that never finish a request can't take a
        // thread each.
        let Some(slot) = state.pending.take() else { continue };
        let state = state.clone();
        let _ = std::thread::Builder::new().name("web client".into()).spawn(move || {
            if let Err(e) = handle(stream, slot, &state) {
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
    /// The open pages, by device, with the role each was opened with.
    pages: Mutex<Vec<(String, access::Role, Arc<ws::Sender>)>>,
    tries: auth::Tries,
    pending: http::Slots,
}

impl Shared {
    /// The paired device a request comes from: its id and role.
    fn device(&self, req: &http::Request) -> Option<(String, access::Role)> {
        let cookie = req.cookie(&self.cookie)?;
        access::check(&self.access.lock().unwrap().0, cookie).map(|d| (d.id.clone(), d.role))
    }

    /// Read who has access again (when its file changed, or `now`), and end
    /// the pages of devices that lost it, or whose role changed (they come
    /// back with the new one).
    fn reload(&self, now: bool) {
        let stamp = modified(&self.mux_dir);
        // Pages to end, and whether their device was unpaired.
        let ended: Vec<(bool, Arc<ws::Sender>)> = {
            let mut a = self.access.lock().unwrap();
            let t = access::now();
            // Sessions also run out.
            if !now && stamp == a.1 && a.0.devices.iter().all(|d| d.expires() > t) {
                return;
            }
            *a = (access::read(&self.mux_dir), stamp);
            a.0.devices.retain(|d| d.expires() > t);
            let role = |id: &str| a.0.devices.iter().find(|d| d.id == id).map(|d| d.role);
            self.pages
                .lock()
                .unwrap()
                .iter()
                .filter_map(|p| match role(&p.0) {
                    None => Some((true, p.2.clone())),
                    Some(r) if r != p.1 => Some((false, p.2.clone())),
                    Some(_) => None,
                })
                .collect()
        };
        for (revoked, s) in ended {
            if revoked {
                let _ = s.text(r#"{"t":"revoked"}"#);
            }
            s.close();
        }
    }

    fn opened(&self, device: &str, role: access::Role, sender: &Arc<ws::Sender>) {
        self.pages.lock().unwrap().push((device.to_owned(), role, sender.clone()));
        self.write_connected();
    }

    fn closed(&self, sender: &Arc<ws::Sender>) {
        self.pages.lock().unwrap().retain(|(_, _, s)| !Arc::ptr_eq(s, sender));
        self.write_connected();
    }

    /// Say in the status which devices have a page open, for Settings.
    fn write_connected(&self) {
        let mut ids: Vec<String> = self.pages.lock().unwrap().iter().map(|(id, _, _)| id.clone()).collect();
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
        _ => return agent_logo(path),
    })
}

/// The agents' logos, as the app's agents panel draws them: by asset stem
/// (`/agents/<stem>.svg`), or by agent id (`/agent/<id>.svg`, its logo for
/// a dark page).
fn agent_logo(path: &str) -> Option<(&'static [u8], &'static str)> {
    if let Some(id) = path.strip_prefix("/agent/").and_then(|p| p.strip_suffix(".svg")) {
        let registry = tern_agent::registry();
        let stem = &registry.get(id)?.logo.as_ref()?.dark;
        return agent_logo(&format!("/agents/{stem}.svg"));
    }
    macro_rules! logos {
        ($($name:literal),* $(,)?) => {
            match path.strip_prefix("/agents/")?.strip_suffix(".svg")? {
                $($name => include_bytes!(concat!("../../tern-app/assets/agents/", $name, ".svg")).as_slice(),)*
                _ => return None,
            }
        };
    }
    let svg = logos![
        "claude",
        "gemini",
        "openai-light",
        "openai-dark",
        "opencode-light",
        "opencode-dark",
        "cursor-light",
        "cursor-dark",
        "grok-light",
        "grok-dark",
        "pi-light",
        "pi-dark",
    ];
    Some((svg, "image/svg+xml"))
}

const CSP: &str = "default-src 'self'; connect-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'";

fn handle(stream: TcpStream, slot: http::Slot, shared: &Shared) -> io::Result<()> {
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let _ = stream.set_nodelay(true);
    let peer = stream.peer_addr()?.ip();
    let mut reader = BufReader::new(http::Deadline::new(stream.try_clone()?, REQUEST_TIME));
    let mut w = stream;
    let Some(req) = http::read_request(&mut reader)? else { return Ok(()) };
    drop(slot);
    let device = shared.device(&req);
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/ws") => {
            let key = req.header("sec-websocket-key");
            let upgrade = req.header("upgrade").is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
            let Some((device, role)) = device.filter(|_| auth::same_origin(req.header("origin"), req.header("host"))) else {
                return http::respond(&mut w, "403 Forbidden", &[], b"");
            };
            let (true, Some(key)) = (upgrade, key) else { return http::respond(&mut w, "400 Bad Request", &[], b"") };
            let head = format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
                ws::accept_key(key)
            );
            w.write_all(head.as_bytes())?;
            // A page may sit idle for hours; dead peers show up as write errors.
            reader.get_mut().lift()?;
            let sender = Arc::new(ws::Sender::new(w));
            // Listed before the access is read again: a role changed or a
            // device revoked since its cookie was checked ends the page now.
            shared.opened(&device, role, &sender);
            shared.touch(&device);
            bridge::serve(reader, sender.clone(), &shared.mux_dir, role);
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
            (Some((device, _)), Some(value)) => {
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
    // Stopped, logged out or starting: the addresses are listed but not on any
    // interface yet, so they can't be served on.
    if v.get("BackendState")?.as_str()? != "Running" {
        return None;
    }
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
        let o = parse_args(&args(&["--port", "9000", "--no-pair"])).unwrap();
        assert_eq!((o.port, o.no_pair), (9000, true));
        assert_eq!(parse_args(&[]).unwrap().port, DEFAULT_PORT);
        // Only Tailscale's network (or this machine): no other address.
        assert!(parse_args(&args(&["--bind", "0.0.0.0"])).is_err());
        assert!(parse_args(&args(&["--what"])).is_err());
    }

    #[test]
    fn reads_tailscale_status() {
        let json =
            br#"{"BackendState":"Running","Self":{"DNSName":"box.tail1234.ts.net.","TailscaleIPs":["fd7a:115c::1","100.101.102.103"]}}"#;
        let t = parse_tailscale_status(json).unwrap();
        assert_eq!(t.ip, "100.101.102.103".parse::<IpAddr>().unwrap());
        assert_eq!(t.name.as_deref(), Some("box.tail1234.ts.net"));
        assert!(parse_tailscale_status(b"{}").is_none());
        // Tailscale turned off still names its addresses, but they are not on
        // any interface: binding one fails with "Can't assign requested address".
        let stopped = br#"{"BackendState":"Stopped","Self":{"TailscaleIPs":["100.101.102.103"]}}"#;
        assert!(parse_tailscale_status(stopped).is_none());
    }

    #[test]
    fn every_asset_is_served() {
        for p in ["/", "/app.js", "/style.css", "/vendor/xterm.js", "/vendor/xterm.css", "/manifest.json", "/agents/claude.svg"] {
            assert!(asset(p).is_some_and(|(b, _)| !b.is_empty()), "{p}");
        }
        assert!(asset("/../Cargo.toml").is_none());
    }

    #[test]
    fn an_agents_logo_is_served_by_its_id() {
        assert!(asset("/agent/codex.svg").is_some());
        assert!(asset("/agent/claude.svg").is_some());
        assert!(asset("/agent/nobody.svg").is_none());
    }

    #[test]
    fn every_builtin_agent_logo_is_served() {
        let registry = tern_agent::Registry::builtin();
        for logo in registry.all().filter_map(|d| d.logo.as_ref()) {
            for stem in [&logo.light, &logo.dark] {
                let path = format!("/agents/{stem}.svg");
                assert!(asset(&path).is_some(), "{path}");
            }
        }
    }

    #[test]
    fn draws_a_qr_code() {
        let qr = qr_text("http://box:7317/#p=ABCDEFGHJKMN").unwrap();
        assert!(qr.lines().count() > 10);
    }
}
