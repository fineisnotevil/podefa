// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Plan 0002 Phase 0 measurement: what reading every page size at open costs (ignored by default;
//! not part of CI).
//!
//! The engine answers `EngineEvent::Opened` with every page's bounds, and `all_page_sizes` gets
//! them through `fz_load_page`, which also loads the page's annotations and links and scans its
//! resources for transparency (`mupdf-sys` `source/pdf/pdf-page.c`, `pdf_load_page_imp`). A
//! continuous layout needs the sizes, not the rest, so this harness times two ways of getting them:
//!
//! - **load**: `MupdfEngine::all_page_sizes`, i.e. one `fz_load_page` + `fz_bound_page` per page;
//! - **object**: the page object from the page tree (`pdf_lookup_page_obj`), its crop box read with
//!   the same inheritance and clamping rules MuPDF applies, transformed by MuPDF's own page CTM
//!   (`pdf_page_obj_transform`) - no page load at all.
//!
//! Both must agree on every page; the harness asserts that before it reports a time, so the faster
//! path is only reported if it is also exact.
//!
//! The documents are synthetic and generated here, so nothing large is committed: a two-level page
//! tree whose root carries the inherited `MediaBox`, with landscape, rotated and cropped pages mixed
//! in, and optionally one link annotation per page. That is a *best case* for the page tree (flat,
//! uncompressed, a correct xref); a real-world file can be measured with `FA_PDF_BENCH_PDF`.
//!
//! Environment knobs:
//! - `FA_PDF_BENCH_PAGES`: comma-separated page counts (default `1000,5000,20000`).
//! - `FA_PDF_BENCH_PDF`: also measure this file.
//!
//! Run: `cargo test --release -p engine-mupdf --test page_sizes_bench -- --ignored --nocapture`

use engine_mupdf::MupdfEngine;
use mupdf::pdf::{PdfDocument, PdfObject};
use mupdf::{Document, Rect};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Kids per intermediate `Pages` node.
const FAN_OUT: usize = 64;

/// A synthetic document of `pages` pages: Letter by default (inherited from the root), with every
/// 7th page A4 landscape, every 11th rotated by 90 and every 13th cropped, so the sizes are mixed.
fn synthetic_pdf(pages: usize, annots: bool) -> Vec<u8> {
    let nodes = pages.div_ceil(FAN_OUT);
    // 1 catalog, 2 root, 3 content, 4.. intermediate nodes, then the pages.
    let first_node = 4;
    let first_page = first_node + nodes;
    let mut objects: Vec<String> = Vec::with_capacity(3 + nodes + pages);
    objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());

    let mut kids = String::new();
    for n in 0..nodes {
        let _ = write!(kids, "{} 0 R ", first_node + n);
    }
    objects.push(format!(
        "<< /Type /Pages /Kids [{kids}] /Count {pages} /MediaBox [0 0 612 792] /Resources << >> >>"
    ));
    let content = "0 0 1 rg 72 72 200 200 re f";
    objects.push(format!(
        "<< /Length {} >>\nstream\n{content}\nendstream",
        content.len()
    ));

    for n in 0..nodes {
        let start = n * FAN_OUT;
        let end = (start + FAN_OUT).min(pages);
        let mut kids = String::new();
        for p in start..end {
            let _ = write!(kids, "{} 0 R ", first_page + p);
        }
        objects.push(format!(
            "<< /Type /Pages /Parent 2 0 R /Kids [{kids}] /Count {} >>",
            end - start
        ));
    }

    for p in 0..pages {
        let mut page = format!(
            "<< /Type /Page /Parent {} 0 R /Contents 3 0 R",
            first_node + p / FAN_OUT
        );
        if p % 7 == 3 {
            page.push_str(" /MediaBox [0 0 842 595]");
        }
        if p % 11 == 5 {
            page.push_str(" /Rotate 90");
        }
        if p % 13 == 6 {
            page.push_str(" /CropBox [36 36 576 756]");
        }
        if annots {
            page.push_str(
                " /Annots [<< /Type /Annot /Subtype /Link /Rect [72 72 144 90] /Border [0 0 0] \
                 /A << /S /URI /URI (https://example.org) >> >>]",
            );
        }
        page.push_str(" >>");
        objects.push(page);
    }

    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    let mut table = format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
    for offset in offsets {
        let _ = writeln!(table, "{offset:010} 00000 n ");
    }
    let _ = write!(
        table,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    );
    out.extend_from_slice(table.as_bytes());
    out
}

