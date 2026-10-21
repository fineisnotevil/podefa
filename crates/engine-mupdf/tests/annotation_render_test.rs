// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Annotations a document already has are drawn (Plan 0004 §3.1, row P): in tiles, in the
//! full-page render, in thumbnails and in the base layer. Until this fix the engine drew the page's
//! content stream alone, so a highlight made by another reader was invisible; every assertion on an
//! annotated page below fails with that behaviour. Pages without annotations render exactly as
//! before.

use engine_mupdf::{EngineHandle, MupdfEngine};
use mupdf::color::AnnotationColor;
use mupdf::pdf::{PdfDocument, PdfPage};
use mupdf::{Colorspace, Document, Matrix, Point, Quad, Rect};
use pdf_core::{EngineCmd, EngineEvent, PageGeometry, PdfEngine, base_scale_milli};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Where the highlight goes, in page points: the top-left margin of `large_200p.pdf`'s first page,
/// which is blank paper, so the highlight's colour shows unmixed.
const MARK: (f32, f32, f32, f32) = (20.0, 10.0, 120.0, 30.0);
/// An ink stroke, a note and a stamp, each in its own region of page 0 (page points).
const INK: (f32, f32, f32, f32) = (200.0, 300.0, 400.0, 340.0);
const NOTE: (f32, f32, f32, f32) = (560.0, 60.0, 580.0, 80.0);
const STAMP: (f32, f32, f32, f32) = (100.0, 500.0, 300.0, 560.0);

