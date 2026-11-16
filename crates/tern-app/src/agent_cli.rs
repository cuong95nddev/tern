//! The commands a coding agent runs Tern with: `tern --service ...` runs its
//! dev servers and other long-running commands as Tern services
//! (`tern_mux::services`), so the user sees them in the Services panel and
//! they outlive the agent; `tern --theme` and `tern --shot` change and check
//! how Tern looks (asking the window, see `agent_ui`); `tern --task` gives
//! a task to another agent in a new tab and waits for its answer
//! (`agent_tasks`); `tern --worktree` makes, lists and removes git
//! worktrees in the one place Tern keeps them (`worktrees`).
//!
//! Each one does its work and exits: nothing stays running beside an agent,
//! which on Windows would keep `tern.exe` from being replaced by an update.
//! An agent learns of them from [`guide`], which `tern --hook` adds to its
//! conversation, or from `tern --skill`.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tern_mux::proto::{ServiceSpec, SessionInfo};
use tern_mux::services::{self, Services};
use tern_term::{SpawnConfig, TermSize};

/// How long `--service run` waits for a new service to listen (or end).
const START_WAIT: Duration = Duration::from_secs(8);
/// How long `--task wait` waits by default: less than the two minutes an
/// agent's shell tool gives a command unless told otherwise.
const DEFAULT_TASK_WAIT: u64 = 100;
/// How often `--task wait` asks again.
const TASK_POLL: Duration = Duration::from_secs(1);
/// How long `--task answer` lets the agent take the answer before it reads
/// how the task stands.
const ANSWER_SETTLE: Duration = Duration::from_millis(1500);
const DEFAULT_LOG_LINES: u32 = 100;
const MAX_LOG_LINES: u32 = 2000;

