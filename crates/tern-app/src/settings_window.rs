//! The Settings window (⌘,): a page per section, as in T3 Code. The pages
//! show and change what `settings.json` holds (its every key stays there
//! for editing by hand: General ▸ Settings file), and Connections turns on
//! `tern --web`, Tern's terminals in a browser on another device: it makes
//! one-time pairing links (with a QR code for a phone's camera), and lists
//! the paired devices to revoke them.
//!
//! Drawn by the `settings` templates; the code fills the rows and attaches
//! what each control does.

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::*;

use tern_web::access::{self, Access, NewPairing};

use crate::template::{self, Framed, on_event};
use crate::theme_picker::OpenSettings;
use crate::{restart, settings, style, theme_picker, update};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    General,
    Appearance,
    Terminal,
    Editor,
    Connections,
}

/// Each section: what it is, its label, icon and page subtitle.
const SECTIONS: &[(Section, &str, &str, &str)] = &[
    (Section::General, "General", "settings-2", "The settings file, background terminals and updates."),
    (Section::Appearance, "Appearance", "palette", "Theme, text size and layout."),
    (Section::Terminal, "Terminal", "square-terminal", "Terminal tabs."),
    (Section::Editor, "Editor", "file-code", "The file editor and code blocks."),
    (Section::Connections, "Connections", "globe", "Use Tern from your phone or another computer."),
];

/// How often Connections looks again at the server (a server started or
/// stopped by hand shows too).
const REFRESH: Duration = Duration::from_secs(2);
/// How long a server may take to listen (it first asks Tailscale for its
/// address) before starting counts as failed.
const START_TIMEOUT: Duration = Duration::from_secs(10);
/// How long "Copied" shows.
const COPIED: Duration = Duration::from_secs(2);

/// The window while it is open, and its view.
#[derive(Default)]
struct Open(Option<(AnyWindowHandle, WeakEntity<SettingsView>)>);

impl Global for Open {}

pub fn init(cx: &mut App) {
    // Main windows don't handle it: they open `settings.json` on `OpenSettingsFile`.
    cx.on_action(|_: &OpenSettings, cx| open(cx));
}

/// Bring the Settings window forward, opening it if needed.
pub fn open(cx: &mut App) {
    if let Some((h, _)) = cx.default_global::<Open>().0.clone()
        && h.update(cx, |_, window, _| window.activate_window()).is_ok()
    {
        return;
    }
    let bounds = Bounds::centered(None, size(px(920.), px(640.)), cx);
    let opened = gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some("Settings".into()), ..Default::default() }),
            window_min_size: Some(size(px(640.), px(420.))),
            window_background: crate::style::backdrop(cx).appearance,
            app_id: Some("dev.tern.Tern".into()),
            ..Default::default()
        },
        cx,
        |window, cx| cx.new(|cx| SettingsView::new(window, cx)),
    );
    match opened {
        Ok((handle, view)) => {
            cx.default_global::<Open>().0 = Some((handle, view.downgrade()));
            let _ = handle.update(cx, |_, window, _| window.activate_window());
        },
        Err(e) => log::error!("failed to open the Settings window: {e}"),
    }
}

/// Debug control: open the window at the section labeled `label`.
#[cfg(debug_assertions)]
pub fn show(label: &str, cx: &mut App) -> String {
    let Some(&(section, ..)) = SECTIONS.iter().find(|s| s.1.eq_ignore_ascii_case(label)) else { return "error: no such section".into() };
    open(cx);
    let Some((_, view)) = cx.default_global::<Open>().0.clone() else { return "error: no window".into() };
    match view.update(cx, |v, cx| {
        v.section = section;
        cx.notify();
    }) {
        Ok(()) => "ok".into(),
        Err(_) => "error: window closed".into(),
    }
}

