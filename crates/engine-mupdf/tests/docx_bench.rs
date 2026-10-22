// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Plan 0005 Phase 0 measurement: PDF to Word through MuPDF's DOCX writer (ignored by default; not
//! part of CI). mupdf-rs' default features already compile the writer (`docx-output`).
//!
//! Converts the fixtures, a generated text document and, when `FA_PDF_DOCX_INPUT` names one, any
//! PDF, writing `<name>.docx` next to the test's scratch files and reporting the time and size. The
//! output is for a person to open in a word processor: whether it is good enough is their call.
//!
//! Run: `cargo test --release -p engine-mupdf --test docx_bench -- --ignored --nocapture`

use mupdf::{Document, DocumentWriter, Matrix};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn convert(input: &Path, out_dir: &Path) {
    let name = input.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
    let output = out_dir.join(format!("{name}.docx"));
    let start = Instant::now();
    let doc = Document::open(input.to_str().expect("utf-8")).expect("open");
    let pages = doc.page_count().expect("count");
    {
        let mut writer =
            DocumentWriter::new(output.to_str().expect("utf-8"), "docx", "").expect("writer");
        for p in 0..pages {
            let page = doc.load_page(p).expect("page");
            let bounds = page.bounds().expect("bounds");
            let device = writer.begin_page(bounds).expect("begin page");
            page.run(&device, &Matrix::IDENTITY).expect("run");
            writer.end_page(device).expect("end page");
        }
        // The writer assembles the file when it is closed (dropped).
    }
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    let in_len = std::fs::metadata(input).map(|m| m.len()).unwrap_or(0);
    let out_len = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
    println!(
        "[docx] {name}: {pages} pages, {:.1} KiB -> {:.1} KiB in {ms:.1} ms ({:.1} ms/page) -> {}",
        in_len as f64 / 1024.0,
        out_len as f64 / 1024.0,
        ms / pages.max(1) as f64,
        output.display()
    );
}

#[test]
#[ignore = "measurement, run by hand"]
fn docx_bench() {
    let out_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("docx");
    std::fs::create_dir_all(&out_dir).expect("out dir");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for name in ["minimal.pdf", "large_200p.pdf", "dense_vector.pdf"] {
        convert(&fixtures.join(name), &out_dir);
    }
    let tmp = Path::new(env!("CARGO_TARGET_TMPDIR"));
    for name in ["select_3p.pdf", "tools_merged.pdf"] {
        let path = tmp.join(name);
        if path.exists() {
            convert(&path, &out_dir);
        }
    }
    if let Some(input) = std::env::var_os("FA_PDF_DOCX_INPUT") {
        convert(Path::new(&input), &out_dir);
    }
}