/// How an agent uses these commands, with `tern` the command that runs them.
fn guide_for(tern: &str) -> String {
    format!(
        "You run inside Tern. Run its commands with your shell tool; each prints its result and exits.

Run dev servers, watchers and other long-running commands as Tern services, not in your own shell or in the \
background: they run in terminals the user can watch and stop, and they keep running after you finish.
  {tern} --service run NAME [--cwd DIR] [--restart] COMMAND   start COMMAND as service NAME (web, api, ...). A running \
service of that name in that folder is left as it is unless --restart is given. Waits a few seconds, then prints its \
state, ports and first output.
  {tern} --service ls [--all]         the services in your folder, or in every folder
  {tern} --service logs NAME [--lines N]   the last lines it printed (default 100)
  {tern} --service stop NAME          stop it and remove it
  {tern} --service restart NAME       stop it and start its command again
NAME is a service of your folder, or the id of any folder's.

To change how Tern looks, edit the files in Tern's config folder (its README.md tells how).
  {tern} --theme        apply the settings and theme files as saved and print the theme in use, every theme there is, \
what is wrong in the files, and where they are. Run it after editing them.
  {tern} --theme NAME   switch to a theme, saved in settings.json
  {tern} --shot         save a PNG of the Tern window showing your folder, as it was last drawn, and print its path. \
Read that file to look at it after changing how Tern looks.
  {tern} --agents       the coding agents Tern knows (its config folder's agents/*.json adds or changes them) and what \
is wrong in those files.

To have another coding agent (codex, claude, opencode, ... as --agents lists them) do a task and get its answer back, \
give it the task with Tern: it runs in a new tab beside yours, where the user can watch it and answer it.
  {tern} --task start AGENT [--wait] [--timeout S] [--worktree[=NAME]] TASK   start AGENT in a new tab and give it \
TASK (\"-\": read it from stdin). Prints the task's ID; --wait also waits for the answer. --worktree runs it in a new git \
worktree of its own (see below; named after the task unless NAME), in its own folder in Tern. AGENT can be several \
(codex,claude): each does the task, in a worktree of its own with --worktree, to compare their work.
  {tern} --task wait ID [--timeout S]   wait until it is done, then print its answer: the last message of its turn. \
Waits S seconds at most (default {DEFAULT_TASK_WAIT}; 0: no limit), then says it still works.
  {tern} --task read ID                 where it stands, and its answer once done
  {tern} --task send ID [--wait] [--timeout S] TEXT   ask it more (a follow-up or a fix) once it is done
  {tern} --task ls [--all]              the agent tabs in your folder (or in every folder), with their IDs
  {tern} --task answer ID [--option N] TEXT   answer a question it asks while \"blocked\", by an option's number or \
as TEXT
  {tern} --task close ID                close the tab of a task you started, ending its agent, once you need nothing \
more from it
Write a task that stands alone: the agent sees only it. Its answer is its last message, so ask it to end with what you \
need back. You need not wait for a task: when it is done, asks something or ends, Tern tells you with a message in \
your conversation (\"[Tern] Task ID ...\"), so you can go on with other work or end your turn. \"blocked\" means it \
asks something in its tab (`--task read` says what): answer a question, or an approval (by its choice's number), with \
`--task answer` when it follows from what the user asked of you; else, and for what it asks as it starts, tell the \
user, who answers in its tab.

When the user asks for a git worktree, or for work on a branch apart from their checkout, make it with Tern, not with \
`git worktree add`: Tern keeps a repository's worktrees in its {dir}/NAME (which git ignores), copies in the files \
the repository's .worktreeinclude names (.env, ...), and shows each in its sidebar under its repository.
  {tern} --worktree create [NAME] [--branch B] [--base REF]   make a worktree of your repository on a new branch \
tern/NAME (or B, made when it isn't there) from REF (default HEAD), open it in Tern, and print its folder. Work in that \
folder (cd there, or give it to your tools).
  {tern} --worktree ls                  your repository's worktrees, with their branches and uncommitted changes
  {tern} --worktree rm NAME [--force]   close it in Tern and remove it; its branch is deleted only when merged into \
another. One with uncommitted changes stays unless --force, which loses them. Remove only worktrees you made or the \
user asked you to.
  {tern} --worktree prune               remove the worktrees Tern made whose branch was worked on and is merged, with \
nothing uncommitted and nothing running in them",
        dir = tern_fs::worktree::DIR,
    )
}

/// [`guide_for`] this executable.
pub fn guide() -> String {
    let exe = std::env::current_exe().unwrap_or_else(|_| "tern".into());
    guide_for(&tern_bridge::statusline::quote(&exe.to_string_lossy()))
}

/// Run the agent command in this process's arguments, when they are one:
/// what to exit with.
pub fn run() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (flag, rest) = args.split_first()?;
    if !matches!(flag.as_str(), "--service" | "--theme" | "--shot" | "--skill" | "--agents" | "--task" | "--worktree") {
        return None;
    }
    let ctx = Context {
        cwd: std::env::current_dir().unwrap_or_default(),
        by: std::env::var("TERN_SESSION_ID").ok().and_then(|s| s.parse().ok()),
    };
    Some(match parse(flag, rest).and_then(|command| call(&ctx, &command)) {
        Ok(out) => {
            println!("{out}");
            0
        },
        Err(e) => {
            eprintln!("{e}");
            1
        },
    })
}

/// Who asks: the folder the agent runs in, and its Tern session.
struct Context {
    cwd: PathBuf,
    by: Option<u64>,
}

#[derive(Debug, PartialEq)]
enum Command {
    Guide,
    Run { name: String, command: String, cwd: Option<String>, restart: bool },
    List { all: bool },
    Logs { name: String, lines: u32 },
    Stop { name: String },
    Restart { name: String },
    Theme { name: Option<String> },
    Shot,
    Agents,
    TaskStart { agent: String, prompt: String, wait: Option<u64>, worktree: Option<Option<String>> },
    TaskWait { id: u64, timeout: u64 },
    TaskRead { id: u64 },
    TaskSend { id: u64, prompt: String, wait: Option<u64> },
    TaskList { all: bool },
    TaskClose { id: u64 },
    TaskAnswer { id: u64, text: String, option: Option<u64> },
    WorktreeCreate { name: Option<String>, branch: Option<String>, base: Option<String> },
    WorktreeList,
    WorktreeRemove { name: String, force: bool },
    WorktreePrune,
}

fn parse(flag: &str, args: &[String]) -> Result<Command, String> {
    match flag {
        "--skill" => Ok(Command::Guide),
        "--shot" => Ok(Command::Shot),
        "--agents" => Ok(Command::Agents),
        "--task" => parse_task(args),
        "--worktree" => parse_worktree(args),
        "--theme" => match args {
            [] => Ok(Command::Theme { name: None }),
            names => Ok(Command::Theme { name: Some(names.join(" ")) }),
        },
        _ => parse_service(args),
    }
}

fn parse_service(args: &[String]) -> Result<Command, String> {
    const USAGE: &str = "usage: tern --service run|ls|logs|stop|restart (tern --skill tells more)";
    let (verb, mut rest) = args.split_first().ok_or(USAGE)?;
    // The service name `rest` starts with.
    let named = |rest: &mut &[String]| -> Result<String, String> {
        let (name, tail) = rest.split_first().ok_or("a service's name is required")?;
        *rest = tail;
        Ok(name.clone())
    };
    match verb.as_str() {
        "run" => {
            let name = named(&mut rest)?;
            let (mut cwd, mut restart) = (None, false);
            loop {
                match rest {
                    [flag, dir, tail @ ..] if flag == "--cwd" => {
                        cwd = Some(dir.clone());
                        rest = tail;
                    },
                    [flag, tail @ ..] if flag == "--restart" => {
                        restart = true;
                        rest = tail;
                    },
                    [flag, tail @ ..] if flag == "--" => {
                        rest = tail;
                        break;
                    },
                    _ => break,
                }
            }
            let command = rest.join(" ");
            if command.trim().is_empty() {
                return Err("a command to run is required".into());
            }
            Ok(Command::Run { name, command, cwd, restart })
        },
        "ls" => match rest {
            [] => Ok(Command::List { all: false }),
            [all] if all == "--all" => Ok(Command::List { all: true }),
            _ => Err(USAGE.into()),
        },
        "logs" => {
            let name = named(&mut rest)?;
            let lines = match rest {
                [] => DEFAULT_LOG_LINES,
                [flag, n] if flag == "--lines" => {
                    n.parse::<u32>().map_err(|_| format!("not a number of lines: {n}"))?.clamp(1, MAX_LOG_LINES)
                },
                _ => return Err(USAGE.into()),
            };
            Ok(Command::Logs { name, lines })
        },
        "stop" => Ok(Command::Stop { name: named(&mut rest)? }),
        "restart" => Ok(Command::Restart { name: named(&mut rest)? }),
        _ => Err(USAGE.into()),
    }
}

/// What `--task start` and `--task send` are given before their text.
struct TaskOptions {
    /// How long to wait for the answer, when waiting.
    wait: Option<u64>,
    /// In a new worktree, named or not (`start` only).
    worktree: Option<Option<String>>,
    text: String,
}

fn parse_task(args: &[String]) -> Result<Command, String> {
    const USAGE: &str = "usage: tern --task start|wait|read|send|answer|ls|close (tern --skill tells more)";
    let (verb, rest) = args.split_first().ok_or(USAGE)?;
    let id_of = |s: Option<&String>| -> Result<(u64, &[String]), String> {
        let s = s.ok_or("a task's ID is required (tern --task ls lists them)")?;
        let id = s.parse::<u64>().map_err(|_| format!("not a task ID: {s}"))?;
        Ok((id, &rest[1..]))
    };
    // `--wait`, `--timeout S` and (for `start`) `--worktree[=NAME]` before
    // the text; `wait` is how long to wait, when waiting.
    let options = |mut rest: &[String], start: bool| -> Result<TaskOptions, String> {
        let (mut wait, mut timeout, mut worktree) = (false, None, None);
        loop {
            match rest {
                [flag, tail @ ..] if start && flag == "--worktree" => {
                    worktree = Some(None);
                    rest = tail;
                },
                [flag, tail @ ..] if start && flag.starts_with("--worktree=") => {
                    worktree = Some(Some(flag["--worktree=".len()..].to_owned()));
                    rest = tail;
                },
                [flag, tail @ ..] if flag == "--wait" => {
                    wait = true;
                    rest = tail;
                },
                [flag, s, tail @ ..] if flag == "--timeout" => {
                    timeout = Some(s.parse::<u64>().map_err(|_| format!("not a number of seconds: {s}"))?);
                    rest = tail;
                },
                [flag, tail @ ..] if flag == "--" => {
                    rest = tail;
                    break;
                },
                _ => break,
            }
        }
        let wait = (wait || timeout.is_some()).then(|| timeout.unwrap_or(DEFAULT_TASK_WAIT));
        let text = rest.join(" ");
        if text.trim().is_empty() {
            return Err("a task to give is required".into());
        }
        Ok(TaskOptions { wait, worktree, text })
    };
    match verb.as_str() {
        "start" => {
            let (agent, rest) = rest.split_first().ok_or("an agent is required (tern --agents lists them)")?;
            let TaskOptions { wait, worktree, text: prompt } = options(rest, true)?;
            Ok(Command::TaskStart { agent: agent.clone(), prompt, wait, worktree })
        },
        "send" => {
            let (id, rest) = id_of(rest.first())?;
            let TaskOptions { wait, text: prompt, .. } = options(rest, false)?;
            Ok(Command::TaskSend { id, prompt, wait })
        },
        "wait" => {
            let (id, rest) = id_of(rest.first())?;
            let timeout = match rest {
                [] => DEFAULT_TASK_WAIT,
                [flag, s] if flag == "--timeout" => s.parse::<u64>().map_err(|_| format!("not a number of seconds: {s}"))?,
                _ => return Err(USAGE.into()),
            };
            Ok(Command::TaskWait { id, timeout })
        },
        "read" => match id_of(rest.first())? {
            (id, []) => Ok(Command::TaskRead { id }),
            _ => Err(USAGE.into()),
        },
        "answer" => {
            let (id, mut rest) = id_of(rest.first())?;
            let mut option = None;
            if let [flag, n, tail @ ..] = rest
                && flag == "--option"
            {
                option = Some(n.parse::<u64>().map_err(|_| format!("not an option's number: {n}"))?);
                rest = tail;
            }
            let text = rest.join(" ");
            if option.is_none() && text.trim().is_empty() {
                return Err("an answer is required: TEXT, yes or no, or --option N".into());
            }
            Ok(Command::TaskAnswer { id, text, option })
        },
        "close" => match id_of(rest.first())? {
            (id, []) => Ok(Command::TaskClose { id }),
            _ => Err(USAGE.into()),
        },
        "ls" => match rest {
            [] => Ok(Command::TaskList { all: false }),
            [all] if all == "--all" => Ok(Command::TaskList { all: true }),
            _ => Err(USAGE.into()),
        },
        _ => Err(USAGE.into()),
    }
}

fn parse_worktree(args: &[String]) -> Result<Command, String> {
    const USAGE: &str = "usage: tern --worktree create|ls|rm|prune (tern --skill tells more)";
    let (verb, mut rest) = args.split_first().ok_or(USAGE)?;
    match verb.as_str() {
        "create" => {
            let (mut name, mut branch, mut base) = (None, None, None);
            loop {
                match rest {
                    [flag, b, tail @ ..] if flag == "--branch" => {
                        branch = Some(b.clone());
                        rest = tail;
                    },
                    [flag, r, tail @ ..] if flag == "--base" => {
                        base = Some(r.clone());
                        rest = tail;
                    },
                    [flag, ..] if flag.starts_with("--") => return Err(format!("no option {flag} ({USAGE})")),
                    [n, tail @ ..] if name.is_none() => {
                        name = Some(n.clone());
                        rest = tail;
                    },
                    [] => break,
                    _ => return Err(USAGE.into()),
                }
            }
            Ok(Command::WorktreeCreate { name, branch, base })
        },
        "ls" => match rest {
            [] => Ok(Command::WorktreeList),
            _ => Err(USAGE.into()),
        },
        "prune" => match rest {
            [] => Ok(Command::WorktreePrune),
            _ => Err(USAGE.into()),
        },
        "rm" => match rest {
            [name] => Ok(Command::WorktreeRemove { name: name.clone(), force: false }),
            [a, b] if a == "--force" || b == "--force" => {
                let name = if a == "--force" { b } else { a };
                Ok(Command::WorktreeRemove { name: name.clone(), force: true })
            },
            [] => Err("a worktree's name is required (tern --worktree ls lists them)".into()),
            _ => Err(USAGE.into()),
        },
        _ => Err(USAGE.into()),
    }
}

fn call(ctx: &Context, command: &Command) -> Result<String, String> {
    match command {
        Command::Guide => Ok(guide()),
        Command::Agents => {
            let (registry, problems) = tern_agent::Registry::load(Some(&crate::dirs::config().join("agents")));
            Ok(agents_text(&registry, &problems))
        },
        Command::Theme { name: None } => ask_ui(json!({ "op": "status" })).map(|a| describe_status(&a)),
        Command::Theme { name: Some(name) } => ask_ui(json!({ "op": "set_theme", "name": name })).map(|a| describe_status(&a)),
        Command::Shot => {
            let a = ask_ui(json!({ "op": "shot", "folder": ctx.cwd }))?;
            let path = PathBuf::from(a.get("path").and_then(Value::as_str).ok_or("no picture")?);
            let (w, h) = (a.get("width").and_then(Value::as_u64).unwrap_or(0), a.get("height").and_then(Value::as_u64).unwrap_or(0));
            Ok(format!("{} ({w}×{h} pixels)", path.display()))
        },
        Command::TaskStart { .. }
        | Command::TaskWait { .. }
        | Command::TaskRead { .. }
        | Command::TaskSend { .. }
        | Command::TaskList { .. }
        | Command::TaskClose { .. }
        | Command::TaskAnswer { .. } => task_call(ctx, command),
        Command::WorktreeCreate { name, branch, base } => {
            let made = make_worktree(ctx, name.as_deref(), branch.as_deref(), base.as_deref())?;
            Ok(format!("{}\nWork in that folder: cd there, or give it to your tools.", describe_made(&made)))
        },
        Command::WorktreeList => list_worktrees(ctx),
        Command::WorktreeRemove { name, force } => remove_worktree(ctx, name, *force),
        Command::WorktreePrune => prune_worktrees(ctx),
        _ => services_call(ctx, command),
    }
}

/// The task text: `-` reads it from stdin (for one with quotes and lines).
fn task_text(prompt: &str) -> Result<String, String> {
    if prompt != "-" {
        return Ok(prompt.to_owned());
    }
    let mut text = String::new();
    io::Read::read_to_string(&mut io::stdin(), &mut text).map_err(|e| format!("can't read the task from stdin: {e}"))?;
    if text.trim().is_empty() { Err("the task read from stdin is empty".into()) } else { Ok(text) }
}

fn task_call(ctx: &Context, command: &Command) -> Result<String, String> {
    match command {
        Command::TaskStart { agent, prompt, wait, worktree } => {
            let prompt = task_text(prompt)?;
            // `codex,claude`: the same task to each (a worktree each, with
            // --worktree), to compare or pick from.
            let agents: Vec<&str> = agent.split(',').map(str::trim).filter(|a| !a.is_empty()).collect();
            if agents.len() > 1 && wait.is_some() {
                return Err("--wait waits for one agent: give the task to several without it; Tern tells you as each is done".into());
            }
            if agents.len() > 1 || worktree.is_some() {
                // Every agent known before any worktree is made for one.
                let (registry, _) = tern_agent::Registry::load(Some(&crate::dirs::config().join("agents")));
                if let Some(bad) = agents.iter().find(|a| registry.get(a).is_none()) {
                    let known: Vec<&str> = registry.all().map(|d| d.id.as_str()).collect();
                    return Err(format!("no agent {bad} (Tern knows: {})", known.join(", ")));
                }
            }
            let mut out = vec![];
            for agent in &agents {
                // In a new worktree: its own folder in Tern, the tab there,
                // named as asked or from the task, and for which agent.
                let made = match worktree {
                    Some(name) => {
                        let wanted = name.clone().or_else(|| crate::worktrees::name_from(&prompt));
                        let wanted = match (wanted, agents.len() > 1) {
                            (Some(n), true) => Some(format!("{n}-{agent}")),
                            (n, _) => n,
                        };
                        let name = match (wanted, name) {
                            // A name given is that name; one made up is one not taken.
                            (Some(n), Some(_)) => Some(n),
                            (Some(n), None) => Some(crate::worktrees::free_name(&ctx.cwd, &n)?),
                            (None, _) => None,
                        };
                        Some(make_worktree(ctx, name.as_deref(), None, None)?)
                    },
                    None => None,
                };
                let (folder, exact) = made.as_ref().map_or((&ctx.cwd, false), |m| (&m.path, true));
                let a = ask_ui(json!({
                    "op": "task_start", "agent": agent, "prompt": prompt, "folder": folder, "exact": exact, "by": ctx.by,
                }))?;
                let id = a.get("id").and_then(Value::as_u64).ok_or("no task ID in Tern's answer")?;
                let name = a.get("name").and_then(Value::as_str).unwrap_or(agent);
                let started = match &made {
                    Some(m) => format!("{}\nStarted {name} there in a new tab (task {id}); the user can watch it there.", describe_made(m)),
                    None => format!("Started {name} in a new tab beside yours (task {id}); the user can watch it there."),
                };
                out.push(match wait {
                    Some(timeout) => wait_task(id, *timeout, ctx.by).map(|answer| format!("{started}\n\n{answer}"))?,
                    None => format!("{started}\n`tern --task wait {id}` waits for its answer."),
                });
            }
            Ok(out.join("\n\n"))
        },
        Command::TaskSend { id, prompt, wait } => {
            let prompt = task_text(prompt)?;
            ask_ui(json!({ "op": "task_send", "id": id, "prompt": prompt, "by": ctx.by }))?;
            match wait {
                Some(timeout) => wait_task(*id, *timeout, ctx.by),
                None => Ok(format!("Sent. `tern --task wait {id}` waits for its answer.")),
            }
        },
        Command::TaskWait { id, timeout } => wait_task(*id, *timeout, ctx.by),
        Command::TaskRead { id } => {
            let a = ask_ui(json!({ "op": "task_read", "id": id, "by": ctx.by }))?;
            describe_task(&a, None)
        },
        Command::TaskAnswer { id, text, option } => {
            ask_ui(json!({ "op": "task_answer", "id": id, "answer": text, "option": option, "by": ctx.by }))?;
            // Its screen takes a moment to show what the answer did.
            std::thread::sleep(ANSWER_SETTLE);
            let a = ask_ui(json!({ "op": "task_read", "id": id, "by": ctx.by }))?;
            describe_task(&a, None).map(|now| format!("Answered.\n\n{now}"))
        },
        Command::TaskClose { id } => {
            ask_ui(json!({ "op": "task_close", "id": id, "by": ctx.by }))?;
            Ok(format!("Closed task {id}'s tab; its agent has ended."))
        },
        Command::TaskList { all } => {
            let a = ask_ui(json!({ "op": "task_list", "folder": ctx.cwd, "all": all }))?;
            let tasks = a.get("tasks").and_then(Value::as_array).cloned().unwrap_or_default();
            if tasks.is_empty() {
                return Ok(if *all { "No agent tabs." } else { "No agent tabs in this folder (--all lists every folder's)." }.into());
            }
            Ok(tasks.iter().map(task_line).collect::<Vec<_>>().join("\n"))
        },
        _ => Err("not a task command".into()),
    }
}

/// Make a worktree of the caller's repository and show it in Tern, under
/// its repository (when Tern runs: the worktree is made either way).
fn make_worktree(ctx: &Context, name: Option<&str>, branch: Option<&str>, base: Option<&str>) -> Result<crate::worktrees::Made, String> {
    let made = crate::worktrees::make(&ctx.cwd, name, branch, base)?;
    if let Err(e) = ask_ui(json!({ "op": "worktree_open", "path": made.path, "by": ctx.by })) {
        log::warn!("worktree {} not opened in Tern: {e}", made.path.display());
    }
    Ok(made)
}

/// A worktree just made, for reading.
fn describe_made(m: &crate::worktrees::Made) -> String {
    use crate::worktrees::{SETUP_FILE, approved, scripts_of};
    let mut out = format!("Made worktree {} on branch {}, of {}.", m.path.display(), m.branch, m.main.display());
    if !m.copied.is_empty() {
        let files: Vec<String> = m.copied.iter().map(|p| p.display().to_string()).collect();
        out.push_str(&format!("\nCopied from the main checkout (.worktreeinclude): {}.", files.join(", ")));
    }
    match scripts_of(&m.main) {
        Ok(Some(scripts)) if scripts.setup.is_none() => {},
        Ok(Some(scripts)) if approved(&scripts) => out.push_str(&format!(
            "\nIts setup ({SETUP_FILE}: {}) runs in Tern as its service `setup`: `tern --service logs setup` from that folder \
shows how it goes.",
            scripts.setup.unwrap_or_default()
        )),
        Ok(Some(scripts)) => out.push_str(&format!(
            "\nThe repository names a setup for it ({SETUP_FILE}: {}), which runs once the user approves it: Tern asks \
them in its window. Tell the user; don't run it yourself.",
            scripts.setup.unwrap_or_default()
        )),
        Ok(None) => {},
        Err(e) => out.push_str(&format!("\nIts setup is not run: {e}")),
    }
    out
}

/// The caller's repository's worktrees, one a line: folder, branch, what
/// it has.
fn list_worktrees(ctx: &Context) -> Result<String, String> {
    use tern_fs::worktree;
    let main = worktree::main_root(&ctx.cwd)?;
    let here = tern_fs::git::repo_for(&ctx.cwd);
    let lines: Vec<String> = worktree::list(&main)?
        .into_iter()
        .map(|w| {
            let branch = w.branch.clone().unwrap_or_else(|| format!("(detached at {})", w.head.get(..8).unwrap_or(&w.head)));
            let mut notes = vec![];
            if w.main {
                notes.push("main checkout".to_owned());
            }
            if w.prunable {
                notes.push("its folder is gone".to_owned());
            } else {
                match worktree::changes(&w.path) {
                    Ok(c) if c.is_empty() => {},
                    Ok(c) => notes.push(format!("{} uncommitted changes", c.len())),
                    Err(_) => notes.push("status unknown".to_owned()),
                }
            }
            if w.locked {
                notes.push("locked".to_owned());
            }
            if here.as_ref() == Some(&w.path) {
                notes.push("you are here".to_owned());
            }
            format!("{}\t{branch}\t{}", w.path.display(), notes.join(", "))
        })
        .collect();
    Ok(lines.join("\n"))
}

/// Remove the caller's repository's worktree `name`: closed in Tern first
/// (its terminals end), its branch deleted when merged.
fn remove_worktree(ctx: &Context, name: &str, force: bool) -> Result<String, String> {
    use tern_fs::worktree;
    let (main, w) = crate::worktrees::find(&ctx.cwd, name)?;
    if w.main {
        return Err(format!("{} is the repository's main checkout, not a worktree to remove", w.path.display()));
    }
    if tern_fs::git::repo_for(&ctx.cwd).is_some_and(|top| top == w.path) {
        return Err(format!("you run in {}: remove it from another folder", w.path.display()));
    }
    if !w.prunable && !force {
        let changes = worktree::changes(&w.path)?;
        if !changes.is_empty() {
            let shown: Vec<&str> = changes.iter().take(10).map(String::as_str).collect();
            let more = if changes.len() > 10 { format!("\n... and {} more", changes.len() - 10) } else { String::new() };
            return Err(format!(
                "{} has uncommitted changes; commit them, or --force removes it and loses them:\n{}{more}",
                w.path.display(),
                shown.join("\n")
            ));
        }
    }
    match ask_ui(json!({ "op": "worktree_close", "path": w.path, "force": force })) {
        Ok(_) => {},
        // No window: nothing shows it.
        Err(e) if e == NOT_RUNNING || e == NOT_OPEN => {},
        Err(e) => return Err(e),
    }
    let done = crate::worktrees::remove(&main, &w, force);
    // Its card goes, removed or not (a failed removal may have done part).
    let _ = ask_ui(json!({ "op": "worktrees_changed" }));
    done
}

/// Remove the caller's repository's worktrees that are done with: those
/// Tern made (in its `.worktrees`) whose branch was worked on and is merged
/// into the main checkout's, with nothing uncommitted and nothing running in
/// Tern; and forget those whose folders are gone. What it did, a line each.
fn prune_worktrees(ctx: &Context) -> Result<String, String> {
    use tern_fs::worktree;
    let main = worktree::main_root(&ctx.cwd)?;
    let here = tern_fs::git::repo_for(&ctx.cwd);
    let place = main.join(worktree::DIR);
    let mut out = vec![];
    for w in worktree::list(&main)? {
        if w.main || !w.path.starts_with(&place) || w.prunable {
            continue;
        }
        let name = w.path.file_name().map_or_else(|| w.path.display().to_string(), |n| n.to_string_lossy().into_owned());
        let Some(branch) = &w.branch else { continue };
        let keep = if w.locked {
            Some("locked".to_owned())
        } else if here.as_ref() == Some(&w.path) {
            Some("you run in it".to_owned())
        } else if !worktree::worked_on(&main, branch) {
            Some("nothing done on its branch yet".to_owned())
        } else if !worktree::merged(&main, branch) {
            Some(format!("{branch} is not merged"))
        } else {
            match worktree::changes(&w.path) {
                Ok(c) if c.is_empty() => None,
                Ok(c) => Some(format!("{} uncommitted changes", c.len())),
                Err(e) => Some(e),
            }
        };
        if let Some(why) = keep {
            out.push(format!("kept {name}: {why}"));
            continue;
        }
        match ask_ui(json!({ "op": "worktree_close", "path": w.path, "force": false })) {
            Ok(_) => {},
            Err(e) if e == NOT_RUNNING || e == NOT_OPEN => {},
            Err(e) => {
                out.push(format!("kept {name}: {e}"));
                continue;
            },
        }
        out.push(match crate::worktrees::remove(&main, &w, false) {
            Ok(done) => done,
            Err(e) => format!("kept {name}: {e}"),
        });
    }
    worktree::prune(&main)?;
    let _ = ask_ui(json!({ "op": "worktrees_changed" }));
    Ok(if out.is_empty() { "No worktrees of Tern's in this repository.".into() } else { out.join("\n") })
}

/// Ask how task `id` stands until it isn't working, `timeout` seconds at
/// most (0: no limit), for agent tab `by`.
fn wait_task(id: u64, timeout: u64, by: Option<u64>) -> Result<String, String> {
    let start = Instant::now();
    loop {
        let a = ask_ui(json!({ "op": "task_read", "id": id, "by": by }))?;
        let working = a.get("state").and_then(Value::as_str) == Some("working");
        let waited = start.elapsed();
        if !working || (timeout > 0 && waited >= Duration::from_secs(timeout)) {
            return describe_task(&a, Some(waited));
        }
        std::thread::sleep(TASK_POLL);
    }
}

/// What a blocked task's agent asks, and how it is answered: as Herdr's
/// guide has it, the giving agent asks the user before it answers.
fn describe_asks(asks: Option<&Value>, id: u64) -> String {
    let s = |k: &str| asks.and_then(|a| a.get(k)).and_then(Value::as_str).unwrap_or_default();
    let what = match s("kind") {
        "startup" => "it asks a question as it starts (whether to trust the folder, ...).".to_owned(),
        "approval" => format!("it asks to run {}: {}.", s("tool"), s("detail")),
        "question" if s("detail").is_empty() => "it asks a question (see its screen).".into(),
        "question" => format!("it asks: {}.", s("detail")),
        _ => "it asks something or waits for an approval (see its screen).".into(),
    };
    match s("kind") {
        "question" => {
            return format!(
                " {what} If the answer follows from what the user asked of you, answer with `tern --task answer {id} --option N` \
or `tern --task answer {id} TEXT`; else ask the user."
            );
        },
        "approval" => {
            return format!(
                " {what} If what the user asked of you covers it, choose on its screen with `tern --task answer {id} --option N`; \
else tell the user, who answers it in its tab."
            );
        },
        _ => {},
    }
    format!(" {what} Only the user answers this, in its tab: tell them, then wait again.")
}

/// One agent tab, for `--task ls`.
fn task_line(a: &Value) -> String {
    let s = |k: &str| a.get(k).and_then(Value::as_str).unwrap_or_default();
    let id = a.get("id").and_then(Value::as_u64).unwrap_or_default();
    let name = if s("name").is_empty() { "agent" } else { s("name") };
    format!("{id}\t{name}\t{}\t{}\t{}", s("state"), s("title"), s("folder"))
}

/// How a task stands, for reading: `waited` is how long `wait` waited.
/// A failed task is an error.
fn describe_task(a: &Value, waited: Option<Duration>) -> Result<String, String> {
    let s = |k: &str| a.get(k).and_then(Value::as_str).unwrap_or_default();
    let id = a.get("id").and_then(Value::as_u64).unwrap_or_default();
    let name = if s("name").is_empty() { "The agent" } else { s("name") };
    let head = format!("{name} (task {id}, in {})", s("folder"));
    let screen = || format!("\n\nIts screen:\n{}", s("screen"));
    Ok(match s("state") {
        "done" => match a.get("reply").and_then(Value::as_str) {
            Some("") => format!("{head} is done, without an answer (its turn was stopped?)."),
            Some(reply) => format!("{head} is done. Its answer:\n\n{reply}"),
            None => format!("{head} is done. Tern has no hooks for it, so here is its screen instead of its answer.{}", screen()),
        },
        "working" => match waited {
            Some(w) => format!("{head} still works after {}s. `tern --task wait {id}` waits again.", w.as_secs()),
            None => format!("{head} is working."),
        },
        "blocked" => format!("{head} is blocked:{}{}", describe_asks(a.get("asks"), id), screen()),
        "idle" => format!("{head} was given nothing yet."),
        "ended" => format!("{head} has ended: its program exited.{}", screen()),
        "failed" => return Err(format!("{head}: the task never reached it: {}", s("error"))),
        other => format!("{head}: {other}"),
    })
}

/// The agents Tern knows, one per line (id, name, command, what Tern does
/// for it), then what is wrong in the user's files (`agents/<id>.json`).
fn agents_text(registry: &tern_agent::Registry, problems: &[String]) -> String {
    use tern_agent::Capability::*;
    const CAPS: [(tern_agent::Capability, &str); 9] = [
        (Launch, "launch"),
        (Status, "status"),
        (Session, "session"),
        (Transcript, "transcript"),
        (Context, "context"),
        (Steer, "steer"),
        (Usage, "usage"),
        (Chat, "chat"),
        (Skills, "skills"),
    ];
    let mut out: Vec<String> = registry
        .all()
        .map(|d| {
            let caps: Vec<&str> = CAPS.iter().filter(|(c, _)| registry.has(d.id.as_str(), *c)).map(|(_, n)| *n).collect();
            let caps = if caps.is_empty() { "detect, screen".to_owned() } else { format!("detect, screen, {}", caps.join(", ")) };
            format!("{}\t{}\t{}\t{caps}", d.id, d.name, d.command)
        })
        .collect();
    out.extend(problems.iter().map(|p| format!("error: {p}")));
    out.join("\n")
}

/// Why Tern can't be reached from an agent's sandbox, and what to do (as
/// Orca's guide says of `runtime_access_denied`).
fn sandboxed(e: &io::Error) -> String {
    format!("can't reach Tern: your sandbox blocked the connection ({e}). Run the command again with escalated permissions.")
}

/// [`ask_ui`]'s error when Tern isn't running.
const NOT_RUNNING: &str = "Tern isn't running.";
/// [`ask_ui`]'s error when Tern's daemon runs without the app (the
/// daemon's own words, `tern_mux` `server`).
const NOT_OPEN: &str = "Tern isn't open: no window to ask";

/// Ask the Tern window `request` (see `agent_ui`): its answer, or why not.
fn ask_ui(request: Value) -> Result<Value, String> {
    let mut s = Services::connect(&tern_mux::paths::mux_dir(), false).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => NOT_RUNNING.to_owned(),
        io::ErrorKind::PermissionDenied => sandboxed(&e),
        _ => format!("can't reach Tern: {e}"),
    })?;
    let answer = s.ask_ui(request.to_string()).map_err(|e| e.to_string())?;
    let answer: Value = serde_json::from_str(&answer).map_err(|e| format!("bad answer from Tern: {e}"))?;
    match answer.get("error").and_then(Value::as_str) {
        Some(e) => Err(e.to_owned()),
        None => Ok(answer),
    }
}

/// `agent_ui`'s status, for reading.
fn describe_status(a: &Value) -> String {
    let s = |k: &str| a.get(k).and_then(Value::as_str).unwrap_or_default();
    let themes: Vec<String> = a
        .get("themes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|t| {
            let name = t.get("name").and_then(Value::as_str).unwrap_or_default();
            format!("{name} ({})", if t.get("dark").and_then(Value::as_bool).unwrap_or(false) { "dark" } else { "light" })
        })
        .collect();
    let problems = match a.get("problems").and_then(Value::as_str) {
        Some(p) => format!("Problems (the rest applies): {p}"),
        None => "No problems.".into(),
    };
    format!(
        "Theme: {}\nThemes: {}\n{problems}\nSettings: {}\nThemes folder: {}",
        s("theme"),
        themes.join(", "),
        s("settings"),
        s("themes_dir")
    )
}