/// Debug control: `click X Y`, `key K` (see `control`) in the window.
#[cfg(debug_assertions)]
pub fn input(line: &str, cx: &mut App) -> String {
    let Some((h, _)) = cx.default_global::<Open>().0.clone() else { return "error: no Settings window".into() };
    h.update(cx, |_, window, cx| crate::control::input(line, window, cx).unwrap_or_else(|| "error: unknown command".into()))
        .unwrap_or_else(|_| "error: window closed".into())
}

/// At launch: serve the terminals again if the app had that on (the server
/// outlives the app, but not a restart of the machine).
pub fn start_remote_at_launch(cx: &mut App) {
    cx.background_spawn(async move {
        if let Some(all_networks) = tern_web::wanted()
            && tern_web::serving().is_none()
            && let Err(e) = tern_web::start(all_networks)
        {
            log::warn!("can't start serving the terminals: {e}");
        }
    })
    .detach();
}

type Handler = Rc<dyn Fn(&mut SettingsView, &mut Window, &mut Context<SettingsView>)>;

/// A setting on a page.
struct Row {
    title: &'static str,
    description: String,
    problem: Option<String>,
    value: Option<String>,
    control: Control,
}

enum Control {
    None,
    Switch { on: bool, busy: bool, toggle: Handler },
    Action { label: &'static str, busy: bool, run: Handler },
    Stepper(Stepper),
}

/// A number in `settings.json`, changed a step at a time.
#[derive(Clone, Copy)]
struct Stepper {
    path: &'static [&'static str],
    value: f64,
    default: f64,
    step: f64,
    min: f64,
    max: f64,
    decimals: usize,
}

impl Stepper {
    fn show(&self, v: f64) -> String {
        format!("{v:.*}", self.decimals)
    }

    /// One step up (`1.`) or down (`-1.`), kept on the step's grid.
    fn stepped(&self, dir: f64) -> f64 {
        let v = ((self.value / self.step).round() + dir) * self.step;
        let scale = 10f64.powi(self.decimals as i32);
        (v.clamp(self.min, self.max) * scale).round() / scale
    }
}

impl Row {
    fn new(title: &'static str, description: impl Into<String>) -> Self {
        Row { title, description: description.into(), problem: None, value: None, control: Control::None }
    }

    fn value(mut self, v: impl Into<String>) -> Self {
        self.value = Some(v.into());
        self
    }

    fn action(mut self, label: &'static str, run: impl Fn(&mut SettingsView, &mut Window, &mut Context<SettingsView>) + 'static) -> Self {
        self.control = Control::Action { label, busy: false, run: Rc::new(run) };
        self
    }

    fn switch(mut self, on: bool, toggle: impl Fn(&mut SettingsView, &mut Window, &mut Context<SettingsView>) + 'static) -> Self {
        self.control = Control::Switch { on, busy: false, toggle: Rc::new(toggle) };
        self
    }

    fn busy(mut self, b: bool) -> Self {
        if let Control::Switch { busy, .. } | Control::Action { busy, .. } = &mut self.control {
            *busy = b;
        }
        self
    }

    fn scope(&self, i: usize) -> template::Scope {
        let mut s = template::Scope::new(format!("row-{i}"))
            .var("title", self.title)
            .var("description", self.description.clone())
            .var("problem", self.problem.clone().unwrap_or_default());
        match &self.control {
            Control::None => {},
            Control::Switch { on, busy, .. } => s = s.var("switch", true).var("on", *on).var("busy", *busy),
            Control::Action { label, busy, .. } => s = s.var("action", *label).var("busy", *busy),
            Control::Stepper(st) => {
                s = s
                    .var("stepper", true)
                    .var("value", st.show(st.value))
                    .var("default", st.show(st.default))
                    .var("changed", st.value != st.default)
                    .var("at_min", st.value <= st.min)
                    .var("at_max", st.value >= st.max)
            },
        }
        if let Some(v) = &self.value {
            s = s.var("value", v.clone());
        }
        s
    }
}

