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

use pdf_core::annot::{
    AnnotInfo, AnnotKind, EditOp, INK_SIMPLIFY_PT, INK_WIDTHS, MarkupKind, PALETTE, SaveState,
    note_rect, simplify,
};
use pdf_core::nav::{
    LINE_STEP_LOGICAL_PX, NavAction, SCROLLBAR_MIN_THUMB_LOGICAL_PX, fit_page_milli,
    fit_width_milli, page_top, scroll_target, scrollbar_pos, scrollbar_thumb, wheel_zoom_milli,
};
use pdf_core::pages::{PageMap, PageOp, follow, kept_rows};
use pdf_core::search::{SearchHit, SearchResults};
use pdf_core::select::{PagePoint, PageSelection, SelectMode};
use pdf_core::settings::{Settings, Theme};
use pdf_core::{
    Bitmap, CANVAS_HALF_LOGICAL_PX, CanvasWindow, DocLayout, EngineCmd, EngineEvent, Margins,
    PAGE_GAP_LOGICAL_PX, PageSize, REANCHOR_MARGIN_LOGICAL_PX, TILE_SIZE_PX, TileAction, TileKey,
    TileScheduler, ZOOM_DEFAULT_MILLI, ZOOM_MAX_MILLI, ZOOM_MIN_MILLI, base_scale_milli,
    zoom_in_milli, zoom_out_milli,
};
use pdf_core::{HistoryChange, OutlineItem, SaveError};
use slint::platform::Key;
use slint::{
    ComponentHandle, Image, Model, ModelRc, Rgb8Pixel, SharedPixelBuffer, SharedString, VecModel,
};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

/// The `--bench-*` / `FA_PDF_TRACE` diagnostic harness (Plan 0001 Phase 2, Plan 0002 Phase 1).
mod bench;
mod convert_ui;
mod fields_ui;
mod stamps_ui;
mod tabs;
mod tools_ui;

slint::include_modules!();

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

/// The gallery's strip (Plan 0007 Phase 3); must match `GalleryMetrics` in `ui/gallery.slint`:
/// one page's card and the gap after it, where the first card starts, and the box a page's
/// picture fits in, in logical pixels.
const GALLERY_PITCH_PX: f32 = 280.0;
const GALLERY_LEAD_PX: f32 = 120.0;
const GALLERY_PAD_PX: f32 = 72.0;
const GALLERY_CARD_PX: f32 = 232.0;
const GALLERY_BOX_W_PX: f32 = 216.0;
const GALLERY_BOX_H_PX: f32 = 300.0;
/// Pages left and right of the gallery's view whose pictures are drawn ahead.
const GALLERY_MARGIN_CARDS: usize = 2;
/// Pictures the gallery keeps: larger than the drawer's, so fewer of them (the view shows about
/// four on the §1 machine).
const GALLERY_THUMB_CACHE: usize = 24;
/// A mode change's motion (`Theme.motion` in `ui/theme.slint`) and the quiet time after it:
/// nothing new is drawn until both have passed (plan row AT).
const MODE_MOTION_MS: u64 = 250;
const MODE_SETTLE_MS: u64 = 30;
/// How long the warm-up frame's items stay shown: long enough for one frame to draw them.
const WARM_MS: u64 = 40;
/// The modes, as the window's `mode` property has them.
const MODE_TOOLS: i32 = 2;

/// How long a finger must rest before a touch starts a selection instead of a pan.
const LONG_PRESS_MS: u64 = 500;
/// A click this soon after a double-click, near it, is a triple-click (a line).
const TRIPLE_CLICK_MS: u128 = 500;
/// How far a finger may move before a pending long press becomes a pan, logical pixels.
const TOUCH_SLOP_LOGICAL_PX: f32 = 8.0;
/// How far a selection drag beyond the viewport's edge scrolls per move, logical pixels.
const SELECT_AUTOSCROLL_LOGICAL_PX: f64 = 24.0;

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

/// What a press on the page does (Plan 0004, §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    /// Select text, or an annotation by clicking it.
    Select,
    /// Select text, and mark it with this markup when the drag ends.
    Markup(MarkupKind),
    /// A click places a sticky note there (Plan 0004 Phase 3).
    Note,
    /// A drag draws a free-hand line (Plan 0004 Phase 4).
    Ink,
    /// A drag draws the rectangle to crop pages to (Plan 0005 Phase 3).
    Crop,
}

/// The note the editor is open for: a new one at a rectangle, or an existing one.
#[derive(Debug, Clone, Copy, PartialEq)]
enum NoteTarget {
    New(pdf_core::Rect),
    Existing(u32),
}

/// A press on a note, which becomes a move once the pointer leaves the click slop.
#[derive(Debug, Clone, Copy, PartialEq)]
struct NoteDrag {
    page: u32,
    id: u32,
    /// Where on the note the pointer took it, in points from its top-left corner.
    grab: (f32, f32),
    /// Where the note would land now; `None` until the pointer has moved past the slop.
    rect: Option<pdf_core::Rect>,
}

impl Tool {
    /// The tool strip's index for the tool, as the markup's `tool` property holds it.
    fn index(self) -> i32 {
        match self {
            Tool::Select => 0,
            Tool::Markup(MarkupKind::Highlight) => 1,
            Tool::Markup(MarkupKind::Underline) => 2,
            Tool::Markup(MarkupKind::StrikeOut) => 3,
            Tool::Markup(MarkupKind::Squiggly) => 4,
            Tool::Note => 5,
            Tool::Ink => 6,
            Tool::Crop => 7,
        }
    }

    fn from_index(index: i32) -> Self {
        match index {
            1 => Tool::Markup(MarkupKind::Highlight),
            2 => Tool::Markup(MarkupKind::Underline),
            3 => Tool::Markup(MarkupKind::StrikeOut),
            4 => Tool::Markup(MarkupKind::Squiggly),
            5 => Tool::Note,
            6 => Tool::Ink,
            7 => Tool::Crop,
            _ => Tool::Select,
        }
    }
}

/// What happens once the document is saved, or once the user chose not to save it (Plan 0004,
/// §4.4): the action that asked "save changes?" first. Opening a document no longer asks: it
/// opens in a tab of its own (Plan 0007 Phase 4).
#[derive(Debug, Clone, PartialEq, Eq)]
enum AfterSave {
    /// Close the window: the next tab with unsaved changes asks in turn.
    Quit,
    /// Close the tab with this id.
    CloseTab(u64),
}

/// What a `Saved` event leaves for the caller to do outside the state, where a modal dialog may run.
enum SavedNext {
    Then(AfterSave),
    SaveAs,
}

/// One page row as last written to the model: page index and canvas rect in logical pixels, and
/// whether it carried a base layer. Compared before a write, so a scroll that keeps the same pages
/// near the viewport does not touch the model.
type PageRow = (u32, f32, f32, f32, f32, bool);

/// What every tab's state shares with the window (Plan 0007 Phase 4): the epoch counters, so an
/// epoch names one tab's work, and the models the window's markup is bound to.
#[derive(Clone)]
struct WindowParts {
    next_epoch: Arc<AtomicU64>,
    current_epoch: Arc<AtomicU64>,
    model: Rc<VecModel<TileView>>,
    pages_model: Rc<VecModel<PageView>>,
    recent_model: Rc<VecModel<SharedString>>,
    thumbs_model: Rc<VecModel<ThumbRow>>,
    outline_model: Rc<VecModel<OutlineRow>>,
    hits_model: Rc<VecModel<HitView>>,
    sel_model: Rc<VecModel<HitView>>,
    annot_sel_model: Rc<VecModel<HitView>>,
    field_model: Rc<VecModel<FieldView>>,
    merge_model: Rc<VecModel<SharedString>>,
}

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
    /// Pages whose thumbnail is held, with the tick they were last in view (LRU, [`THUMB_CACHE`])
    /// and the width in pixels it was drawn at: the drawer and the gallery want different ones.
    thumb_cache: HashMap<u32, (u64, u32)>,
    thumb_tick: u64,
    /// Thumbnails asked for, with the width each was asked at.
    thumb_inflight: HashMap<u32, u32>,
    /// The window's mode (0 Read, 1 Edit, 2 Tools) as Rust last saw it, and when the motion of
    /// the last change ends (Plan 0007 Phase 3): until then nothing new is drawn.
    mode: i32,
    motion_until: Option<Instant>,
    /// The opened document's first view is not drawn yet; once it is, one warm-up frame follows
    /// (main.slint `warm`).
    warm_pending: bool,
    /// The room the islands take at the viewport's edges, in device pixels; every layout keeps it
    /// clear around the pages (main.slint `doc-inset-*`).
    margins: Margins,
    /// The thumbnail row marked as the current page.
    current_thumb: Option<usize>,
    /// What the engine last said can be undone and redone, and whether MuPDF repaired the file:
    /// a tab shown again puts them back on its buttons (Plan 0007 Phase 4).
    can_undo: bool,
    can_redo: bool,
    repaired: bool,
    /// A document made from pages of another (dragged out, split off): the tab's name, and the
    /// folder its first Save As starts in. It has no file of its own until then.
    untitled: Option<String>,
    origin_dir: Option<PathBuf>,
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
    /// The text selection (Plan 0003, §4.5): its two ends, its mode, and what the engine answered.
    sel_generation: u64,
    sel_anchor: Option<PagePoint>,
    sel_focus: Option<PagePoint>,
    sel_mode: SelectMode,
    sel_pages: Vec<PageSelection>,
    sel_text: String,
    sel_model: Rc<VecModel<HitView>>,
    sel_rows: Vec<(f32, f32, f32, f32, bool)>,
    /// A mouse drag is extending the selection.
    sel_pressed: bool,
    /// When the last double-click happened, to tell a triple-click.
    last_double: Option<Instant>,
    /// A touch that may become a long press: where it went down.
    touch_down: Option<(f32, f32)>,
    long_press: slint::Timer,
    touch_selecting: bool,
    /// Text line boxes of the pages near the view, to tell text from margins under the pointer
    /// (Plan 0003 Phase 4 review), and the pages asked for and not answered yet.
    text_lines: HashMap<u32, Vec<pdf_core::Rect>>,
    lines_pending: HashSet<u32>,
    /// Annotating (Plan 0004 Phase 1): whether the open document can be edited, the file it is
    /// saved to, and whether that file has every edit.
    editable: bool,
    doc_path: Option<PathBuf>,
    save_state: SaveState,
    saving: bool,
    after_save: Option<AfterSave>,
    tool: Tool,
    /// Index into [`PALETTE`].
    colour: usize,
    next_edit_id: u64,
    /// Each page's revision as the engine last reported it; a tile, base layer or thumbnail drawn
    /// from an older one is dropped (Plan 0004, §4.1).
    page_revs: HashMap<u32, u32>,
    /// The annotations of the pages near the view, and the pages asked for and not answered yet.
    annots: HashMap<u32, Vec<AnnotInfo>>,
    annots_pending: HashSet<u32>,
    /// The selected annotation as `(page, id)`, and the one under the press that a click without
    /// a drag will select.
    selected_annot: Option<(u32, u32)>,
    annot_candidate: Option<(u32, u32)>,
    press_at: (f32, f32),
    annot_sel_model: Rc<VecModel<HitView>>,
    annot_sel_rows: Vec<(f32, f32, f32, f32, bool)>,
    /// The selection generation the engine answered last, and a markup waiting for the answer to
    /// the newest one.
    sel_answered: u64,
    pending_mark: Option<MarkupKind>,
    /// The note editor's target (page and note) while it is open, and a press on a note that may
    /// become a move (Plan 0004 Phase 3).
    note_edit: Option<(u32, NoteTarget)>,
    note_drag: Option<NoteDrag>,
    /// The Ink tool's line width (index into [`INK_WIDTHS`]), and the stroke being drawn: its
    /// page and its points in page space as the pointer gave them (Plan 0004 Phase 4).
    ink_width: usize,
    ink: Option<(u32, Vec<(f32, f32)>)>,
    /// The live stroke's path commands, rebuilt in place on every pointer move.
    ink_commands: String,
    /// The page whose committed stroke is still drawn live, until its new tiles are all in.
    ink_shown: Option<u32>,
    /// The Crop tool's rectangle (Plan 0005 Phase 3).
    crop: Option<stamps_ui::CropDraw>,
    /// Form fields of the near pages, those asked for, the field being filled, and what the
    /// document's form has (Plan 0006).
    fields: HashMap<u32, Vec<pdf_core::form::FieldInfo>>,
    fields_pending: HashSet<u32>,
    field_edit: Option<fields_ui::FieldEdit>,
    form: pdf_core::form::FormSummary,
    field_model: Rc<VecModel<FieldView>>,
    field_rows: Vec<(f32, f32, f32, f32, bool)>,
    /// The field with the keyboard focus, as (page, index in the page's fields), and the page a
    /// Tab is waiting to arrive on, with whether it goes backwards (Plan 0006 Phase 2).
    field_focus: Option<(u32, usize)>,
    field_tab_pending: Option<(u32, bool)>,
    /// Pages chosen in the thumbnails for a page operation (Plan 0005 Phase 1), the row a Shift+click
    /// range starts from, and the pages a thumbnail drag is moving.
    page_sel: std::collections::BTreeSet<u32>,
    page_sel_anchor: Option<u32>,
    drag_pages: Vec<u32>,
    /// Counts layouts: opens and structural edits. Text lines and annotations are asked for with it,
    /// so an answer about a page index from before a structural edit is dropped.
    layout_generation: u64,
    /// The tool job running on the engine's tool thread, the next job's id, the merge panel's
    /// files and rows, and the properties panel's state (Plan 0005 Phase 2).
    running_tool: Option<tools_ui::RunningTool>,
    next_job_id: u64,
    merge_inputs: Vec<pdf_core::tools::MergeInput>,
    merge_model: Rc<VecModel<SharedString>>,
    props_wanted: bool,
    props_shown: pdf_core::tools::Metadata,
    /// Silent unless `FA_PDF_TRACE` is set or a `--bench-*` flag asked for a run.
    bench: bench::Trace,
}

