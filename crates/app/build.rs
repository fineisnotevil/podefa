// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Compiles the window's markup in `ui/` (Plan 0007 Phase 1); `main.rs` includes the result with
//! `slint::include_modules!()`.

fn main() {
    if let Err(err) = slint_build::compile("ui/main.slint") {
        // A build script reports through the build's own error output; there is no caller to
        // return a typed error to.
        eprintln!("slint compile of ui/main.slint failed: {err}");
        std::process::exit(1);
    }
}
