<div align="center">

<img src="site/assets/icon.png" width="128" height="128" alt="Tern icon">

# Tern

**A fast, native macOS terminal for coding agents.**

[![Latest release](https://img.shields.io/github/v/release/cuong95nddev/tern?label=release&color=111)](https://github.com/cuong95nddev/tern/releases/latest)
[![Downloads](https://img.shields.io/github/downloads/cuong95nddev/tern/total?color=111)](https://github.com/cuong95nddev/tern/releases)
![macOS 12+](https://img.shields.io/badge/macOS-12%2B-111?logo=apple)
![Universal](https://img.shields.io/badge/Apple%20Silicon%20%2B%20Intel-universal-111)

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
- **Background sessions**: terminals keep running after you quit, and reattach on the next launch
- **Automatic updates**: Tern checks for new releases and installs them in place, keeping your session

## Install

1. Download the latest `.dmg` from [Releases](https://github.com/cuong95nddev/tern/releases/latest).
2. Open it and drag **Tern** onto **Applications**.

To open folders from the shell, save this as `~/.local/bin/tern` and make it executable (`chmod +x`):

```sh
#!/bin/sh
dir=$(cd "${1:-.}" && pwd) || exit 1
exec open -a Tern "$dir"
```

Then run `tern ~/code/my-project`. Dropping a folder on the Dock icon works too.

## Keyboard shortcuts

| Keys | Action |
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
| <kbd>⌘</kbd><kbd>⌥</kbd><kbd>←</kbd> / <kbd>⌘</kbd><kbd>⌥</kbd><kbd>→</kbd> | Previous / next workspace |

## Background sessions

Terminals run in a small background process (`tern --server`, started by Tern), so quitting Tern, an update or a crash doesn't stop them: at the next launch every tab comes back attached to the same program, with its screen and scrollback. While Tern is closed, a Claude turn that finishes or needs approval still posts a notification. Closing a tab, a workspace or a window ends its programs.

The background process quits by itself once no tab is left. Its log is `~/Library/Logs/Tern/daemon.log`; `pkill -f 'MacOS/tern --server'` ends it and everything running in it.

## Updates

Tern checks for a new release at launch and every 6 hours. When one is available, **Update to x.y.z** appears in the workspace bar (also under **Tern ▸ Check for Updates…**). Set `TERN_NO_UPDATE_CHECK=1` to turn this off.

## Building from source

Requires macOS and a recent stable Rust.

```sh
cargo run --release -p tern-app -- /path/to/project
scripts/install-macos.sh      # /Applications/Tern.app plus a `tern` command in ~/.local/bin
```

Crates: `tern-term` (VT core, snapshots), `tern-mux` (background daemon: PTYs, protocol, client), `tern-fs` (tree, watcher, git),
`tern-bridge` (Claude Code hook receiver), `tern-app` (UI).

The app icon lives in `assets/icon/source.png`; `scripts/gen-icons.sh` (ImageMagick) regenerates every size from it into `assets/icon/`: macOS `.icns` and an Icon Composer `Tern.icon` (compiled by the install script with Xcode's `actool`, so macOS 26 shows it full-size instead of shrunk into a gray tile), iOS `AppIcon.appiconset` (with dark and tinted looks), Android `res/` (legacy, round, adaptive and monochrome launcher icons, Play Store 512) and web favicons/PWA icons.

The website lives in `site/` and is published to GitHub Pages by `.github/workflows/pages.yml`.

### Releasing

Releases are built by `.github/workflows/release.yml` when a `v*` tag matching
the Cargo.toml version is pushed (`scripts/release-macos.sh` builds the same DMG locally).
The workflow signs with Developer ID and notarizes when these repository secrets are set
(otherwise the app is ad-hoc signed and needs `xattr -dr com.apple.quarantine /Applications/Tern.app`):
`MACOS_CERT_P12` (base64 of the exported "Developer ID Application" `.p12`), `MACOS_CERT_PASSWORD`,
`MACOS_SIGN_ID` (the identity's full name), and for notarization an App Store Connect API key:
`NOTARY_KEY` (base64 of the `.p8`), `NOTARY_KEY_ID`, `NOTARY_ISSUER`.

The app finds the latest release anonymously from github.com's `/releases/latest` redirect (not the rate-limited REST API)
and expects the DMG named as `scripts/release-macos.sh` names it. To publish releases to another repo, set the repository
variable `RELEASE_REPO` and the secret `RELEASE_TOKEN` (a fine-grained token with Contents read/write on it).

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

## Feedback

Found a bug or have an idea? [Open an issue](https://github.com/cuong95nddev/tern/issues). Tern is MIT licensed, see [LICENSE](LICENSE).
