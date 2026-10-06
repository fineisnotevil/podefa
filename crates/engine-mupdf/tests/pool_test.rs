// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! The render worker pool (Plan 0001, §5 Phase 2).
//!
//! Everything is measured through [`EngineHandle`] - the path the app actually uses - so a
//! regression in dispatch, publication or cancellation is caught where a user would meet it:
//!
//! 1. a tile rendered by the pool is byte-identical to the same tile rendered serially, which is
//!    what makes the pool a pure speedup rather than a second renderer;
//! 2. the base layer is one raster covering the whole page, so it can back every zoom level;
//! 3. a superseded epoch delivers no pixels, and a tile that was inside the rasterizer when the
//!    epoch was superseded reports how long it took to notice - while the tiles it had already
//!    published are still byte-identical to the serial render;
//! 4. an epoch cancelled *before* its work is dispatched publishes nothing at all: the pool drops it
//!    whole, and every drain below fails the moment a tile of an epoch nobody is draining arrives;
//! 5. the abort latency is a property of the page, not a constant: MuPDF checks the cookie at node
//!    boundaries, so the largest single node bounds it. Measured on a page of small nodes and on a
//!    page that is one heavy node.

use engine_mupdf::{EngineHandle, MupdfEngine, worker_count};
use pdf_core::{
    Bitmap, EngineCmd, EngineEvent, PageGeometry, PageSize, PdfEngine, PixelFormat, RequestId,
    TileKey, base_scale_milli,
};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

/// Generous: a cold MuPDF context on a loaded machine, plus the actors' own startup.
const TIMEOUT: Duration = Duration::from_secs(60);
/// How long the event channel has to stay quiet before a batch counts as finished. A pool that
/// ignored its epoch would have delivered its whole batch well inside this.
const QUIET: Duration = Duration::from_millis(500);
/// A cancel signal has to reach the rasterizer within this on a page of many small nodes; anything
/// larger means the cookie is never checked and a zoom does not actually abort the work it
/// superseded.
///
/// It is not a general ceiling, and the Letter-page run that checks it does not verify one: the
/// cookie is tested at *node* boundaries, so one expensive node - [`HEAVY_FIXTURE`]'s whole content -
/// bounds the latency and can exceed any fixed number without anything being wrong. The heavy-tile
/// test measures that case instead of asserting this constant against it.
const ABORT_CEILING_MS: u64 = 500;
/// A page whose content is a single `f` (nonzero fill) of 1000 page-spanning cubics: one node, so an
/// abort cannot land faster than that node runs. See the fixtures' README.
const HEAVY_FIXTURE: &str = "dense_vector.pdf";

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// The next event, panicking on a timeout or an engine-reported error.
fn next(rx: &Receiver<EngineEvent>) -> EngineEvent {
    let event = rx.recv_timeout(TIMEOUT).expect("an engine event");
    if let EngineEvent::Error { message } = &event {
        panic!("engine reported: {message}");
    }
    event
}

/// Opens a fixture on the actor and returns the page table it reported.
fn open(handle: &EngineHandle, rx: &Receiver<EngineEvent>, name: &str) -> Vec<PageSize> {
    handle
        .send(EngineCmd::Open {
            path: fixture_path(name),
            password: None,
        })
        .expect("actor is alive");
    match next(rx) {
        EngineEvent::Opened { page_sizes, .. } => page_sizes,
        other => panic!("expected Opened, got {other:?}"),
    }
}

/// What one epoch's batch turned out to be.
#[derive(Debug, Default)]
struct Drained {
    /// Tiles published for the epoch.
    delivered: i32,
    /// Tiles the cancel reached inside the rasterizer, each with how long it took to notice, in ms.
    aborts: Vec<u64>,
    /// Deliveries carrying rendered ink, so that a batch of blank tiles cannot pass a pixel
    /// comparison by comparing blank against blank.
    inked: i32,
}

/// Whether a tile carries any pixel other than the fixtures' white background.
fn has_ink(bitmap: &Bitmap) -> bool {
    bitmap
        .data
        .chunks_exact(PixelFormat::Rgb8.bytes_per_pixel())
        .any(|pixel| pixel != [255, 255, 255])
}

