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
//!
//! Selection (Plan 0003, §4.5) is answered here too, because it needs the same text pages: a
//! selection request is served between two pages of a running search rather than after it, only the
//! latest of a burst (a drag) is computed, and the character lists of the last pages used are kept.

use mupdf::text_page::SearchHitResponse;
use mupdf::{Document, Quad, TextPageFlags};
use pdf_core::select::{self, PagePoint, SelChar, SelectMode};
use pdf_core::{EngineEvent, Rect};
use std::collections::{HashMap, VecDeque};
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
    Select {
        from: PagePoint,
        to: PagePoint,
        mode: SelectMode,
        generation: u64,
    },
    Lines {
        page: u32,
        generation: u64,
    },
}

/// Pages whose character lists are kept for selection: a drag recomputes on every move.
const CHAR_CACHE_PAGES: usize = 16;

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

    /// Asks for a selection; answered ahead of a running search.
    pub(crate) fn select(&self, from: PagePoint, to: PagePoint, mode: SelectMode, generation: u64) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(SearchCmd::Select {
                from,
                to,
                mode,
                generation,
            });
        }
    }

    /// Asks for a page's text line boxes; answered ahead of a running search.
    pub(crate) fn text_lines(&self, page: u32, generation: u64) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(SearchCmd::Lines { page, generation });
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

/// The thread's state between commands.
struct Searcher {
    rx: Receiver<SearchCmd>,
    /// Commands taken off the channel while a search was running, to run after it.
    pending: VecDeque<SearchCmd>,
    current: Arc<AtomicU64>,
    events: Sender<EngineEvent>,
    source: Option<(PathBuf, Option<String>)>,
    doc: Option<Document>,
    /// Character lists of recently selected pages, with the tick they were last used.
    chars: HashMap<u32, (Vec<SelChar>, u64)>,
    tick: u64,
}

fn serve(rx: Receiver<SearchCmd>, current: Arc<AtomicU64>, events: Sender<EngineEvent>) {
    let mut searcher = Searcher {
        rx,
        pending: VecDeque::new(),
        current,
        events,
        source: None,
        doc: None,
        chars: HashMap::new(),
        tick: 0,
    };
    loop {
        let cmd = match searcher.pending.pop_front() {
            Some(cmd) => cmd,
            None => match searcher.rx.recv() {
                Ok(cmd) => cmd,
                Err(_) => return,
            },
        };
        searcher.handle(cmd);
    }
}

impl Searcher {
    fn handle(&mut self, cmd: SearchCmd) {
        match cmd {
            SearchCmd::Document { path, password } => {
                self.doc = open(&path, &password);
                self.source = Some((path, password));
                self.chars.clear();
                if let Some(doc) = &self.doc {
                    warm_up(doc);
                }
            }
            SearchCmd::Search {
                query,
                from_page,
                generation,
            } => {
                if self.current.load(Ordering::SeqCst) != generation {
                    return;
                }
                if self.doc.is_none() {
                    self.doc = self
                        .source
                        .as_ref()
                        .and_then(|(path, password)| open(path, password));
                }
                if self.doc.is_some() {
                    self.run(&query, from_page, generation);
                } else {
                    // Nothing to search: report an empty, finished search rather than nothing.
                    let _ = self.events.send(EngineEvent::SearchDone {
                        generation,
                        pages: 0,
                        ms: 0,
                    });
                }
            }
            select @ SearchCmd::Select { .. } => self.select_latest(select),
            SearchCmd::Lines { page, generation } => self.send_lines(page, generation),
        }
    }

    /// A page's text line boxes, from the same character cache selection uses.
    fn send_lines(&mut self, page: u32, generation: u64) {
        let lines = select::line_boxes(&self.page_chars(page));
        let _ = self.events.send(EngineEvent::TextLines {
            page,
            generation,
            lines,
        });
    }

