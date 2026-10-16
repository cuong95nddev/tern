# Movable Panels (layout phase A) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The user drags Explorer, Scratches and Changes by their headers between the left and right docks (before or
after another panel). Tern remembers this per workspace, "Reset Layout" undoes it, and `layout.locked` turns it off.
The template engine is split so that compiling templates is apart from drawing them (the renderer-neutral rule).

**Architecture:** A new pure module `arrange.rs` (no gpui) holds the arrangement model: per dock, a tree of parts with
their sizes. It moves parts, prunes hidden ones, reconciles the arrangement with the templates and encodes it for the
session file. The template engine learns `part`, `dock` and `handle` nodes. When it draws a dock, it draws the
arrangement's tree for that dock instead of the template's children, and it reports drops and resizes back through
a callback the workspace puts in the `Scope`. A small gpui module `dock.rs` holds the drag payload, the active-drag
global and the lock global.

**Tech Stack:** Rust, gpui (`gpui-kit` 0.7.3: `on_drag`, `drag_over`, `on_drop`, `h_resizable`/`v_resizable` with
`on_resize`), serde/schemars.

**Spec:** `docs/superpowers/specs/2026-10-16-ui-templates-design.md`, section "User-arranged layout (parts, docks,
floating)", phase A, plus the "Templates are renderer-neutral" rule.

**Out of this plan (own plans later):** layout phases B (center as a dock, `edges`, `stack`), C (floating), D (OS
window); templates phases 2–7; the TUI.

## Global Constraints

- Code and comments in English; match the surrounding style: short doc comments that say what a thing is, no banners.
- The default look must not change: with no user templates and no stored arrangement, the window is pixel-identical
  to before (compare screenshots, Task 5).
- Templates stay renderer-neutral: no gpui names in the template format. Lengths are plain numbers. GUI-only
  properties are marked "GUI only" in `settings.schema.json` and `reference/elements.md`.
- `arrange.rs` must not import gpui.
- Problems (unknown part, a part placed twice, bad `accepts`/`zones`/`empty`, a stored part that no longer fits) go to
  the status bar like other settings problems. Never panic.
- Don't commit on `main`: the executor creates a branch first (the user's rule). The tree already holds uncommitted
  theming work (parts 1–11). Ask the user whether to commit that work first on the branch, before Task 1.
