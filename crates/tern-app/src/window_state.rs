//! Remembers where the window was: display, position, size and whether it was
//! maximized or fullscreen, so the next launch opens in the same place
//! instead of on whichever screen is active. Also the session (the
//! workspace's folders and tabs, reopened at launch) and the recent folders.

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
/// as new processes in their folder (`cwd`), and a Claude tab resumes its
/// conversation.
#[derive(Debug, Clone, PartialEq)]
pub enum SavedTab {
    Claude { name: Option<String>, session: Option<String>, mux: Option<u64>, cwd: Option<PathBuf> },
    Shell { name: Option<String>, mux: Option<u64>, cwd: Option<PathBuf> },
    File(PathBuf),
    Diff(PathBuf),
    Scratch(PathBuf),
}

/// A tab group of a saved workspace.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedGroup {
    pub name: String,
    /// The color's name, e.g. `blue`.
    pub color: String,
    pub collapsed: bool,
}

/// The saved workspace: its folders, tabs and active tab.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SavedWorkspace {
    pub folders: Vec<PathBuf>,
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
    /// Side panels the user hid: `explorer`, `changes`, `scratches`.
    pub hidden: Vec<String>,
    /// Panel arrangements as `(key, Arrangement::encode text)`, kept opaque here.
    pub arrangements: Vec<(String, String)>,
}

impl SavedWorkspace {
    /// A workspace of just `folder`, with the default tabs.
    pub fn fresh(folder: PathBuf) -> Self {
        Self { folders: vec![folder], ..Default::default() }
    }

    /// One workspace from the several of an older build's session (each
    /// one folder), in order: their folders and tabs, each terminal in its
    /// workspace's folder. Panes, panels and the active tab come from
    /// `active`'s (the others' tabs join its first pane).
    pub fn merge(list: Vec<SavedWorkspace>, active: usize) -> SavedWorkspace {
        let mut out = SavedWorkspace::default();
        let main = list.get(active).or(list.first()).cloned().unwrap_or_default();
        out.panes = main.panes.clone();
        out.pane_active = vec![];
        out.hidden = main.hidden.clone();
        out.arrangements = main.arrangements.clone();
        for (i, ws) in list.into_iter().enumerate() {
            let (tab_offset, group_offset) = (out.tabs.len(), out.groups.len());
            let home = ws.folders.first().cloned();
            for f in ws.folders {
                if !out.folders.contains(&f) {
                    out.folders.push(f);
                }
            }
            let is_main = i == active;
            if is_main {
                out.active = tab_offset + ws.active;
                out.pane_active = ws.pane_active.iter().map(|a| a.map(|a| a + tab_offset)).collect();
            }
            let n = ws.tabs.len();
            out.tabs.extend(ws.tabs.into_iter().map(|t| match t {
                SavedTab::Claude { name, session, mux, cwd } => SavedTab::Claude { name, session, mux, cwd: cwd.or_else(|| home.clone()) },
                SavedTab::Shell { name, mux, cwd } => SavedTab::Shell { name, mux, cwd: cwd.or_else(|| home.clone()) },
                other => other,
            }));
            out.tab_groups.extend((0..n).map(|t| ws.tab_groups.get(t).copied().flatten().map(|g| g + group_offset)));
            out.groups.extend(ws.groups);
            if !out.panes.is_empty() {
                out.tab_panes.extend((0..n).map(|t| if is_main { ws.tab_panes.get(t).copied().unwrap_or(0) } else { 0 }));
            }
        }
        if out.active >= out.tabs.len() {
            out.active = 0;
        }
        out
    }
}

/// The session: where the window was and its workspace.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SavedWindow {
    pub placement: Option<(String, WindowBounds)>,
    pub workspace: SavedWorkspace,
}

fn session_file() -> PathBuf {
    file().with_file_name("session")
}

