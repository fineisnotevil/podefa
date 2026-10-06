// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Engine-agnostic core types and traits for PDF viewing.
//!
//! This crate defines fundamental domain primitives:
//! - [`PdfEngine`]: Abstraction over underlying PDF rendering backends.
//! - [`Rect`]: 2D floating-point rectangle for geometry and coordinates.
//! - [`Bitmap`]: Raw pixel buffer for rendered pages.
//! - [`EngineCmd`] and [`EngineEvent`]: Inter-thread messages for actor engines.
//! - [`tiling`]: pure tile geometry, and [`scheduler`]: the pure tile cache/request policy.

use std::error::Error;
use std::path::{Path, PathBuf};

pub mod scheduler;
pub mod tiling;

pub use scheduler::{TileAction, TileScheduler};
pub use tiling::{
    BASE_LONG_PX, PageGeometry, TILE_BLEED_PX, TILE_SIZE_PX, TILE_STRIDE_PX, TileKey, TileRect,
    ZOOM_MAX_MILLI, ZOOM_MIN_MILLI, base_scale_milli, clamp_zoom_milli, scale_from_milli,
    zoom_anchor, zoom_in_milli, zoom_out_milli,
};

/// 2D floating-point rectangle representing coordinates and dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    /// Creates a new rectangle.
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// Pixel format representation for rasterized buffers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    /// Opaque 8-bit RGB, 3 bytes per pixel.
    ///
    /// Every raster this workspace produces is opaque: MuPDF clears an alpha-free pixmap to
    /// `0xff`, so the page is white paper, and an alpha channel would spend a quarter of every
    /// cached tile on a byte that is always `255`.
    Rgb8,
}

impl PixelFormat {
    /// Bytes one pixel occupies in a tightly packed buffer: what `Bitmap::stride` equals for a
    /// row of `width` pixels.
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgb8 => 3,
        }
    }
}

/// In-memory bitmap containing raw rasterized pixels.
///
/// # Pixel format
///
/// Pixel data is in **opaque RGB8** format (3 bytes per pixel: R, G, B), tightly packed:
/// `stride == width * [`PixelFormat::bytes_per_pixel`]`. Slint consumes it as an `Rgb8Pixel`
/// buffer, so the pixels are copied into a Slint image once and never converted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub format: PixelFormat,
    pub data: Vec<u8>,
}

impl Bitmap {
    /// Constructs a new bitmap buffer.
    pub fn new(width: u32, height: u32, stride: usize, format: PixelFormat, data: Vec<u8>) -> Self {
        Self {
            width,
            height,
            stride,
            format,
            data,
        }
    }
}

pub type RequestId = u64;

/// Commands sent from the UI thread to the engine actor thread.
#[derive(Debug)]
pub enum EngineCmd {
    /// Open a PDF file, optionally with a password.
    Open {
        path: PathBuf,
        password: Option<String>,
    },
    /// Render one tile of one page.
    ///
    /// `geometry` is plain data (page index, page origin and size in points, permille scale), so
    /// the engine can validate the tile against the page it loads instead of trusting the caller
    /// blindly.
    RenderTile {
        geometry: PageGeometry,
        col: i32,
        row: i32,
        request_id: RequestId,
    },
    /// Render the low-resolution base layer of one page (Plan 0001, §5 Phase 2).
    ///
    /// `scale_milli` is the page's *base* scale (see [`base_scale_milli`]), not a zoom level: the
    /// base layer is one raster of the whole page that every zoom level stretches, so a page needs
    /// it once. It is rendered as soon as the document is opened and again on a page change.
    ///
    /// Unlike a tile, base work is never cancelled: it belongs to a page rather than to a zoom
    /// level, so superseding an epoch must not drop it. `request_id` names the epoch it was asked
    /// under, which the receiver can report but must not use to reject a result.
    RenderBase {
        page: u32,
        scale_milli: u32,
        request_id: RequestId,
    },
    /// Cancel a pending render request.
    Cancel { request_id: RequestId },
    /// Shut down the engine thread.
    Shutdown,
}

/// Page dimensions in PDF points (1 point = 1/72 inch).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageSize {
    /// Page bound origin on the x axis, PDF points.
    pub x: f32,
    /// Page bound origin on the y axis, PDF points.
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl PageSize {
    /// Bounds as a [`Rect`] for [`PageGeometry::new`].
    pub fn bounds(&self) -> Rect {
        Rect::new(self.x, self.y, self.width, self.height)
    }
}

