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

mod edit;
mod fonts;
mod pool;
mod search;

use mupdf::pdf::{PdfDocument, PdfWriteOptions};
use mupdf::{Colorspace, DisplayList, Document, Matrix, Pixmap};
use pdf_core::annot::{AnnotInfo, EditOp};
use pdf_core::pages::{self, PageMap, PageOp};
use pdf_core::{
    Bitmap, EngineCmd, EngineEvent, HistoryChange, OutlineItem, PageGeometry, PageSize, PdfEngine,
    PixelFormat, Rect, RequestId, SaveError, TILE_STRIDE_PX, thumb_scale_milli,
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

    /// A structural edit that cannot be applied to this document (Plan 0005).
    #[error("{0}")]
    PageOp(#[from] pdf_core::pages::PageOpError),

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

    /// The document cannot be annotated or saved: MuPDF opened it, but it is not a PDF.
    #[error("Only PDF documents can be edited")]
    NotEditable,

    /// An edit named an annotation the page does not have (it was deleted meanwhile).
    #[error("Page {page} has no annotation {id}")]
    AnnotationNotFound { page: u32, id: u32 },

    /// MuPDF refused an edit.
    #[error("Failed to edit page {page}: {source}")]
    EditFailed { page: u32, source: mupdf::Error },

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
///
/// A PDF is held as a [`PdfDocument`], which can be annotated and saved; any other format MuPDF
/// opens (images, EPUB, ...) as a plain [`Document`], which is view-only (Plan 0004, §1).
/// `revisions` counts the changes of each page (Plan 0004, §4.1) - edits, undos and redos: every
/// job carries the revision of the display list it was given, so the app can tell pixels drawn
/// before a change. `history` is what each step of MuPDF's undo journal changed, in journal order
/// (ADR 0008): the journal knows how to undo a step, this knows what to redraw - one page, or the
/// page order (Plan 0005).
pub struct MupdfEngine {
    doc: Option<Doc>,
    path: Option<PathBuf>,
    password: Option<String>,
    scratch: Option<Pixmap>,
    display_lists: Vec<(u32, Arc<DisplayList>)>,
    revisions: Vec<u32>,
    history: Vec<Step>,
}

/// What one step of the undo history changed.
#[derive(Debug, Clone, PartialEq)]
enum Step {
    /// One page's content: annotations, its text or position.
    Page(u32),
    /// The pages themselves: `forward` is the step's page map, from a document of `old_count`
    /// pages.
    Pages { forward: PageMap, old_count: u32 },
}

/// What an edit, an undo or a redo changed, for the actor to report.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// One page, now at `revision`.
    Page { page: u32, revision: u32 },
    /// The pages (Plan 0005, §4.1): their sizes now, the revision every one of them is at, and
    /// where each came from.
    Pages {
        page_sizes: Vec<PageSize>,
        revision: u32,
        map: PageMap,
    },
}

/// The open document, editable or not.
enum Doc {
    Plain(Document),
    Pdf(PdfDocument),
}

impl Doc {
    fn document(&self) -> &Document {
        match self {
            Doc::Plain(doc) => doc,
            Doc::Pdf(pdf) => pdf,
        }
    }
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
        fonts::install();
        Self {
            doc: None,
            path: None,
            password: None,
            scratch: None,
            display_lists: Vec::new(),
            revisions: Vec::new(),
            history: Vec::new(),
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

        let doc = if doc.is_pdf() {
            let mut pdf = PdfDocument::try_from(doc).map_err(|source| MupdfError::OpenFailed {
                path: path.to_path_buf(),
                source,
            })?;
            // Undo and redo (ADR 0008). Without a journal the document still opens and edits; it
            // only cannot be undone, which `history_state` reports. `FA_PDF_NO_JOURNAL` leaves it
            // off, so a bench run can measure the journal's share of memory (Plan 0004 row X).
            if std::env::var_os("FA_PDF_NO_JOURNAL").is_none() {
                let _ = pdf.enable_journal();
            }
            Doc::Pdf(pdf)
        } else {
            Doc::Plain(doc)
        };
        self.doc = Some(doc);
        self.path = Some(path.to_path_buf());
        self.password = password.map(str::to_owned);
        // A display list belongs to the document that produced it, and so do the revisions and the
        // history.
        self.display_lists.clear();
        self.revisions = vec![0; count];
        self.history.clear();

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
        let doc = self.document().ok_or(MupdfError::NoDocumentOpen)?;
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
            revision: self.revision(page_index),
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
        let Some(doc) = self.document() else {
            return Vec::new();
        };
        let Ok(tree) = doc.outlines() else {
            return Vec::new();
        };
        let mut items = Vec::new();
        flatten_outline(&tree, 0, &mut items);
        items
    }

    fn document(&self) -> Option<&Document> {
        self.doc.as_ref().map(Doc::document)
    }

    fn pdf(&self) -> Result<&PdfDocument, MupdfError> {
        match self.doc.as_ref() {
            Some(Doc::Pdf(pdf)) => Ok(pdf),
            Some(Doc::Plain(_)) => Err(MupdfError::NotEditable),
            None => Err(MupdfError::NoDocumentOpen),
        }
    }

    fn pdf_mut(&mut self) -> Result<&mut PdfDocument, MupdfError> {
        match self.doc.as_mut() {
            Some(Doc::Pdf(pdf)) => Ok(pdf),
            Some(Doc::Plain(_)) => Err(MupdfError::NotEditable),
            None => Err(MupdfError::NoDocumentOpen),
        }
    }

    /// Whether the open document can be annotated and saved.
    pub fn is_editable(&self) -> bool {
        matches!(self.doc, Some(Doc::Pdf(_)))
    }

    /// How many edits a page has had since the document was opened.
    pub fn revision(&self, page: u32) -> u32 {
        self.revisions.get(page as usize).copied().unwrap_or(0)
    }

    /// A page's annotations, in drawing order; empty for a view-only document.
    pub fn annotations(&self, page: u32) -> Result<Vec<AnnotInfo>, MupdfError> {
        match self.pdf() {
            Ok(pdf) => edit::annotations(pdf, page),
            Err(MupdfError::NotEditable) => Ok(Vec::new()),
            Err(err) => Err(err),
        }
    }

    /// Applies one edit (Plan 0004, §4.1) and gives the page a new revision, which it returns.
    /// The page's cached display list is dropped, so its next job is drawn from the edited page;
    /// lists already handed to workers keep drawing the old revision, which their jobs say.
    ///
    /// The edit is one operation of MuPDF's undo journal (ADR 0008): everything it changes -
    /// cutting old markup included - is undone by one undo, and an edit that fails is rolled back
    /// whole (`pdf_abandon_operation`).
    pub fn edit(&mut self, op: &EditOp) -> Result<Change, MupdfError> {
        if let EditOp::Pages(pages) = op {
            return self.restructure(pages);
        }
        let page = op.page();
        if page as usize >= self.revisions.len() {
            return Err(MupdfError::PageOutOfBounds {
                page_index: page as usize,
                total_pages: self.revisions.len(),
            });
        }
        let pdf = self.pdf()?;
        let journal = |source| MupdfError::EditFailed { page, source };
        let before = journal_position(pdf);
        pdf.begin_operation(op.title()).map_err(journal)?;
        if let Err(err) = edit::apply(pdf, op) {
            let _ = pdf.abandon_operation();
            return Err(err);
        }
        pdf.end_operation().map_err(journal)?;
        // An edit that changed nothing adds no journal step; one that did drops any redo steps.
        let after = journal_position(pdf);
        if after > before {
            self.history.truncate(before);
            self.history.push(Step::Page(page));
        }
        Ok(Change::Page {
            page,
            revision: self.touch(page),
        })
    }

    /// Applies a structural edit (Plan 0005, §4.1) as one journal operation. Every page gets a new
    /// revision, past every revision before, so no tile drawn before the edit - whatever page index
    /// it had - can pass for one drawn after it.
    fn restructure(&mut self, op: &PageOp) -> Result<Change, MupdfError> {
        let count = self.revisions.len() as u32;
        let map = pages::plan(op, count)?;
        let pdf = self.pdf_mut()?;
        // An edit that leaves every page as it was (a move to where the pages are, a full turn)
        // changes nothing: MuPDF's journal would record no step for it, and neither does this.
        if map.len() == count as usize && map.iter().enumerate().all(|(i, o)| *o == Some(i as u32))
        {
            return Ok(self.restructured(map));
        }
        let journal = |source| MupdfError::EditFailed {
            page: op.first_page(),
            source,
        };
        let before = journal_position(pdf);
        pdf.begin_operation(op.title()).map_err(journal)?;
        let applied = edit::apply_pages(pdf, op, count).and_then(|()| {
            let now = pdf.page_count().map_err(journal)?.max(0) as usize;
            if now == map.len() {
                Ok(())
            } else {
                Err(MupdfError::Backend(format!(
                    "{}: {now} pages, {} expected",
                    op.title(),
                    map.len()
                )))
            }
        });
        if let Err(err) = applied {
            let _ = pdf.abandon_operation();
            return Err(err);
        }
        pdf.end_operation().map_err(journal)?;
        let after = journal_position(pdf);
        if after > before {
            self.history.truncate(before);
            self.history.push(Step::Pages {
                forward: map.clone(),
                old_count: count,
            });
        }
        Ok(self.restructured(map))
    }

    /// The pages changed: every display list goes, every page gets the same new revision, and the
    /// new sizes are read.
    fn restructured(&mut self, map: PageMap) -> Change {
        let revision = self.revisions.iter().copied().max().unwrap_or(0) + 1;
        self.display_lists.clear();
        let (page_sizes, _) = self.page_sizes_lenient();
        self.revisions = vec![revision; page_sizes.len()];
        Change::Pages {
            page_sizes,
            revision,
            map,
        }
    }

    /// What one history step changed, undone (`undo`) or redone, as a change to report.
    fn replay(&mut self, step: Step, undo: bool) -> Change {
        match step {
            Step::Page(page) => Change::Page {
                page,
                revision: self.touch(page),
            },
            Step::Pages { forward, old_count } => {
                let map = if undo {
                    pages::invert(&forward, old_count)
                } else {
                    forward
                };
                self.restructured(map)
            }
        }
    }

    /// The document as it is now, unsaved changes included, as PDF bytes: what the search thread
    /// reads after a structural edit. Written without garbage collection, which would break the
    /// undo journal (Plan 0005, §3.4).
    pub fn snapshot(&self) -> Result<Vec<u8>, MupdfError> {
        let pdf = self.pdf()?;
        let mut bytes = Vec::new();
        pdf.write_to(&mut bytes)
            .map_err(|e| MupdfError::Backend(format!("snapshot: {e}")))?;
        Ok(bytes)
    }

    /// Undoes the last edit; returns what it changed, or `None` when there was nothing to undo.
    pub fn undo(&mut self) -> Result<Option<Change>, MupdfError> {
        let pdf = self.pdf_mut()?;
        let position = journal_position(pdf);
        if position == 0 {
            return Ok(None);
        }
        pdf.undo()
            .map_err(|e| MupdfError::Backend(format!("undo: {e}")))?;
        Ok(self
            .history
            .get(position - 1)
            .cloned()
            .map(|step| self.replay(step, true)))
    }

    /// Redoes the last undone edit, like [`Self::undo`].
    pub fn redo(&mut self) -> Result<Option<Change>, MupdfError> {
        let pdf = self.pdf_mut()?;
        let (position, steps) = pdf.undo_redo_state().unwrap_or((0, 0));
        if position >= steps {
            return Ok(None);
        }
        pdf.redo()
            .map_err(|e| MupdfError::Backend(format!("redo: {e}")))?;
        Ok(self
            .history
            .get(position as usize)
            .cloned()
            .map(|step| self.replay(step, false)))
    }

    /// `(can_undo, can_redo)`: both false for a document without a journal.
    pub fn history_state(&self) -> (bool, bool) {
        match self.pdf() {
            Ok(pdf) => {
                let (position, steps) = pdf.undo_redo_state().unwrap_or((0, 0));
                (position > 0, position < steps)
            }
            Err(_) => (false, false),
        }
    }

    /// A page changed: drop its display list and give it a new revision, which is returned.
    fn touch(&mut self, page: u32) -> u32 {
        self.display_lists.retain(|(cached, _)| *cached != page);
        match self.revisions.get_mut(page as usize) {
            Some(revision) => {
                *revision += 1;
                *revision
            }
            None => 0,
        }
    }

    /// Writes the document (Plan 0004, §3.3 and §4.4) and returns the file and its size.
    ///
    /// `None` appends an incremental update to the open file: the bytes already there stay as
    /// they are, existing signatures stay valid, and the open document keeps reading the file.
    /// `Some(path)` writes a full copy there and reopens it, so later saves go to the new file. A
    /// path naming the open file itself is saved incrementally instead, because a full rewrite of
    /// the file MuPDF is reading from would destroy what it reads.
    pub fn save(&mut self, path: Option<&Path>) -> Result<(PathBuf, u64), SaveError> {
        let current = self.path.clone().ok_or(SaveError::NotEditable)?;
        let target = path.map(Path::to_path_buf);
        let in_place = target
            .as_deref()
            .is_none_or(|target| same_file(target, &current));
        let pdf = self.pdf().map_err(|_| SaveError::NotEditable)?;
        if in_place {
            if !pdf.can_be_saved_incrementally() {
                return Err(SaveError::NeedsSaveAs);
            }
            let name = current.to_str().ok_or(SaveError::NeedsSaveAs)?;
            let mut options = PdfWriteOptions::default();
            options.set_incremental(true);
            pdf.save_with_options(name, options)
                .map_err(|e| SaveError::Failed(e.to_string()))?;
            let len = file_len(&current);
            return Ok((current, len));
        }
        let target = target.ok_or(SaveError::NeedsSaveAs)?;
        let name = target
            .to_str()
            .ok_or_else(|| SaveError::Failed(format!("not a UTF-8 path: {}", target.display())))?;
        pdf.save(name)
            .map_err(|e| SaveError::Failed(e.to_string()))?;
        // The copy is the document from now on. Its pages are the edited pages, so they keep their
        // revisions: a tile drawn before the switch then never passes for one drawn after a
        // later edit.
        let password = self.password.clone();
        let revisions = std::mem::take(&mut self.revisions);
        self.open_document_with_password(&target, password.as_deref())
            .map_err(|e| SaveError::Failed(e.to_string()))?;
        if revisions.len() == self.revisions.len() {
            self.revisions = revisions;
        }
        let len = file_len(&target);
        Ok((target, len))
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

/// How many steps of the undo journal are applied (0 without a journal).
fn journal_position(pdf: &PdfDocument) -> usize {
    pdf.undo_redo_state()
        .map_or(0, |(position, _)| position as usize)
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Whether two paths name the same file, by their canonical form when both exist.
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
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
        self.document()
            .and_then(|d| d.page_count().ok())
            .map(|c| c.max(0) as usize)
            .unwrap_or(0)
    }

    fn page_size(&self, page_index: usize) -> Result<Rect, Self::Error> {
        let doc = self.document().ok_or(MupdfError::NoDocumentOpen)?;
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
        let doc = self.document().ok_or(MupdfError::NoDocumentOpen)?;
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
                                        editable: engine.is_editable(),
                                    });
                                    let _ = event_tx.send(EngineEvent::Outline {
                                        items: engine.outline(),
                                    });
                                    Self::history(&event_tx, &engine, 0, HistoryChange::None);
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
                        EngineCmd::Annotations { page, generation } => {
                            match engine.annotations(page) {
                                Ok(items) => {
                                    let _ = event_tx.send(EngineEvent::Annotations {
                                        page,
                                        generation,
                                        items,
                                    });
                                }
                                Err(err) => Self::report(&event_tx, &err),
                            }
                        }
                        EngineCmd::Edit {
                            op,
                            edit_id,
                            generation,
                        } => {
                            // `PageChanged` goes out before any tile of the new revision can be
                            // prepared - this loop prepares nothing until it returns - so the app
                            // always knows a revision before it sees pixels of it.
                            let result = engine.edit(&op).and_then(|change| {
                                Self::changed(&event_tx, &engine, &search, generation, change)
                            });
                            let applied = result.is_ok();
                            let _ = event_tx.send(EngineEvent::Edited {
                                edit_id,
                                generation,
                                result: result.map_err(|err| err.to_string()),
                            });
                            if applied {
                                Self::history(
                                    &event_tx,
                                    &engine,
                                    generation,
                                    HistoryChange::Edited,
                                );
                            }
                        }
                        EngineCmd::Undo { generation } | EngineCmd::Redo { generation } => {
                            let undo = matches!(cmd, EngineCmd::Undo { .. });
                            let moved = if undo { engine.undo() } else { engine.redo() };
                            match moved {
                                Ok(Some(change)) => {
                                    if let Err(err) = Self::changed(
                                        &event_tx, &engine, &search, generation, change,
                                    ) {
                                        Self::report(&event_tx, &err);
                                    }
                                    let change = if undo {
                                        HistoryChange::Undone
                                    } else {
                                        HistoryChange::Redone
                                    };
                                    Self::history(&event_tx, &engine, generation, change);
                                }
                                Ok(None) => Self::history(
                                    &event_tx,
                                    &engine,
                                    generation,
                                    HistoryChange::None,
                                ),
                                Err(err) => Self::report(&event_tx, &err),
                            }
                        }
                        EngineCmd::Save { path } => {
                            let result = engine.save(path.as_deref());
                            let saved_to = match &result {
                                Ok((file, _)) => file.clone(),
                                Err(_) => path.clone().unwrap_or_default(),
                            };
                            // After a Save As the search thread reads the new file too, and the
                            // history starts again with the reopened copy (ADR 0008).
                            if path.is_some() && result.is_ok() {
                                search.document(saved_to.clone(), engine.password.clone());
                            }
                            let switched = path.is_some() && result.is_ok();
                            let _ = event_tx.send(EngineEvent::Saved {
                                path: saved_to,
                                result: result.map(|(_, bytes)| bytes),
                            });
                            if switched {
                                Self::history(&event_tx, &engine, 0, HistoryChange::None);
                            }
                        }
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

    /// Reports where the undo history stands.
    /// Reports what an edit, undo or redo changed: `PageChanged` with the page's annotations, or
    /// `Restructured` (Plan 0005, §4.1), after which the search thread reads the document as it is
    /// now and the outline is sent again, its entries resolved to the new page numbers. Sent
    /// before any tile of the new revision can be prepared.
    fn changed(
        events: &Sender<EngineEvent>,
        engine: &MupdfEngine,
        search: &SearchThread,
        generation: u64,
        change: Change,
    ) -> Result<(), MupdfError> {
        match change {
            Change::Page { page, revision } => {
                let items = engine.annotations(page)?;
                let _ = events.send(EngineEvent::PageChanged {
                    generation,
                    page,
                    revision,
                    items,
                });
            }
            Change::Pages {
                page_sizes,
                revision,
                map,
            } => {
                let _ = events.send(EngineEvent::Restructured {
                    generation,
                    page_sizes,
                    revision,
                    map,
                });
                let _ = events.send(EngineEvent::Outline {
                    items: engine.outline(),
                });
                search.document_bytes(engine.snapshot()?, engine.password.clone());
            }
        }
        Ok(())
    }

    fn history(
        events: &Sender<EngineEvent>,
        engine: &MupdfEngine,
        generation: u64,
        change: HistoryChange,
    ) {
        let (can_undo, can_redo) = engine.history_state();
        let _ = events.send(EngineEvent::History {
            generation,
            change,
            can_undo,
            can_redo,
        });
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
