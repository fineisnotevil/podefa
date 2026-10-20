// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! The search thread (Plan 0003, §3.6; ADR 0005).
//!
//! Search costs ~1.5 ms a page (`docs/benchmarks.md` §9.1), seconds for a long document, so it cannot
//! run on the actor, which prepares every tile. It runs here instead, on a `Document` of its own,
//! opened on the same file the first time a search asks for it; MuPDF gives every thread its own
//! context, and a second document is the supported way to read one file from two threads.
//!
//! The document is opened, and its first page's text extracted, as soon as the actor opens a new
//! document - not on the first search - because MuPDF's first text extraction of a document costs
//! about a second (the one-off of `docs/benchmarks.md` §9.1); paying it while the reader is still
//! looking at the first page keeps it out of the first search's `t_first_hit` (§9.6).
//!
//! A search walks the pages from the reader's page onwards, wrapping, and sends each page's hits as
//! soon as they are found. Cancelling is a shared generation counter: the actor stores the new
//! generation *before* it forwards a search, and the loop compares it before every page, so a
//! superseded search stops within one page's text extraction and never sends another hit.

use mupdf::text_page::SearchHitResponse;
use mupdf::{Document, Quad, TextPageFlags};
use pdf_core::{EngineEvent, Rect};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Instant;

/// What the actor asks of the search thread.
enum SearchCmd {
    /// A new document: forget the old one, open this one and warm it up.
    Document {
        path: PathBuf,
        password: Option<String>,
    },
    Search {
        query: String,
        from_page: u32,
        generation: u64,
    },
}

/// The handle the actor keeps. Dropping it stops and joins the thread.
pub(crate) struct SearchThread {
    tx: Option<Sender<SearchCmd>>,
    current: Arc<AtomicU64>,
    handle: Option<JoinHandle<()>>,
}

impl SearchThread {
    /// Starts the thread; it idles until a document and a search arrive.
    pub(crate) fn start(events: Sender<EngineEvent>) -> Self {
        let (tx, rx) = mpsc::channel();
        let current = Arc::new(AtomicU64::new(0));
        let shared = Arc::clone(&current);
        let handle = thread::Builder::new()
            .name("mupdf-search".to_string())
            .spawn(move || serve(rx, shared, events))
            .ok();
        Self {
            tx: Some(tx),
            current,
            handle,
        }
    }

    /// The actor opened a new document; any search of the old one stops.
    pub(crate) fn document(&self, path: PathBuf, password: Option<String>) {
        self.current.store(0, Ordering::SeqCst);
        if let Some(tx) = &self.tx {
            let _ = tx.send(SearchCmd::Document { path, password });
        }
    }

    /// Starts a search; the one before it stops at its next page.
    pub(crate) fn search(&self, query: String, from_page: u32, generation: u64) {
        self.current.store(generation, Ordering::SeqCst);
        if let Some(tx) = &self.tx {
            let _ = tx.send(SearchCmd::Search {
                query,
                from_page,
                generation,
            });
        }
    }

    /// Stops the running search without starting another.
    pub(crate) fn cancel(&self) {
        self.current.store(0, Ordering::SeqCst);
    }
}

impl Drop for SearchThread {
    fn drop(&mut self) {
        self.current.store(0, Ordering::SeqCst);
        // Closing the channel ends the thread's loop.
        self.tx = None;
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn serve(rx: Receiver<SearchCmd>, current: Arc<AtomicU64>, events: Sender<EngineEvent>) {
    let mut source: Option<(PathBuf, Option<String>)> = None;
    let mut doc: Option<Document> = None;
    while let Ok(cmd) = rx.recv() {
        match cmd {
            SearchCmd::Document { path, password } => {
                doc = open(&path, &password);
                source = Some((path, password));
                if let Some(doc) = &doc {
                    warm_up(doc);
                }
            }
            SearchCmd::Search {
                query,
                from_page,
                generation,
            } => {
                if current.load(Ordering::SeqCst) != generation {
                    continue;
                }
                if doc.is_none() {
                    doc = source
                        .as_ref()
                        .and_then(|(path, password)| open(path, password));
                }
                match &doc {
                    Some(doc) => run(doc, &query, from_page, generation, &current, &events),
                    // Nothing to search: report an empty, finished search rather than nothing.
                    None => {
                        let _ = events.send(EngineEvent::SearchDone {
                            generation,
                            pages: 0,
                            ms: 0,
                        });
                    }
                }
            }
        }
    }
}

/// Extracts the first page's text and drops it: the document's one-off first-touch cost, paid
/// before anyone searches.
fn warm_up(doc: &Document) {
    if let Ok(page) = doc.load_page(0) {
        let _ = page.to_text_page(TextPageFlags::empty());
    }
}

fn open(path: &std::path::Path, password: &Option<String>) -> Option<Document> {
    let mut doc = Document::open(path.to_str()?).ok()?;
    if doc.needs_password().unwrap_or(false) {
        let authed = password
            .as_deref()
            .is_some_and(|pwd| doc.authenticate(pwd).unwrap_or(false));
        if !authed {
            return None;
        }
    }
    Some(doc)
}

/// One search: every page from `from_page`, wrapping, until done or superseded.
fn run(
    doc: &Document,
    query: &str,
    from_page: u32,
    generation: u64,
    current: &AtomicU64,
    events: &Sender<EngineEvent>,
) {
    let started = Instant::now();
    let count = doc.page_count().unwrap_or(0).max(0) as u32;
    for k in 0..count {
        if current.load(Ordering::SeqCst) != generation {
            return;
        }
        let page_index = (from_page.min(count.saturating_sub(1)) + k) % count;
        let hits = page_hits(doc, page_index, query);
        if !hits.is_empty() {
            let _ = events.send(EngineEvent::SearchHits {
                generation,
                page: page_index,
                hits,
            });
        }
    }
    if current.load(Ordering::SeqCst) == generation {
        let _ = events.send(EngineEvent::SearchDone {
            generation,
            pages: count,
            ms: started.elapsed().as_millis() as u64,
        });
    }
}

/// One page's hits, each the rectangles of its quads. A page that fails to load or to extract
/// simply has no hits: one damaged page must not end the search.
fn page_hits(doc: &Document, page_index: u32, query: &str) -> Vec<Vec<Rect>> {
    let Ok(page) = doc.load_page(page_index as i32) else {
        return Vec::new();
    };
    let Ok(text) = page.to_text_page(TextPageFlags::empty()) else {
        return Vec::new();
    };
    let mut hits: Vec<Vec<Rect>> = Vec::new();
    let _ = text.search_cb(query, &mut hits, |hits, quads| {
        hits.push(quads.iter().map(quad_rect).collect());
        SearchHitResponse::ContinueSearch
    });
    hits
}

/// The axis-aligned box around a quad, in page space.
fn quad_rect(quad: &Quad) -> Rect {
    let xs = [quad.ul.x, quad.ur.x, quad.ll.x, quad.lr.x];
    let ys = [quad.ul.y, quad.ur.y, quad.ll.y, quad.lr.y];
    let (x0, x1) = (
        xs.iter().copied().fold(f32::INFINITY, f32::min),
        xs.iter().copied().fold(f32::NEG_INFINITY, f32::max),
    );
    let (y0, y1) = (
        ys.iter().copied().fold(f32::INFINITY, f32::min),
        ys.iter().copied().fold(f32::NEG_INFINITY, f32::max),
    );
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}