impl AppState {
    /// A state with no document, sharing the window's models and epoch counters (`parts`).
    #[allow(clippy::too_many_arguments)]
    fn new(
        cmd_tx: Sender<EngineCmd>,
        parts: WindowParts,
        scheduler: TileScheduler,
        settings: Settings,
        settings_path: Option<PathBuf>,
        scale_factor: f32,
        bench: bench::Trace,
    ) -> Self {
        Self {
            cmd_tx,
            next_epoch: parts.next_epoch,
            current_epoch: parts.current_epoch,
            page_sizes: Vec::new(),
            layout: DocLayout::default(),
            scale_milli: ZOOM_DEFAULT_MILLI,
            pos: (0.0, 0.0),
            win_x: CanvasWindow::default(),
            win_y: CanvasWindow::default(),
            shift_x: 0.0,
            scheduler,
            model: parts.model,
            pages_model: parts.pages_model,
            page_rows: Vec::new(),
            bases: HashMap::new(),
            base_pending: HashSet::new(),
            settings,
            settings_path,
            opening: None,
            recent_model: parts.recent_model,
            doc_generation: 0,
            thumbs_model: parts.thumbs_model,
            thumb_cache: HashMap::new(),
            thumb_tick: 0,
            thumb_inflight: HashMap::new(),
            mode: 0,
            motion_until: None,
            warm_pending: false,
            margins: Margins::default(),
            current_thumb: None,
            can_undo: false,
            can_redo: false,
            repaired: false,
            untitled: None,
            origin_dir: None,
            outline_items: Vec::new(),
            outline_expanded: Vec::new(),
            outline_visible: Vec::new(),
            outline_model: parts.outline_model,
            search: SearchResults::default(),
            search_generation: 0,
            search_query: String::new(),
            hits_model: parts.hits_model,
            hit_rows: Vec::new(),
            search_sent: None,
            sel_generation: 0,
            sel_anchor: None,
            sel_focus: None,
            sel_mode: SelectMode::Chars,
            sel_pages: Vec::new(),
            sel_text: String::new(),
            sel_model: parts.sel_model,
            sel_rows: Vec::new(),
            sel_pressed: false,
            last_double: None,
            touch_down: None,
            long_press: slint::Timer::default(),
            touch_selecting: false,
            text_lines: HashMap::new(),
            lines_pending: HashSet::new(),
            editable: false,
            doc_path: None,
            save_state: SaveState::new(),
            saving: false,
            after_save: None,
            tool: Tool::Select,
            colour: 0,
            next_edit_id: 0,
            page_revs: HashMap::new(),
            annots: HashMap::new(),
            annots_pending: HashSet::new(),
            selected_annot: None,
            annot_candidate: None,
            press_at: (0.0, 0.0),
            annot_sel_model: parts.annot_sel_model,
            annot_sel_rows: Vec::new(),
            sel_answered: 0,
            pending_mark: None,
            note_edit: None,
            note_drag: None,
            ink_width: 1,
            ink: None,
            ink_commands: String::new(),
            ink_shown: None,
            crop: None,
            fields: HashMap::new(),
            fields_pending: HashSet::new(),
            field_edit: None,
            form: pdf_core::form::FormSummary::default(),
            field_model: parts.field_model,
            field_rows: Vec::new(),
            field_focus: None,
            field_tab_pending: None,
            scale_factor,
            rendered: 0,
            page_sel: std::collections::BTreeSet::new(),
            page_sel_anchor: None,
            drag_pages: Vec::new(),
            layout_generation: 0,
            running_tool: None,
            next_job_id: 0,
            merge_inputs: Vec::new(),
            merge_model: parts.merge_model,
            props_wanted: false,
            props_shown: pdf_core::tools::Metadata::default(),
            bench,
        }
    }

    fn sf(&self) -> f64 {
        self.scale_factor as f64
    }

    /// The page gap in device pixels: constant in logical pixels, so it does not grow with zoom.
    fn gap_px(&self) -> u32 {
        (PAGE_GAP_LOGICAL_PX * self.scale_factor).round() as u32
    }

    /// The islands' room at the viewport's edges, from the window, in device pixels.
    fn read_margins(&self, ui: &MainWindow) -> Margins {
        let px = |logical: f32| (logical as f64 * self.sf()).round().max(0.0) as u32;
        Margins {
            top: px(ui.get_doc_inset_top()),
            right: px(ui.get_doc_inset_right()),
            bottom: px(ui.get_doc_inset_bottom()),
            left: px(ui.get_doc_inset_left()),
        }
    }

    /// The document laid out at `milli`, with the islands' current margins.
    fn layout_for(&mut self, ui: &MainWindow, milli: u32) -> DocLayout {
        self.margins = self.read_margins(ui);
        DocLayout::with_margins(&self.page_sizes, milli, self.gap_px(), self.margins)
    }

    /// The viewport less the islands' margins, in device pixels: the room the pages are seen in.
    fn free_view(&self, ui: &MainWindow) -> (f64, f64) {
        let (w, h) = self.view_size(ui);
        let m = self.margins;
        (
            (w - (m.left + m.right) as f64).max(1.0),
            (h - (m.top + m.bottom) as f64).max(1.0),
        )
    }

    /// The document y in the middle of the room the pages are seen in.
    fn view_mid_y(&self, ui: &MainWindow) -> f64 {
        let (_, free_h) = self.free_view(ui);
        self.pos.1 + self.margins.top as f64 + free_h / 2.0
    }