fn services_call(ctx: &Context, command: &Command) -> Result<String, String> {
    let connect = |start| {
        Services::connect(&tern_mux::paths::mux_dir(), start).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => "Tern's daemon isn't running: no services.".to_owned(),
            io::ErrorKind::PermissionDenied => sandboxed(&e),
            _ => format!("can't reach Tern: {e}"),
        })
    };
    match command {
        Command::Run { name, command, cwd, restart } => {
            let cwd = cwd.as_deref().map_or_else(|| ctx.cwd.clone(), |c| absolute(&ctx.cwd, c));
            if !cwd.is_dir() {
                return Err(format!("no folder {}", cwd.display()));
            }
            let mut s = connect(true)?;
            let list = s.list().map_err(|e| e.to_string())?;
            let same = list.iter().filter(|i| i.service.as_ref().is_some_and(|sv| sv.name == *name && sv.cwd == cwd));
            for old in same {
                if !old.exited && !restart {
                    return Ok(format!("{name} already runs (--restart restarts it).\n\n{}", describe(old, &ports_of(&[old]))));
                }
                s.close(old.id).map_err(|e| e.to_string())?;
            }
            let spec = ServiceSpec { name: name.clone(), command: command.clone(), cwd, by: ctx.by, started: 0 };
            start(&mut s, spec)
        },
        Command::List { all } => {
            let mut s = connect(false)?;
            let list = s.list().map_err(|e| e.to_string())?;
            let shown: Vec<&SessionInfo> =
                list.iter().filter(|i| i.service.as_ref().is_some_and(|sv| *all || related(&sv.cwd, &ctx.cwd))).collect();
            if shown.is_empty() {
                return Ok(if *all { "No services." } else { "No services in this folder (--all lists every folder's)." }.into());
            }
            let ports = ports_of(&shown);
            Ok(shown.iter().map(|i| describe(i, &ports)).collect::<Vec<_>>().join("\n\n"))
        },
        Command::Logs { name, lines } => {
            let mut s = connect(false)?;
            let info = find(&mut s, ctx, name)?;
            let text = s.text(info.id, *lines).map_err(|e| e.to_string())?.ok_or("it has gone")?;
            Ok(format!("{}\n\n{text}", describe(&info, &ports_of(&[&info]))))
        },
        Command::Stop { name } => {
            let mut s = connect(false)?;
            let info = find(&mut s, ctx, name)?;
            s.close(info.id).map_err(|e| e.to_string())?;
            Ok(format!("Stopped {}.", service_name(&info)))
        },
        Command::Restart { name } => {
            let mut s = connect(true)?;
            let info = find(&mut s, ctx, name)?;
            let spec = info.service.clone().ok_or("not a service")?;
            s.close(info.id).map_err(|e| e.to_string())?;
            start(&mut s, ServiceSpec { by: ctx.by.or(spec.by), ..spec })
        },
        _ => Err("not a service command".into()),
    }
}