- After each task: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings` and
  `cargo fmt --check` are clean.

## Review Focus

1. A session file from an older build (no `arrange` lines) opens with the default layout and loses nothing else
   (Task 4 test `old_session_has_no_arrangement`).
2. Switching to a theme whose `workspace` template has different docks, then back, restores the first arrangement
   (Task 2 test `arrangements_are_kept_per_key`).
3. A user template that places a part twice, or leaves a part out, reports a problem and draws the part at most once
   (Task 3 test `part_placed_twice_is_reported`, Task 2 test `reconcile_drops_and_adds_parts`).
4. Dropping a part on its own before/after zone, or next to itself, changes nothing and never duplicates it (Task 2
   test `move_next_to_itself_is_a_no_op`).
5. Hiding every part of a dock collapses the dock (no empty column) and showing one again brings back its old size
   (Task 2 test `prune_hides_and_collapses`; Task 5 visual check).

---

### Task 1: Split the template engine; mark GUI-only properties

No behavior change. `template.rs` becomes a directory module: compiling (format, conditions, `{var}` text, `use`)
goes in one file and drawing with gpui in another.

**Files:**
- Move: `crates/tern-app/src/template.rs` → `crates/tern-app/src/template/mod.rs` (Spec, Edges, `builtin`,
  `builtin_text`, Value, Scope, Cond/Test/Cmp, Panel, Node, SLOTS, `compile`, Compiler, `parts`, tests)
- Create: `crates/tern-app/src/template/draw.rs` (`render`, `Frames`, Rem, Drawer, `lookup`, `boxed`, `split`, `look`,
  `Node::is_bare`)
- Modify: `crates/tern-app/src/style.rs` (add `GUI_ONLY`), `crates/tern-app/src/settings.rs` (`elements_doc`,
  `style_schema`: tag GUI-only props)

**Interfaces:**
- Produces: `crate::template::{render, Frames, Scope, Spec, Templates, compile, builtin, builtin_text}` stay at the
  same paths (re-export `draw::{render, Frames}` from `mod.rs`), so callers don't change.
- Produces: `pub const GUI_ONLY: &[&str]` in `style.rs`. It holds property names: `radius`, `radius_top_left`,
  `radius_top_right`, `radius_bottom_right`, `radius_bottom_left`, `shadow`, `opacity`, `background_image`,
  `background_image_fit`, `background_image_opacity`.

- [ ] **Step 1: Write the failing test** in `settings.rs` tests:

```rust
#[test]
fn gui_only_props_are_marked() {
    let doc = elements_doc();
    assert!(doc.contains("`shadow`") && doc.contains("GUI only"));
    for p in crate::style::GUI_ONLY {
        assert!(crate::style::ELEMENTS.iter().any(|e| e.prop(p).is_some()), "{p} is not a property");
    }
}
```

(If the element table is named differently from `ELEMENTS`, use its real name.)

- [ ] **Step 2:** `cargo test -p tern-app gui_only_props_are_marked`. Expected: FAIL (no `GUI_ONLY`).
- [ ] **Step 3:** Do the move. Fields that `draw.rs` reads become `pub(super)`. Add `GUI_ONLY`. In `elements_doc`,
  append ` (GUI only)` to each such property's line. In `style_schema`, prefix such a property's `description` with
  `GUI only: `. Add one README paragraph in `readme()` saying that templates and elements describe structure and
  look for any front end, and that GUI-only properties are ignored elsewhere.
- [ ] **Step 4:** `cargo test --workspace`. Expected: PASS, including `builtin_templates_read_cleanly`.
- [ ] **Step 5: Commit** `template: split compiling from drawing; mark GUI-only properties`.

---

### Task 2: The arrangement model (`arrange.rs`)

Pure data, no gpui. Unit-tested on its own.

**Files:**
- Create: `crates/tern-app/src/arrange.rs`; add `mod arrange;` in `main.rs`

**Interfaces (Produces, used by Tasks 3–7):**

```rust
pub enum Dir { Row, Column }
pub enum Size { Px(f32), Frac(f32), Auto }
pub enum Tree { Part(String), Split(Dir, Vec<(Tree, Size)>) } // Stack comes in phase B
pub enum Drop { Before(String), After(String), End }          // relative to a part already in the dock
pub struct DockInfo { pub name: String, pub dir: Dir, pub accepts: Vec<String>, pub defaults: Tree }
pub struct Arrangement { pub key: String, pub docks: BTreeMap<String, Tree>, pub widths: BTreeMap<String, f32> }

