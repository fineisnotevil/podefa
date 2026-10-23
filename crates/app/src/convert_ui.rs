// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Compress, export as images and images to PDF (Plan 0005 Phase 4).
//!
//! All three run on the engine's tool thread like merge and split (Phase 2): compress and export
//! read the open document as it is now, unsaved changes included. One panel sets compress's preset
//! or the export's format, resolution and pages; platform dialogs then pick where to write, outside
//! the state's borrow.

use crate::{AppState, MainWindow, with_state, with_state_ret};
use pdf_core::tools::{ImageFormat, MergeInput, Preset, ToolJob, ToolSizes, image_name};
use slint::SharedString;
use std::path::PathBuf;

/// JPEG quality of exported pages.
const EXPORT_JPEG_QUALITY: u8 = 85;

/// A byte count as people read it.
fn size_text(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1024.0 * 1024.0 {
        format!("{:.1} MiB", b / (1024.0 * 1024.0))
    } else {
        format!("{:.0} KiB", (b / 1024.0).max(1.0))
    }
}

/// What the status bar says after a compress.
pub(crate) fn compress_message(sizes: ToolSizes) -> String {
    if sizes.kept_original {
        format!(
            "Compress: no smaller copy was possible ({}); the copy is the original",
            size_text(sizes.before)
        )
    } else {
        let saved = 100.0 * (1.0 - sizes.after as f64 / sizes.before.max(1) as f64);
        format!(
            "Compressed: {} to {} ({saved:.0}% smaller)",
            size_text(sizes.before),
            size_text(sizes.after)
        )
    }
}

impl AppState {
    /// Tools > Compress (`mode` 0) or Export as Images (1).
    pub(crate) fn convert_open(&mut self, ui: &MainWindow, mode: i32) {
        if self.page_sizes.is_empty() || (mode == 0 && !self.editable) {
            return;
        }
        ui.set_convert_mode(mode);
        ui.set_convert_error(SharedString::new());
        ui.set_convert_visible(true);
    }

    /// The export the panel describes, its files in `dir`.
    fn export_job(&self, ui: &MainWindow, dir: &std::path::Path) -> Result<ToolJob, String> {
        let dpi: u32 = ui
            .get_convert_dpi()
            .trim()
            .parse()
            .ok()
            .filter(|d| (10..=1200).contains(d))
            .ok_or_else(|| "Which resolution? Type a number of dpi from 10 to 1200.".to_owned())?;
        let format = if ui.get_convert_format() == 1 {
            ImageFormat::Jpeg {
                quality: EXPORT_JPEG_QUALITY,
            }
        } else {
            ImageFormat::Png
        };
        let pages = self.stamp_pages(&ui.get_convert_pages())?;
        let count = self.page_sizes.len() as u32;
        let stem = self.doc_stem();
        Ok(ToolJob::ExportImages {
            pages: pages
                .into_iter()
                .map(|p| (p, dir.join(image_name(&stem, p, count, format))))
                .collect(),
            format,
            dpi,
        })
    }
}

/// The panel's button: Compress... asks where to write the copy, Export... for a folder.
pub(crate) fn convert_run(ui: &MainWindow) {
    if ui.get_convert_mode() == 0 {
        compress_run(ui);
    } else {
        export_run(ui);
    }
}

fn compress_run(ui: &MainWindow) {
    let preset = Preset::ALL
        .get(ui.get_convert_preset().max(0) as usize)
        .copied()
        .unwrap_or(Preset::Balanced);
    let Some(name) = with_state_ret(|state| format!("{}_compressed.pdf", state.doc_stem())) else {
        return;
    };
    let Some(output) = crate::tools_ui::save_dialog("Save the compressed copy", &name) else {
        return;
    };
    with_state(|state| {
        let job = ToolJob::Compress {
            input: MergeInput::Current,
            preset,
            output,
        };
        if state.start_tool(ui, job, true) {
            ui.set_convert_visible(false);
        }
    });
}

fn export_run(ui: &MainWindow) {
    // The values first, so a mistake shows before any dialog.
    if let Some(Err(err)) = with_state_ret(|state| state.export_job(ui, std::path::Path::new(""))) {
        ui.set_convert_error(SharedString::from(err));
        return;
    }
    ui.set_convert_error(SharedString::new());
    let dir = with_state_ret(|state| state.doc_dir()).flatten();
    let mut dialog = rfd::FileDialog::new().set_title("Folder for the images");
    if let Some(dir) = dir {
        dialog = dialog.set_directory(dir);
    }
    let Some(folder) = dialog.pick_folder() else {
        return;
    };
    let Some(Ok(job)) = with_state_ret(|state| state.export_job(ui, &folder)) else {
        return;
    };
    if !crate::tools_ui::confirm_replace(&job) {
        return;
    }
    with_state(|state| {
        if state.start_tool(ui, job, false) {
            ui.set_convert_visible(false);
        }
    });
}

/// Tools > Images to PDF...: the merge panel, ordering images, and at once the dialog to add them.
pub(crate) fn images_to_pdf(ui: &MainWindow) {
    with_state(|state| {
        state.merge_inputs.clear();
        state.merge_add_files(ui, Vec::new());
    });
    ui.set_merge_selected(-1);
    ui.set_merge_images(true);
    ui.set_merge_visible(true);
    add_images(ui);
}

/// Add images... in the panel: they join the end of the list, by name among themselves (platform
/// dialogs return a selection in no promised order); Up and Down set the order.
pub(crate) fn add_images(ui: &MainWindow) {
    let dir = with_state_ret(|state| state.doc_dir()).flatten();
    let mut dialog = rfd::FileDialog::new()
        .set_title("Images to make a PDF of")
        .add_filter(
            "Images",
            &[
                "png", "jpg", "jpeg", "tif", "tiff", "bmp", "gif", "PNG", "JPG", "JPEG",
            ],
        );
    if let Some(dir) = dir {
        dialog = dialog.set_directory(dir);
    }
    let Some(mut images) = dialog.pick_files() else {
        return;
    };
    images.sort();
    with_state(|state| state.merge_add_files(ui, images));
}

/// Make PDF... in the panel: where to write, then the job, the images in the list's order.
pub(crate) fn images_run(ui: &MainWindow) {
    let Some(images) = with_state_ret(|state| {
        state
            .merge_inputs
            .iter()
            .filter_map(|input| match input {
                MergeInput::File(path) => Some(path.clone()),
                MergeInput::Current => None,
            })
            .collect::<Vec<PathBuf>>()
    }) else {
        return;
    };
    let name = images
        .first()
        .and_then(|p| p.file_stem())
        .map_or_else(|| "images".to_owned(), |s| s.to_string_lossy().into_owned());
    let Some(output) = crate::tools_ui::save_dialog("Save the PDF", &format!("{name}.pdf")) else {
        return;
    };
    with_state(|state| {
        if state.start_tool(ui, ToolJob::ImagesToPdf { images, output }, true) {
            state.merge_close(ui);
            ui.set_merge_images(false);
        }
    });
}
