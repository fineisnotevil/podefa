// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Tile geometry for viewport-only rendering (Plan 0001, §4.1).
//!
//! Everything here is pure integer/float math with no engine or UI dependency, so the same
//! functions decide *which* pixels are needed (`app`), *where* a page sits (`core`) and *what*
//! region the rasterizer must draw (`engine-mupdf`).
//!
//! Coordinate systems, from the inside out:
//!
//! - **page points** — PDF user space, origin and size taken from the page bounds.
//! - **device pixels** — page points multiplied by the scale, i.e. what the rasterizer
//!   produces. All rectangles in this module are in this space.
//! - **page raster** — device pixels relative to the page's own raster origin, so `(0, 0)` is
//!   the top-left pixel of a full-page render. Tile keys use *page-raster* cells, which is why
//!   a key never depends on where the page is placed in the canvas.

use crate::Rect;

/// Minimum zoom: 10%.
pub const ZOOM_MIN_MILLI: u32 = 100;
/// Maximum zoom: 6400%.
///
/// Tiling makes the full-page buffer cost irrelevant, so this ceiling is set by rendering
/// precision rather than by memory. The A0 acceptance test in `engine-mupdf` measures that
/// precision at 6400%.
pub const ZOOM_MAX_MILLI: u32 = 64_000;
/// Zoom step numerator: 1.25 = 5/4, exact in integer permille.
pub const ZOOM_STEP_NUM: u32 = 5;
/// Zoom step denominator; see [`ZOOM_STEP_NUM`].
pub const ZOOM_STEP_DEN: u32 = 4;

/// Tile edge in physical pixels.
pub const TILE_SIZE_PX: u32 = 512;
/// Guard band rasterized around every tile and discarded on copy-out.
///
/// Adjacent tiles are rendered independently, so a shape whose antialiasing straddles the
/// boundary would otherwise be resolved differently on each side and show a 1 px seam. Two
/// pixels is above the worst-case antialiasing bleed of MuPDF's rasterizer.
pub const TILE_BLEED_PX: i32 = 2;
/// Edge of the reusable scratch raster: [`TILE_SIZE_PX`] plus the bleed on both sides.
pub const TILE_STRIDE_PX: u32 = TILE_SIZE_PX + 2 * TILE_BLEED_PX as u32;

/// Converts a scale in permille to a scale factor.
pub fn scale_from_milli(milli: u32) -> f32 {
    milli as f32 / 1000.0
}

/// Clamps a scale in permille to the supported range.
pub fn clamp_zoom_milli(milli: u32) -> u32 {
    milli.clamp(ZOOM_MIN_MILLI, ZOOM_MAX_MILLI)
}

/// One zoom step in, rounded to the nearest permille and clamped.
pub fn zoom_in_milli(milli: u32) -> u32 {
    clamp_zoom_milli(mul_div_round(milli, ZOOM_STEP_NUM, ZOOM_STEP_DEN))
}

/// One zoom step out, rounded to the nearest permille and clamped.
pub fn zoom_out_milli(milli: u32) -> u32 {
    clamp_zoom_milli(mul_div_round(milli, ZOOM_STEP_DEN, ZOOM_STEP_NUM))
}

fn mul_div_round(value: u32, num: u32, den: u32) -> u32 {
    ((value as u64 * num as u64 + (den as u64 / 2)) / den as u64) as u32
}

/// Identity of one cached tile.
///
/// Integers only, so it hashes and compares exactly: no float-key cache misses. `page` is the
/// page index and `col`/`row` are page-raster cell indices, so the same key always denotes the
/// same page region regardless of page layout (see Plan 0002).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileKey {
    /// Page index within the document.
    pub page: u32,
    /// Cell column, counted from the page raster's left edge.
    pub col: i32,
    /// Cell row, counted from the page raster's top edge.
    pub row: i32,
    /// Scale in permille; tiles are rendered at exactly this scale.
    pub scale_milli: u32,
}

impl TileKey {
    /// Scale factor this key is rendered at.
    pub fn scale(&self) -> f32 {
        scale_from_milli(self.scale_milli)
    }
}

