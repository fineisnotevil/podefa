// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Filling form fields (Plan 0006 Phases 1 and 2).
//!
//! The engine reports each near page's field widgets, as it does annotations. With the Select
//! tool, a click on a check box toggles it and a click on a radio button turns it on. A click on a
//! text field opens an editor laid exactly over the field, in its text size (§0, question 1). A
//! click on a choice field opens its options under it; a multi-select list's options are ticked
//! and unticked until the list closes. Each committed value is one edit, so one undo step.
//!
//! The keyboard (Phase 2): Tab and Shift+Tab visit the fields in the page's tab order, then go on
//! to the next page with a field, round the document. A check box or radio button with the focus
//! is outlined and Space or Enter sets it; Esc leaves the field. Form > Reset Field and Reset Form
//! put values back to their defaults.

use crate::platform::{FileDialog, MessageDialog};
use crate::{AppState, FieldView, MainWindow, Tool, with_state, with_state_ret};
use pdf_core::EngineCmd;
use pdf_core::annot::EditOp;
use pdf_core::form::{FieldInfo, FieldKind, clip_to_max_len, editor_font_size, hit, tab_sequence};
use pdf_core::form_data::{self, DataFormat};
use pdf_core::pages::PageMap;
use pdf_core::select::PagePoint;
use slint::{ComponentHandle as _, Model as _, SharedString, VecModel};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// The field the editor or the options list is open for.
#[derive(Debug, Clone)]
pub(crate) struct FieldEdit {
    pub(crate) page: u32,
    pub(crate) field: FieldInfo,
    /// A multi-select list's ticked options, committed when the list closes.
    pub(crate) marks: Vec<bool>,
}

impl AppState {
    /// Asks for the fields of the near pages that do not have them yet, and forgets far pages'.
    pub(crate) fn request_fields(&mut self, near: &[u32]) {
        self.fields.retain(|page, _| near.contains(page));
        self.fields_pending.retain(|page| near.contains(page));
        if !(self.editable && self.form.fields) {
            return;
        }
        for &page in near {
            if !self.fields.contains_key(&page) && self.fields_pending.insert(page) {
                let _ = self.cmd_tx.send(EngineCmd::Fields {
                    page,
                    generation: self.layout_generation,
                });
            }
        }
    }

    /// A page's fields arrived; one about a previous layout, or a page no longer asked for, is
    /// dropped. A Tab waiting for this page goes on.
    pub(crate) fn apply_fields(
        &mut self,
        ui: &MainWindow,
        page: u32,
        generation: u64,
        items: Vec<FieldInfo>,
    ) {
        if generation == self.layout_generation && self.fields_pending.remove(&page) {
            self.fields.insert(page, items);
            self.update_field_rects(ui);
            if let Some((waiting, back)) = self.field_tab_pending
                && waiting == page
            {
                self.field_tab_pending = None;
                self.focus_page_end(ui, page, back);
            }
            self.place_field_editor(ui);
        }
    }

    /// A page's content changed (a fill, an undo): its fields are asked for again. The ones known
    /// stay until the answer, so the page's shading does not blink.
    pub(crate) fn fields_changed(&mut self, page: u32) {
        if self.fields.contains_key(&page) || self.fields_pending.remove(&page) {
            self.fields_pending.insert(page);
            let _ = self.cmd_tx.send(EngineCmd::Fields {
                page,
                generation: self.layout_generation,
            });
        }
    }

    /// The pages moved (`map`: each new page's old index): the focus stays on a field whose page
    /// kept its place - a fill on several pages changes them in place - and is dropped otherwise.
    pub(crate) fn fields_restructured(&mut self, ui: &MainWindow, map: &PageMap) {
        self.field_tab_pending = None;
        if let Some((page, _)) = self.field_focus {
            let slot = map.get(page as usize);
            if !(slot == Some(&None) || slot == Some(&Some(page))) {
                self.set_field_focus(ui, None);
            }
        }
    }

