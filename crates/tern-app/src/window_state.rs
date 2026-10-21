//! Remembers where the window was: display, position, size and whether it was
//! maximized or fullscreen, so the next launch opens in the same place
//! instead of on whichever screen is active. Also the session (every window
//! with its workspaces and their tabs, reopened at launch) and the recent
//! folders.

use std::path::PathBuf;

use gpui_kit::*;

fn file() -> PathBuf {
    crate::dirs::data().join("window")
}

/// One line: `<display uuid> <x> <y> <w> <h> <windowed|maximized|fullscreen>`,
/// with the bounds relative to that display, as gpui reports them.
fn encode(display: &str, bounds: WindowBounds) -> String {
    let (kind, b) = match bounds {
        WindowBounds::Windowed(b) => ("windowed", b),
        WindowBounds::Maximized(b) => ("maximized", b),
        WindowBounds::Fullscreen(b) => ("fullscreen", b),
    };
    format!("{display} {} {} {} {} {kind}\n", b.origin.x.as_f32(), b.origin.y.as_f32(), b.size.width.as_f32(), b.size.height.as_f32())
}

fn decode(line: &str) -> Option<(String, WindowBounds)> {
    let mut parts = line.split_whitespace();
    let display = parts.next()?.to_owned();
    let mut num = || parts.next()?.parse::<f32>().ok().filter(|n| n.is_finite());
    let (x, y, w, h) = (num()?, num()?, num()?, num()?);
    if w < 200. || h < 150. {
        return None;
    }
    let b = Bounds::new(point(px(x), px(y)), size(px(w), px(h)));
    let bounds = match parts.next()? {
        "windowed" => WindowBounds::Windowed(b),
        "maximized" => WindowBounds::Maximized(b),
        "fullscreen" => WindowBounds::Fullscreen(b),
        _ => return None,
    };
    Some((display, bounds))
}

/// The saved placement, if its display is still connected and the window
/// would still be reachable on it.
pub fn load(cx: &App) -> Option<(DisplayId, WindowBounds)> {
    let (uuid, bounds) = decode(&std::fs::read_to_string(file()).ok()?)?;
    resolve(&uuid, bounds, cx)
}

/// `bounds` on the display with `uuid`, if it is connected and the window
/// would still be reachable on it.
pub fn resolve(uuid: &str, bounds: WindowBounds, cx: &App) -> Option<(DisplayId, WindowBounds)> {
    let display = cx.displays().into_iter().find(|d| d.uuid().is_ok_and(|u| u.to_string() == uuid))?;
    let b = match bounds {
        WindowBounds::Windowed(b) | WindowBounds::Maximized(b) | WindowBounds::Fullscreen(b) => b,
    };
    // Keep at least the title bar on screen (display sizes may have changed).
    let screen = display.bounds().size;
    let visible = b.origin.x < screen.width - px(100.)
        && b.origin.x + b.size.width > px(100.)
        && b.origin.y >= px(0.)
        && b.origin.y < screen.height - px(40.);
    visible.then(|| (display.id(), bounds))
}

fn recent_file() -> PathBuf {
    file().with_file_name("recent")
}

const MAX_RECENT: usize = 12;

/// Folders opened before, most recent first, keeping only those that still
/// exist.
pub fn recent_folders() -> Vec<PathBuf> {
    let text = std::fs::read_to_string(recent_file())
        // Older builds kept only the last folder.
        .or_else(|_| std::fs::read_to_string(file().with_file_name("last-folder")))
        .unwrap_or_default();
    text.lines().filter(|l| !l.is_empty()).map(PathBuf::from).filter(|p| p.is_dir()).take(MAX_RECENT).collect()
}

/// The folder opened most recently, if it still exists.
pub fn last_folder() -> Option<PathBuf> {
    recent_folders().into_iter().next()
}

/// Move `root` to the front of the recent folders.
pub fn push_recent(root: &std::path::Path) {
    let mut list = recent_folders();
    list.retain(|p| p != root);
    list.insert(0, root.to_owned());
    list.truncate(MAX_RECENT);
    let text: String = list.iter().map(|p| format!("{}\n", p.display())).collect();
    write(&recent_file(), &text);
}

/// Forget every recent folder.
pub fn clear_recent() {
    write(&recent_file(), "");
}

