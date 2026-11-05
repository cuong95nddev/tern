# Agent adapters: one contract for every CLI agent

## Why

Tern's agent features were built for Claude Code and only Claude Code:
tab status from Claude hooks, the chat tab (Claude stream-json), the context
guard and handoff (Claude hooks + `~/.claude/projects` transcripts), the usage
monitor (Anthropic OAuth), the agent sidebar's recent preview, resume and
restore, the floating agent monitor, web and menubar views. Codex, OpenCode
and the rest run in Tern only as a shell tab with a command typed in; they get
an icon and a best-effort screen-read status, nothing else.

The user wants these features **templated**: each feature talks to an
abstract agent, and each CLI agent registers against one standard, declaring
what it can provide. A new agent should plug in once and light up every
feature it has the capabilities for, without touching each feature.

### Prior art (read before building)

- **Orca** (`refs/orca`): closed union of ~45 agent ids, with per-concern
  tables keyed by id. `TuiAgentConfig`
  (`src/shared/agent-launch/catalog/tui-agent-config-types.ts`) for launch and
  detection; one hook installer per agent
  (`src/main/agent-hooks/managed-agent-hook-registry.ts`), all hooks POSTing
  to one local server (`/hook/<source>`), one normalizer per agent into one
  status model (`src/shared/agent-hook-listener/provider-dispatch.ts`,
  `providers/*`); resume argv table
  (`src/shared/agent-launch/startup/agent-resume-argv.ts`); transcript roots
  per agent (`src/main/ai-vault/session-scanner-roots.ts`); usage parsers per
  agent (`src/main/claude-usage`, `codex-usage`, `opencode-usage`,
  `rate-limits/`); structured chat with a driver per protocol (Claude
  stream-json, Codex `app-server`, ACP for OpenCode and others,
  `src/main/acp/`).
- **Herdr** (`refs/herdr`, `refs/herdr-gpui`): compiled agent enum, but status
  is screen-parsing rules in **per-agent TOML manifests**
  (`src/detect/manifests/*.toml`) the user can override in
  `<config>/agent-detection/<id>.toml`; hooks/plugins per agent report session
  ids (`src/integration/targets.rs`); resume table (`src/agent_resume.rs`);
  herdr-gpui's chat speaks ACP to everything, with a translator for Claude
  stream-json, Codex `app-server` and Pi RPC
  (`crates/sheprd-acp/src/harness.rs`).

Both agree on the shape we take: **identity is a registry, every feature is a
separate capability with its own per-agent implementation, and everything is
normalized into one model before features see it.** Neither lets a user add a
fully featured agent by data alone; herdr lets data add detection rules.

## Goals

1. One **agent registry**. An agent is an id plus a descriptor (name, logo,
   command, process names, resume argv, screen rules, where its skills go) and
   a set of **capabilities**, each optional:
   - `detect`: recognize it running in a terminal.
   - `launch`: start it in a terminal tab, with Tern's integration wired in
     (hooks/plugin, env, resume or a fixed session id).
   - `status`: live state from the agent's own events (hooks/plugin), folded
     into one status model; screen rules as the fallback.
   - `session`: its session id, transcript location, and resume command.
   - `transcript`: read the latest assistant text and the recent
     sentence/tool for the sidebar preview.
   - `context`: how full its context window is.
   - `steer`: tell the running agent something between turns (the context
     guard's "write a handoff" and "go on from this handoff"), and hand it the
     Tern guide at session start.
   - `usage`: account quota / rate limits for the usage monitor.
   - `chat`: run it headless and translate its native protocol to and from
     Tern's chat model.
   - `skills`: where its skill files go (`.claude/skills`, `.agents/skills`, …).
2. Every agent feature reads **only** the registry and the normalized models,
   never a specific agent: agent panel/sidebar, tab status and notifications,
   chat tab, terminal ⇄ chat switch, context ring + guard + auto handoff,
   usage monitor, recent preview, "edited by agent" badges, restore/resume,
   floating monitor, web, menubar, palette/menus ("New <Agent> Tab",
   "New <Agent> Chat").
3. The UI is **capability-gated**: a feature an agent lacks is hidden or shown
   reduced, never broken. An agent with only `detect` still gets an icon and
   screen-read status (what every non-Claude agent has today).
4. First-class adapters for **Claude Code, Codex and OpenCode**. Gemini,
   Cursor Agent, Grok and Pi stay at today's level (detect + launch + screen
   status) through descriptors.