/// `pdf_to_rect` on an optional array: normalized, and empty when the entry is missing or short.
fn to_rect(obj: Option<PdfObject>) -> Rect {
    let Some(obj) = obj else {
        return Rect::new(0.0, 0.0, 0.0, 0.0);
    };
    if !obj.is_array().unwrap_or(false) || obj.len().unwrap_or(0) < 4 {
        return Rect::new(0.0, 0.0, 0.0, 0.0);
    }
    let v = |i| {
        obj.get_array(i)
            .ok()
            .flatten()
            .and_then(|o| o.as_float().ok())
            .unwrap_or(0.0)
    };
    let (a, b, c, d) = (v(0), v(1), v(2), v(3));
    Rect::new(a.min(c), b.min(d), a.max(c), b.max(d))
}

fn is_empty(r: &Rect) -> bool {
    r.x0 >= r.x1 || r.y0 >= r.y1
}

/// Page bounds from the page object alone: `pdf_page_obj_transform_box`'s choice of box for
/// `FZ_CROP_BOX` (crop box clipped to the media box, Letter when empty, the unit rect when under
/// 1 pt), transformed by MuPDF's own page CTM - the same thing `fz_bound_page` returns.
fn object_bounds(page: &PdfObject) -> Rect {
    let media = to_rect(page.get_dict_inheritable("MediaBox").ok().flatten());
    let crop = page.get_dict_inheritable("CropBox").ok().flatten();
    let mut used = match crop {
        Some(crop) => media.intersect(&to_rect(Some(crop))),
        None => media,
    };
    if is_empty(&used) {
        used = Rect::new(0.0, 0.0, 612.0, 792.0);
    }
    if used.x1 - used.x0 < 1.0 || used.y1 - used.y0 < 1.0 {
        used = Rect::new(0.0, 0.0, 1.0, 1.0);
    }
    let ctm = page.page_ctm().expect("page ctm");
    used.transform(&ctm)
}

fn measure(path: &Path, label: &str) {
    // Each path gets a document of its own, so neither is served by the other's object cache.
    let t = Instant::now();
    let mut engine = MupdfEngine::new();
    let count = engine
        .open_document_with_password(path, None)
        .expect("open for load path");
    let open_ms = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    let loaded = engine.all_page_sizes().expect("all_page_sizes");
    let load_ms = t.elapsed().as_secs_f64() * 1e3;

    let doc = Document::open(path.to_str().expect("utf-8 path")).expect("open for object path");
    let pdf = PdfDocument::try_from(doc).expect("a PDF");
    let t = Instant::now();
    let mut sizes = Vec::with_capacity(count);
    for i in 0..count {
        let page = pdf.find_page(i as i32).expect("find_page");
        sizes.push(object_bounds(&page));
    }
    let object_ms = t.elapsed().as_secs_f64() * 1e3;

    let mut mismatches = 0;
    for (i, (a, b)) in loaded.iter().zip(&sizes).enumerate() {
        let same = (a.x - b.x0).abs() < 1e-3
            && (a.y - b.y0).abs() < 1e-3
            && (a.width - (b.x1 - b.x0)).abs() < 1e-3
            && (a.height - (b.y1 - b.y0)).abs() < 1e-3;
        if !same {
            if mismatches < 5 {
                eprintln!("  page {i}: load {a:?} vs object {b:?}");
            }
            mismatches += 1;
        }
    }
    println!(
        "{label:<28} pages={count:>6} open={open_ms:>8.1} ms  load={load_ms:>9.1} ms ({:>6.2} us/page)  \
         object={object_ms:>8.1} ms ({:>6.2} us/page)  mismatches={mismatches}",
        load_ms * 1e3 / count.max(1) as f64,
        object_ms * 1e3 / count.max(1) as f64,
    );
    assert_eq!(loaded.len(), count, "all_page_sizes dropped pages");
    assert_eq!(mismatches, 0, "object bounds disagree with fz_bound_page");
}

#[test]
#[ignore = "plan 0002 phase 0 measurement; run explicitly with --release --ignored --nocapture"]
fn bench_page_sizes_at_open() {
    let counts: Vec<usize> = std::env::var("FA_PDF_BENCH_PAGES")
        .unwrap_or_else(|_| "1000,5000,20000".into())
        .split(',')
        .map(|s| s.trim().parse().expect("invalid FA_PDF_BENCH_PAGES entry"))
        .collect();
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    for &pages in &counts {
        for annots in [false, true] {
            let path = dir.join(format!("synthetic_{pages}p_annots_{annots}.pdf"));
            std::fs::write(&path, synthetic_pdf(pages, annots)).expect("write fixture");
            let label = format!("synthetic {pages}p annots={annots}");
            measure(&path, &label);
        }
    }
    if let Ok(path) = std::env::var("FA_PDF_BENCH_PDF") {
        let path = PathBuf::from(path);
        let label = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        measure(&path, &label);
    }
}
