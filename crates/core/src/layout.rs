// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Continuous single-column page layout and the canvas window (Plan 0002, §4.1-4.2).
//!
//! Pure integer/float math, like [`crate::tiling`]. Two coordinate systems are added on top of
//! the page raster:
//!
//! - **document pixels** - device pixels of the whole document laid out top to bottom, origin at
//!   the top-left of the layout (including its outer margin). Page offsets are exact `u64` prefix
//!   sums of integer raster sizes, so a page's position is exact at any page count and zoom; the
//!   scroll position is an `f64`, exact for integers up to 2^53.
//! - **canvas pixels** - document pixels minus the start of a [`CanvasWindow`]. Slint's lengths
//!   are `f32` (`i-slint-core` `Coord`), which resolves 4 px at 50 M px, so only canvas
//!   coordinates - bounded by the window - ever reach the UI.

use crate::tiling::{PageGeometry, TILE_SIZE_PX, TileKey};
use crate::{PageSize, Rect};
use std::ops::Range;

/// Space between two pages, and around the column, in logical pixels.
///
/// Converted to device pixels with the scale factor, so it does not grow with the zoom.
pub const PAGE_GAP_LOGICAL_PX: f32 = 8.0;

/// Half the span of the canvas window either side of the viewport, in logical pixels.
///
/// 2^20: every canvas coordinate stays under 2^21 + the viewport, where `f32` resolves 0.25 px.
pub const CANVAS_HALF_LOGICAL_PX: f64 = 1_048_576.0;

/// How close to a window edge (that is not the document edge) the viewport may come before the
/// window is re-anchored, in logical pixels: 2^18, so a re-anchor happens at most once per
/// ~786 k logical px of travel - far beyond what one fling covers (Plan 0002, §3.2).
pub const REANCHOR_MARGIN_LOGICAL_PX: f64 = 262_144.0;

/// One page placed in the document.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageSlot {
    /// The page's Plan 0001 geometry at the layout's scale.
    pub geometry: PageGeometry,
    /// Left edge of the page raster in document pixels.
    pub x: u64,
    /// Top edge of the page raster in document pixels.
    pub y: u64,
    /// Raster width in device pixels.
    pub w: u32,
    /// Raster height in device pixels.
    pub h: u32,
}

impl PageSlot {
    /// Whether the page is drawn as tiles over a base layer rather than as one page image: a page
    /// that fits in one tile is its own image (Plan 0002, §4.3).
    pub fn is_tiled(&self) -> bool {
        self.w > TILE_SIZE_PX || self.h > TILE_SIZE_PX
    }

    /// One past the bottom edge, in document pixels.
    pub fn bottom(&self) -> u64 {
        self.y + self.h as u64
    }
}

/// Every page of a document in one centred column at one scale.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DocLayout {
    scale_milli: u32,
    gap_px: u32,
    slots: Vec<PageSlot>,
    width: u64,
    height: u64,
}

impl DocLayout {
    /// Lays out `pages` (their displayed bounds, rotation already applied by the engine) at a
    /// permille scale, with `gap_px` device pixels between pages and around the column.
    pub fn new(pages: &[PageSize], scale_milli: u32, gap_px: u32) -> Self {
        let geometries: Vec<PageGeometry> = pages
            .iter()
            .enumerate()
            .map(|(i, size)| PageGeometry::new(i as u32, size.bounds(), scale_milli))
            .collect();
        let sizes: Vec<(u32, u32)> = geometries.iter().map(PageGeometry::device_size).collect();
        let column = sizes.iter().map(|&(w, _)| w).max().unwrap_or(0) as u64;
        let gap = gap_px as u64;

        let mut slots = Vec::with_capacity(geometries.len());
        let mut y = gap;
        for (geometry, (w, h)) in geometries.into_iter().zip(sizes) {
            slots.push(PageSlot {
                geometry,
                x: gap + (column - w as u64) / 2,
                y,
                w,
                h,
            });
            y += h as u64 + gap;
        }
        Self {
            scale_milli,
            gap_px,
            slots,
            width: column + 2 * gap,
            height: if pages.is_empty() { 0 } else { y },
        }
    }

