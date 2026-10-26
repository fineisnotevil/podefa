// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Tabs: several documents in one window (Plan 0007 Phase 4, ADR 0011).
//!
//! Each tab has an engine actor of its own (its MuPDF document, its search and tool threads), so a
//! slow document never holds up another; every engine rasterizes on one shared pool
//! ([`RasterPool`]). The active tab's state lives in [`APP`] as before, so every handler still works
//! on "the document"; the other tabs' states are parked here. The window's tile cache, its models
//! and its settings go to whichever tab is shown ([`AppState::adopt_window`]), and a parked tab
//! holds no tiles, base layers, thumbnails or display lists ([`AppState::park`],
//! [`EngineCmd::Trim`]): one tile budget for the window, whatever the number of tabs. A parked
//! tab's engine events wait in its queue, pixels excepted, and are applied when it is shown again.

use crate::bench;
use crate::tools_ui::RunningTool;
use crate::{
    APP, AfterSave, AppState, MainWindow, TabItem, Tool, apply_event, pick_document, proceed,
    with_state, with_state_ret,
};
use engine_mupdf::{EngineHandle, RasterPool};
use pdf_core::annot::EditOp;
use pdf_core::pages::PageOp;
use pdf_core::scheduler::TileScheduler;
use pdf_core::settings::Settings;
use pdf_core::tools::{ToolJob, part_name};
use pdf_core::{EngineCmd, EngineEvent};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender};
use std::thread;

/// What a tab does with pages a tool job wrote to a file of the app's (Plan 0007 Phase 4).
#[derive(Debug, Clone)]
pub(crate) enum TabAction {
    /// Pages dragged onto the new-tab button: the file opens in a new tab under `title`.
    NewTab {
        file: PathBuf,
        title: String,
        dir: Option<PathBuf>,
    },
    /// Pages dragged onto another tab: they are added at the end of its document.
    CopyTo {
        file: PathBuf,
        tab: u64,
        count: usize,
    },
    /// Split from a gap in the gallery: `pages` leave this document (one undo step) and open in a
    /// new tab.
    Split {
        file: PathBuf,
        pages: Vec<u32>,
        title: String,
        dir: Option<PathBuf>,
    },
}

struct Tab {
    id: u64,
    /// Dropping it shuts the engine down; taken when the tab closes.
    engine: Option<EngineHandle>,
    /// The tab's state while another tab is shown.
    parked: Option<Box<AppState>>,
    /// Engine events that arrived while it was parked, in order.
    queued: Vec<EngineEvent>,
    /// A file the app wrote for this tab (pages dragged out, a split): deleted with the tab.
    temp: Option<PathBuf>,
}

struct Tabs {
    list: Vec<Tab>,
    active: usize,
    next_id: u64,
    pool: RasterPool,
    model: Rc<VecModel<TabItem>>,
    next_temp: u64,
    /// Files whose pages went into another tab's document: deleted when the app ends.
    leftovers: Vec<PathBuf>,
}

thread_local! {
    static TABS: RefCell<Option<Tabs>> = const { RefCell::new(None) };
}

fn with_tabs<T>(f: impl FnOnce(&mut Tabs) -> T) -> Option<T> {
    TABS.with(|cell| cell.borrow_mut().as_mut().map(f))
}

/// Starts the shared pool and the first tab's engine; returns the command channel of that engine.
pub(crate) fn init(ui: &MainWindow) -> Sender<EngineCmd> {
    let pool = RasterPool::new();
    let model = Rc::new(VecModel::from(vec![TabItem::default()]));
    ui.set_tabs(ModelRc::from(model.clone()));
    ui.set_active_tab(0);
    let (engine, events) = EngineHandle::spawn_with(pool.clone());
    let cmd_tx = engine.sender().clone();
    forward(ui, 1, events);
    TABS.with(|cell| {
        *cell.borrow_mut() = Some(Tabs {
            list: vec![Tab {
                id: 1,
                engine: Some(engine),
                parked: None,
                queued: Vec::new(),
                temp: None,
            }],
            active: 0,
            next_id: 2,
            pool,
            model,
            next_temp: 0,
            leftovers: Vec::new(),
        });
    });
    cmd_tx
}

