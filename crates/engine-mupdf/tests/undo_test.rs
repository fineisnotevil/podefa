// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Undo and redo through MuPDF's journal (Plan 0004 Phase 2, row S; ADR 0008).
//!
//! 50 edits, chosen by a fixed pseudo-random sequence over a generated five-page text document that
//! already has two annotations: undoing all of them must give back the annotations the document
//! was opened with, redoing all of them the annotations after the last edit, and a save made
//! halfway back must write that halfway state.

use engine_mupdf::{Change, MupdfEngine};
use mupdf::pdf::{PdfDocument, PdfPage};
use pdf_core::Rect;
use pdf_core::annot::{AnnotInfo, Colour, EditOp, MarkupKind, PALETTE};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const PAGES: usize = 5;
const LINES: usize = 50;

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
        for l in 0..LINES {
            let _ = writeln!(content, "(p{p}l{l} alpha beta gamma delta) Tj T*");
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

/// Line `l` of a page as a markup box (baselines 12 pt apart from 42 pt below the top).
fn line_box(l: usize, from: f32, width: f32) -> Rect {
    Rect::new(from, 42.0 + 12.0 * l as f32 - 10.0, width, 12.0)
}

/// The generated document with a highlight on page 0 and an underline on page 2, written as a
/// full file (`source` is generated, `path` is what the test opens).
fn fixture(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let source = dir.join(format!("{name}_source.pdf"));
    let path = dir.join(format!("{name}.pdf"));
    std::fs::write(&source, text_pdf(PAGES)).expect("write source");
    let doc = PdfDocument::open(source.to_str().expect("utf-8")).expect("open source");
    for (page, quad_line, underline) in [(0, 3, false), (2, 7, true)] {
        let mut pdf_page = PdfPage::try_from(doc.load_page(page).expect("page")).expect("pdf");
        let r = line_box(quad_line, 54.0, 150.0);
        let quad = mupdf::Quad::from(mupdf::Rect::new(r.x, r.y, r.x + r.width, r.y + r.height));
        let mut annot = if underline {
            pdf_page
                .add_underline_annotation([quad])
                .expect("underline")
        } else {
            pdf_page
                .add_highlight_annotation([quad])
                .expect("highlight")
        };
        annot.update().expect("appearance");
    }
    doc.save(path.to_str().expect("utf-8"))
        .expect("save fixture");
    path
}

fn open(path: &Path) -> MupdfEngine {
    let mut engine = MupdfEngine::new();
    engine
        .open_document_with_password(path, None)
        .expect("open");
    engine
}

/// Every page's annotations, ids included: undo has to give back the very objects.
fn snapshot(engine: &MupdfEngine) -> Vec<Vec<AnnotInfo>> {
    (0..PAGES as u32)
        .map(|page| engine.annotations(page).expect("annotations"))
        .collect()
}

/// Like [`snapshot`], without the object numbers: a reopened file numbers its objects anew only
/// if MuPDF renumbers them, which the comparison must not depend on.
fn shape(pages: &[Vec<AnnotInfo>]) -> Vec<Vec<String>> {
    pages
        .iter()
        .map(|items| {
            items
                .iter()
                .map(|a| {
                    let quads: Vec<String> = a
                        .quads
                        .iter()
                        .map(|q| format!("{:.1},{:.1},{:.1},{:.1}", q.x, q.y, q.width, q.height))
                        .collect();
                    let colour = a
                        .colour
                        .map(|c| format!("{:.2},{:.2},{:.2}", c.r, c.g, c.b))
                        .unwrap_or_default();
                    format!("{:?} [{}] {colour}", a.kind, quads.join(" "))
                })
                .collect()
        })
        .collect()
}

/// A fixed linear congruential sequence: the same 50 edits on every run and platform.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) as usize) % bound
    }
}

