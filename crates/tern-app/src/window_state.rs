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

/// Remember `window`'s placement for the next new window.
pub fn save(window: &Window, cx: &App) {
    let Some((uuid, bounds)) = placement(window, cx) else { return };
    write(&file(), &encode(&uuid, bounds));
}

fn write(path: &std::path::Path, text: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = write_atomic(path, text.as_bytes()) {
        log::warn!("can't save {}: {e}", path.display());
    }
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
/// as new processes, and a Claude tab resumes its conversation.
#[derive(Debug, Clone, PartialEq)]
pub enum SavedTab {
    Claude { name: Option<String>, session: Option<String>, mux: Option<u64> },
    Shell { name: Option<String>, mux: Option<u64> },
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
    /// Side panels the user hid: `explorer`, `changes`, `scratches`.
    pub hidden: Vec<String>,
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
            hidden: vec![],
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

/// `window <active> [<placement as in the window file>]`, then per workspace
/// a tab-indented folder line followed by two-tab-indented lines: `active N`,
/// one per tab group (`group`, color, collapsed, name) and one per tab, its
/// kind and fields separated by tabs, after `@N` when it is in group N.
/// A split center adds `panes`, its layout and each pane's tab (`-` for none),
/// and `tab-panes`, each tab's pane; older builds skip both lines. `hidden`
/// lists the side panels the user hid.
fn encode_session(windows: &[SavedWindow]) -> String {
    let mut out = String::new();
    for w in windows {
        match &w.placement {
            Some((display, bounds)) => out.push_str(&format!("window {} {}", w.active, encode(display, *bounds))),
            None => out.push_str(&format!("window {}\n", w.active)),
        }
        for ws in &w.workspaces {
            out.push_str(&format!("\t{}\n", field(&ws.folder.display().to_string())));
            if !ws.hidden.is_empty() {
                out.push_str(&format!("\t\thidden\t{}\n", field(&ws.hidden.join(" "))));
            }
            if ws.tabs.is_empty() {
                continue;
            }
            out.push_str(&format!("\t\tactive\t{}\n", ws.active));
            let opt = |o: &Option<String>| o.as_deref().map(field).unwrap_or_default();
            let path = |p: &PathBuf| field(&p.display().to_string());
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
                    SavedTab::Claude { name, session, mux } => {
                        format!("claude\t{}\t{}\t{}", opt(session), opt(name), mux.map(|m| m.to_string()).unwrap_or_default())
                    },
                    SavedTab::Shell { name, mux } => format!("shell\t{}\t{}", opt(name), mux.map(|m| m.to_string()).unwrap_or_default()),
                    SavedTab::File(p) => format!("file\t{}", path(p)),
                    SavedTab::Diff(p) => format!("diff\t{}", path(p)),
                    SavedTab::Scratch(p) => format!("scratch\t{}", path(p)),
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
            SavedTab::Claude { session, name, mux: next().and_then(|m| m.parse().ok()) }
        },
        "shell" => {
            let name = next();
            SavedTab::Shell { name, mux: next().and_then(|m| m.parse().ok()) }
        },
        "file" => SavedTab::File(next()?.into()),
        "diff" => SavedTab::Diff(next()?.into()),
        "scratch" => SavedTab::Scratch(next()?.into()),
        _ => return None,
    })
}

fn decode_session(text: &str) -> Vec<SavedWindow> {
    let mut out: Vec<SavedWindow> = vec![];
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("\t\t") {
            let Some(ws) = out.last_mut().and_then(|w| w.workspaces.last_mut()) else { continue };
            if let Some(n) = rest.strip_prefix("active\t") {
                ws.active = n.parse().unwrap_or(0);
            } else if let Some(p) = rest.strip_prefix("panes\t") {
                let (layout, active) = p.split_once('\t').unwrap_or((p, ""));
                ws.panes = layout.to_owned();
                ws.pane_active = active.split_whitespace().map(|a| a.parse().ok()).collect();
            } else if let Some(h) = rest.strip_prefix("hidden\t") {
                ws.hidden = h.split_whitespace().map(str::to_owned).collect();
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
                w.workspaces.push(SavedWorkspace::fresh(PathBuf::from(folder)));
            }
        } else if let Some(rest) = line.strip_prefix("window ") {
            let (active, placement) = rest.split_once(' ').unwrap_or((rest, ""));
            out.push(SavedWindow { placement: decode(placement), workspaces: vec![], active: active.parse().unwrap_or(0) });
        }
    }
    out
}

pub fn save_session(windows: &[SavedWindow]) {
    write(&session_file(), &encode_session(windows));
}

/// The last session's windows, keeping only folders that still exist.
pub fn load_session() -> Vec<SavedWindow> {
    let mut windows = decode_session(&std::fs::read_to_string(session_file()).unwrap_or_default());
    for w in &mut windows {
        let before = w.workspaces.len();
        w.workspaces.retain(|ws| ws.folder.is_dir());
        if w.workspaces.len() != before {
            w.active = 0;
        }
    }
    windows
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
                            SavedTab::Claude { name: None, session: Some("s-1".into()), mux: Some(42) },
                            SavedTab::Claude { name: Some("api work".into()), session: None, mux: None },
                            SavedTab::Shell { name: None, mux: Some(7) },
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
                    },
                    SavedWorkspace { hidden: vec!["changes".into()], ..SavedWorkspace::fresh(PathBuf::from("/d")) },
                ],
                active: 1,
            },
            SavedWindow { placement: None, workspaces: vec![], active: 0 },
        ];
        assert_eq!(decode_session(&encode_session(&windows)), windows);
    }

    #[test]
    fn session_from_folder_only_format() {
        let w = decode_session("window 0\n\t/a\n\t/b\n");
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
        let back = decode_session(&encode_session(&[w]));
        assert_eq!(back[0].workspaces[0].tabs, vec![SavedTab::Shell { name: Some("x y z".into()), mux: None }]);
        assert_eq!(back[0].workspaces[0].groups[0].name, "a b");
        assert_eq!(back[0].workspaces[0].tab_groups, vec![Some(0)]);
    }

    #[test]
    fn tabs_from_before_background_sessions() {
        let w = decode_session("window 0\n\t/a\n\t\tclaude\ts-1\tname\n\t\tshell\t\n\t\tshell\tn\n");
        assert_eq!(
            w[0].workspaces[0].tabs,
            vec![
                SavedTab::Claude { name: Some("name".into()), session: Some("s-1".into()), mux: None },
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
