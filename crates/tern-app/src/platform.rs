//! What differs between macOS, Windows and Linux in the UI: shortcuts,
//! labels, the default font and shell.

use gpui_kit::*;

/// The file manager's "show this file" item.
pub const REVEAL: &str = if cfg!(target_os = "macos") {
    "Reveal in Finder"
} else if cfg!(windows) {
    "Reveal in File Explorer"
} else {
    "Reveal in File Manager"
};

/// A key binding written with macOS modifiers (`cmd-t`), translated for
/// this platform (see [`translate`]).
pub fn bind<A: Action>(keystrokes: &str, action: A, context: Option<&str>) -> KeyBinding {
    KeyBinding::new(&translate(keystrokes, context, cfg!(target_os = "macos")), action, context)
}

/// Off macOS there is no ⌘, and Ctrl chords belong to the terminal (Ctrl-C,
/// Ctrl-R, …), so bindings that can fire while a terminal has focus (no
/// context, or `Terminal`) move to Ctrl+Shift, as in Windows Terminal:
///
/// | macOS        | Windows / Linux  |
/// |--------------|------------------|
/// | ⌘X           | Ctrl+Shift+X     |
/// | ⌘⇧X          | Ctrl+Alt+Shift+X |
/// | ⌘⌥X          | Ctrl+Alt+X       |
/// | ⌘1…⌘9        | Alt+1…Alt+9      |
/// | ⌃⌘X          | Alt+Shift+X      |
///
/// Bindings of other contexts (the file tree, an editor) just use Ctrl for ⌘,
/// and ⌘⌫ (delete) is the Delete key.
pub fn translate(keystrokes: &str, context: Option<&str>, macos: bool) -> String {
    if macos {
        return keystrokes.to_owned();
    }
    let terminal = context.is_none_or(|c| c == "Terminal");
    keystrokes.split(' ').map(|ks| translate_one(ks, terminal)).collect::<Vec<_>>().join(" ")
}

fn translate_one(keystroke: &str, terminal: bool) -> String {
    // The key is the last part; it may itself be "-".
    let (mods, key) = match keystroke.strip_suffix("--") {
        Some(m) => (m, "-"),
        None => keystroke.rsplit_once('-').unwrap_or(("", keystroke)),
    };
    let has = |m: &str| mods.split('-').any(|x| x == m);
    if !has("cmd") {
        return keystroke.to_owned();
    }
    let (ctrl, alt, shift) = (has("ctrl"), has("alt"), has("shift"));
    if key == "backspace" && !ctrl && !alt && !shift {
        return "delete".to_owned();
    }
    let (ctrl, alt, shift) = if !terminal {
        (true, alt, shift || ctrl)
    } else if ctrl {
        (false, true, true)
    } else if alt {
        (true, true, shift)
    } else if shift {
        (true, true, true)
    } else if key.len() == 1 && key.as_bytes()[0].is_ascii_digit() {
        (false, true, false)
    } else {
        (true, false, true)
    };
    let mut out = String::new();
    for (on, m) in [(ctrl, "ctrl-"), (alt, "alt-"), (shift, "shift-")] {
        if on {
            out.push_str(m);
        }
    }
    out.push_str(key);
    out
}

/// A shortcut label written for macOS (`⌘⇧G`, `⌘⌥← ⌘⌥→`), as this
/// platform's binding reads (`Ctrl+Alt+Shift+G`): see [`translate`].
pub fn shortcut(label: &str) -> String {
    if cfg!(target_os = "macos") {
        return label.to_owned();
    }
    label.split(' ').map(shortcut_one).collect::<Vec<_>>().join(" ")
}

