mod agents;
mod changes_view;
#[cfg(debug_assertions)]
mod control;
mod diff_pane;
mod file_icons;
mod file_tree_view;
mod links;
mod main_window;
mod panes;
mod scratch;
mod scratches_view;
mod shot;
mod terminal_view;
mod terminate;
mod update;
mod viewer;
mod window_state;
mod workspace;

use std::path::PathBuf;

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;
use main_window::Content;
use terminal_view::TerminalConfig;

actions!(tern, [Quit, OpenFolder]);

/// Process-wide services every window shares.
struct AppServices(workspace::Services);

impl Global for AppServices {}

fn main() {
    env_logger::init();
    // Read before the environment is cleaned for terminals.
    let control_socket = std::env::var_os("TERN_CONTROL").map(PathBuf::from);
    // SAFETY: no other thread runs yet.
    unsafe { tern_term::env::prepare_process_env() };
    // A folder argument opens just that folder. Otherwise the last session's
    // windows come back, plus the cwd when started from a shell (from Finder
    // or the Dock the cwd is `/`); with no session, the last folder or the
    // welcome page.
    let arg: Option<PathBuf> = std::env::args().nth(1).map(PathBuf::from);
    let shell_cwd = std::env::current_dir().ok().filter(|d| d.parent().is_some());
    // One Tern at a time shares the saved session: a second one hands its
    // folder to the first and exits.
    let Ok(_lock) = lock_instance() else {
        forward_to_running(arg.or(shell_cwd));
        return;
    };

    // Folders handed over by the OS (`open -a Tern <dir>`, a folder dropped
    // on the Dock icon), also while running.
    let (url_tx, url_rx) = flume::unbounded::<Vec<String>>();
    let app = gpui_kit::application().with_assets(file_icons::AppAssets);
    app.on_open_urls(move |urls| {
        let _ = url_tx.send(urls);
    });
    app.run(move |cx| {
        gpui_kit::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        terminal_view::bind_keys(cx);
        viewer::bind_keys(cx);
        file_tree_view::bind_keys(cx);
        scratch::bind_keys(cx);
        workspace::bind_keys(cx);
        main_window::bind_keys(cx);
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None), KeyBinding::new("cmd-o", OpenFolder, None)]);
        cx.on_action(|_: &Quit, cx| quit(cx));
        // Quitting from the Dock, AppleScript or logout asks first too.
        let (terminate_tx, terminate_rx) = flume::unbounded();
        terminate::install(terminate_tx);
        cx.spawn(async move |cx| {
            while terminate_rx.recv_async().await.is_ok() {
                cx.update(|cx| {
                    confirm_quit(cx, |ok, cx| {
                        if ok {
                            cx.default_global::<main_window::Session>().quitting = true;
                        }
                        terminate::reply(ok);
                    })
                });
            }
        })
        .detach();
        cx.set_menus([Menu::new("Tern").items([
            MenuItem::action("Check for Updates…", update::CheckForUpdates),
            MenuItem::separator(),
            MenuItem::action("Capture Screenshot (⌘⇧2)", shot::CaptureScreenshot),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Quit Tern", Quit),
        ])]);
        update::init(cx);
        shot::init(cx);
        // Reached only when no window has focus.
        cx.on_action(|_: &OpenFolder, cx| main_window::prompt_open_folder(cx));
        cx.on_action(|_: &main_window::NewWindow, cx| main_window::open_window(Content::Welcome, None, cx));
        let services = workspace::services(TerminalConfig::default());

        // One receiver for hook events, fanned out to every workspace.
        let hook_events = services.hook_events.clone();
        cx.spawn(async move |cx| {
            while let Ok(ev) = hook_events.recv_async().await {
                cx.update(|cx| main_window::dispatch_hook(ev, cx));
            }
        })
        .detach();
        cx.set_global(AppServices(services));

        // Scripted debug runs leave the real session alone (unless
        // `TERN_SESSION` is set, e.g. with a scratch `HOME`).
        let scripted = cfg!(debug_assertions) && control_socket.is_some() && std::env::var_os("TERN_SESSION").is_none();
        cx.set_global(main_window::Session { persist: !scripted, quitting: false });
        #[cfg(debug_assertions)]
        if let Some(rx) = control_socket.and_then(control::listen) {
            cx.spawn(async move |cx| {
                while let Ok((line, reply)) = rx.recv_async().await {
                    let _ = reply.send(cx.update(|cx| main_window::control(&line, cx)));
                }
            })
            .detach();
        }

        match arg {
            Some(root) => main_window::open_folder(root, cx),
            None => {
                let restored = !scripted && main_window::restore_session(cx);
                match shell_cwd.or_else(|| (!restored).then(window_state::last_folder).flatten()) {
                    Some(root) => main_window::open_folder(root, cx),
                    None if !restored => main_window::open_window(Content::Welcome, None, cx),
                    None => {},
                }
            },
        }
        cx.spawn(async move |cx| {
            while let Ok(urls) = url_rx.recv_async().await {
                cx.update(|cx| {
                    for path in urls.iter().filter_map(|u| file_url_path(u)).filter(|p| p.is_dir()) {
                        main_window::open_folder(path, cx);
                    }
                    cx.activate(true);
                });
            }
        })
        .detach();
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            } else {
                // A window closed by the user drops out of the session; the
                // last one stays, to come back at the next launch.
                main_window::save_session(cx);
            }
        })
        .detach();
        cx.activate(true);
    });
}

