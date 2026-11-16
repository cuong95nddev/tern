//! File-type icons for the explorer: a Lucide glyph and tint per file name,
//! plus the asset source that embeds the extra glyphs on top of the
//! component defaults.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::RwLock;

use gpui_kit::assets::IconName;
use gpui_kit::component::{Icon, Theme};
use gpui_kit::*;
use schemars::JsonSchema;
use serde::Deserialize;

gpui_kit::assets::icon_assets!(
    ExtraIcons,
    [
        AppWindow,
        ArrowDown,
        ArrowUp,
        Bell,
        Bot,
        Brain,
        CaseSensitive,
        ChevronsDownUp,
        CircleArrowDown,
        CircleArrowUp,
        CircleCheck,
        CircleDashed,
        Columns2,
        Database,
        Eye,
        FileArchive,
        FileBadge,
        FileBraces,
        FileCode,
        FileCog,
        FileDiff,
        FileImage,
        Image,
        FileKey,
        FileLock,
        FileMusic,
        FilePen,
        FilePlus,
        FileSpreadsheet,
        FileTerminal,
        FileVideoCamera,
        FolderArchive,
        FolderCode,
        FolderGit2,
        FolderOpen,
        FolderPlus,
        GitBranch,
        Globe,
        GripVertical,
        GitCommitHorizontal,
        LayoutGrid,
        ListChecks,
        LogIn,
        PanelTop,
        MessageCircleQuestionMark,
        MessageSquareText,
        Maximize2,
        Regex,
        Rows3,
        Search,
        Server,
        ShieldCheck,
        Smartphone,
        Sparkles,
        SquareArrowOutUpRight,
        SquareTerminal,
        SquareCheck,
        SquareMinus,
        Minimize2,
        Pencil,
        StickyNote,
        TextWrap,
        Trash,
        UserPlus,
        WholeWord,
        Wrench,
    ]
);

/// Component defaults plus [`ExtraIcons`], the agent logos, the
/// screenshot tool's icons and the caffeine cup.
pub struct AppAssets;

/// What the `icons` settings put in place of assets, by asset path.
static OVERRIDES: RwLock<BTreeMap<String, Cow<'static, [u8]>>> = RwLock::new(BTreeMap::new());

/// What the `file_icons` settings give a kind of file, by its key.
static FILE_LOOKS: RwLock<BTreeMap<String, Look>> = RwLock::new(BTreeMap::new());

/// An icon's asset path and a tint, each when the settings give one.
type Look = (Option<SharedString>, Option<Hsla>);

/// A kind of file's look in the `file_icons` settings.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct FileLook {
    /// A bundled icon's name, or a `.svg` file as in `icons`.
    pub icon: Option<String>,
    /// `#rrggbb`, `#rrggbbaa` or a Tailwind name.
    pub color: Option<String>,
}

/// The asset an `icons` key or a bundled name stands for: `folder` is
/// `icons/folder.svg`, a key with a `/` (`agents/claude.svg`) is that path.
fn asset_path(name: &str) -> String {
    if name.contains('/') { name.to_owned() } else { format!("icons/{name}.svg") }
}

/// Replaces the assets `icons` names: each with another bundled icon, or the
/// `.svg` file it names (absolute, `~/…`, or relative to `dir`), and sets the
/// looks of `files`. What can't be read goes in `problems`. Gives the asset
/// paths that now hold something else.
pub fn set_overrides(
    icons: &BTreeMap<String, String>,
    files: &BTreeMap<String, FileLook>,
    dir: &Path,
    problems: &mut Vec<String>,
) -> Vec<String> {
    let read = |to: &str| {
        if to.ends_with(".svg") {
            let file = match (to.strip_prefix("~/"), crate::dirs::home()) {
                (Some(rest), Some(home)) => home.join(rest),
                _ => dir.join(to),
            };
            std::fs::read(&file).map(Cow::Owned).map_err(|e| format!("{}: {e}", file.display()))
        } else {
            base_load(&asset_path(to)).ok().flatten().ok_or_else(|| "no such icon".to_owned())
        }
    };
    let mut out = BTreeMap::new();
    for (key, to) in icons {
        match read(to) {
            Ok(b) => {
                out.insert(asset_path(key), b);
            },
            Err(e) => problems.push(format!("icons.{key}: \"{to}\": {e}")),
        }
    }
    let mut looks = BTreeMap::new();
    for (n, (key, look)) in files.iter().enumerate() {
        let icon = look.icon.as_ref().and_then(|to| match read(to) {
            Ok(b) => {
                let path = format!("icons/file-look-{n}.svg");
                out.insert(path.clone(), b);
                Some(SharedString::from(path))
            },
            Err(e) => {
                problems.push(format!("file_icons.{key}.icon: \"{to}\": {e}"));
                None
            },
        });
        let color = look.color.as_ref().and_then(|c| {
            let color = crate::style::parse_color(c);
            if color.is_none() {
                problems.push(format!("file_icons.{key}.color: can't read \"{c}\""));
            }
            color
        });
        looks.insert(key.to_ascii_lowercase(), (icon, color));
    }
    *FILE_LOOKS.write().unwrap_or_else(|e| e.into_inner()) = looks;
    let mut old = OVERRIDES.write().unwrap_or_else(|e| e.into_inner());
    let changed = old.iter().filter(|(k, v)| out.get(*k) != Some(*v)).map(|(k, _)| k);
    let changed: Vec<String> = changed.chain(out.iter().filter(|(k, v)| old.get(*k) != Some(*v)).map(|(k, _)| k)).cloned().collect();
    *old = out;
    changed
}

