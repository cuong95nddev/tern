# Agent customization: everything in Tern configurable by an agent

## Intent

The user wants Tern to be as dynamic as possible: when they ask the agent
running in a Tern tab ("make the tab bar smaller", "bind ⌘E to Explorer",
"turn off Usage", "put Changes under Explorer"), the agent can do it, see
that it worked, and undo it.

Much of this already works by editing files: `settings.json`, theme files
and template overrides hot-reload, and Tern writes a README, a schema and
`reference/` for whoever edits them (settings.rs:766). What is missing:

1. Keybindings, palette commands and menus are hard-coded in Rust.
2. There are almost no feature switches.
3. The layout (side panels, panel groups, panes) can only be changed by
   the user's mouse, or by the debug-only `TERN_CONTROL` socket.
4. `tern --mcp` only manages services; an agent cannot validate a
   change, run an action, see the window, or undo.
5. Nothing tells the agent how to do any of this (no skill).

This project closes those five gaps. It does **not** let the agent create
new features (new panels with their own data); that is a later project.

## Goals

- Every keybinding, palette command and menu item comes from data that
  `settings.json` can change, hot-reloaded like the rest.
- A defined set of features can be switched off in `settings.json`.
- An agent can, through `tern --mcp`: read and change settings with
  validation, list and run actions, read and change the layout, and undo
  its changes.
- The Settings window gets a Shortcuts section, so the user can see and
  change keybindings without editing JSON.
- A skill teaches the agent the workflow and which mechanism answers which
  request; Claude tabs Tern starts get it automatically.

## Non-goals

- Agent-made features (panels backed by commands, web panels). Later.
- Editing Tern's Rust source.
- Settings window rows for `commands`, `menus` and `features`.
  `settings.json` and the agent are the way in for those; only shortcuts
  get a Settings section (§1b).
- Screenshots of the Tern window for the agent (user decision). The agent
  checks its work through `tern_settings_get` problems and
  `tern_layout()`, and asks the user to look.
- Customizing the menu bar agent (`tern-menubar`).

## Anti-goals (a failure even if it works)

- The agent channel can type into terminals, send chat messages, click or
  press keys. It must not: that turns any prompt injection into code
  execution. The release control channel has an allowlist (below).
- A bad change leaves Tern unusable. Invalid settings must be refused
  before they are written, and every agent write must be undoable.
- The agent changes things the user cannot see happened. Every agent
  write shows a notice with Undo.
- Palette, menu and keymap drift apart (a palette entry showing a shortcut
  that is not bound). Shortcut labels are derived from the live keymap.

## Constraints

- New or reworked UI goes through the template engine
  (`crates/tern-app/src/template/`, `templates.json`); no hand-built
  `div()` UI (CLAUDE.md).
- No workarounds: fix at the root; check `../refs/` (herdr-gpui, t3,
  zed-style keymaps) for prior art.
- Checks run in the repo's own `target/` with CI's Rust.
- The uncommitted terminal⇄chat switch work (control.rs, palette.rs,
  settings.rs, templates.json, workspace.rs, file_icons.rs) is committed
  on its own before this work starts.

## Design

### 1. Keymap as data

- The default keymap moves out of the `bind_keys` functions
  (main.rs:161-168, workspace.rs:111, terminal_view.rs:74,
  file_tree_view.rs:58, viewer.rs:25, scratch.rs:41, theme_picker.rs:16,
  main_window.rs:28) into a built-in `keymap.json` next to
  `templates.json`, with per-platform entries where they differ today
  (`platform::bind`, the Windows-only Ctrl+C/V bindings in
  terminal_view.rs:95-111).
- `settings.json` gains `keys`: a list of `{ "key": "cmd-e", "action":
  "workspace::ToggleExplorer", "context": "Workspace", "args": {…} }`.
  `"action": null` unbinds the key. User entries apply after the
  built-ins.