fn shortcut_one(label: &str) -> String {
    let mut mods = Vec::new();
    let mut key = String::new();
    for c in label.chars() {
        match c {
            '⌘' => mods.push("cmd"),
            '⌃' => mods.push("ctrl"),
            '⌥' => mods.push("alt"),
            '⇧' => mods.push("shift"),
            '↩' => key.push_str("enter"),
            '←' => key.push_str("left"),
            '→' => key.push_str("right"),
            '↑' => key.push_str("up"),
            '↓' => key.push_str("down"),
            c => key.extend(c.to_lowercase()),
        }
    }
    mods.push(&key);
    let translated = translate(&mods.join("-"), None, false);
    let (mods, key) = match translated.rsplit_once('-') {
        Some((m, k)) if !k.is_empty() => (m, k),
        _ => ("", translated.as_str()),
    };
    let key = match key {
        "left" => "←".to_owned(),
        "right" => "→".to_owned(),
        "up" => "↑".to_owned(),
        "down" => "↓".to_owned(),
        "enter" => "Enter".to_owned(),
        k => k.to_uppercase(),
    };
    let mods = mods.split('-').filter(|m| !m.is_empty()).map(|m| match m {
        "ctrl" => "Ctrl",
        "alt" => "Alt",
        "shift" => "Shift",
        m => m,
    });
    mods.chain([key.as_str()]).collect::<Vec<_>>().join("+")
}

/// The shell terminals run: `$SHELL` on Unix; on Windows PowerShell 7
/// (`pwsh`) when installed, else Windows PowerShell. On Windows always a full
/// path: CreateProcess looks for a bare name in Tern's current folder first,
/// which may be an untrusted project.
pub fn shell() -> String {
    if cfg!(windows) {
        let pwsh = std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path).filter(|dir| dir.is_absolute()).map(|dir| dir.join("pwsh.exe")).find(|p| p.is_file())
        });
        return pwsh.unwrap_or_else(windows_powershell).to_string_lossy().into_owned();
    }
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| if cfg!(target_os = "macos") { "/bin/zsh" } else { "/bin/bash" }.to_owned())
}

/// Windows PowerShell, which every Windows 10+ has.
pub fn windows_powershell() -> std::path::PathBuf {
    let root = std::env::var_os("SystemRoot").map(std::path::PathBuf::from).filter(|p| p.is_absolute());
    root.unwrap_or_else(|| r"C:\Windows".into()).join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
}

/// Arguments for an interactive shell: a login shell on Unix.
pub fn shell_args() -> Vec<String> {
    vec![if cfg!(windows) { "-NoLogo" } else { "-l" }.to_owned()]
}

/// Arguments for the shell to run `command` (in its own syntax) in place
/// of an interactive session, keeping its exit status.
pub fn shell_command_args(command: &str) -> Vec<String> {
    if cfg!(windows) {
        // The script as one plain argument: the PTY quotes it by the C
        // runtime's rules, which PowerShell undoes. Never -EncodedCommand:
        // antivirus software takes an unsigned app running base64
        // PowerShell for malware and quarantines Tern (see
        // `windows_command_is_plain`).
        vec!["-NoLogo".to_owned(), "-Command".to_owned(), format!("& {command}; exit $LASTEXITCODE")]
    } else {
        vec!["-lic".to_owned(), format!("exec {command}")]
    }
}

