// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! The render worker pool and the raster jobs it runs (Plan 0001, §5 Phase 2).
//!
//! The pool never sees the document. MuPDF's `fz_context` is thread-local, so only the actor
//! thread that opened it may touch it; what crosses over is *resolved* work: a [`Job`] carries the
//! page display list behind an `Arc`, the grid origin MuPDF itself reported, and the epoch it
//! belongs to. That is also why the pool cannot get a tile's geometry wrong - the actor validated
//! it against the renderer before dispatching ([`crate::Prepared`]).
//!
//! A display list is the exception to the "objects belong to their context" rule and the whole
//! reason a pool is possible at all: `DisplayList` is `Send + Sync` (`patches/mupdf/src/
//! display_list.rs:162-169`), and every context here is a clone of the same base context, so the
//! objects a list holds share one locked allocator (`patches/mupdf/src/context.rs:66-82`).
//!
//! Cancelling: the epoch names the work. A `Cancel` drops queued work of that epoch *before* it
//! rasterizes and aborts a tile that is already inside the rasterizer, through the cookie channel
//! MuPDF exposes for exactly that use ([`mupdf::CookieAbort`]). MuPDF's display list runner stops on
//! that cookie by *breaking out of its node loop* (`source/fitz/list-device.c`, `fz_run_display_list`)
//! rather than by raising, and it swallows an `FZ_ERROR_ABORT` raised by an inner operation - either
//! way the run returns normally, so the abort is reported from the registry `cancel` fired on, and
//! the delay from the abort signal to the rasterizer noticing it is
//! [`EngineEvent::TileAborted`].
//!
//! Ordering: whether a finished raster may be published is decided under the registry lock, in the
//! same critical section `cancel` sets the epoch flag in, so a cancel that has already applied is
//! ordered *before* that decision and the pixels go in the bin - not because a flag happened to be
//! visible on another core in time, but because the lock says so. Only the speculative skip that
//! saves the raster itself reads the flag outside the lock, where a stale read costs nothing but a
//! raster that is then discarded. Reading the flag next to the raster instead would make the
//! invariant a bet on cache propagation.
//!
//! How fast an abort lands is then a property of the page, not of this code: MuPDF tests the cookie
//! at node boundaries, so the longest a cancel can be ignored is one node's execution. A page of
//! thousands of small nodes aborts in well under a millisecond, which is what
//! `engine-mupdf/tests/pool_test.rs` asserts against a 500 ms smoke ceiling on a Letter fixture; a
//! page whose content is a single expensive node (one big fill, one slow shading) cannot be
//! interrupted until that node ends, however stale its epoch is. The same test measures both.

use mupdf::{Colorspace, Cookie, CookieAbort, Device, DisplayList, Matrix, Pixmap};
use pdf_core::{
    Bitmap, EngineEvent, PageGeometry, PixelFormat, RequestId, TILE_BLEED_PX, TILE_SIZE_PX,
    TILE_STRIDE_PX,
};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use crate::MupdfError;

/// Number of raster worker threads: `clamp(cores - 1, 1, 4)`, leaving a core for the UI thread
/// and for the actor.
///
/// `FA_PDF_WORKERS` overrides it (1..=16). `1` rasterizes on a single worker thread, which is the
/// serial end of the Phase 2 comparison; the benchmark's reference render is
/// [`crate::MupdfEngine::render_tile`], which draws through the same code ([`run_tile`]).
pub fn worker_count() -> usize {
    if let Ok(value) = std::env::var("FA_PDF_WORKERS")
        && let Ok(count) = value.trim().parse::<usize>()
    {
        return count.clamp(1, 16);
    }
    thread::available_parallelism()
        .map(|cores| cores.get().saturating_sub(1))
        .unwrap_or(1)
        .clamp(1, 4)
}

/// A page's display list plus the raster grid MuPDF reported for it, resolved on the actor thread.
///
/// The grid agreement check (Plan 0001, §4.1 risk 1.6) happens once per page *and scale* here
/// rather than once per tile, so a worker can neither load the document nor disagree with it.
#[derive(Debug, Clone)]
pub(crate) struct Prepared {
    /// Page, page bounds in points, and the scale this was resolved at.
    pub(crate) geometry: PageGeometry,
    /// The page's display list; scale independent, so one list serves every zoom level.
    pub(crate) list: Arc<DisplayList>,
    /// Raster origin in device pixels, taken from MuPDF's own rounded bounds.
    pub(crate) origin: (i32, i32),
}

