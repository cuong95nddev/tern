//! What runs in a PTY's foreground: the processes of its foreground process
//! group (on Windows, which has none: the shell and the processes below it),
//! named so the UI can tell which program a shell is running.

#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;

/// How long a scan of every process serves later asks: each tab (and the
/// daemon, for each session) asks about its own, all within one tick.
#[cfg(any(windows, target_os = "linux"))]
const SCAN_TTL: std::time::Duration = std::time::Duration::from_millis(250);

/// `scan()`'s result, shared while it is younger than [`SCAN_TTL`].
#[cfg(any(windows, target_os = "linux"))]
struct ScanCache<T>(std::sync::Mutex<Option<(std::time::Instant, std::sync::Arc<T>)>>);

#[cfg(any(windows, target_os = "linux"))]
impl<T> ScanCache<T> {
    const fn new() -> Self {
        Self(std::sync::Mutex::new(None))
    }

    /// A young enough scan that `has` finds what is asked about, else a
    /// new one: a process started since the last scan isn't in it.
    fn get(&self, has: impl Fn(&T) -> bool, scan: impl FnOnce() -> T) -> std::sync::Arc<T> {
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, v)) = &*slot
            && at.elapsed() < SCAN_TTL
            && has(v)
        {
            return v.clone();
        }
        let v = std::sync::Arc::new(scan());
        *slot = Some((std::time::Instant::now(), v.clone()));
        v
    }
}

/// Adds the lowercase file stem of `arg` to `names` once.
fn push_stem(names: &mut Vec<String>, arg: &str) {
    if let Some(stem) = Path::new(arg).file_stem() {
        let stem = stem.to_string_lossy().to_ascii_lowercase();
        if !names.contains(&stem) {
            names.push(stem);
        }
    }
}

/// Names of the programs in `tty`'s foreground process group: each one's
/// executable and, for interpreters (`node cli.js`), its script. Lowercase
/// file stems, e.g. `["node", "codex"]`. Empty when it can't be read.
#[cfg(unix)]
pub fn foreground_programs(tty: &File) -> Vec<String> {
    let pgid = unsafe { libc::tcgetpgrp(tty.as_raw_fd()) };
    if pgid <= 0 {
        return Vec::new();
    }
    let mut names = Vec::new();
    for argv in group_argvs(pgid) {
        // The executable, then the first non-flag argument (a script).
        for arg in argv.iter().take(1).chain(argv.iter().skip(1).find(|a| !a.starts_with('-'))) {
            push_stem(&mut names, arg);
        }
    }
    names
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn group_argvs(pgid: libc::pid_t) -> Vec<Vec<String>> {
    group_pids(pgid).into_iter().filter_map(|p| argv(p, 2)).collect()
}

/// The processes of group `pgid`, its leader among them.
#[cfg(target_os = "macos")]
fn group_pids(pgid: libc::pid_t) -> Vec<libc::pid_t> {
    let mut pids = vec![0 as libc::pid_t; 256];
    let size = (pids.len() * size_of::<libc::pid_t>()) as libc::c_int;
    let n = unsafe { libc::proc_listpgrppids(pgid, pids.as_mut_ptr().cast(), size) };
    let n = if n > 0 { (n as usize).min(pids.len()) } else { 0 };
    pids.truncate(n);
    if !pids.contains(&pgid) {
        pids.push(pgid);
    }
    pids.retain(|&p| p > 0);
    pids
}

/// The first `max` arguments of `pid`'s argv, from `KERN_PROCARGS2`: argc,
/// the exec path, NUL padding, then the arguments.
#[cfg(target_os = "macos")]
fn argv(pid: libc::pid_t, max: usize) -> Option<Vec<String>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut len: libc::size_t = 0;
    let rc = unsafe { libc::sysctl(mib.as_mut_ptr(), 3, std::ptr::null_mut(), &mut len, std::ptr::null_mut(), 0) };
    if rc != 0 || len < 4 {
        return None;
    }
    let mut buf = vec![0u8; len];
    let rc = unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) };
    if rc != 0 {
        return None;
    }
    buf.truncate(len);
    let argc = i32::from_ne_bytes(buf[..4].try_into().ok()?).max(0) as usize;
    let rest = &buf[4..];
    let exec_end = rest.iter().position(|&b| b == 0)?;
    let args_start = exec_end + rest[exec_end..].iter().position(|&b| b != 0)?;
    let args = rest[args_start..].split(|&b| b == 0).take(argc.min(max)).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
    Some(args)
}

