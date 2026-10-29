// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! The Android app (Plan 0008 Phase 1, ADR 0012): `android_main`, which the activity calls on its
//! own thread, starts Slint's backend and the platform layer and runs the app.
//!
//! Android sends stdout and stderr nowhere, so they go to `out.txt` in the app's folder in shared
//! storage (`Android/data/org.fineassociation.podefa/files`), where adb can read them. The launch
//! intent's `args` extra holds command-line arguments, `{files}` standing for that folder: the
//! bench harness on the phone (`docs/benchmarks.md` §14).

#![cfg(target_os = "android")]

use std::os::fd::AsRawFd;
use std::path::Path;

use app::platform::android as platform;

#[unsafe(no_mangle)]
fn android_main(android: slint::android::AndroidApp) {
    if let Err(err) = slint::android::init(android.clone()) {
        eprintln!("podefa: Slint's Android backend did not start: {err}");
        return;
    }
    let opened = platform::init(&android);
    let folder = platform::external_dir().or_else(|| android.internal_data_path());
    if let Some(folder) = &folder {
        redirect_output(folder);
    }
    let mut args = vec!["podefa".to_owned()];
    if let Some(text) = platform::launch_args() {
        let folder = folder
            .as_deref()
            .map(Path::to_string_lossy)
            .unwrap_or_default();
        args.extend(
            text.split_whitespace()
                .map(|arg| arg.replace("{files}", &folder)),
        );
    }
    if let Some(opened) = opened {
        args.push(opened.to_string_lossy().into_owned());
    }
    if let Err(err) = app::run(args) {
        eprintln!("podefa: {err}");
    }
}

fn redirect_output(folder: &Path) {
    let Ok(file) = std::fs::File::create(folder.join("out.txt")) else {
        return;
    };
    // SAFETY: dup2 on descriptors this process owns; the file stays open for the process's life.
    unsafe {
        libc::dup2(file.as_raw_fd(), 1);
        libc::dup2(file.as_raw_fd(), 2);
    }
    std::mem::forget(file);
}
