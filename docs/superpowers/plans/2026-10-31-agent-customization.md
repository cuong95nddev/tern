# Agent Customization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keybindings, palette commands, menus and feature switches become data in `settings.json`; the Settings window gets a Shortcuts section; an agent changes settings and layout through new `tern --mcp` tools over a release-build socket, with undo; a skill teaches it how.

**Architecture:** Built-in defaults move to JSON files compiled in (`keymap.json`, `commands.json`, `menus.json`, `features.json`, `agent_actions.json`, next to `templates.json`). `settings::load` resolves built-ins + user overrides into globals and rebinds keys / rebuilds menus on every hot reload. A new `agent.rs` serves an allowlisted line protocol on `agent.sock`; `mcp.rs` gains tools that talk to it.

**Tech Stack:** Rust, GPUI (vendor/gpui-pre: `clear_key_bindings`, `build_action`, `KeyBinding::load`), serde/schemars, gpui-component, `uds_windows` on Windows.

**Spec:** `docs/superpowers/specs/2026-10-31-agent-customization-design.md`

## Global Constraints

- New UI only through templates (`crates/tern-app/src/template/`, `templates.json`); no hand-built `div()` UI.
- Code and comments in English, matching the surrounding style (short doc comments, plain words).
- Checks run in the repo's own `target/` with `stable` = CI's Rust (`rustc 1.99.1` locally; `rustup update stable` if CI is newer): `cargo fmt --all --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo test --locked --workspace`, and CI's PowerShell grep (`.github/workflows/ci.yml:31-38`).
- No workarounds; prior art in `../refs/` (herdr-gpui `crates/herdr-gpui/src/actions.rs` for keymap/catalog).
- Use `/usr/bin/grep` (the shell's `grep` is aliased).
- Settings errors never stop the app: problems go to `set_error` (status bar), the rest still applies.
- The agent channel never runs: `click`, `drag`, `key`, `term-*`, `chat-send`, `chat-answer`, `settings-input`; never opens/closes tabs, workspaces or windows, quits, or restarts background terminals.
- `features.web_control` may be set to `false` by the agent, never to `true`.
- Each task ends with a commit; commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Do not push.

## Review Focus

1. A user `keys` entry with a typo (unknown action, bad keystroke, bad `args`) — only that entry is dropped, the problem shows in the status bar, all other keys (including gpui-component's input keys) still work. → test in Task 1 (`resolve_keeps_good_entries_and_reports_bad`), manual check in Task 1 step 7.
2. Hot reload rebinding — after a reload, text inputs (palette field, rename field) still have their editing keys (gpui-component bindings captured at startup are re-added). → manual check Task 1 step 7.
3. Off macOS, built-in keys go through `platform::translate`, user keys do not (they write keys for their own platform). → test `user_keys_are_not_translated` in Task 1.
4. An agent `settings-set` that produces an invalid file — nothing written, no snapshot, error returned with the problems. → test `invalid_write_is_refused` in Task 6.
5. App not running / restarted while the MCP server lives — tools answer "Tern is not running" and work again after restart (connect per call). → test `agent_tools_without_app_say_so` in Task 7.

---

### Task 1: Keymap as data

**Files:**
- Create: `crates/tern-app/src/keymap.rs`, `crates/tern-app/src/keymap.json`
- Modify: `crates/tern-app/src/main.rs:156-168` (init order), `workspace.rs:111-149`, `terminal_view.rs:74-113`, `file_tree_view.rs:58-78`, `viewer.rs:25-35`, `scratch.rs:41-43`, `theme_picker.rs:16-20`, `main_window.rs:28-35` (delete each `bind_keys`; move entries to JSON), `settings.rs` (`keys` field, apply in `load`, `reference/keymap.json`, README section)

**Interfaces:**
- Produces:
  - `pub struct KeyEntry { pub key: String, pub action: Option<String>, pub context: Option<String>, pub args: Option<serde_json::Value>, pub platform: Option<String> }` — serde + JsonSchema, `deny_unknown_fields`. `action: None` = unbind (`NoAction`). `platform`: `"macos"`, `"windows"`, `"linux"`, `"!macos"`; absent = all.
  - `pub fn builtin() -> Vec<KeyEntry>` (parses `keymap.json`, `include_str!`).
  - `pub struct Resolved { pub key: String, pub action: Option<String>, pub context: Option<String>, pub args: Option<Value>, pub user: bool }`
  - `pub fn resolve(builtin: &[KeyEntry], user: &[KeyEntry], os: &str) -> Vec<Resolved>` — pure: platform filter, built-in keys through `platform::translate(key, context, os == "macos")`, user keys untouched; user entries appended after built-ins (later wins in GPUI).
  - `pub fn capture_base(cx: &mut App)` — stores the bindings present now (gpui-component's) in a `Base(Vec<KeyBinding>)` global.
  - `pub fn apply(resolved: &[Resolved], cx: &mut App, problems: &mut Vec<String>)` — `cx.clear_key_bindings()`, re-add `Base`, then each entry via `cx.build_action(name, args)` + `KeyBinding::load`; failures push `"keys: <key> → <action>: <error>"` and skip. Stores `Keymap(Vec<Resolved>)` global.
  - `pub fn label(action: &str, cx: &App) -> Option<SharedString>` — the last (winning) key bound to `action` with no context or `Workspace`-level context, formatted as today's labels (`⌘⇧F` on macOS; `Ctrl+Shift+F` elsewhere).
  - `settings::SettingsFile.keys: Vec<keymap::KeyEntry>`.

- [ ] **Step 1: Move every current binding into `keymap.json`** — one object per binding, same keys/contexts/platform conditions as the Rust code today (including the macOS-only readline keys, which must not be translated: give them `"platform": "macos"`; and `"tab"`/`"shift-tab"` → `"action": null` in `Terminal`). Action names are GPUI's namespaced names (`workspace::ToggleExplorer`, `terminal::Copy`, `zed::NoAction` is expressed as `null`).

- [ ] **Step 2: Write failing tests in `keymap.rs`**

```rust
#[test] fn builtin_keymap_reads_cleanly() { assert!(builtin().len() > 50); }
#[test] fn user_entries_come_after_builtins_and_null_unbinds() {
    let b = vec![entry("cmd-b", Some("workspace::ToggleExplorer"), None)];
    let u = vec![entry("cmd-b", None, None)];
    let r = resolve(&b, &u, "macos");
    assert_eq!(r.last().unwrap().action, None);
    assert!(r.last().unwrap().user);
}
#[test] fn platform_filter() {
    let b = vec![KeyEntry { platform: Some("macos".into()), ..entry("cmd-left", Some("terminal::LineStart"), Some("Terminal")) }];
    assert!(resolve(&b, &[], "windows").is_empty());
    assert_eq!(resolve(&b, &[], "macos").len(), 1);
}
#[test] fn builtin_keys_are_translated_off_macos() {
    let b = vec![entry("cmd-b", Some("workspace::ToggleExplorer"), None)];
    assert_eq!(resolve(&b, &[], "windows")[0].key, "ctrl-shift-b");
}
#[test] fn user_keys_are_not_translated() {
    let u = vec![entry("ctrl-e", Some("workspace::ToggleExplorer"), None)];
    assert_eq!(resolve(&[], &u, "windows")[0].key, "ctrl-e");
}
```

(`entry` is a test helper building a `KeyEntry`.) If `platform::translate` is not `pub(crate)` with an explicit `mac: bool`, make it so (it already takes one at `platform.rs:18`).

- [ ] **Step 3: Run** `cargo test -p tern-app keymap` — expect FAIL (module missing).

- [ ] **Step 4: Implement `keymap.rs`** per Interfaces. In `main.rs`, after `gpui_kit::init(cx)`, call `keymap::capture_base(cx)` *before* `settings::init`, and delete the `*::bind_keys(cx)` calls and the `cx.bind_keys([... Quit, OpenFolder])` line (those move to JSON). In `settings::load`, after templates: `keymap::apply(&keymap::resolve(&keymap::builtin(), &settings.keys, std::env::consts::OS), cx, &mut problems)`. In debug builds, `debug_assert!` that the built-in keymap alone applies with no problems (catches a renamed action).

- [ ] **Step 5: `resolve_keeps_good_entries_and_reports_bad`** — `apply` needs an `App`, so split the building into `fn build(r: &Resolved, build_action: impl Fn(&str, Option<Value>) -> Result<Box<dyn Action>, String>) -> Result<KeyBinding, String>` and test it with a closure that fails for `"nope::Nope"`; assert a bad keystroke (`"cmd-"`) and an unknown action each give `Err`, and a good one gives `Ok`.

- [ ] **Step 6: Reference and docs** — `write_reference_files` also writes `reference/keymap.json` (the built-in `keymap.json` text, as `templates.json` is). README (`settings.rs` `readme()`) gets a "Keys" section: shape of an entry, `null` unbinds, contexts that exist (`Terminal`, `Viewer`, `Scratch`, `FileTree && !Input`, none = everywhere), user keys are taken as written on every platform. Regenerated `settings.schema.json` picks `keys` up through schemars.

- [ ] **Step 7: Verify** — `cargo test -p tern-app` PASS; run the debug app (`TERN_CONTROL=/tmp/tc cargo run -p tern-app`), check ⌘B, ⌘T, ⌘P, terminal ⌘C/⌘V, explorer arrows; add `{"key":"cmd-e","action":"workspace::ToggleExplorer"}` to `settings.json` → ⌘E works without restart; add a bad entry → status bar shows the problem and ⌘E still works; type in the palette field after the reload (gpui-component input keys still work).

- [ ] **Step 8: Commit** — `git commit -m "Keys: the keymap is data; settings.json keys rebind and unbind"`

---

### Task 2: Palette and menus from data, labels from the keymap

**Files:**
- Create: `crates/tern-app/src/commands.json`, `crates/tern-app/src/menus.json`
- Modify: `palette.rs:58-115` (`COMMANDS` → data), `menus.rs:34-157`, `settings.rs` (`commands`, `menus` fields; apply in `load`; reference files; README)

**Interfaces:**
- Consumes: `keymap::label(action, cx)` (Task 1).
- Produces:
  - `pub struct CommandEntry { pub title: String, pub action: String, pub args: Option<Value> }` (serde, JsonSchema).
  - `pub struct CommandSettings { pub add: Vec<CommandEntry>, pub hide: Vec<String> }` — `hide` matches a title or an action name.
  - `pub fn commands(builtin: &[CommandEntry], user: &CommandSettings) -> Vec<CommandEntry>` (pure; hidden removed, `add` appended).
  - `#[serde(untagged)] pub enum MenuEntry { Separator { separator: bool }, Dynamic { dynamic: String }, Item { title: String, action: String, args: Option<Value> }, Submenu { title: String, items: Vec<MenuEntry> } }`; built-in `menus.json` is `[{ "title": "File", "items": [...] }, ...]`.
  - `settings::SettingsFile.commands: CommandSettings`, `.menus: BTreeMap<String, Vec<MenuEntry>>` (replace a top-level menu by title, whole-menu, like templates).
  - Globals `Commands(Vec<CommandEntry>)` and `Menus(Vec<(String, Vec<MenuEntry>)>)` set in `settings::load`; `menus::install(cx)` called at the end of `load`.
  - Dynamic menu items: `"open_recent"`, `"menu_bar_icon"` (whatever `menus.rs` builds conditionally today — keep each one's current behavior).
  - Palette: `GoToFile` and `SearchInFiles` actions replace `Do::Show(Mode::Files|Search)` entries; a command's key label is `keymap::label(&cmd.action, cx)`.

- [ ] **Step 1: Failing tests** — in `palette.rs`: `builtin_commands_read_cleanly` (len ≥ 38), `hide_by_title_or_action_and_add_appends` (hide `"Git Log"` and `"workspace::ToggleUsage"`, add one; assert both gone and the added one last). In `menus.rs`: `builtin_menus_read_cleanly` (has `File`, `Edit`, `View`), `user_menu_replaces_by_title`.
- [ ] **Step 2: Run** `cargo test -p tern-app palette menus` — FAIL.
- [ ] **Step 3: Implement.** `menus()` turns `Menus` into GPUI `Menu`/`MenuItem` via `cx.build_action`; an unknown action becomes a settings problem (collected in `load`, so validate there and keep the menu item out). `filter_commands` keeps its ranking over `Commands`.
- [ ] **Step 4: Reference and docs** — `reference/commands.json`, `reference/menus.json`; README sections "Palette commands" and "Menus".
- [ ] **Step 5: Verify** — tests PASS; debug app: palette shows the same commands with the same key labels as before; rebinding ⌘E (Task 1) shows `⌘E` next to Toggle Explorer; hiding a command removes it live; macOS menu bar unchanged by default.
- [ ] **Step 6: Commit** — `"Palette and menus are data; key labels come from the keymap"`

---

### Task 3: Feature switches

**Files:**
- Create: `crates/tern-app/src/features.rs`, `crates/tern-app/src/features.json`
- Modify: `settings.rs` (`features` field), `keymap.rs`/`palette.rs`/`menus.rs` (skip disabled actions), `template/` scope vars, and each feature's entry points and background work (found per feature)

**Interfaces:**
- Produces:
  - `features.json`: `{ "<name>": { "title": "…", "actions": ["workspace::ToggleUsage", …], "parts": ["usage"], "restart": false } }`.
  - `pub struct Features { off: BTreeSet<String> }` global; `pub fn on(name: &str, cx: &App) -> bool`; `pub fn action_on(action: &str, cx: &App) -> bool` (false if any off feature lists it).
  - `settings::SettingsFile.features: BTreeMap<String, bool>`; an unknown name is a problem.
  - Template variables `feature_<name>` (bool) available in every template scope (add once where scopes are rooted, so `when: "feature_usage"` works anywhere).

- [ ] **Step 1: Confirm the set** — for each candidate in the spec (`explorer`, `changes`, `git_log`, `scratches`, `system`, `usage`, `agents`, `services`, `agent_monitor`, `chat`, `caffeine`, `update_check`, `web_control`, `context_guard`, `search_index`), find its actions, side-panel part name, title-bar/menu buttons and background start point; drop any that does not exist and note why in the commit message. Fill `features.json`.
- [ ] **Step 2: Failing tests** — `features.rs`: `builtin_features_read_cleanly`; `unknown_feature_is_a_problem`; `action_off_when_its_feature_is_off`.
- [ ] **Step 3: Implement gating:**
  - keymap `apply` skips entries whose action is off;
  - `commands()` and menu building skip them;
  - the action handlers of an off feature return early (one `if !features::action_on(...)` guard at the handler — no global interception);
  - parts listed are hidden (same path as the user hiding a panel) while off;
  - template buttons for the feature get `"when": "feature_<name>"` in `templates.json`;
  - background work: not started when off at launch; stopped on switch-off where the code has a stop path, else `"restart": true` and the README says a restart applies it.
- [ ] **Step 4: Verify** — tests PASS; debug app: `{"features":{"usage":false}}` hides the Usage panel, its palette entry, its menu item; back to `true` shows them again.
- [ ] **Step 5: Docs** — README "Features" section with the table generated from `features.json` (name, title, needs restart).
- [ ] **Step 6: Commit** — `"Features: switch parts of Tern off in settings.json"`

---

### Task 4: Shortcuts section in the Settings window

**Files:**
- Modify: `settings_window.rs` (new `Section::Shortcuts`, rows, key recorder), `templates.json` (`settings.shortcuts`, `settings.shortcut`, `settings.shortcut.key`), `settings.rs` README template table
- Test: `settings_window.rs` tests

**Interfaces:**
- Consumes: `keymap::Keymap` global (`Vec<Resolved>`), `Commands` (titles), `settings::set_value(&["keys"], …)`.
- Produces:
  - `fn shortcut_rows(keymap: &[Resolved], titles: &BTreeMap<String, String>, filter: &str) -> Vec<ShortcutRow>` (pure) where `ShortcutRow { area: String, action: String, title: String, keys: Vec<String>, user: bool }`. Area from context (`Terminal`, `Viewer`, `Scratch`, `FileTree…` → `Explorer`, none → `Workspace`); title from the command list, else the action name humanized (`workspace::ToggleExplorer` → `Toggle Explorer`). Filter matches title words or the key text.
  - `fn keys_after(file_keys: &[KeyEntry], change: KeyChange) -> Vec<KeyEntry>` (pure) with `enum KeyChange { Bind { action, context, old: Option<String>, new: String }, Remove { action, context, key }, Reset { action }, ResetAll }` — produces the new user `keys` list: `Bind` adds `{new→action}` and, if `old` was a built-in key, `{old→null}`; `Remove` of a built-in key adds `{key→null}`, of a user key removes that entry; `Reset` drops the user's entries for `action`; `ResetAll` empties.
  - `fn conflict(keymap: &[Resolved], context: Option<&str>, key: &str) -> Option<String>` — the action already bound to `key` in the same context.

- [ ] **Step 1: Failing tests** — `rows_group_by_area_and_filter`, `bind_over_builtin_unbinds_old_key`, `remove_user_key_drops_entry`, `reset_action_drops_its_user_entries`, `conflict_in_same_context_only`.
- [ ] **Step 2: Run** — FAIL.
- [ ] **Step 3: Implement** the pure functions, then the section:
  - section entry `(Section::Shortcuts, "Shortcuts", "keyboard", "Keys for every command.")`;
  - templates draw a filter input at the top, a group header per area, one row per action with its key chips and buttons `record`, `remove`, `reset`, plus a page-level `reset_all`;
  - recording: the `record` frame makes a focus handle that captures `on_key_down` and turns the keystroke(s) into GPUI keystroke text; a second keystroke within 1 s makes a chord; Escape cancels;
  - on finish, `conflict` → if any, the row shows "Used by <title>" with Replace / Cancel buttons;
  - every write is `settings::set_value(&["keys"], Some(serde_json::to_value(keys_after(...))))`.
- [ ] **Step 4: Verify** — tests PASS; debug app (`settings shortcuts` via TERN_CONTROL to open it): rebind Toggle Explorer to ⌘E by recording, see it in `settings.json`, ⌘E works, palette label updates; conflict with ⌘T shows the prompt; Reset all restores defaults.
- [ ] **Step 5: Commit** — `"Settings: Shortcuts section to see and change keys"`

---

### Task 5: Agent socket and allowlist

**Files:**
- Create: `crates/tern-app/src/agent.rs`, `crates/tern-app/src/agent_actions.json`
- Modify: `control.rs` (make `serve` usable from `agent.rs`; it stays generic), `main.rs:236-247` (start the agent socket in all builds), `workspace.rs:4805` (split out the layout commands of `control_query` so release builds have them), `main_window.rs:1219` (routing for release builds), `crates/tern-app/Cargo.toml` (`uds_windows` for Windows, the version tern-mux uses)

**Interfaces:**
- Produces:
  - Socket path: `pub fn agent_socket() -> PathBuf` = `tern_mux::paths::mux_dir().join(if cfg!(debug_assertions) { "agent-debug.sock" } else { "agent.sock" })`; dir made with `tern_mux::paths::ensure_private_dir`. If a socket exists and connecting to it succeeds, another app owns it: don't take it (log). If it is stale, replace it (never a non-socket file).
  - Protocol: one JSON request per line → one JSON reply per line. Request `{"cmd": "...", ...}`; reply `{"ok": <value>}` or `{"error": "..."}`.
  - Commands (this task): `{"cmd":"state","window"?:N}`, `{"cmd":"windows"}`, `{"cmd":"actions"}` → `[{"name","title"}]` (allowlisted and registered), `{"cmd":"run","action":"…","args"?:…,"window"?:N}`, `{"cmd":"layout","window"?:N}` → `{"arrangements": [[key, encoded]], "hidden": [...]}`, `{"cmd":"layout-change","op":"part-move", "args":[...],"window"?:N}` with `op` ∈ `part-move part-center part-float part-window part-dock panel-new panel-mode panel-border panel-close layout-edit layout-reset pane-split pane-layout pane-tile pane-zoom pane-close tab-layout group-new group-add group-remove group-move group-toggle group-ungroup group-close group-name group-color` (each forwards to the existing workspace handler with the same arguments it takes as a text command).
  - `agent_actions.json`: list of action names the agent may run (view toggles, splits/zoom/focus-pane, `CommandPalette`, `GoToFile`, `SearchInFiles`, `SelectTheme`, `ToggleEditLayout`, `ResetLayout`, `NewPanel`, `ToggleWordWrap`, `ToggleAgentMonitor`, `NextTab`/`PrevTab`, `NextWorkspace`/`PrevWorkspace`). Not: `NewClaudeTab`, `NewShellTab`, `NewScratch`, `CloseTab`, `CloseWorkspace`, `NewWindow`, `OpenFolder`, `Quit`, `RestartBackground`, anything in `terminal::`/chat.
  - `run` dispatches on the target window (`window` index as in `windows`, default active) with `window.dispatch_action` focused on that window's workspace root focus handle.

- [ ] **Step 1: Failing tests (pure parts)** — `parse_request_rejects_unknown_cmd`, `denied_ops_are_refused` (each of `click`, `key`, `term-type`, `chat-send`, `settings-input` as `cmd` and as `op` → error), `run_refuses_actions_off_the_allowlist` (`workspace::CloseTab` → error), `allowlist_has_no_tab_or_window_openers` (assert none of the "Not" names above appear in `agent_actions.json`).
- [ ] **Step 2: Run** — FAIL.
- [ ] **Step 3: Implement** `agent.rs`: request parsing (pure, tested), the listener (Unix: `UnixListener`; Windows: `uds_windows::UnixListener`, as tern-mux does), dispatch on the main thread like the debug control loop in `main.rs:240`. Move the layout-related branches of `Workspace::control_query` out from under `#[cfg(debug_assertions)]` into `pub fn layout_command(&mut self, op: &str, args: &[&str], window, cx) -> Result<String, String>` used by both the debug socket and `agent.rs`. Add `pub fn layout_snapshot(&self) -> (Vec<(String, String)>, Vec<String>)` (saved arrangements + hidden parts) and `pub fn restore_layout(&mut self, arrangements: &[(String, String)], hidden: &[String], cx)` (rebuild `Kept::new`, set hidden, re-fit, notify) for Task 6.
- [ ] **Step 4: Verify** — tests PASS; release-mode check `cargo build -p tern-app --release` compiles; debug app running, `printf '{"cmd":"actions"}\n' | nc -U "$(…mux dir…)/agent-debug.sock"` lists actions; `{"cmd":"run","action":"workspace::ToggleExplorer"}` toggles it; `{"cmd":"run","action":"workspace::CloseTab"}` → error; `{"cmd":"layout-change","op":"part-move","args":["changes","left","end"]}` moves Changes.
- [ ] **Step 5: Commit** — `"Agent socket: allowlisted actions and layout changes, in release builds too"`

---

### Task 6: Settings writes, history and undo over the socket, with the notice

**Files:**
- Create: `crates/tern-app/src/agent_history.rs`
- Modify: `agent.rs`, `settings.rs` (a validate-without-applying entry point), `templates.json` (`agent_notice`), the window that draws the title bar / status (`main_window.rs` or `workspace.rs`, where `settings_error` is drawn)

**Interfaces:**
- Consumes: `settings::with_value` (settings.rs:317), `Workspace::layout_snapshot/restore_layout` (Task 5).
- Produces:
  - `pub fn settings::check(text: &str, cx: &App) -> Result<Vec<String>, String>` — parse `SettingsFile`, run the same checks as `load` without applying (theme exists, styles, templates compile, keys build, features known); `Err` = cannot load, `Ok(warnings)` = loads.
  - `pub enum Entry { Settings { before: Option<String>, summary: String }, Layout { window: usize, ws: usize, arrangements: Vec<(String,String)>, hidden: Vec<String>, summary: String } }`; `pub struct History` (last 50, stored as JSON lines in `dirs::data()/agent-history`), `push`, `pop`, `list`.
  - Socket commands: `settings-get {"path"?:[..]}` → `{"file": <value at path>, "problems": [...]}`; `settings-set {"path":[..],"value":…}`; `settings-remove {"path":[..]}`; `undo {"steps"?:1}`; `history`.
  - `settings-set` refuses `["features","web_control"]` = `true`, and any path whose result fails `check` (nothing written, nothing pushed). On success: push `Entry::Settings { before }`, write, `load`, show the notice.
  - `layout-change` (Task 5) pushes `Entry::Layout` before applying.
  - `undo` pops and restores; the restore is itself pushed (so undo can be undone).
  - Notice: a global `AgentNotice { summary, until }`; the template `agent_notice` (vars `summary`; button `undo`) is drawn in the title bar area for 10 s; its Undo runs the same `undo`.

- [ ] **Step 1: Failing tests** — `invalid_write_is_refused` (set `["theme"]` to a number → `Err`, file unchanged, history empty), `web_control_cannot_be_turned_on`, `history_keeps_last_50`, `undo_restores_previous_file_and_is_undoable` (pure over a temp dir using `with_value` + `History`).
- [ ] **Step 2: Run** — FAIL.
- [ ] **Step 3: Implement**; reuse `with_value` for the write; `check` shares code with `load` (split `load` into "compute" and "apply" rather than duplicating checks).
- [ ] **Step 4: Verify** — tests PASS; debug app: socket `settings-set ["ui","font_size"] 18` → text grows, notice with Undo shows; Undo → back to before; a layout-change then `undo` restores the panel.
- [ ] **Step 5: Commit** — `"Agent: validated settings writes, layout snapshots, undo and a notice"`

---

### Task 7: MCP tools

**Files:**
- Modify: `crates/tern-app/src/mcp.rs` (tools, `INSTRUCTIONS`), tests in the same file

**Interfaces:**
- Consumes: socket protocol (Tasks 5–6), `agent::agent_socket()`.
- Produces tools (names exact):
  - `tern_settings_get(path?: string[])`, `tern_settings_set(path: string[], value: any)`, `tern_settings_remove(path: string[])`
  - `tern_undo(steps?: integer)`, `tern_history()`
  - `tern_reference(topic: "readme"|"schema"|"elements"|"templates"|"actions"|"keymap"|"commands"|"menus"|"features")` — reads the generated files under `dirs::config()` (README.md, settings.schema.json, reference/*); needs no app.
  - `tern_actions()`, `tern_run_action(action: string, args?: object, window?: integer)`
  - `tern_layout(window?: integer)`, `tern_layout_change(op: string, args: string[], window?: integer)`
  - Each app-backed tool connects to the socket per call; connection refused / missing → tool error `"Tern is not running: open Tern and try again."`
  - `INSTRUCTIONS` gains one sentence: change Tern's look, keys, menus, features and layout with the `tern_*` tools when the user asks; read `tern_reference("readme")` first.

- [ ] **Step 1: Failing tests** — `tools_list_has_agent_tools` (all 11 names present with schemas), `agent_tools_without_app_say_so` (point the socket path at a temp path with no listener via a `Context` field; call `tern_settings_get` → `isError` with that text), `reference_reads_generated_files` (temp config dir with a README → returned).
- [ ] **Step 2: Run** — FAIL.
- [ ] **Step 3: Implement** — `Context` gains `agent_socket: PathBuf`; a small `fn ask(sock, request: Value) -> Result<Value, String>` (connect, write line, read line).
- [ ] **Step 4: Verify** — tests PASS; with the debug app running, `printf` an `initialize` + `tools/call tern_settings_set` sequence into `cargo run -p tern-app -- --mcp` and see the change in the app.
- [ ] **Step 5: Commit** — `"MCP: tools to change Tern's settings, keys, layout and to undo"`

---

### Task 8: The `tern-customize` skill, shipped and loaded

**Files:**
- Create: `crates/tern-app/src/skill/.claude-plugin/plugin.json`, `crates/tern-app/src/skill/skills/tern-customize/SKILL.md`
- Modify: `settings.rs` or a new `skill.rs` (write the plugin dir at start), `workspace.rs:560-565` + its callers at `:1071` (add `--plugin-dir`), `crates/tern-chat/src/launch.rs:66-93` (same for chat tabs), README (using the MCP server and skill from an agent outside Tern: `claude mcp add tern -- <tern> --mcp`, `claude --plugin-dir <data>/agent-plugin`)

**Interfaces:**
- Produces: `pub fn plugin_dir() -> PathBuf` = `dirs::data().join("agent-plugin")`, written at start with `write_if_changed` (files `include_str!`'d); `pub(crate) fn tern_plugin_args() -> Vec<String>` = `["--plugin-dir", <dir>]`, used everywhere `tern_mcp_config()` is.
- `SKILL.md` frontmatter: `name: tern-customize`; `description:` "Use when the user asks to change how Tern looks or behaves: theme, colors, fonts, sizes, panel layout, keyboard shortcuts, palette commands, menus, or turning Tern features on or off." Body (keep under ~150 lines):
  - request → mechanism table (look → `theme`/`colors`/`tokens`/`elements`; structure → `templates`; keys → `keys`; palette → `commands`; menus → `menus`; on/off → `features`; panel placement → `tern_layout_change`; own theme → write `<config>/themes/<name>.json` then `tern_settings_get` for problems);
  - the loop: `tern_reference("readme")` (and the topic's reference) → smallest change with `tern_settings_set` → read the returned problems → tell the user what changed and that Undo is in the notice → `tern_undo` if they dislike it;
  - rules: don't hand-edit `settings.json` when the tools are there; never try to turn `web_control` on; change only what the user asked.

- [ ] **Step 1: Failing test** — `plugin_args_point_at_written_skill` (write into a temp dir, assert `SKILL.md` exists and `plugin.json` parses).
- [ ] **Step 2: Run** — FAIL.
- [ ] **Step 3: Implement**, write `SKILL.md` per the content list above.
- [ ] **Step 4: Verify** — tests PASS; debug app: open a Claude tab, ask "make the UI text bigger" → the agent uses `tern-customize` and `tern_settings_set`; the notice appears; `/skills` lists `tern-customize`.
- [ ] **Step 5: Full check** — `cargo fmt --all --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo test --locked --workspace`, CI grep — all clean.
- [ ] **Step 6: Commit** — `"Skill: tern-customize, loaded into Claude tabs Tern starts"`
