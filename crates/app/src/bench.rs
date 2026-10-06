// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Diagnostic harness for the zoom/scroll blank-frame work (Plan 0001, Phase 2 gate).
//!
//! Two pieces, both inert unless asked for:
//!
//! - a **trace** (`FA_PDF_TRACE=1`) that prints the events which decide whether a viewport change
//!   shows background pixels: the level change itself, the first tile of the new level
//!   (`blank_ms`, the white flash), full coverage (`full_ms`), the hole/pending counts of
//!   every frame (`holes` / `pending` / `unwanted`), and the time an in-flight tile took to notice
//!   it had been superseded (`abort_ms`, Plan 0001 §5 Phase 2).
//! - a **driver** (`--bench-*`) that repeats zoom or pan steps through the same code paths the
//!   toolbar and the wheel use, settles between phases, then prints one summary.
//!
//! `--bench-repeat N` runs the whole scenario `N` times and reports each run's worst beside the
//! min/median/max across runs: one burst on a heavy page leaves `t_blank` a single sample, which
//! is a number, not a distribution. Between runs the viewport returns to where the scenario
//! started, and that reset is not measured.
//!
//! Neither belongs in CI: the driver needs a real window, and every number is machine-specific.
//! Read a run from the trace: the `window` line closes a phase and reports that phase's hole counts
//! and `worst_blank`, and the metrics that matter sit in the window - `blank`/`full` for zoom, hole
//! frames for pan. The first `window run` line is the startup, which on a heavy fixture is the
//! document's own first render rather than anything the zoom did; the summary leaves it out.

use crate::{MainWindow, viewport_center, with_programmatic_guard, with_state};
use pdf_core::{TileKey, TileScheduler, zoom_in_milli};
use slint::{ComponentHandle, Timer, TimerMode};
use std::cell::RefCell;
use std::env;
use std::fmt;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Minimum gap between two `frame` lines whose hole count did not change, in milliseconds.
const REPORT_MS: u128 = 100;

/// Everything a `--bench-*` run needs.
#[derive(Debug, Clone)]
pub struct Cfg {
    /// `+ Zoom` steps, one per tick.
    pub zoom_steps: u32,
    /// Device pixels per scroll step; 0 leaves the pan phase out.
    pub pan_px: f32,
    /// Scroll steps before the settle.
    pub pan_steps: u32,
    /// Tick period of both phases.
    pub step_ms: u64,
    /// Idle time after the last step, so late tiles still count.
    pub settle_ms: u64,
    /// Ceiling for `t_blank` in the verdict.
    pub max_blank_ms: u128,
    /// Times the whole scenario runs; above one, the summary adds each run's worst and the
    /// min/median/max across runs, so a metric with one sample per run still has a distribution.
    pub repeats: u32,
    /// `--cache-mib`: tile cache byte budget, or `None` for the platform default
    /// (`pdf_core::scheduler::default_cache_bytes`: 64 MiB on a desktop, 16 MiB on a phone).
    pub cache_bytes: Option<usize>,
    /// `--cache-rows`: model rows, or `None` for the viewport-derived count.
    ///
    /// The row count is the tile cache's *real* ceiling, because a scheduler never holds more
    /// tiles than it has rows: a window-sized viewport caps the cache near its own tile count (16
    /// rows, so 12 MiB of 512 px RGB8 tiles on a 900x700 window) and the byte budget above that
    /// can never bind. A memory run that needs the budget to bind raises this.
    pub cache_rows: Option<usize>,
}

impl Default for Cfg {
    fn default() -> Self {
        Self {
            zoom_steps: 0,
            pan_px: 0.0,
            pan_steps: 40,
            step_ms: 200,
            settle_ms: 1500,
            max_blank_ms: 50,
            repeats: 1,
            cache_bytes: None,
            cache_rows: None,
        }
    }
}

impl Cfg {
    /// Whether the flags ask for anything at all.
    pub fn is_empty(&self) -> bool {
        self.zoom_steps == 0 && self.pan_px <= 0.0
    }
}

/// Splits the command line into a document path and a bench configuration.
///
/// Flags may appear before or after the path, so `--bench-zoom 4 file.pdf` and
/// `file.pdf --bench-zoom 4` behave the same. Every other argument is the path (the last one
/// wins), which keeps the plain `cargo run -p app -- file.pdf` behaviour.
pub fn parse_args(args: &[String]) -> (Option<String>, Cfg) {
    let mut cfg = Cfg::default();
    let mut path = None;
    let mut i = 1;
    while i < args.len() {
        // Every flag takes one value; a missing or unparseable one falls back to the default.
        let number = args.get(i + 1).and_then(|value| value.parse::<f64>().ok());
        let mut consumed = 1;
        match args[i].as_str() {
            "--bench-zoom" => {
                cfg.zoom_steps = number.unwrap_or(4.0).max(0.0) as u32;
                consumed = 2;
            }
            "--bench-pan" => {
                cfg.pan_px = number.unwrap_or(400.0) as f32;
                consumed = 2;
            }
            "--bench-pan-steps" => {
                cfg.pan_steps = number.unwrap_or(40.0).max(0.0) as u32;
                consumed = 2;
            }
            "--bench-step-ms" => {
                cfg.step_ms = number.unwrap_or(200.0).max(1.0) as u64;
                consumed = 2;
            }
            "--bench-settle-ms" => {
                cfg.settle_ms = number.unwrap_or(1500.0).max(0.0) as u64;
                consumed = 2;
            }
            "--bench-max-blank-ms" => {
                cfg.max_blank_ms = number.unwrap_or(50.0).max(0.0) as u128;
                consumed = 2;
            }
            "--bench-repeat" => {
                cfg.repeats = number.unwrap_or(1.0).max(1.0) as u32;
                consumed = 2;
            }
            // Not step flags: these describe the scheduler the app builds, so they apply with or
            // without a run (trace a manual session with `FA_PDF_TRACE=1`).
            "--cache-mib" => {
                cfg.cache_bytes = number.map(|mib| (mib.max(0.0) * 1024.0 * 1024.0) as usize);
                consumed = 2;
            }
            "--cache-rows" => {
                cfg.cache_rows = number.map(|rows| rows.max(0.0) as usize);
                consumed = 2;
            }
            other => path = Some(other.to_owned()),
        }
        i += consumed;
    }
    (path, cfg)
}

