// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Plan 0004 Phase 0 measurement: what annotating and saving cost, and whether an open file can be
//! saved in place (ignored by default; not part of CI).
//!
//! The questions v0.2 has to answer before its design:
//!
//! - **render**: the viewer builds display lists from the page *contents* only
//!   (`to_display_list(false)`), so annotations already in a document are not drawn. What does a
//!   display list with annotations cost on pages that have none, and on pages that have some?
//! - **create**: what creating a highlight, an ink stroke and a note costs, appearance included,
//!   and whether the next display list shows it;
//! - **save**: whether an incremental save to the *same path* works while that document - and a
//!   second `Document` on the same file, as the search thread holds - are open, whether the second
//!   one can still read afterwards, and what an incremental and a full save cost;
//! - **replace**: whether a file that is open can be replaced by a rename on this platform.
//!
//! The document is generated: `FA_PDF_BENCH_PAGES` (default 1000) Letter pages of 50 lines of
//! Helvetica text.
//!
//! Run: `cargo test --release -p engine-mupdf --test annot_bench -- --ignored --nocapture`

use mupdf::color::AnnotationColor;
use mupdf::pdf::{PdfDocument, PdfPage, PdfWriteOptions};
use mupdf::{Colorspace, Document, Matrix, Point, Quad, Rect};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn text_pdf(pages: usize) -> Vec<u8> {
    let mut objects: Vec<String> = Vec::new();
    objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
    let kids: String = (0..pages).map(|p| format!("{} 0 R ", 4 + 2 * p)).collect();
    objects.push(format!(
        "<< /Type /Pages /Kids [{kids}] /Count {pages} /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 3 0 R >> >> >>"
    ));
    objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());
    for p in 0..pages {
        let mut content = String::from("BT /F1 10 Tf 12 TL 54 750 Td\n");
        for line in 0..50 {
            let _ = writeln!(
                content,
                "(page {p} line {line} lorem ipsum dolor sit amet) Tj T*"
            );
        }
        content.push_str("ET");
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /Contents {} 0 R >>",
            5 + 2 * p
        ));
        objects.push(format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ));
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

fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1e3
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("fa-pdf-annot-bench");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir.join(name)
}

fn page(doc: &PdfDocument, i: i32) -> PdfPage {
    PdfPage::try_from(doc.load_page(i).expect("load page")).expect("pdf page")
}

/// Mean display-list build time over the first `n` pages, contents only and with annotations.
fn list_cost(doc: &Document, n: i32) -> (f64, f64) {
    let (mut contents, mut full) = (0.0, 0.0);
    for i in 0..n {
        let page = doc.load_page(i).expect("load page");
        let t = Instant::now();
        let _ = page.to_display_list(false).expect("contents list");
        contents += ms(t);
        let t = Instant::now();
        let _ = page.to_display_list(true).expect("full list");
        full += ms(t);
    }
    (contents / n as f64, full / n as f64)
}

/// RGB of one point of page 0 rendered at 100% from a display list.
fn pixel(doc: &Document, annots: bool, x: i32, y: i32) -> [u8; 3] {
    let page = doc.load_page(0).expect("load page");
    let list = page.to_display_list(annots).expect("list");
    let pix = list
        .to_pixmap(&Matrix::IDENTITY, &Colorspace::device_rgb(), false)
        .expect("pixmap");
    let stride = pix.stride() as usize;
    let at = y as usize * stride + x as usize * 3;
    let s = pix.samples();
    [s[at], s[at + 1], s[at + 2]]
}

fn annotation_count(path: &Path, page_index: i32) -> usize {
    let doc = PdfDocument::open(path.to_str().expect("utf-8")).expect("reopen");
    page(&doc, page_index).annotations().count()
}

