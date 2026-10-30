# Services over MCP

## Why

Coding agents start dev servers (`npm run dev`, `cargo run`, `docker compose up`)
in their own shells. Those servers are invisible: the user can't see which are
running, on which port, or who started them, and they die or linger with the
agent. Tern should run them instead, so the user can watch and stop them.

## What

- `tern --mcp`: an MCP server on stdio (JSON-RPC 2.0, one message per line).
  Any agent can use it: `claude mcp add tern -- <tern> --mcp`. Claude tabs Tern
  starts get it on their own (`--mcp-config`, inline JSON).
- A **service** is a daemon session (a PTY, like a terminal tab) that runs one
  shell command and carries a `ServiceSpec`: name, command, folder, the session
  that asked for it (`by`, from `TERN_SESSION_ID`, which the daemon now sets in
  every session it starts) and when it started. It outlives the agent and the
  UI, survives a daemon handoff, and an ended one stays listed for the ended
  grace (10 min) with its output.
- MCP tools:
  - `run_service(name, command, cwd?, restart?)`: start it; a running service of
    the same name in the same folder is left alone (said so) unless `restart`.
    Waits a moment, answers with id, pid, ports and the first output.
  - `list_services(all?)`: name, state (running / exited with code), ports,
    folder, command, uptime. Only the caller's folder unless `all`.
  - `service_logs(name, lines?)`: the last lines of its terminal.
  - `stop_service(name)`, `restart_service(name)`.
- Ports: the TCP ports the service's process tree listens on (`ps` + `lsof` on
  Unix; none on Windows for now).
- UI: a **Services** panel (template-rendered, like Shells) lists the services
  under the workspace's folder: state dot, name, ports (click opens
  `http://localhost:<port>`), command, uptime. Clicking a row opens it as a tab;
  Stop / Restart buttons. Service sessions are never adopted as orphan tabs.
  Hidden at first; it shows by itself when an agent starts a new service in the
  workspace. Closing a service's tab (or its workspace) only lets go of it: no
  "still running" prompt, the service runs on.

## Protocol (MINOR 10)

- `ClientMsg::RunService { req, cfg, service }`: spawn without attaching;
  answered by `ServiceStarted { req, id, pid }` or `Error`.
- `ClientMsg::ReadText { req, id, lines }`: answered by `Text { req, id, text }`
  (the last `lines` lines of its screen and scrollback) or `NotFound`.
- `SessionInfo` gains `service: Option<ServiceSpec>` and `exit_code`.
- `HandedSession` gains `service` (handoff 3).

## Not now

Health checks, auto-restart, env files, Windows ports, the phone page.
