//! How busy the machine is: the CPU's load over the last samples and how
//! much memory is in use, read from the kernel (Mach's host statistics, as
//! Activity Monitor does) every [`SAMPLE`].
//!
//! One store for the app ([`System`]); the system panels only draw it. It
//! samples while a panel was drawn lately.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use gpui_kit::*;

const SAMPLE: Duration = Duration::from_secs(2);
/// The CPU samples kept, oldest first: the sparkline's bars.
pub const HISTORY: usize = 12;
/// No panel drawn for this long: nobody looks, so nothing is read.
const UNSEEN: Duration = Duration::from_secs(10);

/// The app's one system store.
pub struct System(pub Entity<SystemStore>);

impl Global for System {}

pub struct SystemStore {
    /// CPU load in percent, oldest first; at most [`HISTORY`].
    pub cpu: VecDeque<f32>,
    /// Memory in use, in percent.
    pub mem: Option<f32>,
    pub mem_used: u64,
    pub mem_total: u64,
    ticks: Option<Ticks>,
    seen: Option<Instant>,
    _task: Task<()>,
}

/// The system store, made on first use.
pub fn store(cx: &mut App) -> Entity<SystemStore> {
    if let Some(s) = cx.try_global::<System>() {
        return s.0.clone();
    }
    let store = cx.new(SystemStore::new);
    cx.set_global(System(store.clone()));
    store
}

impl SystemStore {
    fn new(cx: &mut Context<Self>) -> Self {
        let task = cx.spawn(async move |this, cx| {
            loop {
                match this.update(cx, |this, _| this.seen.is_some_and(|s| s.elapsed() < UNSEEN)) {
                    Ok(true) => {
                        let read = cx.background_executor().spawn(async { (cpu_ticks(), memory()) }).await;
                        if this.update(cx, |this, cx| this.take(read, cx)).is_err() {
                            break;
                        }
                    },
                    Ok(false) => {},
                    Err(_) => break,
                }
                cx.background_executor().timer(SAMPLE).await;
            }
        });
        Self { cpu: VecDeque::new(), mem: None, mem_used: 0, mem_total: 0, ticks: None, seen: None, _task: task }
    }

    /// A panel shows the numbers: keep them fresh.
    pub fn seen(&mut self) {
        self.seen = Some(Instant::now());
    }

    fn take(&mut self, (ticks, mem): (Option<Ticks>, Option<(u64, u64)>), cx: &mut Context<Self>) {
        // A load is the ticks between two samples.
        if let (Some(old), Some(new)) = (self.ticks, ticks) {
            if let Some(load) = old.load_until(new) {
                if self.cpu.len() == HISTORY {
                    self.cpu.pop_front();
                }
                self.cpu.push_back(load);
            }
        }
        self.ticks = ticks;
        if let Some((used, total)) = mem {
            self.mem_used = used;
            self.mem_total = total;
            self.mem = Some((used as f64 / total as f64 * 100.) as f32);
        }
        cx.notify();
    }
}

/// The CPU's ticks since boot, all cores together.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Ticks {
    busy: u64,
    total: u64,
}

impl Ticks {
    /// Percent busy from `self` to `later`; none when no time passed.
    fn load_until(self, later: Ticks) -> Option<f32> {
        let total = later.total.checked_sub(self.total).filter(|t| *t > 0)?;
        let busy = later.busy.saturating_sub(self.busy).min(total);
        Some((busy as f64 / total as f64 * 100.) as f32)
    }
}

/// Bytes as GB with one decimal, for the card: `12.3 GB`.
pub fn gigabytes(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / (1u64 << 30) as f64)
}

#[cfg(target_os = "macos")]
fn host() -> libc::mach_port_t {
    use std::sync::OnceLock;
    // Each call takes a port right: ask once.
    static HOST: OnceLock<libc::mach_port_t> = OnceLock::new();
    #[allow(deprecated)]
    *HOST.get_or_init(|| unsafe { libc::mach_host_self() })
}

#[cfg(target_os = "macos")]
fn cpu_ticks() -> Option<Ticks> {
    let mut info: libc::host_cpu_load_info = unsafe { std::mem::zeroed() };
    let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
    let r = unsafe {
        libc::host_statistics64(host(), libc::HOST_CPU_LOAD_INFO, (&raw mut info).cast(), &mut count)
    };
    if r != libc::KERN_SUCCESS {
        return None;
    }
    let t = info.cpu_ticks.map(u64::from);
    let busy = t[libc::CPU_STATE_USER as usize] + t[libc::CPU_STATE_SYSTEM as usize] + t[libc::CPU_STATE_NICE as usize];
    Some(Ticks { busy, total: busy + t[libc::CPU_STATE_IDLE as usize] })
}

/// Memory in use and in all, in bytes. In use is what Activity Monitor
/// calls Memory Used: app memory, wired and compressed.
#[cfg(target_os = "macos")]
fn memory() -> Option<(u64, u64)> {
    let mut total: u64 = 0;
    let mut len = size_of::<u64>();
    let r = unsafe { libc::sysctlbyname(c"hw.memsize".as_ptr(), (&raw mut total).cast(), &mut len, std::ptr::null_mut(), 0) };
    if r != 0 || total == 0 {
        return None;
    }
    let mut vm: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
    let mut count = libc::HOST_VM_INFO64_COUNT;
    let r = unsafe { libc::host_statistics64(host(), libc::HOST_VM_INFO64, (&raw mut vm).cast(), &mut count) };
    if r != libc::KERN_SUCCESS {
        return None;
    }
    let page = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok().filter(|p| *p > 0)?;
    let app = u64::from(vm.internal_page_count).saturating_sub(u64::from(vm.purgeable_count));
    let pages = app + u64::from(vm.wire_count) + u64::from(vm.compressor_page_count);
    Some(((pages * page).min(total), total))
}

#[cfg(not(target_os = "macos"))]
fn cpu_ticks() -> Option<Ticks> {
    None
}

#[cfg(not(target_os = "macos"))]
fn memory() -> Option<(u64, u64)> {
    None
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use super::{cpu_ticks, memory};
    use super::Ticks;

    #[test]
    fn load_is_busy_ticks_over_all_ticks_between_samples() {
        let a = Ticks { busy: 100, total: 1000 };
        assert_eq!(a.load_until(Ticks { busy: 125, total: 1100 }), Some(25.));
        assert_eq!(a.load_until(a), None);
        // A counter that went back (it wraps) reads as no time passed.
        assert_eq!(a.load_until(Ticks { busy: 0, total: 10 }), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reads_this_machine() {
        let (used, total) = memory().expect("memory");
        assert!(used > 0 && used <= total);
        let t = cpu_ticks().expect("ticks");
        assert!(t.total >= t.busy);
    }
}