/// Start `spec`, wait for it to listen or end (a while at most), and say
/// how it went.
fn start(s: &mut Services, spec: ServiceSpec) -> Result<String, String> {
    let (id, pid) = s.run(spawn_config(&spec), spec).map_err(|e| format!("can't start it: {e}"))?;
    let deadline = Instant::now() + START_WAIT;
    let (info, ports) = loop {
        std::thread::sleep(Duration::from_millis(500));
        let info = s.list().map_err(|e| e.to_string())?.into_iter().find(|i| i.id == id).ok_or("it ended and was removed")?;
        let ports = services::listening_ports(&[pid]);
        if info.exited || ports.get(&pid).is_some_and(|p| !p.is_empty()) || Instant::now() >= deadline {
            break (info, ports);
        }
    };
    let text = s.text(id, 30).map_err(|e| e.to_string())?.unwrap_or_default();
    let head = if info.exited { "It ended already." } else { "Started; the user sees it in Tern's Services panel." };
    Ok(format!("{head}\n\n{}\n\nOutput so far:\n{text}", describe(&info, &ports)))
}

/// Start `spec` as a service (a worktree's setup), not waiting on it.
pub fn run_service(spec: ServiceSpec) -> Result<(), String> {
    let mut s = Services::connect(&tern_mux::paths::mux_dir(), true).map_err(|e| e.to_string())?;
    s.run(spawn_config(&spec), spec).map(|_| ()).map_err(|e| e.to_string())
}

