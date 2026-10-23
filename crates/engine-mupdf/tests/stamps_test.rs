// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Watermarks, page numbers and crop through the engine (Plan 0005 Phase 3, row AD): each is one
//! undo step, the stamped text can be extracted from every page (Turkish letters included) and sits
//! where the page shows it whatever the page's rotation, and a crop changes the page size the
//! layout sees.

use engine_mupdf::{Change, MupdfEngine, MupdfError};
use mupdf::{Document, TextPageFlags};
use pdf_core::annot::{Colour, EditOp};
use pdf_core::pages::{PageOp, PageOpError};
use pdf_core::stamp::{Corner, Stamp};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// `pages` Letter pages saying "page N", their font and resources inherited from the page tree.
fn text_pdf(pages: usize) -> Vec<u8> {
    let mut objects: Vec<String> = Vec::new();
    objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
    let kids: String = (0..pages).map(|p| format!("{} 0 R ", 4 + 2 * p)).collect();
    objects.push(format!(
        "<< /Type /Pages /Kids [{kids}] /Count {pages} \
         /Resources << /Font << /F1 3 0 R >> >> >>"
    ));
    objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());
    for p in 0..pages {
        // An unbalanced matrix change, as some producers leave: the stamp must not inherit it.
        let content = format!("2 0 0 2 0 0 cm BT /F1 5 Tf 27 375 Td (page {p}) Tj ET");
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {} 0 R >>",
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

/// A fixture file of its own for each test, so tests running at once never share one.
fn fixture(name: &str, pages: usize) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::write(&path, text_pdf(pages)).expect("write fixture");
    path
}

fn open(path: &Path) -> MupdfEngine {
    let mut engine = MupdfEngine::new();
    engine
        .open_document_with_password(path, None)
        .expect("open");
    engine
}

const GREY: Colour = Colour::new(0.5, 0.5, 0.5);

fn watermark(text: &str) -> Stamp {
    Stamp::Watermark {
        text: text.to_owned(),
        colour: GREY,
        opacity: 0.3,
        diagonal: true,
    }
}

fn numbers(corner: Corner) -> Stamp {
    Stamp::PageNumbers {
        format: "Sayfa {n} / {total}".to_owned(),
        first: 1,
        corner,
        size: 10.0,
        colour: Colour::new(0.0, 0.0, 0.0),
    }
}

fn all(count: u32) -> Vec<u32> {
    (0..count).collect()
}

/// The document as it is now, unsaved changes included.
fn snapshot(engine: &MupdfEngine) -> Document {
    let bytes = engine.snapshot().expect("snapshot");
    Document::from_bytes(&bytes, "application/pdf").expect("snapshot opens")
}

fn text(doc: &Document, page: i32) -> String {
    doc.load_page(page)
        .expect("page")
        .to_text_page(TextPageFlags::empty())
        .expect("text")
        .to_text()
        .expect("text")
}

fn sizes(change: Change) -> Vec<(f32, f32)> {
    match change {
        Change::Pages { page_sizes, .. } => {
            page_sizes.iter().map(|s| (s.width, s.height)).collect()
        }
        other => panic!("a page edit reported {other:?}"),
    }
}

