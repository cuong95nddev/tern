//! One program in the daemon: its PTY, the terminal that parses its output,
//! and the thread that drives both.
//!
//! The thread reads from the PTY, parses under the terminal lock and hands
//! the same bytes to the attached client before unlocking. Attaching takes
//! that lock to capture the snapshot and set the subscriber, so the client
//! gets every byte after its snapshot exactly once. Resizes, the end of a
//! synchronized update and the exit are sent the same way, as markers.
//! (alacritty's own event loop reads before it locks, so it can't do this.)
//! Viewers (remote browsers) get the same stream; they never own the session.

use std::borrow::Cow;
use std::collections::VecDeque;
#[cfg(unix)]
use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::num::NonZeroUsize;
#[cfg(unix)]
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use polling::{Event as PollEvent, Events, PollMode, Poller};
use tern_term::mirror::Marker;
use tern_term::osc::{OscScanner, OscSignal, PROGRAM_STATUS_PROBE_REPLY, color_scheme_report};
use tern_term::palette::PaletteFile;
use tern_term::snapshot::{self, Snapshot};
use tern_term::status::{ActivityStatus, next_status};
use tern_term::vt::event::{Event, EventListener, OnResize, WindowSize};
use tern_term::vt::sync::FairMutex;
use tern_term::vt::term::{Config, Osc52, Term};
use tern_term::vt::tty::{self, ChildEvent, EventedPty, EventedReadWrite, Options, Shell};
#[cfg(windows)]
use tern_term::vt::tty::{PTY_CHILD_EVENT_TOKEN, PTY_READ_WRITE_TOKEN};
use tern_term::vt::vte::ansi::{Processor, StdSyncHandler};
use tern_term::{SpawnConfig, TermSize};

use crate::proto::SessionInfo;

/// alacritty's poll keys for the PTY and its child-exit pipe: crate-private
/// on Unix, public (and other values) on Windows.
#[cfg(unix)]
const PTY_READ_WRITE_TOKEN: usize = 0;
#[cfg(unix)]
const PTY_CHILD_EVENT_TOKEN: usize = 1;
const READ_BUFFER: usize = 64 << 10;
/// The most bytes parsed in one go under the lock before letting an attach in.
const MAX_LOCKED_READ: usize = 1 << 20;
const PROBE_REPLY_MIN_INTERVAL: Duration = Duration::from_millis(500);
/// How long the rows+1 size of an attach "jiggle" lasts.
const JIGGLE: Duration = Duration::from_millis(50);
/// How often a program that wants color scheme reports has the theme checked.
const SCHEME_CHECK: Duration = Duration::from_millis(300);
/// How long a closed program has to exit after SIGHUP before SIGKILL.
#[cfg(unix)]
const KILL_GRACE: Duration = Duration::from_secs(3);

/// Where an attached client gets its bytes. Called with the terminal locked:
/// must not block (queue and return).
pub trait Subscriber: Send + Sync {
    fn output(&self, id: u64, bytes: &[u8]);
    fn marker(&self, id: u64, marker: &Marker);
}

/// What the daemon wants to know about a program, attached or not.
pub trait Observer: Send + Sync {
    fn status(&self, id: u64, old: ActivityStatus, new: ActivityStatus);
    fn notify(&self, id: u64, title: Option<String>, body: String);
    fn exited(&self, id: u64, code: Option<i32>);
}

enum Msg {
    Input(Cow<'static, [u8]>),
    Resize(TermSize),
    Jiggle(TermSize),
    Shutdown,
}

struct Shared {
    tx: Mutex<Sender<Msg>>,
    poller: Arc<Poller>,
    size: Mutex<TermSize>,
    title: Mutex<Option<String>>,
    status: Mutex<ActivityStatus>,
    exited: Mutex<Option<Option<i32>>>,
    subscriber: Mutex<Option<Arc<dyn Subscriber>>>,
    /// Watch alongside the subscriber, without owning the session.
    viewers: Mutex<Vec<Arc<dyn Subscriber>>>,
}

impl Shared {
    fn send(&self, msg: Msg) {
        let _ = self.tx.lock().unwrap().send(msg);
        let _ = self.poller.notify();
    }

    fn subscriber(&self) -> Option<Arc<dyn Subscriber>> {
        self.subscriber.lock().unwrap().clone()
    }