/// Stop service `id` and start `spec` again (the Services panel's Restart).
pub fn restart(id: u64, spec: ServiceSpec) -> Result<(), String> {
    let mut s = Services::connect(&tern_mux::paths::mux_dir(), true).map_err(|e| e.to_string())?;
    s.close(id).map_err(|e| e.to_string())?;
    s.run(spawn_config(&spec), spec).map(|_| ()).map_err(|e| e.to_string())
}

/// How to run `spec`'s command: in a login shell, so the user's PATH holds.
fn spawn_config(spec: &ServiceSpec) -> SpawnConfig {
    let args =
        if cfg!(windows) { crate::platform::shell_command_args(&spec.command) } else { vec!["-lic".to_owned(), spec.command.clone()] };
    let mut env = std::collections::HashMap::new();
    env.insert("TERN_SERVICE".into(), spec.name.clone());
    SpawnConfig {
        program: Some(crate::platform::shell()),
        args,
        cwd: Some(spec.cwd.clone()),
        env,
        size: TermSize { cols: 120, rows: 40, cell_width: 8, cell_height: 16 },
        scrollback: 10_000,
    }
}

/// The service `name` (or id) is: see [`pick`].
fn find(s: &mut Services, ctx: &Context, name: &str) -> Result<SessionInfo, String> {
    pick(s.list().map_err(|e| e.to_string())?, &ctx.cwd, name)
}