/// One unit of pool work.
///
/// Both variants carry the epoch they were requested under, so a worker can tell a superseded job
/// from a live one without asking the actor.
pub(crate) enum Job {
    /// One tile of a page at the scale `prepared.geometry.scale_milli` names.
    Tile {
        prepared: Prepared,
        col: i32,
        row: i32,
        request_id: RequestId,
    },
    /// The whole page's low-resolution base layer, at the page's base scale.
    Base {
        prepared: Prepared,
        request_id: RequestId,
    },
}

impl Job {
    /// The epoch this job belongs to.
    fn request_id(&self) -> RequestId {
        match self {
            Job::Tile { request_id, .. } | Job::Base { request_id, .. } => *request_id,
        }
    }

    /// Whether a `Cancel` may touch this job.
    ///
    /// A tile belongs to a zoom epoch and its pixels are worthless once that epoch is superseded.
    /// The base layer belongs to a *page*: it is the image that covers the gap while a new epoch
    /// rasterizes, so no epoch invalidates it and cancelling it on a zoom would put the gap back
    /// (Plan 0001, §5 Phase 2).
    fn cancellable(&self) -> bool {
        matches!(self, Job::Tile { .. })
    }

    /// Rasterizes the job. `scratch` is the calling worker's persistent tile buffer; the base
    /// layer allocates its own, because a whole page is far larger than a tile.
    fn run(&self, scratch: &mut Pixmap, cookie: Option<&Cookie>) -> Result<Bitmap, MupdfError> {
        match self {
            Job::Tile {
                prepared, col, row, ..
            } => run_tile(prepared, *col, *row, scratch, cookie),
            Job::Base { prepared, .. } => run_base(prepared, cookie),
        }
    }
}

/// Rasterizes one tile into the worker's scratch buffer and crops it out.
///
/// Shared by the pool and by [`crate::MupdfEngine::render_tile`], which is what makes the parallel
/// and serial paths comparable: they differ only in which thread calls this and which scratch
/// buffer it passes.
///
/// The scratch carries a bleed band around the tile, so antialiasing at a cell boundary resolves
/// the same way on both sides (Plan 0001, §4.1); the surplus is dropped on copy-out.
pub(crate) fn run_tile(
    prepared: &Prepared,
    col: i32,
    row: i32,
    scratch: &mut Pixmap,
    cookie: Option<&Cookie>,
) -> Result<Bitmap, MupdfError> {
    let page_index = prepared.geometry.page;
    let fail = |source| MupdfError::TileRenderFailed {
        page_index,
        col,
        row,
        source,
    };
    let Some(tile) = prepared.geometry.tile_rect(col, row) else {
        return Err(MupdfError::TileOutsidePage {
            page_index,
            col,
            row,
        });
    };

    // Scratch origin in device pixels: the cell's top-left corner minus the bleed band.
    let x0 = prepared.origin.0 + col * TILE_SIZE_PX as i32 - TILE_BLEED_PX;
    let y0 = prepared.origin.1 + row * TILE_SIZE_PX as i32 - TILE_BLEED_PX;
    scratch.clear().map_err(fail)?;
    run_into(
        scratch,
        &prepared.list,
        prepared.geometry.scale(),
        (x0, y0),
        (TILE_STRIDE_PX, TILE_STRIDE_PX),
        cookie,
    )
    .map_err(fail)?;

    let bleed = TILE_BLEED_PX as usize;
    Ok(tight_copy(scratch, (bleed, bleed), tile.w, tile.h))
}

