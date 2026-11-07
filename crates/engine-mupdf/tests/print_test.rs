// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Printing through the engine (ADR 0013): the tool thread drawing into a sink held in memory, the
//! PDF a system printer is handed, and the document's print permissions followed.

use engine_mupdf::EngineHandle;
use mupdf::pdf::{Encryption, PdfDocument, PdfWriteOptions, Permission};
use mupdf::{Colorspace, Matrix, TextPageFlags};
use pdf_core::print::{self, PrintBand, PrintSink, PrintTarget, SharedSink, Sheet};
use pdf_core::tools::ToolJob;
use pdf_core::{EngineCmd, EngineEvent};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const OWNER: &str = "owner";

fn scratch(name: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("print_{n}_{name}"));
    let _ = std::fs::remove_file(&path);
    path
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Three Letter pages, each with its own line of text.
fn three_pages() -> PathBuf {
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [4 0 R 6 0 R 8 0 R] /Count 3 >>".to_owned(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
    ];
    for (page, word) in ["FIRST", "SECOND", "THIRD"].iter().enumerate() {
        let stream = format!("BT /F1 40 Tf 72 {} Td ({word}) Tj ET", 700 - page * 100);
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
            5 + page * 2
        ));
        objects.push(format!(
            "<< /Length {} >>\nstream\n{stream}\nendstream",
            stream.len()
        ));
    }
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /ID [<0123456789abcdef0123456789abcdef> <0123456789abcdef0123456789abcdef>] >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    let path = scratch("three.pdf");
    std::fs::write(&path, out).expect("fixture");
    path
}

/// The three pages with these permissions and no password to open.
fn protected(permissions: Permission) -> PathBuf {
    let source = three_pages();
    let doc = PdfDocument::open(source.to_str().expect("utf-8")).expect("open");
    let mut options = PdfWriteOptions::default();
    options
        .set_encryption(Encryption::Aes256)
        .set_owner_password(OWNER)
        .set_permissions(permissions);
    let out = scratch("protected.pdf");
    doc.save_with_options(out.to_str().expect("utf-8"), options)
        .expect("protect");
    out
}

/// A band's raster width and height, and where it went.
type Band = (u32, u32, (i32, i32, u32, u32));

/// What a printer was told, in order; the sheet comes out as one RGB8 raster per page when every
/// band goes where it came from at the same scale.
#[derive(Debug, Default)]
struct Printed {
    calls: Vec<String>,
    bands: Vec<Band>,
    /// Each page's rows, by concatenating its bands.
    pages: Vec<Vec<u8>>,
}

#[derive(Debug)]
struct Memory {
    sheet: Sheet,
    printed: Arc<Mutex<Printed>>,
    /// Fails at this band.
    fail_at_band: Option<usize>,
}

impl Memory {
    fn sink(sheet: Sheet, fail_at_band: Option<usize>) -> (SharedSink, Arc<Mutex<Printed>>) {
        let printed = Arc::new(Mutex::new(Printed::default()));
        let sink = SharedSink::new(Self {
            sheet,
            printed: Arc::clone(&printed),
            fail_at_band,
        });
        (sink, printed)
    }

    fn call(&self, name: &str) {
        self.printed.lock().expect("lock").calls.push(name.into());
    }
}

impl PrintSink for Memory {
    fn start(&mut self) -> Result<Sheet, String> {
        self.call("start");
        Ok(self.sheet)
    }
    fn begin_page(&mut self) -> Result<(), String> {
        self.call("begin");
        self.printed.lock().expect("lock").pages.push(Vec::new());
        Ok(())
    }
    fn band(&mut self, band: PrintBand<'_>) -> Result<(), String> {
        let mut printed = self.printed.lock().expect("lock");
        if Some(printed.bands.len()) == self.fail_at_band {
            return Err("out of paper".to_owned());
        }
        assert_eq!(
            band.rgb.len(),
            band.width as usize * band.height as usize * 3
        );
        printed.bands.push((band.width, band.height, band.dest));
        if let Some(page) = printed.pages.last_mut() {
            page.extend_from_slice(band.rgb);
        }
        Ok(())
    }
    fn end_page(&mut self) -> Result<(), String> {
        self.call("end");
        Ok(())
    }
    fn finish(&mut self) -> Result<(), String> {
        self.call("finish");
        Ok(())
    }
    fn abort(&mut self) {
        self.call("abort");
    }
}

/// A4 at this resolution with no margin.
fn a4(dpi: u32) -> Sheet {
    let size = (dpi * 827 / 100, dpi * 1169 / 100);
    Sheet {
        dpi: (dpi, dpi),
        paper: size,
        printable: size,
        printable_at: (0, 0),
    }
}

