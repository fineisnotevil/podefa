// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Tile rendering tests: geometry agreement, pixel identity against the full-page path, seams,
//! and the 6400% precision acceptance case (Plan 0001, §5 Phase 1).
//!
//! These are the tests that catch a wrong CTM, a wrong scissor or a missing bleed band, so they
//! deliberately compare tiles against independent renders rather than against themselves.

use engine_mupdf::MupdfEngine;
use pdf_core::{
    PageGeometry, PdfEngine, TILE_BLEED_PX, TILE_SIZE_PX, TILE_STRIDE_PX, scale_from_milli,
};
use std::path::{Path, PathBuf};

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Geometry for a page, taking the page bounds from the engine (so the test never guesses them).
fn geometry(engine: &MupdfEngine, page: u32, scale_milli: u32) -> PageGeometry {
    let bounds = engine.page_size(page as usize).expect("page size");
    PageGeometry::new(page, bounds, scale_milli)
}

/// MuPDF's own raster size and raster origin for a page, computed without `PageGeometry`.
fn mupdf_raster(path: &Path, page: u32, scale_milli: u32) -> (u32, u32, i32, i32) {
    let doc = mupdf::Document::open(path.to_str().expect("utf-8 fixture path")).expect("open");
    let mupdf_page = doc.load_page(page as i32).expect("load page");
    let s = scale_from_milli(scale_milli);
    let bbox = mupdf_page
        .bounds()
        .expect("bounds")
        .transform(&mupdf::Matrix::new_scale(s, s))
        .round();
    (
        (bbox.x1 - bbox.x0) as u32,
        (bbox.y1 - bbox.y0) as u32,
        bbox.x0,
        bbox.y0,
    )
}

/// Renders every tile of a page and blits each one into its place in a full-page buffer.
fn assemble_tiles(engine: &mut MupdfEngine, geo: &PageGeometry) -> Vec<u8> {
    let (dev_w, dev_h) = geo.device_size();
    let stride = dev_w as usize * 4;
    let mut canvas = vec![0u8; stride * dev_h as usize];
    for row in 0..geo.rows() {
        for col in 0..geo.cols() {
            let rect = geo.tile_rect(col, row).expect("cell intersects the page");
            let tile = engine.render_tile(geo, col, row).expect("render tile");
            assert_eq!(tile.width, rect.w);
            assert_eq!(tile.height, rect.h);
            assert_eq!(tile.stride, rect.row_bytes());
            let tile_stride = tile.stride;
            for r in 0..tile.height as usize {
                let src = r * tile_stride;
                let dst = (rect.y as usize + r) * stride + rect.x as usize * 4;
                canvas[dst..dst + tile_stride].copy_from_slice(&tile.data[src..src + tile_stride]);
            }
        }
    }
    canvas
}

/// Largest per-channel difference and how many bytes differ at all.
struct Diff {
    max: u8,
    differing_bytes: usize,
    total_bytes: usize,
}

impl Diff {
    fn differing_ratio(&self) -> f64 {
        self.differing_bytes as f64 / self.total_bytes as f64
    }
}

fn diff(a: &[u8], b: &[u8]) -> Diff {
    assert_eq!(a.len(), b.len(), "buffers must be comparable");
    let mut max = 0u8;
    let mut differing = 0usize;
    for (x, y) in a.iter().zip(b.iter()) {
        let d = x.abs_diff(*y);
        if d > 0 {
            differing += 1;
        }
        max = max.max(d);
    }
    Diff {
        max,
        differing_bytes: differing,
        total_bytes: a.len(),
    }
}

#[test]
fn tile_grid_agrees_with_mupdf_for_every_fixture_and_scale() {
    for (fixture, page, scales) in [
        ("minimal.pdf", 0u32, vec![1_000u32, 4_000, 64_000]),
        ("large_200p.pdf", 0, vec![1_000, 4_000, 64_000]),
        ("large_format_a0.pdf", 0, vec![1_000, 4_000, 64_000]),
    ] {
        let path = fixture_path(fixture);
        let mut engine = MupdfEngine::new();
        engine.open_document(&path, None).expect("open fixture");
        for scale_milli in scales {
            let geo = geometry(&engine, page, scale_milli);
            let (width, height, x0, y0) = mupdf_raster(&path, page, scale_milli);
            assert_eq!(
                geo.device_size(),
                (width, height),
                "{fixture} at {scale_milli} permille: grid and MuPDF disagree"
            );
            assert_eq!(
                geo.raster_origin(),
                (x0, y0),
                "{fixture} at {scale_milli} permille: raster origin mismatch"
            );
        }
    }
}

