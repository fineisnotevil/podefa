// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Plan 0005 Phase 2 through the engine (row AC's engine half): merging and extracting on the tool
//! thread, inserting pages from a file, and the document information fields.

use engine_mupdf::{Change, EngineHandle, MupdfEngine};
use mupdf::pdf::{PdfDocument, PdfPage};
use mupdf::{Document, TextPageFlags};
use pdf_core::annot::{Colour, EditOp, MarkupKind};
use pdf_core::pages::PageOp;
use pdf_core::tools::{MergeInput, Metadata, ToolJob};
use pdf_core::{EngineCmd, EngineEvent, Rect};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::Duration;

/// `pages` Letter pages whose only line is "`tag` N"; with `link`, page 0 has a link to a web
/// address and, when there is a page 1, a link to page 1.
fn tagged_pdf(tag: &str, pages: usize, link: bool) -> Vec<u8> {
    let mut objects: Vec<String> = Vec::new();
    objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
    let kids: String = (0..pages).map(|p| format!("{} 0 R ", 4 + 2 * p)).collect();
    objects.push(format!(
        "<< /Type /Pages /Kids [{kids}] /Count {pages} /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 3 0 R >> >> >>"
    ));
    objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());
    let link_obj = 4 + 2 * pages;
    let goto = link && pages > 1;
    for p in 0..pages {
        let content = format!("BT /F1 10 Tf 54 750 Td ({tag} {p}) Tj ET");
        let annots = match (link && p == 0, goto) {
            (true, true) => format!(" /Annots [{link_obj} 0 R {} 0 R]", link_obj + 1),
            (true, false) => format!(" /Annots [{link_obj} 0 R]"),
            _ => String::new(),
        };
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /Contents {} 0 R{annots} >>",
            5 + 2 * p
        ));
        objects.push(format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ));
    }
    if link {
        objects.push(
            "<< /Type /Annot /Subtype /Link /Rect [50 740 150 760] /Border [0 0 0] \
             /A << /S /URI /URI (https://example.org/) >> >>"
                .into(),
        );
    }
    if goto {
        // Page 1 is object 6.
        objects.push(
            "<< /Type /Annot /Subtype /Link /Rect [50 700 150 720] /Border [0 0 0] \
             /Dest [6 0 R /Fit] >>"
                .into(),
        );
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

fn write(name: &str, bytes: &[u8]) -> PathBuf {
    let path = scratch(name);
    std::fs::write(&path, bytes).expect("write fixture");
    path
}

/// Every page's text, trimmed.
fn texts(path: &Path) -> Vec<String> {
    let doc = Document::open(path.to_str().expect("utf-8")).expect("open output");
    (0..doc.page_count().expect("count"))
        .map(|p| {
            doc.load_page(p)
                .expect("page")
                .to_text_page(TextPageFlags::empty())
                .expect("text")
                .to_text()
                .expect("text")
                .trim()
                .to_owned()
        })
        .collect()
}

fn annotation_kinds(path: &Path, page: i32) -> Vec<String> {
    let doc = PdfDocument::open(path.to_str().expect("utf-8")).expect("open");
    let page = PdfPage::try_from(doc.load_page(page).expect("page")).expect("pdf page");
    let mut kinds: Vec<String> = page
        .annotations()
        .map(|a| format!("{:?}", a.r#type().expect("type")))
        .collect();
    kinds.extend(
        page.links_from_annotations_lossy()
            .expect("links")
            .into_iter()
            .map(|_| "Link".to_owned()),
    );
    kinds.sort();
    kinds.dedup();
    kinds
}

/// A page's links: a web address, or "page N" for a page of the same file.
fn links(path: &Path, page: i32) -> Vec<String> {
    let doc = Document::open(path.to_str().expect("utf-8")).expect("open");
    let mut out: Vec<String> = doc
        .load_page(page)
        .expect("page")
        .links()
        .expect("links")
        .map(|link| match link.dest {
            Some(dest) => format!("page {}", dest.loc.page_number),
            None => link.uri,
        })
        .collect();
    out.sort();
    out
}

fn next(rx: &Receiver<EngineEvent>) -> EngineEvent {
    rx.recv_timeout(Duration::from_secs(60))
        .expect("the engine answers within a minute")
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

/// Runs a job and waits for its end, counting the progress reports on the way.
fn run(
    handle: &EngineHandle,
    rx: &Receiver<EngineEvent>,
    job_id: u64,
    job: ToolJob,
) -> (Result<Vec<PathBuf>, String>, u32) {
    handle
        .send(EngineCmd::RunTool { job_id, job })
        .expect("actor");
    let mut progress = 0;
    loop {
        match next(rx) {
            EngineEvent::ToolProgress { job_id: id, .. } if id == job_id => progress += 1,
            EngineEvent::ToolDone { job_id: id, result } if id == job_id => {
                return (result, progress);
            }
            _ => {}
        }
    }
}

/// A highlight on page 0 of the open document, not saved.
fn mark(handle: &EngineHandle, rx: &Receiver<EngineEvent>) {
    handle
        .send(EngineCmd::Edit {
            op: EditOp::AddMarkup {
                page: 0,
                kind: MarkupKind::Highlight,
                rects: vec![Rect::new(54.0, 40.0, 60.0, 12.0)],
                colour: Colour::new(1.0, 0.85, 0.0),
                author: None,
            },
            edit_id: 1,
            generation: 0,
        })
        .expect("actor");
    while !matches!(next(rx), EngineEvent::History { .. }) {}
}

#[test]
fn a_merge_keeps_order_annotations_links_and_unsaved_changes() {
    let a = write("tools_merge_a.pdf", &tagged_pdf("A", 3, false));
    let b = write("tools_merge_b.pdf", &tagged_pdf("B", 2, true));
    let (handle, rx) = spawn_open(&a);
    mark(&handle, &rx);
    let output = scratch("tools_merged_out.pdf");
    let _ = std::fs::remove_file(&output);
    let (result, progress) = run(
        &handle,
        &rx,
        7,
        ToolJob::Merge {
            inputs: vec![MergeInput::Current, MergeInput::File(b.clone())],
            output: output.clone(),
        },
    );
    assert_eq!(result, Ok(vec![output.clone()]));
    assert_eq!(progress, 3, "one report per input and one for the write");
    assert_eq!(texts(&output), ["A 0", "A 1", "A 2", "B 0", "B 1"]);
    assert_eq!(
        annotation_kinds(&output, 0),
        ["Highlight"],
        "the unsaved highlight"
    );
    assert_eq!(
        links(&output, 3),
        ["https://example.org/", "page 4"],
        "B's links, its page link following its page"
    );
    assert!(!scratch("tools_merged_out.pdf.part").exists());
    // The open file is untouched: the highlight is still only in memory.
    assert!(annotation_kinds(&a, 0).is_empty());
}

#[test]
fn extract_writes_each_part_in_the_order_given() {
    let a = write("tools_extract_a.pdf", &tagged_pdf("A", 4, true));
    let (handle, rx) = spawn_open(&a);
    let first = scratch("tools_part_1.pdf");
    let second = scratch("tools_part_2.pdf");
    let (result, progress) = run(
        &handle,
        &rx,
        8,
        ToolJob::Extract {
            parts: vec![(vec![1, 0], first.clone()), (vec![0], second.clone())],
        },
    );
    assert_eq!(result, Ok(vec![first.clone(), second.clone()]));
    assert_eq!(progress, 2);
    assert_eq!(texts(&first), ["A 1", "A 0"]);
    assert_eq!(texts(&second), ["A 0"]);
    // Page 0's link to page 1 follows it into the first part, and is dropped from the second,
    // which has no page 1.
    assert_eq!(links(&first, 1), ["https://example.org/", "page 0"]);
    assert_eq!(links(&second, 0), ["https://example.org/"]);
}

#[test]
fn a_failed_job_leaves_an_existing_file_alone() {
    let a = write("tools_fail_a.pdf", &tagged_pdf("A", 2, false));
    let (handle, rx) = spawn_open(&a);
    let output = write("tools_fail_out.pdf", b"precious");
    let (result, _) = run(
        &handle,
        &rx,
        9,
        ToolJob::Merge {
            inputs: vec![
                MergeInput::Current,
                MergeInput::File(scratch("tools_no_such_file.pdf")),
            ],
            output: output.clone(),
        },
    );
    assert!(result.is_err());
    assert_eq!(std::fs::read(&output).expect("read"), b"precious");
    assert!(!scratch("tools_fail_out.pdf.part").exists());
}

#[test]
fn the_open_file_is_never_an_output() {
    let a = write("tools_self.pdf", &tagged_pdf("A", 2, false));
    let before = std::fs::read(&a).expect("read");
    let (handle, rx) = spawn_open(&a);
    let (result, _) = run(
        &handle,
        &rx,
        10,
        ToolJob::Extract {
            parts: vec![(vec![0], a.clone())],
        },
    );
    assert!(result.is_err());
    assert_eq!(std::fs::read(&a).expect("read"), before);
}

#[test]
fn a_cancelled_merge_writes_nothing() {
    let a = write("tools_cancel_a.pdf", &tagged_pdf("A", 2, false));
    let inputs: Vec<MergeInput> = (0..200).map(|_| MergeInput::File(a.clone())).collect();
    let (handle, rx) = spawn_open(&a);
    let output = scratch("tools_cancel_out.pdf");
    let _ = std::fs::remove_file(&output);
    handle
        .send(EngineCmd::RunTool {
            job_id: 11,
            job: ToolJob::Merge {
                inputs,
                output: output.clone(),
            },
        })
        .expect("actor");
    handle
        .send(EngineCmd::CancelTool { job_id: 11 })
        .expect("actor");
    let result = loop {
        if let EngineEvent::ToolDone { job_id: 11, result } = next(&rx) {
            break result;
        }
    };
    assert_eq!(result, Err("cancelled".to_owned()));
    assert!(!output.exists());
    assert!(!scratch("tools_cancel_out.pdf.part").exists());
}

#[test]
fn pages_from_a_file_are_inserted_and_undone() {
    let a = write("tools_insert_a.pdf", &tagged_pdf("A", 2, false));
    let b = write("tools_insert_b.pdf", &tagged_pdf("B", 3, true));
    let mut engine = MupdfEngine::new();
    engine.open_document_with_password(&a, None).expect("open");
    let change = engine
        .edit(&EditOp::Pages(PageOp::InsertFrom {
            path: b,
            at: 1,
            source_pages: 0,
        }))
        .expect("insert");
    let Change::Pages { map, .. } = change else {
        panic!("a structural change");
    };
    assert_eq!(map, [Some(0), None, None, None, Some(1)]);
    let snapshot = scratch("tools_insert_snapshot.pdf");
    std::fs::write(&snapshot, engine.snapshot().expect("snapshot")).expect("write");
    assert_eq!(texts(&snapshot), ["A 0", "B 0", "B 1", "B 2", "A 1"]);
    assert_eq!(
        links(&snapshot, 1),
        ["https://example.org/", "page 2"],
        "the file's links, its page link following its page"
    );
    engine.undo().expect("undo");
    std::fs::write(&snapshot, engine.snapshot().expect("snapshot")).expect("write");
    assert_eq!(texts(&snapshot), ["A 0", "A 1"]);
}

#[test]
fn document_properties_are_set_undone_and_saved() {
    let a = write("tools_meta.pdf", &tagged_pdf("A", 1, false));
    let mut engine = MupdfEngine::new();
    engine.open_document_with_password(&a, None).expect("open");
    let title = "Öğrenci raporu — 日本語";
    engine
        .edit(&EditOp::SetMetadata(Metadata {
            title: Some(title.to_owned()),
            author: Some("Ayşe".to_owned()),
            subject: None,
            keywords: Some("pdf, test".to_owned()),
        }))
        .expect("set");
    let info = engine.document_info().expect("info");
    assert_eq!(info.fields.title.as_deref(), Some(title));
    assert_eq!(info.fields.author.as_deref(), Some("Ayşe"));
    assert!(info.modified.starts_with("D:"), "{}", info.modified);
    assert!(!info.has_xmp);

    engine.undo().expect("undo");
    assert_eq!(
        engine
            .document_info()
            .expect("info")
            .fields
            .title
            .as_deref(),
        Some("")
    );
    engine.redo().expect("redo");
    engine.save(None).expect("save");
    let reopened = Document::open(a.to_str().expect("utf-8")).expect("reopen");
    assert_eq!(
        reopened
            .metadata(mupdf::MetadataName::Title)
            .expect("title"),
        title
    );
    assert_eq!(
        reopened
            .metadata(mupdf::MetadataName::Keywords)
            .expect("keywords"),
        "pdf, test"
    );
}

/// Phase 1 regression: a duplicated page lost its annotations and links, because MuPDF's page
/// graft leaves `/Annots` out. The copy has them, and its page link still leads to the page the
/// original's does.
#[test]
fn a_duplicated_page_keeps_its_annotations_and_links() {
    let a = write("tools_duplicate.pdf", &tagged_pdf("A", 3, true));
    let mut engine = MupdfEngine::new();
    engine.open_document_with_password(&a, None).expect("open");
    engine
        .edit(&EditOp::AddMarkup {
            page: 0,
            kind: MarkupKind::Highlight,
            rects: vec![Rect::new(54.0, 40.0, 60.0, 12.0)],
            colour: Colour::new(1.0, 0.85, 0.0),
            author: None,
        })
        .expect("highlight");
    engine
        .edit(&EditOp::Pages(PageOp::Duplicate { pages: vec![0] }))
        .expect("duplicate");
    let snapshot = scratch("tools_duplicate_snapshot.pdf");
    std::fs::write(&snapshot, engine.snapshot().expect("snapshot")).expect("write");
    assert_eq!(texts(&snapshot), ["A 0", "A 0", "A 1", "A 2"]);
    for page in [0, 1] {
        assert_eq!(annotation_kinds(&snapshot, page), ["Highlight", "Link"]);
        assert_eq!(links(&snapshot, page), ["https://example.org/", "page 2"]);
    }
    // The copy's annotations are objects of their own: deleting the original's leaves them.
    let original = engine.annotations(0).expect("annotations");
    let copy = engine.annotations(1).expect("annotations");
    assert_eq!(original.len(), copy.len());
    assert!(original.iter().all(|a| copy.iter().all(|b| a.id != b.id)));
}
