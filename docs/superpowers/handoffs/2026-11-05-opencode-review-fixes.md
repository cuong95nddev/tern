# Handoff: OpenCode, the final review's fixes — written 2026-11-05

## Task

Phase 3 of agent adapters (OpenCode) is built but not ready to merge. Do the final review's fix pass, then finish the branch. Reply to the user in Vietnamese; code and comments in English. The user wants it done properly ("làm chuẩn chỉ") and chose native execution (`superpowers:executing-plans`, no subagent per task).

## State

- Branch `agent-adapters-opencode`, off `main` c14b2e1, in the repo itself (no worktree: builds stay in `target/`). Not merged, not pushed. Ask the user before any merge or push.
- All 7 plan tasks are committed. Before the fixes: 921 tests pass, clippy and Windows clippy clean.
- The user accepted `--standalone` (a server per tab, about 374 MB idle) and minimum OpenCode 2.0.30.
- Ledger: `.superpowers/sdd/2026-11-05-agent-adapters-phase3/progress.md` (git-ignored). Full review: `final-review.md` beside it. Read both first; the ledger's last block is the fix design, summarised below. Its `Ruling:` lines must reach the user in the final message, with the deferred minors. Delete that workspace only once the review is clean.
- Read also: plan `docs/superpowers/plans/2026-11-05-agent-adapters-phase3.md`, spec "Settled for OpenCode" in `docs/superpowers/specs/2026-11-05-agent-adapters-design.md`, `docs/superpowers/handoffs/2026-11-05-agent-adapters.md` (the phase 4 handoff: seams, manual testing, what is left).

## The fix pass

One pass. Each fix: a test that fails first, then passes. No second reviewer. Ledger each as `Final: fixed <finding> — <test> RED→GREEN, suite N/N`. A finding left unfixed is a `Final: Ruling:` line.

- **C1, a reattached chat loses its text.** ACP chunks are the only copy, and `chat.rs` drops `Said::Streamed` lines at each turn's end. `OpenCodeFollow` stops reporting chunks as `Streamed`. Written `session/prompt` lines are kept (a new `Said`, pushed to `st.lines` in `ChatHost::write`) and `OpenCodeDriver::fold_line` folds them as `Entry::User`. Test: replaying `st.lines` into a fresh driver gives the live transcript.
- **I1, a resumed chat stuck busy after a reattach.** `opened()` clears `turn` and `t.busy` before `flush`. `resume_busy` also reads a replayed `User` with no `TurnEnd` as running: give it a rule that knows a load's history.
- **I2, no guard in an OpenCode chat** (no hooks, token `None`). On `Said::TurnEnded` with no token, feed the guard a synthetic `Stop` with the turn's last text (new `Said` from `OpenCodeFollow`, chunks buffered per turn), and send a block's reason and the `Action` through `tell`. A new `Said::Session` after a clear gives a synthetic `SessionStart` (`clear`), its context told as a message. `turn_ended` sets `t.handoff = Some(Some(path))` by `context::handoff_path`. Add the plan's test: over the limit, it is asked with a user message. The Task 6 ruling is reversed.
- **I3, `form.created` dropped.** Its `sessionID` is in `data.form`. Read it there in `opencode_fold.rs` and the plugin; `form.replied`/`cancelled` fall back to the root. Test with the real shape. Correct the phase 4 handoff's line on forms.
- **I4, a finished child comes back.** `child()` starts one only on `session.execution.started`, `session.step.started`, `session.tool.called`. The root's turn end gives `SubagentStop` for each left.
- **I5, a failed parent lookup makes a child the root.** `session.created` has `parentID` in its data: seed from it, in the plugin and the fold. A failed lookup isn't cached and sends `parent: "unknown"` (the child path). Live: make the mock call the `subagent` tool for a real child.
- **I6, too many posts.** The plugin forwards an allowlist: `session.created`, `session.viewed`, `session.inbox.enqueued`, `session.execution.{started,succeeded,failed,interrupted}`, `session.tool.{input.started,called,success,failed}`, `session.text.ended`, `session.step.ended`, `session.compaction.ended`, `permission.{asked,replied}`, `form.{created,replied,cancelled}`. After a failed post, non-terminal kinds are dropped until one succeeds. A Rust test checks the source lists each kind the fold reads.
- **I7, a restarted chat shows everything twice.** In `chat_view::start`, when `driver.resumes(session)`, clear the transcript (`load_history(None)`) before `start_lines`. `warm` runs before `user_line` in `send`, so a typed message is safe.
- **M7, raised to Important.** try/catch around `forward`; `ctx` calls through async wrappers, so a throw can't end the plugin.
- **M12a, raised to Important; also hits Codex by this branch's first commit.** `chat_view`'s `Attached` replay drops `fold_line`'s replies, so an ask auto-rejected with no UI attached waits for ever. Collect replies, drop those a later kept line answers by id, write the rest.

Deferred minors, to report: M1, M2, M3, M4, M5, M6, M8, M9, M10, M11, M12b, M12c.

## Then

1. `cargo fmt --all --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo test --locked --workspace --no-fail-fast`, `PATH=/opt/homebrew/opt/llvm/bin:$PATH cargo xwin clippy --locked --workspace --all-targets --target x86_64-pc-windows-msvc -- -D warnings`.
2. Live with the mock: a chat across `restart-background` and a reopen keeps its text; a real child session; a chat's guard. Also look at why a clean quit reopens with no tabs when given a folder, and try the chat's Stop button.
3. Update the phase 4 handoff, then `superpowers:finishing-a-development-branch`.

## Decisions to keep

- Never write to OpenCode's folders: `--standalone` plus `OPENCODE_CONFIG_CONTENT`, no overlay (`OPENCODE_CONFIG_DIR` replaces the user's config).
- A thin plugin, the logic in Rust (`opencode_fold.rs`): CI has no JS runner.
- No SQLite in Tern: the sidebar reads a per-tab preview file, a chat's history comes from `session/load`.
- Test against the mock in a scratch home: free OpenCode models hit their limit in a few calls, and Copilot's credentials are in the database, not `auth.json`.
- Plain `opencode` commands in the real home start its background service: stop it afterwards.
- In a shell where `HOME` was changed, Tern's service commands fail: run them with the real one.

## Files that matter

`crates/tern-bridge/src/{opencode_hooks.rs,opencode_fold.rs,opencode_plugin.js}`, `crates/tern-term/src/opencode_status.rs`, `crates/tern-chat/src/opencode/{acp.rs,preview.rs}`, `crates/tern-mux/src/{server.rs,chat.rs}`, `crates/tern-app/src/chat_view.rs`, `docs/superpowers/spikes/2026-11-05-opencode/`.
