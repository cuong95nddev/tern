# Parts in Their Own Windows (layout phase D) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A panel can leave the main window for an OS window of its own ("Move to New Window" in its header's
menu). The window is a normal one (title bar, moved and resized by the system); closing it docks the panel again,
"Dock" and "Float" in its header's menu bring it back to the main window. Where it is is stored with the arrangement
and the window comes back at the next launch.

**Architecture:** The arrangement's floating parts gain a `window` (the display it is on): such a part is in its own
OS window, its rect in pixels on that display. The workspace keeps one `PartWindow` per such part, opening and
closing them to match the arrangement as it draws (so hiding the part with its strip button closes the window and
showing it opens it again). A `PartWindow` draws the part with the workspace's values (template `part_window`
around the part's node) and forwards the part menu's actions to the workspace.

**Tech Stack:** Rust, gpui (gpui-kit 0.7.3), templates (`templates.json`).

**Spec:** `docs/superpowers/specs/2026-10-16-ui-templates-design.md`, "User-arranged layout", phase D.

## Calls made (recorded in the spec when done)

- **`window` on a part** (`true`, `false` or a condition, default `true`) says whether it may move to a window of
  its own. The spec's `float: "window"` would read `"window"` as a condition on a value named `window`, and a
  default of "no" would hide the feature from every built-in template. GUI only.
- Stored as `p:<part>=<dock>@<display>@<x>,<y>,<w>,<h>` among the floating parts: `display` is the display's uuid
  (`-` when unknown), `x`/`y`/`w`/`h` the window's content bounds in pixels on it (as `window_state` stores main
  windows). A display that is gone, or bounds off it, open the window centered on the main window's display.
- The window opens at the size the part had (floating: its rect; docked: the `floating` element's `width` /
  `height`), its top-left under the pointer for the menu, or where the control command says.
- Closing the window (its close button) docks the part again. Hiding the part (strip button, View menu) closes the
  window and keeps the entry; showing it opens the window again. Closing the workspace (or its main window) closes
  its part windows and keeps the entry, so they come back with the session.
- A popped part's header doesn't drag (gpui drags don't cross windows); its right-click menu gives Float (into the
  main window) and Dock. `layout.locked` keeps the window where it is and hides those items, like for floating
  parts.
- Part windows don't keep Tern running: with only part windows (and the agent monitor) left, Tern quits, as when
  the last main window closes.
- Part windows of a workspace open when the workspace is drawn (its tab shown), and stay open when another
  workspace tab is shown; the title says whose (`Explorer — tern`).
- Keys the workspace handles (⌘B, ...) reach it only from its main window; the part's own keys work in its window.

## Tasks

1. `arrange.rs`: `Float.window: Option<String>`; `pop_out(part, display, rect)` (also moves a popped one);
   `float_part` from a window keeps `from`; encode/decode `p:`; tests (round trip, malformed, pop out from a dock,
   from floating, float back, dock back).
2. Templates: part `window` (compiled like `float`), `Templates::windowless`; template `part_window` (element
   `part_window`, slot `part`); `menu.part` gains `window` (Move to New Window) and shows Float/Dock by
   `floating`, `popped`, `can_float`, `can_window`; `template::render_part`.
3. `dock.rs`: `PopOutPart` action, `Change::Window`, `Floats.popped` / `.windowless`; the drawer skips popped parts
   in the floating layer and gives their handle a menu with no drag.
4. `part_window.rs`: `PartWindow` (draws, forwards actions, bounds → arrangement after 400 ms, close → dock,
   closes with the workspace), the `PartWindows` global; `main.rs` quits when only part windows are left.
5. Workspace: `part_windows`, reconciled in `render`; `pop_out` action and menu handler; control commands
   `part-window PART X Y W H` and `part-windows` (lists them); strip buttons for popped parts.
6. Docs (settings reference, spec phase D "Done"), `cargo test`, clippy, fmt; a Windows run: pop out by menu and
   control, move/resize the window and restart, close docks it, hide/show, default look unchanged.