/// Events sent from the engine actor thread back to the UI thread.
#[derive(Debug)]
pub enum EngineEvent {
    /// Document was successfully opened.
    Opened {
        page_count: usize,
        page_sizes: Vec<PageSize>,
    },
    /// One tile has been rendered.
    ///
    /// `request_id` is the epoch the request was issued under; results carrying an older epoch
    /// are stale and must be dropped by the receiver.
    TileRendered {
        key: TileKey,
        request_id: RequestId,
        bitmap: Bitmap,
    },
    /// The low-resolution base layer of a page (Plan 0001, §5 Phase 2).
    ///
    /// The bitmap covers the page's whole raster rect at `scale_milli`, so the receiver stretches
    /// it to whatever extent the page currently has. It is *not* stale by epoch - one raster serves
    /// every zoom of its page - so the receiver validates it by `page` instead.
    BaseRendered {
        page: u32,
        scale_milli: u32,
        request_id: RequestId,
        bitmap: Bitmap,
    },
    /// A tile was aborted mid-raster because its epoch had been superseded (Plan 0001, §5 Phase 2).
    ///
    /// `ms` is the measured time from the abort signal to the rasterizer noticing it, which is the
    /// number the Phase 2 gate reports. No pixels follow: the receiver has nothing to draw. The pool
    /// reports this from its own abort registry, because a display list stopped by the cancel cookie
    /// returns as normally as one that ran to the end.
    TileAborted { request_id: RequestId, ms: u64 },
    /// An error occurred.
    Error { message: String },
}

/// Minimum allowed zoom scale factor.
pub const ZOOM_MIN: f32 = 0.1;
/// Maximum allowed zoom scale factor.
///
/// Tiling makes the full-page buffer cost irrelevant, so 6400% is a precision/UX ceiling rather
/// than a memory one. Must agree with [`ZOOM_MAX_MILLI`]; `test_zoom_limits_agree` enforces that.
pub const ZOOM_MAX: f32 = 64.0;
/// Default zoom scale factor (actual size).
pub const ZOOM_DEFAULT: f32 = 1.0;
/// Default zoom in permille (actual size).
pub const ZOOM_DEFAULT_MILLI: u32 = 1_000;
/// Zoom step multiplier for zoom-in/zoom-out, kept for the full-page primitive API.
pub const ZOOM_STEP: f32 = 1.25;

/// Clamps a zoom scale factor to the allowed range.
pub fn clamp_zoom(scale: f32) -> f32 {
    scale.clamp(ZOOM_MIN, ZOOM_MAX)
}

/// Abstraction over a PDF rendering backend.
pub trait PdfEngine {
    /// Associated error type produced by engine operations.
    type Error: Error + Send + Sync + 'static;

    /// Opens a PDF document from the filesystem with an optional password.
    fn open_document(&mut self, path: &Path, password: Option<&str>) -> Result<usize, Self::Error>;

    /// Returns the total number of pages in the currently opened document.
    fn page_count(&self) -> usize;

    /// Queries the dimensions of a specific page (0-indexed).
    fn page_size(&self, page_index: usize) -> Result<Rect, Self::Error>;

    /// Renders a page to an in-memory bitmap.
    fn render_page(&self, page_index: usize, scale: f32) -> Result<Bitmap, Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rect_creation() {
        let r = Rect::new(10.0, 20.0, 100.0, 200.0);
        assert_eq!(r.x, 10.0);
        assert_eq!(r.y, 20.0);
        assert_eq!(r.width, 100.0);
        assert_eq!(r.height, 200.0);
    }

    #[test]
    fn test_bitmap_creation() {
        let b = Bitmap::new(2, 2, 6, PixelFormat::Rgb8, vec![0; 12]);
        assert_eq!(b.width, 2);
        assert_eq!(b.height, 2);
        assert_eq!(b.stride, 6);
        assert_eq!(b.format, PixelFormat::Rgb8);
        assert_eq!(b.data.len(), 12);
        // The stride every producer writes is this, and the tests that blit tiles into a
        // full-page canvas read it from here rather than repeating the `3`.
        assert_eq!(PixelFormat::Rgb8.bytes_per_pixel(), 3);
    }

    #[test]
    fn test_clamp_zoom() {
        assert_eq!(clamp_zoom(0.05), ZOOM_MIN);
        assert_eq!(clamp_zoom(1.0), 1.0);
        assert_eq!(clamp_zoom(5.0), 5.0);
        assert_eq!(clamp_zoom(2.0), 2.0);
        // The ceiling is now the tiled renderer's 6400%, not the old full-page 1000%.
        assert_eq!(clamp_zoom(64.0), ZOOM_MAX);
        assert_eq!(clamp_zoom(1_000.0), ZOOM_MAX);
    }

    #[test]
    fn test_zoom_limits_agree() {
        // The tiled renderer works in permille while the full-page primitive stays in f32, so the
        // two representations must not drift apart.
        assert_eq!(ZOOM_MAX, scale_from_milli(ZOOM_MAX_MILLI));
        assert_eq!(ZOOM_MIN, scale_from_milli(ZOOM_MIN_MILLI));
        assert_eq!(ZOOM_DEFAULT, scale_from_milli(ZOOM_DEFAULT_MILLI));
    }

    #[test]
    fn test_page_size() {
        let ps = PageSize {
            x: 10.0,
            y: 20.0,
            width: 612.0,
            height: 792.0,
        };
        assert_eq!(ps.width, 612.0);
        assert_eq!(ps.height, 792.0);
        assert_eq!(ps.bounds(), Rect::new(10.0, 20.0, 612.0, 792.0));
    }
}