/// The window's tab callbacks.
pub(crate) fn wire(ui: &MainWindow) {
    let weak = ui.as_weak();
    ui.on_select_tab(move |index| {
        if let Some(ui) = weak.upgrade()
            && let Ok(index) = usize::try_from(index)
        {
            switch(&ui, index);
        }
    });
    let weak = ui.as_weak();
    ui.on_close_tab(move |index| {
        if let Some(ui) = weak.upgrade()
            && let Ok(index) = usize::try_from(index)
        {
            close(&ui, index);
        }
    });
    let weak = ui.as_weak();
    ui.on_pages_to_tab(move |index| {
        if let Some(ui) = weak.upgrade()
            && let Ok(index) = usize::try_from(index)
        {
            pages_to_tab(&ui, index);
        }
    });
    let weak = ui.as_weak();
    ui.on_pages_to_new_tab(move || {
        if let Some(ui) = weak.upgrade() {
            pages_to_new_tab(&ui);
        }
    });
}

/// Every tab's engine stops (each joins its threads), and the files the tabs wrote are deleted.
pub(crate) fn shutdown() {
    let Some(tabs) = TABS.with(|cell| cell.borrow_mut().take()) else {
        return;
    };
    for mut tab in tabs.list {
        drop(tab.engine.take());
        if let Some(file) = tab.temp {
            let _ = std::fs::remove_file(file);
        }
    }
    for file in tabs.leftovers {
        let _ = std::fs::remove_file(file);
    }
}

/// Carries one engine's events to the UI thread, marked with its tab.
fn forward(ui: &MainWindow, id: u64, events: Receiver<EngineEvent>) {
    let weak = ui.as_weak();
    thread::spawn(move || {
        while let Ok(event) = events.recv() {
            let _ = weak.upgrade_in_event_loop(move |ui| deliver(&ui, id, event));
        }
    });
}

/// Pixels a parked tab does not keep: it asks for them again when it is shown.
fn is_pixels(event: &EngineEvent) -> bool {
    matches!(
        event,
        EngineEvent::TileRendered { .. }
            | EngineEvent::BaseRendered { .. }
            | EngineEvent::ThumbRendered { .. }
            | EngineEvent::TileAborted { .. }
    )
}

fn deliver(ui: &MainWindow, id: u64, event: EngineEvent) {
    let event = with_tabs(|tabs| {
        if tabs.list.get(tabs.active).map(|tab| tab.id) == Some(id) {
            return Some(event);
        }
        if !is_pixels(&event)
            && let Some(tab) = tabs.list.iter_mut().find(|tab| tab.id == id)
        {
            tab.queued.push(event);
        }
        None
    })
    .flatten();
    if let Some(event) = event {
        apply_event(ui, event);
        sync(ui);
    }
}

/// The active tab's name and unsaved mark follow the window's.
pub(crate) fn sync(ui: &MainWindow) {
    let row = TabItem {
        title: ui.get_doc_title(),
        dirty: ui.get_dirty(),
    };
    with_tabs(|tabs| {
        if tabs.model.row_data(tabs.active).as_ref() != Some(&row) {
            tabs.model.set_row_data(tabs.active, row);
        }
    });
}