/// A field of a tab line: tabs and newlines would break the format.
fn field(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// `window <active> [<placement as in the window file>]`, then per workspace
/// a tab-indented line with its first folder (empty without one) followed by
/// two-tab-indented lines: `folder` for each further folder, `active N`,
/// one per tab group (`group`, color, collapsed, name) and one per tab, its
/// kind and fields separated by tabs, after `@N` when it is in group N.
/// A split center adds `panes`, its layout and each pane's tab (`-` for none),
/// and `tab-panes`, each tab's pane; older builds skip both lines. `hidden`
/// lists the side panels the user hid; `arrange` is one line per panel
/// arrangement, its key and encoded text (older builds skip it). Builds
/// before the single workspace wrote several windows and workspaces.
fn encode_session(w: &SavedWindow) -> String {
    let mut out = match &w.placement {
        Some((display, bounds)) => format!("window 0 {}", encode(display, *bounds)),
        None => "window 0\n".to_owned(),
    };
    let ws = &w.workspace;
    let path = |p: &PathBuf| field(&p.display().to_string());
    out.push_str(&format!("\t{}\n", ws.folders.first().map(path).unwrap_or_default()));
    for f in ws.folders.iter().skip(1) {
        out.push_str(&format!("\t\tfolder\t{}\n", path(f)));
    }
    if !ws.hidden.is_empty() {
        out.push_str(&format!("\t\thidden\t{}\n", field(&ws.hidden.join(" "))));
    }
    for (key, encoded) in &ws.arrangements {
        out.push_str(&format!("\t\tarrange\t{}\t{}\n", field(key), field(encoded)));
    }
    if ws.tabs.is_empty() {
        return out;
    }
    out.push_str(&format!("\t\tactive\t{}\n", ws.active));
    let opt = |o: &Option<String>| o.as_deref().map(field).unwrap_or_default();
    let mux = |m: &Option<u64>| m.map(|m| m.to_string()).unwrap_or_default();
    let cwd = |c: &Option<PathBuf>| c.as_ref().map(path).unwrap_or_default();
    if !ws.panes.is_empty() {
        let active: Vec<String> = ws.pane_active.iter().map(|a| a.map_or("-".into(), |a| a.to_string())).collect();
        let tab_panes: Vec<String> = ws.tab_panes.iter().map(usize::to_string).collect();
        out.push_str(&format!("\t\tpanes\t{}\t{}\n", field(&ws.panes), active.join(" ")));
        out.push_str(&format!("\t\ttab-panes\t{}\n", tab_panes.join(" ")));
    }
    for g in &ws.groups {
        out.push_str(&format!("\t\tgroup\t{}\t{}\t{}\n", field(&g.color), u8::from(g.collapsed), field(&g.name)));
    }
    for (i, tab) in ws.tabs.iter().enumerate() {
        let group = ws.tab_groups.get(i).copied().flatten().map(|g| format!("@{g}\t")).unwrap_or_default();
        let line = match tab {
            SavedTab::Claude { name, session, mux: m, cwd: c } => {
                format!("claude\t{}\t{}\t{}\t{}", opt(session), opt(name), mux(m), cwd(c))
            },
            SavedTab::Shell { name, mux: m, cwd: c } => format!("shell\t{}\t{}\t{}", opt(name), mux(m), cwd(c)),
            SavedTab::File(p) => format!("file\t{}", path(p)),
            SavedTab::Diff(p) => format!("diff\t{}", path(p)),
            SavedTab::Scratch(p) => format!("scratch\t{}", path(p)),
        };
        out.push_str(&format!("\t\t{group}{line}\n"));
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
            SavedTab::Claude { session, name, mux, cwd: next().map(PathBuf::from) }
        },
        "shell" => {
            let name = next();
            let mux = next().and_then(|m| m.parse().ok());
            SavedTab::Shell { name, mux, cwd: next().map(PathBuf::from) }
        },
        "file" => SavedTab::File(next()?.into()),
        "diff" => SavedTab::Diff(next()?.into()),
        "scratch" => SavedTab::Scratch(next()?.into()),
        _ => return None,
    })
}

