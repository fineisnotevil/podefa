// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

use engine_mupdf::{MupdfEngine, MupdfError};
use pdf_core::{PdfEngine, PixelFormat};
use std::path::PathBuf;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

#[test]
fn test_open_valid_document_and_page_count() {
    let mut engine = MupdfEngine::new();
    let path = fixture_path("minimal.pdf");
    let count = engine
        .open_document(&path, None)
        .expect("Failed to open valid minimal.pdf");

    assert_eq!(count, 1);
    assert_eq!(engine.page_count(), 1);
}

#[test]
fn test_render_dimensions_scale_1_and_2() {
    let mut engine = MupdfEngine::new();
    let path = fixture_path("minimal.pdf");
    engine.open_document(&path, None).unwrap();

    let size = engine.page_size(0).unwrap();
    assert_eq!(size.width, 100.0);
    assert_eq!(size.height, 100.0);

    // Render at scale 1.0 (expected 100x100)
    let bmp_1 = engine.render_page(0, 1.0).unwrap();
    assert_eq!(bmp_1.width, 100);
    assert_eq!(bmp_1.height, 100);
    assert_eq!(bmp_1.format, PixelFormat::Rgb8);
    assert_eq!(bmp_1.stride, 100 * 3);
    assert_eq!(bmp_1.data.len(), 100 * 100 * 3);

    // Render at scale 2.0 (expected 200x200)
    let bmp_2 = engine.render_page(0, 2.0).unwrap();
    assert_eq!(bmp_2.width, 200);
    assert_eq!(bmp_2.height, 200);
    assert_eq!(bmp_2.format, PixelFormat::Rgb8);
    assert_eq!(bmp_2.data.len(), 200 * 200 * 3);
}

#[test]
fn test_golden_image_pixel_tolerance() {
    let mut engine = MupdfEngine::new();
    let path = fixture_path("minimal.pdf");
    engine.open_document(&path, None).unwrap();

    let bmp = engine.render_page(0, 1.0).unwrap();

    let n = PixelFormat::Rgb8.bytes_per_pixel();
    let center_offset = (50 * bmp.stride) + (50 * n);
    let r = bmp.data[center_offset];
    let g = bmp.data[center_offset + 1];
    let b = bmp.data[center_offset + 2];

    assert!(r > 200, "Expected red > 200, got {r}");
    assert!(g < 50, "Expected green < 50, got {g}");
    assert!(b < 50, "Expected blue < 50, got {b}");

    let corner_offset = (2 * bmp.stride) + (2 * n);
    let cr = bmp.data[corner_offset];
    let cg = bmp.data[corner_offset + 1];
    let cb = bmp.data[corner_offset + 2];

    // The paper is white and there is no alpha channel to carry it: the raster is opaque by
    // construction, which is why the tiles dropped from RGBA8 to RGB8.
    assert!(cr > 240, "Expected corner white red > 240, got {cr}");
    assert!(cg > 240, "Expected corner white green > 240, got {cg}");
    assert!(cb > 240, "Expected corner white blue > 240, got {cb}");
}

#[test]
fn test_corrupt_file_error() {
    let mut engine = MupdfEngine::new();
    let path = fixture_path("corrupt.pdf");
    let res = engine.open_document(&path, None);

    assert!(res.is_err(), "Expected corrupt file to return an error");
    match res {
        Err(MupdfError::OpenFailed { .. }) => {}
        other => panic!("Expected OpenFailed, got: {:?}", other),
    }
}

#[test]
fn test_wrong_password_or_unencrypted() {
    let mut engine = MupdfEngine::new();
    let path = fixture_path("minimal.pdf");
    let res = engine.open_document(&path, Some("wrongpass"));
    assert!(res.is_ok());
}