/// Where `window` is: its display's uuid and its bounds.
pub fn placement(window: &Window, cx: &App) -> Option<(String, WindowBounds)> {
    let uuid = window.display(cx).and_then(|d| d.uuid().ok())?;
    // gpui reports the frame (title bar included) but opens windows from a
    // content size; store the content size so the window doesn't grow by a
    // title bar on every launch.
    let chrome = window.bounds().size.height - window.viewport_size().height;
    let content = |mut b: Bounds<Pixels>| {
        b.size.height = (b.size.height - chrome).max(px(150.));
        b
    };
    let bounds = match window.window_bounds() {
        WindowBounds::Windowed(b) => WindowBounds::Windowed(content(b)),
        WindowBounds::Maximized(b) => WindowBounds::Maximized(content(b)),
        WindowBounds::Fullscreen(b) => WindowBounds::Fullscreen(b),
    };
    Some((uuid.to_string(), bounds))
}

/// Remember a [`placement`] for the next new window.
pub fn save_placement(uuid: &str, bounds: WindowBounds) {
    write(&file(), &encode(uuid, bounds));
}

/// False when it couldn't be written (logged).
fn write(path: &std::path::Path, text: &str) -> bool {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = write_atomic(path, text.as_bytes()) {
        log::warn!("can't save {}: {e}", path.display());
        return false;
    }
    true
}

/// Replace `path`'s contents all at once: write a temp file beside it, then
/// rename it over, so a crash or a full disk never leaves half a file. A
/// symlink is followed, and the file keeps its permissions.
pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let dir = path.parent().ok_or_else(|| std::io::Error::other("no parent folder"))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.tern-{}.tmp", std::process::id()));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        if let Ok(meta) = std::fs::metadata(&path) {
            f.set_permissions(meta.permissions())?;
        }
        std::fs::rename(&tmp, &path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn claude_args_file() -> PathBuf {
    file().with_file_name("claude-args")
}

/// Extra arguments for every `claude` Tern starts, in shell syntax, e.g.
/// `--dangerously-skip-permissions --chrome`.
pub fn claude_args() -> String {
    one_line(&std::fs::read_to_string(claude_args_file()).unwrap_or_default())
}

pub fn set_claude_args(args: &str) {
    write(&claude_args_file(), &format!("{}\n", one_line(args)));
}

fn caffeine_file() -> PathBuf {
    file().with_file_name("caffeine")
}

/// What keeps the computer awake (`on`, `agents` or `off`), as last chosen.
pub fn caffeine_mode() -> String {
    one_line(&std::fs::read_to_string(caffeine_file()).unwrap_or_default())
}

pub fn set_caffeine_mode(mode: &str) {
    write(&caffeine_file(), &format!("{mode}\n"));
}

/// How a workspace shows its tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabLayout {
    /// One row that scrolls sideways.
    Scroll,
    /// As many rows as the tabs need.
    Wrap,
    /// A list down the left of the center pane.
    Vertical,
}