/// Rectangle in device pixels, page-raster relative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TileRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl TileRect {
    /// One past the right edge.
    pub fn x1(&self) -> i32 {
        self.x + self.w as i32
    }

    /// One past the bottom edge.
    pub fn y1(&self) -> i32 {
        self.y + self.h as i32
    }

    /// True when the rectangle covers no pixels.
    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// Width in bytes of one tightly packed RGBA8 row.
    pub fn row_bytes(&self) -> usize {
        self.w as usize * 4
    }

    /// Total size in bytes of a tightly packed RGBA8 buffer.
    pub fn byte_len(&self) -> usize {
        self.row_bytes() * self.h as usize
    }
}

/// Geometry of one page at one scale.
///
/// Carries the page index and the page origin from the start, so continuous multi-page scroll
/// only needs a layout offset and never a new geometry type.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageGeometry {
    /// Page index within the document.
    pub page: u32,
    /// Page bounds origin, PDF points.
    pub origin_x: f32,
    /// Page bounds origin, PDF points.
    pub origin_y: f32,
    /// Page bounds width, PDF points.
    pub width_pt: f32,
    /// Page bounds height, PDF points.
    pub height_pt: f32,
    /// Scale in permille.
    pub scale_milli: u32,
}

impl PageGeometry {
    /// Builds geometry from a page index, its bounds in points and a permille scale.
    pub fn new(page: u32, bounds: Rect, scale_milli: u32) -> Self {
        Self {
            page,
            origin_x: bounds.x,
            origin_y: bounds.y,
            width_pt: bounds.width,
            height_pt: bounds.height,
            scale_milli,
        }
    }

    /// Scale factor.
    pub fn scale(&self) -> f32 {
        scale_from_milli(self.scale_milli)
    }

    /// Key of one cell of this page.
    pub fn key(&self, col: i32, row: i32) -> TileKey {
        TileKey {
            page: self.page,
            col,
            row,
            scale_milli: self.scale_milli,
        }
    }

    /// Origin of the page raster in device pixels.
    ///
    /// MuPDF derives its full-page pixmap bbox as
    /// `fz_round_rect(fz_transform_rect(page_bounds, scale))`, and for a pure scale
    /// `fz_transform_rect` multiplies each coordinate by the scale. `fz_round_rect` then floors
    /// the low corners and ceils the high corners, each with a 0.001 nudge. Both are reproduced
    /// here so the tile grid and a full-page render agree pixel for pixel.
    pub fn raster_origin(&self) -> (i32, i32) {
        let s = self.scale();
        (
            floor_nudged(self.origin_x * s),
            floor_nudged(self.origin_y * s),
        )
    }

    /// Page raster size in device pixels (never negative).
    pub fn device_size(&self) -> (u32, u32) {
        let s = self.scale();
        let (x0, y0) = self.raster_origin();
        let x1 = ceil_nudged((self.origin_x + self.width_pt) * s);
        let y1 = ceil_nudged((self.origin_y + self.height_pt) * s);
        ((x1 - x0).max(0) as u32, (y1 - y0).max(0) as u32)
    }

    /// Number of tile columns covering the page.
    pub fn cols(&self) -> i32 {
        div_ceil(self.device_size().0, TILE_SIZE_PX) as i32
    }

    /// Number of tile rows covering the page.
    pub fn rows(&self) -> i32 {
        div_ceil(self.device_size().1, TILE_SIZE_PX) as i32
    }

    /// The visible part of a cell, clamped to the page extent; `None` when the cell is entirely
    /// outside the page. The last row/column of a page yields a smaller rectangle instead of a
    /// padded one, so padded pixels never reach the cache or the GPU.
    pub fn tile_rect(&self, col: i32, row: i32) -> Option<TileRect> {
        if col < 0 || row < 0 || col >= self.cols() || row >= self.rows() {
            return None;
        }
        let (dev_w, dev_h) = self.device_size();
        let x = col * TILE_SIZE_PX as i32;
        let y = row * TILE_SIZE_PX as i32;
        let w = (dev_w as i32 - x).min(TILE_SIZE_PX as i32);
        let h = (dev_h as i32 - y).min(TILE_SIZE_PX as i32);
        if w <= 0 || h <= 0 {
            return None;
        }
        Some(TileRect {
            x,
            y,
            w: w as u32,
            h: h as u32,
        })
    }

    /// The full padded raster rectangle of a cell: always `TILE_STRIDE_PX` on a side.
    ///
    /// This is what the engine rasterizes and what the scissor covers; [`Self::tile_rect`] is
    /// the inner part that survives the crop.
    pub fn scratch_rect(&self, col: i32, row: i32) -> TileRect {
        TileRect {
            x: col * TILE_SIZE_PX as i32 - TILE_BLEED_PX,
            y: row * TILE_SIZE_PX as i32 - TILE_BLEED_PX,
            w: TILE_STRIDE_PX,
            h: TILE_STRIDE_PX,
        }
    }