- On every settings load: `cx.clear_key_bindings()` then bind built-ins +
  user entries, building actions by name with `cx.build_action` (vendor
  gpui `app.rs:2658`). An unknown action, bad keystroke or bad args is a
  settings problem (shown like other settings errors), and the rest still
  binds.
- `reference/keymap.json` (the effective keymap) is written with the other
  reference files.
- Actions declared `no_json` cannot be bound by name; the ones that today
  have a default key get JSON support or stay built-in only (builder's
  call, listed in the README).
- Prior art: herdr-gpui `crates/herdr-gpui/src/actions.rs` (catalog +
  resolved keymap, so palette, menu bar and keymap cannot drift).

### 1b. Shortcuts in the Settings window

- A new "Shortcuts" section in `settings_window.rs`, built from
  templates (`settings.shortcuts`, `settings.shortcut_row`, …), not
  hand-built `div()`.
- It lists the effective keymap grouped by area (Workspace, Terminal,
  Explorer, …): command title, its key(s), whether it is a default or the
  user's. A filter field searches by title or key.
- Rebind: click a key, press the new keystroke (chords like `cmd-k
  cmd-t` allowed); a key already used in the same context shows the
  conflict and asks to replace. Remove a binding; reset one or all to
  defaults.
- Every change writes `keys` in `settings.json` through the same
  `set_value` path, so it hot-reloads and the agent sees the same data.
- Prior art: herdr-gpui settings window (`refs/herdr-gpui`), Zed's keymap
  editor.

### 2. Palette commands as data

- `COMMANDS` (palette.rs:77) becomes a built-in `commands` list in JSON:
  `{ "title": "Toggle Explorer", "action": "workspace::ToggleExplorer" }`.
  The two `Do::Show` entries become actions (`GoToFile`,
  `SearchInFiles` already exist).
- The `key` column is dropped: the shortcut shown is looked up from the
  live keymap for that action.
- `settings.json` `commands`: `{ "add": [...], "hide": ["title or
  action"] }` (or a full replacement list; builder's call, documented in
  the README and schema).

### 3. Menus as data

- `menus()` (menus.rs:36) becomes a built-in `menus.json`: menus of items
  `{ "title", "action" }`, `{ "separator": true }`, and named dynamic
  items (`{ "dynamic": "open_recent" }`, the menu bar icon toggle, …).
- `settings.json` `menus` replaces a menu by name (same rule as
  templates: whole-menu replacement). Menus are rebuilt on settings load.

### 4. Feature switches

- `settings.json` `features: { "<name>": false }`. Default: everything on.
- Candidate set (builder confirms each exists and finds every entry
  point): `explorer`, `changes`, `git_log`, `scratches`, `system`,
  `usage`, `agents`, `services`, `agent_monitor`, `chat`, `caffeine`,
  `update_check`, `web_control` (phone/web), `context_guard`,
  `search_index`.
- A switched-off feature: its actions do nothing, its palette commands and
  menu items are hidden, its keybindings are not bound, its panel is not
  drawn, and its background work (index, update check, web server…) does
  not start or stops. Templates get `feature_<name>` variables so `when`
  can hide buttons. Switching back on works without a restart where
  practical; where not, the README says a restart is needed.

### 5. Release control channel

- The app listens, in release builds too, on a Unix domain socket in the
  daemon's private folder (`tern_mux::paths::mux_dir()`, mode 0700; AF_UNIX
  on Windows too, as the daemon does with `uds_windows`), separate from the
  debug `TERN_CONTROL`.
- Allowlisted commands only: settings read/write/validate/undo, action
  list/run (actions that type into terminals or send chat are refused),
  layout read and change (the existing `part-move`, `part-center`,
  `part-float`, `part-window`, `part-dock`, `panel-*`, `layout-edit`,
  `layout-reset`, `pane-split/layout/tile/zoom/close`, `tab-layout`,
  `group-*`), `state`, `windows`. Never: `click`, `drag`,
  `key`, `term-*`, `chat-send`, `chat-answer`, `settings-input`.
- The debug socket keeps everything it has today.

