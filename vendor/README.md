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

`src/svg_renderer.rs` and `Window::paint_svg`: the sprite atlas keeps an SVG by
its path and size for good, so an asset that changed under its path kept its old
picture. `RenderSvgParams` has a `generation`, and `bump_svg_generation` makes
every SVG render again. Tern's `icons` settings use it.

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

## gpui-pre-macos 0.3.9

Left out: `Cargo.lock`.

`src/system_notifications.rs`, `NotificationCenter::show` and
`will_present_notification`: a notification's content had no sound, so every
one arrived silently (an agent that finished or needed an answer made no
sound), and the delegate let a notification shown while the app is in front
have a banner but no sound. The content now has the default sound and the
delegate presents it with `Sound`. `Cargo.toml` turns on
objc2-user-notifications' `UNNotificationSound` for it.

## gpui-component 0.7.3

Left out: `Cargo.lock`.

`src/title_bar.rs`: `TitleBar::controls(false)` leaves out the window controls,
so an app can draw them where it likes.

`src/diff/mod.rs`: `Diff::change_colors` sets the colors of added and removed
lines, which were always the theme's `success` and `danger`.

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

## gpui-base 0.7.3

`src/input/`: `set_caret` (bar, block or underline, its thickness, whether it
blinks) and `set_gutter_padding` on an input's state, which had these as
constants.

Left out: `Cargo.lock`.

`src/input/editor/gutter_markers.rs` (new), `src/input/base/element.rs`
(`layout_line_numbers`, `layout_gutter_markers`), `src/input/base/kind.rs`
(`InputExtras::gutter_markers_*`): the code editor had no way to put an icon
in a line's gutter, which file tabs need for "is implemented by" / "overrides"
markers (as in IntelliJ). `EditorState::set_gutter_markers` takes markers by
line; each is tracked by its line's byte range through edits (with the same
`adjust_range_for_edit` the decorations use), so it stays on its line while
the user types and goes with the line when deleted. They are drawn in a
16px column between the line numbers and the fold icons; a click calls the
host's `GutterMarkerHandler`, a hover shows the host's tooltip view.

`src/input/editor/lsp/definitions.rs`, `handle_click_hover_definition`, and
`Lsp::alt_click`: a ⌘⌥-click (Ctrl+Alt on Windows / Linux) on the text was
ignored; it now calls `Lsp::alt_click` with the clicked offset when the host
set one (go to implementations).

The crate's own tests need its dev-dependencies (criterion, gpui-fast), which
are not built here, so these changes are covered by the app's tests and use.

## gpui-pre, gpui-pre-apple, gpui-pre-macos and gpui-pre-windows 0.3.9: `window-capture`

A new feature, `window-capture`, compiles `Window::render_to_image` (and the
platform window's and the Metal and DirectX renderers' `render_to_image`
under it) outside `test-support`: `src/window.rs`, `src/platform.rs`,
`src/metal_renderer.rs` (and its `read_texture_to_image`), the macOS and
Windows `src/window.rs`, and `src/directx_renderer.rs` take
`feature = "window-capture"` in their `cfg`s, and each crate's `Cargo.toml`
declares it (passing it on to the crates under it). Tern turns it on so an
agent can see the window as it was last drawn (`tern --shot`),
without screen recording permission and even when the
window is covered. The code is unchanged.
