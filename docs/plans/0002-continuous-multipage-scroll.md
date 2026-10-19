<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# Plan 0002: Continuous Multi-Page Scroll with Page Virtualization

## Status

**Phase 0 done, at its gate - awaiting approval before Phase 1.** No feature code has been written.
Phase 0 added one ignored measurement test (`crates/engine-mupdf/tests/page_sizes_bench.rs`) and the
numbers it produced ([`../benchmarks.md`](../benchmarks.md) §8).

Registered at the Plan 0001 Phase 0 gate so that the Plan 0001 tile key and geometry types do not
have to be reshaped later. The original prerequisite - Plan 0001 reaching its Phase 3 gate - was
relaxed by the maintainer on 2026-10-20: [`0001-tiled-viewport-rendering.md`](./0001-tiled-viewport-rendering.md)
is closed through Phase 2, its Phase 3 (zoom gestures, stretched tile level) is deferred, and nothing
below depends on it.

- Phase 0 (research, measurement, this document): **done**. Gate questions in §9.
- Phase 1 (layout, windowed scroll, virtualization): proposed.
- Phase 2 (navigation and input): proposed.
- Phase 3 (progressive page sizes): conditional - only if §3.5's real-world measurement asks for it.

## 0. Gate record

*Empty until the maintainer answers §9.*

## 1. Problem

The viewer shows one page at a time. `set_page` drops every tile and the base layer, starts a new
epoch and resets the scroll position (`crates/app/src/main.rs`, `AppState::set_page`), so reading a
document is a sequence of page flips. Continuous scrolling - every page laid out one under another,
scrolled as one surface - is what every desktop PDF viewer does by default, and it is the layout
every later feature (thumbnails, search hits, text selection across pages, annotations) has to be
placed in.

Two things make it more than "stack the pages":

1. **Coordinates.** A 1000-page Letter document at 6400% is 1000 x 792 pt x 64 = **50.7 M device px**
   tall. Slint lengths are `f32` (§3.1), which is exact for integers only up to 2^24 = 16.7 M, and
   which resolves 4 px steps at 50 M. Scrolling has to be exact at any zoom and any page count,
   without capping either.
2. **Cost has to follow the viewport, not the document.** Plan 0001 made a page's cost scale with
   the visible area; a document of pages has to keep that property: tiles, base layers, display
   lists and model rows held only for the pages near the viewport, and nothing that grows with the
   number of pages visited.

## 2. Goal and scope

Scroll continuously through the whole document, single column, with the same viewport-only cost as
Plan 0001, at every zoom in `ZOOM_MIN_MILLI..=ZOOM_MAX_MILLI` and every page count.

**In scope:** the page layout (mixed sizes, rotation, MediaBox/CropBox origins, a page gap), exact
scrolling at any document height, page virtualization, zoom anchored on a point across pages,
fit-width / fit-page, and navigation: current-page indicator, go-to-page, PgUp/PgDn, Home/End,
arrows, space, zoom shortcuts, ctrl+wheel.

**Out of scope, not designed out:** thumbnails, outline, search, text selection, two-page spreads,
horizontal scroll mode. The layout keeps an `x` offset per page and a page-at-point query so each of
them is an addition, not a rework (§4.1).

## 3. Phase 0 findings (verified, with evidence)

### 3.1 Slint coordinates are `f32`

`i-slint-core-1.18.1/lib.rs:105` defines `pub type Coord = f32` (the `i32` alternative at `:107` is
for the integer-coordinate MCU build), and the `Flickable`'s `content_x/y/width/height` are
`Property<LogicalLength>` over it (`i-slint-core-1.18.1/items/flickable.rs:62-65`). Every item `x/y`
in the tile model is the same type. Consequences:

| Document extent (logical px) | `f32` resolution |
| ---: | ---: |
| 2^20 = 1.05 M | 0.125 px |
| 2^24 = 16.7 M | 1 px |
| 50.7 M (1000 Letter pages @ 6400%) | 4 px |
| 1.01 G (20000 Letter pages @ 6400%) | 64 px |

