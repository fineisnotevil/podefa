// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Slint front end for the continuous, tiled, viewport-only renderer (Plans 0001 and 0002).
//!
//! The UI keeps no rendering policy of its own: it mirrors the `Flickable`'s viewport into Rust,
//! asks [`pdf_core::DocLayout`] which cells of which pages it shows and
//! [`pdf_core::TileScheduler`] what should be on screen, and applies the returned actions to a
//! tile model. Every viewport change - wheel scroll, drag, programmatic scroll, zoom or window
//! resize - funnels through one function, `viewport_moved`.
//!
//! The document position lives here, in `f64` document pixels, never in Slint: Slint's lengths are
//! `f32`, which cannot address a long document exactly (Plan 0002, §3.1). The `Flickable` is given
//! a [`CanvasWindow`] of the document around the viewport, and every coordinate it sees is relative
//! to that window's start.

use engine_mupdf::EngineHandle;
use pdf_core::{
    Bitmap, CANVAS_HALF_LOGICAL_PX, CanvasWindow, DocLayout, EngineCmd, EngineEvent,
    PAGE_GAP_LOGICAL_PX, PageSize, REANCHOR_MARGIN_LOGICAL_PX, TILE_SIZE_PX, TileAction, TileKey,
    TileScheduler, ZOOM_DEFAULT_MILLI, ZOOM_MAX_MILLI, ZOOM_MIN_MILLI, base_scale_milli,
    zoom_in_milli, zoom_out_milli,
};
use slint::{
    ComponentHandle, Image, Model, ModelRc, Rgb8Pixel, SharedPixelBuffer, SharedString, VecModel,
};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::env;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Instant;

/// The `--bench-*` / `FA_PDF_TRACE` diagnostic harness (Plan 0001 Phase 2, Plan 0002 Phase 1).
mod bench;

slint::slint! {
    import { Button } from "std-widgets.slint";

    // One model row: an image plus where it goes, in canvas logical pixels.
    //
    // A row keeps its image even while `visible` is false, so panning or zooming back to a cached
    // tile needs no re-render; the scheduler decides when a row is released.
    export struct TileView {
        image: image,
        x: length,
        y: length,
        w: length,
        h: length,
        visible: bool,
    }

    // One page near the viewport: white paper, with the page's low-resolution base layer stretched
    // over it when the page is drawn as tiles (Plan 0002, §4.3).
    export struct PageView {
        image: image,
        x: length,
        y: length,
        w: length,
        h: length,
    }

    export component MainWindow inherits Window {
        title: "PODEFA - PDF Viewer";
        min-width: 600px;
        min-height: 500px;
        preferred-width: 900px;
        preferred-height: 700px;

        in-out property <[TileView]> tiles;
        in-out property <[PageView]> pages;
        // Extent of the canvas window, not of the document (Plan 0002, §4.2).
        in-out property <length> canvas-w: 0px;
        in-out property <length> canvas-h: 0px;

        // The Flickable's position, bound both ways: the Flickable writes it on every scroll and
        // Rust writes it to scroll programmatically (anchored zoom, re-anchor, go-to-page).
        in-out property <length> scroll-x: 0px;
        in-out property <length> scroll-y: 0px;
        in-out property <length> view-w: 0px;
        in-out property <length> view-h: 0px;

        in-out property <string> status_text: "No document loaded";
        in-out property <string> error_text: "";
        in-out property <bool> has_error: false;
        in-out property <int> current_page: 0;
        in-out property <int> total_pages: 0;
        in-out property <float> zoom_percent: 100.0;
        in-out property <bool> is_loading: false;
        // Diagnostics for the manual acceptance checks.
        in-out property <int> tiles_rendered: 0;
        in-out property <int> tiles_requested: 0;

        callback prev_page();
        callback next_page();
        callback zoom_in();
        callback zoom_out();
        callback zoom_reset();
        callback viewport_moved();

        VerticalLayout {
            padding: 8px;
            spacing: 8px;

            // Toolbar
            HorizontalLayout {
                spacing: 8px;
                height: 36px;

                Button {
                    text: "Previous";
                    enabled: root.current_page > 0 && !root.is_loading;
                    clicked => { root.prev_page(); }
                }

                Button {
                    text: "Next";
                    enabled: root.current_page + 1 < root.total_pages && !root.is_loading;
                    clicked => { root.next_page(); }
                }

                Text {
                    text: root.total_pages > 0 ? "Page " + (root.current_page + 1) + " of " + root.total_pages : "No pages";
                    vertical-alignment: center;
                    horizontal-alignment: center;
                    min-width: 120px;
                }

                Rectangle { horizontal-stretch: 1; }

                Button {
                    text: "- Zoom";
                    enabled: !root.is_loading && root.total_pages > 0;
                    clicked => { root.zoom_out(); }
                }

                Button {
                    text: "+ Zoom";
                    enabled: !root.is_loading && root.total_pages > 0;
                    clicked => { root.zoom_in(); }
                }

                Button {
                    text: "Reset (100%)";
                    enabled: !root.is_loading && root.total_pages > 0;
                    clicked => { root.zoom_reset(); }
                }

                Text {
                    text: Math.round(root.zoom_percent) + "%";
                    vertical-alignment: center;
                    min-width: 70px;
                }
            }

            // Error message banner
            if (root.has_error) : Rectangle {
                background: #fde8e8;
                border-color: #f8b4b4;
                border-width: 1px;
                border-radius: 4px;
                height: 40px;

                Text {
                    text: root.error_text;
                    color: #9b1c1c;
                    vertical-alignment: center;
                    horizontal-alignment: center;
                    overflow: elide;
                }
            }

            // Document viewport
            Rectangle {
                background: #e5e7eb;
                border-width: 1px;
                border-color: #d1d5db;
                vertical-stretch: 1;

                viewport := Flickable {
                    visible: !root.has_error && root.total_pages > 0;
                    content-width: root.canvas-w;
                    content-height: root.canvas-h;
                    content-x <=> root.scroll-x;
                    content-y <=> root.scroll-y;

                    // These fire on every change, user or programmatic, including each tick of
                    // the 180 ms wheel animation, which makes them the viewport source of truth.
                    changed content-x => { root.viewport_moved(); }
                    changed content-y => { root.viewport_moved(); }
                    changed width  => { root.view-w = viewport.width;  root.viewport_moved(); }
                    changed height => { root.view-h = viewport.height; root.viewport_moved(); }

                    // Declared before the tiles so they paint under them: a cell still in flight
                    // shows its page's base layer, or white paper on a page drawn as one image.
                    for page in root.pages : Rectangle {
                        x: page.x; y: page.y; width: page.w; height: page.h;
                        background: white;
                        Image {
                            source: page.image;
                            width: parent.width; height: parent.height;
                        }
                    }

                    for tile in root.tiles : Image {
                        source: tile.image;
                        visible: tile.visible;
                        x: tile.x; y: tile.y; width: tile.w; height: tile.h;
                    }
                }

                if (root.total_pages == 0 && !root.has_error) : Text {
                    text: "Open a PDF by running: cargo run -p app -- <file.pdf>";
                    color: #6b7280;
                    vertical-alignment: center;
                    horizontal-alignment: center;
                }
            }

            // Status bar
            HorizontalLayout {
                height: 20px;
                Text {
                    text: root.status_text;
                    color: #4b5563;
                    font-size: 12px;
                    vertical-alignment: center;
                }
                Rectangle { horizontal-stretch: 1; }
                Text {
                    text: root.tiles_rendered + " tiles rendered / " + root.tiles_requested + " requested";
                    color: #9ca3af;
                    font-size: 12px;
                    vertical-alignment: center;
                }
            }
        }
    }
}

