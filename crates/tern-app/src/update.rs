//! Updates from GitHub releases: Tern checks the latest release at launch and
//! every few hours, and the workspace bar then shows an update button (also
//! Tern ▸ Check for Updates…).
//!
//! On macOS installing downloads the release's DMG, copies its Tern.app next
//! to the running one, and after Tern quits a small shell script swaps the
//! bundles and reopens it, which restores the session. Uses the system's
//! `curl`, `hdiutil`, `ditto` and `codesign`.
//!
//! On Windows it downloads the release's installer, checks its Authenticode
//! certificate against the thumbprints in `TERN_WINDOWS_SIGNER` (set at build
//! time; without them Tern
//! only points to the release page) and starts it: once Tern quits it
//! installs silently over the installed copy and starts Tern again (the
//! system's `curl.exe`, and PowerShell for the signature). On Linux Tern
//! only points to the release page.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use futures::channel::oneshot;
use gpui_kit::*;

/// The GitHub repo whose releases ship Tern; the release workflow sets
/// `TERN_RELEASE_REPO` when they go to a separate public repo.
const REPO: &str = match option_env!("TERN_RELEASE_REPO") {
    Some(r) if !r.is_empty() => r,
    _ => "cuong95nddev/tern",
};
const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);

actions!(tern, [CheckForUpdates]);

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: semver::Version,
    /// The DMG (macOS), installer (Windows) or archive (Linux).
    pub asset_url: String,
    pub page_url: String,
}

#[derive(Clone, Default)]
pub enum State {
    #[default]
    Idle,
    Checking,
    Available(Release),
    Installing(Release),
}

/// Where the update check is; windows observe it for the bar's button.
#[derive(Default)]
pub struct Updater {
    pub state: State,
}

impl Global for Updater {}

pub fn current_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version is semver")
}

/// The release update the bar offers, if any (and whether it is installing).
pub fn offered(cx: &App) -> Option<(Release, bool)> {
    match &cx.try_global::<Updater>()?.state {
        State::Available(r) => Some((r.clone(), false)),
        State::Installing(r) => Some((r.clone(), true)),
        _ => None,
    }
}

pub fn init(cx: &mut App) {
    cx.set_global(Updater::default());
    cx.on_action(|_: &CheckForUpdates, cx| check(true, cx));
    #[cfg(windows)]
    sweep_staged();
    // Only an installed app checks by itself, not `cargo run`.
    if installation().is_some() && std::env::var_os("TERN_NO_UPDATE_CHECK").is_none() {
        cx.spawn(async move |cx| {
            cx.background_executor().timer(Duration::from_secs(5)).await;
            loop {
                cx.update(|cx| check(false, cx));
                cx.background_executor().timer(CHECK_EVERY).await;
            }
        })
        .detach();
    }
}

/// Look for a newer release. `manual` (the menu item) reports the outcome
/// in a dialog; the background check only updates the bar.
pub fn check(manual: bool, cx: &mut App) {
    let state = cx.global::<Updater>().state.clone();
    match state {
        State::Checking | State::Installing(_) => return,
        State::Available(r) if manual => return offer(r, cx),
        _ => {},
    }
    set_state(State::Checking, cx);
    cx.spawn(async move |cx| {
        let latest = cx.background_spawn(async { fetch_latest() }).await;
        cx.update(|cx| {
            let newer = latest.as_ref().ok().and_then(|r| r.clone()).filter(|r| r.version > current_version());
            set_state(newer.clone().map_or(State::Idle, State::Available), cx);
            if !manual {
                if let Err(e) = &latest {
                    log::warn!("update check failed: {e}");
                }
                return;
            }
            match (latest, newer) {
                (Err(e), _) => alert(PromptLevel::Warning, "Couldn't check for updates", &e, cx),
                (Ok(_), Some(r)) => offer(r, cx),
                (Ok(_), None) => {
                    let msg = format!("Tern {} is the newest version.", current_version());
                    alert(PromptLevel::Info, "You're up to date", &msg, cx);
                },
            }
        });
    })
    .detach();
}

