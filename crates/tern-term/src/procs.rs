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

#[cfg(target_os = "macos")]
fn group_argvs(pgid: libc::pid_t) -> Vec<Vec<String>> {
    let mut pids = vec![0 as libc::pid_t; 256];
    let size = (pids.len() * size_of::<libc::pid_t>()) as libc::c_int;
    let n = unsafe { libc::proc_listpgrppids(pgid, pids.as_mut_ptr().cast(), size) };
    let n = if n > 0 { (n as usize).min(pids.len()) } else { 0 };
    pids.truncate(n);
    if !pids.contains(&pgid) {
        pids.push(pgid);
    }
    pids.into_iter().filter(|&p| p > 0).filter_map(argv).collect()
}

/// `pid`'s argv, from `KERN_PROCARGS2`: argc, the exec path, NUL padding,
/// then the arguments.
#[cfg(target_os = "macos")]
fn argv(pid: libc::pid_t) -> Option<Vec<String>> {
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
    let args = rest[args_start..].split(|&b| b == 0).take(argc.min(2)).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
    Some(args)
}

#[cfg(target_os = "linux")]
fn group_argvs(pgid: libc::pid_t) -> Vec<Vec<String>> {
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
    groups
        .iter()
        .filter(|(_, g)| *g == pgid)
        .filter_map(|&(pid, _)| {
            let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
            let args: Vec<String> =
                cmdline.split(|&b| b == 0).filter(|a| !a.is_empty()).take(2).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
            (!args.is_empty()).then_some(args)
        })
        .collect()
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn group_argvs(_pgid: i32) -> Vec<Vec<String>> {
    Vec::new()
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

    use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessCommandLineInformation};
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE, LocalFree, UNICODE_STRING};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

    /// `root` and every process below it with its executable name, breadth first.
    pub fn descendants(root: u32) -> Vec<(u32, String)> {
        static TREE: super::ScanCache<Tree> = super::ScanCache::new();
        let has_root = |t: &Tree| t.exes.contains_key(&root);
        let tree = TREE.get(has_root, || {
            let mut t = Tree::default();
            for (pid, ppid, exe) in snapshot() {
                // The idle process is its own parent.
                if pid != ppid {
                    t.children.entry(ppid).or_default().push(pid);
                }
                t.exes.insert(pid, exe);
            }
            t
        });
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::from([root]);
        while let Some(pid) = queue.pop_front() {
            let Some(exe) = tree.exes.get(&pid).filter(|_| seen.insert(pid)) else { continue };
            out.push((pid, exe.clone()));
            queue.extend(tree.children.get(&pid).into_iter().flatten());
        }
        out
    }

    /// Every process's children and executable name.
    #[derive(Default)]
    struct Tree {
        children: HashMap<u32, Vec<u32>>,
        exes: HashMap<u32, String>,
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
    use super::{is_interpreter, script_arg};

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