/// Spare model rows above the strictly visible tile count: the one-tile ring the prefetch
/// requests, so a one-tile pan finds its next column already rendered instead of blank.
const TILE_MARGIN_RING: usize = 2;

/// Model capacity for a window: viewport size x tile size, plus a margin ring.
///
/// The window alone is a floor, not a bound, in continuous mode: at a low zoom many small pages
/// are visible, each with at least one cell, so `refresh` grows the model past this when the
/// visible set needs it (Plan 0002, §4.3). `--cache-rows` replaces both; see the comment in the
/// body.
fn tile_capacity(view_w_device: f32, view_h_device: f32) -> usize {
    // The row count is the cache's *real* ceiling: the scheduler never holds more tiles than it
    // has rows, so a window-sized viewport caps the cache near its own tile count and the byte
    // budget above that can never bind. A memory run that has to see the budget bind asks for the
    // rows it needs (`--cache-rows`); the override is read here rather than passed in, because
    // this function is what both the startup model and `ensure_capacity` size themselves with.
    if let Some(rows) = bench::cache_rows() {
        return rows.max(4);
    }
    let cols = (view_w_device / TILE_SIZE_PX as f32).ceil().max(1.0) as usize;
    let rows = (view_h_device / TILE_SIZE_PX as f32).ceil().max(1.0) as usize;
    ((cols + TILE_MARGIN_RING) * (rows + TILE_MARGIN_RING)).max(4)
}

/// An empty model row: no image, nothing drawn.
fn empty_tile() -> TileView {
    TileView {
        image: Image::default(),
        x: 0.0,
        y: 0.0,
        w: 0.0,
        h: 0.0,
        visible: false,
    }
}

/// A rendered [`Bitmap`] as a Slint image: one copy into a shared pixel buffer, which Slint then
/// uploads once. It may be called from the event-loop thread, where Slint's image types live.
///
/// `Rgb8Pixel`, not an RGBA expansion: the tiles are opaque, so the pixels stay 3 bytes each in
/// the model and in the upload instead of growing by a quarter on the way in.
fn to_image(bitmap: &Bitmap) -> Image {
    let mut buffer = SharedPixelBuffer::<Rgb8Pixel>::new(bitmap.width, bitmap.height);
    buffer.make_mut_bytes().copy_from_slice(&bitmap.data);
    Image::from_rgb8(buffer)
}

/// One page row as last written to the model: page index and canvas rect in logical pixels, and
/// whether it carried a base layer. Compared before a write, so a scroll that keeps the same pages
/// near the viewport does not touch the model.
type PageRow = (u32, f32, f32, f32, f32, bool);