/// `icon` and `color` with the first of `keys` the `file_icons` settings hold over them.
fn looked(keys: &[&str], icon: IconName, color: Hsla) -> (Icon, Hsla) {
    let looks = FILE_LOOKS.read().unwrap_or_else(|e| e.into_inner());
    let (path, tint) = keys.iter().find_map(|k| looks.get(*k)).cloned().unwrap_or_default();
    let icon = path.map_or_else(|| Icon::from(icon), |p| Icon::default().path(p));
    (icon, tint.unwrap_or(color))
}

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = OVERRIDES.read().unwrap_or_else(|e| e.into_inner()).get(path) {
            return Ok(Some(bytes.clone()));
        }
        base_load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.extend(crate::agents::asset_paths(path));
        paths.extend(crate::caffeine::asset_paths(path));
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

/// The asset bundled at `path`.
fn base_load(path: &str) -> Result<Option<Cow<'static, [u8]>>> {
    if let Some(bytes) = ExtraIcons.load(path)? {
        return Ok(Some(bytes));
    }
    if let Some(bytes) = crate::agents::asset(path).or_else(|| crate::caffeine::asset(path)) {
        return Ok(Some(Cow::Borrowed(bytes)));
    }
    gpui_kit::assets::Assets.load(path)
}

/// Folder `name`'s icon and tint: the `file_icons` settings' (`name/`, else
/// `/` for every folder) over the bundled ones.
pub fn dir_icon(name: &str, expanded: bool, theme: &Theme) -> (Icon, Hsla) {
    let (icon, color) = bundled_dir_icon(name, expanded, theme);
    looked(&[&format!("{}/", name.to_ascii_lowercase()), "/"], icon, color)
}

/// File `name`'s icon and tint: the `file_icons` settings' (its whole name,
/// else `.ext`) over the bundled ones.
pub fn file_icon(name: &str, theme: &Theme) -> (Icon, Hsla) {
    let (icon, color) = bundled_file_icon(name, theme);
    let lower = name.to_ascii_lowercase();
    let ext = lower.rfind('.').map_or("", |i| &lower[i..]);
    looked(&[&lower, ext], icon, color)
}

fn bundled_dir_icon(name: &str, expanded: bool, theme: &Theme) -> (IconName, Hsla) {
    let icon = match name {
        ".git" => IconName::FolderGit2,
        "node_modules" | "target" | "dist" | "build" | "vendor" => IconName::FolderArchive,
        "src" | "lib" | "crates" | "packages" => IconName::FolderCode,
        _ if expanded => IconName::FolderOpen,
        _ => IconName::Folder,
    };
    let color = match name {
        ".git" => orange(),
        "node_modules" | "target" | "dist" | "build" | "vendor" => theme.muted_foreground,
        _ => theme.blue,
    };
    (icon, color)
}

