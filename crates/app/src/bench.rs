// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Diagnostic harness for the zoom/scroll blank-frame work (Plan 0001 Phase 2, Plan 0002 Phase 1).
//!
//! Two pieces, both inert unless asked for:
//!
//! - a **trace** (`FA_PDF_TRACE=1`) that prints the events which decide whether a viewport change
//!   shows background pixels: the level change itself, the first tile of the new level
//!   (`blank_ms`, the white flash), full coverage (`full_ms`), the hole/pending counts of
//!   every frame (`holes` / `pending` / `unwanted`), and the time an in-flight tile took to notice
//!   it had been superseded (`abort_ms`, Plan 0001 §5 Phase 2).
//! - a **driver** (`--bench-*`) that repeats zoom, pan or scroll steps, or jumps to a page, through
//!   the same code paths the toolbar and the wheel use, settles between phases, then prints one
//!   summary.
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

use crate::{AppState, MainWindow, viewport_center, with_state, with_state_ret};
use pdf_core::nav::{
    LINE_STEP_LOGICAL_PX, NavAction, fit_page_milli, fit_width_milli, scroll_target,
    wheel_zoom_milli,
};
use pdf_core::{TileKey, TileScheduler, ZOOM_DEFAULT_MILLI, zoom_in_milli, zoom_out_milli};
use slint::Model;
use slint::platform::{Key, WindowEvent};
use slint::{ComponentHandle, Timer, TimerMode};
use slint::{LogicalPosition, SharedString};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fmt;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
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
    /// `--bench-scroll`: downward scroll speed in device pixels per second; 0 leaves the scroll
    /// phase out (Plan 0002, §6 rows A-B).
    pub scroll_pxs: f64,
    /// `--bench-scroll-s`: how long the scroll phase lasts, in seconds, unless it reaches the end.
    pub scroll_s: f64,
    /// `--bench-jump`: page to jump to (1-based, as the toolbar shows it) before the zoom phase
    /// (Plan 0002, §6 row D).
    pub jump_page: Option<usize>,
    /// `--bench-nav`: drive every navigation action through the window's own input path and check
    /// where each one lands (Plan 0002, §6 row F).
    pub nav: bool,
    /// `--bench-reopen N`: open the same document N times in one process (the first open
    /// included) and sample the working set one step after each (Plan 0003, §6 row H).
    pub reopen: u32,
    /// `--bench-thumbs`: scroll the thumbnail list at this many logical px/s for `scroll_s`
    /// seconds, with a zoom step every second, so the viewport is measured while thumbnails
    /// render (Plan 0003, §6 row J).
    pub thumbs_pxs: f64,
    /// `--bench-outline`: go to every outline entry and check where each lands (row K).
    pub outline: bool,
    /// `--bench-search QUERY`: search the document, zooming in once a second meanwhile, and
    /// report `t_first_hit`, `t_search` and the working set around it (Plan 0003, §6 rows L, O).
    pub search: Option<String>,
    /// `--bench-search-replace QUERY`: three ticks into the search, replace it with this one (row M).
    pub search_replace: Option<String>,
    /// `--bench-search-expect N`: the final search must end with exactly N hits.
    pub search_expect: Option<usize>,
    /// `--bench-search-keys`: once the search is done, press Enter, Shift+Enter, F3 and Shift+F3 in
    /// the find field through the window's input path and check the counter after each.
    pub search_keys: bool,
    /// `--bench-select`: select text with pointer events sent through the window - a drag, a
    /// Shift+click, a double- and a triple-click - on a `select_*p.pdf` document written by
    /// `engine-mupdf/tests/selection_test.rs`, and check the selected text after each.
    pub select: bool,
    /// `--bench-search-zoom`: zoom in once a second while the search runs (row L's `t_blank`);
    /// off by default, so a memory run (row O) measures the search alone.
    pub search_zoom: bool,
    /// `--bench-edit N`: highlight N lines of page 0 of a `select_*p.pdf` document through the
    /// window's input path - the Highlight tool, then a drag over each line - one per tick, and
    /// report `t_edit` (Plan 0004, §6 rows R and W).
    pub edits: u32,
    /// `--bench-edit-pan PX`: scroll down by this many device pixels between two edits, so tiles of
    /// older revisions are still in flight when the next edit lands (row W).
    pub edit_pan_px: f64,
    /// `--bench-edit-save`: save in place after the edits and report `t_save`. It writes to the
    /// document, so run it on a scratch copy.
    pub edit_save: bool,
    /// `--bench-edit-burst K`: K edits per tick, sent directly instead of made by a drag, one line
    /// box of page 1 each - the page's text line when it has one, else a box where the line
    /// would be, so a document without text (`large_200p.pdf`, row R) can be edited too. Above
    /// one, the tiles the first edit asks for are still in flight when the next one changes the
    /// page again (row W). 0, the default, edits by dragging over the text.
    pub edit_burst: u32,
    /// `--bench-note`: place a note with the Note tool on a `select_*p.pdf` document, check its
    /// text and its hover preview, drag it elsewhere and undo the move, all through the window's
    /// input path (Plan 0004 Phase 3).
    pub note: bool,
    /// `--bench-ink`: draw a 2000-point stroke with the Ink tool on page 1 through the window's
    /// input path, report `ink_rebuild_ms`, check the committed stroke against the simplified
    /// points, undo and redo it and select it by a click (Plan 0004 Phase 4, row V).
    pub ink: bool,
    /// `--bench-pages`: rotate, delete, duplicate, move (by the strip and by dragging a thumbnail)
    /// and insert a blank page through the window, undo each, and check the page order after every
    /// step; reports `t_restructure` (Plan 0005 Phase 1, rows AA and AB). Needs a document whose
    /// pages tell themselves apart by height, as `pages_test.rs` writes (`pages_bench_*.pdf`).
    pub pages: bool,
    /// `--bench-tools`: merge the open document with itself and extract two parts on the tool
    /// thread, open each output in the window and check its pages; reports `t_tool` and the working
    /// set around the merge (Plan 0005 Phase 2, rows AC and AG). Same documents as `--bench-pages`;
    /// writes `bench_merged.pdf` and `bench_part_*.pdf` next to the document.
    pub tools: bool,
    /// `--bench-stamps`: a watermark and page numbers through the stamp panel, a crop drawn with
    /// the pointer, then all three undone; checks the page sizes the layout sees and reports
    /// `t_restructure` (Plan 0005 Phase 3, row AD).
    pub stamps: bool,
    /// `--bench-convert`: compress the open document (lossless), export two pages as PNG, make a
    /// PDF of those images and open it, checking the page order from the page sizes; reports
    /// `t_tool` for each (Plan 0005 Phase 4, rows AE and AF); then exports that PDF as Word (Phase
    /// 5, with the `docx` feature). Same documents as `--bench-pages`; writes `bench_compressed.pdf`,
    /// `bench_img_*.png`,
    /// `bench_images.pdf` and `bench_images.docx` next to the document.
    pub convert: bool,
    /// `--bench-forms`: on the generated form of `form_test.rs`, click the check box, a radio
    /// button, the name field (typing Turkish text into its editor) and the combo box (choosing
    /// from its options) through the window, check each value, then undo all four; `t_edit` of
    /// each fill is its `t_fill` (Plan 0006 Phase 1, rows AI, AJ, AK).
    pub forms: bool,
    /// `--bench-idle S`: once the opening view is covered and has settled for a second, count the
    /// frames the window draws in the next S seconds with nothing moving, and report
    /// `t_first_page` (Plan 0007, rows AR and AS; benchmarks §13.1). Runs alone: the other
    /// scenarios are left out.
    pub idle_s: f64,
    /// `--bench-modes N`: once the opening view is covered, change the mode N times round
    /// Read, Edit, Tools, Read, Tools, Edit, Read, and for each change's motion report the tiles
    /// requested and rendered, the frames, the longest frame and the longest gap between two
    /// frames (Plan 0007 Phase 3, row AT; benchmarks §13.4). Runs alone.
    pub modes: u32,
    /// `--bench-tabs N`: once the opening view is covered, opens the same document in N-1 more
    /// tabs, then shows every tab once more, and reports the working set after each step, the
    /// time each tab took to show its view covered, and `peak_ws` (Plan 0007 Phase 4, row AU;
    /// benchmarks §13.5). Runs alone.
    pub tabs: u32,
    /// `--bench-pinch N`: once the opening view is covered, makes N pinches through the window's
    /// pinch callback, in to twice the zoom and out again in turn, each [`PINCH_STEPS`] frames
    /// long with the fingers' centre drifting; reports the tiles requested and rendered and the
    /// frame timing while the fingers move, then how long the view took to be covered after
    /// they lift (Plan 0008 Phase 2, row ME; benchmarks §14.4). Runs alone.
    pub pinches: u32,
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
            scroll_pxs: 0.0,
            scroll_s: 30.0,
            jump_page: None,
            nav: false,
            reopen: 0,
            thumbs_pxs: 0.0,
            outline: false,
            search: None,
            search_replace: None,
            search_expect: None,
            search_zoom: false,
            search_keys: false,
            select: false,
            edits: 0,
            edit_pan_px: 0.0,
            edit_save: false,
            edit_burst: 0,
            note: false,
            ink: false,
            pages: false,
            tools: false,
            stamps: false,
            convert: false,
            forms: false,
            idle_s: 0.0,
            modes: 0,
            pinches: 0,
            tabs: 0,
        }
    }
}

impl Cfg {
    /// Whether the flags ask for anything at all.
    pub fn is_empty(&self) -> bool {
        self.zoom_steps == 0
            && self.pan_px <= 0.0
            && self.scroll_pxs <= 0.0
            && self.jump_page.is_none()
            && !self.nav
            && self.reopen == 0
            && self.thumbs_pxs <= 0.0
            && !self.outline
            && self.search.is_none()
            && !self.select
            && self.edits == 0
            && !self.note
            && !self.ink
            && !self.pages
            && !self.tools
            && !self.stamps
            && !self.convert
            && !self.forms
            && self.idle_s <= 0.0
            && self.modes == 0
            && self.tabs == 0
            && self.pinches == 0
    }
}

/// Splits the command line into document paths and a bench configuration.
///
/// Flags may appear before or after the paths, so `--bench-zoom 4 file.pdf` and
/// `file.pdf --bench-zoom 4` behave the same. Every other argument is a path; each opens in a tab
/// of its own (Plan 0007 Phase 4), and a scripted run works on the first.
pub fn parse_args(args: &[String]) -> (Vec<String>, Cfg) {
    let mut cfg = Cfg::default();
    let mut paths = Vec::new();
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
            "--bench-scroll" => {
                cfg.scroll_pxs = number.unwrap_or(2000.0).max(0.0);
                consumed = 2;
            }
            "--bench-scroll-s" => {
                cfg.scroll_s = number.unwrap_or(30.0).max(0.0);
                consumed = 2;
            }
            "--bench-thumbs" => {
                cfg.thumbs_pxs = number.unwrap_or(20_000.0).max(0.0);
                consumed = 2;
            }
            "--bench-outline" => {
                cfg.outline = number.is_none_or(|on| on != 0.0);
                consumed = if number.is_some() { 2 } else { 1 };
            }
            "--bench-search" => {
                cfg.search = args.get(i + 1).cloned();
                consumed = 2;
            }
            "--bench-search-replace" => {
                cfg.search_replace = args.get(i + 1).cloned();
                consumed = 2;
            }
            "--bench-select" => {
                cfg.select = number.is_none_or(|on| on != 0.0);
                consumed = if number.is_some() { 2 } else { 1 };
            }
            "--bench-search-keys" => {
                cfg.search_keys = number.is_none_or(|on| on != 0.0);
                consumed = if number.is_some() { 2 } else { 1 };
            }
            "--bench-search-zoom" => {
                cfg.search_zoom = number.is_none_or(|on| on != 0.0);
                consumed = if number.is_some() { 2 } else { 1 };
            }
            "--bench-search-expect" => {
                cfg.search_expect = number.map(|n| n.max(0.0) as usize);
                consumed = 2;
            }
            "--bench-edit" => {
                cfg.edits = number.unwrap_or(20.0).max(0.0) as u32;
                consumed = 2;
            }
            "--bench-edit-pan" => {
                cfg.edit_pan_px = number.unwrap_or(40.0).max(0.0);
                consumed = 2;
            }
            "--bench-note" => {
                cfg.note = number.is_none_or(|on| on != 0.0);
                consumed = if number.is_some() { 2 } else { 1 };
            }
            "--bench-ink" => {
                cfg.ink = number.is_none_or(|on| on != 0.0);
                consumed = if number.is_some() { 2 } else { 1 };
            }
            "--bench-pages" => {
                cfg.pages = number.is_none_or(|on| on != 0.0);
                consumed = if number.is_some() { 2 } else { 1 };
            }
            "--bench-tools" => {
                cfg.tools = number.is_none_or(|on| on != 0.0);
                consumed = if number.is_some() { 2 } else { 1 };
            }
            "--bench-stamps" => {
                cfg.stamps = number.is_none_or(|on| on != 0.0);
                consumed = if number.is_some() { 2 } else { 1 };
            }
            "--bench-forms" => {
                cfg.forms = number.is_none_or(|on| on != 0.0);
                consumed = if number.is_some() { 2 } else { 1 };
            }
            "--bench-convert" => {
                cfg.convert = number.is_none_or(|on| on != 0.0);
                consumed = if number.is_some() { 2 } else { 1 };
            }
            "--bench-edit-burst" => {
                cfg.edit_burst = number.unwrap_or(2.0).max(0.0) as u32;
                consumed = 2;
            }
            "--bench-edit-save" => {
                cfg.edit_save = number.is_none_or(|on| on != 0.0);
                consumed = if number.is_some() { 2 } else { 1 };
            }
            "--bench-reopen" => {
                cfg.reopen = number.unwrap_or(10.0).max(0.0) as u32;
                consumed = 2;
            }
            "--bench-nav" => {
                cfg.nav = number.is_none_or(|on| on != 0.0);
                consumed = if number.is_some() { 2 } else { 1 };
            }
            "--bench-jump" => {
                cfg.jump_page = number.map(|page| page.max(1.0) as usize);
                consumed = 2;
            }
            "--bench-modes" => {
                cfg.modes = number.unwrap_or(1.0).max(1.0) as u32;
                consumed = if number.is_some() { 2 } else { 1 };
            }
            "--bench-pinch" => {
                cfg.pinches = number.unwrap_or(4.0).max(1.0) as u32;
                consumed = if number.is_some() { 2 } else { 1 };
            }
            "--bench-tabs" => {
                cfg.tabs = number.unwrap_or(4.0).max(1.0) as u32;
                consumed = if number.is_some() { 2 } else { 1 };
            }
            "--bench-idle" => {
                cfg.idle_s = number.unwrap_or(5.0).max(0.0);
                consumed = if number.is_some() { 2 } else { 1 };
            }
            "--cache-mib" => {
                cfg.cache_bytes = number.map(|mib| (mib.max(0.0) * 1024.0 * 1024.0) as usize);
                consumed = 2;
            }
            "--cache-rows" => {
                cfg.cache_rows = number.map(|rows| rows.max(0.0) as usize);
                consumed = 2;
            }
            other => paths.push(other.to_owned()),
        }
        i += consumed;
    }
    (paths, cfg)
}

/// The instant `main` began, the zero of every `t=` in the trace and of `t_first_page`.
static PROCESS_START: OnceLock<Instant> = OnceLock::new();

/// Frames the window has drawn, counted by the rendering notifier [`count_frames`] installs.
static FRAMES: AtomicU64 = AtomicU64::new(0);

/// Frame timing since the last [`frame_window_reset`] (benchmarks §13.4): the frames drawn, the
/// longest `frame_ms` and the longest gap between the ends of two frames, in microseconds, and
/// when the last frame ended (microseconds after the process start, 0 for none yet).
static WINDOW_FRAMES: AtomicU64 = AtomicU64::new(0);
static FRAME_MAX_US: AtomicU64 = AtomicU64::new(0);
static FRAME_GAP_MAX_US: AtomicU64 = AtomicU64::new(0);
static LAST_FRAME_END_US: AtomicU64 = AtomicU64::new(0);
/// The first frame of the window: how long it took (0 before it ends).
static FRAME_FIRST_US: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// When the frame being drawn began; the notifier runs on the UI thread.
    static FRAME_START: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Frames drawn since [`frame_window_reset`], with the longest frame and gap and the first frame,
/// in milliseconds. The first gap runs from the reset to the end of the first frame.
#[derive(Debug, Clone, Copy)]
pub struct FrameWindow {
    pub frames: u64,
    pub max_ms: f64,
    pub gap_ms: f64,
    pub first_ms: f64,
}

impl fmt::Display for FrameWindow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "frames={} frame_max={:.1}ms frame_first={:.1}ms gap_max={:.1}ms",
            self.frames, self.max_ms, self.first_ms, self.gap_ms
        )
    }
}

/// Starts a new frame timing window.
pub fn frame_window_reset() {
    WINDOW_FRAMES.store(0, Ordering::Relaxed);
    FRAME_MAX_US.store(0, Ordering::Relaxed);
    FRAME_GAP_MAX_US.store(0, Ordering::Relaxed);
    FRAME_FIRST_US.store(0, Ordering::Relaxed);
    let now = process_start().elapsed().as_micros().max(1) as u64;
    LAST_FRAME_END_US.store(now, Ordering::Relaxed);
}

