//! What Tern's terminals cost the machine: each terminal's CPU and memory
//! (its shell and every process under it), by workspace, beside Tern's own
//! processes and the background sessions no tab shows. Read every
//! [`SAMPLE`] while the Resources dialog is open, from the kernel's
//! per-process counters (on macOS, the memory Activity Monitor shows).
//!
//! One store for the app ([`Resources`]); the dialog only draws it.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use gpui_kit::*;
use tern_mux::proto::SessionInfo;

use crate::workspace::Workspace;

const SAMPLE: Duration = Duration::from_secs(2);
/// The first sample has no earlier one to tell CPU time from: the second
/// comes soon after it.
const FIRST: Duration = Duration::from_millis(500);
/// The memory samples kept for a sparkline, oldest first.
pub const HISTORY: usize = 30;
/// Nothing drawn for this long: the dialog closed, so nothing is read.
const UNSEEN: Duration = Duration::from_secs(3);

/// A process as the kernel counts it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    /// CPU time used since it started, all threads, in nanoseconds.
    pub cpu_ns: u64,
    /// Memory it uses, in bytes.
    pub mem: u64,
}

/// What some processes use: CPU in percent of one core (over 100 when more
/// than one core is busy) and memory in bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    pub cpu: f32,
    pub mem: u64,
}

impl std::ops::AddAssign for Usage {
    fn add_assign(&mut self, o: Self) {
        self.cpu += o.cpu;
        self.mem += o.mem;
    }
}

/// Every process's usage at one sample, and who started whom.
#[derive(Debug, Default)]
pub struct Table {
    usage: HashMap<u32, Usage>,
    children: HashMap<u32, Vec<u32>>,
}

impl Table {
    /// What `root` and every process under it use, leaving out those in
    /// `claimed` and adding the rest to it: a process counts once, for the
    /// first root that reaches it.
    pub fn claim(&self, root: u32, claimed: &mut HashSet<u32>) -> Usage {
        let mut sum = Usage::default();
        let mut todo = vec![root];
        while let Some(pid) = todo.pop() {
            if !claimed.insert(pid) {
                continue;
            }
            if let Some(u) = self.usage.get(&pid) {
                sum += *u;
            }
            todo.extend(self.children.get(&pid).into_iter().flatten().copied());
        }
        sum
    }

    pub fn has(&self, pid: u32) -> bool {
        self.usage.contains_key(&pid)
    }
}

/// Turns scans into tables: CPU use is the CPU time between two scans.
#[derive(Default)]
pub struct Sampler {
    prev: HashMap<u32, u64>,
    at: Option<Instant>,
}

impl Sampler {
    /// The table for `procs`, scanned at `now`. A process the last scan
    /// didn't see (or every one, at the first) shows no CPU use yet.
    pub fn sample(&mut self, procs: &[Proc], now: Instant) -> Table {
        let wall = self.at.map(|at| now.saturating_duration_since(at).as_nanos() as f64).filter(|w| *w > 0.);
        let mut table = Table::default();
        let mut prev = HashMap::with_capacity(procs.len());
        for p in procs {
            let cpu = match (wall, self.prev.get(&p.pid)) {
                (Some(wall), Some(&before)) => (p.cpu_ns.saturating_sub(before) as f64 / wall * 100.) as f32,
                _ => 0.,
            };
            table.usage.insert(p.pid, Usage { cpu, mem: p.mem });
            if p.ppid != 0 && p.ppid != p.pid {
                table.children.entry(p.ppid).or_default().push(p.pid);
            }
            prev.insert(p.pid, p.cpu_ns);
        }
        self.prev = prev;
        self.at = Some(now);
        table
    }

    /// Whether a sample was taken: the next one knows CPU use.
    pub fn primed(&self) -> bool {
        self.at.is_some()
    }
}

/// A terminal in the dialog: a workspace's tab, or a background session.
#[derive(Debug, Clone, PartialEq)]
pub enum Owner {
    Tab { window: AnyWindowHandle, ws: WeakEntity<Workspace>, tab: EntityId },
    Session(u64),
}

#[derive(Debug, Clone)]
pub struct Item {
    pub owner: Owner,
    pub label: String,
    /// Runs an agent (Claude Code, Codex, …).
    pub agent: bool,
    /// None until its program is known (still starting, or ended).
    pub usage: Option<Usage>,
}