So the document position cannot live in Slint. It must live in Rust as an exact integer/`f64`, and
whatever Slint is given must stay small (§4.2).

Within one page the Plan 0001 geometry is already safe: the largest page raster in the fixtures is A0
at 6400%, 152,576 x 215,680 px, well under 2^24, and `PageGeometry` works in page-raster coordinates.
The precision problem is purely the *sum* of pages.

### 3.2 The `Flickable` animates `content-y` itself

A fling is a physics animation installed on the `content_x/y` properties
(`set_physic_animation_value`, `flickable.rs:660-680`, deceleration 2000 px/s^2 at `:47`), and a wheel
step is a 180 ms animation (`WHEEL_SCROLL_DURATION`, `:51`). Writing `content-y` from Rust replaces
that value, so any scheme that rewrites `content-y` while the user is scrolling can stop a fling.
The design therefore rewrites it rarely (once per ~260 k logical px of travel, §4.2) rather than every
frame. A fling at 10,000 px/s travels v^2 / 2a = 25,000 px, an order of magnitude below that, so in
practice a re-anchor lands between gestures; Phase 1 measures it instead of assuming it (§7).

### 3.3 What the single-page app does that continuous scroll cannot keep

| Today (`crates/app/src/main.rs`, `crates/engine-mupdf/src/lib.rs`) | Why it breaks |
| :--- | :--- |
| `AppState::page` - one page; `visible`/`ring` take one `PageGeometry` | Several pages are visible at once |
| `set_page` begins an epoch and `release_all`s on every page change | Crossing a page boundary would flush the cache and blank the screen |
| One `base-image` property for the page on screen | Every visible page needs its own backdrop |
| `MupdfEngine::display_list: Option<(u32, Arc<DisplayList>)>` - a one-page cache | Interleaved requests for two visible pages rebuild a display list on every switch; the first build of an A0 page measured 1.8 s (benchmarks §7.2) |
| `tile_capacity` sizes the model from the window alone, and only on resize | At 10% many small pages are visible, each needing at least one tile, so the visible-cell count depends on zoom and page sizes |
| `zoom_anchor` scales the content offset by `new/old` | The page gap is constant in device px, so scaling the whole offset drifts the anchor by `gap x pages_above x (new/old - 1)` |
| `all_page_sizes().unwrap_or_default()` in the actor's `Open` arm | One page whose bounds fail makes the *whole* table empty, and the app then shows no document |

What carries over unchanged, as Plan 0001 §4.7 promised: `TileKey` (with `page`), `PageGeometry`
(with the page origin), permille scales, the eviction rules, `render_tile`, the pool, cancellation and
the actor protocol for tiles.

### 3.4 Page bounds already include rotation and the crop box

`Page::bounds()` calls `fz_bound_page`, which for PDF is `pdf_bound_page`: the crop box intersected
with the media box (Letter when empty, the unit rect when under 1 pt), transformed by the page CTM,
which carries `UserUnit`, the snapped `/Rotate` and the translation of the crop box origin to the top
left (`mupdf-sys-0.8.0/mupdf/source/pdf/pdf-page.c:655-661`, `:731-811`). So the `PageSize` table the
engine already sends is the *displayed* size: a `/Rotate 90` page arrives with width and height
swapped, and the layout needs no rotation logic of its own. §3.5's harness checks this against an
independent reimplementation of the rule on every page it generates - in the 20,000-page document,
5,613 of them are landscape, rotated or cropped - with no mismatch.

### 3.5 What reading every page size at open costs

Measured by `crates/engine-mupdf/tests/page_sizes_bench.rs` (release, 2026-10-20, the §1 machine of
`benchmarks.md`; full table in `benchmarks.md` §8). Two paths, each on a freshly opened document, and
asserted equal page by page before any time is reported:

- **load**: today's `all_page_sizes`, one `fz_load_page` + `fz_bound_page` per page;
- **object**: page object from the page tree plus MuPDF's own page CTM, no page load.