/// Ask whether to install release `r`.
pub fn offer(r: Release, cx: &mut App) {
    let msg = format!("Tern {} is available", r.version);
    let installed = installation().filter(|_| CAN_INSTALL);
    let blocked = installed.as_deref().and_then(install_blocker);
    let detail = match installed {
        Some(_) if blocked.is_some() => format!("You have {}. {}", current_version(), blocked.as_deref().unwrap_or_default()),
        Some(_) => {
            let what = match crate::restart::running(cx) {
                0 => String::new(),
                1 => " 1 terminal runs now: the program in it ends.".to_owned(),
                n => format!(" {n} terminals run now, in all windows: every program in them ends."),
            };
            format!(
                "You have {}. Tern will restart to install it, and so will its background terminals, so they run the \
                 new version too.{what} Your windows and tabs come back, and Claude Code conversations resume.",
                current_version()
            )
        },
        None if !CAN_INSTALL => format!("You have {}. Download the update from the release page.", current_version()),
        None => format!("You have {}. This copy isn't an installed Tern, so download the update from the release page.", current_version()),
    };
    let installable = installed.is_some() && blocked.is_none();
    let answers: &[&str] = if installable { &["Update and Restart", "Release Notes", "Later"] } else { &["Release Notes", "Later"] };
    let Some(answer) = prompt(PromptLevel::Info, &msg, &detail, answers, cx) else { return };
    cx.spawn(async move |cx| {
        let Ok(ix) = answer.await else { return };
        let choice = if installable { ix } else { ix + 1 };
        cx.update(|cx| match choice {
            0 => install(r, cx),
            1 => cx.open_url(&r.page_url),
            _ => {},
        });
    })
    .detach();
}

fn install(r: Release, cx: &mut App) {
    let Some(bundle) = installation() else { return };
    set_state(State::Installing(r.clone()), cx);
    cx.spawn(async move |cx| {
        let url = r.asset_url.clone();
        let target = bundle.clone();
        let staged = cx.background_spawn(async move { stage(&url, &target) }).await;
        cx.update(|cx| match staged.and_then(|staged| relaunch_after_exit(&staged, &bundle)) {
            Ok(()) => stop_terminals_and_quit(cx),
            Err(e) => {
                set_state(State::Available(r), cx);
                alert(PromptLevel::Critical, "Couldn't install the update", &e, cx);
            },
        });
    })
    .detach();
}

/// End the background daemon, which would otherwise keep running the old
/// version (and every terminal with it), then quit. The session is saved as
/// it is first: its tabs start again at the next launch.
fn stop_terminals_and_quit(cx: &mut App) {
    crate::main_window::save_session(cx);
    cx.default_global::<crate::main_window::Session>().quitting = true;
    let mux = cx.global::<crate::AppServices>().0.mux.clone();
    cx.spawn(async move |cx| {
        // Waits up to a couple of seconds for the daemon to go.
        let stopped = cx.background_spawn(async move { crate::restart::stop_all(&mux) }).await;
        if let Err(e) = stopped {
            log::warn!("can't end the background server: {e}");
        }
        cx.update(crate::quit);
    })
    .detach();
}

fn set_state(state: State, cx: &mut App) {
    cx.update_global::<Updater, _>(|u, _| u.state = state);
    cx.refresh_windows();
}

fn alert(level: PromptLevel, msg: &str, detail: &str, cx: &mut App) {
    if let Some(rx) = prompt(level, msg, detail, &["OK"], cx) {
        cx.spawn(async move |_| {
            let _ = rx.await;
        })
        .detach();
    }
}