/// Whether `FA_PDF_TRACE` asks for trace output (`FA_PDF_TRACE=0` switches it off again).
pub fn trace_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| match env::var("FA_PDF_TRACE") {
        Ok(value) => value != "0" && !value.eq_ignore_ascii_case("false"),
        Err(_) => false,
    })
}

/// The `--cache-rows` override: the model row count to use instead of the viewport-derived one.
///
/// Set once from the command line before the first model exists, and read where the model is
/// sized, so it also covers the rebuild `ensure_capacity` does on a zoom. The row count is the
/// tile cache's real ceiling, which is why a memory run that needs the *byte* budget to bind has
/// to raise it (`docs/benchmarks.md` §7.6).
static CACHE_ROWS: OnceLock<usize> = OnceLock::new();

/// Records the `--cache-rows` override; the command line can only say it once, so a second call is
/// ignored.
pub fn set_cache_rows(rows: usize) {
    let _ = CACHE_ROWS.set(rows);
}

/// The `--cache-rows` override, if the command line asked for one.
pub fn cache_rows() -> Option<usize> {
    CACHE_ROWS.get().copied()
}

/// Cadence of the `rss` trace line, in milliseconds.
///
/// Slow enough to stay out of a 16 ms burst's way, fast enough to catch a plateau: the memory
/// question (`docs/benchmarks.md` §7.6) is about the shape of a twenty-second run, not one frame.
const RSS_MS: u128 = 250;

/// The process memory `GetProcessMemoryInfo` reports, in bytes.
#[derive(Debug, Clone, Copy)]
struct Mem {
    /// Resident bytes (`WorkingSetSize`): what a task manager shows as the process's memory.
    ws: usize,
    /// `PrivateUsage`: the process's committed private bytes, which is the part a tile cache can
    /// grow. It tracks the cache's own bytes closely, where `ws` is what is resident *now*.
    private: usize,
    /// High-water mark of `ws` since the process started (`PeakWorkingSetSize`). It is monotonic,
    /// so the last sample carries the process's true peak: a sampler ticking four times a second
    /// can step over a spike, this cannot.
    peak_ws: usize,
}

/// The process memory counters, hand-bound rather than pulled in as a dependency: one call, one
/// struct, nine `SIZE_T`s.
#[cfg(windows)]
mod memory {
    use std::ffi::c_void;
    use std::mem::size_of;