fn opened(path: PathBuf) -> (EngineHandle, Receiver<EngineEvent>) {
    let (handle, rx) = EngineHandle::spawn();
    handle
        .send(EngineCmd::Open {
            path,
            password: None,
        })
        .expect("alive");
    loop {
        if matches!(
            rx.recv_timeout(Duration::from_secs(30)).expect("event"),
            EngineEvent::Opened { .. }
        ) {
            break;
        }
    }
    (handle, rx)
}

/// Runs a job on an open document and returns how it ended.
fn run(path: PathBuf, job: ToolJob) -> Result<Vec<PathBuf>, String> {
    let (handle, rx) = opened(path);
    handle
        .send(EngineCmd::RunTool { job_id: 7, job })
        .expect("alive");
    loop {
        if let EngineEvent::ToolDone { result, .. } =
            rx.recv_timeout(Duration::from_secs(120)).expect("event")
        {
            return result;
        }
    }
}

fn device(sink: SharedSink, pages: Vec<u32>) -> ToolJob {
    ToolJob::Print {
        pages,
        target: PrintTarget::Device(sink),
        low_res: false,
    }
}

fn file(path: &Path, pages: Vec<u32>) -> ToolJob {
    ToolJob::Print {
        pages,
        target: PrintTarget::File(path.to_owned()),
        low_res: false,
    }
}

#[test]
fn the_bands_of_a_page_are_the_page() {
    // dense_vector at 600 dpi takes more than one band; their rows must be the rows of one raster
    // of the whole page, seam for seam.
    let name = "dense_vector.pdf";
    let doc = PdfDocument::open(fixture(name).to_str().expect("utf-8")).expect("open");
    let page = doc.load_page(0).expect("page");
    let bounds = page.bounds().expect("bounds");
    let sheet = Sheet {
        dpi: (600, 600),
        paper: (6_000, 6_000),
        printable: (6_000, 6_000),
        printable_at: (0, 0),
    };
    let size = (bounds.x1 - bounds.x0, bounds.y1 - bounds.y0);
    let at = print::place(size, &sheet, print::MAX_DPI).expect("place");
    let (width, height) = at.render;
    assert!(
        at.bands(print::BAND_BYTES).len() > 1,
        "one band proves nothing: {width} x {height}"
    );

    let (sink, printed) = Memory::sink(sheet, None);
    run(fixture(name), device(sink, vec![0])).expect("print");

    let printed = printed.lock().expect("lock");
    assert_eq!(printed.calls, ["start", "begin", "end", "finish"]);
    let sum: u32 = printed.bands.iter().map(|b| b.1).sum();
    assert_eq!(sum, height);

    let [a, b, c, d, e, f] = at.matrix((bounds.x0, bounds.y0, bounds.x1, bounds.y1));
    let whole = page
        .to_pixmap(
            &Matrix::new(a, b, c, d, e, f),
            &Colorspace::device_rgb(),
            false,
            true,
        )
        .expect("whole");
    assert_eq!((whole.width(), whole.height()), (width, height));
    let n = whole.n() as usize;
    let stride = whole.stride() as usize;
    let printed_page = &printed.pages[0];
    assert_eq!(printed_page.len(), width as usize * height as usize * 3);
    let row_bytes = width as usize * n;
    let samples = whole.samples();
    // Where the bands meet: as clean as tile seams. Elsewhere MuPDF's flattening moves with the
    // translation, by a little, on very few pixels of this fixture's thousand overlapping curves.
    let edges: Vec<usize> = printed
        .bands
        .iter()
        .scan(0usize, |y, band| {
            *y += band.1 as usize;
            Some(*y)
        })
        .collect();
    let (mut seam_max, mut elsewhere, mut elsewhere_max) = (0, 0usize, 0);
    for row in 0..height as usize {
        let at_seam = edges.iter().any(|&e| row + 3 >= e && row < e + 3);
        let drawn = &printed_page[row * row_bytes..(row + 1) * row_bytes];
        let one = &samples[row * stride..row * stride + row_bytes];
        for (a, b) in drawn.iter().zip(one) {
            let delta = a.abs_diff(*b);
            if delta > 0 && at_seam {
                seam_max = seam_max.max(delta);
            } else if delta > 0 {
                elsewhere += 1;
                elsewhere_max = elsewhere_max.max(delta);
            }
        }
    }
    assert!(seam_max <= 4, "a seam shows: channel delta {seam_max}");
    assert!(elsewhere_max <= 32, "channel delta {elsewhere_max}");
    assert!(
        elsewhere <= printed_page.len() / 10_000,
        "{elsewhere} bytes differ of {}",
        printed_page.len()
    );
    assert!(
        samples.iter().any(|&v| v != 0xff),
        "the page must have drawn something"
    );
}

#[test]
fn a_page_larger_than_the_sheet_stays_on_it() {
    let sheet = a4(300);
    let (sink, printed) = Memory::sink(sheet, None);
    run(fixture("large_format_a0.pdf"), device(sink, vec![0])).expect("print");
    let printed = printed.lock().expect("lock");
    assert!(!printed.bands.is_empty());
    for &(_, _, (x, y, w, h)) in &printed.bands {
        assert!(x >= 0 && y >= 0);
        assert!(x as u32 + w <= sheet.printable.0);
        assert!(y as u32 + h <= sheet.printable.1);
    }
}