    /// Scale of every page, in permille.
    pub fn scale_milli(&self) -> u32 {
        self.scale_milli
    }

    /// Gap between pages, device pixels.
    pub fn gap_px(&self) -> u32 {
        self.gap_px
    }

    /// Document width: the widest page plus the margin on both sides.
    pub fn width(&self) -> u64 {
        self.width
    }

    /// Document height: every page, every gap and the margin above and below.
    pub fn height(&self) -> u64 {
        self.height
    }

    /// Number of pages.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether the document has no pages.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// One page's placement.
    pub fn slot(&self, page: usize) -> Option<&PageSlot> {
        self.slots.get(page)
    }

    /// Pages whose raster intersects the half-open band `[y0, y1)` of document pixels, by binary
    /// search on the prefix sums.
    pub fn pages_in(&self, y0: f64, y1: f64) -> Range<usize> {
        let start = self
            .slots
            .partition_point(|slot| (slot.bottom() as f64) <= y0);
        let end = self.slots.partition_point(|slot| (slot.y as f64) < y1);
        start..end.max(start)
    }

    /// The page at a document `y`, or the nearest one when `y` falls in a gap or the margin.
    pub fn page_near(&self, y: f64) -> Option<usize> {
        if self.slots.is_empty() {
            return None;
        }
        // First page whose bottom is below `y`: `y` is inside it, or in the gap just above it.
        let below = self
            .slots
            .partition_point(|slot| (slot.bottom() as f64) <= y);
        if below >= self.slots.len() {
            return Some(self.slots.len() - 1);
        }
        if below == 0 {
            return Some(0);
        }
        let above = &self.slots[below - 1];
        let next = &self.slots[below];
        if y >= next.y as f64 {
            return Some(below);
        }
        // In the gap between two pages: whichever edge is closer.
        let to_above = y - above.bottom() as f64;
        let to_next = next.y as f64 - y;
        Some(if to_next <= to_above {
            below
        } else {
            below - 1
        })
    }

    /// Tile keys of every cell a viewport shows, page after page, row-major within a page.
    ///
    /// `(x, y, w, h)` is the viewport in document pixels.
    pub fn visible_cells(&self, x: f64, y: f64, w: f64, h: f64) -> Vec<TileKey> {
        let mut keys = Vec::new();
        if w <= 0.0 || h <= 0.0 {
            return keys;
        }
        for page in self.pages_in(y, y + h) {
            let slot = &self.slots[page];
            if x + w <= slot.x as f64 || x >= (slot.x + slot.w as u64) as f64 {
                continue;
            }
            // Page-raster coordinates are bounded by the page and the viewport, so `f32` is exact
            // enough here (Plan 0002, §3.1): only the document offset is large.
            let local = Rect::new(
                (x - slot.x as f64) as f32,
                (y - slot.y as f64) as f32,
                w as f32,
                h as f32,
            );
            let geo = slot.geometry;
            keys.extend(geo.visible_cells(local).map(|(c, r)| geo.key(c, r)));
        }
        keys
    }

    /// Where a tile goes, in document pixels: `(x, y, w, h)`, or `None` for a key that is not a
    /// cell of this layout (another scale, a page out of range, a cell outside the page).
    pub fn tile_place(&self, key: &TileKey) -> Option<(f64, f64, u32, u32)> {
        let slot = self.slots.get(key.page as usize)?;
        if key.scale_milli != self.scale_milli {
            return None;
        }
        let rect = slot.geometry.tile_rect(key.col, key.row)?;
        Some((
            slot.x as f64 + rect.x as f64,
            slot.y as f64 + rect.y as f64,
            rect.w,
            rect.h,
        ))
    }

