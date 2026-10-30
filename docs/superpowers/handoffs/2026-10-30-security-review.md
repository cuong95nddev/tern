# Handoff: security review of Tern (2026-10-30)

Discuss with the user in Vietnamese; code and comments in English. New UI goes through templates. Run every
check in the repo's own `target/` (CLAUDE.md). Fix at the root, no workarounds (memory `no-workarounds`); check
`../refs/` (Orca, T3 Code) for how others solved the same thing.

## Task

The user asked for a security review of Tern, continuing in a fresh session. Review the attack surfaces below
(most exposed first), confirm each finding concretely (a reachable path with untrusted input, not a pattern match),
fix it at the root with a test, and report what was found and what was ruled out.

## Done already (don't redo)

- `7f57b1f` Chat: only `http`/`https` open from what Claude or an MCP server sent (MCP form URLs, links in
  replies/plans). `chat_view.rs`: `web_url`, `markdown()` wraps `TextView::markdown` with `on_link_click`.
- `ec8957d` `c0c8c4a` clicked terminal images written only into a folder that is ours (user cache).
- `b299c77` `e37fafc` kitty graphics: read only files/shm written for the image; size shm by the image alone.
- `02a412b` Windows: cmd/PowerShell-safe path quoting, pinned installer thumbprints.
- CI greps for `-EncodedCommand`-style strings and `"hidden"` (`.github/workflows/ci.yml`).
- `tern-mux` daemon now binds its socket atomically (`bind_over` at start too), in `007dbdd`.

## Surfaces to review, by exposure

1. **Web UI on the network** (`crates/tern-web/`, `service.rs`, `lib.rs`). The installed Tern currently runs
   `tern --web --no-pair --bind 0.0.0.0` (listens on every interface). Check: authentication on every route and
   the WebSocket, pairing/token strength and storage, per-device permissions (handoff `2026-10-24-web-ui.md`,
   memory `web-ui-work`, `phone-control-work`), what a paired phone can do (type into terminals = run code),
   CSRF/Origin checks, DNS rebinding (Host header), TLS or not on LAN, rate limiting of pairing.
2. **Markdown preview of repo files** (`crates/tern-app/src/viewer.rs`, uses gpui-component `TextView`): links open
   any scheme by default (`gpui-base` `text_view.rs:122` calls `cx.open_url`). A cloned repo's README is untrusted.
   Same fix as chat (`web_url`), plus relative links to files in the repo.
3. **Terminal escape sequences from untrusted output** (`crates/tern-term/`): OSC 8 hyperlinks (scheme on click),
   OSC 52 clipboard write/read, OSC 7 cwd, OSC 9/777 notifications, kitty graphics file/shm paths (fixed partly),
   title setting, any sequence that makes Tern open a path (`TerminalEvent::OpenPath`, `PreviewImage`).
4. **`tern://` URL handler** (`main.rs`, `session_url_id`, `file_url_path`, `reveal_session`): any web page can
   open a `tern://` URL. Check what each form can trigger without the user.
5. **MCP servers Tern exposes** (`tern --mcp`, `crates/tern-app/src/mcp.rs`, `tern-mux/src/services.rs`):
   `run_service` runs commands; confirm it is reachable only by the user's own agents and what a prompt-injected
   agent could do beyond its own shell.
6. **Hook endpoint** (`crates/tern-bridge/`, an HTTP server on 127.0.0.1 with a per-tab token; the hook command
   is in the user's Claude settings): token entropy, comparison in constant time, other local users/processes,
   request size limits, what a forged hook event can do (statuses, notifications, handoff switches).
7. **Daemon socket and handoff** (`tern-mux`): socket dir permissions (`paths::ensure_private_dir`), peer
   credential check, fd passing in handoff, `ChatSpec.record` (a client-given path the daemon appends to; same
   user, private socket — confirm), `ChatSpec.program` (the daemon runs what the client says).
8. **Control socket** (`crates/tern-app/src/control.rs`, `TERN_CONTROL`): debug-only? Confirm it is compiled out
   or refused in release, and loopback-only on Windows.
9. **Updates** (`crates/tern-app/src/update.rs`): signature/identity check of the downloaded build
   (`identifier "dev.tern"` requirement), TLS, downgrade, where it unpacks.
10. **Files from outside**: Finder drops, pasted images (size limits), explorer rename/delete/move (path traversal,
    symlinks), git log/diff of a hostile repo (filenames with control characters), search index of huge files.
11. **Process spawning**: `claude-args` file, shell quoting in `spawn_config`/`claude_env`, PowerShell paths
    (`shell_quote` in `server.rs`), environment passed to children (tokens leaking into terminals).
12. **Secrets on disk**: accounts (`crates/tern-app/src/accounts.rs`), web pairing tokens, logs
    (`paths::logs()`): file modes, nothing secret in logs.

## How to work

- A Workflow or a few parallel reviewers fits this (one per surface), but only if the user asks for that scale;
  otherwise go surface by surface, starting with 1-3.
- For each finding: the path from untrusted input to effect, a failing test first, the fix, then CI's checks:
  CI grep, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace` (memory `check-before-push`).
- Verify UI-visible fixes in the debug app (memory `debug-app-screenshots`): capture only the debug window by id,
  never the whole screen.
- Commit when the user asks; `main`, local, not pushed (HEAD is `7f57b1f`).
