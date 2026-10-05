<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# Plan 0002: Continuous Multi-Page Scroll

## Status

**Recorded follow-up — not started.** Registered at the Plan 0001 Phase 0 gate so that the
Plan 0001 tile key and geometry types do not have to be reshaped later. Phase 1 of
[`0001-tiled-viewport-rendering.md`](./0001-tiled-viewport-rendering.md) is a hard
prerequisite: nothing here starts before Plan 0001 reaches its Phase 3 gate.

## Goal

Scroll continuously through the document instead of showing one page at a time, with the same
viewport-only tiling cost as Plan 0001.

## What Plan 0001 already guarantees (no rework needed)

- `TileKey` carries `page`, so tiles from different pages never collide in the cache.
- `PageGeometry` carries the page **origin** (`Rect.x/y` from `page.bounds()`), and tile cells
  are aligned to the page raster, so `(page, col, row, scale_milli)` denotes the same page
  region wherever the page is laid out.
- Cache keys are integer permille, so zooming in continuous mode reuses a page's tiles at the
  same scale and evicts other scales by the existing old-scale-first rule.
- The pure `TileScheduler` in `core` is unaware of layout, so "a viewport has a set of desired
  keys" already covers several pages worth of keys.

## What this plan adds

1. A per-page layout table: `offset_y` (and `offset_x` for a spread) in device px, plus a page
   gap constant, recomputed when the scale or the page sizes change.
2. `visible_cells` unioned across the pages that intersect the viewport, with each page's
   offset folded in when converting a tile's raster rect to a viewport position.
3. App-side placement of tile images relative to their page's offset (the model already stores
   absolute `x`/`y` in viewport terms, so only the value written changes).
4. Prefetch policy across a page boundary, and page-transition handling in the epoch logic when
   the visible page set changes.

Explicitly unchanged: the engine (`render_tile`), the cache key type, the eviction policy, and
the actor protocol.

## Acceptance (to be detailed when this plan is scheduled)

- A 200-page document scrolls from page 1 to page 200 at 1000% without the tile count or the
  resident memory growing with the number of pages visited.
- Scrolling across a page boundary requests only the newly exposed tiles.
- Zooming in continuous mode still satisfies the Plan 0001 memory and latency budget.