/// Everything the UI thread needs to turn a viewport into tiles.
struct AppState {
    cmd_tx: Sender<EngineCmd>,
    next_epoch: Arc<AtomicU64>,
    current_epoch: Arc<AtomicU64>,
    page_sizes: Vec<PageSize>,
    /// Every page at the current scale (Plan 0002, §4.1).
    layout: DocLayout,
    scale_milli: u32,
    /// Top-left of the viewport in document pixels: the scroll position, exact at any length.
    pos: (f64, f64),
    /// The bands of the document Slint is given (Plan 0002, §4.2).
    win_x: CanvasWindow,
    win_y: CanvasWindow,
    /// Device pixels the column is shifted right to centre a document narrower than the viewport.
    shift_x: f64,
    scheduler: TileScheduler,
    model: Rc<VecModel<TileView>>,
    pages_model: Rc<VecModel<PageView>>,
    page_rows: Vec<PageRow>,
    /// Base layers of the tiled pages near the viewport, by page (Plan 0002, §4.3).
    bases: HashMap<u32, Image>,
    /// Base layers requested and not yet arrived.
    base_pending: HashSet<u32>,
    scale_factor: f32,
    rendered: i32,
    /// Silent unless `FA_PDF_TRACE` is set or a `--bench-*` flag asked for a run.
    bench: bench::Trace,
}

impl AppState {
    fn sf(&self) -> f64 {
        self.scale_factor as f64
    }

    /// The page gap in device pixels: constant in logical pixels, so it does not grow with zoom.
    fn gap_px(&self) -> u32 {
        (PAGE_GAP_LOGICAL_PX * self.scale_factor).round() as u32
    }

    /// The viewport size in device pixels.
    fn view_size(&self, ui: &MainWindow) -> (f64, f64) {
        (
            ui.get_view_w() as f64 * self.sf(),
            ui.get_view_h() as f64 * self.sf(),
        )
    }

    /// Reads the scroll position back from the `Flickable`, which the user may have moved.
    ///
    /// `scroll-x/y` is the canvas's position relative to the viewport, so it is negative while
    /// scrolled: the viewport starts at `-scroll` inside the canvas window.
    fn read_pos(&mut self, ui: &MainWindow) {
        let sf = self.sf();
        self.pos = (
            self.win_x.start - self.shift_x - ui.get_scroll_x() as f64 * sf,
            self.win_y.start - ui.get_scroll_y() as f64 * sf,
        );
    }

    /// Keeps the scroll position inside the document, and a document narrower than the viewport
    /// centred in it.
    fn clamp_pos(&mut self, ui: &MainWindow) {
        let (view_w, view_h) = self.view_size(ui);
        let doc_w = self.layout.width() as f64;
        let doc_h = self.layout.height() as f64;
        self.shift_x = ((view_w - doc_w) / 2.0).max(0.0).floor();
        self.pos.0 = if self.shift_x > 0.0 {
            -self.shift_x
        } else {
            self.pos.0.clamp(0.0, (doc_w - view_w).max(0.0))
        };
        self.pos.1 = self.pos.1.clamp(0.0, (doc_h - view_h).max(0.0));
    }

    /// A document x in canvas logical pixels.
    fn canvas_x(&self, x: f64) -> f32 {
        ((self.win_x.local(x) + self.shift_x) / self.sf()) as f32
    }

    /// A document y in canvas logical pixels.
    fn canvas_y(&self, y: f64) -> f32 {
        (self.win_y.local(y) / self.sf()) as f32
    }

    /// Re-centres the canvas window on the current position and rewrites everything placed in it:
    /// the extent, the scroll position, every visible tile and the page rows. The view does not
    /// move; only the numbers under it do (Plan 0002, §4.2).
    fn reanchor(&mut self, ui: &MainWindow) {
        let started = Instant::now();
        let (view_w, view_h) = self.view_size(ui);
        let half = CANVAS_HALF_LOGICAL_PX * self.sf();
        let doc_x = self.pos.0.max(0.0);
        self.win_x = CanvasWindow::around(doc_x, view_w, self.layout.width() as f64, half);
        self.win_y = CanvasWindow::around(self.pos.1, view_h, self.layout.height() as f64, half);
        let sf = self.sf();
        let canvas_w = ((self.win_x.len + self.shift_x) / sf) as f32;
        let canvas_h = (self.win_y.len / sf) as f32;
        let scroll_x = -((self.pos.0 - self.win_x.start + self.shift_x) / sf) as f32;
        let scroll_y = -(self.win_y.local(self.pos.1) / sf) as f32;
        with_programmatic_guard(|| {
            ui.set_canvas_w(canvas_w);
            ui.set_canvas_h(canvas_h);
            ui.set_scroll_x(scroll_x);
            ui.set_scroll_y(scroll_y);
        });
        for slot in 0..self.scheduler.capacity() {
            if let Some(key) = self.scheduler.slot_key(slot)
                && self.scheduler.is_visible(&key)
            {
                self.place(slot, &key, None, true);
            }
        }
        self.page_rows.clear();
        self.bench
            .reanchored(started.elapsed().as_micros(), self.pos.1, self.win_y.start);
    }

