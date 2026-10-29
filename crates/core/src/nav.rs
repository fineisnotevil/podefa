// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Navigation math for the continuous layout (Plan 0002, §4.5-4.6).
//!
//! Pure functions over [`DocLayout`] and document pixels, so every keyboard action, the fit zooms,
//! wheel zoom and the scrollbar can be tested without a window. The app maps its input events onto
//! [`NavAction`] and writes whatever position these functions return.

use crate::PageSize;
use crate::layout::DocLayout;
use crate::tiling::{
    ZOOM_MAX_MILLI, ZOOM_MIN_MILLI, ZOOM_STEP_DEN, ZOOM_STEP_NUM, clamp_zoom_milli,
};

/// One arrow-key step, in logical pixels.
pub const LINE_STEP_LOGICAL_PX: f64 = 40.0;

/// Share of the viewport a PgUp/PgDn keeps on screen, so the reader does not lose their line.
pub const PAGE_STEP_OVERLAP: f64 = 0.1;

/// Wheel delta Slint reports for one notch of a line-based wheel, in logical pixels
/// (`i-slint-backend-winit-1.18.1/winitwindowadapter.rs:1472`: `LineDelta` x 60).
pub const WHEEL_NOTCH_LOGICAL_PX: f64 = 60.0;

/// Shortest scrollbar thumb, in logical pixels, so a 20,000-page document still has one to grab.
pub const SCROLLBAR_MIN_THUMB_LOGICAL_PX: f64 = 24.0;

/// A navigation request, independent of the key or button that raised it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavAction {
    /// Arrow up.
    LineUp,
    /// Arrow down.
    LineDown,
    /// PgUp, Shift+Space.
    PageUp,
    /// PgDn, Space.
    PageDown,
    /// Home: the top of the document.
    Home,
    /// End: the bottom of the document.
    End,
}

/// The vertical scroll position an action leads to, clamped to the document.
///
/// `pos_y` and the result are the viewport top in document pixels; `view_h` and `line_px` are
/// device pixels.
pub fn scroll_target(action: NavAction, pos_y: f64, view_h: f64, doc_h: f64, line_px: f64) -> f64 {
    let page = (view_h * (1.0 - PAGE_STEP_OVERLAP)).max(1.0);
    let target = match action {
        NavAction::LineUp => pos_y - line_px,
        NavAction::LineDown => pos_y + line_px,
        NavAction::PageUp => pos_y - page,
        NavAction::PageDown => pos_y + page,
        NavAction::Home => 0.0,
        NavAction::End => doc_h,
    };
    target.clamp(0.0, (doc_h - view_h).max(0.0))
}

/// The scroll position that puts a page's top edge, with the gap above it, at the viewport top,
/// below the layout's top margin: the first page's is 0.
pub fn page_top(layout: &DocLayout, page: usize) -> Option<f64> {
    let slot = layout.slot(page)?;
    Some(slot.y as f64 - layout.gap_px() as f64 - layout.margins().top as f64)
}

/// Largest permille scale at which a page's raster plus the gap on both sides fits in `avail`
/// device pixels along one side.
fn fit_milli(side_pt: f32, avail: f64, gap_px: u32) -> u32 {
    if !side_pt.is_finite() || side_pt <= 0.0 {
        return ZOOM_MIN_MILLI;
    }
    let room = (avail - 2.0 * gap_px as f64).max(1.0);
    // Floor, and one permille less: `PageGeometry::device_size` rounds the raster *up*, so the
    // exact quotient can come out a pixel wider than the room.
    let milli = (room * 1000.0 / side_pt as f64).floor() as i64 - 1;
    clamp_zoom_milli(milli.clamp(0, u32::MAX as i64) as u32)
}

/// Fit-width: the page's width fills the viewport width, gap included on both sides.
pub fn fit_width_milli(page: &PageSize, view_w: f64, gap_px: u32) -> u32 {
    fit_milli(page.width, view_w, gap_px)
}

/// Fit-page: the whole page fits the viewport, gap included on every side.
pub fn fit_page_milli(page: &PageSize, view_w: f64, view_h: f64, gap_px: u32) -> u32 {
    fit_milli(page.width, view_w, gap_px).min(fit_milli(page.height, view_h, gap_px))
}