- One socket per running app: `agent.sock` in that folder (debug
  builds use `agent-debug.sock`, so a debug and a release app sharing the
  data folder do not collide). A second instance that finds a live socket
  does not take it. `tern --mcp` connects per tool call, so an app
  restarted mid-session is found again; no app → the tool says so.
- `tern_run_action` runs against the active window's active workspace
  (or the window given), dispatched there, not at whatever has focus.
  `tern_actions()` lists only actions that are both on the agent
  allowlist and runnable that way.
- The agent allowlist is an explicit list kept as data next to the
  keymap (not a per-action flag in code), with a test that every name in
  it is a registered action. In it: view toggles, splits/zoom/focus,
  palette and theme picker, layout edit/reset, word wrap. Not in it
  (Claude's call, the user did not pick): opening or closing tabs,
  workspaces or windows, quit, restarting background terminals, anything
  that types or sends.

### 6. MCP tools (`tern --mcp`)

`tern --mcp` connects to the release channel for these (services stay on
the daemon). If the app is not running, these tools say so.

- `tern_settings_get(path?)`: the file value and the effective value.
- `tern_settings_set(path, value)` / `tern_settings_remove(path)`:
  validate the would-be file first (same loader as hot reload); invalid
  → refused with the problems, nothing written. Valid → snapshot, write,
  reload, return any warnings.
- `tern_undo(steps?)` and `tern_history()`: restore settings snapshots.
- `tern_reference(topic)`: README, schema, elements, templates, actions,
  keymap, commands, menus, features: the generated reference files.
- `tern_actions()` / `tern_run_action(name, args?)`.
- `tern_layout()` / `tern_layout_change(op, …)`.

The theme files in `<config>/themes/` are edited by the agent directly
(they are whole files it writes); `tern_settings_set` validates
`settings.json` only, and the skill tells the agent to check
`tern_settings_get` for problems after writing a theme.

### 7. Snapshots and the Undo notice

- `settings.json` is plain JSON (serde, no comments) and `set_value`
  already rewrites it pretty-printed; the MCP writes reuse that path, no
  format-preserving editor.
- Before each agent write, the previous state is saved in one history in
  the data folder (last 50 entries): for settings writes the previous
  `settings.json`, for layout changes the previous arrangement of that
  window (the same `arrange` data the session file keeps). Undo restores
  the latest entry (itself recorded, so undo can be undone). Action runs
  are not undoable; the allowlist keeps them to harmless ones.
- After an agent write (settings or layout), the window shows a notice
  ("Changed by agent: <summary>") with Undo, through a template.
- Security state (web/phone device permissions, accounts) is not in
  `settings.json` and stays out of the agent's reach. The one sensitive
  switch inside it, `features.web_control`, can be turned off by the
  agent but never on: `tern_settings_set` refuses that.

### 8. The skill

- A `tern-customize` skill (SKILL.md + short reference) is built into the
  binary and written to the data folder as a plugin directory at start
  (like the reference files), and passed with `--plugin-dir` next to the
  existing `--mcp-config` to both Claude terminal tabs (workspace.rs:562)
  and chat tabs (`tern-chat/src/launch.rs:93`). `--plugin-dir` adds to the
  user's own plugins, it does not replace them.
- It covers: when to use it; map from request to mechanism (look → theme
  / tokens / elements; structure → templates; keys → `keys`; palette →
  `commands`; menus → `menus`; on/off → `features`; arrangement → layout
  tools); the loop: read reference → change with `tern_settings_set` →
  check `tern_settings_get` problems → ask the user to look →
  `tern_undo` if they dislike it;
  never edit `settings.json` by hand when the tool is available.
- The README gains a section on adding the MCP server and skill to an
  agent outside Tern.

## Left to the builder

- Exact JSON shapes of `commands` and `menus` overrides, within the rules
  above.
- Which feature switches need a restart.
- Order of work; a suggested order: keymap → shortcuts section →
  commands → menus → features → control channel → MCP → snapshots/notice
  → skill.
