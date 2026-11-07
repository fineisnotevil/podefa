// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Signing (Plan 0009 Phase 4, §4.2): the Sign tool draws the box a new signature goes in, a
//! click on an empty signature field signs that field, and both open the sign dialog: the key
//! file (`.p12`/`.pfx`) and its password, the reason and the location, and a preview of the
//! appearance. The engine saves the document's changes, signs and saves again (ADR 0015).

use crate::platform::FileDialog;
use crate::stamps_ui::CropDraw;
use crate::{AppState, MainWindow, Tool, with_state, with_state_ret};
use pdf_core::EngineCmd;
use pdf_core::Rect;
use pdf_core::form::{FieldInfo, FieldKind};
use pdf_core::secure::Need;
use pdf_core::select::PagePoint;
use pdf_core::signature::{SignError, SignPlace, SignRequest};
use slint::{ComponentHandle as _, SharedString};
use std::path::PathBuf;

/// The smallest box a drag makes a signature of, in points (the engine refuses smaller).
const SIGN_MIN_PT: f32 = 24.0;

/// What the sign dialog keeps between signatures in one run: never the password.
#[derive(Debug, Clone, Default)]
pub(crate) struct SignMemory {
    pub(crate) key_file: Option<PathBuf>,
    pub(crate) reason: String,
    pub(crate) location: String,
    /// Where the open dialog's signature goes.
    pub(crate) place: Option<SignPlace>,
}

impl AppState {
    /// The Sign tool, or back to Select.
    pub(crate) fn sign_tool(&mut self, ui: &MainWindow) {
        if !self.editable || self.page_sizes.is_empty() {
            return;
        }
        self.crop_cancel(ui);
        self.choose_tool(ui, Tool::Sign);
        if self.tool == Tool::Sign {
            ui.set_status_text(SharedString::from(
                "Sign: drag the box the signature goes in, or click an empty signature field",
            ));
        }
    }

    /// A rectangle the Sign tool drew: the dialog, for a new field there.
    pub(crate) fn sign_drawn(&mut self, ui: &MainWindow, draw: CropDraw) {
        let [x0, y0, x1, y1] = draw.rect();
        if x1 - x0 < SIGN_MIN_PT || y1 - y0 < SIGN_MIN_PT {
            ui.set_status_text(SharedString::from(
                "Draw a larger box: a signature needs room for its name and date",
            ));
            return;
        }
        let place = SignPlace::New {
            page: draw.page,
            rect: Rect::new(x0, y0, x1 - x0, y1 - y0),
        };
        self.open_sign(ui, place);
    }

    /// A press on the page: an empty signature field under it opens the dialog for it.
    pub(crate) fn sign_field_pressed(&mut self, ui: &MainWindow, point: Option<PagePoint>) -> bool {
        if !self.editable || !matches!(self.tool, Tool::Select | Tool::Sign) {
            return false;
        }
        let Some(point) = point else {
            return false;
        };
        let Some(field) = self.empty_signature_at(point).cloned() else {
            return false;
        };
        self.open_sign(
            ui,
            SignPlace::Field {
                page: point.page,
                name: field.name,
            },
        );
        true
    }

    /// The empty, writable signature field under a page point.
    pub(crate) fn empty_signature_at(&self, point: PagePoint) -> Option<&FieldInfo> {
        self.fields.get(&point.page)?.iter().find(|field| {
            field.kind == FieldKind::Signature
                && !field.read_only
                && point.x >= field.rect.x
                && point.x <= field.rect.x + field.rect.width
                && point.y >= field.rect.y
                && point.y <= field.rect.y + field.rect.height
                && !self.signatures.iter().any(|s| s.field == field.name)
        })
    }

    fn open_sign(&mut self, ui: &MainWindow, place: SignPlace) {
        if !self.permits(ui, Need::Fill) {
            return;
        }
        if self.security.certified == Some(1) {
            ui.set_status_text(SharedString::from(
                "This document is certified with no changes allowed, not even a signature",
            ));
            return;
        }
        self.sign_memory.place = Some(place);
        let memory = &self.sign_memory;
        ui.set_sign_file(SharedString::from(
            memory
                .key_file
                .as_deref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        ));
        ui.set_sign_password(SharedString::new());
        ui.set_sign_reason(SharedString::from(memory.reason.as_str()));
        ui.set_sign_location(SharedString::from(memory.location.as_str()));
        ui.set_sign_signer(SharedString::new());
        ui.set_sign_date(SharedString::from(today()));
        ui.set_sign_error(SharedString::new());
        ui.set_sign_busy(false);
        ui.set_sign_visible(true);
    }

    /// The engine signed, or said why not.
    pub(crate) fn signed(
        &mut self,
        ui: &MainWindow,
        generation: u64,
        result: Result<u64, SignError>,
    ) {
        if generation != self.doc_generation {
            return;
        }
        self.saving = false;
        ui.set_sign_busy(false);
        match result {
            Ok(_) => {
                ui.set_sign_visible(false);
                self.sign_memory.place = None;
                if self.tool == Tool::Sign {
                    self.choose_tool(ui, Tool::Sign);
                }
                ui.set_status_text(SharedString::from(
                    "Signed: the signature is saved in the file",
                ));
            }
            Err(error) => {
                ui.set_sign_error(SharedString::from(error_text(&error)));
            }
        }
    }
}

