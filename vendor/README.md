# Patched dependencies

Crates here replace their crates.io release through `[patch.crates-io]` in the
workspace `Cargo.toml`. Each is the published source with `examples/` (and
their `[[example]]` entries) left out, plus the changes listed below. Drop a
crate from here once a release carries its fix.

## gpui-pre 0.3.9

`src/elements/list.rs`, `ListElement::prepaint`: when the list's width
changes, the first layout included, every item became `Unmeasured` with no
size hint, so items not yet drawn counted as zero height. The scroll range
then covered only the visible items, and since a list with no overdraw
measures only what it shows, scrolling stopped at the first screen for good.
It also threw away hints set with `ListState::with_uniform_item_height`
before the first layout, which that method exists for. gpui-component's
`Diff` relies on exactly that, so a diff longer than its view could not be
scrolled. Items now keep their last height (or hint) as the hint until they
are measured again. Zed's `main` has the same code. Test:
`test_uniform_height_hint_survives_first_layout_and_width_change` (the
published crate's tests need Zed's workspace to build).
