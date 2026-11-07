// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Links the NDK's C++ runtime statically into `libpodefa.so`: Skia and MuPDF's C++ parts need
//! it, and the APK carries no `libc++_shared.so` (Plan 0008, §3.3). The flags go at the end of
//! the link line, after the archives that need them.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        println!("cargo:rustc-link-arg-cdylib=-lc++_static");
        println!("cargo:rustc-link-arg-cdylib=-lc++abi");
    }
}