/// A workspace and its terminals.
#[derive(Debug, Clone)]
pub struct Group {
    pub window: AnyWindowHandle,
    pub ws: WeakEntity<Workspace>,
    pub id: EntityId,
    pub name: String,
    pub usage: Usage,
    pub items: Vec<Item>,
}

/// Everything the dialog shows, as of the last sample.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub groups: Vec<Group>,
    /// Sessions in the daemon that no tab shows.
    pub background: Vec<Item>,
    /// The app's own processes (and what it started, like `git`).
    pub app: Usage,
    /// The daemon the terminals run in, by itself.
    pub daemon: Usage,
    pub total: Usage,
}

/// The app's one resources store.
pub struct Resources(pub Entity<ResourceStore>);

impl Global for Resources {}

pub struct ResourceStore {
    /// None before the first sample.
    pub snapshot: Option<Snapshot>,
    /// Memory over the last samples, oldest first, by workspace (and
    /// [`ResourceStore::tern_history`] for Tern itself).
    history: HashMap<Option<EntityId>, VecDeque<u64>>,
    seen: Option<Instant>,
    _task: Task<()>,
}

/// The store, made on first use.
pub fn store(cx: &mut App) -> Entity<ResourceStore> {
    if let Some(s) = cx.try_global::<Resources>() {
        return s.0.clone();
    }
    let store = cx.new(ResourceStore::new);
    cx.set_global(Resources(store.clone()));
    store
}

/// What a sample read off the UI thread.
struct Read {
    table: Table,
    /// None when the daemon couldn't be asked: the last list stands.
    sessions: Option<Vec<SessionInfo>>,
    daemon: Option<u32>,
}

impl ResourceStore {
    fn new(cx: &mut Context<Self>) -> Self {
        let mux = cx.global::<crate::AppServices>().0.mux.clone();
        let task = cx.spawn(async move |this, cx| {
            let mut sampler = Sampler::default();
            let mut sessions: Vec<SessionInfo> = Vec::new();
            loop {
                let Ok(open) = this.update(cx, |this, _| this.seen.is_some_and(|s| s.elapsed() < UNSEEN)) else { break };
                if !open {
                    // Next time it opens, CPU starts from a fresh scan.
                    sampler = Sampler::default();
                    cx.background_executor().timer(Duration::from_millis(250)).await;
                    continue;
                }
                let mux = mux.clone();
                let (back, read) = cx
                    .background_executor()
                    .spawn(async move {
                        let table = sampler.sample(&scan(), Instant::now());
                        let client = mux.existing();
                        let daemon = client.as_ref().map(|c| c.server_pid());
                        // Asked each sample: a session that ended or was
                        // adopted shows so at once.
                        let sessions = client.and_then(|c| c.list().ok());
                        (sampler, Read { table, sessions, daemon })
                    })
                    .await;
                sampler = back;
                if let Some(s) = &read.sessions {
                    sessions.clone_from(s);
                }
                let first = !sampler.primed();
                if this.update(cx, |this, cx| this.take(read.table, &sessions, read.daemon, cx)).is_err() {
                    break;
                }
                cx.background_executor().timer(if first { FIRST } else { SAMPLE }).await;
            }
        });
        Self { snapshot: None, history: HashMap::new(), seen: None, _task: task }
    }

    /// The dialog shows the numbers: keep them fresh.
    pub fn seen(&mut self) {
        self.seen = Some(Instant::now());
    }

    /// Workspace `id`'s memory over the last samples (None: Tern's own).
    pub fn history(&self, id: Option<EntityId>) -> &[u64] {
        self.history.get(&id).map_or(&[], |h| h.as_slices().0)
    }

