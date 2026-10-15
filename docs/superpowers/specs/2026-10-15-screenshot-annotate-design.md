# Screenshot, annotate and send to an agent — design

Date: 2026-10-15. Branch `screenshot` (off `main` 0.1.2). macOS only; the
feature compiles out elsewhere.

## Goal

Capture any part of the screen (any app), mark it up, collect several
captures, add a prompt, and hand all of it to a coding agent (Claude Code,
Codex, OpenCode, …) running in a Tern tab, or in a new one, in one step.

Success: ⌘⇧2 anywhere → drag a region → draw an arrow and a box → ↵ adds it
to the tray → capture another → type "fix these" → ⌘↵ → the agent tab
receives both images and the prompt, and the turn starts.

## User flow

1. **Trigger**: global hotkey ⌘⇧2 (works while Tern is in the background), or
   *Tern ▸ Capture Screenshot*, or the camera button in the tray.
2. **Freeze**: every display is grabbed (`/usr/sbin/screencapture -x -R
   x,y,w,h <png>`, one per display, run in parallel), then a full-screen
   overlay (`WindowKind::PopUp`, level 101, above the menu bar and Dock)
   shows the frozen image of its display, dimmed.
3. **Select**: drag a region; a label shows its size in points. The region
   can be moved (drag inside before drawing) and resized with eight handles.
   Esc cancels the whole capture.
4. **Annotate** (toolbar under the region, above it when there is no room):
   rectangle, ellipse, line, arrow, pen, text, numbered marker (①②③,
   auto-increment), mosaic; stroke width (3 sizes) and 8 colours (red,
   blue, green, amber, pink, black, gray, white); undo/redo (⌘Z / ⌘⇧Z);
   cancel ✕; copy to clipboard; save as…; **Add to tray** ✓ (↵).
5. **Tray**: a small floating panel (bottom right of the main display, above
   all apps) with the thumbnails (× removes one), a multi-line prompt, the
   target picker, a 📷 button for another capture, and **Send** (⌘↵).
6. **Send**: the PNGs (already written to `~/Library/Caches/dev.tern/shots/`)
   are pasted into the target as file paths, then the prompt, then Enter.
   The tray clears and hides. Shots older than 7 days are deleted at launch.

## Target picker

Lists every terminal tab of every window: agent logo (or terminal icon),
`workspace › tab title`. Default: the agent tab focused most recently
(else the active tab of the frontmost window). Extra entries: **New Claude
tab**, **New Codex tab**, **New OpenCode tab** — in the workspace of the
default target (or the frontmost window's active workspace).

- New Claude tab: the normal Claude tab (`TabKind::Claude`).
- New Codex / OpenCode tab: a shell tab, then `codex` / `opencode` + Enter
  typed into it.
- A new tab is used once the agent is ready: it is the detected foreground
  agent *and* it has turned on bracketed paste, then 500 ms without output.
  Timeout 20 s → the tray shows an error and keeps the shots.

## Delivery sequence (all agents)

Pasting is what all three agents understand: each turns a pasted image
path into an attachment. So, into the target terminal:

1. for each image: a bracketed paste of its escaped path plus a space,
   then 120 ms;
2. a bracketed paste of the prompt (if any), then 250 ms;
3. `\r` (submit).

Without bracketed paste the text is written raw (as `paste_text` does).
Pastes reuse `TerminalView::paste_text` (control chars stripped).

## Architecture

New module `crates/tern-app/src/shot/` (macOS):

| File | Role | Depends on |
|---|---|---|
| `model.rs` | `Shape` (Rect, Ellipse, Line, Arrow, Pen, Text, Marker, Mosaic), `Style`, `Doc` (shapes + undo/redo), geometry (arrow head polygon, ellipse polyline, rect normalise, hit tests). Image-pixel coordinates. Pure, unit-tested. | — |
| `raster.rs` | `render(base: &RgbaImage, crop, &Doc) -> RgbaImage`: crop, mosaic (block averages of the base), shapes via `tiny-skia`, text and markers via `ab_glyph` with the system font (SF, fallback Helvetica). Unit-tested on small images. | model |
| `grab.rs` | Screen Recording permission (`CGPreflightScreenCaptureAccess` / `CGRequestScreenCaptureAccess`), `grab_displays()` with `screencapture`, shot dir, cleanup, PNG writing. | — |
| `overlay.rs` | The overlay view: frozen image, dim mask, selection, handles, toolbar, live drawing with GPUI `PathBuilder`/quads/text using the same geometry functions as `raster`. | model, raster, grab |
| `tray.rs` | The tray view + window; global `Tray` state (shots, prompt, target). | targets |
| `targets.rs` | `Target` (window handle, workspace entity, tab entity id) enumeration and `deliver()`: the paste sequence, new-tab creation and the readiness wait. Uses small new pub APIs on `MainWindow`/`Workspace`. | workspace |
| `mod.rs` | `init(cx)`: hotkey (`global-hotkey` 0.8, handler → flume → main thread), menu action `CaptureScreenshot`, `start_capture(cx)`. | all |

The editor draws with GPUI and the export rasterises with tiny-skia; both
use `model`'s geometry (rect corners, arrow head polygon, ellipse polyline,
pen polyline, marker circle radius = 4 × stroke + 6) so they match.

Coordinates: the overlay works in window points; `Doc` stores image pixels
(points × the display's backing scale = image width / display width).

## Error handling

- Permission missing: `CGRequestScreenCaptureAccess()` (system prompt once),
  then a Tern notification "Allow Tern under Privacy & Security ▸ Screen
  Recording" with an *Open Settings* action
  (`x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture`).
- `screencapture` fails / empty file: notification, no overlay.
- A second trigger while the overlay is up: ignored.
- Target tab closed before sending: the picker falls back to the default;
  Send with no terminal at all creates a new Claude tab in a new window's
  workspace only if a window exists, else shows an error.
- Writing PNG fails: notification, shot not added.

## Testing

Unit: model (normalise, arrow head, hit tests, undo/redo, marker numbering),
raster (crop size, a red rect's border pixels are red and interior is the
base, mosaic blocks are uniform, text draws non-base pixels), delivery byte
sequence builder (`paste_plan()` → list of (bytes, delay)), shot cleanup by
age. UI: by hand in a dev build (`scripts/dev.sh`).

## Out of scope (v1)

Global hotkey customisation, window-snapping selection, scrolling capture,
OCR, pin-to-screen, cross-display selection, Windows/Linux.