/// Shows the tab at `index`: the shown one is parked, `index`'s state takes the window.
pub(crate) fn switch(ui: &MainWindow, index: usize) {
    let valid = with_tabs(|tabs| index < tabs.list.len() && index != tabs.active);
    if valid != Some(true) {
        return;
    }
    with_state(|state| state.park(ui));
    let Some(mut outgoing) = APP.with(|cell| cell.borrow_mut().take()) else {
        return;
    };
    let incoming = with_tabs(|tabs| tabs.list[index].parked.take()).flatten();
    let Some(mut incoming) = incoming else {
        APP.with(|cell| *cell.borrow_mut() = Some(outgoing));
        return;
    };
    incoming.adopt_window(&mut outgoing);
    let queued = with_tabs(|tabs| {
        let from = tabs.active;
        tabs.list[from].parked = Some(Box::new(outgoing));
        tabs.active = index;
        std::mem::take(&mut tabs.list[index].queued)
    })
    .unwrap_or_default();
    APP.with(|cell| *cell.borrow_mut() = Some(*incoming));
    ui.set_active_tab(index as i32);
    with_state(|state| state.unpark(ui));
    // What happened while it was parked (a split's `Restructured`, a finished save) comes before
    // the first view, so nothing is asked of pages that are gone.
    for event in queued {
        apply_event(ui, event);
    }
    with_state(|state| state.redraw(ui));
    sync(ui);
}

/// Ctrl+Tab and Ctrl+Shift+Tab: the next or the previous tab, round the end.
pub(crate) fn step(ui: &MainWindow, forward: bool) {
    let Some((active, count)) = with_tabs(|tabs| (tabs.active, tabs.list.len())) else {
        return;
    };
    if count > 1 {
        switch(ui, (active + if forward { 1 } else { count - 1 }) % count);
    }
}

/// A new tab after the shown one, with no document, shown at once. Returns its id.
fn add_tab(ui: &MainWindow, temp: Option<PathBuf>) -> Option<u64> {
    let (engine, events, id) = with_tabs(|tabs| {
        let (engine, events) = EngineHandle::spawn_with(tabs.pool.clone());
        let id = tabs.next_id;
        tabs.next_id += 1;
        (engine, events, id)
    })?;
    forward(ui, id, events);
    let state = with_state_ret(|state| state.sibling(engine.sender().clone()))?;
    let index = with_tabs(|tabs| {
        let index = tabs.active + 1;
        tabs.list.insert(
            index,
            Tab {
                id,
                engine: Some(engine),
                parked: Some(Box::new(state)),
                queued: Vec::new(),
                temp,
            },
        );
        tabs.model.insert(index, TabItem::default());
        // Right after the shown tab, which keeps its index.
        index
    })?;
    switch(ui, index);
    Some(id)
}

/// Whether the shown tab has no document, opened or opening: a file opens there.
fn active_is_empty() -> bool {
    with_state_ret(|state| state.page_sizes.is_empty() && state.opening.is_none()).unwrap_or(true)
}

/// Opens `path` in a tab: the tab that has it already, the shown tab when it has no document, or
/// a new one.
pub(crate) fn open(ui: &MainWindow, path: PathBuf) {
    let path = std::path::absolute(&path).unwrap_or(path);
    if let Some(index) = tab_with(&path) {
        switch(ui, index);
        return;
    }
    if !active_is_empty() && add_tab(ui, None).is_none() {
        return;
    }
    with_state(|state| state.open_path(ui, path));
    sync(ui);
}

/// Opens `path` in a new tab even when a tab has it already: the bench harness's row AU opens one
/// document many times.
pub(crate) fn open_in_new_tab(ui: &MainWindow, path: PathBuf) {
    if add_tab(ui, None).is_some() {
        with_state(|state| state.open_path(ui, path));
        sync(ui);
    }
}

/// The number of open tabs.
pub(crate) fn count() -> usize {
    with_tabs(|tabs| tabs.list.len()).unwrap_or(0)
}

/// The file dialog, then [`open`].
pub(crate) fn pick(ui: &MainWindow) {
    let weak = ui.as_weak();
    pick_document(move |path| {
        if let Some(ui) = weak.upgrade() {
            open(&ui, path);
        }
    });
}

