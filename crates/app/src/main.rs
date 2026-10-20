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
use pdf_core::OutlineItem;
use pdf_core::nav::{
    LINE_STEP_LOGICAL_PX, NavAction, SCROLLBAR_MIN_THUMB_LOGICAL_PX, fit_page_milli,
    fit_width_milli, page_top, scroll_target, scrollbar_pos, scrollbar_thumb, wheel_zoom_milli,
};
use pdf_core::search::{SearchHit, SearchResults};
use pdf_core::settings::{Settings, Theme};
use pdf_core::{
    Bitmap, CANVAS_HALF_LOGICAL_PX, CanvasWindow, DocLayout, EngineCmd, EngineEvent,
    PAGE_GAP_LOGICAL_PX, PageSize, REANCHOR_MARGIN_LOGICAL_PX, TILE_SIZE_PX, TileAction, TileKey,
    TileScheduler, ZOOM_DEFAULT_MILLI, ZOOM_MAX_MILLI, ZOOM_MIN_MILLI, base_scale_milli,
    zoom_in_milli, zoom_out_milli,
};
use slint::platform::Key;
use slint::{
    ComponentHandle, Image, Model, ModelRc, Rgb8Pixel, SharedPixelBuffer, SharedString, VecModel,
};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::env;
use std::path::{Path, PathBuf};
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
    import { Button, LineEdit, ListView, Palette } from "std-widgets.slint";

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

    // One thumbnail row of the sidebar (Plan 0003, §4.3). Rows exist for every page, but the
    // ListView instantiates only the ones in view, and only those near the view hold an image.
    export struct ThumbRow {
        image: image,
        has-image: bool,
        label: string,
        current: bool,
    }

    // One visible row of the outline tree: collapsed children are not in the model at all.
    export struct OutlineRow {
        title: string,
        depth: int,
        expandable: bool,
        expanded: bool,
        enabled: bool,
    }

    // One highlighted search hit rectangle, in canvas logical pixels (Plan 0003, §4.4).
    export struct HitView {
        x: length,
        y: length,
        w: length,
        h: length,
        current: bool,
    }

    export component MainWindow inherits Window {
        title: root.doc-title == "" ? "PODEFA - PDF Viewer" : root.doc-title + " - PODEFA";
        init => {
            root.apply-theme();
            viewport-keys.focus();
        }
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

        // The open document's file name, for the title bar; empty when nothing is open.
        in-out property <string> doc-title: "";
        // 0 = follow the system, 1 = light, 2 = dark (Plan 0003, §4.6).
        in-out property <int> theme: 0;
        // Recent files, newest first, as the File menu shows them.
        in-out property <[string]> recent-files;

        // The sidebar (Plan 0003, §4.3): 0 = page thumbnails, 1 = outline.
        in-out property <bool> sidebar-visible: false;
        in-out property <int> sidebar-tab: 0;
        in-out property <[ThumbRow]> thumbs;
        in-out property <[OutlineRow]> outline-rows;
        // The thumbnail list's scroll position, bound both ways like the viewport's.
        in-out property <length> thumb-scroll-y: 0px;

        // The find bar (Plan 0003, §4.4).
        in-out property <bool> find-visible: false;
        in-out property <string> find-text: "";
        in-out property <string> find-counter: "";
        in-out property <[HitView]> hit-rects;

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

        // The document scrollbar (Plan 0002, §4.2): Rust places the thumb from the document
        // position, because the Flickable only knows the canvas window.
        in-out property <bool> sb-visible: false;
        in-out property <length> sb-thumb-y: 0px;
        in-out property <length> sb-thumb-h: 0px;
        // The go-to-page field's text; Rust follows the current page while the field is not
        // being edited.
        in-out property <string> page-input: "";
        out property <bool> page-field-focused: page-field.has-focus;

        callback prev_page();
        callback next_page();
        callback zoom_in();
        callback zoom_out();
        callback zoom_reset();
        callback viewport_moved();
        callback fit_width();
        callback fit_page();
        // A key on the viewport: text as Slint reports it, plus ctrl and shift; true when handled.
        callback nav_key(string, bool, bool) -> bool;
        // Ctrl+wheel: the wheel delta and the cursor, viewport-relative, in logical pixels.
        callback wheel_zoom(length, length, length);
        callback go_to_page(string);
        // The scrollbar thumb was dragged to `offset` within a track of `track`.
        callback scrollbar_moved(length, length);
        callback find_open();
        callback find_changed(string);
        callback find_next();
        callback find_previous();
        callback find_close();
        // Gives the keyboard back to the viewport (after the find bar closes).
        callback focus_viewport();
        focus_viewport => { viewport-keys.focus(); }
        callback toggle_sidebar();
        callback sidebar_tab_changed();
        callback thumbs_moved();
        callback thumb_clicked(int);
        callback outline_clicked(int);
        callback outline_toggled(int);
        callback open_file();
        callback open_recent(int);
        callback choose_theme(int);
        callback quit();
        // A drag over the window: whether it carries a document we can open.
        callback can_open_drop(data-transfer) -> bool;
        // A drop: open the document it carries.
        callback open_drop(data-transfer);

        function apply-theme() {
            Palette.color-scheme = root.theme == 1 ? ColorScheme.light
                : root.theme == 2 ? ColorScheme.dark : ColorScheme.unknown;
        }
        changed theme => { root.apply-theme(); }

        MenuBar {
            Menu {
                title: "File";
                MenuItem {
                    title: "Open...";
                    shortcut: @keys(Control + O);
                    activated => { root.open_file(); }
                }
                Menu {
                    title: "Open Recent";
                    enabled: root.recent-files.length > 0;
                    for path[index] in root.recent-files : MenuItem {
                        title: path;
                        activated => { root.open_recent(index); }
                    }
                }
                MenuSeparator {}
                MenuItem {
                    title: "Quit";
                    shortcut: @keys(Control + Q);
                    activated => { root.quit(); }
                }
            }
            Menu {
                title: "Edit";
                MenuItem {
                    title: "Find...";
                    shortcut: @keys(Control + F);
                    activated => { root.find_open(); }
                }
            }
            Menu {
                title: "View";
                MenuItem {
                    title: "Sidebar";
                    checkable: true;
                    checked: root.sidebar-visible;
                    shortcut: @keys(F4);
                    activated => { root.toggle_sidebar(); }
                }
                Menu {
                    title: "Theme";
                    MenuItem {
                        title: "System";
                        checkable: true;
                        checked: root.theme == 0;
                        activated => { root.choose_theme(0); }
                    }
                    MenuItem {
                        title: "Light";
                        checkable: true;
                        checked: root.theme == 1;
                        activated => { root.choose_theme(1); }
                    }
                    MenuItem {
                        title: "Dark";
                        checkable: true;
                        checked: root.theme == 2;
                        activated => { root.choose_theme(2); }
                    }
                }
            }
        }

        VerticalLayout {
            padding: 8px;
            spacing: 8px;

            // Toolbar
            HorizontalLayout {
                spacing: 8px;
                height: 36px;

                Button {
                    text: "Sidebar";
                    checkable: true;
                    checked: root.sidebar-visible;
                    enabled: root.total_pages > 0;
                    clicked => { root.toggle_sidebar(); }
                }

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

                page-field := LineEdit {
                    width: 64px;
                    enabled: root.total_pages > 0;
                    horizontal-alignment: right;
                    text <=> root.page-input;
                    accepted(text) => {
                        root.go_to_page(text);
                        viewport-keys.focus();
                    }
                }

                Text {
                    text: root.total_pages > 0 ? "of " + root.total_pages : "No pages";
                    vertical-alignment: center;
                    min-width: 60px;
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

                Button {
                    text: "Fit width";
                    enabled: !root.is_loading && root.total_pages > 0;
                    clicked => { root.fit_width(); }
                }

                Button {
                    text: "Fit page";
                    enabled: !root.is_loading && root.total_pages > 0;
                    clicked => { root.fit_page(); }
                }

                Text {
                    text: Math.round(root.zoom_percent) + "%";
                    vertical-alignment: center;
                    min-width: 70px;
                }
            }

            // The find bar: Enter / F3 next, Shift+Enter / Shift+F3 previous, Esc closes.
            if root.find-visible : FocusScope {
                height: 32px;
                capture-key-pressed(event) => {
                    if (event.text == Key.Escape) {
                        root.find_close();
                        return accept;
                    }
                    if (event.text == Key.Return && event.modifiers.shift) {
                        root.find_previous();
                        return accept;
                    }
                    if (event.text == Key.F3) {
                        if (event.modifiers.shift) { root.find_previous(); } else { root.find_next(); }
                        return accept;
                    }
                    return reject;
                }
                HorizontalLayout {
                    spacing: 8px;
                    LineEdit {
                        placeholder-text: "Find  (Enter: next, Shift+Enter: previous)";
                        text <=> root.find-text;
                        init => { self.focus(); }
                        edited(text) => { root.find_changed(text); }
                        accepted => { root.find_next(); }
                    }
                    Text {
                        text: root.find-counter;
                        vertical-alignment: center;
                        min-width: 90px;
                        color: Palette.foreground;
                    }
                    Button { text: "Previous"; clicked => { root.find_previous(); } }
                    Button { text: "Next"; clicked => { root.find_next(); } }
                    Button { text: "Close"; clicked => { root.find_close(); } }
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

            HorizontalLayout {
              spacing: 8px;
              vertical-stretch: 1;

              if root.sidebar-visible && root.total_pages > 0 : Rectangle {
                width: 196px;
                border-width: 1px;
                border-color: Palette.border;
                VerticalLayout {
                    padding: 4px;
                    spacing: 4px;
                    HorizontalLayout {
                        spacing: 4px;
                        height: 30px;
                        Button {
                            text: "Pages";
                            checkable: true;
                            checked: root.sidebar-tab == 0;
                            clicked => { root.sidebar-tab = 0; root.sidebar_tab_changed(); }
                        }
                        Button {
                            text: "Outline";
                            checkable: true;
                            checked: root.sidebar-tab == 1;
                            clicked => { root.sidebar-tab = 1; root.sidebar_tab_changed(); }
                        }
                    }
                    if root.sidebar-tab == 0 : ListView {
                        vertical-stretch: 1;
                        content-y <=> root.thumb-scroll-y;
                        changed content-y => { root.thumbs_moved(); }
                        for row[index] in root.thumbs : Rectangle {
                            // Fixed height: Rust derives the rows in view from the scroll
                            // position alone (THUMB_ROW_LOGICAL_PX).
                            height: 232px;
                            background: row.current ? Palette.selection-background : transparent;
                            TouchArea {
                                clicked => { root.thumb_clicked(index); }
                            }
                            VerticalLayout {
                                padding: 6px;
                                spacing: 4px;
                                Rectangle {
                                    height: 200px;
                                    border-width: row.has-image ? 0px : 1px;
                                    border-color: Palette.border;
                                    Image {
                                        source: row.image;
                                        image-fit: contain;
                                        width: parent.width;
                                        height: parent.height;
                                    }
                                }
                                Text {
                                    text: row.label;
                                    horizontal-alignment: center;
                                    color: row.current ? Palette.selection-foreground : Palette.foreground;
                                }
                            }
                        }
                    }
                    if root.sidebar-tab == 1 : ListView {
                        vertical-stretch: 1;
                        for row[index] in root.outline-rows : Rectangle {
                            height: 26px;
                            TouchArea {
                                clicked => { root.outline_clicked(index); }
                            }
                            HorizontalLayout {
                                padding-left: 4px + row.depth * 14px;
                                spacing: 4px;
                                Rectangle {
                                    width: 14px;
                                    Text {
                                        text: row.expandable ? (row.expanded ? "v" : ">") : "";
                                        color: Palette.foreground;
                                        vertical-alignment: center;
                                    }
                                    TouchArea {
                                        enabled: row.expandable;
                                        clicked => { root.outline_toggled(index); }
                                    }
                                }
                                Text {
                                    text: row.title;
                                    overflow: elide;
                                    vertical-alignment: center;
                                    color: Palette.foreground;
                                    opacity: row.enabled ? 1.0 : 0.5;
                                }
                            }
                        }
                    }
                    if root.sidebar-tab == 1 && root.outline-rows.length == 0 : Text {
                        text: "This document has no outline";
                        color: Palette.foreground;
                        opacity: 0.6;
                        horizontal-alignment: center;
                        vertical-stretch: 1;
                    }
                }
              }

            // Document viewport. The FocusScope takes the navigation keys (Plan 0002, §4.6); it has
            // focus from startup and takes it back on a click into the page.
            viewport-keys := FocusScope {
              horizontal-stretch: 1;
              key-pressed(event) => {
                  if (root.nav_key(event.text, event.modifiers.control, event.modifiers.shift)) {
                      return accept;
                  }
                  return reject;
              }
              // Files dropped from other applications (Plan 0003, §3.2): Slint 1.18.1's winit backend
              // never delivers them, so this is inert until the Slint release carrying
              // slint-ui/slint#13527, and needs no change then.
              DropArea {
                can-drop(event) => {
                    return root.can_open_drop(event.data) ? DragAction.copy : DragAction.none;
                }
                dropped(event) => {
                    root.open_drop(event.data);
                    return DragAction.copy;
                }
              Rectangle {
                background: Palette.alternate-background;
                border-width: 1px;
                border-color: Palette.border;

                viewport := Flickable {
                    // A failed open keeps the document that was already showing.
                    visible: root.total_pages > 0;
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

                    // Search hits over the tiles: page-relative rectangles placed like tiles.
                    for hit in root.hit-rects : Rectangle {
                        x: hit.x; y: hit.y; width: hit.w; height: hit.h;
                        background: hit.current ? #ff8c0080 : #ffd60066;
                        border-width: hit.current ? 1px : 0px;
                        border-color: #e65100;
                    }

                    // Ctrl+wheel zooms at the cursor. The Flickable forwards wheel events to its
                    // children first, so this area - kept over the viewport, not the canvas -
                    // sees them; without ctrl it rejects them and the Flickable scrolls.
                    TouchArea {
                        x: -root.scroll-x; y: -root.scroll-y;
                        width: viewport.width; height: viewport.height;
                        scroll-event(event) => {
                            if (event.modifiers.control) {
                                root.wheel_zoom(event.delta-y, self.mouse-x, self.mouse-y);
                                return accept;
                            }
                            return reject;
                        }
                        pointer-event(event) => {
                            if (event.kind == PointerEventKind.down) {
                                viewport-keys.focus();
                            }
                        }
                    }
                }

                // The document scrollbar, over the right edge of the viewport.
                Rectangle {
                    x: parent.width - 12px;
                    width: 12px;
                    visible: root.sb-visible;
                    background: #0000000d;
                    Rectangle {
                        y: root.sb-thumb-y;
                        height: root.sb-thumb-h;
                        x: 2px;
                        width: parent.width - 4px;
                        border-radius: 4px;
                        background: thumb-area.pressed ? #6b7280 : #9ca3af;
                    }
                    thumb-area := TouchArea {
                        // Where in the thumb it was grabbed, so a drag does not snap its top to
                        // the cursor.
                        property <length> grab: 0px;
                        pointer-event(event) => {
                            if (event.kind == PointerEventKind.down) {
                                if (self.mouse-y >= root.sb-thumb-y && self.mouse-y < root.sb-thumb-y + root.sb-thumb-h) {
                                    self.grab = self.mouse-y - root.sb-thumb-y;
                                } else {
                                    self.grab = root.sb-thumb-h / 2;
                                    root.scrollbar_moved(self.mouse-y - self.grab, self.height);
                                }
                            }
                        }
                        moved => {
                            if (self.pressed) {
                                root.scrollbar_moved(self.mouse-y - self.grab, self.height);
                            }
                        }
                    }
                }

                if (root.total_pages == 0 && !root.has_error) : Text {
                    text: "Open a PDF with File > Open (Ctrl+O)";
                    color: Palette.foreground;
                    opacity: 0.6;
                    vertical-alignment: center;
                    horizontal-alignment: center;
                }
              }
              }
            }
            }

            // Status bar
            HorizontalLayout {
                height: 20px;
                Text {
                    text: root.status_text;
                    color: Palette.foreground;
                    opacity: 0.8;
                    font-size: 12px;
                    vertical-alignment: center;
                }
                Rectangle { horizontal-stretch: 1; }
                Text {
                    text: root.tiles_rendered + " tiles rendered / " + root.tiles_requested + " requested";
                    color: Palette.foreground;
                    opacity: 0.5;
                    font-size: 12px;
                    vertical-alignment: center;
                }
            }
        }
    }
}

/// Height of one thumbnail row in logical pixels; must match the `height` of the row in the
/// markup, because the rows in view are computed from the scroll position alone.
const THUMB_ROW_LOGICAL_PX: f32 = 232.0;
/// Width a thumbnail is rendered at, in logical pixels (the row's image box is a little wider).
const THUMB_WIDTH_LOGICAL_PX: f32 = 160.0;
/// Thumbnails kept as images; the rest of the rows are empty (Plan 0003, §4.3, row J).
const THUMB_CACHE: usize = 64;
/// Thumbnail requests in flight at once: a fast scroll through the list then wastes at most this
/// many rasters on rows the user has already passed.
const THUMB_INFLIGHT: usize = 4;
/// Rows above and below the visible ones that are requested ahead.
const THUMB_MARGIN_ROWS: usize = 2;

/// Delay between the last keystroke in the find field and the search it starts.
const FIND_DEBOUNCE_MS: u64 = 200;

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
    /// Theme and recent files, and where they are kept (Plan 0003, §4.7).
    settings: Settings,
    settings_path: Option<PathBuf>,
    /// The document an `Open` was sent for and has not been answered yet.
    opening: Option<PathBuf>,
    recent_model: Rc<VecModel<SharedString>>,
    /// Counts document opens; thumbnails carry it as their request id, so one of a previous
    /// document is recognised and dropped.
    doc_generation: u64,
    thumbs_model: Rc<VecModel<ThumbRow>>,
    /// Pages whose thumbnail is held, with the tick they were last in view (LRU, [`THUMB_CACHE`]).
    thumb_cache: HashMap<u32, u64>,
    thumb_tick: u64,
    thumb_inflight: HashSet<u32>,
    /// The thumbnail row marked as the current page.
    current_thumb: Option<usize>,
    outline_items: Vec<OutlineItem>,
    outline_expanded: Vec<bool>,
    /// Outline row -> index into `outline_items`.
    outline_visible: Vec<usize>,
    outline_model: Rc<VecModel<OutlineRow>>,
    /// The current search's hits (Plan 0003, §4.4) and its generation counter.
    search: SearchResults,
    search_generation: u64,
    search_query: String,
    hits_model: Rc<VecModel<HitView>>,
    hit_rows: Vec<(f32, f32, f32, f32, bool)>,
    /// When the current search was sent, for `t_first_hit` (benchmarks §9.3).
    search_sent: Option<Instant>,
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
        self.update_hits(ui);

        let (_, view_h) = self.view_size(ui);
        if let Some(page) = self.layout.page_near(self.pos.1 + view_h / 2.0) {
            ui.set_current_page(page as i32);
            // The field follows the page, except while the user is typing a page into it.
            let label = SharedString::from((page + 1).to_string());
            if !ui.get_page_field_focused() && ui.get_page_input() != label {
                ui.set_page_input(label);
            }
            self.set_current_thumb(ui, page);
        }
        self.update_scrollbar(ui);
    }

    /// Starts a search for `query` from the reader's page; an empty query just clears the results.
    fn start_search(&mut self, ui: &MainWindow, query: &str) {
        self.search_generation += 1;
        self.search.begin(self.search_generation);
        self.search_query = query.to_owned();
        self.hit_rows.clear();
        if query.trim().is_empty() || self.layout.is_empty() {
            let _ = self.cmd_tx.send(EngineCmd::CancelSearch);
            self.search_sent = None;
        } else {
            let from_page = self.current_page(ui) as u32;
            let _ = self.cmd_tx.send(EngineCmd::Search {
                query: query.to_owned(),
                from_page,
                generation: self.search_generation,
            });
            self.search_sent = Some(Instant::now());
            self.bench.search_started(self.search_generation, query);
        }
        ui.set_find_counter(SharedString::from(self.search.counter()));
        self.update_hits(ui);
    }

    /// Stops any search and forgets its results (a new document, or the find bar closed).
    fn cancel_search(&mut self) {
        self.search_generation += 1;
        self.search.begin(self.search_generation);
        self.search_sent = None;
        self.hit_rows.clear();
        self.hits_model.set_vec(Vec::new());
        let _ = self.cmd_tx.send(EngineCmd::CancelSearch);
    }

    /// A page of hits arrived. The first hit of a search is shown at once; later ones only update
    /// the counter and the highlights, so the view does not jump while the reader looks around.
    fn apply_hits(
        &mut self,
        ui: &MainWindow,
        generation: u64,
        page: u32,
        hits: Vec<Vec<pdf_core::Rect>>,
    ) {
        let had_current = self.search.current().is_some();
        if !self.search.add(generation, page, hits) {
            return;
        }
        if !had_current {
            if let Some(sent) = self.search_sent {
                self.bench.first_hit(sent.elapsed().as_millis());
            }
            if let Some(hit) = self.search.current_hit().cloned() {
                self.show_hit(ui, &hit);
            }
        }
        ui.set_find_counter(SharedString::from(self.search.counter()));
        self.hit_rows.clear();
        self.update_hits(ui);
    }

    /// The search of `generation` covered the document.
    fn search_done(&mut self, ui: &MainWindow, generation: u64, pages: u32, ms: u64) {
        if generation != self.search.generation() {
            return;
        }
        self.search.finish(generation);
        ui.set_find_counter(SharedString::from(self.search.counter()));
        let wall = self.search_sent.map(|sent| sent.elapsed().as_millis());
        self.bench.search_done(
            pages,
            ms,
            wall,
            self.search.hits().len(),
            self.search.stale(),
        );
    }

    /// Next or previous hit, wrapping.
    fn step_hit(&mut self, ui: &MainWindow, forward: bool) {
        let hit = if forward {
            self.search.next_hit().cloned()
        } else {
            self.search.previous_hit().cloned()
        };
        if let Some(hit) = hit {
            ui.set_find_counter(SharedString::from(self.search.counter()));
            self.show_hit(ui, &hit);
        }
    }

    /// Scrolls so a hit is in view: left alone if it already is, otherwise placed a third of the
    /// way down the viewport (and centred horizontally when it is off to a side).
    fn show_hit(&mut self, ui: &MainWindow, hit: &SearchHit) {
        let Some(slot) = self.layout.slot(hit.page as usize).copied() else {
            return;
        };
        let Some(rect) = hit.rects.first() else {
            return;
        };
        let s = self.scale_milli as f64 / 1000.0;
        let (x, y) = (
            slot.x as f64 + rect.x as f64 * s,
            slot.y as f64 + rect.y as f64 * s,
        );
        let (w, h) = (rect.width as f64 * s, rect.height as f64 * s);
        let (view_w, view_h) = self.view_size(ui);
        let mut target = self.pos;
        if y < self.pos.1 || y + h > self.pos.1 + view_h {
            target.1 = y - view_h / 3.0;
        }
        if x < self.pos.0 || x + w > self.pos.0 + view_w {
            target.0 = x + w / 2.0 - view_w / 2.0;
        }
        if target != self.pos {
            if (target.1 - self.pos.1).abs() > 2.0 * view_h {
                self.begin_epoch();
            }
            self.scroll_to(ui, target);
        }
        self.hit_rows.clear();
        self.update_hits(ui);
    }

    /// The highlight rectangles of the hits on the pages near the view. Hits are in page order, so
    /// the ones to draw are found by binary search, however many the document has.
    fn update_hits(&mut self, ui: &MainWindow) {
        let near = self.near_pages(ui);
        let hits = self.search.hits();
        let start = hits.partition_point(|h| (h.page as usize) < near.start);
        let end = hits.partition_point(|h| (h.page as usize) < near.end);
        let current = self.search.current();
        let s = self.scale_milli as f64 / 1000.0;
        let sf = self.sf() as f32;
        let mut rows = Vec::new();
        for (i, hit) in hits.iter().enumerate().take(end).skip(start) {
            let Some(slot) = self.layout.slot(hit.page as usize) else {
                continue;
            };
            for rect in &hit.rects {
                rows.push((
                    self.canvas_x(slot.x as f64 + rect.x as f64 * s),
                    self.canvas_y(slot.y as f64 + rect.y as f64 * s),
                    (rect.width as f64 * s) as f32 / sf,
                    (rect.height as f64 * s) as f32 / sf,
                    Some(i) == current,
                ));
            }
        }
        if rows == self.hit_rows {
            return;
        }
        let views: Vec<HitView> = rows
            .iter()
            .map(|&(x, y, w, h, current)| HitView {
                x,
                y,
                w,
                h,
                current,
            })
            .collect();
        self.hits_model.set_vec(views);
        self.hit_rows = rows;
    }

    /// Ctrl+F: show the find bar (its field takes the focus); search again if it has text.
    fn find_open(&mut self, ui: &MainWindow) {
        ui.set_find_visible(true);
    }

    /// Esc or Close: hide the find bar, stop the search, drop the highlights.
    fn find_close(&mut self, ui: &MainWindow) {
        ui.set_find_visible(false);
        self.cancel_search();
        ui.set_find_counter(SharedString::new());
    }

    /// One empty thumbnail row per page, and nothing held or in flight.
    fn reset_thumbs(&mut self, pages: usize) {
        let rows: Vec<ThumbRow> = (0..pages)
            .map(|page| ThumbRow {
                image: Image::default(),
                has_image: false,
                label: SharedString::from((page + 1).to_string()),
                current: false,
            })
            .collect();
        self.thumbs_model.set_vec(rows);
        self.thumb_cache.clear();
        self.thumb_inflight.clear();
        self.current_thumb = None;
    }

    /// Thumbnail rows in view plus the margin, in order from the top; empty while the thumbnail
    /// list is not shown.
    fn thumb_range(&self, ui: &MainWindow) -> std::ops::Range<usize> {
        let rows = self.thumbs_model.row_count();
        if !ui.get_sidebar_visible() || ui.get_sidebar_tab() != 0 || rows == 0 {
            return 0..0;
        }
        let top = (-ui.get_thumb_scroll_y()).max(0.0) / THUMB_ROW_LOGICAL_PX;
        let shown = (ui.get_view_h() / THUMB_ROW_LOGICAL_PX).ceil() as usize + 1;
        let first = (top.floor() as usize).saturating_sub(THUMB_MARGIN_ROWS);
        let last = (top.floor() as usize + shown + THUMB_MARGIN_ROWS).min(rows);
        first.min(last)..last
    }

    /// Requests the missing thumbnails of the rows in view, nearest the top first, keeping at most
    /// [`THUMB_INFLIGHT`] in flight.
    fn pump_thumbs(&mut self, ui: &MainWindow) {
        self.thumb_tick += 1;
        let tick = self.thumb_tick;
        let width_px = (THUMB_WIDTH_LOGICAL_PX * self.scale_factor).round() as u32;
        for row in self.thumb_range(ui) {
            let page = row as u32;
            if let Some(seen) = self.thumb_cache.get_mut(&page) {
                *seen = tick;
                continue;
            }
            if self.thumb_inflight.len() >= THUMB_INFLIGHT {
                break;
            }
            if self.thumb_inflight.insert(page) {
                let _ = self.cmd_tx.send(EngineCmd::RenderThumb {
                    page,
                    width_px,
                    request_id: self.doc_generation,
                });
            }
        }
    }

    /// A thumbnail arrived: show it, keep at most [`THUMB_CACHE`], and ask for the next one.
    fn apply_thumb(&mut self, ui: &MainWindow, page: u32, generation: u64, bitmap: &Bitmap) {
        if generation != self.doc_generation {
            return;
        }
        self.thumb_inflight.remove(&page);
        let row = page as usize;
        if let Some(mut data) = self.thumbs_model.row_data(row) {
            data.image = to_image(bitmap);
            data.has_image = true;
            self.thumbs_model.set_row_data(row, data);
            self.thumb_tick += 1;
            self.thumb_cache.insert(page, self.thumb_tick);
        }
        while self.thumb_cache.len() > THUMB_CACHE {
            let Some((&oldest, _)) = self
                .thumb_cache
                .iter()
                .min_by_key(|(page, tick)| (**tick, **page))
            else {
                break;
            };
            self.thumb_cache.remove(&oldest);
            if let Some(mut data) = self.thumbs_model.row_data(oldest as usize) {
                data.image = Image::default();
                data.has_image = false;
                self.thumbs_model.set_row_data(oldest as usize, data);
            }
        }
        self.bench.thumbs_held(self.thumb_cache.len());
        self.pump_thumbs(ui);
    }

    /// Marks the current page's thumbnail and, when the list is shown, scrolls it into view.
    fn set_current_thumb(&mut self, ui: &MainWindow, page: usize) {
        if self.current_thumb == Some(page) {
            return;
        }
        for (row, current) in [(self.current_thumb, false), (Some(page), true)] {
            if let Some(row) = row
                && let Some(mut data) = self.thumbs_model.row_data(row)
            {
                data.current = current;
                self.thumbs_model.set_row_data(row, data);
            }
        }
        self.current_thumb = Some(page);
        if !ui.get_sidebar_visible() || ui.get_sidebar_tab() != 0 {
            return;
        }
        let top = page as f32 * THUMB_ROW_LOGICAL_PX;
        let view = ui.get_view_h();
        let scrolled = -ui.get_thumb_scroll_y();
        if top < scrolled || top + THUMB_ROW_LOGICAL_PX > scrolled + view {
            ui.set_thumb_scroll_y(-(top - (view - THUMB_ROW_LOGICAL_PX) / 2.0).max(0.0));
            self.pump_thumbs(ui);
        }
    }

    /// The document's outline arrived: every node starts collapsed, so the top level shows.
    fn set_outline(&mut self, items: Vec<OutlineItem>) {
        self.outline_expanded = vec![false; items.len()];
        self.outline_items = items;
        self.rebuild_outline();
    }

    /// The rows the tree shows: an entry is hidden while any ancestor is collapsed.
    fn rebuild_outline(&mut self) {
        let mut rows = Vec::new();
        self.outline_visible.clear();
        for (i, expandable) in visible_outline(&self.outline_items, &self.outline_expanded) {
            let item = &self.outline_items[i];
            let expanded = self.outline_expanded.get(i).copied().unwrap_or(false);
            self.outline_visible.push(i);
            rows.push(OutlineRow {
                title: SharedString::from(item.title.as_str()),
                depth: item.depth as i32,
                expandable,
                expanded,
                enabled: item.page.is_some(),
            });
        }
        self.outline_model.set_vec(rows);
    }

    /// Expands or collapses an outline row.
    fn outline_toggled(&mut self, row: usize) {
        if let Some(&item) = self.outline_visible.get(row)
            && let Some(expanded) = self.outline_expanded.get_mut(item)
        {
            *expanded = !*expanded;
            self.rebuild_outline();
        }
    }

    /// Goes to an outline row's destination: its page, at the position it names.
    ///
    /// A click on the title of a collapsed entry with children also opens it, so the reader does
    /// not have to aim for the chevron; it never closes one, so a click meant to navigate cannot
    /// hide the entries below. Closing is the chevron's job.
    fn outline_clicked(&mut self, ui: &MainWindow, row: usize) {
        let Some(&index) = self.outline_visible.get(row) else {
            return;
        };
        if expand_on_click(&self.outline_items, &mut self.outline_expanded, index) {
            self.rebuild_outline();
        }
        if let Some(item) = self.outline_items.get(index)
            && let Some(page) = item.page
        {
            let top = item.top;
            self.go_to_dest(ui, page as usize, top);
        }
    }

    /// Jumps to `top` points below the top of `page` (MuPDF's page space), or to the page's top.
    fn go_to_dest(&mut self, ui: &MainWindow, page: usize, top: Option<f32>) {
        let Some(slot) = self.layout.slot(page).copied() else {
            return;
        };
        let y = match top {
            Some(top) => slot.y as f64 + top as f64 * self.scale_milli as f64 / 1000.0,
            None => page_top(&self.layout, page).unwrap_or(0.0),
        };
        self.begin_epoch();
        self.pos.1 = y;
        self.clamp_pos(ui);
        self.reanchor(ui);
        self.refresh(ui);
    }

    /// Shows or hides the sidebar, and remembers the choice.
    fn toggle_sidebar(&mut self, ui: &MainWindow) {
        self.settings.sidebar = !self.settings.sidebar;
        ui.set_sidebar_visible(self.settings.sidebar);
        self.save_settings();
        self.current_thumb = None;
        self.refresh(ui);
        self.pump_thumbs(ui);
    }

    /// Places the scrollbar thumb for the whole document (Plan 0002, §4.2).
    fn update_scrollbar(&self, ui: &MainWindow) {
        let (_, view_h) = self.view_size(ui);
        let doc_h = self.layout.height() as f64;
        let track = ui.get_view_h() as f64;
        let visible = doc_h > view_h && track > 0.0;
        ui.set_sb_visible(visible);
        if !visible {
            return;
        }
        let (offset, len) = scrollbar_thumb(
            self.pos.1,
            view_h,
            doc_h,
            track,
            SCROLLBAR_MIN_THUMB_LOGICAL_PX,
        );
        ui.set_sb_thumb_y(offset as f32);
        ui.set_sb_thumb_h(len as f32);
    }

    /// Moves to `pos_y` for a navigation action: a jump - further than two viewports - starts an
    /// epoch, because what is pending belongs to a place the user has left (Plan 0002, §4.4); a
    /// short step scrolls like the wheel.
    fn navigate_to(&mut self, ui: &MainWindow, pos_y: f64) {
        let (_, view_h) = self.view_size(ui);
        if (pos_y - self.pos.1).abs() > 2.0 * view_h {
            self.begin_epoch();
        }
        self.scroll_to(ui, (self.pos.0, pos_y));
    }

    /// One keyboard navigation action.
    fn nav(&mut self, ui: &MainWindow, action: NavAction) {
        let (_, view_h) = self.view_size(ui);
        let target = scroll_target(
            action,
            self.pos.1,
            view_h,
            self.layout.height() as f64,
            LINE_STEP_LOGICAL_PX * self.sf(),
        );
        self.navigate_to(ui, target);
    }

    /// The scrollbar thumb was dragged to `offset` in a track of `track` logical pixels.
    fn scrollbar_moved(&mut self, ui: &MainWindow, offset: f32, track: f32) {
        let (_, view_h) = self.view_size(ui);
        let sf = self.sf();
        // The thumb math works in the track's logical pixels; the document in device pixels.
        let pos = scrollbar_pos(
            offset as f64,
            view_h / sf,
            self.layout.height() as f64 / sf,
            track as f64,
            SCROLLBAR_MIN_THUMB_LOGICAL_PX,
        ) * sf;
        self.navigate_to(ui, pos);
    }

    /// Fit-width or fit-page for the current page, anchored at that page's top (Plan 0002, §4.5).
    fn fit(&mut self, ui: &MainWindow, whole_page: bool) {
        let page = self.current_page(ui);
        let Some(size) = self.page_sizes.get(page).copied() else {
            return;
        };
        let (view_w, view_h) = self.view_size(ui);
        let gap = self.gap_px();
        let milli = if whole_page {
            fit_page_milli(&size, view_w, view_h, gap)
        } else {
            fit_width_milli(&size, view_w, gap)
        };
        self.zoom_to_page(ui, milli, page);
    }

    /// Zooms to `milli` with `page`'s top at the viewport top.
    fn zoom_to_page(&mut self, ui: &MainWindow, milli: u32, page: usize) {
        let milli = milli.clamp(ZOOM_MIN_MILLI, ZOOM_MAX_MILLI);
        if milli != self.scale_milli {
            self.layout = DocLayout::new(&self.page_sizes, milli, self.gap_px());
            self.scale_milli = milli;
            self.bench.level_changed(milli, "fit");
            ui.set_zoom_percent(milli as f32 / 10.0);
        }
        self.begin_epoch();
        self.pos.1 = page_top(&self.layout, page).unwrap_or(0.0);
        self.clamp_pos(ui);
        self.reanchor(ui);
        self.refresh(ui);
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

    /// Asks the engine to open `path`, replacing the document on screen (Plan 0003, §4.2).
    ///
    /// The current document stops requesting work at once - a new epoch, no rows, an empty layout -
    /// because the engine answers commands in order: a tile asked for the old document after the
    /// `Open` would be prepared against the new one. The page table is kept until the answer, so a
    /// failed open can put the old document back.
    fn open_path(&mut self, ui: &MainWindow, path: PathBuf) {
        // A command-line path is relative to wherever the app was started; the recent list has to
        // work from anywhere. `absolute` neither touches the file system nor adds Windows' `\\?\`.
        let path = std::path::absolute(&path).unwrap_or(path);
        self.doc_generation += 1;
        self.cancel_search();
        ui.set_find_visible(false);
        ui.set_find_text(SharedString::new());
        ui.set_find_counter(SharedString::new());
        self.search_query.clear();
        self.reset_thumbs(0);
        self.set_outline(Vec::new());
        self.begin_epoch();
        self.release_all(ui);
        self.layout = DocLayout::default();
        self.pages_model.set_vec(Vec::new());
        ui.set_has_error(false);
        ui.set_is_loading(true);
        ui.set_status_text(SharedString::from(format!(
            "Opening '{}'...",
            path.display()
        )));
        let _ = self.cmd_tx.send(EngineCmd::Open {
            path: path.clone(),
            password: None,
        });
        self.opening = Some(path);
    }

    /// The engine opened the document `open_path` asked for: remember it.
    fn document_opened(&mut self, ui: &MainWindow) {
        // The find bar belongs to the document it searched: a new document starts without it.
        let Some(path) = self.opening.take() else {
            return;
        };
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        ui.set_doc_title(SharedString::from(name));
        self.settings.opened(&path);
        self.save_settings();
        self.update_recent();
    }

    /// The engine could not open the document: the one that was showing is still open there, so
    /// it comes back with its page table and scroll position.
    fn open_failed(&mut self, ui: &MainWindow) {
        if self.opening.take().is_none() || self.page_sizes.is_empty() {
            return;
        }
        self.layout = DocLayout::new(&self.page_sizes, self.scale_milli, self.gap_px());
        // The old document is still the engine's; its outline went with the open and is not
        // resent, so the sidebar keeps only the thumbnails, which are re-rendered.
        self.reset_thumbs(self.page_sizes.len());
        self.clamp_pos(ui);
        self.reanchor(ui);
        self.refresh(ui);
    }

    /// The File menu's recent list, from the settings.
    fn update_recent(&self) {
        let entries: Vec<SharedString> = self
            .settings
            .recent
            .iter()
            .map(|path| SharedString::from(path.display().to_string()))
            .collect();
        self.recent_model.set_vec(entries);
    }

    /// The theme chosen in the View menu: applied at once and remembered.
    fn choose_theme(&mut self, ui: &MainWindow, index: i32) {
        self.settings.theme = match index {
            1 => Theme::Light,
            2 => Theme::Dark,
            _ => Theme::System,
        };
        ui.set_theme(theme_index(self.settings.theme));
        self.save_settings();
    }

    fn save_settings(&self) {
        if let Some(path) = &self.settings_path {
            save_settings(path, &self.settings);
        }
    }

    /// A document was opened: forget everything and show the top of page 1 at 100%.
    fn set_document(&mut self, ui: &MainWindow, page_sizes: Vec<PageSize>) {
        self.page_sizes = page_sizes;
        self.scale_milli = ZOOM_DEFAULT_MILLI;
        self.layout = DocLayout::new(&self.page_sizes, self.scale_milli, self.gap_px());
        self.reset_thumbs(self.page_sizes.len());
        ui.set_thumb_scroll_y(0.0);
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
        let Some(top) = page_top(&self.layout, page) else {
            return;
        };
        self.begin_epoch();
        self.pos.1 = top;
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
            state.pump_thumbs(ui);
        }
    });
}