/// The service an agent in `cwd` means by `name`: of those so named in its
/// folder, a running one, the newest. Another folder's service only by its
/// id, so an agent can't stop or read another project's by a name they share
/// (as Orca's CLI keeps to the caller's worktree unless asked).
fn pick(list: Vec<SessionInfo>, cwd: &Path, name: &str) -> Result<SessionInfo, String> {
    let services = list.into_iter().filter(|i| i.service.is_some());
    let (by_id, named): (Vec<SessionInfo>, Vec<SessionInfo>) = services
        .filter(|i| i.id.to_string() == name || i.service.as_ref().is_some_and(|sv| sv.name == name))
        .partition(|i| i.id.to_string() == name);
    if let Some(info) = by_id.into_iter().next() {
        return Ok(info);
    }
    let (mut here, elsewhere): (Vec<SessionInfo>, Vec<SessionInfo>) =
        named.into_iter().partition(|i| related(&i.service.as_ref().unwrap().cwd, cwd));
    here.sort_by_key(|i| (i.exited, std::cmp::Reverse(i.service.as_ref().unwrap().started)));
    if let Some(info) = here.into_iter().next() {
        return Ok(info);
    }
    if elsewhere.is_empty() {
        return Err(format!("no service {name} (--service ls lists them)"));
    }
    let others: Vec<String> = elsewhere.iter().map(|i| format!("{} (id {})", i.service.as_ref().unwrap().cwd.display(), i.id)).collect();
    Err(format!("no service {name} in this folder. Another folder has one: {}. Give its id to use it.", others.join(", ")))
}