/// Asserts that a tile from the pool is byte-identical to the serial render of the same cell.
fn assert_matches_serial(
    serial: &mut MupdfEngine,
    geometry: &PageGeometry,
    key: TileKey,
    bitmap: &Bitmap,
) {
    let reference = serial
        .render_tile(geometry, key.col, key.row)
        .expect("serial tile");
    assert_eq!(
        (bitmap.width, bitmap.height, bitmap.stride),
        (reference.width, reference.height, reference.stride),
        "pooled tile {key:?} has the wrong raster shape"
    );
    assert_eq!(
        bitmap.data, reference.data,
        "pooled tile {key:?} differs from the serial render"
    );
}

/// Counts `request_id`'s deliveries and aborts until the channel has been quiet for [`QUIET`],
/// dropping every bitmap as it arrives.
///
/// The first event is awaited with [`TIMEOUT`] rather than [`QUIET`]: a batch that has not started
/// yet is not a batch that has finished, and a loaded machine can take a moment to schedule the
/// first raster.
///
/// With `reference`, every delivered tile is compared against the serial render and dropped right
/// after, so a cancelled batch of megabyte tiles never accumulates in memory. A delivery or an abort
/// for any *other* epoch fails the test: that is what makes "a cancelled epoch publishes nothing"
/// observable from out here, since a pool that ignored the epoch would show up as exactly this.
fn drain_batch(
    rx: &Receiver<EngineEvent>,
    request_id: RequestId,
    mut reference: Option<(&mut MupdfEngine, &PageGeometry)>,
) -> Drained {
    let mut drained = Drained::default();
    let mut wait = TIMEOUT;
    loop {
        let Ok(event) = rx.recv_timeout(wait) else {
            return drained;
        };
        match event {
            EngineEvent::Error { message } => panic!("engine reported: {message}"),
            EngineEvent::TileRendered {
                key,
                request_id: id,
                bitmap,
            } => {
                assert_eq!(
                    id, request_id,
                    "the pool published {key:?} for epoch {id}, which nothing is draining"
                );
                if let Some((serial, geometry)) = reference.as_mut() {
                    assert_matches_serial(serial, geometry, key, &bitmap);
                }
                if has_ink(&bitmap) {
                    drained.inked += 1;
                }
                drained.delivered += 1;
            }
            EngineEvent::TileAborted { request_id: id, ms } => {
                assert_eq!(
                    id, request_id,
                    "the pool aborted work of epoch {id}, which nothing is draining"
                );
                drained.aborts.push(ms);
            }
            _ => {}
        }
        wait = QUIET;
    }
}

/// Queues tiles under `request_id`; the queue is what the pool pulls from, so this is the whole
/// "request" side of the protocol.
fn dispatch(
    handle: &EngineHandle,
    geometry: &PageGeometry,
    request_id: RequestId,
    cells: &[(i32, i32)],
) {
    for (col, row) in cells {
        handle
            .send(EngineCmd::RenderTile {
                geometry: *geometry,
                col: *col,
                row: *row,
                request_id,
            })
            .expect("actor is alive");
    }
}

/// Dispatches the first `count` tiles of `geometry` under `request_id`, optionally superseding the
/// epoch as soon as the first of them is on the wire, and reports what the batch turned out to be.
///
/// Cancelling only once one tile is back is what makes the abort path deterministic: the pool is
/// then provably mid-batch with the queue still deep, so its workers are between registering and
/// publishing a raster rather than idle and about to notice an empty queue.
fn batch(
    handle: &EngineHandle,
    rx: &Receiver<EngineEvent>,
    geometry: &PageGeometry,
    request_id: RequestId,
    count: i32,
    supersede: bool,
    reference: Option<(&mut MupdfEngine, &PageGeometry)>,
) -> Drained {
    let cols = geometry.cols();
    let cells: Vec<(i32, i32)> = (0..count)
        .map(|index| (index % cols, index / cols))
        .collect();
    dispatch(handle, geometry, request_id, &cells);

    if supersede {
        loop {
            if let EngineEvent::TileRendered { request_id: id, .. } = next(rx) {
                if id == request_id {
                    break;
                }
            }
        }
        handle
            .send(EngineCmd::Cancel { request_id })
            .expect("actor is alive");
    }

    drain_batch(rx, request_id, reference)
}