    fn take(&mut self, table: Table, sessions: &[SessionInfo], daemon: Option<u32>, cx: &mut Context<Self>) {
        let mut tabs = Vec::new();
        for (window, ws) in crate::main_window::all_workspaces(cx) {
            let w = ws.read(cx);
            let terms = w
                .term_tabs(cx)
                .into_iter()
                .map(|t| {
                    let mux = t.view.read(cx).mux_id();
                    (t.id, t.title, t.agent.is_some(), mux)
                })
                .collect();
            tabs.push((window, ws.downgrade(), ws.entity_id(), w.name(), terms));
        }
        let snap = attribute(&table, sessions, &tabs, std::process::id(), daemon);
        let mut keep = HashSet::new();
        for g in &snap.groups {
            keep.insert(Some(g.id));
            push(self.history.entry(Some(g.id)).or_default(), g.usage.mem);
        }
        keep.insert(None);
        push(self.history.entry(None).or_default(), snap.app.mem + snap.daemon.mem);
        self.history.retain(|k, _| keep.contains(k));
        self.snapshot = Some(snap);
        cx.notify();
    }
}

fn push(h: &mut VecDeque<u64>, v: u64) {
    if h.len() == HISTORY {
        h.pop_front();
    }
    h.push_back(v);
    // One slice, for `history`.
    h.make_contiguous();
}

/// A workspace as the store reads it: its window, entity, id, name, and its
/// terminal tabs (id, title, runs an agent, session id).
type TabsOf = (AnyWindowHandle, WeakEntity<Workspace>, EntityId, String, Vec<(EntityId, String, bool, Option<u64>)>);

/// Who uses what: each tab's session's processes, then the sessions no tab
/// shows, then the app's, then the daemon's own.
fn attribute(table: &Table, sessions: &[SessionInfo], workspaces: &[TabsOf], app: u32, daemon: Option<u32>) -> Snapshot {
    let by_id: HashMap<u64, &SessionInfo> = sessions.iter().map(|s| (s.id, s)).collect();
    let mut claimed = HashSet::new();
    // The daemon's own process isn't any terminal's.
    if let Some(d) = daemon {
        claimed.insert(d);
    }
    let usage_of = |id: Option<u64>, claimed: &mut HashSet<u32>| {
        let s = by_id.get(&id?)?;
        // An ended session's pid may be another process's by now.
        (!s.exited && s.pid != 0 && table.has(s.pid)).then(|| table.claim(s.pid, claimed))
    };
    let mut shown = HashSet::new();
    let mut snap = Snapshot::default();
    for (window, ws, id, name, terms) in workspaces {
        let mut group = Group { window: *window, ws: ws.clone(), id: *id, name: name.clone(), usage: Usage::default(), items: vec![] };
        for (tab, title, agent, mux) in terms {
            shown.extend(*mux);
            let usage = usage_of(*mux, &mut claimed);
            group.usage += usage.unwrap_or_default();
            group.items.push(Item {
                owner: Owner::Tab { window: *window, ws: ws.clone(), tab: *tab },
                label: title.clone(),
                agent: *agent,
                usage,
            });
        }
        snap.total += group.usage;
        snap.groups.push(group);
    }
    for s in sessions.iter().filter(|s| !shown.contains(&s.id) && !s.exited) {
        let usage = usage_of(Some(s.id), &mut claimed);
        snap.total += usage.unwrap_or_default();
        let label = s.title.clone().or_else(|| s.program.clone()).unwrap_or_else(|| format!("Session {}", s.id));
        snap.background.push(Item { owner: Owner::Session(s.id), label, agent: s.claude, usage });
    }
    snap.app = table.claim(app, &mut claimed);
    if let Some(d) = daemon {
        claimed.remove(&d);
        snap.daemon = table.claim(d, &mut claimed);
    }
    snap.total += snap.app;
    snap.total += snap.daemon;
    snap
}

/// Bytes for the dialog: `812 MB`, `1.4 GB`.
pub fn bytes(b: u64) -> String {
    const MB: f64 = (1u64 << 20) as f64;
    let mb = b as f64 / MB;
    if mb >= 1024. { format!("{:.1} GB", mb / 1024.) } else { format!("{} MB", mb.round()) }
}

/// CPU for the dialog: `0.4%`, `12%`, `140%`.
pub fn percent(cpu: f32) -> String {
    if cpu < 10. { format!("{cpu:.1}%") } else { format!("{}%", cpu.round()) }
}