    /// The shading of every fillable field on the near pages, placed like the search hits.
    pub(crate) fn update_field_rects(&mut self, ui: &MainWindow) {
        let near = self.near_pages(ui);
        let s = self.scale_milli as f64 / 1000.0;
        let sf = self.sf() as f32;
        let mut rows = Vec::new();
        let mut pages: Vec<_> = self
            .fields
            .iter()
            .filter(|(page, _)| near.contains(&(**page as usize)))
            .collect();
        pages.sort_unstable_by_key(|(page, _)| **page);
        for (&page, fields) in pages {
            let Some(slot) = self.layout.slot(page as usize) else {
                continue;
            };
            for field in fields.iter().filter(|f| f.fillable()) {
                let r = field.rect;
                rows.push((
                    self.canvas_x(slot.x as f64 + r.x as f64 * s),
                    self.canvas_y(slot.y as f64 + r.y as f64 * s),
                    (r.width as f64 * s) as f32 / sf,
                    (r.height as f64 * s) as f32 / sf,
                    field.required,
                ));
            }
        }
        if rows == self.field_rows {
            return;
        }
        let views: Vec<FieldView> = rows
            .iter()
            .map(|&(x, y, w, h, required)| FieldView {
                x,
                y,
                w,
                h,
                required,
            })
            .collect();
        self.field_model.set_vec(views);
        self.field_rows = rows;
    }

    /// View > Highlight Form Fields: shows or hides the shading, and remembers the choice.
    pub(crate) fn toggle_field_highlight(&mut self, ui: &MainWindow) {
        self.settings.plain_fields = !self.settings.plain_fields;
        ui.set_field_highlight(!self.settings.plain_fields);
        self.save_settings();
    }

    /// The index of the fillable field widget under a page point.
    fn field_index_at(&self, point: PagePoint) -> Option<usize> {
        let fields = self.fields.get(&point.page)?;
        let i = hit(fields, point.x, point.y)?;
        fields[i].fillable().then_some(i)
    }

    /// The fillable field widget under a page point.
    pub(crate) fn field_at(&self, point: PagePoint) -> Option<&FieldInfo> {
        let i = self.field_index_at(point)?;
        self.fields.get(&point.page).map(|fields| &fields[i])
    }

    /// The pointer moved over the page: the cursor says what a click on a field would do, and the
    /// field's description shows. Returns whether the pointer is over a field.
    pub(crate) fn field_hover(
        &mut self,
        ui: &MainWindow,
        point: Option<PagePoint>,
        x: f32,
        y: f32,
    ) -> bool {
        let field = point
            .filter(|_| self.tool == Tool::Select)
            .and_then(|p| self.field_at(p));
        let cursor = match field.map(|f| f.kind) {
            Some(FieldKind::Text) => 1,
            Some(_) => 2,
            None => 0,
        };
        if ui.get_field_cursor() != cursor {
            ui.set_field_cursor(cursor);
        }
        let tooltip = field.and_then(|f| f.tooltip.clone());
        if let Some(text) = tooltip {
            ui.set_note_hover_text(SharedString::from(text));
            ui.set_note_hover_x(x + 14.0);
            ui.set_note_hover_y(y + 14.0);
            ui.set_note_hover_visible(true);
        }
        field.is_some()
    }

    /// A press on the page with the Select tool. An open editor is committed first. Returns
    /// whether the press was on a field and is handled; a press anywhere else leaves the field.
    pub(crate) fn field_pressed(&mut self, ui: &MainWindow, point: Option<PagePoint>) -> bool {
        if self.field_edit.is_some() {
            self.field_commit(ui);
        }
        if self.tool != Tool::Select || !self.editable {
            return false;
        }
        let Some((page, index)) = point.and_then(|p| Some((p.page, self.field_index_at(p)?)))
        else {
            self.set_field_focus(ui, None);
            return false;
        };
        self.clear_selection(ui);
        self.select_annotation(ui, None);
        self.set_field_focus(ui, Some((page, index)));
        let Some(field) = self.focused_field().cloned() else {
            return false;
        };
        match field.kind {
            FieldKind::CheckBox | FieldKind::Radio => self.toggle_field(page, &field),
            _ => self.open_field(ui, page, field),
        }
        true
    }

    /// The field with the focus, while its page's fields are known.
    fn focused_field(&self) -> Option<&FieldInfo> {
        let (page, index) = self.field_focus?;
        self.fields.get(&page)?.get(index)
    }

    /// Moves the focus to a field (or nowhere) and outlines it.
    fn set_field_focus(&mut self, ui: &MainWindow, focus: Option<(u32, usize)>) {
        self.field_focus = focus;
        ui.set_has_field_focus(focus.is_some());
        self.place_field_editor(ui);
    }