| Synthetic document | open | load path | object path |
| :--- | ---: | ---: | ---: |
| 1,000 pages | 12-19 ms | 7.6 ms (7.6 us/page) | 7.8 ms |
| 1,000 pages, 1 link annotation each | 12 ms | 13.3 ms (13.3 us/page) | 10.6 ms |
| 5,000 pages | 13 ms | 36.5 ms | 39.0 ms |
| 5,000 pages, annotated | 14 ms | 67.8 ms | 54.4 ms |
| 20,000 pages | 22 ms | 145.9 ms (7.3 us/page) | 148.7 ms |
| 20,000 pages, annotated | 23 ms | 251.7 ms (12.6 us/page) | 220.7 ms |

Findings:

1. **The cost is linear and small** - 7-13 us a page, 8-13 ms for 1000 pages.
2. **Skipping the page load buys almost nothing** (0-20%): the time goes into resolving the page
   objects, not into the annotation and transparency work `fz_load_page` adds. So there is no reason
   to reimplement MuPDF's box rules in Rust; the load path stays, with no drift risk.
3. **These are best cases.** The generated files are uncompressed, with a flat two-level page tree and
   a correct xref. A real large PDF typically stores its objects in compressed object streams, so the
   first touch of a page decompresses a stream. No real-world document was measured, because none is
   in the repository; the harness takes one through `FA_PDF_BENCH_PDF` (§9 question 5).

Decision proposed: Phase 1 keeps reading every size at open on the actor thread (13 ms per 1000
annotated pages is below anything a user notices), fixes the error swallowing (§3.3), and keeps the
layout able to hold *estimated* sizes (§4.1) so that progressive resolution - Phase 3 - is an addition
if a real document shows it is needed. The open threshold that would trigger Phase 3 is stated in §6.

## 4. Design

### 4.1 Layout (`core`, pure - new module `layout`)

```rust
pub struct DocLayout {
    scale_milli: u32,
    gap_px: u32,               // device px between pages, constant across zoom
    pages: Vec<PageSlot>,      // one per page, in document order
    width: u64,                // widest page raster, device px
    height: u64,               // sum of page heights + gaps, device px
}
pub struct PageSlot {
    pub geometry: PageGeometry, // Plan 0001's, unchanged: page index, origin, size, scale
    pub x: u64,                 // left edge in document device px (pages are centred)
    pub y: u64,                 // top edge in document device px (prefix sum)
    pub estimated: bool,        // Phase 3 hook: size is a guess, to be corrected
}
```

- **Exact integers.** Each page's raster size is `PageGeometry::device_size()` - the MuPDF rounding
  rule Plan 0001 already pins to MuPDF pixel for pixel - and offsets are `u64` prefix sums of those
  integers plus the gap, so a page's position is exact at any page count and zoom. Nothing is
  accumulated in floating point.
- **Rebuilt per scale.** A zoom recomputes the table: one `device_size` per page, which is a few float
  operations; 20,000 pages is well under a millisecond (to be confirmed by a `core` unit benchmark in
  Phase 1, not assumed).
- **Queries:** `page_at(y) -> Option<usize>` and `pages_in(y0..y1) -> Range<usize>` by binary search
  on the prefix sums; `visible_cells(view) -> Vec<TileKey>` unions each intersecting page's Plan 0001
  `visible_cells` with that page's offset subtracted; `place(key) -> (x, y, w, h)` in document px.
- **Mixed sizes and rotation** come from the page table (§3.4). Narrower pages are centred on the
  widest one, so the horizontal extent is the widest page.
- **Gap:** one constant, `PAGE_GAP_LOGICAL_PX` (proposed 8), converted to device px with the scale
  factor, so it does not grow with zoom.
- **Not designed out:** spreads and a horizontal mode are a different `x`/`y` assignment over the
  same `PageSlot`s; thumbnails are the same table at a small scale; search hits and selections are
  page-relative rectangles placed with `PageSlot::x/y`.

### 4.2 Exact scrolling: document position in Rust, a bounded canvas window in Slint