    /// Whether the viewport has come close enough to a canvas window edge to move the window.
    fn needs_reanchor(&self, ui: &MainWindow) -> bool {
        let (view_w, view_h) = self.view_size(ui);
        let margin = REANCHOR_MARGIN_LOGICAL_PX * self.sf();
        self.win_y
            .needs_reanchor(self.pos.1, view_h, self.layout.height() as f64, margin)
            || self.win_x.needs_reanchor(
                self.pos.0.max(0.0),
                view_w,
                self.layout.width() as f64,
                margin,
            )
    }

    /// The tile cells the live viewport actually shows, across every page in view: what `refresh`
    /// diffs against, and what the trace counts holes over.
    fn visible(&self, ui: &MainWindow) -> Vec<TileKey> {
        let (w, h) = self.view_size(ui);
        self.layout.visible_cells(self.pos.0, self.pos.1, w, h)
    }

    /// The prefetch ring: the same viewport grown by one tile on every side, across pages.
    ///
    /// These cells are requested but never *visible*: the scheduler holds them as ordinary
    /// evictable entries, so a one-tile pan finds the column it uncovers already rendered - the
    /// single-frame gaps `docs/benchmarks.md` §7.5 measures - and a budget that cannot hold them
    /// simply evicts them again. Requested after the visible cells, so it cannot delay them.
    ///
    /// ponytail: one ring for every direction of travel, so a pan pays for the ring it is not
    /// moving towards. At the default row count the model holds the visible cells plus only some
    /// of the ring, so ring cells that never become visible are rendered and evicted again:
    /// `docs/benchmarks.md` §7.6 run A renders 1937 tiles for a sweep that rendered 601 before the
    /// ring existed. The measured effect on what the user sees is the other way round - 0 hole
    /// frames against 241 - and the upgrade path is a ring weighted by the direction the viewport
    /// last moved in (or a row count with room for the whole ring).
    fn ring(&self, ui: &MainWindow) -> Vec<TileKey> {
        let (w, h) = self.view_size(ui);
        if w <= 0.0 || h <= 0.0 {
            return Vec::new();
        }
        let tile = TILE_SIZE_PX as f64;
        self.layout.visible_cells(
            self.pos.0 - tile,
            self.pos.1 - tile,
            w + 2.0 * tile,
            h + 2.0 * tile,
        )
    }

    /// Diffs the live viewport against the cache and applies the result.
    fn refresh(&mut self, ui: &MainWindow) {
        if self.layout.is_empty() {
            return;
        }
        let visible = self.visible(ui);
        // Many small pages at a low zoom each need a cell: grow instead of degrading into blank
        // pages, unless `--cache-rows` fixed the row count for a memory experiment.
        if bench::cache_rows().is_none() && visible.len() * 2 > self.scheduler.capacity() {
            self.grow(visible.len() * 2);
        }
        // Visible cells first, then the ring: the pool's queue is FIFO, so the tiles on screen
        // rasterize before the ones that are only candidates for the next pan step.
        let ring = self.ring(ui);
        let mut actions = self.scheduler.update_view(&visible, self.scale_milli);
        actions.extend(self.scheduler.prefetch(&ring));
        self.bench.frame(&self.scheduler, &visible, &ring);
        self.apply(ui, actions);
        self.update_pages(ui);

        let (_, view_h) = self.view_size(ui);
        if let Some(page) = self.layout.page_near(self.pos.1 + view_h / 2.0) {
            ui.set_current_page(page as i32);
        }
    }

    /// Pages that hold a page row and, when tiled, a base layer: the ones in view plus one page
    /// either side, so the paper and the base layer are already there when a scroll reaches them.
    fn near_pages(&self, ui: &MainWindow) -> std::ops::Range<usize> {
        let (_, view_h) = self.view_size(ui);
        let range = self.layout.pages_in(self.pos.1, self.pos.1 + view_h);
        let start = range.start.saturating_sub(1);
        let end = (range.end + 1).min(self.layout.len());
        start..end.max(start)
    }

    /// Rewrites the page rows and keeps exactly the base layers of the tiled pages near the view.
    ///
    /// A page that fits in one tile has no base layer: its one tile *is* the page image, and a
    /// 1024 px base for a page drawn 80 px tall would cost 2.3 MiB to show nothing extra (Plan
    /// 0002, §4.3).
    fn update_pages(&mut self, ui: &MainWindow) {
        let near = self.near_pages(ui);
        let mut wanted = HashSet::new();
        let mut rows = Vec::with_capacity(near.len());
        for page in near {
            let Some(slot) = self.layout.slot(page).copied() else {
                continue;
            };
            let index = page as u32;
            if slot.is_tiled() {
                wanted.insert(index);
                if !self.bases.contains_key(&index) && self.base_pending.insert(index) {
                    self.request_base(index);
                }
            }
            let sf = self.sf() as f32;
            rows.push((
                index,
                self.canvas_x(slot.x as f64),
                self.canvas_y(slot.y as f64),
                slot.w as f32 / sf,
                slot.h as f32 / sf,
                slot.is_tiled() && self.bases.contains_key(&index),
            ));
        }
        self.bases.retain(|page, _| wanted.contains(page));
        if rows == self.page_rows {
            return;
        }
        let views: Vec<PageView> = rows
            .iter()
            .map(|&(page, x, y, w, h, has_base)| PageView {
                image: if has_base {
                    self.bases.get(&page).cloned().unwrap_or_default()
                } else {
                    Image::default()
                },
                x,
                y,
                w,
                h,
            })
            .collect();
        self.pages_model.set_vec(views);
        self.page_rows = rows;
    }

