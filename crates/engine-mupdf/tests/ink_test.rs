// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Ink through the engine (Plan 0004 Phase 4, row V): a scripted 2000-point stroke is simplified
//! in `core`, committed as one `Ink` annotation, and its ink list read back from the saved file
//! equals the simplified points; colour, width, recolouring and undo work as for markup.

use engine_mupdf::MupdfEngine;
use mupdf::pdf::{PdfAnnotationType, PdfDocument, PdfPage};
use pdf_core::annot::{
    AnnotInfo, AnnotKind, EditOp, INK_SIMPLIFY_PT, INK_WIDTHS, PALETTE, simplify,
};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

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
        let content = format!("BT /F1 10 Tf 54 750 Td (page {p}) Tj ET");
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
    let mut offsets = Vec::new();
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

fn open(path: &Path) -> MupdfEngine {
    let mut engine = MupdfEngine::new();
    engine
        .open_document_with_password(path, None)
        .expect("open");
    engine
}

fn inks(engine: &MupdfEngine, page: u32) -> Vec<AnnotInfo> {
    engine
        .annotations(page)
        .expect("annotations")
        .into_iter()
        .filter(|a| a.kind == AnnotKind::Ink)
        .collect()
}

/// 2000 pointer samples of a hand-drawn wave across the page, with the sub-point jitter a real
/// pointer has.
fn drawn_stroke() -> Vec<(f32, f32)> {
    (0..2000)
        .map(|i| {
            let t = i as f32 / 1999.0;
            let jitter = if i % 3 == 0 { 0.15 } else { -0.1 };
            (72.0 + 460.0 * t, 400.0 + 80.0 * (t * 12.0).sin() + jitter)
        })
        .collect()
}

fn assert_points_eq(got: &[(f32, f32)], want: &[(f32, f32)]) {
    assert_eq!(got.len(), want.len(), "point count");
    for (g, w) in got.iter().zip(want) {
        assert!(
            (g.0 - w.0).abs() < 0.01 && (g.1 - w.1).abs() < 0.01,
            "{g:?} != {w:?}"
        );
    }
}

#[test]
fn a_drawn_stroke_is_saved_as_its_simplified_points() {
    let path = scratch("ink_round_trip.pdf");
    std::fs::write(&path, text_pdf(2)).expect("write fixture");
    let mut engine = open(&path);

    let drawn = drawn_stroke();
    let simple = simplify(&drawn, INK_SIMPLIFY_PT);
    assert!(
        simple.len() < drawn.len() / 4,
        "the simplification keeps {} of {} points",
        simple.len(),
        drawn.len()
    );
    engine
        .edit(&EditOp::AddInk {
            page: 1,
            strokes: vec![simple.clone()],
            colour: PALETTE[2],
            width: INK_WIDTHS[2],
            author: None,
        })
        .expect("add ink");
    let listed = inks(&engine, 1);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].strokes.len(), 1);
    assert_points_eq(&listed[0].strokes[0], &simple);
    // The bounds cover the stroke and its width.
    let rect = listed[0].rect;
    assert!(rect.x <= 72.0 && rect.x + rect.width >= 532.0, "{rect:?}");
    assert!(rect.y <= 320.5 && rect.y + rect.height >= 479.5, "{rect:?}");

    engine.save(None).expect("in-place save");
    let reopened = PdfDocument::open(path.to_str().expect("utf-8")).expect("reopen");
    let page = PdfPage::try_from(reopened.load_page(1).expect("page")).expect("pdf page");
    let annots: Vec<_> = page.annotations().collect();
    assert_eq!(annots.len(), 1);
    assert_eq!(annots[0].r#type().expect("type"), PdfAnnotationType::Ink);
    let strokes = annots[0].ink_list().expect("ink list");
    assert_eq!(strokes.len(), 1);
    let read: Vec<(f32, f32)> = strokes[0].iter().map(|p| (p.x, p.y)).collect();
    assert_points_eq(&read, &simple);
    let width = annots[0].border_width().expect("width");
    assert!((width - INK_WIDTHS[2]).abs() < 0.01, "width {width}");
}

#[test]
fn ink_is_recoloured_undone_and_deleted() {
    let path = scratch("ink_edit.pdf");
    std::fs::write(&path, text_pdf(1)).expect("write fixture");
    let mut engine = open(&path);
    let stroke = vec![(100.0, 100.0), (200.0, 150.0), (300.0, 100.0)];
    engine
        .edit(&EditOp::AddInk {
            page: 0,
            strokes: vec![stroke.clone()],
            colour: PALETTE[0],
            width: INK_WIDTHS[1],
            author: Some("Tester".to_owned()),
        })
        .expect("add");
    let id = inks(&engine, 0)[0].id;

    engine
        .edit(&EditOp::SetColour {
            page: 0,
            id,
            colour: PALETTE[5],
        })
        .expect("recolour");
    let colour = inks(&engine, 0)[0].colour.expect("colour");
    assert!((colour.r - PALETTE[5].r).abs() < 0.01 && (colour.g - PALETTE[5].g).abs() < 0.01);
    assert_points_eq(&inks(&engine, 0)[0].strokes[0], &stroke);

    engine.undo().expect("undo recolour");
    let colour = inks(&engine, 0)[0].colour.expect("colour");
    assert!((colour.g - PALETTE[0].g).abs() < 0.01);

    engine
        .edit(&EditOp::Delete { page: 0, id })
        .expect("delete");
    assert!(inks(&engine, 0).is_empty());
    engine.undo().expect("undo delete");
    assert_eq!(inks(&engine, 0).len(), 1);
    engine.undo().expect("undo add");
    assert!(inks(&engine, 0).is_empty());
    engine.redo().expect("redo add");
    assert_points_eq(&inks(&engine, 0)[0].strokes[0], &stroke);
}

/// An in-place save compresses the streams it adds (Plan 0005 Phase 1, §3.5): a long stroke's
/// appearance stream goes into the appended section deflated, and undo still works after it.
#[test]
fn an_in_place_save_compresses_new_streams_and_keeps_undo() {
    let path = scratch("ink_compressed.pdf");
    std::fs::write(&path, text_pdf(1)).expect("write fixture");
    let opened = std::fs::metadata(&path).expect("size").len() as usize;
    let mut engine = open(&path);
    let stroke = simplify(&drawn_stroke(), INK_SIMPLIFY_PT);
    engine
        .edit(&EditOp::AddInk {
            page: 0,
            strokes: vec![stroke],
            colour: PALETTE[0],
            width: INK_WIDTHS[1],
            author: None,
        })
        .expect("add ink");
    engine.save(None).expect("in-place save");
    let bytes = std::fs::read(&path).expect("read");
    let appended = &bytes[opened..];
    assert!(
        appended.windows(12).any(|w| w == b"/FlateDecode"),
        "the appended section has a deflated stream"
    );
    engine.undo().expect("undo after the save");
    assert!(inks(&engine, 0).is_empty(), "undo still restores the page");
}
