// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! MuPDF backend implementing [`pdf_core::PdfEngine`] and an actor-based engine worker.
//!
//! This crate provides:
//! - Concrete bridge between core PDF traits and the MuPDF C library via `mupdf`.
//! - Typed error handling via [`MupdfError`].
//! - [`EngineHandle`]: A thread-safe actor handle that sends [`pdf_core::EngineCmd`] messages
//!   to a dedicated background thread owning the `mupdf::Document`, receiving [`pdf_core::EngineEvent`]s.

use mupdf::{Colorspace, Device, DisplayList, Document, Matrix, Pixmap};
use pdf_core::{
    Bitmap, EngineCmd, EngineEvent, PageGeometry, PageSize, PdfEngine, PixelFormat, Rect,
    RequestId, TILE_BLEED_PX, TILE_SIZE_PX, TILE_STRIDE_PX, TileRect,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use thiserror::Error;

/// Typed error enum for MuPDF engine operations.
#[derive(Debug, Error)]
pub enum MupdfError {
    /// Failed to open document (e.g. invalid path, corrupted format).
    #[error("Failed to open document '{path}': {source}")]
    OpenFailed { path: PathBuf, source: mupdf::Error },

    /// The document is password protected and requires authentication.
    #[error("Document is password protected")]
    PasswordRequired,

    /// Password authentication failed.
    #[error("Invalid password provided")]
    InvalidPassword,

    /// No document is currently open.
    #[error("No document is currently open")]
    NoDocumentOpen,

    /// Page index was out of bounds.
    #[error("Page index {page_index} out of bounds (total pages: {total_pages})")]
    PageOutOfBounds {
        page_index: usize,
        total_pages: usize,
    },

    /// Failed to load the requested page.
    #[error("Failed to load page {page_index}: {source}")]
    PageLoadFailed {
        page_index: usize,
        source: mupdf::Error,
    },

    /// Failed to determine page bounds.
    #[error("Failed to get bounds for page {page_index}: {source}")]
    PageBoundsFailed {
        page_index: usize,
        source: mupdf::Error,
    },

    /// Failed to render the page to a pixmap.
    #[error("Failed to render page {page_index}: {source}")]
    RenderFailed {
        page_index: usize,
        source: mupdf::Error,
    },

    /// Failed to rasterize one tile.
    #[error("Failed to render tile ({col},{row}) of page {page_index}: {source}")]
    TileRenderFailed {
        page_index: u32,
        col: i32,
        row: i32,
        source: mupdf::Error,
    },

    /// The requested cell does not intersect the page.
    #[error("Tile ({col},{row}) lies outside page {page_index}")]
    TileOutsidePage { page_index: u32, col: i32, row: i32 },

    /// The caller's tile grid and MuPDF disagree about the page raster size.
    ///
    /// This means the grid would place tiles at the wrong offsets, so it is reported instead of
    /// silently drawing the wrong region.
    #[error(
        "Tile grid disagrees with MuPDF on page {page_index}: grid {expected:?}, renderer {actual:?}"
    )]
    TileGeometryMismatch {
        page_index: u32,
        expected: (u32, u32),
        actual: (u32, u32),
    },

    /// Non-UTF8 path provided.
    #[error("Path is not valid UTF-8: '{0}'")]
    InvalidPath(PathBuf),

    /// General backend or system error.
    #[error("Backend error: {0}")]
    Backend(String),
}