    /// Distinct pages holding a cached tile or a base layer: the virtualization claim is that this
    /// never exceeds the pages near the viewport (`docs/benchmarks.md` §8.2, `pages_resident`).
    fn pages_resident(&self) -> usize {
        let mut pages: HashSet<u32> = self.scheduler.cached_keys().map(|key| key.page).collect();
        pages.extend(self.bases.keys().copied());
        pages.len()
    }

    /// Applies scheduler actions to the model and the engine.
    fn apply(&mut self, ui: &MainWindow, actions: Vec<TileAction>) {
        let mut requested = 0;
        for action in actions {
            match action {
                TileAction::Show { slot, key } => self.show(slot, &key),
                TileAction::Hide { slot } => self.hide(slot),
                TileAction::Release { slot } => self.release(slot),
                TileAction::Request { key } => {
                    self.request(&key);
                    requested += 1;
                }
            }
        }
        if requested > 0 {
            ui.set_tiles_requested(ui.get_tiles_requested() + requested);
        }
    }

    /// Where a tile goes, in canvas logical pixels, resolved from the layout.
    fn row_geometry(&self, key: &TileKey) -> (f32, f32, f32, f32) {
        let sf = self.sf() as f32;
        match self.layout.tile_place(key) {
            Some((x, y, w, h)) => (
                self.canvas_x(x),
                self.canvas_y(y),
                w as f32 / sf,
                h as f32 / sf,
            ),
            None => (0.0, 0.0, 0.0, 0.0),
        }
    }

    /// Writes one row, replacing the image only when one is given.
    fn place(&self, slot: usize, key: &TileKey, image: Option<Image>, visible: bool) {
        let mut row = self.model.row_data(slot).unwrap_or_else(empty_tile);
        if let Some(image) = image {
            row.image = image;
        }
        let (x, y, w, h) = self.row_geometry(key);
        row.x = x;
        row.y = y;
        row.w = w;
        row.h = h;
        row.visible = visible;
        self.model.set_row_data(slot, row);
    }

    /// A cached tile is wanted again: draw it (its pixels are already in the row).
    fn show(&mut self, slot: usize, key: &TileKey) {
        self.place(slot, key, None, true);
    }

    /// Stop drawing a row but keep its pixels cached.
    fn hide(&mut self, slot: usize) {
        if let Some(mut row) = self.model.row_data(slot) {
            row.visible = false;
            self.model.set_row_data(slot, row);
        }
    }

    /// The scheduler evicted this row: drop its image to release the memory.
    fn release(&mut self, slot: usize) {
        self.model.set_row_data(slot, empty_tile());
    }

    /// Asks the engine for one tile under the current epoch.
    fn request(&mut self, key: &TileKey) {
        let Some(slot) = self.layout.slot(key.page as usize) else {
            return;
        };
        let _ = self.cmd_tx.send(EngineCmd::RenderTile {
            geometry: slot.geometry,
            col: key.col,
            row: key.row,
            request_id: self.current_epoch.load(Ordering::Relaxed),
        });
    }

    /// Asks the engine for the low-resolution base layer of one page (Plan 0001, §5 Phase 2).
    ///
    /// The base layer is a page property, not a level one: one raster serves every zoom of that
    /// page, so it is requested when a tiled page comes near the viewport rather than on a zoom
    /// step. The epoch it carries is the one in flight, but no `Cancel` can reach it: the pool only
    /// registers tiles as abortable, so a zoom during panning cannot blank the page it was supposed
    /// to cover.
    fn request_base(&mut self, page: u32) {
        let Some(size) = self.page_sizes.get(page as usize) else {
            return;
        };
        let _ = self.cmd_tx.send(EngineCmd::RenderBase {
            page,
            scale_milli: base_scale_milli(size.width.max(size.height)),
            request_id: self.current_epoch.load(Ordering::Relaxed),
        });
    }

    /// A tile arrived: cache it and put it on screen.
    fn apply_tile(&mut self, ui: &MainWindow, key: &TileKey, bitmap: &Bitmap) {
        let outcome = self.scheduler.insert(*key, bitmap.data.len());
        self.apply(ui, outcome.actions);
        let Some(slot) = outcome.slot else {
            return;
        };

        let image = to_image(bitmap);

        // Only show it if it is still part of the viewport; otherwise it waits in the cache.
        let visible = self.scheduler.is_visible(key);
        self.place(slot, key, Some(image), visible);

        self.rendered += 1;
        ui.set_tiles_rendered(self.rendered);
        ui.set_is_loading(false);
        let visible = self.visible(ui);
        self.bench.tile(&self.scheduler, &visible);
    }

    /// A base layer arrived: stretch it under its page's tiles, if the page is still near the view.
    ///
    /// A base layer *is* the page, so it is valid across every zoom of that page; it is only
    /// dropped when the page has moved away from the viewport since it was requested.
    fn apply_base(&mut self, ui: &MainWindow, page: u32, bitmap: &Bitmap) {
        if !self.base_pending.remove(&page) {
            return;
        }
        if !self.near_pages(ui).contains(&(page as usize)) {
            return;
        }
        self.bases.insert(page, to_image(bitmap));
        self.update_pages(ui);
    }