/// Every process the kernel lets this user read.
#[cfg(target_os = "macos")]
fn scan() -> Vec<Proc> {
    use std::sync::OnceLock;
    // CPU times come in Mach ticks (not nanoseconds on Apple silicon).
    static TIMEBASE: OnceLock<(u64, u64)> = OnceLock::new();
    #[allow(deprecated)]
    let &(numer, denom) = TIMEBASE.get_or_init(|| {
        let mut tb = libc::mach_timebase_info { numer: 0, denom: 0 };
        if unsafe { libc::mach_timebase_info(&mut tb) } != 0 || tb.denom == 0 {
            return (1, 1);
        }
        (u64::from(tb.numer), u64::from(tb.denom))
    });
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let Ok(n) = usize::try_from(n) else { return Vec::new() };
    // Room for those started meanwhile.
    let mut pids = vec![0 as libc::pid_t; n + 64];
    let size = libc::c_int::try_from(pids.len() * size_of::<libc::pid_t>()).unwrap_or(libc::c_int::MAX);
    let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), size) };
    pids.truncate(usize::try_from(n).unwrap_or(0).min(pids.len()));
    pids.into_iter()
        .filter(|&p| p > 0)
        .filter_map(|pid| {
            let mut bsd: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
            let size = size_of::<libc::proc_bsdinfo>() as libc::c_int;
            if unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut bsd).cast(), size) } != size {
                return None;
            }
            let mut ru: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
            if unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V2, (&raw mut ru).cast()) } != 0 {
                return None;
            }
            let ticks = u128::from(ru.ri_user_time + ru.ri_system_time);
            let cpu_ns = u64::try_from(ticks * u128::from(numer) / u128::from(denom)).unwrap_or(u64::MAX);
            Some(Proc { pid: pid as u32, ppid: bsd.pbi_ppid, cpu_ns, mem: ru.ri_phys_footprint })
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn scan() -> Vec<Proc> {
    let tick = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1) as u64;
    let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
    dir.filter_map(|e| {
        let pid: u32 = e.ok()?.file_name().to_str()?.parse().ok()?;
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        parse_stat(pid, &stat, tick, page)
    })
    .collect()
}

/// A `/proc/<pid>/stat` line: the fields after the command (which may hold
/// spaces and parentheses) are state, ppid, …, utime (14), stime (15), …,
/// rss (24, in pages).
#[cfg(any(target_os = "linux", test))]
fn parse_stat(pid: u32, stat: &str, tick: u64, page: u64) -> Option<Proc> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    let field = |n: usize| f.get(n - 3).and_then(|v| v.parse::<u64>().ok());
    let cpu = field(14)? + field(15)?;
    Some(Proc { pid, ppid: u32::try_from(field(4)?).ok()?, cpu_ns: cpu * 1_000_000_000 / tick, mem: field(24)? * page })
}