    /// The scroll position after a zoom from this layout to `to`, keeping the page point under
    /// `anchor` (viewport-relative, device pixels) still.
    ///
    /// The anchor is page-relative: the gap is constant in device pixels, so scaling the whole
    /// document offset would drift by `gap x pages_above x (new/old - 1)` (Plan 0002, §4.5). A
    /// point in a gap or the margin is clamped onto the nearest page edge.
    pub fn zoom_anchor(&self, to: &DocLayout, pos: (f64, f64), anchor: (f64, f64)) -> (f64, f64) {
        let ay = pos.1 + anchor.1;
        let ax = pos.0 + anchor.0;
        let (Some(page), false) = (self.page_near(ay), to.is_empty()) else {
            return pos;
        };
        let (Some(old), Some(new)) = (self.slot(page), to.slot(page)) else {
            return pos;
        };
        let ratio = to.scale_milli.max(1) as f64 / self.scale_milli.max(1) as f64;
        let dy = (ay - old.y as f64).clamp(0.0, old.h as f64);
        let dx = (ax - old.x as f64).clamp(0.0, old.w as f64);
        (
            new.x as f64 + dx * ratio - anchor.0,
            new.y as f64 + dy * ratio - anchor.1,
        )
    }
}

/// The band of the document Slint is given along one axis (Plan 0002, §4.2).
///
/// Everything Slint sees is relative to `start`, and the band is at most a viewport plus twice
/// `half` long, so its `f32` lengths stay exact to a fraction of a pixel. `start` is an integer, so
/// two document edges that meet are still equal after the subtraction.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CanvasWindow {
    /// First document pixel of the band.
    pub start: f64,
    /// Length of the band in device pixels.
    pub len: f64,
}

impl CanvasWindow {
    /// The band around a viewport at `pos` of length `view`, clamped to a document of length
    /// `doc`, with `half` device pixels either side.
    pub fn around(pos: f64, view: f64, doc: f64, half: f64) -> Self {
        let start = (pos - half).max(0.0).floor();
        let end = (pos + view + half).min(doc).ceil().max(start);
        Self {
            start,
            len: end - start,
        }
    }

    /// One past the last document pixel of the band.
    pub fn end(&self) -> f64 {
        self.start + self.len
    }

    /// Whether a viewport at `pos` of length `view` is within `margin` of a band edge that is not
    /// also the document's edge, i.e. whether the band has to move before the user reaches it.
    pub fn needs_reanchor(&self, pos: f64, view: f64, doc: f64, margin: f64) -> bool {
        let near_top = self.start > 0.0 && pos - self.start < margin;
        let near_bottom = self.end() < doc && self.end() - (pos + view) < margin;
        near_top || near_bottom
    }