/// Rasterizes a whole page at its base scale, into a pixmap of its own.
///
/// One allocation per page rather than per level: the base layer is a page property, so this runs
/// once on open and once per page change, and `base_scale_milli` keeps it at about 2.3 MiB of RGB8.
fn run_base(prepared: &Prepared, cookie: Option<&Cookie>) -> Result<Bitmap, MupdfError> {
    let page_index = prepared.geometry.page;
    let fail = |source| MupdfError::RenderFailed {
        page_index: page_index as usize,
        source,
    };
    let (width, height) = prepared.geometry.device_size();
    let colorspace = Colorspace::device_rgb();
    let mut pixmap =
        Pixmap::new(&colorspace, 0, 0, width as i32, height as i32, false).map_err(fail)?;
    // `Pixmap::new` leaves the buffer uninitialized; `clear` is the same init as the page and tile
    // paths (white, because the pixmap has no alpha), so the base layer and the tiles share one
    // background convention.
    pixmap.clear().map_err(fail)?;
    run_into(
        &pixmap,
        &prepared.list,
        prepared.geometry.scale(),
        prepared.origin,
        (width, height),
        cookie,
    )
    .map_err(fail)?;
    Ok(tight_copy(&pixmap, (0, 0), width, height))
}

/// Draws a display list into one pixmap with the CTM every rasterizer path in this crate uses.
///
/// The pixmap origin is `(0, 0)`, so `fz_new_draw_device`'s identity transform makes device space
/// and buffer space the same thing, the CTM carries the raster origin, and the scissor is simply
/// the target rect (Plan 0001, §3.3). `cookie` is the abort channel; `None` is "not cancellable".
fn run_into(
    target: &Pixmap,
    list: &DisplayList,
    scale: f32,
    origin: (i32, i32),
    size: (u32, u32),
    cookie: Option<&Cookie>,
) -> Result<(), mupdf::Error> {
    let device = Device::from_pixmap(target)?;
    let mut ctm = Matrix::new_scale(scale, scale);
    ctm.concat(Matrix::new_translate(
        -(origin.0 as f32),
        -(origin.1 as f32),
    ));
    let area = mupdf::Rect::new(0.0, 0.0, size.0 as f32, size.1 as f32);
    match cookie {
        Some(cookie) => list.run_with_cookie(&device, &ctm, area, cookie),
        None => list.run(&device, &ctm, area),
    }
}

/// Copies a `w`x`h` region at `at` out of a pixmap into a tightly packed RGB8 bitmap.
///
/// The component count comes from the pixmap rather than a constant, and every pixmap this crate
/// rasterizes into is created alpha-free - so the buffer is 3 bytes per pixel, the format
/// [`Bitmap`] declares, and the page's white paper is `0xff` rather than a transparent hole.
fn tight_copy(source: &Pixmap, at: (usize, usize), w: u32, h: u32) -> Bitmap {
    let stride = source.stride().max(0) as usize;
    let n = source.n() as usize;
    debug_assert_eq!(n, PixelFormat::Rgb8.bytes_per_pixel(), "alpha pixmap");
    let row_bytes = w as usize * n;
    let mut data = vec![0u8; row_bytes * h as usize];
    let samples = source.samples();
    for row in 0..h as usize {
        let start = (row + at.1) * stride + at.0 * n;
        data[row * row_bytes..(row + 1) * row_bytes]
            .copy_from_slice(&samples[start..start + row_bytes]);
    }
    Bitmap::new(w, h, row_bytes, PixelFormat::Rgb8, data)
}

/// Everything the workers pull from: the queue, the abort registry and the event channel.
struct Shared {
    queue: Mutex<VecDeque<Job>>,
    /// Signalled on a new job and on shutdown.
    work: Condvar,
    /// Epoch named by the most recent `Cancel` (Plan 0001, §4.5 steps 1-4): its queued jobs are
    /// dropped before rasterizing, and its running ones are aborted where they are. Stored under
    /// [`Shared::running`]'s lock (see [`Shared::cancel`]) with a release/acquire pair, so the
    /// speculative skip in a worker cannot see it before the rest of the cancel is in place.
    cancelled: AtomicU64,
    /// Tiles currently inside the rasterizer, the only place a `Cancel` can still reach them.
    running: Mutex<Vec<Running>>,
    next_id: AtomicU64,
    stop: AtomicBool,
    events: Sender<EngineEvent>,
}

/// A tile that is inside the rasterizer right now.
struct Running {
    id: u64,
    request_id: RequestId,
    abort: CookieAbort,
    /// When a `Cancel` fired the abort, so the worker can report how long it took to land.
    aborted_at: Option<Instant>,
}