/// What Connections knows of the server.
#[derive(Default)]
struct Remote {
    /// The running server, as of the last look.
    serving: Option<tern_web::Serving>,
    /// Who has access, as of the last look.
    access: Access,
    /// The pairing link made here, until it's used, revoked or expired.
    pairing: Option<NewPairing>,
    /// Looked at least once.
    checked: bool,
    /// Starting or stopping the server, or changing who has access.
    busy: bool,
    error: Option<String>,
    /// Listen on every network (else Tailscale's); used at the next start.
    all_networks: bool,
    copied: Option<Instant>,
}

pub struct SettingsView {
    section: Section,
    /// `settings.json` as of the templates' `generation` (each load compiles
    /// them anew).
    file: serde_json::Value,
    generation: u64,
    remote: Remote,
    /// What the desktop does behind the window, as last set.
    backdrop: crate::style::Backdrop,
    _refresh: Task<()>,
}

impl SettingsView {
    fn new(_: &mut Window, cx: &mut Context<Self>) -> Self {
        let refresh = cx.spawn(async move |this, cx| {
            loop {
                let Ok(look) = this.update(cx, |v, _| v.section == Section::Connections && !v.remote.busy) else { break };
                if look {
                    let found = cx.background_spawn(async move { (tern_web::serving(), tern_web::wanted(), access::load()) }).await;
                    if this.update(cx, |v, cx| v.found(found, cx)).is_err() {
                        break;
                    }
                }
                cx.background_executor().timer(REFRESH).await;
            }
        });
        SettingsView {
            section: Section::General,
            file: settings::file_value(cx),
            generation: generation(cx),
            remote: Remote::default(),
            backdrop: crate::style::Backdrop::default(),
            _refresh: refresh,
        }
    }

    fn found(&mut self, (serving, wanted, access): (Option<tern_web::Serving>, Option<bool>, Access), cx: &mut Context<Self>) {
        if self.remote.busy {
            return;
        }
        if !self.remote.checked {
            self.remote.all_networks = serving.as_ref().map(|s| s.all_networks).or(wanted).unwrap_or(false);
        }
        self.remote.checked = true;
        if self.remote.serving != serving || self.remote.access != access {
            // Running after all (started by hand, or late): why it didn't start is old news.
            if serving.is_some() && self.remote.serving.is_none() {
                self.remote.error = None;
            }
            self.remote.serving = serving;
            self.set_access(access);
            cx.notify();
        } else if self.remote.pairing.is_some() {
            // Its countdown.
            cx.notify();
        }
    }

    /// Who has access now; the pairing link shown goes once it's used,
    /// revoked or expired.
    fn set_access(&mut self, access: Access) {
        let r = &mut self.remote;
        if r.pairing.as_ref().is_some_and(|p| !access.pairings.iter().any(|q| q.id == p.id) || p.expires <= access::now()) {
            r.pairing = None;
        }
        r.access = access;
    }

    /// `settings.json`, read again after a load.
    fn file(&mut self, cx: &App) -> &serde_json::Value {
        let g = generation(cx);
        if g != self.generation {
            self.generation = g;
            self.file = settings::file_value(cx);
        }
        &self.file
    }

    fn get(&mut self, path: &[&str], cx: &App) -> Option<serde_json::Value> {
        path.iter().try_fold(self.file(cx), |v, k| v.get(*k)).cloned()
    }