    /// Call `f` for the subscriber and every viewer (with the terminal locked).
    fn each(&self, mut f: impl FnMut(&dyn Subscriber)) {
        if let Some(sub) = self.subscriber() {
            f(&*sub);
        }
        let viewers = self.viewers.lock().unwrap();
        for v in viewers.iter() {
            f(&**v);
        }
    }
}

fn same(a: &Arc<dyn Subscriber>, b: &Arc<dyn Subscriber>) -> bool {
    std::ptr::addr_eq(Arc::as_ptr(a), Arc::as_ptr(b))
}

#[derive(Clone)]
struct Listener {
    shared: Arc<Shared>,
    term: Arc<OnceLock<Arc<FairMutex<HostTerm>>>>,
}

type HostTerm = Term<Listener>;

/// The app's terminal palette, shared by every session.
fn theme_palette() -> &'static PaletteFile {
    static FILE: OnceLock<PaletteFile> = OnceLock::new();
    FILE.get_or_init(|| PaletteFile::new(crate::paths::palette_path()))
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let sh = &self.shared;
        match event {
            Event::PtyWrite(text) => sh.send(Msg::Input(text.into_bytes().into())),
            Event::ColorRequest(idx, fmt) => {
                // Asked from inside the parser, with the lock held: overrides
                // are read when the lock happens to be free.
                let palette = theme_palette().get();
                let rgb = self
                    .term
                    .get()
                    .and_then(|t| t.try_lock_unfair().map(|t| palette.indexed(t.colors(), idx)))
                    .unwrap_or_else(|| palette.indexed(&Default::default(), idx));
                sh.send(Msg::Input(fmt(rgb).into_bytes().into()));
            },
            Event::TextAreaSizeRequest(fmt) => {
                let s = *sh.size.lock().unwrap();
                let ws = WindowSize { num_lines: s.rows, num_cols: s.cols, cell_width: s.cell_width, cell_height: s.cell_height };
                sh.send(Msg::Input(fmt(ws).into_bytes().into()));
            },
            Event::Title(t) => *sh.title.lock().unwrap() = Some(t),
            Event::ResetTitle => *sh.title.lock().unwrap() = None,
            _ => {},
        }
    }
}

pub struct Host {
    id: u64,
    pid: u32,
    /// The PTY master, for foreground-process queries and fd passing.
    #[cfg(unix)]
    master: File,
    shared: Arc<Shared>,
    term: Arc<FairMutex<HostTerm>>,
    /// Locked after `term`, never before.
    parser: Arc<Mutex<Processor<StdSyncHandler>>>,
}

fn window_size(s: TermSize) -> WindowSize {
    WindowSize { num_lines: s.rows, num_cols: s.cols, cell_width: s.cell_width, cell_height: s.cell_height }
}

