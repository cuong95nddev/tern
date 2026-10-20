//! What a terminal's program is doing, from its OSC 7501 status reports
//! (and, for Claude Code, its session file).
//! Shared by the UI (tab badges) and the daemon (notifications while no UI
//! is attached).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::osc::OscSignal;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActivityStatus {
    #[default]
    None,
    Working,
    Blocked,
    Done,
}

/// The status after `sig`, or `None` when `sig` doesn't change it.
pub fn next_status(prev: ActivityStatus, sig: &OscSignal) -> Option<ActivityStatus> {
    let OscSignal::ProgramStatus(st) = sig else { return None };
    let status = match st.state.as_str() {
        "working" => ActivityStatus::Working,
        "blocked" => ActivityStatus::Blocked,
        "clear" => ActivityStatus::None,
        _ => ActivityStatus::Done,
    };
    // A finished "working" turn with no explicit idle state reads as done.
    let status = if status == ActivityStatus::None && prev == ActivityStatus::Working { ActivityStatus::Done } else { status };
    (status != prev).then_some(status)
}

/// Where Claude Code keeps a file per running process, `<pid>.json`, with
/// its `status` (`busy`, `shell`, `waiting`, `idle`): its own folder's
/// `sessions`, and those of the Claude accounts Tern added (each runs
/// with a `CLAUDE_CONFIG_DIR` of its own).
pub fn claude_sessions_dirs() -> Vec<PathBuf> {
    // Asked per tab per tick: the accounts folder is listed again only
    // once the last listing is this old.
    const TTL: Duration = Duration::from_secs(2);
    static LISTED: Mutex<Option<(Instant, Vec<PathBuf>)>> = Mutex::new(None);
    let mut slot = LISTED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, dirs)) = &*slot
        && at.elapsed() < TTL
    {
        return dirs.clone();
    }
    let var = |k| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let home = var("HOME").or_else(|| if cfg!(windows) { var("USERPROFILE") } else { None });
    let own = var("CLAUDE_CONFIG_DIR").or_else(|| Some(home?.join(".claude")));
    let mut dirs: Vec<PathBuf> = own.map(|d| d.join("sessions")).into_iter().collect();
    if let Some(Ok(entries)) = ACCOUNTS_DIR.get().map(std::fs::read_dir) {
        dirs.extend(entries.flatten().map(|e| e.path().join("sessions")));
    }
    *slot = Some((Instant::now(), dirs.clone()));
    dirs
}

/// The folder the Claude accounts Tern added live in, one folder each
/// (see [`claude_sessions_dirs`]); set once at start.
static ACCOUNTS_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Sets where the Claude accounts Tern added live; set once, before any
/// [`claude_sessions_dirs`].
pub fn set_claude_accounts_dir(dir: PathBuf) {
    let _ = ACCOUNTS_DIR.set(dir);
}

/// The folder (of [`claude_sessions_dirs`]) with Claude Code process
/// `pid`'s file.
pub fn claude_sessions_dir_of(pid: u32) -> Option<PathBuf> {
    claude_sessions_dirs().into_iter().find(|d| d.join(format!("{pid}.json")).is_file())
}

/// What Claude Code process `pid` is doing, from its file in `dir` (see
/// [`claude_sessions_dir_of`]): richer than its OSC 7501 reports, which say
/// nothing at all while it shows a session parked on a background job.
/// `shell` is idle with a background shell still running (a dev server, a
/// watcher): nothing waits on it, so it is not working. `None` when there is
/// no file or no status it knows.
pub fn claude_session_status(dir: &Path, pid: u32) -> Option<ActivityStatus> {
    let mut file = read_json(&dir.join(format!("{pid}.json")))?;
    // Parked: the conversation runs in a background job's process.
    if let Some(job) = file.get("parkedJobId").and_then(|j| j.as_str()).map(str::to_owned) {
        file = job_file(dir, &job)?;
    }
    Some(match file.get("status")?.as_str()? {
        "busy" => ActivityStatus::Working,
        "waiting" => ActivityStatus::Blocked,
        "idle" | "shell" => ActivityStatus::None,
        _ => return None,
    })
}

