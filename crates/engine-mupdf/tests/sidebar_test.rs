// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Sidebar data from the engine: the flattened outline and uncached thumbnails (Plan 0003, §4.1
//! and §4.3).
//!
//! The outline document is generated here, so every destination kind and its expected position is
//! written down next to the bytes that produce it.

use engine_mupdf::MupdfEngine;
use pdf_core::{OutlineItem, PdfEngine};
use std::fmt::Write as _;
use std::path::PathBuf;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Three Letter pages and an outline:
///
/// - "Chapter 1" -> page 1, `/XYZ 0 692 0` (PDF y is up: 100 pt below the top);
///   - "Section 1.1" -> page 2, `/FitH 500` (292 pt below the top);
/// - "Chapter 2" -> page 3, `/Fit` (no position);
/// - "Website" -> a URI action (no page).
pub fn outline_pdf() -> Vec<u8> {
    let objects: Vec<String> = vec![
        // 1 catalog, 2 pages, 3-5 pages, 6 outlines root, 7-10 items
        "<< /Type /Catalog /Pages 2 0 R /Outlines 6 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 /MediaBox [0 0 612 792] >>".into(),
        "<< /Type /Page /Parent 2 0 R >>".into(),
        "<< /Type /Page /Parent 2 0 R >>".into(),
        "<< /Type /Page /Parent 2 0 R >>".into(),
        "<< /Type /Outlines /First 7 0 R /Last 10 0 R /Count 4 >>".into(),
        "<< /Title (Chapter 1) /Parent 6 0 R /Next 9 0 R /First 8 0 R /Last 8 0 R /Count 1 \
         /Dest [3 0 R /XYZ 0 692 0] >>"
            .into(),
        "<< /Title (Section 1.1) /Parent 7 0 R /Dest [4 0 R /FitH 500] >>".into(),
        "<< /Title (Chapter 2) /Parent 6 0 R /Prev 7 0 R /Next 10 0 R /Dest [5 0 R /Fit] >>".into(),
        "<< /Title (Website) /Parent 6 0 R /Prev 9 0 R \
         /A << /S /URI /URI (https://example.org/) >> >>"
            .into(),
    ];
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

fn open(path: &std::path::Path) -> MupdfEngine {
    let mut engine = MupdfEngine::new();
    engine.open_document(path, None).expect("open");
    engine
}

#[test]
fn the_outline_is_flattened_in_reading_order_with_page_space_positions() {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("outline.pdf");
    std::fs::write(&path, outline_pdf()).expect("write fixture");
    let items = open(&path).outline();
    let item = |depth, title: &str, page, top| OutlineItem {
        depth,
        title: title.to_owned(),
        page,
        top,
    };
    assert_eq!(
        items,
        vec![
            item(0, "Chapter 1", Some(0), Some(100.0)),
            item(1, "Section 1.1", Some(1), Some(292.0)),
            item(0, "Chapter 2", Some(2), None),
            item(0, "Website", None, None),
        ]
    );
}

#[test]
fn a_document_without_an_outline_gives_an_empty_list() {
    assert!(open(&fixture_path("large_200p.pdf")).outline().is_empty());
    assert!(MupdfEngine::new().outline().is_empty(), "no document open");
}

#[test]
fn a_thumbnail_fits_its_width_and_leaves_the_display_list_cache_alone() {
    let mut engine = open(&fixture_path("large_200p.pdf"));
    for page in [0, 1] {
        let geometry = pdf_core::PageGeometry::new(
            page,
            engine.page_size(page as usize).expect("size"),
            1_000,
        );
        engine.render_tile(&geometry, 0, 0).expect("tile");
    }
    let before = engine.display_list_pages();

    for page in 2..40 {
        let thumb = engine.render_thumb(page, 160).expect("thumbnail");
        // Letter at 261 permille: 160 x 207.
        assert_eq!((thumb.width, thumb.height), (160, 207), "page {page}");
        assert_eq!(thumb.data.len(), 160 * 207 * 3);
    }
    assert_eq!(
        engine.display_list_pages(),
        before,
        "thumbnails must not touch the viewport's display lists"
    );

    // A page wider than 10x the thumbnail width hits the zoom floor and comes back wider.
    let mut a0 = open(&fixture_path("large_format_a0.pdf"));
    let thumb = a0.render_thumb(0, 160).expect("thumbnail");
    assert_eq!(thumb.width, 239, "A0 at the 100 permille floor");
}