fn bundled_file_icon(name: &str, theme: &Theme) -> (IconName, Hsla) {
    let lower = name.to_ascii_lowercase();
    let muted = theme.muted_foreground;
    match lower.as_str() {
        "cargo.lock" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml" | "bun.lockb" | "poetry.lock" | "uv.lock" | "go.sum"
        | "gemfile.lock" => return (IconName::FileLock, muted),
        "dockerfile" | "containerfile" | "makefile" | "justfile" | "cmakelists.txt" => {
            return (IconName::FileCog, theme.cyan);
        },
        ".gitignore" | ".gitattributes" | ".gitmodules" | ".dockerignore" => return (IconName::FileCog, orange()),
        ".env" | ".editorconfig" | ".npmrc" => return (IconName::FileCog, muted),
        _ if lower.starts_with("license") || lower.starts_with("copying") => {
            return (IconName::FileBadge, theme.yellow);
        },
        _ if lower.starts_with(".env.") => return (IconName::FileCog, muted),
        _ => {},
    }
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "rs" => (IconName::FileCode, orange()),
        "ts" | "tsx" | "mts" | "cts" => (IconName::FileCode, theme.blue),
        "js" | "jsx" | "mjs" | "cjs" => (IconName::FileCode, theme.yellow),
        "py" | "pyi" => (IconName::FileCode, theme.blue),
        "go" => (IconName::FileCode, theme.cyan),
        "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "m" | "mm" => (IconName::FileCode, theme.blue),
        "java" | "kt" | "kts" | "scala" | "swift" | "rb" | "php" | "lua" | "zig" | "dart" | "ex" | "exs" | "hs" | "ml" | "cs" | "fs"
        | "vue" | "svelte" => (IconName::FileCode, theme.magenta),
        "html" | "htm" | "xml" => (IconName::FileCode, orange()),
        "css" | "scss" | "sass" | "less" => (IconName::FileCode, theme.cyan),
        "sh" | "bash" | "zsh" | "fish" | "ps1" | "bat" | "cmd" => (IconName::FileTerminal, theme.green),
        "json" | "jsonc" | "json5" => (IconName::FileBraces, theme.yellow),
        "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf" | "properties" | "plist" => (IconName::FileCog, muted),
        "lock" => (IconName::FileLock, muted),
        "md" | "mdx" | "markdown" | "rst" | "adoc" => (IconName::FileText, theme.blue),
        "txt" | "log" => (IconName::FileText, muted),
        "pdf" => (IconName::FileText, theme.red),
        "csv" | "tsv" | "xls" | "xlsx" | "ods" => (IconName::FileSpreadsheet, theme.green),
        "diff" | "patch" => (IconName::FileDiff, theme.green),
        "sql" | "db" | "sqlite" | "sqlite3" => (IconName::Database, theme.magenta),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "icns" | "svg" | "avif" | "tiff" => (IconName::FileImage, theme.magenta),
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" => (IconName::FileMusic, theme.red),
        "mp4" | "mov" | "mkv" | "webm" | "avi" => (IconName::FileVideoCamera, theme.red),
        "zip" | "tar" | "gz" | "tgz" | "xz" | "bz2" | "zst" | "7z" | "rar" | "jar" | "dmg" => (IconName::FileArchive, theme.yellow),
        "pem" | "key" | "crt" | "cer" | "pub" | "p12" | "gpg" | "asc" => (IconName::FileKey, theme.yellow),
        _ => (IconName::File, muted),
    }
}

fn orange() -> Hsla {
    rgb(0xe8834d).into()
}

#[cfg(test)]
mod override_tests {
    // Not `super::*`: gpui's `test` attribute would shadow the std one.
    use super::{AppAssets, BTreeMap, FileLook, base_load, set_overrides};
    use gpui_kit::AssetSource as _;

    #[test]
    fn an_icon_takes_another_ones_place_and_a_bad_one_is_a_problem() {
        let dir = std::env::temp_dir();
        let icons = BTreeMap::from([
            ("bot".to_owned(), "folder".to_owned()),
            ("cpu".to_owned(), "no-such-icon".to_owned()),
            ("server".to_owned(), "no-such-file.svg".to_owned()),
        ]);
        let mut problems = vec![];
        let files = BTreeMap::from([(".rs".to_owned(), FileLook { icon: Some("nope".into()), color: Some("nope".into()) })]);
        let changed = set_overrides(&icons, &files, &dir, &mut problems);
        assert_eq!(changed, ["icons/bot.svg"]);
        let folder = base_load("icons/folder.svg").unwrap();
        assert!(folder.is_some());
        assert_eq!(AppAssets.load("icons/bot.svg").unwrap(), folder);
        assert_eq!(problems.len(), 4, "{problems:?}");
        set_overrides(&BTreeMap::new(), &BTreeMap::new(), &dir, &mut vec![]);
        assert_ne!(AppAssets.load("icons/bot.svg").unwrap(), folder);
    }
}
