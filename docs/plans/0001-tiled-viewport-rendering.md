<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# Plan 0001: Tiled, Viewport-Only Rendering

## Status

**Approved.** Phase 0 gate passed: the three §8 questions are answered and seven amendments
are folded in (§0.2). Phase 1 (geometry, tile rendering, viewport-only app) is **done and at
its gate** — see the "Phase 1 results" block in §5 for the evidence and the numbers.

- Phase 0 (research + measurement + this document) is complete.
- Phase 1 is implemented, verified and gated.
- Phase 2 (worker pool, epoch cancellation through MuPDF's cancel cookie, low-res base layer,
  prefetch ring) is **implemented, measured and closed**: every item on its list is in the code,
  it is gated by the same checks as Phase 1, and both halves of its acceptance have measured
  results — the working-set target in §5 Phase 2, with the numbers in
  [`../benchmarks.md`](../benchmarks.md) §7.6, and the app-level gate run re-measured on the pooled
  build in §7.5. That gate run's one FAIL belongs to the document's own cold open when a zoom burst
  interrupts it — the §7.2-7.3 one-off, which the pool does not move because the display-list build
  it waits on is serial — and the stretched-level item that covers it is Phase 3's.
- Phase 3 remains a proposal.
- The next plan to execute is [`0002-continuous-multipage-scroll.md`](./0002-continuous-multipage-scroll.md).

## 0. Gate record

### 0.1 Decisions (answers to §8)

| Question | Decision |
| :--- | :--- |
| Target `ZOOM_MAX` | **6400%** (`ZOOM_MAX_MILLI = 64_000`). Add an acceptance test that renders the A0 fixture at 6400% and checks for precision jitter; if jitter appears, propose a per-page maximum virtual extent as a precision guard instead of lowering the zoom. |
| Tile cache ceiling | **64 MiB** of bitmap data for desktop, **configurable**, with a **lower default for mobile**. The ceiling a run actually reaches is usually the *row* count instead (a viewport-sized model holds 12 MiB at 900x700), so `--cache-rows` exists to raise that too; on a zoom change, **evict old-scale tiles first**. |
| Tile size | **512 physical px**, as a single named constant (`TILE_SIZE_PX`). |

### 0.2 Amendments (all applied in this revision)

1. Viewport tracking uses `changed content-x` / `changed content-y` (fires on *any* change,
   user or programmatic, including animation ticks), **not** `flicked()` — §3.1, §4.4, §5.
2. Cache keys use **scale in integer permille**; tiles render at exactly that scale. Not
   deferred — §4.1, §4.2, §4.5.
3. Tile keys and geometry types already carry **page index** and a **page origin offset**, so
   Plan 0002 (continuous multi-page scroll) needs no rework. Plan 0002 is recorded as
   [`0002-continuous-multipage-scroll.md`](./0002-continuous-multipage-scroll.md) — §4.1, §4.7.
4. The tile model capacity is **stated as a bound** and overflow is **graceful, never
   panicking** — §4.4, §4.6.
5. **One scratch pixmap per rasterizing thread**, reused for the `TILE+2B` raster; no
   per-tile allocation churn — §3.3, §4.3.
6. The **worker pool** and the **low-res base layer** move to Phase 2, together with the
   parallel-vs-serial pixel-identity test and the MuPDF `Cookie` abort investigation
   (answered in §5 Phase 2: the abort path *is* exposed by the patched crate) — §5, §7.
7. Cache and scheduler logic stay **pure and testable in `core`**, with no `mupdf` or `slint`
   dependency (the separate crate stays dropped) — §4.5, §5.

## 1. Problem

Every zoom or page change re-rasterizes the **entire page** at the new scale
(`EngineCmd::RenderPage`). Cost is proportional to `page_px * scale^2`, so a single zoom
step on a large-format page costs hundreds of megabytes and hundreds of milliseconds, and
at the top of the zoom range the buffer simply cannot be allocated.

Measured baseline (commit `31819cf`, full method and tables in
[`../benchmarks.md`](../benchmarks.md) sections 5-6):

| Fixture | Zoom | Buffer | One zoom step | App peak RSS |
| :--- | ---: | ---: | ---: | ---: |
| Letter 612x792 pt | 1000% | 184.90 MiB | 75.0 ms | **474.24 MB** |
| A0 2384x3370 pt | 400% | 490.36 MiB | 541.3 ms | - |
| A0 2384x3370 pt | 1000% | 3,064.8 MiB | *not attempted* | - |
| A0 2384x3370 pt | 6400% | 125,532.5 MiB | *impossible* | - |

Two independent walls:

1. **Memory.** The 6400% target needs 125 GiB for one buffer. Tiling is mandatory, not an
   optimization.
2. **Texture size.** A 1000% A0 raster is 23840x33700 px, which exceeds the typical GPU
   `MAX_TEXTURE_SIZE` (16384). It cannot be uploaded as one texture at all.

The same math applies to a perfect-bound page: at any zoom only the viewport
(~900x700 logical px, ~4-6 tiles) is ever visible. Work should scale with the viewport,
not the page.

## 2. Goal

Rasterize and retain only the tiles that intersect the viewport, at the current zoom.

Non-goals for Phase 1: text selection, rotation, thumbnail sidebar, multi-page spread,
parallel tile rendering, progressive/low-res previews.

## 3. Phase 0 findings (verified, with evidence)

### 3.1 Slint API (v1.18.1)

| Question | Answer | Evidence |
| :--- | :--- | :--- |
| Element for panning | `Flickable`. `ScrollView` is `Flickable` + scrollbars; we size content from page geometry, so we own the extent and do not want Slint's derived sizing. | `i-slint-compiler-1.18.1/builtin_elements.rs:1494-1520` |
| Property names | `content-x`, `content-y`, `content-width`, `content-height`. `viewport-x/y/width/height` still parse but are deprecated aliases. Use `content-*`. | `builtin_elements.rs:1514-1517` |
| Viewport-changed callback | `callback flicked` fires **only for user input** (drag `flickable.rs:903`, wheel `:563`, phased scroll `:587`, kinetic animation `:669`) and only **once at the start** of the fixed-duration wheel scroll animation (`WHEEL_SCROLL_DURATION = 180 ms`, `flickable.rs:51`). Do **not** use it as the source of truth. Use `changed content-x => { ... }` / `changed content-y => { ... }`, which fires on every change, user or programmatic, including animation ticks. | `i-slint-core-1.18.1/items/flickable.rs` |
| Viewport tracking (amendment 1) | Confirmed: viewport tracking is driven **only** by `changed content-x` / `changed content-y`, never by `flicked()`. A wheel scroll is a 180 ms animation, so `flicked()` alone would request tiles once and then render blank strips while the animation runs; `changed` fires on every animation tick, so tiles stream in during the scroll. The same callback covers programmatic writes. Phase 1 manual checks: (a) animated wheel scroll — tiles must fill in *during* the 180 ms animation, not after it; (b) programmatic write — a scrollbar drag writes `content-x/y`, so the equivalent code path is exercised by the `viewport_moved` counter (§4.4) and by a `core` scheduler test. Phase 1 ships no scrollbars (see §7), so there is no scrollbar widget to drag. | `flickable.rs:520-700`, `builtin_elements.rs:1518-1520` |
| Ctrl+wheel | `TouchArea.scroll-event: Callback<PointerScrollEvent, EventResult>` and `PointerScrollEvent` carries `modifiers: KeyboardModifiers`. Return `EventResult.reject` to let the event fall through to the `Flickable`. | `builtin_elements.rs:1333`; `i-slint-common-1.18.1/builtin_structs.rs:93-100` |
| Pinch | `ScaleRotateGestureHandler` provides `active`, `scale` (cumulative, >1 = zoom in), `rotation`, and a center point. Note: Windows precision touchpads normally deliver pinch as **ctrl+wheel**, so ctrl+wheel is the primary path on this platform; the gesture handler is a bonus. | `builtin_elements.rs:1698-1728` |
| Device pixels | `slint::Window::scale_factor() -> f32` is public. Required to map logical viewport px to device px. | `i-slint-core-1.18.1/window.rs:2290` |
| Texture limits | No explicit cap in Slint/femtovg; the cap is the backend's `MAX_TEXTURE_SIZE` (16384 typical). 512 px tiles are far below it. Confirm the actual value on this machine in Phase 1. | *to verify* |
| Clipping the viewport | `Flickable` unconditionally clips its children and clips to its own bounds, so no wrapper `clip: true` is required. | `items/flickable.rs:236-238` |

