// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Plan 0005 Phase 5 through the engine: the open document, unsaved changes included, exported as
//! Word on the tool thread. Row AH itself is the maintainer's judgement of real conversions; this
//! checks that the file is a DOCX MuPDF reads back with the text in order, that a cancel leaves
//! nothing, and how long a page takes.

use engine_mupdf::EngineHandle;
use mupdf::{Document, TextPageFlags};
use pdf_core::tools::ToolJob;
use pdf_core::{EngineCmd, EngineEvent};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

/// `pages` Letter pages, each a heading "Chapter N" and ten lines of text.
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
        let mut content = format!("BT /F1 18 Tf 54 720 Td (Chapter {p}) Tj ET");
        for line in 0..10 {
            let _ = write!(
                content,
                " BT /F1 11 Tf 54 {} Td (Line {line} of chapter {p} says something.) Tj ET",
                680 - 16 * line
            );
        }
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

fn scratch(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name)
}

fn next(rx: &Receiver<EngineEvent>) -> EngineEvent {
    rx.recv_timeout(Duration::from_secs(120))
        .expect("the engine answers within two minutes")
}

fn spawn_open(path: &Path) -> (EngineHandle, Receiver<EngineEvent>) {
    let (handle, rx) = EngineHandle::spawn();
    handle
        .send(EngineCmd::Open {
            path: path.to_path_buf(),
            password: None,
        })
        .expect("actor");
    while !matches!(next(&rx), EngineEvent::History { .. }) {}
    (handle, rx)
}

fn done(rx: &Receiver<EngineEvent>, job_id: u64) -> Result<Vec<PathBuf>, String> {
    loop {
        if let EngineEvent::ToolDone {
            job_id: id, result, ..
        } = next(rx)
            && id == job_id
        {
            return result;
        }
    }
}

#[test]
fn the_document_exports_as_a_word_file_with_its_text_in_order() {
    const PAGES: usize = 100;
    let path = scratch("word_source.pdf");
    std::fs::write(&path, text_pdf(PAGES)).expect("fixture");
    let (handle, rx) = spawn_open(&path);
    let output = scratch("word_out.docx");
    let _ = std::fs::remove_file(&output);
    let t = Instant::now();
    handle
        .send(EngineCmd::RunTool {
            job_id: 1,
            job: ToolJob::ExportWord {
                output: output.clone(),
            },
        })
        .expect("actor");
    assert_eq!(done(&rx, 1), Ok(vec![output.clone()]));
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    let bytes = std::fs::read(&output).expect("docx");
    println!(
        "[word] {PAGES} pages: {ms:.0} ms ({:.1} ms per page), {} KiB",
        ms / PAGES as f64,
        bytes.len() / 1024
    );
    // A DOCX is a zip archive with the document part in it.
    assert_eq!(&bytes[..4], b"PK\x03\x04");
    let names = String::from_utf8_lossy(&bytes);
    assert!(names.contains("word/document.xml"), "no document part");
    assert!(!scratch("word_out.docx.part").exists());
    drop(handle);

    // MuPDF reads DOCX too, through its HTML layout, which is slow (minutes for the 100 pages): a
    // four-page export is read back, and every chapter's heading and lines come back, in order.
    let path = scratch("word_small.pdf");
    std::fs::write(&path, text_pdf(4)).expect("fixture");
    let (handle, rx) = spawn_open(&path);
    let output = scratch("word_small.docx");
    handle
        .send(EngineCmd::RunTool {
            job_id: 2,
            job: ToolJob::ExportWord {
                output: output.clone(),
            },
        })
        .expect("actor");
    assert_eq!(done(&rx, 2), Ok(vec![output.clone()]));
    let doc = Document::open(output.to_str().expect("utf-8")).expect("MuPDF opens the DOCX");
    let mut text = String::new();
    for p in 0..doc.page_count().expect("count") {
        text.push_str(
            &doc.load_page(p)
                .expect("page")
                .to_text_page(TextPageFlags::empty())
                .expect("text")
                .to_text()
                .expect("text"),
        );
    }
    // Line breaks depend on the fonts the layout finds (Linux lays the text out narrower than
    // Windows), so whitespace is compared as single spaces.
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut from = 0;
    for chapter in 0..4 {
        for needle in [
            format!("Chapter {chapter}"),
            format!("Line 9 of chapter {chapter} says something."),
        ] {
            let at = text[from..].find(&needle).unwrap_or_else(|| {
                let tail: String = text[from..].chars().take(400).collect();
                panic!("{needle:?} after byte {from}, in: {tail:?}")
            });
            from += at + needle.len();
        }
    }
}

#[test]
fn a_cancelled_export_leaves_nothing() {
    let path = scratch("word_cancel.pdf");
    std::fs::write(&path, text_pdf(400)).expect("fixture");
    let (handle, rx) = spawn_open(&path);
    let output = scratch("word_cancel.docx");
    let _ = std::fs::remove_file(&output);
    handle
        .send(EngineCmd::RunTool {
            job_id: 7,
            job: ToolJob::ExportWord {
                output: output.clone(),
            },
        })
        .expect("actor");
    handle
        .send(EngineCmd::CancelTool { job_id: 7 })
        .expect("actor");
    assert_eq!(done(&rx, 7), Err("cancelled".to_owned()));
    assert!(!output.exists());
    assert!(!scratch("word_cancel.docx.part").exists());
}
