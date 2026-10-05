// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Slint front end for the tiled, viewport-only renderer (Plan 0001).
//!
//! The UI keeps no rendering policy of its own: it mirrors the `Flickable`'s viewport into Rust,
//! asks [`pdf_core::TileScheduler`] what should be on screen, and applies the returned actions to
//! a fixed-size tile model. Every viewport change - wheel scroll, drag, programmatic scroll,
//! zoom or window resize - funnels through one function, `viewport_moved`.

use engine_mupdf::EngineHandle;
use pdf_core::{
    Bitmap, EngineCmd, EngineEvent, PageGeometry, PageSize, TILE_SIZE_PX, TileAction, TileKey,
    TileScheduler, ZOOM_DEFAULT_MILLI, ZOOM_MAX_MILLI, ZOOM_MIN_MILLI, zoom_anchor, zoom_in_milli,
    zoom_out_milli,
};
use slint::{
    ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel,
};
use std::cell::{Cell, RefCell};
use std::env;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::thread;

slint::slint! {
    import { Button } from "std-widgets.slint";

    // One model row: an image plus where it goes, in logical pixels.
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

    export component MainWindow inherits Window {
        title: "PODEFA - PDF Viewer";
        min-width: 600px;
        min-height: 500px;
        preferred-width: 900px;
        preferred-height: 700px;

        in-out property <[TileView]> tiles;
        in-out property <length> page-w: 0px;
        in-out property <length> page-h: 0px;

        // Read side: the Flickable pushes its position and size here on every change, so Rust
        // never has to rely on `flicked` (user input only, once per wheel animation).
        in-out property <length> view-x: 0px;
        in-out property <length> view-y: 0px;
        in-out property <length> view-w: 0px;
        in-out property <length> view-h: 0px;
        // Write side: Rust sets these to scroll programmatically (anchored zoom).
        in-out property <length> scroll-x: 0px;
        in-out property <length> scroll-y: 0px;

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
                    content-width: root.page-w;
                    content-height: root.page-h;

                    // These fire on every change, user or programmatic, including each tick of
                    // the 180 ms wheel animation, which makes them the viewport source of truth.
                    changed content-x => { root.view-x = viewport.content-x; root.viewport_moved(); }
                    changed content-y => { root.view-y = viewport.content-y; root.viewport_moved(); }
                    changed width  => { root.view-w = viewport.width;  root.viewport_moved(); }
                    changed height => { root.view-h = viewport.height; root.viewport_moved(); }

                    for tile in root.tiles : Image {
                        source: tile.image;
                        visible: tile.visible;
                        x: tile.x; y: tile.y; width: tile.w; height: tile.h;
                    }
                }

                // Programmatic scroll: Rust writes these and the Flickable follows.

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

        // Programmatic scroll: Rust writes scroll-x/scroll-y and the Flickable follows. Declared
        // after the layout so the `viewport` id is already known, and on the root so `changed`
        // refers to the root property rather than an enclosing element's.
        changed scroll-x => { viewport.content-x = root.scroll-x; }
        changed scroll-y => { viewport.content-y = root.scroll-y; }
    }
}

/// Spare model rows above the strictly visible tile count, so a one-tile pan does not have to
/// evict anything.
const TILE_MARGIN_RING: usize = 2;

