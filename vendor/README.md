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

## gpui-pre-apple 0.3.9 and gpui-pre-windows 0.3.9

`src/metal_renderer.rs` (`build_pipeline_state`, `build_path_sprite_pipeline_state`)
and `src/directx_renderer.rs` (`create_blend_state`,
`create_blend_state_for_path_sprite`): the blend kept the destination's alpha
whole (`One`), so each layer's alpha was added to what was under it rather
than laid over it. In an opaque window that changes nothing, but in a
see-through one (`WindowBackgroundAppearance::Transparent` or `Blurred`) a
few half-transparent backgrounds over each other came out fully opaque: a
panel at 30% over the window at 30% over the workspace at 30% was solid. The
destination's alpha now goes by `OneMinusSourceAlpha`, the usual "over", as
the wgpu renderer (Linux) already does with `PREMULTIPLIED_ALPHA_BLENDING`.

## gpui-component 0.7.3

Left out: `Cargo.lock`.

`src/text/compat.rs`, `TextView::image_source`: the component's `TextView`
wraps gpui-base's and forwards its builder methods, all but `image_source`.
Without it an image's URL went to GPUI's loader, which only fetches over
HTTP, so a Markdown preview showed none of a note's own images
(`![](img/shot.png)`). The method forwards to the base view's, as
`on_link_click` does.

## alacritty_terminal 0.26.0

Left out: `tests/` (46 MB of reference recordings) and its `[[test]]` entry.

`src/term/mod.rs`, `Term::input` / `Term::widen_for_emoji`: a character that
is one cell wide alone but has an emoji presentation (☁, ❤, ✔) followed by
VS16 (U+FE0F) stayed one cell wide, with VS16 kept as a zero-width char, so
the two-cell emoji glyph covered the next character. Programs that measure
text with Unicode's emoji rules (Claude Code through Node's `string-width`)
count it as two cells, so their cursor also drifted. It now becomes a wide
char with its spacer, as in kitty, WezTerm, Ghostty and Warp; only when the
cursor still sits right after it (not one in the last column). Test:
`vs16_widens_text_emoji`.
