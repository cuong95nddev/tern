# Screenshot annotate — implementation plan

Spec: `docs/superpowers/specs/2026-10-15-screenshot-annotate-design.md`.
Each task ends with `cargo fmt`, `cargo clippy --workspace --all-targets`,
`cargo test --workspace` green, and a commit.

1. **Deps + skeleton**: add `tiny-skia = "0.11"`, `image = { version = "0.25", default-features = false, features = ["png"] }`, `ab_glyph = "0.2"`, `global-hotkey = "0.8"` (macOS target) to `tern-app`; empty `shot` module behind `#[cfg(target_os = "macos")]`.
2. **model.rs** (TDD): shapes, style, `Doc` with undo/redo, marker numbering, geometry helpers, hit test.
3. **raster.rs** (TDD): crop + mosaic + shapes + text/markers; font loading.
4. **grab.rs**: permission FFI, `grab_displays`, shot dir, `save_png`, `cleanup_old` (tested with a temp dir).
5. **targets.rs**: `paste_plan()` (TDD), `Target` enumeration, `deliver()`, new-tab + readiness wait; pub APIs on `MainWindow`/`Workspace`/`TerminalView`.
6. **overlay.rs**: overlay window per display, selection/handles, toolbar, drawing tools, text entry, add/copy/save/cancel.
7. **tray.rs**: tray panel, thumbnails, prompt input, target dropdown, capture-more, send.
8. **mod.rs wiring**: hotkey, menu item, startup cleanup; README shortcut row.
9. **Manual check** in a dev build; fix what it finds.
