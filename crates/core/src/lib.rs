// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Engine-agnostic core types and traits for PDF viewing and editing.
//!
//! This crate defines fundamental domain primitives:
//! - [`PdfEngine`]: Abstraction over underlying PDF rendering and parsing backends.
//! - [`DocumentInfo`]: Metadata describing an opened PDF document.
//! - [`Rect`]: 2D floating-point rectangle for geometry and bounding boxes.
//! - [`Bitmap`]: Raw pixel buffer for rendered pages and tiles.
//! - [`Command`]: Undo/redo command abstraction.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::path::Path;

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
    Rgb8,
    Bgra8,
}

/// In-memory bitmap containing raw rasterized pixels.
///
/// # Pixel format
///
/// When produced by the MuPDF backend, pixel data is in **non-premultiplied RGBA8**
/// format (4 bytes per pixel: R, G, B, A). The `format` field indicates the
/// exact layout. Slint expects non-premultiplied RGBA8, so no conversion is
/// needed when using [`PixelFormat::Rgba8`].
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

    /// Returns the total expected byte count for the pixel data.
    pub fn expected_data_len(width: u32, height: u32, format: PixelFormat) -> usize {
        let bytes_per_pixel = match format {
            PixelFormat::Rgba8 | PixelFormat::Bgra8 => 4,
            PixelFormat::Rgb8 => 3,
        };
        (width as usize) * (height as usize) * bytes_per_pixel
    }
}

/// Metadata summary of a PDF document.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DocumentInfo {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
    pub page_count: usize,
}

pub type RequestId = u64;

/// Commands sent from the UI thread to the engine actor thread.
#[derive(Debug)]
pub enum EngineCmd {
    /// Open a PDF file, optionally with a password.
    Open {
        path: std::path::PathBuf,
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

/// Abstraction over a PDF rendering and manipulation backend.
pub trait PdfEngine {
    /// Associated error type produced by engine operations.
    type Error: Error + Send + Sync + 'static;

    /// Opens a PDF document from the filesystem.
    fn open_document(&mut self, path: &Path) -> Result<DocumentInfo, Self::Error>;

    /// Returns the total number of pages in the currently opened document.
    fn page_count(&self) -> usize;

    /// Queries the dimensions of a specific page (0-indexed).
    fn page_size(&self, page_index: usize) -> Result<Rect, Self::Error>;

    /// Renders a page or sub-rectangle of a page to an in-memory bitmap.
    fn render_page(
        &self,
        page_index: usize,
        scale: f32,
        bounds: Option<Rect>,
    ) -> Result<Bitmap, Self::Error>;
}

/// Trait representing an invertible action for undo/redo stacks.
pub trait Command {
    /// Error type returned if command execution or reversal fails.
    type Error: Error;

    /// Human-readable label for this command (e.g. for display in undo menus).
    fn name(&self) -> &str;

    /// Executes the command, applying changes to the state.
    fn execute(&mut self) -> Result<(), Self::Error>;

    /// Reverses the changes applied by [`Command::execute`].
    fn undo(&mut self) -> Result<(), Self::Error>;
}

/// Minimal command error type for testing and generic usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandError(pub String);

impl Display for CommandError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Error for CommandError {}

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

    struct MockCommand {
        applied: bool,
    }

    impl Command for MockCommand {
        type Error = CommandError;

        fn name(&self) -> &str {
            "MockAction"
        }

        fn execute(&mut self) -> Result<(), Self::Error> {
            self.applied = true;
            Ok(())
        }

        fn undo(&mut self) -> Result<(), Self::Error> {
            self.applied = false;
            Ok(())
        }
    }

    #[test]
    fn test_command_undo_redo() {
        let mut cmd = MockCommand { applied: false };
        assert_eq!(cmd.name(), "MockAction");
        assert!(cmd.execute().is_ok());
        assert!(cmd.applied);
        assert!(cmd.undo().is_ok());
        assert!(!cmd.applied);
    }

    #[test]
    fn test_clamp_zoom() {
        assert_eq!(clamp_zoom(0.05), ZOOM_MIN);
        assert_eq!(clamp_zoom(1.0), 1.0);
        assert_eq!(clamp_zoom(15.0), ZOOM_MAX);
        assert_eq!(clamp_zoom(5.0), 5.0);
    }

    #[test]
    fn test_expected_data_len() {
        assert_eq!(
            Bitmap::expected_data_len(100, 200, PixelFormat::Rgba8),
            80_000
        );
        assert_eq!(
            Bitmap::expected_data_len(100, 200, PixelFormat::Rgb8),
            60_000
        );
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