    fn number(&mut self, path: &'static [&'static str], default: f64, step: f64, (min, max): (f64, f64), cx: &App) -> Control {
        let value = self.get(path, cx).and_then(|v| v.as_f64()).unwrap_or(default);
        let decimals = if step < 0.1 {
            2
        } else if step < 1. {
            1
        } else {
            0
        };
        Control::Stepper(Stepper { path, value, default, step, min, max, decimals })
    }

    /// A number of the `window` element at `path`: its default is the
    /// theme's when it sets one, else `builtin`.
    fn window_number(&mut self, path: &'static [&'static str], builtin: f64, step: f64, range: (f64, f64), cx: &App) -> Control {
        let default = settings::theme_element("window", path[path.len() - 1], cx).and_then(|v| v.as_f64()).unwrap_or(builtin);
        self.number(path, default, step, range, cx)
    }

    fn flag(&mut self, path: &'static [&'static str], default: bool, cx: &App) -> (bool, Handler) {
        let on = self.get(path, cx).and_then(|v| v.as_bool()).unwrap_or(default);
        // Back to the default leaves the key out.
        let toggle: Handler = Rc::new(move |_, _, cx| settings::set_value(path, (on == default).then_some((!on).into()), cx));
        (on, toggle)
    }

    fn text(&mut self, path: &[&str], cx: &App) -> Option<String> {
        self.get(path, cx).and_then(|v| v.as_str().map(str::to_owned))
    }

    fn rows(&mut self, cx: &mut Context<Self>) -> Vec<Row> {
        match self.section {
            Section::General => {
                let mut rows = vec![
                    Row::new(
                        "Settings file",
                        "Every setting is in settings.json, those not shown here too: fonts, colors, the window's layout.",
                    )
                    .action("Open", |_, _, cx| crate::main_window::open_settings_file(cx)),
                    Row::new("Config folder", crate::dirs::tilde(&crate::dirs::config()))
                        .action("Show", |_, _, cx| cx.reveal_path(&crate::dirs::config())),
                ];
                if cfg!(target_os = "macos") {
                    let shown = tern_menubar::setting::enabled(&tern_menubar::setting::file());
                    rows.push(
                        Row::new("Menu bar icon", "Lists the terminals running in the background, and opens them.")
                            .switch(shown, |_, window, cx| window.dispatch_action(Box::new(crate::ToggleMenuBarIcon), cx)),
                    );
                }
                rows.push(
                    Row::new(
                        "Background terminals",
                        "Programs in Tern's terminals run in a background process. Restarting it ends them; the tabs start again in their folders.",
                    )
                    .action("Restart…", |_, window, cx| restart::confirm(window, cx)),
                );
                rows.push(
                    Row::new("Updates", format!("Tern {}", env!("CARGO_PKG_VERSION")))
                        .action("Check for Updates", |_, window, cx| window.dispatch_action(Box::new(update::CheckForUpdates), cx)),
                );
                rows
            },
            Section::Appearance => {
                let (_, theme) = settings::themes(cx);
                let (locked, lock) = self.flag(&["layout", "locked"], false, cx);
                vec![
                    Row::new("Theme", "Colors of the window and the terminals. Themes of your own go in the themes folder.")
                        .value(theme.to_string())
                        .action("Change…", |_, window, cx| theme_picker::open(window, cx)),
                    Row {
                        control: self.number(&["ui", "font_size"], 16., 1., (10., 24.), cx),
                        ..Row::new("Text size", "Panels, tabs, menus and dialogs.")
                    },
                    Row {
                        control: self.window_number(&["elements", "window", "background_opacity"], 1., 0.05, (0., 1.), cx),
                        ..Row::new("Window opacity", "Below 1 the desktop shows through the window. Menus and dialogs stay opaque.")
                    },
                    Row {
                        control: self.window_number(&["elements", "window", "background_blur"], 20., 5., (0., 100.), cx),
                        ..Row::new(
                            "Background blur",
                            if cfg!(target_os = "macos") {
                                "How much what shows through the window is blurred, in points; 0 doesn't blur."
                            } else {
                                "Blurs what shows through the window (the system's blur); 0 doesn't blur."
                            },
                        )
                    },
                    Row::new("Lock layout", "Panels can't be moved or resized.").switch(locked, move |v, w, cx| lock(v, w, cx)),
                ]
            },
            Section::Terminal => {
                let font = self.text(&["terminal", "font_family"], cx).unwrap_or_else(|| "Default".into());
                let mut rows = vec![
                    Row { control: self.number(&["terminal", "font_size"], 13., 1., (6., 40.), cx), ..Row::new("Font size", "") },
                    Row {
                        control: self.number(&["terminal", "line_height"], 1.25, 0.05, (1., 2.), cx),
                        ..Row::new("Line height", "As a multiple of the font size.")
                    },
                    Row::new("Font", "Set terminal.font_family in the settings file.").value(font),
                ];
                if cfg!(windows) {
                    let (on, toggle) = self.flag(&["terminal", "ctrl_v_paste"], true, cx);
                    rows.push(
                        Row::new("Ctrl+V pastes", "Off sends ^V to the program (Vim's block selection); Ctrl+Shift+V still pastes.")
                            .switch(on, move |v, w, cx| toggle(v, w, cx)),
                    );
                }
                rows
            },
            Section::Editor => {
                let font = self.text(&["editor", "font_family"], cx).unwrap_or_else(|| "The terminal's".into());
                vec![
                    Row { control: self.number(&["editor", "font_size"], 13., 1., (6., 40.), cx), ..Row::new("Font size", "") },
                    Row::new("Font", "Set editor.font_family in the settings file.").value(font),
                ]
            },
            Section::Connections => self.remote_rows(),
        }
    }

    fn remote_rows(&self) -> Vec<Row> {
        let r = &self.remote;
        let description = match &r.serving {
            _ if !r.checked => "Looking…".to_owned(),
            None => "Open Tern's terminals in a browser on your phone or another computer, to watch and answer agents away from \
                     the desk. It keeps serving after Tern quits."
                .to_owned(),
            Some(s) if s.local_only => {
                "On, but no Tailscale address was found: only this machine can reach it. Start Tailscale, or listen on all networks."
                    .to_owned()
            },
            Some(s) if s.all_networks => "On: reachable on every network this machine is on.".to_owned(),
            Some(_) => "On: reachable over Tailscale.".to_owned(),
        };
        let mut access = Row::new("Network access", description)
            .switch(r.serving.is_some(), |v, _, cx| if v.remote.serving.is_some() { v.stop_remote(cx) } else { v.start_remote(cx) })
            .busy(r.busy || !r.checked);
        access.problem = r.error.clone();
        let all = Row::new(
            "Listen on all networks",
            "Off: only over Tailscale, a private network between your devices. On: on any network, over plain HTTP: only on networks you trust.",
        )
        .switch(r.all_networks, |v, _, cx| {
            v.remote.all_networks = !v.remote.all_networks;
            // A running server moves over.
            if v.remote.serving.is_some() {
                v.start_remote(cx);
            }
            cx.notify();
        })
        .busy(r.busy);
        vec![access, all]
    }

    /// (Re)start the server with the current choice; then wait for it to
    /// listen.
    fn start_remote(&mut self, cx: &mut Context<Self>) {
        let all_networks = self.remote.all_networks;
        let was_running = self.remote.serving.is_some();
        self.remote.busy = true;
        self.remote.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    if was_running {
                        tern_web::stop().map_err(|e| format!("can't stop the server: {e}"))?;
                    }
                    tern_web::start(all_networks).map_err(|e| format!("can't start the server: {e}"))
                })
                .await;
            let result = match result {
                Ok(()) => wait_for(cx, START_TIMEOUT, true).await.ok_or_else(|| {
                    let why = tern_web::last_log_line().unwrap_or_default();
                    format!("The server didn't start. {why}").trim().to_owned()
                }),
                Err(e) => Err(e),
            };
            let _ = this.update(cx, |v, cx| {
                v.remote.busy = false;
                v.remote.checked = true;
                match result {
                    Ok(s) => v.remote.serving = Some(s),
                    Err(e) => {
                        v.remote.serving = None;
                        v.remote.error = Some(e);
                    },
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn stop_remote(&mut self, cx: &mut Context<Self>) {
        self.remote.busy = true;
        self.remote.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { tern_web::stop() }).await;
            let still = match result {
                Ok(()) => wait_for(cx, Duration::from_secs(3), false).await,
                Err(_) => tern_web::serving(),
            };
            let _ = this.update(cx, |v, cx| {
                v.remote.busy = false;
                v.remote.error = match (&result, &still) {
                    (Err(e), _) => Some(format!("can't stop the server: {e}")),
                    (Ok(()), Some(_)) => Some("The server is still running.".into()),
                    _ => None,
                };
                v.remote.serving = still;
                cx.notify();
            });
        })
        .detach();
    }

    /// The pairing link shown, on the server's address (`0`) or its
    /// MagicDNS name (`1`).
    fn pairing_link(&self, which: usize) -> Option<String> {
        let base = self.remote.serving.as_ref()?.links.get(which)?;
        Some(self.remote.pairing.as_ref()?.link(base))
    }

    /// Make a pairing link (in place of one not used yet).
    fn create_pairing(&mut self, cx: &mut Context<Self>) {
        self.change_access(|| access::pair().map(Some), cx);
    }

    /// Change who has access with `f` (in the background), then look again.
    /// `f` may make a pairing link to show.
    fn change_access(&mut self, f: impl FnOnce() -> std::io::Result<Option<NewPairing>> + Send + 'static, cx: &mut Context<Self>) {
        self.remote.busy = true;
        self.remote.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (result, now) = cx.background_spawn(async move { (f(), access::load()) }).await;
            let _ = this.update(cx, |v, cx| {
                v.remote.busy = false;
                match result {
                    Ok(Some(p)) => v.remote.pairing = Some(p),
                    Ok(None) => {},
                    Err(e) => v.remote.error = Some(format!("can't change who has access: {e}")),
                }
                v.set_access(now);
                cx.notify();
            });
        })
        .detach();
    }

    fn copy_link(&mut self, cx: &mut Context<Self>) {
        let Some(link) = self.pairing_link(0) else { return };
        cx.write_to_clipboard(ClipboardItem::new_string(link));
        self.remote.copied = Some(Instant::now());
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }

    /// The pairing link and the paired devices (`settings.remote`).
    fn render_remote(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let r = &self.remote;
        if !r.checked {
            return None;
        }
        let now = access::now();
        let link = self.pairing_link(0);
        let copied = r.copied.is_some_and(|t| t.elapsed() < COPIED);
        let mut devices = r.access.devices.clone();
        devices.sort_by_key(|d| std::cmp::Reverse(d.last_seen));
        let connected = |id: &str| r.serving.as_ref().is_some_and(|s| s.connected.iter().any(|c| c == id));
        let rows: Vec<template::Scope> = devices
            .iter()
            .map(|d| {
                let on = connected(&d.id);
                let used = if on { "Connected".to_owned() } else { format!("Last used {}", ago(now, d.last_seen)) };
                template::Scope::new(format!("device-{}", d.id))
                    .var("name", d.name.clone())
                    .var("detail", format!("{} · {used} · Paired {}", d.ip, ago(now, d.paired)))
                    .var("connected", on)
                    .var("busy", r.busy)
            })
            .collect();
        let summary = match devices.len() {
            0 => "No device is paired.".to_owned(),
            1 => "1 device can use your terminals.".to_owned(),
            n => format!("{n} devices can use your terminals."),
        };
        let left = r.pairing.as_ref().map_or(0, |p| p.expires.saturating_sub(now));
        let scope = template::Scope::new("remote")
            .var("serving", r.serving.is_some())
            .var("pairing", link.is_some())
            .var("link", link.clone().unwrap_or_default())
            .var("dns_link", self.pairing_link(1).unwrap_or_default())
            .var("expires", format!("Works once, for {}:{:02} more.", left / 60, left % 60))
            .var("copy_label", if copied { "Copied" } else { "Copy Link" })
            .var("copy_icon", if copied { "check" } else { "copy" })
            .var("busy", r.busy)
            .var("devices_summary", summary)
            .var("has_devices", !devices.is_empty())
            .list("devices", rows)
            .slot_opt("qr", link.map(|l| qr_code(&l, cx)));
        let me = cx.entity().downgrade();
        let ids: Vec<String> = devices.iter().map(|d| d.id.clone()).collect();
        let pairing_id = r.pairing.as_ref().map(|p| p.id.clone());
        Some(template::render(
            "settings.remote",
            scope,
            &mut |name, ix, f| match (name, f) {
                ("copy", Framed::Button(b)) => b.on_click(on_event(&me, |v, _: &ClickEvent, _, cx| v.copy_link(cx))).into_any_element(),
                ("create_link", Framed::Button(b)) => {
                    b.on_click(on_event(&me, |v, _: &ClickEvent, _, cx| v.create_pairing(cx))).into_any_element()
                },
                ("revoke_link", Framed::Button(b)) => {
                    let Some(id) = pairing_id.clone() else { return b.into_any_element() };
                    b.on_click(on_event(&me, move |v, _: &ClickEvent, _, cx| {
                        let id = id.clone();
                        v.remote.pairing = None;
                        v.change_access(move || access::revoke_pairing(&id).map(|()| None), cx);
                    }))
                    .into_any_element()
                },
                ("revoke", Framed::Button(b)) => {
                    let Some(id) = ix.and_then(|i| ids.get(i)).cloned() else { return b.into_any_element() };
                    b.on_click(on_event(&me, move |v, _: &ClickEvent, _, cx| {
                        let id = id.clone();
                        v.change_access(move || access::revoke(&id).map(|()| None), cx);
                    }))
                    .into_any_element()
                },
                ("revoke_all", Framed::Button(b)) => b
                    .on_click(on_event(&me, |v, _: &ClickEvent, _, cx| {
                        v.remote.pairing = None;
                        v.change_access(|| access::revoke_all().map(|()| None), cx);
                    }))
                    .into_any_element(),
                (_, f) => f.into_any_element(),
            },
            cx,
        ))
    }
}