    /// Drops every row and its pixels, and every base layer, for a new document.
    fn release_all(&mut self, ui: &MainWindow) {
        let actions = self.scheduler.clear_all();
        self.apply(ui, actions);
        self.bases.clear();
        self.page_rows.clear();
    }

    /// Starts a new epoch: in-flight tiles are cancelled and their results are dropped.
    fn begin_epoch(&mut self) {
        let epoch = self.next_epoch.fetch_add(1, Ordering::Relaxed) + 1;
        let previous = self.current_epoch.swap(epoch, Ordering::Relaxed);
        if previous != 0 {
            let _ = self.cmd_tx.send(EngineCmd::Cancel {
                request_id: previous,
            });
        }
        // The pending set described the previous epoch, which no longer exists.
        self.scheduler.clear_pending();
    }

    /// Adds model rows without dropping anything cached (Plan 0002, §4.3).
    fn grow(&mut self, rows: usize) {
        if rows <= self.scheduler.capacity() {
            return;
        }
        self.scheduler.grow(rows);
        while self.model.row_count() < rows {
            self.model.push(empty_tile());
        }
        self.bench.model_grown(rows);
    }

    /// Grows the model if the window (or device scale factor) needs more rows than it has.
    fn ensure_capacity(&mut self, ui: &MainWindow) {
        let (view_w, view_h) = self.view_size(ui);
        self.grow(tile_capacity(view_w as f32, view_h as f32));
    }

    /// A document was opened: forget everything and show the top of page 1 at 100%.
    fn set_document(&mut self, ui: &MainWindow, page_sizes: Vec<PageSize>) {
        self.page_sizes = page_sizes;
        self.scale_milli = ZOOM_DEFAULT_MILLI;
        self.layout = DocLayout::new(&self.page_sizes, self.scale_milli, self.gap_px());
        self.rendered = 0;
        ui.set_tiles_rendered(0);
        ui.set_tiles_requested(0);
        ui.set_zoom_percent(self.scale_milli as f32 / 10.0);
        self.bench.level_changed(ZOOM_DEFAULT_MILLI, "open");
        self.begin_epoch();
        self.release_all(ui);
        self.base_pending.clear();
        self.pos = (0.0, 0.0);
        self.clamp_pos(ui);
        self.reanchor(ui);
        self.refresh(ui);
    }

    /// Moves the viewport to `pos` (document pixels) without a new epoch, as a scroll does.
    ///
    /// Written under the programmatic guard and followed by its own `refresh`, so the result does
    /// not depend on when Slint runs the `changed` handlers the write raises; they run
    /// `viewport_moved` again later, which is idempotent.
    fn scroll_to(&mut self, ui: &MainWindow, pos: (f64, f64)) {
        self.pos = pos;
        self.clamp_pos(ui);
        if self.needs_reanchor(ui) {
            self.reanchor(ui);
        }
        let sf = self.sf();
        let scroll_x = -((self.pos.0 - self.win_x.start + self.shift_x) / sf) as f32;
        let scroll_y = -(self.win_y.local(self.pos.1) / sf) as f32;
        with_programmatic_guard(|| {
            ui.set_scroll_x(scroll_x);
            ui.set_scroll_y(scroll_y);
        });
        self.refresh(ui);
    }

    /// Jumps to the top of a page: everything still pending belongs to a place the user has left,
    /// so a jump starts an epoch (Plan 0002, §4.4).
    fn go_to_page(&mut self, ui: &MainWindow, page: usize) {
        let Some(slot) = self.layout.slot(page).copied() else {
            return;
        };
        self.begin_epoch();
        self.pos.1 = slot.y as f64 - self.layout.gap_px() as f64;
        self.clamp_pos(ui);
        self.reanchor(ui);
        self.refresh(ui);
        ui.set_current_page(page as i32);
    }

    /// The page the viewport's vertical centre is on.
    fn current_page(&self, ui: &MainWindow) -> usize {
        let (_, view_h) = self.view_size(ui);
        self.layout
            .page_near(self.pos.1 + view_h / 2.0)
            .unwrap_or(0)
    }

    /// Zooms around a viewport anchor (logical pixels), keeping the page point under it still.
    ///
    /// The anchor is resolved on its page, not on the whole document: the gap between pages does
    /// not scale, so scaling the document offset would drift (Plan 0002, §4.5).
    fn zoom(&mut self, ui: &MainWindow, new_milli: u32, anchor_x: f32, anchor_y: f32) {
        let new_milli = new_milli.clamp(ZOOM_MIN_MILLI, ZOOM_MAX_MILLI);
        if new_milli == self.scale_milli || self.layout.is_empty() {
            return;
        }
        let sf = self.sf();
        let to = DocLayout::new(&self.page_sizes, new_milli, self.gap_px());
        let anchor = (anchor_x as f64 * sf, anchor_y as f64 * sf);
        self.pos = self.layout.zoom_anchor(&to, self.pos, anchor);
        self.layout = to;
        self.scale_milli = new_milli;
        self.bench.level_changed(new_milli, "zoom");
        // The old scale's tiles stay cached: only the in-flight requests belong to a dead epoch.
        self.begin_epoch();
        ui.set_zoom_percent(new_milli as f32 / 10.0);
        self.clamp_pos(ui);
        self.reanchor(ui);
        self.refresh(ui);
    }
}