    /// Cell boundaries at or just past a device-pixel viewport rectangle, as
    /// `(col0, col1, row0, row1)` with the ranges half-open and clamped to the grid.
    ///
    /// `view` is `(x, y, width, height)` in page-raster device pixels; pass a negative `x`/`y`
    /// when the page is scrolled partly out of view.
    pub fn cell_range(&self, view: Rect) -> (i32, i32, i32, i32) {
        let cols = self.cols();
        let rows = self.rows();
        let tile = TILE_SIZE_PX as f32;
        // Both ends are clamped into the grid, so a viewport entirely past the page yields an
        // empty range instead of an inverted one.
        let c0 = ((view.x / tile).floor() as i32).clamp(0, cols);
        let r0 = ((view.y / tile).floor() as i32).clamp(0, rows);
        let c1 = (((view.x + view.width) / tile).ceil() as i32).clamp(c0, cols);
        let r1 = (((view.y + view.height) / tile).ceil() as i32).clamp(r0, rows);
        (c0, c1, r0, r1)
    }

    /// Cells intersecting `view`, row-major.
    pub fn visible_cells(&self, view: Rect) -> impl Iterator<Item = (i32, i32)> + '_ {
        let (c0, c1, r0, r1) = self.cell_range(view);
        (r0..r1).flat_map(move |row| (c0..c1).map(move |col| (col, row)))
    }
}

/// New content offset that keeps the page point under `anchor_view` stationary across a zoom.
///
/// `content` is the current content offset in device pixels and `anchor_view` is the position of
/// the anchor inside the viewport, also in device pixels.
pub fn zoom_anchor(content: f32, anchor_view: f32, old_milli: u32, new_milli: u32) -> f32 {
    let old_scale = scale_from_milli(old_milli.max(1));
    let new_scale = scale_from_milli(new_milli.max(1));
    (content + anchor_view) / old_scale * new_scale - anchor_view
}

fn floor_nudged(v: f32) -> i32 {
    (v + 0.001).floor() as i32
}

fn ceil_nudged(v: f32) -> i32 {
    (v - 0.001).ceil() as i32
}

