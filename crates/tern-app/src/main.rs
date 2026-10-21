// A release build is a GUI app on Windows: no console window of its own.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod accounts;
mod agent_monitor;
mod agents;
mod arrange;
mod caffeine;
mod changes_view;
#[cfg(debug_assertions)]
mod control;
mod diff_pane;
mod dirs;
mod dock;
mod dock_badge;
mod file_icons;
mod file_tree_view;
mod git_log_view;
#[cfg(not(target_os = "macos"))]
mod handoff;
mod links;
mod main_window;
mod menus;
mod palette;
mod panes;
mod part_window;
mod platform;
mod resources;
mod resources_view;
mod restart;
mod scratch;
mod scratches_view;
mod settings;
mod settings_window;
mod shells_view;
#[cfg(any(target_os = "macos", windows))]
mod shot;
mod shot_icons;
mod style;
mod system;
mod system_view;
mod template;
mod terminal_find;
mod terminal_view;
#[cfg(target_os = "macos")]
mod terminate;
mod theme_picker;
mod update;
mod usage;
mod usage_view;
mod viewer;
mod window_grip;
mod window_state;
mod workspace;

use std::path::PathBuf;

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;
use main_window::Content;
use terminal_view::TerminalConfig;

actions!(tern, [Quit, OpenFolder, ToggleMenuBarIcon]);

/// Process-wide services every window shares.
struct AppServices(workspace::Services);

impl Global for AppServices {}