5. A user can add an agent, or override a built-in's descriptor fields
   (command, args, process names, screen rules, resume argv), with a file in
   `~/.config/tern/agents/<id>.toml`, without rebuilding Tern. Data gives the
   basic tier; hook/chat/usage capabilities come from Rust adapters.

## Non-goals

- Making every agent reach Claude's depth. Codex and OpenCode get what their
  CLIs actually expose; gaps are gated in the UI, not faked.
- Forcing one wire protocol (e.g. ACP for everything). Each chat driver speaks
  its agent's native protocol; only Tern's internal chat model is shared.
- Fully data-defined agents with chat/usage/hooks (no plugin runtime, no
  scripting). Revisit if users ask.
- Agents beyond the seven Tern already knows, in this work.

## Anti-goals (would make this a failure even if it works)

- **Any regression for Claude.** Claude is the main user; every Claude
  behavior today (hook status lane, subagents/monitoring/owed notifications,
  chat, switch, guard, handoff, accounts, usage, restore) must behave
  identically after the port. The existing tests and fixtures
  (`tern-term/src/claude_status/`) keep passing unchanged in substance.
- **Editing the user's global agent config.** Today Claude gets a private
  `--settings` file per tab and `~/.claude/settings.json` is never touched.
  Keep that: integration for every agent goes through per-launch overlays
  (flag, env var pointing at a Tern-owned dir), never by writing into
  `~/.codex`, `~/.config/opencode`, etc. Orca's managed `CODEX_HOME` and
  `OPENCODE_CONFIG_DIR` overlays are the model.
- Workarounds instead of root fixes: no "if agent == Claude" left behind in a
  feature after its capability exists; the old Claude-specific names and
  paths are renamed, not wrapped.
- Breaking the daemon protocol for an older UI or daemon mid-update: changes
  go through the existing MINOR-version compatibility rules in `proto.rs`
  (older peers skip what they don't know; saved windows from before still
  restore).

## Constraints

- New and reworked UI goes through the template engine
  (`crates/tern-app/src/template/`, `templates.json`), per CLAUDE.md. Strings
  like "Ask Claude anything", "Claude Chat", "New Claude Tab" become
  agent-parameterized template variables.
- Builds and checks run in the repo's `target/` (CLAUDE.md).
- The daemon keeps running agents while the UI is closed, so status trackers,
  hook receiving, chat drivers and the context meter live daemon-side as they
  do now; the UI receives normalized messages.
- No polling where an event exists; transcript/rollout reads are tail reads
  triggered by events, as `recent.rs` does today.

## Current state (inventory)

Paths under `crates/`. This is what the port must cover.

| Area | Claude-specific today |
|---|---|
| Registry | `tern-app/src/agents.rs:337-471` `enum Agent` (detect by exact program name, command, name, logo, `read_screen`); a second ad-hoc detector `tern-mux/src/server.rs:1399` `agent_name()` |
| Tabs | `workspace.rs:155` `TabKind { Claude, Shell }`; other agents are Shell tabs with a typed command (`workspace.rs:2981`); actions `NewClaudeTab/NewClaudeChat/SwitchClaudeView` (workspace, palette, menus, control, templates.json) |
| Detection | `tern-term/src/procs.rs:73` `is_claude`, Claude Bash-tool shells (`:218-290`, `:418`); `status.rs:63-147` Claude `sessions/<pid>.json`; OSC 7501 `app=claude-code` (`osc.rs`, `host.rs:1142`) |
| Hooks | `tern-bridge/src/hooks.rs` (private `--settings` file, `HookServer` with per-tab tokens, `EDIT_TOOLS`); `claude_version.rs`, `tern-mux/src/claude_probe.rs`; `server.rs:1121-1157,1347-1390` |
| Status model | `tern-term/src/claude_status.rs` `ClaudeTracker` / `ClaudeStatus` (generic shape: state, subagents, held, monitoring, wakeup); `proto.rs` `AgentStatus { status: Box<ClaudeStatus> }`, `SessionInfo.claude: bool`, `claude_session`, `claude: Option<ClaudeTracker>` |
| Status line | `tern-bridge/src/statusline.rs`, `agents.rs:22` `ContextUse::read` |
| Chat | `tern-chat` (stream-json launch/wire/transcript/summary/recent); `tern-mux/src/chat.rs`; `tern-app/src/chat_view.rs`; `folder_trust.rs` (`.claude.json`) |
| Switch | `workspace.rs:596-640,1711-1765` (`--resume`, `claude attach <job>`) |
| Guard/handoff | `tern-bridge/src/context.rs` (hook answers, `Meter`, `last_transcript_text`); `agents.rs:49-108`; `workspace.rs:2476-2560`; `proto.rs:147-170`; `chat.rs:324-360` |
| Usage | `tern-app/src/usage.rs` (Anthropic OAuth), `usage_view.rs`, `accounts.rs` (Claude accounts as `CLAUDE_CONFIG_DIR`s) |
| Preview | `tern-chat/src/recent.rs`, `workspace.rs:2816-2860`, `agents.rs:231` |
| Persistence | `window_state.rs` `SavedTab::Claude {..}`, `main_window.rs`, `restart.rs`, `update.rs` |
| Other views | `agent_monitor.rs:181` (chats = Claude), `tern-web` (`s.claude`), `tern-menubar`, `resources.rs`, `file_tree_view.rs` "edited by Claude" |
| Env | `tern-term/src/env.rs` scrubs `CLAUDE*`, `CODEX_THREAD_ID` |

