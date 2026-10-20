// The scripts below need a POSIX shell.
#![cfg(unix)]

use std::sync::{Arc, mpsc};
use std::time::Duration;

use tern_term::osc::OscSignal;
use tern_term::palette::Palette;
use tern_term::vt::grid::Dimensions;
use tern_term::vt::index::{Column, Line};
use tern_term::{Session, SpawnConfig, TermEvent, TermSize};

fn spawn(program: &str, args: &[&str]) -> (Session, mpsc::Receiver<TermEvent>) {
    let (tx, rx) = mpsc::channel();
    let cfg = SpawnConfig {
        program: Some(program.into()),
        args: args.iter().map(|s| s.to_string()).collect(),
        cwd: None,
        env: Default::default(),
        size: TermSize { cols: 80, rows: 24, cell_width: 8, cell_height: 16 },
        scrollback: 1000,
    };
    let s = Session::spawn(
        cfg,
        Palette::default(),
        Arc::new(move |e| {
            let _ = tx.send(e);
        }),
    )
    .unwrap();
    (s, rx)
}

fn wait_exit(rx: &mpsc::Receiver<TermEvent>, mut on: impl FnMut(&TermEvent)) -> Option<i32> {
    loop {
        let e = rx.recv_timeout(Duration::from_secs(5)).expect("timed out");
        on(&e);
        if let TermEvent::Exit(code) = e {
            return code;
        }
    }
}

fn row_text(s: &Session, line: i32) -> String {
    let term = s.term().lock();
    let grid = term.grid();
    (0..grid.columns()).map(|c| grid[Line(line)][Column(c)].c).collect::<String>().trim_end().to_owned()
}

#[test]
fn runs_child_and_renders_output() {
    let (s, rx) = spawn("/bin/sh", &["-c", "printf 'hello tern'; exit 3"]);
    assert_eq!(wait_exit(&rx, |_| {}), Some(3));
    assert_eq!(row_text(&s, 0), "hello tern");
}

#[test]
fn taps_program_status_and_answers_probe() {
    // The child sends the probe, then reads our reply from stdin and echoes its length.
    let script = r#"stty raw -echo; printf '\033]7501;?\033\\'; r=$(dd bs=1 count=10 2>/dev/null | od -An -c | tr -d ' \n'); printf '\033]7501;state=working:app=claude-code\007'; printf "R:%s" "$r""#;
    let (s, rx) = spawn("/bin/sh", &["-c", script]);
    let mut signals = vec![];
    wait_exit(&rx, |e| {
        if let TermEvent::Signal(sig) = e {
            signals.push(sig.clone());
        }
    });
    assert_eq!(signals[0], OscSignal::ProgramStatusProbe);
    assert!(matches!(&signals[1], OscSignal::ProgramStatus(st) if st.state == "working"));
    assert!(row_text(&s, 0).starts_with("R:033]7501;?033"), "{}", row_text(&s, 0));
}

#[test]
fn foreground_programs_name_the_running_command_and_its_script() {
    let dir = std::env::temp_dir().join(format!("tern-procs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("codex.js");
    std::fs::write(&script, "sleep 3\n").unwrap();
    let (s, _rx) = spawn("/bin/sh", &[script.to_str().unwrap()]);
    let mut programs = Vec::new();
    for _ in 0..50 {
        programs = s.foreground_programs();
        if programs.iter().any(|p| p == "sleep") {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = std::fs::remove_dir_all(&dir);
    for want in ["sh", "codex", "sleep"] {
        assert!(programs.iter().any(|p| p == want), "{want} missing from {programs:?}");
    }
}

#[test]
fn agent_shells_are_the_commands_run_in_groups_of_their_own() {
    // With job control on, the background job gets a group of its own, as
    // Claude Code's Bash tool gives each command; `wait` keeps the parent in
    // the foreground. `; true` keeps the inner shell from exec-ing `sleep`.
    let out = std::env::temp_dir().join(format!("tern-shell-out-{}", std::process::id()));
    let script = format!("set -m; /bin/sh -c 'sleep 3; true' > '{}' 2>&1 & wait", out.display());
    let (s, _rx) = spawn("/bin/sh", &["-c", &script]);
    let mut shells = Vec::new();
    for _ in 0..50 {
        shells = s.agent_shells();
        if !shells.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(shells.len(), 1, "{shells:?}");
    assert_eq!(shells[0].command, "sleep 3; true");
    let age = shells[0].started.elapsed().unwrap_or_default();
    assert!(age < Duration::from_secs(5), "started {age:?} ago");
    // Its output's file, by its real path (`/tmp` is a link on macOS).
    let want = std::fs::canonicalize(&out).unwrap();
    assert_eq!(shells[0].output.as_ref().map(|p| std::fs::canonicalize(p).unwrap()), Some(want));
    let _ = std::fs::remove_file(&out);
}