/// The processes of group `pgid`.
#[cfg(target_os = "linux")]
fn group_pids(pgid: libc::pid_t) -> Vec<libc::pid_t> {
    static GROUPS: ScanCache<Vec<(u32, libc::pid_t)>> = ScanCache::new();
    let has_group = |groups: &Vec<(u32, libc::pid_t)>| groups.iter().any(|&(_, g)| g == pgid);
    let groups = GROUPS.get(has_group, || {
        let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
        dir.flatten()
            .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
            .filter_map(|pid| {
                // `pid (comm) state ppid pgrp ...`; comm may hold spaces.
                let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
                let (_, after) = stat.rsplit_once(')')?;
                Some((pid, after.split_whitespace().nth(2)?.parse::<libc::pid_t>().ok()?))
            })
            .collect()
    });
    groups.iter().filter(|(_, g)| *g == pgid).map(|&(pid, _)| pid as libc::pid_t).collect()
}

/// The first `max` arguments of `pid`'s argv.
#[cfg(target_os = "linux")]
fn argv(pid: libc::pid_t, max: usize) -> Option<Vec<String>> {
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let args: Vec<String> =
        cmdline.split(|&b| b == 0).filter(|a| !a.is_empty()).take(max).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
    (!args.is_empty()).then_some(args)
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn group_argvs(_pgid: i32) -> Vec<Vec<String>> {
    Vec::new()
}

/// `tty`'s foreground process group leader: the program a shell runs
/// (it leads its own group), else the shell.
#[cfg(unix)]
pub fn foreground_pid(tty: &File) -> Option<u32> {
    let pgid = unsafe { libc::tcgetpgrp(tty.as_raw_fd()) };
    u32::try_from(pgid).ok().filter(|&p| p > 0)
}

/// The working directory of `tty`'s foreground process group leader (the
/// shell, or the program it runs), to resolve the relative paths it prints.
#[cfg(unix)]
pub fn foreground_cwd(tty: &File) -> Option<PathBuf> {
    let pgid = unsafe { libc::tcgetpgrp(tty.as_raw_fd()) };
    if pgid <= 0 {
        return None;
    }
    cwd(pgid)
}

#[cfg(target_os = "macos")]
fn cwd(pid: libc::pid_t) -> Option<PathBuf> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;

    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    let n = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO, 0, (&raw mut info).cast(), size) };
    if n != size {
        return None;
    }
    let raw = &info.pvi_cdir.vip_path;
    let bytes = unsafe { std::slice::from_raw_parts(raw.as_ptr().cast::<u8>(), size_of_val(raw)) };
    let path = CStr::from_bytes_until_nul(bytes).ok()?;
    (!path.is_empty()).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(path.to_bytes())))
}

#[cfg(target_os = "linux")]
fn cwd(pid: libc::pid_t) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn cwd(_pid: i32) -> Option<PathBuf> {
    None
}

/// A shell a foreground program started to run a command: a `<shell> -c
/// <script>` child of the foreground group in a process group of its own,
/// as Claude Code's Bash tool runs each command (in the foreground or not).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentShell {
    pub pid: u32,
    /// What it runs: the script, or the command Claude Code's wrapper
    /// script `eval`s.
    pub command: String,
    pub started: std::time::SystemTime,
    /// The file its output goes to, when it goes to one (Claude Code's go
    /// to a task file of the session, foreground commands' too).
    pub output: Option<std::path::PathBuf>,
}

/// The shells the programs in `tty`'s foreground group run commands in,
/// oldest first. Empty when they can't be read.
#[cfg(unix)]
pub fn agent_shells(tty: &File) -> Vec<AgentShell> {
    let pgid = unsafe { libc::tcgetpgrp(tty.as_raw_fd()) };
    if pgid <= 0 {
        return Vec::new();
    }
    let mut parents = group_pids(pgid);
    parents.extend(claude_job_pids(&parents));
    let mut shells: Vec<AgentShell> = group_children(&parents)
        .into_iter()
        .filter_map(|(pid, started, argv)| {
            let command = shell_command(&argv)?;
            let output = stdout_path(pid as libc::pid_t).filter(|p| p.is_file());
            Some(AgentShell { pid, command, started, output })
        })
        .collect();
    shells.sort_by_key(|s| (s.started, s.pid));
    shells
}

