# LSP navigation: definition, implementations, super, gutter markers (2026-11-02)

## Why

File tabs jump to definitions from a tree-sitter tags index (`crates/tern-app/src/symbols.rs`): fast and
setup-free, but it matches by name only, so it can't tell two `new`s apart and can't answer "who implements
this trait / interface". The user wants IDE-grade navigation in file tabs, modelled on OpenIDEA
(https://github.com/l0ng-ai/openidea, read at commit `8e8a998`): Go to Implementation(s), Super Method, and the
"is implemented by" / "implements" gutter icons. OpenIDEA gets all of that from language servers, and the user
chose the same: Tern gets an LSP client, and go to definition moves onto it too, keeping tree-sitter as the
fallback.

## Goals

1. **An LSP client** in Tern: language servers found on `PATH`, configured per language, started on demand per
   project root, kept in sync with the open file's text.
2. **Go to Definition** asks the language server first; tree-sitter (`symbols.rs`) answers when no server runs
   for the file, the server isn't ready / fails, or it times out.
3. **Go to Implementation(s)** (⌘⌥B; ⌘⌥-click): `textDocument/implementation` at the caret. One target: open it.
   Several: a chooser popup listing them.
4. **Super Method** (⌘U): from a method, the method(s) it implements / overrides; from a type, its supertypes.
5. **Gutter icons** in file tabs:
   - ↓ on a type or method that something implements / overrides ("Is implemented by A, B and 3 more");
   - ↑ on a method that implements / overrides a super method ("Implements method m in T").
   Clicking one opens its single target or the chooser.

## Non-goals (this round)

- Hover, completion, diagnostics (squiggles), code actions, rename, formatting, find usages, semantic tokens.
  The client is built so they can be added later, but none is wired up now.
- Downloading or installing servers. Tern runs what is on `PATH` and tells the user how to install what is
  missing.
- Debuggers (OpenIDEA's `[debugger]` sections), OpenIDEA's server-specific bridges (jdtls `jdt://` virtual
  documents, tsserver bridge, Angular), and its Java usages plugin.
- Language servers for Claude / chat / terminal tabs. Only file tabs (the `viewer.rs` editor) use them.

## Anti-goals

- **Never block typing or scrolling** on a server. Every request is async, cancellable and has a timeout; a slow
  or crashed server degrades to tree-sitter / no icons, never to a frozen editor.
- **No error spam.** A missing server is reported once per server per session, with its install hint, not on
  every keystroke or file open.
- **No orphaned processes.** Closing a workspace or quitting Tern shuts its servers down (`shutdown` + `exit`,
  then kill after a grace period).
- **No workarounds** (standing rule): when something doesn't fit, fix it at the root (in the vendored editor if
  needed), checking OpenIDEA and the `refs/` projects first.

## Decided

- **Model: OpenIDEA.** Server discovery, configuration shape and the navigation behaviour follow OpenIDEA;
  read its sources when in doubt (`crates/ide-lsp`, `crates/ide-language`, `languages/*.toml`,
  `src/editor/lsp/navigation.ts`, `src/editor/navigation/{implementationGutter,lineMarkers,structureNav}.ts`).
- **Servers come from `PATH` only.** If the command isn't found, the next server marked `fallback` for the
  language is tried; if none runs, the user gets the `install-hint` once (status bar notice, like other
  settings problems) and the file keeps tree-sitter definitions.
- **Per-language config, built-in plus user override,** with OpenIDEA's fields: server `name`, `command`,
  `args`, `root-markers`, `require-root-marker`, `fallback`, `initialization-options`, `install-hint`; and per
  language the file extensions it covers.
- **Languages:** the ones `symbols.rs` covers today, with OpenIDEA's default servers:
  Rust `rust-analyzer`; C/C++ `clangd`; Go `gopls`; Java `jdtls`; JS/TS `vtsls`, fallback
  `typescript-language-server --stdio`; Python and Swift: take OpenIDEA's entries (`languages/python.toml`,
  `languages/swift.toml`) as they are.
- **Project root:** the nearest ancestor of the file holding one of the server's `root-markers`, else the
  workspace root (unless `require-root-marker`). One server process per (server, root). A server starts the
  first time a file it covers is opened, not at workspace open.
- **Document sync:** `didOpen` when a file tab shows a file, `didChange` (full text is fine) as the user edits,
  `didSave`, `didClose` when no tab shows it any more. Files changed on disk (the workspace watcher that already
  feeds `symbols::Index::changed`) are sent as `workspace/didChangeWatchedFiles`.