### 3.2 Does a Slint `Image` keep its CPU pixels after GPU upload? (Yes — budget 2x)

- femtovg keeps an `Rc<Texture>` in a `TextureCache` keyed by image key, and drains entries
  whose strong count drops to 1, i.e. once no element references the `Image`
  (`i-slint-renderer-femtovg-1.18.1/images.rs:254-301`).
- The `Image` value itself owns a `SharedPixelBuffer`, so the CPU bytes stay resident for
  as long as the model holds the image — *in addition to* the GPU texture.
- Confirmed empirically: Letter at 1000% costs 200-290 MB over the 87 MB baseline against
  an 184.90 MiB buffer, i.e. ~2x the buffer (see `../benchmarks.md` section 5.2).

**Consequence:** each live tile costs roughly `2 x 768 KiB` at 512x512 RGB8 (3 bytes a pixel, §4.9),
and releasing a tile means dropping its `Image` from the model.

### 3.3 MuPDF region rendering (`patches/mupdf`)

The tile recipe is fully supported:

```rust
// Per rasterizing thread, allocated once (amendment 5). The origin is (0,0) on purpose:
// fz_new_draw_device gives the device the identity transform, so device space and buffer
// space coincide and the *CTM* carries the tile offset instead of the pixmap origin.
// alpha = false: three channels, and fz_clear_pixmap then clears to 0xff, i.e. white paper.
let mut scratch = Pixmap::new(&cs, 0, 0, TILE_STRIDE_PX, TILE_STRIDE_PX, false)?;

// Per tile:
let dl   = page.to_display_list(false)?;                  // scale-independent, cached per page
let s    = scale_from_milli(key.scale_milli);             // exact, straight from the key
let bbox = page.bounds()?.transform(&Matrix::new_scale(s, s)).round(); // page raster bbox
let x0   = bbox.x0 + key.col * TILE_SIZE_PX as i32 - TILE_BLEED_PX;    // scratch origin, device px
let y0   = bbox.y0 + key.row * TILE_SIZE_PX as i32 - TILE_BLEED_PX;
scratch.clear()?;                               // fz_clear_pixmap: white, the same init as the page path
let dev  = Device::from_pixmap(&scratch)?;      // identity transform, clip = 0..TILE_STRIDE
let mut ctm = Matrix::new_scale(s, s);          // page pt -> device px
ctm.concat(Matrix::new_translate(-(x0 as f32), -(y0 as f32)));         // concat post-multiplies
dl.run(&dev, &ctm, Rect::new(0.0, 0.0, TILE_STRIDE_PX as f32, TILE_STRIDE_PX as f32))?;
drop(dev);
// copy rows [B .. B+h] x [B .. B+w] out of the scratch into a tight w*h*3 Bitmap
```

Verified (re-checked for this revision):

- `DisplayList::run`, `DisplayList::run_with_cookie`, `Page::run`, `Page::run_with_cookie`,
  `Pixmap::new`, `Pixmap::clear`, `Device::from_pixmap` and
  `Matrix::{new_scale, new_translate, concat}` all exist
  (`patches/mupdf/src/{display_list,page,pixmap,device,matrix}.rs`).
- `Device::from_pixmap` passes `IRect::INF`, and the shim turns that into
  `fz_new_draw_device(ctx, fz_identity, pixmap)` — the device transform is the **identity**,
  not `translate(-x0,-y0)` (`mupdf-sys-0.8.0/wrapper/device.c:4-23`). With a `(0,0)`-origin
  scratch pixmap, device coordinates and buffer coordinates are the same thing, which removes
  every ambiguity around `area`.
- The `area`/`scissor` argument is post-CTM: `fz_run_display_list` binds it to `top_ctm` and
  culls each node with `fz_intersect_rect(fz_transform_rect(rect, top_ctm), scissor)`
  (`list-device.c:1888-1911`). Because the CTM passed to `run` *is* the tile CTM here, the
  scissor is simply the scratch rect `(0, 0, TILE_STRIDE, TILE_STRIDE)`.
- `Matrix::concat` post-multiplies (`matrix.rs:66-81`, `p·M` order), so `S(s) · T(-x0,-y0)`
  maps a page point `p` to `p*s - (x0,y0)` — exactly the device coordinate that the same pixel
  has in a full-page raster whose bbox origin is `bbox.x0/bbox.y0`.
- The full-page path is `fz_new_pixmap_from_page_contents`: bbox =
  `fz_round_rect(fz_transform_rect(fz_bound_page, ctm))`, `fz_clear_pixmap` when `alpha=1`
  (zeros, i.e. transparent) or `fz_clear_pixmap_with_value(..., 0xFF)` when `alpha=0` (white,
  i.e. opaque paper), then `fz_new_draw_device(ctx, ctm, pix)` with the page run at
  `fz_identity` (`fitz/util.c:155-190`). So `page.to_pixmap(&S(s), &cs, false, false)` and a
  tiled render of the same page draw the same objects with the same CTM; tiling only changes
  which objects are culled and where the buffer starts. Both the tiles and the full-page
  primitive are `alpha=0`, which is what makes a 3-channel bitmap safe - the paper is white
  rather than a transparent hole, and the tests assert that corner pixel.
  `fz_round_rect` is `floor(x0+0.001)..ceil(x1-0.001)` (`fitz/geometry.c`), reproduced
  bit-for-bit by `PageGeometry::device_size` in `core`.
- The scratch is always `TILE_SIZE_PX + 2*TILE_BLEED_PX` a side, so one allocation serves every
  tile including the clamped edge tiles; edge tiles rasterize slightly past the page edge and
  the surplus is cropped on copy-out.
- `DisplayList` is `Send + Sync` (`display_list.rs:162-169`).
- `Context::get()` is **thread-local** (`patches/mupdf/src/context.rs:28-82`): each thread
  lazily gets its own `fz_context`, and objects are bound to the context that created them.
  Phase 1 stays single-threaded (ADR 0003) and keeps the display list owned by the actor
  thread. The Phase 2 worker pool needs one `Document` per worker thread rather than one shared
  display list.
- MuPDF's abort channel **is** exposed by the patched crate: `Cookie::new()`, `Cookie::abort()`,
  `progress()`/`max_progress()`/`errors()`/`incomplete()`, plus `DisplayList::run_with_cookie`
  and `Page::run_with_cookie` (`patches/mupdf/src/cookie.rs:14-56`, `display_list.rs:114-131`,
  `page.rs:155-175`). A stale in-flight tile can therefore be aborted mid-render, not merely
  dropped on arrival (§5 Phase 2).