/// A dialog on the active window (else any).
fn prompt(level: PromptLevel, msg: &str, detail: &str, answers: &[&str], cx: &mut App) -> Option<oneshot::Receiver<usize>> {
    let window = cx.active_window().or_else(|| cx.windows().first().copied())?;
    window.update(cx, |_, window, cx| window.prompt(level, msg, Some(detail), answers, cx)).ok()
}

/// SHA-1 thumbprints (comma separated, as `Get-AuthenticodeSignature` shows
/// them) of the certificates the Windows installer may be signed with, set
/// at build time. Without them Tern can't verify a downloaded installer, so
/// it doesn't run one.
const WINDOWS_SIGNER: &str = match option_env!("TERN_WINDOWS_SIGNER") {
    Some(s) => s,
    None => "",
};

/// Whether Tern can install an update itself here: on macOS (the app's
/// signature is checked), on Windows only when the installer's signer is known.
const CAN_INSTALL: bool = cfg!(target_os = "macos") || (cfg!(windows) && !WINDOWS_SIGNER.is_empty());

/// The installed Tern this process runs from, `None` for a build in
/// `target/`: the Tern.app bundle (macOS), the folder the installer put it in
/// (Windows: it holds the uninstaller) or the executable (Linux).
fn installation() -> Option<PathBuf> {
    let exe = dunce::canonicalize(std::env::current_exe().ok()?).ok()?;
    if cfg!(target_os = "macos") {
        bundle_of(&exe)
    } else if cfg!(windows) {
        exe.parent().filter(|d| d.join("unins000.exe").is_file()).map(Path::to_owned)
    } else {
        (!exe.components().any(|c| c.as_os_str() == "target")).then_some(exe)
    }
}

/// `program` without a console window of its own (on Windows a GUI app's
/// console children would each flash one).
fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

/// The system's curl: macOS and Windows 10+ ship one.
fn curl_path() -> PathBuf {
    if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        PathBuf::from(root).join(r"System32\curl.exe")
    } else if cfg!(target_os = "macos") {
        "/usr/bin/curl".into()
    } else {
        "curl".into()
    }
}

/// The release asset this platform installs from.
fn asset_name(version: &semver::Version) -> String {
    // The names scripts/release-{macos.sh,windows.ps1,linux.sh} give them.
    if cfg!(target_os = "macos") {
        format!("Tern-{version}-macos-universal.dmg")
    } else if cfg!(windows) {
        format!("Tern-{version}-windows-x64-setup.exe")
    } else {
        format!("Tern-{version}-linux-x86_64.tar.gz")
    }
}

/// Why Tern can't replace `bundle` in place: macOS runs it from a random
/// read-only copy (App Translocation, when it was never moved out of the
/// download), or it runs from the DMG or another folder it can't write.
fn install_blocker(bundle: &Path) -> Option<String> {
    if cfg!(windows) {
        // The folder itself: the installer replaces the files in it.
        return (!writable(bundle))
            .then(|| format!("Tern can't write to {}: download the update from the release page.", bundle.display()));
    }
    if is_translocated(bundle) {
        return Some("Move Tern to the Applications folder and open it from there to install updates.".to_owned());
    }
    let dir = bundle.parent()?;
    match writable(dir) {
        true => None,
        false if bundle.starts_with("/Volumes") => {
            Some("Tern is running from its disk image: drag it to the Applications folder and open it from there.".to_owned())
        },
        false => Some(format!("Tern can't write to {}: download the update from the release page.", dir.display())),
    }
}

fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".tern-write-test-{}", std::process::id()));
    let ok = std::fs::OpenOptions::new().write(true).create_new(true).open(&probe).is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

fn is_translocated(bundle: &Path) -> bool {
    bundle.components().any(|c| c.as_os_str() == "AppTranslocation")
}

fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let ok = macos.file_name()? == "MacOS" && contents.file_name()? == "Contents" && bundle.extension()? == "app";
    ok.then(|| bundle.to_owned())
}