/// Where the settings file lives: the platform's per-user configuration directory, from the
/// environment alone (Plan 0003, §3.8). `None` when the environment names none, in which case the
/// app runs with defaults and remembers nothing.
fn settings_path() -> Option<PathBuf> {
    let dir = if cfg!(windows) {
        PathBuf::from(env::var_os("APPDATA")?).join("PODEFA")
    } else if cfg!(target_os = "macos") {
        PathBuf::from(env::var_os("HOME")?).join("Library/Application Support/PODEFA")
    } else {
        match env::var_os("XDG_CONFIG_HOME").filter(|dir| !dir.is_empty()) {
            Some(dir) => PathBuf::from(dir).join("podefa"),
            None => PathBuf::from(env::var_os("HOME")?).join(".config/podefa"),
        }
    };
    Some(dir.join("settings.txt"))
}

/// The settings in `path`, or the defaults when it is missing or unreadable.
fn load_settings(path: &Path) -> Settings {
    std::fs::read_to_string(path)
        .map(|text| Settings::parse(&text))
        .unwrap_or_default()
}

/// Writes the settings through a temporary file and a rename, so a crash mid-write never leaves a
/// half-written file. A failure is reported on stderr and otherwise ignored: losing a recent-files
/// entry must not interrupt reading.
fn save_settings(path: &Path, settings: &Settings) {
    let write = || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("txt.tmp");
        std::fs::write(&tmp, settings.serialize())?;
        std::fs::rename(&tmp, path)
    };
    if let Err(err) = write() {
        eprintln!(
            "podefa: could not save settings to {}: {err}",
            path.display()
        );
    }
}

