# Kitty graphics (Unicode placeholders) for Tern — design

Date: 2026-10-15
Status: approved in chat, awaiting written-spec review

## Goal

Programs running in Tern can show real images using the kitty graphics protocol in its
Unicode-placeholder form (virtual placements, `U=1`). First consumer: Claude Code, which
then draws pasted images and other pictures inline instead of their alt text. A small
Claude Code mod previews pasted images above the prompt.

Success: in a dev build of Tern, run `claude`, paste a screenshot → a thumbnail of the
screenshot appears above the prompt, scrolls with the text, and disappears after submit.
`kitten icat --unicode-placeholder <png>` (if kitty is installed) shows the image too.

## Scope

In:
- Transmission: `a=t`, `a=T`; formats `f=100` (PNG), `f=24` (RGB), `f=32` (RGBA);
  media `t=d` (direct, chunked with `m=1`), `t=f` (file), `t=t` (temp file), `t=s` (POSIX
  shared memory); optional `S`/`O` (size/offset) for `t=f`/`t=s`.
- `a=q` (query: validate, never store), `a=d` (delete: `d=a/A`, `d=i/I`), `a=p` with `U=1`
  (virtual placement, records `c`×`r`).
- Responses `ESC _ G i=<id>[,I=<n>];OK|<ECODE>:<msg> ESC \`, honouring `q=1` / `q=2`.
- Rendering of U+10EEEE placeholder cells.
- A setting to turn the feature off.
- The Claude Code mod (second sub-project, after the Tern part works).

Out (YAGNI): direct placements without `U=1` (`icat` default mode, yazi, timg), animation
(`a=f/a=a/a=c`), z-index, relative placements, compressed payloads (`o=z`), sixel, iTerm2
`OSC 1337`. A direct placement request is answered with `ENOTSUPPORTED` rather than ignored.

## Architecture

```
PTY bytes ─▶ TapReader (tern-term/session.rs) ─┬─▶ OscScanner (existing)
                                               └─▶ graphics::ApcParser
                                                        │ complete commands
                                                        ▼
                                                 graphics::ImageStore  (Arc<Mutex>, per session)
                                                        │ reply bytes ─▶ PTY (same path as OSC 7501 probe reply)
alacritty Term grid (cells holding U+10EEEE) ─▶ TerminalView::build_frame (tern-app)
                                                        └─ looks up ImageStore ─▶ window.paint_image