impl TabLayout {
    pub const ALL: [(TabLayout, &'static str); 3] =
        [(TabLayout::Scroll, "Scroll"), (TabLayout::Wrap, "Wrap"), (TabLayout::Vertical, "Vertical")];

    pub fn key(self) -> &'static str {
        match self {
            TabLayout::Scroll => "scroll",
            TabLayout::Wrap => "wrap",
            TabLayout::Vertical => "vertical",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().map(|(l, _)| l).find(|l| l.key() == s.trim())
    }
}

fn tab_layout_file() -> PathBuf {
    file().with_file_name("tab-layout")
}

static TAB_LAYOUT: std::sync::Mutex<Option<TabLayout>> = std::sync::Mutex::new(None);

/// How tab bars lay out their tabs, in every window.
pub fn tab_layout() -> TabLayout {
    let mut cached = TAB_LAYOUT.lock().unwrap_or_else(|e| e.into_inner());
    *cached.get_or_insert_with(|| {
        let saved = std::fs::read_to_string(tab_layout_file()).ok().and_then(|s| TabLayout::parse(&s));
        // Older builds only knew whether tabs wrap.
        saved.unwrap_or_else(|| if file().with_file_name("tab-wrap").exists() { TabLayout::Wrap } else { TabLayout::Scroll })
    })
}

pub fn set_tab_layout(layout: TabLayout) {
    *TAB_LAYOUT.lock().unwrap_or_else(|e| e.into_inner()) = Some(layout);
    write(&tab_layout_file(), &format!("{}\n", layout.key()));
    let _ = std::fs::remove_file(file().with_file_name("tab-wrap"));
}

fn word_wrap_file() -> PathBuf {
    file().with_file_name("word-wrap")
}

static WORD_WRAP: std::sync::Mutex<Option<bool>> = std::sync::Mutex::new(None);

/// Whether file and scratch editors wrap long lines, in every window.
pub fn word_wrap() -> bool {
    let mut cached = WORD_WRAP.lock().unwrap_or_else(|e| e.into_inner());
    *cached.get_or_insert_with(|| std::fs::read_to_string(word_wrap_file()).is_ok_and(|s| s.trim() == "on"))
}

pub fn set_word_wrap(on: bool) {
    *WORD_WRAP.lock().unwrap_or_else(|e| e.into_inner()) = Some(on);
    write(&word_wrap_file(), if on { "on\n" } else { "off\n" });
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether `args` passes `flag`.
pub fn has_flag(args: &str, flag: &str) -> bool {
    args.split_whitespace().any(|a| a == flag)
}

/// `args` with `flag` added, or removed when it is there.
pub fn toggle_flag(args: &str, flag: &str) -> String {
    if has_flag(args, flag) {
        args.split_whitespace().filter(|a| *a != flag).collect::<Vec<_>>().join(" ")
    } else {
        one_line(&format!("{args} {flag}"))
    }
}

/// One center tab of a saved workspace. Terminals reattach to their
/// background session (`mux`) when it still runs; otherwise they come back
/// as new processes, and a Claude tab resumes its conversation.
#[derive(Debug, Clone, PartialEq)]
pub enum SavedTab {
    /// `account`: the Claude account it runs as; `None` is the default one.
    /// `guard`: the context limit and auto switch it keeps, when turned on
    /// (see [`crate::agents::ContextGuard`]).
    Claude {
        name: Option<String>,
        session: Option<String>,
        mux: Option<u64>,
        account: Option<String>,
        guard: Option<(u8, bool)>,
    },
    Shell {
        name: Option<String>,
        mux: Option<u64>,
    },
    File(PathBuf),
    Diff(PathBuf),
    Scratch(PathBuf),
    /// A file's diff in a commit: `rel` (and a rename's `from`) relative to `repo`.
    CommitDiff {
        repo: PathBuf,
        hash: String,
        rel: String,
        from: Option<String>,
    },
    GitLog,
}

/// A tab group of a saved workspace.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedGroup {
    pub name: String,
    /// The color's name, e.g. `blue`.
    pub color: String,
    pub collapsed: bool,
}

/// One workspace of a saved window: its folder, tabs and active tab.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedWorkspace {
    pub folder: PathBuf,
    pub tabs: Vec<SavedTab>,
    /// Per tab, its index in `groups`, if it is in one.
    pub tab_groups: Vec<Option<usize>>,
    pub groups: Vec<SavedGroup>,
    pub active: usize,
    /// How the center is split (`panes::Layout::encode`); empty for one pane.
    pub panes: String,
    /// Per tab, its pane's place in the layout.
    pub tab_panes: Vec<usize>,
    /// Per pane, the tab it shows (none when it is empty).
    pub pane_active: Vec<Option<usize>>,
}

/// The side panels, the same in every workspace.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedPanels {
    /// Side panels the user hid: `explorer`, `changes`, `scratches`, `shells`.
    pub hidden: Vec<String>,
    /// Panel arrangements as `(key, Arrangement::encode text)`, kept opaque here.
    pub arrangements: Vec<(String, String)>,
}

/// The panels hidden until the user shows them.
const HIDDEN_AT_FIRST: &[&str] = &["explorer", "changes", "scratches", "shells"];

impl Default for SavedPanels {
    fn default() -> Self {
        Self { hidden: HIDDEN_AT_FIRST.iter().map(|p| (*p).to_owned()).collect(), arrangements: vec![] }
    }
}

impl SavedWorkspace {
    /// A folder opened fresh, with the default tabs.
    pub fn fresh(folder: PathBuf) -> Self {
        Self {
            folder,
            tabs: vec![],
            tab_groups: vec![],
            groups: vec![],
            active: 0,
            panes: String::new(),
            tab_panes: vec![],
            pane_active: vec![],
        }
    }
}

/// One window of the session.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedWindow {
    pub placement: Option<(String, WindowBounds)>,
    pub workspaces: Vec<SavedWorkspace>,
    pub active: usize,
}

fn session_file() -> PathBuf {
    file().with_file_name("session")
}