impl Arrangement {
    pub fn defaults(key: &str, docks: &[DockInfo]) -> Self;
    pub fn dock_of(&self, part: &str) -> Option<&str>;
    /// Moves `part` into `dock` at `at`; false (and no change) when the dock doesn't accept
    /// `kind`, the dock is unknown, or the move would leave everything as it is.
    pub fn move_part(&mut self, part: &str, kind: &str, dock: &str, at: Drop, docks: &[DockInfo]) -> bool;
    /// Sets the sizes of the split at `path` (child indices from the dock's root) in `dock`.
    pub fn set_sizes(&mut self, dock: &str, path: &[usize], sizes: &[Size]);
    /// Fits a stored arrangement to the templates in effect; returns the problems.
    pub fn reconcile(&mut self, docks: &[DockInfo], parts: &[(&str, &str)] /* name, kind */) -> Vec<String>;
    pub fn encode(&self) -> String;
    pub fn decode(key: &str, text: &str) -> Option<Self>;
}
/// `tree` with the parts `shown` rejects removed; a split left with one child becomes that child; None when empty.
pub fn prune(tree: &Tree, shown: &dyn Fn(&str) -> bool) -> Option<Tree>;
/// The key an arrangement is stored under: template name, `/`, the dock names sorted and joined with `+`
/// (`workspace/left+right`).
pub fn key(template: &str, docks: &[DockInfo]) -> String;
```

Encoding (one line, no tabs or newlines): docks are separated by `;` as `name=tree`. `tree` is a part name, or
`[r item item…]` / `[c item item…]`. `item` is `tree@size`, and `size` is `240px`, `0.35` (a fraction) or `-` (auto).
An empty dock is written `name=`. Stored dock sizes (Task 5) come last as `;w:left=240,right=300`, left out when
there are none. Part names are `[A-Za-z0-9_.-]+`. `decode` returns `None` on anything malformed
(the caller then uses the defaults).

Moving: the part is first removed from wherever it is (then `prune`d of empty splits, keeping hidden parts). After
that, `Before`/`After` insert next to the target in the target's parent split. `End` appends to the dock's root
split. If the root is a single part, the root becomes a split along the dock's `dir`. The moved part's size becomes
`Auto`.

Reconcile: drop parts the templates no longer have. Drop the second copy of a part that appears twice. A part
whose dock is gone or no longer accepts its kind goes back to its default dock (`End`). A part in the templates but
in no dock goes to its default dock (`End`). Each fix except "new part" adds a problem:
`layout: "<part>" moved back to dock "<dock>"`.

- [ ] **Step 1: Write the failing tests** in `arrange.rs` `mod tests`, with a fixture `docks()` = left (Column,
  accepts `["panel"]`, defaults `[c explorer@- scratches@0.3]`) and right (Column, accepts `["panel"]`, defaults
  `changes`):
  - `encode_decode_round_trip`: `defaults` → `encode` equals `left=[c explorer@- scratches@0.3];right=changes`, and
    `decode` of that gives back an equal `Arrangement`. `decode("k", "left=[x")` is `None`.
  - `move_before_and_after`: moving `changes` `Before("explorer")` in `left` gives
    `left=[c changes@- explorer@- scratches@0.3];right=`. Moving it back `End` in `right` gives the default encoding
    except `right=changes`.
  - `move_next_to_itself_is_a_no_op`: `move_part("explorer", "panel", "left", Drop::Before("explorer"))` returns
    false, and so does `After("explorer")` on a dock where explorer is already last; the encoding is unchanged.
  - `move_refuses_kinds_not_accepted`: kind `"pane"` into `left` returns false.
  - `prune_hides_and_collapses`: pruning `[c explorer@- scratches@0.3]` with only `scratches` shown gives
    `Part("scratches")`; with nothing shown, `None`. The stored tree still has both (prune doesn't mutate).
  - `reconcile_drops_and_adds_parts`: stored `left=[c explorer@- ghost@-];right=changes` with parts explorer,
    scratches, changes gives `left=[c explorer@- scratches@-];right=changes` and one problem mentioning `ghost`. A
    part listed twice is kept once.
  - `arrangements_are_kept_per_key`: `key("workspace", &docks())` is `workspace/left+right`, and a different dock set
    gives a different key.
- [ ] **Step 2:** `cargo test -p tern-app arrange`. Expected: FAIL (module missing).
- [ ] **Step 3:** Implement `arrange.rs` per the interfaces above.
- [ ] **Step 4:** `cargo test -p tern-app arrange`. Expected: PASS.
- [ ] **Step 5: Commit** `arrange: per-dock part trees (move, prune, reconcile, encode)`.

---

### Task 3: `part`, `dock`, `handle` in the template format

Compile only; drawing is Task 5.

**Files:**
- Modify: `crates/tern-app/src/template/mod.rs`

**Interfaces:**
- Consumes: `arrange::{DockInfo, Dir, Tree, Size}`.
- New `Spec` fields (each with a doc comment, which the schema shows):
  - `part: Option<String>`: a piece the user may move; the code fills it like a slot of the same name.
  - `kind: Option<String>`: what a dock matches on (default `"panel"`).
  - `dock: Option<String>`: this box holds parts; its children are its default parts.
  - `accepts: Vec<String>`: part names or kinds the dock takes.
  - `zones: Vec<String>`: phase A knows `before` and `after` (others are a problem: `"<z>" is not one of before, after`).
  - `empty: Option<String>`: `hide` (default) or `keep`.
  - `strip: Option<String>`: `left` or `right`, the tool strip whose buttons show this dock's parts.
  - `handle: bool`: dragging this box drags the part it is drawn for.
  - `drag: Option<serde_json::Value>`: `true`/`false`/a condition (default true).
  - `part_size: Option<serde_json::Value>`: the size a part gets when it arrives here from elsewhere.
- New `What::Part(&'static str)` (validated against a `PARTS: &[&str] = &["explorer", "scratches", "changes"]` list,
  the same way as `SLOTS`).