## 4. Design

### 4.1 Tile geometry (pure functions, in `core`)

```rust
// ---- scale is an integer permille value: one source of truth (amendment 2) ----
pub const ZOOM_MIN_MILLI: u32 = 100;        // 10%
pub const ZOOM_MAX_MILLI: u32 = 64_000;     // 6400% (approved target)
pub const ZOOM_STEP_NUM: u32 = 5;           // 1.25 = 5/4, exact in integer permille
pub const ZOOM_STEP_DEN: u32 = 4;
pub fn scale_from_milli(milli: u32) -> f32;         // milli as f32 / 1000.0
pub fn zoom_in_milli(milli: u32) -> u32;            // round(milli * 5/4), clamped
pub fn zoom_out_milli(milli: u32) -> u32;           // round(milli * 4/5), clamped

pub const TILE_SIZE_PX: u32 = 512;          // 768 KiB RGB8 per tile
pub const TILE_BLEED_PX: i32 = 2;           // hidden guard band so AA straddles no seam
pub const TILE_STRIDE_PX: u32 = TILE_SIZE_PX + 2 * TILE_BLEED_PX as u32;

/// Identity of one cached tile. Integers only, so it hashes and compares exactly.
///
/// `page` is the page index; `col`/`row` are cell indices in the *page raster's*
/// device-pixel space, aligned to the raster's top-left corner. The same
/// `(page, col, row, scale_milli)` therefore always denotes the same page region, wherever
/// the page sits in the canvas — which is why Plan 0002 needs no new key type (§4.7).
pub struct TileKey { pub page: u32, pub col: i32, pub row: i32, pub scale_milli: u32 }

/// Geometry of one page at one scale: page index, page origin and size in points, permille
/// scale. The origin is carried from day one (amendment 3), so continuous multi-page scroll
/// only adds a page *offset*, never a new type.
pub struct PageGeometry { /* page, origin (pt), size (pt), scale_milli */ }

impl PageGeometry {
    pub fn new(page: u32, bounds: Rect, scale_milli: u32) -> Self;
    pub fn scale(&self) -> f32;
    /// Page raster size in device px, rounded exactly like MuPDF's `fz_round_rect`
    /// (`floor(x0+0.001) .. ceil(x1-0.001)`), so the grid and the full-page pixmap agree.
    pub fn device_size(&self) -> (u32, u32);
    /// Origin of the page raster in device px, using the same `fz_round_rect` rule.
    pub fn raster_origin(&self) -> (i32, i32);
    pub fn cols(&self) -> i32;
    pub fn rows(&self) -> i32;
    pub fn key(&self, col: i32, row: i32) -> TileKey;
    /// Visible part of a cell, clamped to the page extent (`None` when fully outside).
    pub fn tile_rect(&self, col: i32, row: i32) -> Option<TileRect>;
    /// The full padded raster rect of a cell: `TILE_STRIDE_PX` a side, always.
    pub fn scratch_rect(&self, col: i32, row: i32) -> TileRect;
    /// Cells intersecting a device-pixel viewport rect, clamped to the grid, row-major.
    pub fn visible_cells(&self, view: Rect) -> impl Iterator<Item = (i32, i32)> + '_;
}

/// Device-pixel rectangle, page-raster relative.
pub struct TileRect { pub x: i32, pub y: i32, pub w: u32, pub h: u32 }

/// Keeps a page point stationary across a zoom change (anchored zoom). Permille in and out.
pub fn zoom_anchor(content: f32, anchor_view: f32, old_milli: u32, new_milli: u32) -> f32;
```

Clamping the last tile to the page extent means a Letter page at 25% is a single 153x198
tile (118 KiB), not a padded 512x512 one. The rasterized scratch is always padded, but the
*returned* bitmap is clamped, so neither the cache nor the GPU ever sees the padding.

### 4.2 Message types (`core`)

`EngineCmd::RenderPage` and `EngineEvent::PageRendered` are **removed** — the tiled
renderer replaces them. `PdfEngine::render_page` stays as the full-page primitive used by
the benchmark harness and to validate tiles in tests. `PageSize` gains the page **origin**
(`x`, `y`), because the app needs it to build `PageGeometry`; it is currently dropped by
`all_page_sizes`.

```rust
EngineCmd::RenderTile { geometry: PageGeometry, col: i32, row: i32, request_id: RequestId }

EngineEvent::TileRendered { key: TileKey, request_id: RequestId, bitmap: Bitmap }
```

`PageGeometry` is plain `Copy` integer/float data, so it crosses the thread boundary by value
and the engine needs no extra lookup to know which page region a key denotes. `TileKey` is
echoed back verbatim, so the app matches a result to its request and its model row without
recomputing anything.

`request_id` doubles as the **epoch**: the app bumps it on every page change and every zoom
change, and discards any `TileRendered` carrying an older id. This reuses the cancellation
machinery from ADR 0003 instead of adding a second mechanism.

### 4.3 Engine (`engine-mupdf`)

Add one method and two reusable buffers to `MupdfEngine`. Both take `&mut self` (the actor
thread owns the engine exclusively), so no interior mutability is needed:

```rust
pub fn render_tile(&mut self, geometry: &PageGeometry, col: i32, row: i32)
    -> Result<Bitmap, MupdfError>;

// One scratch raster, allocated on first use and cleared per tile (amendment 5): no
// per-tile allocation churn, and a single pixmap is always TILE_STRIDE_PX a side.
scratch: Option<Pixmap>,
// Display lists are scale-independent, so one is reused across every zoom level of a page.
display_list: Option<(u32 /*page*/, DisplayList)>,
```

Per tile the engine:

1. loads the page (MuPDF caches it) and gets `bbox = page.bounds()?.transform(&S(s)).round()`,
   then checks `bbox` size against `geometry.device_size()`; a mismatch means the grid and the
   renderer disagree, and is reported as `TileRenderFailed` rather than silently drawing the
   wrong region;
2. clears the scratch and runs the cached display list into it with the tile CTM and the
   scratch rect as the scissor (§3.3), creating and dropping the lightweight draw device per
   tile while the 786 KiB scratch pixels are reused;
3. copies rows `[B .. B+h] x [B .. B+w]` out of the scratch into a tight `w*h*3` `Bitmap`
   (`stride == width * 3`, so the UI side needs no stride fixup).

The bleed guard band is what makes the crop exact: the clip boundary sits `B` pixels outside
the returned tile, so every returned pixel is identical to the corresponding pixel of a
full-page render, and antialiasing that straddles a tile edge is already resolved in the
scratch. The actor loop's `EngineCmd::RenderTile` arm mirrors the existing `RenderPage` arm
and keeps the same pre-render and post-render cancellation checks.

New error variant: `MupdfError::TileRenderFailed { page_index: u32, col: i32, row: i32, source: mupdf::Error }`,
plus `MupdfError::TileGeometryMismatch { page_index, expected: (u32, u32), actual: (u32, u32) }`
for step 1.

### 4.4 App (`crates/app`)

**Slint.** Replace the `ScrollView` viewport with a `Flickable` that owns the page extent
and positions tile images at absolute offsets. Tiles live in a fixed-capacity model so the
viewport can change without insert/remove churn:

```slint
export struct TileView { image: image, x: length, y: length, w: length, h: length, visible: bool }

in-out property <[TileView]> tiles;
in-out property <length> page-w;          // device px / scale-factor
in-out property <length> page-h;
in-out property <float> scale-factor: 1.0;
in-out property <int> tiles-requested: 0; // observable counter for the manual checks below

callback viewport_moved();
callback zoom_at(anchor-x: length, anchor-y: length, anchor-milli: int);

Rectangle {
    f := Flickable {
        content-width: root.page-w;
        content-height: root.page-h;
        // Fires on *every* change, user or programmatic, including each tick of the
        // 180 ms wheel animation (amendment 1). Never listen to flicked() instead.
        changed content-x => { root.viewport_moved(); }
        changed content-y => { root.viewport_moved(); }
        // A resize changes which tiles are visible without moving content-x/y.
        changed width  => { root.viewport_moved(); }
        changed height => { root.viewport_moved(); }
        for tile in root.tiles : Image {
            source: tile.image;
            visible: tile.visible;
            x: tile.x; y: tile.y; width: tile.w; height: tile.h;
        }
    }

    // Declared after the Flickable so it receives wheel events first.
    // Rejecting a non-ctrl wheel event forwards it to the Flickable underneath.
    TouchArea {
        scroll-event(event) => {
            if (event.modifiers.control) {
                root.zoom_at(...);
                EventResult.accept
            } else {
                EventResult.reject
            }
        }
    }
}
```

No wrapper `clip: true` is needed: `Flickable` unconditionally clips its children
(`clips_children() -> true`, `items/flickable.rs:236-238`) and clips to its own bounds
(`clip_geometry`, `:226-234`). Slint forwards a wheel event to the parent container only
when the `Flickable` cannot scroll in that direction
(`builtin_elements.rs:1588-1589`), so at a zoom level where the whole page fits the
viewport, non-ctrl wheel events pass through rather than being swallowed.

Tile rectangles are clamped to the page extent and each `Image` is sized explicitly, so the
last row/column of tiles is sized to its true bitmap size rather than to `TILE_SIZE_PX`.

**Rust.** The app owns no cache policy: it holds one `core::TileScheduler` plus the Slint
model, and every viewport change reduces to "ask the scheduler what to show, apply the
actions". The callbacks keep their existing shape (`on_prev_page`, `on_next_page`,
`on_zoom_in/out/reset`) but now end in `viewport_moved()` instead of a single `RenderPage`:

- `viewport_moved()` (fires from `changed content-x/y`, `changed width/height`, and every
  programmatic write): read `content-x/y`, the Flickable's `width`/`height` and
  `Window::scale_factor()`, convert them into a device-pixel rect (all app-side math stays in
  device px; logical px exist only at the Slint boundary: `length = device_px / scale_factor`),
  ask `PageGeometry::visible_cells` for the desired set, then
  `scheduler.update_view(&desired, scale_milli)` and apply the returned actions:
  `Display { slot, .. }` fills a model row, `Request { key }` emits one
  `EngineCmd::RenderTile`, `Clear { slot }` hides and releases a row.
- `TileRendered` is applied on the UI thread via the existing
  `weak.upgrade_in_event_loop(...)` bridge; `scheduler.insert(key, bytes)` picks the model row
  and returns it, or `None` when the tile is stale, duplicated, or there is no free row.
- Zoom uses `zoom_anchor` to keep the viewport centre stationary, writes the new
  `content-x/y` back, then runs the §4.5 epoch sequence. The programmatic write re-enters
  `viewport_moved()`, which is harmless (it computes the same set) but is guarded against
  re-entrancy before pending state is flushed.
- `tiles-requested` is incremented for every emitted `Request`, so the animated-wheel-scroll
  and programmatic-write manual checks have something objective to watch.

Because every tile is tightly packed (`stride == width * 3`), the existing ~25-line
stride-to-stride copy branch in `main.rs:216-229` collapses into a single
`SharedPixelBuffer::clone_from_slice(&bitmap.data, w, h)` call. `set_page_image` and the
`page_image` property go away, replaced by the `tiles` model.

### 4.5 Cache and scheduler (`core`, pure — amendment 7)

`TileScheduler` is pure Rust with **no `mupdf` and no `slint` dependency**, so the whole policy
(eviction order, budget, pending set, capacity clamp, rapid zoom) is unit-testable in `core`
without a document or a UI:

```rust
pub enum TileAction {
    Display { slot: usize, key: TileKey },   // cache hit: put/bring up the image in this row
    Request { key: TileKey },                // miss: send EngineCmd::RenderTile
    Clear { slot: usize },                   // row no longer wanted: release its image
}

pub struct TileScheduler { /* slots, cache, pending, tick, current_scale_milli, budget */ }

impl TileScheduler {
    pub fn new(model_capacity: usize, cache_max_bytes: usize) -> Self;
    /// Diff the live viewport against the cache: set the current scale, clear rows that are no
    /// longer wanted, promote hits, queue misses (skipping keys already pending).
    pub fn update_view(&mut self, desired: &[TileKey], current_scale_milli: u32) -> Vec<TileAction>;
    /// Request tiles *ahead* of the viewport (the one-tile ring). They are cached like any other
    /// tile but are never visible, so they stay evictable and never count as a hole; only rows
    /// the visible set does not claim are used, and nothing is requested once the cache holds its
    /// whole byte budget.
    pub fn prefetch(&mut self, ring: &[TileKey]) -> Vec<TileAction>;
    /// A tile has arrived. Returns the model row it was placed in, or `None` when it is stale,
    /// already present, or no row is free. Enforces the byte budget by eviction.
    pub fn insert(&mut self, key: TileKey, bytes: usize) -> Option<usize>;
    pub fn contains(&self, key: &TileKey) -> bool;
    pub fn slot_of(&self, key: &TileKey) -> Option<usize>;
    pub fn is_pending(&self, key: &TileKey) -> bool;
    pub fn cache_bytes(&self) -> usize;
    pub fn cache_len(&self) -> usize;
    /// Page change: release everything, returning the rows to clear.
    pub fn clear_all(&mut self) -> Vec<TileAction>;
}
```

- Key: `(page, col, row, scale_milli)`. The scale is an **integer permille** value and tiles are
  rendered at exactly `scale_from_milli(scale_milli)`, so a cached tile is always pixel-correct
  for the scale it is drawn at and the key hashes exactly — no float keys, nothing deferred
  (amendment 2). Zoom steps are computed in permille too, so a 1.25x in/out round-trip returns
  the same key: `1000 -> 1250 -> 1563 -> 1954` and back to `1000`.
- `TILE_CACHE_MAX_BYTES` is a byte budget rather than a tile count, because the last
  row/column tiles are smaller. **64 MiB of bitmap data by default on desktop, configurable,
  with a lower mobile default** (`TILE_CACHE_MAX_BYTES_MOBILE = 16 MiB`). Per §3.2 the resident
  cost was about twice the budget while tiles were RGBA8; with RGB8 tiles the Slint image is
  3 bytes a pixel too, so the two now match and the budget is the resident cost.
- **The ring is speculation and is kept apart from what is visible.** `prefetch` requests the
  cells one tile outside the viewport so that a pan finds the column it uncovers already
  rendered; the tiles land in the cache as *hidden* entries, so they are ordinary eviction
  candidates and the hole count (`docs/benchmarks.md` §7.5) does not see them. The visible set
  claims its rows first, and a ring request that would arrive into a cache already at its byte
  budget is not made at all. Measured on the §7.6 sweep: 0 hole frames against 241 of 296
  without it, at the cost of rendering the ring cells the model cannot hold (~3x the tile count
  at the default row count).