/// One edit: mostly new markup (which may cut old markup of its kind), sometimes a recolour or a
/// deletion of an annotation the page has.
fn random_edit(engine: &MupdfEngine, rng: &mut Lcg) -> EditOp {
    let page = rng.next(PAGES) as u32;
    let items = engine.annotations(page).expect("annotations");
    let colour = PALETTE[rng.next(PALETTE.len())];
    match (rng.next(10), items.is_empty()) {
        (0..=1, false) => EditOp::Delete {
            page,
            id: items[rng.next(items.len())].id,
        },
        (2..=3, false) => EditOp::SetColour {
            page,
            id: items[rng.next(items.len())].id,
            colour,
        },
        _ => {
            let kind = [
                MarkupKind::Highlight,
                MarkupKind::Underline,
                MarkupKind::StrikeOut,
                MarkupKind::Squiggly,
            ][rng.next(4)];
            let line = rng.next(12);
            let from = 54.0 + rng.next(8) as f32 * 20.0;
            let width = 40.0 + rng.next(8) as f32 * 20.0;
            EditOp::AddMarkup {
                page,
                kind,
                rects: vec![line_box(line, from, width)],
                colour,
                author: None,
            }
        }
    }
}

#[test]
fn fifty_edits_undo_and_redo_exactly() {
    let path = fixture("undo_fifty");
    let mut engine = open(&path);
    let opened = snapshot(&engine);
    assert_eq!(engine.history_state(), (false, false));

    let mut rng = Lcg(0x5eed_0004);
    let mut states = vec![opened.clone()];
    for n in 0..50 {
        let op = random_edit(&engine, &mut rng);
        engine
            .edit(&op)
            .unwrap_or_else(|e| panic!("edit {n} {op:?}: {e}"));
        states.push(snapshot(&engine));
    }
    let last = states.last().cloned().expect("states");
    assert_ne!(last, opened, "the edits changed something");

    // Undo every edit, checking each step against the state it returns to.
    for step in (0..50).rev() {
        let changed = engine.undo().expect("undo");
        assert!(changed.is_some(), "step {step} undone");
        assert_eq!(
            snapshot(&engine),
            states[step],
            "after undoing back to {step}"
        );
    }
    assert_eq!(snapshot(&engine), opened);
    assert_eq!(engine.undo().expect("nothing left"), None);
    assert_eq!(engine.history_state(), (false, true));

    // Redo all of them.
    for (step, state) in states.iter().enumerate().skip(1) {
        assert!(engine.redo().expect("redo").is_some());
        assert_eq!(&snapshot(&engine), state, "after redoing to {step}");
    }
    assert_eq!(engine.redo().expect("nothing to redo"), None);
    assert_eq!(engine.history_state(), (true, false));
    assert_eq!(snapshot(&engine), last);
}

#[test]
fn a_save_after_undoing_writes_the_undone_state() {
    let path = fixture("undo_save");
    let mut engine = open(&path);
    let mut rng = Lcg(0x5eed_0005);
    let mut states = vec![snapshot(&engine)];
    for _ in 0..20 {
        let op = random_edit(&engine, &mut rng);
        engine.edit(&op).expect("edit");
        states.push(snapshot(&engine));
    }
    for _ in 0..8 {
        engine.undo().expect("undo");
    }
    assert_eq!(snapshot(&engine), states[12]);
    engine.save(None).expect("in-place save");

    // The file holds the undone state; the open document can still redo past it.
    let reopened = open(&path);
    assert_eq!(shape(&snapshot(&reopened)), shape(&states[12]));
    assert_eq!(engine.history_state(), (true, true));
    engine.redo().expect("redo");
    assert_eq!(snapshot(&engine), states[13]);
}

#[test]
fn a_new_edit_after_undoing_drops_the_redo_steps() {
    let path = fixture("undo_branch");
    let mut engine = open(&path);
    let mark = |page, line| EditOp::AddMarkup {
        page,
        kind: MarkupKind::Highlight,
        rects: vec![line_box(line, 54.0, 100.0)],
        colour: Colour::new(1.0, 0.0, 0.0),
        author: None,
    };
    engine.edit(&mark(1, 10)).expect("edit");
    engine.edit(&mark(3, 10)).expect("edit");
    // Undo reports the page each step changed, so the app redraws the right one.
    assert_eq!(undone_page(engine.undo().expect("undo")), Some(3));
    assert_eq!(engine.history_state(), (true, true));
    engine.edit(&mark(4, 20)).expect("edit on a new branch");
    assert_eq!(engine.history_state(), (true, false));
    assert_eq!(undone_page(engine.undo().expect("undo")), Some(4));
    assert_eq!(undone_page(engine.undo().expect("undo")), Some(1));
    assert_eq!(engine.undo().expect("start"), None);
    // Each change gave its page a new revision.
    assert!(engine.revision(1) >= 2 && engine.revision(4) >= 2);
}