/// MuPDF-backed implementation of [`PdfEngine`].
///
/// Encapsulates a [`mupdf::Document`] and provides direct, synchronous access.
/// Note that `mupdf::Document` is not thread-safe; use [`EngineHandle`] when interacting
/// across threads.
///
/// The tiled renderer keeps two reusable buffers so that rendering a tile allocates no pixel
/// memory (Plan 0001, amendments 5 and 6):
///
/// - `scratch` is one `TILE_STRIDE_PX` square RGBA8 pixmap, cleared per tile. It is allocated
///   with origin `(0, 0)`, which makes the draw device transform the identity, so device
///   coordinates and buffer coordinates coincide and the tile offset lives in the CTM.
/// - `display_list` caches the page's display list, which is scale independent, so every zoom
///   level of a page reuses one parse of its content. One such list per engine means one per
///   rasterizing thread, as Phase 2's worker pool requires.
pub struct MupdfEngine {
    doc: Option<Document>,
    path: Option<PathBuf>,
    scratch: Option<Pixmap>,
    display_list: Option<(u32, DisplayList)>,
}

impl Default for MupdfEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl MupdfEngine {
    /// Creates a new uninitialized MuPDF engine instance.
    pub fn new() -> Self {
        Self {
            doc: None,
            path: None,
            scratch: None,
            display_list: None,
        }
    }

    /// Opens a PDF document with an optional password, returning the page count.
    pub fn open_document_with_password(
        &mut self,
        path: &Path,
        password: Option<&str>,
    ) -> Result<usize, MupdfError> {
        let path_str = path
            .to_str()
            .ok_or_else(|| MupdfError::InvalidPath(path.to_path_buf()))?;

        let mut doc = Document::open(path_str).map_err(|e| MupdfError::OpenFailed {
            path: path.to_path_buf(),
            source: e,
        })?;

        let needs_pwd = doc.needs_password().unwrap_or(false);
        if needs_pwd {
            if let Some(pwd) = password {
                let authed = doc.authenticate(pwd).unwrap_or(false);
                if !authed {
                    return Err(MupdfError::InvalidPassword);
                }
            } else {
                return Err(MupdfError::PasswordRequired);
            }
        }

        let count = doc
            .page_count()
            .map_err(|e| MupdfError::Backend(e.to_string()))?
            .max(0) as usize;

        self.doc = Some(doc);
        self.path = Some(path.to_path_buf());
        // A display list belongs to the document that produced it.
        self.display_list = None;

        Ok(count)
    }

    /// Returns a list of all page dimensions in points.
    pub fn all_page_sizes(&self) -> Result<Vec<PageSize>, MupdfError> {
        let count = self.page_count();
        let mut sizes = Vec::with_capacity(count);
        for i in 0..count {
            let rect = self.page_size(i)?;
            sizes.push(PageSize {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
            });
        }
        Ok(sizes)
    }

    /// Renders one tile of one page at the scale carried by `geometry`.
    ///
    /// The tile is drawn into a single reused scratch pixmap with a bleed band around it and
    /// cropped down to the tile size, so the result is pixel-identical to the same region of a
    /// full-page render while no buffer is allocated per tile (Plan 0001, §3.3 and §4.3).
    pub fn render_tile(
        &mut self,
        geometry: &PageGeometry,
        col: i32,
        row: i32,
    ) -> Result<Bitmap, MupdfError> {
        let page_index = geometry.page;
        let total = self.page_count();
        if page_index as usize >= total {
            return Err(MupdfError::PageOutOfBounds {
                page_index: page_index as usize,
                total_pages: total,
            });
        }
        let Some(tile) = geometry.tile_rect(col, row) else {
            return Err(MupdfError::TileOutsidePage {
                page_index,
                col,
                row,
            });
        };

        // Take the scratch out of `self` so the display-list cache can still be borrowed
        // mutably; it is put back before returning, including on the error path.
        let mut scratch = match self.scratch.take() {
            Some(scratch) => scratch,
            None => {
                let cs = Colorspace::device_rgb();
                let side = TILE_STRIDE_PX as i32;
                Pixmap::new(&cs, 0, 0, side, side, true).map_err(|e| {
                    MupdfError::TileRenderFailed {
                        page_index,
                        col,
                        row,
                        source: e,
                    }
                })?
            }
        };
        let outcome = self.rasterize_tile(&mut scratch, geometry, col, row, tile);
        self.scratch = Some(scratch);
        outcome
    }