/// Renders `cells` through the pool and checks every one of them against the serial path.
///
/// `cells` receives the grid so that a caller can sample it (the A0's is 30x43) or cover it (the
/// Letter page's 80 cells).
fn assert_pooled_matches_serial(fixture: &str, cells: impl Fn(PageGeometry) -> Vec<(i32, i32)>) {
    const SCALE: u32 = 6_400;

    // The reference is the Phase 1 serial path on the same fixture and geometry.
    let mut serial = MupdfEngine::new();
    serial
        .open_document(&fixture_path(fixture), None)
        .expect("open serial");
    let size = serial.page_size(0).expect("page size");

    let (handle, rx) = EngineHandle::spawn();
    let sizes = open(&handle, &rx, fixture);
    assert_eq!(
        sizes[0].bounds(),
        size,
        "the actor and the serial engine disagree about {fixture}'s page"
    );

    let geometry = PageGeometry::new(0, size, SCALE);
    let cells = cells(geometry);
    dispatch(&handle, &geometry, 1, &cells);

    let drained = drain_batch(&rx, 1, Some((&mut serial, &geometry)));
    assert_eq!(
        drained.delivered,
        cells.len() as i32,
        "the pool dropped tiles of an epoch nobody superseded"
    );
    assert!(
        drained.aborts.is_empty(),
        "an epoch nobody cancelled aborted tiles"
    );
    // Blank is a legitimate tile, so at least one requested cell has to carry ink: otherwise every
    // "identical" comparison above could be comparing two empty buffers.
    assert!(
        drained.inked > 0,
        "no requested cell contains any rendered ink"
    );
}

#[test]
fn pooled_tiles_are_pixel_identical_to_serial_tiles() {
    // Every cell of a small page: parity for the whole grid rather than a sample of it, so a mistake
    // that only shows at one parity - the bleed band, an odd and an even column - cannot hide.
    assert_pooled_matches_serial("large_200p.pdf", |geometry| {
        assert_eq!(
            (geometry.cols(), geometry.rows()),
            (8, 10),
            "the fixture or the tile size changed; this test covers its whole grid"
        );
        (0..geometry.cols())
            .flat_map(|col| (0..geometry.rows()).map(move |row| (col, row)))
            .collect()
    });

    // The A0's grid is 30x43, so it is sampled instead: corners, edges and the middle, which is
    // where the large-format path's raster origin would show up wrong.
    assert_pooled_matches_serial("large_format_a0.pdf", |geometry| {
        let (cols, rows) = (geometry.cols(), geometry.rows());
        vec![
            (0, 0),
            (cols - 1, 0),
            (0, rows - 1),
            (cols - 1, rows - 1),
            (cols / 2, rows / 2),
            (cols / 2 + 1, rows / 2 + 1),
            (1, rows / 2),
            (cols - 2, rows / 2),
        ]
    });
}

#[test]
fn the_base_layer_is_the_whole_page_at_its_base_scale() {
    let (handle, rx) = EngineHandle::spawn();
    let sizes = open(&handle, &rx, "minimal.pdf");
    let scale_milli = base_scale_milli(sizes[0].width.max(sizes[0].height));

    handle
        .send(EngineCmd::RenderBase {
            page: 0,
            scale_milli,
            request_id: 1,
        })
        .expect("actor is alive");

    let (page, scale, bitmap) = match next(&rx) {
        EngineEvent::BaseRendered {
            page,
            scale_milli,
            bitmap,
            ..
        } => (page, scale_milli, bitmap),
        other => panic!("expected BaseRendered, got {other:?}"),
    };
    assert_eq!(page, 0);
    assert_eq!(scale, scale_milli);

    // One raster of the whole page is what lets the app show something before any tile lands, and
    // it is what survives a zoom, so its shape is part of the contract: the page's device rect,
    // not a tile.
    let expected = PageGeometry::new(0, sizes[0].bounds(), scale_milli).device_size();
    assert_eq!(
        (bitmap.width, bitmap.height),
        expected,
        "the base layer must cover the page's whole raster"
    );
    assert!(
        bitmap.width > pdf_core::TILE_SIZE_PX,
        "a base layer no larger than a tile is just a tile"
    );

    // The fixture is a red square centred on white, like the golden-image test: a raster that
    // really ran the page's display list shows both, an empty or cropped one shows neither.
    let n = PixelFormat::Rgb8.bytes_per_pixel();
    let pixel = |x: u32, y: u32| -> [u8; 3] {
        let offset = y as usize * bitmap.stride + x as usize * n;
        bitmap.data[offset..offset + n]
            .try_into()
            .expect("three channels")
    };
    let centre = pixel(bitmap.width / 2, bitmap.height / 2);
    assert!(
        centre[0] > 200 && centre[1] < 50 && centre[2] < 50,
        "page centre should be the fixture's red square, got {centre:?}"
    );
    let corner = pixel(2, 2);
    assert!(
        corner[0] > 240 && corner[1] > 240 && corner[2] > 240,
        "page corner should be white, got {corner:?}"
    );
}

