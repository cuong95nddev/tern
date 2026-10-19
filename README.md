<div align="center">

<img src="site/assets/icon.png" width="128" height="128" alt="Tern icon">

# Tern

**A fast, native terminal for coding agents, on macOS, Windows and Linux.**

[![Latest release](https://img.shields.io/github/v/release/cuong95nddev/tern?label=release&color=111)](https://github.com/cuong95nddev/tern/releases/latest)
[![Downloads](https://img.shields.io/github/downloads/cuong95nddev/tern/total?color=111)](https://github.com/cuong95nddev/tern/releases)
![macOS 12+](https://img.shields.io/badge/macOS-12%2B-111?logo=apple)
![Universal](https://img.shields.io/badge/Apple%20Silicon%20%2B%20Intel-universal-111)
![Windows 10+](https://img.shields.io/badge/Windows-10%2B-111)
![Linux](https://img.shields.io/badge/Linux-x86__64-111?logo=linux&logoColor=white)

[**Download**](https://github.com/cuong95nddev/tern/releases/latest) · [Website](https://cuong95nddev.github.io/tern/) · [Releases](https://github.com/cuong95nddev/tern/releases)

</div>

---

Tern runs Claude Code, OpenCode, Codex or any other CLI agent next to the code it changes: a file explorer, a viewer with diffs against `HEAD`, and the git changes of every repository under the opened folder, all in one window. It is written in Rust and rendered on the GPU.

## Features

- **Agent and shell tabs** side by side, with workspaces (one folder each) as tabs across the top of the window
- **File explorer** and a light editor that saves, diffs against `HEAD` and renders Markdown
- **Changes panel** listing the git changes of every repo under the folder; click a file to preview its diff
- **Scratch files** that save as you type
- **Session restore**: windows, workspaces and tabs come back on launch, on the display where you left them
- **Screenshots for agents**: <kbd>⌘</kbd><kbd>⇧</kbd><kbd>2</kbd> (<kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>2</kbd> on Windows) anywhere freezes the screen; pick a region, mark it up (boxes, arrows, text, numbered markers, mosaic), collect a few, add a prompt and send them all to an agent tab (or a new Claude Code, Codex or OpenCode tab). On macOS it needs Screen Recording access. Not on Linux yet.
- **Background sessions**: terminals keep running after you quit, and reattach on the next launch
- **Themes and settings** in plain files: colors, fonts and sizes, applied as you save, easy for an agent to edit
- **Automatic updates**: Tern checks for new releases and installs them in place, keeping your session (macOS; on Windows once the installer is code-signed)

## Install

### macOS

1. Download the latest `.dmg` from [Releases](https://github.com/cuong95nddev/tern/releases/latest).
2. Open it and drag **Tern** onto **Applications**.

To open folders from the shell, save this as `~/.local/bin/tern` and make it executable (`chmod +x`):

```sh
#!/bin/sh
dir=$(cd "${1:-.}" && pwd) || exit 1
exec open -a Tern "$dir"
```

Then run `tern ~/code/my-project`. Dropping a folder on the Dock icon works too.

### Windows (10 1809+ / 11, x64)

Run `Tern-x.y.z-windows-x64-setup.exe` from [Releases](https://github.com/cuong95nddev/tern/releases/latest).
It installs for your user without admin rights (into `%LOCALAPPDATA%\Programs\Tern`), and can add the `tern`
command to PATH (`tern C:\code\my-project`) and **Open in Tern** to the Explorer folder menu. The installer isn't
code-signed yet, so SmartScreen asks first (**More info ▸ Run anyway**).

Shell tabs run PowerShell 7 (`pwsh`) when installed, else Windows PowerShell; Claude tabs run `claude` through it.

### Linux (x86_64)

```sh
tar xzf Tern-x.y.z-linux-x86_64.tar.gz
Tern-x.y.z/install.sh          # tern in ~/.local/bin, menu entry and icons; --uninstall removes them
```

Needs a Vulkan driver (Mesa is fine) and X11 or Wayland. Updates aren't installed automatically: Tern points to the release.

## Keyboard shortcuts

On Windows and Linux, Ctrl chords stay with the terminal (Ctrl+C, Ctrl+R, …), so <kbd>⌘</kbd> becomes
<kbd>Ctrl</kbd><kbd>Shift</kbd>, <kbd>⌘</kbd><kbd>⇧</kbd> becomes <kbd>Ctrl</kbd><kbd>Alt</kbd><kbd>Shift</kbd>,
<kbd>⌘</kbd><kbd>⌥</kbd> becomes <kbd>Ctrl</kbd><kbd>Alt</kbd> and <kbd>⌘</kbd><kbd>1</kbd>…<kbd>9</kbd> become
<kbd>Alt</kbd><kbd>1</kbd>…<kbd>9</kbd> (copy / paste: <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>C</kbd> / <kbd>V</kbd>,
also <kbd>Ctrl</kbd><kbd>Insert</kbd> / <kbd>Shift</kbd><kbd>Insert</kbd>; on Windows, as in Windows Terminal,
<kbd>Ctrl</kbd><kbd>C</kbd> copies when there is a selection and <kbd>Ctrl</kbd><kbd>V</kbd> pastes, unless
`terminal.ctrl_v_paste` is `false`; next / previous tab also
<kbd>Ctrl</kbd><kbd>Tab</kbd> / <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>Tab</kbd>). Links open with <kbd>Ctrl</kbd>-click.
Tooltips show each platform's keys.

| Keys (macOS) | Action |
|---|---|
| <kbd>⌘</kbd><kbd>T</kbd> / <kbd>⌘</kbd><kbd>⇧</kbd><kbd>T</kbd> | New Claude tab / shell tab |
| <kbd>⌘</kbd><kbd>O</kbd> | Open a folder as a workspace |
| <kbd>⌘</kbd><kbd>⇧</kbd><kbd>N</kbd> | New window |
| <kbd>⌘</kbd><kbd>N</kbd> | New scratch file |
| <kbd>⌘</kbd><kbd>B</kbd> | Toggle explorer |
| <kbd>⌘</kbd><kbd>⇧</kbd><kbd>G</kbd> | Toggle Changes panel |
| <kbd>⌘</kbd><kbd>⇧</kbd><kbd>D</kbd> | Diff the open file against `HEAD` |
| <kbd>⌘</kbd><kbd>⇧</kbd><kbd>V</kbd> | Toggle Markdown preview |
| <kbd>⌘</kbd><kbd>⇧</kbd><kbd>O</kbd> | Open the file in VS Code or Cursor |
| <kbd>⌘</kbd><kbd>⇧</kbd><kbd>2</kbd> | Capture a screenshot for an agent (works from any app; <kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>2</kbd> on Windows, also **File ▸ Capture Screenshot**) |
| <kbd>⌘</kbd><kbd>⌥</kbd><kbd>←</kbd> / <kbd>⌘</kbd><kbd>⌥</kbd><kbd>→</kbd> | Previous / next workspace |
| <kbd>⌘</kbd><kbd>K</kbd> <kbd>⌘</kbd><kbd>T</kbd> | Pick a theme |
| <kbd>⌘</kbd><kbd>,</kbd> | Open `settings.json` |

## Background sessions

Terminals run in a small background process (`tern --server`, started by Tern), so quitting Tern, an update or a crash doesn't stop them: at the next launch every tab comes back attached to the same program, with its screen and scrollback. While Tern is closed, a Claude turn that finishes or needs approval still posts a notification (macOS). Closing a tab, a workspace or a window ends its programs.

The background process quits by itself once no tab is left. Its log is `daemon.log` in `~/Library/Logs/Tern` (macOS),
`~/.local/state/tern` (Linux) or `%LOCALAPPDATA%\Tern\logs` (Windows). To end it and everything running in it:
`pkill -f 'tern --server'` (macOS, Linux), or end `tern-server-x.y.z-pN.exe` in Task Manager (Windows, where it runs from a copy
in `%LOCALAPPDATA%\Tern\mux`, so updates can replace `tern.exe` while it runs).

On Windows it talks over an AF_UNIX socket and runs programs in ConPTY; who runs in a tab is found from the shell's pid.

On macOS, a menu bar icon lists every terminal running in the background, also while Tern is closed: the count beside it, a dot when one waits for you. Pick one to open it in Tern (in the active workspace, or a new window) or end it. Hide it from its own menu or with Tern ▸ Hide Menu Bar Icon. Its log is `~/Library/Logs/Tern/menubar.log`.

## Themes and settings

Settings live in `~/.config/tern` (macOS, Linux; `XDG_CONFIG_HOME` if set) or `%APPDATA%\Tern` (Windows). Saving a file
there applies it at once; if it can't be read, Tern keeps what it applied last and says why in the status bar.

- `settings.json`: the theme (`"Tern Dark"`, `"Tern Light"` or one of yours), the UI, editor and terminal fonts, UI
  colors over the theme's by key, terminal colors, and the look of each part of the window. Every element (a panel, its
  rows, a bar) has its own properties (height, paddings, font size, colors, a background image); by default they point
  at shared tokens like `$row_height`, so change a token to restyle every element that uses it, or one element's
  property to restyle just that one. `reference/elements.md` lists them all.
- `themes/*.json`: your themes, in [gpui-component](https://github.com/longbridge/gpui-component)'s theme format plus a
  `terminal` palette. `reference/` has the built-in ones to copy from.
- `settings.schema.json` (every key and what it does) and `README.md`, which Tern rewrites on each start.

<kbd>⌘</kbd><kbd>K</kbd> <kbd>⌘</kbd><kbd>T</kbd> picks a theme, showing each as you move through the list;
<kbd>⌘</kbd><kbd>,</kbd> opens `settings.json` in a tab.

Ask an agent in a Tern tab to "make the terminal background navy" and it can do it from those files.

## Updates

Tern checks for a new release at launch and every 6 hours. When one is available, **Update to x.y.z** appears in the workspace bar (also under **Tern ▸ Check for Updates…** on macOS, **Help ▸ Check for Updates…** on Windows and Linux). Set `TERN_NO_UPDATE_CHECK=1` to turn this off.

## Building from source

Requires a recent stable Rust.

```sh
cargo run --release -p tern-app -- /path/to/project
scripts/install-macos.sh      # macOS: /Applications/Tern.app plus a `tern` command in ~/.local/bin
scripts/release-windows.ps1   # Windows: the installer, in target\dist (needs Inno Setup 6)
scripts/release-linux.sh      # Linux: the archive, in target/dist
```

On Linux install GPUI's build dependencies first (Debian/Ubuntu names in `packaging/linux/build-deps.txt`):
`sudo apt-get install $(cat packaging/linux/build-deps.txt)`.

From macOS, `cargo xwin check --target x86_64-pc-windows-msvc --workspace` type-checks the Windows build
([cargo-xwin](https://github.com/rust-cross/cargo-xwin), plus Homebrew `llvm` on PATH for `llvm-rc`).

Crates: `tern-term` (VT core, snapshots), `tern-mux` (background daemon: PTYs, protocol, client), `tern-menubar` (menu bar agent; macOS), `tern-fs` (tree, watcher, git),
`tern-bridge` (Claude Code hook receiver), `tern-app` (UI).

The app icon lives in `assets/icon/source.png`; `scripts/gen-icons.sh` (ImageMagick) regenerates every size from it into `assets/icon/`: macOS `.icns` and an Icon Composer `Tern.icon` (compiled by the install script with Xcode's `actool`, so macOS 26 shows it full-size instead of shrunk into a gray tile), iOS `AppIcon.appiconset` (with dark and tinted looks), Android `res/` (legacy, round, adaptive and monochrome launcher icons, Play Store 512), web favicons/PWA icons, the Windows `tern.ico` (linked into `tern.exe` by `crates/tern-app/build.rs`) and Linux hicolor PNGs.

The website lives in `site/` and is published to GitHub Pages by `.github/workflows/pages.yml`.

### Releasing

Releases are built by `.github/workflows/release.yml` when a `v*` tag matching
the Cargo.toml version is pushed: the macOS DMG, the Windows installer and the Linux archive
(`scripts/release-{macos.sh,windows.ps1,linux.sh}` build the same files locally).
`.github/workflows/ci.yml` runs fmt, clippy and the tests on all three platforms for every pushed branch and every PR.
The workflow signs with Developer ID and notarizes when these repository secrets are set
(otherwise the app is ad-hoc signed and needs `xattr -dr com.apple.quarantine /Applications/Tern.app`):
`MACOS_CERT_P12` (base64 of the exported "Developer ID Application" `.p12`), `MACOS_CERT_PASSWORD`,
`MACOS_SIGN_ID` (the identity's full name), and for notarization an App Store Connect API key:
`NOTARY_KEY` (base64 of the `.p8`), `NOTARY_KEY_ID`, `NOTARY_ISSUER`.

The app finds the latest release anonymously from github.com's `/releases/latest` redirect (not the rate-limited REST API)
and expects each platform's file named as the release scripts name them. On Windows, Tern installs an update itself only
when built with `TERN_WINDOWS_SIGNER` (repository variable `WINDOWS_SIGNER`: the code-signing certificates' SHA-1 thumbprints, comma separated) and the
downloaded installer carries a valid Authenticode signature from one of them; the installer isn't signed yet, so until then Windows
users get the release page. To publish releases to another repo, set the repository
variable `RELEASE_REPO` and the secret `RELEASE_TOKEN` (a fine-grained token with Contents read/write on it).

Windows signing goes through [SignPath](https://signpath.org) (see [Code signing policy](#code-signing-policy)). With the
repository variable `SIGNPATH_ORGANIZATION_ID` and the secret `SIGNPATH_API_TOKEN` set, the release workflow has
SignPath sign `tern.exe`, packs it into the installer and has the installer signed; each request waits for approval
in SignPath. The project there needs the artifact configurations in `packaging/windows/signpath/` (slugs `tern-exe`,
`tern-setup`), project slug `tern` and signing policy `release-signing` (or set `SIGNPATH_PROJECT_SLUG` /
`SIGNPATH_POLICY_SLUG`). Then set `WINDOWS_SIGNER` to the certificate's thumbprint so Tern installs updates itself.

## Reference

Tern reopens the last session: every window with its workspaces, where it was (`~/Library/Application Support/dev.tern/session`). `tern <folder>` adds the folder as a workspace in the running Tern (or launches it with the session restored); so does dropping a folder on the Dock icon. With no session, the window shows a welcome page (Open Workspace…, recent folders), as it does after its last workspace closes.

A window holds several workspaces (one folder each) as tabs in the top bar; the folder button at the end of the bar opens another. Drag a workspace tab out of the window to give it its own window, or onto another Tern window to join it (also in the tab's right-click menu: Move to New Window / Move to Window, Close Other Workspaces). Dragging within the bar reorders.

| Keys | Action |
|---|---|
| ⌘O | Open a folder as a new workspace tab in this window (also the bar's folder button and the + menu); an already open folder is brought forward |
| ⌘⇧W | Close workspace (or the × on its tab; closing its last tab does too) |
| ⌘⌥← ⌘⌥→ | Previous / next workspace |
| ⌘⇧N | New window |
| ⌘T / ⌘⇧T | New Claude tab / shell tab (also in the tab bar's + menu) |
| ⌘N | New scratch file (IntelliJ-style): kept in `~/Library/Application Support/dev.tern/scratches`, saved as you type; other types and reopening old scratches are in the + menu. Closing an empty one deletes it |
| ⌘W, ⌘1–9, ⌘⇧[ ⌘⇧] | Close / switch tabs (or the × on each tab) |
| Right-click a tab | Close / Close Others / Close to the Right / Close File Tabs, Rename… (terminal title or scratch file name), Copy Path, Reveal in Finder |
| Drag a tab onto a pane | Split the editor area like VS Code: drop near an edge to open a new pane on that side, in the middle to move the tab into that pane. A pane closes when its last tab leaves |
| ⌘\\ / ⌘⇧\\ | Split right / down (moves the tab; a pane's only tab gets a new terminal of its kind beside it). Also in the tab's right-click menu: Split Left/Right/Up/Down |
| Pane Layout (grid button, + menu) | Presets for big screens: 2/3/4 columns, 2/3 rows, grids 2×2, 3×2, 4×2, 3×3 (new panes start empty); **Tile All Tabs** gives each tab its own pane; Even Pane Sizes |
| ⌃⌘←→↑↓ | Focus the pane on that side; ⌘1–9, ⌘W and ⌘⇧[ ] act on the focused pane's tabs |
| ⌘⇧↩ | Maximize the focused pane / restore the others |
| ⌘K ⌘T | Pick a theme (also Tern ▸ Theme…): moving through the list shows each one, ↩ saves it in `settings.json`, Esc goes back. In a terminal, ⌘K alone still clears it once the second key doesn't come |
| ⌘, | Open `settings.json` in a tab (also Tern ▸ Settings…) |
| ⌘-hover / ⌘-click in a terminal | Underline / open the link under the mouse: a URL (in the browser), an OSC 8 hyperlink, or a file path that exists (`src/main.rs:42:7` opens it in a tab at that line; relative paths resolve against the folder of the program running in the terminal, e.g. Claude Code's). A folder is revealed in the explorer |
| ⌘B | Toggle explorer (or the panel buttons at the right of the tab bar) |
| click in Changes | Preview the file's diff in a popup; **Open in Tab** moves it to a center tab |
| ⌘⇧G | Toggle Changes panel (git changes of every repo under the folder) |
| ⌘⇧S | Toggle Scratches panel (under the explorer): every scratch file, newest first; click to open, right-click for Copy Path / Reveal in Finder / Delete… |
| ⌘⇧D | Toggle diff vs HEAD for the open file |
| ⌘⇧V | Toggle rendered Markdown preview (`.md` files open in preview; relative links open in the viewer) |
| ⌘⇧O | Open current file in VS Code / Cursor |
| ⌘S | Save file in viewer (scratches save themselves) |
| ⌘C / ⌘V / ⌘K | Copy / paste / clear terminal |

The window reopens on the display, position and size it last had (`~/Library/Application Support/dev.tern/window`).

Debug builds can be driven without the real mouse/keyboard: run with
`TERN_CONTROL=tern.sock` (a short path; Unix sockets cap it at 104 bytes) and
send line commands to that socket — `click X Y` / `rclick X Y` (window coordinates),
`key escape` / `key cmd-w`, `state` (tabs, active tab, dialog open, viewer mode, scratch count or -1 when hidden),
`scratch-new EXT` / `scratch-open NAME` (what the + menu does), `scratch-text TEXT` (replace
the active scratch tab's text; `\n` for newlines), `windows`, `ws`, `ws-open PATH`,
`ws-close [N]`, `ws-activate N`, `ws-reorder FROM TO`, `ws-detach N`, `ws-move N WINDOW`,
`ws-dropout N X Y` (what releasing a dragged workspace tab at window coordinates X Y does),
`new-window`. Commands go to the active window; prefix `@N ` to target window N. Scripted runs
neither restore nor save the session unless `TERN_SESSION=1` (pair it with a scratch `HOME`). Pair with
`screencapture -l <window id>` to see the result.

## Code signing policy

Windows releases are signed once SignPath approves the project: free code signing provided by
[SignPath.io](https://about.signpath.io), certificate by [SignPath Foundation](https://signpath.org).

- Committers and reviewers: [@cuong95nddev](https://github.com/cuong95nddev)
- Approvers: [@cuong95nddev](https://github.com/cuong95nddev)

Only builds made by `.github/workflows/release.yml` from this repository's sources are signed, and every
release is approved by hand before it is.

## Privacy

Tern sends nothing about you or your files anywhere. Its only network request of its own is the update check:
anonymous requests to github.com for this repository's latest release (`TERN_NO_UPDATE_CHECK=1` turns it off).
Programs you run in its terminals, such as Claude Code, follow their own privacy policies.

## Feedback

Found a bug or have an idea? [Open an issue](https://github.com/cuong95nddev/tern/issues). Tern is MIT licensed, see [LICENSE](LICENSE).