    /// Draws one tile into `scratch` and crops it out. See [`Self::render_tile`].
    fn rasterize_tile(
        &mut self,
        scratch: &mut Pixmap,
        geometry: &PageGeometry,
        col: i32,
        row: i32,
        tile: TileRect,
    ) -> Result<Bitmap, MupdfError> {
        let page_index = geometry.page;
        let doc = self.doc.as_ref().ok_or(MupdfError::NoDocumentOpen)?;
        let page = doc
            .load_page(page_index as i32)
            .map_err(|e| MupdfError::PageLoadFailed {
                page_index: page_index as usize,
                source: e,
            })?;

        // The full-page raster is `fz_round_rect(transform(bounds, scale))`; check that the grid
        // agrees before trusting its cell offsets, otherwise tiles land at the wrong offsets.
        let scale = geometry.scale();
        let scale_matrix = Matrix::new_scale(scale, scale);
        let bbox = page
            .bounds()
            .map_err(|e| MupdfError::PageBoundsFailed {
                page_index: page_index as usize,
                source: e,
            })?
            .transform(&scale_matrix)
            .round();
        let actual = (
            (bbox.x1 - bbox.x0).max(0) as u32,
            (bbox.y1 - bbox.y0).max(0) as u32,
        );
        let expected = geometry.device_size();
        if actual != expected {
            return Err(MupdfError::TileGeometryMismatch {
                page_index,
                expected,
                actual,
            });
        }

        // Scratch origin in device pixels: the cell's top-left corner minus the bleed band.
        let x0 = bbox.x0 + col * TILE_SIZE_PX as i32 - TILE_BLEED_PX;
        let y0 = bbox.y0 + row * TILE_SIZE_PX as i32 - TILE_BLEED_PX;

        scratch.clear().map_err(|e| MupdfError::TileRenderFailed {
            page_index,
            col,
            row,
            source: e,
        })?;

        let display_list = self.display_list_for(page_index, &page)?;
        {
            let device =
                Device::from_pixmap(scratch).map_err(|e| MupdfError::TileRenderFailed {
                    page_index,
                    col,
                    row,
                    source: e,
                })?;
            let mut ctm = Matrix::new_scale(scale, scale);
            ctm.concat(Matrix::new_translate(-(x0 as f32), -(y0 as f32)));
            // The scratch device transform is the identity, so the scissor is expressed in the
            // same space as the CTM applied to page coordinates.
            let area = mupdf::Rect::new(0.0, 0.0, TILE_STRIDE_PX as f32, TILE_STRIDE_PX as f32);
            display_list
                .run(&device, &ctm, area)
                .map_err(|e| MupdfError::TileRenderFailed {
                    page_index,
                    col,
                    row,
                    source: e,
                })?;
        }

        // Crop the inner tile out of the padded scratch, tightly packed (stride == w * 4).
        let stride = scratch.stride().max(0) as usize;
        let bleed = TILE_BLEED_PX as usize;
        let row_bytes = tile.row_bytes();
        let mut data = vec![0u8; tile.byte_len()];
        {
            let samples = scratch.samples();
            for r in 0..tile.h as usize {
                let src = (r + bleed) * stride + bleed * 4;
                let dst = r * row_bytes;
                data[dst..dst + row_bytes].copy_from_slice(&samples[src..src + row_bytes]);
            }
        }
        Ok(Bitmap::new(
            tile.w,
            tile.h,
            row_bytes,
            PixelFormat::Rgba8,
            data,
        ))
    }