/// Zoom after a ctrl+wheel delta: one notch is one zoom step (x1.25), and a touchpad's fractional
/// deltas zoom by the matching fraction of a step, so a pinch reported as ctrl+wheel is smooth.
/// Positive `delta_logical` (wheel up) zooms in. Quantised to permille and clamped.
pub fn wheel_zoom_milli(milli: u32, delta_logical: f64) -> u32 {
    if !delta_logical.is_finite() || delta_logical == 0.0 {
        return milli;
    }
    let step = ZOOM_STEP_NUM as f64 / ZOOM_STEP_DEN as f64;
    let factor = step.powf(delta_logical / WHEEL_NOTCH_LOGICAL_PX);
    let next = (milli.max(1) as f64 * factor).round();
    clamp_zoom_milli(next.clamp(ZOOM_MIN_MILLI as f64, ZOOM_MAX_MILLI as f64) as u32)
}

/// A vertical scrollbar thumb for the whole document: `(offset, length)` inside a track of
/// `track` pixels. All lengths in the same unit as `track`; `pos`, `view` and `doc` in document
/// pixels.
///
/// The thumb's share of the track is the viewport's share of the document, never shorter than
/// `min_thumb`, so its offset maps the scroll range onto the track minus the thumb.
pub fn scrollbar_thumb(pos: f64, view: f64, doc: f64, track: f64, min_thumb: f64) -> (f64, f64) {
    if doc <= view || track <= 0.0 {
        return (0.0, track.max(0.0));
    }
    let len = (track * view / doc).clamp(min_thumb.min(track), track);
    let range = doc - view;
    let offset = (pos / range).clamp(0.0, 1.0) * (track - len);
    (offset, len)
}