    /// An island moved or changed size (the drawer opened, the window maximized): the pages are
    /// laid out again around it, and what was in view stays put.
    fn insets_changed(&mut self, ui: &MainWindow) {
        let margins = self.read_margins(ui);
        if margins == self.margins {
            return;
        }
        if self.layout.is_empty() {
            self.margins = margins;
            return;
        }
        let top = self.margins.top as f64;
        self.layout = self.layout_for(ui, self.scale_milli);
        self.pos.1 += self.margins.top as f64 - top;
        self.clamp_pos(ui);
        self.reanchor(ui);
        self.refresh(ui);
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
        self.warm_when_covered(ui, &visible);
        self.apply(ui, actions);
        self.update_pages(ui);
        self.update_hits(ui);
        self.update_field_rects(ui);
        self.update_selection(ui);
        self.update_annot_selection(ui);

        if let Some(page) = self.layout.page_near(self.view_mid_y(ui)) {
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
        if let Some(rect) = hit.rects.first() {
            self.reveal_rect(ui, hit.page, *rect);
        }
        self.hit_rows.clear();
        self.update_hits(ui);
    }

    /// Scrolls a page rectangle into view when it is not: a third of the way down the view, and
    /// centred across it when it is off to a side.
    fn reveal_rect(&mut self, ui: &MainWindow, page: u32, rect: pdf_core::Rect) {
        let Some(slot) = self.layout.slot(page as usize).copied() else {
            return;
        };
        let s = self.scale_milli as f64 / 1000.0;
        let (x, y) = (
            slot.x as f64 + rect.x as f64 * s,
            slot.y as f64 + rect.y as f64 * s,
        );
        let (w, h) = (rect.width as f64 * s, rect.height as f64 * s);
        // In view means in the room the islands leave, not under them.
        let (view_w, view_h) = self.free_view(ui);
        let (left, top) = (self.margins.left as f64, self.margins.top as f64);
        let (seen_x, seen_y) = (self.pos.0 + left, self.pos.1 + top);
        let mut target = self.pos;
        if y < seen_y || y + h > seen_y + view_h {
            target.1 = y - view_h / 3.0 - top;
        }
        if x < seen_x || x + w > seen_x + view_w {
            target.0 = x + w / 2.0 - view_w / 2.0 - left;
        }
        if target != self.pos {
            if (target.1 - self.pos.1).abs() > 2.0 * view_h {
                self.begin_epoch();
            }
            self.scroll_to(ui, target);
        }
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

    /// A viewport point (logical pixels) as a point on the page under it, in page space. Between
    /// pages it belongs to the nearest page, outside its box; selection snaps it to the text.
    fn page_point(&self, ui: &MainWindow, x: f32, y: f32) -> Option<PagePoint> {
        let sf = self.sf();
        let doc_x = self.pos.0 + x as f64 * sf;
        let doc_y = self.pos.1 + y as f64 * sf;
        let page = self.layout.page_near(doc_y)?;
        let slot = self.layout.slot(page)?;
        let s = self.scale_milli as f64 / 1000.0;
        let _ = ui;
        Some(PagePoint {
            page: page as u32,
            x: ((doc_x - slot.x as f64) / s) as f32,
            y: ((doc_y - slot.y as f64) / s) as f32,
        })
    }

    /// A page's text line boxes arrived; one about a previous document, or a page that has left
    /// the view since, is dropped.
    fn apply_text_lines(&mut self, page: u32, generation: u64, lines: Vec<pdf_core::Rect>) {
        if generation == self.layout_generation && self.lines_pending.remove(&page) {
            self.text_lines.insert(page, lines);
        }
    }

    /// Whether a page point is over text. A page whose lines have not arrived yet counts as no
    /// text: the pointer then behaves as it does over a margin until they do.
    fn on_text(&self, point: Option<PagePoint>) -> bool {
        point.is_some_and(|p| {
            self.text_lines
                .get(&p.page)
                .is_some_and(|lines| pdf_core::select::on_text(lines, p.x, p.y))
        })
    }

    /// The pointer moved without a button: show the text cursor only over text.
    fn hover(&mut self, ui: &MainWindow, x: f32, y: f32) {
        if self.sel_pressed || self.touch_selecting {
            return;
        }
        let point = self.page_point(ui, x, y);
        let over = self.on_text(point);
        if ui.get_text_cursor() != over {
            ui.set_text_cursor(over);
        }
        // A note's text shows while the pointer rests on the note (Plan 0004 Phase 3).
        let text = point
            .and_then(|p| self.note_at(p))
            .and_then(|note| note.contents.clone());
        match text {
            Some(text) if self.note_drag.is_none() => {
                ui.set_note_hover_text(SharedString::from(text));
                ui.set_note_hover_x(x + 14.0);
                ui.set_note_hover_y(y + 14.0);
                ui.set_note_hover_visible(true);
            }
            _ => {
                if ui.get_note_hover_visible() {
                    ui.set_note_hover_visible(false);
                }
            }
        }
        self.field_hover(ui, point, x, y);
    }

    /// The note under a page point, if there is one there.
    fn note_at(&self, point: PagePoint) -> Option<&AnnotInfo> {
        let (page, id) = self.annot_at(point)?;
        self.annots
            .get(&page)?
            .iter()
            .find(|item| item.id == id && item.kind == AnnotKind::Note)
    }

    /// Opens the note editor for a new note at `rect` or for an existing one, beside the note.
    fn open_note_editor(&mut self, ui: &MainWindow, page: u32, target: NoteTarget) {
        if !self.editable {
            return;
        }
        let (rect, text) = match target {
            NoteTarget::New(rect) => (rect, String::new()),
            NoteTarget::Existing(id) => {
                let Some(note) = self
                    .annots
                    .get(&page)
                    .and_then(|items| items.iter().find(|item| item.id == id))
                else {
                    return;
                };
                (note.rect, note.contents.clone().unwrap_or_default())
            }
        };
        let Some(slot) = self.layout.slot(page as usize) else {
            return;
        };
        // Just below the note's icon, kept inside the viewport.
        let s = self.scale_milli as f64 / 1000.0;
        let sf = self.sf();
        let x = ((slot.x as f64 + rect.x as f64 * s - self.pos.0) / sf) as f32;
        let y =
            ((slot.y as f64 + (rect.y + rect.height) as f64 * s - self.pos.1) / sf) as f32 + 4.0;
        let x = x.clamp(0.0, (ui.get_view_w() - 284.0).max(0.0));
        let y = y.clamp(0.0, (ui.get_view_h() - 174.0).max(0.0));
        ui.set_note_editor_x(x);
        ui.set_note_editor_y(y);
        ui.set_note_text(SharedString::from(text));
        ui.set_note_hover_visible(false);
        ui.set_note_editor_visible(true);
        self.note_edit = Some((page, target));
    }

    /// Save in the note editor: a new note with text is placed, an existing one gets its new text;
    /// a new note left empty is not created.
    fn note_save(&mut self, ui: &MainWindow) {
        let Some((page, target)) = self.note_edit.take() else {
            return;
        };
        ui.set_note_editor_visible(false);
        let text = ui.get_note_text().to_string();
        match target {
            NoteTarget::New(rect) if !text.trim().is_empty() => self.send_edit(EditOp::AddNote {
                page,
                rect,
                contents: text,
                colour: PALETTE[self.colour],
                author: self.settings.author.clone(),
            }),
            NoteTarget::New(_) => {}
            NoteTarget::Existing(id) => {
                let old = self
                    .annots
                    .get(&page)
                    .and_then(|items| items.iter().find(|item| item.id == id))
                    .and_then(|note| note.contents.clone())
                    .unwrap_or_default();
                if old != text {
                    self.send_edit(EditOp::SetContents {
                        page,
                        id,
                        contents: text,
                    });
                }
            }
        }
    }

    /// Cancel or Esc in the note editor: nothing changes.
    fn note_cancel(&mut self, ui: &MainWindow) {
        self.note_edit = None;
        ui.set_note_editor_visible(false);
    }

    /// Pointer input on the page (Plan 0003, §4.5): a mouse drag selects characters, a
    /// double-click a word, a triple-click a line, Shift+click extends; a touch pans unless it
    /// rests for [`LONG_PRESS_MS`], which starts a word selection that the finger then extends.
    fn select_pointer(
        &mut self,
        ui: &MainWindow,
        kind: i32,
        x: f32,
        y: f32,
        shift: bool,
        touch: bool,
    ) {
        if self.tool == Tool::Crop && self.editable {
            self.crop_pointer(ui, kind, x, y);
            return;
        }
        let point = self.page_point(ui, x, y);
        // A press on a form field fills it (Plan 0006); any press commits an open field editor.
        if kind == 0 && !shift && self.field_pressed(ui, point) {
            return;
        }
        // The Ink tool draws with the mouse, a pen and a finger alike (Plan 0004, §4.2).
        if self.tool == Tool::Ink && self.editable {
            match kind {
                0 => {
                    if let Some(point) = point {
                        self.clear_selection(ui);
                        self.select_annotation(ui, None);
                        self.ink = Some((point.page, Vec::new()));
                        self.ink_point(ui, x, y);
                    }
                }
                2 => self.ink_point(ui, x, y),
                1 => self.finish_ink(ui, true),
                4 => self.finish_ink(ui, false),
                _ => {}
            }
            return;
        }
        match kind {
            // Down.
            0 if touch => {
                self.touch_down = Some((x, y));
                let weak = ui.as_weak();
                self.long_press.start(
                    slint::TimerMode::SingleShot,
                    std::time::Duration::from_millis(LONG_PRESS_MS),
                    move || {
                        if let Some(ui) = weak.upgrade() {
                            with_state(|state| state.long_press(&ui, x, y));
                        }
                    },
                );
            }
            0 => {
                let Some(point) = point else { return };
                let extend = shift && self.sel_anchor.is_some();
                // The Note tool places a note where it is pressed, or opens the one pressed on.
                if self.tool == Tool::Note && self.editable {
                    self.clear_selection(ui);
                    let target = match self.note_at(point) {
                        Some(note) => NoteTarget::Existing(note.id),
                        None => {
                            let (w, h) = self
                                .page_sizes
                                .get(point.page as usize)
                                .map_or((f32::MAX, f32::MAX), |size| (size.width, size.height));
                            NoteTarget::New(note_rect(point.x, point.y, w, h))
                        }
                    };
                    self.open_note_editor(ui, point.page, target);
                    return;
                }
                // An annotation under the press is selected if the button comes up without a drag
                // (Plan 0004, §4.2); a press anywhere else drops the annotation selection.
                self.press_at = (x, y);
                self.annot_candidate = if extend { None } else { self.annot_at(point) };
                if self.annot_candidate.is_none() {
                    self.select_annotation(ui, None);
                }
                // A press on a note may move it: no text selection starts under it.
                if !extend && let Some(note) = self.note_at(point) {
                    self.note_drag = Some(NoteDrag {
                        page: point.page,
                        id: note.id,
                        grab: (point.x - note.rect.x, point.y - note.rect.y),
                        rect: None,
                    });
                    self.sel_pressed = false;
                    self.clear_selection(ui);
                    return;
                }
                if !extend && !self.on_text(Some(point)) {
                    // A click in a margin, between paragraphs or beside the page selects nothing:
                    // it only clears what was selected.
                    self.sel_pressed = false;
                    self.clear_selection(ui);
                    return;
                }
                self.sel_pressed = true;
                if extend {
                    self.sel_focus = Some(point);
                    self.request_selection();
                    return;
                }
                let triple = self
                    .last_double
                    .is_some_and(|at| at.elapsed().as_millis() < TRIPLE_CLICK_MS);
                self.sel_anchor = Some(point);
                self.sel_focus = Some(point);
                if triple {
                    self.last_double = None;
                    self.sel_mode = SelectMode::Lines;
                    self.request_selection();
                } else {
                    // A plain click starts a new selection and, until the drag moves, clears it. A
                    // new generation, so an answer still on its way for the old one cannot bring it
                    // back.
                    self.sel_mode = SelectMode::Chars;
                    self.sel_generation += 1;
                    self.sel_answered = self.sel_generation;
                    self.pending_mark = None;
                    self.show_selection(ui, Vec::new(), String::new());
                }
            }
            // Move with the button (or a selecting finger) down.
            2 => {
                if let Some((dx, dy)) = self.touch_down
                    && !self.touch_selecting
                    && ((x - dx).abs() > TOUCH_SLOP_LOGICAL_PX
                        || (y - dy).abs() > TOUCH_SLOP_LOGICAL_PX)
                {
                    // The finger is panning: no long press.
                    self.long_press.stop();
                    self.touch_down = None;
                    return;
                }
                let dragged = (x - self.press_at.0).abs() > TOUCH_SLOP_LOGICAL_PX
                    || (y - self.press_at.1).abs() > TOUCH_SLOP_LOGICAL_PX;
                if let Some(mut drag) = self.note_drag {
                    // Moving a note: its outline follows the pointer, on its own page.
                    if dragged
                        && let Some((px, py)) = self.page_local(drag.page, x, y)
                        && let Some(size) = self.page_sizes.get(drag.page as usize)
                    {
                        let rect =
                            note_rect(px - drag.grab.0, py - drag.grab.1, size.width, size.height);
                        drag.rect = Some(rect);
                        self.note_drag = Some(drag);
                        self.annot_candidate = None;
                        self.selected_annot = Some((drag.page, drag.id));
                        ui.set_has_annot_selection(true);
                        ui.set_note_hover_visible(false);
                        self.update_annot_selection(ui);
                    }
                    return;
                }
                if dragged {
                    // A drag, not a click: it selects text, not the annotation under it.
                    self.annot_candidate = None;
                }
                if !(self.sel_pressed || self.touch_selecting) {
                    return;
                }
                if let Some(point) = point {
                    self.sel_focus = Some(point);
                    self.request_selection();
                }
                // Dragging past the top or bottom edge scrolls, so a selection can run on.
                let view_h = ui.get_view_h();
                let step = SELECT_AUTOSCROLL_LOGICAL_PX * self.sf();
                if y < 0.0 {
                    self.scroll_to(ui, (self.pos.0, self.pos.1 - step));
                } else if y > view_h {
                    self.scroll_to(ui, (self.pos.0, self.pos.1 + step));
                }
            }
            // Double-click: a word.
            3 => {
                // A double-click on a note opens it.
                if let Some(p) = point
                    && let Some(note) = self.note_at(p)
                {
                    let id = note.id;
                    self.note_drag = None;
                    self.open_note_editor(ui, p.page, NoteTarget::Existing(id));
                    return;
                }
                let Some(point) = point.filter(|p| self.on_text(Some(*p))) else {
                    return;
                };
                self.last_double = Some(Instant::now());
                self.sel_mode = SelectMode::Words;
                self.sel_anchor = Some(point);
                self.sel_focus = Some(point);
                self.request_selection();
                if let Tool::Markup(kind) = self.tool {
                    self.pending_mark = Some(kind);
                }
            }
            // Up or cancel.
            _ => {
                let was_selecting = self.sel_pressed || self.touch_selecting;
                // A finger that came up before the long press and without panning is a tap: like a
                // click, it clears the selection.
                if kind == 1 && touch && self.touch_down.is_some() && !self.touch_selecting {
                    self.clear_selection(ui);
                    self.select_annotation(ui, None);
                }
                if let Some(drag) = self.note_drag.take() {
                    match drag.rect {
                        // Released after a move: the note goes there.
                        Some(rect) if kind == 1 => self.send_edit(EditOp::MoveNote {
                            page: drag.page,
                            id: drag.id,
                            rect,
                        }),
                        // Cancelled: back where it was.
                        Some(_) => self.update_annot_selection(ui),
                        None => {}
                    }
                    if drag.rect.is_some() {
                        self.annot_candidate = None;
                    }
                }
                if kind == 1 {
                    if let Some(annot) = self.annot_candidate.take() {
                        self.clear_selection(ui);
                        self.select_annotation(ui, Some(annot));
                    } else if was_selecting && let Tool::Markup(markup) = self.tool {
                        self.mark_when_answered(ui, markup);
                    }
                }
                self.annot_candidate = None;
                self.sel_pressed = false;
                self.long_press.stop();
                self.touch_down = None;
                if self.touch_selecting {
                    self.touch_selecting = false;
                    ui.set_touch_selecting(false);
                }
            }
        }
    }

    /// A viewport point (logical pixels) in the space of a given page, which it may lie outside.
    fn page_local(&self, page: u32, x: f32, y: f32) -> Option<(f32, f32)> {
        let slot = self.layout.slot(page as usize)?;
        let s = self.scale_milli as f64 / 1000.0;
        let sf = self.sf();
        Some((
            ((self.pos.0 + x as f64 * sf - slot.x as f64) / s) as f32,
            ((self.pos.1 + y as f64 * sf - slot.y as f64) / s) as f32,
        ))
    }

    /// The pointer moved while drawing: the stroke gets the point, kept on its page.
    fn ink_point(&mut self, ui: &MainWindow, x: f32, y: f32) {
        let Some(page) = self.ink.as_ref().map(|(page, _)| *page) else {
            return;
        };
        let (Some((px, py)), Some(size)) = (
            self.page_local(page, x, y),
            self.page_sizes.get(page as usize),
        ) else {
            return;
        };
        let point = (px.clamp(0.0, size.width), py.clamp(0.0, size.height));
        if let Some((_, points)) = self.ink.as_mut() {
            points.push(point);
        }
        self.rebuild_ink(ui);
    }

    /// The live stroke's path, in viewport logical pixels: rebuilt whole on each move or scroll
    /// (`ink_rebuild_ms`, benchmarks §10.2), since the points are in page space.
    fn rebuild_ink(&mut self, ui: &MainWindow) {
        let started = Instant::now();
        let Some((page, points)) = self.ink.as_ref() else {
            return;
        };
        let Some(slot) = self.layout.slot(*page as usize) else {
            return;
        };
        let s = self.scale_milli as f64 / 1000.0;
        let sf = self.sf();
        let (ox, oy) = (
            (slot.x as f64 - self.pos.0) / sf,
            (slot.y as f64 - self.pos.1) / sf,
        );
        let k = s / sf;
        self.ink_commands.clear();
        for (i, &(px, py)) in points.iter().enumerate() {
            let vx = ox + px as f64 * k;
            let vy = oy + py as f64 * k;
            let _ = write!(
                self.ink_commands,
                "{}{vx:.1} {vy:.1} ",
                if i == 0 { "M" } else { "L" }
            );
        }
        // One point draws a dot: a zero-length segment with round caps.
        if let [(px, py)] = points.as_slice() {
            let _ = write!(
                self.ink_commands,
                "L{:.1} {:.1}",
                ox + *px as f64 * k,
                oy + *py as f64 * k
            );
        }
        let colour = PALETTE[self.colour];
        ui.set_ink_colour(slint::Color::from_rgb_f32(colour.r, colour.g, colour.b));
        ui.set_ink_stroke_width((INK_WIDTHS[self.ink_width] as f64 * k) as f32);
        ui.set_ink_commands(SharedString::from(self.ink_commands.as_str()));
        let points = points.len();
        self.bench.ink_rebuilt(points, started.elapsed());
    }

    /// The pointer came up (`commit`) or was taken away: the stroke is simplified and drawn by
    /// the engine, or dropped. The live path stays until the page's new tiles replace it.
    fn finish_ink(&mut self, ui: &MainWindow, commit: bool) {
        let Some((page, points)) = self.ink.take() else {
            return;
        };
        if !commit || points.is_empty() {
            ui.set_ink_commands(SharedString::new());
            return;
        }
        self.ink_shown = Some(page);
        let mut stroke = simplify(&points, INK_SIMPLIFY_PT);
        // A tap is a dot: a stroke needs two points for other readers to draw it.
        if let [only] = stroke.as_slice() {
            stroke.push(*only);
        }
        self.bench.ink_sent(&stroke);
        self.send_edit(EditOp::AddInk {
            page,
            strokes: vec![stroke],
            colour: PALETTE[self.colour],
            width: INK_WIDTHS[self.ink_width],
            author: self.settings.author.clone(),
        });
    }

    /// A line width chip of the Ink tool.
    fn choose_ink_width(&mut self, ui: &MainWindow, index: usize) {
        if index < INK_WIDTHS.len() {
            self.ink_width = index;
            ui.set_ink_width_index(index as i32);
        }
    }

    /// A finger rested long enough: select the word under it and let the finger extend it.
    fn long_press(&mut self, ui: &MainWindow, x: f32, y: f32) {
        if self.touch_down.is_none() {
            return;
        }
        let Some(point) = self.page_point(ui, x, y).filter(|p| self.on_text(Some(*p))) else {
            return;
        };
        self.touch_selecting = true;
        ui.set_touch_selecting(true);
        self.sel_mode = SelectMode::Words;
        self.sel_anchor = Some(point);
        self.sel_focus = Some(point);
        self.request_selection();
    }

    /// Asks the engine for the selection between the two ends.
    fn request_selection(&mut self) {
        let (Some(from), Some(to)) = (self.sel_anchor, self.sel_focus) else {
            return;
        };
        self.sel_generation += 1;
        let _ = self.cmd_tx.send(EngineCmd::Select {
            from,
            to,
            mode: self.sel_mode,
            generation: self.sel_generation,
        });
    }

    /// The engine answered a selection; an older answer than the last request is dropped.
    fn apply_selection(
        &mut self,
        ui: &MainWindow,
        generation: u64,
        pages: Vec<PageSelection>,
        text: String,
    ) {
        if generation == self.sel_generation {
            self.sel_answered = generation;
            self.show_selection(ui, pages, text);
            if let Some(kind) = self.pending_mark.take() {
                self.mark_selection(ui, kind);
            }
        }
    }

    /// Shows a selection's rectangles. The model is diffed against the rows on screen, so an empty
    /// selection clears them (Plan 0004 Phase 1 review).
    fn show_selection(&mut self, ui: &MainWindow, pages: Vec<PageSelection>, text: String) {
        self.sel_pages = pages;
        self.sel_text = text;
        self.update_selection(ui);
    }

    /// No selection (a new document, Esc).
    fn clear_selection(&mut self, ui: &MainWindow) {
        self.sel_generation += 1;
        // Nothing is asked for, so nothing is awaited.
        self.sel_answered = self.sel_generation;
        self.pending_mark = None;
        self.sel_anchor = None;
        self.sel_focus = None;
        self.sel_pressed = false;
        self.show_selection(ui, Vec::new(), String::new());
    }

    /// The selection's rectangles on the pages near the view, like the search highlights.
    fn update_selection(&mut self, ui: &MainWindow) {
        let near = self.near_pages(ui);
        let s = self.scale_milli as f64 / 1000.0;
        let sf = self.sf() as f32;
        let mut rows = Vec::new();
        for page in &self.sel_pages {
            if !near.contains(&(page.page as usize)) {
                continue;
            }
            let Some(slot) = self.layout.slot(page.page as usize) else {
                continue;
            };
            for rect in &page.rects {
                rows.push((
                    self.canvas_x(slot.x as f64 + rect.x as f64 * s),
                    self.canvas_y(slot.y as f64 + rect.y as f64 * s),
                    (rect.width as f64 * s) as f32 / sf,
                    (rect.height as f64 * s) as f32 / sf,
                    false,
                ));
            }
        }
        if rows == self.sel_rows {
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
        self.sel_model.set_vec(views);
        self.sel_rows = rows;
    }

    /// The annotation under a page point, as `(page, id)`.
    fn annot_at(&self, point: PagePoint) -> Option<(u32, u32)> {
        let items = self.annots.get(&point.page)?;
        pdf_core::annot::hit(items, point.x, point.y).map(|item| (point.page, item.id))
    }

    /// Selects an annotation (or none) and outlines it.
    fn select_annotation(&mut self, ui: &MainWindow, annot: Option<(u32, u32)>) {
        if self.selected_annot == annot {
            return;
        }
        self.selected_annot = annot;
        ui.set_has_annot_selection(annot.is_some());
        self.update_annot_selection(ui);
    }

    /// The selected annotation's outline, placed like the selection's rectangles.
    fn update_annot_selection(&mut self, ui: &MainWindow) {
        let mut rows = Vec::new();
        if let Some((page, id)) = self.selected_annot
            && self.near_pages(ui).contains(&(page as usize))
            && let Some(slot) = self.layout.slot(page as usize)
            && let Some(item) = self
                .annots
                .get(&page)
                .and_then(|items| items.iter().find(|item| item.id == id))
        {
            let s = self.scale_milli as f64 / 1000.0;
            let sf = self.sf() as f32;
            // A note being moved is outlined where it would land.
            let rect = self
                .note_drag
                .filter(|drag| drag.page == page && drag.id == id)
                .and_then(|drag| drag.rect)
                .unwrap_or(item.rect);
            rows.push((
                self.canvas_x(slot.x as f64 + rect.x as f64 * s),
                self.canvas_y(slot.y as f64 + rect.y as f64 * s),
                (rect.width as f64 * s) as f32 / sf,
                (rect.height as f64 * s) as f32 / sf,
                false,
            ));
        }
        if rows == self.annot_sel_rows {
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
        self.annot_sel_model.set_vec(views);
        self.annot_sel_rows = rows;
    }

    /// A page's annotations arrived; one about a previous document, or a page that has left the
    /// view since, is dropped.
    fn apply_annotations(
        &mut self,
        ui: &MainWindow,
        page: u32,
        generation: u64,
        items: Vec<AnnotInfo>,
    ) {
        if generation == self.layout_generation && self.annots_pending.remove(&page) {
            self.annots.insert(page, items);
            self.update_annot_selection(ui);
        }
    }

    /// The pages changed (Plan 0005, §4.1): the layout is rebuilt from the new sizes, the reader
    /// stays on their page wherever it went, and what is kept per page - base layers, thumbnails,
    /// text lines, annotations, the chosen pages - follows `map`; the rest is asked for again. Tiles
    /// are dropped: their places in the canvas changed with the layout.
    fn restructured(
        &mut self,
        ui: &MainWindow,
        generation: u64,
        page_sizes: Vec<PageSize>,
        revision: u32,
        map: PageMap,
    ) {
        if generation != self.doc_generation || page_sizes.len() != map.len() {
            return;
        }
        let started = Instant::now();
        let old_count = self.page_sizes.len() as u32;
        // Where the reader was: a page and how far down it, which the new layout keeps.
        let (_, view_h) = self.view_size(ui);
        let reading = self.current_page(ui) as u32;
        let into = self
            .layout
            .slot(reading as usize)
            .map_or(0.0, |slot| self.pos.1 - slot.y as f64);
        let left = self.pos.0;

        // Whatever is open on the old pages ends.
        self.finish_ink(ui, false);
        self.crop_cancel(ui);
        self.field_cancel(ui);
        self.note_cancel(ui);
        self.note_drag = None;
        self.select_annotation(ui, None);
        self.clear_selection(ui);
        self.cancel_search();

        // Per-page state follows the map; what has no old page is asked for again.
        self.text_lines = remap(&map, &self.text_lines);
        self.annots = remap(&map, &self.annots);
        self.fields = remap(&map, &self.fields);
        self.fields_restructured(ui, &map);
        self.layout_generation += 1;
        self.lines_pending.clear();
        self.annots_pending.clear();
        self.fields_pending.clear();
        let bases = remap(&map, &self.bases);
        let same_count = map.len() as u32 == old_count;
        self.page_sel = map
            .iter()
            .enumerate()
            .filter(|(new, from)| match from {
                Some(from) => self.page_sel.contains(from),
                // A turned page keeps its place, and its choice.
                None => same_count && self.page_sel.contains(&(*new as u32)),
            })
            .map(|(new, _)| new as u32)
            .collect();
        self.page_sel_anchor = None;

        self.page_sizes = page_sizes;
        let count = self.page_sizes.len();
        self.page_revs = (0..count as u32).map(|page| (page, revision)).collect();
        self.layout = self.layout_for(ui, self.scale_milli);
        ui.set_total_pages(count as i32);
        self.remap_thumbs(&map, old_count);
        self.show_page_sel(ui);

        self.begin_epoch();
        self.release_all(ui);
        self.bases = bases;
        self.base_pending.clear();
        let page = follow(&map, reading).min(count.saturating_sub(1) as u32);
        let top = self
            .layout
            .slot(page as usize)
            .map_or(0.0, |slot| slot.y as f64);
        let height = self
            .layout
            .slot(page as usize)
            .map_or(0.0, |slot| slot.h as f64);
        self.pos = (left, top + into.clamp(-view_h, height.max(0.0)));
        self.clamp_pos(ui);
        self.reanchor(ui);
        self.refresh(ui);
        self.pump_thumbs(ui);
        self.bench.restructured(started.elapsed());
        // A search that was showing runs again over the new pages.
        let query = self.search_query.clone();
        if !query.is_empty() && ui.get_find_visible() {
            self.start_search(ui, &query);
        }
    }

    /// The pages a page operation acts on: the chosen thumbnails, or the current page.
    fn page_targets(&self, ui: &MainWindow) -> Vec<u32> {
        if self.page_sel.is_empty() {
            vec![self.current_page(ui) as u32]
        } else {
            self.page_sel.iter().copied().collect()
        }
    }

    /// A page operation from the sidebar strip or the Pages menu (Plan 0005 Phase 1).
    fn page_op(&mut self, ui: &MainWindow, which: i32) {
        if !self.editable || self.page_sizes.is_empty() {
            return;
        }
        let pages = self.page_targets(ui);
        let count = self.page_sizes.len() as u32;
        let (first, last) = (
            pages.first().copied().unwrap_or(0),
            pages.last().copied().unwrap_or(0),
        );
        let op = match which {
            0 | 1 => PageOp::Rotate {
                pages,
                quarter_turns: if which == 0 { -1 } else { 1 },
            },
            2 => PageOp::Duplicate { pages },
            3 => PageOp::Delete { pages },
            4 if first > 0 => PageOp::Move {
                pages,
                before: first - 1,
            },
            // Past the page after the last one: that page and the moved ones change places.
            5 if last + 1 < count => PageOp::Move {
                pages,
                before: (last + 2).min(count),
            },
            6 => {
                // As large as the page it follows.
                let size = self.page_sizes[last as usize];
                PageOp::InsertBlank {
                    at: last + 1,
                    width: size.width,
                    height: size.height,
                }
            }
            _ => return,
        };
        self.send_edit(EditOp::Pages(op));
    }

    /// A click on a thumbnail: alone it goes to the page and chooses it; Ctrl adds or removes it,
    /// Shift chooses the run from the last click.
    fn thumb_clicked(&mut self, ui: &MainWindow, row: u32, ctrl: bool, shift: bool) {
        if shift && let Some(anchor) = self.page_sel_anchor {
            let (from, to) = (anchor.min(row), anchor.max(row));
            if !ctrl {
                self.page_sel.clear();
            }
            self.page_sel.extend(from..=to);
        } else if ctrl {
            if !self.page_sel.remove(&row) {
                self.page_sel.insert(row);
            }
            self.page_sel_anchor = Some(row);
        } else {
            self.page_sel.clear();
            self.page_sel.insert(row);
            self.page_sel_anchor = Some(row);
            self.go_to_page(ui, row as usize);
        }
        self.show_page_sel(ui);
    }

    /// Marks the chosen thumbnails.
    fn show_page_sel(&mut self, ui: &MainWindow) {
        for row in 0..self.thumbs_model.row_count() {
            let chosen = self.page_sel.contains(&(row as u32));
            if let Some(mut data) = self.thumbs_model.row_data(row)
                && data.selected != chosen
            {
                data.selected = chosen;
                self.thumbs_model.set_row_data(row, data);
            }
        }
        ui.set_pages_chosen(self.page_sel.len() as i32);
    }

    /// A thumbnail drag began: it moves the chosen pages when it started on one of them, else
    /// that page alone.
    fn thumb_drag_started(&mut self, ui: &MainWindow, row: u32) {
        if !self.page_sel.contains(&row) {
            self.page_sel.clear();
            self.page_sel.insert(row);
            self.page_sel_anchor = Some(row);
            self.show_page_sel(ui);
        }
        self.drag_pages = self.page_sel.iter().copied().collect();
    }

    /// The gallery's gap before page `before` got the dragged pages (Plan 0007 Phase 3).
    fn gallery_dropped(&mut self, before: u32) {
        let pages = std::mem::take(&mut self.drag_pages);
        if pages.is_empty() || !self.editable {
            return;
        }
        let before = before.min(self.page_sizes.len() as u32);
        self.send_edit(EditOp::Pages(PageOp::Move { pages, before }));
    }

    /// An action on a page in the gallery acts on the chosen pages when it is one of them, and
    /// otherwise chooses it alone first, so the action and what it acts on are what is shown.
    fn gallery_choose_for_action(&mut self, ui: &MainWindow, page: u32) {
        if !self.page_sel.contains(&page) {
            self.page_sel.clear();
            self.page_sel.insert(page);
            self.page_sel_anchor = Some(page);
            self.show_page_sel(ui);
        }
    }

    /// A gap's action in the gallery: a blank page there (as large as the page before it, or the
    /// first page). Pages from a file go through
    /// [`tools_ui::insert_from_file`], whose dialog runs with the state unborrowed.
    fn gallery_divider(&mut self, at: u32, which: i32) {
        let count = self.page_sizes.len() as u32;
        if !self.editable || count == 0 {
            return;
        }
        let at = at.min(count);
        // 1: a blank page the size of the one before. Split (0) and pages from a file (2) need
        // the state unborrowed, so they do not come here ([`tabs::split_at`]).
        if which == 1 {
            let size = self.page_sizes[at.saturating_sub(1) as usize];
            self.send_edit(EditOp::Pages(PageOp::InsertBlank {
                at,
                width: size.width,
                height: size.height,
            }));
        }
    }

    /// Keys in the gallery (Plan 0007 Phase 3): the arrows, Home and End go through the pages (with
    /// Shift choosing the run), Ctrl+A chooses every page, Esc unchooses them, Delete deletes the
    /// chosen pages. Zoom keys do nothing there. `None` when the key is not the gallery's.
    fn gallery_key(&mut self, ui: &MainWindow, text: &str, ctrl: bool, shift: bool) -> Option<()> {
        let mut chars = text.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else {
            return None;
        };
        let count = self.page_sizes.len() as u32;
        if count == 0 {
            return None;
        }
        let current = (ui.get_current_page().max(0) as u32).min(count - 1);
        let step = |to: u32| to.min(count - 1);
        let target = if c == char::from(Key::LeftArrow) || c == char::from(Key::UpArrow) {
            Some(current.saturating_sub(1))
        } else if c == char::from(Key::RightArrow) || c == char::from(Key::DownArrow) {
            Some(step(current + 1))
        } else if c == char::from(Key::Home) {
            Some(0)
        } else if c == char::from(Key::End) {
            Some(count - 1)
        } else {
            None
        };
        if let Some(target) = target {
            if !ctrl {
                if shift && self.page_sel_anchor.is_none() {
                    self.page_sel_anchor = Some(current);
                }
                self.thumb_clicked(ui, target, false, shift);
                if shift {
                    self.go_to_page(ui, target as usize);
                }
            }
            return Some(());
        }
        if ctrl && (c == 'a' || c == 'A') {
            self.page_sel = (0..count).collect();
            self.page_sel_anchor = Some(0);
            self.show_page_sel(ui);
            return Some(());
        }
        if ctrl && matches!(c, '+' | '=' | '-' | '0') {
            return Some(());
        }
        if c == char::from(Key::Escape) && !self.page_sel.is_empty() {
            self.page_sel.clear();
            self.page_sel_anchor = None;
            self.show_page_sel(ui);
            return Some(());
        }
        if c == char::from(Key::Delete) {
            self.page_op(ui, 3);
            return Some(());
        }
        None
    }

    /// The drag ended on a row: the pages go before it, or after it.
    fn thumb_dropped(&mut self, row: u32, after: bool) {
        let pages = std::mem::take(&mut self.drag_pages);
        if pages.is_empty() || !self.editable {
            return;
        }
        let count = self.page_sizes.len() as u32;
        let before = if after { row + 1 } else { row }.min(count);
        self.send_edit(EditOp::Pages(PageOp::Move { pages, before }));
    }

    fn page_revision(&self, page: u32) -> u32 {
        self.page_revs.get(&page).copied().unwrap_or(0)
    }

    /// An edit changed a page (Plan 0004, §4.1): everything drawn of it is out of date. Its tiles
    /// on screen stay until their new pixels arrive, so the page never blanks; its base layer
    /// and thumbnail are asked for again.
    fn page_changed(
        &mut self,
        ui: &MainWindow,
        generation: u64,
        page: u32,
        revision: u32,
        items: Vec<AnnotInfo>,
    ) {
        if generation != self.doc_generation || revision <= self.page_revision(page) {
            return;
        }
        self.page_revs.insert(page, revision);
        self.annots_pending.remove(&page);
        self.fields_changed(page);
        if let Some((selected_page, id)) = self.selected_annot
            && selected_page == page
            && !items.iter().any(|item| item.id == id)
        {
            self.select_annotation(ui, None);
        }
        self.annots.insert(page, items);
        let actions = self.scheduler.invalidate_page(page);
        self.apply(ui, actions);
        if self.bases.contains_key(&page) || self.base_pending.contains(&page) {
            self.base_pending.insert(page);
            self.request_base(page);
        }
        if self.thumb_cache.contains_key(&page) || self.thumb_inflight.contains_key(&page) {
            self.send_thumb(page);
        }
        let stale = self.scheduler.stale_visible(page);
        self.bench.page_changed(page, stale);
        if stale == 0 {
            self.ink_drawn(ui, page);
        }
        self.update_annot_selection(ui);
    }

    /// Sends one edit of the open document.
    fn send_edit(&mut self, op: EditOp) {
        if !self.editable {
            return;
        }
        self.next_edit_id += 1;
        if matches!(op, EditOp::Pages(_)) {
            self.bench.restructure_sent();
        } else {
            self.bench.edit_sent(op.page());
        }
        let _ = self.cmd_tx.send(EngineCmd::Edit {
            op,
            edit_id: self.next_edit_id,
            generation: self.doc_generation,
        });
    }

    /// The engine applied an edit, or refused it. An applied edit is counted when its `History`
    /// arrives, like an undo or a redo.
    fn edited(&mut self, ui: &MainWindow, generation: u64, result: Result<(), String>) {
        if generation != self.doc_generation {
            return;
        }
        if let Err(message) = result {
            ui.set_status_text(SharedString::from(format!("Could not edit: {message}")));
            if let Some(page) = self.ink_shown {
                self.ink_drawn(ui, page);
            }
        }
    }

    /// Ctrl+Z: undo the last edit (Plan 0004 Phase 2, ADR 0008).
    fn undo(&mut self) {
        if self.editable {
            let _ = self.cmd_tx.send(EngineCmd::Undo {
                generation: self.doc_generation,
            });
        }
    }

    /// Ctrl+Y or Ctrl+Shift+Z: redo the last undone edit.
    fn redo(&mut self) {
        if self.editable {
            let _ = self.cmd_tx.send(EngineCmd::Redo {
                generation: self.doc_generation,
            });
        }
    }

    /// The undo history moved: count the step for the unsaved-changes state, and enable Undo and
    /// Redo for what is left. A report that moved nothing (after opening or a Save As) is the
    /// engine's state whatever the generation; a step of a previous document is dropped.
    fn history(
        &mut self,
        ui: &MainWindow,
        generation: u64,
        change: HistoryChange,
        can_undo: bool,
        can_redo: bool,
    ) {
        if change != HistoryChange::None && generation != self.doc_generation {
            return;
        }
        match change {
            HistoryChange::Edited => self.save_state.edited(),
            HistoryChange::Undone => self.save_state.undone(),
            HistoryChange::Redone => self.save_state.redone(),
            HistoryChange::None => {}
        }
        self.can_undo = can_undo;
        self.can_redo = can_redo;
        ui.set_dirty(self.has_unsaved_changes());
        ui.set_can_undo(self.editable && can_undo);
        ui.set_can_redo(self.editable && can_redo);
    }

    /// Marks the selected text: one markup annotation per page of the selection.
    fn mark_selection(&mut self, ui: &MainWindow, kind: MarkupKind) {
        let colour = PALETTE[self.colour];
        let parts = std::mem::take(&mut self.sel_pages);
        for part in parts.into_iter().filter(|part| !part.rects.is_empty()) {
            self.send_edit(EditOp::AddMarkup {
                page: part.page,
                kind,
                rects: part.rects,
                colour,
                author: self.settings.author.clone(),
            });
        }
        self.clear_selection(ui);
    }

    /// Marks the selection once the engine has answered the newest request for it: a drag's last
    /// position may still be on its way when the button comes up.
    fn mark_when_answered(&mut self, ui: &MainWindow, kind: MarkupKind) {
        if self.sel_answered == self.sel_generation {
            self.mark_selection(ui, kind);
        } else {
            self.pending_mark = Some(kind);
        }
    }

    /// A tool button or its key (Plan 0004, §4.2): with text selected, a markup marks it and the
    /// tool stays; with nothing selected, the markup tool is switched on (or off again).
    fn choose_tool(&mut self, ui: &MainWindow, tool: Tool) {
        if matches!(tool, Tool::Note | Tool::Ink | Tool::Crop) && !self.editable {
            return;
        }
        if let Tool::Markup(kind) = tool
            && self.editable
            && !self.sel_pages.is_empty()
        {
            self.mark_selection(ui, kind);
            return;
        }
        self.tool = if self.tool == tool {
            Tool::Select
        } else {
            tool
        };
        ui.set_tool(self.tool.index());
        if self.tool != Tool::Crop {
            self.crop_cancel(ui);
        }
    }

    /// A swatch: the colour of the next markup, and of the selected annotation.
    fn choose_colour(&mut self, ui: &MainWindow, index: usize) {
        if index >= PALETTE.len() {
            return;
        }
        self.colour = index;
        ui.set_colour_index(index as i32);
        if let Some((page, id)) = self.selected_annot {
            self.send_edit(EditOp::SetColour {
                page,
                id,
                colour: PALETTE[index],
            });
        }
    }

    /// Delete: removes the selected annotation.
    fn delete_annotation(&mut self, ui: &MainWindow) {
        if let Some((page, id)) = self.selected_annot {
            self.select_annotation(ui, None);
            self.send_edit(EditOp::Delete { page, id });
        }
    }

    /// Esc: back to the Select tool, nothing selected.
    fn escape(&mut self, ui: &MainWindow) {
        self.finish_ink(ui, false);
        self.find_close(ui);
        self.clear_selection(ui);
        self.select_annotation(ui, None);
        self.tool = Tool::Select;
        ui.set_tool(self.tool.index());
    }

    /// Asks the engine to write the document: in place (`None`) or as a copy that becomes the
    /// document (Plan 0004, §4.4).
    fn send_save(&mut self, ui: &MainWindow, path: Option<PathBuf>) {
        if !self.editable || self.doc_path.is_none() || self.saving {
            return;
        }
        self.saving = true;
        ui.set_status_text(SharedString::from("Saving..."));
        self.bench.save_sent();
        let _ = self.cmd_tx.send(EngineCmd::Save { path });
    }

    /// A save finished. Returns what the caller still has to do outside the state: the action
    /// that waited for the save, or a Save As the file needs.
    fn saved(
        &mut self,
        ui: &MainWindow,
        path: PathBuf,
        result: Result<u64, SaveError>,
    ) -> Option<SavedNext> {
        self.saving = false;
        match result {
            Ok(bytes) => {
                self.save_state.saved();
                ui.set_dirty(false);
                if self.doc_path.as_deref() != Some(path.as_path()) {
                    // Save As: the copy is the document now.
                    let name = path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    ui.set_doc_title(SharedString::from(name));
                    self.settings.opened(&path);
                    self.save_settings();
                    self.update_recent();
                    self.doc_path = Some(path.clone());
                    // A document made from pages has a file of its own now.
                    self.untitled = None;
                    self.origin_dir = None;
                }
                ui.set_status_text(SharedString::from(format!(
                    "Saved {} ({:.1} KB)",
                    path.display(),
                    bytes as f64 / 1024.0
                )));
                self.bench.saved(bytes);
                self.after_save.take().map(SavedNext::Then)
            }
            Err(SaveError::NeedsSaveAs) => {
                ui.set_status_text(SharedString::from(
                    "This file cannot be updated in place; choose where to save a copy",
                ));
                Some(SavedNext::SaveAs)
            }
            Err(SaveError::NotEditable) => {
                self.after_save = None;
                ui.set_status_text(SharedString::from("Only PDF documents can be saved"));
                None
            }
            Err(SaveError::Failed(message)) => {
                self.after_save = None;
                ui.set_has_error(true);
                ui.set_error_text(SharedString::from(format!("Could not save: {message}")));
                ui.set_status_text(SharedString::from(format!("Could not save: {message}")));
                None
            }
        }
    }

    /// The name the title bar shows for the document.
    fn doc_name(&self) -> String {
        if let Some(title) = &self.untitled {
            return title.clone();
        }
        self.doc_path
            .as_deref()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Whether closing the document now would lose edits.
    fn has_unsaved_changes(&self) -> bool {
        self.editable && (self.save_state.is_dirty() || self.untitled.is_some())
    }

    /// The user chose not to save: closing the document now loses nothing they want.
    fn discard_changes(&mut self) {
        self.save_state = SaveState::new();
        self.untitled = None;
    }

    /// Ctrl+C: the selected text to the clipboard (Plan 0003, §3.3).
    fn copy_selection(&mut self, ui: &MainWindow) {
        if self.sel_text.is_empty() {
            return;
        }
        let copied = CLIPBOARD.with(|cell| {
            let mut slot = cell.borrow_mut();
            if slot.is_none() {
                *slot = arboard::Clipboard::new().ok();
            }
            slot.as_mut()
                .map(|clipboard| clipboard.set_text(self.sel_text.clone()).is_ok())
                .unwrap_or(false)
        });
        let chars = self.sel_text.chars().count();
        ui.set_status_text(SharedString::from(if copied {
            format!("Copied {chars} characters")
        } else {
            "Could not copy to the clipboard".to_owned()
        }));
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
        let rows: Vec<ThumbRow> = (0..pages).map(|page| self.empty_thumb(page)).collect();
        self.thumbs_model.set_vec(rows);
        self.thumb_cache.clear();
        self.thumb_inflight.clear();
        self.current_thumb = None;
    }

    fn empty_thumb(&self, page: usize) -> ThumbRow {
        ThumbRow {
            image: Image::default(),
            has_image: false,
            label: SharedString::from((page + 1).to_string()),
            current: false,
            selected: false,
            ratio: self.page_ratio(page),
        }
    }

    /// A page's width over its height, as shown.
    fn page_ratio(&self, page: usize) -> f32 {
        self.page_sizes
            .get(page)
            .filter(|size| size.width > 0.0 && size.height > 0.0)
            .map_or(1.0 / std::f32::consts::SQRT_2, |size| {
                size.width / size.height
            })
    }

    /// The thumbnail rows follow a structural edit (Plan 0007 Phase 3): a row whose page only
    /// moved stays and takes its new place, so the gallery slides it there instead of building it
    /// again; the other old rows are removed and new ones inserted ([`kept_rows`]). Pictures of
    /// pages that only moved are kept; turned and new pages are drawn again.
    fn remap_thumbs(&mut self, map: &PageMap, old_count: u32) {
        let old_rows = (self.thumbs_model.row_count() as u32).min(old_count);
        let kept = kept_rows(map, old_rows);
        let kept_old: HashSet<u32> = kept.iter().flatten().copied().collect();
        let mut pictures: HashMap<u32, (Image, u32)> = HashMap::new();
        for (&old, &(_, width)) in &self.thumb_cache {
            if let Some(row) = self.thumbs_model.row_data(old as usize)
                && row.has_image
            {
                pictures.insert(old, (row.image, width));
            }
        }
        for old in (0..old_rows).rev() {
            if !kept_old.contains(&old) {
                self.thumbs_model.remove(old as usize);
            }
        }
        let mut held = HashMap::new();
        for (new, from) in map.iter().enumerate() {
            let mut row = self.empty_thumb(new);
            if let Some((image, width)) = from.and_then(|old| pictures.get(&old)) {
                row.image = image.clone();
                row.has_image = true;
                self.thumb_tick += 1;
                held.insert(new as u32, (self.thumb_tick, *width));
            }
            if kept[new].is_some() {
                self.thumbs_model.set_row_data(new, row);
            } else {
                self.thumbs_model.insert(new, row);
            }
        }
        self.thumb_cache = held;
        self.thumb_inflight.clear();
        self.current_thumb = None;
    }

    fn in_gallery(&self) -> bool {
        self.mode == MODE_TOOLS
    }

    /// Whether a mode change is still moving: nothing new is drawn until it settles.
    fn in_motion(&self) -> bool {
        self.motion_until
            .is_some_and(|until| Instant::now() < until)
    }

    /// The width a page's thumbnail is drawn at, in pixels: the drawer's, or in the gallery the
    /// width that fits the page's box.
    fn thumb_width_px(&self, page: u32) -> u32 {
        let logical = if self.in_gallery() {
            GALLERY_BOX_W_PX.min(GALLERY_BOX_H_PX * self.page_ratio(page as usize))
        } else {
            THUMB_WIDTH_LOGICAL_PX
        };
        (logical * self.scale_factor).round().max(1.0) as u32
    }

    /// How many thumbnails are kept.
    fn thumb_cap(&self) -> usize {
        if self.in_gallery() {
            GALLERY_THUMB_CACHE
        } else {
            THUMB_CACHE
        }
    }

    /// The mode changed (Plan 0007 Phase 3). The motion only fades and scales what is drawn
    /// already; once it settles, the thumbnails the new mode shows are drawn at its size.
    fn mode_changed(&mut self, ui: &MainWindow) {
        let mode = ui.get_mode();
        if mode == self.mode {
            return;
        }
        self.mode = mode;
        let motion = Duration::from_millis(MODE_MOTION_MS + MODE_SETTLE_MS);
        self.motion_until = Some(Instant::now() + motion);
        if self.in_gallery() {
            // The gallery opens on the current page.
            self.current_thumb = None;
            let page = self.current_page(ui);
            self.set_current_thumb(ui, page);
        }
        let weak = ui.as_weak();
        slint::Timer::single_shot(motion, move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| {
                    if state.in_motion() {
                        return;
                    }
                    state.motion_until = None;
                    state.trim_thumbs();
                    state.pump_thumbs(&ui);
                });
            }
        });
    }

    /// Once the opened document's first view is drawn, one warm-up frame (main.slint `warm`).
    fn warm_when_covered(&mut self, ui: &MainWindow, visible: &[TileKey]) {
        if self.warm_pending && bench::holes(&self.scheduler, visible) == 0 {
            self.warm_pending = false;
            warm_up(ui);
        }
    }

    /// Scrolls the gallery so `page` is in view, in the middle when it was not.
    fn reveal_in_gallery(&mut self, ui: &MainWindow, page: usize) {
        let view = ui.get_gallery_view_w();
        let count = self.page_sizes.len() as f32;
        let content = GALLERY_LEAD_PX + count * GALLERY_PITCH_PX + GALLERY_PAD_PX;
        let max = (content - view).max(0.0);
        let left = GALLERY_LEAD_PX + page as f32 * GALLERY_PITCH_PX;
        let scroll = ui.get_gallery_scroll();
        if left < scroll + GALLERY_PAD_PX || left + GALLERY_CARD_PX > scroll + view - GALLERY_PAD_PX
        {
            ui.set_gallery_scroll((left - (view - GALLERY_CARD_PX) / 2.0).clamp(0.0, max));
            self.pump_thumbs(ui);
        }
    }

    /// Thumbnail rows in view plus the margin, in order from the top; empty while the thumbnail
    /// list is not shown.
    fn thumb_range(&self, ui: &MainWindow) -> std::ops::Range<usize> {
        let rows = self.thumbs_model.row_count();
        if rows == 0 {
            return 0..0;
        }
        if self.in_gallery() {
            let scroll = ui.get_gallery_scroll().max(0.0);
            let view = ui.get_gallery_view_w().max(0.0);
            let first = ((scroll - GALLERY_LEAD_PX) / GALLERY_PITCH_PX)
                .floor()
                .max(0.0) as usize;
            let last = ((scroll + view - GALLERY_LEAD_PX) / GALLERY_PITCH_PX)
                .ceil()
                .max(0.0) as usize;
            let first = first.saturating_sub(GALLERY_MARGIN_CARDS);
            let last = (last + GALLERY_MARGIN_CARDS).min(rows);
            return first.min(last)..last;
        }
        if !ui.get_sidebar_visible() || ui.get_sidebar_tab() != 0 {
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
    ///
    /// A held thumbnail drawn at another width (the drawer's in the gallery, or the other way) is
    /// shown scaled until the new one arrives. Nothing is asked for while a mode change moves.
    fn pump_thumbs(&mut self, ui: &MainWindow) {
        if self.in_motion() {
            return;
        }
        self.thumb_tick += 1;
        let tick = self.thumb_tick;
        for row in self.thumb_range(ui) {
            let page = row as u32;
            let wanted = self.thumb_width_px(page);
            if let Some(held) = self.thumb_cache.get_mut(&page) {
                held.0 = tick;
                if held.1 == wanted {
                    continue;
                }
            }
            if self.thumb_inflight.len() >= THUMB_INFLIGHT {
                break;
            }
            if !self.thumb_inflight.contains_key(&page) {
                self.send_thumb_at(page, wanted);
            }
        }
    }

    fn send_thumb(&mut self, page: u32) {
        let width_px = self.thumb_width_px(page);
        self.send_thumb_at(page, width_px);
    }

    fn send_thumb_at(&mut self, page: u32, width_px: u32) {
        self.thumb_inflight.insert(page, width_px);
        let _ = self.cmd_tx.send(EngineCmd::RenderThumb {
            page,
            width_px,
            request_id: self.doc_generation,
        });
    }

    /// A thumbnail arrived: show it, keep at most [`THUMB_CACHE`], and ask for the next one.
    fn apply_thumb(
        &mut self,
        ui: &MainWindow,
        page: u32,
        generation: u64,
        revision: u32,
        bitmap: &Bitmap,
    ) {
        // A thumbnail drawn before an edit is dropped: the one asked for after it is on its way.
        if generation != self.doc_generation || revision < self.page_revision(page) {
            return;
        }
        let width = self.thumb_inflight.remove(&page).unwrap_or(bitmap.width);
        let row = page as usize;
        if let Some(mut data) = self.thumbs_model.row_data(row) {
            data.image = to_image(bitmap);
            data.has_image = true;
            self.thumbs_model.set_row_data(row, data);
            self.thumb_tick += 1;
            self.thumb_cache.insert(page, (self.thumb_tick, width));
        }
        self.trim_thumbs();
        self.bench.thumbs_held(self.thumb_cache.len());
        self.pump_thumbs(ui);
    }

    /// Keeps at most [`AppState::thumb_cap`] thumbnails, dropping the longest out of view.
    fn trim_thumbs(&mut self) {
        while self.thumb_cache.len() > self.thumb_cap() {
            let Some((&oldest, _)) = self
                .thumb_cache
                .iter()
                .min_by_key(|(page, (tick, _))| (*tick, **page))
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
        if self.in_gallery() {
            self.reveal_in_gallery(ui, page);
            return;
        }
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
            // The destination lands just under the title row, as a page's top does.
            Some(top) => {
                slot.y as f64 + top as f64 * self.scale_milli as f64 / 1000.0
                    - self.margins.top as f64
            }
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
        let track = ui.get_sb_track_h() as f64;
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
        // A page step is the room the pages are seen in, so nothing passes under the islands
        // unseen; `scroll_to` clamps to the whole viewport.
        let (_, free_h) = self.free_view(ui);
        let target = scroll_target(
            action,
            self.pos.1,
            free_h,
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
        // The page fits the room the islands leave, not the whole window under them.
        let (view_w, view_h) = self.free_view(ui);
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
            self.layout = self.layout_for(ui, milli);
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
        // The text line boxes follow the same pages: asked for once, dropped when the page leaves.
        self.text_lines
            .retain(|page, _| near.contains(&(*page as usize)));
        self.lines_pending
            .retain(|page| near.contains(&(*page as usize)));
        for page in near.clone() {
            let page = page as u32;
            if !self.text_lines.contains_key(&page) && self.lines_pending.insert(page) {
                let _ = self.cmd_tx.send(EngineCmd::TextLines {
                    page,
                    generation: self.layout_generation,
                });
            }
        }
        // And so do the annotations, for hit-testing and the selection outline (Plan 0004).
        self.annots
            .retain(|page, _| near.contains(&(*page as usize)));
        self.annots_pending
            .retain(|page| near.contains(&(*page as usize)));
        if self.editable {
            for page in near.clone() {
                let page = page as u32;
                if !self.annots.contains_key(&page) && self.annots_pending.insert(page) {
                    let _ = self.cmd_tx.send(EngineCmd::Annotations {
                        page,
                        generation: self.layout_generation,
                    });
                }
            }
        }
        // And the form fields, to fill them (Plan 0006).
        let near_pages: Vec<u32> = near.clone().map(|page| page as u32).collect();
        self.request_fields(&near_pages);
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
        self.bench.tile_requested(key.page);
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

    /// A tile arrived: cache it and put it on screen. One drawn from an older revision of its page
    /// than the engine has reported shows the page before an edit, and is dropped.
    fn apply_tile(&mut self, ui: &MainWindow, key: &TileKey, revision: u32, bitmap: &Bitmap) {
        if revision < self.page_revision(key.page) {
            self.bench.stale_dropped();
            return;
        }
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
        self.warm_when_covered(ui, &visible);
        self.bench.restructure_tile(&self.scheduler, &visible);
        let stale = self.scheduler.stale_visible(key.page);
        self.bench.tile_after_edit(key.page, stale);
        if stale == 0 {
            self.ink_drawn(ui, key.page);
        }
    }

    /// The page's tiles show its new revision: a stroke drawn live on it is in them now.
    fn ink_drawn(&mut self, ui: &MainWindow, page: u32) {
        if self.ink_shown == Some(page) && self.ink.is_none() {
            self.ink_shown = None;
            ui.set_ink_commands(SharedString::new());
        }
    }

    /// A base layer arrived: stretch it under its page's tiles, if the page is still near the view.
    ///
    /// A base layer *is* the page, so it is valid across every zoom of that page; it is only
    /// dropped when the page has moved away from the viewport since it was requested.
    fn apply_base(&mut self, ui: &MainWindow, page: u32, revision: u32, bitmap: &Bitmap) {
        // Drawn before an edit: the base layer asked for after it is still pending.
        if revision < self.page_revision(page) || !self.base_pending.remove(&page) {
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
        self.clear_selection(ui);
        self.select_annotation(ui, None);
        self.note_cancel(ui);
        self.note_drag = None;
        ui.set_note_hover_visible(false);
        self.text_lines.clear();
        self.lines_pending.clear();
        self.annots.clear();
        self.annots_pending.clear();
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
        self.doc_path = Some(path.clone());
        ui.set_doc_title(SharedString::from(self.doc_name()));
        // A document made from pages is a file the app wrote, not one to reopen from the list.
        if self.untitled.is_none() {
            self.settings.opened(&path);
            self.save_settings();
            self.update_recent();
        }
        ui.set_dirty(self.has_unsaved_changes());
    }

    /// The engine could not open the document: the one that was showing is still open there, so
    /// it comes back with its page table and scroll position.
    fn open_failed(&mut self, ui: &MainWindow) {
        if self.opening.take().is_none() || self.page_sizes.is_empty() {
            return;
        }
        self.layout = self.layout_for(ui, self.scale_milli);
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
    fn set_document(&mut self, ui: &MainWindow, page_sizes: Vec<PageSize>, editable: bool) {
        self.crop_cancel(ui);
        self.field_cancel(ui);
        self.fields.clear();
        self.fields_pending.clear();
        self.field_focus = None;
        self.field_tab_pending = None;
        ui.set_has_field_focus(false);
        ui.set_field_focus_visible(false);
        ui.set_stamp_visible(false);
        ui.set_convert_visible(false);
        self.page_sizes = page_sizes;
        self.layout_generation += 1;
        self.page_sel.clear();
        self.page_sel_anchor = None;
        ui.set_pages_chosen(0);
        // The engine's revisions start again with the document, and so does the save state.
        self.page_revs.clear();
        self.editable = editable;
        ui.set_editable(editable);
        self.save_state = SaveState::new();
        ui.set_dirty(false);
        ui.set_can_undo(false);
        ui.set_can_redo(false);
        self.can_undo = false;
        self.can_redo = false;
        self.saving = false;
        self.after_save = None;
        if !editable {
            self.tool = Tool::Select;
            ui.set_tool(self.tool.index());
        }
        self.scale_milli = ZOOM_DEFAULT_MILLI;
        self.layout = self.layout_for(ui, self.scale_milli);
        self.reset_thumbs(self.page_sizes.len());
        ui.set_thumb_scroll_y(0.0);
        ui.set_gallery_scroll(0.0);
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
        // In the gallery the first cards are in view without a scroll that would ask for them:
        // a document opened there (a tab made by Split) asks here.
        self.pump_thumbs(ui);
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
        let to = self.layout_for(ui, new_milli);
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
    /// A `--bench-*` run's config until the first open takes it.
    static BENCH_CFG: RefCell<Option<bench::Cfg>> = const { RefCell::new(None) };
    /// The one clipboard handle, created on the first copy. Kept outside [`APP`] so `main` can drop
    /// it explicitly after the event loop: on X11 that drop hands the copied text to the clipboard
    /// manager, and Rust does not promise to run the main thread's thread-local destructors.
    static CLIPBOARD: RefCell<Option<arboard::Clipboard>> = const { RefCell::new(None) };
}

/// Per-page entries moved to the pages they went to (Plan 0005, §4.1); a page `map` gives no old
/// page has none.
fn remap<T: Clone>(map: &[Option<u32>], old: &HashMap<u32, T>) -> HashMap<u32, T> {
    map.iter()
        .enumerate()
        .filter_map(|(new, from)| Some((new as u32, old.get(&(*from)?)?.clone())))
        .collect()
}

/// Runs `f` with the programmatic-update guard raised.
fn with_programmatic_guard<T>(f: impl FnOnce() -> T) -> T {
    PROGRAMMATIC.with(|flag| flag.set(true));
    let out = f();
    PROGRAMMATIC.with(|flag| flag.set(false));
    out
}

/// A page action from the gallery on the chosen pages (or the current one): Extract asks for a
/// file with the state unborrowed; the rest are page operations ([`AppState::page_op`]).
fn gallery_run(ui: &MainWindow, which: i32) {
    if which == 7 {
        tools_ui::extract_pages(ui);
    } else {
        with_state(|state| state.page_op(ui, which));
    }
}

/// Shows every mode's dot grid, gallery and commands for one frame, unseen (main.slint `warm`).
fn warm_up(ui: &MainWindow) {
    ui.set_warm(true);
    let weak = ui.as_weak();
    slint::Timer::single_shot(Duration::from_millis(WARM_MS), move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_warm(false);
        }
    });
}

/// Applies one engine event of the active tab (Plan 0007 Phase 4: [`tabs`] routes them, and keeps
/// a parked tab's until it is shown again).
fn apply_event(ui: &MainWindow, event: EngineEvent) {
    match event {
        EngineEvent::Opened {
            page_count,
            page_sizes,
            unsized_pages,
            editable,
            repaired,
            form,
        } => {
            ui.set_has_error(false);
            ui.set_error_text(SharedString::from(""));
            ui.set_total_pages(page_count as i32);
            ui.set_current_page(0);
            ui.set_is_loading(false);
            // A page whose bounds could not be read is laid out as Letter; say so rather
            // than draw it at a size the document never had without a word.
            let pages = if page_count == 1 { "page" } else { "pages" };
            let mut status = if unsized_pages.is_empty() {
                format!("{page_count} {pages}")
            } else {
                format!(
                    "{page_count} {pages} ({} with unreadable sizes, shown as Letter)",
                    unsized_pages.len()
                )
            };
            // A form whose scripts do not run here (Plan 0006, §0 question 2), or an XFA
            // form MuPDF cannot read.
            if form.xfa {
                status.push_str(" - this form is XFA, which PODEFA cannot fill");
            } else if form.scripts {
                status.push_str(
                    " - this form has scripts (calculations, checks), which PODEFA does not run",
                );
            }
            ui.set_form_fields(form.fields);
            with_state(|state| state.form = form);
            // A damaged file MuPDF repaired: a full copy is a clean one (Plan 0005 Phase 4).
            ui.set_repaired(repaired);
            with_state(|state| state.repaired = repaired);
            if repaired {
                status.push_str(
                    " - the file was damaged and repaired when opened: \
                     Save > Save Repaired Copy writes a clean copy",
                );
            }
            ui.set_status_text(SharedString::from(status));
            with_state(|state| {
                state.set_document(ui, page_sizes, editable);
                state.document_opened(ui);
                state.warm_pending = true;
            });
            // A scripted run starts on the open, so its first step is a real zoom or pan
            // rather than a no-op against an empty document.
            if let Some(cfg) = BENCH_CFG.with(|cell| cell.borrow_mut().take()) {
                bench::start(ui, cfg);
            }
        }
        EngineEvent::TileRendered {
            key,
            request_id,
            revision,
            bitmap,
        } => {
            if current_epoch() != Some(request_id) {
                return;
            }
            with_state(|state| state.apply_tile(ui, &key, revision, &bitmap));
        }
        EngineEvent::BaseRendered {
            page,
            revision,
            bitmap,
            ..
        } => {
            // No epoch guard: a base layer belongs to a page, not to a zoom level, and it
            // arrives while the epoch it was requested under is already superseded.
            with_state(|state| state.apply_base(ui, page, revision, &bitmap));
        }
        EngineEvent::ThumbRendered {
            page,
            request_id,
            revision,
            bitmap,
        } => {
            with_state(|state| state.apply_thumb(ui, page, request_id, revision, &bitmap));
        }
        EngineEvent::SearchHits {
            generation,
            page,
            hits,
        } => {
            with_state(|state| state.apply_hits(ui, generation, page, hits));
        }
        EngineEvent::SearchDone {
            generation,
            pages,
            ms,
        } => {
            with_state(|state| state.search_done(ui, generation, pages, ms));
        }
        EngineEvent::Selection {
            generation,
            pages,
            text,
        } => {
            with_state(|state| state.apply_selection(ui, generation, pages, text));
        }
        EngineEvent::TextLines {
            page,
            generation,
            lines,
        } => {
            with_state(|state| state.apply_text_lines(page, generation, lines));
        }
        EngineEvent::Outline { items } => {
            with_state(|state| state.set_outline(items));
        }
        EngineEvent::Annotations {
            page,
            generation,
            items,
        } => {
            with_state(|state| state.apply_annotations(ui, page, generation, items));
        }
        EngineEvent::Fields {
            page,
            generation,
            items,
        } => {
            with_state(|state| state.apply_fields(ui, page, generation, items));
        }
        EngineEvent::FieldPage {
            generation,
            page,
            back,
        } => {
            with_state(|state| state.field_page(ui, generation, page, back));
        }
        EngineEvent::FormExported {
            generation,
            path,
            result,
        } => {
            with_state(|state| state.form_exported(ui, generation, &path, result));
        }
        EngineEvent::PageChanged {
            generation,
            page,
            revision,
            items,
        } => {
            with_state(|state| state.page_changed(ui, generation, page, revision, items));
        }
        EngineEvent::Restructured {
            generation,
            page_sizes,
            revision,
            map,
        } => {
            with_state(|state| state.restructured(ui, generation, page_sizes, revision, map));
        }
        EngineEvent::ToolProgress {
            job_id,
            done,
            total,
        } => {
            with_state(|state| state.tool_progress(ui, job_id, done, total));
        }
        EngineEvent::ToolDone {
            job_id,
            result,
            sizes,
        } => {
            // A job a tab started (Plan 0007 Phase 4) hands its pages to the tab, not to a dialog.
            let tab = with_state_ret(|state| {
                state
                    .running_tool
                    .as_mut()
                    .filter(|tool| tool.job_id == job_id)
                    .and_then(|tool| tool.tab.take())
            })
            .flatten();
            let written = result.is_ok();
            let outcome =
                with_state_ret(|state| state.tool_done(ui, job_id, result, sizes)).flatten();
            match tab {
                Some(action) if written => tabs::pages_written(ui, action),
                Some(action) => {
                    tabs::discard(action);
                    tools_ui::tool_finished(ui, outcome);
                }
                None => tools_ui::tool_finished(ui, outcome),
            }
        }
        EngineEvent::DocumentInfo { generation, info } => {
            with_state(|state| state.document_info(ui, generation, info));
        }
        EngineEvent::Edited {
            generation, result, ..
        } => {
            with_state(|state| state.edited(ui, generation, result));
        }
        EngineEvent::History {
            generation,
            change,
            can_undo,
            can_redo,
        } => {
            with_state(|state| state.history(ui, generation, change, can_undo, can_redo));
        }
        EngineEvent::Saved { path, result } => {
            match with_state_ret(|state| state.saved(ui, path, result)).flatten() {
                Some(SavedNext::Then(then)) => run_after(ui, then),
                Some(SavedNext::SaveAs) => save_as(ui),
                None => {}
            }
        }
        EngineEvent::TileAborted { ms, .. } => {
            with_state(|state| state.bench.aborted(ms));
        }
        EngineEvent::Error { message } => {
            ui.set_has_error(true);
            ui.set_error_text(SharedString::from(message.clone()));
            ui.set_status_text(SharedString::from(format!("Error: {message}")));
            ui.set_is_loading(false);
            with_state(|state| state.open_failed(ui));
        }
    }
}

/// Ends a scripted run with `code`. The tabs' engines stop first: left in their thread-local
/// slot, they would be torn down while the process exits and its other threads are already gone.
pub(crate) fn exit_process(code: i32) -> ! {
    tabs::shutdown();
    std::process::exit(code)
}

/// The epoch the active tab's tiles belong to.
fn current_epoch() -> Option<u64> {
    APP.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|state| state.current_epoch.load(Ordering::Relaxed))
    })
}

/// Runs `f` on the application state, if a document is loaded.
fn with_state(f: impl FnOnce(&mut AppState)) {
    APP.with(|cell| {
        if let Some(state) = cell.borrow_mut().as_mut() {
            f(state);
        }
    });
}

/// [`with_state`], returning what `f` returns.
fn with_state_ret<T>(f: impl FnOnce(&mut AppState) -> T) -> Option<T> {
    APP.with(|cell| cell.borrow_mut().as_mut().map(f))
}

/// Before `then` closes the document, asks whether to save its unsaved changes (Plan 0004,
/// §4.4): Yes saves and runs `then` once the save succeeded, No runs it at once, Cancel does
/// nothing. Called with the state unborrowed, because the dialog is modal.
fn proceed(ui: &MainWindow, then: AfterSave) {
    let unsaved =
        with_state_ret(|state| state.has_unsaved_changes().then(|| state.doc_name())).flatten();
    let Some(name) = unsaved else {
        run_after(ui, then);
        return;
    };
    let answer = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Warning)
        .set_title("PODEFA")
        .set_description(format!("Save the changes to \"{name}\" before closing it?"))
        .set_buttons(rfd::MessageButtons::YesNoCancel)
        .show();
    match answer {
        rfd::MessageDialogResult::Yes => {
            with_state(|state| state.after_save = Some(then));
            save(ui);
        }
        rfd::MessageDialogResult::No => run_after(ui, then),
        _ => {}
    }
}

/// Runs the action that waited for "save changes?".
fn run_after(ui: &MainWindow, then: AfterSave) {
    match then {
        AfterSave::Quit => {
            // Answered: this document may close now without asking again; the next one asks.
            with_state(|state| state.discard_changes());
            tabs::quit(ui);
        }
        AfterSave::CloseTab(id) => tabs::close_now(ui, id),
    }
}

/// Save: in place, or through Save As for a document that has no file of its own yet.
fn save(ui: &MainWindow) {
    if with_state_ret(|state| state.untitled.is_some()).unwrap_or(false) {
        save_as(ui);
    } else {
        with_state(|state| state.send_save(ui, None));
    }
}

/// File > Save As: the platform's save dialog, then a full copy that becomes the document.
fn save_as(ui: &MainWindow) {
    let Some((dir, name)) = with_state_ret(|state| {
        let current = state.doc_path.clone().filter(|_| state.editable)?;
        // A document made from pages starts where the pages came from, under its tab's name.
        Some(match &state.untitled {
            Some(title) => (state.origin_dir.clone(), Some(format!("{title}.pdf"))),
            None => (
                current.parent().map(Path::to_path_buf),
                current
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned()),
            ),
        })
    })
    .flatten() else {
        return;
    };
    let mut dialog = rfd::FileDialog::new().add_filter("PDF document", &["pdf"]);
    if let Some(dir) = dir {
        dialog = dialog.set_directory(dir);
    }
    if let Some(name) = name {
        dialog = dialog.set_file_name(name);
    }
    match dialog.save_file() {
        Some(path) => with_state(|state| state.send_save(ui, Some(path))),
        // Cancelled: whatever waited for this save does not happen.
        None => with_state(|state| state.after_save = None),
    }
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
            if state.ink.is_some() {
                state.rebuild_ink(ui);
            }
            if state.crop.is_some() {
                state.rebuild_crop(ui);
            }
            if state.field_edit.is_some() {
                state.place_field_editor(ui);
            }
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
    Copy,
    Save,
    SaveAs,
    Mark(MarkupKind),
    NoteTool,
    InkTool,
    DeleteAnnotation,
    Undo,
    Redo,
    /// Ctrl+W: closes the tab, asking first about unsaved changes; the last tab closes the window
    /// (Plan 0007 §0 and Phase 4).
    Close,
    /// Ctrl+T: a file into a new tab.
    NewTab,
    /// Ctrl+Tab and Ctrl+Page Down; Ctrl+Shift+Tab and Ctrl+Page Up.
    NextTab,
    PreviousTab,
}

impl KeyCommand {
    /// Whether the key works wherever the keyboard is, a text field included (Plan 0007 Phase 2):
    /// the commands the menu bar's shortcuts reached before the window lost its menu bar. Keys a
    /// text field needs for itself (copy, undo, arrows, Delete, Esc) stay with the viewport.
    fn is_global(self) -> bool {
        matches!(
            self,
            KeyCommand::Open
                | KeyCommand::Save
                | KeyCommand::SaveAs
                | KeyCommand::Close
                | KeyCommand::NewTab
                | KeyCommand::NextTab
                | KeyCommand::PreviousTab
                | KeyCommand::Find
                | KeyCommand::ToggleSidebar
                | KeyCommand::ZoomIn
                | KeyCommand::ZoomOut
                | KeyCommand::ZoomReset
        )
    }
}

/// Maps a Slint key event onto a command (Plan 0002, §4.6). `text` is what Slint reports: a
/// printable character, or one of the private-use code points of [`Key`].
fn key_command(text: &str, ctrl: bool, shift: bool) -> Option<KeyCommand> {
    let mut chars = text.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return None;
    };
    if ctrl {
        if c == char::from(Key::Tab) || c == char::from(Key::Backtab) {
            return Some(if shift || c == char::from(Key::Backtab) {
                KeyCommand::PreviousTab
            } else {
                KeyCommand::NextTab
            });
        }
        if c == char::from(Key::PageDown) {
            return Some(KeyCommand::NextTab);
        }
        if c == char::from(Key::PageUp) {
            return Some(KeyCommand::PreviousTab);
        }
        return match c {
            '+' | '=' => Some(KeyCommand::ZoomIn),
            '-' => Some(KeyCommand::ZoomOut),
            '0' => Some(KeyCommand::ZoomReset),
            'o' | 'O' => Some(KeyCommand::Open),
            'b' | 'B' => Some(KeyCommand::ToggleSidebar),
            'f' | 'F' => Some(KeyCommand::Find),
            'c' | 'C' => Some(KeyCommand::Copy),
            's' | 'S' if shift => Some(KeyCommand::SaveAs),
            's' | 'S' => Some(KeyCommand::Save),
            'z' | 'Z' if shift => Some(KeyCommand::Redo),
            'z' | 'Z' => Some(KeyCommand::Undo),
            'y' | 'Y' => Some(KeyCommand::Redo),
            'w' | 'W' => Some(KeyCommand::Close),
            't' | 'T' => Some(KeyCommand::NewTab),
            _ => None,
        };
    }
    if c == char::from(Key::Delete) {
        return Some(KeyCommand::DeleteAnnotation);
    }
    match c {
        'h' | 'H' => return Some(KeyCommand::Mark(MarkupKind::Highlight)),
        'u' | 'U' => return Some(KeyCommand::Mark(MarkupKind::Underline)),
        'k' | 'K' => return Some(KeyCommand::Mark(MarkupKind::StrikeOut)),
        'n' | 'N' => return Some(KeyCommand::NoteTool),
        'i' | 'I' => return Some(KeyCommand::InkTool),
        _ => {}
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
    bench::mark_process_start();
    let main_window = MainWindow::new()?;

    // `--bench-*` flags script a run; `FA_PDF_TRACE=1` records one by hand. Both are inert
    // otherwise, and neither belongs in CI: every number here is machine-specific.
    let args: Vec<String> = env::args().collect();
    let (paths, cfg) = bench::parse_args(&args);
    let trace_on = bench::trace_enabled() || !cfg.is_empty();
    if trace_on {
        bench::count_frames(&main_window);
    }
    main_window.set_diagnostics(trace_on);
    // macOS keeps its native frame and traffic lights (Plan 0007 §0, question 4); elsewhere the
    // window draws its own.
    main_window.set_own_frame(!cfg!(target_os = "macos"));
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
    // The run's config is taken by the first open.
    BENCH_CFG.with(|cell| *cell.borrow_mut() = (!cfg.is_empty()).then_some(cfg));

    // The first tab's engine; every tab's engine rasterizes on one shared pool (ADR 0011).
    let cmd_tx = tabs::init(&main_window);

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
    main_window.set_field_highlight(!settings.plain_fields);
    let thumbs_model = Rc::new(VecModel::<ThumbRow>::default());
    main_window.set_thumbs(ModelRc::from(thumbs_model.clone()));
    let outline_model = Rc::new(VecModel::<OutlineRow>::default());
    main_window.set_outline_rows(ModelRc::from(outline_model.clone()));
    let hits_model = Rc::new(VecModel::<HitView>::default());
    main_window.set_hit_rects(ModelRc::from(hits_model.clone()));
    let sel_model = Rc::new(VecModel::<HitView>::default());
    main_window.set_sel_rects(ModelRc::from(sel_model.clone()));
    let annot_sel_model = Rc::new(VecModel::<HitView>::default());
    main_window.set_annot_sel_rects(ModelRc::from(annot_sel_model.clone()));
    let field_model = Rc::new(VecModel::<FieldView>::default());
    main_window.set_field_rects(ModelRc::from(field_model.clone()));
    let swatches: Vec<slint::Color> = PALETTE
        .iter()
        .map(|c| slint::Color::from_rgb_f32(c.r, c.g, c.b))
        .collect();
    main_window.set_swatches(ModelRc::new(VecModel::from(swatches)));
    main_window.set_word_export(cfg!(feature = "docx"));
    let merge_model = Rc::new(VecModel::<SharedString>::default());
    main_window.set_merge_files(ModelRc::from(merge_model.clone()));

    APP.with(|cell| {
        *cell.borrow_mut() = Some(AppState::new(
            cmd_tx.clone(),
            WindowParts {
                next_epoch: Arc::clone(&next_epoch),
                current_epoch: Arc::clone(&current_epoch),
                model: model.clone(),
                pages_model: pages_model.clone(),
                recent_model,
                thumbs_model,
                outline_model,
                hits_model,
                sel_model,
                annot_sel_model,
                field_model,
                merge_model: merge_model.clone(),
            },
            TileScheduler::new(capacity, cache_bytes),
            settings,
            settings_path,
            main_window.window().scale_factor(),
            bench::Trace::new(trace_on),
        ));
    });

    // Engine events are applied on the UI thread ([`apply_event`]); a tile from a superseded epoch
    // is dropped before it reaches the scheduler.
    // The memory sampler starts here rather than at the first document: the resident set before
    // anything is open is the baseline a plateau is read against, and on a `--bench-*` run it also
    // covers the document's own first render.
    if trace_on {
        bench::start_sampler();
    }

    // `viewport_moved` is raised by the Flickable on every content-x/y/size change.
    {
        let weak = main_window.as_weak();
        main_window.on_viewport_moved(move || {
            if let Some(ui) = weak.upgrade() {
                viewport_moved(&ui);
            }
        });
        let weak = main_window.as_weak();
        main_window.on_insets_changed(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.insets_changed(&ui));
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
            // Tab between form fields, Space or Enter on a focused check box (Plan 0006 Phase 2).
            if !ctrl && with_state_ret(|state| state.field_key(&ui, &text, shift)) == Some(true) {
                return true;
            }
            if ui.get_mode() == MODE_TOOLS
                && with_state_ret(|state| state.gallery_key(&ui, &text, ctrl, shift)).flatten()
                    == Some(())
            {
                return true;
            }
            let Some(command) = key_command(&text, ctrl, shift) else {
                return false;
            };
            let (ax, ay) = viewport_center(&ui);
            // The file dialogs are modal, so they run with the state unborrowed.
            match command {
                KeyCommand::Open | KeyCommand::NewTab => {
                    tabs::pick(&ui);
                    return true;
                }
                KeyCommand::Save => {
                    save(&ui);
                    return true;
                }
                KeyCommand::SaveAs => {
                    save_as(&ui);
                    return true;
                }
                KeyCommand::Close => {
                    tabs::close_active(&ui);
                    return true;
                }
                KeyCommand::NextTab => {
                    tabs::step(&ui, true);
                    return true;
                }
                KeyCommand::PreviousTab => {
                    tabs::step(&ui, false);
                    return true;
                }
                _ => {}
            }
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
                KeyCommand::FindClose => state.escape(&ui),
                KeyCommand::Copy => state.copy_selection(&ui),
                KeyCommand::Mark(kind) => {
                    if state.editable {
                        state.choose_tool(&ui, Tool::Markup(kind));
                    }
                }
                KeyCommand::DeleteAnnotation => state.delete_annotation(&ui),
                KeyCommand::NoteTool => state.choose_tool(&ui, Tool::Note),
                KeyCommand::InkTool => state.choose_tool(&ui, Tool::Ink),
                KeyCommand::Undo => state.undo(),
                KeyCommand::Redo => state.redo(),
                KeyCommand::Open
                | KeyCommand::NewTab
                | KeyCommand::Save
                | KeyCommand::SaveAs
                | KeyCommand::Close
                | KeyCommand::NextTab
                | KeyCommand::PreviousTab => {}
            });
            true
        });
    }

    // The shortcuts that work from any field: the window sees each key before the focused
    // element and hands these on as if the viewport had them.
    {
        let weak = main_window.as_weak();
        main_window.on_global_key(move |text, ctrl, shift| {
            let Some(ui) = weak.upgrade() else {
                return false;
            };
            match key_command(&text, ctrl, shift) {
                Some(command) if command.is_global() => ui.invoke_nav_key(text, ctrl, shift),
                _ => false,
            }
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
            if let Some(ui) = weak.upgrade() {
                tabs::pick(&ui);
            }
        });
    }

    {
        let weak = main_window.as_weak();
        main_window.on_open_recent(move |index| {
            if let Some(ui) = weak.upgrade() {
                let path = with_state_ret(|state| {
                    usize::try_from(index)
                        .ok()
                        .and_then(|i| state.settings.recent.get(i).cloned())
                })
                .flatten();
                if let Some(path) = path {
                    tabs::open(&ui, path);
                }
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
        main_window.on_thumb_clicked(move |row, ctrl, shift| {
            if let Some(ui) = weak.upgrade()
                && let Ok(row) = u32::try_from(row)
            {
                with_state(|state| state.thumb_clicked(&ui, row, ctrl, shift));
            }
        });
    }
    {
        let weak = main_window.as_weak();
        main_window.on_page_op(move |which| {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.page_op(&ui, which));
            }
        });
    }
    {
        let weak = main_window.as_weak();
        main_window.on_thumb_drag_started(move |row| {
            if let Some(ui) = weak.upgrade()
                && let Ok(row) = u32::try_from(row)
            {
                with_state(|state| state.thumb_drag_started(&ui, row));
            }
        });
    }
    {
        let weak = main_window.as_weak();
        main_window.on_merge_open(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.merge_open(&ui));
            }
        });
        let weak = main_window.as_weak();
        main_window.on_merge_add(move || {
            if let Some(ui) = weak.upgrade() {
                tools_ui::merge_add(&ui);
            }
        });
        let weak = main_window.as_weak();
        main_window.on_merge_add_current(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.merge_add_current(&ui));
            }
        });
        let weak = main_window.as_weak();
        main_window.on_merge_move(move |by| {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.merge_move(&ui, by));
            }
        });
        let weak = main_window.as_weak();
        main_window.on_merge_remove(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.merge_remove(&ui));
            }
        });
        let weak = main_window.as_weak();
        main_window.on_merge_run(move || {
            if let Some(ui) = weak.upgrade() {
                tools_ui::merge_run(&ui);
            }
        });
        let weak = main_window.as_weak();
        main_window.on_merge_close(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.merge_close(&ui));
            }
        });
        let weak = main_window.as_weak();
        main_window.on_split_open(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_split_error(SharedString::new());
                ui.set_split_visible(true);
            }
        });
        let weak = main_window.as_weak();
        main_window.on_split_run(move || {
            if let Some(ui) = weak.upgrade() {
                tools_ui::split_run(&ui);
            }
        });
        let weak = main_window.as_weak();
        main_window.on_split_close(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_split_visible(false);
            }
        });
        let weak = main_window.as_weak();
        main_window.on_extract_pages(move || {
            if let Some(ui) = weak.upgrade() {
                tools_ui::extract_pages(&ui);
            }
        });
        let weak = main_window.as_weak();
        main_window.on_insert_from_file(move || {
            if let Some(ui) = weak.upgrade() {
                tools_ui::insert_from_file(&ui);
            }
        });
        main_window.on_props_open(|| with_state(|state| state.props_open()));
        let weak = main_window.as_weak();
        main_window.on_props_save(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.props_save(&ui));
            }
        });
        let weak = main_window.as_weak();
        main_window.on_props_close(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_props_visible(false);
            }
        });
        main_window.on_cancel_tool(|| with_state(|state| state.cancel_tool()));
        let weak = main_window.as_weak();
        // When the editor or the options close, the keys go back to the page (Ctrl+Z, arrows).
        main_window.on_field_commit(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.field_commit(&ui));
                ui.invoke_focus_viewport();
            }
        });
        let weak = main_window.as_weak();
        main_window.on_field_cancel(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.field_cancel(&ui));
                ui.invoke_focus_viewport();
            }
        });
        let weak = main_window.as_weak();
        main_window.on_field_choose(move |index| {
            if let Some(ui) = weak.upgrade()
                && with_state_ret(|state| state.field_choose(&ui, index)) != Some(false)
            {
                ui.invoke_focus_viewport();
            }
        });
        // Tab from the editor or the options: an editor or options opened for the next field take
        // the keys back when they show (`field-editor-serial`); a check box leaves them to the page.
        let weak = main_window.as_weak();
        main_window.on_field_tab(move |back| {
            if let Some(ui) = weak.upgrade() {
                ui.invoke_focus_viewport();
                with_state(|state| state.field_tab(&ui, back));
            }
        });
        let weak = main_window.as_weak();
        main_window.on_reset_field(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.reset_field(&ui));
                ui.invoke_focus_viewport();
            }
        });
        let weak = main_window.as_weak();
        main_window.on_reset_form(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.reset_form(&ui));
                ui.invoke_focus_viewport();
            }
        });
        let weak = main_window.as_weak();
        main_window.on_flatten_form(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.flatten_form(&ui));
                ui.invoke_focus_viewport();
            }
        });
        let weak = main_window.as_weak();
        main_window.on_import_form_data(move || {
            if let Some(ui) = weak.upgrade() {
                fields_ui::import_form_data(&ui);
                ui.invoke_focus_viewport();
            }
        });
        main_window.on_export_form_data(fields_ui::export_form_data);
        let weak = main_window.as_weak();
        main_window.on_field_edited(move |text| {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.field_edited(&ui, &text));
            }
        });
        let weak = main_window.as_weak();
        main_window.on_toggle_field_highlight(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.toggle_field_highlight(&ui));
            }
        });
        let weak = main_window.as_weak();
        main_window.on_convert_open(move |mode| {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.convert_open(&ui, mode));
            }
        });
        let weak = main_window.as_weak();
        main_window.on_convert_run(move || {
            if let Some(ui) = weak.upgrade() {
                convert_ui::convert_run(&ui);
            }
        });
        let weak = main_window.as_weak();
        main_window.on_convert_close(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_convert_visible(false);
            }
        });
        let weak = main_window.as_weak();
        main_window.on_export_word(move || {
            if let Some(ui) = weak.upgrade() {
                convert_ui::export_word(&ui);
            }
        });
        let weak = main_window.as_weak();
        main_window.on_images_to_pdf(move || {
            if let Some(ui) = weak.upgrade() {
                convert_ui::images_to_pdf(&ui);
            }
        });
        let weak = main_window.as_weak();
        main_window.on_stamp_open(move |mode| {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.stamp_open(&ui, mode));
            }
        });
        let weak = main_window.as_weak();
        main_window.on_stamp_apply(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.stamp_apply(&ui));
            }
        });
        let weak = main_window.as_weak();
        main_window.on_stamp_close(move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_stamp_visible(false);
            }
        });
        let weak = main_window.as_weak();
        main_window.on_crop_tool(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.crop_tool(&ui));
            }
        });
        let weak = main_window.as_weak();
        main_window.on_crop_apply(move |all| {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.crop_apply(&ui, all));
            }
        });
        let weak = main_window.as_weak();
        main_window.on_crop_cancel(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.crop_cancel(&ui));
            }
        });
    }
    // The modes and the gallery (Plan 0007 Phase 3).
    {
        let weak = main_window.as_weak();
        main_window.on_mode_changed(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.mode_changed(&ui));
            }
        });
        let weak = main_window.as_weak();
        main_window.on_gallery_scrolled(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.pump_thumbs(&ui));
            }
        });
        let weak = main_window.as_weak();
        main_window.on_gallery_toggled(move |row| {
            if let Some(ui) = weak.upgrade()
                && let Ok(row) = u32::try_from(row)
            {
                with_state(|state| state.thumb_clicked(&ui, row, true, false));
            }
        });
        let weak = main_window.as_weak();
        main_window.on_gallery_opened(move |row| {
            if let Some(ui) = weak.upgrade()
                && let Ok(row) = usize::try_from(row)
            {
                with_state(|state| state.go_to_page(&ui, row));
                ui.set_mode(0);
            }
        });
        let weak = main_window.as_weak();
        main_window.on_gallery_action(move |row, which| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let Ok(row) = u32::try_from(row) else {
                return;
            };
            with_state(|state| state.gallery_choose_for_action(&ui, row));
            gallery_run(&ui, which);
        });
        let weak = main_window.as_weak();
        main_window.on_gallery_chosen_action(move |which| {
            if let Some(ui) = weak.upgrade() {
                gallery_run(&ui, which);
            }
        });
        let weak = main_window.as_weak();
        main_window.on_gallery_clear(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| {
                    state.page_sel.clear();
                    state.page_sel_anchor = None;
                    state.show_page_sel(&ui);
                });
            }
        });
        let weak = main_window.as_weak();
        main_window.on_gallery_divider(move |at, which| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let Ok(at) = u32::try_from(at) else {
                return;
            };
            if which == 0 {
                tabs::split_at(&ui, at);
            } else if which == 2 {
                tools_ui::insert_from_file_at(&ui, Some(at));
            } else {
                with_state(|state| state.gallery_divider(at, which));
            }
        });
        main_window.on_gallery_dropped(move |at| {
            if let Ok(at) = u32::try_from(at) {
                with_state(|state| state.gallery_dropped(at));
            }
        });
    }
    main_window.on_thumb_dropped(move |row, after| {
        if let Ok(row) = u32::try_from(row) {
            with_state(|state| state.thumb_dropped(row, after));
        }
    });
    // The drag's payload: application data only, since the app itself knows which pages move. A
    // payload with text would be handed to the platform's drag and drop, whose drops the winit
    // backend never delivers back (Plan 0003 §3.2); with none, Slint runs the drag in the window.
    let mut page_drag = slint::DataTransfer::default();
    page_drag.set_user_data(Rc::new(()));
    main_window.set_page_drag_data(page_drag);

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

    {
        let weak = main_window.as_weak();
        main_window.on_select_pointer(move |kind, x, y, shift, touch| {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.select_pointer(&ui, kind, x, y, shift, touch));
            }
        });
    }

    {
        let weak = main_window.as_weak();
        main_window.on_hover(move |x, y| {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.hover(&ui, x, y));
            }
        });
    }

    {
        let weak = main_window.as_weak();
        main_window.on_copy_selection(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.copy_selection(&ui));
            }
        });
    }

    {
        let weak = main_window.as_weak();
        main_window.on_quit(move || {
            if let Some(ui) = weak.upgrade() {
                tabs::quit(&ui);
            }
        });
    }

    // Closing the window with unsaved changes asks first (Plan 0004, §4.4). The question runs
    // after this handler returns, from the event loop, and the window stays until it is answered.
    {
        let weak = main_window.as_weak();
        main_window.window().on_close_requested(move || {
            if !tabs::any_unsaved() {
                return slint::CloseRequestResponse::HideWindow;
            }
            let weak = weak.clone();
            slint::Timer::single_shot(std::time::Duration::ZERO, move || {
                if let Some(ui) = weak.upgrade() {
                    tabs::quit(&ui);
                }
            });
            slint::CloseRequestResponse::KeepWindowShown
        });
    }

    {
        let weak = main_window.as_weak();
        main_window.on_save(move || {
            if let Some(ui) = weak.upgrade() {
                save(&ui);
            }
        });
    }

    {
        let weak = main_window.as_weak();
        main_window.on_save_as(move || {
            if let Some(ui) = weak.upgrade() {
                save_as(&ui);
            }
        });
    }

    {
        let weak = main_window.as_weak();
        main_window.on_choose_tool(move |index| {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.choose_tool(&ui, Tool::from_index(index)));
            }
        });
    }

    {
        let weak = main_window.as_weak();
        main_window.on_choose_ink_width(move |index| {
            if let Some(ui) = weak.upgrade()
                && let Ok(index) = usize::try_from(index)
            {
                with_state(|state| state.choose_ink_width(&ui, index));
            }
        });
    }

    {
        let weak = main_window.as_weak();
        main_window.on_choose_colour(move |index| {
            if let Some(ui) = weak.upgrade()
                && let Ok(index) = usize::try_from(index)
            {
                with_state(|state| state.choose_colour(&ui, index));
            }
        });
    }

    main_window.on_undo(|| with_state(|state| state.undo()));
    {
        let weak = main_window.as_weak();
        main_window.on_note_save(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.note_save(&ui));
                ui.invoke_focus_viewport();
            }
        });
    }
    {
        let weak = main_window.as_weak();
        main_window.on_note_cancel(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.note_cancel(&ui));
                ui.invoke_focus_viewport();
            }
        });
    }
    main_window.on_redo(|| with_state(|state| state.redo()));

    {
        let weak = main_window.as_weak();
        main_window.on_delete_annotation(move || {
            if let Some(ui) = weak.upgrade() {
                with_state(|state| state.delete_annotation(&ui));
            }
        });
    }

    main_window.on_can_open_drop(|data| dropped_document(&data).is_some());

    {
        let weak = main_window.as_weak();
        main_window.on_open_drop(move |data| {
            if let Some(ui) = weak.upgrade()
                && let Some(path) = dropped_document(&data)
            {
                tabs::open(&ui, path);
            }
        });
    }

    with_state(|state| state.update_recent());

    tabs::wire(&main_window);

    // CLI argument handling: each file on the command line opens in a tab, through the same path
    // as the menu, so it lands in the recent list too.
    for (index, path) in paths.into_iter().enumerate() {
        if index == 0 {
            with_state(|state| state.open_path(&main_window, PathBuf::from(path)));
        } else {
            tabs::open(&main_window, PathBuf::from(path));
        }
    }

    let result = main_window.run();
    // Every tab's engine stops, and the files the tabs wrote are deleted.
    tabs::shutdown();
    // Dropped here, not left to thread-local destruction: on X11 this hands the clipboard to the
    // clipboard manager, so text copied just before quitting can still be pasted.
    let clipboard = CLIPBOARD.with(|cell| cell.borrow_mut().take());
    drop(clipboard);
    result
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

    /// Plan 0007 Phase 4: Ctrl+T opens into a new tab; Ctrl+Tab and Ctrl+Page Down step right,
    /// Ctrl+Shift+Tab (or Backtab) and Ctrl+Page Up left, from any field; Tab alone is not one.
    #[test]
    fn tab_shortcuts() {
        assert_eq!(key_command("t", true, false), Some(KeyCommand::NewTab));
        assert_eq!(
            key_command(&key(Key::Tab), true, false),
            Some(KeyCommand::NextTab)
        );
        assert_eq!(
            key_command(&key(Key::Tab), true, true),
            Some(KeyCommand::PreviousTab)
        );
        assert_eq!(
            key_command(&key(Key::Backtab), true, true),
            Some(KeyCommand::PreviousTab)
        );
        assert_eq!(
            key_command(&key(Key::PageDown), true, false),
            Some(KeyCommand::NextTab)
        );
        assert_eq!(
            key_command(&key(Key::PageUp), true, false),
            Some(KeyCommand::PreviousTab)
        );
        assert_eq!(key_command(&key(Key::Tab), false, false), None);
        for command in [
            KeyCommand::NewTab,
            KeyCommand::NextTab,
            KeyCommand::PreviousTab,
        ] {
            assert!(command.is_global(), "{command:?}");
        }
    }

    #[test]
    fn ctrl_w_closes_and_the_menu_shortcuts_work_from_any_field() {
        assert_eq!(key_command("w", true, false), Some(KeyCommand::Close));
        assert_eq!(key_command("w", false, false), None);
        for (text, shift) in [
            ("o", false),
            ("s", false),
            ("s", true),
            ("w", false),
            ("f", false),
        ] {
            let command = key_command(text, true, shift);
            assert!(
                command.is_some_and(KeyCommand::is_global),
                "Ctrl+{text} {shift}"
            );
        }
        assert!(key_command(&key(Key::F4), false, false).is_some_and(KeyCommand::is_global));
        // A text field keeps copy, undo, redo and its arrows.
        for (text, ctrl) in [("c", true), ("z", true), ("y", true)] {
            assert!(!key_command(text, ctrl, false).is_some_and(KeyCommand::is_global));
        }
        assert!(
            !key_command(&key(Key::LeftArrow), false, false).is_some_and(KeyCommand::is_global)
        );
        assert!(!key_command(&key(Key::Escape), false, false).is_some_and(KeyCommand::is_global));
        assert!(!key_command(&key(Key::Delete), false, false).is_some_and(KeyCommand::is_global));
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
    fn ctrl_c_copies() {
        assert_eq!(key_command("c", true, false), Some(KeyCommand::Copy));
        assert_eq!(key_command("c", false, false), None);
    }

    #[test]
    fn ctrl_o_opens() {
        assert_eq!(key_command("o", true, false), Some(KeyCommand::Open));
        assert_eq!(key_command("o", false, false), None);
    }

    #[test]
    fn save_markup_and_delete_keys() {
        assert_eq!(key_command("s", true, false), Some(KeyCommand::Save));
        assert_eq!(key_command("S", true, true), Some(KeyCommand::SaveAs));
        assert_eq!(key_command("s", false, false), None);
        assert_eq!(
            key_command("h", false, false),
            Some(KeyCommand::Mark(MarkupKind::Highlight))
        );
        assert_eq!(
            key_command("U", false, true),
            Some(KeyCommand::Mark(MarkupKind::Underline))
        );
        assert_eq!(
            key_command("k", false, false),
            Some(KeyCommand::Mark(MarkupKind::StrikeOut))
        );
        assert_eq!(key_command("n", false, false), Some(KeyCommand::NoteTool));
        assert_eq!(key_command("I", false, true), Some(KeyCommand::InkTool));
        assert_eq!(key_command("z", true, false), Some(KeyCommand::Undo));
        assert_eq!(key_command("Z", true, true), Some(KeyCommand::Redo));
        assert_eq!(key_command("y", true, false), Some(KeyCommand::Redo));
        assert_eq!(key_command("z", false, false), None);
        // With ctrl they are not markup: Ctrl+H, Ctrl+U and Ctrl+K stay free.
        assert_eq!(key_command("h", true, false), None);
        assert_eq!(
            key_command(&key(Key::Delete), false, false),
            Some(KeyCommand::DeleteAnnotation)
        );
    }

    #[test]
    fn tool_indices_round_trip() {
        for index in 0..7 {
            assert_eq!(Tool::from_index(index).index(), index);
        }
        assert_eq!(Tool::from_index(99), Tool::Select);
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