    /// Serves a selection, skipping to the newest one already waiting: during a drag only the
    /// last position matters. Other commands met on the way are kept for later, in order.
    fn select_latest(&mut self, mut latest: SearchCmd) {
        while let Ok(cmd) = self.rx.try_recv() {
            match cmd {
                newer @ SearchCmd::Select { .. } if self.pending.is_empty() => latest = newer,
                SearchCmd::Lines { page, generation } if self.pending.is_empty() => {
                    self.send_lines(page, generation)
                }
                // Once a new document is waiting, everything after it waits too: it is about
                // that document, not the one still open here.
                other => self.pending.push_back(other),
            }
        }
        let SearchCmd::Select {
            from,
            to,
            mode,
            generation,
        } = latest
        else {
            return;
        };
        let (pages, text) = select::select(from, to, mode, |page| self.page_chars(page));
        let _ = self.events.send(EngineEvent::Selection {
            generation,
            pages,
            text,
        });
    }

    /// A page's characters, from the cache or extracted now.
    fn page_chars(&mut self, page: u32) -> Vec<SelChar> {
        self.tick += 1;
        let tick = self.tick;
        if let Some((chars, used)) = self.chars.get_mut(&page) {
            *used = tick;
            return chars.clone();
        }
        let chars = self
            .doc
            .as_ref()
            .map(|doc| extract_chars(doc, page))
            .unwrap_or_default();
        if self.chars.len() >= CHAR_CACHE_PAGES
            && let Some((&oldest, _)) = self.chars.iter().min_by_key(|(p, (_, used))| (*used, **p))
        {
            self.chars.remove(&oldest);
        }
        self.chars.insert(page, (chars.clone(), tick));
        chars
    }

    /// One search: every page from `from_page`, wrapping, until done or superseded. Between pages
    /// it answers waiting selections, so a drag never waits for a long search.
    fn run(&mut self, query: &str, from_page: u32, generation: u64) {
        let started = Instant::now();
        let count = self
            .doc
            .as_ref()
            .and_then(|doc| doc.page_count().ok())
            .unwrap_or(0)
            .max(0) as u32;
        for k in 0..count {
            if self.current.load(Ordering::SeqCst) != generation {
                return;
            }
            while let Ok(cmd) = self.rx.try_recv() {
                match cmd {
                    select @ SearchCmd::Select { .. } if self.pending.is_empty() => {
                        self.select_latest(select)
                    }
                    SearchCmd::Lines { page, generation } if self.pending.is_empty() => {
                        self.send_lines(page, generation)
                    }
                    other => self.pending.push_back(other),
                }
            }
            let page_index = (from_page.min(count.saturating_sub(1)) + k) % count;
            let hits = match &self.doc {
                Some(doc) => page_hits(doc, page_index, query),
                None => return,
            };
            if !hits.is_empty() {
                let _ = self.events.send(EngineEvent::SearchHits {
                    generation,
                    page: page_index,
                    hits,
                });
            }
        }
        if self.current.load(Ordering::SeqCst) == generation {
            let _ = self.events.send(EngineEvent::SearchDone {
                generation,
                pages: count,
                ms: started.elapsed().as_millis() as u64,
            });
        }
    }
}

/// A page's characters in reading order, each with its box in page space and its line, for
/// selection. A page that fails to load or extract has none.
fn extract_chars(doc: &Document, page_index: u32) -> Vec<SelChar> {
    let Ok(page) = doc.load_page(page_index as i32) else {
        return Vec::new();
    };
    let Ok(text) = page.to_text_page(TextPageFlags::empty()) else {
        return Vec::new();
    };
    let mut chars = Vec::new();
    let mut line_no = 0u32;
    for block in text.blocks() {
        for line in block.lines() {
            for c in line.chars() {
                if let Some(ch) = c.char() {
                    chars.push(SelChar {
                        ch,
                        rect: quad_rect(&c.quad()),
                        line: line_no,
                    });
                }
            }
            line_no += 1;
        }
    }
    chars
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