    /// A click, Space or Enter on a check box or radio button: a check box toggles, a radio
    /// button turns on.
    fn toggle_field(&mut self, page: u32, field: &FieldInfo) {
        if let Some(value) = field.clicked_value()
            && value != field.value
        {
            self.fill_field(page, &field.name, value);
        }
    }

    /// Opens what fills a text or choice field: the editor, or the options.
    fn open_field(&mut self, ui: &MainWindow, page: u32, field: FieldInfo) {
        match field.kind {
            FieldKind::Text => self.open_field_editor(ui, page, field),
            FieldKind::ComboBox | FieldKind::ListBox => self.open_field_choices(ui, page, field),
            _ => {}
        }
    }

    fn fill_field(&mut self, page: u32, name: &str, value: String) {
        self.send_edit(EditOp::SetField {
            page,
            name: name.to_owned(),
            value,
        });
    }

    /// Tab (or Shift+Tab, `back`): what is being typed is kept, and the focus moves to the next
    /// field in the page's tab order; past the page's last one, to the next page with a field,
    /// which the engine finds. With no field focused, Tab starts on the page in view.
    pub(crate) fn field_tab(&mut self, ui: &MainWindow, back: bool) {
        if !(self.editable && self.form.fields) || self.layout.is_empty() {
            return;
        }
        if self.field_edit.is_some() {
            self.field_commit(ui);
        }
        let count = self.page_sizes.len() as u32;
        let Some((page, index)) = self.field_focus else {
            // Start on the page in view: ask for the next page from the one before it.
            let current = self.current_page(ui) as u32;
            let from = if back {
                (current + 1) % count
            } else {
                (current + count - 1) % count
            };
            self.ask_field_page(from, back);
            return;
        };
        let next = self.fields.get(&page).and_then(|fields| {
            let seq = tab_sequence(fields);
            let at = seq.iter().position(|&i| i == index)?;
            if back {
                at.checked_sub(1).map(|p| seq[p])
            } else {
                seq.get(at + 1).copied()
            }
        });
        match next {
            Some(next) => self.focus_field(ui, page, next),
            None => self.ask_field_page(page, back),
        }
    }

    fn ask_field_page(&mut self, from: u32, back: bool) {
        let _ = self.cmd_tx.send(EngineCmd::NextFieldPage {
            from,
            back,
            generation: self.layout_generation,
        });
    }

    /// The engine found the page Tab goes on to: its first field (its last, going back) gets the
    /// focus, once the page's fields are known.
    pub(crate) fn field_page(
        &mut self,
        ui: &MainWindow,
        generation: u64,
        page: Option<u32>,
        back: bool,
    ) {
        if generation != self.layout_generation {
            return;
        }
        let Some(page) = page else {
            return;
        };
        if self.fields.contains_key(&page) {
            self.focus_page_end(ui, page, back);
            return;
        }
        // Not near the view: go there, and focus the field when the page's fields arrive.
        self.field_tab_pending = Some((page, back));
        self.go_to_page(ui, page as usize);
    }

    fn focus_page_end(&mut self, ui: &MainWindow, page: u32, back: bool) {
        let Some(fields) = self.fields.get(&page) else {
            return;
        };
        let seq = tab_sequence(fields);
        let end = if back { seq.last() } else { seq.first() };
        if let Some(&index) = end {
            self.focus_field(ui, page, index);
        }
    }

    /// Gives a field the focus from the keyboard: it is scrolled into view, a text or choice field
    /// opens, a check box or radio button is outlined.
    fn focus_field(&mut self, ui: &MainWindow, page: u32, index: usize) {
        let Some(field) = self.fields.get(&page).and_then(|f| f.get(index)).cloned() else {
            return;
        };
        self.reveal_rect(ui, page, field.rect);
        self.set_field_focus(ui, Some((page, index)));
        match field.kind {
            FieldKind::CheckBox | FieldKind::Radio => {}
            _ => self.open_field(ui, page, field),
        }
    }

    /// A key on the page while the form is being filled: Tab and Shift+Tab move between fields,
    /// Space and Enter set a focused check box or radio button, Esc leaves the field. Returns
    /// whether the key was taken.
    pub(crate) fn field_key(&mut self, ui: &MainWindow, text: &str, shift: bool) -> bool {
        let key = |k: slint::platform::Key| text == SharedString::from(k).as_str();
        use slint::platform::Key;
        if key(Key::Tab) || key(Key::Backtab) {
            if !(self.editable && self.form.fields) {
                return false;
            }
            self.field_tab(ui, shift || key(Key::Backtab));
            return true;
        }
        let Some((page, _)) = self.field_focus else {
            return false;
        };
        if key(Key::Escape) {
            self.set_field_focus(ui, None);
            return true;
        }
        if text == " " || key(Key::Return) {
            let Some(field) = self.focused_field().cloned() else {
                return false;
            };
            match field.kind {
                FieldKind::CheckBox | FieldKind::Radio => self.toggle_field(page, &field),
                _ => self.open_field(ui, page, field),
            }
            return true;
        }
        false
    }