- Eviction order when the budget is exceeded: **(1) tiles whose `scale_milli` differs from the
  current scale, oldest first** — a zoom change is what makes the budget overflow, and the
  previous scale's tiles are exactly the ones that can no longer be shown; **(2) then LRU among
  the rest**. A tile that is currently desired is never evicted, and a requested-but-unarrived
  tile reserves no budget.
- Overflow of the *model* is graceful and can never panic: `update_view` clamps the desired set
  to free rows, `insert` returns `None` instead of writing out of bounds, and any key that
  cannot be placed is simply left for the next viewport move (§4.6).
- Dropping a row means `image: Image::default()` and `visible: false`, which releases the
  `SharedPixelBuffer` and lets the renderer's texture cache drain.

**Epoch handling.** A zoom or page change must not leave the viewport half-rendered:

1. bump the epoch (a fresh `request_id`),
2. send `Cancel { request_id: previous_epoch }` once,
3. clear the scheduler's `pending` set,
4. immediately recompute and enqueue the desired set under the new epoch.

This is correct because the engine command channel is FIFO: the `Cancel` reaches the actor
before the new epoch's `RenderTile` messages, so the stale tiles are discarded while the new
ones are not. Whether a finished raster may be published is decided under the pool's registry
lock, in the same critical section the cancel sets the epoch flag in, so a raster whose epoch
was cancelled cannot be published whatever the timing of the two threads
(`crates/engine-mupdf/src/pool.rs`); the flag read before a raster starts is only a way to skip
work that would be discarded anyway. Without step 4 the viewport would stay partially
blank until the next `viewport_moved`, because the tiles dropped by the cancel are still
marked pending.

### 4.6 Tile model capacity (amendment 4)

The model is a fixed-size `Vec<TileView>` allocated at startup (and reallocated only on a
window resize), so a moving viewport never triggers insert/remove churn:

```text
capacity = (ceil(view_w_device / TILE_SIZE_PX) + 2)      // + 2: one margin ring
         * (ceil(view_h_device / TILE_SIZE_PX) + 2)
```

with `view_*_device = viewport_logical_px * scale_factor`, recomputed from the real window on
resize. The `+ 2` is the **prefetch ring** of §4.5: the ring requests the cells one tile outside
the viewport, a viewport that straddles a tile boundary touches at most `ceil(...) + 1` columns, so
the ring's cells always fit inside `ceil(...) + 2` - the ring is prefetched *inside* the model
rather than growing it.

That is the bound: it is derived from window size x scale factor plus a one-tile
margin, and it does **not** grow with the page or with the zoom level, because the number of
visible tiles depends on the viewport, not the page extent. Reference numbers: a 900x700
logical window at scale factor 1.0 needs `(2+2) x (2+2) = 16` rows; the same window at scale
factor 3.0 needs `(6+2) x (5+2) = 56` rows (~3 KB of model memory, since a row is an image
handle plus four lengths and a bool). The *bitmap budget*, not the row count, is what bounds
memory - which is why a run whose cache plateau has to follow the budget has to raise the row
count with `--cache-rows` (`docs/benchmarks.md` §7.6).

Graceful behaviour on overflow, in the order it can happen:

- **More desired tiles than rows** (only if the window grew between the geometry computation
  and the model reallocation): `update_view` places the first `capacity` desired tiles and
  returns a `Request` only for those; the rest stay blank until the next `viewport_moved`. No
  panic, no unbounded growth, and the model is reallocated on the resize event that caused it.
- **More arriving tiles than free rows**: `insert` returns `None`, the tile is dropped, and the
  next `viewport_moved` re-requests it if it is still desired.
- **A zoom or page change with tiles in flight**: old-scale tiles are evicted first, so a burst
  of stale arrivals cannot displace the new scale's tiles.

A `tiles-dropped` counter is exposed next to `tiles-requested` so the manual acceptance run can
confirm overflow stays at zero in normal use.

### 4.7 Plan 0002 readiness (multi-page scroll)

What Phase 1 already provides, so Plan 0002
([`0002-continuous-multipage-scroll.md`](./0002-continuous-multipage-scroll.md)) is additive
rather than a rework:

- `TileKey` and `PageGeometry` both carry the **page index**; keys are never mixed across pages.
- `PageGeometry` carries the page **origin** (`Rect.x/y` from `page.bounds()`), and cells are
  aligned to the page raster, so a page's keys depend only on `(page, col, row, scale_milli)`
  and never on where the page is placed in the canvas. Plan 0002 therefore needs no new key
  type and no cache invalidation when the page layout changes.
- Cache keys are permille-integer, so a multi-page zoom either reuses a page's tiles (same
  scale) or evicts them by the old-scale rule — no partial-key scheme needed.
- What Plan 0002 adds: a page *offset* per page (device px) applied when converting a tile's
  raster rect into a viewport position, a page-gap constant, and the union of `visible_cells`
  across the pages that intersect the viewport. Neither the engine nor the scheduler changes.

### 4.8 Data flow

```
UI thread (Slint)                                Actor thread (MuPDF)
-----------------                                --------------------
changed content-x / content-y / width / height ──> viewport_moved()
   │
   ├─ device_rect = (content-x, content-y, viewport-w, viewport-h) * scale_factor
   ├─ desired = PageGeometry::visible_cells(device_rect) -> [TileKey]
   ├─ ring    = PageGeometry::visible_cells(device_rect grown by one tile) -> [TileKey]
   ├─ scheduler.update_view(desired, scale_milli) -> [TileAction]   // the cells on screen
   └─ scheduler.prefetch(ring) -> [TileAction]                      // the ones a pan takes next
        Display{slot,key} ──> model[slot].image = cached image, visible: true
        Clear{slot}       ──> model[slot].image = default,    visible: false
        Request{key}      ──> EngineCmd::RenderTile { geometry, col, row, epoch } ──┐
                                                                                    v
                                              render_tile(geometry, col, row):
                                                 bbox = page.bounds().round()   [checked]
                                                 dl   = display_list(page)      [cached]
                                                 scratch.clear()
                                                 dl.run(dev, S(s) ∥ T(-x0,-y0),
                                                        (0,0,TILE_STRIDE,TILE_STRIDE))
                                                 crop inner tile -> Bitmap (stride == w*3)
                                                       │
   event listener thread  <── EngineEvent::TileRendered {key, epoch, bitmap} <──────────┘
        └─ upgrade_in_event_loop ──> epoch stale? drop
                                     else scheduler.insert(key, bytes) -> slot
                                          -> model[slot].image = Image::from_rgb8(...)
```

### 4.9 Memory and time budget

Tiles are **opaque RGB8** (3 bytes a pixel): MuPDF clears an alpha-free pixmap to `0xff`, so the
paper is white, and an alpha channel would spend a quarter of every tile on a byte that is always
`255`. That is the same change as the tiles' format in `core` (`PixelFormat::Rgb8`) and it halves
the "CPU + GPU" pair below from the RGBA8 revision.