/// The tab whose document is `path`, if one is.
fn tab_with(path: &Path) -> Option<usize> {
    let shown = with_state_ret(|state| state.doc_path.clone()).flatten();
    with_tabs(|tabs| {
        tabs.list.iter().enumerate().find_map(|(index, tab)| {
            let doc = if index == tabs.active {
                shown.clone()
            } else {
                tab.parked.as_ref().and_then(|state| state.doc_path.clone())
            };
            (doc.as_deref() == Some(path)).then_some(index)
        })
    })
    .flatten()
}

/// Ctrl+W: closes the shown tab; the last one closes the window.
pub(crate) fn close_active(ui: &MainWindow) {
    if let Some(index) = with_tabs(|tabs| tabs.active) {
        close(ui, index);
    }
}

/// Closes the tab at `index`, asking first about its unsaved changes: the tab is shown for that,
/// so the question is about what is on screen.
pub(crate) fn close(ui: &MainWindow, index: usize) {
    let Some((count, id)) = with_tabs(|tabs| (tabs.list.len(), tabs.list.get(index).map(|t| t.id)))
    else {
        return;
    };
    let Some(id) = id else {
        return;
    };
    if count == 1 {
        proceed(ui, AfterSave::Quit);
        return;
    }
    switch(ui, index);
    proceed(ui, AfterSave::CloseTab(id));
}

/// Closes the tab `id` without asking: its neighbour is shown, its engine stops off the UI thread
/// and its file, if the app wrote one, is deleted after.
pub(crate) fn close_now(ui: &MainWindow, id: u64) {
    let Some(Some(index)) = with_tabs(|tabs| tabs.list.iter().position(|tab| tab.id == id)) else {
        return;
    };
    let count = with_tabs(|tabs| tabs.list.len()).unwrap_or(0);
    if count < 2 {
        return;
    }
    let active = with_tabs(|tabs| tabs.active).unwrap_or(0);
    if active == index {
        switch(
            ui,
            if index + 1 < count {
                index + 1
            } else {
                index - 1
            },
        );
    }
    let tab = with_tabs(|tabs| {
        let tab = tabs.list.remove(index);
        tabs.model.remove(index);
        if tabs.active > index {
            tabs.active -= 1;
        }
        tab
    });
    if let Some(active) = with_tabs(|tabs| tabs.active) {
        ui.set_active_tab(active as i32);
    }
    let Some(mut tab) = tab else {
        return;
    };
    // The state holds the window's models and is the UI thread's; only the engine moves.
    drop(tab.parked.take());
    let engine = tab.engine.take();
    let temp = tab.temp.take();
    thread::spawn(move || {
        drop(engine);
        if let Some(file) = temp {
            let _ = std::fs::remove_file(file);
        }
    });
}

/// Whether closing the window now would lose edits in any tab.
pub(crate) fn any_unsaved() -> bool {
    let shown = with_state_ret(|state| state.has_unsaved_changes()).unwrap_or(false);
    shown
        || with_tabs(|tabs| {
            tabs.list.iter().any(|tab| {
                tab.parked
                    .as_ref()
                    .is_some_and(|state| state.has_unsaved_changes())
            })
        })
        .unwrap_or(false)
}

/// Closing the window: each tab with unsaved changes is shown and asks in turn ([`proceed`] comes
/// back here once it is answered); with none left, the window closes.
pub(crate) fn quit(ui: &MainWindow) {
    if with_state_ret(|state| state.has_unsaved_changes()).unwrap_or(false) {
        proceed(ui, AfterSave::Quit);
        return;
    }
    let next = with_tabs(|tabs| {
        tabs.list.iter().position(|tab| {
            tab.parked
                .as_ref()
                .is_some_and(|state| state.has_unsaved_changes())
        })
    })
    .flatten();
    match next {
        Some(index) => {
            switch(ui, index);
            proceed(ui, AfterSave::Quit);
        }
        None => {
            let _ = slint::quit_event_loop();
        }
    }
}

