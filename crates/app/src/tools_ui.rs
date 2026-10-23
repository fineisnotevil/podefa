// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Tools that write new files, inserting pages from a file, and the document's properties
//! (Plan 0005 Phase 2).
//!
//! The jobs run on the engine's tool thread (§4.2); the app shows their progress in the status bar,
//! lets them be cancelled, and offers to open what a merge or an extraction wrote. Platform dialogs
//! run outside the state's borrow, like Save As.

use crate::{AfterSave, AppState, MainWindow, proceed, with_state, with_state_ret};
use pdf_core::EngineCmd;
use pdf_core::annot::EditOp;
use pdf_core::pages::PageOp;
use pdf_core::tools::{
    DocumentInfo, MergeInput, Metadata, ToolJob, ToolSizes, parse_ranges, part_name, split_every,
};
use slint::SharedString;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// A job the app sent and has not heard the end of.
pub(crate) struct RunningTool {
    pub(crate) job_id: u64,
    pub(crate) title: &'static str,
    pub(crate) started: Instant,
    /// Offer to open the output when the job wrote one file.
    pub(crate) offer_open: bool,
}

impl AppState {
    /// The open document's file name without its extension, for naming outputs.
    pub(crate) fn doc_stem(&self) -> String {
        self.doc_path
            .as_deref()
            .and_then(Path::file_stem)
            .map_or_else(
                || "document".to_owned(),
                |s| s.to_string_lossy().into_owned(),
            )
    }

    pub(crate) fn doc_dir(&self) -> Option<PathBuf> {
        self.doc_path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
    }

    /// Sends a job to the tool thread; refused while another one runs.
    pub(crate) fn start_tool(&mut self, ui: &MainWindow, job: ToolJob, offer_open: bool) -> bool {
        if self.running_tool.is_some() {
            return false;
        }
        self.next_job_id += 1;
        let job_id = self.next_job_id;
        let title = job.title();
        self.running_tool = Some(RunningTool {
            job_id,
            title,
            started: Instant::now(),
            offer_open,
        });
        ui.set_tool_running(true);
        ui.set_status_text(SharedString::from(format!("{title}...")));
        let _ = self.cmd_tx.send(EngineCmd::RunTool { job_id, job });
        true
    }

    pub(crate) fn tool_progress(&mut self, ui: &MainWindow, job_id: u64, done: u32, total: u32) {
        if let Some(tool) = self.running_tool.as_ref().filter(|t| t.job_id == job_id) {
            ui.set_status_text(SharedString::from(format!(
                "{}... {done} of {total}",
                tool.title
            )));
        }
    }

    /// A job ended. Returns the file to offer opening, when the job wrote one and asked for that.
    pub(crate) fn tool_done(
        &mut self,
        ui: &MainWindow,
        job_id: u64,
        result: Result<Vec<PathBuf>, String>,
        sizes: Option<ToolSizes>,
    ) -> Option<Result<PathBuf, String>> {
        let tool = self.running_tool.take_if(|t| t.job_id == job_id)?;
        ui.set_tool_running(false);
        self.bench
            .tool_done(tool.title, tool.started.elapsed(), &result);
        match result {
            Ok(files) => {
                let message = match (files.as_slice(), sizes) {
                    (_, Some(sizes)) => crate::convert_ui::compress_message(sizes),
                    ([one], None) => format!("Wrote {}", one.display()),
                    (many, None) => format!("Wrote {} files", many.len()),
                };
                ui.set_status_text(SharedString::from(message));
                match files.as_slice() {
                    [one] if tool.offer_open => Some(Ok(one.clone())),
                    _ => None,
                }
            }
            Err(err) if err == "cancelled" => {
                ui.set_status_text(SharedString::from(format!("{}: cancelled", tool.title)));
                None
            }
            Err(err) => {
                ui.set_status_text(SharedString::from(format!("{} failed: {err}", tool.title)));
                Some(Err(err))
            }
        }
    }