/// The date the appearance shows, for the preview: the day, as MuPDF writes it with the time.
fn today() -> String {
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400) as i64;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// What the dialog says about a refusal, as a sentence.
fn error_text(error: &SignError) -> String {
    match error {
        SignError::WrongPassword => "The key file's password is wrong.".to_owned(),
        SignError::NeedsSaveAs => "This file cannot take a signature as it is: save it as a copy \
                                   (Save As), then sign the copy."
            .to_owned(),
        other => {
            let text = other.to_string();
            let mut chars = text.chars();
            match chars.next() {
                Some(first) => format!("{}{}.", first.to_uppercase(), chars.as_str()),
                None => text,
            }
        }
    }
}

/// Choose... in the dialog: the key file.
fn choose_file(ui: &MainWindow) {
    let dir = with_state_ret(|state| {
        state
            .sign_memory
            .key_file
            .as_deref()
            .and_then(|p| p.parent())
            .map(PathBuf::from)
    })
    .flatten();
    let mut dialog = FileDialog::new().add_filter("Key file", &["p12", "pfx"]);
    if let Some(dir) = dir {
        dialog = dialog.set_directory(dir);
    }
    let weak = ui.as_weak();
    dialog.pick_file(move |path| {
        let (Some(ui), Some(path)) = (weak.upgrade(), path) else {
            return;
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        with_state(|state| state.sign_memory.key_file = Some(path));
        ui.set_sign_file(SharedString::from(name));
        ui.set_sign_signer(SharedString::new());
        ui.set_sign_error(SharedString::new());
        check(&ui);
    });
}

/// The key file and the password as typed: the signer's name for the preview, or why not.
fn check(ui: &MainWindow) -> bool {
    let Some(key_file) = with_state_ret(|state| state.sign_memory.key_file.clone()).flatten()
    else {
        ui.set_sign_error(SharedString::from("Choose the key file to sign with."));
        return false;
    };
    match engine_mupdf::signer_name(&key_file, &ui.get_sign_password()) {
        Ok(name) => {
            ui.set_sign_signer(SharedString::from(name));
            ui.set_sign_error(SharedString::new());
            true
        }
        // Not said before a password was typed: the file asks for one.
        Err(SignError::WrongPassword) if ui.get_sign_password().is_empty() => {
            ui.set_sign_signer(SharedString::new());
            false
        }
        Err(error) => {
            ui.set_sign_signer(SharedString::new());
            ui.set_sign_error(SharedString::from(error_text(&error)));
            false
        }
    }
}

/// Sign in the dialog.
fn apply(ui: &MainWindow) {
    if ui.get_sign_busy() || !check(ui) {
        if ui.get_sign_error().is_empty() {
            ui.set_sign_error(SharedString::from("The key file's password is wrong."));
        }
        return;
    }
    let reason = ui.get_sign_reason().to_string();
    let location = ui.get_sign_location().to_string();
    let password = ui.get_sign_password().to_string();
    let sent = with_state_ret(|state| {
        let (Some(key_file), Some(place)) = (
            state.sign_memory.key_file.clone(),
            state.sign_memory.place.clone(),
        ) else {
            return false;
        };
        if state.saving {
            return false;
        }
        state.sign_memory.reason = reason.clone();
        state.sign_memory.location = location.clone();
        let request = SignRequest {
            key_file,
            password,
            reason,
            location,
            place,
        };
        state.saving = state
            .cmd_tx
            .send(EngineCmd::Sign {
                request: Box::new(request),
                generation: state.doc_generation,
            })
            .is_ok();
        state.saving
    })
    .unwrap_or(false);
    if sent {
        ui.set_sign_busy(true);
        ui.set_status_text(SharedString::from("Signing..."));
    }
}

fn close(ui: &MainWindow) {
    if ui.get_sign_busy() {
        return;
    }
    ui.set_sign_visible(false);
    ui.set_sign_password(SharedString::new());
    with_state(|state| state.sign_memory.place = None);
}

pub(crate) fn connect(ui: &MainWindow) {
    let weak = ui.as_weak();
    ui.on_sign_tool(move || {
        if let Some(ui) = weak.upgrade() {
            with_state(|state| state.sign_tool(&ui));
        }
    });
    let weak = ui.as_weak();
    ui.on_sign_choose_file(move || {
        if let Some(ui) = weak.upgrade() {
            choose_file(&ui);
        }
    });
    let weak = ui.as_weak();
    ui.on_sign_check(move || {
        if let Some(ui) = weak.upgrade() {
            check(&ui);
        }
    });
    let weak = ui.as_weak();
    ui.on_sign_apply(move || {
        if let Some(ui) = weak.upgrade() {
            apply(&ui);
        }
    });
    let weak = ui.as_weak();
    ui.on_sign_close(move || {
        if let Some(ui) = weak.upgrade() {
            close(&ui);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_read_as_sentences() {
        assert_eq!(
            error_text(&SignError::Certified),
            "The document is certified with no changes allowed, not even a signature."
        );
        assert!(error_text(&SignError::NeedsSaveAs).contains("Save As"));
    }

    #[test]
    fn today_is_a_date() {
        let day = today();
        assert_eq!(day.len(), 10, "{day}");
        assert_eq!(&day[4..5], "-");
        assert!(day.as_str() >= "2026-01-01");
    }
}
