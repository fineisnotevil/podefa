// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! MuPDF backend implementing [`core::PdfEngine`].
//!
//! This crate provides the concrete bridge between the core PDF engine traits
//! and the MuPDF C library via the `mupdf` crate bindings.

use core::{Bitmap, DocumentInfo, PdfEngine, Rect};
use std::fmt::{self, Display, Formatter};
use std::path::Path;

/// Error type produced by the MuPDF engine backend.
#[derive(Debug)]
pub enum MupdfError {
    /// Generic engine or backend failure.
    Backend(String),
    /// Requested page was not found or out of bounds.
    PageNotFound(usize),
}

impl Display for MupdfError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(msg) => write!(f, "MuPDF backend error: {msg}"),
            Self::PageNotFound(page) => write!(f, "Page {page} not found"),
        }
    }
}

impl std::error::Error for MupdfError {}

/// MuPDF-backed implementation of [`PdfEngine`].
#[derive(Default)]
pub struct MupdfEngine {
    // TODO: Hold an optional mupdf::Document once opened.
    _marker: Option<()>,
}

impl MupdfEngine {
    /// Creates a new uninitialized MuPDF engine instance.
    pub fn new() -> Self {
        Self::default()
    }
}

impl PdfEngine for MupdfEngine {
    type Error = MupdfError;

    fn open_document(&mut self, _path: &Path) -> Result<DocumentInfo, Self::Error> {
        // TODO: Implement document opening using mupdf::Document::open
        Err(MupdfError::Backend(
            "Document loading not implemented yet".to_string(),
        ))
    }

    fn page_count(&self) -> usize {
        // TODO: Return page count from mupdf document
        0
    }

    fn page_size(&self, _page_index: usize) -> Result<Rect, Self::Error> {
        // TODO: Return page rectangle using mupdf page bounds
        Err(MupdfError::Backend(
            "Page size lookup not implemented yet".to_string(),
        ))
    }

    fn render_page(
        &self,
        _page_index: usize,
        _scale: f32,
        _bounds: Option<Rect>,
    ) -> Result<Bitmap, Self::Error> {
        // TODO: Implement page/tile rasterization using mupdf pixmap and colorspace
        Err(MupdfError::Backend(
            "Rendering not implemented yet".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mupdf_engine_stub_initialization() {
        // Verify mupdf crate linkage and stub initial state
        let _ = mupdf::Colorspace::device_rgb;
        let engine = MupdfEngine::new();
        assert_eq!(engine.page_count(), 0);
    }
}