/// One folder is in the other.
fn related(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

fn absolute(base: &Path, p: &str) -> PathBuf {
    let p = Path::new(p);
    if p.is_absolute() { p.to_owned() } else { base.join(p) }
}

fn ports_of(infos: &[&SessionInfo]) -> std::collections::HashMap<u32, Vec<u16>> {
    let pids: Vec<u32> = infos.iter().filter(|i| !i.exited).map(|i| i.pid).collect();
    services::listening_ports(&pids)
}

fn service_name(info: &SessionInfo) -> String {
    info.service.as_ref().map_or_else(|| info.id.to_string(), |s| s.name.clone())
}

/// A service as the agent reads it.
fn describe(info: &SessionInfo, ports: &std::collections::HashMap<u32, Vec<u16>>) -> String {
    let Some(sv) = &info.service else { return String::new() };
    let state = match (info.exited, info.exit_code) {
        (false, _) => format!("running (pid {}, up {})", info.pid, uptime(sv.started)),
        (true, Some(code)) => format!("exited with code {code}"),
        (true, None) => "exited".to_owned(),
    };
    let ports = ports
        .get(&info.pid)
        .filter(|p| !p.is_empty())
        .map(|p| p.iter().map(|p| format!("http://localhost:{p}")).collect::<Vec<_>>().join(", "));
    let mut out = format!("{} [{}]: {state}\n  command: {}\n  folder: {}", sv.name, info.id, sv.command, sv.cwd.display());
    if let Some(ports) = ports {
        out.push_str(&format!("\n  listening: {ports}"));
    }
    out
}

fn uptime(started: u64) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let s = now.saturating_sub(started);
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m {}s", s / 60, s % 60),
        _ => format!("{}h {}m", s / 3600, s % 3600 / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    fn service_args(line: &str) -> Result<Command, String> {
        parse("--service", &args(line))
    }

    #[test]
    fn parses_a_service_to_run() {
        let run = |command: &str, cwd: Option<&str>, restart| Command::Run {
            name: "web".into(),
            command: command.into(),
            cwd: cwd.map(str::to_owned),
            restart,
        };
        assert_eq!(service_args("run web npm run dev"), Ok(run("npm run dev", None, false)));
        assert_eq!(service_args("run web --cwd site --restart npm start"), Ok(run("npm start", Some("site"), true)));
        // The command's own options are its own.
        assert_eq!(service_args("run web vite --restart"), Ok(run("vite --restart", None, false)));
        assert_eq!(service_args("run web -- --restart x"), Ok(run("--restart x", None, false)));
        assert!(service_args("run web").unwrap_err().contains("command"));
        assert!(service_args("run").unwrap_err().contains("name"));
    }

    #[test]
    fn parses_the_other_service_commands() {
        assert_eq!(service_args("ls"), Ok(Command::List { all: false }));
        assert_eq!(service_args("ls --all"), Ok(Command::List { all: true }));
        assert_eq!(service_args("logs web"), Ok(Command::Logs { name: "web".into(), lines: DEFAULT_LOG_LINES }));
        assert_eq!(service_args("logs web --lines 5"), Ok(Command::Logs { name: "web".into(), lines: 5 }));
        assert_eq!(service_args("logs web --lines 999999"), Ok(Command::Logs { name: "web".into(), lines: MAX_LOG_LINES }));
        assert!(service_args("logs web --lines x").is_err());
        assert_eq!(service_args("stop web"), Ok(Command::Stop { name: "web".into() }));
        assert_eq!(service_args("restart 7"), Ok(Command::Restart { name: "7".into() }));
        assert!(service_args("").unwrap_err().starts_with("usage"));
        assert!(service_args("nope").unwrap_err().starts_with("usage"));
    }

    #[test]
    fn parses_the_window_commands() {
        assert_eq!(parse("--theme", &[]), Ok(Command::Theme { name: None }));
        assert_eq!(parse("--theme", &args("Tern Dark")), Ok(Command::Theme { name: Some("Tern Dark".into()) }));
        assert_eq!(parse("--shot", &[]), Ok(Command::Shot));
        assert_eq!(parse("--skill", &[]), Ok(Command::Guide));
        assert_eq!(parse("--agents", &[]), Ok(Command::Agents));
    }

    #[test]
    fn parses_the_task_commands() {
        let task = |line: &str| parse("--task", &args(line));
        let start = |prompt: &str, wait| Command::TaskStart { agent: "codex".into(), prompt: prompt.into(), wait, worktree: None };
        assert_eq!(task("start codex review the diff"), Ok(start("review the diff", None)));
        assert_eq!(task("start codex --wait review"), Ok(start("review", Some(DEFAULT_TASK_WAIT))));
        assert_eq!(task("start codex --timeout 0 review"), Ok(start("review", Some(0))));
        assert_eq!(task("start codex --wait --timeout 30 -- --wait is text"), Ok(start("--wait is text", Some(30))));
        assert_eq!(task("start codex -"), Ok(start("-", None)));
        let in_worktree = |worktree| Command::TaskStart { agent: "codex".into(), prompt: "fix".into(), wait: None, worktree };
        assert_eq!(task("start codex --worktree fix"), Ok(in_worktree(Some(None))));
        assert_eq!(task("start codex --worktree=login fix"), Ok(in_worktree(Some(Some("login".into())))));
        assert!(task("send 7 --worktree fix").is_ok_and(|c| matches!(c, Command::TaskSend { prompt, .. } if prompt == "--worktree fix")));
        assert!(task("start codex").unwrap_err().contains("task"));
        assert!(task("start").unwrap_err().contains("agent"));
        assert!(task("start codex --timeout x y").unwrap_err().contains("seconds"));
        assert_eq!(task("send 7 fix it"), Ok(Command::TaskSend { id: 7, prompt: "fix it".into(), wait: None }));
        assert_eq!(task("send 7 --wait fix"), Ok(Command::TaskSend { id: 7, prompt: "fix".into(), wait: Some(DEFAULT_TASK_WAIT) }));
        assert!(task("send x fix").unwrap_err().contains("not a task ID"));
        assert_eq!(task("wait 7"), Ok(Command::TaskWait { id: 7, timeout: DEFAULT_TASK_WAIT }));
        assert_eq!(task("wait 7 --timeout 0"), Ok(Command::TaskWait { id: 7, timeout: 0 }));
        assert!(task("wait").unwrap_err().contains("ID"));
        assert_eq!(task("read 7"), Ok(Command::TaskRead { id: 7 }));
        assert_eq!(task("close 7"), Ok(Command::TaskClose { id: 7 }));
        assert_eq!(task("answer 7 yes"), Ok(Command::TaskAnswer { id: 7, text: "yes".into(), option: None }), "a question's text");
        assert_eq!(task("answer 7 --option 2"), Ok(Command::TaskAnswer { id: 7, text: String::new(), option: Some(2) }));
        assert_eq!(task("answer 7 use the cache"), Ok(Command::TaskAnswer { id: 7, text: "use the cache".into(), option: None }));
        assert!(task("answer 7").unwrap_err().contains("answer is required"));
        assert!(task("answer 7 --option x").unwrap_err().contains("option"));
        assert!(task("close").unwrap_err().contains("ID"));
        assert_eq!(task("ls"), Ok(Command::TaskList { all: false }));
        assert_eq!(task("ls --all"), Ok(Command::TaskList { all: true }));
        assert!(task("").unwrap_err().starts_with("usage"));
    }

    #[test]
    fn describes_a_task() {
        let task = |state: &str, extra: Value| {
            let mut a = json!({ "id": 7, "name": "Codex", "folder": "/w/app", "state": state });
            a.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            a
        };
        let done = describe_task(&task("done", json!({ "reply": "LGTM" })), None).unwrap();
        assert_eq!(done, "Codex (task 7, in /w/app) is done. Its answer:\n\nLGTM");
        let screen = describe_task(&task("done", json!({ "screen": "> ok" })), None).unwrap();
        assert!(screen.contains("no hooks") && screen.ends_with("Its screen:\n> ok"), "{screen}");
        let working = describe_task(&task("working", json!({})), Some(Duration::from_secs(100))).unwrap();
        assert!(working.contains("still works after 100s") && working.contains("tern --task wait 7"), "{working}");
        let blocked = describe_task(&task("blocked", json!({ "screen": "Allow? (y/n)" })), None).unwrap();
        assert!(blocked.contains("Only the user answers this") && blocked.ends_with("Allow? (y/n)"), "{blocked}");
        let asks = |kind: &str| {
            let a = task("blocked", json!({ "asks": { "kind": kind, "tool": "Bash", "detail": "curl x" } }));
            describe_task(&a, None).unwrap()
        };
        let approval = asks("approval");
        assert!(approval.contains("asks to run Bash: curl x.") && approval.contains("--option N"), "{approval}");
        assert!(!approval.contains("TEXT"), "an approval takes no text: {approval}");
        let question = asks("question");
        assert!(question.contains("it asks: curl x.") && question.contains("--option N"), "{question}");
        assert!(asks("startup").contains("Only the user answers this"));
        let failed = describe_task(&task("failed", json!({ "error": "The agent didn't start in time" })), None).unwrap_err();
        assert!(failed.ends_with("never reached it: The agent didn't start in time"), "{failed}");
        let line = task_line(&json!({ "id": 7, "name": "Codex", "state": "done", "title": "codex", "folder": "/w/app" }));
        assert_eq!(line, "7\tCodex\tdone\tcodex\t/w/app");
    }

    #[test]
    fn parses_the_worktree_commands() {
        let wt = |line: &str| parse("--worktree", &args(line));
        let create = |name: Option<&str>, branch: Option<&str>, base: Option<&str>| Command::WorktreeCreate {
            name: name.map(str::to_owned),
            branch: branch.map(str::to_owned),
            base: base.map(str::to_owned),
        };
        assert_eq!(wt("create"), Ok(create(None, None, None)));
        assert_eq!(wt("create login"), Ok(create(Some("login"), None, None)));
        assert_eq!(wt("create login --base main"), Ok(create(Some("login"), None, Some("main"))));
        assert_eq!(wt("create --branch feat/x"), Ok(create(None, Some("feat/x"), None)));
        assert!(wt("create --nope").unwrap_err().contains("no option --nope"));
        assert!(wt("create a b").unwrap_err().starts_with("usage"));
        assert_eq!(wt("ls"), Ok(Command::WorktreeList));
        assert_eq!(wt("prune"), Ok(Command::WorktreePrune));
        assert_eq!(wt("rm login"), Ok(Command::WorktreeRemove { name: "login".into(), force: false }));
        assert_eq!(wt("rm --force login"), Ok(Command::WorktreeRemove { name: "login".into(), force: true }));
        assert_eq!(wt("rm login --force"), Ok(Command::WorktreeRemove { name: "login".into(), force: true }));
        assert!(wt("rm").unwrap_err().contains("name is required"));
        assert!(wt("").unwrap_err().starts_with("usage"));
    }

    #[test]
    fn agents_listing_names_every_builtin_and_its_problems() {
        let registry = tern_agent::Registry::builtin();
        let text = agents_text(&registry, &["amp.json: a new agent needs a \"name\"".to_owned()]);
        for id in tern_agent::Registry::builtin_ids() {
            assert!(text.lines().any(|l| l.starts_with(id)), "{id}");
        }
        let claude = text.lines().find(|l| l.starts_with("claude")).unwrap();
        assert!(claude.contains("Claude Code") && claude.contains("chat"), "{claude}");
        assert_eq!(text.lines().filter(|l| l.starts_with("error: ")).count(), 1);
    }

    #[test]
    fn the_guide_names_the_command_it_is_given() {
        let guide = guide_for("'/A pp/tern'");
        for command in [
            "--service run NAME",
            "--service ls",
            "--service logs NAME",
            "--theme NAME",
            "--shot",
            "--task start AGENT",
            "--task wait ID",
            "--worktree create",
            "--worktree rm NAME",
        ] {
            assert!(guide.contains(&format!("'/A pp/tern' {command}")), "{command}");
        }
    }

    #[test]
    fn describes_the_window_status() {
        let a = json!({
            "theme": "Tern Dark",
            "themes": [{ "name": "Tern Dark", "dark": true }, { "name": "Tern Light", "dark": false }],
            "problems": null,
            "settings": "/c/settings.json",
            "themes_dir": "/c/themes",
        });
        assert_eq!(
            describe_status(&a),
            "Theme: Tern Dark\nThemes: Tern Dark (dark), Tern Light (light)\nNo problems.\nSettings: /c/settings.json\nThemes folder: /c/themes"
        );
        let a = json!({ "theme": "Mine", "themes": [], "problems": "themes/mine.json: expected `,`" });
        assert!(describe_status(&a).contains("Problems (the rest applies): themes/mine.json: expected `,`"));
    }

    #[test]
    fn describes_a_service() {
        let info = SessionInfo {
            id: 9,
            pid: 42,
            service: Some(ServiceSpec { name: "web".into(), command: "npm run dev".into(), cwd: "/w/app".into(), by: None, started: 0 }),
            exited: true,
            exit_code: Some(1),
            ..Default::default()
        };
        let text = describe(&info, &Default::default());
        assert_eq!(text, "web [9]: exited with code 1\n  command: npm run dev\n  folder: /w/app");
        let running = SessionInfo { exited: false, ..info };
        let ports = [(42, vec![5173])].into_iter().collect();
        assert!(describe(&running, &ports).ends_with("listening: http://localhost:5173"));
    }

    fn service(id: u64, name: &str, cwd: &str, exited: bool, started: u64) -> SessionInfo {
        SessionInfo {
            id,
            exited,
            service: Some(ServiceSpec { name: name.into(), command: "x".into(), cwd: cwd.into(), by: None, started }),
            ..Default::default()
        }
    }

    #[test]
    fn a_name_finds_only_this_folders_services() {
        let list = || {
            vec![
                service(1, "dev", "/w/other", false, 5),
                service(2, "web", "/w/app", true, 1),
                service(3, "web", "/w/app/site", false, 2),
                service(4, "web", "/w/app", true, 3),
            ]
        };
        let cwd = Path::new("/w/app");
        // A running one first, then the newest.
        assert_eq!(pick(list(), cwd, "web").unwrap().id, 3);
        // Another folder's `dev` is not this folder's: named by its id only.
        let err = pick(list(), cwd, "dev").unwrap_err();
        assert!(err.contains("no service dev in this folder") && err.contains("/w/other (id 1)"), "{err}");
        assert_eq!(pick(list(), cwd, "1").unwrap().id, 1);
        assert!(pick(list(), cwd, "nope").unwrap_err().starts_with("no service nope"));
    }

    #[test]
    fn folders_relate_either_way() {
        assert!(related(Path::new("/w/app/web"), Path::new("/w/app")));
        assert!(related(Path::new("/w"), Path::new("/w/app")));
        assert!(!related(Path::new("/w/other"), Path::new("/w/app")));
    }
}
