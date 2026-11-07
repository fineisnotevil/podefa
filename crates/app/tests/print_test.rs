// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Printing on Windows through a real driver (ADR 0013): "Microsoft Print to PDF" takes a file name
//! with the job, so the whole way runs without a dialog and without paper, and the file it writes
//! can be looked at. No physical printer is ever used. Skipped where that printer is not installed.
#![cfg(windows)]

use app::platform::print_win::{self, GdiSink};
use engine_mupdf::{EngineHandle, MupdfEngine};
use pdf_core::print::{PrintTarget, SharedSink};
use pdf_core::tools::ToolJob;
use pdf_core::{EngineCmd, EngineEvent, PdfEngine as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const PRINTER: &str = "Microsoft Print to PDF";

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../engine-mupdf/tests/fixtures")
        .join(name)
}

fn print(document: &Path, sink: SharedSink, pages: Vec<u32>) -> Result<(), String> {
    let (handle, rx) = EngineHandle::spawn();
    handle
        .send(EngineCmd::Open {
            path: document.to_owned(),
            password: None,
        })
        .expect("alive");
    let job = ToolJob::Print {
        pages,
        target: PrintTarget::Device(sink),
        low_res: false,
    };
    handle
        .send(EngineCmd::RunTool { job_id: 1, job })
        .expect("alive");
    loop {
        if let EngineEvent::ToolDone { result, .. } =
            rx.recv_timeout(Duration::from_secs(120)).expect("event")
        {
            return result.map(|_| ());
        }
    }
}

/// The spooler finishes the file after the job has ended.
fn wait_for(path: &Path) -> MupdfEngine {
    let start = Instant::now();
    loop {
        let mut engine = MupdfEngine::new();
        if path.exists() && engine.open_document(path, None).is_ok() {
            return engine;
        }
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "the printer wrote no file"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn a_document_prints_through_a_real_driver() {
    let printers = print_win::printers();
    assert!(
        printers
            .iter()
            .all(|name| !name.is_empty() && !name.contains('\0'))
    );
    if !printers.iter().any(|name| name == PRINTER) {
        eprintln!("skipped: {PRINTER} is not installed");
        return;
    }
    let output = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("print_to_pdf.pdf");
    let _ = std::fs::remove_file(&output);
    let sink = GdiSink::new(PRINTER, "PODEFA test", None).to_file(output.to_str().expect("utf-8"));
    print(
        &fixture("large_format_a0.pdf"),
        SharedSink::new(sink),
        vec![0, 0],
    )
    .expect("print");

    let engine = wait_for(&output);
    assert_eq!(engine.page_count(), 2, "one sheet for each page asked");
    let page = engine.render_page(0, 0.5).expect("render");
    let ink = page.data.chunks_exact(3).filter(|p| p[0] < 0x80).count();
    assert!(ink > 0, "the sheet is blank");
    // Upright, on a sheet of the printer's: a poster shrunk to it, not its corner.
    let white = page.data.chunks_exact(3).filter(|p| p[0] > 0xf0).count();
    assert!(white > 0, "no paper left");
    drop(engine);
    let _ = std::fs::remove_file(&output);
}

#[test]
fn an_unknown_printer_fails_with_a_reason() {
    let sink = GdiSink::new("No such printer of PODEFA", "PODEFA test", None);
    let err =
        print(&fixture("minimal.pdf"), SharedSink::new(sink), vec![0]).expect_err("no printer");
    assert!(err.contains("printer"), "{err}");
}
