// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Sticky notes through the engine (Plan 0004 Phase 3, row U): placed with their text, edited,
//! moved, undone and deleted, and the text read back byte for byte from the saved file, non-ASCII
//! included.

use engine_mupdf::MupdfEngine;
use mupdf::pdf::{PdfDocument, PdfPage};
use pdf_core::annot::{AnnotInfo, AnnotKind, EditOp, NOTE_SIZE_PT, PALETTE, note_rect};
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

fn notes(engine: &MupdfEngine, page: u32) -> Vec<AnnotInfo> {
    engine
        .annotations(page)
        .expect("annotations")
        .into_iter()
        .filter(|a| a.kind == AnnotKind::Note)
        .collect()
}

/// Turkish, a dash, CJK, an emoji and a line break: text MuPDF has to store as UTF-16 in the PDF.
const TEXT: &str = "Öğrenci notu — çşığüÖ\n日本語のメモ ✓ 📌";

#[test]
fn a_note_keeps_its_text_byte_for_byte_through_save_and_reopen() {
    let path = scratch("note_round_trip.pdf");
    std::fs::write(&path, text_pdf(2)).expect("write fixture");
    let mut engine = open(&path);

    let rect = note_rect(100.0, 200.0, 612.0, 792.0);
    engine
        .edit(&EditOp::AddNote {
            page: 1,
            rect,
            contents: TEXT.to_owned(),
            colour: PALETTE[0],
            author: None,
        })
        .expect("add note");
    let placed = notes(&engine, 1);
    assert_eq!(placed.len(), 1);
    assert_eq!(placed[0].contents.as_deref(), Some(TEXT));
    assert!((placed[0].rect.x - rect.x).abs() < 0.5 && (placed[0].rect.y - rect.y).abs() < 0.5);
    assert!((placed[0].rect.width - NOTE_SIZE_PT).abs() < 0.5);

    engine.save(None).expect("in-place save");
    let reopened = PdfDocument::open(path.to_str().expect("utf-8")).expect("reopen");
    let page = PdfPage::try_from(reopened.load_page(1).expect("page")).expect("pdf page");
    let annots: Vec<_> = page.annotations().collect();
    assert_eq!(annots.len(), 1);
    let read = annots[0].contents().expect("contents").expect("some text");
    assert_eq!(read.as_bytes(), TEXT.as_bytes(), "byte for byte");
}

#[test]
fn a_note_is_edited_moved_undone_and_deleted() {
    let path = scratch("note_edit.pdf");
    std::fs::write(&path, text_pdf(1)).expect("write fixture");
    let mut engine = open(&path);
    let first = note_rect(50.0, 50.0, 612.0, 792.0);
    engine
        .edit(&EditOp::AddNote {
            page: 0,
            rect: first,
            contents: "first".to_owned(),
            colour: PALETTE[1],
            author: Some("Tester".to_owned()),
        })
        .expect("add");
    let id = notes(&engine, 0)[0].id;

    engine
        .edit(&EditOp::SetContents {
            page: 0,
            id,
            contents: "second ✓".to_owned(),
        })
        .expect("edit");
    assert_eq!(notes(&engine, 0)[0].contents.as_deref(), Some("second ✓"));

    let moved = note_rect(400.0, 600.0, 612.0, 792.0);
    engine
        .edit(&EditOp::MoveNote {
            page: 0,
            id,
            rect: moved,
        })
        .expect("move");
    let note = &notes(&engine, 0)[0];
    assert!((note.rect.x - moved.x).abs() < 0.5 && (note.rect.y - moved.y).abs() < 0.5);
    assert_eq!(
        note.contents.as_deref(),
        Some("second ✓"),
        "a move keeps the text"
    );

    // Undo the move, then the edit: each is one step, and the note keeps its id.
    engine.undo().expect("undo move");
    let note = &notes(&engine, 0)[0];
    assert_eq!(note.id, id);
    assert!((note.rect.x - first.x).abs() < 0.5 && (note.rect.y - first.y).abs() < 0.5);
    engine.undo().expect("undo edit");
    assert_eq!(notes(&engine, 0)[0].contents.as_deref(), Some("first"));

    engine
        .edit(&EditOp::Delete { page: 0, id })
        .expect("delete");
    assert!(notes(&engine, 0).is_empty());
    engine.undo().expect("undo delete");
    assert_eq!(notes(&engine, 0).len(), 1);
}

#[test]
fn an_empty_text_clears_a_comment() {
    let path = scratch("note_empty.pdf");
    std::fs::write(&path, text_pdf(1)).expect("write fixture");
    let mut engine = open(&path);
    engine
        .edit(&EditOp::AddNote {
            page: 0,
            rect: note_rect(10.0, 10.0, 612.0, 792.0),
            contents: "temporary".to_owned(),
            colour: PALETTE[0],
            author: None,
        })
        .expect("add");
    let id = notes(&engine, 0)[0].id;
    engine
        .edit(&EditOp::SetContents {
            page: 0,
            id,
            contents: String::new(),
        })
        .expect("clear");
    assert_eq!(notes(&engine, 0)[0].contents, None);
}
