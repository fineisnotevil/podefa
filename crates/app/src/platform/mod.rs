// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! What the desktop and Android do differently (Plan 0008 Phase 1, ADR 0012): file dialogs,
//! message boxes, the clipboard, and where settings and temporary files live.
//!
//! The dialogs answer through a callback, not a return value: on Android the system's pickers
//! report back through the activity while the event loop keeps running, so nothing may wait for
//! them. On the desktop the callback runs before the call returns, as the modal dialogs always
//! did. Either way the caller must not hold the application state across the call.
//!
//! On Android a picked document is a `content://` URI. The app works on a copy in its cache
//! folder, and whatever the app writes to a path a dialog gave it is written back to the URI it
//! stands for (`written`).

use std::path::{Path, PathBuf};

#[cfg(not(target_os = "android"))]
mod desktop;
#[cfg(not(target_os = "android"))]
use desktop as imp;

#[cfg(target_os = "android")]
pub mod android;
#[cfg(target_os = "android")]
use android as imp;

/// A file dialog: built like `rfd::FileDialog`, answered through a callback.
#[derive(Default)]
pub struct FileDialog {
    title: String,
    /// A name for each filter and its extensions, without the dot.
    filters: Vec<(String, Vec<String>)>,
    directory: Option<PathBuf>,
    file_name: Option<String>,
}

impl FileDialog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_title(mut self, title: &str) -> Self {
        title.clone_into(&mut self.title);
        self
    }

    pub fn add_filter(mut self, name: &str, extensions: &[&str]) -> Self {
        let extensions = extensions.iter().map(|ext| (*ext).to_owned()).collect();
        self.filters.push((name.to_owned(), extensions));
        self
    }

    pub fn set_directory(mut self, directory: impl AsRef<Path>) -> Self {
        self.directory = Some(directory.as_ref().to_path_buf());
        self
    }

    pub fn set_file_name(mut self, name: impl Into<String>) -> Self {
        self.file_name = Some(name.into());
        self
    }

    /// One existing file, or `None` when cancelled.
    pub fn pick_file(self, then: impl FnOnce(Option<PathBuf>) + 'static) {
        imp::pick_file(self, Box::new(then));
    }

    /// Existing files, or `None` when cancelled.
    pub fn pick_files(self, then: impl FnOnce(Option<Vec<PathBuf>>) + 'static) {
        imp::pick_files(self, Box::new(then));
    }

    /// A folder to write into, or `None` when cancelled.
    pub fn pick_folder(self, then: impl FnOnce(Option<PathBuf>) + 'static) {
        imp::pick_folder(self, Box::new(then));
    }

    /// Where to write a new file, or `None` when cancelled.
    pub fn save_file(self, then: impl FnOnce(Option<PathBuf>) + 'static) {
        imp::save_file(self, Box::new(then));
    }

    /// The MIME types the filters stand for, for Android's pickers; a catch-all filter only
    /// when it is the only one, since a picker offers no choice between filters.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    fn mime_types(&self) -> Vec<&'static str> {
        let mut types = Vec::new();
        for ext in self.filters.iter().flat_map(|(_, exts)| exts) {
            let mime = mime_for(ext);
            if !types.contains(&mime) {
                types.push(mime);
            }
        }
        if types.len() > 1 {
            types.retain(|mime| *mime != "*/*");
        }
        types
    }
}

/// The MIME type of a file extension (without the dot); `*/*` for one the app does not know.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn mime_for(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "tif" | "tiff" => "image/tiff",
        "bmp" => "image/bmp",
        "gif" => "image/gif",
        "json" => "application/json",
        "xfdf" | "xml" => "application/xml",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        _ => "*/*",
    }
}

/// A message box's look.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Info,
    Warning,
    Error,
}

/// A message box's buttons.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Buttons {
    Ok,
    YesNo,
    YesNoCancel,
}

/// The button pressed; OK answers `Yes`, and closing the box without one `Cancel`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Answer {
    Yes,
    No,
    Cancel,
}

/// A message box: built like `rfd::MessageDialog`, answered through a callback.
pub struct MessageDialog {
    /// Android's boxes have one look.
    #[cfg_attr(target_os = "android", allow(dead_code))]
    level: Level,
    description: String,
    buttons: Buttons,
}

impl MessageDialog {
    pub fn new(description: impl Into<String>) -> Self {
        Self {
            level: Level::Info,
            description: description.into(),
            buttons: Buttons::Ok,
        }
    }

    pub fn level(mut self, level: Level) -> Self {
        self.level = level;
        self
    }

    pub fn buttons(mut self, buttons: Buttons) -> Self {
        self.buttons = buttons;
        self
    }

    pub fn show(self, then: impl FnOnce(Answer) + 'static) {
        imp::message(self, Box::new(then));
    }

    /// A box with OK alone, for an error; nothing waits for it.
    pub fn error(description: impl Into<String>) {
        Self::new(description).level(Level::Error).show(|_| {});
    }
}

/// Puts `text` on the system clipboard; false when that failed.
pub fn copy_text(text: &str) -> bool {
    imp::copy_text(text)
}

/// Where the settings file lives.
pub fn settings_path() -> Option<PathBuf> {
    imp::settings_dir().map(|dir| dir.join("settings.txt"))
}

/// A folder for files the app makes for itself and deletes again.
pub fn temp_dir() -> PathBuf {
    imp::temp_dir()
}

/// The app wrote `path` (a save, a tool's output): on Android, copies it to the document it stands
/// for. The desktop has nothing to do.
pub fn written(path: &Path) {
    imp::written(path);
}

/// Closes the app once nothing is left unsaved: the desktop ends the event loop; Android finishes
/// the activity, as its Back key does when the window does not take it.
pub fn close_app() {
    imp::close_app();
}

/// After the event loop: lets go of what the platform held (the desktop's clipboard).
pub fn shutdown() {
    imp::shutdown();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_catch_all_filter_only_counts_alone() {
        let open = FileDialog::new()
            .add_filter("PDF documents", &["pdf", "PDF"])
            .add_filter("All files", &["*"]);
        assert_eq!(open.mime_types(), ["application/pdf"]);
        let any = FileDialog::new().add_filter("All files", &["*"]);
        assert_eq!(any.mime_types(), ["*/*"]);
        let images = FileDialog::new().add_filter("Images", &["png", "jpg", "JPEG", "PNG"]);
        assert_eq!(images.mime_types(), ["image/png", "image/jpeg"]);
    }
}