/// What a finished raster turned out to be worth when its worker retired it.
enum Retired {
    /// No `Cancel` names its epoch: the pixels are live and belong on the wire.
    Live,
    /// A `Cancel` reached this raster while it ran; the `Instant` is when it fired, so the worker
    /// can report how long the rasterizer took to notice.
    Aborted(Instant),
    /// The raster finished before the cancel reached it, but its epoch is gone: the pixels are
    /// dropped without a report, because nothing aborted the raster itself.
    Superseded,
}

impl Shared {
    /// Publishes a running tile and returns the handle used to retire it.
    fn begin(&self, request_id: RequestId, abort: CookieAbort) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.running.lock().unwrap().push(Running {
            id,
            request_id,
            abort,
            aborted_at: None,
        });
        id
    }

    /// Retires a running tile and rules on its pixels.
    ///
    /// The verdict is reached under the registry lock, which is what makes "a cancelled epoch
    /// publishes nothing" a consequence of the lock rather than of timing: `cancel` raises the flag
    /// and stamps the rasters it reaches in one critical section, so a retire that runs after it
    /// sees the stamp - or, for a raster that had already finished, the flag - and drops the pixels,
    /// while a retire that runs before it is publishing an epoch that is still current.
    fn retire(&self, id: u64, request_id: RequestId) -> Retired {
        let mut running = self.running.lock().unwrap();
        let aborted_at = running
            .iter()
            .position(|run| run.id == id)
            .and_then(|index| running.swap_remove(index).aborted_at);
        if let Some(at) = aborted_at {
            return Retired::Aborted(at);
        }
        // Read here rather than in the worker for the same reason: this load is ordered after any
        // cancel that already applied, whereas the worker's own check is only a guess.
        if self.cancelled.load(Ordering::Acquire) == request_id {
            return Retired::Superseded;
        }
        Retired::Live
    }

    /// Drops `request_id`'s queued work and aborts it where it is already rasterizing.
    ///
    /// The flag goes up inside the critical section that stamps the running rasters, so a worker
    /// retiring under this lock sees a cancel whole: flag and stamp, or neither.
    fn cancel(&self, request_id: RequestId) {
        // Read the clock before queueing for the lock: the worker then reports the cancel's full
        // trip, contention included.
        let now = Instant::now();
        let mut running = self.running.lock().unwrap();
        self.cancelled.store(request_id, Ordering::Release);
        for run in running.iter_mut() {
            if run.request_id == request_id && run.aborted_at.is_none() {
                run.aborted_at = Some(now);
                run.abort.abort();
            }
        }
    }

    /// Whether `request_id` is the epoch a `Cancel` named.
    ///
    /// Only the speculative skip before a raster consults this: it saves the raster, and a read that
    /// is stale by the time the raster finishes costs nothing, because [`Shared::retire`] decides
    /// what happens to the pixels. Never publish or drop on this answer alone.
    fn is_cancelled(&self, request_id: RequestId) -> bool {
        self.cancelled.load(Ordering::Acquire) == request_id
    }
}

/// A fixed set of raster worker threads.
pub(crate) struct Pool {
    shared: Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
}

impl Pool {
    /// Spawns the raster threads; each gets its own cloned MuPDF context through `Context::get`.
    pub(crate) fn start(workers: usize, events: Sender<EngineEvent>) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(VecDeque::new()),
            work: Condvar::new(),
            cancelled: AtomicU64::new(0),
            running: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            events,
        });
        let threads = (0..workers.max(1))
            .filter_map(|index| {
                let shared = Arc::clone(&shared);
                thread::Builder::new()
                    .name(format!("mupdf-raster-{index}"))
                    .spawn(move || worker(shared))
                    .ok()
            })
            .collect();
        Self {
            shared,
            workers: threads,
        }
    }

    /// Queues a tile behind whatever is already pending.
    pub(crate) fn dispatch(&self, job: Job) {
        self.shared.queue.lock().unwrap().push_back(job);
        self.shared.work.notify_one();
    }

    /// Queues a job ahead of every pending tile: the base layer uses this so a cold open shows a
    /// whole page as soon as the display list exists, rather than a partly filled grid.
    pub(crate) fn dispatch_first(&self, job: Job) {
        self.shared.queue.lock().unwrap().push_front(job);
        self.shared.work.notify_one();
    }

    /// Cancels one epoch: queued jobs of it are dropped, running ones are aborted.
    pub(crate) fn cancel(&self, request_id: RequestId) {
        self.shared.cancel(request_id);
    }
}

