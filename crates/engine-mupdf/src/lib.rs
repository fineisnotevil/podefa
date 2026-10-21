// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! MuPDF backend implementing [`pdf_core::PdfEngine`] and an actor-based engine worker.
//!
//! This crate provides:
//! - Concrete bridge between core PDF traits and the MuPDF C library via `mupdf`.
//! - Typed error handling via [`MupdfError`].
//! - [`EngineHandle`]: A thread-safe actor handle that sends [`pdf_core::EngineCmd`] messages
//!   to a dedicated background thread owning the `mupdf::Document`, receiving [`pdf_core::EngineEvent`]s.
//! - The render worker pool the actor dispatches to (`pool`): tiles rasterize on
//!   [`worker_count`] threads, which never touch the document because the actor hands them
//!   *resolved* jobs (page display list included) instead of commands.

mod pool;
mod search;

use mupdf::{Colorspace, DisplayList, Document, Matrix, Pixmap};
use pdf_core::{
    Bitmap, EngineCmd, EngineEvent, OutlineItem, PageGeometry, PageSize, PdfEngine, PixelFormat,
    Rect, RequestId, TILE_STRIDE_PX, thumb_scale_milli,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use thiserror::Error;

use pool::{Job, Pool, Prepared, run_tile};
pub use pool::{MID_RASTER_ABORT, worker_count};
use search::SearchThread;

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
/// - `scratch` is one `TILE_STRIDE_PX` square RGB8 pixmap, cleared per tile. It is allocated
///   with origin `(0, 0)`, which makes the draw device transform the identity, so device
///   coordinates and buffer coordinates coincide and the tile offset lives in the CTM.
/// - `display_lists` caches the display lists of the last [`DISPLAY_LIST_CACHE_PAGES`] pages used,
///   most recent last. A list is scale independent, so every zoom level of a page reuses one parse
///   of its content, and it is handed to the pool as an `Arc`, so one list per *page* serves every
///   rasterizing thread at once. Several entries because continuous scroll shows several pages at
///   once: a one-page cache would rebuild each visible page's list every time a tile of the other
///   one was requested (Plan 0002, §3.3).
pub struct MupdfEngine {
    doc: Option<Document>,
    path: Option<PathBuf>,
    scratch: Option<Pixmap>,
    display_lists: Vec<(u32, Arc<DisplayList>)>,
}

/// Whether pages are drawn with their annotations (highlights, notes, stamps, form-field
/// appearances), as other PDF readers draw them. `false` would draw the page's content stream alone,
/// which hid every annotation a document had until Plan 0004 §3.1. One constant for the display
/// lists and the full-page render, so tiles and their reference render cannot disagree.
const DRAW_ANNOTATIONS: bool = true;

/// Display lists kept per document: the pages near the viewport, plus room for the one the user
/// just left (Plan 0002, §4.3).
///
/// Bounded by count, not bytes, because a list's size is not known yet (`docs/backlog.md` item 1
/// investigates it); a page that is drawn as tiles is at least one tile tall, so a viewport shows
/// only a handful, and smaller pages are one-tile jobs whose list is rebuilt cheaply if evicted.
pub const DISPLAY_LIST_CACHE_PAGES: usize = 8;

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
            display_lists: Vec::new(),
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
        self.display_lists.clear();

        Ok(count)
    }

    /// Every page's displayed size, never failing as a whole: a page whose bounds cannot be read
    /// gets Letter, and its index is returned beside the table so the caller can say so.
    ///
    /// The actor used `all_page_sizes().unwrap_or_default()`, so one unreadable page emptied the
    /// whole table and the app showed no document at all (Plan 0002, §3.3).
    pub fn page_sizes_lenient(&self) -> (Vec<PageSize>, Vec<u32>) {
        let count = self.page_count();
        let mut sizes = Vec::with_capacity(count);
        let mut failed = Vec::new();
        for i in 0..count {
            let rect = self.page_size(i).unwrap_or_else(|_| {
                failed.push(i as u32);
                Rect::new(0.0, 0.0, 612.0, 792.0)
            });
            sizes.push(PageSize {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
            });
        }
        (sizes, failed)
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

    /// Renders one tile of one page at the scale carried by `geometry`, on the calling thread.
    ///
    /// This is the serial reference: the benchmarks and the pool tests use it as the render the
    /// pool has to reproduce pixel for pixel. The actor itself always dispatches to the pool, where
    /// `FA_PDF_WORKERS=1` gives the same serial rasterization on one worker thread. The drawing
    /// itself is [`run_tile`], so both paths rasterize through the same code.
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
        let prepared = self.prepare(geometry)?;
        if self.scratch.is_none() {
            let cs = Colorspace::device_rgb();
            let side = TILE_STRIDE_PX as i32;
            self.scratch = Some(Pixmap::new(&cs, 0, 0, side, side, false).map_err(|source| {
                MupdfError::TileRenderFailed {
                    page_index: geometry.page,
                    col,
                    row,
                    source,
                }
            })?);
        }
        let scratch = self
            .scratch
            .as_mut()
            .expect("scratch was allocated just above");
        run_tile(&prepared, col, row, scratch, None)
    }

    /// Resolves and validates everything a tile render needs, without rasterizing anything.
    ///
    /// The pool works from the result ([`Prepared`]) rather than from [`EngineCmd`]s, which is what
    /// keeps the document on this thread: the page display list comes back as a shared pointer, and
    /// the raster origin is taken from MuPDF's own rounded bounds, so a worker cannot disagree with
    /// the renderer about where a tile goes.
    ///
    /// This is also where the grid agreement check (Plan 0001, §4.1 risk 1.6) happens: MuPDF's
    /// raster size at this scale must equal what `core`'s geometry computes, otherwise the cell
    /// offsets would silently drift. Once per page *and scale*, instead of once per tile.
    fn prepare(&mut self, geometry: &PageGeometry) -> Result<Prepared, MupdfError> {
        self.prepare_with(geometry, true)
    }

    /// [`Self::prepare`], with the display list either taken from the page cache (`cache`) or
    /// built for this one job and left out of it - what thumbnails need, so a sidebar scrolled
    /// through thousands of pages cannot evict the viewport's lists (Plan 0003, §3.5).
    fn prepare_with(
        &mut self,
        geometry: &PageGeometry,
        cache: bool,
    ) -> Result<Prepared, MupdfError> {
        let page_index = geometry.page;
        let total = self.page_count();
        if page_index as usize >= total {
            return Err(MupdfError::PageOutOfBounds {
                page_index: page_index as usize,
                total_pages: total,
            });
        }
        let doc = self.doc.as_ref().ok_or(MupdfError::NoDocumentOpen)?;
        let page =
            doc.load_page(page_index as i32)
                .map_err(|source| MupdfError::PageLoadFailed {
                    page_index: page_index as usize,
                    source,
                })?;

        // The full-page raster is `fz_round_rect(transform(bounds, scale))`; check that the grid
        // agrees before trusting its cell offsets, otherwise tiles land at the wrong offsets.
        let scale = geometry.scale();
        let scale_matrix = Matrix::new_scale(scale, scale);
        let bbox = page
            .bounds()
            .map_err(|source| MupdfError::PageBoundsFailed {
                page_index: page_index as usize,
                source,
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

        let list = if cache {
            self.display_list_for(page_index, &page)?
        } else {
            Arc::new(page.to_display_list(DRAW_ANNOTATIONS).map_err(|e| {
                MupdfError::RenderFailed {
                    page_index: page_index as usize,
                    source: e,
                }
            })?)
        };
        Ok(Prepared {
            geometry: *geometry,
            list,
            origin: (bbox.x0, bbox.y0),
        })
    }

    /// Resolves the low-resolution base layer of a page (Plan 0001, §5 Phase 2).
    ///
    /// The base layer is a *page* raster, so its geometry is built here from the page's own bounds
    /// rather than from the caller: `scale_milli` is the page's base scale
    /// ([`pdf_core::base_scale_milli`]), not a zoom level, and one bitmap therefore serves every
    /// zoom level of that page.
    pub(crate) fn prepare_base(
        &mut self,
        page_index: u32,
        scale_milli: u32,
    ) -> Result<Prepared, MupdfError> {
        let bounds = self.page_size(page_index as usize)?;
        let geometry = PageGeometry::new(page_index, bounds, scale_milli);
        self.prepare(&geometry)
    }

    /// Pages whose display list is cached, least recently used first.
    pub fn display_list_pages(&self) -> Vec<u32> {
        self.display_lists.iter().map(|(page, _)| *page).collect()
    }

    /// Resolves a thumbnail of a page, about `width_px` wide, without touching the display-list
    /// cache (Plan 0003, §4.3).
    pub(crate) fn prepare_thumb(
        &mut self,
        page_index: u32,
        width_px: u32,
    ) -> Result<Prepared, MupdfError> {
        let bounds = self.page_size(page_index as usize)?;
        let geometry = PageGeometry::new(
            page_index,
            bounds,
            thumb_scale_milli(bounds.width, width_px),
        );
        self.prepare_with(&geometry, false)
    }

    /// Renders a thumbnail on the calling thread: what the pool does for `RenderThumb`, for tests.
    pub fn render_thumb(&mut self, page_index: u32, width_px: u32) -> Result<Bitmap, MupdfError> {
        let prepared = self.prepare_thumb(page_index, width_px)?;
        pool::run_whole_page(&prepared)
    }

    /// The document's outline, flattened depth-first in reading order (Plan 0003, §4.1). A
    /// document without one, or one MuPDF cannot read, gives an empty list rather than an error:
    /// the outline is an aid, and the document is still readable without it.
    pub fn outline(&self) -> Vec<OutlineItem> {
        let Some(doc) = self.doc.as_ref() else {
            return Vec::new();
        };
        let Ok(tree) = doc.outlines() else {
            return Vec::new();
        };
        let mut items = Vec::new();
        flatten_outline(&tree, 0, &mut items);
        items
    }

    /// Returns the cached display list for a page, building it on a miss and evicting the least
    /// recently used one beyond [`DISPLAY_LIST_CACHE_PAGES`].
    ///
    /// Display lists are scale independent, so one list serves every zoom level of a page and a
    /// zoom step re-interprets the page content not at all. The list comes back as an `Arc` because
    /// several rasterizing threads run the *same* list at once (Plan 0001, §5 Phase 2;
    /// `DisplayList` is `Send + Sync`).
    fn display_list_for(
        &mut self,
        page_index: u32,
        page: &mupdf::Page,
    ) -> Result<Arc<DisplayList>, MupdfError> {
        if let Some(at) = self
            .display_lists
            .iter()
            .position(|(cached, _)| *cached == page_index)
        {
            let entry = self.display_lists.remove(at);
            let list = Arc::clone(&entry.1);
            self.display_lists.push(entry);
            return Ok(list);
        }
        let list = Arc::new(page.to_display_list(DRAW_ANNOTATIONS).map_err(|e| {
            MupdfError::RenderFailed {
                page_index: page_index as usize,
                source: e,
            }
        })?);
        if self.display_lists.len() >= DISPLAY_LIST_CACHE_PAGES {
            self.display_lists.remove(0);
        }
        self.display_lists.push((page_index, Arc::clone(&list)));
        Ok(list)
    }
}

/// Depth-first, so the list reads in the order the tree shows its entries.
fn flatten_outline(nodes: &[mupdf::Outline], depth: u32, out: &mut Vec<OutlineItem>) {
    for node in nodes {
        let (page, top) = match node.dest {
            Some(dest) => (Some(dest.loc.page_number), destination_top(dest.kind)),
            None => (None, None),
        };
        out.push(OutlineItem {
            depth,
            title: node.title.clone(),
            page,
            top,
        });
        flatten_outline(&node.down, depth + 1, out);
    }
}

/// The vertical position a destination names, in MuPDF's page space (points from the page's top).
fn destination_top(kind: mupdf::DestinationKind) -> Option<f32> {
    use mupdf::DestinationKind as Kind;
    match kind {
        Kind::FitH { top } | Kind::FitBH { top } | Kind::XYZ { top, .. } => top,
        Kind::FitR { top, bottom, .. } => Some(top.min(bottom)),
        _ => None,
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
        // alpha = false produces 3 channels: opaque RGB, cleared to white (the page's paper), so
        // no byte is spent on a channel that would be 255 everywhere.
        let pixmap = page
            .to_pixmap(&ctm, &cs, false, DRAW_ANNOTATIONS)
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

        Ok(Bitmap::new(width, height, stride, PixelFormat::Rgb8, data))
    }
}

/// Actor handle for the background MuPDF engine thread.
///
/// Sends commands to a single dedicated thread owning the MuPDF document, which resolves them into
/// jobs for the render worker pool ([`Pool`]) and forwards the pool's events.
/// Upon dropping, sends [`EngineCmd::Shutdown`] and joins the worker thread,
/// preventing thread leaks.
pub struct EngineHandle {
    cmd_tx: Sender<EngineCmd>,
    worker_thread: Option<JoinHandle<()>>,
}

impl EngineHandle {
    /// Spawns a new dedicated engine actor thread and its render worker pool.
    ///
    /// The returned handle provides command sending, and events from the engine
    /// are received on the returned `Receiver<EngineEvent>`.
    pub fn spawn() -> (Self, Receiver<EngineEvent>) {
        let (cmd_tx, cmd_rx) = mpsc::channel::<EngineCmd>();
        let (event_tx, event_rx) = mpsc::channel::<EngineEvent>();

        let worker_thread = thread::Builder::new()
            .name("mupdf-engine-actor".to_string())
            .spawn(move || {
                let mut engine = MupdfEngine::new();
                // One core is left for the UI thread; `FA_PDF_WORKERS` overrides the count, and a
                // count of one gives a single-worker pool, i.e. serial rasterization.
                let pool = Pool::start(worker_count(), event_tx.clone());
                // Search runs beside the pool, on a document of its own (Plan 0003, §3.6).
                let search = SearchThread::start(event_tx.clone());

                while let Ok(cmd) = cmd_rx.recv() {
                    match cmd {
                        EngineCmd::Open { path, password } => {
                            match engine.open_document_with_password(&path, password.as_deref()) {
                                Ok(page_count) => {
                                    search.document(path.clone(), password.clone());
                                    let (page_sizes, unsized_pages) = engine.page_sizes_lenient();
                                    let _ = event_tx.send(EngineEvent::Opened {
                                        page_count,
                                        page_sizes,
                                        unsized_pages,
                                    });
                                    let _ = event_tx.send(EngineEvent::Outline {
                                        items: engine.outline(),
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
                            // nothing to coalesce: each command is one tile, resolved here and
                            // rasterized by the pool. A superseded epoch costs no raster time.
                            match engine.prepare(&geometry) {
                                Ok(prepared) => pool.dispatch(Job::Tile {
                                    prepared,
                                    col,
                                    row,
                                    request_id,
                                }),
                                Err(err) => Self::report(&event_tx, &err),
                            }
                        }
                        EngineCmd::RenderBase {
                            page,
                            scale_milli,
                            request_id,
                        } => {
                            // The base layer is queued *ahead* of the tiles: at a cold open it is
                            // the only job that can put a whole page on screen, and watching it
                            // appear beats watching a grid fill in.
                            match engine.prepare_base(page, scale_milli) {
                                Ok(prepared) => pool.dispatch_first(Job::Base {
                                    prepared,
                                    request_id,
                                }),
                                Err(err) => Self::report(&event_tx, &err),
                            }
                        }
                        EngineCmd::RenderThumb {
                            page,
                            width_px,
                            request_id,
                        } => {
                            // Behind the tiles already queued: the viewport comes first.
                            match engine.prepare_thumb(page, width_px) {
                                Ok(prepared) => pool.dispatch(Job::Thumb {
                                    prepared,
                                    request_id,
                                }),
                                Err(err) => Self::report(&event_tx, &err),
                            }
                        }
                        EngineCmd::Search {
                            query,
                            from_page,
                            generation,
                        } => search.search(query, from_page, generation),
                        EngineCmd::CancelSearch => search.cancel(),
                        EngineCmd::TextLines { page, generation } => {
                            search.text_lines(page, generation)
                        }
                        EngineCmd::Select {
                            from,
                            to,
                            mode,
                            generation,
                        } => search.select(from, to, mode, generation),
                        EngineCmd::Cancel { request_id } => pool.cancel(request_id),
                        EngineCmd::Shutdown => break,
                    }
                }

                // Dropping the pool and the search thread stops and joins them before the actor
                // thread exits.
                drop(search);
                drop(pool);
            })
            .expect("Failed to spawn engine actor thread");

        let handle = Self {
            cmd_tx,
            worker_thread: Some(worker_thread),
        };

        (handle, event_rx)
    }

    /// Reports an engine failure to the UI thread.
    fn report(events: &Sender<EngineEvent>, error: &MupdfError) {
        let _ = events.send(EngineEvent::Error {
            message: error.to_string(),
        });
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
    ///
    /// The pool drops queued work of that epoch and aborts a tile that is already rasterizing, so
    /// cancellation is a property of the pool rather than of the actor's command loop.
    pub fn cancel(&self, request_id: RequestId) {
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