/// The session; older builds' windows and workspaces become one.
fn decode_session(text: &str) -> SavedWindow {
    struct Window {
        placement: Option<(String, WindowBounds)>,
        workspaces: Vec<SavedWorkspace>,
        active: usize,
    }
    let mut out: Vec<Window> = vec![];
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("\t\t") {
            let Some(ws) = out.last_mut().and_then(|w| w.workspaces.last_mut()) else { continue };
            if let Some(f) = rest.strip_prefix("folder\t") {
                ws.folders.push(PathBuf::from(f));
            } else if let Some(n) = rest.strip_prefix("active\t") {
                ws.active = n.parse().unwrap_or(0);
            } else if let Some(p) = rest.strip_prefix("panes\t") {
                let (layout, active) = p.split_once('\t').unwrap_or((p, ""));
                ws.panes = layout.to_owned();
                ws.pane_active = active.split_whitespace().map(|a| a.parse().ok()).collect();
            } else if let Some(h) = rest.strip_prefix("hidden\t") {
                ws.hidden = h.split_whitespace().map(str::to_owned).collect();
            } else if let Some(a) = rest.strip_prefix("arrange\t") {
                if let Some((key, encoded)) = a.split_once('\t') {
                    ws.arrangements.push((key.to_owned(), encoded.to_owned()));
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
                }
            }
        } else if let Some(folder) = line.strip_prefix('\t') {
            if let Some(w) = out.last_mut() {
                let folders = if folder.is_empty() { vec![] } else { vec![PathBuf::from(folder)] };
                w.workspaces.push(SavedWorkspace { folders, ..Default::default() });
            }
        } else if let Some(rest) = line.strip_prefix("window ") {
            let (active, placement) = rest.split_once(' ').unwrap_or((rest, ""));
            out.push(Window { placement: decode(placement), workspaces: vec![], active: active.parse().unwrap_or(0) });
        }
    }
    let placement = out.first().and_then(|w| w.placement.clone());
    // The first window's shown workspace leads.
    let active = out.first().map_or(0, |w| w.active.min(w.workspaces.len().saturating_sub(1)));
    let all: Vec<SavedWorkspace> = out.into_iter().flat_map(|w| w.workspaces).collect();
    let workspace = if all.len() == 1 { all.into_iter().next().unwrap_or_default() } else { SavedWorkspace::merge(all, active) };
    SavedWindow { placement, workspace }
}

/// The session text last read or written, so an unchanged session is not
/// written again (a tab's status changing asks for a save, too).
static LAST_SESSION: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn save_session(window: &SavedWindow) {
    let text = encode_session(window);
    let mut last = LAST_SESSION.lock().unwrap_or_else(|e| e.into_inner());
    if last.as_deref() == Some(text.as_str()) {
        return;
    }
    if write(&session_file(), &text) {
        *last = Some(text);
    }
}

/// The last session, keeping only folders that still exist; `None` without
/// one.
pub fn load_session() -> Option<SavedWindow> {
    let text = std::fs::read_to_string(session_file()).ok().filter(|t| !t.trim().is_empty())?;
    *LAST_SESSION.lock().unwrap_or_else(|e| e.into_inner()) = Some(text.clone());
    let mut w = decode_session(&text);
    w.workspace.folders.retain(|f| f.is_dir());
    Some(w)
}

#[cfg(test)]
mod tests {
    use super::{SavedGroup, SavedTab, SavedWindow, SavedWorkspace, decode, decode_session, encode, encode_session, has_flag, toggle_flag};
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

    fn claude(name: Option<&str>, session: Option<&str>, mux: Option<u64>, cwd: Option<&str>) -> SavedTab {
        SavedTab::Claude { name: name.map(Into::into), session: session.map(Into::into), mux, cwd: cwd.map(PathBuf::from) }
    }

    fn shell(name: Option<&str>, mux: Option<u64>, cwd: Option<&str>) -> SavedTab {
        SavedTab::Shell { name: name.map(Into::into), mux, cwd: cwd.map(PathBuf::from) }
    }

    #[test]
    fn session_round_trip() {
        let b = Bounds::new(point(px(10.), px(20.)), size(px(1200.), px(800.)));
        let window = SavedWindow {
            placement: Some(("ABC".into(), WindowBounds::Windowed(b))),
            workspace: SavedWorkspace {
                folders: vec![PathBuf::from("/a b/c"), PathBuf::from("/d")],
                tabs: vec![
                    claude(None, Some("s-1"), Some(42), Some("/a b/c")),
                    claude(Some("api work"), None, None, None),
                    shell(None, Some(7), Some("/d")),
                    SavedTab::File(PathBuf::from("/a b/c/x y.rs")),
                    SavedTab::Diff(PathBuf::from("/a b/c/d.rs")),
                    SavedTab::Scratch(PathBuf::from("/s/n.txt")),
                ],
                tab_groups: vec![Some(0), Some(0), None, Some(1), None, None],
                groups: vec![
                    SavedGroup { name: "api".into(), color: "blue".into(), collapsed: false },
                    SavedGroup { name: String::new(), color: "red".into(), collapsed: true },
                ],
                active: 3,
                panes: "h(0,v(1,2))".into(),
                tab_panes: vec![0, 0, 1, 1, 2, 0],
                pane_active: vec![Some(0), Some(3), None],
                hidden: vec!["explorer".into(), "scratches".into()],
                arrangements: vec![("k".into(), "left=explorer".into())],
            },
        };
        assert_eq!(decode_session(&encode_session(&window)), window);
    }