| Quantity | Value |
| :--- | :--- |
| Tile, 512x512 RGB8 | 768 KiB CPU + 768 KiB GPU |
| Scratch raster, `TILE_STRIDE_PX^2` RGB8 | 786 KiB, **allocated once per rasterizing thread** |
| Tile model rows | `ceil(view_w/sf/TILE+2) * ceil(view_h/sf/TILE+2)`; 16 rows at 900x700 @ sf 1.0 |
| Viewport, 900x700 logical px @ scale factor 1.0 | 2x2 = 4 tiles = 3 MiB |
| Viewport, 900x700 logical px @ scale factor 2.0 | 4x3 = 12 tiles = 9 MiB |
| New tiles per pan step | ~1-2 visible (one new row or column) + ~1-2 prefetched ring cells |
| Tiles re-rendered per zoom step | 4-12 (the whole viewport, but nothing else) |
| Cache ceiling (64 MiB bitmap budget) | ~64 MiB resident, since the Slint copy is 3 bytes too |
| Base layer per page | ~2.3 MiB (1024 px long side, RGB8) |
| Measured plateau, default configuration | 12 MiB of cache, `ws` 160.4 MiB median, `peak_ws` 192.7 MiB |
| A0 @ 6400% | scale-invariant, vs 125 GiB for a full-page buffer |

**The memory target is one metric: the process working set.** `WorkingSetSize`, the number a task
manager shows and the one `docs/benchmarks.md` §7.6 samples every 250 ms as `ws` / `peak_ws`. The
target is **`peak_ws` <= 250 MiB while panning the A0 at 6400% in the default configuration**, and
the measured result is **192.7 MiB**, with no growth across 19 zoom steps and a 296-step pan (§7.6).
Two things are not claimed. The 250 MiB ceiling is a property of the default configuration, not of
every configuration: §7.6's run B raises the row count so the 64 MiB budget can bind and measures
`peak_ws` 267.3 MiB, over the target. And the cache explains only 12 MiB of that 160 MiB plateau:
the process sits about 75 MiB above its own empty baseline (84.9 MiB `ws`) with the default cache,
and an earlier framing of this target - "tiling costs at most +50 MiB over the baseline" - is
**not** met by that measurement. Where the rest goes (MuPDF's store, the display list, the
allocator, committed-but-unresident pages) is not explained here: `docs/backlog.md` tracks it as
investigation, and the target above is stated against the metric that was measured.

Per-tile raster cost extrapolated from the A0 measurements (541 ms for 128.5 M px at 400%,
about 4.2 ns/px) is ~1.1 ms of pixel work per tile, plus a small fixed cost per tile for
device setup, scratch clear and display-list traversal. A 4-6 tile zoom step should therefore
land in the 10-30 ms range instead of 75-541 ms. These are the *targets*; measured Phase 1
numbers are recorded in [`../benchmarks.md`](../benchmarks.md).

## 5. Phases

Each phase is independently shippable and verifiable.

### Phase 1 - Geometry, tile rendering, viewport-only app

Concrete touchpoints:

| File | Change |
| :--- | :--- |
| `crates/core/src/tiling.rs` (new) | `ZOOM_MIN_MILLI`/`ZOOM_MAX_MILLI`, `ZOOM_STEP_NUM/DEN`, `scale_from_milli`, `zoom_in_milli`, `zoom_out_milli`, `TILE_SIZE_PX`, `TILE_BLEED_PX`, `TILE_STRIDE_PX`, `TileKey`, `TileRect`, `PageGeometry`, `zoom_anchor` |
| `crates/core/src/scheduler.rs` (new) | pure `TileScheduler` + `TileAction`, `TILE_CACHE_MAX_BYTES` (64 MiB) and `TILE_CACHE_MAX_BYTES_MOBILE` (16 MiB) |
| `crates/core/src/lib.rs` | `PageSize` gains `x`/`y`; `EngineCmd::RenderPage`/`EngineEvent::PageRendered` become `RenderTile`/`TileRendered`; `ZOOM_MAX` derived from `ZOOM_MAX_MILLI` (64 000 = 6400%); fix `test_clamp_zoom`; re-export the new modules |
| `crates/engine-mupdf/src/lib.rs` | `render_tile` + the reused scratch raster + the per-page display-list cache; `TileRenderFailed`/`TileGeometryMismatch`; `all_page_sizes` fills in the page origin; the `RenderTile` actor arm |
| `crates/engine-mupdf/tests/tile_render_test.rs` (new) | geometry-identity, seam, A0 @ 6400% precision and rapid-zoom tests |
| `crates/app/src/main.rs` | replace the `ScrollView` + `page_image` viewport (`:23`, `:117-125`) with `Flickable` + the `tiles` model, add `viewport_moved`/epoch/scheduler wiring, retarget `trigger_render` (`:254-283`) and the zoom/page callbacks (`:323`, `:340`, `:357`), drop the stride fixup (`:216-229`) |
| `README.md` | drop the references to the removed `render` crate (`:20`, `:73`) |

> **Note:** `core::tests::test_clamp_zoom` asserts `clamp_zoom(5.0) == ZOOM_MAX`
> (`crates/core/src/lib.rs:181`), which fails once `ZOOM_MAX` exceeds 5.0. Update that
> assertion in the same change.

Steps:

1. `core`: the two pure modules (§4.1, §4.5) plus the `PageSize`/`EngineCmd`/`EngineEvent`
   changes and the 6400% ceiling.
2. `engine-mupdf`: `render_tile` with the bleed band and scratch reuse (§4.3), the per-page
   display-list cache, the new error variants, and the actor arm.
3. `app`: `Flickable` viewport, `TileView` model with a computed capacity (§4.6),
   `viewport_moved`, epoch handling, zoom and pagination driving the tile set through the
   scheduler.
4. `README.md` fix; `just check`.

**Checks**

- `core`, geometry: `PageGeometry` cols/rows for Letter and A0 at 100% and 6400%; `tile_rect`
  clamping (full tile, last-column, last-row, fully-outside cell); `device_size` and
  `raster_origin` follow the `fz_round_rect` rule (`floor(x0+0.001)..ceil(x1-0.001)`) for
  origins with fractional parts; `visible_cells` for a viewport straddling a boundary, for one
  past the page edge, and for one starting at a negative device coordinate.
- `core`, zoom: `zoom_anchor` is the identity when the scale is unchanged and keeps the anchor
  point fixed otherwise; `zoom_in_milli`/`zoom_out_milli` round-trip (`1000 -> 1250 -> 1563 ->
  1954 -> ...` and back to `1000`) and clamp at both ends; `ZOOM_MAX_MILLI` is 64 000.
- `core`, scheduler: the byte budget is never exceeded; old-scale tiles are evicted before
  current-scale ones; a desired (visible) tile is never evicted; the model never exceeds its
  capacity and `insert` on a full model returns `None` instead of panicking; a duplicate or
  stale arrival is ignored.
- `core`, **prefetch ring** (Phase 2): a prefetched tile is cached but not visible, and is
  evictable like any other hidden tile; the ring takes only the rows the visible set does not
  claim; it advances on a full cache by taking rows the old ring held; and a ring tile that
  arrives after the ring moved on is cached rather than counted as stale.
- `core`, **rapid zoom**: 20 zoom-in steps in one simulated second
  (`update_view` + `insert` for each step, no waiting) must keep the cache within budget, keep
  the pending set bounded, never exceed the model capacity, and never panic. This is the
  in-process stand-in for the 20-clicks-in-1-second GUI test.
- `engine-mupdf`: `render_tile` returns exact bitmap dimensions for a full tile and for a
  clamped edge tile, with non-blank content, an RGB8 buffer of `width * height * 3` bytes
  (`stride == width * 3`), and white paper in the cell's corner - which is what makes dropping
  the alpha channel safe.
