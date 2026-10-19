# Center as a Dock, `edges` and `stack` (layout phase B) Implementation Plan

**Goal:** One drag-and-drop system for panels and panes. Panels can be dropped on a panel's side (split it, also
across the dock's axis) or onto it (stack them as tabs in one area, a `dock_tabs` row picks the shown one). The
center is a dock in the template (`"dock": "center"`), so the template decides whether tabs and panes may split
(`edges`) or join (`stack`) panes; a whole pane moves by dragging its tab bar's empty space.

**Spec:** `docs/superpowers/specs/2026-10-16-ui-templates-design.md`, "User-arranged layout", phase B.

## Calls made (recorded in the spec when done)

- `Edge` and `Zone` (and `Zone::at`) move from `panes.rs` to `arrange.rs`: the zone geometry is the docks'.
- The dock named `center` is the pane dock: its tree is the pane layout (`panes::Layout`), kept by the code and
  stored as today; it has no strip, no split and no parts; it draws the code's `center` slot. It is left out of the
  arrangement (so stored arrangements keep their key).
- Mixing panels into the center (or panes into a side dock) is left for later: the center's tree only has panes.
- A stack is stored as `[s a *b c]` (`*` marks the shown part); a stack holds parts only. A part dropped on an edge of
  a stacked part splits around the whole stack.
- The hint under the pointer is one global (`dock::DropHint`), updated by `on_drag_move`, shared by panes and docks.
  The overlay is `dock::zone_overlay`, generic over the drag payload.
- A whole pane drags as a `DraggedPart` of kind `pane` (part `pane:<id>`). Dropped on a pane's side: the pane
  moves there; onto a pane: its tabs join that pane. Only when the center `accepts` `pane`.

## Tasks

1. `arrange.rs`: `Edge`, `Zone` (from `panes.rs`), `Tree::Stack`, `Drop::Edge`, `Drop::Stack`, placing, pruning,
   `Shown::Stack`, `set_active`, encode/decode; unit tests.
2. Templates: zones `edges`, `stack`; the `center` dock; `dock_tabs` (+ `.tab`) built-in; `Templates::center_dock`.
3. Drawing: `dock::zone_overlay` + `DropHint`; leaves with `edges`/`stack`; stacks with `dock_tabs`; `Change::Activate`.
4. Workspace: panes use `zone_overlay` and the center's zones; pane drag by the tab bar's empty space;
   `Layout::move_pane`; control command for checks.
5. Docs: settings reference, schema, spec. `cargo test`, clippy, fmt; a Windows run (default look unchanged).