thread_local! {
    /// UI-thread-only application state: the tile images and the model are reference counted, and
    /// every access happens on the Slint thread.
    static APP: RefCell<Option<AppState>> = const { RefCell::new(None) };
    /// Set while Rust writes the scroll position itself, so `viewport_moved` does not run against
    /// a half-updated viewport.
    static PROGRAMMATIC: Cell<bool> = const { Cell::new(false) };
}

/// Runs `f` with the programmatic-update guard raised.
fn with_programmatic_guard<T>(f: impl FnOnce() -> T) -> T {
    PROGRAMMATIC.with(|flag| flag.set(true));
    let out = f();
    PROGRAMMATIC.with(|flag| flag.set(false));
    out
}

/// Runs `f` on the application state, if a document is loaded.
fn with_state(f: impl FnOnce(&mut AppState)) {
    APP.with(|cell| {
        if let Some(state) = cell.borrow_mut().as_mut() {
            f(state);
        }
    });
}

/// The single entry point for viewport changes.
///
/// It reads the position back from the `Flickable`, re-anchors the canvas window when the view
/// nears its edge (or the window was resized), and diffs the view against the cache. It is
/// idempotent, so the `changed` callbacks that a programmatic write raises later are harmless.
fn viewport_moved(ui: &MainWindow) {
    if PROGRAMMATIC.with(Cell::get) {
        return;
    }
    APP.with(|cell| {
        if let Some(state) = cell.borrow_mut().as_mut() {
            state.ensure_capacity(ui);
            if state.layout.is_empty() {
                return;
            }
            state.read_pos(ui);
            let shift = state.shift_x;
            state.clamp_pos(ui);
            if state.shift_x != shift || state.needs_reanchor(ui) {
                state.reanchor(ui);
            }
            state.refresh(ui);
        }
    });
}

/// Where a zoom should keep the page still: the middle of the viewport.
fn viewport_center(ui: &MainWindow) -> (f32, f32) {
    (ui.get_view_w() / 2.0, ui.get_view_h() / 2.0)
}