#[test]
fn a_watermark_and_page_numbers_are_text_on_every_page_and_one_step_each() {
    const PAGES: u32 = 1000;
    let path = fixture("stamps_text.pdf", PAGES as usize);
    let mut engine = open(&path);
    let mark = "TASLAK ĞÜŞİÖÇ ğüşıöç";
    let t = Instant::now();
    engine
        .edit(&EditOp::Pages(PageOp::Stamp {
            pages: all(PAGES),
            stamp: watermark(mark),
        }))
        .expect("watermark");
    let watermark_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    engine
        .edit(&EditOp::Pages(PageOp::Stamp {
            pages: all(PAGES),
            stamp: numbers(Corner::BottomCentre),
        }))
        .expect("page numbers");
    let numbers_ms = t.elapsed().as_secs_f64() * 1000.0;
    let per_page = (watermark_ms + numbers_ms) / f64::from(2 * PAGES);
    println!(
        "[stamps] watermark {watermark_ms:.1} ms, page numbers {numbers_ms:.1} ms on {PAGES} \
         pages: {per_page:.3} ms per page and stamp"
    );
    // Row AD's bound, for the optimised build the bench uses. On 1000 pages it also catches a
    // cost that grows faster than the page count: stamping through mupdf-rs' `insert_contents`
    // took 14 ms per page here (benchmarks §11.6).
    if !cfg!(debug_assertions) {
        assert!(per_page <= 1.0, "{per_page:.3} ms per page");
    }

    let doc = snapshot(&engine);
    for page in 0..PAGES as i32 {
        let text = text(&doc, page);
        assert!(text.contains(mark), "page {page}: {text:?}");
        assert!(
            text.contains(&format!("Sayfa {} / {PAGES}", page + 1)),
            "page {page}: {text:?}"
        );
        assert!(
            text.contains(&format!("page {page}")),
            "the page's own text"
        );
    }
    // The page's own content is wrapped in q ... Q before each stamp, so the matrix it leaves
    // changed does not reach the stamp. MuPDF would forgive the unwrapped case (a Q with nothing to
    // restore goes back to the page's first state), stricter readers do not; so the structure is
    // checked: the content streams are q, q, the page's own, then the two stamps, each from Q.
    let pdf =
        mupdf::pdf::PdfDocument::from_bytes(&engine.snapshot().expect("snapshot")).expect("pdf");
    let contents = pdf
        .load_pdf_page(0)
        .expect("page")
        .object()
        .get_dict("Contents")
        .expect("dict")
        .expect("contents");
    let streams: Vec<String> = (0..contents.len().expect("len") as i32)
        .map(|i| {
            let stream = contents.get_array(i).expect("entry").expect("entry");
            String::from_utf8_lossy(&stream.read_stream().expect("stream")).into_owned()
        })
        .collect();
    assert_eq!(streams.len(), 5, "{streams:?}");
    assert_eq!(streams[0].trim(), "q");
    assert_eq!(streams[1].trim(), "q");
    assert!(streams[2].contains("(page 0)"));
    assert!(streams[3].starts_with("Q\nq ") && streams[4].starts_with("Q\nq "));

    // One step each: the first undo takes the numbers, the second the watermark.
    engine.undo().expect("undo").expect("a step");
    let doc = snapshot(&engine);
    assert!(text(&doc, 3).contains(mark) && !text(&doc, 3).contains("Sayfa"));
    engine.undo().expect("undo").expect("a step");
    assert!(!text(&snapshot(&engine), 3).contains(mark));
    assert_eq!(engine.undo().expect("nothing left"), None);
    engine.redo().expect("redo").expect("a step");
    engine.redo().expect("redo").expect("a step");

    // Saved in place and opened again.
    engine.save(None).expect("save in place");
    drop(engine);
    let doc = Document::open(path.to_str().expect("utf-8")).expect("reopen");
    let last = text(&doc, PAGES as i32 - 1);
    assert!(last.contains(mark) && last.contains(&format!("Sayfa {PAGES} / {PAGES}")));
}