#[test]
fn a_superseded_epoch_delivers_no_pixels_and_aborts_what_was_running() {
    const SCALE: u32 = 6_400;

    // Letter at 6400 permille is one page of 8x10 tiles: the whole batch queues in milliseconds,
    // which is what makes the cancel land mid-batch instead of after it.
    const CELLS: i32 = 80;

    // The serial path on the same fixture, for the tiles the cancel did *not* reach: pixels
    // published while an epoch is being cancelled still have to be the right pixels.
    let mut serial = MupdfEngine::new();
    serial
        .open_document(&fixture_path("large_200p.pdf"), None)
        .expect("open serial");

    let (handle, rx) = EngineHandle::spawn();
    let sizes = open(&handle, &rx, "large_200p.pdf");
    let geometry = PageGeometry::new(0, sizes[0].bounds(), SCALE);
    assert_eq!(
        (geometry.cols(), geometry.rows()),
        (8, 10),
        "the fixture or the tile size changed; the cell count is sized to this grid"
    );

    // Control: an epoch nobody supersedes delivers every tile it was asked for, so a shortfall
    // below is the cancel's doing and not a lost event.
    let control = batch(&handle, &rx, &geometry, 1, CELLS, false, None);
    assert_eq!(
        control.delivered, CELLS,
        "the pool dropped tiles nobody cancelled"
    );
    assert!(
        control.aborts.is_empty(),
        "an epoch nobody cancelled aborted tiles"
    );

    let superseded = batch(
        &handle,
        &rx,
        &geometry,
        2,
        CELLS,
        true,
        Some((&mut serial, &geometry)),
    );
    let workers = worker_count();
    // A pool that ignored the cancel altogether would deliver the whole batch, so a shortfall is the
    // epoch being dropped *at all*. How far short is timing, and only timing: which tiles had already
    // finished when the cancel landed is a race. The sharp form of the contract - an epoch whose
    // cancel has applied publishes nothing, whatever the timing - is
    // `a_cancelled_epoch_publishes_nothing`, where the cancel is ordered ahead of the dispatches and
    // the count carries no timing at all.
    assert!(
        superseded.delivered < CELLS,
        "the superseded epoch delivered all {CELLS} tiles: the cancel lost its race with the pool, \
         so a zoom rasterizes work it has already replaced"
    );
    assert!(
        !superseded.aborts.is_empty(),
        "no tile was inside the rasterizer when its epoch was superseded, so the abort path went \
         untested"
    );
    // Tiles finished before the cancel landed are delivered; the ones still rasterizing when it did
    // are aborted. Only one raster per worker can be in the second group, so a larger count means a
    // raster was reported twice or the registry handed `retire` a stale entry.
    assert!(
        superseded.aborts.len() <= workers,
        "{} rasters reported aborted on {workers} workers",
        superseded.aborts.len()
    );
    let worst = superseded.aborts.iter().copied().max().unwrap_or(0);
    assert!(
        worst < ABORT_CEILING_MS,
        "an aborted tile took {worst} ms to notice, over the {ABORT_CEILING_MS} ms ceiling: the \
         cancel cookie is not reaching the rasterizer"
    );

    println!(
        "[pool] {CELLS} tiles, {workers} workers: {} delivered after the supersede, {} aborted, \
         abort latency {:?} ms (worst {worst})",
        superseded.delivered,
        superseded.aborts.len(),
        superseded.aborts
    );
}

