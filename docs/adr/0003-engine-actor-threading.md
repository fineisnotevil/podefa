<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# ADR 0003: Dedicated Engine Actor Threading Model & Render Cancellation

## Status

Accepted; amended 2026-10-20 (see "Amendment" below - the rest of this record is the original
decision, kept as written).

Amended again by ADR 0011 (2026-10-25): a window has one engine actor per tab, and they share one
render pool instead of each starting its own.

## Amendment: tiles, a worker pool and epochs (Plan 0001, Phases 1-2)

The decision below still holds in its core - **one actor thread owns the `Document`, and the UI only
sends commands and receives events** - but the rendering it describes was replaced:

- **No `RenderPage`.** The UI asks for tiles (`EngineCmd::RenderTile`) and a per-page base layer
  (`EngineCmd::RenderBase`); the actor answers with `TileRendered`, `BaseRendered`, `TileAborted`
  and `Opened` (`crates/core/src/lib.rs`).
- **The actor no longer rasterizes.** It loads the page, validates the tile grid against MuPDF,
  takes the page's display list from an LRU of eight, and dispatches the resolved job to a **render
  worker pool** of `clamp(cores - 1, 1, 4)` threads (`crates/engine-mupdf/src/pool.rs`). The pool
  never touches the document; `DisplayList` is `Send + Sync`, which is what makes parallel rendering
  possible. The "Single-Page Rendering Throughput" consequence below is resolved by this.
- **Cancellation is by epoch, not by request.** A zoom, a jump or a new document starts an epoch;
  `Cancel` drops the old epoch's queued tiles, and whether a finished raster may be published is
  decided under the registry lock `cancel` takes, so a cancelled epoch never publishes pixels.
  Aborting the tile *inside* the rasterizer through MuPDF's cookie is implemented but **switched off**
  since 2026-10-20 (`MID_RASTER_ABORT`): with mupdf-rs 0.8 an aborted run can end the process
  (`docs/benchmarks.md` §9.7, `docs/backlog.md` item 7).

ADR 0004 and Plan 0003 add further work to this model (opening on a running app; a search thread
with its own document is planned for Plan 0003 Phase 3 and will get its own ADR).

## Context

`PODEFA` requires rendering complex PDF pages and responding interactively to UI inputs (such as rapid zoom and page flips) without stalling the user interface.

Two fundamental architectural constraints govern this interaction:
1. **MuPDF Thread Safety Limitations**: MuPDF documents (`fz_document`) and context structures (`fz_context`) are not safely shareable across multiple threads simultaneously. Attempting to access or render pages from multiple threads without locks leads to race conditions, and passing raw pointers across threads violates Rust's memory model (`*mut fz_document` is neither `Send` nor `Sync`).
2. **UI Responsiveness & Interactive Latency**: Rendering a high-resolution or vector-dense PDF page can take tens to hundreds of milliseconds. If executed synchronously on the Slint UI thread, the interface freezes, dropping frames and degrading user experience.
3. **Outdated Work Cancellation**: In an interactive viewer, users frequently spin the scroll wheel or trigger rapid zoom commands. Generating full-resolution rasterizations for intermediate zoom levels or skipped pages wastes CPU cycles and memory.

## Decision

We adopt a **Single-Threaded Engine Actor Pattern** with explicit **Request ID Cancellation**:

1. **Document Ownership**: Exactly one dedicated background worker thread (`mupdf-engine-actor`) owns the `MupdfEngine` and the underlying `mupdf::Document`. The UI thread never directly touches the MuPDF engine or any FFI pointers.
2. **Asynchronous Command Channel**: Communication between the UI thread and the engine actor occurs exclusively over message-passing channels:
   - UI thread dispatches `EngineCmd` (`Open`, `RenderPage`, `Cancel`, `Shutdown`) via `std::sync::mpsc::Sender<EngineCmd>`.
   - Engine actor emits `EngineEvent` (`Opened`, `PageRendered`, `Error`) back to the UI thread via `std::sync::mpsc::Sender<EngineEvent>`.
3. **UI Dispatch via Weak Handles**: The event listener thread holds a `slint::Weak<MainWindow>` handle. When an event arrives from the actor, it schedules updates onto the main event loop using `weak.upgrade_in_event_loop(closure)`. No UI elements are ever transferred across thread boundaries.
4. **Cancellation Strategy**:
   - Every `RenderPage` command carries a monotonically increasing `RequestId`.
   - The UI thread records the most recent request ID in an `AtomicU64`. When a new request supersedes a pending one, or when `EngineCmd::Cancel` is issued, the cancelled ID is recorded.
   - The engine actor checks cancellation **before** initiating page rasterization and **before** dispatching the completed bitmap back across the channel.
   - The UI thread performs a final check upon receiving `PageRendered` to discard any frame rendered before cancellation was registered.
5. **Clean Shutdown**: `EngineHandle` implements `Drop`, which transmits `EngineCmd::Shutdown` and joins the background thread, eliminating thread leaks.

## Consequences

### Positive
- **Guaranteed Memory Safety**: `*mut fz_document` never escapes the single thread that created it, adhering strictly to MuPDF's concurrency model.
- **Fluid 60 FPS UI Thread**: The Slint event loop remains completely unblocked during rasterization workloads.
- **Resource Conservation**: Superseded zoom levels and fast page flips are dropped early, minimizing CPU consumption and memory allocations.
- **Leak-Free Lifecycle**: Worker threads terminate predictably when the application window or engine handle is dropped.

### Negative
- **Single-Page Rendering Throughput**: Because only one worker thread touches the document, multiple visible tiles or facing pages cannot be rasterized in parallel across multiple CPU cores without maintaining separate document contexts or page-level locking. (This will be addressed in future phases if tile scheduling requires multi-worker pipelines).
- **Channel Allocation Overhead**: Passing messages and `Bitmap` buffers introduces minor heap allocation and synchronization overhead, though this is negligible compared to rasterization time.
