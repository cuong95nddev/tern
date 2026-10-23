//! Frame timing, to judge how smoothly a terminal draws: run with
//! `TERN_FRAME_TRACE=<file>` and each terminal view appends to that file,
//! once a second while it draws, how long the parts of its frames took and
//! how far apart its frames came. Off (no clock reads) without the variable.

use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The trace file, opened on first use; `None` when tracing is off.
fn sink() -> Option<&'static Mutex<File>> {
    static SINK: OnceLock<Option<Mutex<File>>> = OnceLock::new();
    SINK.get_or_init(|| {
        let path = std::env::var_os("TERN_FRAME_TRACE")?;
        File::options().create(true).append(true).open(path).ok().map(Mutex::new)
    })
    .as_ref()
}

pub fn enabled() -> bool {
    sink().is_some()
}

/// One part of a frame, over the frames of a report.
#[derive(Default)]
struct Part {
    total: Duration,
    max: Duration,
}

impl Part {
    fn add(&mut self, d: Duration) {
        self.total += d;
        self.max = self.max.max(d);
    }

    fn line(&self, n: u32) -> String {
        format!("{:.2}/{:.2}", ms(self.total) / n.max(1) as f64, ms(self.max))
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.
}

/// What one view measured since its last report.
#[derive(Default)]
pub struct FrameTrace {
    id: u64,
    since: Option<Instant>,
    frames: u32,
    /// Waiting for the terminal lock, walking the grid with it held,
    /// shaping the text, the whole prepaint, and painting.
    lock_wait: Part,
    walk: Part,
    shape: Part,
    prepaint: Part,
    paint: Part,
    segments: u64,
    /// From the first output after a draw to the paint that shows it.
    latency: Part,
    latency_n: u32,
    /// Between the starts of two paints in a row.
    gaps: Vec<f64>,
    last_paint: Option<Instant>,
    /// The first output not drawn yet.
    output_at: Option<Instant>,
}

/// Paints further apart than this are not a stream of frames.
const IDLE_GAP: Duration = Duration::from_millis(250);

impl FrameTrace {
    pub fn new() -> Option<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        enabled().then(|| Self { id: NEXT.fetch_add(1, Ordering::Relaxed), ..Default::default() })
    }

    pub fn output(&mut self) {
        self.output_at.get_or_insert_with(Instant::now);
    }

    pub fn prepaint(&mut self, lock_wait: Duration, walk: Duration, shape: Duration, total: Duration, segments: usize) {
        self.lock_wait.add(lock_wait);
        self.walk.add(walk);
        self.shape.add(shape);
        self.prepaint.add(total);
        self.segments += segments as u64;
    }

    /// A paint that took `took`, started at `start`.
    pub fn paint(&mut self, start: Instant, took: Duration) {
        self.frames += 1;
        self.paint.add(took);
        if let Some(at) = self.output_at.take() {
            self.latency.add(start - at);
            self.latency_n += 1;
        }
        if let Some(last) = self.last_paint.replace(start) {
            let gap = start - last;
            if gap < IDLE_GAP {
                self.gaps.push(ms(gap));
            }
        }
        let since = *self.since.get_or_insert(start);
        if start - since >= Duration::from_secs(1) {
            self.report(start - since);
            *self = Self { id: self.id, last_paint: self.last_paint, ..Default::default() };
        }
    }

    fn report(&mut self, span: Duration) {
        let Some(sink) = sink() else { return };
        let n = self.frames;
        self.gaps.sort_by(f64::total_cmp);
        let pick = |q: f64| self.gaps.get(((self.gaps.len() as f64 - 1.) * q).round() as usize).copied().unwrap_or(0.);
        let janky = self.gaps.iter().filter(|g| **g > 25.).count();
        let line = format!(
            "view={} fps={:.0} gap p50/p95/max={:.1}/{:.1}/{:.1} >25ms={} | avg/max ms: lock={} walk={} shape={} prepaint={} paint={} latency={} | segs/frame={}\n",
            self.id,
            n as f64 / span.as_secs_f64(),
            pick(0.5),
            pick(0.95),
            self.gaps.last().copied().unwrap_or(0.),
            janky,
            self.lock_wait.line(n),
            self.walk.line(n),
            self.shape.line(n),
            self.prepaint.line(n),
            self.paint.line(n),
            self.latency.line(self.latency_n),
            self.segments / n.max(1) as u64,
        );
        if let Ok(mut f) = sink.lock() {
            let _ = f.write_all(line.as_bytes());
        }
    }
}