#[test]
#[ignore = "measurement; run explicitly"]
fn annotate_and_save() {
    let pages: usize = std::env::var("FA_PDF_BENCH_PAGES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000);
    let path = scratch("annotate.pdf");
    let bytes = text_pdf(pages);
    std::fs::write(&path, &bytes).expect("write fixture");
    let p = path.to_str().expect("utf-8");
    println!("document: {pages} pages, {} bytes", bytes.len());

    // Render cost without annotations: the switch to `to_display_list(true)` on plain pages.
    {
        let doc = Document::open(p).expect("open");
        let _ = list_cost(&doc, 5); // first-touch warm-up
        let (contents, full) = list_cost(&doc, 200);
        println!(
            "list, no annots, mean of 200 pages: contents {contents:.3} ms, with annots {full:.3} ms"
        );
    }

    // A second document on the same file, as the search thread keeps one (ADR 0005).
    let reader = Document::open(p).expect("second document");
    let _ = reader.load_page(0).expect("reader page 0");

    let doc = PdfDocument::open(p).expect("open pdf");
    println!(
        "fresh: has_unsaved_changes {}, can_be_saved_incrementally {}",
        doc.has_unsaved_changes(),
        doc.can_be_saved_incrementally()
    );

    // Line 0 of page 0 sits at baseline y = 750 (PDF space), i.e. ~42 px from the top at 100%.
    let line = Rect::new(54.0, 792.0 - 760.0, 300.0, 792.0 - 746.0);
    let before = pixel(&doc, true, 40, 37);
    let mut first = page(&doc, 0);
    let t = Instant::now();
    let mut hl = first
        .add_highlight_annotation([Quad::from(line)])
        .expect("highlight");
    hl.set_color(AnnotationColor::Rgb {
        red: 1.0,
        green: 0.9,
        blue: 0.0,
    })
    .expect("color");
    hl.update().expect("update");
    let create_hl = ms(t);

    let t = Instant::now();
    let stroke: Vec<Point> = (0..200)
        .map(|i| Point::new(100.0 + i as f32, 400.0 + 20.0 * (i as f32 / 10.0).sin()))
        .collect();
    let mut ink = first.add_ink_annotation([stroke]).expect("ink");
    ink.set_color(AnnotationColor::Rgb {
        red: 0.0,
        green: 0.0,
        blue: 1.0,
    })
    .expect("color");
    ink.update().expect("update");
    let create_ink = ms(t);

    let t = Instant::now();
    let mut note = first
        .add_text_annotation(Rect::new(500.0, 100.0, 520.0, 120.0), "a note")
        .expect("note");
    note.update().expect("update");
    let create_note = ms(t);
    println!(
        "create + appearance: highlight {create_hl:.3} ms, ink (200 points) {create_ink:.3} ms, note {create_note:.3} ms"
    );

    let hl_px = (60, 792 - 753);
    let contents_px = pixel(&doc, false, hl_px.0, hl_px.1);
    let full_px = pixel(&doc, true, hl_px.0, hl_px.1);
    println!(
        "pixel under the highlight: contents-only {contents_px:?}, with annots {full_px:?} (blank margin before {before:?})"
    );
    {
        let page0 = doc.load_page(0).expect("page 0");
        let t = Instant::now();
        let _ = page0.to_display_list(true).expect("list");
        println!("list of page 0 with its 3 annotations: {:.3} ms", ms(t));
    }
    println!(
        "after edit: has_unsaved_changes {}",
        doc.has_unsaved_changes()
    );

    // Incremental save to the same path while this document and the reader are open.
    let size_before = std::fs::metadata(&path).expect("meta").len();
    let mut incremental = PdfWriteOptions::default();
    incremental.set_incremental(true);
    let t = Instant::now();
    let saved = doc.save_with_options(p, incremental);
    let save_ms = ms(t);
    let size_after = std::fs::metadata(&path).expect("meta").len();
    let on_disk = std::fs::read(&path).expect("read back");
    let prefix_kept = on_disk.len() >= bytes.len() && on_disk[..bytes.len()] == bytes[..];
    println!(
        "incremental save in place: {:?} in {save_ms:.1} ms; {size_before} -> {size_after} bytes; original bytes kept as prefix: {prefix_kept}",
        saved.as_ref().map(|_| "ok").map_err(|e| e.to_string())
    );
    println!(
        "after save: has_unsaved_changes {}",
        doc.has_unsaved_changes()
    );
    println!(
        "reopened: page 0 has {} annotations",
        annotation_count(&path, 0)
    );

    // Can the two open documents still read after the file changed under them?
    let reader_ok = reader
        .load_page((pages - 1) as i32)
        .and_then(|pg| pg.to_display_list(false))
        .is_ok();
    let doc_ok = doc
        .load_page((pages - 1) as i32)
        .and_then(|pg| pg.to_display_list(true))
        .is_ok();
    println!("after save, last page readable: reader {reader_ok}, editing document {doc_ok}");

    // A second edit and a second incremental save on the same open document.
    let mut second = page(&doc, 1);
    let mut hl2 = second
        .add_highlight_annotation([Quad::from(line)])
        .expect("highlight 2");
    hl2.update().expect("update");
    let t = Instant::now();
    let saved2 = doc.save_with_options(p, incremental);
    println!(
        "second incremental save: {:?} in {:.1} ms; reopened page 1 has {} annotations; size {} bytes",
        saved2.as_ref().map(|_| "ok").map_err(|e| e.to_string()),
        ms(t),
        annotation_count(&path, 1),
        std::fs::metadata(&path).expect("meta").len()
    );

    // A full (non-incremental) save, to another path: its cost for "Save As".
    let copy = scratch("annotate-full.pdf");
    let t = Instant::now();
    let full = doc.save(copy.to_str().expect("utf-8"));
    println!(
        "full save to another path: {:?} in {:.1} ms, {} bytes",
        full.as_ref().map(|_| "ok").map_err(|e| e.to_string()),
        ms(t),
        std::fs::metadata(&copy).map(|m| m.len()).unwrap_or(0)
    );

    // Replacing an open file by rename (write-then-rename, the usual atomic save).
    let renamed = std::fs::rename(&copy, &path);
    println!(
        "rename over the open file: {:?}",
        renamed.map_err(|e| e.to_string())
    );
    drop(reader);
    drop(second);
    drop(first);
    drop(doc);
}

/// Workers replaying display lists while the owning thread annotates the same document - the
/// shape v0.2 would have (pool renders, actor edits). Every render of a page that is not edited
/// must stay pixel-identical, and nothing may crash.
#[test]
#[ignore = "measurement; run explicitly"]
fn edits_while_workers_render() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let path = scratch("concurrent.pdf");
    std::fs::write(&path, text_pdf(60)).expect("write fixture");
    let doc = PdfDocument::open(path.to_str().expect("utf-8")).expect("open");

    // Pages 0-29 are rendered by the workers and never edited; 30-59 are edited.
    let lists: Vec<Arc<mupdf::DisplayList>> = (0..30)
        .map(|i| {
            Arc::new(
                doc.load_page(i)
                    .and_then(|p| p.to_display_list(true))
                    .expect("list"),
            )
        })
        .collect();
    let render = |list: &mupdf::DisplayList| -> Vec<u8> {
        list.to_pixmap(&Matrix::IDENTITY, &Colorspace::device_rgb(), false)
            .expect("pixmap")
            .samples()
            .to_vec()
    };
    let reference: Arc<Vec<Vec<u8>>> = Arc::new(lists.iter().map(|l| render(l)).collect());

    let stop = Arc::new(AtomicBool::new(false));
    let renders = Arc::new(AtomicUsize::new(0));
    let mismatches = Arc::new(AtomicUsize::new(0));
    let workers: Vec<_> = (0..3)
        .map(|w| {
            let (lists, reference, stop, renders, mismatches) = (
                lists.clone(),
                reference.clone(),
                stop.clone(),
                renders.clone(),
                mismatches.clone(),
            );
            std::thread::spawn(move || {
                let mut i = w;
                while !stop.load(Ordering::Relaxed) {
                    let k = i % lists.len();
                    let pix = lists[k]
                        .to_pixmap(&Matrix::IDENTITY, &Colorspace::device_rgb(), false)
                        .expect("pixmap");
                    if pix.samples() != reference[k].as_slice() {
                        mismatches.fetch_add(1, Ordering::Relaxed);
                    }
                    renders.fetch_add(1, Ordering::Relaxed);
                    i += 3;
                }
            })
        })
        .collect();

    let t = Instant::now();
    let mut edits = 0;
    for round in 0..20 {
        for i in 30..60 {
            let mut pg = page(&doc, i);
            let y = 40.0 + (round as f32) * 30.0;
            let mut hl = pg
                .add_highlight_annotation([Quad::from(Rect::new(54.0, y, 300.0, y + 12.0))])
                .expect("highlight");
            hl.update().expect("update");
            let _ = pg.to_display_list(true).expect("edited list");
            edits += 1;
        }
    }
    let edit_ms = ms(t);
    stop.store(true, Ordering::Relaxed);
    for w in workers {
        w.join().expect("worker");
    }
    println!(
        "{edits} highlight edits + list rebuilds in {edit_ms:.1} ms ({:.3} ms each) while 3 workers rendered {} untouched pages; mismatches: {}",
        edit_ms / edits as f64,
        renders.load(Ordering::Relaxed),
        mismatches.load(Ordering::Relaxed)
    );
    assert_eq!(mismatches.load(Ordering::Relaxed), 0);
}