    /// Returns the cached display list for a page, rebuilding it when the page changes.
    ///
    /// Display lists are scale independent, so one list serves every zoom level of a page and a
    /// zoom step re-interprets the page content not at all.
    fn display_list_for(
        &mut self,
        page_index: u32,
        page: &mupdf::Page,
    ) -> Result<&DisplayList, MupdfError> {
        let stale = self
            .display_list
            .as_ref()
            .is_none_or(|(cached, _)| *cached != page_index);
        if stale {
            let list = page
                .to_display_list(false)
                .map_err(|e| MupdfError::RenderFailed {
                    page_index: page_index as usize,
                    source: e,
                })?;
            self.display_list = Some((page_index, list));
        }
        match &self.display_list {
            Some((_, list)) => Ok(list),
            None => Err(MupdfError::Backend(
                "display list cache was not populated".to_string(),
            )),
        }
    }
}

impl PdfEngine for MupdfEngine {
    type Error = MupdfError;

    fn open_document(&mut self, path: &Path, password: Option<&str>) -> Result<usize, Self::Error> {
        self.open_document_with_password(path, password)
    }

    fn page_count(&self) -> usize {
        self.doc
            .as_ref()
            .and_then(|d| d.page_count().ok())
            .map(|c| c.max(0) as usize)
            .unwrap_or(0)
    }

    fn page_size(&self, page_index: usize) -> Result<Rect, Self::Error> {
        let doc = self.doc.as_ref().ok_or(MupdfError::NoDocumentOpen)?;
        let total = self.page_count();
        if page_index >= total {
            return Err(MupdfError::PageOutOfBounds {
                page_index,
                total_pages: total,
            });
        }

        let page = doc
            .load_page(page_index as i32)
            .map_err(|e| MupdfError::PageLoadFailed {
                page_index,
                source: e,
            })?;

        let b = page.bounds().map_err(|e| MupdfError::PageBoundsFailed {
            page_index,
            source: e,
        })?;

        Ok(Rect::new(b.x0, b.y0, b.width(), b.height()))
    }

    fn render_page(&self, page_index: usize, scale: f32) -> Result<Bitmap, Self::Error> {
        let doc = self.doc.as_ref().ok_or(MupdfError::NoDocumentOpen)?;
        let total = self.page_count();
        if page_index >= total {
            return Err(MupdfError::PageOutOfBounds {
                page_index,
                total_pages: total,
            });
        }

        let page = doc
            .load_page(page_index as i32)
            .map_err(|e| MupdfError::PageLoadFailed {
                page_index,
                source: e,
            })?;

        let ctm = Matrix::new_scale(scale, scale);
        let cs = Colorspace::device_rgb();
        // alpha = true produces 4 channels: RGBA (non-premultiplied)
        let pixmap =
            page.to_pixmap(&ctm, &cs, true, false)
                .map_err(|e| MupdfError::RenderFailed {
                    page_index,
                    source: e,
                })?;

        let width = pixmap.width();
        let height = pixmap.height();
        let stride = pixmap.stride().max(0) as usize;
        let samples = pixmap.samples();

        // Convert / clone raw samples into owned vector
        let data = samples.to_vec();

        Ok(Bitmap::new(width, height, stride, PixelFormat::Rgba8, data))
    }
}

/// Actor handle for the background MuPDF engine thread.
///
/// Sends commands to a single dedicated thread owning the MuPDF document.
/// Upon dropping, sends [`EngineCmd::Shutdown`] and joins the worker thread,
/// preventing thread leaks.
pub struct EngineHandle {
    cmd_tx: Sender<EngineCmd>,
    cancelled_request: Arc<AtomicU64>,
    worker_thread: Option<JoinHandle<()>>,
}