/// A field of a tab line: tabs and newlines would break the format.
fn field(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// First the side panels, shared by every workspace: `hidden` lists those
/// the user hid; `arrange` is one line per panel arrangement, its key and
/// encoded text. Then per window `window <active> [<placement as in the
/// window file>]`, then per workspace
/// a tab-indented folder line followed by two-tab-indented lines: `active N`,
/// one per tab group (`group`, color, collapsed, name) and one per tab, its
/// kind and fields separated by tabs, after `@N` when it is in group N.
/// A split center adds `panes`, its layout and each pane's tab (`-` for none),
/// and `tab-panes`, each tab's pane; older builds skip both lines. Builds
/// before shared panels kept `hidden` and `arrange` per workspace.
fn encode_session(windows: &[SavedWindow], panels: &SavedPanels) -> String {
    let mut out = String::new();
    // Always written: without it, the panels start as `HIDDEN_AT_FIRST`.
    out.push_str(&format!("hidden\t{}\n", field(&panels.hidden.join(" "))));
    for (key, encoded) in &panels.arrangements {
        out.push_str(&format!("arrange\t{}\t{}\n", field(key), field(encoded)));
    }
    for w in windows {
        match &w.placement {
            Some((display, bounds)) => out.push_str(&format!("window {} {}", w.active, encode(display, *bounds))),
            None => out.push_str(&format!("window {}\n", w.active)),
        }
        for ws in &w.workspaces {
            out.push_str(&format!("\t{}\n", field(&ws.folder.display().to_string())));
            // Also with no tabs: panels among the panes keep their places.
            if !ws.panes.is_empty() {
                let active: Vec<String> = ws.pane_active.iter().map(|a| a.map_or("-".into(), |a| a.to_string())).collect();
                let tab_panes: Vec<String> = ws.tab_panes.iter().map(usize::to_string).collect();
                out.push_str(&format!("\t\tpanes\t{}\t{}\n", field(&ws.panes), active.join(" ")));
                out.push_str(&format!("\t\ttab-panes\t{}\n", tab_panes.join(" ")));
            }
            if ws.tabs.is_empty() {
                continue;
            }
            out.push_str(&format!("\t\tactive\t{}\n", ws.active));
            let opt = |o: &Option<String>| o.as_deref().map(field).unwrap_or_default();
            let path = |p: &PathBuf| field(&p.display().to_string());
            for g in &ws.groups {
                out.push_str(&format!("\t\tgroup\t{}\t{}\t{}\n", field(&g.color), u8::from(g.collapsed), field(&g.name)));
            }
            for (i, tab) in ws.tabs.iter().enumerate() {
                let group = ws.tab_groups.get(i).copied().flatten().map(|g| format!("@{g}\t")).unwrap_or_default();
                let line = match tab {
                    SavedTab::Claude { name, session, mux, account, guard } => {
                        let mux = mux.map(|m| m.to_string()).unwrap_or_default();
                        // The field after `mux` is a one-workspace build's folder.
                        let rest = match (account, guard) {
                            (None, None) => String::new(),
                            (_, None) => format!("\t\t{}", opt(account)),
                            (_, Some((limit, switch))) => format!("\t\t{}\t{limit}{}", opt(account), if *switch { "s" } else { "" }),
                        };
                        format!("claude\t{}\t{}\t{mux}{rest}", opt(session), opt(name))
                    },
                    SavedTab::Shell { name, mux } => format!("shell\t{}\t{}", opt(name), mux.map(|m| m.to_string()).unwrap_or_default()),
                    SavedTab::File(p) => format!("file\t{}", path(p)),
                    SavedTab::Diff(p) => format!("diff\t{}", path(p)),
                    SavedTab::Scratch(p) => format!("scratch\t{}", path(p)),
                    SavedTab::CommitDiff { repo, hash, rel, from } => {
                        format!("commit_diff\t{}\t{}\t{}\t{}", path(repo), field(hash), field(rel), opt(from))
                    },
                    SavedTab::GitLog => "git_log".to_owned(),
                };
                out.push_str(&format!("\t\t{group}{line}\n"));
            }
        }
    }
    out
}

fn decode_tab(line: &str) -> Option<SavedTab> {
    let mut parts = line.split('\t');
    let kind = parts.next()?;
    let mut next = || parts.next().filter(|s| !s.is_empty()).map(str::to_owned);
    Some(match kind {
        "claude" => {
            let session = next();
            let name = next();
            let mux = next().and_then(|m| m.parse().ok());
            let _folder = next();
            let account = next();
            let guard = next().and_then(|g| {
                let (limit, switch) = g.strip_suffix('s').map_or((g.as_str(), false), |l| (l, true));
                Some((limit.parse().ok()?, switch))
            });
            SavedTab::Claude { session, name, mux, account, guard }
        },
        "shell" => {
            let name = next();
            SavedTab::Shell { name, mux: next().and_then(|m| m.parse().ok()) }
        },
        "file" => SavedTab::File(next()?.into()),
        "diff" => SavedTab::Diff(next()?.into()),
        "scratch" => SavedTab::Scratch(next()?.into()),
        "commit_diff" => SavedTab::CommitDiff { repo: next()?.into(), hash: next()?, rel: next()?, from: next() },
        "git_log" => SavedTab::GitLog,
        _ => return None,
    })
}

/// The folder a terminal line of a one-workspace build's session starts in
/// (its field after `mux`).
fn tab_cwd(line: &str) -> Option<PathBuf> {
    let at = match line.split('\t').next()? {
        "claude" => 4,
        "shell" => 3,
        _ => return None,
    };
    line.split('\t').nth(at).filter(|s| !s.is_empty()).map(PathBuf::from)
}

/// What a one-workspace build's session adds to a workspace: its further
/// folders and each tab's folder.
#[derive(Default)]
struct Merged {
    folders: Vec<PathBuf>,
    cwds: Vec<Option<PathBuf>>,
}

/// Split a one-workspace build's workspace back into one per folder: each
/// tab goes to the folder it runs in (a terminal) or lies in (a file or
/// diff), else to the first. Tab groups follow their tabs; a split center
/// does not come back. Also returns the index of the one with the active tab.
fn split_merged(ws: SavedWorkspace, merged: Merged) -> (Vec<SavedWorkspace>, usize) {
    let mut out: Vec<SavedWorkspace> = std::iter::once(ws.folder.clone()).chain(merged.folders).map(SavedWorkspace::fresh).collect();
    let folders: Vec<PathBuf> = out.iter().map(|w| w.folder.clone()).collect();
    let home = |p: &std::path::Path| {
        (0..folders.len()).filter(|&i| p.starts_with(&folders[i])).max_by_key(|&i| folders[i].components().count()).unwrap_or(0)
    };
    let mut active_ws = 0;
    for (i, tab) in ws.tabs.into_iter().enumerate() {
        let place = match &tab {
            SavedTab::Claude { .. } | SavedTab::Shell { .. } => merged.cwds.get(i).cloned().flatten().map_or(0, |c| home(&c)),
            SavedTab::File(p) | SavedTab::Diff(p) | SavedTab::CommitDiff { repo: p, .. } => home(p),
            SavedTab::Scratch(_) | SavedTab::GitLog => 0,
        };
        let to = &mut out[place];
        if i == ws.active {
            (active_ws, to.active) = (place, to.tabs.len());
        }
        let group = ws.tab_groups.get(i).copied().flatten().and_then(|g| {
            let saved = ws.groups.get(g)?;
            Some(to.groups.iter().position(|have| have == saved).unwrap_or_else(|| {
                to.groups.push(saved.clone());
                to.groups.len() - 1
            }))
        });
        to.tabs.push(tab);
        to.tab_groups.push(group);
    }
    (out, active_ws)
}

/// A panel line's fields: `hidden` or `arrange`, at the top or (older
/// builds) in a workspace.
fn decode_panel_line(line: &str, panels: &mut SavedPanels) -> bool {
    if let Some(h) = line.strip_prefix("hidden\t") {
        panels.hidden = h.split_whitespace().map(str::to_owned).collect();
    } else if let Some(a) = line.strip_prefix("arrange\t") {
        if let Some((key, encoded)) = a.split_once('\t') {
            panels.arrangements.push((key.to_owned(), encoded.to_owned()));
        }
    } else {
        return false;
    }
    true
}

fn decode_session(text: &str) -> (Vec<SavedWindow>, SavedPanels) {
    let mut out: Vec<SavedWindow> = vec![];
    let mut panels = SavedPanels::default();
    let mut shared = false;
    // An older build's panels, per workspace of the first window: its
    // active workspace's are taken.
    let mut per_ws: Vec<SavedPanels> = vec![];
    // Per window, per workspace: what a one-workspace build added to it.
    let mut merged: Vec<Vec<Merged>> = vec![];
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("\t\t") {
            let first_window = out.len() == 1;
            let Some(ws) = out.last_mut().and_then(|w| w.workspaces.last_mut()) else { continue };
            let Some(m) = merged.last_mut().and_then(|w| w.last_mut()) else { continue };
            if let Some(f) = rest.strip_prefix("folder\t") {
                m.folders.push(PathBuf::from(f));
            } else if let Some(n) = rest.strip_prefix("active\t") {
                ws.active = n.parse().unwrap_or(0);
            } else if let Some(p) = rest.strip_prefix("panes\t") {
                let (layout, active) = p.split_once('\t').unwrap_or((p, ""));
                ws.panes = layout.to_owned();
                ws.pane_active = active.split_whitespace().map(|a| a.parse().ok()).collect();
            } else if rest.starts_with("hidden\t") || rest.starts_with("arrange\t") {
                if first_window && let Some(p) = per_ws.last_mut() {
                    decode_panel_line(rest, p);
                }
            } else if let Some(p) = rest.strip_prefix("tab-panes\t") {
                ws.tab_panes = p.split_whitespace().map(|n| n.parse().unwrap_or(0)).collect();
            } else if let Some(g) = rest.strip_prefix("group\t") {
                let mut parts = g.splitn(3, '\t');
                let color = parts.next().unwrap_or_default().to_owned();
                let collapsed = parts.next() == Some("1");
                ws.groups.push(SavedGroup { name: parts.next().unwrap_or_default().to_owned(), color, collapsed });
            } else {
                let (group, rest) = match rest.strip_prefix('@').and_then(|r| r.split_once('\t')) {
                    Some((n, rest)) => (n.parse().ok(), rest),
                    None => (None, rest),
                };
                if let Some(tab) = decode_tab(rest) {
                    ws.tabs.push(tab);
                    ws.tab_groups.push(group);
                    m.cwds.push(tab_cwd(rest));
                }
            }
        } else if let Some(folder) = line.strip_prefix('\t') {
            if let (Some(w), Some(m)) = (out.last_mut(), merged.last_mut()) {
                w.workspaces.push(SavedWorkspace::fresh(PathBuf::from(folder)));
                m.push(Merged::default());
                if out.len() == 1 {
                    per_ws.push(SavedPanels::default());
                }
            }
        } else if let Some(rest) = line.strip_prefix("window ") {
            let (active, placement) = rest.split_once(' ').unwrap_or((rest, ""));
            out.push(SavedWindow { placement: decode(placement), workspaces: vec![], active: active.parse().unwrap_or(0) });
            merged.push(vec![]);
        } else if out.is_empty() && decode_panel_line(line, &mut panels) {
            shared = true;
        }
    }
    if !shared && let Some(first) = out.first() {
        panels = per_ws.get(first.active).or(per_ws.first()).cloned().unwrap_or_default();
    }
    for (w, merged) in out.iter_mut().zip(merged) {
        if merged.iter().all(|m| m.folders.is_empty()) {
            continue;
        }
        let (mut split, mut active) = (vec![], 0);
        for (i, (ws, m)) in std::mem::take(&mut w.workspaces).into_iter().zip(merged).enumerate() {
            let at = split.len();
            let (list, ix) = if m.folders.is_empty() { (vec![ws], 0) } else { split_merged(ws, m) };
            if i == w.active {
                active = at + ix;
            }
            split.extend(list);
        }
        (w.workspaces, w.active) = (split, active);
    }
    (out, panels)
}