fn main() {
    // Claude Code's status line in a Tern tab: before anything else, as it
    // runs after every message.
    if std::env::args().nth(1).as_deref() == Some("--statusline") {
        let settings = std::env::args_os().nth(2).map(PathBuf::from).unwrap_or_default();
        std::process::exit(tern_bridge::statusline::run(&settings));
    }
    // A Claude Code hook it runs only as a command (see `tern_bridge::relay`).
    if std::env::args().nth(1).as_deref() == Some("--hook") {
        let settings = std::env::args_os().nth(2).map(PathBuf::from).unwrap_or_default();
        std::process::exit(tern_bridge::relay::run(&settings));
    }
    env_logger::init();
    // The UI and the daemon both read the added Claude accounts' sessions.
    tern_term::status::set_claude_accounts_dir(accounts::accounts_dir());
    // The background daemon that runs the terminals (started by the UI).
    if std::env::args().nth(1).as_deref() == Some("--server") {
        // SAFETY: no other thread runs yet. Its programs inherit this env.
        unsafe { tern_term::env::prepare_process_env() };
        tern_mux::server::run();
    }
    // Tern's terminals in a browser on another device (run by hand).
    if std::env::args().nth(1).as_deref() == Some("--web") {
        let args: Vec<String> = std::env::args().skip(2).collect();
        tern_web::run(&args);
    }
    // The menu bar agent (started by the UI): lists background terminals.
    #[cfg(target_os = "macos")]
    if std::env::args().nth(1).as_deref() == Some("--menubar") {
        tern_menubar::run();
    }
    // Read before the environment is cleaned for terminals.
    let control_socket = std::env::var_os("TERN_CONTROL").map(PathBuf::from);
    // SAFETY: no other thread runs yet.
    unsafe { tern_term::env::prepare_process_env() };
    // A folder argument opens just that folder. Otherwise the last session's
    // windows come back, plus the cwd when started from a shell (from Finder
    // or the Dock the cwd is `/`); with no session, the last folder or the
    // welcome page.
    // `--open-session <id>`: the menu bar agent's Open in debug builds
    // (release ones get a `tern://session/<id>` URL).
    let open_session = match std::env::args().nth(1).as_deref() {
        Some("--open-session") => std::env::args().nth(2).and_then(|s| s.parse::<u64>().ok()),
        _ => None,
    };
    // Absolute, as a running Tern it may be handed to has another cwd.
    let arg: Option<PathBuf> = if open_session.is_some() {
        None
    } else {
        std::env::args_os().nth(1).map(PathBuf::from).map(|p| std::path::absolute(&p).unwrap_or(p))
    };
    // `--open-session` shows a session, not the folder it was started in.
    let shell_cwd = std::env::current_dir().ok().filter(|d| d.parent().is_some() && open_session.is_none());
    // One Tern at a time shares the saved session: a second one hands its
    // folder to the first and exits.
    let Ok(_lock) = lock_instance() else {
        forward_to_running(arg.or(shell_cwd));
        return;
    };
    #[cfg(not(target_os = "macos"))]
    let handoff = handoff::listen();

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
        let mono_font = platform::terminal_font(cx);
        settings::init(mono_font, cx);
        terminal_view::bind_keys(cx);
        theme_picker::bind_keys(cx);
        viewer::bind_keys(cx);
        file_tree_view::bind_keys(cx);
        scratch::bind_keys(cx);
        workspace::bind_keys(cx);
        main_window::bind_keys(cx);
        cx.bind_keys([platform::bind("cmd-q", Quit, None), platform::bind("cmd-o", OpenFolder, None)]);
        cx.on_action(|_: &Quit, cx| quit(cx));
        // Quitting from the Dock, AppleScript or logout asks first too.
        #[cfg(target_os = "macos")]
        {
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
        }
        menus::install(cx);
        menus::init(cx);
        cx.on_action(|_: &ToggleMenuBarIcon, cx| toggle_menu_bar_icon(cx));
        update::init(cx);
        agent_monitor::init(cx);
        settings_window::init(cx);
        #[cfg(any(target_os = "macos", windows))]
        shot::init(cx);
        // Reached only when no window has focus.
        cx.on_action(|_: &OpenFolder, cx| main_window::prompt_open_folder(cx));
        cx.on_action(|_: &main_window::NewWindow, cx| main_window::open_window(Content::Welcome, None, cx));
        let services = workspace::services(TerminalConfig::from_look(cx));

        // One receiver for hook events, fanned out to every workspace.
        let hook_events = services.hook_events.clone();
        cx.spawn(async move |cx| {
            while let Ok(ev) = hook_events.recv_async().await {
                cx.update(|cx| main_window::dispatch_hook(ev, cx));
            }
        })
        .detach();
        // Sessions the menu bar agent asks to show.
        let reveal_events = services.reveal_events.clone();
        cx.spawn(async move |cx| {
            while let Ok(id) = reveal_events.recv_async().await {
                cx.update(|cx| main_window::reveal_session(id, cx));
            }
        })
        .detach();
        cx.set_global(AppServices(services));

        // Scripted debug runs leave the real session alone (unless
        // `TERN_SESSION` is set, e.g. with a scratch `HOME`).
        let scripted = cfg!(debug_assertions) && control_socket.is_some() && std::env::var_os("TERN_SESSION").is_none();
        cx.set_global(main_window::Session { persist: !scripted, quitting: false });
        // The menu bar agent outlives us; a newer one replaces an older one.
        if cfg!(target_os = "macos") && !scripted && tern_menubar::setting::enabled(&tern_menubar::setting::file()) {
            tern_menubar::launch();
        }
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
            Some(root) => {
                main_window::open_folder(root, cx);
                // The last session's windows don't come back: their
                // terminals join this one rather than run unseen.
                if !scripted {
                    main_window::adopt_orphans(Default::default(), cx);
                }
            },
            None => {
                let restored = !scripted && main_window::restore_session(cx);
                match shell_cwd.or_else(|| (!restored).then(window_state::last_folder).flatten()) {
                    Some(root) => main_window::open_folder(root, cx),
                    None if !restored => main_window::open_window(Content::Welcome, None, cx),
                    None => {},
                }
            },
        }
        if let Some(id) = open_session {
            main_window::reveal_session(id, cx);
        }
        if !scripted {
            restart::check_at_launch(cx);
            settings_window::start_remote_at_launch(cx);
        }
        cx.spawn(async move |cx| {
            while let Ok(urls) = url_rx.recv_async().await {
                cx.update(|cx| {
                    for url in &urls {
                        if let Some(id) = session_url_id(url) {
                            main_window::reveal_session(id, cx);
                        }
                    }
                    for path in urls.iter().filter_map(|u| file_url_path(u)).filter(|p| p.is_dir()) {
                        main_window::open_folder(path, cx);
                    }
                    cx.activate(true);
                });
            }
        })
        .detach();
        // Folders handed over by a later `tern <dir>` (see `handoff`).
        #[cfg(not(target_os = "macos"))]
        if let Some(rx) = handoff {
            cx.spawn(async move |cx| {
                while let Ok(folder) = rx.recv_async().await {
                    cx.update(|cx| {
                        match folder.filter(|p| p.is_dir()) {
                            Some(path) => main_window::open_folder(path, cx),
                            // No folder: bring a window forward.
                            None => {
                                if let Some(w) = cx.windows().first() {
                                    let _ = w.update(cx, |_, window, _| window.activate_window());
                                }
                            },
                        }
                        cx.activate(true);
                    });
                }
            })
            .detach();
        }
        cx.on_window_closed(|cx, _| {
            // Its agents keep nothing awake any more.
            caffeine::settle(cx);
            // The agent monitor and the parts' own windows don't keep Tern open by themselves.
            let monitor = agent_monitor::window(cx);
            if cx.windows().iter().all(|w| Some(*w) == monitor || part_window::is_part_window(*w, cx)) {
                agent_monitor::hide(cx);
                // Approved up front: `cx.quit()` terminates from a main-queue
                // block, where a "later" answer could never come.
                quit_now(cx);
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

fn toggle_menu_bar_icon(cx: &mut App) {
    let file = tern_menubar::setting::file();
    let show = !tern_menubar::setting::enabled(&file);
    if let Err(e) = tern_menubar::setting::set_enabled(&file, show) {
        log::warn!("can't save the menu bar setting: {e}");
        return;
    }
    if show {
        tern_menubar::launch()
    } else {
        tern_menubar::stop()
    }
    menus::install(cx);
}

/// Quit, keeping every open window in the saved session. Programs keep
/// running in the background and are there again at the next launch.
pub fn quit(cx: &mut App) {
    confirm_quit(cx, |ok, cx| {
        if ok {
            quit_now(cx)
        }
    });
}

/// Quitting ends nothing (terminals run in the background daemon), so it
/// needs no confirmation; `done` gets the answer.
fn confirm_quit(cx: &mut App, done: impl FnOnce(bool, &mut App) + 'static) {
    done(true, cx)
}

fn quit_now(cx: &mut App) {
    cx.default_global::<main_window::Session>().quitting = true;
    #[cfg(target_os = "macos")]
    terminate::approve();
    cx.quit()
}

/// Take the lock held while this Tern runs: `Err` if another holds it,
/// `Ok(None)` if it can't be taken for another reason (Tern runs anyway).
/// Debug builds use their own, to run beside the installed app.
fn lock_instance() -> Result<Option<std::fs::File>, ()> {
    let dir = dirs::cache();
    let _ = std::fs::create_dir_all(&dir);
    let name = if cfg!(debug_assertions) { "dev.lock" } else { "tern.lock" };
    let Ok(file) = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(name)) else { return Ok(None) };
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Err(()),
        Err(_) => Ok(None),
    }
}

/// Hand `folder` to the Tern already running, which brings it forward.
#[cfg(not(target_os = "macos"))]
fn forward_to_running(folder: Option<PathBuf>) {
    if let Err(e) = handoff::send(folder) {
        eprintln!("tern: already running; couldn't reach it: {e}");
    }
}

/// Hand `folder` to the Tern already running (it gets it as an open-URL
/// event), or just bring it forward.
#[cfg(target_os = "macos")]
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

/// The session id in a `tern://session/<id>` URL (from the menu bar agent).
fn session_url_id(url: &str) -> Option<u64> {
    let id = url.strip_prefix("tern://session/")?;
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    id.parse().ok()
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
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
    }
    // `file:///C:/x` holds a drive path.
    #[cfg(windows)]
    {
        let s = String::from_utf8(out).ok()?;
        let s = s.strip_prefix('/').filter(|r| r.as_bytes().get(1) == Some(&b':')).unwrap_or(&s);
        Some(PathBuf::from(s.replace('/', "\\")))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::file_url_path;
    use std::path::PathBuf;

    #[test]
    fn decodes_file_urls() {
        assert_eq!(file_url_path("file:///Users/a/my%20dir/"), Some(PathBuf::from("/Users/a/my dir/")));
        assert_eq!(file_url_path("file://localhost/tmp/x"), Some(PathBuf::from("/tmp/x")));
        assert_eq!(file_url_path("https://x"), None);
    }

    #[test]
    fn parses_session_urls() {
        use super::session_url_id;
        assert_eq!(session_url_id("tern://session/42"), Some(42));
        assert_eq!(session_url_id("tern://session/9223372036854775807"), Some(9223372036854775807));
        assert_eq!(session_url_id("tern://session/abc"), None);
        assert_eq!(session_url_id("tern://session/1/x"), None);
        assert_eq!(session_url_id("tern://session/-1"), None);
        assert_eq!(session_url_id("tern://session/99999999999999999999999"), None);
        assert_eq!(session_url_id("tern://session/"), None);
        assert_eq!(session_url_id("file:///tmp"), None);
    }
}