/// `large_200p.pdf` with a red highlight, a blue ink stroke, a note and a stamp on page 0, written
/// into the test's scratch dir under `name` (one per test: the tests run in parallel).
fn annotated_fixture(name: &str) -> PathBuf {
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let source = fixture("large_200p.pdf");
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

    let (x0, y0, x1, y1) = INK;
    let stroke: Vec<Point> = (0..=20)
        .map(|i| {
            let t = i as f32 / 20.0;
            Point::new(
                x0 + (x1 - x0) * t,
                if i % 2 == 0 { y0 + 5.0 } else { y1 - 5.0 },
            )
        })
        .collect();
    let mut ink = page.add_ink_annotation([stroke]).expect("ink");
    ink.set_color(AnnotationColor::Rgb {
        red: 0.0,
        green: 0.0,
        blue: 1.0,
    })
    .expect("colour");
    ink.set_border_width(3.0).expect("width");
    ink.update().expect("appearance");

    let (x0, y0, x1, y1) = NOTE;
    let mut note = page
        .add_text_annotation(Rect::new(x0, y0, x1, y1), "a note")
        .expect("note");
    note.update().expect("appearance");

    let (x0, y0, x1, y1) = STAMP;
    let mut stamp = page
        .add_stamp_annotation(Rect::new(x0, y0, x1, y1), "Approved")
        .expect("stamp");
    stamp.update().expect("appearance");

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

/// Page `index` drawn from its content stream alone, as the viewer drew every page before the
/// fix: the reference an annotation must change and a page without one must equal.
fn contents_only(path: &Path, index: i32, scale: f32) -> (u32, usize, Vec<u8>) {
    let doc = Document::open(path.to_str().expect("utf-8")).expect("open");
    let page = doc.load_page(index).expect("page");
    let pix = page
        .to_pixmap(
            &Matrix::new_scale(scale, scale),
            &Colorspace::device_rgb(),
            false,
            false,
        )
        .expect("pixmap");
    (
        pix.width(),
        pix.stride().max(0) as usize,
        pix.samples().to_vec(),
    )
}

/// Whether any pixel of `region` (page points) differs between a bitmap at `scale` and the
/// contents-only reference at the same scale.
fn region_differs(
    bitmap: &pdf_core::Bitmap,
    reference: &(u32, usize, Vec<u8>),
    region: (f32, f32, f32, f32),
    scale: f32,
) -> bool {
    let (x0, y0, x1, y1) = region;
    let (rx0, ry0) = ((x0 * scale) as u32, (y0 * scale) as u32);
    let (rx1, ry1) = ((x1 * scale) as u32, (y1 * scale) as u32);
    (ry0..ry1).any(|y| {
        (rx0..rx1).any(|x| {
            let at = y as usize * reference.1 + x as usize * 3;
            rgb(bitmap, x, y) != [reference.2[at], reference.2[at + 1], reference.2[at + 2]]
        })
    })
}

#[test]
fn existing_annotations_are_drawn_in_tiles_full_page_and_thumbnails() {
    let path = annotated_fixture("annotated_200p_tiles.pdf");
    let mut engine = open(&path);
    let reference = contents_only(&path, 0, 1.0);

    // The middle of the highlight, at 100%.
    let (x0, y0, x1, y1) = MARK;
    let (cx, cy) = (((x0 + x1) / 2.0) as u32, ((y0 + y1) / 2.0) as u32);

    let bounds = engine.page_size(0).expect("page size");
    let geometry = PageGeometry::new(0, bounds, 1000);
    let tile = engine.render_tile(&geometry, 0, 0).expect("tile");
    assert!(is_red(rgb(&tile, cx, cy)), "tile: {:?}", rgb(&tile, cx, cy));

    let page = engine.render_page(0, 1.0).expect("full page");
    assert!(is_red(rgb(&page, cx, cy)), "page: {:?}", rgb(&page, cx, cy));
    for (name, region) in [("ink", INK), ("note", NOTE), ("stamp", STAMP)] {
        assert!(
            region_differs(&page, &reference, region, 1.0),
            "{name} is drawn"
        );
    }

    // A thumbnail 153 px wide is a quarter of the 612 pt page.
    let thumb = engine.render_thumb(0, 153).expect("thumbnail");
    let scale = thumb.width as f32 / bounds.width;
    let (tx, ty) = ((cx as f32 * scale) as u32, (cy as f32 * scale) as u32);
    assert!(
        is_red(rgb(&thumb, tx, ty)),
        "thumb: {:?}",
        rgb(&thumb, tx, ty)
    );

    // Page 1 has no annotations: paper stays white where the highlight is on page 0.
    let geometry = PageGeometry::new(1, engine.page_size(1).expect("size"), 1000);
    let plain = engine.render_tile(&geometry, 0, 0).expect("tile of page 1");
    assert_eq!(rgb(&plain, cx, cy), [255, 255, 255]);
}

#[test]
fn existing_annotations_are_drawn_in_the_base_layer() {
    let path = annotated_fixture("annotated_200p_base.pdf");
    let (handle, rx) = EngineHandle::spawn();
    handle
        .send(EngineCmd::Open {
            path: path.clone(),
            password: None,
        })
        .expect("actor is alive");
    let sizes = loop {
        match rx.recv_timeout(Duration::from_secs(60)).expect("answer") {
            EngineEvent::Opened { page_sizes, .. } => break page_sizes,
            EngineEvent::Error { message } => panic!("{message}"),
            _ => {}
        }
    };
    let scale_milli = base_scale_milli(sizes[0].width.max(sizes[0].height));
    handle
        .send(EngineCmd::RenderBase {
            page: 0,
            scale_milli,
            request_id: 1,
        })
        .expect("send");
    let bitmap = loop {
        match rx.recv_timeout(Duration::from_secs(60)).expect("answer") {
            EngineEvent::BaseRendered { bitmap, .. } => break bitmap,
            EngineEvent::Error { message } => panic!("{message}"),
            _ => {}
        }
    };
    let scale = scale_milli as f32 / 1000.0;
    let reference = contents_only(&path, 0, scale);
    for (name, region) in [("highlight", MARK), ("ink", INK), ("stamp", STAMP)] {
        assert!(
            region_differs(&bitmap, &reference, region, scale),
            "{name} is drawn in the base layer"
        );
    }
}

#[test]
fn pages_without_annotations_render_exactly_as_before() {
    // Every page of `large_200p.pdf`, which has no annotations, at 50%: drawn with annotations it
    // must be byte-identical to the contents-only render the viewer used until Plan 0004.
    let path = fixture("large_200p.pdf");
    let engine = open(&path);
    let pages = engine.page_count();
    assert_eq!(pages, 200);
    for index in 0..pages {
        let drawn = engine.render_page(index, 0.5).expect("render");
        let (width, stride, before) = contents_only(&path, index as i32, 0.5);
        assert_eq!((drawn.width, drawn.stride), (width, stride), "page {index}");
        assert!(drawn.data == before, "page {index} changed");
    }
}