/// The session text last read or written, so an unchanged session is not
/// written again (a tab's status changing asks for a save, too).
static LAST_SESSION: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn save_session(windows: &[SavedWindow], panels: &SavedPanels) {
    let text = encode_session(windows, panels);
    let mut last = LAST_SESSION.lock().unwrap_or_else(|e| e.into_inner());
    if last.as_deref() == Some(text.as_str()) {
        return;
    }
    if write(&session_file(), &text) {
        *last = Some(text);
    }
}

/// The last session's windows, keeping only folders that still exist, and
/// its side panels.
pub fn load_session() -> (Vec<SavedWindow>, SavedPanels) {
    let text = std::fs::read_to_string(session_file()).unwrap_or_default();
    *LAST_SESSION.lock().unwrap_or_else(|e| e.into_inner()) = Some(text.clone());
    let (mut windows, panels) = decode_session(&text);
    for w in &mut windows {
        let before = w.workspaces.len();
        w.workspaces.retain(|ws| ws.folder.is_dir());
        if w.workspaces.len() != before {
            w.active = 0;
        }
    }
    (windows, panels)
}

#[cfg(test)]
mod tests {
    use super::{
        SavedGroup, SavedPanels, SavedTab, SavedWindow, SavedWorkspace, decode, decode_session, encode, encode_session, has_flag,
        toggle_flag,
    };
    use gpui_kit::{Bounds, WindowBounds, point, px, size};
    use std::path::PathBuf;

