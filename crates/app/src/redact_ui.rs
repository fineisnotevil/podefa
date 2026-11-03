// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! The Redact tool (Plan 0009 Phase 1, §4.1).
//!
//! A drag marks an area (drawn like the Crop tool's rectangle) and Mark Search Results marks every
//! hit of the current search; each is one undo step, and the marks show on the pages as MuPDF draws
//! them. Apply asks first, then where to write the redacted copy (`name-redacted.pdf` beside the
//! document), applies the marks and saves there with garbage collection, so the copy becomes the
//! open document and the original stays as it was. Outline entries that name the removed text are
//! put to the user before the save (§0, question 2).

use crate::platform::{Answer, Buttons, FileDialog, Level, MessageDialog};
use crate::stamps_ui::CropDraw;
use crate::{AppState, MainWindow, Tool, with_state, with_state_ret};
use pdf_core::Rect;
use pdf_core::annot::{EditOp, RedactionMark};
use slint::{ComponentHandle as _, SharedString};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// How many outline entries the question names before it says how many more there are.
const OUTLINE_SHOWN: usize = 8;

impl AppState {
    /// The Redact tool, or back to Select.
    pub(crate) fn redact_tool(&mut self, ui: &MainWindow) {
        if !self.editable || self.page_sizes.is_empty() {
            return;
        }
        self.crop_cancel(ui);
        self.choose_tool(ui, Tool::Redact);
        if self.tool == Tool::Redact {
            ui.set_status_text(SharedString::from(
                "Redact: drag over what to remove; Apply writes the redacted copy",
            ));
        }
    }

    /// A rectangle the Redact tool drew: one mark.
    pub(crate) fn redact_drawn(&mut self, draw: CropDraw) {
        let [x0, y0, x1, y1] = draw.rect();
        self.send_edit(EditOp::MarkRedactions {
            marks: vec![RedactionMark {
                page: draw.page,
                rects: vec![Rect::new(x0, y0, x1 - x0, y1 - y0)],
            }],
        });
    }

    /// Mark Search Results: one mark per hit, all of them one undo step.
    pub(crate) fn redact_search(&mut self, ui: &MainWindow) {
        let mut by_page: BTreeMap<u32, Vec<RedactionMark>> = BTreeMap::new();
        for hit in self.search.hits() {
            by_page.entry(hit.page).or_default().push(RedactionMark {
                page: hit.page,
                rects: hit.rects.clone(),
            });
        }
        let marks: Vec<RedactionMark> = by_page.into_values().flatten().collect();
        if marks.is_empty() {
            ui.set_status_text(SharedString::from(
                "Find the text to redact first (Ctrl+F), then mark the results",
            ));
            return;
        }
        let count = marks.len();
        self.send_edit(EditOp::MarkRedactions { marks });
        ui.set_status_text(SharedString::from(format!(
            "Marked {count} search {} for redaction",
            if count == 1 { "result" } else { "results" }
        )));
    }

    /// An edit the engine refused: an Apply waiting for its outcome saves nothing.
    pub(crate) fn redact_refused(&mut self, edit_id: u64) {
        if self
            .redact_pending
            .as_ref()
            .is_some_and(|(id, _)| *id == edit_id)
        {
            self.redact_pending = None;
        }
    }
}

/// Where the redacted copy goes by default: `name-redacted.pdf` beside the document.
fn redacted_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document".to_owned());
    format!("{stem}-redacted.pdf")
}

/// Apply in the Redact panel: asks, then where to save, then applies and saves.
pub(crate) fn apply(ui: &MainWindow) {
    if !with_state_ret(|state| state.editable).unwrap_or(false) {
        return;
    }
    let clean = ui.get_redact_clean();
    let mut text = "Apply the redaction marks?\n\nThe text, images and drawing under the marks, \
                    and the annotations and form fields touching them, are removed for good from \
                    a new file. The original document stays as it is."
        .to_owned();
    if clean {
        text.push_str(
            "\n\nThe document's title, author, metadata, attachments, scripts and structure \
             tree are removed too.",
        );
    }
    let weak = ui.as_weak();
    MessageDialog::new(text)
        .level(Level::Warning)
        .buttons(Buttons::YesNo)
        .show(move |answer| {
            if let (Some(ui), Answer::Yes) = (weak.upgrade(), answer) {
                choose_target(&ui, clean);
            }
        });
}