    pub(crate) fn cancel_tool(&mut self) {
        if let Some(tool) = &self.running_tool {
            let _ = self.cmd_tx.send(EngineCmd::CancelTool {
                job_id: tool.job_id,
            });
        }
    }

    /// Opens the merge panel; it starts with the open document, as it is now.
    pub(crate) fn merge_open(&mut self, ui: &MainWindow) {
        // The list of an Images to PDF left open is not a merge's.
        if ui.get_merge_images() {
            self.merge_inputs.clear();
            ui.set_merge_selected(-1);
            ui.set_merge_images(false);
        }
        if self.merge_inputs.is_empty() && !self.page_sizes.is_empty() {
            self.merge_inputs.push(MergeInput::Current);
        }
        self.show_merge(ui);
        ui.set_merge_visible(true);
    }

    fn show_merge(&mut self, ui: &MainWindow) {
        let names: Vec<SharedString> = self
            .merge_inputs
            .iter()
            .map(|input| match input {
                MergeInput::Current => SharedString::from(format!("{} (open)", self.doc_name())),
                MergeInput::File(path) => SharedString::from(path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                )),
            })
            .collect();
        self.merge_model.set_vec(names);
        let selected = ui.get_merge_selected();
        if selected >= self.merge_inputs.len() as i32 {
            ui.set_merge_selected(self.merge_inputs.len() as i32 - 1);
        }
    }

    pub(crate) fn merge_add_files(&mut self, ui: &MainWindow, files: Vec<PathBuf>) {
        self.merge_inputs
            .extend(files.into_iter().map(MergeInput::File));
        self.show_merge(ui);
    }

    pub(crate) fn merge_add_current(&mut self, ui: &MainWindow) {
        if !self.page_sizes.is_empty() {
            self.merge_inputs.push(MergeInput::Current);
            self.show_merge(ui);
        }
    }

    /// Moves the chosen file up (`-1`) or down (`1`) the list.
    pub(crate) fn merge_move(&mut self, ui: &MainWindow, by: i32) {
        let from = ui.get_merge_selected();
        let to = from + by;
        let len = self.merge_inputs.len() as i32;
        if from < 0 || from >= len || to < 0 || to >= len {
            return;
        }
        self.merge_inputs.swap(from as usize, to as usize);
        ui.set_merge_selected(to);
        self.show_merge(ui);
    }

    pub(crate) fn merge_remove(&mut self, ui: &MainWindow) {
        let at = ui.get_merge_selected();
        if at >= 0 && (at as usize) < self.merge_inputs.len() {
            self.merge_inputs.remove(at as usize);
            self.show_merge(ui);
        }
    }

    pub(crate) fn merge_close(&mut self, ui: &MainWindow) {
        self.merge_inputs.clear();
        ui.set_merge_selected(-1);
        ui.set_merge_visible(false);
    }

    /// The groups of pages Split would write, from the panel.
    pub(crate) fn split_groups(&self, ui: &MainWindow) -> Result<Vec<Vec<u32>>, String> {
        let count = self.page_sizes.len() as u32;
        if ui.get_split_mode() == 0 {
            let every: u32 = ui
                .get_split_every()
                .trim()
                .parse()
                .ok()
                .filter(|n| *n >= 1)
                .ok_or_else(|| "Every how many pages? Type a number from 1.".to_owned())?;
            Ok(split_every(every, count))
        } else {
            parse_ranges(&ui.get_split_ranges(), count).map_err(|e| e.to_string())
        }
    }

    /// The files Split writes into `dir`: one per group, named by its pages.
    pub(crate) fn split_job(&self, groups: Vec<Vec<u32>>, dir: &Path) -> ToolJob {
        let stem = self.doc_stem();
        ToolJob::Extract {
            parts: groups
                .into_iter()
                .map(|pages| {
                    let path = dir.join(part_name(&stem, &pages));
                    (pages, path)
                })
                .collect(),
        }
    }

    /// Inserts every page of a file after the chosen pages, or after the current page.
    pub(crate) fn insert_from(&mut self, ui: &MainWindow, path: PathBuf) {
        let after = self.page_targets(ui).last().copied().unwrap_or(0);
        self.send_edit(EditOp::Pages(PageOp::InsertFrom {
            path,
            at: after + 1,
            source_pages: 0,
        }));
    }

    pub(crate) fn props_open(&mut self) {
        self.props_wanted = true;
        let _ = self.cmd_tx.send(EngineCmd::DocumentInfo {
            generation: self.doc_generation,
        });
    }

    /// The engine's answer: the panel shows it when it was asked for, and an undo of a properties
    /// edit refreshes a panel that is open.
    pub(crate) fn document_info(&mut self, ui: &MainWindow, generation: u64, info: DocumentInfo) {
        if generation != self.doc_generation || !(self.props_wanted || ui.get_props_visible()) {
            return;
        }
        self.props_wanted = false;
        let text = |v: &Option<String>| SharedString::from(v.clone().unwrap_or_default());
        ui.set_props_title(text(&info.fields.title));
        ui.set_props_author(text(&info.fields.author));
        ui.set_props_subject(text(&info.fields.subject));
        ui.set_props_keywords(text(&info.fields.keywords));
        let mut lines = vec![format!("{} pages, {}", self.page_sizes.len(), info.format)];
        for (label, value) in [
            ("Creator", &info.creator),
            ("Producer", &info.producer),
            ("Created", &info.created),
            ("Modified", &info.modified),
        ] {
            if !value.is_empty() {
                lines.push(format!("{label}: {value}"));
            }
        }
        if info.has_xmp {
            lines.push(
                "This file also keeps its information as XMP, which is not rewritten: some \
                 readers may still show the old values."
                    .to_owned(),
            );
        }
        ui.set_props_info(SharedString::from(lines.join("\n")));
        self.props_shown = info.fields;
        ui.set_props_visible(true);
    }

    /// Save in the properties panel: the fields that changed, as one undoable edit.
    pub(crate) fn props_save(&mut self, ui: &MainWindow) {
        let changed = |now: SharedString, before: &Option<String>| -> Option<String> {
            let now = now.trim().to_owned();
            (now != before.clone().unwrap_or_default()).then_some(now)
        };
        let fields = Metadata {
            title: changed(ui.get_props_title(), &self.props_shown.title),
            author: changed(ui.get_props_author(), &self.props_shown.author),
            subject: changed(ui.get_props_subject(), &self.props_shown.subject),
            keywords: changed(ui.get_props_keywords(), &self.props_shown.keywords),
        };
        ui.set_props_visible(false);
        if fields != Metadata::default() {
            self.send_edit(EditOp::SetMetadata(fields));
        }
    }
}

