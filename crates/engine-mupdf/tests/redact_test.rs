// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Redaction (Plan 0009 Phase 1, row AY): nothing under a mark survives Apply and a save.
//!
//! One page keeps a different secret, `SECRET-<letter>`, in every place §3.6 and §3.1 found text
//! can sit under a box or about it: visible text, OCR layers, clipping and zero-alpha text, a
//! marked sequence's `/ActualText`, a form XObject, a tiling pattern, a soft mask, a hidden layer,
//! a structure element, the page's thumbnail and private data, a form field and a highlight over
//! the box; and in the document's hidden information: its title, XMP, an attachment and a script.
//! A note away from the box and a public line stay, and the outline entry that names the secret
//! is listed for the user, not removed.

use engine_mupdf::{EngineHandle, MupdfEngine};
use mupdf::pdf::{PdfDocument, PdfWriteOptions};
use mupdf::{Colorspace, Matrix, TextPageFlags};
use pdf_core::annot::{AnnotKind, EditOp, RedactionMark};
use pdf_core::tools::{MergeInput, Preset, ToolJob};
use pdf_core::{EngineCmd, EngineEvent, Rect, SaveError};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::time::Duration;

/// The box over the secrets, page space (points from the top-left): PDF y 332 to 462.
const BOX: Rect = Rect::new(50.0, 330.0, 510.0, 130.0);

/// Secrets under the box or in the hidden information, each with its own letter.
const UNDER: &str = "VOZKCTFPMHSUIWAXEJN";

