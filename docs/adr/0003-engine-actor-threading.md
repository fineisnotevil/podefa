<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# ADR 0003: Dedicated Engine Actor Threading Model & Render Cancellation

## Status

Accepted

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
