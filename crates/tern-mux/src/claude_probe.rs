//! Which Claude Code a Claude tab will start: asked of `claude --version`,
//! run the way the tab runs `claude` (its shell, its env), so a version
//! manager's shim picks the same one. Its settings name only the hook
//! events that version knows (see [`tern_bridge::claude_version`]).

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tern_bridge::claude_version::{self, Version};
use tern_term::SpawnConfig;

/// A version this old is asked again (on the side: the old one serves
/// meanwhile).
const FRESH: Duration = Duration::from_secs(5 * 60);
/// A probe that takes longer is given up on.
const TIMEOUT: Duration = Duration::from_secs(5);

struct Known {
    at: Instant,
    version: Option<Version>,
    asking: bool,
}

/// Per shell program: what its `claude --version` said.
static KNOWN: Mutex<Option<HashMap<String, Known>>> = Mutex::new(None);

/// The version of the Claude that `cfg` (a Claude tab's, its command the
/// argument with `placeholder` in it) starts; `None` when it can't be told.
/// The first ask waits for the answer; later ones get the last answer and
/// ask again on the side once it is old.
pub fn version(cfg: &SpawnConfig, placeholder: &str) -> Option<Version> {
    let probe = probe_command(cfg, placeholder)?;
    let key = probe.0.clone();
    {
        let mut known = KNOWN.lock().unwrap();
        let known = known.get_or_insert_with(HashMap::new);
        if let Some(k) = known.get_mut(&key) {
            if k.at.elapsed() >= FRESH && !k.asking {
                k.asking = true;
                let key = key.clone();
                std::thread::spawn(move || {
                    let version = run(&probe);
                    let mut known = KNOWN.lock().unwrap();
                    known.get_or_insert_with(HashMap::new).insert(key, Known { at: Instant::now(), version, asking: false });
                });
            }
            return k.version;
        }
    }
    let version = run(&probe);
    KNOWN.lock().unwrap().get_or_insert_with(HashMap::new).insert(key, Known { at: Instant::now(), version, asking: false });
    version
}

/// What to run: the tab's shell, its arguments with the `claude …` command
/// swapped for `claude --version`, its env and folder.
type Probe = (String, Vec<String>, HashMap<String, String>, Option<std::path::PathBuf>);

fn probe_command(cfg: &SpawnConfig, placeholder: &str) -> Option<Probe> {
    let program = cfg.program.clone().or_else(|| std::env::var("SHELL").ok().filter(|s| !s.is_empty()))?;
    let at = cfg.args.iter().position(|a| a.contains(placeholder))?;
    let mut args = cfg.args.clone();
    args[at] = if cfg!(windows) { "& claude --version; exit $LASTEXITCODE".into() } else { "exec claude --version".into() };
    Some((program, args, cfg.env.clone(), cfg.cwd.clone()))
}

fn run((program, args, env, cwd): &Probe) -> Option<Version> {
    let mut cmd = Command::new(program);
    cmd.args(args).envs(env).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    if let Some(dir) = cwd.as_ref().filter(|d| d.is_dir()) {
        cmd.current_dir(dir);
    }
    let mut child = cmd.spawn().ok()?;
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                log::warn!("claude --version gave no answer");
                return None;
            },
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    let version = claude_version::parse(&out);
    log::info!("Claude Code {version:?}");
    version
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn swaps_the_command_for_the_version() {
        let cfg = SpawnConfig {
            program: Some("/bin/sh".into()),
            args: vec!["-lic".into(), "exec claude --resume x --settings {s}".into()],
            cwd: None,
            env: HashMap::new(),
            size: tern_term::TermSize { cols: 80, rows: 24, cell_width: 8, cell_height: 16 },
            scrollback: 0,
        };
        let (program, args, ..) = probe_command(&cfg, "{s}").unwrap();
        assert_eq!(program, "/bin/sh");
        assert_eq!(args, ["-lic", "exec claude --version"]);
        assert!(probe_command(&SpawnConfig { args: vec!["-l".into()], ..cfg }, "{s}").is_none());
    }

    #[test]
    fn reads_what_it_answers() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("claude");
        std::fs::write(&claude, "#!/bin/sh\necho '2.1.312 (Claude Code)'\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!("{}:/usr/bin:/bin", dir.path().display());
        let probe = (
            "/bin/sh".to_owned(),
            vec!["-c".to_owned(), "exec claude --version".to_owned()],
            HashMap::from([("PATH".to_owned(), path)]),
            None,
        );
        assert_eq!(run(&probe), Some((2, 1, 312)));
    }
}