- `Node` gains `dock: Option<Arc<DockNode>>`, where `pub(crate) struct DockNode { name, dir, accepts, zones,
  empty_keep: bool, strip: Option<SharedString>, part_size: Option<AbsoluteLength>, info: arrange::DockInfo }`, plus
  `handle: bool` and `drag: Option<Cond>`. A dock must also be a `split` (its `dir`); a `dock` without `split` is a
  problem, and the dock is treated as `split: "column"`.
- Produces: `impl Templates { pub fn docks(&self, template: &str) -> Vec<arrange::DockInfo>; pub fn parts(&self) ->
  Vec<(&'static str, String)> /* name, kind */; pub(crate) fn part(&self, name: &str) -> Option<&Node> }`. Parts are
  indexed by name across all templates while compiling; `defaults` comes from the dock's children (`Tree::Part`, or
  a `Split` when it has several, with `Size::Px` from a `fixed` child's `size`, else `Auto`).

- [ ] **Step 1: Write the failing tests:**
  - `docks_and_parts_compile`: compile a spec map with `"w": {"split": "row", "children": [{"dock": "left",
    "split": "column", "accepts": ["panel"], "zones": ["before","after"], "strip": "left", "children": [{"part":
    "explorer"}, {"part": "scratches"}]}]}`. Expect no problems, `t.docks("w")` gives one `DockInfo` named `left`
    with `accepts == ["panel"]` and defaults encoding `[c explorer@- scratches@-]`, and `t.parts()` contains
    `("explorer", "panel")`.
  - `part_placed_twice_is_reported`: the same part in two docks gives exactly one problem containing `placed twice`,
    and `t.parts()` lists it once.
  - Add `{"part": "nope"}`, `{"dock": "d", "zones": ["middle"]}` and `{"dock": "d2"}` (no split) to
    `mistakes_are_reported` and raise its expected count by 3.
- [ ] **Step 2:** `cargo test -p tern-app template`. Expected: FAIL.
- [ ] **Step 3:** Implement. `use` sites may override `part`'s panel settings like any node.
- [ ] **Step 4:** `cargo test -p tern-app template`. Expected: PASS.
- [ ] **Step 5: Commit** `template: part, dock and handle nodes`.

---

### Task 4: Store arrangements in the session

**Files:**
- Modify: `crates/tern-app/src/window_state.rs`

**Interfaces:**
- `SavedWorkspace` gains `pub arrangements: Vec<(String, String)>` (key, `Arrangement::encode` text); `fresh` sets it
  empty.
- One line per entry under the workspace: `\t\tarrange\t<key>\t<encoded>`, written right after `hidden`, and
  written even when the workspace has no tabs (like `hidden`). The decoder reads `arrange\t` lines; older builds
  ignore unknown lines. Check `decode_session`'s fallback branch: it treats unknown lines as tabs, and `decode_tab`
  returns `None` for `arrange`, so they are skipped. Add a doc line to `encode_session`'s comment.

- [ ] **Step 1: Write the failing tests** in `window_state.rs` tests (next to the existing session tests):
  - `arrangements_round_trip`: a `SavedWindow` with one workspace whose `arrangements` is
    `[("workspace/left+right", "left=explorer;right=[c changes@- scratches@-]")]` survives
    `decode_session(&encode_session(..))` unchanged, both with tabs and without.
  - `old_session_has_no_arrangement`: a session text with no `arrange` line decodes with `arrangements` empty and its
    tabs intact.