impl Host {
    pub fn spawn(id: u64, cfg: &SpawnConfig, observer: Arc<dyn Observer>) -> io::Result<Arc<Self>> {
        let (tx, rx) = mpsc::channel();
        let poller = Arc::new(Poller::new()?);
        let shared = Arc::new(Shared {
            tx: Mutex::new(tx),
            poller: poller.clone(),
            size: Mutex::new(cfg.size),
            title: Mutex::new(None),
            status: Mutex::new(ActivityStatus::None),
            exited: Mutex::new(None),
            subscriber: Mutex::new(None),
            viewers: Mutex::new(Vec::new()),
        });
        let cell = Arc::new(OnceLock::new());
        let listener = Listener { shared: shared.clone(), term: cell.clone() };
        let config = Config { scrolling_history: cfg.scrollback, kitty_keyboard: true, osc52: Osc52::OnlyCopy, ..Config::default() };
        let term = Arc::new(FairMutex::new(Term::new(config, &cfg.size, listener)));
        let _ = cell.set(term.clone());

        let mut env = cfg.env.clone();
        env.entry("TERM".into()).or_insert_with(|| "xterm-256color".into());
        env.entry("COLORTERM".into()).or_insert_with(|| "truecolor".into());
        env.entry("TERM_PROGRAM".into()).or_insert_with(|| "Tern".into());
        // Programs that pick light or dark colors read this before asking
        // (OSC 11, CSI ? 996 n); "foreground;background" as ANSI colors.
        let fgbg = if theme_palette().get().is_dark() { "15;0" } else { "0;15" };
        env.insert("COLORFGBG".into(), fgbg.into());
        let opts = Options {
            shell: cfg.program.clone().map(|p| Shell::new(tern_term::session::program_word(p), cfg.args.clone())),
            working_directory: cfg.cwd.clone(),
            drain_on_exit: true,
            env,
            // Quote arguments for the child's command line (MSVCRT rules).
            #[cfg(windows)]
            escape_args: true,
        };
        let pty = tty::new(&opts, window_size(cfg.size), id)?;
        #[cfg(unix)]
        let (pid, master) = (pty.child().id(), pty.file().try_clone()?);
        #[cfg(windows)]
        let pid = pty.child_watcher().pid().map_or(0, |p| p.get());
        let parser = Arc::new(Mutex::new(Processor::new()));
        let host = Arc::new(Self {
            id,
            pid,
            #[cfg(unix)]
            master,
            shared: shared.clone(),
            term: term.clone(),
            parser: parser.clone(),
        });
        let driver = Driver {
            id,
            pty,
            poller,
            rx,
            term,
            shared,
            observer,
            parser,
            scanner: OscScanner::new(),
            writes: VecDeque::new(),
            last_probe_reply: None,
            scheme_reports: false,
            dark: theme_palette().get().is_dark(),
            scheme_checked: Instant::now(),
            jiggle: None,
        };
        std::thread::Builder::new().name(format!("host {id}")).spawn(move || driver.run())?;
        Ok(host)
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    #[cfg(unix)]
    pub fn master_fd(&self) -> RawFd {
        self.master.as_raw_fd()
    }

    pub fn size(&self) -> TermSize {
        *self.shared.size.lock().unwrap()
    }

    pub fn status(&self) -> ActivityStatus {
        *self.shared.status.lock().unwrap()
    }

    pub fn exited(&self) -> Option<Option<i32>> {
        *self.shared.exited.lock().unwrap()
    }

    /// Start sending to `sub`; returns the state it starts from. Replaces a
    /// previous subscriber.
    pub fn attach(&self, sub: Arc<dyn Subscriber>) -> Snapshot {
        let mut out = None;
        self.attach_with(sub, |s| out = Some(s));
        out.unwrap()
    }

    /// [`Self::attach`], handing the snapshot to `f` while the terminal is
    /// still locked: whatever `f` queues goes before the bytes that follow.
    pub fn attach_with(&self, sub: Arc<dyn Subscriber>, f: impl FnOnce(Snapshot)) {
        let mut term = self.term.lock();
        f(self.capture(&mut term));
        *self.shared.subscriber.lock().unwrap() = Some(sub);
    }

    /// The snapshot an attach or a view starts from (`term` locked).
    fn capture(&self, term: &mut HostTerm) -> Snapshot {
        // Bytes held for an open synchronized update aren't in the grid yet:
        // end it, so the snapshot has them (those watching end it too).
        let mut parser = self.parser.lock().unwrap();
        if parser.sync_bytes_count() > 0 {
            parser.stop_sync(term);
            self.shared.each(|old| old.marker(self.id, &Marker::SyncFlush));
        }
        drop(parser);
        let title = self.shared.title.lock().unwrap().clone();
        snapshot::capture(term, self.size(), title, self.status())
    }

    /// Stop sending to `sub` (if it is still the one attached).
    pub fn detach(&self, sub: &Arc<dyn Subscriber>) {
        let _term = self.term.lock();
        let mut cur = self.shared.subscriber.lock().unwrap();
        if cur.as_ref().is_some_and(|c| same(c, sub)) {
            *cur = None;
        }
    }

    /// Like [`Self::attach_with`], but `sub` watches beside the subscriber
    /// instead of replacing it. Viewing again takes a new snapshot.
    pub fn view_with(&self, sub: Arc<dyn Subscriber>, f: impl FnOnce(Snapshot)) {
        let mut term = self.term.lock();
        f(self.capture(&mut term));
        let mut viewers = self.shared.viewers.lock().unwrap();
        if !viewers.iter().any(|v| same(v, &sub)) {
            viewers.push(sub);
        }
    }

    /// Stop sending to viewer `sub`.
    pub fn unview(&self, sub: &Arc<dyn Subscriber>) {
        let _term = self.term.lock();
        self.shared.viewers.lock().unwrap().retain(|v| !same(v, sub));
    }

    pub fn attached(&self) -> bool {
        self.shared.subscriber.lock().unwrap().is_some()
    }

    pub fn input(&self, bytes: Vec<u8>) {
        self.shared.send(Msg::Input(bytes.into()));
    }

    pub fn resize(&self, size: TermSize) {
        self.shared.send(Msg::Resize(size));
    }

    /// Resize to one more row, then to `size`: full-screen programs repaint
    /// even when the size didn't change (as shpool does on attach).
    pub fn jiggle(&self, size: TermSize) {
        self.shared.send(Msg::Jiggle(size));
    }

    pub fn info(&self) -> SessionInfo {
        SessionInfo { cwd: self.cwd(), program: self.foreground_programs().into_iter().next(), ..self.info_without_processes() }
    }

    /// [`Self::info`] without `cwd` and `program`, which ask the OS.
    pub fn info_without_processes(&self) -> SessionInfo {
        SessionInfo {
            id: self.id,
            pid: self.pid,
            exited: self.exited().is_some(),
            title: self.shared.title.lock().unwrap().clone(),
            status: self.status(),
            attached: self.attached(),
            ..Default::default()
        }
    }

    pub fn cwd(&self) -> Option<PathBuf> {
        #[cfg(unix)]
        return tern_term::procs::foreground_cwd(&self.master);
        #[cfg(windows)]
        return tern_term::procs::foreground_cwd(self.pid);
    }

    pub fn foreground_programs(&self) -> Vec<String> {
        #[cfg(unix)]
        return tern_term::procs::foreground_programs(&self.master);
        #[cfg(windows)]
        return tern_term::procs::foreground_programs(self.pid);
    }

    /// Whether a shell is in the foreground (at its prompt), rather than a
    /// program run from it.
    pub fn shell_in_foreground(&self) -> bool {
        const SHELLS: [&str; 12] = ["sh", "bash", "zsh", "fish", "dash", "ksh", "tcsh", "csh", "nu", "pwsh", "powershell", "cmd"];
        let programs = self.foreground_programs();
        let name = |p: &str| p.trim_start_matches('-').trim_end_matches(".exe").to_ascii_lowercase();
        !programs.is_empty() && programs.iter().all(|p| SHELLS.contains(&name(p).as_str()))
    }

    /// End the program: SIGHUP, then SIGKILL if it is still there after a
    /// grace period. Returns at once.
    pub fn kill(&self) {
        self.shared.send(Msg::Shutdown);
    }
}

struct Driver {
    id: u64,
    pty: tty::Pty,
    poller: Arc<Poller>,
    rx: Receiver<Msg>,
    term: Arc<FairMutex<HostTerm>>,
    shared: Arc<Shared>,
    observer: Arc<dyn Observer>,
    parser: Arc<Mutex<Processor<StdSyncHandler>>>,
    scanner: OscScanner,
    writes: VecDeque<(Cow<'static, [u8]>, usize)>,
    last_probe_reply: Option<Instant>,
    /// The program asked for color scheme reports (`CSI ? 2031 h`).
    scheme_reports: bool,
    /// The app's theme was dark when last looked at, and when that was.
    dark: bool,
    scheme_checked: Instant,
    /// The size to go back to after an attach's extra row, and when.
    jiggle: Option<(Instant, TermSize)>,
}

impl Driver {
    fn run(mut self) {
        let mut interest = PollEvent::readable(0);
        // SAFETY: the PTY's fds stay open (owned by `self.pty`) until they
        // are deregistered below.
        if let Err(e) = unsafe { self.pty.register(&self.poller, interest, PollMode::Level) } {
            log::error!("host {}: poll registration failed: {e}", self.id);
            return self.finish(None);
        }
        let mut events = Events::with_capacity(NonZeroUsize::new(64).unwrap());
        let mut buf = vec![0u8; READ_BUFFER];
        let code = 'run: loop {
            let now = Instant::now();
            let sync_deadline = self.parser.lock().unwrap().sync_timeout().sync_timeout();
            let scheme_check = self.scheme_reports.then(|| self.scheme_checked + SCHEME_CHECK);
            let deadlines = [sync_deadline, self.jiggle.map(|j| j.0), scheme_check];
            let timeout = deadlines.into_iter().flatten().min().map(|d| d.saturating_duration_since(now));
            events.clear();
            if let Err(e) = self.poller.wait(&mut events, timeout) {
                if e.kind() == ErrorKind::Interrupted {
                    continue;
                }
                log::error!("host {}: poll failed: {e}", self.id);
                break None;
            }
            let now = Instant::now();
            if sync_deadline.is_some_and(|t| t <= now) {
                let mut term = self.term.lock();
                let mut parser = self.parser.lock().unwrap();
                // An attach may have ended it meanwhile.
                if parser.sync_timeout().sync_timeout().is_some_and(|t| t <= now) {
                    parser.stop_sync(&mut *term);
                    self.emit_marker(&Marker::SyncFlush);
                }
            }
            if scheme_check.is_some_and(|t| t <= now) {
                self.check_scheme(now);
            }
            if let Some((at, size)) = self.jiggle
                && at <= now
            {
                self.jiggle = None;
                self.resize(size);
            }
            loop {
                match self.rx.try_recv() {
                    Ok(Msg::Input(bytes)) => self.writes.push_back((bytes, 0)),
                    Ok(Msg::Resize(size)) => {
                        self.jiggle = None;
                        self.resize(size);
                    },
                    Ok(Msg::Jiggle(size)) => {
                        self.resize(TermSize { rows: size.rows + 1, ..size });
                        self.jiggle = Some((Instant::now() + JIGGLE, size));
                    },
                    Ok(Msg::Shutdown) => {
                        // On Windows, closing the pseudoconsole (dropping
                        // the PTY in `finish`) ends the program.
                        #[cfg(unix)]
                        if !self.hang_up() {
                            // Dropping the PTY would wait for it forever: leave it.
                            log::warn!("host {}: the program won't end; leaving it", self.id);
                            return self.abandon();
                        }
                        return self.finish(None);
                    },
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => break 'run None,
                }
            }
            for event in events.iter() {
                match event.key {
                    PTY_CHILD_EVENT_TOKEN => {
                        if let Some(ChildEvent::Exited(status)) = self.pty.next_child_event() {
                            let _ = self.read(&mut buf);
                            break 'run status.and_then(|s| s.code());
                        }
                    },
                    PTY_READ_WRITE_TOKEN if !event.is_interrupt() => {
                        if event.readable
                            && let Err(e) = self.read(&mut buf)
                        {
                            // The program closed the PTY; its exit follows.
                            log::debug!("host {}: read: {e}", self.id);
                        }
                        if event.writable
                            && let Err(e) = self.write()
                        {
                            log::warn!("host {}: write: {e}", self.id);
                        }
                    },
                    _ => {},
                }
            }
            let needs_write = !self.writes.is_empty();
            if needs_write != interest.writable {
                interest.writable = needs_write;
                if let Err(e) = self.pty.reregister(&self.poller, interest, PollMode::Level) {
                    log::error!("host {}: poll reregistration failed: {e}", self.id);
                    break None;
                }
            }
        };
        self.finish(code)
    }