#[test]
fn a_failed_edit_leaves_no_step() {
    let path = fixture("undo_failed");
    let mut engine = open(&path);
    let before = snapshot(&engine);
    let missing = EditOp::Delete {
        page: 1,
        id: 999_999,
    };
    assert!(engine.edit(&missing).is_err());
    assert_eq!(engine.history_state(), (false, false));
    assert_eq!(snapshot(&engine), before);
}

#[test]
fn the_journal_headers_are_the_linked_mupdf() {
    // journal/include must be the headers of the MuPDF mupdf-sys links; MuPDF checks it.
    assert!(mupdf::pdf::journal::journal_headers_match());
}

/// The `/Prev` of a PDF file's last trailer (or cross-reference stream), and the `startxref`
/// offset of its first section: the file's own table, which every incremental save must name.
fn last_prev_and_first_startxref(bytes: &[u8]) -> (Option<u64>, Option<u64>) {
    let text = String::from_utf8_lossy(bytes);
    let number_after = |at: usize, key: &str| -> Option<u64> {
        let rest = text[at + key.len()..].trim_start();
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    };
    let prev = text.rfind("/Prev").and_then(|at| number_after(at, "/Prev"));
    let first = text
        .find("startxref")
        .and_then(|at| number_after(at, "startxref"));
    (prev, first)
}

/// Saving in place several times in one session (Plan 0004 §3.3): each save rewrites one
/// cumulative section after the bytes the file had at open, and its `/Prev` must name the file's
/// own cross-reference section. MuPDF 1.27.2 named the previous save's section, which the save
/// had just overwritten: from the second save on, a reader lost pages (a maintainer's file opened
/// with none). After every save the file opens on its own, unrepaired, with every page and edit.
#[test]
fn saving_in_place_again_and_again_keeps_the_file_readable() {
    let path = fixture("undo_resave");
    let opened = std::fs::read(&path).expect("read");
    let (_, own) = last_prev_and_first_startxref(&opened);
    let mut engine = open(&path);
    let before = snapshot(&engine);
    for round in 0..3 {
        let mark = EditOp::AddMarkup {
            page: round,
            kind: MarkupKind::StrikeOut,
            rects: vec![line_box(12 + round as usize, 54.0, 90.0)],
            colour: PALETTE[round as usize],
            author: None,
        };
        engine.edit(&mark).expect("edit");
        engine.save(None).expect("in-place save");
        let bytes = std::fs::read(&path).expect("read");
        let (prev, _) = last_prev_and_first_startxref(&bytes);
        assert_eq!(
            prev, own,
            "save {round}: /Prev names the file's own section"
        );

        // Read on its own, the file needs no repair (MuPDF repairs a broken /Prev chain, and a
        // repaired document cannot be saved incrementally) and has every edit so far.
        let mut reader = open(&path);
        assert_eq!(snapshot(&reader).len(), PAGES, "save {round}: every page");
        assert_eq!(
            shape(&snapshot(&reader)),
            shape(&snapshot(&engine)),
            "save {round}: the edits so far"
        );
        assert!(
            reader.save(None).is_ok(),
            "save {round}: not a repaired file"
        );
    }
    // Undo still reaches the document as opened, across the saves.
    for _ in 0..3 {
        engine.undo().expect("undo");
    }
    assert_eq!(shape(&snapshot(&engine)), shape(&before));
}

/// The page an undo reported changing.
fn undone_page(change: Option<Change>) -> Option<u32> {
    match change? {
        Change::Page { page, .. } => Some(page),
        Change::Pages { .. } => None,
    }
}

/// A file whose page tree yields no page (the broken `/Prev` above did that to a real file) is
/// refused, so the app reports it and keeps the document it had open.
#[test]
fn a_document_without_readable_pages_is_refused() {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("undo_no_pages.pdf");
    std::fs::write(&path, text_pdf(0)).expect("write");
    let mut engine = MupdfEngine::new();
    assert!(matches!(
        engine.open_document_with_password(&path, None),
        Err(engine_mupdf::MupdfError::NoPages(_))
    ));
}