The document position is `(scroll_x, scroll_y): (f64, f64)` in device px, owned by Rust (exact for
integers up to 2^53). Slint is given a **canvas window**: a band of the document around an integer
anchor `A`, at most `CANVAS_HALF = 2^20` logical px either side of it, clamped to the document.

- `Flickable.content-height = (window_end - window_start) / sf`; every tile/backdrop row is placed at
  `(document_y - window_start) / sf`, computed in `f64` and cast once, so all positions Slint sees are
  below 2^21 logical px, where `f32` resolves 0.25 px or better. Two edges that meet in the document
  are cast from the same `f64` and stay identical in `f32`, so no seam appears between tiles.
- `scroll_y = window_start + (-content-y) * sf`, read in the existing `changed content-y` handler.
- **Re-anchor:** when the viewport comes within `2^18` logical px of a window edge that is not the
  document edge, Rust moves `A` to the current position, recomputes `window_start/end`, and rewrites
  `content-y`, `content-height` and every placed row in one programmatic update (the existing
  `with_programmatic_guard`). The view does not move; only the numbers under it do. At most once per
  ~260 k px of travel, so the fling interruption of §3.2 is rare - and measured in Phase 1.
- **The window ends where the document ends**, so the `Flickable`'s own clamping and overscroll
  behaviour are unchanged at the top and bottom of the document.
- The same scheme applies to `x` for generality (a wide page at 6400% is 152 k px, still inside one
  window, but `UserUnit` pages can be larger).
- **Scrollbar:** a native scrollbar over the window would show the window, not the document, so the
  scrollbar is a small custom Slint component whose thumb position and size Rust computes from
  `scroll_y / height`, and whose drag Rust maps back to a document position (Phase 2).

*Alternative considered:* drop `Flickable` and implement drag, wheel and kinetic scrolling in Rust
over a `TouchArea`. Exact by construction, but it re-implements input handling that Slint already
gets right on three platforms, and it loses the platform's touchpad phases. Kept as the fallback if
Phase 1 measures re-anchoring as visible.

### 4.3 Virtualization and level of detail

Per page, by its displayed size at the current scale:

- **Large page** (raster larger than one tile in either direction): Plan 0001 as it is - tiles for the
  visible cells plus the ring, over the page's own low-res base layer.
- **Small page** (raster at most `TILE_SIZE_PX` square): one **page image** at the display scale,
  rendered as a single tile-sized job. No base layer: the page image *is* the page.

Why the split: at low zoom many pages are visible and each one needs at least one tile, so a tile
model sized from the window alone (§3.3) would run out of rows; and a 1024 px base layer per small
page would cost 2.3 MiB each to draw something 80 px tall. With the split:

- tiled pages are at least one tile tall, so at most `ceil(view_h / TILE_SIZE_PX) + 1` of them are
  visible, and the tile model bound becomes `(cols + 2) x (rows + 2 + pages_visible)` instead of the
  window alone;
- small-page images are fully visible except the first and last, so their total pixels are bounded by
  the viewport area plus two tiles - memory follows the viewport, not the page count;
- base layers are held only for visible tiled pages (a small LRU keyed by page, so scrolling back
  does not re-render the one just left).

All three - tiles, page images, base layers - are evictable entries keyed by page, and none is held
for a page outside the visible range plus a one-page margin. The page table is the only thing
proportional to the page count: `PageSize` (16 bytes) plus `PageSlot` (about 48 bytes) a page, about
1.25 MiB for 20,000 pages.

**Engine side** (`engine-mupdf`), the one change Plan 0002's earlier sketch said would not be needed:
the display-list cache becomes a small LRU keyed by page (capacity = visible pages + margin, proposed
8), because two visible pages would otherwise rebuild each other's list on every tile. The size of a
display list is not known (backlog item 1); Phase 1 reports it per fixture and bounds the LRU by count
until there is a number to bound it by bytes.

### 4.4 Epochs and cancellation across pages

- **Scrolling never starts an epoch.** Keys carry the page, so tiles of every page coexist; the
  scheduler's `update_view` already hides what left the view and requests what entered it.
