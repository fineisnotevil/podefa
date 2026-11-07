// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Plan 0005 Phase 4 through the engine (rows AE and AF's engine half): exporting pages as PNG and
//! JPEG, making a PDF of images, and compressing, all on the tool thread; and the open document's
//! undo still working after a compress of its unsaved state (the trap of plan §3.4).

use engine_mupdf::EngineHandle;
use mupdf::pdf::{InsertImageOptions, PageImageSource, PdfDocument, PdfWriteOptions};
use mupdf::{Colorspace, Document, Image, Matrix, Rect, Size, TextPageFlags};
use pdf_core::annot::{Colour, EditOp, MarkupKind};
use pdf_core::tools::{ImageFormat, MergeInput, Preset, ToolJob, ToolSizes};
use pdf_core::{EngineCmd, EngineEvent};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

/// `pages` Letter pages, each a heading, a coloured band and forty lines of text, so a page has
/// both text and smooth areas when rendered.
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
        let mut content = format!(
            "0.2 0.4 0.8 rg 54 600 504 80 re f 0 g BT /F1 24 Tf 54 700 Td (page {p}) Tj ET"
        );
        // Forty lines of body text, as a text document has.
        for line in 0..40 {
            let _ = write!(
                content,
                " BT /F1 9 Tf 54 {} Td (Line {line} of page {p}: the quick brown fox jumps over \
                 the lazy dog.) Tj ET",
                560 - 12 * line
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

fn write(name: &str, bytes: &[u8]) -> PathBuf {
    let path = scratch(name);
    std::fs::write(&path, bytes).expect("write fixture");
    path
}

fn len(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |m| m.len())
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

/// Runs a job and waits for its end.
fn run(
    handle: &EngineHandle,
    rx: &Receiver<EngineEvent>,
    job_id: u64,
    job: ToolJob,
) -> (Result<Vec<PathBuf>, String>, Option<ToolSizes>) {
    handle
        .send(EngineCmd::RunTool { job_id, job })
        .expect("actor");
    loop {
        if let EngineEvent::ToolDone {
            job_id: id,
            result,
            sizes,
        } = next(rx)
            && id == job_id
        {
            return (result, sizes);
        }
    }
}

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

/// A scanned document: each page of `text_pdf` rendered at 300 dpi, stored as a JPEG of quality
/// 90 filling a Letter page, as a scanner writes it.
fn scanned_pdf(name: &str, pages: usize) -> PathBuf {
    let source = PdfDocument::from_bytes(&text_pdf(pages)).expect("source");
    let mut scan = PdfDocument::new();
    for p in 0..pages as i32 {
        let pixmap = source
            .load_page(p)
            .expect("page")
            .to_pixmap(
                &Matrix::new_scale(300.0 / 72.0, 300.0 / 72.0),
                &Colorspace::device_rgb(),
                false,
                true,
            )
            .expect("render");
        let jpeg = scratch(&format!("{name}_{p}.jpg"));
        pixmap
            .save_as_jpeg(jpeg.to_str().expect("utf-8"), 90)
            .expect("jpeg");
        let image = Image::from_bytes(&std::fs::read(&jpeg).expect("read")).expect("image");
        let mut page = scan.new_page(Size::new(612.0, 792.0)).expect("page");
        page.insert_image(
            &mut scan,
            Rect::new(0.0, 0.0, 612.0, 792.0),
            PageImageSource::Image(&image),
            InsertImageOptions::default(),
        )
        .expect("insert");
    }
    let path = scratch(name);
    let mut options = PdfWriteOptions::default();
    options.set_compress(true);
    scan.save_with_options(path.to_str().expect("utf-8"), options)
        .expect("save scan");
    path
}

#[test]
fn pages_export_as_png_and_jpeg_at_their_resolution() {
    let path = write("convert_export.pdf", &text_pdf(3));
    let (handle, rx) = spawn_open(&path);
    let png = [scratch("convert_p3.png"), scratch("convert_p1.png")];
    let t = Instant::now();
    let (result, _) = run(
        &handle,
        &rx,
        1,
        ToolJob::ExportImages {
            pages: vec![(2, png[0].clone()), (0, png[1].clone())],
            format: ImageFormat::Png,
            dpi: 150,
        },
    );
    let per_page = t.elapsed().as_secs_f64() * 1000.0 / 2.0;
    assert_eq!(result, Ok(png.to_vec()));
    println!("[convert] PNG at 150 dpi: {per_page:.1} ms per page");
    // Row AF's bound, for the optimised build.
    if !cfg!(debug_assertions) {
        assert!(per_page <= 300.0, "{per_page:.1} ms per page");
    }
    for file in &png {
        let bytes = std::fs::read(file).expect("png");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        let image = Image::from_bytes(&bytes).expect("decodes");
        assert_eq!((image.width(), image.height()), (1275, 1650));
        assert_eq!(image.resolution(), (150, 150));
        assert!(!file.with_extension("png.part").exists());
    }

    let jpg = scratch("convert_p2.jpg");
    let (result, _) = run(
        &handle,
        &rx,
        2,
        ToolJob::ExportImages {
            pages: vec![(1, jpg.clone())],
            format: ImageFormat::Jpeg { quality: 85 },
            dpi: 96,
        },
    );
    assert_eq!(result, Ok(vec![jpg.clone()]));
    let bytes = std::fs::read(&jpg).expect("jpg");
    assert_eq!(&bytes[..3], b"\xff\xd8\xff");
    let image = Image::from_bytes(&bytes).expect("decodes");
    assert_eq!((image.width(), image.height()), (816, 1056));
}

#[test]
fn images_become_pages_of_their_size_stored_as_they_are() {
    let path = write("convert_album_src.pdf", &text_pdf(3));
    let (handle, rx) = spawn_open(&path);
    let files = [
        scratch("convert_album_0.jpg"),
        scratch("convert_album_1.png"),
        scratch("convert_album_2.jpg"),
    ];
    for (job_id, (page, file)) in files
        .iter()
        .enumerate()
        .map(|(i, f)| (i as u32, f))
        .enumerate()
    {
        let format = if file.extension().is_some_and(|e| e == "png") {
            ImageFormat::Png
        } else {
            ImageFormat::Jpeg { quality: 85 }
        };
        let (result, _) = run(
            &handle,
            &rx,
            job_id as u64 + 1,
            ToolJob::ExportImages {
                pages: vec![(page, file.clone())],
                format,
                dpi: 150,
            },
        );
        assert!(result.is_ok(), "{result:?}");
    }
    let output = scratch("convert_album.pdf");
    let (result, _) = run(
        &handle,
        &rx,
        9,
        ToolJob::ImagesToPdf {
            images: files.to_vec(),
            output: output.clone(),
        },
    );
    assert_eq!(result, Ok(vec![output.clone()]));
    let doc = Document::open(output.to_str().expect("utf-8")).expect("open");
    assert_eq!(doc.page_count().expect("count"), 3);
    for p in 0..3 {
        // 150 dpi images of Letter pages make Letter pages again.
        let b = doc.load_page(p).expect("page").bounds().expect("bounds");
        assert!(
            (b.x1 - 612.0).abs() < 0.5 && (b.y1 - 792.0).abs() < 0.5,
            "{b:?}"
        );
    }
    // Row AF: stored compressed, the JPEGs as they are.
    let inputs: u64 = files.iter().map(|f| len(f)).sum();
    let ratio = len(&output) as f64 / inputs as f64;
    println!(
        "[convert] images to PDF: {} KiB of images, {} KiB of PDF ({ratio:.2}x)",
        inputs / 1024,
        len(&output) / 1024
    );
    assert!(ratio <= 1.2, "{ratio:.2}x the input files");
    // A file that is not an image fails the job and leaves nothing.
    let bad = write("convert_not_an_image.png", b"not an image");
    let output = scratch("convert_album_bad.pdf");
    let (result, _) = run(
        &handle,
        &rx,
        10,
        ToolJob::ImagesToPdf {
            images: vec![files[0].clone(), bad],
            output: output.clone(),
        },
    );
    assert!(result.is_err());
    assert!(!output.exists() && !scratch("convert_album_bad.pdf.part").exists());
}

#[test]
fn compress_is_never_larger_and_balanced_halves_a_scan() {
    // Lossless on an uncompressed text document: smaller, same text.
    let path = write("convert_plain.pdf", &text_pdf(40));
    let (handle, rx) = spawn_open(&path);
    let output = scratch("convert_plain_small.pdf");
    let (result, sizes) = run(
        &handle,
        &rx,
        1,
        ToolJob::Compress {
            input: MergeInput::Current,
            preset: Preset::Lossless,
            output: output.clone(),
        },
    );
    assert_eq!(result, Ok(vec![output.clone()]));
    let sizes = sizes.expect("sizes");
    // The document has no unsaved changes, so the file itself was read.
    assert_eq!(sizes.before, len(&path));
    assert_eq!(sizes.after, len(&output));
    assert!(
        sizes.after < sizes.before && !sizes.kept_original,
        "{sizes:?}"
    );
    assert_eq!(texts(&output), texts(&path));
    println!(
        "[convert] lossless, text: {} -> {} bytes",
        sizes.before, sizes.after
    );

    // Lossless on what it wrote: nothing more to gain, so the copy is the input itself.
    drop(handle);
    let (handle, rx) = spawn_open(&output);
    let again = scratch("convert_plain_again.pdf");
    let (_, sizes) = run(
        &handle,
        &rx,
        2,
        ToolJob::Compress {
            input: MergeInput::Current,
            preset: Preset::Lossless,
            output: again.clone(),
        },
    );
    let sizes = sizes.expect("sizes");
    assert!(sizes.after <= sizes.before, "{sizes:?}");
    if sizes.kept_original {
        assert_eq!(
            std::fs::read(&again).expect("read"),
            std::fs::read(&output).expect("read")
        );
    }
    drop(handle);

    // Balanced on a 300 dpi scan: at least half off (row AE), the pages still there.
    let scan = scanned_pdf("convert_scan.pdf", 4);
    let (handle, rx) = spawn_open(&scan);
    for (job_id, preset) in [(3, Preset::Balanced), (4, Preset::Strong)] {
        let output = scratch(&format!("convert_scan_{preset:?}.pdf"));
        let t = Instant::now();
        let (result, sizes) = run(
            &handle,
            &rx,
            job_id,
            ToolJob::Compress {
                input: MergeInput::Current,
                preset,
                output: output.clone(),
            },
        );
        assert_eq!(result, Ok(vec![output.clone()]));
        let sizes = sizes.expect("sizes");
        println!(
            "[convert] {preset:?}, 4-page scan: {} -> {} KiB in {:.0} ms",
            sizes.before / 1024,
            sizes.after / 1024,
            t.elapsed().as_secs_f64() * 1000.0
        );
        assert!(sizes.after * 2 <= sizes.before, "{preset:?}: {sizes:?}");
        let doc = Document::open(output.to_str().expect("utf-8")).expect("open");
        assert_eq!(doc.page_count().expect("count"), 4);
        let b = doc.load_page(3).expect("page").bounds().expect("bounds");
        assert_eq!((b.x1, b.y1), (612.0, 792.0));
    }
}

/// Plan §3.4: a garbage-collecting save of the open document leaves undo reporting success and
/// changing nothing. Compress collects garbage, so it must work on a document of its own: after
/// compressing the open document's unsaved state, undoing the unsaved edit still removes it.
#[test]
fn undo_still_works_after_compressing_the_unsaved_state() {
    let path = write("convert_undo.pdf", &text_pdf(2));
    let (handle, rx) = spawn_open(&path);
    handle
        .send(EngineCmd::Edit {
            op: EditOp::AddMarkup {
                page: 0,
                kind: MarkupKind::Highlight,
                rects: vec![pdf_core::Rect::new(54.0, 80.0, 80.0, 24.0)],
                colour: Colour::new(1.0, 0.85, 0.0),
                author: None,
            },
            edit_id: 1,
            generation: 0,
        })
        .expect("actor");
    while !matches!(next(&rx), EngineEvent::History { .. }) {}
    let output = scratch("convert_undo_small.pdf");
    let (result, sizes) = run(
        &handle,
        &rx,
        1,
        ToolJob::Compress {
            input: MergeInput::Current,
            preset: Preset::Lossless,
            output: output.clone(),
        },
    );
    assert_eq!(result, Ok(vec![output.clone()]));
    // The unsaved highlight is in the copy.
    let copy = PdfDocument::open(output.to_str().expect("utf-8")).expect("copy");
    let highlights = |doc: &PdfDocument| {
        mupdf::pdf::PdfPage::try_from(doc.load_page(0).expect("page"))
            .expect("pdf page")
            .annotations()
            .count()
    };
    assert_eq!(highlights(&copy), 1);
    assert!(
        sizes.is_some_and(|s| s.before != len(&path)),
        "the state, not the file, was read"
    );

    // Undo takes the highlight off the open document.
    handle
        .send(EngineCmd::Undo { generation: 0 })
        .expect("actor");
    while !matches!(next(&rx), EngineEvent::History { .. }) {}
    handle
        .send(EngineCmd::Annotations {
            page: 0,
            generation: 0,
        })
        .expect("actor");
    let count = loop {
        if let EngineEvent::Annotations { page: 0, items, .. } = next(&rx) {
            break items.len();
        }
    };
    assert_eq!(count, 0, "undo removed the highlight");
}

/// A file MuPDF has to repair to open says so, cannot be saved in place, and Save As writes a copy
/// that opens without a repair (the repaired copy of Phase 4).
#[test]
fn a_repaired_file_says_so_and_save_as_writes_a_clean_copy() {
    let good = text_pdf(2);
    // The cross-reference table moved by a few bytes: every offset in it is wrong.
    let text = String::from_utf8(good.clone()).expect("ascii");
    let at = text.rfind("startxref\n").expect("startxref") + "startxref\n".len();
    let offset: usize = text[at..]
        .lines()
        .next()
        .expect("offset")
        .parse()
        .expect("number");
    let damaged = format!("{}{}\n%%EOF\n", &text[..at], offset + 7);
    let path = write("convert_damaged.pdf", damaged.as_bytes());
    let fine = write("convert_fine.pdf", &good);

    let mut engine = engine_mupdf::MupdfEngine::new();
    engine
        .open_document_with_password(&fine, None)
        .expect("open");
    assert!(!engine.was_repaired());
    engine
        .open_document_with_password(&path, None)
        .expect("a damaged file still opens");
    assert!(engine.was_repaired());
    assert_eq!(engine.save(None), Err(pdf_core::SaveError::NeedsSaveAs));
    let copy = scratch("convert_repaired_copy.pdf");
    engine.save(Some(&copy)).expect("save as");
    drop(engine);

    let mut engine = engine_mupdf::MupdfEngine::new();
    engine
        .open_document_with_password(&copy, None)
        .expect("open the copy");
    assert!(!engine.was_repaired(), "the copy needs no repair");
    assert_eq!(texts(&copy).len(), 2);
}