#[test]
fn tile_dimensions_cover_the_page_exactly() {
    let path = fixture_path("large_200p.pdf");
    let mut engine = MupdfEngine::new();
    engine.open_document(&path, None).expect("open fixture");
    let geo = geometry(&engine, 0, 1_000);

    // Letter at 100%: five columns, seven rows, and the last of each is clamped.
    assert_eq!(geo.device_size(), (612, 792));
    assert_eq!((geo.cols(), geo.rows()), (2, 2));

    let full = geo.tile_rect(0, 0).unwrap();
    assert_eq!((full.w, full.h), (TILE_SIZE_PX, TILE_SIZE_PX));
    let edge = geo.tile_rect(1, 1).unwrap();
    assert_eq!((edge.x, edge.y, edge.w, edge.h), (512, 512, 100, 280));

    let tile = engine.render_tile(&geo, 1, 1).expect("edge tile");
    assert_eq!((tile.width, tile.height), (100, 280));
    assert_eq!(tile.stride, 100 * 4);
    assert_eq!(tile.data.len(), 100 * 280 * 4);

    // Off-grid cells are refused rather than rasterized from a bogus origin.
    assert!(engine.render_tile(&geo, 3, 0).is_err());
}

#[test]
fn stale_engine_has_no_document() {
    let mut engine = MupdfEngine::new();
    let geo = PageGeometry::new(0, pdf_core::Rect::new(0.0, 0.0, 612.0, 792.0), 1_000);
    assert!(engine.render_tile(&geo, 0, 0).is_err());
}

/// A full page reassembled from its tiles must equal the full-page raster, byte for byte.
#[test]
fn tiles_reassemble_the_full_page_exactly() {
    for (fixture, scale_milli) in [("minimal.pdf", 1_000u32), ("large_200p.pdf", 1_000)] {
        let path = fixture_path(fixture);
        let mut engine = MupdfEngine::new();
        engine.open_document(&path, None).expect("open fixture");
        let geo = geometry(&engine, 0, scale_milli);

        let assembled = assemble_tiles(&mut engine, &geo);
        let full = engine
            .render_page(0, scale_from_milli(scale_milli))
            .expect("full page");
        assert_eq!((full.width, full.height), geo.device_size());

        let d = diff(&assembled, &full.data);
        assert_eq!(
            d.max, 0,
            "{fixture} at {scale_milli} permille: {} of {} bytes differ (max {})",
            d.differing_bytes, d.total_bytes, d.max
        );
    }
}

/// The seam test: at 400% every tile, blitted into place, matches the full-page raster to within
/// antialiasing noise. This is what catches a wrong CTM, a wrong scissor or a missing bleed.
#[test]
fn tiles_leave_no_seam_at_400_percent() {
    let path = fixture_path("large_200p.pdf");
    let mut engine = MupdfEngine::new();
    engine.open_document(&path, None).expect("open fixture");
    let geo = geometry(&engine, 0, 4_000);
    assert_eq!(geo.device_size(), (2_448, 3_168));
    assert_eq!((geo.cols(), geo.rows()), (5, 7));

    let assembled = assemble_tiles(&mut engine, &geo);
    let full = engine.render_page(0, 4.0).expect("full page");
    let d = diff(&assembled, &full.data);

    println!(
        "seam check 400%: max channel delta {}, differing bytes {} ({:.6}%)",
        d.max,
        d.differing_bytes,
        d.differing_ratio() * 100.0
    );
    assert!(
        d.max <= 1,
        "max channel delta {} exceeds antialiasing noise",
        d.max
    );
    assert!(
        d.differing_ratio() < 0.001,
        "{} of {} bytes differ",
        d.differing_bytes,
        d.total_bytes
    );
}