fn div_ceil(value: u32, divisor: u32) -> u32 {
    value.div_ceil(divisor)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn letter() -> PageGeometry {
        PageGeometry::new(0, Rect::new(0.0, 0.0, 612.0, 792.0), 1_000)
    }

    fn a0() -> PageGeometry {
        PageGeometry::new(0, Rect::new(0.0, 0.0, 2384.0, 3370.0), 1_000)
    }

    #[test]
    fn device_size_follows_the_mupdf_rounding_rule() {
        // floor(x0 + 0.001) .. ceil(x1 - 0.001): 100.5 pt at 1.1 is 110.55 px -> 111 px wide.
        let geo = PageGeometry::new(0, Rect::new(0.0, 0.0, 100.5, 10.0), 1_100);
        assert_eq!(geo.device_size(), (111, 11));
        // Whole-point scales stay exact.
        assert_eq!(letter().device_size(), (612, 792));
        assert_eq!(a0().device_size(), (2384, 3370));
    }

    #[test]
    fn device_size_accounts_for_a_non_zero_page_origin() {
        // Origin 10.5 pt at 1.0: raster starts at floor(10.501) = 10 px and ends at
        // ceil(10.5 + 100 - 0.001) = 111, so the raster is 101 px wide, not 100.
        let geo = PageGeometry::new(0, Rect::new(10.5, 0.0, 100.0, 50.0), 1_000);
        assert_eq!(geo.raster_origin(), (10, 0));
        assert_eq!(geo.device_size(), (101, 50));
    }

    #[test]
    fn cols_and_rows_cover_the_page_exactly() {
        assert_eq!(letter().cols(), 2);
        assert_eq!(letter().rows(), 2);
        assert_eq!(a0().cols(), 5);
        assert_eq!(a0().rows(), 7);

        let a0_6400 = PageGeometry::new(0, Rect::new(0.0, 0.0, 2384.0, 3370.0), 64_000);
        assert_eq!(a0_6400.device_size(), (152_576, 215_680));
        assert_eq!(a0_6400.cols(), 298);
        assert_eq!(a0_6400.rows(), 422);
    }

    #[test]
    fn tile_rect_clamps_the_last_row_and_column() {
        let geo = letter();
        assert_eq!(
            geo.tile_rect(0, 0),
            Some(TileRect {
                x: 0,
                y: 0,
                w: 512,
                h: 512
            })
        );
        assert_eq!(
            geo.tile_rect(1, 1),
            Some(TileRect {
                x: 512,
                y: 512,
                w: 100,
                h: 280
            })
        );
        assert_eq!(geo.tile_rect(2, 0), None);
        assert_eq!(geo.tile_rect(0, 2), None);
        assert_eq!(geo.tile_rect(-1, 0), None);
    }

    #[test]
    fn scratch_rect_is_always_the_padded_stride() {
        let geo = letter();
        let scratch = geo.scratch_rect(1, 1);
        assert_eq!(scratch.x, 512 - TILE_BLEED_PX);
        assert_eq!(scratch.y, 512 - TILE_BLEED_PX);
        assert_eq!(scratch.w, TILE_STRIDE_PX);
        assert_eq!(scratch.h, TILE_STRIDE_PX);
        assert_eq!(scratch.w, 516);
    }

    #[test]
    fn visible_cells_handles_boundaries_and_negative_origins() {
        let geo = letter();
        // A 600x700 viewport at the origin touches columns 0-1 and rows 0-1.
        assert_eq!(
            geo.visible_cells(Rect::new(0.0, 0.0, 600.0, 700.0))
                .collect::<Vec<_>>(),
            vec![(0, 0), (1, 0), (0, 1), (1, 1)]
        );
        // Scrolled past the page edge: everything clamps to the grid.
        assert_eq!(
            geo.visible_cells(Rect::new(600.0, 780.0, 100.0, 100.0))
                .collect::<Vec<_>>(),
            vec![(1, 1)]
        );
        // Scrolled up-left of the page: the visible part clamps to cell 0.
        assert_eq!(
            geo.visible_cells(Rect::new(-100.0, -100.0, 200.0, 200.0))
                .collect::<Vec<_>>(),
            vec![(0, 0)]
        );
        // Entirely before the page start: nothing is visible.
        assert!(
            geo.visible_cells(Rect::new(-200.0, -200.0, 50.0, 50.0))
                .next()
                .is_none()
        );
        // Entirely outside the page.
        assert!(
            geo.visible_cells(Rect::new(5000.0, 5000.0, 10.0, 10.0))
                .next()
                .is_none()
        );
    }

    #[test]
    fn zoom_anchor_is_a_fixed_point() {
        // Unchanged scale leaves the offset alone.
        assert_eq!(zoom_anchor(1234.5, 100.0, 1_000, 1_000), 1234.5);

        // Zooming in around an anchor at 100 px: the page point that was at content 0 + anchor
        // 100 is at device 200 afterwards, so content must move to 200 - 100.
        let content = zoom_anchor(0.0, 100.0, 1_000, 2_000);
        assert_eq!(content, 100.0);
        // The invariant, stated directly: the page point under the anchor does not move.
        let page_point = (0.0 + 100.0) / 1.0;
        assert_eq!((content + 100.0) / 2.0, page_point);

        // Zooming out around the viewport origin keeps the content origin.
        assert_eq!(zoom_anchor(-300.0, 0.0, 2_000, 1_000), -150.0);
    }

    #[test]
    fn permille_zoom_steps_round_trip() {
        let mut milli = 1_000;
        for expected in [1_250, 1_563, 1_954, 2_443] {
            milli = zoom_in_milli(milli);
            assert_eq!(milli, expected);
        }
        for expected in [1_954, 1_563, 1_250, 1_000] {
            milli = zoom_out_milli(milli);
            assert_eq!(milli, expected);
        }
    }

    #[test]
    fn permille_zoom_steps_clamp_at_both_ends() {
        assert_eq!(zoom_in_milli(ZOOM_MAX_MILLI), ZOOM_MAX_MILLI);
        assert_eq!(zoom_out_milli(ZOOM_MIN_MILLI), ZOOM_MIN_MILLI);
        assert_eq!(clamp_zoom_milli(0), ZOOM_MIN_MILLI);
        assert_eq!(scale_from_milli(ZOOM_MAX_MILLI), 64.0);
    }
}
