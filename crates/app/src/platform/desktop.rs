// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! The desktop: the system's modal dialogs through `rfd`, the clipboard through `arboard`.

use std::cell::RefCell;
use std::env;
use std::path::{Path, PathBuf};

use super::{Answer, Buttons, FileDialog, Level, MessageDialog};

thread_local! {
    /// The one clipboard handle, created on the first copy. Kept here so `shutdown` can drop it
    /// explicitly after the event loop: on X11 that drop hands the copied text to the clipboard
    /// manager, and Rust does not promise to run the main thread's thread-local destructors.
    static CLIPBOARD: RefCell<Option<arboard::Clipboard>> = const { RefCell::new(None) };
}

fn dialog(spec: FileDialog) -> rfd::FileDialog {
    let mut dialog = rfd::FileDialog::new();
    if !spec.title.is_empty() {
        dialog = dialog.set_title(&spec.title);
    }
    for (name, extensions) in &spec.filters {
        dialog = dialog.add_filter(name, extensions);
    }
    if let Some(dir) = spec.directory {
        dialog = dialog.set_directory(dir);
    }
    if let Some(name) = spec.file_name {
        dialog = dialog.set_file_name(name);
    }
    dialog
}

pub fn pick_file(spec: FileDialog, then: Box<dyn FnOnce(Option<PathBuf>)>) {
    then(dialog(spec).pick_file());
}

pub fn pick_files(spec: FileDialog, then: Box<dyn FnOnce(Option<Vec<PathBuf>>)>) {
    then(dialog(spec).pick_files());
}

pub fn pick_folder(spec: FileDialog, then: Box<dyn FnOnce(Option<PathBuf>)>) {
    then(dialog(spec).pick_folder());
}

pub fn save_file(spec: FileDialog, then: Box<dyn FnOnce(Option<PathBuf>)>) {
    then(dialog(spec).save_file());
}

pub fn message(spec: MessageDialog, then: Box<dyn FnOnce(Answer)>) {
    let answer = rfd::MessageDialog::new()
        .set_level(match spec.level {
            Level::Info => rfd::MessageLevel::Info,
            Level::Warning => rfd::MessageLevel::Warning,
            Level::Error => rfd::MessageLevel::Error,
        })
        .set_title("PODEFA")
        .set_description(spec.description)
        .set_buttons(match spec.buttons {
            Buttons::Ok => rfd::MessageButtons::Ok,
            Buttons::YesNo => rfd::MessageButtons::YesNo,
            Buttons::YesNoCancel => rfd::MessageButtons::YesNoCancel,
        })
        .show();
    then(match answer {
        rfd::MessageDialogResult::Yes | rfd::MessageDialogResult::Ok => Answer::Yes,
        rfd::MessageDialogResult::No => Answer::No,
        _ => Answer::Cancel,
    });
}

pub fn copy_text(text: &str) -> bool {
    CLIPBOARD.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = arboard::Clipboard::new().ok();
        }
        slot.as_mut()
            .is_some_and(|clipboard| clipboard.set_text(text.to_owned()).is_ok())
    })
}

pub fn settings_dir() -> Option<PathBuf> {
    Some(if cfg!(windows) {
        PathBuf::from(env::var_os("APPDATA")?).join("PODEFA")
    } else if cfg!(target_os = "macos") {
        PathBuf::from(env::var_os("HOME")?).join("Library/Application Support/PODEFA")
    } else {
        match env::var_os("XDG_CONFIG_HOME").filter(|dir| !dir.is_empty()) {
            Some(dir) => PathBuf::from(dir).join("podefa"),
            None => PathBuf::from(env::var_os("HOME")?).join(".config/podefa"),
        }
    })
}

pub fn temp_dir() -> PathBuf {
    env::temp_dir()
}

pub fn written(_path: &Path) {}

pub fn close_app() {
    let _ = slint::quit_event_loop();
}

pub fn request_orientation(window: &slint::Window, landscape: Option<bool>) {
    let size = window.size().to_logical(window.scale_factor());
    let (mut long, mut short) = (size.width.max(size.height), size.width.min(size.height));
    if short <= 0.0 {
        // Not shown yet: the mobile shell's preferred size (`main.slint`).
        (long, short) = (832.0, 384.0);
    }
    let size = match landscape {
        Some(true) => slint::LogicalSize::new(long, short),
        Some(false) | None => slint::LogicalSize::new(short, long),
    };
    window.set_size(size);
}

pub fn shutdown() {
    let clipboard = CLIPBOARD.with(|cell| cell.borrow_mut().take());
    drop(clipboard);
}