```

`alacritty_terminal`'s VT parser already discards APC strings, so the tap needs no change to
alacritty. Images are transmitted before their placeholder cells are printed, and the tap
runs before alacritty parses the same bytes, so the image is stored by the time the cells
render.

### Units (tern-term, no UI dependency, unit-tested)

- `graphics/apc.rs` — `ApcParser`: streaming state machine over raw bytes, emits
  `ApcCommand { keys: Vec<(u8, Value)>, payload: Vec<u8> }` for `ESC _ G … ESC \`. Survives
  splits at any byte across `read()` calls. Caps one command at 32 MiB of payload (excess →
  dropped, `EFBIG` reply once the terminator arrives).
- `graphics/command.rs` — turns `ApcCommand` into a typed `Command` (action, ids, format,
  medium, size, offset, placement cols/rows, quiet, more). Unknown keys ignored; bad values →
  `EINVAL`.
- `graphics/load.rs` — gathers chunks (`m=1`) per image, base64-decodes, reads the medium,
  decodes to RGBA8:
  - PNG via the `image` crate (0.25, already in Cargo.lock), PNG feature only.
  - RGB/RGBA raw: requires `s`,`v`; length must equal `s*v*3|4`.
  - `t=f`: absolute path, regular file only, not under `/dev`, `/proc`, `/sys`, size ≤ 64 MiB.
  - `t=t`: same, and deleted after reading only if it is under the system temp dir and its
    name contains `tty-graphics-protocol` (kitty's rule).
  - `t=s`: `shm_open` read-only, `fstat`, `mmap`, copy, `shm_unlink`.
  - Decoded image ≤ 10000×10000 px.
- `graphics/store.rs` — `ImageStore`: `id → Image { rgba: Arc<[u8]>, width, height,
  generation, placements: Vec<VirtualPlacement { id, cols, rows }> }`; number→id map for
  `I=`; total decoded bytes capped at 256 MiB per session, least recently used evicted first.
  `generation` increments on every replace so the UI cache knows to refresh.
- `graphics/reply.rs` — formats responses; applies `q`.
- `graphics/placeholder.rs` — the 297-entry row/column diacritic table from the kitty spec,
  `diacritic_index(char) -> Option<u16>`, and `PlaceholderCell::decode(fg, zerowidth, prev)`
  implementing the inheritance rules (missing row/col/MSB taken from the cell to the left
  in the same row: same row, column + 1, same id).

The store is created in `Session::spawn`, shared with `TapReader`, exposed as
`Session::images() -> &Arc<Mutex<ImageStore>>`, and dropped with the session.

### Rendering (tern-app/terminal_view.rs)

- While `build_frame` walks the visible cells, a cell whose char is U+10EEEE is not drawn as
  text. Image id: 24-bit fg colour → `r<<16|g<<8|b`; 256-colour fg → the index; third
  diacritic, if any, is the most significant byte. Placement id from the underline colour
  (same encoding), 0 when absent.
- Consecutive placeholder cells on one row with the same image/placement and consecutive
  columns form a run. For each run: placement box = `cols×rows` cells from the store (fallback:
  the image's natural size in cells); the image is fitted into the box keeping aspect ratio and
  centred (kitty behaviour); the whole fitted image is painted with its origin at
  `run_origin - (col, row) * cell_size`, inside a content mask equal to the run's rectangle.
- Missing image (not yet arrived, evicted, error) → the run stays blank.
- `TerminalView` caches `Arc<RenderImage>` (BGRA) keyed by `(image id, generation)`; entries
  not used in a frame for 10 s are dropped.

### Setting

`TerminalConfig.graphics: bool`, default `true`; `TERN_GRAPHICS=0` in Tern's own environment
at launch sets it to `false`. When off, the parser still consumes APC (alacritty ignores it
anyway) but sends no replies and stores nothing, so programs see an unsupporting terminal and
fall back to text.

## Claude Code mod (sub-project 2)

- Location: `~/.claude/dev-mods/<session>/paste-preview/` while developing; later a plugin in
  its own repo if wanted.
- `prompt.change` hook: find `[Image #N]` tokens in the draft; for each, locate
  `images/N.png` in the current Claude Code session's temp directory; keep the list in
  `$.state`.
- `ui.render` on `AbovePrompt`: up to 3 `Image` elements (`{ file, format: 'png' }`, ~8 rows
  tall, width from the PNG's aspect ratio, max 40 columns) with `alt` = "Image #N"; nothing
  when the list is empty, so the band vanishes after submit.
- First task is a spike: confirm the mod can learn the session temp directory (session id +
  uid + project slug) through `$`; if not, fall back to scanning the newest `images/` dir
  under `/private/tmp/claude-<uid>/<cwd slug>/`.
- The temp path is undocumented Claude Code behaviour: if it changes the mod shows nothing;
  it never writes or deletes there.

## Error handling

- Malformed or oversized commands never panic the PTY thread; they produce an error reply
  (subject to `q`) and are dropped.
- I/O for `t=f`/`t=t`/`t=s` runs on the PTY thread; it is bounded by the size caps. If this
  shows up as lag in practice, move loading to a worker thread (not in this iteration).
- Poisoned store mutex → feature disables itself for that session, logs once.

## Testing

Unit tests (tern-term):
- APC parser: whole command, split at every byte offset, nested ESC in payload, overflow.
- Command parsing: every supported key, bad values.
- Loading: PNG, RGB, RGBA (sizes match / mismatch), chunked base64, `t=f` regular file and
  refusals (`/dev/zero`, directory, too large), `t=t` deletion rule, `t=s` round trip.
- Replies: exact bytes for OK/error under `q=0/1/2`, `I=` echo.
- Store: replace bumps generation, delete, LRU eviction at the cap.
- Placeholder decoding: diacritic table spot checks, 24-bit and 256-colour ids, MSB, the
  inheritance rules.

Manual (dev build, `scripts/dev.sh` + control socket for screenshots):
- `claude`, paste a screenshot → thumbnail above the prompt; scroll back; resize the pane;
  `/clear`.
- `TERN_GRAPHICS=0` → Claude Code shows its alt text as before.
- `printf` a hand-built query → `OK` reply.

Gates: `cargo fmt --check`, `cargo clippy --workspace --all-targets`, `cargo test --workspace`,
`cargo build --release` all clean.

## Notes

- Public repo: code and commits cite only the kitty spec
  (https://sw.kovidgoyal.net/kitty/graphics-protocol/), not other programs' internals.
- The main tree currently has another session's uncommitted file-tree work; implementation
  should run in its own worktree/branch.