    #[test]
    #[cfg(unix)]
    fn atomic_write_replaces_and_keeps_mode() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("tern-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("f.txt");
        super::write_atomic(&p, b"one").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        super::write_atomic(&p, b"two").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "two");
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn round_trip() {
        let b = Bounds::new(point(px(-40.), px(25.5)), size(px(1440.), px(900.)));
        let line = encode("ABC-123", WindowBounds::Maximized(b));
        let (display, bounds) = decode(&line).unwrap();
        assert_eq!(display, "ABC-123");
        assert_eq!(bounds, WindowBounds::Maximized(b));
    }

    #[test]
    fn session_round_trip() {
        let b = Bounds::new(point(px(10.), px(20.)), size(px(1200.), px(800.)));
        let windows = vec![
            SavedWindow {
                placement: Some(("ABC".into(), WindowBounds::Windowed(b))),
                workspaces: vec![
                    SavedWorkspace {
                        folder: PathBuf::from("/a b/c"),
                        tabs: vec![
                            SavedTab::Claude { name: None, session: Some("s-1".into()), mux: Some(42), account: None, guard: None },
                            SavedTab::Claude {
                                name: Some("api work".into()),
                                session: None,
                                mux: None,
                                account: Some("account-2".into()),
                                guard: None,
                            },
                            SavedTab::Shell { name: None, mux: Some(7) },
                            SavedTab::File(PathBuf::from("/a b/c/x y.rs")),
                            SavedTab::Diff(PathBuf::from("/a b/c/d.rs")),
                            SavedTab::Scratch(PathBuf::from("/s/n.txt")),
                            SavedTab::CommitDiff { repo: "/a b".into(), hash: "abc".into(), rel: "c/d e.rs".into(), from: None },
                            SavedTab::CommitDiff {
                                repo: "/r".into(),
                                hash: "def".into(),
                                rel: "new.rs".into(),
                                from: Some("old.rs".into()),
                            },
                            SavedTab::GitLog,
                            SavedTab::Claude { name: None, session: Some("s-2".into()), mux: None, account: None, guard: Some((50, true)) },
                            SavedTab::Claude {
                                name: None,
                                session: None,
                                mux: Some(3),
                                account: Some("account-2".into()),
                                guard: Some((70, false)),
                            },
                        ],
                        tab_groups: vec![Some(0), Some(0), None, Some(1), None, None, None, None, None, None, None],
                        groups: vec![
                            SavedGroup { name: "api".into(), color: "blue".into(), collapsed: false },
                            SavedGroup { name: String::new(), color: "red".into(), collapsed: true },
                        ],
                        active: 3,
                        panes: "h(0,v(1,2))".into(),
                        tab_panes: vec![0, 0, 1, 1, 2, 0, 0, 0, 2],
                        pane_active: vec![Some(0), Some(3), None],
                    },
                    SavedWorkspace::fresh(PathBuf::from("/d")),
                ],
                active: 1,
            },
            SavedWindow { placement: None, workspaces: vec![], active: 0 },
        ];
        let panels =
            SavedPanels { hidden: vec!["explorer".into(), "scratches".into()], arrangements: vec![("k".into(), "left=explorer".into())] };
        assert_eq!(decode_session(&encode_session(&windows, &panels)), (windows, panels));
    }

