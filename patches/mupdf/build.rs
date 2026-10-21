// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Compiles the undo-journal wrappers (`journal/journal.c`, ADR 0008) the way mupdf-sys compiles
//! its own (`mupdf-sys-0.8.0/build.rs`, `build_wrapper`: a plain `cc::Build` with the MuPDF
//! headers and no extra defines). Whether the vendored headers are the linked library's version is
//! checked at run time by MuPDF itself (`fa_pdf_journal_headers_match`).

fn main() {
    println!("cargo:rerun-if-changed=journal");

    cc::Build::new()
        .file("journal/journal.c")
        .include("journal/include")
        .warnings(false)
        .compile("fa-pdf-journal");
}