impl EngineHandle {
    /// Spawns a new dedicated engine actor thread.
    ///
    /// The returned handle provides command sending, and events from the engine
    /// are received on the returned `Receiver<EngineEvent>`.
    pub fn spawn() -> (Self, Receiver<EngineEvent>) {
        let (cmd_tx, cmd_rx) = mpsc::channel::<EngineCmd>();
        let (event_tx, event_rx) = mpsc::channel::<EngineEvent>();
        let cancelled_request = Arc::new(AtomicU64::new(0));
        let cancelled_for_worker = Arc::clone(&cancelled_request);

        let worker_thread = thread::Builder::new()
            .name("mupdf-engine-actor".to_string())
            .spawn(move || {
                let mut engine = MupdfEngine::new();

                while let Ok(cmd) = cmd_rx.recv() {
                    match cmd {
                        EngineCmd::Open { path, password } => {
                            match engine.open_document_with_password(&path, password.as_deref()) {
                                Ok(page_count) => {
                                    let page_sizes = engine.all_page_sizes().unwrap_or_default();
                                    let _ = event_tx.send(EngineEvent::Opened {
                                        page_count,
                                        page_sizes,
                                    });
                                }
                                Err(err) => {
                                    let _ = event_tx.send(EngineEvent::Error {
                                        message: err.to_string(),
                                    });
                                }
                            }
                        }
                        EngineCmd::RenderTile {
                            geometry,
                            col,
                            row,
                            request_id,
                        } => {
                            // Tiles are additive, so unlike the old full-page request there is
                            // nothing to coalesce: each command is one tile. A request from a
                            // superseded epoch is dropped before any raster time is spent.
                            let cancelled = cancelled_for_worker.load(Ordering::Relaxed);
                            if cancelled == request_id {
                                continue;
                            }

                            match engine.render_tile(&geometry, col, row) {
                                Ok(bitmap) => {
                                    // The epoch may have moved on while this tile rendered; the
                                    // UI thread would drop such a result anyway, so keep it off
                                    // the event channel.
                                    if cancelled_for_worker.load(Ordering::Relaxed) != request_id {
                                        let _ = event_tx.send(EngineEvent::TileRendered {
                                            key: geometry.key(col, row),
                                            request_id,
                                            bitmap,
                                        });
                                    }
                                }
                                Err(err) => {
                                    let _ = event_tx.send(EngineEvent::Error {
                                        message: err.to_string(),
                                    });
                                }
                            }
                        }
                        EngineCmd::Cancel { request_id } => {
                            cancelled_for_worker.store(request_id, Ordering::Relaxed);
                        }
                        EngineCmd::Shutdown => {
                            break;
                        }
                    }
                }
            })
            .expect("Failed to spawn engine actor thread");

        let handle = Self {
            cmd_tx,
            cancelled_request,
            worker_thread: Some(worker_thread),
        };

        (handle, event_rx)
    }

    /// Returns a reference to the command sender.
    pub fn sender(&self) -> &Sender<EngineCmd> {
        &self.cmd_tx
    }

    /// Convenience method to send an [`EngineCmd`].
    pub fn send(&self, cmd: EngineCmd) -> Result<(), mpsc::SendError<EngineCmd>> {
        self.cmd_tx.send(cmd)
    }

    /// Cancels a specific request ID.
    pub fn cancel(&self, request_id: RequestId) {
        self.cancelled_request.store(request_id, Ordering::Relaxed);
        let _ = self.cmd_tx.send(EngineCmd::Cancel { request_id });
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(EngineCmd::Shutdown);
        if let Some(handle) = self.worker_thread.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mupdf_engine_uninitialized() {
        let engine = MupdfEngine::new();
        assert_eq!(engine.page_count(), 0);
        assert!(matches!(
            engine.page_size(0),
            Err(MupdfError::NoDocumentOpen)
        ));
        assert!(matches!(
            engine.render_page(0, 1.0),
            Err(MupdfError::NoDocumentOpen)
        ));
    }

    #[test]
    fn test_engine_actor_spawn_and_shutdown() {
        let (handle, rx) = EngineHandle::spawn();
        handle.send(EngineCmd::Cancel { request_id: 42 }).unwrap();
        // Drop handle, verifying clean shutdown and join
        drop(handle);
        // Receiver should disconnect after actor thread exits
        assert!(rx.recv().is_err());
    }
}