/// Quit, keeping every open window in the saved session; ask first when
/// that ends running programs.
pub fn quit(cx: &mut App) {
    confirm_quit(cx, |ok, cx| {
        if ok {
            quit_now(cx)
        }
    });
}

/// Ask whether to quit when that ends running programs; `done` gets the answer.
fn confirm_quit(cx: &mut App, done: impl FnOnce(bool, &mut App) + 'static) {
    let jobs = main_window::running(cx);
    let window = cx.active_window().or_else(|| cx.windows().first().copied());
    let (Some(window), false) = (window, jobs.is_empty()) else { return done(true, cx) };
    let detail = format!("Still running: {}. Quitting ends {}.", jobs.join(", "), if jobs.len() == 1 { "it" } else { "them" });
    let Ok(answer) =
        window.update(cx, |_, window, cx| window.prompt(PromptLevel::Warning, "Quit Tern?", Some(&detail), &["Quit", "Cancel"], cx))
    else {
        return done(true, cx);
    };
    cx.spawn(async move |cx| {
        let ok = answer.await == Ok(0);
        cx.update(|cx| done(ok, cx));
    })
    .detach();
}

fn quit_now(cx: &mut App) {
    cx.default_global::<main_window::Session>().quitting = true;
    terminate::approve();
    cx.quit()
}

/// Take the lock held while this Tern runs: `Err` if another holds it,
/// `Ok(None)` if it can't be taken for another reason (Tern runs anyway).
/// Debug builds use their own, to run beside the installed app.
fn lock_instance() -> Result<Option<std::fs::File>, ()> {
    use std::os::fd::AsRawFd;
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/tmp".into());
    let dir = home.join("Library/Caches/dev.tern");
    let _ = std::fs::create_dir_all(&dir);
    let name = if cfg!(debug_assertions) { "dev.lock" } else { "tern.lock" };
    let Ok(file) = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(name)) else { return Ok(None) };
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(Some(file));
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::EWOULDBLOCK) => Err(()),
        _ => Ok(None),
    }
}

/// Hand `folder` to the Tern already running (it gets it as an open-URL
/// event), or just bring it forward.
fn forward_to_running(folder: Option<PathBuf>) {
    if cfg!(debug_assertions) {
        eprintln!("tern: a debug Tern is already running");
        return;
    }
    let mut open = std::process::Command::new("/usr/bin/open");
    open.args(["-b", "dev.tern"]);
    if let Some(folder) = folder {
        open.arg(folder);
    }
    if let Err(e) = open.status() {
        eprintln!("tern: already running; couldn't reach it: {e}");
    }
}

/// The path of a `file://` URL, percent-decoded.
fn file_url_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%'
            && let (Some(h), Some(l)) = (bytes.get(i + 1).copied().and_then(hex), bytes.get(i + 2).copied().and_then(hex))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
}

#[cfg(test)]
mod tests {
    use super::file_url_path;
    use std::path::PathBuf;

    #[test]
    fn decodes_file_urls() {
        assert_eq!(file_url_path("file:///Users/a/my%20dir/"), Some(PathBuf::from("/Users/a/my dir/")));
        assert_eq!(file_url_path("file://localhost/tmp/x"), Some(PathBuf::from("/tmp/x")));
        assert_eq!(file_url_path("https://x"), None);
    }
}
