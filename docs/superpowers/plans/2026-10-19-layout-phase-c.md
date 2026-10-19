# Floating Parts in the Window (layout phase C) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A panel can leave every dock and float over the workspace: drop it outside all drop zones, or pick
"Float" in its header's menu. It moves by its handle, resizes from its edges and corners, docks again by "Dock" in
its menu or by being dropped on a zone, and its place is stored with the arrangement.

**Architecture:** The arrangement (`arrange.rs`, pure data) gains the floating parts with their rects; the drawer
draws them in a layer over the root of the template that holds the docks; the workspace's root box takes the drops
no zone took (gpui hands a drop to the innermost target, which takes the drag) and tracks the pointer for live moves
and resizes.

**Tech Stack:** Rust, gpui (gpui-kit 0.7.3), templates (`templates.json`).

**Spec:** `docs/superpowers/specs/2026-10-16-ui-templates-design.md`, "User-arranged layout", phase C.

## Global Constraints

- Templates stay renderer-neutral: `float` is a condition like `drag`; the `floating` element's radius and shadow
  are GUI only.
- UI goes through templates: the part menu is `menu.part`; floating boxes are styled by element `floating`.
- Code and comments in English, comment density like the surrounding code.

## Calls made (recorded in the spec when done)

- A floating part is stored as `f:<part>=<dock>@<x>,<y>,<w>,<h>` (one `;` segment per part, after the docks, with
  `w:`): `dock` is the dock it came from (its strip button stays there, "Dock" sends it back there), `x`/`y` are
  fractions of the area (so a smaller window keeps it in view), `w`/`h` pixels.
- The floating layer is over the root of the template whose scope has the arrangement (`workspace`), last child,
  clipped. No new node property.
- Floating a part (drop or menu): its size is the `floating` element's `width` / `height` (defaults 300 × 400); on a
  drop its top-left sits where the grabbed point of its handle was, so the pointer keeps holding the same spot.
- A floating part being dragged follows the pointer itself (its drag chip draws nothing) and doesn't block the
  zones under it, so dropping it on a zone docks it; dropped anywhere else, it stays where it was let go.
- Releasing a docked part on its own area (no zone there: it is alone in its dock) does nothing, so a short drag
  of a header doesn't float it by accident.
- Sizes are kept between `floating.min_width` / `min_height` (defaults 120 × 80) and the area; a move keeps the
  rect inside the area.
- Menu: right-click on a part's handle opens `menu.part` (`float` / `dock` buttons, `when` on `floating`), whose
  items dispatch `dock::FloatPart` / `dock::DockPart` (actions with the part's name: the header is drawn by the
  panel's own view, which can't reach the workspace's `OnChange`).
- `float: false` (or a condition) on a part forbids floating: such parts are in the `Grounded` global, like
  `PinnedParts`. `layout.locked` stops floating, moving and resizing floating parts (they stay where they are).

## Tasks

1. `arrange.rs`: `Float { from, x, y, w, h }`, `Rect`, `Sides`, `Arrangement::floating`, `float_part`,
   `dock_part`, `set_float_rect`; `move_part` takes a part out of `floating`; `reconcile` keeps floating parts
   (dropping unknown ones, a `from` dock gone falls back to the default dock); encode/decode `f:`; `moved`,
   `resized` (pure rect math, clamped). Unit tests: round trip, float then dock back to `from`, move of a floating
   part into a dock, reconcile with a missing part and a missing dock, clamping on move and resize.
2. Templates: `float` on parts (compiled like `drag`), `Templates::grounded`, element `floating` (+ defaults),
   built-in `menu.part`. Test: `float` compiles and `grounded` reads it.
3. Drawing (`template/draw.rs`, `dock.rs`): `DraggedPart.floating`, `FloatTrack` + `Resizing` globals,
   `FloatResize` drag, the floating layer (live position while moved or resized, edge/corner handles, occlude when
   still), the handle's context menu, the no-op catcher on a lone part's own area. `Change::Float`, `Change::Dock`.
4. Workspace: root box `on_drag_move` / `on_drop` for `DraggedPart` and `FloatResize`, the two actions, strip buttons
   for floating parts (in their `from` dock's strip), clearing the globals when a drag ends; control command
   `part-float PART X Y W H` / `part-dock PART` for checks.
5. Docs (settings reference in `settings.rs`, spec phase C "Done"), `cargo test`, clippy, fmt; a Windows run: float
   by drop and menu, move, resize, dock again, restart restores it, default look unchanged.

## Review Focus

- A window made smaller than a floating part's stored place: the part must stay reachable (x/y are fractions,
  size capped by the area when drawn).
- Hiding a floating part with its strip button and showing it again: it comes back floating, same place.
- A template switch that drops the part's `from` dock: the part keeps floating and "Dock" sends it to its default.
- Clicks on a floating panel must not reach the panel under it (occlude when not being dragged).
- `layout.locked` while a part floats: it stays drawn, no handles, no drag.
