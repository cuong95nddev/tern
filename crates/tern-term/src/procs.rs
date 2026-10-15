//! What runs in a PTY's foreground: the processes of its foreground process
//! group, named so the UI can tell which program a shell is running.

use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

/// Names of the programs in `tty`'s foreground process group: each one's
/// executable and, for interpreters (`node cli.js`), its script. Lowercase
/// file stems, e.g. `["node", "codex"]`. Empty when it can't be read.
pub fn foreground_programs(tty: &File) -> Vec<String> {
    let pgid = unsafe { libc::tcgetpgrp(tty.as_raw_fd()) };
    if pgid <= 0 {
        return Vec::new();
    }
    let mut names = Vec::new();
    for argv in group_argvs(pgid) {
        // The executable, then the first non-flag argument (a script).
        for arg in argv.iter().take(1).chain(argv.iter().skip(1).find(|a| !a.starts_with('-'))) {
            if let Some(stem) = Path::new(arg).file_stem() {
                let stem = stem.to_string_lossy().to_ascii_lowercase();
                if !names.contains(&stem) {
                    names.push(stem);
                }
            }
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
    let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
    dir.flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            // `pid (comm) state ppid pgrp ...`; comm may hold spaces.
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { return false };
            let Some((_, after)) = stat.rsplit_once(')') else { return false };
            after.split_whitespace().nth(2).and_then(|g| g.parse::<libc::pid_t>().ok()) == Some(pgid)
        })
        .filter_map(|pid| {
            let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
            let args: Vec<String> =
                cmdline.split(|&b| b == 0).filter(|a| !a.is_empty()).take(2).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
            (!args.is_empty()).then_some(args)
        })
        .collect()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn group_argvs(_pgid: i32) -> Vec<Vec<String>> {
    Vec::new()
}

/// The working directory of `tty`'s foreground process group leader (the
/// shell, or the program it runs), to resolve the relative paths it prints.
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

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn cwd(_pid: i32) -> Option<PathBuf> {
    None
}