/// The scroll position for a thumb dragged to `offset`: the inverse of [`scrollbar_thumb`].
pub fn scrollbar_pos(offset: f64, view: f64, doc: f64, track: f64, min_thumb: f64) -> f64 {
    if doc <= view || track <= 0.0 {
        return 0.0;
    }
    let (_, len) = scrollbar_thumb(0.0, view, doc, track, min_thumb);
    let free = (track - len).max(f64::MIN_POSITIVE);
    (offset / free).clamp(0.0, 1.0) * (doc - view)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiling::zoom_in_milli;

    fn letter() -> PageSize {
        PageSize {
            x: 0.0,
            y: 0.0,
            width: 612.0,
            height: 792.0,
        }
    }

    #[test]
    fn keys_step_and_clamp_to_the_document() {
        let (view, doc, line) = (700.0, 10_000.0, 40.0);
        assert_eq!(
            scroll_target(NavAction::LineDown, 0.0, view, doc, line),
            40.0
        );
        assert_eq!(scroll_target(NavAction::LineUp, 10.0, view, doc, line), 0.0);
        assert_eq!(
            scroll_target(NavAction::PageDown, 100.0, view, doc, line),
            730.0
        );
        assert_eq!(
            scroll_target(NavAction::PageUp, 730.0, view, doc, line),
            100.0
        );
        assert_eq!(
            scroll_target(NavAction::Home, 5_000.0, view, doc, line),
            0.0
        );
        assert_eq!(scroll_target(NavAction::End, 0.0, view, doc, line), 9_300.0);
        assert_eq!(
            scroll_target(NavAction::PageDown, 9_000.0, view, doc, line),
            9_300.0
        );
        // A document shorter than the viewport never scrolls.
        assert_eq!(scroll_target(NavAction::End, 0.0, view, 500.0, line), 0.0);
    }

    #[test]
    fn page_down_through_a_thousand_pages_visits_every_page() {
        // Plan 0002 §6 row F: PgDn never skips a page, because it keeps 10% overlap and a Letter
        // page at 100% is taller than one step.
        let layout = DocLayout::new(&[letter(); 1000], 1_000, 8);
        let view = 700.0;
        let doc = layout.height() as f64;
        let mut pos = 0.0;
        let mut seen = vec![false; 1000];
        loop {
            for page in layout.pages_in(pos, pos + view) {
                seen[page] = true;
            }
            let next = scroll_target(NavAction::PageDown, pos, view, doc, 40.0);
            if next == pos {
                break;
            }
            pos = next;
        }
        assert!(seen.iter().all(|&s| s), "a page was skipped");
        assert_eq!(layout.page_near(pos + view / 2.0), Some(999));
    }

    #[test]
    fn page_top_reaches_the_named_page() {
        let layout = DocLayout::new(&[letter(); 1000], 1_000, 8);
        for page in [0, 1, 499, 900, 999] {
            let top = page_top(&layout, page).unwrap_or(-1.0);
            assert_eq!(layout.page_near(top + 100.0), Some(page));
        }
        assert_eq!(page_top(&layout, 1000), None);
    }

    #[test]
    fn fit_width_fills_the_viewport_without_overflowing() {
        let gap = 8;
        for view_w in [600.0, 884.0, 1920.0, 3840.0] {
            let milli = fit_width_milli(&letter(), view_w, gap);
            let layout = DocLayout::new(&[letter()], milli, gap);
            let width = layout.width() as f64;
            assert!(width <= view_w, "{view_w}: {width} overflows");
            assert!(width >= view_w - 4.0, "{view_w}: {width} leaves a margin");
        }
    }

    #[test]
    fn fit_page_fits_both_sides() {
        let (view_w, view_h, gap) = (884.0, 580.0, 8);
        let milli = fit_page_milli(&letter(), view_w, view_h, gap);
        let layout = DocLayout::new(&[letter()], milli, gap);
        assert!(layout.height() as f64 <= view_h);
        assert!(layout.width() as f64 <= view_w);
        // Letter is taller than wide, so the height is what binds in a landscape window.
        assert!(layout.height() as f64 >= view_h - 4.0);
    }

    #[test]
    fn fit_zoom_stays_in_range_for_degenerate_pages() {
        let tiny = PageSize {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        };
        assert_eq!(fit_width_milli(&tiny, 1920.0, 8), ZOOM_MAX_MILLI);
        let empty = PageSize { width: 0.0, ..tiny };
        assert_eq!(fit_width_milli(&empty, 1920.0, 8), ZOOM_MIN_MILLI);
        let huge = PageSize {
            width: 200_000.0,
            ..tiny
        };
        assert_eq!(fit_width_milli(&huge, 1920.0, 8), ZOOM_MIN_MILLI);
    }

    #[test]
    fn one_wheel_notch_is_one_zoom_step() {
        assert_eq!(wheel_zoom_milli(1_000, 60.0), zoom_in_milli(1_000));
        assert_eq!(wheel_zoom_milli(1_000, -60.0), 800);
        assert_eq!(wheel_zoom_milli(1_000, 0.0), 1_000);
        // Half a notch (a touchpad pinch) is half a step: sqrt(1.25) = 1.118.
        assert_eq!(wheel_zoom_milli(1_000, 30.0), 1_118);
        assert_eq!(wheel_zoom_milli(ZOOM_MAX_MILLI, 600.0), ZOOM_MAX_MILLI);
        assert_eq!(wheel_zoom_milli(ZOOM_MIN_MILLI, -600.0), ZOOM_MIN_MILLI);
        assert_eq!(wheel_zoom_milli(1_000, f64::NAN), 1_000);
    }

    #[test]
    fn the_scrollbar_maps_the_whole_document_onto_its_track() {
        let (view, track, min) = (700.0, 600.0, 24.0);
        let doc = 1.0e9; // 20,000 Letter pages at 6400%
        let (top, len) = scrollbar_thumb(0.0, view, doc, track, min);
        assert_eq!((top, len), (0.0, min), "a minimum-length thumb at the top");
        let (bottom, _) = scrollbar_thumb(doc - view, view, doc, track, min);
        assert_eq!(bottom, track - min);
        // Dragging the thumb is the inverse of placing it.
        for pos in [0.0, 1.0e6, 4.0e8, doc - view] {
            let (offset, _) = scrollbar_thumb(pos, view, doc, track, min);
            let back = scrollbar_pos(offset, view, doc, track, min);
            assert!((back - pos).abs() <= 1.0e-3 * doc, "{pos} -> {back}");
        }
        assert_eq!(scrollbar_pos(1.0e9, view, doc, track, min), doc - view);
        // A document that fits shows a full-length thumb and never scrolls.
        assert_eq!(scrollbar_thumb(0.0, view, 500.0, track, min), (0.0, track));
        assert_eq!(scrollbar_pos(100.0, view, 500.0, track, min), 0.0);
    }
}