- [ ] **Step 2:** `cargo test -p tern-app window_state`. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test -p tern-app window_state`. Expected: PASS.
- [ ] **Step 5: Commit** `session: keep each workspace's panel arrangements`.

---

### Task 5: Draw docks from the arrangement (no dragging yet)

After this task the side panels are drawn from the arrangement, and resizing stores sizes. The look is unchanged.

**Files:**
- Create: `crates/tern-app/src/dock.rs` (`mod dock;` in `main.rs`)
- Modify: `crates/tern-app/src/template/draw.rs`, `crates/tern-app/src/template/mod.rs` (Scope),
  `crates/tern-app/src/templates.json`, `crates/tern-app/src/workspace.rs`

**Interfaces:**
- `dock.rs` produces:

```rust
pub enum Change { Move { part: SharedString, kind: SharedString, dock: SharedString, at: arrange::Drop },
                  Resize { dock: SharedString, path: Vec<usize>, sizes: Vec<arrange::Size> } }
pub type OnChange = Rc<dyn Fn(Change, &mut Window, &mut App)>;
```

- `Scope` gains `pub fn docks(self, arrangement: Arc<arrange::Arrangement>, on_change: dock::OnChange) -> Self`.
- `draw.rs`: a node with `dock` draws `arrange::prune(tree_for_dock, shown)` instead of its children. `shown(part)`
  is the part node's `when` evaluated in the scope. A leaf draws the part's `Node` (found with `Templates::part`) the
  way a slot is drawn. A split draws as today's `split()` with the tree's sizes: `Px` sets `fixed` + `size`, and
  `Auto` takes the part node's own `size`/`min`/`max`/`fixed`. When the part is not in its default dock, `Auto` uses
  the dock's `part_size` if set, and otherwise flexes. The split's `on_resize` reports `Change::Resize`, with
  `Size::Px` for fixed children and `Size::Auto` for flexible ones (they take what is left). Phase A therefore never
  writes `Size::Frac`. It stays in the format for phase B, where two flexible panels can share a split. Also
  `on_resize` of the split that holds a dock (the workspace's row) stores the dock's own width: give
  `Arrangement` a `pub widths: BTreeMap<String, f32>` (encoded as a trailing `;w:left=240,right=300`, and extend
  Task 2's round-trip test) and draw the dock's panel with that size. When the pruned tree is `None`
  and `empty` is `hide`, the dock's panel in its parent split is `visible(false)` (as a hidden `when` panel is
  today); with `keep` it is drawn empty with its own style. The split ids include the dock name and tree path, so
  gpui's resizable state follows the tree.
- `templates.json`: replace `workspace.left` and the `when`-gated panels in `workspace` with
  `{"dock": "left", "split": "column", "accepts": ["panel"], "zones": ["before","after"], "strip": "left",
  "fixed": true, "size": "$explorer.start_width", "min": 140, "max": 600, "children": [{"part": "explorer", "when":
  "explorer"}, {"part": "scratches", "when": "scratches", "fixed": true, "size": "$scratches.start_height", "min":
  90, "max": 800}]}`, and the same for `right` with `changes` (`$changes.start_width`, 180–700, `strip: "right"`).
  Delete the `workspace.left` template.
- `workspace.rs`: `Workspace` gains `arrangements: HashMap<String, arrange::Arrangement>` (restored from
  `SavedWorkspace::arrangements`, saved back in the session code that already writes `hidden`). On render, take the
  key's arrangement (create it from `Arrangement::defaults` if missing, else `reconcile` once per settings change and
  keep the problems in `arrange_problems: Vec<String>`). Pass it with `.docks(..)`. `on_change` updates the
  workspace through a `WeakEntity` (Resize: `set_sizes`, then emit `TabsChanged` so the session saves). Slots
  `explorer`/`scratches`/`changes` are given to the scope as parts (same `Scope::slot` call). The status bar's
  `settings_error` var shows settings problems and `arrange_problems` joined with `; `.
- Strips: `left_strip`/`right_strip` build their buttons from the arrangement. A part's button goes to the strip of
  the dock it is in now (`DockNode.strip`, exposed as `Templates::dock_strip(template, dock) -> Option<SharedString>`).
  Keep today's order within a strip: the order of the parts in the dock tree.

