# Handoff: LSP navigation (2026-11-02)

## Task

The user asked to do everything OpenIDEA (https://github.com/l0ng-ai/openidea) does for navigating to implementations, natively in Tern ("làm hết luôn, native"): an LSP client for file tabs; Go to Definition through the server with tree-sitter as fallback; Go to Implementation(s) (⌘⌥B, ⌘⌥-click); Super Method (⌘U); and ↓/↑ inheritance gutter icons. The user wants inline execution with no check-ins. Reply to the user in Vietnamese. Code and comments are in English.

- Spec: `docs/superpowers/specs/2026-11-02-lsp-navigation-design.md`
- Plan: `docs/superpowers/plans/2026-11-02-lsp-navigation.md` (10 tasks)
- Ledger: `.superpowers/sdd/2026-11-02-lsp-navigation/progress.md` (git-ignored). It holds every ruling and the Task 8 status, so read it first.
- Executing skill: superpowers:executing-plans. Scripts: `~/.claude/plugins/cache/claude-plugins-official/superpowers/7.0.0/skills/executing-plans/scripts/{task-start,task-done}`.
- OpenIDEA clone (may be gone; re-clone if so): `/private/tmp/claude-501/-Users-cuongpham-ws-tern-ws-tern/5120addd-4223-4bd9-bf7b-d3c87a764094/scratchpad/src/openidea` at commit `8e8a998`.

## Branch

`lsp-navigation`, cut from `main` (the spec and plan were cherry-picked). Not pushed. Tasks 1–7 are complete. Task 8 is in a WIP commit (`1275e49`).

Note: `main` has no `keymap.json`, `commands.json`, `menus.json` or features; those live on the unmerged `agent-customization` branch. So on this branch:
- keys go in `viewer::bind_keys` (`platform::bind` turns cmd into ctrl off macOS);
- commands go in the `palette.rs` `cmd!` list;
- menus go in `menus.rs`.

## Done

1. **Vendored gpui-base 0.7.3** (`vendor/gpui-base`, `[patch]` in the root `Cargo.toml`).
   - New `GutterMarker` API: `EditorState::set_gutter_markers`, `set_gutter_marker_handler`, `set_gutter_marker_tooltip`, `reserve_gutter_markers`. Markers are given by line and follow their line's byte range through edits.
   - `Lsp::alt_click` handles ⌘⌥-click.
   - Re-exported through vendored `gpui-component` (`gpui_kit::component::input`).
   - Described in `vendor/README.md`.
   - gpui-base's own tests can't run here: they need criterion and gpui-fast.
2. **Crate `crates/tern-lsp`:**
   - `config.rs` + `servers.json`: OpenIDEA's default servers, `resolve`, `language_id`, `find_root`, `servers_for`.
   - `text.rs`: UTF-8/16/32 column conversion.
   - `uri.rs`.
   - `rpc.rs`: JSON-RPC over stdio. Dropping a request future sends cancel.
   - `manager.rs`: one server per (server, root); fallback servers; `Missing` and `Failed` events, each with an install hint; initialize timeout of 60 s; restart backoff of 60 s; `Drop` sends shutdown/exit, then kills; `read_text` refuses non-files and files over 20 MB; `absolute_path_entries`; a PATH provider.
   - `requests.rs`: `definition`, `declaration`, `implementation`, `implementations` (batched, 4 in flight), `outline`, `supertypes`, and `impl SuperDeps for Manager`.
   - `nav.rs`: port of OpenIDEA's `lineMarkers.ts` and `structureNav.ts` (`candidates`, `inheritors_marker`, `supers_marker`, `super_method_targets`, `member_at`, `describe`, `super_fallback_kind`).
   - Mock server: `src/bin/tern-lsp-mock.rs`. Tests: `tests/{manager,requests}.rs`.
3. **tern-app wiring:**
   - `settings.rs`: `lsp.gutter_icons` and `lsp.servers`; the `LspConfig` global; `reference/language-servers.json`.
   - `platform::login_path()`.
   - `workspace.rs`: field `lsp: Arc<tern_lsp::Manager>`, `lsp_event` notifications (once per server), `files_changed`.
   - `viewer.rs`: `lsp_sync`/`lsp_hide` keep documents in sync; `Definitions` asks the server first, then tree-sitter.
   - Control command `file-at REL LINE COL`; the viewer's debug state now includes the cursor.
   - Verified with clangd: F12 on `a.area()` in `main.cpp` opens `shape.h:6`. Quitting leaves no orphan clangd.
4. **Task 8 (WIP):**
   - Palette: `Mode::Targets`, `Target`, `filter_targets`, `open_targets`, and the `palette.target` template.
   - Viewer: `GoToImplementation` (⌘⌥B and alt-click), `GoToSuper` (⌘U, with fallback), `show_targets`, `go_to_location`, `targets()`.
   - `ViewerEvent::ChooseTargets` → `Workspace::choose_targets`.
   - Palette commands and View menu items.

## Left

- **Task 8:** e2e on `scratchpad/cppdemo` (C++ with `Shape`/`Circle`/`Square`). ⌘⌥B on `Shape::area` (`file-at shape.h 6 20`) shows "No implementations found" because clangd answered `[]`.
  - Check clangd's background index: wait longer, or open `main.cpp` first. Then check the column.
  - Then test ⌘U in `circle.h` line 7 and screenshot the chooser.
  - Then squash or finish the commit, run `task-done 8`.
- **Task 9:** gutter icons (`crates/tern-app/src/markers.rs`, viewer hooks, icons, theme colors). See the plan.
- **Task 10:** README docs; CI gate (`cargo fmt --all --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo test --locked --workspace`); e2e.
- **Final review:** final whole-branch review (`review-package`, then a code reviewer subagent on the most capable model), then superpowers:finishing-a-development-branch.

## Testing the app

```sh
cargo build -p tern-app
S=/private/tmp/claude-501/-Users-cuongpham-ws-tern-ws-tern/5120addd-4223-4bd9-bf7b-d3c87a764094/scratchpad
XDG_CONFIG_HOME=$S/xdg TERN_SESSION=lsptest scripts/dev-app-macos.sh $S/cppdemo
printf 'file-at shape.h 6 20\n' | nc -U -w 3 target/dev.sock; printf 'key cmd-alt-b\n' | nc -U -w 3 target/dev.sock
$S/bin/winid "Tern Dev"   # then screencapture -x -o -l <id> out.png
pkill -f "Tern Dev.app/Contents/MacOS/tern"
```

If the scratchpad is gone, recreate a demo:
- **C++:** `compile_flags.txt` containing `-std=c++17`, plus headers with a virtual base class and overrides.
- **Rust:** `scratchpad/demo` (trait `Shape`).

## Open questions for the user

1. **Workspace trust:** the security review flagged that servers run project build logic (cargo build scripts, Gradle/Maven import) when a file is opened. Should Tern ask whether to trust a folder before starting servers there? Not decided.
2. **rust-analyzer:** only rustup's proxy is on this machine. May I run `rustup component add rust-analyzer` to test Rust? Not asked yet; it changes the user's environment.