/// The save dialog for the redacted copy; the marks are applied once a file is chosen.
fn choose_target(ui: &MainWindow, clean: bool) {
    let Some((dir, name)) = with_state_ret(|state| {
        let path = state.doc_path.clone()?;
        let dir = match &state.untitled {
            Some(_) => state.origin_dir.clone(),
            None => path.parent().map(Path::to_path_buf),
        };
        let name = match &state.untitled {
            Some(title) => format!("{title}-redacted.pdf"),
            None => redacted_name(&path),
        };
        Some((dir, name))
    })
    .flatten() else {
        return;
    };
    let mut dialog = FileDialog::new()
        .add_filter("PDF document", &["pdf"])
        .set_file_name(name);
    if let Some(dir) = dir {
        dialog = dialog.set_directory(dir);
    }
    let weak = ui.as_weak();
    dialog.save_file(move |path| {
        let (Some(ui), Some(path)) = (weak.upgrade(), path) else {
            return;
        };
        with_state(|state| {
            state.send_edit(EditOp::ApplyRedactions { clean });
            state.redact_pending = Some((state.next_edit_id, path));
            ui.set_status_text(SharedString::from("Applying the redaction marks..."));
        });
    });
}

/// The marks were applied: says what went with them, asks about outline entries that name the
/// removed text, and saves the copy.
pub(crate) fn redacted(
    ui: &MainWindow,
    generation: u64,
    annotations: u32,
    fields: u32,
    outline: Vec<(u32, String)>,
) {
    let Some(path) = with_state_ret(|state| {
        if generation != state.doc_generation {
            return None;
        }
        state.redact_pending.take().map(|(_, path)| path)
    })
    .flatten() else {
        return;
    };
    let mut status = "Redacted".to_owned();
    if annotations + fields > 0 {
        let _ = write!(
            status,
            "; removed {annotations} annotation{} and {fields} form field{} touching the marks",
            if annotations == 1 { "" } else { "s" },
            if fields == 1 { "" } else { "s" },
        );
    }
    ui.set_status_text(SharedString::from(status));
    if outline.is_empty() {
        save(ui, path);
        return;
    }
    let mut text = format!(
        "{} outline {} the redacted text:\n",
        outline.len(),
        if outline.len() == 1 {
            "entry names"
        } else {
            "entries name"
        }
    );
    for (_, title) in outline.iter().take(OUTLINE_SHOWN) {
        let _ = write!(text, "\n- {title}");
    }
    if outline.len() > OUTLINE_SHOWN {
        let _ = write!(text, "\n- and {} more", outline.len() - OUTLINE_SHOWN);
    }
    text.push_str("\n\nRemove them, with the entries under them, from the redacted copy?");
    let items: Vec<u32> = outline.into_iter().map(|(index, _)| index).collect();
    let weak = ui.as_weak();
    MessageDialog::new(text)
        .level(Level::Warning)
        .buttons(Buttons::YesNo)
        .show(move |answer| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if answer == Answer::Yes {
                with_state(|state| state.send_edit(EditOp::RemoveOutline { items }));
            }
            save(&ui, path);
        });
}

/// Writes the redacted copy, which becomes the open document.
fn save(ui: &MainWindow, path: PathBuf) {
    with_state(|state| state.send_save(ui, Some(path)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_copy_is_named_after_the_document() {
        assert_eq!(
            redacted_name(Path::new("/a/report.final.pdf")),
            "report.final-redacted.pdf"
        );
        assert_eq!(redacted_name(Path::new("")), "document-redacted.pdf");
    }
}