    /// A document coordinate in band (canvas) coordinates.
    pub fn local(&self, doc: f64) -> f64 {
        doc - self.start
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiling::{ZOOM_MAX_MILLI, ZOOM_MIN_MILLI, zoom_in_milli, zoom_out_milli};

    fn letter() -> PageSize {
        PageSize {
            x: 0.0,
            y: 0.0,
            width: 612.0,
            height: 792.0,
        }
    }

    fn landscape() -> PageSize {
        PageSize {
            x: 0.0,
            y: 0.0,
            width: 842.0,
            height: 595.0,
        }
    }

    #[test]
    fn pages_stack_with_exact_integer_offsets() {
        let layout = DocLayout::new(&[letter(), letter(), letter()], 1_000, 8);
        assert_eq!(layout.len(), 3);
        assert_eq!(layout.slot(0).map(|s| s.y), Some(8));
        assert_eq!(layout.slot(1).map(|s| s.y), Some(8 + 792 + 8));
        assert_eq!(layout.slot(2).map(|s| s.y), Some(8 + 2 * (792 + 8)));
        assert_eq!(layout.height(), 8 + 3 * (792 + 8));
        assert_eq!(layout.width(), 612 + 16);
    }

    #[test]
    fn narrower_pages_are_centred_on_the_widest() {
        let layout = DocLayout::new(&[letter(), landscape()], 1_000, 8);
        assert_eq!(layout.width(), 842 + 16);
        assert_eq!(layout.slot(1).map(|s| s.x), Some(8));
        assert_eq!(layout.slot(0).map(|s| s.x), Some(8 + (842 - 612) / 2));
    }

    #[test]
    fn a_non_zero_origin_uses_the_mupdf_raster_size() {
        // 10.5 pt origin at 100%: the raster is 101 px wide, not 100 (tiling's rounding rule).
        let odd = PageSize {
            x: 10.5,
            y: 0.0,
            width: 100.0,
            height: 50.0,
        };
        let layout = DocLayout::new(&[odd], 1_000, 0);
        assert_eq!(layout.slot(0).map(|s| (s.w, s.h)), Some((101, 50)));
    }

    #[test]
    fn an_empty_document_has_no_extent() {
        let layout = DocLayout::new(&[], 1_000, 8);
        assert!(layout.is_empty());
        assert_eq!(layout.height(), 0);
        assert!(layout.pages_in(0.0, 1e9).is_empty());
        assert_eq!(layout.page_near(100.0), None);
        assert!(layout.visible_cells(0.0, 0.0, 900.0, 700.0).is_empty());
    }

    #[test]
    fn pages_in_finds_the_intersecting_band() {
        let layout = DocLayout::new(&[letter(); 10], 1_000, 8);
        // Page 1 spans [808, 1600).
        assert!(
            layout.pages_in(0.0, 8.0).is_empty(),
            "the top margin shows no page"
        );
        assert_eq!(layout.pages_in(0.0, 9.0), 0..1);
        assert!(
            layout.pages_in(800.0, 808.0).is_empty(),
            "a gap shows no page"
        );
        assert_eq!(layout.pages_in(799.0, 809.0), 0..2);
        assert_eq!(layout.pages_in(900.0, 1000.0), 1..2);
        assert_eq!(layout.pages_in(0.0, 1e12), 0..10);
    }

    #[test]
    fn page_near_resolves_gaps_to_the_closer_page() {
        let layout = DocLayout::new(&[letter(); 3], 1_000, 8);
        assert_eq!(layout.page_near(0.0), Some(0));
        assert_eq!(layout.page_near(500.0), Some(0));
        assert_eq!(layout.page_near(801.0), Some(0), "gap, closer to page 0");
        assert_eq!(layout.page_near(807.0), Some(1), "gap, closer to page 1");
        assert_eq!(layout.page_near(1000.0), Some(1));
        assert_eq!(layout.page_near(1e9), Some(2));
    }

    #[test]
    fn visible_cells_span_a_page_boundary() {
        let layout = DocLayout::new(&[letter(); 3], 1_000, 8);
        // A 600 px band across the page 0 / page 1 boundary sees the last row of page 0 and the
        // first row of page 1; Letter at 100% is 2 columns x 2 rows of 512 px.
        let keys = layout.visible_cells(0.0, 700.0, 628.0, 300.0);
        let pages: Vec<u32> = keys.iter().map(|k| k.page).collect();
        assert!(pages.contains(&0) && pages.contains(&1));
        assert!(keys.contains(&TileKey {
            page: 0,
            col: 0,
            row: 1,
            scale_milli: 1_000
        }));
        assert!(keys.contains(&TileKey {
            page: 1,
            col: 1,
            row: 0,
            scale_milli: 1_000
        }));
        assert!(keys.iter().all(|k| k.page != 2));
    }

    #[test]
    fn crossing_a_boundary_only_adds_the_newly_exposed_cells() {
        // Plan 0002 §6 row C: a slow scroll across a boundary requests the new cells only.
        let layout = DocLayout::new(&[letter(); 3], 1_000, 8);
        let before = layout.visible_cells(0.0, 500.0, 628.0, 300.0);
        let after = layout.visible_cells(0.0, 520.0, 628.0, 300.0);
        let mut sched = crate::TileScheduler::new(64, usize::MAX);
        sched.update_view(&before, 1_000);
        for key in &before {
            sched.insert(*key, 1);
        }
        let new: Vec<TileKey> = after
            .iter()
            .filter(|k| !before.contains(k))
            .copied()
            .collect();
        let requested: Vec<TileKey> = sched
            .update_view(&after, 1_000)
            .into_iter()
            .filter_map(|action| match action {
                crate::TileAction::Request { key } => Some(key),
                _ => None,
            })
            .collect();
        assert_eq!(requested, new);
        assert!(
            new.iter().all(|k| k.page == 1),
            "only page 1 is newly exposed"
        );
    }

    #[test]
    fn tile_place_adds_the_page_offset() {
        let layout = DocLayout::new(&[letter(); 2], 1_000, 8);
        let key = TileKey {
            page: 1,
            col: 1,
            row: 1,
            scale_milli: 1_000,
        };
        // Page 1 at (8, 808); cell (1, 1) at (512, 512), clipped to 100 x 280.
        assert_eq!(layout.tile_place(&key), Some((520.0, 1320.0, 100, 280)));
        let other_scale = TileKey {
            scale_milli: 2_000,
            ..key
        };
        assert_eq!(layout.tile_place(&other_scale), None);
    }

    #[test]
    fn small_pages_are_one_image_and_large_ones_are_tiled() {
        let small = DocLayout::new(&[letter()], 500, 0);
        assert!(!small.slot(0).is_some_and(PageSlot::is_tiled));
        let large = DocLayout::new(&[letter()], 1_000, 0);
        assert!(large.slot(0).is_some_and(PageSlot::is_tiled));
    }

    #[test]
    fn rotated_and_mixed_pages_keep_their_own_sizes() {
        // The engine reports displayed bounds, so a /Rotate 90 Letter page is 792 x 612.
        let rotated = PageSize {
            x: 0.0,
            y: 0.0,
            width: 792.0,
            height: 612.0,
        };
        let layout = DocLayout::new(&[letter(), rotated, landscape()], 2_000, 8);
        assert_eq!(layout.slot(1).map(|s| (s.w, s.h)), Some((1584, 1224)));
        assert_eq!(layout.width(), 842 * 2 + 16);
        assert_eq!(layout.slot(2).map(|s| s.y), Some(8 + 1584 + 8 + 1224 + 8));
    }

    #[test]
    fn twenty_thousand_pages_are_exact_at_the_top_zoom() {
        let layout = DocLayout::new(&vec![letter(); 20_000], ZOOM_MAX_MILLI, 8);
        let page_h = 792 * 64;
        let last = layout.slot(19_999).map(|s| s.y);
        assert_eq!(last, Some(8 + 19_999 * (page_h + 8)));
        // About 1.01e9 px tall, 60x past f32's exact range (2^24), and still exact here.
        assert!(layout.height() > 60 << 24);
        let y = 8 + 12_345 * (page_h + 8) + 1;
        assert_eq!(layout.page_near(y as f64), Some(12_345));
        assert_eq!(layout.pages_in(y as f64, y as f64 + 700.0), 12_345..12_346);
    }

    /// The page point under the anchor, in page points, for the anchor test below.
    fn point_under(layout: &DocLayout, pos: (f64, f64), anchor: (f64, f64)) -> (usize, f64, f64) {
        let ay = pos.1 + anchor.1;
        let ax = pos.0 + anchor.0;
        let page = layout.page_near(ay).unwrap_or(0);
        let slot = layout.slot(page).copied().unwrap_or_else(|| unreachable!());
        let s = layout.scale_milli() as f64 / 1000.0;
        (page, (ax - slot.x as f64) / s, (ay - slot.y as f64) / s)
    }

    #[test]
    fn a_zoom_keeps_the_anchored_page_point_still_across_the_range() {
        // Plan 0002 §4.5 / §6 row D: an anchor on page 1 and on page 900 of 1000 stays within one
        // device pixel through every step from 10% to 6400% and back.
        let pages = vec![letter(); 1000];
        let anchor = (450.0, 350.0);
        for start_page in [1usize, 900] {
            let mut milli = 1_000;
            let mut layout = DocLayout::new(&pages, milli, 8);
            let slot = layout
                .slot(start_page)
                .copied()
                .unwrap_or_else(|| unreachable!());
            let mut pos = (
                slot.x as f64 + 100.0 - anchor.0,
                slot.y as f64 + 300.0 - anchor.1,
            );
            let (page0, px0, py0) = point_under(&layout, pos, anchor);
            assert_eq!(page0, start_page);

            let mut steps = Vec::new();
            while milli > ZOOM_MIN_MILLI {
                milli = zoom_out_milli(milli);
                steps.push(milli);
            }
            while milli < ZOOM_MAX_MILLI {
                milli = zoom_in_milli(milli);
                steps.push(milli);
            }
            for next in steps {
                let to = DocLayout::new(&pages, next, 8);
                pos = layout.zoom_anchor(&to, pos, anchor);
                layout = to;
                let (page, px, py) = point_under(&layout, pos, anchor);
                let s = next as f64 / 1000.0;
                assert_eq!(page, start_page, "anchor left its page at {next}");
                assert!((px - px0).abs() * s <= 1.0, "x drifted at {next}");
                assert!((py - py0).abs() * s <= 1.0, "y drifted at {next}");
            }
        }
    }

    #[test]
    fn layout_of_twenty_thousand_pages_is_cheap() {
        // Plan 0002 §4.1: a zoom rebuilds the table; this bounds that cost loosely so a regression
        // to something quadratic fails, without asserting a machine-specific number.
        let pages = vec![letter(); 20_000];
        let start = std::time::Instant::now();
        let layout = DocLayout::new(&pages, 1_250, 8);
        assert_eq!(layout.len(), 20_000);
        assert!(start.elapsed() < std::time::Duration::from_millis(500));
    }

    #[test]
    fn the_window_follows_the_document_edges() {
        let doc = 50_000_000.0;
        let half = CANVAS_HALF_LOGICAL_PX;
        let top = CanvasWindow::around(0.0, 700.0, doc, half);
        assert_eq!(top.start, 0.0);
        assert_eq!(top.len, 700.0 + half);
        let bottom = CanvasWindow::around(doc - 700.0, 700.0, doc, half);
        assert_eq!(bottom.end(), doc);
        // A small document fits in one window and never re-anchors.
        let small = CanvasWindow::around(100.0, 700.0, 5_000.0, half);
        assert_eq!((small.start, small.len), (0.0, 5_000.0));
        assert!(!small.needs_reanchor(4_300.0, 700.0, 5_000.0, REANCHOR_MARGIN_LOGICAL_PX));
    }

    #[test]
    fn the_window_reanchors_only_near_an_inner_edge() {
        let doc = 50_000_000.0;
        let half = CANVAS_HALF_LOGICAL_PX;
        let margin = REANCHOR_MARGIN_LOGICAL_PX;
        let win = CanvasWindow::around(10_000_000.0, 700.0, doc, half);
        assert!(!win.needs_reanchor(10_000_000.0, 700.0, doc, margin));
        let near_bottom = win.end() - 700.0 - margin + 1.0;
        assert!(win.needs_reanchor(near_bottom, 700.0, doc, margin));
        let near_top = win.start + margin - 1.0;
        assert!(win.needs_reanchor(near_top, 700.0, doc, margin));
        // Canvas coordinates stay small and exact in f32 for both edges of a page.
        let edge = 10_000_000.0 + 12_345.0;
        let local = win.local(edge) as f32;
        assert_eq!(local as f64, win.local(edge));
    }
}
