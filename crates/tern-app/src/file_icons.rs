//! File-type icons for the explorer: a Lucide glyph and tint per file name,
//! plus the asset source that embeds the extra glyphs on top of the
//! component defaults.

use std::borrow::Cow;

use gpui_kit::assets::IconName;
use gpui_kit::component::Theme;
use gpui_kit::*;

gpui_kit::assets::icon_assets!(
    ExtraIcons,
    [
        ChevronsDownUp,
        CircleArrowDown,
        Columns2,
        Database,
        FileArchive,
        FileBadge,
        FileBraces,
        FileCode,
        FileCog,
        FileDiff,
        FileImage,
        FileKey,
        FileLock,
        FileMusic,
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
        LayoutGrid,
        StickyNote,
    ]
);

/// Component defaults plus [`ExtraIcons`] and the agent logos.
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        if let Some(bytes) = crate::agents::asset(path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.extend(crate::agents::asset_paths(path));
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

pub fn dir_icon(name: &str, expanded: bool, theme: &Theme) -> (IconName, Hsla) {
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

pub fn file_icon(name: &str, theme: &Theme) -> (IconName, Hsla) {
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