    /// Form > Reset Field: the focused field goes back to its default value.
    pub(crate) fn reset_field(&mut self, ui: &MainWindow) {
        if self.field_edit.is_some() {
            self.field_cancel(ui);
        }
        let Some((page, name)) = self
            .field_focus
            .and_then(|(page, _)| Some((page, self.focused_field()?.name.clone())))
        else {
            return;
        };
        self.send_edit(EditOp::ResetField { page, name });
    }

    /// Form > Reset Form: every field goes back to its default value, as one undo step.
    pub(crate) fn reset_form(&mut self, ui: &MainWindow) {
        if self.field_edit.is_some() {
            self.field_cancel(ui);
        }
        if self.editable && self.form.fields {
            self.send_edit(EditOp::ResetForm);
        }
    }

    /// Form > Flatten Form (Plan 0006 Phase 3): every field is drawn into its page and the form
    /// goes, as one undo step.
    pub(crate) fn flatten_form(&mut self, ui: &MainWindow) {
        if self.field_edit.is_some() {
            self.field_cancel(ui);
        }
        if self.editable && self.form.fields {
            self.set_field_focus(ui, None);
            self.send_edit(EditOp::FlattenForm);
        }
    }

    /// Writes every field's value to `path`, as XFDF unless its name ends in `.json`; the engine
    /// answers with `FormExported`.
    pub(crate) fn export_form_to(&mut self, path: PathBuf) {
        let format = DataFormat::from_path(&path).unwrap_or(DataFormat::Xfdf);
        let _ = self.cmd_tx.send(EngineCmd::ExportForm {
            path,
            format,
            generation: self.doc_generation,
        });
    }

    /// The engine wrote the form's values, or could not.
    pub(crate) fn form_exported(
        &mut self,
        ui: &MainWindow,
        generation: u64,
        path: &Path,
        result: Result<usize, String>,
    ) {
        if generation != self.doc_generation {
            return;
        }
        let name = file_name(path);
        match result {
            Ok(count) => {
                crate::platform::written(path);
                ui.set_status_text(SharedString::from(format!(
                    "Exported {count} field values to \"{name}\""
                )));
            }
            Err(message) => error_box(&format!("Could not export to \"{name}\": {message}")),
        }
    }

    /// Reads values from a data file (XFDF unless its name ends in `.json`) and sets the fields
    /// that take them, as one undo step. Returns how many values the file had, or why it could
    /// not be read.
    pub(crate) fn import_form_from(
        &mut self,
        ui: &MainWindow,
        path: &Path,
    ) -> Result<usize, String> {
        if self.field_edit.is_some() {
            self.field_cancel(ui);
        }
        let format = DataFormat::from_path(path).unwrap_or(DataFormat::Xfdf);
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        let values = form_data::read(text, format).map_err(|e| e.to_string())?;
        let count = values.len();
        self.send_edit(EditOp::ImportForm { values });
        ui.set_status_text(SharedString::from(format!(
            "Read {count} field values from \"{}\"",
            file_name(path)
        )));
        Ok(count)
    }

    /// The field's rectangle in viewport logical pixels, and the factor from points to them.
    fn field_view_rect(&self, page: u32, field: &FieldInfo) -> Option<(f32, f32, f32, f32, f32)> {
        let slot = self.layout.slot(page as usize)?;
        let s = self.scale_milli as f64 / 1000.0;
        let sf = self.sf();
        let k = s / sf;
        let x = (slot.x as f64 - self.pos.0) / sf + field.rect.x as f64 * k;
        let y = (slot.y as f64 - self.pos.1) / sf + field.rect.y as f64 * k;
        Some((
            x as f32,
            y as f32,
            (field.rect.width as f64 * k) as f32,
            (field.rect.height as f64 * k) as f32,
            k as f32,
        ))
    }

