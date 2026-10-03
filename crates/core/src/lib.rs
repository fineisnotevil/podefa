// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 [YOUR_NAME] <[YOUR_EMAIL]>

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

/// Abstraction over a PDF rendering and manipulation backend.
pub trait PdfEngine: Send + Sync {
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
}
