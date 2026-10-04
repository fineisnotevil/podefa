// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Engine-agnostic core types and traits for PDF viewing.
//!
//! This crate defines fundamental domain primitives:
//! - [`PdfEngine`]: Abstraction over underlying PDF rendering backends.
//! - [`Rect`]: 2D floating-point rectangle for geometry and coordinates.
//! - [`Bitmap`]: Raw pixel buffer for rendered pages.
//! - [`EngineCmd`] and [`EngineEvent`]: Inter-thread messages for actor engines.

use std::error::Error;
use std::path::{Path, PathBuf};

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
    Rgba8,
}

/// In-memory bitmap containing raw rasterized pixels.
///
/// # Pixel format
///
/// Pixel data is in **non-premultiplied RGBA8** format (4 bytes per pixel: R, G, B, A).
/// Slint natively consumes non-premultiplied RGBA8.
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
    /// Render a specific page at a given scale.
    RenderPage {
        page: usize,
        scale: f32,
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
    pub width: f32,
    pub height: f32,
}

/// Events sent from the engine actor thread back to the UI thread.
#[derive(Debug)]
pub enum EngineEvent {
    /// Document was successfully opened.
    Opened {
        page_count: usize,
        page_sizes: Vec<PageSize>,
    },
    /// A page has been rendered to a bitmap.
    PageRendered {
        page: usize,
        scale: f32,
        request_id: RequestId,
        bitmap: Bitmap,
    },
    /// An error occurred.
    Error { message: String },
}

/// Minimum allowed zoom scale factor.
pub const ZOOM_MIN: f32 = 0.1;
/// Maximum allowed zoom scale factor.
pub const ZOOM_MAX: f32 = 10.0;
/// Default zoom scale factor (fit to actual size).
pub const ZOOM_DEFAULT: f32 = 1.0;
/// Zoom step multiplier for zoom-in/zoom-out.
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
        let b = Bitmap::new(2, 2, 8, PixelFormat::Rgba8, vec![0; 16]);
        assert_eq!(b.width, 2);
        assert_eq!(b.height, 2);
        assert_eq!(b.stride, 8);
        assert_eq!(b.format, PixelFormat::Rgba8);
        assert_eq!(b.data.len(), 16);
    }

    #[test]
    fn test_clamp_zoom() {
        assert_eq!(clamp_zoom(0.05), ZOOM_MIN);
        assert_eq!(clamp_zoom(1.0), 1.0);
        assert_eq!(clamp_zoom(15.0), ZOOM_MAX);
        assert_eq!(clamp_zoom(5.0), 5.0);
    }

    #[test]
    fn test_page_size() {
        let ps = PageSize {
            width: 612.0,
            height: 792.0,
        };
        assert_eq!(ps.width, 612.0);
        assert_eq!(ps.height, 792.0);
    }
}