/// Model capacity: viewport size x tile size, plus a margin ring.
///
/// This bound depends only on the window and the device scale factor - never on the page extent
/// or the zoom level, because a bigger page or a deeper zoom shows the same number of tiles
/// (Plan 0001, §4.6).
fn tile_capacity(view_w_device: f32, view_h_device: f32) -> usize {
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

/// Everything the UI thread needs to turn a viewport into tiles.
struct AppState {
    cmd_tx: Sender<EngineCmd>,
    next_epoch: Arc<AtomicU64>,
    current_epoch: Arc<AtomicU64>,
    page_sizes: Vec<PageSize>,
    page: u32,
    scale_milli: u32,
    scheduler: TileScheduler,
    model: Rc<VecModel<TileView>>,
    scale_factor: f32,
    rendered: i32,
}

impl AppState {
    /// Geometry of a page at a scale, from the document's page table.
    fn geometry_of(&self, page: u32, scale_milli: u32) -> Option<PageGeometry> {
        let size = self.page_sizes.get(page as usize)?;
        Some(PageGeometry::new(page, size.bounds(), scale_milli))
    }

    /// Geometry of the page on screen.
    fn geometry(&self) -> Option<PageGeometry> {
        self.geometry_of(self.page, self.scale_milli)
    }

    /// The viewport in page-raster device pixels.
    ///
    /// `content-x` is the scrollable content's position relative to the viewport, so it is
    /// negative while scrolled: the visible region starts at `-content-x`.
    fn view_rect(&self, ui: &MainWindow) -> pdf_core::Rect {
        let sf = self.scale_factor;
        pdf_core::Rect::new(
            -ui.get_view_x() * sf,
            -ui.get_view_y() * sf,
            ui.get_view_w() * sf,
            ui.get_view_h() * sf,
        )
    }

    /// Diffs the live viewport against the cache and applies the result.
    fn refresh(&mut self, ui: &MainWindow) {
        let Some(geo) = self.geometry() else {
            return;
        };
        let view = self.view_rect(ui);
        if view.width <= 0.0 || view.height <= 0.0 {
            return;
        }
        let desired: Vec<TileKey> = geo
            .visible_cells(view)
            .map(|(c, r)| geo.key(c, r))
            .collect();
        let actions = self.scheduler.update_view(&desired, self.scale_milli);
        self.apply(ui, actions);
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

    /// Where a tile goes, in logical pixels, resolved from the page table.
    fn row_geometry(&self, key: &TileKey) -> (f32, f32, f32, f32) {
        let sf = self.scale_factor;
        let rect = self
            .geometry_of(key.page, key.scale_milli)
            .and_then(|geo| geo.tile_rect(key.col, key.row));
        match rect {
            Some(rect) => (
                rect.x as f32 / sf,
                rect.y as f32 / sf,
                rect.w as f32 / sf,
                rect.h as f32 / sf,
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
        let Some(geometry) = self.geometry_of(key.page, key.scale_milli) else {
            return;
        };
        let _ = self.cmd_tx.send(EngineCmd::RenderTile {
            geometry,
            col: key.col,
            row: key.row,
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

        let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(bitmap.width, bitmap.height);
        buffer.make_mut_bytes().copy_from_slice(&bitmap.data);
        let image = Image::from_rgba8(buffer);

        // Only show it if it is still part of the viewport; otherwise it waits in the cache.
        let visible = self.scheduler.is_visible(key);
        self.place(slot, key, Some(image), visible);

        self.rendered += 1;
        ui.set_tiles_rendered(self.rendered);
        ui.set_is_loading(false);
    }

    /// Drops every row and its pixels, for a page change or a model resize.
    fn release_all(&mut self, ui: &MainWindow) {
        let actions = self.scheduler.clear_all();
        self.apply(ui, actions);
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

    /// Grows the model if the window (or device scale factor) needs more rows than it has.
    ///
    /// Resizing is the only event that can change the tile count, and it rebuilds the model
    /// rather than faulting later (Plan 0001, §4.6).
    fn ensure_capacity(&mut self, ui: &MainWindow) {
        let sf = self.scale_factor;
        let needed = tile_capacity(ui.get_view_w() * sf, ui.get_view_h() * sf);
        if needed <= self.scheduler.capacity() {
            return;
        }
        let budget = self.scheduler.cache_max_bytes();
        self.release_all(ui);
        self.scheduler = TileScheduler::new(needed, budget);
        self.model = Rc::new(VecModel::from(vec![empty_tile(); needed]));
        ui.set_tiles(ModelRc::from(self.model.clone()));
    }

    /// Publishes the page extent and the zoom readout, without re-entering `viewport_moved`.
    fn update_extent(&self, ui: &MainWindow) {
        let Some(geo) = self.geometry() else {
            return;
        };
        let (device_w, device_h) = geo.device_size();
        let sf = self.scale_factor;
        with_programmatic_guard(|| {
            ui.set_page_w(device_w as f32 / sf);
            ui.set_page_h(device_h as f32 / sf);
            ui.set_zoom_percent(self.scale_milli as f32 / 10.0);
        });
    }

    /// A document was opened: forget everything and show page 0 at 100%.
    fn set_document(&mut self, ui: &MainWindow, page_sizes: Vec<PageSize>) {
        self.page_sizes = page_sizes;
        self.page = 0;
        self.scale_milli = ZOOM_DEFAULT_MILLI;
        self.rendered = 0;
        ui.set_tiles_rendered(0);
        ui.set_tiles_requested(0);
        self.begin_epoch();
        self.release_all(ui);
        self.update_extent(ui);
        with_programmatic_guard(|| {
            ui.set_scroll_x(0.0);
            ui.set_scroll_y(0.0);
        });
        self.refresh(ui);
    }

    /// Switches page, dropping the previous page's tiles.
    fn set_page(&mut self, ui: &MainWindow, page: u32) {
        if page as usize >= self.page_sizes.len() || page == self.page {
            return;
        }
        self.page = page;
        ui.set_current_page(page as i32);
        self.begin_epoch();
        self.release_all(ui);
        self.update_extent(ui);
        with_programmatic_guard(|| {
            ui.set_scroll_x(0.0);
            ui.set_scroll_y(0.0);
        });
        self.refresh(ui);
    }

    /// Zooms around a viewport anchor (logical pixels), keeping the page point under it still.
    fn zoom(&mut self, ui: &MainWindow, new_milli: u32, anchor_x: f32, anchor_y: f32) {
        let new_milli = new_milli.clamp(ZOOM_MIN_MILLI, ZOOM_MAX_MILLI);
        if new_milli == self.scale_milli || self.geometry().is_none() {
            return;
        }
        let sf = self.scale_factor;
        let old_offset_x = -ui.get_view_x() * sf;
        let old_offset_y = -ui.get_view_y() * sf;
        let new_offset_x = zoom_anchor(old_offset_x, anchor_x * sf, self.scale_milli, new_milli);
        let new_offset_y = zoom_anchor(old_offset_y, anchor_y * sf, self.scale_milli, new_milli);

        self.scale_milli = new_milli;
        // The old scale's tiles stay cached: only the in-flight requests belong to a dead epoch.
        self.begin_epoch();

        let Some(geo) = self.geometry() else {
            return;
        };
        let (device_w, device_h) = geo.device_size();
        let view_w = ui.get_view_w() * sf;
        let view_h = ui.get_view_h() * sf;
        let offset_x = new_offset_x.clamp(0.0, (device_w as f32 - view_w).max(0.0));
        let offset_y = new_offset_y.clamp(0.0, (device_h as f32 - view_h).max(0.0));

        with_programmatic_guard(|| {
            ui.set_page_w(device_w as f32 / sf);
            ui.set_page_h(device_h as f32 / sf);
            ui.set_scroll_x(-offset_x / sf);
            ui.set_scroll_y(-offset_y / sf);
            ui.set_zoom_percent(new_milli as f32 / 10.0);
        });

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
fn viewport_moved(ui: &MainWindow) {
    if PROGRAMMATIC.with(Cell::get) {
        return;
    }
    APP.with(|cell| {
        if let Some(state) = cell.borrow_mut().as_mut() {
            state.ensure_capacity(ui);
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

    APP.with(|cell| {
        *cell.borrow_mut() = Some(AppState {
            cmd_tx: cmd_tx.clone(),
            next_epoch: Arc::clone(&next_epoch),
            current_epoch: Arc::clone(&current_epoch),
            page_sizes: Vec::new(),
            page: 0,
            scale_milli: ZOOM_DEFAULT_MILLI,
            scheduler: TileScheduler::new(capacity, pdf_core::scheduler::default_cache_bytes()),
            model: model.clone(),
            scale_factor: main_window.window().scale_factor(),
            rendered: 0,
        });
    });

    // Engine events are applied on the UI thread; a tile from a superseded epoch is dropped
    // before it reaches the scheduler.
    let weak = main_window.as_weak();
    let epoch_for_events = Arc::clone(&current_epoch);
    thread::spawn(move || {
        while let Ok(event) = event_rx.recv() {
            let weak = weak.clone();
            let epoch = Arc::clone(&epoch_for_events);
            let _ = weak.upgrade_in_event_loop(move |ui| match event {
                EngineEvent::Opened {
                    page_count,
                    page_sizes,
                } => {
                    ui.set_has_error(false);
                    ui.set_error_text(SharedString::from(""));
                    ui.set_total_pages(page_count as i32);
                    ui.set_current_page(0);
                    ui.set_is_loading(false);
                    ui.set_status_text(SharedString::from(format!("{page_count} pages")));
                    with_state(|state| state.set_document(&ui, page_sizes));
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
                    let page = state.page.saturating_sub(1);
                    state.set_page(&ui, page);
                });
            }
        });
    }

    {
        let weak = main_window.as_weak();
        main_window.on_next_page(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| {
                    let page = state.page + 1;
                    state.set_page(&ui, page);
                });
            }
        });
    }

    // CLI argument handling: open the file passed as the first argument.
    let args: Vec<String> = env::args().collect();
    if let Some(path_arg) = args.get(1) {
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