/// How long before `now` the time `then` was, said shortly ("5 min ago").
fn ago(now: u64, then: u64) -> String {
    let s = now.saturating_sub(then);
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", s / 60),
        3600..86400 => format!("{} h ago", s / 3600),
        86400..172800 => "yesterday".into(),
        _ => format!("{} days ago", s / 86400),
    }
}

fn generation(cx: &App) -> u64 {
    cx.try_global::<template::Templates>().map_or(0, |t| t.generation())
}

/// Look at the server until it runs (`running`) or doesn't, for up to
/// `timeout`; what it serves at the end.
async fn wait_for(cx: &mut AsyncApp, timeout: Duration, running: bool) -> Option<tern_web::Serving> {
    let deadline = Instant::now() + timeout;
    loop {
        let s = cx.background_spawn(async move { tern_web::serving() }).await;
        if s.is_some() == running || Instant::now() >= deadline {
            return s;
        }
        cx.background_executor().timer(Duration::from_millis(200)).await;
    }
}

/// `url` as a QR code (`settings.qr`): dark squares on a light card, in
/// every theme, for a camera to read.
fn qr_code(url: &str, cx: &App) -> AnyElement {
    let look = style::el("settings.qr", cx);
    let side = look.px("size");
    let dark = look.color("color");
    let Some((width, modules)) = tern_web::qr(url) else { return div().into_any_element() };
    look.apply(div().flex_none())
        .child(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    let cell = bounds.size.width / width as f32;
                    for (i, _) in modules.iter().enumerate().filter(|(_, d)| **d) {
                        let (x, y) = ((i % width) as f32, (i / width) as f32);
                        let origin = point(bounds.origin.x + cell * x, bounds.origin.y + cell * y);
                        // A hair over, so neighbors leave no seams.
                        window.paint_quad(fill(Bounds::new(origin, size(cell + px(0.5), cell + px(0.5))), dark));
                    }
                },
            )
            .size(side),
        )
        .into_any_element()
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::style::sync_backdrop(&mut self.backdrop, window, cx);
        let rows = self.rows(cx);
        let &(_, title, _, subtitle) = SECTIONS.iter().find(|s| s.0 == self.section).expect("every section is listed");
        let sections = SECTIONS
            .iter()
            .map(|&(s, label, icon, _)| {
                template::Scope::new(format!("section-{label}")).var("label", label).var("icon", icon).var("active", s == self.section)
            })
            .collect();
        let scope = template::Scope::new("settings")
            .var("title", title)
            .var("subtitle", subtitle)
            .list("sections", sections)
            .list("rows", rows.iter().enumerate().map(|(i, r)| r.scope(i)).collect())
            .slot_opt("extra", if self.section == Section::Connections { self.render_remote(cx) } else { None });
        let me = cx.entity().downgrade();
        let handler =
            |ix: Option<usize>, pick: fn(&Control) -> Option<Handler>| ix.and_then(|i| rows.get(i)).and_then(|r| pick(&r.control));
        let body = template::render(
            "settings",
            scope,
            &mut |name, ix, f| match (name, f) {
                ("section", Framed::Box(d)) => {
                    let Some(&(section, ..)) = ix.and_then(|i| SECTIONS.get(i)) else { return d.into_any_element() };
                    d.id(("settings-section", ix.unwrap_or(0)))
                        .cursor_pointer()
                        .on_click(on_event(&me, move |v, _: &ClickEvent, _, cx| {
                            v.section = section;
                            cx.notify();
                        }))
                        .into_any_element()
                },
                ("page", Framed::Box(d)) => d.id("settings-page").overflow_y_scroll().into_any_element(),
                ("switch", Framed::Box(d)) => {
                    let pick = |c: &Control| match c {
                        Control::Switch { toggle, busy: false, .. } => Some(toggle.clone()),
                        _ => None,
                    };
                    match handler(ix, pick) {
                        Some(h) => d
                            .id(("settings-switch", ix.unwrap_or(0)))
                            .cursor_pointer()
                            .on_click(on_event(&me, move |v, _: &ClickEvent, w, cx| h(v, w, cx)))
                            .into_any_element(),
                        None => d.into_any_element(),
                    }
                },
                ("action", Framed::Button(b)) => {
                    let pick = |c: &Control| match c {
                        Control::Action { run, .. } => Some(run.clone()),
                        _ => None,
                    };
                    match handler(ix, pick) {
                        Some(h) => b.on_click(on_event(&me, move |v, _: &ClickEvent, w, cx| h(v, w, cx))).into_any_element(),
                        None => b.into_any_element(),
                    }
                },
                (step @ ("inc" | "dec" | "reset"), Framed::Button(b)) => {
                    let Some(Control::Stepper(st)) = ix.and_then(|i| rows.get(i)).map(|r| &r.control) else { return b.into_any_element() };
                    let st = *st;
                    let to = match step {
                        "inc" => Some(st.stepped(1.)),
                        "dec" => Some(st.stepped(-1.)),
                        _ => None,
                    };
                    b.on_click(move |_, _, cx| {
                        // The default leaves the key out.
                        let value = to.filter(|v| *v != st.default).and_then(|v| match st.decimals {
                            0 => Some((v as i64).into()),
                            _ => serde_json::Number::from_f64(v).map(Into::into),
                        });
                        settings::set_value(st.path, value, cx);
                    })
                    .into_any_element()
                },
                (_, f) => f.into_any_element(),
            },
            cx,
        );
        div().id("settings-window").size_full().child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::Stepper;

    #[test]
    fn steps_stay_on_the_grid_and_in_range() {
        let st = Stepper { path: &["x"], value: 1.25, default: 1.25, step: 0.05, min: 1., max: 2., decimals: 2 };
        assert_eq!(st.stepped(1.), 1.3);
        assert_eq!(st.stepped(-1.), 1.2);
        assert_eq!(Stepper { value: 2., ..st }.stepped(1.), 2.);
        assert_eq!(Stepper { value: 1.27, ..st }.stepped(1.), 1.3);
        let size = Stepper { path: &["x"], value: 13., default: 13., step: 1., min: 6., max: 40., decimals: 0 };
        assert_eq!(size.stepped(1.), 14.);
        assert_eq!(Stepper { value: 6., ..size }.stepped(-1.), 6.);
        assert_eq!(size.show(13.), "13");
        assert_eq!(st.show(1.3), "1.30");
    }
}