/// With `spill`, the pattern's and the soft mask's fills reach past the box, so MuPDF's pass keeps
/// them and the engine's second pass has to remove them; without, the first pass does.
fn fixture_with(spill: bool) -> Vec<u8> {
    let (pw, mw) = if spill { (528, 300) } else { (200, 200) };
    let content = format!(
        "BT /F1 14 Tf 72 700 Td (PUBLIC-LINE) Tj ET\n\
        q 570 0 0 60 20 340 cm /Im1 Do Q\n\
        BT /F1 12 Tf 72 450 Td (SECRET-V visible) Tj ET\n\
        BT 3 Tr /F1 12 Tf 72 430 Td (SECRET-O ocr) Tj ET\n\
        BT 3 Tr /F1 10 Tf 1 0 0 1 300 430 Tm 60 Tz (SECRET-Z scaled) Tj ET\n\
        q BT 7 Tr /F1 12 Tf 72 410 Td (SECRET-K clip) Tj ET Q\n\
        q /GS0 gs BT /F1 12 Tf 300 410 Td (SECRET-C alpha) Tj ET Q\n\
        /Span << /ActualText (SECRET-T) >> BDC BT 3 Tr /F1 12 Tf 72 390 Td (xxxx) Tj ET EMC\n\
        q 1 0 0 1 300 385 cm /Fm1 Do Q\n\
        q /Pattern cs /P1 scn 72 345 {pw} 30 re f Q\n\
        q /GSm gs 0 0 1 rg 300 345 {mw} 30 re f Q\n\
        /OC /oc1 BDC BT /F1 12 Tf 72 360 Td (SECRET-H hidden) Tj ET EMC\n\
        /P << /MCID 0 >> BDC BT /F1 12 Tf 300 360 Td (yyyy) Tj ET EMC"
    );
    let font = "/Resources << /Font << /F1 4 0 R >> >>";
    let stream = |dict: String, data: &[u8]| -> Vec<u8> {
        let mut v = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
        v.extend_from_slice(data);
        v.extend_from_slice(b"\nendstream");
        v
    };
    let objects: Vec<Vec<u8>> = vec![
        // 1 catalog.
        b"<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [13 0 R] /D << /OFF [13 0 R] >> >> \
          /StructTreeRoot 14 0 R /MarkInfo << /Marked true >> \
          /AcroForm << /Fields [16 0 R] /DA (/Helv 0 Tf 0 g) >> /Metadata 17 0 R \
          /Names << /EmbeddedFiles << /Names [(f.txt) 18 0 R] >> /JavaScript << /Names [(js) 20 0 R] >> >> \
          /Outlines 21 0 R >>"
            .to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R \
          /Resources << /Font << /F1 4 0 R >> /XObject << /Im1 6 0 R /Fm1 7 0 R >> \
          /Pattern << /P1 8 0 R >> /ExtGState << /GS0 9 0 R /GSm 10 0 R >> \
          /Properties << /oc1 13 0 R >> >> /Thumb 12 0 R /StructParents 0 \
          /PieceInfo << /Illustrator << /Private (SECRET-I) >> >> /Annots [16 0 R 23 0 R 24 0 R] >>"
            .to_vec(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        stream(String::new(), content.as_bytes()),
        // 6 a scanned image: grey 0x11.
        stream(
            "/Type /XObject /Subtype /Image /Width 57 /Height 6 /ColorSpace /DeviceRGB \
             /BitsPerComponent 8"
                .into(),
            &[0x11u8; 57 * 6 * 3],
        ),
        stream(
            format!("/Type /XObject /Subtype /Form /BBox [0 0 300 50] {font}"),
            b"BT 3 Tr /F1 12 Tf 0 5 Td (SECRET-F form) Tj ET",
        ),
        stream(
            format!(
                "/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 200 30] \
                 /XStep 200 /YStep 30 /Matrix [1 0 0 1 72 345] {font}"
            ),
            b"BT /F1 10 Tf 2 10 Td (SECRET-P pattern) Tj ET",
        ),
        b"<< /Type /ExtGState /ca 0 >>".to_vec(),
        b"<< /Type /ExtGState /SMask << /Type /Mask /S /Luminosity /G 11 0 R >> >>".to_vec(),
        stream(
            format!(
                "/Type /XObject /Subtype /Form /BBox [0 0 612 792] \
                 /Group << /S /Transparency /CS /DeviceGray >> {font}"
            ),
            b"1 g 0 0 612 792 re f BT 0 g /F1 12 Tf 300 350 Td (SECRET-M mask) Tj ET",
        ),
        // 12 thumbnail.
        stream(
            "/Width 3 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8".into(),
            b"SECRET-UX",
        ),
        b"<< /Type /OCG /Name (Layer) >>".to_vec(),
        b"<< /Type /StructTreeRoot /K 15 0 R /ParentTree << /Nums [0 [15 0 R]] >> >>".to_vec(),
        b"<< /Type /StructElem /S /P /P 14 0 R /Pg 3 0 R /K 0 /ActualText (SECRET-S) >>".to_vec(),
        // 16 a text field over the box.
        b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /V (SECRET-W) /Rect [300 340 500 356] \
          /P 3 0 R /DA (/Helv 10 Tf 0 g) >>"
            .to_vec(),
        stream(
            "/Type /Metadata /Subtype /XML".into(),
            b"<x:xmpmeta xmlns:x='adobe:ns:meta/'>SECRET-X</x:xmpmeta>",
        ),
        b"<< /Type /Filespec /F (f.txt) /EF << /F 19 0 R >> >>".to_vec(),
        stream("/Type /EmbeddedFile".into(), b"SECRET-E attached"),
        b"<< /S /JavaScript /JS (app.alert\\('SECRET-J'\\)) >>".to_vec(),
        // 21 the outline: 26, 22 (with 27 under it), 28.
        b"<< /Type /Outlines /First 26 0 R /Last 28 0 R /Count 4 >>".to_vec(),
        b"<< /Title (Chapter SECRET-Q) /Parent 21 0 R /Prev 26 0 R /Next 28 0 R /First 27 0 R           /Last 27 0 R /Count 1 /Dest [3 0 R /XYZ 0 792 0] >>"
            .to_vec(),
        // 23 a highlight over the box, 24 a note away from it.
        b"<< /Type /Annot /Subtype /Highlight /Rect [70 440 230 460] \
          /QuadPoints [70 460 230 460 70 440 230 440] /C [1 1 0] /Contents (SECRET-A) >>"
            .to_vec(),
        b"<< /Type /Annot /Subtype /Text /Rect [500 700 520 720] /Contents (kept-note) >>".to_vec(),
        b"<< /Title (SECRET-N) /Producer (fixture) >>".to_vec(),
        b"<< /Title (Introduction) /Parent 21 0 R /Next 22 0 R /Dest [3 0 R /XYZ 0 792 0] >>"
            .to_vec(),
        b"<< /Title (Details) /Parent 22 0 R /Dest [3 0 R /XYZ 0 500 0] >>".to_vec(),
        b"<< /Title (Appendix) /Parent 21 0 R /Prev 22 0 R /Dest [3 0 R /XYZ 0 100 0] >>".to_vec(),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /Info 25 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

fn fixture() -> Vec<u8> {
    fixture_with(true)
}

fn scratch(name: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("redact_{n}_{name}"))
}

fn count(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

/// The letters of the secrets in `bytes`, literal or hex-encoded.
fn found(bytes: &[u8], letters: &str) -> String {
    let lower = bytes.to_ascii_lowercase();
    letters
        .chars()
        .filter(|c| {
            let token = format!("SECRET-{c}");
            let hex: String = token.bytes().map(|b| format!("{b:02x}")).collect();
            count(bytes, token.as_bytes()) > 0 || count(&lower, hex.as_bytes()) > 0
        })
        .collect()
}

/// A saved file's bytes with every stream decompressed, so its text can be searched.
fn decompressed(path: &std::path::Path) -> Vec<u8> {
    let doc = PdfDocument::open(path.to_str().expect("utf-8")).expect("open saved");
    let out = scratch("plain.pdf");
    let mut options = PdfWriteOptions::default();
    options.set_decompress(true);
    doc.save_with_options(out.to_str().expect("utf-8"), options)
        .expect("decompress");
    std::fs::read(out).expect("read")
}

fn page_text(path: &std::path::Path) -> String {
    let doc = PdfDocument::open(path.to_str().expect("utf-8")).expect("open");
    doc.load_page(0)
        .expect("page")
        .to_text_page(TextPageFlags::empty())
        .expect("text")
        .to_text()
        .expect("text")
}

fn mark() -> EditOp {
    EditOp::MarkRedactions {
        marks: vec![RedactionMark {
            page: 0,
            rects: vec![BOX],
        }],
    }
}

fn open_engine() -> (MupdfEngine, PathBuf) {
    open_fixture(fixture())
}

fn open_fixture(bytes: Vec<u8>) -> (MupdfEngine, PathBuf) {
    let path = scratch("source.pdf");
    std::fs::write(&path, bytes).expect("fixture");
    let mut engine = MupdfEngine::new();
    engine
        .open_document_with_password(&path, None)
        .expect("open");
    (engine, path)
}

#[test]
fn the_fixture_holds_every_secret() {
    // The test can fail: every secret is in the file before the redaction.
    assert_eq!(found(&fixture(), UNDER), UNDER);
    assert_eq!(found(&fixture(), "Q"), "Q");
}

#[test]
fn nothing_under_the_marks_is_written() {
    let (mut engine, source) = open_engine();
    engine.edit(&mark()).expect("mark");
    let marks = engine.annotations(0).expect("annotations");
    assert!(marks.iter().any(|a| a.kind == AnnotKind::Redaction));
    engine
        .edit(&EditOp::ApplyRedactions { clean: true })
        .expect("apply");

    // The open document still holds the removed objects (only a garbage-collecting save drops
    // them): what makes the written file clean is the save, not the edit.
    let snapshot = engine.snapshot().expect("snapshot");
    assert!(!found(&snapshot, "V").is_empty());

    // Not in place: an incremental update keeps every removed byte.
    assert_eq!(engine.save(None), Err(SaveError::NeedsSaveAs));
    let target = scratch("redacted.pdf");
    engine.save(Some(&target)).expect("save as");

    let bytes = decompressed(&target);
    assert_eq!(found(&bytes, UNDER), "", "secrets left in the saved file");
    // The outline entry is the user's to decide (§0 question 2).
    assert_eq!(found(&bytes, "Q"), "Q");
    assert_eq!(
        count(&bytes, b"kept-note"),
        1,
        "a note away from the box stays"
    );
    let text = page_text(&target);
    assert!(text.contains("PUBLIC-LINE"), "{text}");
    assert_eq!(found(text.as_bytes(), UNDER), "");

    // The scanned image: blanked and boxed under the mark, as it was beside it.
    let doc = PdfDocument::open(target.to_str().expect("utf-8")).expect("open");
    let pix = doc
        .load_page(0)
        .expect("page")
        .to_pixmap(&Matrix::IDENTITY, &Colorspace::device_rgb(), false, false)
        .expect("render");
    let px = |x: i32| pix.pixel(x, 400).expect("pixel").components().to_vec();
    assert_eq!(px(30), vec![0x11, 0x11, 0x11]);
    assert_eq!(px(300), vec![0, 0, 0]);
    // The original file is untouched.
    assert_eq!(std::fs::read(&source).expect("source"), fixture());
}

#[test]
fn a_mark_shows_on_the_page_until_it_is_applied() {
    // The app shows marks as the engine draws the page: a mark must change the page's pixels.
    let (mut engine, _) = open_engine();
    let before = engine.render_thumb(0, 306).expect("thumb");
    engine.edit(&mark()).expect("mark");
    let marked = engine.render_thumb(0, 306).expect("thumb");
    assert_eq!((before.width, before.height), (marked.width, marked.height));
    assert_ne!(before.data, marked.data, "the mark is not drawn");
}

#[test]
fn one_pass_is_enough_when_the_fills_are_covered() {
    // MuPDF's own pass removes the covered pattern and mask fills; the engine's second pass does
    // not run, so the replacement text and the resources are the first pass's to clean.
    let (mut engine, _) = open_fixture(fixture_with(false));
    engine.edit(&mark()).expect("mark");
    engine
        .edit(&EditOp::ApplyRedactions { clean: false })
        .expect("apply");
    let target = scratch("one_pass.pdf");
    engine.save(Some(&target)).expect("save as");
    let bytes = decompressed(&target);
    assert_eq!(found(&bytes, "VOZKCTFPMHSUIWA"), "");
}

#[test]
fn without_clean_up_the_hidden_information_stays() {
    let (mut engine, _) = open_engine();
    engine.edit(&mark()).expect("mark");
    engine
        .edit(&EditOp::ApplyRedactions { clean: false })
        .expect("apply");
    let target = scratch("kept.pdf");
    engine.save(Some(&target)).expect("save as");
    let bytes = decompressed(&target);
    // Under the box: gone either way. The title, XMP, attachment and script: kept.
    assert_eq!(found(&bytes, "VOZKCTFPMHSUIWA"), "");
    assert_eq!(found(&bytes, "XEJN"), "XEJN");
}

#[test]
fn undo_brings_everything_back_until_the_save() {
    let (mut engine, _) = open_engine();
    engine.edit(&mark()).expect("mark");
    engine
        .edit(&EditOp::ApplyRedactions { clean: true })
        .expect("apply");
    let text = |engine: &MupdfEngine| {
        let bytes = engine.snapshot().expect("snapshot");
        let doc = PdfDocument::from_bytes(&bytes).expect("reopen");
        doc.load_page(0)
            .expect("page")
            .to_text_page(TextPageFlags::empty())
            .expect("text")
            .to_text()
            .expect("text")
    };
    assert!(!text(&engine).contains("SECRET-V"));
    engine.undo().expect("undo").expect("a step");
    assert!(text(&engine).contains("SECRET-V"));
    let annotations = engine.annotations(0).expect("annotations");
    assert!(annotations.iter().any(|a| a.kind == AnnotKind::Redaction));
    assert!(annotations.iter().any(|a| a.kind == AnnotKind::Highlight));
    engine.redo().expect("redo").expect("a step");
    assert!(!text(&engine).contains("SECRET-V"));
}

#[test]
fn apply_without_marks_is_refused() {
    let (mut engine, _) = open_engine();
    let err = engine
        .edit(&EditOp::ApplyRedactions { clean: true })
        .expect_err("nothing to apply");
    assert!(err.to_string().contains("no redaction mark"), "{err}");
    // Nothing happened: the document can still be saved in place.
    assert_eq!(engine.history_state(), (false, false));
}

fn next(rx: &Receiver<EngineEvent>) -> EngineEvent {
    rx.recv_timeout(Duration::from_secs(60))
        .expect("the engine answers within a minute")
}

#[test]
fn search_and_outline_after_a_redaction() {
    let path = scratch("actor.pdf");
    std::fs::write(&path, fixture()).expect("fixture");
    let (handle, rx) = EngineHandle::spawn();
    handle
        .send(EngineCmd::Open {
            path,
            password: None,
        })
        .expect("alive");
    let generation = 0;
    for (edit_id, op) in [mark(), EditOp::ApplyRedactions { clean: true }]
        .into_iter()
        .enumerate()
    {
        handle
            .send(EngineCmd::Edit {
                op,
                edit_id: edit_id as u64,
                generation,
            })
            .expect("alive");
    }
    let mut redacted = None;
    let mut edits = 0;
    while redacted.is_none() {
        match next(&rx) {
            EngineEvent::Edited { result, .. } => {
                result.expect("edit");
                edits += 1;
            }
            EngineEvent::Redacted {
                annotations,
                fields,
                outline,
                ..
            } => redacted = Some((annotations, fields, outline)),
            _ => {}
        }
    }
    assert_eq!(edits, 2);
    let (annotations, fields, outline) = redacted.expect("redacted");
    assert_eq!((annotations, fields), (1, 1));
    assert_eq!(outline, [(1, "Chapter SECRET-Q".to_owned())]);

    // The user removes the entry; the entry under it goes with it, and Undo brings both back.
    let titles = |rx: &Receiver<EngineEvent>| loop {
        if let EngineEvent::Outline { items } = next(rx) {
            return items.into_iter().map(|i| i.title).collect::<Vec<_>>();
        }
    };
    handle
        .send(EngineCmd::Edit {
            op: EditOp::RemoveOutline { items: vec![1] },
            edit_id: 2,
            generation,
        })
        .expect("alive");
    assert_eq!(titles(&rx), ["Introduction", "Appendix"]);
    handle.send(EngineCmd::Undo { generation }).expect("alive");
    assert_eq!(
        titles(&rx),
        ["Introduction", "Chapter SECRET-Q", "Details", "Appendix"]
    );
    handle.send(EngineCmd::Redo { generation }).expect("alive");
    assert_eq!(titles(&rx), ["Introduction", "Appendix"]);

    // The search thread reads the redacted document: the public line is found, the secret not.
    for (query, expected) in [("PUBLIC-LINE", 1), ("SECRET-V", 0), ("SECRET-O", 0)] {
        handle
            .send(EngineCmd::Search {
                query: query.to_owned(),
                from_page: 0,
                generation: 7,
            })
            .expect("alive");
        let mut hits = 0;
        loop {
            match next(&rx) {
                EngineEvent::SearchHits { hits: h, .. } => hits += h.len(),
                EngineEvent::SearchDone { .. } => break,
                _ => {}
            }
        }
        assert_eq!(hits, expected, "{query}");
    }
}

#[test]
fn the_tools_write_nothing_that_was_redacted() {
    // Before the save the open document still holds the removed objects; the tools that read it
    // (Extract and Split, Merge, Compress) must not carry them into their files. With the
    // clean-up off, Compress keeps the hidden information, as a save would.
    let path = scratch("tools.pdf");
    std::fs::write(&path, fixture()).expect("fixture");
    let (handle, rx) = EngineHandle::spawn();
    handle
        .send(EngineCmd::Open {
            path,
            password: None,
        })
        .expect("alive");
    for (edit_id, op) in [mark(), EditOp::ApplyRedactions { clean: true }]
        .into_iter()
        .enumerate()
    {
        handle
            .send(EngineCmd::Edit {
                op,
                edit_id: edit_id as u64,
                generation: 0,
            })
            .expect("alive");
    }
    while !matches!(next(&rx), EngineEvent::Redacted { .. }) {}
    let (part, merged, compressed) = (
        scratch("part.pdf"),
        scratch("merged.pdf"),
        scratch("compressed.pdf"),
    );
    let jobs = [
        ToolJob::Extract {
            parts: vec![(vec![0], part.clone())],
        },
        ToolJob::Merge {
            inputs: vec![MergeInput::Current],
            output: merged.clone(),
        },
        ToolJob::Compress {
            input: MergeInput::Current,
            preset: Preset::Lossless,
            output: compressed.clone(),
        },
    ];
    for (job_id, job) in jobs.into_iter().enumerate() {
        let job_id = job_id as u64 + 1;
        handle
            .send(EngineCmd::RunTool { job_id, job })
            .expect("alive");
        loop {
            if let EngineEvent::ToolDone {
                job_id: id, result, ..
            } = next(&rx)
                && id == job_id
            {
                result.expect("tool");
                break;
            }
        }
    }
    for output in [&part, &merged, &compressed] {
        let bytes = decompressed(output);
        assert_eq!(found(&bytes, UNDER), "", "{}", output.display());
        assert!(page_text(output).contains("PUBLIC-LINE"));
    }
}

/// One turned page (`/Rotate 90`) with two secrets and a public word, for marks in the page's
/// visible space.
fn turned_fixture() -> Vec<u8> {
    let content = "BT /F1 14 Tf 72 700 Td (TURNED-ONE) Tj ET\n\
                   BT /F1 14 Tf 300 400 Td (TURNED-TWO) Tj ET\n\
                   BT /F1 14 Tf 72 100 Td (KEEP-THIS) Tj ET";
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Rotate 90 /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R >> >> >>"
            .to_owned(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    out
}

#[test]
fn marks_follow_a_turned_page_and_one_mark_takes_several_lines() {
    let (mut engine, source) = open_fixture(turned_fixture());
    // Where MuPDF shows the secrets: the page's visible space, as the app's marks are.
    let doc = PdfDocument::open(source.to_str().expect("utf-8")).expect("open");
    let page = doc.load_page(0).expect("page");
    let bounds = |word: &str| {
        let hits = page.search(word, 4).expect("search");
        let q = &hits[0];
        let xs = [q.ul.x, q.ur.x, q.ll.x, q.lr.x];
        let ys = [q.ul.y, q.ur.y, q.ll.y, q.lr.y];
        let (x0, x1) = (
            xs.iter().copied().fold(f32::MAX, f32::min),
            xs.iter().copied().fold(f32::MIN, f32::max),
        );
        let (y0, y1) = (
            ys.iter().copied().fold(f32::MAX, f32::min),
            ys.iter().copied().fold(f32::MIN, f32::max),
        );
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    };
    let (one, two) = (bounds("TURNED-ONE"), bounds("TURNED-TWO"));
    drop(page);
    // One mark of one rectangle (written as its `/Rect`), one of two (written as quads; the
    // second rectangle covers nothing).
    let empty = Rect::new(two.x, two.y + 40.0, two.width, two.height);
    engine
        .edit(&EditOp::MarkRedactions {
            marks: vec![
                RedactionMark {
                    page: 0,
                    rects: vec![one],
                },
                RedactionMark {
                    page: 0,
                    rects: vec![two, empty],
                },
            ],
        })
        .expect("mark");
    let marks: Vec<_> = engine
        .annotations(0)
        .expect("annotations")
        .into_iter()
        .filter(|a| a.kind == AnnotKind::Redaction)
        .collect();
    assert_eq!(marks.len(), 2);
    // Each mark is where it was given (one in the PDF's own coordinates would come out turned);
    // the display bounds take the mark's 1 pt border.
    let near = |r: Rect, want: Rect| {
        [
            (r.x, want.x),
            (r.y, want.y),
            (r.x + r.width, want.x + want.width),
            (r.y + r.height, want.y + want.height),
        ]
        .iter()
        .all(|(a, b)| (a - b).abs() <= 1.01)
    };
    let union = Rect::new(two.x, two.y, two.width, empty.y + empty.height - two.y);
    assert!(marks.iter().any(|m| near(m.rect, one)), "{marks:?}");
    assert!(marks.iter().any(|m| near(m.rect, union)), "{marks:?}");
    engine
        .edit(&EditOp::ApplyRedactions { clean: false })
        .expect("apply");
    let target = scratch("turned.pdf");
    engine.save(Some(&target)).expect("save as");
    let text = page_text(&target);
    assert!(text.contains("KEEP-THIS"), "{text}");
    assert!(!text.contains("TURNED-ONE"), "{text}");
    assert!(!text.contains("TURNED-TWO"), "{text}");
}
