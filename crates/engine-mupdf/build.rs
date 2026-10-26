// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! On Android, links the NDK's C++ runtime statically into this crate's test binaries: MuPDF's
//! C++ parts need it, and a test binary pushed to the phone has no `libc++_shared.so` beside it
//! (Plan 0008, §3.3; `docs/backlog.md` item 9). They go at the end of the link line, after
//! MuPDF's archives; `crates/android/build.rs` does the same for the app's library.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        println!("cargo:rustc-link-arg=-lc++_static");
        println!("cargo:rustc-link-arg=-lc++abi");
    }
}
