// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Annotations a document already has are drawn (Plan 0004 §3.1): in tiles, in the full-page
//! render and in thumbnails. Until this fix the engine drew the page's content stream alone, so a
//! highlight made by another reader was invisible; every assertion below fails with that behaviour.

use engine_mupdf::MupdfEngine;
use mupdf::color::AnnotationColor;
use mupdf::pdf::{PdfDocument, PdfPage};
use mupdf::{Quad, Rect};
use pdf_core::{PageGeometry, PdfEngine};
use std::path::{Path, PathBuf};

/// Where the highlight goes, in page points: the top-left margin of `large_200p.pdf`'s first page,
/// which is blank paper, so the highlight's colour shows unmixed.
const MARK: (f32, f32, f32, f32) = (20.0, 10.0, 120.0, 30.0);

/// `large_200p.pdf` with one red highlight on page 0, written once into the test's scratch dir.
fn annotated_fixture() -> PathBuf {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("large_200p.pdf");
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("annotated_200p.pdf");
    let doc = PdfDocument::open(source.to_str().expect("utf-8")).expect("open fixture");
    let mut page = PdfPage::try_from(doc.load_page(0).expect("page 0")).expect("pdf page");
    let (x0, y0, x1, y1) = MARK;
    let mut highlight = page
        .add_highlight_annotation([Quad::from(Rect::new(x0, y0, x1, y1))])
        .expect("highlight");
    highlight
        .set_color(AnnotationColor::Rgb {
            red: 1.0,
            green: 0.0,
            blue: 0.0,
        })
        .expect("colour");
    highlight.update().expect("appearance");
    doc.save(out.to_str().expect("utf-8")).expect("save");
    out
}

fn open(path: &Path) -> MupdfEngine {
    let mut engine = MupdfEngine::new();
    engine
        .open_document_with_password(path, None)
        .expect("open annotated copy");
    engine
}

/// RGB of the pixel at device position (`x`, `y`) of an RGB8 bitmap.
fn rgb(bitmap: &pdf_core::Bitmap, x: u32, y: u32) -> [u8; 3] {
    let at = y as usize * bitmap.stride + x as usize * 3;
    [bitmap.data[at], bitmap.data[at + 1], bitmap.data[at + 2]]
}

fn is_red(px: [u8; 3]) -> bool {
    px[0] > 200 && px[1] < 80 && px[2] < 80
}

#[test]
fn an_existing_highlight_is_drawn_in_tiles_full_page_and_thumbnails() {
    let path = annotated_fixture();
    let mut engine = open(&path);

    // The middle of the highlight, at 100%.
    let (x0, y0, x1, y1) = MARK;
    let (cx, cy) = (((x0 + x1) / 2.0) as u32, ((y0 + y1) / 2.0) as u32);

    let bounds = engine.page_size(0).expect("page size");
    let geometry = PageGeometry::new(0, bounds, 1000);
    let tile = engine.render_tile(&geometry, 0, 0).expect("tile");
    assert!(is_red(rgb(&tile, cx, cy)), "tile: {:?}", rgb(&tile, cx, cy));

    let page = engine.render_page(0, 1.0).expect("full page");
    assert!(is_red(rgb(&page, cx, cy)), "page: {:?}", rgb(&page, cx, cy));

    // A thumbnail 153 px wide is a quarter of the 612 pt page.
    let thumb = engine.render_thumb(0, 153).expect("thumbnail");
    let scale = thumb.width as f32 / bounds.width;
    let (tx, ty) = ((cx as f32 * scale) as u32, (cy as f32 * scale) as u32);
    assert!(
        is_red(rgb(&thumb, tx, ty)),
        "thumb: {:?}",
        rgb(&thumb, tx, ty)
    );

    // A page without annotations is unaffected: paper stays white where the highlight is on page 0.
    let geometry = PageGeometry::new(1, engine.page_size(1).expect("size"), 1000);
    let plain = engine.render_tile(&geometry, 0, 0).expect("tile of page 1");
    assert_eq!(rgb(&plain, cx, cy), [255, 255, 255]);
}