    fn open_field_editor(&mut self, ui: &MainWindow, page: u32, field: FieldInfo) {
        ui.set_field_editor_text(SharedString::from(field.value.as_str()));
        ui.set_field_editor_multiline(field.multiline);
        ui.set_field_editor_password(field.password);
        ui.set_field_editor_cells(field.comb_cells().map_or(0, |n| n as i32));
        set_comb_chars(ui, &field.value);
        self.field_edit = Some(FieldEdit {
            page,
            field,
            marks: Vec::new(),
        });
        self.place_field_editor(ui);
        ui.set_field_editor_visible(true);
        // An editor already showing (Tab from one text field to the next) takes the keys again.
        ui.set_field_editor_serial(ui.get_field_editor_serial() + 1);
    }

    fn open_field_choices(&mut self, ui: &MainWindow, page: u32, field: FieldInfo) {
        let labels: Vec<SharedString> = field
            .options
            .iter()
            .map(|o| SharedString::from(o.label.as_str()))
            .collect();
        let selected = field
            .options
            .iter()
            .position(|o| o.export == field.value)
            .map_or(-1, |i| i as i32);
        ui.set_field_choices(Rc::new(VecModel::from(labels)).into());
        ui.set_field_choice_selected(selected.max(0));
        // A multi-select list ticks its options; the others mark the one chosen.
        let multi = field.kind == FieldKind::ListBox && field.multi_select;
        let marks: Vec<bool> = field
            .options
            .iter()
            .map(|o| field.values.contains(&o.export))
            .collect();
        ui.set_field_choices_multi(multi);
        set_marks(ui, &marks);
        // An editable combo box also takes typed text, in the editor over the field.
        let editable = field.kind == FieldKind::ComboBox && field.editable;
        if editable {
            ui.set_field_editor_text(SharedString::from(field.value.as_str()));
            ui.set_field_editor_multiline(false);
            ui.set_field_editor_password(false);
            ui.set_field_editor_cells(0);
        }
        self.field_edit = Some(FieldEdit { page, field, marks });
        self.place_field_editor(ui);
        ui.set_field_editor_visible(editable);
        ui.set_field_choices_visible(true);
        ui.set_field_editor_serial(ui.get_field_editor_serial() + 1);
    }

    /// Keeps the editor, the options and the focus outline over their field as the view scrolls
    /// or zooms.
    pub(crate) fn place_field_editor(&mut self, ui: &MainWindow) {
        let ring = self
            .field_focus
            .and_then(|(page, _)| self.field_view_rect(page, self.focused_field()?));
        ui.set_field_focus_visible(ring.is_some());
        if let Some((x, y, w, h, _)) = ring {
            ui.set_field_focus_x(x);
            ui.set_field_focus_y(y);
            ui.set_field_focus_w(w);
            ui.set_field_focus_h(h);
        }
        let Some(edit) = self.field_edit.as_ref() else {
            return;
        };
        let Some((x, y, w, h, k)) = self.field_view_rect(edit.page, &edit.field) else {
            return;
        };
        ui.set_field_editor_x(x);
        ui.set_field_editor_y(y);
        ui.set_field_editor_w(w);
        ui.set_field_editor_h(h);
        ui.set_field_editor_font_size(editor_font_size(&edit.field) * k);
        // The options under the field, or above it when the view ends below.
        let rows = ui.get_field_choices().row_count().min(8) as f32;
        let list_h = rows * 24.0 + 4.0;
        let below = y + h + list_h <= ui.get_view_h();
        ui.set_field_choices_x(x);
        ui.set_field_choices_y(if below { y + h } else { (y - list_h).max(0.0) });
        ui.set_field_choices_w(w.max(120.0));
    }

    /// Enter, Tab or a click elsewhere: the editor's text, or a multi-select list's ticks, become
    /// the field's value.
    pub(crate) fn field_commit(&mut self, ui: &MainWindow) {
        let Some(edit) = self.field_edit.take() else {
            return;
        };
        let editor = ui.get_field_editor_visible();
        let multi = ui.get_field_choices_multi();
        ui.set_field_editor_visible(false);
        ui.set_field_choices_visible(false);
        ui.set_field_choices_multi(false);
        if multi {
            let values: Vec<String> = edit
                .field
                .options
                .iter()
                .zip(&edit.marks)
                .filter(|(_, on)| **on)
                .map(|(o, _)| o.export.clone())
                .collect();
            if values != edit.field.values {
                self.send_edit(EditOp::SetChoices {
                    page: edit.page,
                    name: edit.field.name,
                    values,
                });
            }
            return;
        }
        if !editor {
            return;
        }
        let text = clip_to_max_len(&ui.get_field_editor_text(), edit.field.max_len);
        if text != edit.field.value {
            self.fill_field(edit.page, &edit.field.name, text);
        }
    }

