// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Regression test: cancelling a tile while its page is inside a clip must not end the process.
//!
//! MuPDF's display-list runner stops on an aborted cookie by breaking out of its loop and leaves
//! the clips it had pushed on the draw device's stack (`source/fitz/list-device.c`,
//! `fz_run_display_list`). Closing that device then throws "items left on stack in draw device"
//! (`source/fitz/draw-device.c`), and mupdf-rs 0.8 closes devices in `Drop` with no `fz_try`
//! around the call, so MuPDF reaches its "uncaught error" path and calls `exit(1)`. A user found it
//! with a real PDF zoomed quickly; this page reproduces it: every tile spends a long time inside two
//! nested clips, so a cancel lands while they are open. If the abort path comes back without a
//! guarded close, this test binary exits with code 1 instead of passing (Plan 0003, Phase 3 review).

use engine_mupdf::EngineHandle;
use pdf_core::{EngineCmd, EngineEvent, PageGeometry};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Duration;

/// One Letter page: two nested clips around 2000 full-page fills, so a tile's raster spends nearly
/// all its time with both clips pushed.
fn clipped_pdf() -> Vec<u8> {
    let mut content = String::from("q 0 0 612 792 re W n q 40 40 532 712 re W n\n");
    for i in 0..2000 {
        let _ = writeln!(
            content,
            "{} 0 0 rg 0 0 612 792 re f",
            (i % 10) as f32 / 10.0
        );
    }
    content.push_str("Q Q\n");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>".to_owned(),
        format!(
            "<< /Length {} >>\nstream\n{content}endstream",
            content.len()
        ),
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

fn next(rx: &Receiver<EngineEvent>) -> EngineEvent {
    rx.recv_timeout(Duration::from_secs(120))
        .expect("the engine answers within two minutes")
}

#[test]
fn cancelling_tiles_inside_a_clip_does_not_end_the_process() {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("clipped.pdf");
    std::fs::write(&path, clipped_pdf()).expect("write fixture");
    let (handle, rx) = EngineHandle::spawn();
    handle
        .send(EngineCmd::Open {
            path,
            password: None,
        })
        .expect("actor is alive");
    let EngineEvent::Opened { page_sizes, .. } = next(&rx) else {
        panic!("expected Opened");
    };
    assert!(matches!(next(&rx), EngineEvent::Outline { .. }));
    let geometry = PageGeometry::new(0, page_sizes[0].bounds(), 2_000);

    // A batch of slow tiles, cancelled while the pool is inside them, several times over.
    for round in 0..3u64 {
        let epoch = round + 1;
        for (col, row) in [
            (0, 0),
            (1, 0),
            (0, 1),
            (1, 1),
            (2, 0),
            (0, 2),
            (1, 2),
            (2, 1),
        ] {
            handle
                .send(EngineCmd::RenderTile {
                    geometry,
                    col,
                    row,
                    request_id: epoch,
                })
                .expect("send");
        }
        std::thread::sleep(Duration::from_millis(50));
        handle
            .send(EngineCmd::Cancel { request_id: epoch })
            .expect("send");
    }

    // The process is still here and still renders: one tile of a fresh epoch arrives.
    handle
        .send(EngineCmd::RenderTile {
            geometry,
            col: 0,
            row: 0,
            request_id: 99,
        })
        .expect("send");
    loop {
        match next(&rx) {
            EngineEvent::TileRendered { request_id: 99, .. } => break,
            EngineEvent::TileRendered { .. } | EngineEvent::TileAborted { .. } => {}
            other => panic!("unexpected event {other:?}"),
        }
    }
}