/// An epoch cancelled before its work is even dispatched publishes nothing at all.
///
/// The command mailbox is FIFO and one actor thread drains it, so the `Cancel` is applied before the
/// dispatches behind it are seen: every job of that epoch arrives dead. The pool drops the queued
/// ones on sight, and for any that still rasterized `retire` refuses the pixels under the same lock
/// the cancel took - so nothing can arrive, and [`drain_batch`] fails this test on a tile or an
/// abort belonging to an epoch it is not draining. That check is the observable form of the
/// invariant: without it, a pool that ignored the epoch would show up as extra deliveries nobody
/// looked at.
///
/// The control batch keeps this from passing on a pool that simply does not work: the same cells,
/// dispatched the same way under a live epoch, must all come back.
#[test]
fn a_cancelled_epoch_publishes_nothing() {
    const SCALE: u32 = 6_400;
    const CELLS: i32 = 80;

    let (handle, rx) = EngineHandle::spawn();
    let sizes = open(&handle, &rx, "large_200p.pdf");
    let geometry = PageGeometry::new(0, sizes[0].bounds(), SCALE);
    let cols = geometry.cols();
    let cells: Vec<(i32, i32)> = (0..CELLS)
        .map(|index| (index % cols, index / cols))
        .collect();

    handle.cancel(2);
    dispatch(&handle, &geometry, 2, &cells);

    let control = batch(&handle, &rx, &geometry, 3, CELLS, false, None);
    assert_eq!(
        control.delivered, CELLS,
        "the control batch did not deliver, so its silence about epoch 2 proves nothing"
    );
    assert!(control.aborts.is_empty(), "the control batch aborted tiles");
}

/// What a cancel costs when the tiles are not cheap.
///
/// The ceiling above is a smoke test on a page of hundreds of small nodes and nothing more: MuPDF
/// checks the cookie at *node* boundaries, so what bounds the delay is the largest single node the
/// rasterizer is inside when the cancel arrives. Two fixtures at the same zoom make that visible:
///
/// - `large_format_a0.pdf` is a grid of short strokes and text, so a tile is hundreds of tiny nodes
///   and a cancel lands on the next boundary, in the noise;
/// - `dense_vector.pdf` is one path of 1000 page-spanning cubics, which has to be flattened in
///   device space for every tile it touches: a tile *is* one node, so the delay is however much of
///   that node is left. Nothing is wrong when this one overshoots the ceiling - the page is.
///
/// The report prints a tile's own serial cost next to the latencies, which is the only sane way to
/// read them: the numbers are machine- and load-dependent, the shape is not.
#[test]
fn abort_latency_on_a_heavy_tile() {
    const SCALE: u32 = 6_400;
    let workers = worker_count();

    // Enough work queued that the cancel lands while the pool is deep in the batch: a few of the
    // heavy fixture's tiles per worker (each is slow enough to still be running), a whole row per
    // worker of the A0's sub-millisecond ones, which otherwise finish before the cancel is seen.
    for (fixture, per_worker) in [("large_format_a0.pdf", 30), (HEAVY_FIXTURE, 4)] {
        let mut serial = MupdfEngine::new();
        serial
            .open_document(&fixture_path(fixture), None)
            .expect("open serial");

        let (handle, rx) = EngineHandle::spawn();
        let sizes = open(&handle, &rx, fixture);
        let geometry = PageGeometry::new(0, sizes[0].bounds(), SCALE);

        // A tile's own cost, serially, as the denominator for the latencies below. The first tile of
        // a page carries the one-off display-list build, so a later one is the one timed.
        let _ = serial.render_tile(&geometry, 0, 0).expect("warm-up tile");
        let started = Instant::now();
        let _ = serial
            .render_tile(&geometry, geometry.cols() / 2, geometry.rows() / 2)
            .expect("serial tile");
        let tile_ms = started.elapsed().as_secs_f64() * 1000.0;

        let count = workers as i32 * per_worker;
        let aborted = batch(
            &handle,
            &rx,
            &geometry,
            1,
            count,
            true,
            Some((&mut serial, &geometry)),
        );
        assert!(
            !aborted.aborts.is_empty(),
            "no tile of {fixture} was inside the rasterizer when its epoch was superseded: the \
             fixture is too cheap for this measurement, not the pool too fast"
        );
        assert!(
            aborted.aborts.len() <= workers,
            "{} rasters reported aborted on {workers} workers",
            aborted.aborts.len()
        );
        assert!(
            aborted.delivered + aborted.aborts.len() as i32 <= count,
            "the batch reported more tiles than it was given: {} delivered, {} aborted, {count} \
             dispatched",
            aborted.delivered,
            aborted.aborts.len()
        );

        let worst = aborted.aborts.iter().copied().max().unwrap_or(0);
        println!(
            "[abort] {fixture}: {count} cells on {workers} workers, a tile is {tile_ms:.1} ms \
             serially, {} delivered before the cancel, {} aborted, latency {:?} ms (worst {worst})",
            aborted.delivered,
            aborted.aborts.len(),
            aborted.aborts
        );
    }
}