pub fn frame_window() -> FrameWindow {
    FrameWindow {
        frames: WINDOW_FRAMES.load(Ordering::Relaxed),
        max_ms: FRAME_MAX_US.load(Ordering::Relaxed) as f64 / 1000.0,
        gap_ms: FRAME_GAP_MAX_US.load(Ordering::Relaxed) as f64 / 1000.0,
        first_ms: FRAME_FIRST_US.load(Ordering::Relaxed) as f64 / 1000.0,
    }
}

/// Marks the start of the process; call it first in `main`, before the window is built, so the
/// window's own construction is part of `t_first_page`.
pub fn mark_process_start() {
    PROCESS_START.get_or_init(Instant::now);
}

fn process_start() -> Instant {
    *PROCESS_START.get_or_init(Instant::now)
}

/// Counts the frames the window draws (`frames`, benchmarks §13.1). Only installed when tracing:
/// the notifier is one atomic add per frame, but an untraced run needs none.
///
/// It also times each frame (benchmarks §13.4): `frame_ms` runs from Slint's `BeforeRendering` to
/// its `AfterRendering`, the drawing and the hand-over to the GPU without the wait for the
/// display (`i-slint-renderer-femtovg-1.18.1/lib.rs:260, 333`).
pub fn count_frames(ui: &MainWindow) {
    let installed = ui.window().set_rendering_notifier(|state, _| match state {
        slint::RenderingState::BeforeRendering => {
            FRAME_START.with(|start| start.set(Some(Instant::now())));
        }
        slint::RenderingState::AfterRendering => {
            FRAMES.fetch_add(1, Ordering::Relaxed);
            WINDOW_FRAMES.fetch_add(1, Ordering::Relaxed);
            let now = Instant::now();
            if let Some(start) = FRAME_START.with(Cell::take) {
                let took = now.duration_since(start).as_micros() as u64;
                FRAME_MAX_US.fetch_max(took, Ordering::Relaxed);
                let _ = FRAME_FIRST_US.compare_exchange(
                    0,
                    took.max(1),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                );
            }
            let end = now.duration_since(process_start()).as_micros().max(1) as u64;
            let last = LAST_FRAME_END_US.swap(end, Ordering::Relaxed);
            if last != 0 {
                FRAME_GAP_MAX_US.fetch_max(end.saturating_sub(last), Ordering::Relaxed);
            }
        }
        _ => {}
    });
    if let Err(err) = installed {
        println!("[bench] no frame counter: {err}");
    }
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
pub(crate) fn holes(sched: &TileScheduler, visible: &[TileKey]) -> u32 {
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
/// `blank_ms`/`full_ms`/`abort_ms`/`grows` are cumulative, because a level change is a single
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
    /// Model growths: a resize, or a zoom that shows more small pages than the rows hold.
    grows: u32,
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
    /// Model grows during the run.
    grows: u32,
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
    /// Distinct pages that were visible at least once (`pages_visited`, benchmarks §8.2).
    pages_visited: HashSet<u32>,
    /// Canvas-window re-anchors, each with how long its rewrite took in microseconds.
    reanchor_us: Vec<u128>,
    /// Worst `pages_resident` sampled.
    max_resident: usize,
    /// Thumbnails held now, and the most ever held (`thumbs_resident`, benchmarks §9.3).
    thumbs: usize,
    max_thumbs: usize,
    /// Index into `memory` where the scroll phase started, for `ws_growth`.
    scroll_mem_from: Option<usize>,
    /// `t_first_hit` samples, and searches finished.
    first_hit_ms: Vec<u128>,
    searches_done: u32,
    /// `--bench-nav` checks: `(step, passed)`.
    nav_checks: Vec<(String, bool)>,
    /// `--bench-reopen` samples: `(open number, working set bytes)`.
    reopen_ws: Vec<(u32, usize)>,
    /// Edits waiting for their page's tiles (Plan 0004, `t_edit` in benchmarks §10.2): when the
    /// first edit of the page was sent, and whether its `PageChanged` has arrived.
    edits: HashMap<u32, (Instant, bool)>,
    edit_ms: Vec<u128>,
    /// Tiles drawn from an older page revision than the app knows, dropped on arrival.
    stale_dropped: u32,
    save_started: Option<Instant>,
    /// `t_save` samples: `(ms, file bytes)`.
    saves: Vec<(u128, u64)>,
    /// Tile requests per page while an edit phase runs: without a pan, only the edited page may
    /// be asked for again (row W).
    edit_requests: Option<HashMap<u32, u32>>,
    /// `ws_after_edits_n` samples: `(edits, working set bytes)` (row X).
    edit_ws: Vec<(u32, usize)>,
    /// `ink_rebuild_ms` samples as `(points in the stroke, microseconds)`, and the last stroke
    /// the app committed, simplified (row V).
    ink_rebuild_us: Vec<(usize, u128)>,
    /// Where the stroke being drawn starts in `ink_rebuild_us`.
    stroke_from: usize,
    last_ink: Option<Vec<(f32, f32)>>,
    /// When the opening view was first fully covered, in ms from the process start
    /// (`t_first_page`, benchmarks §13.1).
    first_full_ms: Option<u128>,
    /// `t_restructure` (Plan 0005, benchmarks §11.2): when the last structural edit was sent, its
    /// samples, and how long the app took to rebuild its layout on each answer.
    restructure_started: Option<Instant>,
    restructure_ms: Vec<u128>,
    rebuild_us: Vec<u128>,
    restructure_timed: bool,
    tools_done: u32,
}

impl Trace {
    /// Creates a trace that is either silent or printing to stdout.
    pub fn new(on: bool) -> Self {
        Self {
            on,
            started: process_start(),
            wait: None,
            last_report: Instant::now(),
            last_holes: 0,
            window_name: "run".to_owned(),
            stats: Stats::default(),
            runs: Vec::new(),
            quiet: false,
            memory: Vec::new(),
            last_rss: Instant::now(),
            pages_visited: HashSet::new(),
            reanchor_us: Vec::new(),
            max_resident: 0,
            thumbs: 0,
            max_thumbs: 0,
            scroll_mem_from: None,
            first_hit_ms: Vec::new(),
            searches_done: 0,
            nav_checks: Vec::new(),
            reopen_ws: Vec::new(),
            edits: HashMap::new(),
            edit_ms: Vec::new(),
            stale_dropped: 0,
            save_started: None,
            saves: Vec::new(),
            edit_requests: None,
            edit_ws: Vec::new(),
            ink_rebuild_us: Vec::new(),
            stroke_from: 0,
            last_ink: None,
            restructure_started: None,
            restructure_ms: Vec::new(),
            rebuild_us: Vec::new(),
            restructure_timed: true,
            tools_done: 0,
            first_full_ms: None,
        }
    }

    /// A structural edit was sent: `t_restructure` starts.
    pub fn restructure_sent(&mut self) {
        if self.on && self.restructure_timed {
            self.restructure_started = Some(Instant::now());
        }
    }

    /// Whether structural edits are timed: not in a burst, where each waits for the ones before.
    pub fn time_restructures(&mut self, on: bool) {
        self.restructure_timed = on;
        if !on {
            self.restructure_started = None;
        }
    }

    /// Layout rebuilds so far (one per `Restructured` answer).
    pub fn rebuilds(&self) -> usize {
        self.rebuild_us.len()
    }

    /// The app rebuilt its layout for a structural change in `took`.
    pub fn restructured(&mut self, took: Duration) {
        if self.on {
            self.rebuild_us.push(took.as_micros());
        }
    }

    /// A tile landed: once every visible tile of the new layout is there, `t_restructure` ends.
    pub fn restructure_tile(&mut self, sched: &TileScheduler, visible: &[TileKey]) {
        if !self.on || holes(sched, visible) > 0 {
            return;
        }
        if let Some(started) = self.restructure_started.take() {
            let ms = started.elapsed().as_millis();
            self.restructure_ms.push(ms);
            let t = self.t_ms();
            self.print(format_args!("restructure t={t}ms t_restructure={ms}"));
        }
    }

    /// The working set `n` edits into an edit phase (`ws_after_edits_n`, row X).
    pub fn edit_memory(&mut self, n: u32) {
        if !self.on {
            return;
        }
        let ws = process_memory().map_or(0, |mem| mem.ws);
        self.edit_ws.push((n, ws));
        println!("[bench] ws_after_edits n={n} ws={}", mem_str(Some(ws)));
    }

    /// A tool job ended (`t_tool`, benchmarks §11.2).
    pub fn tool_done(
        &mut self,
        title: &str,
        took: Duration,
        result: &Result<Vec<std::path::PathBuf>, String>,
    ) {
        if !self.on {
            return;
        }
        let ws = process_memory().map(|mem| mem.ws);
        let outcome = match result {
            Ok(files) => format!("{} file(s)", files.len()),
            Err(err) => format!("failed: {err}"),
        };
        println!(
            "[bench] tool {title} t_tool={}ms {outcome} ws={}",
            took.as_millis(),
            mem_str(ws)
        );
        self.tools_done += 1;
    }

    /// Tool jobs ended so far.
    pub fn tools_done(&self) -> u32 {
        self.tools_done
    }

    /// The working set after `n` structural edits (row AG).
    pub fn pages_memory(&mut self, n: u32) {
        if !self.on {
            return;
        }
        let ws = process_memory().map_or(0, |mem| mem.ws);
        println!("[bench] ws_after_page_edits n={n} ws={}", mem_str(Some(ws)));
    }

    /// The live ink path was rebuilt for a stroke of `points` points.
    pub fn ink_rebuilt(&mut self, points: usize, took: Duration) {
        if self.on {
            self.ink_rebuild_us.push((points, took.as_micros()));
        }
    }

    /// A stroke was committed with these simplified points.
    pub fn ink_sent(&mut self, stroke: &[(f32, f32)]) {
        if self.on {
            self.last_ink = Some(stroke.to_vec());
        }
    }

    /// The last stroke committed.
    pub fn last_ink(&self) -> Option<&[(f32, f32)]> {
        self.last_ink.as_deref()
    }

    /// Starts or ends counting tile requests per page (row W).
    pub fn requests_during_edits(&mut self, on: bool) {
        if !self.on {
            return;
        }
        if on {
            self.edit_requests = Some(HashMap::new());
        } else if let Some(counts) = self.edit_requests.take() {
            let mut pages: Vec<(u32, u32)> = counts.into_iter().collect();
            pages.sort_unstable();
            let listed: Vec<String> = pages
                .iter()
                .map(|(page, n)| format!("page{}={n}", page + 1))
                .collect();
            self.print(format_args!(
                "edit_requests {}",
                if listed.is_empty() {
                    "none".to_owned()
                } else {
                    listed.join(" ")
                }
            ));
        }
    }

    /// A tile was requested.
    pub fn tile_requested(&mut self, page: u32) {
        if let Some(counts) = self.edit_requests.as_mut() {
            *counts.entry(page).or_insert(0) += 1;
        }
    }

    /// An edit of `page` was sent: `t_edit` starts, unless an earlier edit of it is still waiting.
    pub fn edit_sent(&mut self, page: u32) {
        if !self.on {
            return;
        }
        self.edits.entry(page).or_insert((Instant::now(), false));
    }

    /// The engine reported the edited page's new revision; `stale` of its tiles are on screen.
    /// A page with none in view has nothing to wait for.
    pub fn page_changed(&mut self, page: u32, stale: usize) {
        if !self.on {
            return;
        }
        if stale == 0 {
            self.edits.remove(&page);
        } else if let Some(edit) = self.edits.get_mut(&page) {
            edit.1 = true;
        }
    }

    /// A tile of `page` landed and `stale` of its visible tiles still show the old revision: at
    /// zero, the edit is on screen and `t_edit` ends.
    pub fn tile_after_edit(&mut self, page: u32, stale: usize) {
        if !self.on || stale > 0 {
            return;
        }
        if let Some((started, true)) = self.edits.get(&page).copied() {
            self.edits.remove(&page);
            let ms = started.elapsed().as_millis();
            self.edit_ms.push(ms);
            let t = self.t_ms();
            self.print(format_args!("edit t={t}ms page={page} t_edit={ms}"));
        }
    }

    /// A tile of an older revision was dropped (`stale_tiles` counts the ones that were not).
    pub fn stale_dropped(&mut self) {
        self.stale_dropped += 1;
    }

    /// A save was sent: `t_save` starts.
    pub fn save_sent(&mut self) {
        if self.on {
            self.save_started = Some(Instant::now());
        }
    }

    /// A save finished with the file at `bytes`.
    pub fn saved(&mut self, bytes: u64) {
        if let Some(started) = self.save_started.take() {
            let ms = started.elapsed().as_millis();
            self.saves.push((ms, bytes));
            let t = self.t_ms();
            self.print(format_args!("save t={t}ms t_save={ms} bytes={bytes}"));
        }
    }

    /// When the opening view was first fully covered, in ms from the process start.
    pub fn first_full_ms(&self) -> Option<u128> {
        self.first_full_ms
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
            self.first_full_ms.get_or_insert(t);
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
        self.pages_visited
            .extend(visible.iter().map(|key| key.page));
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
    pub fn sample_memory(&mut self, sched: &TileScheduler, pages_resident: usize) {
        if !self.on || self.last_rss.elapsed().as_millis() < RSS_MS {
            return;
        }
        self.last_rss = Instant::now();
        let mem = process_memory();
        if let Some(mem) = mem {
            self.memory.push(mem);
        }
        self.max_resident = self.max_resident.max(pages_resident);
        let t = self.t_ms();
        let cache_mib = mib(sched.cache_bytes() as u128);
        self.print(format_args!(
            "rss t={t}ms ws={} private={} cache={cache_mib:.1}MiB/{} rows dropped={} evicted={} \
             pages_resident={pages_resident} thumbs_resident={}",
            mem_str(mem.map(|mem| mem.ws)),
            mem_str(mem.map(|mem| mem.private)),
            sched.cache_len(),
            sched.dropped(),
            sched.evicted(),
            self.thumbs
        ));
    }

    /// The model grew. Rows are added, nothing cached is dropped (Plan 0002, §4.3).
    pub fn model_grown(&mut self, capacity: usize) {
        if !self.on {
            return;
        }
        self.stats.grows += 1;
        let t = self.t_ms();
        self.print(format_args!("grow t={t}ms capacity={capacity}"));
    }

    /// The working set one step after the `n`-th open of the same document (`ws_after_open_n`).
    pub fn reopen_sample(&mut self, n: u32) {
        let ws = process_memory().map_or(0, |mem| mem.ws);
        self.reopen_ws.push((n, ws));
        println!("[bench] reopen n={n} ws={}", mem_str(Some(ws)));
    }

    /// One `--bench-nav` step checked: what the core predicted and what the window shows.
    pub fn nav_check(&mut self, step: &str, expected: &str, got: &str) {
        let pass = expected == got;
        self.nav_checks.push((step.to_owned(), pass));
        println!(
            "[bench] nav {step:<14} expected {expected:<22} got {got:<22} {}",
            if pass { "PASS" } else { "FAIL" }
        );
    }

    /// Most thumbnails ever held at once.
    pub fn thumbs_max(&self) -> usize {
        self.max_thumbs
    }

    /// A search was sent (Plan 0003, §9.3).
    pub fn search_started(&mut self, generation: u64, query: &str) {
        if !self.on {
            return;
        }
        let t = self.t_ms();
        let ws = process_memory().map(|mem| mem.ws);
        self.print(format_args!(
            "search t={t}ms generation={generation} query={query:?} ws={}",
            mem_str(ws)
        ));
    }

    /// The first hit of a search reached the results: `t_first_hit`.
    pub fn first_hit(&mut self, ms: u128) {
        if !self.on {
            return;
        }
        self.first_hit_ms.push(ms);
        let t = self.t_ms();
        self.print(format_args!("first_hit t={t}ms {ms}"));
    }

    /// A search covered the document: `t_search` as the search thread measured it (`ms`) and as the
    /// app saw it (`wall`, from the send), with the hit count and the stale hits refused so far.
    pub fn search_done(
        &mut self,
        pages: u32,
        ms: u64,
        wall: Option<u128>,
        hits: usize,
        stale: u64,
    ) {
        if !self.on {
            return;
        }
        let t = self.t_ms();
        let ws = process_memory().map(|mem| mem.ws);
        let per_page = ms as f64 / pages.max(1) as f64;
        self.print(format_args!(
            "search_done t={t}ms pages={pages} ms={ms} ({per_page:.2} ms/page) wall={} hits={hits} \
             stale_refused={stale} ws={}",
            wall.map_or_else(|| "-".to_owned(), |w| w.to_string()),
            mem_str(ws)
        ));
        self.searches_done += 1;
    }

    /// Searches that reported done.
    pub fn searches_done(&self) -> u32 {
        self.searches_done
    }

    /// The sidebar holds `n` thumbnails now.
    pub fn thumbs_held(&mut self, n: usize) {
        self.thumbs = n;
        self.max_thumbs = self.max_thumbs.max(n);
    }

    /// The pointer or a finger went down (`kind` 0), came up (1), double-clicked (3) or was
    /// cancelled (4) on the page, viewport-relative: taps and double taps can be followed in a
    /// run on the phone.
    pub fn pointer(&mut self, kind: i32, x: f32, y: f32, touch: bool) {
        if !self.on || self.quiet {
            return;
        }
        let t = self.t_ms();
        self.print(format_args!(
            "pointer t={t}ms kind={kind} x={x:.0} y={y:.0} touch={touch}"
        ));
    }

    /// An ink stroke began (`None`) or ended with this many points: the frames between the two
    /// are timed (§14.6, row MJ), with the longest rebuild of the live stroke.
    pub fn stroke(&mut self, points: Option<usize>) {
        if !self.on || self.quiet {
            return;
        }
        let t = self.t_ms();
        match points {
            None => {
                frame_window_reset();
                self.stroke_from = self.ink_rebuild_us.len();
                self.print(format_args!("ink started t={t}ms"));
            }
            Some(points) => {
                let window = frame_window();
                let rebuild_max = self
                    .ink_rebuild_us
                    .get(self.stroke_from..)
                    .and_then(|rebuilds| rebuilds.iter().map(|(_, us)| *us).max())
                    .unwrap_or(0);
                self.print(format_args!(
                    "ink ended t={t}ms points={points} rebuild_max_us={rebuild_max} {window}"
                ));
            }
        }
    }

    /// A pinch began (`phase` 0) at `milli`, or ended (2) and zoomed to it. The frames between the
    /// two are timed (§14.4), so a pinch by hand is measured like a scripted one.
    pub fn pinch(&mut self, phase: i32, milli: u32) {
        if !self.on || self.quiet {
            return;
        }
        let t = self.t_ms();
        if phase == 0 {
            frame_window_reset();
            self.print(format_args!("pinch started t={t}ms scale={milli}"));
        } else {
            let window = frame_window();
            self.print(format_args!("pinch ended t={t}ms scale={milli} {window}"));
        }
    }

    /// The canvas window moved (Plan 0002, §4.2): `us` is how long rewriting the canvas took,
    /// `pos_y` the document position and `start` the new window start, both in device pixels.
    pub fn reanchored(&mut self, us: u128, pos_y: f64, start: f64) {
        if !self.on || self.quiet {
            return;
        }
        self.reanchor_us.push(us);
        let t = self.t_ms();
        self.print(format_args!(
            "reanchor t={t}ms us={us} pos_y={pos_y:.0} start={start:.0}"
        ));
    }

    /// The repeat in progress, as a [`Run`]: the worst of each metric measured so far.
    fn current(&self) -> Run {
        Run {
            blank_max: self.stats.blank_ms.iter().copied().max(),
            full_max: self.stats.full_ms.iter().copied().max(),
            abort_max: self.stats.abort_ms.iter().copied().max(),
            grows: self.stats.grows,
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
        let frames = frame_window();
        frame_window_reset();
        self.print(format_args!("window {closed} t={t}ms {window} {frames}"));
        self.stats.reset_window();
        if name == "scroll" {
            self.scroll_mem_from = Some(self.memory.len());
        }
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
    pub fn report(
        &self,
        cfg: &Cfg,
        sched: &TileScheduler,
        rendered: i32,
        requested: i32,
        page: i32,
    ) -> bool {
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
            "[bench] tiles       rendered={rendered} requested={requested} grows={}",
            runs.iter().map(|run| run.grows).sum::<u32>()
        );
        if !self.first_hit_ms.is_empty() {
            let (h_min, h_med, h_max) = spread(&self.first_hit_ms);
            println!(
                "[bench] t_first_hit min={h_min} med={h_med} max={h_max} n={} (ms from send)",
                self.first_hit_ms.len()
            );
            println!(
                "[bench] ws_at_report {}",
                mem_str(process_memory().map(|mem| mem.ws))
            );
        }
        if !self.edit_ms.is_empty() || self.stale_dropped > 0 {
            let (e_min, e_med, e_max) = spread(&self.edit_ms);
            println!(
                "[bench] t_edit_ms   min={e_min} med={e_med} max={e_max} n={} \
                 (old-revision tiles dropped={})",
                self.edit_ms.len(),
                self.stale_dropped
            );
        }
        for (ms, bytes) in &self.saves {
            println!("[bench] t_save_ms   {ms} ({bytes} bytes)");
        }
        if !self.restructure_ms.is_empty() {
            let (r_min, r_med, r_max) = spread(&self.restructure_ms);
            let (b_min, b_med, b_max) = spread(&self.rebuild_us);
            println!(
                "[bench] t_restructure_ms min={r_min} med={r_med} max={r_max} n={} \
                 (layout rebuild us min={b_min} med={b_med} max={b_max})",
                self.restructure_ms.len()
            );
        }
        if !self.edit_ws.is_empty() {
            let journal = if env::var_os("FA_PDF_NO_JOURNAL").is_some() {
                "off"
            } else {
                "on"
            };
            let listed: Vec<String> = self
                .edit_ws
                .iter()
                .map(|(n, ws)| format!("n={n}:{}", mem_str(Some(*ws))))
                .collect();
            println!(
                "[bench] ws_after_edits {} (journal {journal})",
                listed.join(" ")
            );
        }
        let mut ink_failed = false;
        if !self.ink_rebuild_us.is_empty() {
            let us: Vec<u128> = self.ink_rebuild_us.iter().map(|(_, us)| *us).collect();
            let (_, med, max) = spread(&us);
            let points = self
                .ink_rebuild_us
                .iter()
                .map(|(n, _)| *n)
                .max()
                .unwrap_or(0);
            // Row V: every move of the stroke rebuilds its path within 4 ms.
            ink_failed = max > 4000;
            println!(
                "[bench] ink_rebuild_ms med={:.3} max={:.3} moves={} ink_points={points} {}",
                med as f64 / 1000.0,
                max as f64 / 1000.0,
                us.len(),
                if ink_failed { "FAIL" } else { "PASS" }
            );
        }
        let (r_min, r_med, r_max) = spread(&self.reanchor_us);
        println!(
            "[bench] pages       visited={} resident_max={} reanchors={} reanchor_us \
             min={r_min} med={r_med} max={r_max}",
            self.pages_visited.len(),
            self.max_resident,
            self.reanchor_us.len()
        );
        println!(
            "[bench] position    page={} (1-based, under the viewport centre)",
            page + 1
        );
        // `ws_growth` (benchmarks §8.2): the scroll phase's last 5 s against its first 5 s, as
        // medians of the 250 ms samples, so one allocation spike cannot decide it.
        let window = (5_000 / RSS_MS) as usize;
        let scroll = self
            .scroll_mem_from
            .and_then(|from| self.memory.get(from..))
            .unwrap_or(&[]);
        if scroll.len() >= 2 * window {
            let median = |samples: &[Mem]| {
                let mut ws: Vec<usize> = samples.iter().map(|mem| mem.ws).collect();
                ws.sort_unstable();
                ws[ws.len() / 2] as f64
            };
            let first = median(&scroll[..window]);
            let last = median(&scroll[scroll.len() - window..]);
            println!(
                "[bench] ws_growth   {:+.1} MiB (scroll phase: first 5 s median {:.1}, last 5 s median {:.1}, n={})",
                (last - first) / (1024.0 * 1024.0),
                first / (1024.0 * 1024.0),
                last / (1024.0 * 1024.0),
                scroll.len()
            );
        } else {
            println!("[bench] ws_growth   n/a - the scroll phase has fewer than 10 s of samples");
        }
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
                "[bench] per-run     t_blank {} | t_full {} | t_abort {} | grows {}",
                listed(&blanks),
                listed(&fulls),
                listed(&aborts),
                runs.iter()
                    .map(|run| run.grows.to_string())
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

        let nav_failed = self.nav_checks.iter().filter(|(_, pass)| !pass).count();
        if !self.nav_checks.is_empty() {
            println!(
                "[bench] nav         {} of {} steps passed",
                self.nav_checks.len() - nav_failed,
                self.nav_checks.len()
            );
        }
        // Row J: the thumbnail cache never holds more than its cap.
        let thumbs_failed = self.max_thumbs > crate::THUMB_CACHE;
        if self.max_thumbs > 0 {
            println!(
                "[bench] thumbs      resident_max={} (cap {}) {}",
                self.max_thumbs,
                crate::THUMB_CACHE,
                if thumbs_failed { "FAIL" } else { "PASS" }
            );
        }
        // Row H: the working set after the last open against after the first, 10 MiB allowed.
        let mut reopen_failed = false;
        if let (Some(first), Some(last)) = (self.reopen_ws.first(), self.reopen_ws.last()) {
            let delta = last.1 as f64 - first.1 as f64;
            reopen_failed = delta > 10.0 * 1024.0 * 1024.0;
            println!(
                "[bench] ws_after_open first(n={})={:.1}MiB last(n={})={:.1}MiB delta={:+.1}MiB \
                 (limit +10 MiB) {}",
                first.0,
                mib(first.1 as u128),
                last.0,
                mib(last.1 as u128),
                delta / (1024.0 * 1024.0),
                if reopen_failed { "FAIL" } else { "PASS" }
            );
        }
        let worst_blank = runs.iter().filter_map(|run| run.blank_max).max();
        let Some(worst_blank) = worst_blank else {
            if nav_failed > 0 {
                println!("[bench] VERDICT     FAIL - {nav_failed} navigation step(s) missed");
                return false;
            }
            if reopen_failed {
                println!("[bench] VERDICT     FAIL - the working set grew across reopening");
                return false;
            }
            if thumbs_failed {
                println!("[bench] VERDICT     FAIL - more thumbnails held than the cap");
                return false;
            }
            if ink_failed {
                println!("[bench] VERDICT     FAIL - an ink path took over 4 ms to rebuild");
                return false;
            }
            println!("[bench] VERDICT     n/a - no level change was measured");
            return true;
        };
        let pass = worst_blank <= cfg.max_blank_ms
            && nav_failed == 0
            && !reopen_failed
            && !thumbs_failed
            && !ink_failed;
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
    /// One go-to-page, before the zoom (Plan 0002, §6 row D).
    Jump,
    /// `+ Zoom` steps.
    Zoom,
    /// Rightward scroll steps, up to the document edge.
    Pan,
    /// Downward scroll at a constant speed, up to the end of the document or of `scroll_s`.
    Scroll,
    /// The navigation steps of [`NAV_STEPS`], one per tick, each checked on the next tick.
    Nav,
    /// Sample the working set, then open the same document again; one open per tick.
    Reopen,
    /// Scroll the thumbnail list, with a zoom step every second.
    Thumbs,
    /// Go to every outline entry, each checked on the next tick.
    Outline,
    /// Search, with a zoom step a second, until the (last) search reports done.
    Search,
    /// Pointer selections, one per tick, each checked on the next tick.
    Select,
    /// Highlights made through the tool and a drag, one per tick (Plan 0004 rows R, W).
    Edit,
    /// A sticky note placed, previewed, moved and the move undone (Plan 0004 Phase 3).
    Note,
    /// An ink stroke drawn, checked, undone, redone and selected (Plan 0004 Phase 4).
    Ink,
    /// Page operations through the sidebar, each undone (Plan 0005 Phase 1).
    Pages,
    /// Merge and extract on the tool thread, each output opened and checked (Plan 0005 Phase 2).
    Tools,
    /// Watermark, page numbers and a crop, then undone (Plan 0005 Phase 3).
    Stamps,
    /// Compress, export as images and images to PDF, the result opened (Plan 0005 Phase 4).
    Convert,
    /// Form fields filled by clicking and typing, then undone (Plan 0006 Phase 1).
    Forms,
    /// Idle time, so the previous phase's tiles arrive before the next phase or the summary.
    Settle,
}

/// The phases a run still has to do after a settle, in order.
#[derive(Debug, Clone, Copy, Default)]
struct Pending {
    jump: bool,
    zoom: bool,
    pan: bool,
    scroll: bool,
    nav: bool,
    reopen: bool,
    thumbs: bool,
    outline: bool,
    search: bool,
    select: bool,
    edit: bool,
    note: bool,
    ink: bool,
    pages: bool,
    tools: bool,
    stamps: bool,
    convert: bool,
    forms: bool,
}

impl Pending {
    /// Every phase the configuration asks for.
    fn all(cfg: &Cfg) -> Self {
        Self {
            jump: cfg.jump_page.is_some(),
            zoom: cfg.zoom_steps > 0,
            pan: cfg.pan_px > 0.0 && cfg.pan_steps > 0,
            scroll: cfg.scroll_pxs > 0.0 && cfg.scroll_s > 0.0,
            nav: cfg.nav,
            reopen: cfg.reopen > 0,
            thumbs: cfg.thumbs_pxs > 0.0,
            outline: cfg.outline,
            search: cfg.search.is_some(),
            select: cfg.select,
            edit: cfg.edits > 0,
            note: cfg.note,
            ink: cfg.ink,
            pages: cfg.pages,
            tools: cfg.tools,
            stamps: cfg.stamps,
            convert: cfg.convert,
            forms: cfg.forms,
        }
    }

    /// Takes the next phase to run, or `None` when the run is over.
    fn take(&mut self) -> Option<Phase> {
        let slots = [
            (&mut self.jump, Phase::Jump),
            (&mut self.zoom, Phase::Zoom),
            (&mut self.pan, Phase::Pan),
            (&mut self.scroll, Phase::Scroll),
            (&mut self.nav, Phase::Nav),
            (&mut self.reopen, Phase::Reopen),
            (&mut self.thumbs, Phase::Thumbs),
            (&mut self.outline, Phase::Outline),
            (&mut self.search, Phase::Search),
            (&mut self.select, Phase::Select),
            (&mut self.edit, Phase::Edit),
            (&mut self.note, Phase::Note),
            (&mut self.ink, Phase::Ink),
            (&mut self.pages, Phase::Pages),
            (&mut self.tools, Phase::Tools),
            (&mut self.stamps, Phase::Stamps),
            (&mut self.convert, Phase::Convert),
            (&mut self.forms, Phase::Forms),
        ];
        for (flag, phase) in slots {
            if *flag {
                *flag = false;
                return Some(phase);
            }
        }
        None
    }
}

/// One run in progress, driven by the repeating timer on the UI thread.
struct Driver {
    cfg: Cfg,
    phase: Phase,
    /// Steps done in the current phase.
    step: u32,
    /// Ticks left in the settle phase.
    settle_ticks: u32,
    /// Phases still to run after the current settle. A run opens with a settle so the first step
    /// does not land inside the document's own first tile, which on a dense page takes over a
    /// second; `--bench-settle-ms 0` gets that case back.
    pending: Pending,
    /// Repeats still to run, including the one in progress.
    runs_left: u32,
    /// Viewport at the scenario's start, `(scale_milli, document position)`: recorded on the first
    /// step and restored before every repeat, so each run measures the same scenario.
    home: Option<(u32, (f64, f64))>,
    /// A repeat boundary is settling after that restore; the run starts when it ends.
    resetting: bool,
    /// The `--bench-nav` step performed on the last tick and the state it should have produced.
    nav_expect: Option<(&'static str, String)>,
    /// Searches finished before the search phase started.
    searches_before: u32,
    /// The edit phase waits until this moment, then samples the working set after this many
    /// edits (`ws_after_edits_n`: 2 s after the n-th edit).
    edit_hold: Option<(Instant, u32)>,
    /// The page count `--bench-pages` started from, and the layout rebuilds its burst waits for.
    pages_base: u32,
    pages_rebuilds: usize,
    /// `--bench-tools` waits for this many finished jobs, or for an open of this many pages.
    tools_wait: u32,
    open_wait: Option<i32>,
    /// `--bench-stamps`: the first page's size as opened.
    base_size: String,
    /// `--bench-forms`: the check the last step waits for (its name, the field, the value), and
    /// how long it waits.
    form_expect: Option<(&'static str, &'static str, String)>,
    form_deadline: Instant,
    /// Where and when the forms bench pressed the button down.
    form_press: Option<(LogicalPosition, Instant)>,
}

/// Where the forms bench writes the form's values: beside the document.
fn form_data_path(state: &AppState) -> Option<std::path::PathBuf> {
    Some(
        state
            .doc_path
            .as_deref()?
            .parent()?
            .join("bench_form_data.json"),
    )
}

/// A page-space point of page 0, `(x, line)`, as a window position (logical pixels).
fn window_point(ui: &MainWindow, (x, line): (f32, usize)) -> LogicalPosition {
    page_window_point(ui, x, 42.0 + 12.0 * line as f32 - 3.0)
}

/// A page-space point of page 0, in points from its top-left, as a window position.
fn page_window_point(ui: &MainWindow, x: f32, page_y: f32) -> LogicalPosition {
    let mut at = LogicalPosition::new(0.0, 0.0);
    with_state(|state| {
        let Some(slot) = state.layout.slot(0) else {
            return;
        };
        let s = state.scale_milli as f64 / 1000.0;
        let sf = state.sf();
        let page_y = page_y as f64;
        let doc_x = slot.x as f64 + x as f64 * s;
        let doc_y = slot.y as f64 + page_y * s;
        at = LogicalPosition::new(
            ui.get_viewport_abs_x() + ((doc_x - state.pos.0) / sf) as f32,
            ui.get_viewport_abs_y() + ((doc_y - state.pos.1) / sf) as f32,
        );
    });
    at
}

/// A left-button drag from one point to another, through the window's input path.
fn select_drag(ui: &MainWindow, from: (f32, usize), to: (f32, usize)) {
    let window = ui.window();
    let (a, b) = (window_point(ui, from), window_point(ui, to));
    window.dispatch_event(WindowEvent::PointerMoved { position: a });
    window.dispatch_event(WindowEvent::PointerPressed {
        position: a,
        button: slint::platform::PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerMoved { position: b });
    window.dispatch_event(WindowEvent::PointerReleased {
        position: b,
        button: slint::platform::PointerEventButton::Left,
    });
}

/// The page order the app shows, as the bench document encodes it: each page of
/// `pages_bench_*.pdf` is 800 + its number points tall, so its size names it; `r` marks a page
/// turned sideways.
fn page_order() -> String {
    let mut pages = Vec::new();
    with_state(|state| {
        pages = state
            .page_sizes
            .iter()
            .map(|size| {
                let (long, turned) = if size.height >= size.width {
                    (size.height, false)
                } else {
                    (size.width, true)
                };
                let page = (long.round() as i64 - 800).to_string();
                if turned { page + "r" } else { page }
            })
            .collect();
    });
    order_str(pages.len(), pages)
}

/// A page order as the checks compare it: the count and the first eight pages.
fn order_str(count: usize, pages: Vec<String>) -> String {
    let head: Vec<String> = pages.into_iter().take(8).collect();
    format!("n={count} [{}]", head.join(","))
}

/// A point of a thumbnail row (`frac` of the way down it), as a window position.
fn thumb_point(ui: &MainWindow, row: usize, frac: f32) -> LogicalPosition {
    LogicalPosition::new(
        ui.get_thumb_list_x() + 90.0,
        ui.get_thumb_list_y()
            + ui.get_thumb_scroll_y()
            + (row as f32 + frac) * crate::THUMB_ROW_LOGICAL_PX,
    )
}

/// One left click at a window position.
fn click_at(ui: &MainWindow, position: LogicalPosition) {
    let window = ui.window();
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed {
        position,
        button: slint::platform::PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position,
        button: slint::platform::PointerEventButton::Left,
    });
}

/// One left click, with Shift held when asked.
fn select_click(ui: &MainWindow, at: (f32, usize), shift: bool) {
    let window = ui.window();
    let position = window_point(ui, at);
    let click = || {
        window.dispatch_event(WindowEvent::PointerMoved { position });
        window.dispatch_event(WindowEvent::PointerPressed {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
        window.dispatch_event(WindowEvent::PointerReleased {
            position,
            button: slint::platform::PointerEventButton::Left,
        });
    };
    if shift {
        with_modifier(ui, Key::Shift, click);
    } else {
        click();
    }
}

/// Whether the open document has unsaved changes.
fn with_dirty() -> bool {
    let mut dirty = false;
    with_state(|state| dirty = state.save_state.is_dirty());
    dirty
}

/// The most thumbnails held so far in this run.
fn with_thumbs_max() -> usize {
    let mut max = 0;
    with_state(|state| max = state.bench.thumbs_max());
    max
}

/// One `--bench-nav` step (Plan 0002, §6 row F). Keys and the wheel go through
/// `Window::dispatch_event`, i.e. the same `FocusScope` and `TouchArea` the user's input reaches;
/// the page field, the fit buttons and the scrollbar through the callbacks their widgets invoke.
#[derive(Debug, Clone, Copy)]
enum NavStep {
    Key(Key),
    Space { shift: bool },
    Ctrl(char),
    CtrlWheel(f32),
    GoTo(usize),
    FitWidth,
    FitPage,
    ScrollbarBottom,
    ScrollbarTop,
}

/// The `--bench-nav` script: every §4.6 action, starting from the document's top at 100%.
const NAV_STEPS: &[NavStep] = &[
    NavStep::Key(Key::End),
    NavStep::Key(Key::Home),
    NavStep::GoTo(900),
    NavStep::Key(Key::PageDown),
    NavStep::Key(Key::DownArrow),
    NavStep::Key(Key::UpArrow),
    NavStep::Key(Key::PageUp),
    NavStep::Space { shift: false },
    NavStep::Space { shift: true },
    NavStep::Ctrl('='),
    NavStep::Ctrl('-'),
    NavStep::Ctrl('-'),
    NavStep::Ctrl('0'),
    NavStep::CtrlWheel(60.0),
    NavStep::CtrlWheel(-30.0),
    NavStep::FitWidth,
    NavStep::FitPage,
    NavStep::Ctrl('0'),
    NavStep::ScrollbarBottom,
    NavStep::ScrollbarTop,
];

fn nav_step_name(step: NavStep) -> &'static str {
    match step {
        NavStep::Key(Key::End) => "End",
        NavStep::Key(Key::Home) => "Home",
        NavStep::Key(Key::PageDown) => "PgDn",
        NavStep::Key(Key::PageUp) => "PgUp",
        NavStep::Key(Key::DownArrow) => "Down",
        NavStep::Key(Key::UpArrow) => "Up",
        NavStep::Key(_) => "key",
        NavStep::Space { shift: false } => "Space",
        NavStep::Space { shift: true } => "Shift+Space",
        NavStep::Ctrl('=') => "Ctrl+=",
        NavStep::Ctrl('-') => "Ctrl+-",
        NavStep::Ctrl('0') => "Ctrl+0",
        NavStep::Ctrl(_) => "Ctrl",
        NavStep::CtrlWheel(d) if d > 0.0 => "Ctrl+wheel up",
        NavStep::CtrlWheel(_) => "Ctrl+wheel down",
        NavStep::GoTo(_) => "go to page",
        NavStep::FitWidth => "Fit width",
        NavStep::FitPage => "Fit page",
        NavStep::ScrollbarBottom => "scrollbar end",
        NavStep::ScrollbarTop => "scrollbar top",
    }
}

/// What the window shows now: the zoom and the page under the viewport centre, as the toolbar
/// shows them, and the scroll position in document pixels.
fn nav_observed(ui: &MainWindow) -> String {
    let mut out = String::new();
    with_state(|state| {
        out = format!(
            "zoom={} page={} y={:.0}",
            state.scale_milli,
            ui.get_current_page() + 1,
            state.pos.1
        );
    });
    out
}

fn press(ui: &MainWindow, text: SharedString) {
    let window = ui.window();
    window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    window.dispatch_event(WindowEvent::KeyReleased { text });
}

fn with_modifier(ui: &MainWindow, modifier: Key, f: impl FnOnce()) {
    let window = ui.window();
    let text: SharedString = char::from(modifier).into();
    window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    f();
    window.dispatch_event(WindowEvent::KeyReleased { text });
}

/// Performs one step and returns what the core math says it should produce, in the same format as
/// [`nav_observed`]. The prediction is computed *before* the input is sent, from the state the
/// input will act on, so a step that the input path drops or misroutes shows as a mismatch.
fn nav_perform(ui: &MainWindow, step: NavStep) -> String {
    let mut expected = String::new();
    with_state(|state| {
        let (_, view_h) = state.view_size(ui);
        let doc_h = state.layout.height() as f64;
        let line = LINE_STEP_LOGICAL_PX * state.sf();
        // Scroll steps also name the exact position they lead to, so a 40 px arrow step that
        // stays on the same page is still checked.
        let page_after = |milli: u32, pos_y: f64| {
            let margins = state.layout.margins();
            let layout = pdf_core::DocLayout::with_margins(
                &state.page_sizes,
                milli,
                state.gap_px(),
                margins,
            );
            let free_h = view_h - (margins.top + margins.bottom) as f64;
            let page = layout
                .page_near(pos_y + margins.top as f64 + free_h / 2.0)
                .unwrap_or(0)
                + 1;
            format!("zoom={milli} page={page} y={pos_y:.0}")
        };
        // The app pages and fits by the room the islands leave (`AppState::free_view`).
        let (free_w, free_h) = state.free_view(ui);
        // `scroll_to` then clamps to the whole viewport.
        let scroll = |action| {
            scroll_target(action, state.pos.1, free_h, doc_h, line)
                .clamp(0.0, (doc_h - view_h).max(0.0))
        };
        let current = state.current_page(ui);
        expected = match step {
            NavStep::Key(key) => {
                let action = match key {
                    Key::End => NavAction::End,
                    Key::Home => NavAction::Home,
                    Key::PageDown => NavAction::PageDown,
                    Key::PageUp => NavAction::PageUp,
                    Key::DownArrow => NavAction::LineDown,
                    _ => NavAction::LineUp,
                };
                page_after(state.scale_milli, scroll(action))
            }
            NavStep::Space { shift } => {
                let action = if shift {
                    NavAction::PageUp
                } else {
                    NavAction::PageDown
                };
                page_after(state.scale_milli, scroll(action))
            }
            NavStep::Ctrl(c) => {
                let milli = match c {
                    '=' | '+' => zoom_in_milli(state.scale_milli),
                    '-' => zoom_out_milli(state.scale_milli),
                    _ => ZOOM_DEFAULT_MILLI,
                };
                // A centre-anchored zoom keeps the centre page.
                format!("zoom={milli} page={}", current + 1)
            }
            NavStep::CtrlWheel(delta) => {
                let milli = wheel_zoom_milli(state.scale_milli, delta as f64);
                format!("zoom={milli} page={}", current + 1)
            }
            NavStep::GoTo(page) => format!("zoom={} page={page}", state.scale_milli),
            NavStep::FitWidth | NavStep::FitPage => {
                let size = state.page_sizes.get(current).copied();
                let gap = state.gap_px();
                let milli = size.map_or(state.scale_milli, |size| match step {
                    NavStep::FitWidth => fit_width_milli(&size, free_w, gap),
                    _ => fit_page_milli(&size, free_w, free_h, gap),
                });
                // Anchored at the page's top, so the page is still the one in view.
                format!("zoom={milli} page={}", current + 1)
            }
            NavStep::ScrollbarBottom => {
                format!("zoom={} page={}", state.scale_milli, state.layout.len())
            }
            NavStep::ScrollbarTop => format!("zoom={} page=1", state.scale_milli),
        };
    });
    match step {
        NavStep::Key(key) => press(ui, char::from(key).into()),
        NavStep::Space { shift: false } => press(ui, " ".into()),
        NavStep::Space { shift: true } => with_modifier(ui, Key::Shift, || press(ui, " ".into())),
        NavStep::Ctrl(c) => with_modifier(ui, Key::Control, || press(ui, c.into())),
        NavStep::CtrlWheel(delta) => {
            // The middle of the viewport: below the toolbar and above the status bar.
            let size = ui.window().size().to_logical(ui.window().scale_factor());
            let position = LogicalPosition::new(size.width / 2.0, size.height / 2.0);
            with_modifier(ui, Key::Control, || {
                ui.window().dispatch_event(WindowEvent::PointerScrolled {
                    position,
                    delta_x: 0.0,
                    delta_y: delta,
                });
            });
        }
        NavStep::GoTo(page) => ui.invoke_go_to_page(page.to_string().into()),
        NavStep::FitWidth => ui.invoke_fit_width(),
        NavStep::FitPage => ui.invoke_fit_page(),
        NavStep::ScrollbarBottom => {
            ui.invoke_scrollbar_moved(ui.get_sb_track_h(), ui.get_sb_track_h())
        }
        NavStep::ScrollbarTop => ui.invoke_scrollbar_moved(0.0, ui.get_sb_track_h()),
    }
    expected
}

impl Driver {
    /// Records the scenario's starting viewport once, so a repeat can return to it.
    fn remember_home(&mut self) {
        if self.home.is_some() || self.cfg.repeats <= 1 {
            return;
        }
        let mut home = None;
        with_state(|state| home = Some((state.scale_milli, state.pos)));
        self.home = home;
    }

    /// Ends the current phase and settles before the next one.
    fn settle(&mut self) {
        self.step = 0;
        self.phase = Phase::Settle;
        self.settle_ticks = settle_ticks(&self.cfg);
    }

    /// Ends the run in progress and puts the viewport back where the scenario started, so the next
    /// repeat measures the same thing: same scale, same offset, same steps.
    ///
    /// The reset raises a level change of its own, which would land in the next run's `t_blank`
    /// and in its abort list; the trace is suspended across it and resumes when the settle that
    /// follows has drained it.
    fn restart(&mut self, ui: &MainWindow) {
        with_state(|state| state.bench.close_run());
        self.pending = Pending::all(&self.cfg);
        self.settle();
        let Some((scale_milli, pos)) = self.home else {
            return;
        };
        with_state(|state| state.bench.suspend());
        self.resetting = true;
        // The cached tiles go with the viewport: a repeat that inherited the previous run's cache
        // would find every level already rendered and measure nothing at all.
        let (anchor_x, anchor_y) = viewport_center(ui);
        with_state(|state| {
            state.release_all(ui);
            state.zoom(ui, scale_milli, anchor_x, anchor_y);
            state.scroll_to(ui, pos);
        });
    }

    /// Starts the next pending phase, opening its trace window; returns whether there was one.
    fn next_phase(&mut self) -> bool {
        let Some(phase) = self.pending.take() else {
            return false;
        };
        let name = match phase {
            Phase::Jump => "jump",
            Phase::Zoom => "zoom",
            Phase::Pan => "pan",
            Phase::Scroll => "scroll",
            Phase::Nav => "nav",
            Phase::Reopen => "reopen",
            Phase::Thumbs => "thumbs",
            Phase::Outline => "outline",
            Phase::Search => "search",
            Phase::Select => "select",
            Phase::Edit => "edit",
            Phase::Note => "note",
            Phase::Ink => "ink",
            Phase::Pages => "pages",
            Phase::Tools => "tools",
            Phase::Stamps => "stamps",
            Phase::Convert => "convert",
            Phase::Forms => "forms",
            Phase::Settle => "settle",
        };
        // The window before this one (the startup, or the previous phase's settle) closes here:
        // from now on the counters describe this phase.
        with_state(|state| state.bench.phase(name));
        self.step = 0;
        self.phase = phase;
        true
    }

    /// Runs one tick and returns whether the run is over.
    fn tick(&mut self, ui: &MainWindow) -> bool {
        match self.phase {
            Phase::Jump => {
                self.remember_home();
                let page = self.cfg.jump_page.unwrap_or(1).saturating_sub(1);
                with_state(|state| state.go_to_page(ui, page));
                self.settle();
                false
            }
            Phase::Zoom => {
                self.remember_home();
                // The same call the `+ Zoom` button makes, at the viewport centre.
                let (anchor_x, anchor_y) = viewport_center(ui);
                with_state(|state| {
                    let next = zoom_in_milli(state.scale_milli);
                    state.zoom(ui, next, anchor_x, anchor_y);
                });
                self.step += 1;
                if self.step >= self.cfg.zoom_steps {
                    self.settle();
                }
                false
            }
            Phase::Pan => {
                self.remember_home();
                // Rightward, clamped to the document edge, starting from where the viewport
                // really is rather than from where the last write asked it to go.
                let mut done = false;
                let pan_px = self.cfg.pan_px as f64;
                with_state(|state| {
                    let (view_w, _) = state.view_size(ui);
                    let doc_w = state.layout.width() as f64;
                    let reach = (doc_w - view_w).max(0.0);
                    let next = (state.pos.0 + pan_px).min(reach);
                    state.bench.pan_step(
                        state.pos.0 as f32,
                        reach as f32,
                        doc_w as f32,
                        view_w as f32,
                        next as f32,
                    );
                    state.scroll_to(ui, (next, state.pos.1));
                    done = next >= reach;
                });
                self.step += 1;
                if self.step >= self.cfg.pan_steps || done {
                    self.settle();
                }
                false
            }
            Phase::Scroll => {
                self.remember_home();
                let dy = self.cfg.scroll_pxs * self.cfg.step_ms as f64 / 1000.0;
                let mut done = false;
                with_state(|state| {
                    let (_, view_h) = state.view_size(ui);
                    let reach = (state.layout.height() as f64 - view_h).max(0.0);
                    let next = (state.pos.1 + dy).min(reach);
                    state.scroll_to(ui, (state.pos.0, next));
                    done = next >= reach;
                });
                self.step += 1;
                let elapsed_ms = self.step as f64 * self.cfg.step_ms as f64;
                if done || elapsed_ms >= self.cfg.scroll_s * 1000.0 {
                    self.settle();
                }
                false
            }
            Phase::Thumbs => {
                if self.step == 0 {
                    ui.set_sidebar_visible(true);
                    ui.set_sidebar_tab(0);
                }
                let dy = (self.cfg.thumbs_pxs * self.cfg.step_ms as f64 / 1000.0) as f32;
                let rows = ui.get_thumbs().row_count() as f32;
                let end = -(rows * crate::THUMB_ROW_LOGICAL_PX - ui.get_view_h()).max(0.0);
                let next = (ui.get_thumb_scroll_y() - dy).max(end);
                ui.set_thumb_scroll_y(next);
                with_state(|state| state.pump_thumbs(ui));
                // One zoom-in step a second, so `t_blank` is measured while the pool also renders
                // thumbnails. Always inwards: a level zoomed back to would come from the cache and
                // log no `t_blank` at all. It stops changing at the zoom ceiling.
                let per_second = (1000 / self.cfg.step_ms.max(1)).max(1) as u32;
                if self.step % per_second == per_second - 1 {
                    let (anchor_x, anchor_y) = viewport_center(ui);
                    with_state(|state| {
                        let next = zoom_in_milli(state.scale_milli);
                        state.zoom(ui, next, anchor_x, anchor_y);
                    });
                }
                self.step += 1;
                let elapsed_ms = self.step as f64 * self.cfg.step_ms as f64;
                if next <= end || elapsed_ms >= self.cfg.scroll_s * 1000.0 {
                    let held = with_thumbs_max();
                    println!(
                        "[bench] thumbs scrolled to row {:.0} of {rows:.0}, max held {held}",
                        -next / crate::THUMB_ROW_LOGICAL_PX
                    );
                    self.settle();
                }
                false
            }
            Phase::Search => {
                if self.step == 0 {
                    let query = self.cfg.search.clone().unwrap_or_default();
                    with_state(|state| {
                        self.searches_before = state.bench.searches_done();
                        ui.set_find_visible(true);
                        ui.set_find_text(query.as_str().into());
                        state.start_search(ui, &query);
                    });
                }
                if self.step == 3
                    && let Some(replace) = self.cfg.search_replace.clone()
                {
                    with_state(|state| {
                        ui.set_find_text(replace.as_str().into());
                        state.start_search(ui, &replace);
                    });
                }
                let per_second = (1000 / self.cfg.step_ms.max(1)).max(1) as u32;
                if self.cfg.search_zoom && self.step % per_second == per_second - 1 {
                    let (anchor_x, anchor_y) = viewport_center(ui);
                    with_state(|state| {
                        let next = zoom_in_milli(state.scale_milli);
                        state.zoom(ui, next, anchor_x, anchor_y);
                    });
                }
                self.step += 1;
                let mut done = false;
                with_state(|state| {
                    // With a replacement, the first search is cancelled and never finishes.
                    let replaced = self.cfg.search_replace.is_some() && self.step > 3;
                    let alone = self.cfg.search_replace.is_none();
                    done =
                        (replaced || alone) && state.bench.searches_done() > self.searches_before;
                });
                let timed_out = self.step as u64 * self.cfg.step_ms > 300_000;
                if done || timed_out {
                    with_state(|state| {
                        let hits = state.search.hits().len();
                        let pages: std::collections::BTreeSet<u32> =
                            state.search.hits().iter().map(|h| h.page).collect();
                        println!(
                            "[bench] search_result hits={hits} pages_with_hits={} stale_refused={} \
                             counter={:?}{}",
                            pages.len(),
                            state.search.stale(),
                            ui.get_find_counter().as_str(),
                            if timed_out { " TIMED OUT" } else { "" }
                        );
                        if let Some(expected) = self.cfg.search_expect {
                            state.bench.nav_check(
                                "search hits",
                                &format!("hits={expected}"),
                                &format!("hits={hits}"),
                            );
                        }
                    });
                    // Keys go through `dispatch_event`, which runs the callbacks at once: they borrow
                    // the app state themselves, so this must happen outside `with_state`.
                    let mut hits = 0;
                    with_state(|state| hits = state.search.hits().len());
                    if self.cfg.search_keys && hits >= 3 {
                        let steps: [(&'static str, bool, Key, usize); 4] = [
                            ("Enter", false, Key::Return, 2),
                            ("Shift+Enter", true, Key::Return, 1),
                            ("F3", false, Key::F3, 2),
                            ("Shift+F3", true, Key::F3, 1),
                        ];
                        for (name, shift, key, expected) in steps {
                            let text: SharedString = char::from(key).into();
                            let send = || press(ui, text.clone());
                            if shift {
                                with_modifier(ui, Key::Shift, send);
                            } else {
                                send();
                            }
                            let got = ui.get_find_counter().to_string();
                            with_state(|state| {
                                state
                                    .bench
                                    .nav_check(name, &format!("{expected} of {hits}"), &got)
                            });
                        }
                    }
                    self.settle();
                }
                false
            }
            Phase::Select => {
                if let Some((step, expected)) = self.nav_expect.take() {
                    // The text and the highlight on screen: an empty selection must show no rows.
                    let mut got = String::new();
                    with_state(|state| got = state.sel_text.clone());
                    let rows = ui.get_sel_rects().row_count();
                    let shown = |text: &str| {
                        format!("{text:?} rows={}", if text.is_empty() { "0" } else { ">0" })
                    };
                    let got = format!("{got:?} rows={}", if rows == 0 { "0" } else { ">0" });
                    let expected = shown(&expected);
                    with_state(|state| state.bench.nav_check(step, &expected, &got));
                }
                // `select_*p.pdf`: page p, line l reads "p{p}l{l} alpha beta", baselines 12 pt
                // apart from 42 pt below the page top (selection_test.rs).
                let line = |l: usize| format!("p0l{l} alpha beta");
                let lines = |a: usize, b: usize| (a..=b).map(line).collect::<Vec<_>>().join("\n");
                let step = self.step;
                let (name, expected): (&'static str, String) = match step {
                    0 => {
                        // The text starts at x = 54 pt; the drag starts on its first character.
                        select_drag(ui, (55.0, 2), (600.0, 4));
                        ("drag lines 2-4", lines(2, 4))
                    }
                    1 => {
                        select_click(ui, (600.0, 6), true);
                        ("shift+click to 6", lines(2, 6))
                    }
                    2 => {
                        // Inside "alpha" (after "p0l9 ", ~25 pt of 10 pt Helvetica).
                        select_click(ui, (94.0, 9), false);
                        select_click(ui, (94.0, 9), false);
                        ("double-click word", "alpha".to_owned())
                    }
                    3 => {
                        select_click(ui, (94.0, 11), false);
                        select_click(ui, (94.0, 11), false);
                        select_click(ui, (94.0, 11), false);
                        ("triple-click line", line(11))
                    }
                    // The review's finding: a click or drag that starts away from the text selects
                    // nothing, and the text cursor shows only over text.
                    4 => {
                        select_click(ui, (20.0, 11), false);
                        ("click in the margin clears", String::new())
                    }
                    5 => {
                        select_drag(ui, (20.0, 2), (600.0, 4));
                        ("drag from the margin", String::new())
                    }
                    6 => {
                        select_drag(ui, (55.0, 2), (600.0, 4));
                        ("drag lines 2-4 again", lines(2, 4))
                    }
                    7 => {
                        select_click(ui, (94.0, 12), false);
                        ("click on text clears", String::new())
                    }
                    8 => {
                        for (name, at, expected) in [
                            ("cursor over text", (94.0, 9), true),
                            ("cursor in the margin", (20.0, 9), false),
                            ("cursor below the text", (94.0, 60), false),
                        ] {
                            let position = window_point(ui, at);
                            ui.window()
                                .dispatch_event(WindowEvent::PointerMoved { position });
                            let got = ui.get_text_cursor();
                            with_state(|state| {
                                state.bench.nav_check(
                                    name,
                                    &format!("text_cursor={expected}"),
                                    &format!("text_cursor={got}"),
                                )
                            });
                        }
                        self.step += 1;
                        return false;
                    }
                    _ => {
                        self.settle();
                        return false;
                    }
                };
                self.nav_expect = Some((name, expected));
                self.step += 1;
                false
            }
            Phase::Note => {
                // `select_*p.pdf` page 1: line 30 is at 399 pt from the top, inside the default
                // window's viewport; x = 300 pt is beside its text, so the note goes on empty
                // paper.
                const TEXT: &str = "Öğrenci notu ✓";
                let check = |name: &str, expected: String, got: String| {
                    with_state(|state| state.bench.nav_check(name, &expected, &got));
                };
                let note = || {
                    let mut found = None;
                    with_state(|state| {
                        found = state.annots.get(&0).and_then(|items| {
                            items
                                .iter()
                                .find(|item| item.kind == pdf_core::annot::AnnotKind::Note)
                                .cloned()
                        });
                    });
                    found
                };
                let at = |n: &Option<pdf_core::annot::AnnotInfo>| {
                    n.as_ref().map_or_else(
                        || "none".to_owned(),
                        |n| format!("({:.0},{:.0})", n.rect.x, n.rect.y),
                    )
                };
                match self.step {
                    0 => {
                        ui.invoke_choose_tool(5);
                        select_click(ui, (300.0, 30), false);
                    }
                    1 => {
                        check(
                            "Note tool opens the editor",
                            "editor=true".to_owned(),
                            format!("editor={}", ui.get_note_editor_visible()),
                        );
                        ui.set_note_text(TEXT.into());
                        ui.invoke_note_save();
                    }
                    2 => {
                        let n = note();
                        check(
                            "the note is placed with its text",
                            format!("{TEXT} at (300,399)"),
                            format!(
                                "{} at {}",
                                n.as_ref()
                                    .and_then(|n| n.contents.clone())
                                    .unwrap_or_default(),
                                at(&n)
                            ),
                        );
                        ui.invoke_choose_tool(0);
                        let position = window_point(ui, (305.0, 30));
                        ui.window()
                            .dispatch_event(WindowEvent::PointerMoved { position });
                        check(
                            "hovering shows its text",
                            format!("visible=true {TEXT}"),
                            format!(
                                "visible={} {}",
                                ui.get_note_hover_visible(),
                                ui.get_note_hover_text()
                            ),
                        );
                        // Grabbed 5 pt right of its top-left corner, dropped 100 pt right and on
                        // line 10 (159 pt).
                        select_drag(ui, (305.0, 30), (405.0, 10));
                    }
                    3 => {
                        check("a drag moves it", "(400,159)".to_owned(), at(&note()));
                        with_modifier(ui, Key::Control, || press(ui, "z".into()));
                    }
                    4 => {
                        check("Ctrl+Z puts it back", "(300,399)".to_owned(), at(&note()));
                    }
                    _ => {
                        self.settle();
                        return false;
                    }
                }
                self.step += 1;
                false
            }
            Phase::Forms => {
                let check = |name: &str, expected: String, got: String| {
                    with_state(|state| state.bench.nav_check(name, &expected, &got));
                };
                // A field's value on page 1, once the page's fields are in; `name[]` is a list's
                // chosen values, `@focus` the page and name of the field with the focus.
                let value = |name: &str| {
                    let mut out = None;
                    with_state(|state| {
                        if name == "@fields" {
                            out = state.fields.get(&0).map(|f| f.len().to_string());
                            return;
                        }
                        if name == "@focus" {
                            out = state.field_focus.and_then(|(page, i)| {
                                let f = state.fields.get(&page)?.get(i)?;
                                Some(format!("{}:{}", page + 1, f.name))
                            });
                            return;
                        }
                        let (name, list) =
                            name.strip_suffix("[]").map_or((name, false), |n| (n, true));
                        out = state.fields.get(&0).and_then(|fields| {
                            fields.iter().find(|f| f.name == name).map(|f| {
                                if list {
                                    f.values.join(",")
                                } else {
                                    f.value.clone()
                                }
                            })
                        });
                    });
                    out
                };
                // The centre of a field's widget (the `nth` with that name) in the viewport.
                let centre = |name: &str, nth: usize| {
                    let mut out = None;
                    with_state(|state| {
                        let rect = state.fields.get(&0).and_then(|fields| {
                            fields
                                .iter()
                                .filter(|f| f.name == name)
                                .nth(nth)
                                .map(|f| f.rect)
                        });
                        out = rect.and_then(|r| {
                            state.viewport_point(0, r.x + r.width / 2.0, r.y + r.height / 2.0)
                        });
                    });
                    out
                };
                // One click through the window's input path, as the user's reaches the page.
                let click = |(x, y): (f32, f32)| {
                    let position = LogicalPosition::new(
                        ui.get_viewport_abs_x() + x,
                        ui.get_viewport_abs_y() + y,
                    );
                    click_at(ui, position);
                };
                // Each step waits for the value the last one expects.
                if let Some((name, field, expected)) = self.form_expect.take() {
                    match value(field) {
                        Some(got) if got == expected => check(name, expected, got),
                        _ if Instant::now() < self.form_deadline => {
                            self.form_expect = Some((name, field, expected));
                            return false;
                        }
                        got => check(name, expected, got.unwrap_or_default()),
                    }
                }
                let expect = |s: &mut Self, name: &'static str, field: &'static str, v: &str| {
                    s.form_expect = Some((name, field, v.to_owned()));
                    s.form_deadline = Instant::now() + Duration::from_secs(5);
                };
                match self.step {
                    0 => {
                        if value("agree").is_none() {
                            return false;
                        }
                        // Every fillable field of the near pages is shaded before anything is clicked: 11 on
                        // page 1 and "shared" on page 2.
                        check(
                            "fields shaded before a click",
                            "12".into(),
                            ui.get_field_rects().row_count().to_string(),
                        );
                        let Some(at) = centre("agree", 0) else {
                            self.settle();
                            return false;
                        };
                        click(at);
                        expect(self, "click the check box", "agree", "Yes");
                    }
                    1 => {
                        let Some(at) = centre("choice", 1) else {
                            self.settle();
                            return false;
                        };
                        click(at);
                        expect(self, "click the second radio button", "choice", "b");
                    }
                    // A click as a hand makes one: the button stays down for 150 ms, longer than
                    // the Flickable holds a press back to see whether it becomes a drag.
                    2 => {
                        let Some((x, y)) = centre("name", 0) else {
                            self.settle();
                            return false;
                        };
                        let position = LogicalPosition::new(
                            ui.get_viewport_abs_x() + x,
                            ui.get_viewport_abs_y() + y,
                        );
                        ui.window()
                            .dispatch_event(WindowEvent::PointerMoved { position });
                        ui.window().dispatch_event(WindowEvent::PointerPressed {
                            position,
                            button: slint::platform::PointerEventButton::Left,
                        });
                        self.form_press = Some((position, Instant::now()));
                    }
                    3 => {
                        let Some((position, at)) = self.form_press else {
                            return false;
                        };
                        if at.elapsed() < Duration::from_millis(150) {
                            return false;
                        }
                        ui.window().dispatch_event(WindowEvent::PointerReleased {
                            position,
                            button: slint::platform::PointerEventButton::Left,
                        });
                    }
                    4 => {
                        let open = ui.get_field_editor_visible();
                        check(
                            "the editor opens over the field",
                            "true".into(),
                            open.to_string(),
                        );
                        // Typed through the window: the editor has the keyboard after one click.
                        for c in "Öğüt Şükrü İnce".chars() {
                            press(ui, c.to_string().into());
                        }
                        press(ui, Key::Return.into());
                        expect(self, "type Turkish text, Enter", "name", "Öğüt Şükrü İnce");
                    }
                    5 => {
                        let Some(at) = centre("city", 0) else {
                            self.settle();
                            return false;
                        };
                        click(at);
                        let open = ui.get_field_choices_visible();
                        check(
                            "the options open under the field",
                            "true".into(),
                            open.to_string(),
                        );
                        ui.invoke_field_choose(2);
                        expect(self, "choose the third city", "city", "Muğla");
                    }
                    // A comb field: the typed characters land one per box, and no more than it takes.
                    6 => {
                        let Some(at) = centre("code", 0) else {
                            self.settle();
                            return false;
                        };
                        click(at);
                        for c in "AB12345".chars() {
                            press(ui, c.to_string().into());
                        }
                        let boxes: Vec<String> = ui
                            .get_field_editor_chars()
                            .iter()
                            .map(|c| c.to_string())
                            .collect();
                        check(
                            "comb boxes as typed",
                            "6:A,B,1,2,3,4".into(),
                            format!("{}:{}", ui.get_field_editor_cells(), boxes.join(",")),
                        );
                        press(ui, Key::Return.into());
                        expect(self, "comb field filled", "code", "AB1234");
                    }
                    7 => {
                        with_modifier(ui, Key::Control, || press(ui, "z".into()));
                        for _ in 0..4 {
                            with_modifier(ui, Key::Control, || press(ui, "z".into()));
                        }
                        expect(self, "undo all four (check box off)", "agree", "Off");
                    }
                    8 => {
                        check(
                            "undo all four (name empty)",
                            String::new(),
                            value("name").unwrap_or_else(|| "?".to_owned()),
                        );
                    }
                    // Phase 2, the keyboard. The page is tabbed by rows: name, code, notes, agree,
                    // the two radio buttons, city, langs, a, b, shared.
                    9 => {
                        let Some(at) = centre("notes", 0) else {
                            self.settle();
                            return false;
                        };
                        click(at);
                        press(ui, Key::Tab.into());
                        expect(
                            self,
                            "Tab from a text field to the check box",
                            "@focus",
                            "1:agree",
                        );
                    }
                    10 => {
                        check(
                            "no editor over a check box",
                            "false".into(),
                            ui.get_field_editor_visible().to_string(),
                        );
                        press(ui, " ".into());
                        expect(self, "Space sets the check box", "agree", "Yes");
                    }
                    11 => {
                        press(ui, Key::Tab.into());
                        press(ui, " ".into());
                        expect(self, "Tab, Space: the first radio button", "choice", "a");
                    }
                    12 => {
                        press(ui, Key::Tab.into());
                        press(ui, Key::Tab.into());
                        expect(self, "Tab twice: the combo box", "@focus", "1:city");
                    }
                    13 => {
                        check(
                            "its options open",
                            "true".into(),
                            ui.get_field_choices_visible().to_string(),
                        );
                        press(ui, Key::DownArrow.into());
                        press(ui, Key::DownArrow.into());
                        press(ui, Key::Return.into());
                        expect(self, "Down, Down, Enter: the third city", "city", "Muğla");
                    }
                    14 => {
                        press(ui, Key::Tab.into());
                        expect(self, "Tab: the multi-select list", "@focus", "1:langs");
                    }
                    15 => {
                        press(ui, " ".into());
                        press(ui, Key::DownArrow.into());
                        press(ui, Key::DownArrow.into());
                        press(ui, " ".into());
                        press(ui, Key::Return.into());
                        expect(self, "tick the first and third, Enter", "langs[]", "tr,de");
                    }
                    16 => {
                        ui.invoke_reset_form();
                        expect(self, "Reset Form: the city's default", "city", "Ankara");
                    }
                    17 => {
                        check(
                            "Reset Form: the list and the check box",
                            "|Off".into(),
                            format!(
                                "{}|{}",
                                value("langs[]").unwrap_or_default(),
                                value("agree").unwrap_or_default()
                            ),
                        );
                        with_modifier(ui, Key::Control, || press(ui, "z".into()));
                        expect(self, "one undo brings the form back", "langs[]", "tr,de");
                    }
                    18 => {
                        let Some(at) = centre("shared", 0) else {
                            self.settle();
                            return false;
                        };
                        click(at);
                        press(ui, Key::Tab.into());
                        expect(
                            self,
                            "Tab from page 1's last field to page 2",
                            "@focus",
                            "2:shared",
                        );
                    }
                    19 => {
                        check(
                            "the editor opens on page 2",
                            "true".into(),
                            ui.get_field_editor_visible().to_string(),
                        );
                        with_modifier(ui, Key::Shift, || press(ui, Key::Tab.into()));
                        expect(self, "Shift+Tab goes back to page 1", "@focus", "1:shared");
                    }
                    // From one text field to the next, the editor stays and takes the keys again.
                    20 => {
                        let Some(at) = centre("a", 0) else {
                            self.settle();
                            return false;
                        };
                        click(at);
                        press(ui, Key::Tab.into());
                        for c in "77".chars() {
                            press(ui, c.to_string().into());
                        }
                        press(ui, Key::Return.into());
                        expect(self, "Tab to the next text field and type", "b", "77");
                    }
                    // Phase 3: export, reset, import, flatten.
                    21 => {
                        with_state(|state| {
                            if let Some(path) = form_data_path(state) {
                                let _ = std::fs::remove_file(&path);
                                state.export_form_to(path);
                            }
                        });
                        self.form_deadline = Instant::now() + Duration::from_secs(5);
                    }
                    22 => {
                        let path = with_state_ret(|state| form_data_path(state)).flatten();
                        let text = path
                            .as_deref()
                            .and_then(|p| std::fs::read_to_string(p).ok());
                        if text.is_none() && Instant::now() < self.form_deadline {
                            return false;
                        }
                        check(
                            "Export Data writes the values as JSON",
                            "true".into(),
                            text.is_some_and(|t| t.contains("\"b\": \"77\""))
                                .to_string(),
                        );
                        ui.invoke_reset_form();
                        expect(self, "Reset Form before the import", "b", "");
                    }
                    23 => {
                        let read = with_state_ret(|state| {
                            let path = form_data_path(state)?;
                            state.import_form_from(ui, &path).ok()
                        })
                        .flatten();
                        check(
                            "Import Data reads the file",
                            "true".into(),
                            read.is_some_and(|n| n > 0).to_string(),
                        );
                        expect(self, "Import Data sets the values again", "b", "77");
                    }
                    24 => {
                        check(
                            "the check box and the list too",
                            "Yes|tr,de".into(),
                            format!(
                                "{}|{}",
                                value("agree").unwrap_or_default(),
                                value("langs[]").unwrap_or_default()
                            ),
                        );
                        ui.invoke_flatten_form();
                        expect(self, "Flatten Form: no fields left", "@fields", "0");
                    }
                    25 => {
                        check(
                            "no field shaded",
                            "0".into(),
                            ui.get_field_rects().row_count().to_string(),
                        );
                        with_modifier(ui, Key::Control, || press(ui, "z".into()));
                        expect(self, "one undo brings the form back", "b", "77");
                    }
                    _ => {
                        self.settle();
                        return false;
                    }
                }
                self.step += 1;
                false
            }
            Phase::Convert => {
                let check = |name: &str, expected: String, got: String| {
                    with_state(|state| state.bench.nav_check(name, &expected, &got));
                };
                // Wait for the job or the open the last step started.
                let mut done = 0;
                let mut path = None;
                with_state(|state| {
                    done = state.bench.tools_done();
                    path = state.doc_path.clone();
                });
                if done < self.tools_wait {
                    return false;
                }
                if let Some(pages) = self.open_wait {
                    if ui.get_total_pages() != pages || ui.get_is_loading() {
                        return false;
                    }
                    self.open_wait = None;
                }
                let Some(dir) = path.as_deref().and_then(std::path::Path::parent) else {
                    self.settle();
                    return false;
                };
                let compressed = dir.join("bench_compressed.pdf");
                let images = [dir.join("bench_img_6.png"), dir.join("bench_img_1.png")];
                let album = dir.join("bench_images.pdf");
                let start = |job: pdf_core::tools::ToolJob| {
                    with_state(|state| {
                        state.start_tool(ui, job, false);
                    });
                };
                match self.step {
                    0 => {
                        let Some(source) = path.clone() else {
                            self.settle();
                            return false;
                        };
                        self.base_size = std::fs::metadata(&source)
                            .map_or(0, |m| m.len())
                            .to_string();
                        start(pdf_core::tools::ToolJob::Compress {
                            input: pdf_core::tools::MergeInput::Current,
                            preset: pdf_core::tools::Preset::Lossless,
                            output: compressed.clone(),
                        });
                        self.tools_wait = done + 1;
                    }
                    1 => {
                        let before: u64 = self.base_size.parse().unwrap_or(0);
                        let after = std::fs::metadata(&compressed).map_or(u64::MAX, |m| m.len());
                        check(
                            "compressed copy not larger",
                            "true".to_owned(),
                            (after <= before).to_string(),
                        );
                        start(pdf_core::tools::ToolJob::ExportImages {
                            pages: vec![(5, images[0].clone()), (0, images[1].clone())],
                            format: pdf_core::tools::ImageFormat::Png,
                            dpi: 150,
                        });
                        self.tools_wait = done + 1;
                    }
                    2 => {
                        start(pdf_core::tools::ToolJob::ImagesToPdf {
                            images: images.to_vec(),
                            output: album.clone(),
                        });
                        self.tools_wait = done + 1;
                    }
                    3 => {
                        with_state(|state| state.open_path(ui, album.clone()));
                        self.open_wait = Some(2);
                    }
                    4 => {
                        check(
                            "pages 6 and 1 as images, then a PDF again",
                            order_str(2, vec!["5".to_owned(), "0".to_owned()]),
                            page_order(),
                        );
                        // Phase 5: that PDF as Word, in a build with it.
                        if cfg!(feature = "docx") {
                            let _ = std::fs::remove_file(dir.join("bench_images.docx"));
                            start(pdf_core::tools::ToolJob::ExportWord {
                                output: dir.join("bench_images.docx"),
                            });
                            self.tools_wait = done + 1;
                        }
                    }
                    5 if cfg!(feature = "docx") => {
                        let docx = std::fs::read(dir.join("bench_images.docx")).unwrap_or_default();
                        check(
                            "exported as Word",
                            "true".to_owned(),
                            docx.starts_with(b"PK").to_string(),
                        );
                    }
                    _ => {
                        self.settle();
                        return false;
                    }
                }
                self.step += 1;
                false
            }
            Phase::Stamps => {
                let check = |name: &str, expected: String, got: String| {
                    with_state(|state| state.bench.nav_check(name, &expected, &got));
                };
                let mut rebuilt = 0;
                let mut first = String::new();
                let mut count = 0;
                with_state(|state| {
                    rebuilt = state.bench.rebuilds();
                    count = state.page_sizes.len();
                    if let Some(size) = state.page_sizes.first() {
                        first = format!("{:.0}x{:.0}", size.width, size.height);
                    }
                });
                // Each step waits for the layout rebuilds of the one before.
                if rebuilt < self.pages_rebuilds {
                    return false;
                }
                if let Some((name, expected)) = self.nav_expect.take() {
                    check(name, expected, format!("{count} pages, page 1 {first}"));
                }
                if self.step == 0 {
                    self.pages_base = count as u32;
                    self.open_wait = None;
                }
                let base = self.pages_base;
                let opened = |size: &str| format!("{base} pages, page 1 {size}");
                match self.step {
                    0 => {
                        self.tools_wait = 0;
                        // The page as opened, for the undo check.
                        self.nav_expect = Some(("opened", opened(&first)));
                        with_state(|state| state.bench.time_restructures(true));
                        ui.set_stamp_mode(0);
                        ui.set_stamp_text("TASLAK ĞŞİ".into());
                        ui.set_stamp_pages("".into());
                        ui.invoke_stamp_apply();
                        self.pages_rebuilds = rebuilt + 1;
                        self.base_size = first;
                    }
                    1 => {
                        ui.set_stamp_mode(1);
                        ui.set_stamp_format("Sayfa {n} / {total}".into());
                        ui.invoke_stamp_apply();
                        self.pages_rebuilds = rebuilt + 1;
                        self.nav_expect =
                            Some(("watermark and page numbers", opened(&self.base_size)));
                    }
                    2 => {
                        // Draw 200 x 340 points of page 1 with the Crop tool, from (50, 60).
                        ui.invoke_crop_tool();
                        let mut from = None;
                        let mut to = None;
                        with_state(|state| {
                            from = state.viewport_point(0, 50.0, 60.0);
                            to = state.viewport_point(0, 250.0, 400.0);
                        });
                        let (Some(from), Some(to)) = (from, to) else {
                            self.settle();
                            return false;
                        };
                        ui.invoke_select_pointer(0, from.0, from.1, false, false);
                        for k in 1..=10 {
                            let f = k as f32 / 10.0;
                            ui.invoke_select_pointer(
                                2,
                                from.0 + (to.0 - from.0) * f,
                                from.1 + (to.1 - from.1) * f,
                                false,
                                false,
                            );
                        }
                        ui.invoke_select_pointer(1, to.0, to.1, false, false);
                        ui.invoke_crop_apply(false);
                        self.pages_rebuilds = rebuilt + 1;
                        self.nav_expect = Some(("crop page 1 with the pointer", opened("200x340")));
                    }
                    3 => {
                        for _ in 0..3 {
                            with_modifier(ui, Key::Control, || press(ui, "z".into()));
                        }
                        self.pages_rebuilds = rebuilt + 3;
                        self.nav_expect = Some(("undo all three", opened(&self.base_size)));
                    }
                    _ => {
                        self.settle();
                        return false;
                    }
                }
                self.step += 1;
                false
            }
            Phase::Tools => {
                let check = |name: &str, expected: String, got: String| {
                    with_state(|state| state.bench.nav_check(name, &expected, &got));
                };
                // Wait for the job or the open the last step started.
                let mut done = 0;
                let mut path = None;
                with_state(|state| {
                    done = state.bench.tools_done();
                    path = state.doc_path.clone();
                });
                if done < self.tools_wait {
                    return false;
                }
                if let Some(pages) = self.open_wait {
                    if ui.get_total_pages() != pages || ui.get_is_loading() {
                        return false;
                    }
                    self.open_wait = None;
                }
                let Some(dir) = path.as_deref().and_then(std::path::Path::parent) else {
                    self.settle();
                    return false;
                };
                let merged = dir.join("bench_merged.pdf");
                let part_a = dir.join("bench_part_a.pdf");
                let part_b = dir.join("bench_part_b.pdf");
                let n = ui.get_total_pages();
                match self.step {
                    0 => {
                        self.pages_base = n as u32;
                        with_state(|state| {
                            state.bench.pages_memory(0);
                            state.start_tool(
                                ui,
                                pdf_core::tools::ToolJob::Merge {
                                    inputs: vec![
                                        pdf_core::tools::MergeInput::Current,
                                        pdf_core::tools::MergeInput::Current,
                                    ],
                                    output: merged.clone(),
                                },
                                false,
                            );
                        });
                        self.tools_wait = done + 1;
                    }
                    1 => {
                        let base = self.pages_base as i32;
                        with_state(|state| state.open_path(ui, merged.clone()));
                        self.open_wait = Some(2 * base);
                    }
                    2 => {
                        let base = self.pages_base as usize;
                        let all: Vec<String> = (0..base).map(|p| p.to_string()).collect();
                        let mut twice = all.clone();
                        twice.extend(all.iter().cloned());
                        let mut second_half = String::new();
                        with_state(|state| {
                            if let Some(size) = state.page_sizes.get(base) {
                                second_half = format!("{}", size.height.round() as i64 - 800);
                            }
                        });
                        check(
                            "merged with itself",
                            format!("{} second half starts at 0", order_str(twice.len(), twice)),
                            format!("{} second half starts at {second_half}", page_order()),
                        );
                        // Two parts of the merged document: its last page then its first, and
                        // page 6 alone.
                        let last = (2 * base - 1) as u32;
                        with_state(|state| {
                            state.start_tool(
                                ui,
                                pdf_core::tools::ToolJob::Extract {
                                    parts: vec![
                                        (vec![last, 0], part_a.clone()),
                                        (vec![5], part_b.clone()),
                                    ],
                                },
                                false,
                            );
                        });
                        self.tools_wait = done + 1;
                    }
                    3 => {
                        with_state(|state| state.open_path(ui, part_a.clone()));
                        self.open_wait = Some(2);
                    }
                    4 => {
                        let last = (self.pages_base - 1).to_string();
                        check(
                            "part a: last page, then first",
                            order_str(2, vec![last, "0".to_owned()]),
                            page_order(),
                        );
                        with_state(|state| state.open_path(ui, part_b.clone()));
                        self.open_wait = Some(1);
                    }
                    5 => {
                        check(
                            "part b: page 6",
                            order_str(1, vec!["5".to_owned()]),
                            page_order(),
                        );
                    }
                    _ => {
                        self.settle();
                        return false;
                    }
                }
                self.step += 1;
                false
            }
            Phase::Pages => {
                let check = |name: &str, expected: String, got: String| {
                    with_state(|state| state.bench.nav_check(name, &expected, &got));
                };
                let undo = || with_modifier(ui, Key::Control, || press(ui, "z".into()));
                let rebuilds = || {
                    let mut n = 0;
                    with_state(|state| n = state.bench.rebuilds());
                    n
                };
                // The previous step's expectation, checked now that its answer had a tick.
                if let Some((name, expected)) = self.nav_expect.take() {
                    check(name, expected, page_order());
                }
                // Every step starts from the document as opened (the step before it is undone).
                if self.step == 0 {
                    self.pages_base = ui.get_total_pages() as u32;
                }
                let ident = |pages: Vec<String>| order_str(pages.len(), pages);
                let all: Vec<String> = (0..self.pages_base).map(|p| p.to_string()).collect();
                let with = |f: &dyn Fn(&mut Vec<String>)| {
                    let mut v = all.clone();
                    f(&mut v);
                    v
                };
                let (name, expected): (&'static str, Vec<String>) = match self.step {
                    0 => {
                        ui.set_sidebar_visible(true);
                        ui.set_sidebar_tab(0);
                        ui.invoke_sidebar_tab_changed();
                        ("opened", all.clone())
                    }
                    1 => {
                        ui.invoke_thumb_clicked(0, false, false);
                        ui.invoke_page_op(1);
                        ("rotate page 1 right", with(&|v| v[0] = "0r".into()))
                    }
                    2 => {
                        undo();
                        ("undo the rotation", all.clone())
                    }
                    3 => {
                        ui.invoke_thumb_clicked(2, false, false);
                        ui.invoke_thumb_clicked(4, false, true);
                        ui.invoke_page_op(3);
                        (
                            "delete pages 3-5",
                            with(&|v| {
                                v.drain(2..5);
                            }),
                        )
                    }
                    4 => {
                        undo();
                        ("undo the delete", all.clone())
                    }
                    5 => {
                        ui.invoke_thumb_clicked(1, false, false);
                        ui.invoke_page_op(2);
                        ("duplicate page 2", with(&|v| v.insert(2, "1".into())))
                    }
                    6 => {
                        undo();
                        ("undo the duplicate", all.clone())
                    }
                    7 => {
                        ui.invoke_thumb_clicked(0, false, false);
                        ui.invoke_page_op(5);
                        ("move page 1 later", with(&|v| v.swap(0, 1)))
                    }
                    8 => {
                        undo();
                        ("undo the move", all.clone())
                    }
                    9 => {
                        ui.invoke_thumb_clicked(0, false, false);
                        ui.invoke_page_op(6);
                        // As tall as the page it follows, so it reads as that page.
                        (
                            "insert a blank page after 1",
                            with(&|v| v.insert(1, "0".into())),
                        )
                    }
                    10 => {
                        undo();
                        ("undo the blank page", all.clone())
                    }
                    11 => {
                        // Drag thumbnail 1 into the lower half of thumbnail 2 with the mouse (two
                        // rows fit the default window).
                        let from = thumb_point(ui, 0, 0.5);
                        let to = thumb_point(ui, 1, 0.8);
                        let window = ui.window();
                        window.dispatch_event(WindowEvent::PointerMoved { position: from });
                        window.dispatch_event(WindowEvent::PointerPressed {
                            position: from,
                            button: slint::platform::PointerEventButton::Left,
                        });
                        for k in 1..=20 {
                            let f = k as f32 / 20.0;
                            let position = LogicalPosition::new(
                                from.x + (to.x - from.x) * f,
                                from.y + (to.y - from.y) * f,
                            );
                            window.dispatch_event(WindowEvent::PointerMoved { position });
                        }
                        window.dispatch_event(WindowEvent::PointerReleased {
                            position: to,
                            button: slint::platform::PointerEventButton::Left,
                        });
                        ("drag page 1 after page 2", with(&|v| v.swap(0, 1)))
                    }
                    12 => {
                        undo();
                        ("undo the drag", all.clone())
                    }
                    // Row AG: the working set before and 2 s after 100 structural edits (each turns
                    // page 1, so the hundredth leaves it as it was). The burst is not timed: its
                    // edits queue behind each other.
                    13 => {
                        with_state(|state| {
                            state.bench.pages_memory(0);
                            state.bench.time_restructures(false);
                        });
                        ui.invoke_thumb_clicked(0, false, false);
                        for _ in 0..100 {
                            ui.invoke_page_op(1);
                        }
                        self.pages_rebuilds = rebuilds() + 100;
                        self.step += 1;
                        return false;
                    }
                    14 => {
                        // Every answer in, then the 2 s.
                        if rebuilds() < self.pages_rebuilds {
                            return false;
                        }
                        self.edit_hold = Some((Instant::now() + Duration::from_secs(2), 100));
                        ("100 rotations", all.clone())
                    }
                    15 => {
                        if let Some((until, n)) = self.edit_hold {
                            if Instant::now() < until {
                                return false;
                            }
                            self.edit_hold = None;
                            with_state(|state| state.bench.pages_memory(n));
                        }
                        self.settle();
                        return false;
                    }
                    _ => {
                        self.settle();
                        return false;
                    }
                };
                self.nav_expect = Some((name, ident(expected)));
                self.step += 1;
                false
            }
            Phase::Ink => {
                // `select_*p.pdf` page 1, inside the default window's viewport: a wave from
                // x = 100 to 500 pt around y = 220 pt.
                const POINTS: usize = 2000;
                let wave = |i: usize| {
                    let t = i as f32 / (POINTS - 1) as f32;
                    (100.0 + 400.0 * t, 220.0 + 60.0 * (t * 10.0).sin())
                };
                let check = |name: &str, expected: String, got: String| {
                    with_state(|state| state.bench.nav_check(name, &expected, &got));
                };
                let inks = || {
                    let mut found = Vec::new();
                    with_state(|state| {
                        found = state.annots.get(&0).map_or_else(Vec::new, |items| {
                            items
                                .iter()
                                .filter(|item| item.kind == pdf_core::annot::AnnotKind::Ink)
                                .cloned()
                                .collect()
                        });
                    });
                    found
                };
                match self.step {
                    0 => {
                        ui.invoke_choose_tool(6);
                        let window = ui.window();
                        let (x, y) = wave(0);
                        let start = page_window_point(ui, x, y);
                        window.dispatch_event(WindowEvent::PointerMoved { position: start });
                        window.dispatch_event(WindowEvent::PointerPressed {
                            position: start,
                            button: slint::platform::PointerEventButton::Left,
                        });
                        let mut end = start;
                        for i in 1..POINTS {
                            let (x, y) = wave(i);
                            end = page_window_point(ui, x, y);
                            window.dispatch_event(WindowEvent::PointerMoved { position: end });
                        }
                        window.dispatch_event(WindowEvent::PointerReleased {
                            position: end,
                            button: slint::platform::PointerEventButton::Left,
                        });
                    }
                    1 => {
                        let found = inks();
                        let mut sent = None;
                        with_state(|state| sent = state.bench.last_ink().map(<[_]>::to_vec));
                        let equal = match (found.first(), sent) {
                            (Some(ink), Some(sent)) => {
                                ink.strokes.len() == 1
                                    && ink.strokes[0].len() == sent.len()
                                    && ink.strokes[0].iter().zip(&sent).all(|(a, b)| {
                                        (a.0 - b.0).abs() < 0.01 && (a.1 - b.1).abs() < 0.01
                                    })
                            }
                            _ => false,
                        };
                        check(
                            "the stroke is committed simplified",
                            "ink=1 equal=true live=false".to_owned(),
                            format!(
                                "ink={} equal={equal} live={}",
                                found.len(),
                                !ui.get_ink_commands().is_empty()
                            ),
                        );
                        with_modifier(ui, Key::Control, || press(ui, "z".into()));
                    }
                    2 => {
                        check(
                            "Ctrl+Z removes it",
                            "ink=0".to_owned(),
                            format!("ink={}", inks().len()),
                        );
                        with_modifier(ui, Key::Control, || press(ui, "y".into()));
                    }
                    3 => {
                        check(
                            "Ctrl+Y brings it back",
                            "ink=1".to_owned(),
                            format!("ink={}", inks().len()),
                        );
                        // Select: a click on the line selects the stroke; one inside its bounds
                        // but away from the line (a trough of the wave, 100 pt below a crest)
                        // does not.
                        ui.invoke_choose_tool(0);
                        let (x, y) = wave(POINTS / 2);
                        let position = page_window_point(ui, x, y);
                        click_at(ui, position);
                    }
                    4 => {
                        let on_line = ui.get_has_annot_selection();
                        let crest = (0..POINTS).map(wave).fold((0.0_f32, f32::MAX), |acc, p| {
                            if p.1 < acc.1 { p } else { acc }
                        });
                        let position = page_window_point(ui, crest.0, crest.1 + 100.0);
                        click_at(ui, position);
                        let off_line = ui.get_has_annot_selection();
                        check(
                            "a click selects ink on its line only",
                            "on_line=true off_line=false".to_owned(),
                            format!("on_line={on_line} off_line={off_line}"),
                        );
                    }
                    _ => {
                        self.settle();
                        return false;
                    }
                }
                self.step += 1;
                false
            }
            Phase::Edit => {
                let edits = self.cfg.edits;
                if let Some((until, n)) = self.edit_hold {
                    if Instant::now() < until {
                        return false;
                    }
                    self.edit_hold = None;
                    with_state(|state| state.bench.edit_memory(n));
                }
                if self.step == 0 {
                    // The tool stays on, so each drag below marks the line it selects.
                    ui.invoke_choose_tool(1);
                    with_state(|state| {
                        state.bench.requests_during_edits(true);
                        state.bench.edit_memory(0);
                    });
                }
                if self.step < edits {
                    let before = self.step;
                    // Lines 2..=41 of page 0, starting on the line's first character.
                    let burst = self.cfg.edit_burst.min(edits - self.step);
                    if burst == 0 {
                        let line = 2 + (self.step as usize % 40);
                        select_drag(ui, (55.0, line), (600.0, line));
                    } else {
                        with_state(|state| {
                            for k in 0..burst {
                                let line = 2 + ((self.step + k) as usize % 40);
                                // Where `window_point` puts line `line`: baselines 12 pt apart
                                // from 42 pt below the top.
                                let rect = state
                                    .text_lines
                                    .get(&0)
                                    .and_then(|lines| lines.get(line))
                                    .copied()
                                    .unwrap_or_else(|| {
                                        pdf_core::Rect::new(
                                            54.0,
                                            42.0 + 12.0 * line as f32 - 10.0,
                                            246.0,
                                            12.0,
                                        )
                                    });
                                let op = pdf_core::annot::EditOp::AddMarkup {
                                    page: 0,
                                    kind: pdf_core::annot::MarkupKind::Highlight,
                                    rects: vec![rect],
                                    colour: pdf_core::annot::PALETTE[0],
                                    author: None,
                                };
                                state.send_edit(op);
                            }
                        });
                    }
                    self.step += burst.max(1) - 1;
                    // Row X: the working set 2 s after the 100th and the 1000th edit.
                    for n in [100, 1000] {
                        if before < n && self.step + 1 >= n {
                            self.edit_hold = Some((Instant::now() + Duration::from_secs(2), n));
                        }
                    }
                    if self.cfg.edit_pan_px > 0.0 {
                        // Down and back up in turn, so page 1 stays where the next drag lands
                        // while every edit still races a scroll's tile requests.
                        let step = if self.step % 2 == 0 {
                            self.cfg.edit_pan_px
                        } else {
                            -self.cfg.edit_pan_px
                        };
                        with_state(|state| {
                            let to = (state.pos.0, state.pos.1 + step);
                            state.scroll_to(ui, to);
                        });
                    }
                    self.step += 1;
                    return false;
                }
                // After the edits, one step a tick, each checked on the next: the page has them
                // all; a click selects the highlight of line 2; a swatch recolours it; Delete
                // removes it; and the save, if asked for, leaves nothing unsaved.
                // The edits cycle over 40 lines, and a highlight over a highlighted line replaces
                // it (Plan 0004 Phase 1 review, option B): one annotation per distinct line.
                let marked = edits.min(40);
                let marks = || {
                    let mut n = 0;
                    with_state(|state| n = state.annots.get(&0).map_or(0, |items| items.len()));
                    n
                };
                let check = |name: &str, expected: String, got: String| {
                    with_state(|state| state.bench.nav_check(name, &expected, &got));
                };
                match self.step - edits {
                    0 => {
                        with_state(|state| state.bench.requests_during_edits(false));
                        let dirty = with_dirty();
                        check(
                            "edits on page 1",
                            format!("annotations={marked} dirty=true"),
                            format!("annotations={} dirty={dirty}", marks()),
                        );
                        ui.invoke_choose_tool(0);
                        select_click(ui, (94.0, 2), false);
                    }
                    1 => {
                        check(
                            "click selects it",
                            "selected=true".to_owned(),
                            format!("selected={}", ui.get_has_annot_selection()),
                        );
                        ui.invoke_choose_colour(2);
                    }
                    2 => {
                        let mut colour = None;
                        with_state(|state| {
                            colour = state.selected_annot.and_then(|(page, id)| {
                                state
                                    .annots
                                    .get(&page)
                                    .and_then(|items| items.iter().find(|item| item.id == id))
                                    .and_then(|item| item.colour)
                            });
                        });
                        let want = pdf_core::annot::PALETTE[2];
                        let recoloured = colour.is_some_and(|c| {
                            (c.r - want.r).abs() < 0.01
                                && (c.g - want.g).abs() < 0.01
                                && (c.b - want.b).abs() < 0.01
                        });
                        check(
                            "swatch recolours it",
                            "recoloured=true".to_owned(),
                            format!("recoloured={recoloured}"),
                        );
                        press(ui, char::from(Key::Delete).into());
                    }
                    3 => {
                        check(
                            "Delete removes it",
                            format!("annotations={} selected=false", marked - 1),
                            format!(
                                "annotations={} selected={}",
                                marks(),
                                ui.get_has_annot_selection()
                            ),
                        );
                        with_modifier(ui, Key::Control, || press(ui, "z".into()));
                    }
                    4 => {
                        check(
                            "Ctrl+Z undoes the delete",
                            format!("annotations={marked} can_redo=true"),
                            format!("annotations={} can_redo={}", marks(), ui.get_can_redo()),
                        );
                        with_modifier(ui, Key::Control, || press(ui, "y".into()));
                    }
                    5 => {
                        check(
                            "Ctrl+Y redoes it",
                            format!("annotations={} can_redo=false", marked - 1),
                            format!("annotations={} can_redo={}", marks(), ui.get_can_redo()),
                        );
                        if self.cfg.edit_save {
                            ui.invoke_save();
                        }
                    }
                    6 if self.cfg.edit_save => {
                        check(
                            "saved",
                            "dirty=false".to_owned(),
                            format!("dirty={}", with_dirty()),
                        );
                    }
                    _ => {
                        self.settle();
                        return false;
                    }
                }
                self.step += 1;
                false
            }
            Phase::Outline => {
                // The tree starts collapsed and is opened only by the title clicks themselves, so
                // reaching a child entry at all checks that a click opens its parent; the row count
                // after each click is checked too.
                if let Some((step, expected)) = self.nav_expect.take() {
                    let got = format!(
                        "{} rows={}",
                        nav_observed(ui),
                        ui.get_outline_rows().row_count()
                    );
                    with_state(|state| state.bench.nav_check(step, &expected, &got));
                }
                let row = self.step as usize;
                let mut expected = None;
                with_state(|state| {
                    let Some(item) = state
                        .outline_visible
                        .get(row)
                        .and_then(|&i| state.outline_items.get(i))
                    else {
                        return;
                    };
                    let (_, view_h) = state.view_size(ui);
                    let doc_h = state.layout.height() as f64;
                    let y = match item.page.and_then(|p| state.layout.slot(p as usize)) {
                        // An entry without a page does not move the view.
                        None => state.pos.1,
                        Some(slot) => match item.top {
                            Some(top) => {
                                slot.y as f64 + top as f64 * state.scale_milli as f64 / 1000.0
                                    - state.layout.margins().top as f64
                            }
                            None => {
                                slot.y as f64
                                    - state.layout.gap_px() as f64
                                    - state.layout.margins().top as f64
                            }
                        },
                    };
                    let y = y.clamp(0.0, (doc_h - view_h).max(0.0));
                    let (_, free_h) = state.free_view(ui);
                    let mid = y + state.layout.margins().top as f64 + free_h / 2.0;
                    let page = state.layout.page_near(mid).unwrap_or(0) + 1;
                    let mut opened = state.outline_expanded.clone();
                    if let Some(&index) = state.outline_visible.get(row) {
                        crate::expand_on_click(&state.outline_items, &mut opened, index);
                    }
                    let rows = crate::visible_outline(&state.outline_items, &opened).len();
                    expected = Some(format!(
                        "zoom={} page={page} y={y:.0} rows={rows}",
                        state.scale_milli
                    ));
                });
                let Some(expected) = expected else {
                    self.settle();
                    return false;
                };
                ui.invoke_outline_clicked(row as i32);
                self.nav_expect = Some(("outline entry", expected));
                self.step += 1;
                false
            }
            Phase::Reopen => {
                // Tick k samples the working set left by open k (the first open is the one the
                // run started on, already settled), then starts open k+1 - so with a step of a
                // couple of seconds every sample sees a settled document.
                let n = self.step + 1;
                with_state(|state| state.bench.reopen_sample(n));
                if n >= self.cfg.reopen {
                    self.settle();
                    return false;
                }
                with_state(|state| {
                    if let Some(path) = state.settings.recent.first().cloned() {
                        state.open_path(ui, path);
                    }
                });
                self.step += 1;
                false
            }
            Phase::Nav => {
                // Check the step before this one, then perform the next: one tick lets the
                // window process the dispatched events and the state settle.
                if let Some((step, expected)) = self.nav_expect.take() {
                    let mut got = nav_observed(ui);
                    // Zoom, fit and jump steps predict the page, not the position.
                    if !expected.contains(" y=")
                        && let Some(at) = got.find(" y=")
                    {
                        got.truncate(at);
                    }
                    with_state(|state| state.bench.nav_check(step, &expected, &got));
                }
                let Some(&step) = NAV_STEPS.get(self.step as usize) else {
                    self.settle();
                    return false;
                };
                self.nav_expect = Some((nav_step_name(step), nav_perform(ui, step)));
                self.step += 1;
                false
            }
            Phase::Settle => {
                self.settle_ticks = self.settle_ticks.saturating_sub(1);
                if self.settle_ticks > 0 {
                    return false;
                }
                if self.resetting {
                    // The reset has drained: this run is measured from here.
                    self.resetting = false;
                    with_state(|state| state.bench.resume());
                }
                if self.next_phase() {
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
        move || {
            with_state(|state| {
                let resident = state.pages_resident();
                state.bench.sample_memory(&state.scheduler, resident);
            })
        },
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
    if cfg.idle_s > 0.0 {
        start_idle(cfg.idle_s);
        return;
    }
    if cfg.modes > 0 {
        start_modes(ui, cfg.modes);
        return;
    }
    if cfg.tabs > 0 {
        start_tabs(ui, cfg.tabs);
        return;
    }
    if cfg.pinches > 0 {
        start_pinches(ui, cfg.pinches);
        return;
    }
    if cfg.is_empty() {
        println!(
            "[bench] nothing to do: give --bench-zoom, --bench-pan, --bench-scroll, --bench-jump or --bench-nav"
        );
        return;
    }
    println!(
        "[bench] run jump={} zoom_steps={} pan_px={} pan_steps={} scroll_pxs={} scroll_s={} \
         step_ms={} settle_ms={} max_blank_ms={} repeats={} cache={} rows={}",
        cfg.jump_page
            .map_or_else(|| "-".to_owned(), |page| page.to_string()),
        cfg.zoom_steps,
        cfg.pan_px,
        cfg.pan_steps,
        cfg.scroll_pxs,
        cfg.scroll_s,
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
    // The viewport every number of the run depends on: logical size and the device scale factor.
    println!(
        "[bench] viewport {:.0}x{:.0} logical, scale factor {}",
        ui.get_view_w(),
        ui.get_view_h(),
        ui.window().scale_factor()
    );

    // A pan-only scenario opens on its pan phase, as it always did; anything else settles first so
    // its first step does not land inside the document's own first render.
    let mut pending = Pending::all(&cfg);
    let only_pan = !pending.jump
        && !pending.zoom
        && !pending.scroll
        && !pending.nav
        && !pending.reopen
        && !pending.thumbs
        && !pending.outline
        && !pending.search
        && !pending.select;
    let phase = if only_pan {
        pending.pan = false;
        Phase::Pan
    } else {
        Phase::Settle
    };
    DRIVER.with(|cell| {
        *cell.borrow_mut() = Some(Driver {
            cfg: cfg.clone(),
            phase,
            step: 0,
            settle_ticks: settle_ticks(&cfg),
            pending,
            runs_left: cfg.repeats.max(1),
            home: None,
            resetting: false,
            nav_expect: None,
            searches_before: 0,
            edit_hold: None,
            pages_base: 0,
            pages_rebuilds: 0,
            tools_wait: 0,
            open_wait: None,
            base_size: String::new(),
            form_expect: None,
            form_deadline: Instant::now(),
            form_press: None,
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
                        ui.get_current_page(),
                    )
                })
            });
            crate::exit_process(if pass { 0 } else { 1 });
        },
    );
    TIMER.with(|cell| *cell.borrow_mut() = Some(timer));
}

/// How long the opening view may take to be covered before `--bench-idle` gives up.
const IDLE_COVER_TIMEOUT: Duration = Duration::from_secs(60);
/// Quiet time between the covered opening view and the counted window.
const IDLE_SETTLE: Duration = Duration::from_secs(1);

/// `--bench-idle S` (rows AR and AS): waits for the opening view to be covered, settles, then
/// counts frames for `secs` seconds with no input. Exits 0 when no frame was drawn.
fn start_idle(secs: f64) {
    println!(
        "[bench] idle run: settle {}ms, count {secs}s",
        IDLE_SETTLE.as_millis()
    );
    let counted = Duration::from_secs_f64(secs);
    // (when the view was covered, frames at the start of the count, when the count started)
    let mut covered: Option<Instant> = None;
    let mut count_from: Option<(u64, Instant)> = None;
    let opened = Instant::now();
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(50), move || {
        let first = crate::APP.with(|cell| {
            cell.borrow()
                .as_ref()
                .and_then(|state| state.bench.first_full_ms())
        });
        let Some(first) = first else {
            if opened.elapsed() > IDLE_COVER_TIMEOUT {
                println!("[bench] idle FAIL: the opening view was never covered");
                crate::exit_process(1);
            }
            return;
        };
        let now = Instant::now();
        let since = *covered.get_or_insert(now);
        if count_from.is_none() && since.elapsed() >= IDLE_SETTLE {
            count_from = Some((FRAMES.load(Ordering::Relaxed), now));
        }
        let Some((frames_from, from)) = count_from else {
            return;
        };
        if from.elapsed() < counted {
            return;
        }
        let frames = FRAMES.load(Ordering::Relaxed) - frames_from;
        let ms = from.elapsed().as_millis();
        println!("[bench] t_first_page={first}ms (process start to the opening view covered)");
        println!(
            "[bench] idle frames={frames} over {ms}ms, {} before the count",
            frames_from
        );
        let pass = frames == 0;
        println!("[bench] idle {}", if pass { "PASS" } else { "FAIL" });
        crate::exit_process(if pass { 0 } else { 1 });
    });
    TIMER.with(|cell| *cell.borrow_mut() = Some(timer));
}

/// How long one tab may take to show its view covered before `--bench-tabs` gives up.
const TABS_COVER_TIMEOUT: Duration = Duration::from_secs(60);
/// Quiet time after a tab's view is covered before the working set is read.
const TABS_SETTLE: Duration = Duration::from_millis(1500);
/// Row AU's ceiling for `peak_ws` with up to 4 tabs, the default configuration's (§7.6).
const TABS_PEAK_MIB: f32 = 250.0;

/// `--bench-tabs N` (row AU): opens the document in N tabs one by one, then shows each tab once
/// more. After each step it waits for the view to be covered and settles, then reads the working
/// set. `doc_mem` is what the second tab added: one document's own memory. Passes when every view
/// was covered, the working set with N tabs is at most the one-tab figure plus N times
/// `doc_mem`, and, up to 4 tabs, `peak_ws` is at most 250 MiB.
fn start_tabs(ui: &MainWindow, count: u32) {
    let Some(path) = with_state_ret(|state| state.doc_path.clone()).flatten() else {
        println!("[bench] tabs FAIL: no document");
        crate::exit_process(1);
    };
    println!("[bench] tabs run: {count} tabs of {}", path.display());
    // The steps: open tab 2..=N, then show tab 1..=N again.
    let steps = 2 * count as usize - 1;
    let mut step = 0usize;
    let mut asked = Instant::now();
    let mut covered: Option<Instant> = None;
    let mut ws: Vec<(String, f32)> = Vec::new();
    let mut shown_ms: Vec<u128> = Vec::new();
    let weak = ui.as_weak();
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(20), move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let full = with_state_ret(|state| {
            let visible = state.visible(&ui);
            !visible.is_empty() && holes(&state.scheduler, &visible) == 0
        })
        .unwrap_or(false);
        if covered.is_none() {
            if !full {
                if asked.elapsed() > TABS_COVER_TIMEOUT {
                    println!("[bench] tabs FAIL: step {step}'s view was never covered");
                    crate::exit_process(1);
                }
                return;
            }
            covered = Some(Instant::now());
            shown_ms.push(asked.elapsed().as_millis());
        }
        if covered.is_some_and(|at| at.elapsed() < TABS_SETTLE) {
            return;
        }
        let tabs = crate::tabs::count();
        let label = if step < count as usize {
            format!("{tabs} tabs")
        } else {
            format!("{tabs} tabs, tab {} shown again", step - count as usize + 1)
        };
        let mem = process_memory();
        let mib_ws = mem.map_or(0.0, |mem| mib(mem.ws as u128));
        println!(
            "[bench] tabs step={step} {label}: ws={} private={} shown_ms={}",
            mem_str(mem.map(|m| m.ws)),
            mem_str(mem.map(|m| m.private)),
            shown_ms.last().copied().unwrap_or(0)
        );
        ws.push((label, mib_ws));
        step += 1;
        covered = None;
        asked = Instant::now();
        if step < count as usize {
            crate::tabs::open_in_new_tab(&ui, path.clone());
            return;
        }
        if step < steps {
            crate::tabs::switch(&ui, step - count as usize);
            return;
        }

        // Done: the summary.
        let one = ws.first().map_or(0.0, |(_, v)| *v);
        let doc_mem = ws.get(1).map_or(0.0, |(_, v)| *v - one);
        let all = ws.get(count as usize - 1).map_or(0.0, |(_, v)| *v);
        let after = ws.last().map_or(0.0, |(_, v)| *v);
        let peak = process_memory().map_or(0.0, |mem| mib(mem.peak_ws as u128));
        let ceiling = one + count as f32 * doc_mem.max(0.0);
        let (open_ms, again_ms) = shown_ms.split_at((count as usize).min(shown_ms.len()));
        println!(
            "[bench] tabs ws_1={one:.1}MiB doc_mem={doc_mem:.1}MiB ws_{count}={all:.1}MiB \
             ws_after_round={after:.1}MiB ceiling={ceiling:.1}MiB peak_ws={peak:.1}MiB"
        );
        println!("[bench] tabs t_open_ms={open_ms:?} t_switch_ms={again_ms:?}");
        let linear = count < 2 || (all <= ceiling && after <= ceiling);
        let peak_ok = count > 4 || peak <= TABS_PEAK_MIB;
        let pass = linear && peak_ok;
        println!(
            "[bench] tabs linear={} peak={} {}",
            if linear { "PASS" } else { "FAIL" },
            if count > 4 {
                "n/a".to_owned()
            } else if peak_ok {
                "PASS".to_owned()
            } else {
                "FAIL".to_owned()
            },
            if pass { "PASS" } else { "FAIL" }
        );
        crate::exit_process(if pass { 0 } else { 1 });
    });
    TIMER.with(|cell| *cell.borrow_mut() = Some(timer));
}

/// The modes `--bench-modes` goes through, one round: every change between two modes once.
const MODE_ROUND: [i32; 6] = [1, 2, 0, 2, 1, 0];
/// Quiet time after a change's motion before the next change, so thumbnails the gallery draws
/// after it settles are not counted in the next one.
const MODE_PAUSE: Duration = Duration::from_millis(800);
/// The frame budget of row AT: one frame at 60 Hz.
const FRAME_BUDGET_MS: f64 = 1000.0 / 60.0;

/// `--bench-modes N` (row AT): waits for the opening view to be covered, settles, then changes the
/// mode `N` rounds of [`MODE_ROUND`]. For each change, over its motion plus the settle margin
/// (`MODE_MOTION_MS + MODE_SETTLE_MS`), it reports the tiles requested and rendered and the frame
/// timing. Exits 0 when no change rendered or requested a tile and no frame took longer than
/// [`FRAME_BUDGET_MS`].
fn start_modes(ui: &MainWindow, rounds: u32) {
    let motion = Duration::from_millis(crate::MODE_MOTION_MS + crate::MODE_SETTLE_MS);
    println!(
        "[bench] modes run: {rounds} round(s) of {MODE_ROUND:?}, each change measured over {}ms",
        motion.as_millis()
    );
    let weak = ui.as_weak();
    let changes = rounds as usize * MODE_ROUND.len();
    let mut covered: Option<Instant> = None;
    let mut next_at: Option<Instant> = None;
    // (when the change began, from, to, tiles requested and rendered before it)
    let mut running: Option<(Instant, i32, i32, i32, i32)> = None;
    let mut done = 0usize;
    let (mut worst_frame, mut worst_gap, mut tiles, mut frames) = (0.0f64, 0.0f64, 0i32, 0u64);
    let opened = Instant::now();
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(5), move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let first = crate::APP.with(|cell| {
            cell.borrow()
                .as_ref()
                .and_then(|state| state.bench.first_full_ms())
        });
        if first.is_none() {
            if opened.elapsed() > IDLE_COVER_TIMEOUT {
                println!("[bench] modes FAIL: the opening view was never covered");
                crate::exit_process(1);
            }
            return;
        }
        let now = Instant::now();
        let since = *covered.get_or_insert(now);
        if since.elapsed() < IDLE_SETTLE {
            return;
        }
        if let Some((began, from, to, requested, rendered)) = running {
            if began.elapsed() < motion {
                return;
            }
            let window = frame_window();
            let requested = ui.get_tiles_requested() - requested;
            let rendered = ui.get_tiles_rendered() - rendered;
            println!(
                "[bench] mode {from}>{to} tiles_requested={requested} tiles_rendered={rendered} {window}"
            );
            worst_frame = worst_frame.max(window.max_ms);
            worst_gap = worst_gap.max(window.gap_ms);
            tiles += requested + rendered;
            frames += window.frames;
            running = None;
            next_at = Some(now + MODE_PAUSE);
            return;
        }
        if next_at.is_some_and(|at| now < at) {
            return;
        }
        if done == changes {
            let pass = tiles == 0 && worst_frame <= FRAME_BUDGET_MS;
            println!(
                "[bench] modes changes={changes} tiles={tiles} frames={frames}                  worst_frame={worst_frame:.1}ms worst_gap={worst_gap:.1}ms budget={FRAME_BUDGET_MS:.1}ms"
            );
            println!("[bench] modes {}", if pass { "PASS" } else { "FAIL" });
            crate::exit_process(if pass { 0 } else { 1 });
        }
        let to = MODE_ROUND[done % MODE_ROUND.len()];
        frame_window_reset();
        running = Some((
            now,
            ui.get_mode(),
            to,
            ui.get_tiles_requested(),
            ui.get_tiles_rendered(),
        ));
        ui.set_mode(to);
        done += 1;
    });
    TIMER.with(|cell| *cell.borrow_mut() = Some(timer));
}

/// Frames of one scripted pinch, one per [`PINCH_STEP`].
const PINCH_STEPS: u32 = 30;
const PINCH_STEP: Duration = Duration::from_millis(16);
/// How far the fingers' centre drifts during one pinch, in logical pixels.
const PINCH_DRIFT: (f32, f32) = (30.0, 20.0);
/// How long, after the fingers lift, the view has to be covered.
const PINCH_COVER_TIMEOUT: Duration = Duration::from_secs(10);

/// `--bench-pinch N` (row ME): waits for the opening view to be covered, settles, then makes `N`
/// pinches through `MainWindow::invoke_pinch`, the callback Slint's `ScaleRotateGestureHandler`
/// calls: the even ones to twice the zoom, the odd ones to half, centred on the view and drifting
/// by [`PINCH_DRIFT`]. While the fingers move it counts the tiles requested and rendered and times
/// the frames; after they lift it waits until the view is covered again (`t_cover`). Exits 0 when
/// no pinch requested or rendered a tile while moving and no frame took longer than
/// [`FRAME_BUDGET_MS`].
fn start_pinches(ui: &MainWindow, pinches: u32) {
    println!(
        "[bench] pinch run: {pinches} pinch(es) of {PINCH_STEPS} frames every {}ms, drift {PINCH_DRIFT:?}",
        PINCH_STEP.as_millis()
    );
    let weak = ui.as_weak();
    let mut covered: Option<Instant> = None;
    // (step, tiles requested and rendered before the pinch, the centre it started at)
    let mut moving: Option<(u32, i32, i32, (f32, f32))> = None;
    // When the fingers lifted, while the view is not covered yet.
    let mut lifted: Option<Instant> = None;
    let mut done = 0u32;
    let (mut worst_frame, mut tiles, mut worst_cover) = (0.0f64, 0i32, 0u128);
    let opened = Instant::now();
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, PINCH_STEP, move || {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let full = crate::APP.with(|cell| {
            cell.borrow()
                .as_ref()
                .map(|state| {
                    let visible = state.visible(&ui);
                    (state.bench.first_full_ms(), holes(&state.scheduler, &visible) == 0)
                })
        });
        let Some((first, now_covered)) = full else {
            return;
        };
        if first.is_none() {
            if opened.elapsed() > IDLE_COVER_TIMEOUT {
                println!("[bench] pinch FAIL: the opening view was never covered");
                crate::exit_process(1);
            }
            return;
        }
        let since = *covered.get_or_insert_with(Instant::now);
        if since.elapsed() < IDLE_SETTLE {
            return;
        }
        if let Some(at) = lifted {
            if !now_covered {
                if at.elapsed() > PINCH_COVER_TIMEOUT {
                    println!("[bench] pinch FAIL: the view was not covered after a pinch");
                    crate::exit_process(1);
                }
                return;
            }
            let cover = at.elapsed().as_millis();
            worst_cover = worst_cover.max(cover);
            println!(
                "[bench] pinch {done} t_cover={cover}ms zoom={:.1}%",
                ui.get_zoom_percent()
            );
            lifted = None;
            covered = Some(Instant::now());
            return;
        }
        let Some((step, requested, rendered, (cx, cy))) = moving else {
            if done == pinches {
                let pass = tiles == 0 && worst_frame <= FRAME_BUDGET_MS;
                println!(
                    "[bench] pinch pinches={pinches} tiles={tiles} worst_frame={worst_frame:.1}ms \
                     budget={FRAME_BUDGET_MS:.1}ms worst_cover={worst_cover}ms"
                );
                println!("[bench] pinch {}", if pass { "PASS" } else { "FAIL" });
                crate::exit_process(if pass { 0 } else { 1 });
            }
            let centre = (ui.get_view_w() / 2.0, ui.get_view_h() / 2.0);
            frame_window_reset();
            moving = Some((
                0,
                ui.get_tiles_requested(),
                ui.get_tiles_rendered(),
                centre,
            ));
            ui.invoke_pinch(0, 1.0, centre.0, centre.1);
            return;
        };
        let target = if done % 2 == 0 { 2.0f32 } else { 0.5 };
        let step = step + 1;
        let f = step as f32 / PINCH_STEPS as f32;
        let scale = 1.0 + (target - 1.0) * f;
        let (x, y) = (cx + PINCH_DRIFT.0 * f, cy + PINCH_DRIFT.1 * f);
        if step < PINCH_STEPS {
            ui.invoke_pinch(1, scale, x, y);
            moving = Some((step, requested, rendered, (cx, cy)));
            return;
        }
        // The fingers lift: what the motion cost, before the zoom asks for tiles.
        let window = frame_window();
        let requested = ui.get_tiles_requested() - requested;
        let rendered = ui.get_tiles_rendered() - rendered;
        println!(
            "[bench] pinch {} x{target} tiles_requested={requested} tiles_rendered={rendered} {window}",
            done + 1
        );
        worst_frame = worst_frame.max(window.max_ms);
        tiles += requested + rendered;
        ui.invoke_pinch(2, scale, x, y);
        moving = None;
        lifted = Some(Instant::now());
        done += 1;
    });
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
        assert!(trace.report(&Cfg::default(), &sched, 0, 0, 0));
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
        assert_eq!(path.first().map(String::as_str), Some("doc.pdf"));
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
    fn scroll_and_jump_flags_are_parsed() {
        let (path, cfg) = parse_args(&args(&[
            "doc.pdf",
            "--bench-scroll",
            "4000",
            "--bench-scroll-s",
            "12",
            "--bench-jump",
            "900",
        ]));
        assert_eq!(path.first().map(String::as_str), Some("doc.pdf"));
        assert_eq!(cfg.scroll_pxs, 4000.0);
        assert_eq!(cfg.scroll_s, 12.0);
        assert_eq!(cfg.jump_page, Some(900));
        assert!(!cfg.is_empty());

        let mut pending = Pending::all(&cfg);
        assert_eq!(pending.take(), Some(Phase::Jump));
        assert_eq!(pending.take(), Some(Phase::Scroll));
        assert_eq!(pending.take(), None);

        // A jump alone is a scenario too (Plan 0002 §6 row D), and page 0 means page 1.
        let (_, cfg) = parse_args(&args(&["--bench-jump", "0"]));
        assert_eq!(cfg.jump_page, Some(1));
        assert!(!cfg.is_empty());
    }

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