/// Add files... in the merge panel (Add images... when it orders images).
pub(crate) fn merge_add(ui: &MainWindow) {
    if ui.get_merge_images() {
        crate::convert_ui::add_images(ui);
        return;
    }
    let dir = with_state_ret(|state| state.doc_dir()).flatten();
    let mut dialog = rfd::FileDialog::new()
        .set_title("Add PDF files")
        .add_filter("PDF documents", &["pdf", "PDF"]);
    if let Some(dir) = dir {
        dialog = dialog.set_directory(dir);
    }
    if let Some(files) = dialog.pick_files() {
        with_state(|state| state.merge_add_files(ui, files));
    }
}

/// A save dialog for one new PDF, next to the open document.
pub(crate) fn save_dialog(title: &str, name: &str) -> Option<PathBuf> {
    let dir = with_state_ret(|state| state.doc_dir()).flatten();
    let mut dialog = rfd::FileDialog::new()
        .set_title(title)
        .add_filter("PDF document", &["pdf"])
        .set_file_name(name);
    if let Some(dir) = dir {
        dialog = dialog.set_directory(dir);
    }
    dialog.save_file()
}

/// Merge... in the merge panel: where to write, then the job (Make PDF... for images).
pub(crate) fn merge_run(ui: &MainWindow) {
    if ui.get_merge_images() {
        crate::convert_ui::images_run(ui);
        return;
    }
    let Some(output) = save_dialog("Save the merged document", "merged.pdf") else {
        return;
    };
    with_state(|state| {
        let inputs = state.merge_inputs.clone();
        if state.start_tool(ui, ToolJob::Merge { inputs, output }, true) {
            state.merge_close(ui);
        }
    });
}

