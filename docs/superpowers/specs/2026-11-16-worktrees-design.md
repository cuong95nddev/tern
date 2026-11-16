# Git worktrees managed by Tern

## Why

Agents made git worktrees each their own way (`git worktree add` wherever, any
branch name), so the user could not see them in Tern and the checkouts piled up.
Orca, T3 Code and herdr all have the app make and keep worktrees, and tell the
agent to go through the app. Tern now does the same.

## What (phase 1, done)

- **One place, one naming.** `REPO/.worktrees/NAME` (`tern_fs::worktree::DIR`),
  branch `tern/NAME` unless `--branch` names one (an existing branch is checked
  out, a missing one is made from `--base`, default HEAD). No name: a made-up one
  (`calm-otter-3f2a`). Tern adds `/.worktrees/` to the repository's
  `info/exclude` (no tracked file changes).
  - Inside the repository rather than `~/.tern/worktrees` (herdr, T3) because
    folder trust (Claude Code asks for every new folder outside a trusted one)
    and agents' sandboxes (write access under the repo) then cover worktrees as
    they cover the repo. Verified live: a worktree in `~/.tern/worktrees` stopped a
    task agent at Claude's trust question. Same idea as `claude --worktree`
    (`.claude/worktrees`).
  - `.worktrees` is in `SKIP_DIRS`: the main folder's watcher, repo scan and
    search index skip it; each worktree is a folder of its own.
- **Setup.** Files named by the repo's `.worktreeinclude` (gitignore patterns)
  that git doesn't track are copied from the main checkout (`.env`, ...), as in
  Orca and Claude Code desktop.
- **CLI** (`agent_cli.rs`), told to agents in the SessionStart guide:
  - `tern --worktree create [NAME] [--branch B] [--base REF]`
  - `tern --worktree ls` (branch, uncommitted changes, "you are here")
  - `tern --worktree rm NAME|BRANCH|PATH [--force]`: refuses the main checkout,
    the caller's own worktree, uncommitted changes (unless `--force`) and a folder
    open in Tern with programs running (unless `--force`); closes it in Tern,
    `git worktree remove`, then `git branch -d` (a branch with unmerged commits is
    kept).
  - `tern --task start AGENT --worktree[=NAME] TASK`: the task runs in a new
    worktree, in its own Tern folder (`exact` workspace match in `agent_tasks`).
- **No list of our own.** `git worktree list` is the list (as Orca).
- **Sidebar** (reworked after a look at Orca, herdr-gpui, Conductor and Superset;
  the user picked Orca's cards from three mockups). A repository with worktrees
  open in the window shows as a group: a header (`agents.group`: fold chevron,
  name, "N worktrees", how many agents wait, a hover "+" for New Worktree), then
  one card (`agents.card`) per checkout, the main one first. A card holds a state
  dot and its name (the main checkout's branch with a `main` badge, or the
  worktree's folder), the worktree's branch under it, and its agent tabs inside
  it: the card's edge groups them, so nothing is indented. The shown workspace's
  card is highlighted. A repository without worktrees, and a worktree whose
  repository isn't open, look as folders always did. Nested lists number their
  entries in one run (`Scope::at`, `Scope::numbered`) so the code tells a card's
  tabs apart. Opened by the CLI without taking focus.
- **Root fixes for linked worktrees** (`.git` is a file there):
  `git::index_locked` reads the worktree's own git dir; the watcher also watches
  the worktree's git dir and the common `refs/` (else Changes went stale on
  commit/checkout); `tree.rs` uses the shared `worktree::git_dirs`;
  `folder_trust::trusted` trusts a worktree whose main checkout is trusted.

## Phase 2 (done)

- **Folder menu** (`menu.workspace_tab`): "New Worktree…" on a folder in a git
  repository opens template dialog `dialog.worktree` (a name, or none for a
  made-up one); the worktree is made off the UI thread, then opened and shown.
  "Remove Worktree…" on a worktree folder asks (native prompt, as closing a
  workspace does) with what it loses (uncommitted changes, programs still
  running, the branch kept unless merged), then closes the folder and removes it
  (`worktrees::remove`, shared with the CLI).
- **Setup** (`worktrees.rs`): a repository names `{"setup": "COMMAND"}` in
  `.tern/worktree.json`. Anyone who can push can change it, so Tern runs it only
  after the user approves that file as it is: SHA-256 of the main checkout's path
  and the file's bytes, kept in `data()/worktree-setups` (herdr does the same).
  Asked in the window when a worktree is made (CLI or menu), with the command
  shown; approved, it runs as the worktree's service `setup` (Services panel,
  `tern --service logs setup`) and from then on without asking until the file
  changes. Agents can't approve it: the CLI only tells them it waits for the user.
  No `TERN_WORKTREE_PATH`-style variables: a service's command goes through the
  platform shell as one string, and PowerShell's `& COMMAND` form leaves no clean
  place for them; the setup runs in the worktree folder.

## Phase 3 (done)

- **Archive script**: `{"archive": "COMMAND"}` in the same `.tern/worktree.json`,
  covered by the same approval (the prompt shows every script). Before a worktree
  is removed (CLI `rm`, `prune`, the menu) it runs in the worktree in a login shell
  and is waited for; a failure keeps the worktree and says why (Orca does the
  same). Not approved: not run, and the removal says so.
- **Fan-out**: `tern --task start codex,claude --worktree TASK` gives the task to
  each agent, each in a worktree of its own (`NAME-AGENT`). Every agent is checked
  before any worktree is made; `--wait` is refused with several.
- **Names from the task**: `--worktree` without a name names the worktree after
  the task's first words (`fix-the-login-bug`), made unique (`-2`, ...). This
  stands in for Orca's and T3's rename after the first message, without asking
  a model.
- **`tern --worktree prune`**: removes Tern's worktrees (in `.worktrees`) whose
  branch was worked on (its reflog holds more than its making), is merged into
  the main checkout's HEAD (`merge-base --is-ancestor`; squash merges are not
  seen), has nothing uncommitted, and has nothing running in Tern; then
  `git worktree prune`. Each kept one is listed with why. On demand, not on a
  timer (herdr-gpui's automatic cleanup is off by default too).

## Not done

- Squash-merge detection for `prune` (would need the forge, as herdr-gpui asks
  GitHub).
