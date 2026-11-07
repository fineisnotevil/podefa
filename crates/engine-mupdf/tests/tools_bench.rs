// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Plan 0005 Phase 0 measurement: what the v0.3 page operations and everyday tools cost through
//! mupdf-rs 0.8, and whether they are one undoable journal step (ignored by default; not part of
//! CI).
//!
//! - **pages**: move, rotate, delete, copy and crop on a 200-page document, each undone;
//! - **merge / split**: two documents into one, and a page range out into a new file;
//! - **compress**: a lossless rewrite (garbage collection, stream compression) of a text document
//!   and of an image document;
//! - **export / import**: pages rendered to PNG at 150 dpi, and PNGs placed as pages of a new PDF;
//! - **stamp**: a watermark and a page number added to every page as content.
//!
//! The text document is generated: `FA_PDF_BENCH_PAGES` (default 200) Letter pages of 50 lines of
//! Helvetica. The image document is built from it: its first pages rendered at 150 dpi.
//!
//! Run: `cargo test --release -p engine-mupdf --test tools_bench -- --ignored --nocapture`

use mupdf::pdf::{
    InsertFontOptions, InsertImageOptions, InsertPdfOptions, InsertPosition, PageImageSource,
    PdfDocument, PdfWriteOptions,
};
use mupdf::{Colorspace, ImageFormat, Matrix, Rect, Size};
use std::fmt::Write as _;
use std::path::PathBuf;
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

fn scratch(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name)
}

fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

fn kib(bytes: u64) -> f64 {
    bytes as f64 / 1024.0
}

fn file_len(path: &PathBuf) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn open(bytes: &[u8]) -> PdfDocument {
    let mut doc = PdfDocument::from_bytes(bytes).expect("open");
    doc.enable_journal().expect("journal");
    doc
}

/// The text of a page's first line, which names the page it came from.
fn first_line(doc: &PdfDocument, page: i32) -> String {
    let page = doc.load_page(page).expect("page");
    let text = page
        .to_text_page(mupdf::TextPageFlags::empty())
        .expect("text page")
        .to_text()
        .expect("text");
    text.lines().next().unwrap_or_default().to_owned()
}

fn compressed() -> PdfWriteOptions {
    let mut options = PdfWriteOptions::default();
    options
        .set_garbage_level(4)
        .set_compress(true)
        .set_compress_images(true)
        .set_compress_fonts(true);
    options
}