The unmerged `agent-customization` branch (e1d4b25, `--mcp-config` and a
Claude plugin dir) is abandoned by the user's decision: it is not merged and
this work does not account for it. Claude's `launch` capability is today's
`--settings` file plus session and account env.

## Design

### 1. Identity and descriptors

`AgentId` becomes an open id (a short string newtype, e.g. `"claude"`,
`"codex"`, `"opencode"`), not a closed enum, so user descriptors can add ids.
Built-ins are constants.

An `AgentDescriptor` is plain data:

```toml
# ~/.config/tern/agents/codex.toml  (or built in)
id = "codex"
name = "Codex"
command = "codex"                 # what a new tab runs
args = []                         # user-added flags
processes = ["codex"]             # foreground program names that identify it
logo = { light = "openai-light.svg", dark = "openai-dark.svg" }
resume = ["codex", "resume", "{session}"]
skills_dir = ".agents/skills"     # under $HOME; none = no skills

[[screen.blocked]]                # screen fallback rules (today's read_screen)
title_contains = "Action Required"
[[screen.blocked]]
bottom_contains = "allow command?"
[[screen.working]]
title_any_char = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"
```

- Built-in descriptors ship compiled in (one TOML per agent embedded, like
  `templates.json`). A user file with the same id **overrides fields**; a new
  id **adds** a basic-tier agent (detect, launch, screen status, resume).
- `processes` replaces both `Agent::detect` and `server.rs::agent_name()`;
  there is exactly one detector. Detection looks through `node`/`bun`
  wrappers by argv, as herdr does (`detect/mod.rs:381`) and as
  `procs.rs:56` already does for `codex.js`.
- Today's `read_screen` cues move into the built-in descriptors' screen rules
  unchanged. The rule language is the minimum that expresses today's cues
  (title/bottom contains, title leading/any char in a set); herdr's richer
  regions are a later extension, not in this work.
- Errors in user descriptors are reported the way theme errors are
  (`tern --theme` prints what is wrong), and a broken file never disables a
  built-in.

### 2. Capabilities (Rust adapters)

Each built-in agent has an adapter that implements the capability traits it
supports; the registry maps id → descriptor + optional adapter. Features ask
the registry, e.g. `registry.get(id).chat()` → `Option<&dyn ChatDriver>`.