- **A zoom starts an epoch**, as today.
- **A jump** (go-to-page, Home/End, a scrollbar drag that lands far away) starts an epoch: everything
  pending belongs to a place the user has left.
- **A fast fling** leaves `unwanted` work in the pool (Plan 0001 §7.5's metric). Phase 1 measures it on
  the fling benchmark; if it delays the visible tiles measurably, the fix is to begin an epoch when
  `unwanted` exceeds the visible count (no new protocol), not a per-key cancel.

### 4.5 Zoom across pages, fit-width, fit-page

The anchor is page-relative, because the gap does not scale: find the page under the anchor point,
take the point in that page's raster at the old scale, scale it to the new scale, add the page's new
offset, subtract the anchor's position in the viewport. Exact with the integer layout, and it fixes the
`gap x pages_above` drift of the global `zoom_anchor` (§3.3). Unit tests in `core`: an anchor on
page 1 and on page 900 of a 1000-page layout stays within one device pixel across every zoom step from
10% to 6400% and back.

- **Fit-width:** the scale at which the current page's width fills the viewport width.
- **Fit-page:** the scale at which the current page fits both dimensions.
- Both quantised to permille, clamped to the zoom range, and anchored at the current page's top.

### 4.6 Navigation and input (Phase 2)

| Action | Behaviour |
| :--- | :--- |
| Current-page indicator | The page under the viewport's vertical centre (nearest page when the centre is in a gap) |
| Go-to-page | A numeric field; scrolls so the page's top is at the viewport top; starts an epoch (§4.4) |
| PgUp / PgDn, Space / Shift+Space | One viewport height minus a 10% overlap |
| Home / End | Document top / bottom |
| Arrow keys | A fixed line step (proposed 40 logical px) |
| Ctrl + / Ctrl - / Ctrl 0 | Zoom in / out / 100%, anchored at the viewport centre |
| Ctrl+wheel | Zoom anchored at the cursor: `TouchArea.scroll-event`, accepted only with `modifiers.control`, rejected otherwise so the `Flickable` scrolls (Plan 0001 §3.1 has the API evidence) |

Keyboard input goes through a `FocusScope` around the viewport. **Ctrl+wheel moves here from Plan 0001
Phase 3** (input only; pinch and the stretched second tile level stay in the deferred phase) - §9
question 4.

## 5. Phases

### Phase 1 - Layout, windowed scroll, virtualization

- `core::layout` (§4.1) with unit tests: prefix sums, binary search, mixed sizes, rotated pages,
  non-zero origins, empty document, 20,000 pages; page-relative zoom anchor (§4.5).
- App: document position in Rust and the canvas window with re-anchoring (§4.2); tiles across pages,
  page images for small pages, per-page base layers (§4.3); epoch rules (§4.4); Previous/Next become
  "scroll to page"; the existing page indicator follows the scroll.
- Engine: display-list LRU (§4.3); `all_page_sizes` failures no longer empty the table - a page whose
  bounds fail gets Letter and an `EngineEvent::Error` naming it.
- Bench harness: `--bench-scroll PX_PER_S --bench-scroll-s N`, `--bench-jump PAGE`, and a
  `pages_resident` field on the `rss` line; metrics defined in `benchmarks.md` §8 before any run.
- **Acceptance (Phase 1 gate):** §6, rows A-E.

### Phase 2 - Navigation and input

- §4.6 in full, the custom scrollbar (§4.2), fit-width / fit-page buttons (§4.5).
- **Acceptance:** each action has an app-level test through `AppState` where it is pure state, and a
  scripted harness step where it needs a window; §6 row F.

### Phase 3 - Progressive page sizes (conditional)

Only if a real-world document measured with `FA_PDF_BENCH_PDF` exceeds §6's open budget: the engine
sends sizes in chunks (`EngineEvent::PageSizes { start, sizes }`), the layout fills unresolved pages
with the first page's size marked `estimated`, and a correction keeps the view still by anchoring on
`(page under the viewport top, offset inside it)`.

## 6. Acceptance and benchmarks

Metric definitions go into `benchmarks.md` §8 *before* any of these is measured (CLAUDE.md §3).
Fixtures: `large_200p.pdf`, `large_format_a0.pdf`, and synthetic documents written by
`page_sizes_bench.rs` (1000 and 20,000 pages, mixed Letter / A4 landscape / rotated / cropped).

| Row | Scenario | Pass |
| :--- | :--- | :--- |
| A | Fling-scroll `large_200p.pdf` at 1000% top to bottom (30 s, constant velocity) | `peak_ws` within 10 MiB of a 1-page scroll of the same length; tile model rows constant; `hole_frames` reported (no target yet - this sets the baseline) |
| B | Same scroll on the 1000- and 20,000-page synthetic documents at 100% and 6400% | `peak_ws` within 10 MiB of row A; `pages_resident` never above visible + margin |
| C | Crossing a page boundary during a slow scroll | requests only the newly exposed cells (`core` test on the layout + scheduler, not a timing) |
| D | Jump to page 900 of 1000, then zoom to 6400% around the viewport centre | the page under the centre stays the same page and the anchor point moves < 1 device px; Plan 0001's `t_blank` verdict ceiling (50 ms) holds |
| E | Re-anchor cost | re-anchors counted per run; a fling that crosses one is reported with its frame time; a re-anchor that stops a fling is reported, not hidden |
| F | Navigation | every §4.6 action reaches the page it names on the 1000-page document |
| G | Open budget (decides Phase 3) | page table built in <= 100 ms for the largest real-world document supplied; above it, Phase 3 is scheduled |

The Plan 0001 memory target (`peak_ws` <= 250 MiB, default configuration) stays the ceiling for every
row.

## 7. Risks

| Risk | Impact | Mitigation |
| :--- | :--- | :--- |
| A re-anchor stops a fling or a wheel animation (§3.2) | Medium, UX | Rare by construction (once per ~260 k px); measured as row E; fallback is Rust-side scrolling (§4.2) |
| Display lists of many visible pages are large | Medium, memory | Count-bounded LRU; Phase 1 reports list sizes; backlog item 1 already investigates the fixed overhead |
| Tile model capacity changes with zoom | Medium | Recomputed on zoom as well as resize; the bound (§4.3) is derived from tile size, not from the page count; overflow stays graceful (Plan 0001 §4.6) |
| Real documents open much slower than §3.5's synthetic ones | Medium, open latency | Row G decides; Phase 3 is designed and the layout already carries `estimated` |
| Current-page indicator flickers at page boundaries | Low, cosmetic | Centre-line rule with a gap rule, unit-tested |
| `Flickable` overscroll/clamping at a window edge that is not the document edge | Medium | The window always extends 2^18 px past the viewport before a re-anchor, and is clamped only at the real document edges |

## 8. Deliberate non-goals

- **No two-page spreads, no horizontal mode** - the layout keeps the hooks (§4.1).
- **No thumbnails, outline, search or selection** - v0.1 items that build on this layout.
- **No pinch zoom and no stretched second tile level** - Plan 0001 Phase 3, deferred.
- **No per-key cancel in the engine protocol** - an epoch covers every case in §4.4; revisited only
  if row A's `unwanted` says otherwise.
- **No reimplementation of MuPDF's page box rules** - §3.5 shows it buys nothing.

## 9. Questions for the approver

1. **Scrolling model.** Approve the `Flickable` with a re-anchored canvas window (§4.2), with
   Rust-side scrolling as the fallback if row E fails?
2. **Level of detail.** Approve the split into tiled pages and single page images for small pages
   (§4.3), and the engine's display-list LRU that comes with it?
3. **Layout defaults.** Single column, pages centred, 8 logical px gap, grey background as today; open
   at 100% (as today) or at fit-width?
4. **Ctrl+wheel** moves into this plan's Phase 2; pinch and the stretched level stay in the deferred
   Plan 0001 Phase 3. Agreed?
5. **Real-world documents for row G.** Can you supply one to three large real PDFs (ideally 1000+
   pages, compressed object streams), or point me at a directory I may use? The open-time numbers in
   §3.5 are synthetic best cases until then.
