// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Filling form fields (Plan 0006 Phase 1).
//!
//! The engine reports each near page's field widgets, as it does annotations. With the Select
//! tool, a click on a check box toggles it and a click on a radio button turns it on. A click on a
//! text field opens an editor laid exactly over the field, in its text size (§0, question 1). A
//! click on a choice field opens its options under it. Each committed value is one edit, so one
//! undo step.

use crate::{AppState, MainWindow, Tool};
use pdf_core::EngineCmd;
use pdf_core::annot::EditOp;
use pdf_core::form::{FieldInfo, FieldKind, clip_to_max_len, editor_font_size, hit};
use pdf_core::select::PagePoint;
use slint::{Model as _, SharedString, VecModel};
use std::rc::Rc;

/// The field the editor or the options list is open for.
#[derive(Debug, Clone)]
pub(crate) struct FieldEdit {
    pub(crate) page: u32,
    pub(crate) field: FieldInfo,
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
    /// dropped.
    pub(crate) fn apply_fields(&mut self, page: u32, generation: u64, items: Vec<FieldInfo>) {
        if generation == self.layout_generation && self.fields_pending.remove(&page) {
            self.fields.insert(page, items);
        }
    }

    /// A page's content changed (a fill, an undo): its fields are asked for again.
    pub(crate) fn fields_changed(&mut self, page: u32) {
        if self.fields.remove(&page).is_some() || self.fields_pending.remove(&page) {
            self.fields_pending.insert(page);
            let _ = self.cmd_tx.send(EngineCmd::Fields {
                page,
                generation: self.layout_generation,
            });
        }
    }

    /// The fillable field widget under a page point.
    pub(crate) fn field_at(&self, point: PagePoint) -> Option<&FieldInfo> {
        let fields = self.fields.get(&point.page)?;
        let i = hit(fields, point.x, point.y)?;
        Some(&fields[i]).filter(|f| f.fillable())
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
    /// whether the press was on a field and is handled.
    pub(crate) fn field_pressed(&mut self, ui: &MainWindow, point: Option<PagePoint>) -> bool {
        if self.field_edit.is_some() {
            self.field_commit(ui);
        }
        if self.tool != Tool::Select || !self.editable {
            return false;
        }
        let Some((page, field)) = point.and_then(|p| Some((p.page, self.field_at(p)?.clone())))
        else {
            return false;
        };
        self.clear_selection(ui);
        self.select_annotation(ui, None);
        match field.kind {
            FieldKind::CheckBox | FieldKind::Radio => {
                if let Some(value) = field.clicked_value()
                    && value != field.value
                {
                    self.fill_field(page, &field.name, value);
                }
            }
            FieldKind::Text => self.open_field_editor(ui, page, field),
            FieldKind::ComboBox | FieldKind::ListBox => self.open_field_choices(ui, page, field),
            _ => return false,
        }
        true
    }

    fn fill_field(&mut self, page: u32, name: &str, value: String) {
        self.send_edit(EditOp::SetField {
            page,
            name: name.to_owned(),
            value,
        });
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
        self.field_edit = Some(FieldEdit { page, field });
        self.place_field_editor(ui);
        ui.set_field_editor_visible(true);
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
        ui.set_field_choice_selected(selected);
        // An editable combo box also takes typed text, in the editor over the field.
        let editable = field.kind == FieldKind::ComboBox && field.editable;
        if editable {
            ui.set_field_editor_text(SharedString::from(field.value.as_str()));
            ui.set_field_editor_multiline(false);
            ui.set_field_editor_password(false);
        }
        self.field_edit = Some(FieldEdit { page, field });
        self.place_field_editor(ui);
        ui.set_field_editor_visible(editable);
        ui.set_field_choices_visible(true);
    }

    /// Keeps the editor and the options over their field as the view scrolls or zooms.
    pub(crate) fn place_field_editor(&mut self, ui: &MainWindow) {
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

    /// Enter, Tab or a click elsewhere: the editor's text becomes the field's value.
    pub(crate) fn field_commit(&mut self, ui: &MainWindow) {
        let Some(edit) = self.field_edit.take() else {
            return;
        };
        let editor = ui.get_field_editor_visible();
        ui.set_field_editor_visible(false);
        ui.set_field_choices_visible(false);
        if !editor {
            return;
        }
        let text = clip_to_max_len(&ui.get_field_editor_text(), edit.field.max_len);
        if text != edit.field.value {
            self.fill_field(edit.page, &edit.field.name, text);
        }
    }

    /// Esc: the field keeps its value.
    pub(crate) fn field_cancel(&mut self, ui: &MainWindow) {
        self.field_edit = None;
        ui.set_field_editor_visible(false);
        ui.set_field_choices_visible(false);
    }

    /// A choice in the options list.
    pub(crate) fn field_choose(&mut self, ui: &MainWindow, index: i32) {
        let Some(edit) = self.field_edit.take() else {
            return;
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
    }
}