/// A file of the app's for pages on their way to another tab, in the system's temporary folder.
fn temp_file() -> Option<PathBuf> {
    let dir = crate::platform::temp_dir().join("PODEFA");
    std::fs::create_dir_all(&dir).ok()?;
    let n = with_tabs(|tabs| {
        tabs.next_temp += 1;
        tabs.next_temp
    })?;
    Some(dir.join(format!("tab-{}-{n}.pdf", std::process::id())))
}

/// The pages a drag carries: the dragged ones, or the chosen ones (or the current page).
fn dragged_pages(ui: &MainWindow) -> Option<(Vec<u32>, String, Option<PathBuf>)> {
    with_state_ret(|state| {
        let mut pages = std::mem::take(&mut state.drag_pages);
        if pages.is_empty() {
            pages = state.page_targets(ui);
        }
        let title = part_name(&state.doc_stem(), &pages)
            .trim_end_matches(".pdf")
            .to_owned();
        (pages, title, state.doc_dir())
    })
    .filter(|(pages, ..)| !pages.is_empty())
}

/// Writes `pages` of the shown document to `file` on its tool thread; `action` runs when it is
/// written.
fn extract(ui: &MainWindow, pages: Vec<u32>, file: PathBuf, action: TabAction) {
    let started = with_state_ret(|state| {
        let started = state.start_tool(
            ui,
            ToolJob::Extract {
                parts: vec![(pages, file)],
            },
            false,
        );
        if started && let Some(tool) = state.running_tool.as_mut() {
            tool.tab = Some(action);
        }
        started
    });
    if started == Some(false) {
        ui.set_status_text(SharedString::from(
            "Another tool is running; try again when it ends",
        ));
    }
}

/// Pages dropped on the new-tab button: they open as a new document in a new tab.
fn pages_to_new_tab(ui: &MainWindow) {
    let Some((pages, title, dir)) = dragged_pages(ui) else {
        return;
    };
    let Some(file) = temp_file() else {
        return;
    };
    let action = TabAction::NewTab {
        file: file.clone(),
        title,
        dir,
    };
    extract(ui, pages, file, action);
}

/// Pages dropped on a tab: copied to the end of its document. The shown tab gets nothing.
fn pages_to_tab(ui: &MainWindow, index: usize) {
    let target = with_tabs(|tabs| (index != tabs.active).then(|| tabs.list[index].id)).flatten();
    let Some(tab) = target else {
        with_state(|state| state.drag_pages.clear());
        return;
    };
    let Some((pages, ..)) = dragged_pages(ui) else {
        return;
    };
    let Some(file) = temp_file() else {
        return;
    };
    let action = TabAction::CopyTo {
        file: file.clone(),
        tab,
        count: pages.len(),
    };
    extract(ui, pages, file, action);
}

/// Split from the gallery's gap before page `at`: the pages from there on move to a new tab.
pub(crate) fn split_at(ui: &MainWindow, at: u32) {
    let Some((pages, title, dir)) = with_state_ret(|state| {
        let count = state.page_sizes.len() as u32;
        (state.editable && at > 0 && at < count).then(|| {
            let pages: Vec<u32> = (at..count).collect();
            let title = part_name(&state.doc_stem(), &pages)
                .trim_end_matches(".pdf")
                .to_owned();
            (pages, title, state.doc_dir())
        })
    })
    .flatten() else {
        return;
    };
    let Some(file) = temp_file() else {
        return;
    };
    let action = TabAction::Split {
        file: file.clone(),
        pages: pages.clone(),
        title,
        dir,
    };
    extract(ui, pages, file, action);
}