#[test]
fn pages_print_in_the_order_given() {
    let (sink, printed) = Memory::sink(a4(100), None);
    run(three_pages(), device(sink, vec![2, 0, 2])).expect("print");
    let printed = printed.lock().expect("lock");
    assert_eq!(printed.pages.len(), 3);
    assert_eq!(printed.pages[0], printed.pages[2]);
    assert_ne!(printed.pages[0], printed.pages[1]);
}

#[test]
fn a_printer_failure_aborts_the_job() {
    let (sink, printed) = Memory::sink(a4(100), Some(0));
    let err = run(fixture("minimal.pdf"), device(sink, vec![0])).expect_err("failed");
    assert!(err.contains("out of paper"), "{err}");
    let printed = printed.lock().expect("lock");
    assert_eq!(printed.calls, ["start", "begin", "abort"]);
}

#[test]
fn no_pages_nothing_starts() {
    let (sink, printed) = Memory::sink(a4(100), None);
    assert!(run(fixture("minimal.pdf"), device(sink, Vec::new())).is_err());
    assert!(printed.lock().expect("lock").calls.is_empty());
}

#[test]
fn no_print_permission_nothing_starts() {
    let path = protected(Permission::COPY);
    let (sink, printed) = Memory::sink(a4(100), None);
    let err = run(path, device(sink, vec![0])).expect_err("refused");
    assert!(err.contains("print"), "{err}");
    assert!(printed.lock().expect("lock").calls.is_empty());
    assert!(printed.lock().expect("lock").bands.is_empty());
}

#[test]
fn without_high_quality_the_raster_is_low_resolution() {
    let sheet = a4(600);
    let dest = print::place((612.0, 792.0), &sheet, print::MAX_DPI).expect("place");
    let path = protected(Permission::PRINT);
    let (sink, printed) = Memory::sink(sheet, None);
    run(path, device(sink, vec![0])).expect("print");
    let printed = printed.lock().expect("lock");
    let (width, _, band_dest) = printed.bands[0];
    // 600 dpi on the sheet from a quarter of the pixels across.
    assert!(
        width as f64 <= band_dest.2 as f64 * 150.0 / 600.0 + 1.0,
        "{width} for {}",
        band_dest.2
    );
    assert!(dest.render.0 > width);

    // With high quality the same document gets the printer's pixels.
    let path = protected(Permission::PRINT | Permission::PRINT_HQ);
    let (sink, printed) = Memory::sink(sheet, None);
    run(path, device(sink, vec![0])).expect("print");
    let printed = printed.lock().expect("lock");
    assert_eq!(printed.bands[0].0, printed.bands[0].2.2);
}

fn page_text(path: &Path, page: i32) -> String {
    let doc = PdfDocument::open(path.to_str().expect("utf-8")).expect("open");
    doc.load_page(page)
        .expect("page")
        .to_text_page(TextPageFlags::empty())
        .expect("text")
        .to_text()
        .expect("text")
}

#[test]
fn the_file_for_a_system_printer_has_the_pages_chosen() {
    let original = three_pages();
    let output = scratch("spool.pdf");
    let files = run(original.clone(), file(&output, vec![2, 0])).expect("print");
    assert_eq!(files, std::slice::from_ref(&output));
    let doc = PdfDocument::open(output.to_str().expect("utf-8")).expect("open");
    assert_eq!(doc.page_count().expect("count"), 2);
    assert!(!doc.needs_password().expect("password"));
    assert!(page_text(&output, 0).contains("THIRD"));
    assert!(page_text(&output, 1).contains("FIRST"));
    assert!(page_text(&original, 0).contains("FIRST"));
}

#[test]
fn without_high_quality_the_file_holds_images_of_the_pages() {
    let path = protected(Permission::PRINT);
    let output = scratch("spool_low.pdf");
    run(path, file(&output, vec![0])).expect("print");
    let doc = PdfDocument::open(output.to_str().expect("utf-8")).expect("open");
    assert_eq!(doc.page_count().expect("count"), 1);
    let page = doc.load_page(0).expect("page");
    let printed = page.bounds().expect("bounds");
    assert!(((printed.x1 - printed.x0) - 612.0).abs() < 0.5);
    assert!(((printed.y1 - printed.y0) - 792.0).abs() < 0.5);
    // The text is gone: only its picture goes to the printer's queue.
    assert!(page_text(&output, 0).trim().is_empty());
    // And the picture has ink.
    let pixmap = page
        .to_pixmap(
            &Matrix::new_scale(1.0, 1.0),
            &Colorspace::device_rgb(),
            false,
            true,
        )
        .expect("pixmap");
    assert!(pixmap.samples().iter().any(|&v| v < 0x80));
}