- [ ] **Step 1:** Before changing anything, launch the debug build per [[debug-app-screenshots]] (`scripts/dev-app-macos.sh`,
  `XDG_CONFIG_HOME` = a scratch dir) with explorer, scratches and changes shown, and save a screenshot as
  `before.png` in the scratchpad.
- [ ] **Step 2: Write the failing test** in `template/mod.rs` tests: `builtin_has_side_docks`: the built-in
  templates compile cleanly and `docks("workspace")` names `left` and `right` with the defaults above (encoding
  `left=[c explorer@- scratches@<px>px];right=changes`, where `<px>` is the `$scratches.start_height` default).
- [ ] **Step 3:** `cargo test -p tern-app builtin_has_side_docks`. Expected: FAIL.
- [ ] **Step 4:** Implement `dock.rs`, the drawing, the template change and the workspace wiring.
- [ ] **Step 5:** `cargo test --workspace`. Expected: PASS.
- [ ] **Step 6:** Relaunch and take `after.png` the same way. It should be pixel-identical to `before.png` (`compare -metric AE` from
  ImageMagick, or a byte-compare of the decoded pixels). Then hide explorer and scratches (⌘B, ⌘⇧S), check that
  the left column is gone, and show explorer again at its old width. Resize the left dock, quit, relaunch: the width
  is kept.
- [ ] **Step 7: Commit** `workspace: side panels drawn from the arrangement`.

---

### Task 6: Drag panels by their headers and drop them in docks

**Files:**
- Modify: `crates/tern-app/src/dock.rs`, `crates/tern-app/src/template/draw.rs`, `crates/tern-app/src/templates.json`,
  `crates/tern-app/src/style.rs` (element `drop_zone`), `crates/tern-app/src/file_tree_view.rs`,
  `crates/tern-app/src/changes_view.rs`, `crates/tern-app/src/scratches_view.rs`, `crates/tern-app/src/workspace.rs`,
  `crates/tern-app/src/settings.rs`

**Interfaces:**
- `dock.rs` adds:

```rust
#[derive(Clone)] pub struct DraggedPart { pub part: SharedString, pub kind: SharedString, pub title: SharedString }
impl Render for DraggedPart // style::drag_chip(cx).child(title)
#[derive(Default)] pub struct ActiveDrag(pub Option<DraggedPart>); impl Global for ActiveDrag {}
#[derive(Default)] pub struct LayoutLock(pub bool); impl Global for LayoutLock {}
```

  gpui keeps the active drag's value private, so the `on_drag` constructor sets `ActiveDrag`. The workspace clears it
  in `render` when `!cx.has_active_drag()`, the way it clears `tab_drag` today.
- New template `panel_header`: `{"element": "$element", "handle": true, "layout": "row", "align": "center",
  "shrink": 0, "children": [{"grow": 1, "layout": "row", "style": {"gap": "$element.count_gap"}, "children":
  [{"truncate": true, "text": "{title}"}, {"when": "count > 0", "style": {"text": "$element.count_text"}, "text":
  "{count}"}]}, {"slot": "buttons"}]}`. If `element` can't take a variable, use one `use` site per panel that sets
  `element` (`explorer.header`, `changes.header`, `scratches.header`); pick whichever the engine supports and keep
  the look identical. `style::panel_header` goes away. The three views render the header with
  `template::render("panel_header", Scope::new(..).var("part", "<name>").var("title", ..).var("count", ..).slot("buttons", ..))`.
  Add `buttons` to `SLOTS`. The element's `title` override keeps working (the view passes the element's `title`
  when set).
- `draw.rs`: a box with `handle: true`, drawn in a scope with a `part` var, gets an id
  `("handle", part)` and `on_drag(DraggedPart{..})`. This happens only when the part's `drag` condition holds and
  `LayoutLock` is off. The part's `kind` comes from `Templates::parts()`.
