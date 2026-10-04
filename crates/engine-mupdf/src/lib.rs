// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! MuPDF backend implementing [`pdf_core::PdfEngine`] and an actor-based engine worker.
//!
//! This crate provides:
//! - Concrete bridge between core PDF traits and the MuPDF C library via `mupdf`.
//! - Typed error handling via [`MupdfError`].
//! - [`EngineHandle`]: A thread-safe actor handle that sends [`pdf_core::EngineCmd`] messages
//!   to a dedicated background thread owning the `mupdf::Document`, receiving [`pdf_core::EngineEvent`]s.

use pdf_core::{
    Bitmap, DocumentInfo, EngineCmd, EngineEvent, PageSize, PdfEngine, PixelFormat, Rect,
    RequestId,
};
use mupdf::{Colorspace, Document, Matrix};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use thiserror::Error;

/// Typed error enum for MuPDF engine operations.
#[derive(Debug, Error)]
pub enum MupdfError {
    /// Failed to open document (e.g. invalid path, corrupted format).
    #[error("Failed to open document '{path}': {source}")]
    OpenFailed {
        path: PathBuf,
        source: mupdf::Error,
    },

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
pub struct MupdfEngine {
    doc: Option<Document>,
    path: Option<PathBuf>,
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
        }
    }

    /// Opens a PDF document with an optional password.
    pub fn open_document_with_password(
        &mut self,
        path: &Path,
        password: Option<&str>,
    ) -> Result<DocumentInfo, MupdfError> {
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

        let count = doc.page_count().map_err(|e| MupdfError::Backend(e.to_string()))? as usize;

        let info = DocumentInfo {
            title: doc.metadata(mupdf::MetadataName::Title).ok().filter(|s| !s.is_empty()),
            author: doc.metadata(mupdf::MetadataName::Author).ok().filter(|s| !s.is_empty()),
            subject: doc.metadata(mupdf::MetadataName::Subject).ok().filter(|s| !s.is_empty()),
            keywords: doc.metadata(mupdf::MetadataName::Keywords).ok().filter(|s| !s.is_empty()),
            creator: doc.metadata(mupdf::MetadataName::Creator).ok().filter(|s| !s.is_empty()),
            producer: doc.metadata(mupdf::MetadataName::Producer).ok().filter(|s| !s.is_empty()),
            page_count: count,
        };

        self.doc = Some(doc);
        self.path = Some(path.to_path_buf());

        Ok(info)
    }

    /// Returns a list of all page dimensions in points.
    pub fn all_page_sizes(&self) -> Result<Vec<PageSize>, MupdfError> {
        let count = self.page_count();
        let mut sizes = Vec::with_capacity(count);
        for i in 0..count {
            let rect = self.page_size(i)?;
            sizes.push(PageSize {
                width: rect.width,
                height: rect.height,
            });
        }
        Ok(sizes)
    }
}

impl PdfEngine for MupdfEngine {
    type Error = MupdfError;

    fn open_document(&mut self, path: &Path) -> Result<DocumentInfo, Self::Error> {
        self.open_document_with_password(path, None)
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

        let b = page
            .bounds()
            .map_err(|e| MupdfError::PageBoundsFailed {
                page_index,
                source: e,
            })?;

        Ok(Rect::new(b.x0, b.y0, b.width(), b.height()))
    }

    fn render_page(
        &self,
        page_index: usize,
        scale: f32,
        _bounds: Option<Rect>,
    ) -> Result<Bitmap, Self::Error> {
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
        let pixmap = page
            .to_pixmap(&ctm, &cs, true, false)
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
                                Ok(_info) => {
                                    let page_count = engine.page_count();
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
                        EngineCmd::RenderPage {
                            page,
                            scale,
                            request_id,
                        } => {
                            // Check if cancelled before starting expensive render
                            let latest_cancelled = cancelled_for_worker.load(Ordering::Relaxed);
                            if latest_cancelled == request_id {
                                continue;
                            }

                            match engine.render_page(page, scale, None) {
                                Ok(bitmap) => {
                                    // Check cancellation again after render before sending
                                    let latest_cancelled = cancelled_for_worker.load(Ordering::Relaxed);
                                    if latest_cancelled != request_id {
                                        let _ = event_tx.send(EngineEvent::PageRendered {
                                            page,
                                            scale,
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
        assert!(matches!(engine.page_size(0), Err(MupdfError::NoDocumentOpen)));
        assert!(matches!(
            engine.render_page(0, 1.0, None),
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