#[cfg(any(target_os = "macos", windows))]
fn curl() -> Command {
    let mut c = command(curl_path());
    c.args(["-fsSL", "--connect-timeout", "15", "-A", concat!("Tern/", env!("CARGO_PKG_VERSION"))]);
    // HTTPS only, also after redirects; give up on a stalled download (under
    // 1 KB/s for 30 s) or one that takes over 15 minutes.
    c.args(["--proto", "=https", "--proto-redir", "=https", "--speed-limit", "1024", "--speed-time", "30", "--max-time", "900"]);
    c
}

/// The latest published release, if it has this platform's asset. Reads the redirect of
/// github.com's /releases/latest (which skips drafts and prereleases) rather
/// than the REST API, whose anonymous limit of 60 requests an hour is shared
/// by everyone behind the same IP address.
fn fetch_latest() -> Result<Option<Release>, String> {
    let latest = format!("https://github.com/{REPO}/releases/latest");
    let Some(tag) = parse_latest_redirect(&head(&latest)?)? else { return Ok(None) };
    let Ok(version) = semver::Version::parse(tag.trim_start_matches('v')) else { return Ok(None) };
    let asset_url = format!("https://github.com/{REPO}/releases/download/{tag}/{}", asset_name(&version));
    // A release whose asset is missing (or still uploading) isn't offered yet.
    if !matches!(head(&asset_url)?.0, 200 | 302) {
        return Ok(None);
    }
    let page_url = format!("https://github.com/{REPO}/releases/tag/{tag}");
    Ok(Some(Release { version, asset_url, page_url }))
}