/// Split... in the split panel: the groups, a folder, a check for files it would replace.
pub(crate) fn split_run(ui: &MainWindow) {
    let groups = match with_state_ret(|state| state.split_groups(ui)) {
        Some(Ok(groups)) => groups,
        Some(Err(err)) => {
            ui.set_split_error(SharedString::from(err));
            return;
        }
        None => return,
    };
    ui.set_split_error(SharedString::new());
    let dir = with_state_ret(|state| state.doc_dir()).flatten();
    let mut dialog = rfd::FileDialog::new().set_title("Folder for the parts");
    if let Some(dir) = dir {
        dialog = dialog.set_directory(dir);
    }
    let Some(folder) = dialog.pick_folder() else {
        return;
    };
    let Some(job) = with_state_ret(|state| state.split_job(groups, &folder)) else {
        return;
    };
    if !confirm_replace(&job) {
        return;
    }
    with_state(|state| {
        if state.start_tool(ui, job, false) {
            ui.set_split_visible(false);
        }
    });
}

/// Asks before a job writes over files that exist; true to go on.
pub(crate) fn confirm_replace(job: &ToolJob) -> bool {
    let existing = job.outputs().iter().filter(|p| p.exists()).count();
    if existing == 0 {
        return true;
    }
    let answer = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Warning)
        .set_title("PODEFA")
        .set_description(format!(
            "{existing} of the {} files already exist in that folder. Replace them?",
            job.outputs().len()
        ))
        .set_buttons(rfd::MessageButtons::YesNo)
        .show();
    answer == rfd::MessageDialogResult::Yes
}

/// Tools > Extract Pages...: the chosen pages (or the current one) into one new file.
pub(crate) fn extract_pages(ui: &MainWindow) {
    let Some((pages, name)) = with_state_ret(|state| {
        let pages = state.page_targets(ui);
        let name = part_name(&state.doc_stem(), &pages);
        (pages, name)
    }) else {
        return;
    };
    let Some(output) = save_dialog("Save the extracted pages", &name) else {
        return;
    };
    with_state(|state| {
        state.start_tool(
            ui,
            ToolJob::Extract {
                parts: vec![(pages, output)],
            },
            true,
        );
    });
}

/// Insert Pages from File...: the file, then one undoable edit.
pub(crate) fn insert_from_file(ui: &MainWindow) {
    let dir = with_state_ret(|state| state.doc_dir()).flatten();
    let mut dialog = rfd::FileDialog::new()
        .set_title("Insert pages from")
        .add_filter("PDF documents", &["pdf", "PDF"]);
    if let Some(dir) = dir {
        dialog = dialog.set_directory(dir);
    }
    if let Some(path) = dialog.pick_file() {
        with_state(|state| state.insert_from(ui, path));
    }
}

/// A job ended: offer to open what it wrote, or say why it failed.
pub(crate) fn tool_finished(ui: &MainWindow, outcome: Option<Result<PathBuf, String>>) {
    match outcome {
        Some(Ok(path)) => {
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            let answer = rfd::MessageDialog::new()
                .set_title("PODEFA")
                .set_description(format!("Open \"{name}\" now?"))
                .set_buttons(rfd::MessageButtons::YesNo)
                .show();
            if answer == rfd::MessageDialogResult::Yes {
                proceed(ui, AfterSave::Open(path));
            }
        }
        Some(Err(err)) => {
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Error)
                .set_title("PODEFA")
                .set_description(err)
                .set_buttons(rfd::MessageButtons::Ok)
                .show();
        }
        None => {}
    }
}
