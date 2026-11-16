# Handoff: Tern-managed git worktrees (2026-11-16)

Talk to the user in Vietnamese; code and comments in English. UI goes through
templates (`templates.json` + `frame`/`slot`), never hand-built `div()`.

## Task

The user wanted one standard for git worktrees instead of each agent running
`git worktree add` its own way. We compared Orca, T3 Code and herdr (refs in
`../refs/`; `~/ws/t3code` is a broken clone), then built Tern-managed worktrees.
Full design: `docs/superpowers/specs/2026-11-16-worktrees-design.md` (phases 1-3
and the sidebar section are accurate).

## State

- All of it is **uncommitted on main**, mixed with the user's other uncommitted
  work (usage, template/markup, mux paths, term procs, ...). Not pushed.
- **Installed** into `/Applications/Tern.app` (user asked) on 2026-11-16 12:38.
- fmt, clippy `-D warnings`, full workspace tests pass. Windows clippy
  (`cargo xwin`) not run.
- CI's grep step flags `crates/tern-app/src/template/markup.rs:412` (`"hidden"`):
  the user's own uncommitted change, not ours. Told the user.
- A test worktree exists: `.worktrees/try-worktree` (branch `tern/try-worktree`),
  left for the user to try removing; `tern --worktree rm try-worktree` removes it.
- Demo data for the debug build: scratchpad `demo/shop-api` + `demo.sh` (may be
  gone; scratchpad is per session).

## What was built (files)

- `crates/tern-fs/src/worktree.rs` (new): `git_dirs`, `linked` (verifies the
  link from the repo side: own git dir under `common/worktrees`, `gitdir` points
  back; security fix), `branch_of`, `list` (porcelain), `create`, `remove`,
  `changes`, `delete_merged_branch`, `worked_on` (reflog), `merged`, `prune`,
  `copy_included` (`.worktreeinclude`), `exclude_dir`, `slug`,
  `generated_name`, `DIR = ".worktrees"`.
- `tern-fs`: `git.rs` (`index_locked` via own git dir; `.worktrees` in
  `SKIP_DIRS`), `watch.rs` (also watches a worktree's own git dir + common
  `refs/`), `tree.rs` (uses `git_dirs`).
- `crates/tern-app/src/worktrees.rs` (new): `make`, `find`, `remove` (runs
  archive first), `name_from`, `free_name`, scripts in `.tern/worktree.json`
  (`setup`/`archive`), approval by SHA-256 of repo path + file bytes in
  `data()/worktree-setups`, `run_setup` (service `setup`), `archive` (login
  shell `-lic`, not `shell_command_args`, whose `exec` lost the exit code).
- `agent_cli.rs`: `--worktree create|ls|rm|prune`; `--task start a,b
  --worktree[=NAME]` (fan-out, names from the task); guide text;
  `run_service`; `NOT_RUNNING`/`NOT_OPEN`.
- `agent_ui.rs`: ops `worktree_open`, `worktree_close`. `agent_tasks.rs`:
  `exact` folder match for tasks in a worktree.
- `main_window.rs`: `open_worktree`, `close_folder`, `add_folder`,
  `place_for`, `ask_new_worktree` (template `dialog.worktree`),
  `make_worktree`, `offer_setup` (native prompt to approve scripts),
  `ask_remove_worktree` (native prompt), folder menu items, and
  `render_agents` rewritten: a repo with worktrees open shows as a group
  (`agents.group` header, fold via `folded_groups`, hover "+") with one card
  per checkout (`agents.card`, `.head`, `.branch`, `.badge`) holding its tabs.
  `status_vars` helper.
- `template/mod.rs`: `Scope::at`, `Scope::numbered` (nested lists numbered in
  one run so frames map to `entries`). `style.rs`: the new elements.
- `templates.json`: `agents` item switch, `agents.group`, `agents.card`,
  `dialog.worktree(.footer)`, `menu.workspace_tab` worktree items.
- `workspace.rs`: `worktree` field + `worktree()`, refreshed on git changes.
- `folder_trust.rs`: a verified worktree inherits its main checkout's trust.

## Decisions

- Worktrees live in `REPO/.worktrees/NAME` (not `~/.tern/worktrees`): a live
  test showed Claude's trust prompt blocking a task agent outside the repo;
  sandboxes also cover in-repo paths. Ignored via `info/exclude`.
- Git's list is the only list (as Orca). Branch `tern/NAME` by default.
- Sidebar: user rejected indentation and the lighter-header attempt; after a
  mockup page (artifact https://claude.ai/artifact/GnUSFeaVh3rcMZiCXdEfvf) they
  picked **A, Orca-style cards**.
- Agents can't approve repo scripts; only the user, in the window prompt.

## Left / ideas

- After the install (built, checked, installed 2026-11-16), as the user asked:
  - A worktree card's × **removes** it, always through the confirm prompt of
    `ask_remove_worktree` (`request_close_folder`); tooltip "Remove Worktree…".
    The main checkout's × still closes.
  - A repository's worktrees in `.worktrees` **always show** as cards, open or
    not (`Workspace::tern_worktrees`, read from disk, refreshed on git changes and
    by `main_window::refresh_worktrees`, which the CLI triggers with op
    `worktrees_changed` after `rm`/`prune`). An unopened card is dimmed
    (`closed`), has no "New Tab"; a click opens it, × removes it.
- Main card shows "main  main" (branch + badge) when the branch is `main`:
  offered to hide the badge then. Not done.
- `prune` misses squash merges. Remove's "Remove" button in the native prompt
  was never clicked in a test (no Accessibility); its code path is the CLI's.
- Commit the worktree work apart from the user's other changes when asked.