Sketch of the contracts (names are the builder's to refine):

- `Launch`: given a tab's descriptor, cwd, account, optional session to resume
  or a fresh session id, and the tab's hook endpoint/token, produce the argv
  and env, and any per-launch files to write (and remove on exit). Claude:
  today's `--settings {tern-settings}` file, `--session-id`/`--resume`,
  account env. Codex: a Tern-owned `CODEX_HOME` overlay that
  links the user's real `~/.codex` content and adds Tern's `hooks.json` (see
  Orca `src/main/codex/configuration/homes/`). OpenCode: an
  `OPENCODE_CONFIG_DIR` overlay with Tern's plugin
  (Orca `src/main/opencode/hook-service.ts`).
- `HookLane`: turns one raw hook/plugin event into normalized `AgentEvent`s,
  and keeps the per-tab tracker. Claude: today's `ClaudeTracker` lane,
  unchanged. Codex: hook events `SessionStart, UserPromptSubmit, PreToolUse,
  PermissionRequest, PostToolUse, SubagentStart/Stop, Stop, Interrupt`
  (Orca `codex-hook-definition.ts`, mapping in `providers/codex-state.ts`).
  OpenCode: plugin events (`session.status`, `tool.execute.*`,
  `permission.asked`, `question.asked`, `session.idle`, `session.error`, child
  sessions as subagents; herdr `assets/opencode/herdr-agent-state.js`).
- `Sessions`: session id from events; transcript path; resume argv (from the
  descriptor template). Claude `~/.claude/projects/<slug>/<id>.jsonl`; Codex
  `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-*-<id>.jsonl`; OpenCode its SQLite
  store (`~/.local/share/opencode/opencode.db`).
- `TranscriptReader`: latest assistant text (handoff), recent sentence + tool
  (sidebar preview), edited files (badges). Claude: today's `recent.rs` and
  `last_transcript_text`. Codex: rollout records. OpenCode: plugin events
  carry it, or DB reads.
- `ContextMeter`: `{percent, size}` per conversation. Claude: status line and
  stream-json usage (today). Codex: rollout `event_msg/token_count` gives
  `last_token_usage` and `model_context_window` (verified in a local rollout).
  OpenCode: message token counts from plugin events / DB, window from model.
- `Steer`: deliver text to the agent at a safe point: the session-start guide,
  "write a handoff now", "go on from this handoff". Claude: hook answers
  (`additionalContext`, `Stop` block), as today. Others: the hook answer if
  their hooks support it (to verify per agent, see Open items), else Tern
  types it into the idle prompt as a bracketed paste + Enter
  (Orca `agent-prompt-injection.ts`; herdr `agent.prompt` refuses while
  blocked). Pasting is a fallback, gated on the agent being idle.
- `UsageSource`: quota windows + reset times + plan for the usage monitor.
  Claude: today's OAuth endpoint. Codex: `rate_limits` in the latest rollout
  `token_count` (primary/secondary windows, `used_percent`, `resets_at`,
  plan); no network call. OpenCode: none (it brings its own providers); the
  usage view shows no quota for it.
- `ChatDriver`: start headless, write user turns/answers/interrupts, and fold
  native output into Tern's chat model. Claude: today's stream-json code.
  Codex: `codex app-server` (JSON-RPC). OpenCode: `opencode acp` (ACP
  JSON-RPC over stdio). See §4.
- `Skills`: where Tern's skill goes for this agent (`skills_dir`), so the
  agent-commands guide reaches non-Claude agents too.

### 3. One status model

`ClaudeStatus` is already agent-neutral in shape (state working/waiting/done,
main agent, subagents, held, monitoring, wakeup, session boundary). It is
renamed `AgentStatus` and moved out of the Claude module; each `HookLane`
produces it. Fields an agent can't fill stay empty (`held` all false,
`subagents` empty). `ActivityStatus` stays the coarse badge status derived
from it, and from screen rules when an agent has no hook lane.

Protocol: `AgentStatus { status: Box<ClaudeStatus> }` becomes the generic
status; `SessionInfo.claude: bool` becomes `agent: Option<AgentId>` and
`claude_session` becomes `agent_session`. Per the MINOR rules, the daemon
keeps sending the old fields to old clients, and the new client reads old
saves (`SavedTab::Claude` maps to `SavedTab::Agent { id: claude, .. }`).

Precedence stays as today: hook lane > OSC 7501 > screen rules.

### 4. Chat model

`tern-chat` today folds Claude stream-json straight into `Entry`s. Split it:

- A neutral **chat model** (`Entry`, `Tool`, `Ask`, `Elicit`, `TurnEnd`,
  `Model`, answers) and a neutral **chat event** stream that drivers emit.
- One driver per protocol: `claude` (today's `launch`/`wire`/`transcript`
  code), `codex_app_server`, `acp` (OpenCode; reusable for any ACP agent later,
  Gemini and others speak ACP).
- Tool labels in `summary.rs` become per-driver tables (Claude tool names,
  Codex `exec`/`apply_patch`, ACP tool kinds).
- The daemon (`tern-mux/src/chat.rs`) stays a dumb pipe that runs the process
  and persists what is needed to replay; the driver lives where the folding
  lives today. If a driver needs daemon-side state (e.g. JSON-RPC request ids
  for a reattach), the builder places it daemon-side.
- Chat features a protocol lacks are gated: permission modes, model switch,
  elicitations, image input.

Herdr-gpui translates everything into ACP and Orca keeps per-protocol
drivers; we take Orca's route (§ Non-goals) because Claude's stream-json is
richer than ACP and Claude must not regress.

### 5. Features become agent-generic

Each item: what it reads after the change.