/// The terminal font: Menlo on macOS; on Windows Cascadia Mono when installed
/// (Windows 11, Windows Terminal), else Consolas; on Linux the fontconfig
/// monospace alias.
pub fn terminal_font(cx: &App) -> SharedString {
    if cfg!(target_os = "macos") {
        return "Menlo".into();
    }
    let installed = cx.text_system().all_font_names();
    let candidates: &[&str] = if cfg!(windows) {
        &["Cascadia Mono", "Consolas"]
    } else {
        &["JetBrains Mono", "DejaVu Sans Mono", "Liberation Mono", "monospace"]
    };
    let found = candidates.iter().find(|c| installed.iter().any(|f| f == *c));
    found.copied().unwrap_or(candidates[candidates.len() - 1]).to_owned().into()
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use super::shell_command_args;
    use super::{shortcut_one, translate};

    #[test]
    fn translates_shortcuts() {
        let t = |ks: &str, ctx: Option<&str>| translate(ks, ctx, false);
        assert_eq!(t("cmd-t", None), "ctrl-shift-t");
        assert_eq!(t("cmd-shift-t", None), "ctrl-alt-shift-t");
        assert_eq!(t("cmd-alt-right", None), "ctrl-alt-right");
        assert_eq!(t("ctrl-cmd-left", None), "alt-shift-left");
        assert_eq!(t("cmd-1", None), "alt-1");
        assert_eq!(t("cmd-c", Some("Terminal")), "ctrl-shift-c");
        assert_eq!(t("cmd-\\", None), "ctrl-shift-\\");
        assert_eq!(t("cmd-shift-[", None), "ctrl-alt-shift-[");
        assert_eq!(t("shift-pageup", Some("Terminal")), "shift-pageup");
        assert_eq!(t("cmd-s", Some("Viewer")), "ctrl-s");
        assert_eq!(t("cmd-alt-shift-c", Some("FileTree")), "ctrl-alt-shift-c");
        assert_eq!(t("cmd-backspace", Some("FileTree")), "delete");
        assert_eq!(translate("cmd-t", None, true), "cmd-t");
    }

    /// A plain script, not the encoded one antivirus software flags
    /// (Bitdefender quarantined Tern 0.2.0 for it).
    #[test]
    #[cfg(windows)]
    fn windows_command_is_plain() {
        let args = shell_command_args("claude");
        assert_eq!(args, ["-NoLogo", "-Command", "& claude; exit $LASTEXITCODE"]);
    }

    /// The script reaches PowerShell intact (quotes, `$`, spaces) and its
    /// exit status comes back, as the PTY passes it with the same quoting
    /// rules as `Command`.
    #[test]
    #[cfg(windows)]
    fn powershell_runs_the_command_as_written() {
        let ps = super::windows_powershell();
        let out = std::process::Command::new(&ps)
            .arg("-NoProfile")
            .args(shell_command_args(r#"Write-Output 'a b' "c`$d" 'it''s' "e ""f""" ; cmd /c exit 3"#))
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines, ["a b", "c$d", "it's", r#"e "f""#], "{text}");
        assert_eq!(out.status.code(), Some(3));
    }

    #[test]
    fn shortcut_labels() {
        assert_eq!(shortcut_one("⌘⇧G"), "Ctrl+Alt+Shift+G");
        assert_eq!(shortcut_one("⌘B"), "Ctrl+Shift+B");
        assert_eq!(shortcut_one("⌘⌥←"), "Ctrl+Alt+←");
        assert_eq!(shortcut_one("⌘⇧↩"), "Ctrl+Alt+Shift+Enter");
        assert_eq!(shortcut_one("⌘\\"), "Ctrl+Shift+\\");
    }
}

/// Blurs what's behind `window` by `radius` points (0: not at all), where
/// the window is see-through. NSVisualEffectView, which gpui blurs with,
/// has one strength; the window server's own radius, as Ghostty, kitty and
/// Alacritty set theirs, takes any.
#[cfg(target_os = "macos")]
pub fn set_blur_radius(window: &Window, radius: u32) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGSDefaultConnectionForThread() -> i32;
        fn CGSSetWindowBackgroundBlurRadius(connection: i32, window: isize, radius: i32) -> i32;
    }

    let Ok(handle) = HasWindowHandle::window_handle(window) else { return };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return };
    let view = handle.ns_view.as_ptr().cast::<AnyObject>();
    // SAFETY: `view` is this live window's content view; `window` and
    // `windowNumber` are plain NSView / NSWindow getters.
    unsafe {
        let ns_window: *mut AnyObject = msg_send![view, window];
        if ns_window.is_null() {
            return;
        }
        let number: isize = msg_send![ns_window, windowNumber];
        CGSSetWindowBackgroundBlurRadius(CGSDefaultConnectionForThread(), number, radius.min(i32::MAX as u32) as i32);
    }
}