    #[test]
    fn one_workspace_session_splits_by_folder() {
        let text = "window 0\n\t/a\n\t\tfolder\t/b\n\t\tfolder\t/b/sub\n\t\thidden\tchanges\n\t\tactive\t2\n\
                    \t\tgroup\tblue\t0\tapi\n\t\t@0\tclaude\ts-1\t\t42\t/b\n\t\tshell\t\t7\t/a\n\t\t@0\tfile\t/b/sub/x.rs\n\
                    \t\tscratch\t/s/n.txt\n\t\tshell\t\t\t/elsewhere\n";
        let (windows, panels) = decode_session(text);
        let w = &windows[0];
        let folders: Vec<&PathBuf> = w.workspaces.iter().map(|ws| &ws.folder).collect();
        assert_eq!(folders, [&PathBuf::from("/a"), &PathBuf::from("/b"), &PathBuf::from("/b/sub")]);
        let [a, b, sub] = &w.workspaces[..] else { panic!("three workspaces") };
        assert_eq!(
            a.tabs,
            [SavedTab::Shell { name: None, mux: Some(7) }, SavedTab::Scratch("/s/n.txt".into()), SavedTab::Shell { name: None, mux: None }]
        );
        assert_eq!(b.tabs, [SavedTab::Claude { name: None, session: Some("s-1".into()), mux: Some(42), account: None, guard: None }]);
        assert_eq!(sub.tabs, [SavedTab::File("/b/sub/x.rs".into())]);
        assert_eq!((&a.tab_groups[..], &b.tab_groups[..], &sub.tab_groups[..]), (&[None, None, None][..], &[Some(0)][..], &[Some(0)][..]));
        assert!(a.groups.is_empty() && sub.groups[0].name == "api");
        assert_eq!((w.active, sub.active), (2, 0));
        assert!(w.workspaces.iter().all(|ws| ws.panes.is_empty()));
        assert_eq!(panels.hidden, ["changes"]);
    }