- `engine-mupdf`, **identity test**: reassembling all tiles of `large_200p.pdf` page 0 at 100%
  by their raster rects reproduces `render_page` byte for byte, for every pixel, with no
  tolerance. This is the strongest form of the seam test at a scale where a full-page
  reference is cheap.
- `engine-mupdf`, **seam test**: render the two tiles either side of a boundary on
  `large_200p.pdf` at 400% and compare their shared edge against the same region of a
  full-page `render_page`, allowing a small antialiasing tolerance. This is the test that
  catches a wrong CTM or a missing bleed.
- `engine-mupdf`, **A0 @ 6400% acceptance** (approved decision 1): the grid is 298 x 422 cells;
  rendering a 2x2 block of tiles on a grid crossing must (a) succeed, (b) be deterministic
  (rendering the same tile twice is byte-identical), (c) stay consistent with an independent
  reference produced by `Page::run` into a pixmap for the same region, and (d) show no jitter
  at the tile boundaries. The block sits on a crossing (`1100 pt` and `1600 pt`, ~70,000 device
  px in) rather than on the page centre: the centre is inside one of the fixture's 100 pt cells,
  and a block of white paper would compare two blank buffers and prove nothing. If (c)/(d) fail,
  the plan's fallback is a per-page maximum virtual extent as a precision guard, *not* a lower
  zoom ceiling; the measurement is recorded either way.
- `just check` clean (`fmt-check`, `lint`, `test`, `deny`, `reuse`).

**Manual acceptance**

- Open `large_format_a0.pdf`, zoom to 6400%: content renders, panning stays responsive, RSS
  stays bounded (target <= 250 MB, no growth with repeated zoom-in/zoom-out).
- Open `large_200p.pdf` at 1000%: a zoom step is visibly faster than the 75 ms baseline and RSS
  no longer reaches the 474 MB peak.
- **Animated wheel scroll** (amendment 1): scroll with the wheel and confirm `tiles-requested`
  rises *during* the 180 ms animation and tiles fill in progressively, rather than appearing
  only after the scroll stops.
- **Programmatic viewport write** (amendment 1): zoom with the toolbar buttons and confirm the
  same counter rises, proving `changed content-x/y` — not `flicked()` — is driving requests.
  There are no scrollbars to drag in Phase 1 (§7); a scrollbar drag is the same code path.
- `tiles-dropped` stays 0 throughout the above.

**Phase 1 results (gate)**