#[test]
#[ignore = "measurement, run by hand"]
fn tools_bench() {
    let pages: usize = std::env::var("FA_PDF_BENCH_PAGES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);
    let bytes = text_pdf(pages);
    println!(
        "[tools] text document: {pages} pages, {:.1} KiB",
        kib(bytes.len() as u64)
    );

    // Pages: each operation, then its undo.
    let mut doc = open(&bytes);
    let t = Instant::now();
    doc.move_page(0, pages - 1).expect("move");
    let move_ms = ms(t);
    assert!(first_line(&doc, pages as i32 - 1).starts_with("page 0 "));
    let t = Instant::now();
    doc.undo().expect("undo move");
    let undo_ms = ms(t);
    assert!(first_line(&doc, 0).starts_with("page 0 "));
    println!("[tools] move page 1 to the end: {move_ms:.2} ms, undo {undo_ms:.2} ms");

    let t = Instant::now();
    doc.begin_operation("Rotate all").expect("begin");
    for p in 0..pages as i32 {
        doc.load_pdf_page(p)
            .expect("page")
            .set_rotation(90)
            .expect("rotate");
    }
    doc.end_operation().expect("end");
    let rotate_ms = ms(t);
    let t = Instant::now();
    doc.undo().expect("undo rotate");
    let undo_ms = ms(t);
    assert_eq!(
        doc.load_pdf_page(5).expect("page").rotation().expect("rot"),
        0
    );
    println!(
        "[tools] rotate all {pages} pages (one step): {rotate_ms:.2} ms, undo {undo_ms:.2} ms"
    );

    let t = Instant::now();
    doc.delete_pages(0..pages / 2).expect("delete");
    let delete_ms = ms(t);
    assert_eq!(doc.page_count().expect("count") as usize, pages - pages / 2);
    doc.undo().expect("undo delete");
    assert_eq!(doc.page_count().expect("count") as usize, pages);
    println!(
        "[tools] delete {} pages: {delete_ms:.2} ms, undone",
        pages / 2
    );

    let t = Instant::now();
    doc.copy_page(3, InsertPosition::After(3)).expect("copy");
    let copy_ms = ms(t);
    assert!(first_line(&doc, 4).starts_with("page 3 "));
    doc.undo().expect("undo copy");
    println!("[tools] duplicate a page: {copy_ms:.2} ms, undone");

    let t = Instant::now();
    doc.begin_operation("Crop all").expect("begin");
    for p in 0..pages as i32 {
        doc.load_pdf_page(p)
            .expect("page")
            .set_crop_box(Rect::new(36.0, 36.0, 576.0, 756.0))
            .expect("crop");
    }
    doc.end_operation().expect("end");
    let crop_ms = ms(t);
    let bounds = doc.load_page(0).expect("page").bounds().expect("bounds");
    doc.undo().expect("undo crop");
    println!(
        "[tools] crop all {pages} pages: {crop_ms:.2} ms (page now {:.0}x{:.0} pt), undone",
        bounds.x1 - bounds.x0,
        bounds.y1 - bounds.y0
    );

    // Merge and split.
    let other = PdfDocument::from_bytes(&bytes).expect("open other");
    let t = Instant::now();
    doc.insert_pdf(&other, InsertPdfOptions::default())
        .expect("merge");
    let merge_ms = ms(t);
    let merged = scratch("tools_merged.pdf");
    let t = Instant::now();
    doc.save_with_options(merged.to_str().expect("utf-8"), compressed())
        .expect("save merged");
    let merged_save_ms = ms(t);
    println!(
        "[tools] merge {pages}+{pages} pages: {merge_ms:.1} ms, full save {merged_save_ms:.1} ms, \
         {:.1} KiB",
        kib(file_len(&merged))
    );
    drop(doc);

    let t = Instant::now();
    let mut part = PdfDocument::new();
    part.insert_pdf(
        &other,
        InsertPdfOptions {
            source_pages: (10..20).into(),
            ..InsertPdfOptions::default()
        },
    )
    .expect("split");
    let split = scratch("tools_split.pdf");
    part.save_with_options(split.to_str().expect("utf-8"), compressed())
        .expect("save split");
    println!(
        "[tools] split pages 11-20 into a new file: {:.1} ms, {:.1} KiB",
        ms(t),
        kib(file_len(&split))
    );
    assert!(first_line(&part, 0).starts_with("page 10 "));

    // Compress, lossless.
    let plain = scratch("tools_plain.pdf");
    std::fs::write(&plain, &bytes).expect("write");
    let t = Instant::now();
    let packed = scratch("tools_packed.pdf");
    PdfDocument::from_bytes(&bytes)
        .expect("open")
        .save_with_options(packed.to_str().expect("utf-8"), compressed())
        .expect("compress");
    println!(
        "[tools] lossless compress, text: {:.1} -> {:.1} KiB in {:.1} ms",
        kib(file_len(&plain)),
        kib(file_len(&packed)),
        ms(t)
    );

    // Export: pages to PNG at 150 dpi.
    let images = 10.min(pages);
    let scale = 150.0 / 72.0;
    let mut pngs = Vec::new();
    let t = Instant::now();
    for p in 0..images as i32 {
        let page = other.load_page(p).expect("page");
        let pixmap = page
            .to_pixmap(
                &Matrix::new_scale(scale, scale),
                &Colorspace::device_rgb(),
                false,
                true,
            )
            .expect("render");
        let path = scratch(&format!("tools_page{p}.png"));
        pixmap
            .save_as(path.to_str().expect("utf-8"), ImageFormat::PNG)
            .expect("png");
        pngs.push(path);
    }
    let png_bytes: u64 = pngs.iter().map(file_len).sum();
    println!(
        "[tools] export {images} pages to PNG at 150 dpi: {:.1} ms per page, {:.1} KiB per page",
        ms(t) / images as f64,
        kib(png_bytes) / images as f64
    );

    // Import: the PNGs as pages of a new PDF, then that image document compressed.
    let t = Instant::now();
    let mut album = PdfDocument::new();
    for path in &pngs {
        let data = std::fs::read(path).expect("read png");
        let mut page = album.new_page(Size::new(612.0, 792.0)).expect("page");
        page.insert_image(
            &mut album,
            Rect::new(0.0, 0.0, 612.0, 792.0),
            PageImageSource::Bytes {
                data: &data,
                format_hint: Some("png"),
            },
            InsertImageOptions::default(),
        )
        .expect("image");
    }
    let album_path = scratch("tools_album.pdf");
    album
        .save(album_path.to_str().expect("utf-8"))
        .expect("save album");
    println!(
        "[tools] import {images} PNGs as pages: {:.1} ms, {:.1} KiB",
        ms(t),
        kib(file_len(&album_path))
    );
    let t = Instant::now();
    let album_packed = scratch("tools_album_packed.pdf");
    PdfDocument::open(album_path.to_str().expect("utf-8"))
        .expect("open album")
        .save_with_options(album_packed.to_str().expect("utf-8"), compressed())
        .expect("compress album");
    println!(
        "[tools] lossless compress, images: {:.1} -> {:.1} KiB in {:.1} ms",
        kib(file_len(&album_path)),
        kib(file_len(&album_packed)),
        ms(t)
    );

    // Stamp: a watermark and a page number on every page, one undo step.
    let mut doc = open(&bytes);
    let t = Instant::now();
    doc.begin_operation("Stamp").expect("begin");
    for p in 0..pages as i32 {
        let mut page = doc.load_pdf_page(p).expect("page");
        let (font, _, _) = page
            .insert_font(&mut doc, &InsertFontOptions::new("Helvetica"))
            .expect("font");
        // `insert_font` returns the resource name with its slash.
        let content = format!(
            "q 0.8 g BT {font} 72 Tf 0.7071 0.7071 -0.7071 0.7071 160 250 Tm (DRAFT) Tj ET Q\n\
             q 0 g BT {font} 10 Tf 296 30 Td ({} / {pages}) Tj ET Q",
            p + 1
        );
        page.insert_contents(&mut doc, content.as_bytes(), true)
            .expect("contents");
    }
    doc.end_operation().expect("end");
    let stamp_ms = ms(t);
    let page_text = |doc: &PdfDocument| {
        doc.load_page(1)
            .expect("page")
            .to_text_page(mupdf::TextPageFlags::empty())
            .expect("text")
            .to_text()
            .expect("text")
    };
    let text = page_text(&doc);
    assert!(text.contains("DRAFT") && text.contains(&format!("2 / {pages}")));
    doc.undo().expect("undo stamp");
    assert!(!page_text(&doc).contains("DRAFT"));
    doc.redo().expect("redo stamp");
    let stamped = scratch("tools_stamped.pdf");
    doc.save_with_options(stamped.to_str().expect("utf-8"), compressed())
        .expect("save stamped");
    println!(
        "[tools] watermark + page number on {pages} pages: {stamp_ms:.1} ms ({:.3} ms/page), \
         undone and redone, saved {:.1} KiB",
        stamp_ms / pages as f64,
        kib(file_len(&stamped))
    );
}

/// A full save with garbage collection rewrites the live document's objects, after which MuPDF's
/// journal still steps back but no longer restores them: undo reports success and changes nothing.
/// A full save without garbage collection, and an incremental one, leave the journal working. The
/// v0.3 tools that rewrite a file (compress) must therefore write a copy from a separate document,
/// never the open one. Investigation, 2026-10-22.
#[test]
#[ignore = "measurement, run by hand"]
fn a_garbage_collecting_save_breaks_undo() {
    let bytes = text_pdf(3);
    let stamped = |doc: &PdfDocument| {
        doc.load_page(1)
            .expect("p")
            .to_text_page(mupdf::TextPageFlags::empty())
            .expect("t")
            .to_text()
            .expect("t")
            .contains("STAMP")
    };
    let mut garbage = PdfWriteOptions::default();
    garbage.set_garbage_level(4);
    let mut incremental = PdfWriteOptions::default();
    incremental.set_incremental(true);
    for (name, options, undoes) in [
        ("garbage 4", garbage, false),
        (
            "compress, no garbage",
            {
                let mut o = PdfWriteOptions::default();
                o.set_compress(true);
                o
            },
            true,
        ),
        ("plain", PdfWriteOptions::default(), true),
        ("incremental", incremental, true),
    ] {
        let path = scratch("tools_journal_save.pdf");
        std::fs::write(&path, &bytes).expect("write");
        let mut doc = PdfDocument::open(path.to_str().expect("utf-8")).expect("open");
        doc.enable_journal().expect("journal");
        let mut page = doc.load_pdf_page(1).expect("page");
        page.insert_contents(&mut doc, b"BT /F1 20 Tf 100 100 Td (STAMP) Tj ET", true)
            .expect("contents");
        drop(page);
        let out = if name == "incremental" {
            path.clone()
        } else {
            scratch("tools_journal_copy.pdf")
        };
        doc.save_with_options(out.to_str().expect("utf-8"), options)
            .expect("save");
        doc.undo().expect("undo");
        let undone = !stamped(&doc);
        println!("[tools] save {name}, then undo: change undone = {undone}");
        assert_eq!(undone, undoes, "{name}");
    }
}
