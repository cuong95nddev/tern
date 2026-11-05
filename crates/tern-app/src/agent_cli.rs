//! The commands a coding agent runs Tern with: `tern --service ...` runs its
//! dev servers and other long-running commands as Tern services
//! (`tern_mux::services`), so the user sees them in the Services panel and
//! they outlive the agent; `tern --theme` and `tern --shot` change and check
//! how Tern looks (asking the window, see `agent_ui`).
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
Read that file to look at it after changing how Tern looks."
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
    if !matches!(flag.as_str(), "--service" | "--theme" | "--shot" | "--skill") {
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
}

fn parse(flag: &str, args: &[String]) -> Result<Command, String> {
    match flag {
        "--skill" => Ok(Command::Guide),
        "--shot" => Ok(Command::Shot),
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

fn call(ctx: &Context, command: &Command) -> Result<String, String> {
    match command {
        Command::Guide => Ok(guide()),
        Command::Theme { name: None } => ask_ui(json!({ "op": "status" })).map(|a| describe_status(&a)),
        Command::Theme { name: Some(name) } => ask_ui(json!({ "op": "set_theme", "name": name })).map(|a| describe_status(&a)),
        Command::Shot => {
            let a = ask_ui(json!({ "op": "shot", "folder": ctx.cwd }))?;
            let path = PathBuf::from(a.get("path").and_then(Value::as_str).ok_or("no picture")?);
            let (w, h) = (a.get("width").and_then(Value::as_u64).unwrap_or(0), a.get("height").and_then(Value::as_u64).unwrap_or(0));
            Ok(format!("{} ({w}×{h} pixels)", path.display()))
        },
        _ => services_call(ctx, command),
    }
}

/// Ask the Tern window `request` (see `agent_ui`): its answer, or why not.
fn ask_ui(request: Value) -> Result<Value, String> {
    let mut s = Services::connect(&tern_mux::paths::mux_dir(), false).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => "Tern isn't running.".to_owned(),
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
    }

    #[test]
    fn the_guide_names_the_command_it_is_given() {
        let guide = guide_for("'/A pp/tern'");
        for command in ["--service run NAME", "--service ls", "--service logs NAME", "--theme NAME", "--shot"] {
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