fn main() -> Result<(), slint::PlatformError> {
    let main_window = MainWindow::new()?;

    // `--bench-*` flags script a run; `FA_PDF_TRACE=1` records one by hand. Both are inert
    // otherwise, and neither belongs in CI: every number here is machine-specific.
    let args: Vec<String> = env::args().collect();
    let (path, cfg) = bench::parse_args(&args);
    let trace_on = bench::trace_enabled() || !cfg.is_empty();
    // The two memory knobs describe the scheduler the app builds, not the run: `--cache-mib 8`
    // alone is a valid experiment (trace it by hand with `FA_PDF_TRACE=1`), and neither flag
    // starts one. `--cache-rows` is read where the model is sized, so it covers the rebuild
    // `ensure_capacity` does as well.
    let cache_bytes = cfg
        .cache_bytes
        .unwrap_or_else(pdf_core::scheduler::default_cache_bytes);
    if let Some(rows) = cfg.cache_rows {
        bench::set_cache_rows(rows);
    }
    // The `Opened` handler runs on the UI thread inside a `Send` closure, so the run's config
    // crosses over in a shared cell and is taken by the first open.
    let bench_cfg = Arc::new(Mutex::new((!cfg.is_empty()).then_some(cfg)));

    // Spawn engine actor
    let (engine, event_rx) = EngineHandle::spawn();
    let cmd_tx = engine.sender().clone();

    let next_epoch = Arc::new(AtomicU64::new(0));
    let current_epoch = Arc::new(AtomicU64::new(0));

    // Start from the window size: the viewport can never be larger than the window, so the model
    // rarely has to grow once the first layout pass reports the real viewport size.
    let window_size = main_window.window().size();
    let capacity = tile_capacity(window_size.width as f32, window_size.height as f32);
    let model = Rc::new(VecModel::from(vec![empty_tile(); capacity]));
    main_window.set_tiles(ModelRc::from(model.clone()));
    let pages_model = Rc::new(VecModel::<PageView>::default());
    main_window.set_pages(ModelRc::from(pages_model.clone()));

    APP.with(|cell| {
        *cell.borrow_mut() = Some(AppState {
            cmd_tx: cmd_tx.clone(),
            next_epoch: Arc::clone(&next_epoch),
            current_epoch: Arc::clone(&current_epoch),
            page_sizes: Vec::new(),
            layout: DocLayout::default(),
            scale_milli: ZOOM_DEFAULT_MILLI,
            pos: (0.0, 0.0),
            win_x: CanvasWindow::default(),
            win_y: CanvasWindow::default(),
            shift_x: 0.0,
            scheduler: TileScheduler::new(capacity, cache_bytes),
            model: model.clone(),
            pages_model: pages_model.clone(),
            page_rows: Vec::new(),
            bases: HashMap::new(),
            base_pending: HashSet::new(),
            scale_factor: main_window.window().scale_factor(),
            rendered: 0,
            bench: bench::Trace::new(trace_on),
        });
    });

    // Engine events are applied on the UI thread; a tile from a superseded epoch is dropped
    // before it reaches the scheduler.
    let weak = main_window.as_weak();

    // The memory sampler starts here rather than at the first document: the resident set before
    // anything is open is the baseline a plateau is read against, and on a `--bench-*` run it also
    // covers the document's own first render.
    if trace_on {
        bench::start_sampler();
    }
    let epoch_for_events = Arc::clone(&current_epoch);
    let bench_for_events = Arc::clone(&bench_cfg);
    thread::spawn(move || {
        while let Ok(event) = event_rx.recv() {
            let weak = weak.clone();
            let epoch = Arc::clone(&epoch_for_events);
            let bench_cfg = Arc::clone(&bench_for_events);
            let _ = weak.upgrade_in_event_loop(move |ui| match event {
                EngineEvent::Opened {
                    page_count,
                    page_sizes,
                    unsized_pages,
                } => {
                    ui.set_has_error(false);
                    ui.set_error_text(SharedString::from(""));
                    ui.set_total_pages(page_count as i32);
                    ui.set_current_page(0);
                    ui.set_is_loading(false);
                    // A page whose bounds could not be read is laid out as Letter; say so rather
                    // than draw it at a size the document never had without a word.
                    let status = if unsized_pages.is_empty() {
                        format!("{page_count} pages")
                    } else {
                        format!(
                            "{page_count} pages ({} with unreadable sizes, shown as Letter)",
                            unsized_pages.len()
                        )
                    };
                    ui.set_status_text(SharedString::from(status));
                    with_state(|state| state.set_document(&ui, page_sizes));
                    // A scripted run starts on the open, so its first step is a real zoom or pan
                    // rather than a no-op against an empty document.
                    if let Some(cfg) = bench_cfg.lock().unwrap().take() {
                        bench::start(&ui, cfg);
                    }
                }
                EngineEvent::TileRendered {
                    key,
                    request_id,
                    bitmap,
                } => {
                    if epoch.load(Ordering::Relaxed) != request_id {
                        return;
                    }
                    with_state(|state| state.apply_tile(&ui, &key, &bitmap));
                }
                EngineEvent::BaseRendered { page, bitmap, .. } => {
                    // No epoch guard: a base layer belongs to a page, not to a zoom level, and it
                    // arrives while the epoch it was requested under is already superseded.
                    with_state(|state| state.apply_base(&ui, page, &bitmap));
                }
                EngineEvent::TileAborted { ms, .. } => {
                    with_state(|state| state.bench.aborted(ms));
                }
                EngineEvent::Error { message } => {
                    ui.set_has_error(true);
                    ui.set_error_text(SharedString::from(message.clone()));
                    ui.set_status_text(SharedString::from(format!("Error: {message}")));
                    ui.set_is_loading(false);
                }
            });
        }
    });

    // `viewport_moved` is raised by the Flickable on every content-x/y/size change.
    {
        let weak = main_window.as_weak();
        main_window.on_viewport_moved(move || {
            if let Some(ui) = weak.upgrade() {
                viewport_moved(&ui);
            }
        });
    }

    // Zooming keeps the viewport centre stationary.
    {
        let weak = main_window.as_weak();
        main_window.on_zoom_in(move || {
            if let Some(ui) = weak.upgrade() {
                let (ax, ay) = viewport_center(&ui);
                with_state(|state| {
                    let next = zoom_in_milli(state.scale_milli);
                    state.zoom(&ui, next, ax, ay);
                });
            }
        });
    }

    {
        let weak = main_window.as_weak();
        main_window.on_zoom_out(move || {
            if let Some(ui) = weak.upgrade() {
                let (ax, ay) = viewport_center(&ui);
                with_state(|state| {
                    let next = zoom_out_milli(state.scale_milli);
                    state.zoom(&ui, next, ax, ay);
                });
            }
        });
    }

    {
        let weak = main_window.as_weak();
        main_window.on_zoom_reset(move || {
            if let Some(ui) = weak.upgrade() {
                let (ax, ay) = viewport_center(&ui);
                with_state(|state| state.zoom(&ui, ZOOM_DEFAULT_MILLI, ax, ay));
            }
        });
    }

    {
        let weak = main_window.as_weak();
        main_window.on_prev_page(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| {
                    let page = state.current_page(&ui).saturating_sub(1);
                    state.go_to_page(&ui, page);
                });
            }
        });
    }

    {
        let weak = main_window.as_weak();
        main_window.on_next_page(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| {
                    let page = state.current_page(&ui) + 1;
                    state.go_to_page(&ui, page);
                });
            }
        });
    }

    // CLI argument handling: open the file passed as the first argument.
    if let Some(path_arg) = path {
        let pdf_path = PathBuf::from(path_arg);
        main_window.set_status_text(SharedString::from(format!(
            "Opening '{}'...",
            pdf_path.display()
        )));
        main_window.set_is_loading(true);

        let _ = cmd_tx.send(EngineCmd::Open {
            path: pdf_path,
            password: None,
        });
    }

    main_window.run()
}