impl Drop for Pool {
    /// Stops the workers and joins them, so an actor that exits - or panics - cannot leak threads.
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.work.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// The worker loop: pull, drop cancelled work, rasterize, publish.
fn worker(shared: Arc<Shared>) {
    let colorspace = Colorspace::device_rgb();
    let mut scratch = match Pixmap::new(
        &colorspace,
        0,
        0,
        TILE_STRIDE_PX as i32,
        TILE_STRIDE_PX as i32,
        false,
    ) {
        Ok(scratch) => scratch,
        Err(error) => {
            let _ = shared.events.send(EngineEvent::Error {
                message: format!("raster worker could not allocate its scratch buffer: {error}"),
            });
            return;
        }
    };

    loop {
        let Some(job) = next_job(&shared) else {
            return;
        };
        let request_id = job.request_id();
        if job.cancellable() && shared.is_cancelled(request_id) {
            continue;
        }

        let cookie = Cookie::new().ok();
        let abort = cookie.as_ref().map(Cookie::abort_handle);
        // Only cancellable jobs enter the abort registry: `cancel` fires at every job carrying the
        // epoch it names, and the base layer has to be immune to that ([`Job::cancellable`]).
        let running = if job.cancellable() {
            abort.map(|abort| shared.begin(request_id, abort))
        } else {
            None
        };
        let outcome = job.run(&mut scratch, cookie.as_ref());
        // The base layer never enters the registry, so no cancel can have reached it.
        let retired = match running {
            Some(id) => shared.retire(id, request_id),
            None => Retired::Live,
        };

        match (outcome, retired) {
            // A cancel that reached this raster owns the report either way: MuPDF's display list
            // runner stops on the cookie by *breaking out of its loop* rather than by raising, so
            // the only evidence an abort produced is the stamp `cancel` left. The `_` also covers
            // the `Err` case, where an inner operation did raise `FZ_ERROR_ABORT` instead.
            (_, Retired::Aborted(at)) => {
                let ms = at.elapsed().as_millis() as u64;
                let _ = shared
                    .events
                    .send(EngineEvent::TileAborted { request_id, ms });
            }
            // Reported even for superseded work: a raster that fails is news whether or not the
            // epoch that asked for it is still current.
            (Err(error), _) => {
                let _ = shared.events.send(EngineEvent::Error {
                    message: error.to_string(),
                });
            }
            // Finished before the cancel reached it, but its epoch is gone: the UI would discard
            // these pixels anyway.
            (Ok(_), Retired::Superseded) => {}
            (Ok(bitmap), Retired::Live) => publish(&shared, &job, bitmap),
        }
    }
}

/// Blocks until there is work, or returns `None` once the pool is shutting down.
fn next_job(shared: &Shared) -> Option<Job> {
    let mut queue = shared.queue.lock().unwrap();
    loop {
        if let Some(job) = queue.pop_front() {
            return Some(job);
        }
        if shared.stop.load(Ordering::Relaxed) {
            return None;
        }
        queue = shared.work.wait(queue).unwrap();
    }
}

/// Publishes finished pixels on the event channel.
fn publish(shared: &Shared, job: &Job, bitmap: Bitmap) {
    let event = match job {
        Job::Tile {
            prepared,
            col,
            row,
            request_id,
        } => EngineEvent::TileRendered {
            key: prepared.geometry.key(*col, *row),
            request_id: *request_id,
            bitmap,
        },
        Job::Base {
            prepared,
            request_id,
        } => EngineEvent::BaseRendered {
            page: prepared.geometry.page,
            scale_milli: prepared.geometry.scale_milli,
            request_id: *request_id,
            bitmap,
        },
    };
    let _ = shared.events.send(event);
}