#[cfg(windows)]
fn scan() -> Vec<Proc> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
    use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows_sys::Win32::System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snap == INVALID_HANDLE_VALUE {
        return Vec::new();
    }
    let mut entries = vec![];
    let mut e = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
    let mut more = unsafe { Process32FirstW(snap, &mut e) } != 0;
    while more {
        entries.push((e.th32ProcessID, e.th32ParentProcessID));
        more = unsafe { Process32NextW(snap, &mut e) } != 0;
    }
    unsafe { CloseHandle(snap) };
    let t = |f: FILETIME| (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);
    // With the time each started, so a parent id reused since is told apart.
    let read: Vec<(Proc, u64)> = entries
        .into_iter()
        .filter(|&(pid, _)| pid != 0)
        .filter_map(|(pid, ppid)| {
            let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
            if h.is_null() {
                return None;
            }
            let (mut created, mut exited, mut kernel, mut user) = Default::default();
            let times = unsafe { GetProcessTimes(h, &mut created, &mut exited, &mut kernel, &mut user) } != 0;
            let mut m = PROCESS_MEMORY_COUNTERS { cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32, ..Default::default() };
            let mem = unsafe { K32GetProcessMemoryInfo(h, &mut m, m.cb) } != 0;
            unsafe { CloseHandle(h) };
            // Times count 100 ns.
            let proc = Proc { pid, ppid, cpu_ns: (t(kernel) + t(user)).saturating_mul(100), mem: m.WorkingSetSize as u64 };
            (times && mem).then_some((proc, t(created)))
        })
        .collect();
    // Windows keeps a child's parent id after the parent exits, and the id
    // may then go to a newer process: that one isn't its parent.
    let started: HashMap<u32, u64> = read.iter().map(|(p, at)| (p.pid, *at)).collect();
    read.into_iter()
        .map(|(mut p, at)| {
            if started.get(&p.ppid).is_none_or(|&parent| parent > at) {
                p.ppid = 0;
            }
            p
        })
        .collect()
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn scan() -> Vec<Proc> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::{Duration, Instant};

    use tern_mux::proto::SessionInfo;

    #[cfg(any(target_os = "macos", windows))]
    use super::scan;
    use super::{Proc, Sampler, Usage, attribute, bytes, parse_stat, percent};

    fn p(pid: u32, ppid: u32, cpu_ns: u64, mem: u64) -> Proc {
        Proc { pid, ppid, cpu_ns, mem }
    }

    #[test]
    fn cpu_is_time_used_over_time_passed() {
        let mut s = Sampler::default();
        let t0 = Instant::now();
        let first = s.sample(&[p(1, 0, 1_000, 10)], t0);
        assert_eq!(first.usage[&1], Usage { cpu: 0., mem: 10 });
        // 0.5 s of CPU in 1 s, and a process the first scan didn't see.
        let t = s.sample(&[p(1, 0, 500_001_000, 10), p(2, 1, 9_000, 5)], t0 + Duration::from_secs(1));
        assert_eq!(t.usage[&1].cpu, 50.);
        assert_eq!(t.usage[&2].cpu, 0.);
    }

    #[test]
    fn a_process_counts_for_the_first_root_reaching_it() {
        let mut s = Sampler::default();
        // 10 → 11 → 12, 10 → 13; 20 alone.
        let t = s.sample(&[p(10, 1, 0, 1), p(11, 10, 0, 2), p(12, 11, 0, 4), p(13, 10, 0, 8), p(20, 1, 0, 16)], Instant::now());
        let mut claimed = HashSet::new();
        assert_eq!(t.claim(11, &mut claimed).mem, 6);
        assert_eq!(t.claim(10, &mut claimed).mem, 9);
        assert_eq!(t.claim(20, &mut claimed).mem, 16);
        assert_eq!(t.claim(10, &mut claimed).mem, 0);
    }

    fn session(id: u64, pid: u32) -> SessionInfo {
        SessionInfo { id, pid, ..Default::default() }
    }

    #[test]
    fn background_sessions_and_tern_split_the_rest() {
        let mut s = Sampler::default();
        // App 1 (with git 2); daemon 5 runs shells 6 (tab) and 7 (no tab),
        // whose child 8 counts with it.
        let procs = [p(1, 0, 0, 100), p(2, 1, 0, 10), p(5, 0, 0, 50), p(6, 5, 0, 30), p(7, 5, 0, 20), p(8, 7, 0, 5)];
        let t = s.sample(&procs, Instant::now());
        let sessions = [session(1, 6), session(2, 7), SessionInfo { exited: true, ..session(3, 2) }];
        let snap = attribute(&t, &sessions, &[], 1, Some(5));
        assert_eq!(snap.background.len(), 2);
        assert_eq!(snap.background[0].usage.map(|u| u.mem), Some(30));
        assert_eq!(snap.background[1].usage.map(|u| u.mem), Some(25));
        assert_eq!(snap.app.mem, 110);
        assert_eq!(snap.daemon.mem, 50);
        assert_eq!(snap.total.mem, 215);
    }

    #[test]
    fn labels() {
        assert_eq!(bytes(5 << 20), "5 MB");
        assert_eq!(bytes(3 << 29), "1.5 GB");
        assert_eq!(percent(0.42), "0.4%");
        assert_eq!(percent(140.4), "140%");
    }

    #[test]
    fn reads_a_proc_stat_line() {
        let line = "42 (my (odd) cmd) S 7 42 42 0 -1 4194560 100 0 0 0 250 50 0 0 20 0 1 0 100 1000000 300 18446744073709551615";
        let proc = parse_stat(42, line, 100, 4096).expect("parsed");
        assert_eq!(proc, Proc { pid: 42, ppid: 7, cpu_ns: 3_000_000_000, mem: 300 * 4096 });
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn scans_this_machine() {
        let procs = scan();
        let me = procs.iter().find(|p| p.pid == std::process::id()).expect("this process");
        assert!(me.mem > 0 && me.cpu_ns > 0);
    }
}
