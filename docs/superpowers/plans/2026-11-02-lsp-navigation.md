# LSP Navigation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** File tabs get an LSP client: Go to Definition through the language server (tree-sitter fallback), Go to Implementation(s) (⌘⌥B), Super Method (⌘U), and ↓/↑ inheritance gutter icons.

**Architecture:** A new crate `crates/tern-lsp` owns everything that is not UI: server configs, JSON-RPC over stdio, one process per (server, root), document sync, typed requests with position-encoding conversion, and the pure navigation logic ported from OpenIDEA (marker candidates, super-method search). `tern-app` wires it into the workspace (one `Manager` per workspace, like `symbols::Index`), the viewer (doc sync, providers, actions, gutter pass), and the palette (a `Targets` mode as the chooser). `gpui-base` is vendored and gains generic gutter markers and an alt-click hook.

**Tech Stack:** Rust 2024, gpui (`gpui-kit`/`gpui-base` 0.7.3, vendored), `lsp-types` 0.97, `serde_json`, `futures`, `flume`, `futures-timer`.

**Spec:** `docs/superpowers/specs/2026-11-02-lsp-navigation-design.md`. OpenIDEA reference clone: `/private/tmp/claude-501/-Users-cuongpham-ws-tern-ws-tern/5120addd-4223-4bd9-bf7b-d3c87a764094/scratchpad/src/openidea` (commit `8e8a998`; re-clone from https://github.com/l0ng-ai/openidea if gone).

## Global Constraints

- Code, comments, UI strings in English; match the surrounding code's comment density and idiom.
- New UI goes through the template engine (`templates.json`, `frame`/`slot`), never hand-built `div()` trees.
- Every check (fmt, clippy, tests) runs in the repo's own `target/`; local `stable` must be CI's Rust (`rustc 1.99.1` now).
- CI gate: `cargo fmt --all --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo test --locked --workspace`.
- macOS and Windows ship: stdio pipes, `CREATE_NO_WINDOW` (`0x0800_0000`) on spawned servers on Windows, URIs via `symbols::path_to_uri`/`uri_to_path`.
- Servers come from `PATH` only; nothing is downloaded.
- Never block the UI thread on a server: every request is async with a timeout; dropping a request future sends `$/cancelRequest`.
- A missing server is reported once per server name per session, with its `install_hint`.
- Gutter pass: viewport ±60 lines, ≤60 candidates per pass, 500 ms debounce, ≤4 implementation requests in flight.
- Super search depth 3 levels (OpenIDEA `superMethodTargets`).
- No workarounds: when the editor can't do something, change the vendored `gpui-base`, generically.

## Review Focus

1. **A server that never answers `initialize`** (jdtls on a huge project, a wrapper script waiting on stdin) — definition must fall back to tree-sitter after the timeout and the tab must stay responsive. Test: `manager::tests::initialize_timeout_falls_back` (Task 4).
2. **Non-ASCII text before a symbol** (`é`, emoji, CJK) — positions must round-trip between the editor's char columns and the server's UTF-16. Test: `text::tests::utf16_round_trip_with_astral_chars` (Task 3).
3. **A server that crashes mid-session** — pending requests fail with `Unavailable`, the next request restarts it at most once per minute, no panic. Test: `manager::tests::crashed_server_fails_pending_and_restarts` (Task 4).
4. **Edits while a gutter pass is in flight** — stale results must not be placed on the wrong lines. Test: `markers::tests::stale_pass_is_dropped` (Task 9).
5. **Closing the workspace / quitting with servers running** — no orphan processes. Test: `manager::tests::drop_shuts_servers_down` (Task 4).

---

### Task 1: Vendor gpui-base 0.7.3

**Files:**
- Create: `vendor/gpui-base/` (copy of `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui-base-0.7.3`, without `examples/`, their `[[example]]` entries and `Cargo.lock`)
- Modify: `Cargo.toml` (`[patch.crates-io]`: `gpui-base = { path = "vendor/gpui-base" }`, comment line in the block above)
- Modify: `vendor/README.md` (new `## gpui-base 0.7.3` section; filled in by Task 2)

- [ ] **Step 1:** Copy the crate, drop `examples/`, `[[example]]` sections and `Cargo.lock`.
- [ ] **Step 2:** Add the patch entry; run `cargo build -p tern-app` — Expected: builds, `Cargo.lock` shows `gpui-base` without a registry `source`.
- [ ] **Step 3:** Commit `Vendor: gpui-base 0.7.3, unchanged`.

### Task 2: gpui-base — gutter markers and alt-click

**Files:**
- Create: `vendor/gpui-base/src/input/editor/gutter_markers.rs`
- Modify: `vendor/gpui-base/src/input/editor/mod.rs` (`adjust_annotations`, `reset_annotations`, `on_click`)
- Modify: `vendor/gpui-base/src/input/base/element.rs` (`layout_line_numbers` reserves width; prepaint/paint markers like `layout_fold_icons`/`paint_fold_icons`)
- Modify: `vendor/gpui-base/src/input/editor/lsp/mod.rs` (`Lsp::alt_click`), `.../lsp/definitions.rs` (`handle_click_hover_definition`)
- Modify: `vendor/README.md`

**Interfaces:**
- Produces:
  - `pub struct GutterMarker { pub offset: usize /* any byte in its line */, pub icon: SharedString /* asset path, e.g. "icons/arrow-down.svg" */, pub color: Option<Hsla>, pub tooltip: SharedString, pub id: usize }`
  - `pub type GutterMarkerHandler = Rc<dyn Fn(usize /* id */, Bounds<Pixels> /* icon bounds, window coords */, &mut Window, &mut App)>;`
  - `impl EditorState { pub fn set_gutter_markers(&mut self, markers: Vec<GutterMarker>, cx: &mut Context<Self>); pub fn set_gutter_marker_handler(&mut self, h: Option<GutterMarkerHandler>); pub fn reserve_gutter_markers(&mut self, on: bool, cx: &mut Context<Self>); pub fn gutter_markers(&self) -> &[GutterMarker] }`
  - `Lsp::alt_click: Option<Rc<dyn Fn(usize /* byte offset */, &mut Window, &mut App)>>` — secondary+alt mouse-down on text calls it (instead of returning `false`).
  - Re-exported through `gpui_kit::component::input` like `DefinitionProvider`.

- [ ] **Step 1: Write the failing test** in `gutter_markers.rs`:

```rust
#[test]
fn markers_follow_their_line_through_edits() {
    let mut m = GutterMarkers::default();
    m.set(vec![marker(10, 1), marker(30, 2)]);
    m.adjust_for_edit(&(0..0), 5);   // insert 5 bytes at the start
    assert_eq!(m.offsets(), [15, 35]);
    m.adjust_for_edit(&(12..20), 0); // delete 12..20: marker at 15 is inside, dropped
    assert_eq!(m.offsets(), [27]);   // the one after shifts left by 8
}
```

- [ ] **Step 2:** `cargo test -p gpui-base gutter_markers` — Expected: FAIL (no `GutterMarkers`).
- [ ] **Step 3:** Implement `GutterMarkers` (sorted `Vec<GutterMarker>`, `set`, `adjust_for_edit(range, new_len)`, `offsets()`), store it in `EditorExtras`, hook `adjust_annotations`/`reset_annotations`.
- [ ] **Step 4:** Paint: `layout_line_numbers` adds `GUTTER_MARKER_WIDTH = px(16.)` when markers are reserved; a new `layout_gutter_markers` (after `layout_fold_icons`) prepaints, for each visible buffer line with markers, one 12px svg per marker left of the line numbers, tinted `color` or the text color; mouse-down calls the handler with the marker id and its bounds, `cx.stop_propagation()`; hover shows `tooltip` (gpui `tooltip` on the icon div). Several markers on one line sit side by side.
- [ ] **Step 5:** Alt-click: in `handle_click_hover_definition`, when `secondary && alt` and `lsp.alt_click` is set, call it with `offset` and return `true`.
- [ ] **Step 6:** `cargo test -p gpui-base gutter_markers` PASS; `cargo build -p tern-app` OK.
- [ ] **Step 7:** README section listing both changes and why; commit `gpui-base: gutter markers and alt-click hook`.

### Task 3: tern-lsp — config, languages, roots, positions

**Files:**
- Create: `crates/tern-lsp/Cargo.toml` (deps: `lsp-types = "0.97"`, `serde`, `serde_json`, `futures`, `flume`, `futures-timer`, `log`; workspace member via `crates/*`)
- Create: `crates/tern-lsp/src/lib.rs`, `src/config.rs`, `src/servers.json`, `src/text.rs`

**Interfaces:**
- Produces:
  - `#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq)] pub struct ServerConfig { pub name: String, pub languages: Vec<String>, pub command: String, pub args: Vec<String>, pub root_markers: Vec<String>, pub require_root_marker: bool, pub fallback: bool, pub initialization_options: Option<Value>, pub settings: Option<Value>, pub install_hint: Option<String> }` (`#[serde(default, deny_unknown_fields)]`, `name`/`command`/`languages` required)
  - `pub fn builtin() -> Vec<ServerConfig>`; `pub fn builtin_text() -> &'static str`
  - `pub fn resolve(builtin: &[ServerConfig], user: &BTreeMap<String, Option<ServerConfig>>) -> Vec<ServerConfig>` — user entry replaces the built-in of that name, `null` removes it, a new name is appended.
  - `pub fn language_id(path: &Path) -> Option<&'static str>` — LSP languageIds: `rust`, `c`, `cpp`, `go`, `java`, `javascript`, `javascriptreact`, `typescript`, `typescriptreact`, `python`, `swift`; extensions as `symbols::lang_for` plus OpenIDEA's (`pyw`, `swiftinterface`, `tpp`, `inl`, `ixx`, `cppm`, `h++`).
  - `pub fn servers_for<'a>(configs: &'a [ServerConfig], language: &str) -> Vec<&'a ServerConfig>` — non-fallback first, then fallbacks, config order.
  - `pub fn find_root(file: &Path, markers: &[String], workspace: &Path, require: bool) -> Option<PathBuf>` — nearest ancestor (stopping at `workspace`) holding a marker; else `workspace` unless `require`; `None` for files outside `workspace` (Task 4 handles those).
  - `text.rs`: `pub enum Encoding { Utf8, Utf16, Utf32 }`, `pub fn to_server(line: &str, col_chars: u32, enc: Encoding) -> u32`, `pub fn from_server(line: &str, col: u32, enc: Encoding) -> u32`.
- `servers.json` entries (from OpenIDEA `languages/*.toml`): `rust-analyzer`; `clangd` (`--background-index --header-insertion=never`, `languages: ["c","cpp"]`); `gopls` (markers `go.work`,`go.mod`); `jdtls` (markers + `initialization_options` exactly as OpenIDEA's `java.toml`, minus `bundles`); `vtsls --stdio` then fallback `typescript-language-server --stdio` (`languages`: the four JS/TS ids); `basedpyright-langserver --stdio`, fallbacks `pyright-langserver --stdio`, `ty server`; `sourcekit-lsp`. Root markers and install hints copied verbatim.

- [ ] **Step 1: Failing tests** in `config.rs`:
  - `builtin_parses_and_names_are_unique`
  - `user_entry_replaces_removes_and_adds` (replace `gopls` args, `"clangd": null` removes, new `"zls"` appended)
  - `fallbacks_come_after` (`servers_for(&builtin(), "python")` names = `["basedpyright","pyright","ty"]`)
  - `root_is_nearest_marker_inside_workspace` (tmp tree `ws/a/Cargo.toml`, file `ws/a/src/x.rs` → `ws/a`; no marker → `ws`; `require` → `None`; file outside → `None`)
  - `language_ids_by_extension` (`x.tsx`→`typescriptreact`, `x.h`→`cpp`, `x.pyw`→`python`, `x.txt`→`None`)
  and in `text.rs`: `utf16_round_trip_with_astral_chars` (`"a😀é b"`: char col 4 ↔ utf16 col 5; `from_server(to_server(c)) == c` for every c).
- [ ] **Step 2:** `cargo test -p tern-lsp` — FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test -p tern-lsp` — PASS. Commit `tern-lsp: server configs, languages, roots, position encodings`.

### Task 4: tern-lsp — JSON-RPC connection and server manager

**Files:**
- Create: `crates/tern-lsp/src/rpc.rs`, `src/manager.rs`, `src/bin/mock_lsp.rs` (test server, scripted by `MOCK_LSP_SCRIPT` = path of a JSON file: capabilities, canned responses by method, delays, `crash_after`)
- Test: `crates/tern-lsp/tests/manager.rs` (uses `env!("CARGO_BIN_EXE_mock_lsp")`)

**Interfaces:**
- Produces:
  - `pub enum LspError { NotFound, Unavailable(String), Unsupported, Cancelled, Timeout, Server { code: i64, message: String } }` (`Display`, `std::error::Error`)
  - `pub struct Location { pub path: PathBuf, pub range: lsp_types::Range /* char columns */ }`
  - `pub enum LspEvent { Missing { server: String, hint: Option<String> }, Failed { server: String, message: String }, Settled }`
  - `pub struct Manager` — `pub fn new(workspace: PathBuf, configs: Vec<ServerConfig>, events: flume::Sender<LspEvent>) -> Arc<Manager>`; `pub fn set_configs(&self, configs: Vec<ServerConfig>)` (servers whose config changed are shut down; next use restarts); `pub fn supports(&self, path: &Path) -> bool`
  - Doc sync: `pub fn open(&self, path: &Path, text: String)`, `pub fn change(&self, path: &Path, text: String)` (full sync, version += 1), `pub fn save(&self, path: &Path)`, `pub fn close(&self, path: &Path)`, `pub fn files_changed(&self, paths: &[PathBuf])` (`workspace/didChangeWatchedFiles`, type Changed/Created/Deleted by existence). `open` is ref-counted per path (several tabs on one file).
  - Internals the requests task uses: `async fn server_for(&self, path: &Path, capability: Cap) -> Result<(Arc<Server>, DocKey), LspError>` — first config for the language whose command runs (spawn failure `NotFound` → next fallback; all missing → one `LspEvent::Missing` per name per `Manager`), started once per `(name, root)`; a file outside the workspace uses an already running server of its language (any root), never starts one, `Unavailable` if none; `didOpen` from disk text if the file isn't open.
  - Handshake: `initialize` with `processId`, `rootUri`, `workspaceFolders`, `initializationOptions`, client capabilities: `general.positionEncodings ["utf-32","utf-16"]`, `textDocument.{definition,declaration,implementation}.linkSupport`, `documentSymbol.hierarchicalDocumentSymbolSupport`, `typeHierarchy`, `window.workDoneProgress`, `workspace.{configuration,workspaceFolders,didChangeWatchedFiles}`; then `initialized`, then `workspace/didChangeConfiguration { settings }` when `settings` is set.
  - Server → client requests answered: `workspace/configuration` (items from `settings` by `section` path, else `null`), `window/workDoneProgress/create`, `client/registerCapability`, `client/unregisterCapability` (→ `null`), `window/showMessageRequest` (→ `null`); others → MethodNotFound. `$/progress` begin/end tracked; when the last end arrives → `LspEvent::Settled`.
  - Timeouts: `initialize` 60 s (then `Unavailable`, server killed, requests fall back); requests 10 s. Dropping a request future sends `$/cancelRequest`.
  - Crash: reader EOF fails every pending request with `Unavailable`; the next use restarts it, at most once per 60 s per `(name, root)`.
  - `impl Drop for Manager`: on a thread, `shutdown` (2 s) + `exit` to each server, then kill after 3 s more.
  - Windows: `CREATE_NO_WINDOW`; stderr goes to `log::debug!` line by line.
- [ ] **Step 1: Failing tests** (`tests/manager.rs`):
  - `starts_on_first_use_and_opens_document` (mock records received notifications; after `open` + a request, it saw `initialize`, `initialized`, `textDocument/didOpen` with the text and languageId)
  - `change_sends_full_text_with_increasing_version`
  - `missing_server_tries_fallback_then_reports_once` (configs: `nope-1` (missing), fallback mock → request succeeds through mock; with only missing ones → two requests → exactly one `Missing` event)
  - `initialize_timeout_falls_back` (mock delays `initialize` 120 s; request returns `Err(Unavailable)` within the timeout — test with timeout overridden to 300 ms via `Manager::with_timeouts(init, request)` `#[cfg(test)]`/`pub(crate)` constructor)
  - `crashed_server_fails_pending_and_restarts` (`crash_after: 1` → pending request `Err(Unavailable)`; the next request after the backoff (test backoff 0) succeeds)
  - `cancel_on_drop_sends_cancel_request` (start a delayed request, drop it, mock saw `$/cancelRequest` with that id)
  - `answers_workspace_configuration` (`settings: {"gopls": {"x": 1}}`, mock asks section `gopls` → gets `{"x":1}`)
  - `outside_workspace_reuses_running_server_or_unavailable`
  - `drop_shuts_servers_down` (mock writes a marker file on `exit`; after drop + 3 s it exists and the pid is gone)
- [ ] **Step 2:** `cargo test -p tern-lsp --test manager` — FAIL.
- [ ] **Step 3:** Implement `rpc.rs` (Content-Length framing, writer thread fed by a flume channel, reader thread dispatching responses to `futures::channel::oneshot` by id, notifications/requests to a handler), then `manager.rs`.
- [ ] **Step 4:** PASS. Commit `tern-lsp: JSON-RPC client and per-root server manager`.

### Task 5: tern-lsp — navigation requests

**Files:**
- Create: `crates/tern-lsp/src/requests.rs`
- Test: `crates/tern-lsp/tests/requests.rs`

**Interfaces:**
- Produces (on `Manager`, all positions in char columns, results deduped and sorted by path/line):
  - `pub async fn definition(&self, path: &Path, pos: Position) -> Result<Vec<Location>, LspError>`
  - `pub async fn declaration(..)`, `pub async fn implementation(..)` — same shape; `Unsupported` when no server for the file has the capability.
  - `pub async fn implementations(&self, path: &Path, positions: &[Position]) -> Result<Vec<Vec<Location>>, LspError>` — `buffered(4)`; a failing position gets `[]`; all failing → the first error.
  - `pub async fn outline(&self, path: &Path) -> Result<Vec<lsp_types::DocumentSymbol>, LspError>` — flat `SymbolInformation` answers converted to a one-level tree; ranges in char columns.
  - `pub async fn supertypes(&self, path: &Path, pos: Position) -> Result<Vec<HierarchyItem>, LspError>` with `pub struct HierarchyItem { pub name: String, pub kind: lsp_types::SymbolKind, pub path: PathBuf, pub range: Range, pub selection_range: Range }` (`prepareTypeHierarchy` then `typeHierarchy/supertypes` on the first item).
  - Conversion: `GotoDefinitionResponse` (Scalar / Array / Link → target_selection_range), URIs via a `uri` helper copied from `symbols::{path_to_uri, uri_to_path}` (move those two into `tern-lsp::uri` and re-export from `symbols` to avoid two copies), columns via `text::from_server` reading the target line from the open doc or disk.
- [ ] **Step 1: Failing tests:** `definition_handles_scalar_array_and_links`, `implementations_batch_keeps_order_and_empty_for_failures`, `unsupported_without_capability`, `outline_converts_flat_symbols`, `supertypes_two_step`, `utf16_server_columns_converted` (mock answers in utf-16 on a line with `😀`).
- [ ] **Step 2:** FAIL. **Step 3:** Implement. **Step 4:** PASS. Commit `tern-lsp: definition, implementation, outline, type hierarchy`.

### Task 6: tern-lsp — navigation logic (ported from OpenIDEA)

**Files:**
- Create: `crates/tern-lsp/src/nav.rs`

**Interfaces:**
- Produces (pure, ported from `lineMarkers.ts` / `structureNav.ts`, same constants and regexes):
  - `pub struct Candidate { pub symbol: String, pub name: String, pub line: u32, pub col: u32, pub method: bool, pub is_abstract: bool, pub inheritable: bool, pub overriding: bool, pub owner: Option<Position> }`
  - `pub fn candidates(outline: &[DocumentSymbol], line_text: impl Fn(u32) -> String) -> Vec<Candidate>` (`INHERITABLE_TYPES` = Class/Interface (+ Rust trait: rust-analyzer reports traits as `Interface`), `MEMBER_OWNERS` adds Enum/Struct/Object (rust-analyzer `impl` blocks are `Object`), `NOT_INHERITABLE`, `NOT_OVERRIDING`, `ABSTRACT`, `DEFAULT` as OpenIDEA)
  - `pub fn without_self(path: &Path, c: &Candidate, targets: Vec<Location>) -> Vec<Location>`
  - `pub fn target_name(lines: &[String], target: &Location, method: bool) -> String` (identifier at the target, or `enclosing_type_name` with OpenIDEA's `TYPE_DECLARATION` regex, else file stem)
  - `pub enum MarkerKind { Implemented, Overridden, Implementing, Overriding }`, `pub struct LineMarker { pub kind: MarkerKind, pub line: u32, pub name: String, pub targets: Vec<Location>, pub tooltip: String, pub chooser_title: String }`
  - `pub fn inheritors_marker(c: &Candidate, targets: Vec<Location>, names: Vec<String>) -> Option<LineMarker>`, `pub fn supers_marker(c: &Candidate, supers: Vec<SuperMethod>, abstract_flags: Vec<bool>, owners: Vec<String>) -> Option<LineMarker>` — tooltip / title strings exactly as OpenIDEA's (`Is implemented by {names}`, `Is subclassed by`, `Is implemented in`, `Is overridden in`, `Implements method {m} in {owners}`, `Overrides method …`, `Choose Implementation of {name} ({n} found)`, `Choose Subclass of …`, `Choose Overriding Method of …`, `Choose Super Method of …`), names list capped at 8 + `and {k} more`.
  - `pub struct SuperMethod { pub location: Location, pub owner: HierarchyItem, pub method: DocumentSymbol }`
  - `pub async fn super_method_targets(path, type_pos, method, supertypes: impl AsyncFn(&Path, Position) -> Vec<HierarchyItem>, outline: impl AsyncFn(&Path) -> Vec<DocumentSymbol>) -> Vec<SuperMethod>` — depth 3, `seen` set, as OpenIDEA.
  - `pub fn super_fallback_kind(language: &str) -> Option<GotoKind>` (`go` → Implementation, `rust` → Declaration); `pub enum GotoKind { Definition, Declaration, Implementation }`.
- [ ] **Step 1: Failing tests** — port OpenIDEA's `lineMarkers.test.ts` and `structureNav.test.ts` cases that cover the functions above (Java abstract class/interface/default method/final, Rust trait + impl, TS class), plus `names_list_caps_at_eight`.
- [ ] **Step 2:** FAIL. **Step 3:** Implement. **Step 4:** PASS. Commit `tern-lsp: inheritance markers and super method search`.

### Task 7: tern-app — settings, workspace manager, doc sync, definition through LSP

**Files:**
- Modify: `crates/tern-app/Cargo.toml` (`tern-lsp = { path = "../tern-lsp" }`)
- Modify: `crates/tern-app/src/settings.rs` (`SettingsFile.lsp: LspSettings`; `LspSettings { gutter_icons: Option<bool> /* default true */, servers: BTreeMap<String, Option<tern_lsp::ServerConfig>> }`; global `LspConfig { gutter_icons: bool, servers: Vec<ServerConfig> }` set in `reload`; write `reference/language-servers.json` from `tern_lsp::builtin_text()` like the other reference files; doc comments for the schema)
- Modify: `crates/tern-app/src/workspace.rs` (field `lsp: Arc<tern_lsp::Manager>` beside `symbols`; pass to `Viewer::new`; `files_changed` → `lsp.files_changed`; `LspConfig` observer → `set_configs`; `LspEvent::Missing` → `window.push_notification("No {server} for this file: {hint}")`, `Failed` → `log::warn!`, `Settled` → notify viewers)
- Modify: `crates/tern-app/src/viewer.rs` (`Viewer::new(root, symbols, lsp, ..)`; `open`/`restore`/`set_path`/reload → `lsp.open/close`; editor `InputEvent::Change` → `lsp.change`; `save` → `lsp.save`; on release → `lsp.close`)
- Modify: `crates/tern-app/src/viewer.rs` `Definitions` → asks `lsp.definition` first; on `Err(_)` or not `supports` → the tree-sitter lookup as today; `Ok(vec![])` from the server → also tree-sitter (a server that knows nothing yet).
- Modify: `README.md` settings section (`lsp.gutter_icons`, `lsp.servers`).

**Interfaces:**
- Consumes: Task 3–5 `Manager` API.
- Produces: `Viewer` fields `lsp: Arc<tern_lsp::Manager>`; `crate::settings::LspConfig` global; `ViewerEvent::LspSettled` handling hook `Viewer::lsp_settled(&mut self, cx)` (used by Task 9).
- [ ] **Step 1: Failing test** in `settings.rs`: `lsp_servers_override_by_name` (settings JSON with `"lsp": {"servers": {"gopls": null}, "gutter_icons": false}` → `LspConfig.servers` lacks gopls, `gutter_icons == false`).
- [ ] **Step 2:** FAIL. **Step 3:** Implement all of the above.
- [ ] **Step 4:** Tests PASS; manual: open `crates/tern-app/src/viewer.rs` in a debug build with `rust-analyzer` on PATH, ⌘-click `symbols::Index` → jumps to `symbols.rs`; with PATH lacking it, one notification, tree-sitter still jumps.
- [ ] **Step 5:** Commit `File tabs: language servers for go to definition`.

### Task 8: Go to Implementation(s), Super Method, chooser

**Files:**
- Modify: `crates/tern-app/src/viewer.rs` (actions `GoToImplementation`, `GoToSuper` in `actions!(viewer, ..)`; listeners; `lsp.alt_click` → implementation at that offset; `ViewerEvent::ChooseTargets { title: SharedString, targets: Vec<tern_lsp::Location> }`; hint via `window.push_notification` for "No implementations found" / "No super method found" / "… : no language server is running for this file")
- Modify: `crates/tern-app/src/palette.rs` (`Mode::Targets`; `pub fn open_targets(palette: &Entity<Palette>, title: SharedString, targets: Vec<Target>, window, cx)`; `pub struct Target { pub path: PathBuf, pub line: u32, pub col: u32, pub name: SharedString, pub container: SharedString }`; typed text filters by name/path fuzzy (`tern_fs::fuzzy`); title in the `status` line; Enter/click emits `PaletteEvent::Open`)
- Modify: `crates/tern-app/src/templates.json` (`palette.target`: icon slot, `{name}`, `{container}`, `{dir}:{line}`, styled as `palette.file`)
- Modify: `crates/tern-app/src/workspace.rs` (`ViewerEvent::ChooseTargets` → open palette in `Targets`)
- Modify: `crates/tern-app/src/keymap.json` (`cmd-alt-b` → `viewer::GoToImplementation`, `cmd-u` → `viewer::GoToSuper`, context `Viewer`; Windows/Linux `ctrl-alt-b`, `ctrl-u` with `platform`), `commands.json` ("Go to Implementation(s)", "Go to Super Method"), `menus.json` (beside Word Wrap)

**Interfaces:**
- Consumes: `Manager::{implementation, supertypes, outline, declaration}`, `nav::{super_method_targets, super_fallback_kind, target_name}`.
- Produces: `fn open_targets(&mut self, title, locations, window, cx)` on `Viewer` (one → `OpenFileAt`/in-file move; several → `ChooseTargets`), reused by Task 9.
- Super Method: method at caret (innermost outline item of kind Method/Function containing the caret, owner = parent type) → `super_method_targets`; a type at caret → its `supertypes`; empty and `super_fallback_kind(lang)` is set → that request, minus the caret's own line.
- [ ] **Step 1: Failing tests:** `palette::tests::targets_filter_by_name_and_path`, `viewer::tests::member_at_caret_finds_method_and_owner` (pure helper over a `DocumentSymbol` tree).
- [ ] **Step 2:** FAIL. **Step 3:** Implement. **Step 4:** PASS; manual in the debug build on Tern's own code: ⌘⌥B on `trait DefinitionProvider`'s use → chooser lists `Definitions`; ⌘U inside `impl DefinitionProvider for Definitions { fn definitions }` → jumps to the trait method; ⌘⌥-click works.
- [ ] **Step 5:** Commit `File tabs: go to implementations and super method`.

### Task 9: Gutter icons

**Files:**
- Create: `crates/tern-app/src/markers.rs` (pass scheduling and cache; no gpui tree building)
- Modify: `crates/tern-app/src/viewer.rs` (own a `Markers`; hook scroll/edit/focus/settled; `set_gutter_markers`; handler → `open_targets`)
- Modify: `assets/icons/` (four 12px icons, Lucide: `arrow-down-circle` implemented, `arrow-down` overridden, `arrow-up-circle` implementing, `arrow-up` overriding — bundled like the others) and their colors as theme elements `viewer.gutter_marker.{implemented,overridden,implementing,overriding}` in the element defaults

**Interfaces:**
- Consumes: Tasks 2, 5, 6, 8.
- Produces: `pub struct Markers { cache: MarkerCache, outline: Option<(u64 /* text hash */, Vec<DocumentSymbol>, Vec<Candidate>)>, generation: u64, dirty_since_pass: bool }` with `pub fn schedule(&mut self, cx)` (500 ms debounce), `pub fn invalidate(&mut self)`, `pub fn note_edit(&mut self)`, and the pure `fn visible_candidates(c: &[Candidate], first: u32, last: u32) -> Vec<Candidate>` (±60, ≤60).
- Pass: outline (once per text) → visible candidates not cached → `implementations` (inheritable) and `super_method_targets` (overriding with owner) → read target files' lines → `inheritors_marker`/`supers_marker` → cache by `line:col:symbol` → `set_gutter_markers` for all cached candidates. A pass whose generation or text changed before it ends is dropped. `Unsupported`/`Unavailable` turns that group off for this text. `Settled` → invalidate + schedule; tab activated with `dirty_since_pass` → invalidate + schedule; `LspConfig.gutter_icons == false` → markers cleared and not reserved.
- [ ] **Step 1: Failing tests** (`markers.rs`): `visible_candidates_window_and_cap`, `stale_pass_is_dropped` (pass started at generation 1, generation bumped, results not applied), `unsupported_turns_group_off`.
- [ ] **Step 2:** FAIL. **Step 3:** Implement.
- [ ] **Step 4:** PASS; manual: `vendor/gpui-base/src/input/editor/lsp/definitions.rs` shows ↓ on `trait DefinitionProvider` ("Is implemented by …"), `viewer.rs` shows ↑ on `fn definitions`; clicking opens; typing above moves icons with their lines; turning `lsp.gutter_icons` off removes the column.
- [ ] **Step 5:** Commit `File tabs: inheritance gutter icons`.

### Task 10: Docs, full CI gate, end-to-end check

**Files:**
- Modify: `README.md` (Features: language servers, the table of built-in servers and install hints; keys ⌘⌥B / ⌘U / ⌥⌘-click)
- [ ] **Step 1:** `cargo fmt --all --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo test --locked --workspace` — all pass.
- [ ] **Step 2:** End-to-end in the debug build (screenshots per `debug-app-screenshots` memory) on Rust (rust-analyzer) and, if installed, TypeScript (vtsls) and Go (gopls): definition, implementations chooser, super, gutter icons, a missing server's single notice, quitting leaves no server processes (`pgrep rust-analyzer` empty).
- [ ] **Step 3:** Commit `Docs: language servers`.