fn theme_index(theme: Theme) -> i32 {
    match theme {
        Theme::System => 0,
        Theme::Light => 1,
        Theme::Dark => 2,
    }
}

/// The document a drag carries: its first file whose extension is `.pdf`, any case. `None` for a
/// drag without file paths, which the `DropArea` then refuses.
fn dropped_document(data: &slint::DataTransfer) -> Option<PathBuf> {
    let paths = data.file_paths().ok()?;
    paths
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
        })
        .map(Path::to_path_buf)
        .next()
}

/// Asks for a document with the platform's file dialog (Plan 0003, §3.1). Modal: it blocks the
/// UI thread until the user answers, as a modal dialog does.
fn pick_document() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Open")
        .add_filter("PDF documents", &["pdf", "PDF"])
        .add_filter("All files", &["*"])
        .pick_file()
}

/// The outline entries a tree with these `expanded` flags shows, in order, each with whether it has
/// children: an entry is hidden while any ancestor is collapsed.
fn visible_outline(items: &[OutlineItem], expanded: &[bool]) -> Vec<(usize, bool)> {
    let mut rows = Vec::new();
    let mut collapsed_at: Option<u32> = None;
    for (i, item) in items.iter().enumerate() {
        if let Some(depth) = collapsed_at {
            if item.depth > depth {
                continue;
            }
            collapsed_at = None;
        }
        let expandable = items.get(i + 1).is_some_and(|next| next.depth > item.depth);
        if expandable && !expanded.get(i).copied().unwrap_or(false) {
            collapsed_at = Some(item.depth);
        }
        rows.push((i, expandable));
    }
    rows
}