    #[test]
    fn panels_are_shared() {
        let panels = SavedPanels {
            hidden: vec![],
            arrangements: vec![("workspace/left+right".to_owned(), "left=explorer;right=[c changes@- scratches@-]".to_owned())],
        };
        let with_tabs = SavedWorkspace {
            tabs: vec![SavedTab::Shell { name: None, mux: None }],
            tab_groups: vec![None],
            ..SavedWorkspace::fresh(PathBuf::from("/a"))
        };
        let windows = vec![SavedWindow { placement: None, workspaces: vec![with_tabs, SavedWorkspace::fresh("/b".into())], active: 0 }];
        let text = encode_session(&windows, &panels);
        assert!(text.starts_with("hidden\t\narrange\t"));
        assert_eq!(decode_session(&text), (windows, panels));
    }

    #[test]
    fn old_session_panels_come_from_the_active_workspace() {
        let (w, panels) = decode_session(
            "window 1\n\t/a\n\t\thidden\texplorer\n\t\tshell\tn\n\t/b\n\t\thidden\tchanges\n\t\tarrange\tk\tleft=explorer\n\
             window 0\n\t/c\n\t\thidden\tshells\n",
        );
        assert_eq!(panels, SavedPanels { hidden: vec!["changes".into()], arrangements: vec![("k".into(), "left=explorer".into())] });
        assert_eq!(w[0].workspaces[0].tabs, vec![SavedTab::Shell { name: Some("n".into()), mux: None }]);
        assert_eq!(decode_session("window 0\n\t/a\n\t\tshell\tn\n").1, SavedPanels::default());
    }

    #[test]
    fn session_from_folder_only_format() {
        let (w, _) = decode_session("window 0\n\t/a\n\t/b\n");
        assert_eq!(w[0].workspaces, vec![SavedWorkspace::fresh("/a".into()), SavedWorkspace::fresh("/b".into())]);
    }

    #[test]
    fn tab_names_cannot_break_lines() {
        let ws = SavedWorkspace {
            folder: PathBuf::from("/a"),
            tabs: vec![SavedTab::Shell { name: Some("x\ty\nz".into()), mux: None }],
            tab_groups: vec![Some(0)],
            groups: vec![SavedGroup { name: "a\tb".into(), color: "grey".into(), collapsed: false }],
            ..SavedWorkspace::fresh(PathBuf::from("/a"))
        };
        let w = SavedWindow { placement: None, workspaces: vec![ws], active: 0 };
        let (back, _) = decode_session(&encode_session(&[w], &SavedPanels::default()));
        assert_eq!(back[0].workspaces[0].tabs, vec![SavedTab::Shell { name: Some("x y z".into()), mux: None }]);
        assert_eq!(back[0].workspaces[0].groups[0].name, "a b");
        assert_eq!(back[0].workspaces[0].tab_groups, vec![Some(0)]);
    }

    #[test]
    fn tabs_from_before_background_sessions() {
        let (w, _) = decode_session("window 0\n\t/a\n\t\tclaude\ts-1\tname\n\t\tshell\t\n\t\tshell\tn\n");
        assert_eq!(
            w[0].workspaces[0].tabs,
            vec![
                SavedTab::Claude { name: Some("name".into()), session: Some("s-1".into()), mux: None, account: None, guard: None },
                SavedTab::Shell { name: None, mux: None },
                SavedTab::Shell { name: Some("n".into()), mux: None },
            ]
        );
    }

    #[test]
    fn flags_toggle() {
        let a = toggle_flag("", "--chrome");
        assert_eq!(a, "--chrome");
        let a = toggle_flag(&a, "--dangerously-skip-permissions");
        assert_eq!(a, "--chrome --dangerously-skip-permissions");
        assert!(has_flag(&a, "--chrome"));
        assert_eq!(toggle_flag(&a, "--chrome"), "--dangerously-skip-permissions");
        assert!(!has_flag("--chromeless", "--chrome"));
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode("").is_none());
        assert!(decode("X 1 2 3").is_none());
        assert!(decode("X 0 0 10 10 windowed").is_none());
        assert!(decode("X 0 0 800 600 sideways").is_none());
        assert!(decode("X NaN 0 800 600 windowed").is_none());
    }
}
