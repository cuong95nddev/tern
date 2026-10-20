# Handoff: command palette + Search in Files (2026-10-20)

## What the user asked

"Look at how T3 Code does the search panel, then support search in Tern." Mid-way: "T3's search, Escape goes back
to the list of actions; do that too." The user opted out of design questions ("làm đi"), so choices below are
mine unless marked as theirs. Talk to the user in Vietnamese; code and comments in English.

## How T3 Code does it (source cloned to the session scratchpad; re-clone `github.com/pingdotgg/t3code` if needed)

- `apps/web/src/components/search/ProjectContentSearchDialog.tsx`: content search is a *mode of the command
  palette* (`CommandPalette.tsx`, modes `command` / `files` / `content`), opened by `mod+shift+f`
  (`projectSearch.toggle`) or the palette item "Search project contents". `mod+p` is the file picker mode.
- Escape in a child mode dispatches `BackToCommand` (`CommandPalette.logic.ts`): back to the command list with
  the earlier query restored; Escape again closes. The footer says "Esc Back".
- UI: field with Aa / ab / .* toggles, status line ("N+ results in M files", spinner, "Invalid regular
  expression"), results grouped by file (sticky header: icon, name, dir, count), rows = line number + syntax
  highlighted line with matches marked, ↑/↓ + Enter, rows rendered 100 at a time.
- Server: `apps/server/src/workspace/WorkspaceSearchIndex.ts`, native `fff` grep; 120ms debounce, limit 500,
  100 matches per file, 250ms time budget, whole word filtered after the grep (not `\b`).

## What is built (uncommitted on `main`, working tree)

- `crates/tern-fs/src/search.rs` (new, `regex` dep added): `search(root, &Query, limit, &cancel) -> Results`.
  Parallel `ignore` walk (gitignore, hidden files included, `.git` skipped), skips binary (NUL in first 8KB) and
  >4MB files, ≤100 lines per file, lines cut at 1000 bytes, byte ranges per match, whole word post-filtered,
  results sorted by path. 6 tests.
- `crates/tern-app/src/palette.rs` (new): `Palette` entity, modes `Commands` / `Search`.
  - Commands: static `COMMANDS` table (title, shortcut label, action), filtered by words, prefix matches first.
    "Search in Files" switches mode inside the palette. Running one emits `PaletteEvent::Run(action)`.
  - Search: debounced 120ms, `LIMIT` 500, cancels the previous search via `AtomicBool`, status line, rows
    `Row::File` / `Row::Match` in a `uniform_list`, line text split into `parts` (indent dropped, long lead cut
    with `…`). Enter/click emits `PaletteEvent::Open { path, line, col }`.
  - Keys: ↑/↓ via `capture_action` of the input's `MoveUp`/`MoveDown`; Enter via `InputEvent::PressEnter`;
    Escape via the input's `Escape` bubbling up (dialog opened with `.keyboard(false)`).
  - `palette::open` shows it in `window.open_dialog` (width = element `palette.dialog_width`).
- `workspace.rs`: actions `CommandPalette` (⌘⇧P) and `SearchInFiles` (⌘⇧F); `palette: Option<(Entity, Sub)>`
  kept per workspace (search survives reopening); `open_palette`; new `open_file_at` (also used by
  `TerminalEvent::OpenPath`).
- `viewer.rs`: `go_to(line, col, window, cx)` applies at once when the file has unsaved edits (no reload
  happens then); after a load, the cursor is set again two frames later, because the editor only scrolls to
  the cursor once it has a layout (a just-opened tab didn't scroll before; this also fixes `OpenPath`).
- Templates (`templates.json`): `palette`, `palette.command`, `palette.file`, `palette.match`. Elements
  (`style.rs`): `palette`, `palette.field`, `palette.option`, `palette.status`, `palette.list`, `palette.row`,
  `palette.file`, `palette.empty`, `palette.footer`. Docs table in `settings.rs`. Icons added to `ExtraIcons`
  (`file_icons.rs`): case-sensitive, whole-word, regex, search. View menu: "Command Palette…", "Search in Files".

Verified: `cargo test -p tern-app -p tern-fs` green (156 + 24), clippy clean on the new code, and visually in
the debug build (palette list, search results, Enter opens at the line and scrolls, Esc back then close).
Not verified by hand: the Aa/ab/.* buttons by click (logic is unit tested in `tern-fs`), a match in a file with
unsaved edits, Windows/Linux.

## Session 2 (same day)

- Search committed as `aa402ad` (only its hunks; `main.rs`/`templates.json` had another session's grip work).
- `⌘P` Go to File, uncommitted: third palette mode `Mode::Files` (also "Go to File…" first in the command
  list, View menu). `tern_fs::search::files` lists files with the same walk as the search (shared `walker`);
  `tern_fs::fuzzy::score` is an fzf-v1 style match (tightest window, word-start and run bonuses, gap penalty,
  a match within the file name wins). The list is read again on every show, filtered off the main thread,
  best 500 shown, ties by shorter path. Template `palette.path` (icon, name and dir with matched chars
  marked). Enter opens as a preview tab; Esc back to the commands.
- Verified: tests green, and in the debug build (list, ranking, Enter opens, Esc back, the command).
- Gotcha: a `TERN_CONTROL` socket under the long scratchpad path silently fails (Unix socket path limit,
  104 bytes), and then the debug app is *not* scripted and may save the real session. Use `target/<x>.sock`.
- Disk is tight (the repo `target` filled the disk once); another session ran `cargo clean`.

## Next steps

1. Ask the user before committing ⌘P.
2. Maybe: recently opened files first with an empty query, `file:line` in Go to File, option keys
   (⌥⌘C / ⌥⌘W / ⌥⌘R) in Search in Files.

## How to see it

`docs/superpowers/handoffs/2026-10-16-theming.md` § "How to see the app": `cargo build -p tern-app`,
`XDG_CONFIG_HOME=<scratch>/xdg scripts/dev-app-macos.sh <folder>`, drive with
`printf 'key cmd-shift-f\n' | nc -U -w 2 target/dev.sock` (typing works one `key <char>` at a time,
`key shift-_` for `_`), capture with the `winid` Swift helper + `screencapture -x -o -l <id>`. The window id
changes on every launch, so pick the big window again.