    /// The editor's text changed: it is held to the field's maximum length as it is typed, and a
    /// comb field's boxes follow it.
    pub(crate) fn field_edited(&mut self, ui: &MainWindow, text: &str) {
        let Some(edit) = self.field_edit.as_ref() else {
            return;
        };
        let clipped = clip_to_max_len(text, edit.field.max_len);
        if clipped != text {
            ui.set_field_editor_text(SharedString::from(clipped.as_str()));
        }
        if ui.get_field_editor_cells() > 0 {
            set_comb_chars(ui, &clipped);
        }
    }

    /// Esc: the field keeps its value.
    pub(crate) fn field_cancel(&mut self, ui: &MainWindow) {
        self.field_edit = None;
        ui.set_field_editor_visible(false);
        ui.set_field_choices_visible(false);
        ui.set_field_choices_multi(false);
    }

    /// A choice in the options list: chosen at once, or for a multi-select list ticked or
    /// unticked, the list staying open. Returns whether the list closed.
    pub(crate) fn field_choose(&mut self, ui: &MainWindow, index: i32) -> bool {
        if ui.get_field_choices_multi() {
            if let Some(edit) = self.field_edit.as_mut()
                && let Some(mark) = usize::try_from(index)
                    .ok()
                    .and_then(|i| edit.marks.get_mut(i))
            {
                *mark = !*mark;
                ui.set_field_choice_selected(index);
                let marks = edit.marks.clone();
                set_marks(ui, &marks);
            }
            return false;
        }
        let Some(edit) = self.field_edit.take() else {
            return true;
        };
        ui.set_field_editor_visible(false);
        ui.set_field_choices_visible(false);
        if let Some(option) = usize::try_from(index)
            .ok()
            .and_then(|i| edit.field.options.get(i))
            && option.export != edit.field.value
        {
            self.fill_field(edit.page, &edit.field.name, option.export.clone());
        }
        true
    }
}

/// The characters of a comb field's text, one per box.
fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

fn error_box(message: &str) {
    MessageDialog::error(message);
}

/// Form > Export Data...: asks where, then writes the values (Plan 0006 Phase 3).
pub(crate) fn export_form_data() {
    let Some((name, dir)) =
        with_state_ret(|state| (format!("{}.xfdf", state.doc_stem()), state.doc_dir()))
    else {
        return;
    };
    let mut dialog = FileDialog::new()
        .set_title("Export Form Data")
        .add_filter("XFDF form data", &["xfdf"])
        .add_filter("JSON", &["json"])
        .set_file_name(name);
    if let Some(dir) = dir {
        dialog = dialog.set_directory(dir);
    }
    dialog.save_file(|path| {
        if let Some(path) = path {
            with_state(|state| state.export_form_to(path));
        }
    });
}

/// Form > Import Data...: asks for a file, then sets the fields from it.
pub(crate) fn import_form_data(ui: &MainWindow) {
    let dir = with_state_ret(|state| state.doc_dir()).flatten();
    let mut dialog = FileDialog::new()
        .set_title("Import Form Data")
        .add_filter("Form data", &["xfdf", "json", "xml"]);
    if let Some(dir) = dir {
        dialog = dialog.set_directory(dir);
    }
    let weak = ui.as_weak();
    dialog.pick_file(move |path| {
        let (Some(ui), Some(path)) = (weak.upgrade(), path) else {
            return;
        };
        if let Some(Err(message)) = with_state_ret(|state| state.import_form_from(&ui, &path)) {
            error_box(&format!(
                "Could not read \"{}\": {message}",
                file_name(&path)
            ));
        }
    });
}

fn set_comb_chars(ui: &MainWindow, text: &str) {
    let chars: Vec<SharedString> = text
        .chars()
        .map(|c| SharedString::from(c.to_string()))
        .collect();
    ui.set_field_editor_chars(Rc::new(VecModel::from(chars)).into());
}

/// A multi-select list's ticks, one per option.
fn set_marks(ui: &MainWindow, marks: &[bool]) {
    ui.set_field_choice_marks(Rc::new(VecModel::from(marks.to_vec())).into());
}