- **Go to Implementation(s)** behaves like OpenIDEA's `gotoTargets(view, "implementation")`: dedupe the
  locations; none → a "No implementations found" hint (or "no language server is running for this file");
  one → open it at its line/column (same path as go to definition's `OpenFileAt`); several → chooser. No
  tree-sitter fallback for implementations.
- **Super Method** follows OpenIDEA's `structureNav.ts` / `superMethodTargets`: `textDocument/prepareTypeHierarchy`
  + `typeHierarchy/supertypes` on the owning type, then the supertypes' `documentSymbol` outlines to find the
  method of the same name. Without type hierarchy: Go asks `implementation`, Rust asks `declaration`
  (OpenIDEA's `superFallback`).
- **Gutter icons** follow OpenIDEA's `implementationGutter.ts` + `lineMarkers.ts`:
  - candidates from the file's `documentSymbol` outline (types that can be inherited, methods inside types),
    reading the declaration line for `abstract` / `final` / `sealed` etc. as OpenIDEA does;
  - only candidates within the viewport ±60 lines, at most 60 per pass, one pass 500 ms after the last edit
    or scroll;
  - ↓ from one cancellable batch of `implementation` requests (at most 4 in flight), self-locations removed;
  - ↑ as Super Method above;
  - results cached per document text; the cache is dropped when the server finishes indexing (`$/progress`
    end) and when the tab regains focus (another file may have added an implementation);
  - a server that can't answer (no capability / error) turns that icon group off for the file until the
    server changes.
  - Settings: a switch for the gutter icons, on by default (OpenIDEA has it in Editor › General › Gutter
    Icons).
- **The chooser** ("Choose Implementation of X (n found)") is new UI, so it is a template (built-ins in
  `templates.json`, behaviour attached with `frame` / `slot`), per `CLAUDE.md` and
  `docs/superpowers/specs/2026-10-16-ui-templates-design.md`. Each row: file icon, the target's name (the
  declaration line's identifier) and container, path relative to the workspace, line. Arrow keys / Enter /
  Escape / click, like the palette's lists.
- **Keys:** `cmd-alt-b` Go to Implementation(s), `cmd-u` Super Method (macOS; `ctrl-alt-b` / `ctrl-u` on
  Windows / Linux), in `keymap.json`, plus command palette entries in `commands.json`. ⌘⌥-click on a symbol
  is Go to Implementation(s), like ⌘-click is go to definition.
- **The editor gets a gutter-marker API.** The file-tab editor is `gpui-kit`/`gpui-base` 0.7.3 from crates.io,
  whose `Lsp` struct has definition / hover / completion hooks but nothing for implementations or gutter icons.
  `gpui-base` is vendored (as `gpui-component` already is, `[patch]` in the workspace `Cargo.toml`) and
  gains what is needed: clickable per-line gutter markers that follow their line through edits. Changes to it
  stay small and generic (no Tern specifics in the editor).

## Constraints

- Builds and checks run in the repo's own `target/` (see `CLAUDE.md`), with CI's Rust.
- macOS and Windows both ship (`vendor/gpui-pre-windows`); the client uses stdio pipes and paths/URIs that
  work on both (`symbols::path_to_uri` / `uri_to_path` already handle Windows drive letters).
- Code and comments in English; UI strings in English.
- Language servers can be very slow to start (jdtls, rust-analyzer indexing a big workspace). While a server
  is starting or indexing, definition falls back to tree-sitter and gutter icons wait for it.

## Left to the builder

- Crate layout. Suggested: a new `crates/tern-lsp` (JSON-RPC over stdio, server processes, config registry,
  document sync, typed requests), with `tern-app` doing editor/UI wiring. `lsp-types` 0.97 is already a
  dependency.
- Where the per-language config lives and its file format. It should match how Tern configures everything
  else (`settings.json` with a schema, built-ins in a JSON file next to `templates.json`), with the user
  override replacing a built-in server entry by name.
- How the status bar shows server state (starting / indexing / failed), if at all beyond the one-time
  missing-server notice.
- The exact gutter-marker API in the vendored editor, the icons used (bundled Lucide names), and how the
  ↓ / ↑ icons look, within the theme's existing element/token system.
- Request timeouts and process shutdown grace periods.
- Tests: OpenIDEA tests its LSP layer against a mock server (`crates/ide-lsp/tests/mock_server.rs`); a similar
  scripted mock server for Tern's client is expected, plus unit tests for marker candidates and the
  chooser's ordering.

## Calls made by Claude (not discussed with the user)

- Hover, completion and diagnostics are out of this round.
- Language servers run in the app process (not the `tern-mux` daemon).
- Gutter icons are on by default, with a setting to turn them off.
- "The tab regains focus" means the file tab becomes the active tab of its pane. The cache drop then re-runs a
  pass only if some document was edited, saved, or changed on disk since the last pass, so plain tab switching
  sends no requests.
- Files outside the workspace (a definition jump into `~/.cargo/registry`, `GOROOT`, `node_modules` of another
  tree): never start a new server. They are opened (`didOpen`) on the already running server for their
  language in that workspace, if any, so navigation and gutter icons work inside them; with none running they
  get tree-sitter only.