/// A tab's tool job wrote its pages: the action it waited for.
pub(crate) fn pages_written(ui: &MainWindow, action: TabAction) {
    match action {
        TabAction::NewTab { file, title, dir } => open_written(ui, file, title, dir),
        TabAction::Split {
            file,
            pages,
            title,
            dir,
        } => {
            // Out of this document as one undo step, then in a tab of their own.
            with_state(|state| state.send_edit(EditOp::Pages(PageOp::Delete { pages })));
            open_written(ui, file, title, dir);
        }
        TabAction::CopyTo { file, tab, count } => {
            let outcome = with_tabs(|tabs| {
                tabs.leftovers.push(file.clone());
                let index = tabs.list.iter().position(|t| t.id == tab)?;
                let state = tabs.list[index].parked.as_mut()?;
                if !state.editable || state.page_sizes.is_empty() {
                    return Some(Err(state.doc_name()));
                }
                let at = state.page_sizes.len() as u32;
                state.send_edit(EditOp::Pages(PageOp::InsertFrom {
                    path: file,
                    at,
                    source_pages: 0,
                }));
                let mut row = tabs.model.row_data(index).unwrap_or_default();
                row.dirty = true;
                tabs.model.set_row_data(index, row);
                Some(Ok(state.doc_name()))
            })
            .flatten();
            let message = match outcome {
                Some(Ok(name)) => format!(
                    "Copied {count} page{} to the end of \"{name}\"",
                    if count == 1 { "" } else { "s" }
                ),
                Some(Err(name)) => format!("\"{name}\" cannot take pages"),
                None => "The tab was closed".to_owned(),
            };
            ui.set_status_text(SharedString::from(message));
        }
    }
}

/// A tool job for a tab failed or was cancelled: its file is not needed.
pub(crate) fn discard(action: TabAction) {
    let file = match action {
        TabAction::NewTab { file, .. }
        | TabAction::CopyTo { file, .. }
        | TabAction::Split { file, .. } => file,
    };
    let _ = std::fs::remove_file(file);
}

/// Opens pages the app wrote in a new tab, as a document with no file of its own yet.
fn open_written(ui: &MainWindow, file: PathBuf, title: String, dir: Option<PathBuf>) {
    if add_tab(ui, Some(file.clone())).is_none() {
        return;
    }
    with_state(|state| {
        state.untitled = Some(title);
        state.origin_dir = dir;
        state.open_path(ui, file);
    });
    sync(ui);
}

impl AppState {
    /// A state for a new tab: no document, the window's models and epoch counters, and `cmd_tx`
    /// to its own engine. What belongs to the window comes with [`AppState::adopt_window`].
    pub(crate) fn sibling(&self, cmd_tx: Sender<EngineCmd>) -> AppState {
        AppState::new(
            cmd_tx,
            self.parts(),
            TileScheduler::new(1, 1),
            Settings::default(),
            self.settings_path.clone(),
            self.scale_factor,
            bench::Trace::new(false),
        )
    }

    fn parts(&self) -> crate::WindowParts {
        crate::WindowParts {
            next_epoch: self.next_epoch.clone(),
            current_epoch: self.current_epoch.clone(),
            model: self.model.clone(),
            pages_model: self.pages_model.clone(),
            recent_model: self.recent_model.clone(),
            thumbs_model: self.thumbs_model.clone(),
            outline_model: self.outline_model.clone(),
            hits_model: self.hits_model.clone(),
            sel_model: self.sel_model.clone(),
            annot_sel_model: self.annot_sel_model.clone(),
            field_model: self.field_model.clone(),
            merge_model: self.merge_model.clone(),
        }
    }

    /// Takes what belongs to the window from the state that was shown: the tile cache (one budget
    /// for every tab), the settings, the trace, the mode and the tool.
    fn adopt_window(&mut self, from: &mut AppState) {
        std::mem::swap(&mut self.scheduler, &mut from.scheduler);
        std::mem::swap(&mut self.settings, &mut from.settings);
        std::mem::swap(&mut self.bench, &mut from.bench);
        self.rendered = from.rendered;
        self.scale_factor = from.scale_factor;
        self.mode = from.mode;
        self.motion_until = from.motion_until;
        self.margins = from.margins;
        self.tool = from.tool;
        self.colour = from.colour;
        self.ink_width = from.ink_width;
    }

