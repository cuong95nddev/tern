Discuss with me in Vietnamese
The code and comment allway in English
New UI (and any UI you rework) must go through the template render engine
(`crates/tern-app/src/template/`, built-ins in `templates.json`): structure and
look in a template, behavior attached in code by `frame`/`slot`. Do not build new
UI by hand with `div()`. See `docs/superpowers/specs/2026-10-16-ui-templates-design.md`.

Build disk use: run every check (fmt, clippy, tests, the pre-push CI checks) in
the repo's own `target/`, never a separate `CARGO_TARGET_DIR` (one in the
scratchpad rebuilds everything, gigabytes each time). The local `stable` must be
CI's Rust: if `rustc --version` is older than CI's, `rustup update stable` first.
Do not `cargo clean` before builds (it loses the cache); if `target/` grows past
~40 GB, tell the user and suggest `cargo clean`.