    fn emit_marker(&self, marker: &Marker) {
        self.shared.each(|sub| sub.marker(self.id, marker));
    }

    fn resize(&mut self, size: TermSize) {
        *self.shared.size.lock().unwrap() = size;
        self.pty.on_resize(window_size(size));
        let mut term = self.term.lock();
        term.resize(size);
        self.emit_marker(&Marker::Resized(size));
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<()> {
        let mut locked = 0;
        loop {
            let n = match self.pty.reader().read(buf) {
                Ok(0) => return Ok(()),
                Ok(n) => n,
                Err(e) if matches!(e.kind(), ErrorKind::Interrupted) => continue,
                Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(()),
                Err(e) => return Err(e),
            };
            let bytes = &buf[..n];
            {
                let mut term = self.term.lock();
                self.parser.lock().unwrap().advance(&mut *term, bytes);
                self.shared.each(|sub| sub.output(self.id, bytes));
            }
            self.scan(n, buf);
            locked += n;
            if locked >= MAX_LOCKED_READ {
                return Ok(());
            }
        }
    }

    /// Watch the bytes just read for OSC 7501 / 9 / 777.
    fn scan(&mut self, n: usize, buf: &[u8]) {
        let mut signals = vec![];
        self.scanner.feed(&buf[..n], |sig| signals.push(sig));
        for sig in signals {
            match &sig {
                OscSignal::ProgramStatusProbe => {
                    // If the tty echoes our reply back as output (echoctl
                    // off), an unthrottled reply would loop forever.
                    let now = Instant::now();
                    if self.last_probe_reply.is_none_or(|t| now.duration_since(t) >= PROBE_REPLY_MIN_INTERVAL) {
                        self.last_probe_reply = Some(now);
                        self.writes.push_back((PROGRAM_STATUS_PROBE_REPLY.into(), 0));
                    }
                },
                OscSignal::Notify { title, body } => self.observer.notify(self.id, title.clone(), body.clone()),
                OscSignal::ColorSchemeReports(on) => {
                    self.scheme_reports = *on;
                    self.check_scheme(Instant::now());
                },
                OscSignal::ColorSchemeQuery => {
                    self.dark = theme_palette().get().is_dark();
                    self.writes.push_back((color_scheme_report(self.dark).into(), 0));
                },
                OscSignal::ProgramStatus(_) => {
                    let mut status = self.shared.status.lock().unwrap();
                    if let Some(new) = next_status(*status, &sig) {
                        let old = std::mem::replace(&mut *status, new);
                        drop(status);
                        self.observer.status(self.id, old, new);
                    }
                },
            }
        }
    }

    /// Tell a program that asked for it when the app's theme turned from
    /// dark to light or back.
    fn check_scheme(&mut self, now: Instant) {
        self.scheme_checked = now;
        let dark = theme_palette().get().is_dark();
        if dark != self.dark {
            self.dark = dark;
            if self.scheme_reports {
                self.writes.push_back((color_scheme_report(dark).into(), 0));
            }
        }
    }

    fn write(&mut self) -> io::Result<()> {
        while let Some((bytes, done)) = self.writes.front_mut() {
            match self.pty.writer().write(&bytes[*done..]) {
                Ok(0) => return Ok(()),
                Ok(n) => {
                    *done += n;
                    if *done == bytes.len() {
                        self.writes.pop_front();
                    }
                },
                Err(e) if matches!(e.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock) => return Ok(()),
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// End the program and the jobs it runs in the foreground: SIGHUP to
    /// their process groups (as the kernel would on a hangup, which can't
    /// come while the UI holds the PTY too), SIGKILL after a grace period.
    /// Whether it ended: a program can be stuck past SIGKILL, and this never
    /// waits for one forever.
    #[cfg(unix)]
    fn hang_up(&self) -> bool {
        let pid = self.pty.child().id() as libc::pid_t;
        // SAFETY: plain syscalls on our own child and its PTY.
        let fg = unsafe { libc::tcgetpgrp(self.pty.file().as_raw_fd()) };
        let signal = |sig| {
            // The child leads its own session, so its group id is its pid.
            unsafe { libc::kill(-pid, sig) };
            if fg > 0 && fg != pid {
                unsafe { libc::kill(-fg, sig) };
            }
        };
        signal(libc::SIGHUP);
        // Stopped jobs only see the hangup once continued.
        signal(libc::SIGCONT);
        // Reap it here, so the Pty's drop doesn't wait forever on a program
        // that ignores SIGHUP.
        let mut deadline = Instant::now() + KILL_GRACE;
        let mut killed = false;
        let mut sink = [0u8; 4096];
        loop {
            // A program on its way out still writes its last output to the
            // tty, and its exit waits until that's read: read it, or it
            // never ends (and the wait below never returns).
            while matches!(self.pty.file().read(&mut sink), Ok(n) if n > 0) {}
            let mut st = 0;
            let r = unsafe { libc::waitpid(pid, &mut st, libc::WNOHANG) };
            if r != 0 {
                // Its foreground job may outlive it.
                if fg > 0 && fg != pid {
                    unsafe { libc::kill(-fg, libc::SIGKILL) };
                }
                return true;
            }
            if Instant::now() >= deadline {
                if killed {
                    return false;
                }
                signal(libc::SIGKILL);
                killed = true;
                deadline = Instant::now() + KILL_GRACE;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The program is gone (`code` = its exit status, if known).
    fn finish(mut self, code: Option<i32>) {
        self.report_exit(code);
    }

    /// Report the end without dropping the PTY, whose drop would wait for a
    /// program that won't end. Its fds and this driver leak.
    #[cfg(unix)]
    fn abandon(mut self) {
        self.report_exit(None);
        std::mem::forget(self);
    }

    fn report_exit(&mut self, code: Option<i32>) {
        let _ = self.pty.deregister(&self.poller);
        {
            let _term = self.term.lock();
            *self.shared.exited.lock().unwrap() = Some(code);
            *self.shared.status.lock().unwrap() = ActivityStatus::None;
            self.emit_marker(&Marker::Exited(code));
        }
        self.observer.exited(self.id, code);
    }
}