/// Opens entry `index` if it has children and is closed; returns whether anything changed.
fn expand_on_click(items: &[OutlineItem], expanded: &mut [bool], index: usize) -> bool {
    let has_children = items
        .get(index)
        .zip(items.get(index + 1))
        .is_some_and(|(item, next)| next.depth > item.depth);
    match expanded.get_mut(index) {
        Some(open) if has_children && !*open => {
            *open = true;
            true
        }
        _ => false,
    }
}

/// What a key on the viewport does: a scroll action, a zoom, or nothing (the event is rejected and
/// goes on to the parent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyCommand {
    Scroll(NavAction),
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Open,
    ToggleSidebar,
    Find,
    FindNext,
    FindPrevious,
    FindClose,
}

/// Maps a Slint key event onto a command (Plan 0002, §4.6). `text` is what Slint reports: a
/// printable character, or one of the private-use code points of [`Key`].
fn key_command(text: &str, ctrl: bool, shift: bool) -> Option<KeyCommand> {
    let mut chars = text.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return None;
    };
    if ctrl {
        return match c {
            '+' | '=' => Some(KeyCommand::ZoomIn),
            '-' => Some(KeyCommand::ZoomOut),
            '0' => Some(KeyCommand::ZoomReset),
            'o' | 'O' => Some(KeyCommand::Open),
            'b' | 'B' => Some(KeyCommand::ToggleSidebar),
            'f' | 'F' => Some(KeyCommand::Find),
            _ => None,
        };
    }
    if c == char::from(Key::F4) {
        return Some(KeyCommand::ToggleSidebar);
    }
    if c == char::from(Key::F3) {
        return Some(if shift {
            KeyCommand::FindPrevious
        } else {
            KeyCommand::FindNext
        });
    }
    if c == char::from(Key::Escape) {
        return Some(KeyCommand::FindClose);
    }
    let action = match c {
        ' ' if shift => NavAction::PageUp,
        ' ' => NavAction::PageDown,
        c if c == char::from(Key::UpArrow) => NavAction::LineUp,
        c if c == char::from(Key::DownArrow) => NavAction::LineDown,
        c if c == char::from(Key::PageUp) => NavAction::PageUp,
        c if c == char::from(Key::PageDown) => NavAction::PageDown,
        c if c == char::from(Key::Home) => NavAction::Home,
        c if c == char::from(Key::End) => NavAction::End,
        _ => return None,
    };
    Some(KeyCommand::Scroll(action))
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
    let recent_model = Rc::new(VecModel::<SharedString>::default());
    main_window.set_recent_files(ModelRc::from(recent_model.clone()));
    let settings_path = settings_path();
    let settings = settings_path
        .as_deref()
        .map(load_settings)
        .unwrap_or_default();
    main_window.set_theme(theme_index(settings.theme));
    main_window.set_sidebar_visible(settings.sidebar);
    let thumbs_model = Rc::new(VecModel::<ThumbRow>::default());
    main_window.set_thumbs(ModelRc::from(thumbs_model.clone()));
    let outline_model = Rc::new(VecModel::<OutlineRow>::default());
    main_window.set_outline_rows(ModelRc::from(outline_model.clone()));
    let hits_model = Rc::new(VecModel::<HitView>::default());
    main_window.set_hit_rects(ModelRc::from(hits_model.clone()));

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
            settings,
            settings_path,
            opening: None,
            recent_model,
            doc_generation: 0,
            thumbs_model,
            thumb_cache: HashMap::new(),
            thumb_tick: 0,
            thumb_inflight: HashSet::new(),
            current_thumb: None,
            outline_items: Vec::new(),
            outline_expanded: Vec::new(),
            outline_visible: Vec::new(),
            outline_model,
            search: SearchResults::default(),
            search_generation: 0,
            search_query: String::new(),
            hits_model,
            hit_rows: Vec::new(),
            search_sent: None,
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
                    with_state(|state| {
                        state.set_document(&ui, page_sizes);
                        state.document_opened(&ui);
                    });
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
                EngineEvent::ThumbRendered {
                    page,
                    request_id,
                    bitmap,
                } => {
                    with_state(|state| state.apply_thumb(&ui, page, request_id, &bitmap));
                }
                EngineEvent::SearchHits {
                    generation,
                    page,
                    hits,
                } => {
                    with_state(|state| state.apply_hits(&ui, generation, page, hits));
                }
                EngineEvent::SearchDone {
                    generation,
                    pages,
                    ms,
                } => {
                    with_state(|state| state.search_done(&ui, generation, pages, ms));
                }
                // Selection arrives with the selection UI (Plan 0003 Phase 4).
                EngineEvent::Selection { .. } => {}
                EngineEvent::Outline { items } => {
                    with_state(|state| state.set_outline(items));
                }
                EngineEvent::TileAborted { ms, .. } => {
                    with_state(|state| state.bench.aborted(ms));
                }
                EngineEvent::Error { message } => {
                    ui.set_has_error(true);
                    ui.set_error_text(SharedString::from(message.clone()));
                    ui.set_status_text(SharedString::from(format!("Error: {message}")));
                    ui.set_is_loading(false);
                    with_state(|state| state.open_failed(&ui));
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

    {
        let weak = main_window.as_weak();
        main_window.on_nav_key(move |text, ctrl, shift| {
            let Some(ui) = weak.upgrade() else {
                return false;
            };
            let Some(command) = key_command(&text, ctrl, shift) else {
                return false;
            };
            let (ax, ay) = viewport_center(&ui);
            with_state(|state| match command {
                KeyCommand::Scroll(action) => state.nav(&ui, action),
                KeyCommand::ZoomIn => {
                    let next = zoom_in_milli(state.scale_milli);
                    state.zoom(&ui, next, ax, ay);
                }
                KeyCommand::ZoomOut => {
                    let next = zoom_out_milli(state.scale_milli);
                    state.zoom(&ui, next, ax, ay);
                }
                KeyCommand::ZoomReset => state.zoom(&ui, ZOOM_DEFAULT_MILLI, ax, ay),
                KeyCommand::ToggleSidebar => state.toggle_sidebar(&ui),
                KeyCommand::Find => state.find_open(&ui),
                KeyCommand::FindNext => state.step_hit(&ui, true),
                KeyCommand::FindPrevious => state.step_hit(&ui, false),
                KeyCommand::FindClose => state.find_close(&ui),
                KeyCommand::Open => {
                    if let Some(path) = pick_document() {
                        state.open_path(&ui, path);
                    }
                }
            });
            true
        });
    }

    {
        let weak = main_window.as_weak();
        main_window.on_wheel_zoom(move |delta, x, y| {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| {
                    let next = wheel_zoom_milli(state.scale_milli, delta as f64);
                    state.zoom(&ui, next, x, y);
                });
            }
        });
    }

    {
        let weak = main_window.as_weak();
        main_window.on_go_to_page(move |text| {
            if let Some(ui) = weak.upgrade() {
                // 1-based, as the field shows it; anything that is not a page number is ignored
                // and the field goes back to the current page on the next refresh.
                let page = text.trim().parse::<usize>().ok();
                with_state(|state| {
                    if let Some(page) = page.filter(|&p| p >= 1 && p <= state.layout.len()) {
                        state.go_to_page(&ui, page - 1);
                    }
                    let current = state.current_page(&ui) + 1;
                    ui.set_page_input(SharedString::from(current.to_string()));
                });
            }
        });
    }

    {
        let weak = main_window.as_weak();
        main_window.on_scrollbar_moved(move |offset, track| {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.scrollbar_moved(&ui, offset, track));
            }
        });
    }

    {
        let weak = main_window.as_weak();
        main_window.on_fit_width(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.fit(&ui, false));
            }
        });
    }

    {
        let weak = main_window.as_weak();
        main_window.on_fit_page(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.fit(&ui, true));
            }
        });
    }

    {
        let weak = main_window.as_weak();
        main_window.on_open_file(move || {
            if let Some(ui) = weak.upgrade()
                && let Some(path) = pick_document()
            {
                with_state(|state| state.open_path(&ui, path));
            }
        });
    }

    {
        let weak = main_window.as_weak();
        main_window.on_open_recent(move |index| {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| {
                    let path = usize::try_from(index)
                        .ok()
                        .and_then(|i| state.settings.recent.get(i).cloned());
                    if let Some(path) = path {
                        state.open_path(&ui, path);
                    }
                });
            }
        });
    }

    {
        let weak = main_window.as_weak();
        main_window.on_choose_theme(move |index| {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.choose_theme(&ui, index));
            }
        });
    }

    {
        let weak = main_window.as_weak();
        main_window.on_toggle_sidebar(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.toggle_sidebar(&ui));
            }
        });
    }

    {
        let weak = main_window.as_weak();
        let pump = move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.pump_thumbs(&ui));
            }
        };
        main_window.on_thumbs_moved(pump.clone());
        main_window.on_sidebar_tab_changed(pump);
    }

    {
        let weak = main_window.as_weak();
        main_window.on_thumb_clicked(move |row| {
            if let Some(ui) = weak.upgrade()
                && let Ok(page) = usize::try_from(row)
            {
                with_state(|state| state.go_to_page(&ui, page));
            }
        });
    }

    {
        let weak = main_window.as_weak();
        main_window.on_outline_clicked(move |row| {
            if let Some(ui) = weak.upgrade()
                && let Ok(row) = usize::try_from(row)
            {
                with_state(|state| state.outline_clicked(&ui, row));
            }
        });
    }

    main_window.on_outline_toggled(|row| {
        if let Ok(row) = usize::try_from(row) {
            with_state(|state| state.outline_toggled(row));
        }
    });

    {
        let weak = main_window.as_weak();
        main_window.on_find_open(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.find_open(&ui));
            }
        });
    }

    {
        // Typing restarts a single-shot timer; the search starts when the typing pauses.
        let weak = main_window.as_weak();
        let debounce = Rc::new(slint::Timer::default());
        main_window.on_find_changed(move |text| {
            let weak = weak.clone();
            let query = text.to_string();
            debounce.start(
                slint::TimerMode::SingleShot,
                std::time::Duration::from_millis(FIND_DEBOUNCE_MS),
                move || {
                    if let Some(ui) = weak.upgrade() {
                        with_state(|state| state.start_search(&ui, &query));
                    }
                },
            );
        });
    }

    {
        let weak = main_window.as_weak();
        main_window.on_find_next(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| {
                    // Enter on a query that has not been searched yet (typed faster than the
                    // debounce) searches it first.
                    let query = ui.get_find_text().to_string();
                    if query != state.search_query {
                        state.start_search(&ui, &query);
                    } else {
                        state.step_hit(&ui, true);
                    }
                });
            }
        });
    }

    {
        let weak = main_window.as_weak();
        main_window.on_find_previous(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.step_hit(&ui, false));
            }
        });
    }

    {
        let weak = main_window.as_weak();
        main_window.on_find_close(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.find_close(&ui));
                ui.invoke_focus_viewport();
            }
        });
    }

    main_window.on_quit(|| {
        let _ = slint::quit_event_loop();
    });

    main_window.on_can_open_drop(|data| dropped_document(&data).is_some());

    {
        let weak = main_window.as_weak();
        main_window.on_open_drop(move |data| {
            if let Some(ui) = weak.upgrade()
                && let Some(path) = dropped_document(&data)
            {
                with_state(|state| state.open_path(&ui, path));
            }
        });
    }

    with_state(|state| state.update_recent());

    // CLI argument handling: open the file passed as the first argument, through the same path as
    // the menu, so it lands in the recent list too.
    if let Some(path_arg) = path {
        let weak = main_window.as_weak();
        if let Some(ui) = weak.upgrade() {
            with_state(|state| state.open_path(&ui, PathBuf::from(path_arg)));
        }
    }

    main_window.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(k: Key) -> String {
        char::from(k).to_string()
    }

    #[test]
    fn navigation_keys_map_to_scroll_actions() {
        let cases = [
            (key(Key::UpArrow), false, NavAction::LineUp),
            (key(Key::DownArrow), false, NavAction::LineDown),
            (key(Key::PageUp), false, NavAction::PageUp),
            (key(Key::PageDown), false, NavAction::PageDown),
            (key(Key::Home), false, NavAction::Home),
            (key(Key::End), false, NavAction::End),
            (" ".to_owned(), false, NavAction::PageDown),
            (" ".to_owned(), true, NavAction::PageUp),
        ];
        for (text, shift, action) in cases {
            assert_eq!(
                key_command(&text, false, shift),
                Some(KeyCommand::Scroll(action)),
                "{text:?}"
            );
        }
    }

    #[test]
    fn a_drop_opens_its_first_pdf() {
        let mut data = slint::DataTransfer::default();
        data.set_file_paths(["/home/u/notes.txt", "/home/u/Report.PDF", "/home/u/b.pdf"]);
        assert_eq!(
            dropped_document(&data),
            Some(PathBuf::from("/home/u/Report.PDF"))
        );

        let mut none = slint::DataTransfer::default();
        none.set_file_paths(["/home/u/image.png"]);
        assert_eq!(dropped_document(&none), None);

        let mut text = slint::DataTransfer::default();
        text.set_plain_text("/home/u/b.pdf".into());
        assert_eq!(dropped_document(&text), None, "text is not a file");
        assert_eq!(dropped_document(&slint::DataTransfer::default()), None);
    }

    #[test]
    fn f4_and_ctrl_b_toggle_the_sidebar() {
        assert_eq!(
            key_command(&key(Key::F4), false, false),
            Some(KeyCommand::ToggleSidebar)
        );
        assert_eq!(
            key_command("b", true, false),
            Some(KeyCommand::ToggleSidebar)
        );
        assert_eq!(key_command("b", false, false), None);
    }

    fn entry(depth: u32, title: &str) -> OutlineItem {
        OutlineItem {
            depth,
            title: title.to_owned(),
            page: Some(0),
            top: None,
        }
    }

    #[test]
    fn collapsed_entries_hide_their_descendants_only() {
        let items = [
            entry(0, "1"),
            entry(1, "1.1"),
            entry(2, "1.1.1"),
            entry(1, "1.2"),
            entry(0, "2"),
        ];
        let titles = |expanded: &[bool]| -> Vec<(String, bool)> {
            visible_outline(&items, expanded)
                .into_iter()
                .map(|(i, expandable)| (items[i].title.clone(), expandable))
                .collect()
        };
        let row = |t: &str, e: bool| (t.to_owned(), e);
        assert_eq!(titles(&[false; 5]), vec![row("1", true), row("2", false)]);
        assert_eq!(
            titles(&[true, false, false, false, false]),
            vec![
                row("1", true),
                row("1.1", true),
                row("1.2", false),
                row("2", false)
            ]
        );
        assert_eq!(titles(&[true; 5]).len(), 5);
        // A collapsed child hides only its own subtree, even under an open parent.
        assert_eq!(
            titles(&[true, false, true, true, true]),
            vec![
                row("1", true),
                row("1.1", true),
                row("1.2", false),
                row("2", false)
            ]
        );
    }

    #[test]
    fn a_title_click_opens_a_closed_parent_and_never_closes_one() {
        let items = [entry(0, "1"), entry(1, "1.1"), entry(0, "2")];
        let mut expanded = vec![false; 3];
        assert!(
            expand_on_click(&items, &mut expanded, 0),
            "closed parent opens"
        );
        assert_eq!(expanded, [true, false, false]);
        assert!(
            !expand_on_click(&items, &mut expanded, 0),
            "an open parent stays open"
        );
        assert_eq!(expanded, [true, false, false]);
        assert!(
            !expand_on_click(&items, &mut expanded, 1),
            "a leaf has nothing to open"
        );
        assert!(
            !expand_on_click(&items, &mut expanded, 2),
            "the last entry is a leaf"
        );
        assert!(
            !expand_on_click(&items, &mut expanded, 9),
            "out of range is ignored"
        );
    }

    #[test]
    fn find_keys() {
        assert_eq!(key_command("f", true, false), Some(KeyCommand::Find));
        assert_eq!(
            key_command(&key(Key::F3), false, false),
            Some(KeyCommand::FindNext)
        );
        assert_eq!(
            key_command(&key(Key::F3), false, true),
            Some(KeyCommand::FindPrevious)
        );
        assert_eq!(
            key_command(&key(Key::Escape), false, false),
            Some(KeyCommand::FindClose)
        );
        assert_eq!(key_command("f", false, false), None);
    }

    #[test]
    fn ctrl_o_opens() {
        assert_eq!(key_command("o", true, false), Some(KeyCommand::Open));
        assert_eq!(key_command("o", false, false), None);
    }

    #[test]
    fn zoom_shortcuts_need_ctrl() {
        assert_eq!(key_command("+", true, false), Some(KeyCommand::ZoomIn));
        assert_eq!(key_command("=", true, false), Some(KeyCommand::ZoomIn));
        assert_eq!(key_command("-", true, false), Some(KeyCommand::ZoomOut));
        assert_eq!(key_command("0", true, false), Some(KeyCommand::ZoomReset));
        assert_eq!(key_command("+", false, false), None);
        assert_eq!(key_command("a", false, false), None);
        assert_eq!(key_command("", false, false), None);
        // Ctrl with a navigation key is left alone (it is not one of ours).
        assert_eq!(key_command(&key(Key::Home), true, false), None);
    }
}