/// Renders a 2x2 block's region straight from the page into one padded pixmap.
///
/// This deliberately bypasses `render_tile` and the display list, so it is an independent
/// implementation of the same mapping: if the two disagree at 6400%, the tile path is wrong.
fn render_reference(
    path: &Path,
    page: u32,
    scale_milli: u32,
    col0: i32,
    row0: i32,
    side: usize,
) -> Vec<u8> {
    let doc = mupdf::Document::open(path.to_str().expect("utf-8 fixture path")).expect("open");
    let mupdf_page = doc.load_page(page as i32).expect("load page");
    let s = scale_from_milli(scale_milli);
    let bbox = mupdf_page
        .bounds()
        .expect("bounds")
        .transform(&mupdf::Matrix::new_scale(s, s))
        .round();
    let x0 = bbox.x0 + col0 * TILE_SIZE_PX as i32 - TILE_BLEED_PX;
    let y0 = bbox.y0 + row0 * TILE_SIZE_PX as i32 - TILE_BLEED_PX;

    let cs = mupdf::Colorspace::device_rgb();
    let mut pix = mupdf::Pixmap::new(&cs, 0, 0, side as i32, side as i32, true).expect("pixmap");
    pix.clear().expect("clear");
    {
        let device = mupdf::Device::from_pixmap(&pix).expect("draw device");
        let mut ctm = mupdf::Matrix::new_scale(s, s);
        ctm.concat(mupdf::Matrix::new_translate(-(x0 as f32), -(y0 as f32)));
        mupdf_page.run(&device, &ctm).expect("run page");
    }
    pix.samples().to_vec()
}

/// Compares one tile against its sub-region of the reference pixmap.
fn diff_region(reference: &[u8], side: usize, i: usize, j: usize, tile: &pdf_core::Bitmap) -> Diff {
    let ref_stride = side * 4;
    let x_off = (TILE_BLEED_PX as usize + i * TILE_SIZE_PX as usize) * 4;
    let y_off = TILE_BLEED_PX as usize + j * TILE_SIZE_PX as usize;
    let row_bytes = tile.width as usize * 4;
    let mut region = Vec::with_capacity(row_bytes * tile.height as usize);
    for r in 0..tile.height as usize {
        let src = (y_off + r) * ref_stride + x_off;
        region.extend_from_slice(&reference[src..src + row_bytes]);
    }
    diff(&region, &tile.data)
}

/// The approved acceptance case: A0 at 6400% renders, is deterministic, and shows no precision
/// jitter at a tile boundary.
#[test]
fn a0_at_6400_percent_is_deterministic_and_precise() {
    let path = fixture_path("large_format_a0.pdf");
    let mut engine = MupdfEngine::new();
    engine.open_document(&path, None).expect("open fixture");
    let geo = geometry(&engine, 0, 64_000);

    assert_eq!(geo.device_size(), (152_576, 215_680));
    assert_eq!((geo.cols(), geo.rows()), (298, 422));
    assert_eq!(TILE_STRIDE_PX, 516);

    // A 2x2 block straddling two tile boundaries near the middle of the page: the offsets here
    // are ~100 million pixels, which is where a float precision problem shows up first.
    let col0 = geo.cols() / 2 - 1;
    let row0 = geo.rows() / 2 - 1;

    // Deterministic: the same tile rendered twice is byte-identical.
    let first = engine.render_tile(&geo, col0, row0).expect("tile");
    let second = engine.render_tile(&geo, col0, row0).expect("tile again");
    assert_eq!(
        first.data, second.data,
        "tile rendering must be deterministic at 6400%"
    );
    assert_eq!((first.width, first.height), (TILE_SIZE_PX, TILE_SIZE_PX));
    assert!(
        first.data.chunks_exact(4).any(|px| px[3] != 0),
        "the tile must contain rendered content, otherwise this test proves nothing"
    );

    let reference_side = 2 * TILE_SIZE_PX as usize + 2 * TILE_BLEED_PX as usize;
    let reference = render_reference(&path, 0, 64_000, col0, row0, reference_side);

    let mut worst = 0u8;
    let mut differing = 0usize;
    let mut total = 0usize;
    for j in 0..2usize {
        for i in 0..2usize {
            let tile = engine
                .render_tile(&geo, col0 + i as i32, row0 + j as i32)
                .expect("tile");
            let d = diff_region(&reference, reference_side, i, j, &tile);
            worst = worst.max(d.max);
            differing += d.differing_bytes;
            total += d.total_bytes;
        }
    }

    println!(
        "A0 @ 6400%: max channel delta {worst}, differing bytes {differing} of {total} ({:.4}%)",
        differing as f64 / total as f64 * 100.0
    );
    // Two independent code paths through MuPDF at 6400%: identical to within antialiasing noise.
    // A failure here is the signal to add the per-page maximum virtual extent guard, not to
    // lower the zoom ceiling.
    assert!(
        worst <= 2,
        "precision jitter at 6400%: max channel delta {worst}"
    );
    assert!(
        differing as f64 <= total as f64 * 0.01,
        "{differing} of {total} bytes differ"
    );
}