    #[test]
    fn workspace_without_folders_round_trips() {
        let window = SavedWindow {
            placement: None,
            workspace: SavedWorkspace { tabs: vec![shell(None, Some(3), Some("/x"))], tab_groups: vec![None], ..Default::default() },
        };
        assert_eq!(decode_session(&encode_session(&window)), window);
        assert_eq!(decode_session(&encode_session(&SavedWindow::default())), SavedWindow::default());
    }

    #[test]
    fn arrangements_round_trip() {
        let arrangements = vec![("workspace/left+right".to_owned(), "left=explorer;right=[c changes@- scratches@-]".to_owned())];
        let with_tabs = SavedWorkspace {
            tabs: vec![shell(None, None, None)],
            tab_groups: vec![None],
            tab_panes: vec![],
            arrangements: arrangements.clone(),
            ..SavedWorkspace::fresh(PathBuf::from("/a"))
        };
        let window = SavedWindow { placement: None, workspace: with_tabs };
        assert_eq!(decode_session(&encode_session(&window)), window);
        let without_tabs =
            SavedWindow { placement: None, workspace: SavedWorkspace { arrangements, ..SavedWorkspace::fresh("/b".into()) } };
        assert_eq!(decode_session(&encode_session(&without_tabs)), without_tabs);
    }

    #[test]
    fn old_session_has_no_arrangement() {
        let w = decode_session("window 0\n\t/a\n\t\thidden\texplorer\n\t\tshell\tn\n");
        assert!(w.workspace.arrangements.is_empty());
        assert_eq!(w.workspace.tabs, vec![shell(Some("n"), None, None)]);
    }

    #[test]
    fn older_workspaces_become_one() {
        // Two windows: the first shows its second workspace.
        let text = "window 1\n\t/a\n\t\tactive\t0\n\t\tshell\tx\t5\n\t/b\n\t\thidden\tchanges\n\t\tactive\t1\n\t\tgroup\tblue\t0\tg\n\t\tclaude\ts\t\t9\n\t\t@0\tfile\t/b/f.rs\n\t\tpanes\th(0,1)\t0 1\n\t\ttab-panes\t0 1\nwindow 0\n\t/c\n\t/a\n";
        let w = decode_session(text).workspace;
        assert_eq!(w.folders, [PathBuf::from("/a"), PathBuf::from("/b"), PathBuf::from("/c")]);
        assert_eq!(
            w.tabs,
            vec![shell(Some("x"), Some(5), Some("/a")), claude(None, Some("s"), Some(9), Some("/b")), SavedTab::File("/b/f.rs".into())]
        );
        assert_eq!(w.tab_groups, vec![None, None, Some(0)]);
        assert_eq!(w.groups.len(), 1);
        // The shown workspace's tab, panes and panels.
        assert_eq!(w.active, 2);
        assert_eq!(w.hidden, ["changes"]);
        assert_eq!(w.panes, "h(0,1)");
        assert_eq!(w.tab_panes, [0, 0, 1]);
        assert_eq!(w.pane_active, [Some(1), Some(2)]);
    }

    #[test]
    fn session_from_folder_only_format() {
        let w = decode_session("window 0\n\t/a\n\t/b\n");
        assert_eq!(w.workspace, SavedWorkspace { folders: vec!["/a".into(), "/b".into()], ..Default::default() });
    }

    #[test]
    fn tab_names_cannot_break_lines() {
        let ws = SavedWorkspace {
            tabs: vec![shell(Some("x\ty\nz"), None, None)],
            tab_groups: vec![Some(0)],
            groups: vec![SavedGroup { name: "a\tb".into(), color: "grey".into(), collapsed: false }],
            ..SavedWorkspace::fresh(PathBuf::from("/a"))
        };
        let back = decode_session(&encode_session(&SavedWindow { placement: None, workspace: ws })).workspace;
        assert_eq!(back.tabs, vec![shell(Some("x y z"), None, None)]);
        assert_eq!(back.groups[0].name, "a b");
        assert_eq!(back.tab_groups, vec![Some(0)]);
    }

    #[test]
    fn tabs_from_before_background_sessions() {
        let w = decode_session("window 0\n\t/a\n\t\tclaude\ts-1\tname\n\t\tshell\t\n\t\tshell\tn\n");
        assert_eq!(
            w.workspace.tabs,
            vec![claude(Some("name"), Some("s-1"), None, None), shell(None, None, None), shell(Some("n"), None, None)]
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