/// The HTTP status of `url` and where it redirects, without following it.
fn head(url: &str) -> Result<(u16, String), String> {
    let out = command(curl_path())
        .args([
            "-sSI",
            "-o",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
            "-w",
            "%{http_code} %{redirect_url}",
            "--connect-timeout",
            "15",
            "--max-time",
            "30",
        ])
        .args(["--proto", "=https"])
        .args(["-A", concat!("Tern/", env!("CARGO_PKG_VERSION"))])
        .arg(url)
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("Couldn't reach GitHub: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (code, location) = text.trim().split_once(' ').unwrap_or((text.trim(), ""));
    Ok((code.parse().map_err(|_| format!("unexpected curl output: {text}"))?, location.to_owned()))
}

/// The tag /releases/latest redirects to; `None` when the repo has no
/// release yet (it then redirects to the release list).
fn parse_latest_redirect((code, location): &(u16, String)) -> Result<Option<String>, String> {
    match code {
        301 | 302 => Ok(location.split_once("/releases/tag/").map(|(_, tag)| tag.to_owned()).filter(|t| !t.is_empty())),
        403 | 429 => Err("GitHub is limiting requests from this network. Try again later.".to_owned()),
        404 => Err(format!("The release repository {REPO} wasn't found.")),
        _ => Err(format!("GitHub answered HTTP {code}.")),
    }
}

#[cfg(any(target_os = "macos", windows))]
fn run(cmd: &mut Command) -> Result<(), String> {
    let out = cmd.output().map_err(|e| format!("{:?}: {e}", cmd.get_program()))?;
    if out.status.success() {
        Ok(())
    } else {
        let prog = Path::new(cmd.get_program()).file_name().unwrap_or_default().to_string_lossy().into_owned();
        Err(format!("{prog}: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Drop the folders earlier updates downloaded their installer to (the
/// installer can't delete itself); one still in use stays.
#[cfg(windows)]
fn sweep_staged() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else { return };
    for e in entries.flatten() {
        if e.file_name().to_string_lossy().starts_with("tern-update-") && e.file_type().is_ok_and(|t| t.is_dir()) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// Download the installer into a fresh temporary folder; returns its path.
#[cfg(windows)]
fn stage(url: &str, _install_dir: &Path) -> Result<PathBuf, String> {
    let tmp = std::env::temp_dir().join(format!("tern-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let setup = tmp.join("Tern-setup.exe");
    let verified = run(curl().arg("-o").arg(&setup).arg(url)).and_then(|()| verify_signature(&setup));
    if let Err(e) = verified {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    Ok(setup)
}

/// Require a valid Authenticode signature on `file` by one of the
/// certificates in `WINDOWS_SIGNER`.
#[cfg(windows)]
fn verify_signature(file: &Path) -> Result<(), String> {
    if WINDOWS_SIGNER.is_empty() {
        return Err("this Tern can't verify installers: download the update from the release page.".to_owned());
    }
    // A plain -Command (never -EncodedCommand, which antivirus software
    // flags) without double quotes, so the C runtime's quoting has nothing
    // to escape; a path can't hold a `"`.
    let script = format!(
        "$s = Get-AuthenticodeSignature -LiteralPath '{}'; $s.Status.ToString(); $s.SignerCertificate.Thumbprint",
        file.to_string_lossy().replace('\'', "''")
    );
    let out = command(crate::platform::windows_powershell())
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .map_err(|e| format!("powershell: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines().map(str::trim);
    match (lines.next(), lines.next()) {
        (Some("Valid"), Some(thumb)) if !thumb.is_empty() && WINDOWS_SIGNER.split(',').any(|t| t.trim().eq_ignore_ascii_case(thumb)) => {
            Ok(())
        },
        (status, thumb) => Err(format!(
            "the downloaded installer isn't signed by Tern's certificate (status {}, signer {})",
            status.unwrap_or("unknown"),
            thumb.filter(|s| !s.is_empty()).unwrap_or("none")
        )),
    }
}

/// Start the installer, which waits for this process to exit, installs
/// silently over the installed copy and starts the new Tern (`/UPDATE` and
/// `/WAITPID` in packaging/windows/tern.iss). Nothing in between: a hidden
/// PowerShell started by an unsigned app is what antivirus software flags.
#[cfg(windows)]
fn relaunch_after_exit(setup: &Path, _install_dir: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    Command::new(setup)
        .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/UPDATE"])
        .arg(format!("/WAITPID={}", std::process::id()))
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|e| format!("couldn't start the installer: {e}"))
}

#[cfg(not(any(target_os = "macos", windows)))]
fn stage(_: &str, _: &Path) -> Result<PathBuf, String> {
    Err("Tern can't install updates here: download the update from the release page.".to_owned())
}

#[cfg(not(any(target_os = "macos", windows)))]
fn relaunch_after_exit(_: &Path, _: &Path) -> Result<(), String> {
    Err("unsupported".to_owned())
}

/// Download the DMG and copy its Tern.app beside `bundle` (same volume, so
/// the swap is a rename). Returns the staged copy.
#[cfg(target_os = "macos")]
fn stage(dmg_url: &str, bundle: &Path) -> Result<PathBuf, String> {
    let tmp = std::env::temp_dir().join(format!("tern-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let result = (|| {
        let dmg = tmp.join("Tern.dmg");
        run(curl().arg("-o").arg(&dmg).arg(dmg_url))?;
        let mnt = tmp.join("mnt");
        run(Command::new("/usr/bin/hdiutil").args(["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint"]).arg(&mnt).arg(&dmg))?;
        let copied = (|| {
            let app = mnt.join("Tern.app");
            if !app.join("Contents/MacOS/tern").is_file() {
                return Err("the DMG holds no Tern.app".to_owned());
            }
            run(Command::new("/usr/bin/codesign").args(["--verify", "--deep", "--strict"]).arg(signer_requirement(bundle)).arg(&app))?;
            let staged = bundle.with_file_name(".Tern-update.app");
            let _ = std::fs::remove_dir_all(&staged);
            run(Command::new("/usr/bin/ditto").arg(&app).arg(&staged))
                .map_err(|e| format!("{e}\n\nTern can't write to {}.", bundle.parent().unwrap_or(bundle).display()))?;
            Ok(staged)
        })();
        let _ = run(Command::new("/usr/bin/hdiutil").args(["detach", "-force"]).arg(&mnt));
        copied
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// What the new app's signature must satisfy: Tern's bundle id and, when the
/// running app is Developer ID signed, an Apple-issued certificate of the same
/// team (an ad-hoc build has no signer to pin, so then only the id).
#[cfg(target_os = "macos")]
fn signer_requirement(bundle: &Path) -> String {
    let id = r#"identifier "dev.tern""#;
    match team_id(bundle) {
        Some(team) => format!(r#"-R={id} and anchor apple generic and certificate leaf[subject.OU] = "{team}""#),
        None => format!("-R={id}"),
    }
}

/// The Team ID that signed `bundle`, if any.
#[cfg(target_os = "macos")]
fn team_id(bundle: &Path) -> Option<String> {
    let out = Command::new("/usr/bin/codesign").arg("-dv").arg(bundle).output().ok()?;
    parse_team_id(&String::from_utf8_lossy(&out.stderr))
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_team_id(info: &str) -> Option<String> {
    let team = info.lines().find_map(|l| l.strip_prefix("TeamIdentifier="))?.trim();
    (!team.is_empty() && team != "not set" && team.chars().all(|c| c.is_ascii_alphanumeric())).then(|| team.to_owned())
}

/// Start a detached script that waits for this process to exit, swaps
/// `staged` into `bundle` (putting the old one back if that fails) and opens it.
#[cfg(target_os = "macos")]
fn relaunch_after_exit(staged: &Path, bundle: &Path) -> Result<(), String> {
    use std::os::unix::process::CommandExt as _;
    const SCRIPT: &str = r#"
while kill -0 "$1" 2>/dev/null; do sleep 0.2; done
old="$3.old-$$"
if mv "$3" "$old"; then
    if mv "$2" "$3"; then rm -rf "$old"; else mv "$old" "$3"; fi
fi
open "$3"
"#;
    Command::new("/bin/sh")
        .args(["-c", SCRIPT, "tern-update"])
        .arg(std::process::id().to_string())
        .arg(staged)
        .arg(bundle)
        .process_group(0)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|e| format!("couldn't start the installer: {e}"))
}

#[cfg(test)]
mod tests {
    use super::{bundle_of, is_translocated, parse_latest_redirect, parse_team_id};
    use std::path::{Path, PathBuf};

    #[test]
    fn detects_app_translocation() {
        assert!(is_translocated(Path::new("/private/var/folders/x/T/AppTranslocation/1234-ABCD/d/Tern.app")));
        assert!(!is_translocated(Path::new("/Applications/Tern.app")));
    }

    #[test]
    fn finds_the_app_bundle() {
        assert_eq!(bundle_of(Path::new("/Applications/Tern.app/Contents/MacOS/tern")), Some(PathBuf::from("/Applications/Tern.app")));
        assert_eq!(bundle_of(Path::new("/x/target/release/tern-app")), None);
    }

    #[test]
    fn reads_the_team_id() {
        assert_eq!(parse_team_id("Identifier=dev.tern\nTeamIdentifier=AB12CD34EF\n"), Some("AB12CD34EF".into()));
        assert_eq!(parse_team_id("Signature=adhoc\nTeamIdentifier=not set\n"), None);
    }

    #[test]
    fn reads_the_latest_tag_from_the_redirect() {
        let r = |code: u16, loc: &str| parse_latest_redirect(&(code, loc.to_owned()));
        assert_eq!(r(302, "https://github.com/o/tern/releases/tag/v0.2.0"), Ok(Some("v0.2.0".into())));
        // No release yet: GitHub redirects to the release list.
        assert_eq!(r(302, "https://github.com/o/tern/releases"), Ok(None));
        assert!(r(403, "").unwrap_err().contains("limiting"));
        assert!(r(429, "").unwrap_err().contains("limiting"));
        assert!(r(500, "").is_err());
    }
}