/// The processes Claude Code runs the background jobs in that `pids` show:
/// `claude attach <job>` shows one the Claude daemon runs, in a process (and
/// terminal) of its own, as does a session parked on a job.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn claude_job_pids(pids: &[libc::pid_t]) -> Vec<libc::pid_t> {
    let Some(dir) = crate::status::claude_sessions_dir() else { return Vec::new() };
    pids.iter()
        .filter_map(|&pid| {
            let job = argv(pid, 3)
                .and_then(|a| attached_job(&a))
                .or_else(|| crate::status::claude_parked_job(&dir, pid as u32))?;
            libc::pid_t::try_from(crate::status::claude_job_pid(&dir, &job)?).ok()
        })
        .collect()
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn claude_job_pids(_pids: &[i32]) -> Vec<i32> {
    Vec::new()
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn group_pids(pgid: i32) -> Vec<i32> {
    vec![pgid]
}

/// The job `argv` attaches to when it is `claude attach <job>`.
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
fn attached_job(argv: &[String]) -> Option<String> {
    let [exe, cmd, job, ..] = argv else { return None };
    let claude = Path::new(exe).file_name().is_some_and(|n| n == "claude");
    (claude && cmd == "attach" && !job.starts_with('-')).then(|| job.clone())
}

/// The children of `parents` that lead groups of their own (not their
/// parent's): each one's pid, start time and first three arguments.
#[cfg(target_os = "macos")]
fn group_children(parents: &[libc::pid_t]) -> Vec<(u32, std::time::SystemTime, Vec<String>)> {
    let bsd_info = |pid: libc::pid_t| {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let got = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
        (got == size).then_some(info)
    };
    let mut out = Vec::new();
    for &parent in parents {
        let Some(pgid) = bsd_info(parent).map(|i| i.pbi_pgid as libc::pid_t) else { continue };
        let mut pids = vec![0 as libc::pid_t; 64];
        let size = (pids.len() * size_of::<libc::pid_t>()) as libc::c_int;
        let n = unsafe { libc::proc_listchildpids(parent, pids.as_mut_ptr().cast(), size) };
        let n = if n > 0 { (n as usize).min(pids.len()) } else { 0 };
        for &pid in pids[..n].iter().filter(|&&p| p > 0) {
            let Some(info) = bsd_info(pid).filter(|i| i.pbi_pgid as libc::pid_t != pgid) else { continue };
            let started = std::time::UNIX_EPOCH
                + std::time::Duration::from_secs(info.pbi_start_tvsec)
                + std::time::Duration::from_micros(info.pbi_start_tvusec);
            if let Some(argv) = argv(pid, 3) {
                out.push((pid as u32, started, argv));
            }
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn group_children(parents: &[libc::pid_t]) -> Vec<(u32, std::time::SystemTime, Vec<String>)> {
    use std::time::{Duration, UNIX_EPOCH};
    // `pid (comm) state ppid pgrp ... starttime` (field 22, in clock ticks
    // since boot); comm may hold spaces.
    let stat = |pid: u32| -> Option<(libc::pid_t, libc::pid_t, u64)> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let (_, after) = stat.rsplit_once(')')?;
        let f: Vec<&str> = after.split_whitespace().collect();
        Some((f.get(1)?.parse().ok()?, f.get(2)?.parse().ok()?, f.get(19)?.parse().ok()?))
    };
    let boot = std::fs::read_to_string("/proc/stat")
        .ok()
        .and_then(|s| s.lines().find_map(|l| l.strip_prefix("btime ")?.trim().parse::<u64>().ok()))
        .unwrap_or(0);
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
    let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
    let procs: Vec<(u32, libc::pid_t, libc::pid_t, u64)> = dir
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(|pid| stat(pid).map(|(ppid, pgrp, start)| (pid, ppid, pgrp, start)))
        .collect();
    // Each parent's group, to tell the children that lead their own.
    let groups: Vec<(libc::pid_t, libc::pid_t)> =
        procs.iter().filter(|p| parents.contains(&(p.0 as libc::pid_t))).map(|p| (p.0 as libc::pid_t, p.2)).collect();
    procs
        .iter()
        .filter(|(_, ppid, pgrp, _)| groups.iter().any(|(parent, g)| parent == ppid && g != pgrp))
        .filter_map(|&(pid, _, _, start)| {
            let started = UNIX_EPOCH + Duration::from_secs(boot) + Duration::from_millis(start * 1000 / ticks);
            Some((pid, started, argv(pid as libc::pid_t, 3)?))
        })
        .collect()
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn group_children(_parents: &[i32]) -> Vec<(u32, std::time::SystemTime, Vec<String>)> {
    Vec::new()
}

/// What `pid`'s standard output (fd 1) is open on, as a path.
#[cfg(target_os = "macos")]
fn stdout_path(pid: libc::pid_t) -> Option<PathBuf> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;

    // `struct vnode_fdinfowithpath` of <sys/proc_info.h>, which libc lacks.
    #[repr(C)]
    struct ProcFileInfo {
        openflags: u32,
        status: u32,
        offset: libc::off_t,
        kind: i32,
        guardflags: u32,
    }
    #[repr(C)]
    struct VnodeFdInfoWithPath {
        pfi: ProcFileInfo,
        pvip: libc::vnode_info_path,
    }
    const PROC_PIDFDVNODEPATHINFO: libc::c_int = 2;

    let mut info: VnodeFdInfoWithPath = unsafe { std::mem::zeroed() };
    let size = size_of::<VnodeFdInfoWithPath>() as libc::c_int;
    let n = unsafe { libc::proc_pidfdinfo(pid, 1, PROC_PIDFDVNODEPATHINFO, (&raw mut info).cast(), size) };
    if n != size {
        return None;
    }
    let raw = &info.pvip.vip_path;
    let bytes = unsafe { std::slice::from_raw_parts(raw.as_ptr().cast::<u8>(), size_of_val(raw)) };
    let path = CStr::from_bytes_until_nul(bytes).ok()?;
    (!path.is_empty()).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(path.to_bytes())))
}

#[cfg(target_os = "linux")]
fn stdout_path(pid: libc::pid_t) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/fd/1")).ok()
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn stdout_path(_pid: i32) -> Option<PathBuf> {
    None
}

/// What `argv` runs when it is `<shell> -c <script>` (or `-lc`, ...): the
/// script, or the command in it Claude Code's wrapper `eval`s.
fn shell_command(argv: &[String]) -> Option<String> {
    let [exe, flag, script, ..] = argv else { return None };
    let name = Path::new(exe).file_name()?.to_str()?.trim_start_matches('-');
    let shell = matches!(name, "sh" | "bash" | "zsh" | "dash" | "ksh" | "fish");
    let runs = flag.starts_with('-') && !flag.starts_with("--") && flag.ends_with('c');
    if !shell || !runs {
        return None;
    }
    let command = wrapped_command(script).unwrap_or_else(|| script.trim().to_owned());
    (!command.is_empty()).then_some(command)
}

/// The command Claude Code's wrapper script (`source <snapshot> && ... &&
/// eval '<command>' < /dev/null && pwd -P >| <file>`) runs, unquoted.
fn wrapped_command(script: &str) -> Option<String> {
    if !script.contains("shell-snapshots/") {
        return None;
    }
    let mut rest = &script[script.find(" eval '")? + " eval '".len()..];
    let mut out = String::new();
    // Single quoted, a quote in it written `'\''`.
    loop {
        let end = rest.find('\'')?;
        out.push_str(&rest[..end]);
        rest = &rest[end..];
        match rest.strip_prefix("'\\''") {
            Some(after) => {
                out.push('\'');
                rest = after;
            },
            None => return Some(out.trim().to_owned()),
        }
    }
}

/// Names of the programs running in the console of `shell` (the PTY's
/// child): the shell's executable and those of the processes below it,
/// shallowest first, plus the script of an interpreter (`node codex.js`).
/// Lowercase file stems, as on Unix.
#[cfg(windows)]
pub fn foreground_programs(shell: u32) -> Vec<String> {
    let mut names = Vec::new();
    for (pid, exe) in win::descendants(shell) {
        push_stem(&mut names, &exe);
        if is_interpreter(&exe)
            && let Some(script) = win::command_line(pid).and_then(|argv| script_arg(&argv))
        {
            push_stem(&mut names, &script);
        }
    }
    names.retain(|n| n != "conhost" && n != "openconsole");
    names
}

/// Reading another process's working directory on Windows means reading
/// its memory; links then resolve against the tab's own folder.
#[cfg(windows)]
pub fn foreground_cwd(_shell: u32) -> Option<std::path::PathBuf> {
    None
}

/// A program and the processes below it, held open (so their ids can't go
/// to new processes) until [`ProcessTree::end`] ends them.
#[cfg(windows)]
pub struct ProcessTree(Vec<win::Process>);

#[cfg(windows)]
impl ProcessTree {
    /// `root` and what runs below it now; processes already gone are left out.
    pub fn open(root: u32) -> Self {
        // 0 is the idle process: a program whose id isn't known.
        let pids = if root == 0 { Vec::new() } else { win::Tree::scan().walk(root) };
        Self(pids.into_iter().filter_map(|(pid, _)| win::Process::open(pid)).collect())
    }

    /// Give the root up to `grace` to end on its own, then terminate it and
    /// whatever below it is still running.
    pub fn end(self, grace: std::time::Duration) {
        if let Some(root) = self.0.first() {
            root.wait(grace);
        }
        for p in &self.0 {
            p.terminate();
        }
    }
}

/// Programs whose first non-flag argument names the real program.
#[cfg_attr(not(windows), allow(dead_code))]
fn is_interpreter(exe: &str) -> bool {
    let name = exe.rsplit(['/', '\\']).next().unwrap_or(exe).to_ascii_lowercase();
    let stem = name.strip_suffix(".exe").unwrap_or(&name);
    matches!(stem, "node" | "bun" | "deno" | "python" | "python3" | "pythonw" | "py" | "ruby")
}

/// The script an interpreter's `argv` runs: its first argument that isn't a flag.
#[cfg_attr(not(windows), allow(dead_code))]
fn script_arg(argv: &[String]) -> Option<String> {
    argv.iter().skip(1).find(|a| !a.starts_with('-')).cloned()
}

#[cfg(windows)]
mod win {
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::time::Duration;

    use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessCommandLineInformation};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, LocalFree, UNICODE_STRING};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
    };
    use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

    /// `root` and every process below it with its executable name, breadth first.
    pub fn descendants(root: u32) -> Vec<(u32, String)> {
        static TREE: super::ScanCache<Tree> = super::ScanCache::new();
        let has_root = |t: &Tree| t.exes.contains_key(&root);
        TREE.get(has_root, Tree::scan).walk(root)
    }

    /// Every process's children and executable name.
    #[derive(Default)]
    pub struct Tree {
        children: HashMap<u32, Vec<u32>>,
        exes: HashMap<u32, String>,
    }

    impl Tree {
        /// A fresh scan of every process.
        pub fn scan() -> Self {
            let mut t = Tree::default();
            for (pid, ppid, exe) in snapshot() {
                // The idle process is its own parent.
                if pid != ppid {
                    t.children.entry(ppid).or_default().push(pid);
                }
                t.exes.insert(pid, exe);
            }
            t
        }

        /// `root` and every process below it with its executable name, breadth first.
        pub fn walk(&self, root: u32) -> Vec<(u32, String)> {
            let mut out = Vec::new();
            let mut seen = std::collections::HashSet::new();
            let mut queue = std::collections::VecDeque::from([root]);
            while let Some(pid) = queue.pop_front() {
                let Some(exe) = self.exes.get(&pid).filter(|_| seen.insert(pid)) else { continue };
                out.push((pid, exe.clone()));
                queue.extend(self.children.get(&pid).into_iter().flatten());
            }
            out
        }
    }

    /// A process opened to wait for and end.
    pub struct Process(HANDLE);

    impl Process {
        pub fn open(pid: u32) -> Option<Self> {
            let h = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, 0, pid) };
            (!h.is_null()).then_some(Self(h))
        }

        /// Wait up to `timeout` for it to end.
        pub fn wait(&self, timeout: Duration) {
            let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
            unsafe { WaitForSingleObject(self.0, ms) };
        }

        /// End it, if it still runs.
        pub fn terminate(&self) {
            unsafe { TerminateProcess(self.0, 1) };
        }
    }

    impl Drop for Process {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    /// Every process: `(pid, parent pid, executable file name)`.
    fn snapshot() -> Vec<(u32, u32, String)> {
        let mut out = Vec::new();
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE {
                return out;
            }
            let mut e: PROCESSENTRY32W = std::mem::zeroed();
            e.dwSize = size_of::<PROCESSENTRY32W>() as u32;
            let mut ok = Process32FirstW(snap, &mut e);
            while ok != 0 {
                let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                out.push((e.th32ProcessID, e.th32ParentProcessID, String::from_utf16_lossy(&e.szExeFile[..len])));
                ok = Process32NextW(snap, &mut e);
            }
            CloseHandle(snap);
        }
        out
    }

    /// `pid`'s command line, split as the C runtime does.
    pub fn command_line(pid: u32) -> Option<Vec<String>> {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return None;
            }
            // A UNICODE_STRING followed by its text; ask for the size first.
            let mut len = 0u32;
            NtQueryInformationProcess(h, ProcessCommandLineInformation, std::ptr::null_mut(), 0, &mut len);
            let mut buf = vec![0u64; (len as usize).div_ceil(8).max(4)];
            let size = (buf.len() * 8) as u32;
            let status = NtQueryInformationProcess(h, ProcessCommandLineInformation, buf.as_mut_ptr().cast::<c_void>(), size, &mut len);
            CloseHandle(h);
            if status < 0 {
                return None;
            }
            let us = &*buf.as_ptr().cast::<UNICODE_STRING>();
            if us.Buffer.is_null() || us.Length == 0 {
                return None;
            }
            let mut wide = std::slice::from_raw_parts(us.Buffer, us.Length as usize / 2).to_vec();
            wide.push(0);
            let mut argc = 0;
            let argv = CommandLineToArgvW(wide.as_ptr(), &mut argc);
            if argv.is_null() {
                return None;
            }
            let args = (0..argc.max(0) as usize)
                .map(|i| {
                    let p = *argv.add(i);
                    let n = (0..).take_while(|&j| *p.add(j) != 0).count();
                    String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
                })
                .collect();
            LocalFree(argv.cast());
            Some(args)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{attached_job, is_interpreter, script_arg, shell_command};

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn shell_commands() {
        let wrapped = "source /Users/x/.claude/shell-snapshots/snapshot-zsh-1-a.sh 2>/dev/null || true && setopt NO_EXTENDED_GLOB 2>/dev/null || true && eval 'sleep 30 && echo '\\''done'\\''' < /dev/null && pwd -P >| /tmp/claude-edee-cwd";
        assert_eq!(shell_command(&argv(&["/bin/zsh", "-c", wrapped])), Some("sleep 30 && echo 'done'".into()));
        assert_eq!(shell_command(&argv(&["bash", "-lc", "npm run dev"])), Some("npm run dev".into()));
        assert_eq!(shell_command(&argv(&["-zsh", "-c", "ls"])), Some("ls".into()));
        // Not a shell running a command.
        assert_eq!(shell_command(&argv(&["bash", "/Users/x/.claude/statusline-command.sh"])), None);
        assert_eq!(shell_command(&argv(&["node", "-c", "x"])), None);
        assert_eq!(shell_command(&argv(&["zsh", "--norc", "x"])), None);
        assert_eq!(shell_command(&argv(&["zsh", "-c"])), None);
    }

    #[test]
    fn attached_jobs() {
        assert_eq!(attached_job(&argv(&["claude", "attach", "fd5baae5"])), Some("fd5baae5".into()));
        assert_eq!(attached_job(&argv(&["/Users/x/.local/bin/claude", "attach", "j1"])), Some("j1".into()));
        assert_eq!(attached_job(&argv(&["claude", "--resume", "j1"])), None);
        assert_eq!(attached_job(&argv(&["claude", "attach", "--help"])), None);
        assert_eq!(attached_job(&argv(&["node", "attach", "j1"])), None);
    }

    #[test]
    fn interpreter_scripts() {
        assert!(is_interpreter(r"C:\Program Files\nodejs\node.exe"));
        assert!(is_interpreter("node"));
        assert!(!is_interpreter("claude.exe"));
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(script_arg(&argv(&["node", "--no-warnings", r"C:\x\codex.js", "-m"])), Some(r"C:\x\codex.js".into()));
        assert_eq!(script_arg(&argv(&["node"])), None);
    }
}