#[test]
fn page_numbers_sit_where_the_page_shows_them_whatever_its_rotation() {
    let path = fixture("stamps_rotated.pdf", 4);
    let mut engine = open(&path);
    engine
        .edit(&EditOp::Pages(PageOp::Rotate {
            pages: vec![1, 2, 3],
            quarter_turns: 1,
        }))
        .expect("rotate");
    engine
        .edit(&EditOp::Pages(PageOp::Rotate {
            pages: vec![2, 3],
            quarter_turns: 1,
        }))
        .expect("rotate");
    engine
        .edit(&EditOp::Pages(PageOp::Rotate {
            pages: vec![3],
            quarter_turns: 1,
        }))
        .expect("rotate");
    engine
        .edit(&EditOp::Pages(PageOp::Stamp {
            pages: all(4),
            stamp: numbers(Corner::BottomRight),
        }))
        .expect("page numbers");
    let doc = snapshot(&engine);
    for page in 0..4 {
        let p = doc.load_page(page).expect("page");
        let bounds = p.bounds().expect("bounds");
        let (w, h) = (bounds.x1 - bounds.x0, bounds.y1 - bounds.y0);
        let hits = p
            .search(&format!("Sayfa {} / 4", page + 1), 4)
            .expect("search");
        let quad = hits.iter().next().expect("the number is found");
        // Upright on the page as shown: left to right, top above bottom.
        assert!(quad.ur.x > quad.ul.x && (quad.ur.y - quad.ul.y).abs() < 0.5);
        assert!(quad.ll.y > quad.ul.y);
        // In the bottom right corner, inside the margin.
        assert!(
            quad.lr.x <= w - 23.0 && quad.lr.x >= w - 25.0,
            "page {page}: {quad:?} on {w}x{h}"
        );
        assert!(
            quad.ll.y <= h - 20.0 && quad.ll.y >= h - 30.0,
            "page {page}: {quad:?} on {w}x{h}"
        );
    }
}

#[test]
fn a_crop_changes_the_page_size_and_undoes() {
    let path = fixture("stamps_crop.pdf", 3);
    let mut engine = open(&path);
    engine
        .edit(&EditOp::Pages(PageOp::Rotate {
            pages: vec![1],
            quarter_turns: 1,
        }))
        .expect("rotate");
    // 200 x 340 points of what each page shows, from 50 points in and 60 down.
    let rect = [50.0, 60.0, 250.0, 400.0];
    let cropped = sizes(
        engine
            .edit(&EditOp::Pages(PageOp::Crop {
                pages: vec![0, 1],
                rect,
            }))
            .expect("crop"),
    );
    assert_eq!(cropped[0], (200.0, 340.0));
    assert_eq!(cropped[1], (200.0, 340.0), "the turned page keeps its turn");
    assert_eq!(cropped[2], (612.0, 792.0));

    // The page shows what was at that place: the turned page's crop is where its visible corner
    // was, not where the unturned page had it.
    let doc = snapshot(&engine);
    let turned = doc.load_page(1).expect("page");
    let pdf =
        mupdf::pdf::PdfDocument::from_bytes(&engine.snapshot().expect("snapshot")).expect("pdf");
    let crop_box = pdf
        .load_pdf_page(1)
        .expect("page")
        .object()
        .get_dict("CropBox")
        .expect("dict")
        .expect("a crop box");
    let values: Vec<f32> = (0..4)
        .map(|i| {
            crop_box
                .get_array(i)
                .expect("entry")
                .expect("entry")
                .as_float()
                .expect("number")
        })
        .collect();
    // Turned 90 degrees clockwise, the visible top left is the unrotated bottom left: visible x
    // runs up the page from y = 0, visible y runs right from x = 0.
    assert_eq!(values, [60.0, 50.0, 400.0, 250.0], "{values:?}");
    assert_eq!(turned.bounds().expect("bounds").x1, 200.0);

    // A crop off the page is refused and adds no step.
    let outside = engine.edit(&EditOp::Pages(PageOp::Crop {
        pages: vec![2],
        rect: [700.0, 0.0, 800.0, 100.0],
    }));
    assert!(matches!(
        outside,
        Err(MupdfError::PageOp(PageOpError::CropOutside { page: 2 }))
    ));

    let back = sizes(engine.undo().expect("undo").expect("a step"));
    assert_eq!(back[0], (612.0, 792.0));
    assert_eq!(back[1], (792.0, 612.0));
}

#[test]
fn a_letter_the_font_lacks_is_refused() {
    let path = fixture("stamps_glyph.pdf", 2);
    let mut engine = open(&path);
    let refused = engine.edit(&EditOp::Pages(PageOp::Stamp {
        pages: all(2),
        stamp: watermark("草稿"),
    }));
    assert!(
        matches!(
            refused,
            Err(MupdfError::PageOp(PageOpError::MissingGlyph('草')))
        ),
        "{refused:?}"
    );
    assert_eq!(engine.history_state(), (false, false), "no step was added");
    assert!(!text(&snapshot(&engine), 0).contains('草'));
}