/// The background job Claude Code process `pid` shows, from its file in
/// `dir`: one it is parked on.
pub fn claude_parked_job(dir: &Path, pid: u32) -> Option<String> {
    Some(read_json(&dir.join(format!("{pid}.json")))?.get("parkedJobId")?.as_str()?.to_owned())
}

/// The process Claude Code background job `job` runs in, from its file in
/// `dir`.
pub fn claude_job_pid(dir: &Path, job: &str) -> Option<u32> {
    u32::try_from(job_file(dir, job)?.get("pid")?.as_u64()?).ok()
}

/// The session file in `dir` of the process that runs job `job`.
fn job_file(dir: &Path, job: &str) -> Option<serde_json::Value> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| read_json(&e.path()))
        .find(|f| f.get("jobId").and_then(|j| j.as_str()) == Some(job))
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osc::ProgramStatus;

    fn st(state: &str) -> OscSignal {
        OscSignal::ProgramStatus(ProgramStatus { state: state.into(), ..Default::default() })
    }

    #[test]
    fn maps_states() {
        use ActivityStatus::*;
        assert_eq!(next_status(None, &st("working")), Some(Working));
        assert_eq!(next_status(Working, &st("blocked")), Some(Blocked));
        assert_eq!(next_status(Working, &st("idle")), Some(Done));
        assert_eq!(next_status(Working, &st("clear")), Some(Done));
        assert_eq!(next_status(Blocked, &st("clear")), Some(None));
        assert_eq!(next_status(Working, &st("working")), Option::None);
    }

    #[test]
    fn other_signals_change_nothing() {
        let notify = OscSignal::Notify { title: None, body: "x".into() };
        assert_eq!(next_status(ActivityStatus::Working, &notify), None);
        assert_eq!(next_status(ActivityStatus::Working, &OscSignal::ProgramStatusProbe), None);
    }

    #[test]
    fn claude_from_its_session_file() {
        use ActivityStatus::*;
        let dir = std::env::temp_dir().join(format!("tern-claude-sessions-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |pid: u32, json: &str| std::fs::write(dir.join(format!("{pid}.json")), json).unwrap();
        write(1, r#"{"pid":1,"status":"busy"}"#);
        write(2, r#"{"pid":2,"status":"shell"}"#);
        write(3, r#"{"pid":3,"status":"waiting"}"#);
        write(4, r#"{"pid":4,"status":"idle"}"#);
        write(5, r#"{"pid":5,"status":"later"}"#);
        // Parked on a background job: the job's status counts, not its own.
        write(6, r#"{"pid":6,"status":"idle","parkedJobId":"j1"}"#);
        write(7, r#"{"pid":7,"kind":"bg","jobId":"j1","status":"busy"}"#);
        write(8, r#"{"pid":8,"status":"idle","parkedJobId":"gone"}"#);
        assert_eq!(claude_session_status(&dir, 1), Some(Working));
        assert_eq!(claude_session_status(&dir, 2), Some(None));
        assert_eq!(claude_session_status(&dir, 3), Some(Blocked));
        assert_eq!(claude_session_status(&dir, 4), Some(None));
        assert_eq!(claude_session_status(&dir, 5), Option::None);
        assert_eq!(claude_session_status(&dir, 6), Some(Working));
        assert_eq!(claude_session_status(&dir, 8), Option::None);
        assert_eq!(claude_session_status(&dir, 9), Option::None);
        assert_eq!(claude_parked_job(&dir, 6).as_deref(), Some("j1"));
        assert_eq!(claude_parked_job(&dir, 1), Option::None);
        assert_eq!(claude_job_pid(&dir, "j1"), Some(7));
        assert_eq!(claude_job_pid(&dir, "gone"), Option::None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