- **Tabs**: `TabKind::Agent(AgentId)` replaces `TabKind::Claude`; every
  first-class agent gets a real agent tab (launched with its integration),
  not a shell with typed text. Basic-tier agents keep the typed command.
  Actions become `NewAgentTab(id)`, `NewAgentChat(id)`, `SwitchAgentView`;
  palette/menus list the agents that have `launch` / `chat`.
- **Terminal ⇄ chat switch**: needs `chat` + `session` (resume the same
  session in the other form). Claude's background-job handling
  (`claude attach <job>`) stays in Claude's adapter.
- **Context ring, guard, auto handoff**: need `context` + `steer` +
  `transcript`; ring alone needs `context`.
- **Usage monitor**: one card per agent that has `usage`, using the agent's
  logo; accounts stay Claude-only for now (an account is a Claude concept in
  Tern today) but the account type lives in Claude's adapter.
- **Recent preview, edited badges**: need `transcript` (edited files from tool
  events in the normalized stream where the agent reports them).
- **Restore/resume**: `session` + descriptor `resume`.
- **Floating monitor, web, menubar, resources**: show any agent with its logo
  and name; filter on `agent.is_some()` instead of `claude`.
- **Notifications**: `status_notification` already takes the agent name;
  callers pass the descriptor's name.

### 6. Rollout order

Each phase is shippable on its own and leaves main green.

1. **Foundation + Claude port.** Registry, descriptors (built-in + user
   overrides), single detector, `AgentStatus` rename, protocol fields, saved
   tab migration, `TabKind::Agent`, generic actions and templates, chat model
   split with the Claude driver, capability traits with Claude as the only
   full adapter. Zero visible change for Claude users; other agents behave
   as today.
2. **Codex.** Launch overlay + hooks, hook lane, sessions/rollouts, context
   meter, usage from rollouts, steer, resume, recent preview; then the
   `app-server` chat driver.
3. **OpenCode.** Config overlay + plugin, lane, sessions, context, steer,
   resume, preview; then the ACP chat driver.
4. **Basic-tier polish.** Gemini/Cursor/Grok/Pi descriptors, user
   `agents/*.toml` documented in Tern's config README.

## Decided (by the user)

- Agents register against a standard; each Tern agent feature works for any
  agent that registers.
- Approach: Rust adapters per agent for rich capabilities, plus a data file
  (descriptor) for the basic parts, so users can add/override agents at the
  basic level without rebuilding.
- The `agent-customization` branch is dropped.

## Calls made in this design (not asked; check while reading)

- First-class targets are Claude, Codex, OpenCode; others stay basic.
- Chat keeps native protocols per agent, not ACP-for-all.
- No edits to users' global agent configs; per-launch overlays only.
- Descriptor format TOML under `~/.config/tern/agents/`.
- Codex usage from rollout files, not Codex's backend API.
- Claude accounts stay Claude-only. Codex/OpenCode get one Tern-owned
  overlay home per agent (shared by its tabs, keyed by the user's real home),
  not per account and not per tab; the account model stays inside Claude's
  adapter.
- Steering and metering have two surfaces. Terminal tabs: the hook lane
  (Claude hook answers; others hook answer or idle paste). Chats: the chat
  driver owns both, as the daemon does today for Claude chats (guard text sent
  as a user message over stream-json, usage read from its output). The guard
  logic (limit, ask, auto switch) is one generic piece calling whichever
  surface the tab has. Claude's shipped chat guard must not regress.
- Version gating: each adapter may probe its CLI's version (as
  `claude_version.rs` / `claude_probe.rs` do); below what a capability needs,
  that capability is absent and the agent falls back to screen rules. No hard
  minimum version.
- Compatibility: old saves and old daemons/clients → new Tern must work. A
  downgrade reading new saves is not required. An old client sees non-Claude
  agent tabs as plain terminals.
- Planning: phase 1 is planned in full; phases 2 and 3 each get their own
  plan later, starting with a spike that settles the Open items for that
  agent.

## Open items for the builder to verify

- Whether Codex hook outputs can inject context / block `Stop` like Claude's
  (decides if Codex `steer` uses hooks or the paste fallback).
- Whether OpenCode's plugin API can inject a message into the session
  (`client.session.prompt` with `noReply`, per its SDK docs) for `steer`.
- Whether `codex app-server` and `opencode acp` can resume a session the TUI
  started (needed for the terminal ⇄ chat switch).
- Codex hook trust: Codex asks to trust hooks in `config.toml`; Orca writes
  trust entries in its managed home (`src/main/codex/trust/`).