Implementation finished and verified. Equivalent of `just check` is clean: `cargo fmt --all --
--check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
`cargo test --workspace` (22 `pdf-core`, 2 + 5 + 6 `engine-mupdf` tests), `cargo deny check`,
`reuse lint` (180/180).

| Evidence | Result |
| :--- | :--- |
| Tiles reassembled vs `render_page`, Letter at 100% and 400% | byte-identical, 0 differing bytes |
| A0 at 6400% (298 x 422 grid) vs an independent `Page::run` render | 0 differing bytes; re-render byte-identical |
| Tile grid vs MuPDF's own raster bbox, 3 fixtures x 3 scales | exact match in size and origin |
| 20 zoom clicks in 1 s, Letter | 37 ms total, slowest step 3.8 ms, peak cache 16 MiB, 0 dropped |
| 20 zoom clicks in 1 s, A0 | 97 ms total, slowest step 7.5 ms, peak cache 16 MiB, 0 dropped |
| A0 400% zoom step | 541 ms / 490 MiB before -> ~5 ms / 6 MiB after |
| A0 6400% zoom step | impossible before -> ~6 ms / 6 MiB after |

Full tables in [`../benchmarks.md`](../benchmarks.md) §7. Four things to report rather than
bury:

1. **An A0 document stalls ~1-2 s on first touch, in both renderers.** The first display-list
   build in a process measured 1813 ms, the second 0.1 ms, and the first full-page render 29 ms:
   it is a one-off MuPDF initialization, the same phenomenon Phase 0 recorded at A0 100%
   (4.3 s). Tiling neither causes nor fixes it; it is now measured in `benchmarks.md` §7.2.
2. **The viewport-derived row capacity binds before the byte budget** for a normal window
   (16 rows = 16 MiB against a 64 MiB budget, and 12 MiB now that tiles are RGB8, §7.6), so the
   budget is the safety net for large or high-DPI windows rather than the everyday bound
   (§4.6, §6).
3. `PageSize` gained the page **origin**, because the app needs it to build `PageGeometry` and
   `all_page_sizes` was dropping it. Required by amendment 3, and reused by Plan 0002.
4. The app was smoke-run (A0 fixture): no panic, and it settles at 137 MB RSS / 0.21% idle CPU,
   which also confirms the `changed content-x/y` tracking does not spin.

Left to the user, because it needs repeated interactive input this environment cannot script:
the 1000% interactive zoom CPU capture, and the two viewport-tracking manual checks (animated
wheel scroll, programmatic write). The `tiles rendered / tiles requested` counters in the
status bar exist for exactly those checks.

**Gate: stopped here, awaiting approval before Phase 2.**

### Phase 2 - Cache, eviction, prefetch, parallel raster

- Enforce `TILE_CACHE_MAX_BYTES` explicitly at the app boundary too, prefetch the ring around
  the viewport so panning does not show blanks, and drop pending requests on epoch change.
- **Render worker pool** (moved here from Phase 1, amendment 6): `N = clamp(cores - 1, 1, 4)`
  threads, one `Document` per thread (MuPDF's `fz_context` is thread-local), sharing the
  display list as `Arc<DisplayList>` (`DisplayList: Send + Sync`). Required test: a
  parallel render of a tile set is **pixel-identical** to the serial result, tile by tile.
  Done in `pool_test.rs`: every cell of the Letter page's 8x10 grid plus an A0 sample, compared
  byte for byte against `MupdfEngine::render_tile`, and the tiles delivered *during* a
  cancellation compared the same way.
- **Low-res base layer** (moved here): a single stretched bitmap of the page for the frame
  during which the new scale's tiles are still in flight.
- **Cookie**: answered in §3.3 — the patched crate already exposes `Cookie::abort()` and
  `DisplayList::run_with_cookie`, so a worker check inside `render_tile` can abort a stale
  in-flight tile in the middle of a raster instead of completing and discarding it. Use the
  epoch as the abort signal; report the measured time-to-abort. Implemented; the honest reading
  of "time-to-abort" is that it is bounded by the *node* the rasterizer is inside, not by a fixed
  ceiling, because MuPDF tests the cookie at node boundaries — a page of small nodes aborts in well
  under a millisecond, while one expensive node cannot be interrupted until it ends. Reported as
  `EngineEvent::TileAborted`; `abort_latency_on_a_heavy_tile` measures both shapes rather than
  asserting a ceiling, which only the fine-grained case can meet.
- **Prefetch ring**: the app requests the cells one tile outside the viewport after the visible
  ones, so a pan finds the column it uncovers already rendered; the scheduler keeps them hidden
  and evictable (`TileScheduler::prefetch`, §4.5) and asks for nothing once the cache holds its
  budget. Done: `docs/benchmarks.md` §7.6 measures **0 hole frames** over a 296-step pan against
  241 of 296 with the ring blocked, and ~3x the tile rasters at the default row count, which is
  the trade the note on `AppState::ring` records.
- Acceptance: pan the A0 fixture at 6400% for 30 s; *working-set* plateau holds within the §4.9
  target, no visible blank tiles after the first frame of a pan step, and a zoom during heavy
  panning aborts stale tiles.

**Phase 2 results (closed)**

Every item above is implemented and tested, and the acceptance run has been made as a release
build in a real window through the `--bench-*` harness. `just check`'s equivalent is clean
(`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace`; 28 `pdf-core`, 5 `app`, and 2 + 5 + 5 + 6 `engine-mupdf` tests).

| Evidence (`cargo test -p engine-mupdf --test pool_test`) | Result |
| :--- | :--- |
| Pool vs serial, Letter @ 6400% (all 80 cells) and an A0 sample | byte-identical, 0 differing bytes |
| A superseded epoch, Letter @ 6400% | queued tiles dropped; in-flight rasters aborted, latency 0 ms |
| An epoch cancelled before its dispatch | 0 deliveries, 0 aborts, 0 strays; the control batch delivered 80/80 |
| Abort latency, `large_format_a0.pdf` @ 6400% (tile 0.9 ms serially) | worst 0 ms |
| Abort latency, `dense_vector.pdf` @ 6400% (one node, tile 24 ms serially) | worst 48 ms |

The last row is the point of the measurement rather than an exception: the cookie is tested between
nodes, so a page whose content is one expensive node bounds the delay, and the abort lands when that
node ends whether or not the epoch is already gone.

The RSS half of the acceptance item is measured in `docs/benchmarks.md` §7.6: a 250 ms sampler, the
cache's row ceiling told apart from its byte budget, and the A0 at 6400% panned to the page edge at
three configurations. Against the §4.9 target - `peak_ws` <= 250 MiB in the default configuration -
the measured result is **192.7 MiB**, the cache plateau follows whichever ceiling is lower (12 MiB of
rows by default, 63.8 MiB of the 64 MiB budget when the rows are raised, 7.5 MiB of 8), and the pan
shows no hole frames. §7.6's table is that half of the acceptance; the app-level gate run is §7.5.

The other half - zoom and pan driven through the real window - is `docs/benchmarks.md` §7.5,
re-measured on the pooled, RGB8, ring build. Settled zoom steps and six-step bursts land in 1-3 ms
`t_blank`, with the *whole-viewport* wait (`t_full`) down from a pre-pool worst of 22 ms to 9 ms on
the A0; panning reports **0 hole frames** at 3900 and 24700 px/s where the pre-ring build reported
3, 9 and 13; `abort_ms` never exceeds 2 ms on any run that sampled one (0 ms in the interrupted-open
run's three aborts, 2 ms worst across the five repeats); running the burst five times per process
passes all five. Its single FAIL is a zoom burst fired while the document's own first
render is still running (714 ms, inside the pre-pool 124-929 ms envelope): the pool parallelizes
tile rasters, not the display-list build, so that one-off is unmoved, and it is the item Phase 3's
stretched level exists to cover. Nothing else on this phase's list is open.

### Phase 3 - Zoom UX and tile pyramid

- Ctrl+wheel zoom (`TouchArea.scroll-event` accepting only when `modifiers.control`),
  `ScaleRotateGestureHandler` pinch, and a second tile level so that during a zoom a
  lower-resolution level can be stretched for one frame instead of showing blanks.
- Acceptance: pinch and ctrl+wheel zoom without a visible flash; with permille keys already in
  place, pinch is quantised to the nearest permille before it reaches the scheduler.

## 6. Risks

| Risk | Impact | Mitigation |
| :--- | :--- | :--- |
| Wrong CTM/scissor composition produces shifted or blank tiles | High, silent | The scratch device transform is the identity and the scissor is post-CTM, both verified in source (§3.3); the tile-identity test and the seam test pin it down |
| Visible 1 px seams between tiles | Medium, cosmetic | Bleed band of 2 px, cropped after rasterizing (§4.3) |
| Each live tile costs 2x its bytes (CPU + GPU) | Medium | Byte-budgeted cache; documented in §3.2 (768 KiB a side since the RGB8 change) |
| The prefetch ring renders tiles the model cannot hold | Low, CPU only | The ring is hidden and evictable, so it costs no memory and is not counted as a hole; §7.6 measures ~3x the tile rasters at the default row count against 0 hole frames, and `AppState::ring` records the upgrade path (a direction-weighted ring) |
| Programmatic `content-x/y` writes re-enter `viewport_moved` | Medium | Re-entrancy guard; `update_view` is idempotent, so a second pass over the same set emits no requests |
| `core`'s `fz_round_rect` replication drifts from MuPDF by 1 px | Low, layout shift only | The engine validates the real bbox size against `PageGeometry::device_size()` and fails the tile loudly; an `engine-mupdf` test compares both for every fixture and scale |
| Deep zoom gives f32 sub-pixel jitter (A0 @ 6400%) | Medium | Measured deterministically against an independent `Page::run` reference by the A0 @ 6400% acceptance test; the agreed fallback is a per-page maximum virtual extent, not a lower ceiling |
| `TileScheduler` model overflow | Low | Bounded capacity plus graceful blank/`None` behaviour (§4.6); covered by the rapid-zoom test |
| `Flickable` clamping of `content-x/y` after a page-extent change | Low | Verify empirically in Phase 1; clamp explicitly after zoom |
| MuPDF objects are bound to a thread-local `fz_context` | Blocks parallelism | Phase 1 is single-threaded; the Phase 2 pool uses one document per thread (§3.3) |
| `README.md` still describes a `render` crate removed in `234acba` | Documentation drift | Fixed in Phase 1; the tile geometry lives in `core`, so no new crate is introduced |

## 7. Deliberate non-goals

- **No new crate.** An earlier draft put tile logic behind a `TileRenderer` trait in a new
  `render` crate. There is exactly one backend, so the trait would be speculative, and
  `234acba` removed that crate for the same reason. The geometry *and* the scheduler are pure
  and live in `core` (amendment 7); the rasterizer stays in the only crate that has MuPDF.
- **No parallel tile rendering in Phase 1.** It needs one document per thread under MuPDF's
  thread-local contexts, and ADR 0003 already documents this as a deferred concern; amendment 6
  moves it to Phase 2, where the abort path (Cookie) is available too.
- **No direction-aware prefetch ring.** The ring requests one tile on *every* side of the
  viewport, so a pan also pays for the tile it is not moving towards (§4.9, §6). One ring for all
  directions is what makes `prefetch` a pure function of the viewport rect - no travel history, no
  extra state - and it is the version §7.6 measured; a ring weighted by the last movement is the
  upgrade path if the extra rasters ever matter more than the blank frames they remove.
- **No scrollbars in Phase 1.** This plan deliberately replaces `ScrollView` with `Flickable` to
  own the content extent. A scrollbar is only another writer of `content-x/y`, so it adds no new
  tracking path (amendment 1); add one in Phase 3 if the UX needs it.
- **No interim `ZOOM_MAX` bump.** The ceiling rises to 6400% in the same change that introduces
  tiling, so full-page buffers stay bounded until then.

## 8. Questions for the approver — resolved

All three questions were answered at the Phase 0 gate and are recorded in §0.1 (6400% ceiling
with the A0 precision acceptance test; 64 MiB configurable cache with a lower mobile default and
old-scale-first eviction; 512 px tiles as a single named constant). No open question remains
before Phase 1. The Phase 1 gate re-opens this section only if the A0 @ 6400% precision
measurement turns out to need the per-page virtual-extent guard.