    /// `PROCESS_MEMORY_COUNTERS_EX` as `psapi.h` declares it. `#[repr(C)]` inserts the four bytes
    /// of padding the C struct has after `PageFaultCount`, and the test below pins the size,
    /// because a wrong `cb` fails the call while a wrong field order succeeds with wrong numbers.
    #[repr(C)]
    #[derive(Default)]
    struct Counters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
        private_usage: usize,
    }

    // The kernel32 entry point behind psapi's `GetProcessMemoryInfo`, so nothing extra is linked.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn K32GetProcessMemoryInfo(process: *mut c_void, counters: *mut Counters, cb: u32) -> i32;
        fn GetCurrentProcess() -> *mut c_void;
    }

    /// The current process's counters, or `None` when the call fails.
    pub(super) fn read() -> Option<super::Mem> {
        let mut counters = Counters {
            cb: size_of::<Counters>() as u32,
            ..Counters::default()
        };
        // SAFETY: `GetCurrentProcess` is a pseudo-handle - always valid, never closed - and
        // `counters` is a live `Counters` whose `cb` field says how large it is.
        let ok =
            unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
        (ok != 0).then_some(super::Mem {
            ws: counters.working_set_size,
            private: counters.private_usage,
            peak_ws: counters.peak_working_set_size,
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The layout is the API's, not merely plausible.
        #[test]
        fn the_counters_struct_matches_the_api() {
            assert_eq!(size_of::<Counters>(), 8 + 9 * size_of::<usize>());
        }

        /// The call works on this process and its numbers land in the fields the names promise.
        #[test]
        fn the_process_memory_is_readable() {
            let mem = read().expect("the counters of the current process");
            assert!(mem.ws > 0, "a running process has resident bytes");
            assert!(mem.private > 0, "and committed private bytes");
            assert!(mem.peak_ws >= mem.ws, "the peak is a high-water mark");
        }
    }
}

/// Reads the process's memory counters, or `None` where the platform has no implementation.
#[cfg(windows)]
fn process_memory() -> Option<Mem> {
    memory::read()
}

/// No counters off Windows: the `rss` lines carry `-` and the summary says `n/a`.
#[cfg(not(windows))]
fn process_memory() -> Option<Mem> {
    None
}

/// Bytes as mebibytes, the unit every memory line here is read in.
fn mib(bytes: u128) -> f32 {
    bytes as f32 / (1024.0 * 1024.0)
}

/// One counter as `87.2MiB`, or `-` when the platform has no counters.
fn mem_str(bytes: Option<usize>) -> String {
    bytes.map_or_else(
        || "-".to_owned(),
        |bytes| format!("{:.1}MiB", mib(bytes as u128)),
    )
}

/// `min/med/max` of one [`Mem`] field across samples, in MiB.
fn spread_mib(samples: &[Mem], pick: fn(&Mem) -> usize) -> String {
    let values: Vec<u128> = samples.iter().map(|mem| pick(mem) as u128).collect();
    let (min, med, max) = spread(&values);
    format!("{:.1}/{:.1}/{:.1}", mib(min), mib(med), mib(max))
}

/// Ticks in one `settle_ms` interval, so a later phase does not inherit a backlog.
fn settle_ticks(cfg: &Cfg) -> u32 {
    (cfg.settle_ms / cfg.step_ms.max(1)).max(1) as u32
}

/// Number of visible cells with no cached tile: exactly the pixels the user sees as background.
///
/// Visible cells only: the prefetch ring is requested ahead of the viewport but is not something
/// the user is waiting for, so it must not read as a hole.
fn holes(sched: &TileScheduler, visible: &[TileKey]) -> u32 {
    visible.iter().filter(|key| !sched.contains(key)).count() as u32
}

/// `(min, median, max)` of a metric, all zero when nothing was measured.
fn spread(values: &[u128]) -> (u128, u128, u128) {
    if values.is_empty() {
        return (0, 0, 0);
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    (
        sorted[0],
        sorted[sorted.len() / 2],
        sorted[sorted.len() - 1],
    )
}

/// The counters only the trace sees; the summary prints them all.
///
/// `blank_ms`/`full_ms`/`abort_ms`/`rebuilds` are cumulative, because a level change is a single
/// event that must not disappear when a phase window rotates. The exception is the startup `run`
/// window, whose samples belong to the document's own first render; [`Trace::phase`] drops those.
/// The rest belong to one phase at a time.
#[derive(Debug, Default)]
struct Stats {
    /// Time from the level change to the first new-level tile, one entry per change.
    blank_ms: Vec<u128>,
    /// Time from the level change to a fully covered viewport, one entry per change.
    full_ms: Vec<u128>,
    /// Time from an abort signal to the rasterizer noticing it, one entry per aborted tile.
    abort_ms: Vec<u128>,
    /// Runs of consecutive frames that had at least one hole.
    hole_episodes: u32,
    /// Frames that had at least one hole.
    hole_frames: u32,
    /// Worst hole count seen in one frame.
    max_holes: u32,
    /// Worst in-flight request count.
    max_pending: u32,
    /// Worst count of in-flight requests the viewport no longer wants.
    max_unwanted: u32,
    /// Model rebuilds (window or device-scale-factor changes).
    rebuilds: u32,
}

impl Stats {
    /// The per-phase counters, as one line of `name=value` pairs.
    fn window(&self) -> String {
        let worst_blank = self.blank_ms.iter().copied().max().unwrap_or(0);
        format!(
            "holes episodes={} frames={} worst_frame={} worst_pending={} worst_unwanted={} \
             worst_blank={worst_blank}",
            self.hole_episodes,
            self.hole_frames,
            self.max_holes,
            self.max_pending,
            self.max_unwanted
        )
    }

    /// Starts a new phase window.
    fn reset_window(&mut self) {
        self.hole_episodes = 0;
        self.hole_frames = 0;
        self.max_holes = 0;
        self.max_pending = 0;
        self.max_unwanted = 0;
    }
}

/// One level change whose first new-level tile has not arrived yet.
struct Wait {
    target_milli: u32,
    started: Instant,
    blank_logged: bool,
}

/// One repeat of the scenario, as the summary aggregates it.
///
/// A repeat is a whole `--bench-repeat` iteration, not a level change: the value of each metric
/// here is the worst that repeat saw, so min/median/max over runs describes the run, while the
/// metric lines of the summary describe one run's samples.
#[derive(Debug, Clone, Copy, Default)]
struct Run {
    /// Worst `t_blank` of the run, or `None` when no level change landed in it.
    blank_max: Option<u128>,
    /// Worst `t_full` of the run.
    full_max: Option<u128>,
    /// Worst `t_abort` of the run.
    abort_max: Option<u128>,
    /// Model rebuilds during the run.
    rebuilds: u32,
}

/// The opt-in trace. Every method but `new` is one `bool` check when tracing is off.
pub struct Trace {
    on: bool,
    started: Instant,
    wait: Option<Wait>,
    last_report: Instant,
    last_holes: u32,
    /// Label of the phase window being collected; see [`Trace::phase`].
    window_name: String,
    stats: Stats,
    /// Completed repeats, oldest first; the repeat in progress is appended at report time.
    runs: Vec<Run>,
    /// Set while a repeat reset moves the viewport back to where the scenario started: that level
    /// change belongs to no run, so nothing it causes is recorded.
    quiet: bool,
    /// One entry per `rss` line: the process's memory beside the cache that produced it.
    ///
    /// Unlike [`Stats`], this outlives a repeat boundary. The plateau is a property of the
    /// process, and the reset between two runs is a few frames of allocation churn rather than a
    /// new baseline, so sampling through it is what makes the shape of a long run visible.
    memory: Vec<Mem>,
    /// When the last sample was taken; the timer's period is the cadence, this is the guard.
    last_rss: Instant,
}

impl Trace {
    /// Creates a trace that is either silent or printing to stdout.
    pub fn new(on: bool) -> Self {
        Self {
            on,
            started: Instant::now(),
            wait: None,
            last_report: Instant::now(),
            last_holes: 0,
            window_name: "run".to_owned(),
            stats: Stats::default(),
            runs: Vec::new(),
            quiet: false,
            memory: Vec::new(),
            last_rss: Instant::now(),
        }
    }

    /// Milliseconds since the process started: the timestamp on every line.
    fn t_ms(&self) -> u128 {
        self.started.elapsed().as_millis()
    }

    fn print(&self, args: fmt::Arguments) {
        println!("[bench] {args}");
    }

    /// A zoom, page change or document open: from here on, the expected image is the new level.
    pub fn level_changed(&mut self, target_milli: u32, reason: &str) {
        if !self.on || self.quiet {
            return;
        }
        self.wait = Some(Wait {
            target_milli,
            started: Instant::now(),
            blank_logged: false,
        });
        let t = self.t_ms();
        self.print(format_args!(
            "level t={t}ms scale={target_milli} ({reason})"
        ));
    }

    /// A tile arrived: the first new-level one ends `t_blank`, full coverage ends `t_full`.
    pub fn tile(&mut self, sched: &TileScheduler, visible: &[TileKey]) {
        if !self.on || self.quiet || self.wait.is_none() {
            return;
        }
        let elapsed = self
            .wait
            .as_ref()
            .map_or(0, |w| w.started.elapsed().as_millis());
        let target = self.wait.as_ref().map_or(0, |w| w.target_milli);
        let first = self.wait.as_ref().is_some_and(|w| !w.blank_logged);
        let covered = holes(sched, visible) == 0;

        if first {
            if let Some(wait) = self.wait.as_mut() {
                wait.blank_logged = true;
            }
            self.stats.blank_ms.push(elapsed);
            let t = self.t_ms();
            self.print(format_args!("blank t={t}ms {elapsed} scale={target}"));
        }
        if covered {
            self.stats.full_ms.push(elapsed);
            self.wait = None;
            let t = self.t_ms();
            self.print(format_args!("full t={t}ms {elapsed} scale={target}"));
        }
    }

    /// A tile was aborted mid-raster because a later level change superseded it: `ms` is how long
    /// the rasterizer took to notice, which is the latency the Phase 2 gate reports.
    pub fn aborted(&mut self, ms: u64) {
        if !self.on || self.quiet {
            return;
        }
        self.stats.abort_ms.push(ms as u128);
        let t = self.t_ms();
        self.print(format_args!("abort t={t}ms {ms}"));
    }

    /// One viewport diff: the hole count is what the user sees while panning.
    ///
    /// `unwanted` is the opposite side of the same coin: work in flight that is neither on screen
    /// nor part of the ring the app asked for, i.e. raster time spent on a viewport the user has
    /// already left. The ring is excluded because it is wanted work - it is exactly what a pan
    /// step consumes.
    pub fn frame(&mut self, sched: &TileScheduler, visible: &[TileKey], ring: &[TileKey]) {
        if !self.on || self.quiet {
            return;
        }
        let holes = holes(sched, visible);
        let pending = sched.pending_len() as u32;
        let unwanted = sched
            .pending_keys()
            .filter(|key| !visible.contains(key) && !ring.contains(key))
            .count() as u32;

        if holes > 0 {
            if self.last_holes == 0 {
                self.stats.hole_episodes += 1;
            }
            self.stats.hole_frames += 1;
        }
        self.stats.max_holes = self.stats.max_holes.max(holes);
        self.stats.max_pending = self.stats.max_pending.max(pending);
        self.stats.max_unwanted = self.stats.max_unwanted.max(unwanted);

        let entered = holes > 0 && self.last_holes == 0;
        let left = holes == 0 && self.last_holes > 0;
        self.last_holes = holes;

        // Throttled, so a 60 Hz wheel animation reads as a burst instead of 60 lines a second -
        // but a transition into or out of a hole is never dropped.
        let due = self.last_report.elapsed().as_millis() >= REPORT_MS;
        if entered || left || (holes > 0 && due) {
            self.last_report = Instant::now();
            let t = self.t_ms();
            let cache_mib = mib(sched.cache_bytes() as u128);
            self.print(format_args!(
                "frame t={t}ms scale={} holes={holes} pending={pending} unwanted={unwanted} \
                 cache={cache_mib:.1}MiB/{} rows dropped={}",
                sched.scale_milli(),
                sched.cache_len(),
                sched.dropped()
            ));
        }
    }

    /// One memory sample: the process's resident and private bytes beside the cache that produced
    /// them, on the same timeline as the level changes, the aborts and the frames.
    ///
    /// Two counters because they answer different questions. `private` is the process's committed
    /// private bytes, which is what a tile cache grows, so its plateau is the cache's; `ws` is how
    /// much of that stayed resident. Both are logged with the cache's own bytes so the three can
    /// be read against each other rather than guessed at.
    ///
    /// Throttled on top of the timer's period: after a long stall (a display-list build blocks the
    /// event loop for over a second) several ticks can arrive together, and duplicated lines would
    /// read as a plateau that is not there.
    pub fn sample_memory(&mut self, sched: &TileScheduler) {
        if !self.on || self.last_rss.elapsed().as_millis() < RSS_MS {
            return;
        }
        self.last_rss = Instant::now();
        let mem = process_memory();
        if let Some(mem) = mem {
            self.memory.push(mem);
        }
        let t = self.t_ms();
        let cache_mib = mib(sched.cache_bytes() as u128);
        self.print(format_args!(
            "rss t={t}ms ws={} private={} cache={cache_mib:.1}MiB/{} rows dropped={} evicted={}",
            mem_str(mem.map(|mem| mem.ws)),
            mem_str(mem.map(|mem| mem.private)),
            sched.cache_len(),
            sched.dropped(),
            sched.evicted()
        ));
    }

    /// The model grew: every row was rebuilt, so a hole burst here is expected, not a leak.
    pub fn model_rebuilt(&mut self, capacity: usize) {
        if !self.on {
            return;
        }
        self.stats.rebuilds += 1;
        let t = self.t_ms();
        self.print(format_args!("rebuild t={t}ms capacity={capacity}"));
    }

    /// The repeat in progress, as a [`Run`]: the worst of each metric measured so far.
    fn current(&self) -> Run {
        Run {
            blank_max: self.stats.blank_ms.iter().copied().max(),
            full_max: self.stats.full_ms.iter().copied().max(),
            abort_max: self.stats.abort_ms.iter().copied().max(),
            rebuilds: self.stats.rebuilds,
        }
    }

    /// Ends the repeat in progress, so the next one starts from empty counters.
    pub fn close_run(&mut self) {
        let run = self.current();
        self.runs.push(run);
        self.stats = Stats::default();
        self.wait = None;
        self.last_holes = 0;
    }

    /// Every repeat in order, the one in progress last.
    fn per_run(&self) -> Vec<Run> {
        let mut all = self.runs.clone();
        all.push(self.current());
        all
    }

    /// Stops recording: the reset between two repeats is not part of either run.
    pub fn suspend(&mut self) {
        self.quiet = true;
    }

    /// Starts recording again, forgetting the level change the reset raised.
    pub fn resume(&mut self) {
        self.quiet = false;
        self.wait = None;
    }

    /// Opens a phase window: prints what the previous one accumulated and starts a new one.
    ///
    /// The closing `run` window is the process startup: it carries the document's own first
    /// render, which costs over a second on the A0 and belongs to §7.3, not to the zoom the run
    /// measures. Its `blank`/`full` samples are dropped with it, and its `worst_blank` stays in
    /// the line above. Later windows keep their samples, so a pan phase cannot hide a zoom burst.
    pub fn phase(&mut self, name: &str) {
        if !self.on {
            return;
        }
        let t = self.t_ms();
        let window = self.stats.window();
        let closed = std::mem::replace(&mut self.window_name, name.to_owned());
        self.print(format_args!("window {closed} t={t}ms {window}"));
        self.stats.reset_window();
        if closed == "run" {
            self.stats.blank_ms.clear();
            self.stats.full_ms.clear();
        }
        // A window can start or end inside a hole run; the first hole of the next window is a new
        // episode by definition, so the episode count never carries over.
        self.last_holes = 0;
    }

    /// One pan step: the current content offset, how far it can still go, and what was written.
    pub fn pan_step(&self, offset_x: f32, reach: f32, page_w: f32, view_w: f32, next: f32) {
        if !self.on {
            return;
        }
        let t = self.t_ms();
        self.print(format_args!(
            "pan t={t}ms offset_x={offset_x:.0} reach={reach:.0} page_w={page_w:.0} \
             view_w={view_w:.0} next={next:.0}"
        ));
    }

    /// Prints the run summary and returns whether `t_blank` stayed under its ceiling.
    pub fn report(&self, cfg: &Cfg, sched: &TileScheduler, rendered: i32, requested: i32) -> bool {
        let (b_min, b_med, b_max) = spread(&self.stats.blank_ms);
        let (f_min, f_med, f_max) = spread(&self.stats.full_ms);
        let cache_mib = mib(sched.cache_bytes() as u128);
        let runs = self.per_run();

        println!("[bench] --- summary ---");
        println!(
            "[bench] t_blank_ms  min={b_min} med={b_med} max={b_max} n={}",
            self.stats.blank_ms.len()
        );
        println!(
            "[bench] t_full_ms   min={f_min} med={f_med} max={f_max} n={}",
            self.stats.full_ms.len()
        );
        let (a_min, a_med, a_max) = spread(&self.stats.abort_ms);
        println!(
            "[bench] t_abort_ms  min={a_min} med={a_med} max={a_max} n={}",
            self.stats.abort_ms.len()
        );
        println!(
            "[bench] window {:<6} {}",
            self.window_name,
            self.stats.window()
        );
        println!(
            "[bench] cache       {cache_mib:.1}MiB in {} of {} rows (budget {:.1}MiB) dropped={} \
             evicted={}",
            sched.cache_len(),
            sched.capacity(),
            mib(sched.cache_max_bytes() as u128),
            sched.dropped(),
            sched.evicted()
        );
        println!(
            "[bench] tiles       rendered={rendered} requested={requested} rebuilds={}",
            runs.iter().map(|run| run.rebuilds).sum::<u32>()
        );
        // The memory summary is the process's, not a run's: samples accumulate across repeats.
        if self.memory.is_empty() {
            println!("[bench] rss_mib     n/a - no memory samples (Windows only, and 250 ms in)");
        } else {
            println!(
                "[bench] rss_mib     ws {} | private {} | peak_ws {:.1} (min/med/max, n={})",
                spread_mib(&self.memory, |mem| mem.ws),
                spread_mib(&self.memory, |mem| mem.private),
                mib(self.memory.last().map_or(0, |mem| mem.peak_ws) as u128),
                self.memory.len()
            );
        }

        // Across repeats: each metric's worst per run, so `--bench-repeat` reports a distribution
        // where a single run has one sample. A single run has nothing to add: the lines above are
        // its numbers.
        if runs.len() > 1 {
            let listed = |values: &[Option<u128>]| {
                values
                    .iter()
                    .map(|value| value.map_or_else(|| "-".to_owned(), |ms| ms.to_string()))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            let spread_over_runs = |values: &[Option<u128>]| {
                spread(&values.iter().flatten().copied().collect::<Vec<_>>())
            };
            let blanks: Vec<Option<u128>> = runs.iter().map(|run| run.blank_max).collect();
            let fulls: Vec<Option<u128>> = runs.iter().map(|run| run.full_max).collect();
            let aborts: Vec<Option<u128>> = runs.iter().map(|run| run.abort_max).collect();
            println!(
                "[bench] per-run     t_blank {} | t_full {} | t_abort {} | rebuilds {}",
                listed(&blanks),
                listed(&fulls),
                listed(&aborts),
                runs.iter()
                    .map(|run| run.rebuilds.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            );
            let (b0, b1, b2) = spread_over_runs(&blanks);
            let (f0, f1, f2) = spread_over_runs(&fulls);
            let (a0, a1, a2) = spread_over_runs(&aborts);
            println!(
                "[bench] across      {} runs: t_blank {b0}/{b1}/{b2} | t_full {f0}/{f1}/{f2} | \
                 t_abort {a0}/{a1}/{a2} (min/med/max of each run's worst)",
                runs.len()
            );
        }

        let worst_blank = runs.iter().filter_map(|run| run.blank_max).max();
        let Some(worst_blank) = worst_blank else {
            println!("[bench] VERDICT     n/a - no level change was measured");
            return true;
        };
        let pass = worst_blank <= cfg.max_blank_ms;
        println!(
            "[bench] VERDICT     {} - worst t_blank {worst_blank} ms vs {} ms ceiling",
            if pass { "PASS" } else { "FAIL" },
            cfg.max_blank_ms
        );
        pass
    }
}

/// Which phase of a `--bench-*` run the timer is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// `+ Zoom` steps.
    Zoom,
    /// Rightward scroll steps, up to the page edge.
    Pan,
    /// Idle time, so the previous phase's tiles arrive before the next phase or the summary.
    Settle,
}

/// One run in progress, driven by the repeating timer on the UI thread.
struct Driver {
    cfg: Cfg,
    phase: Phase,
    /// Steps done in the current phase.
    step: u32,
    /// Ticks left in the settle phase.
    settle_ticks: u32,
    /// Whether the zoom phase still has to run. A run opens with a settle so the first zoom step
    /// does not land inside the document's own first tile, which on a dense page takes over a
    /// second; `--bench-settle-ms 0` gets that case back.
    zoom_pending: bool,
    /// Whether the pan phase still has to run after the settle.
    pan_pending: bool,
    /// Repeats still to run, including the one in progress.
    runs_left: u32,
    /// Viewport at the scenario's start, `(scale_milli, view_x, view_y)`: recorded on the first
    /// step and restored before every repeat, so each run measures the same scenario.
    home: Option<(u32, f32, f32)>,
    /// A repeat boundary is settling after that restore; the run starts when it ends.
    resetting: bool,
}

impl Driver {
    /// Records the scenario's starting viewport once, so a repeat can return to it.
    fn remember_home(&mut self, ui: &MainWindow) {
        if self.home.is_some() || self.cfg.repeats <= 1 {
            return;
        }
        let mut home = None;
        with_state(|state| home = Some((state.scale_milli, ui.get_view_x(), ui.get_view_y())));
        self.home = home;
    }

    /// Ends the run in progress and puts the viewport back where the scenario started, so the next
    /// repeat measures the same thing: same scale, same offset, same steps.
    ///
    /// The reset raises a level change of its own, which would land in the next run's `t_blank`
    /// and in its abort list; the trace is suspended across it and resumes when the settle that
    /// follows has drained it.
    fn restart(&mut self, ui: &MainWindow) {
        with_state(|state| state.bench.close_run());
        self.step = 0;
        self.zoom_pending = self.cfg.zoom_steps > 0;
        self.pan_pending = self.cfg.pan_px > 0.0 && self.cfg.pan_steps > 0;
        self.phase = Phase::Settle;
        self.settle_ticks = settle_ticks(&self.cfg);
        let Some((scale_milli, view_x, view_y)) = self.home else {
            return;
        };
        with_state(|state| state.bench.suspend());
        self.resetting = true;
        // The cached tiles go with the viewport: a repeat that inherited the previous run's cache
        // would find every level already rendered and measure nothing at all.
        with_state(|state| state.release_all(ui));
        let (anchor_x, anchor_y) = viewport_center(ui);
        with_state(|state| state.zoom(ui, scale_milli, anchor_x, anchor_y));
        with_programmatic_guard(|| {
            ui.set_scroll_x(view_x);
            ui.set_scroll_y(view_y);
        });
    }

    /// Runs one tick and returns whether the run is over.
    fn tick(&mut self, ui: &MainWindow) -> bool {
        match self.phase {
            Phase::Zoom => {
                if self.step == 0 {
                    // The startup window ends here: from now on the counters describe the zoom.
                    with_state(|state| state.bench.phase("zoom"));
                }
                self.remember_home(ui);
                // The same call the `+ Zoom` button makes, at the viewport centre.
                let (anchor_x, anchor_y) = viewport_center(ui);
                with_state(|state| {
                    let next = zoom_in_milli(state.scale_milli);
                    state.zoom(ui, next, anchor_x, anchor_y);
                });
                self.step += 1;
                if self.step >= self.cfg.zoom_steps {
                    self.step = 0;
                    self.pan_pending = self.cfg.pan_px > 0.0 && self.cfg.pan_steps > 0;
                    self.phase = Phase::Settle;
                    self.settle_ticks = settle_ticks(&self.cfg);
                }
                false
            }
            Phase::Pan => {
                self.remember_home(ui);
                // The wheel writes `scroll-x`, which is `content-x`: the offset is its negation,
                // so panning right means subtracting from it. Clamped to the page edge, like
                // `zoom`. `view-x` mirrors the Flickable's own position, so the next step starts
                // from where the viewport really is, not from where the last write asked it to go.
                let sf = ui.window().scale_factor();
                let reach = (ui.get_page_w() - ui.get_view_w()).max(0.0);
                let next = (ui.get_view_x() - self.cfg.pan_px / sf).max(-reach);
                with_state(|state| {
                    state.bench.pan_step(
                        -ui.get_view_x(),
                        reach,
                        ui.get_page_w(),
                        ui.get_view_w(),
                        next,
                    )
                });
                ui.set_scroll_x(next);
                self.step += 1;
                if self.step >= self.cfg.pan_steps || next <= -reach {
                    self.step = 0;
                    self.phase = Phase::Settle;
                    self.settle_ticks = settle_ticks(&self.cfg);
                }
                false
            }
            Phase::Settle => {
                self.settle_ticks = self.settle_ticks.saturating_sub(1);
                if self.settle_ticks > 0 {
                    return false;
                }
                if self.resetting {
                    // The reset has drained: this run is measured from here. Consuming the pending
                    // flag here as well is what keeps a run to one zoom phase: without it the
                    // settle after the steps would start the same phase again.
                    self.resetting = false;
                    with_state(|state| state.bench.resume());
                    if self.zoom_pending {
                        self.zoom_pending = false;
                        self.phase = Phase::Zoom;
                    } else {
                        // A pan-only scenario opens on its pan phase, exactly as run one does.
                        self.pan_pending = false;
                        self.phase = Phase::Pan;
                    }
                    return false;
                }
                if self.zoom_pending {
                    // The document has had its settle: the zoom numbers from here are the zoom's.
                    self.zoom_pending = false;
                    self.phase = Phase::Zoom;
                    return false;
                }
                if self.pan_pending {
                    // The zoom burst has drained: panning now measures the pan path, not the
                    // zoom backlog. The marker splits the two in the trace.
                    self.pan_pending = false;
                    with_state(|state| state.bench.phase("pan"));
                    self.phase = Phase::Pan;
                    return false;
                }
                if self.runs_left > 1 {
                    // Another repeat of the same scenario: this one's counters close before the
                    // reset, which is why the reset is not measured.
                    self.runs_left -= 1;
                    self.restart(ui);
                    return false;
                }
                true
            }
        }
    }
}

thread_local! {
    /// The run in progress, if any.
    static DRIVER: RefCell<Option<Driver>> = const { RefCell::new(None) };
    /// Keeps the bench timer alive: dropping it would stop the run.
    static TIMER: RefCell<Option<Timer>> = const { RefCell::new(None) };
    /// Keeps the memory sampler alive; it runs on its own period, independent of any run.
    static RSS_TIMER: RefCell<Option<Timer>> = const { RefCell::new(None) };
}

/// Starts the memory sampler: one `rss` line every [`RSS_MS`], for as long as the process runs.
///
/// Started from the trace flag rather than from a run, because the resident set *before* anything
/// is open is the baseline a plateau has to be read against, and because a manual
/// `FA_PDF_TRACE=1` session then gets the same timeline as a scripted one. Diagnostic only: it
/// keeps the event loop waking four times a second.
pub fn start_sampler() {
    let timer = Timer::default();
    timer.start(
        TimerMode::Repeated,
        Duration::from_millis(RSS_MS as u64),
        move || with_state(|state| state.bench.sample_memory(&state.scheduler)),
    );
    RSS_TIMER.with(|cell| *cell.borrow_mut() = Some(timer));
}

/// Starts a `--bench-*` run: a settle, the zoom steps, a settle, the pan steps, a settle, the
/// summary.
///
/// Every phase drives a Rust entry point the UI itself uses (`zoom` for `+ Zoom`, a `scroll-x`
/// write for the wheel), so the numbers describe the shipped path rather than a copy of it.
/// The settle between phases matters: without it the pan phase would inherit the zoom burst's
/// backlog, and both would be measured as one, and the opening settle keeps the first zoom step
/// out of the document's own first tile. The process exits from the last settle tick, which is
/// what makes the run scriptable.
pub fn start(ui: &MainWindow, cfg: Cfg) {
    if cfg.is_empty() {
        println!("[bench] nothing to do: give --bench-zoom and/or --bench-pan");
        return;
    }
    println!(
        "[bench] run zoom_steps={} pan_px={} pan_steps={} step_ms={} settle_ms={} \
         max_blank_ms={} repeats={} cache={} rows={}",
        cfg.zoom_steps,
        cfg.pan_px,
        cfg.pan_steps,
        cfg.step_ms,
        cfg.settle_ms,
        cfg.max_blank_ms,
        cfg.repeats,
        cfg.cache_bytes.map_or_else(
            || "default".to_owned(),
            |bytes| format!("{:.1}MiB", mib(bytes as u128))
        ),
        cfg.cache_rows
            .map_or_else(|| "auto".to_owned(), |rows| rows.to_string())
    );

    let phase = if cfg.zoom_steps > 0 {
        Phase::Settle
    } else {
        Phase::Pan
    };
    DRIVER.with(|cell| {
        *cell.borrow_mut() = Some(Driver {
            cfg: cfg.clone(),
            phase,
            step: 0,
            settle_ticks: settle_ticks(&cfg),
            zoom_pending: cfg.zoom_steps > 0,
            pan_pending: false,
            runs_left: cfg.repeats.max(1),
            home: None,
            resetting: false,
        });
    });

    let timer = Timer::default();
    let weak = ui.as_weak();
    timer.start(
        TimerMode::Repeated,
        Duration::from_millis(cfg.step_ms),
        move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let done = DRIVER.with(|cell| {
                cell.borrow_mut()
                    .as_mut()
                    .is_some_and(|driver| driver.tick(&ui))
            });
            if !done {
                return;
            }

            // Stop the run before reporting, so no further action can pollute the numbers.
            let cfg = DRIVER.with(|cell| cell.borrow_mut().take().map(|driver| driver.cfg));
            TIMER.with(|cell| *cell.borrow_mut() = None);
            let Some(cfg) = cfg else {
                return;
            };
            let pass = crate::APP.with(|cell| {
                cell.borrow_mut().as_mut().is_some_and(|state| {
                    state.bench.report(
                        &cfg,
                        &state.scheduler,
                        ui.get_tiles_rendered(),
                        ui.get_tiles_requested(),
                    )
                })
            });
            std::process::exit(if pass { 0 } else { 1 });
        },
    );
    TIMER.with(|cell| *cell.borrow_mut() = Some(timer));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The key of one cell nobody has rendered.
    fn cell(scale_milli: u32) -> TileKey {
        TileKey {
            page: 0,
            col: 0,
            row: 0,
            scale_milli,
        }
    }

    /// A repeat closes the run before it, and the reset between two repeats is not measured.
    ///
    /// This is the invariant behind `--bench-repeat`: without it the reset's own level change
    /// would be the next run's `t_blank`, and its aborts the next run's `t_abort`.
    #[test]
    fn a_repeat_closes_the_run_before_it_and_ignores_the_reset() {
        let sched = TileScheduler::new(4, 1024 * 1024);
        let visible = [cell(1_000)];
        let mut trace = Trace::new(true);

        // Run one: a level change whose first new-level tile arrives.
        trace.level_changed(1_000, "zoom");
        trace.tile(&sched, &visible);
        trace.aborted(4);
        assert_eq!(trace.stats.blank_ms.len(), 1);
        trace.close_run();
        assert_eq!(trace.runs.len(), 1);
        assert!(trace.runs[0].blank_max.is_some());
        assert_eq!(trace.runs[0].abort_max, Some(4));
        assert!(trace.stats.blank_ms.is_empty());
        assert!(trace.stats.abort_ms.is_empty());

        // The reset between repeats: a level change, tiles, aborts and frames, none of them part
        // of either run.
        trace.suspend();
        trace.level_changed(2_000, "zoom");
        trace.tile(&sched, &visible);
        trace.aborted(9);
        trace.frame(&sched, &visible, &[]);
        trace.resume();
        assert!(trace.stats.blank_ms.is_empty());
        assert!(trace.stats.abort_ms.is_empty());
        assert_eq!(trace.stats.hole_frames, 0);

        // Run two measures again, and the summary spans both runs.
        trace.level_changed(1_000, "zoom");
        trace.tile(&sched, &visible);
        assert_eq!(trace.stats.blank_ms.len(), 1);
        let runs = trace.per_run();
        assert_eq!(runs.len(), 2);
        assert!(runs.iter().all(|run| run.blank_max.is_some()));
        assert_eq!(runs[0].abort_max, Some(4));
        assert_eq!(runs[1].abort_max, None);
        assert!(trace.report(&Cfg::default(), &sched, 0, 0));
    }

    /// A command line for `parse_args`: the program, a document path, then the flags.
    fn args(rest: &[&str]) -> Vec<String> {
        ["app.exe", "doc.pdf"]
            .iter()
            .chain(rest)
            .map(|arg| (*arg).to_owned())
            .collect()
    }

    /// `--bench-repeat` reaches the config, and a missing value leaves one run.
    #[test]
    fn repeat_flag_is_parsed() {
        let (path, cfg) = parse_args(&args(&["--bench-zoom", "4", "--bench-repeat", "5"]));
        assert_eq!(path.as_deref(), Some("doc.pdf"));
        assert_eq!(cfg.repeats, 5);
        assert_eq!(cfg.zoom_steps, 4);

        let (_, cfg) = parse_args(&args(&["--bench-zoom", "4"]));
        assert_eq!(cfg.repeats, 1);
        // A zero or negative repeat is still one run, so the summary never reads a missing run.
        let (_, cfg) = parse_args(&args(&["--bench-zoom", "4", "--bench-repeat", "0"]));
        assert_eq!(cfg.repeats, 1);
    }

    /// The two memory knobs reach the config, and neither one starts a run on its own.
    #[test]
    fn cache_flags_are_parsed() {
        let (_, cfg) = parse_args(&args(&["--cache-mib", "8", "--cache-rows", "80"]));
        assert_eq!(cfg.cache_bytes, Some(8 * 1024 * 1024));
        assert_eq!(cfg.cache_rows, Some(80));
        // MiB, and a fraction is a byte count rather than a rounded tile count: half is 512 KiB.
        let (_, cfg) = parse_args(&args(&["--cache-mib", "0.5", "--bench-zoom", "1"]));
        assert_eq!(cfg.cache_bytes, Some(512 * 1024));
        assert_eq!(cfg.zoom_steps, 1);

        // Absent, or present without a value, leaves the defaults: the platform budget and the
        // rows the viewport asks for.
        let (_, cfg) = parse_args(&args(&["--bench-zoom", "1"]));
        assert_eq!((cfg.cache_bytes, cfg.cache_rows), (None, None));
        let (_, cfg) = parse_args(&args(&["--cache-mib"]));
        assert_eq!((cfg.cache_bytes, cfg.cache_rows), (None, None));
        assert!(cfg.is_empty(), "the memory knobs are not a scenario");

        // A scenario the driver does mean: `--cache-mib` on its own is a manual-trace experiment
        // (`FA_PDF_TRACE=1`), so the flags have to work without a `--bench-*` step.
        let (_, cfg) = parse_args(&args(&["--cache-mib", "8", "--bench-pan", "512"]));
        assert_eq!(cfg.cache_bytes, Some(8 * 1024 * 1024));
        assert!(!cfg.is_empty());
    }
}