    /// The tab goes out of view: everything drawn for it is let go - tiles, base layers,
    /// thumbnails, the engine's display lists - and what is open over the pages closes. Its
    /// document, its place, its zoom and its history stay.
    fn park(&mut self, ui: &MainWindow) {
        if !self.layout.is_empty() {
            self.read_pos(ui);
        }
        self.clear_selection(ui);
        self.select_annotation(ui, None);
        self.note_cancel(ui);
        self.note_drag = None;
        self.crop_cancel(ui);
        self.field_cancel(ui);
        self.ink = None;
        self.ink_shown = None;
        ui.set_ink_commands(SharedString::new());
        self.cancel_search();
        self.search_query.clear();
        ui.set_find_visible(false);
        ui.set_find_text(SharedString::new());
        ui.set_find_counter(SharedString::new());
        for close in [
            MainWindow::set_split_visible,
            MainWindow::set_merge_visible,
            MainWindow::set_props_visible,
            MainWindow::set_stamp_visible,
            MainWindow::set_convert_visible,
            MainWindow::set_note_editor_visible,
            MainWindow::set_note_hover_visible,
            MainWindow::set_field_editor_visible,
            MainWindow::set_field_choices_visible,
        ] {
            close(ui, false);
        }
        self.page_sel.clear();
        self.page_sel_anchor = None;
        self.drag_pages.clear();
        self.begin_epoch();
        self.release_all(ui);
        self.pages_model.set_vec(Vec::new());
        self.thumbs_model.set_vec(Vec::new());
        self.thumb_cache.clear();
        self.thumb_inflight.clear();
        self.current_thumb = None;
        self.text_lines.clear();
        self.lines_pending.clear();
        self.hit_rows.clear();
        self.sel_rows.clear();
        self.annot_sel_rows.clear();
        self.field_rows.clear();
        self.field_model.set_vec(Vec::new());
        let _ = self.cmd_tx.send(EngineCmd::Trim);
    }

    /// The tab is shown again: its buttons, its sidebar and its pages come back where they were.
    fn unpark(&mut self, ui: &MainWindow) {
        ui.set_doc_title(SharedString::from(self.doc_name()));
        ui.set_total_pages(self.page_sizes.len() as i32);
        ui.set_editable(self.editable);
        ui.set_dirty(self.has_unsaved_changes());
        ui.set_can_undo(self.editable && self.can_undo);
        ui.set_can_redo(self.editable && self.can_redo);
        ui.set_repaired(self.repaired);
        ui.set_form_fields(self.form.fields);
        ui.set_tool_running(self.running_tool.is_some());
        ui.set_status_text(SharedString::from(
            self.running_tool
                .as_ref()
                .map_or_else(String::new, |tool: &RunningTool| {
                    format!("{}...", tool.title)
                }),
        ));
        ui.set_has_error(false);
        ui.set_error_text(SharedString::new());
        ui.set_is_loading(self.opening.is_some());
        ui.set_pages_chosen(0);
        ui.set_zoom_percent(self.scale_milli as f32 / 10.0);
        ui.set_gallery_scroll(0.0);
        if !self.editable {
            self.tool = Tool::Select;
        }
        ui.set_tool(self.tool.index());
        self.reset_thumbs(self.page_sizes.len());
        self.rebuild_outline();
        if self.page_sizes.is_empty() {
            self.layout = Default::default();
            ui.set_current_page(0);
            ui.set_page_input(SharedString::new());
            ui.set_sb_visible(false);
            self.clamp_pos(ui);
            self.reanchor(ui);
            return;
        }
        self.layout = self.layout_for(ui, self.scale_milli);
    }

    /// The shown tab's first view after [`AppState::unpark`] and its queued events: tiles and
    /// thumbnails are asked for again.
    fn redraw(&mut self, ui: &MainWindow) {
        if self.page_sizes.is_empty() {
            return;
        }
        self.layout = self.layout_for(ui, self.scale_milli);
        self.begin_epoch();
        self.clamp_pos(ui);
        self.reanchor(ui);
        self.refresh(ui);
        self.pump_thumbs(ui);
    }
}