- While `ActiveDrag` holds a part a dock accepts, each part leaf in that dock gets an absolute overlay split in two
  halves along the dock's `dir` (`before`/`after`, only the zones the dock lists). Each half is drawn as element
  `drop_zone` and styled with `drop_zone.hover` on `drag_over::<DraggedPart>`. Its `on_drop` sends `Change::Move`
  with `Drop::Before/After(leaf)`. A dock whose pruned tree is empty (`hide`) is drawn during such a drag as a
  `drop_zone` strip of width `$drop_zone.size` (default 24) that sends `Drop::End`.
- Element `drop_zone`: `background` default `$primary/15` (or the theme's `primary` at 0.15, the same as today's pane
  hint), `border_width` 2, `border` `$primary/60`, `radius` `$radius`, `size` 24. Element `drop_zone.hover`:
  `background` `$primary/30`. The pane drop hint in `workspace.rs` (`drop_overlay`) switches to `drop_zone` too, so
  there is one look.
- `workspace.rs` `on_change` `Move`: `move_part`. If it returned true, the part is shown (set its `show_*` true),
  then `cx.notify()` and the `TabsChanged` emit (saves the session).
- `settings.rs`: `SettingsFile` gains `layout: LayoutSettings { /// Turns off moving and resizing panels. pub locked: bool }`.
  `load` sets `LayoutLock`. When locked, the drawer also passes `size_range(size..size)` for every dock panel, so
  edges can't move.
- Debug control (`workspace.rs`, next to `pane-*`): `part-move PART DOCK before|after|end [TARGET]`, which does what
  the drop does. `state` gains `arrange <encoded>`.

- [ ] **Step 1: Write the failing test** in `template/mod.rs`: `panel_header_template_compiles`: builtin has
  `panel_header`, and its root is a handle (expose `Templates::is_handle(template) -> bool` for the test only, under
  `#[cfg(test)]`).
- [ ] **Step 2:** `cargo test -p tern-app panel_header`. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test --workspace`. Expected: PASS.
- [ ] **Step 5: Verify in the app** (debug build, scratch config):
  - The headers look as in `after.png` from Task 5 (screenshot compare, panel headers region).
  - `part-move changes left before explorer` → screenshot: Changes is above Explorer on the left, the right column
    is gone, and the Changes button is in the left strip.
  - Drag a header by hand with the mouse (cliclick or the control socket's mouse commands, if any) and confirm that
    the zones show and the drop works. If driving a real drag isn't possible from the shell, say so in the report;
    don't claim it was checked.
  - `"layout": {"locked": true}` in the scratch `settings.json`: headers don't start a drag and split edges don't
    move.
  - Quit and relaunch: the arrangement is kept.
- [ ] **Step 6: Commit** `dock: drag panels between docks`.

---

### Task 7: Reset Layout, docs

**Files:**
- Modify: `crates/tern-app/src/workspace.rs`, `crates/tern-app/src/menus.rs`, `crates/tern-app/src/settings.rs`
  (README), `docs/superpowers/specs/2026-10-16-ui-templates-design.md` (mark phase A done)

**Interfaces:**
- Action `workspace::ResetLayout`: removes the current key's arrangement (the next render rebuilds it from the
  defaults), then notify and save. It goes in the View menu after the panel toggles, and in the command palette if
  actions are listed there.
- README section "Moving panels": `part`, `dock` (`accepts`, `zones`, `empty`, `strip`, `part_size`), `handle`,
  `drag`, `layout.locked`, Reset Layout, and that arrangements are kept per workspace and per template.

- [ ] **Step 1: Write the failing test** in `arrange.rs`: `reset_gives_defaults`: after moves, replacing the arrangement
  with `Arrangement::defaults(key, docks)` encodes as the defaults. (This pins that defaults don't read the stored
  state; the action itself is checked in the app.)
- [ ] **Step 2–4:** Implement. `cargo test --workspace` PASS. In the app: `part-move …`, then View ▸ Reset Layout →
  screenshot matches `after.png`.
- [ ] **Step 5:** Update the spec (phase A: done) and the theming handoff doc with a short "Part 12: movable panels"
  section: what was built, the control commands, what's left (phases B–D).
- [ ] **Step 6: Commit** `workspace: Reset Layout; document moving panels`.
