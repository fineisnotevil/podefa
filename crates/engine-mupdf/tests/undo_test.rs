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

/// The page an undo reported changing.
fn undone_page(change: Option<Change>) -> Option<u32> {
    match change? {
        Change::Page { page, .. } => Some(page),
        Change::Pages { .. } => None,
    }
}
