// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! The tool thread (Plan 0005, §4.2): merging and extracting pages into new files, exporting pages
//! as images, making a PDF of images and compressing (Phase 4), and exporting as Word (Phase 5).
//!
//! Jobs run here, on documents of their own, never on the open one: rewriting a file with garbage
//! collection breaks the open document's undo journal (Plan 0005 §3.4), and a merge of thousands of
//! pages must not hold up rendering. The open document comes in as a snapshot of its current state,
//! unsaved changes included (§0, question 3). Every output is written next to its target as
//! `<name>.part`, and only when the whole job has succeeded are they renamed to the names the user
//! chose: a cancelled or failed job deletes its `.part` files and leaves any file it would have
//! replaced untouched.

#[cfg(feature = "docx")]
use mupdf::DocumentWriter;
use mupdf::pdf::{
    InsertImageOptions, InsertPdfOptions, PageImageSource, PageSelection, PdfDocument,
    PdfWriteOptions,
};
use mupdf::{Colorspace, Device, Image, Matrix, Pixmap, Rect, Size};
use pdf_core::print::{self, PrintBand, PrintSink, PrintTarget};
use pdf_core::tools::{ImageFormat, MergeInput, ToolJob, ToolSizes};
use pdf_core::{EngineEvent, TILE_BLEED_PX};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

/// A job and what it reads: the open document's bytes when the job needs them.
struct Run {
    job_id: u64,
    job: ToolJob,
    current: Option<Arc<Vec<u8>>>,
    /// The open document's password, for a protected one (Plan 0007 Phase 5).
    password: Option<String>,
    /// The open document had a redaction applied (Plan 0009 §3.6): its snapshot still holds what
    /// was removed, so it is never written out as it is.
    redacted: bool,
}

/// The handle the actor keeps. Dropping it stops and joins the thread.
pub(crate) struct ToolThread {
    tx: Option<Sender<Run>>,
    /// The job to stop at its next step, 0 for none.
    cancelled: Arc<AtomicU64>,
    handle: Option<JoinHandle<()>>,
}

impl ToolThread {
    pub(crate) fn start(events: Sender<EngineEvent>) -> Self {
        let (tx, rx) = mpsc::channel::<Run>();
        let cancelled = Arc::new(AtomicU64::new(0));
        let shared = Arc::clone(&cancelled);
        let handle = thread::Builder::new()
            .name("mupdf-tools".to_string())
            .spawn(move || {
                while let Ok(run) = rx.recv() {
                    let (result, sizes) = match execute(&run, &shared, &events) {
                        Ok((files, sizes)) => (Ok(files), sizes),
                        Err(err) => (Err(err), None),
                    };
                    let _ = events.send(EngineEvent::ToolDone {
                        job_id: run.job_id,
                        result,
                        sizes,
                    });
                }
            })
            .ok();
        Self {
            tx: Some(tx),
            cancelled,
            handle,
        }
    }

    /// Queues a job; `current` is the open document as bytes, for jobs that read it, `password`
    /// the one it was opened with, and `redacted` whether a redaction was applied to it.
    pub(crate) fn run(
        &self,
        job_id: u64,
        job: ToolJob,
        current: Option<Vec<u8>>,
        password: Option<String>,
        redacted: bool,
    ) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Run {
                job_id,
                job,
                current: current.map(Arc::new),
                password,
                redacted,
            });
        }
    }

    /// Stops a job at its next step.
    pub(crate) fn cancel(&self, job_id: u64) {
        self.cancelled.store(job_id, Ordering::SeqCst);
    }
}

impl Drop for ToolThread {
    fn drop(&mut self) {
        self.cancelled.store(u64::MAX, Ordering::SeqCst);
        self.tx = None;
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Whether a job reads the open document.
pub(crate) fn needs_current(job: &ToolJob) -> bool {
    match job {
        ToolJob::Merge { inputs, .. } => inputs.contains(&MergeInput::Current),
        ToolJob::Extract { .. }
        | ToolJob::ExportImages { .. }
        | ToolJob::ExportWord { .. }
        | ToolJob::Print { .. } => true,
        ToolJob::ImagesToPdf { .. } => false,
        ToolJob::Compress { input, .. } => *input == MergeInput::Current,
    }
}

/// How the tools write: a new file, compressed and with duplicate objects merged. Garbage
/// collection is safe here, on a document of the tool's own.
fn write_options() -> PdfWriteOptions {
    let mut options = PdfWriteOptions::default();
    options
        .set_garbage_level(4)
        .set_compress(true)
        .set_compress_images(true)
        .set_compress_fonts(true);
    options
}

fn part_path(output: &Path) -> PathBuf {
    let mut name = output.as_os_str().to_owned();
    name.push(".part");
    PathBuf::from(name)
}

/// Writes `bytes` to the `.part` file of `output`, then renames it to `output`: a file that was
/// there stays whole until the new one is.
pub(crate) fn write_replacing(output: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let part = part_path(output);
    std::fs::write(&part, bytes)
        .and_then(|()| std::fs::rename(&part, output))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&part);
        })
}

/// Writes a document to the `.part` file of `output`, which it returns, with this app as its
/// `/Producer`.
fn write_part(doc: &mut PdfDocument, output: &Path) -> Result<PathBuf, String> {
    crate::edit::set_producer(doc).map_err(|e| format!("{}: {e}", output.display()))?;
    let part = part_path(output);
    let name = part
        .to_str()
        .ok_or_else(|| format!("not a UTF-8 path: {}", output.display()))?;
    match doc.save_with_options(name, write_options()) {
        Ok(()) => Ok(part),
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(format!("{}: {e}", output.display()))
        }
    }
}

fn utf8(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("not a UTF-8 path: {}", path.display()))
}

/// Renders one page at `dpi` and writes it to the `.part` file of `output`, which it returns.
fn export_page(
    doc: &PdfDocument,
    page: u32,
    output: &Path,
    format: ImageFormat,
    dpi: u32,
) -> Result<PathBuf, String> {
    let fail = |e: mupdf::Error| format!("page {}: {e}", page + 1);
    let scale = dpi.clamp(1, 2400) as f32 / 72.0;
    let mut pixmap = doc
        .load_page(page as i32)
        .map_err(fail)?
        .to_pixmap(
            &Matrix::new_scale(scale, scale),
            &Colorspace::device_rgb(),
            false,
            true,
        )
        .map_err(fail)?;
    // The files say their resolution, so a page made of one again has the size it had.
    let dpi = dpi.clamp(1, 2400) as i32;
    pixmap.set_resolution(dpi, dpi);
    let part = part_path(output);
    let name = utf8(&part)?;
    let written = match format {
        ImageFormat::Png => pixmap.save_as(name, mupdf::ImageFormat::PNG),
        ImageFormat::Jpeg { quality } => pixmap.save_as_jpeg(name, quality),
    };
    match written {
        Ok(()) => Ok(part),
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(format!("{}: {e}", output.display()))
        }
    }
}

/// Adds one image file to `doc` as a page of the image's size at its resolution (72 dpi when it
/// names none). The file's bytes go in as they are where MuPDF can keep them (JPEG).
fn add_image_page(doc: &mut PdfDocument, path: &Path) -> Result<(), String> {
    let fail = |e: mupdf::Error| format!("{}: {e}", path.display());
    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let image = Image::from_bytes(&data).map_err(fail)?;
    let (xres, yres) = image.resolution();
    let points =
        |pixels: u32, res: i32| pixels as f32 * 72.0 / if res > 0 { res as f32 } else { 72.0 };
    let (w, h) = (points(image.width(), xres), points(image.height(), yres));
    if !(w >= 1.0 && h >= 1.0) {
        return Err(format!("{}: the image is empty", path.display()));
    }
    let mut page = doc.new_page(Size::new(w, h)).map_err(fail)?;
    page.insert_image(
        doc,
        Rect::new(0.0, 0.0, w, h),
        PageImageSource::Image(&image),
        InsertImageOptions::default(),
    )
    .map_err(fail)?;
    Ok(())
}

/// Writes every page of `doc` to the `.part` file of `output` as DOCX, through MuPDF's DOCX writer
/// (the `extract` library): text in reading order, with images. `progress` follows each page and
/// `stop` is asked between pages. The writer is always closed through the `fz_try` wrapper, also
/// when stopping, and a part file left by a stop or a failure is deleted. Built with the `docx`
/// feature only.
#[cfg(feature = "docx")]
fn export_word(
    doc: &PdfDocument,
    output: &Path,
    stop: &dyn Fn() -> bool,
    progress: &dyn Fn(usize, usize),
) -> Result<PathBuf, String> {
    let part = part_path(output);
    let fail = |e: mupdf::Error| format!("{}: {e}", output.display());
    let count = doc.page_count().map_err(fail)?.max(0) as usize;
    let mut writer = DocumentWriter::new(utf8(&part)?, "docx", "").map_err(fail)?;
    let mut written = (|| -> Result<(), String> {
        for p in 0..count {
            if stop() {
                return Err("cancelled".to_owned());
            }
            let page = doc
                .load_page(p as i32)
                .map_err(|e| format!("page {}: {e}", p + 1))?;
            let bounds = page.bounds().map_err(|e| format!("page {}: {e}", p + 1))?;
            let device = writer.begin_page(bounds).map_err(fail)?;
            let run = page.run(&device, &Matrix::IDENTITY);
            writer.end_page(device).map_err(fail)?;
            run.map_err(|e| format!("page {}: {e}", p + 1))?;
            progress(p + 1, count + 1);
        }
        Ok(())
    })();
    let closed = writer.close().map_err(fail);
    if written.is_ok() {
        written = closed;
    }
    match written {
        Ok(()) => Ok(part),
        Err(err) => {
            let _ = std::fs::remove_file(&part);
            Err(err)
        }
    }
}

/// Copies pages of the open document, with their annotations, into a new document.
fn copy_pages(source: &PdfDocument, pages: &[u32]) -> Result<PdfDocument, String> {
    let mut part = PdfDocument::new();
    let options = InsertPdfOptions {
        source_pages: PageSelection::Pages(pages.iter().map(|&p| p as usize).collect()),
        ..InsertPdfOptions::default()
    };
    part.insert_pdf(source, options)
        .map_err(|e| e.to_string())?;
    // Links to pages left out of the part are dropped.
    let pairs: Vec<(i32, i32)> = pages
        .iter()
        .enumerate()
        .map(|(to, &from)| (from as i32, to as i32))
        .collect();
    crate::edit::copy_annotations(&mut part, Some(source), &pairs, &|p| {
        pages.iter().position(|&q| q as i32 == p).map(|i| i as i32)
    })
    .map_err(|e| e.to_string())?;
    Ok(part)
}

/// Makes a new PDF of the images of `pages` at [`print::LOW_RES_DPI`], for a system that prints
/// PDFs when the document withholds high-quality printing: the pages themselves would hand it what
/// the permission keeps back.
fn raster_copy(
    source: &PdfDocument,
    pages: &[u32],
    stop: &dyn Fn() -> bool,
    progress: &dyn Fn(usize, usize),
) -> Result<PdfDocument, String> {
    let mut out = PdfDocument::new();
    let scale = print::LOW_RES_DPI as f32 / 72.0;
    for (done, &p) in pages.iter().enumerate() {
        if stop() {
            return Err("cancelled".to_owned());
        }
        let fail = |e: mupdf::Error| format!("page {}: {e}", p + 1);
        let page = source.load_page(p as i32).map_err(fail)?;
        let bounds = page.bounds().map_err(fail)?;
        let pixmap = page
            .to_pixmap(
                &Matrix::new_scale(scale, scale),
                &Colorspace::device_rgb(),
                false,
                true,
            )
            .map_err(fail)?;
        let image = Image::from_pixmap(&pixmap).map_err(fail)?;
        let (w, h) = (bounds.x1 - bounds.x0, bounds.y1 - bounds.y0);
        let mut sheet = out.new_page(Size::new(w, h)).map_err(fail)?;
        sheet
            .insert_image(
                &mut out,
                Rect::new(0.0, 0.0, w, h),
                PageImageSource::Image(&image),
                InsertImageOptions::default(),
            )
            .map_err(fail)?;
        progress(done + 1, pages.len() + 1);
    }
    Ok(out)
}

/// Draws one page into the sink, band by band. Each band is drawn with bleed rows, as a tile is,
/// so an anti-aliased clip edge does not show as a line where two bands meet.
fn print_page(
    source: &PdfDocument,
    p: u32,
    sink: &mut dyn PrintSink,
    sheet: &print::Sheet,
    dpi_limit: u32,
    stop: &dyn Fn() -> bool,
) -> Result<(), String> {
    let fail = |e: mupdf::Error| format!("page {}: {e}", p + 1);
    let page = source.load_page(p as i32).map_err(fail)?;
    let bounds = page.bounds().map_err(fail)?;
    let at = print::place(
        (bounds.x1 - bounds.x0, bounds.y1 - bounds.y0),
        sheet,
        dpi_limit,
    )
    .ok_or_else(|| format!("page {} has no size", p + 1))?;
    let list = page.to_display_list(true).map_err(fail)?;
    let [a, b, c, d, e, f] = at.matrix((bounds.x0, bounds.y0, bounds.x1, bounds.y1));
    sink.begin_page()?;
    let colorspace = Colorspace::device_rgb();
    let width = at.render.0;
    for span in at.bands(print::BAND_BYTES) {
        if stop() {
            return Err("cancelled".to_owned());
        }
        let bleed = TILE_BLEED_PX as u32;
        let top = span.y.min(bleed);
        let bottom = bleed.min(at.render.1 - span.y - span.height);
        let rows = span.height + top + bottom;
        let mut pixmap =
            Pixmap::new(&colorspace, 0, 0, width as i32, rows as i32, false).map_err(fail)?;
        pixmap.clear().map_err(fail)?;
        let device = Device::from_pixmap(&pixmap).map_err(fail)?;
        let ctm = Matrix::new(a, b, c, d, e, f - (span.y - top) as f32);
        list.run(
            &device,
            &ctm,
            Rect::new(0.0, 0.0, width as f32, rows as f32),
        )
        .map_err(fail)?;
        drop(device);
        let band = crate::pool::tight_copy(&pixmap, (0, top as usize), width, span.height);
        sink.band(PrintBand {
            rgb: &band.data,
            width,
            height: span.height,
            dest: (at.dest.0, span.dest_y, at.dest.2, span.dest_height),
        })?;
    }
    sink.end_page()
}

/// Sends `pages` to the sink; a failure or a stop aborts the printer's job, so nothing half done
/// comes out of it.
fn print_to_sink(
    source: &PdfDocument,
    pages: &[u32],
    sink: &mut dyn PrintSink,
    low_res: bool,
    stop: &dyn Fn() -> bool,
    progress: &dyn Fn(usize, usize),
) -> Result<(), String> {
    let sheet = sink.start()?;
    let limit = if low_res {
        print::LOW_RES_DPI
    } else {
        print::MAX_DPI
    };
    let printed = (|| {
        for (done, &p) in pages.iter().enumerate() {
            if stop() {
                return Err("cancelled".to_owned());
            }
            print_page(source, p, sink, &sheet, limit, stop)?;
            progress(done + 1, pages.len());
        }
        sink.finish()
    })();
    if printed.is_err() {
        sink.abort();
    }
    printed
}

fn open(
    input: &MergeInput,
    current: Option<&Arc<Vec<u8>>>,
    password: Option<&str>,
) -> Result<PdfDocument, String> {
    let mut doc = match input {
        MergeInput::Current => {
            let bytes = current.ok_or("the open document is not available")?;
            PdfDocument::from_bytes(bytes).map_err(|e| e.to_string())?
        }
        MergeInput::File(path) => {
            let name = path
                .to_str()
                .ok_or_else(|| format!("not a UTF-8 path: {}", path.display()))?;
            PdfDocument::open(name).map_err(|e| format!("{}: {e}", path.display()))?
        }
    };
    let name = match input {
        MergeInput::Current => "the open document".to_owned(),
        MergeInput::File(path) => path.display().to_string(),
    };
    unlock(&mut doc, password, &name)?;
    Ok(doc)
}

/// A protected document is read with the open document's password (the open document itself, as
/// bytes or as its file); any other protected one is refused.
fn unlock(doc: &mut PdfDocument, password: Option<&str>, name: &str) -> Result<(), String> {
    if !doc.needs_password().unwrap_or(false) {
        return Ok(());
    }
    if let Some(password) = password
        && doc.authenticate(password).unwrap_or(false)
    {
        return Ok(());
    }
    Err(format!("{name} is password-protected"))
}

/// Runs a job; returns the files it wrote and, for Compress, the sizes.
fn execute(
    run: &Run,
    cancelled: &AtomicU64,
    events: &Sender<EngineEvent>,
) -> Result<(Vec<PathBuf>, Option<ToolSizes>), String> {
    let stop = || {
        let id = cancelled.load(Ordering::SeqCst);
        id == run.job_id || id == u64::MAX
    };
    let progress = |done: usize, total: usize| {
        let _ = events.send(EngineEvent::ToolProgress {
            job_id: run.job_id,
            done: done as u32,
            total: total as u32,
        });
    };
    // `(part file, output)` for every file written so far.
    let mut written: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut sizes = None;
    let result = (|| -> Result<(), String> {
        match &run.job {
            ToolJob::Merge { inputs, output } => {
                if inputs.is_empty() {
                    return Err("nothing to merge".to_owned());
                }
                let mut merged = PdfDocument::new();
                // One more step for writing the file.
                let total = inputs.len() + 1;
                for (done, input) in inputs.iter().enumerate() {
                    if stop() {
                        return Err("cancelled".to_owned());
                    }
                    let source = open(input, run.current.as_ref(), run.password.as_deref())?;
                    let offset = merged.page_count().map_err(|e| e.to_string())?;
                    merged
                        .insert_pdf(&source, InsertPdfOptions::default())
                        .map_err(|e| e.to_string())?;
                    // Annotations and links come along (MuPDF's graft leaves them out); links
                    // stay within this input's pages.
                    let n = source.page_count().map_err(|e| e.to_string())?;
                    let pairs: Vec<(i32, i32)> = (0..n).map(|p| (p, offset + p)).collect();
                    crate::edit::copy_annotations(&mut merged, Some(&source), &pairs, &|p| {
                        (p < n).then_some(offset + p)
                    })
                    .map_err(|e| e.to_string())?;
                    progress(done + 1, total);
                }
                if stop() {
                    return Err("cancelled".to_owned());
                }
                written.push((write_part(&mut merged, output)?, output.clone()));
                progress(total, total);
            }
            ToolJob::Extract { parts } => {
                let source = open(
                    &MergeInput::Current,
                    run.current.as_ref(),
                    run.password.as_deref(),
                )?;
                for (done, (pages, output)) in parts.iter().enumerate() {
                    if stop() {
                        return Err("cancelled".to_owned());
                    }
                    let mut part = copy_pages(&source, pages)?;
                    written.push((write_part(&mut part, output)?, output.clone()));
                    progress(done + 1, parts.len());
                }
            }
            ToolJob::ExportImages { pages, format, dpi } => {
                let source = open(
                    &MergeInput::Current,
                    run.current.as_ref(),
                    run.password.as_deref(),
                )?;
                for (done, (page, output)) in pages.iter().enumerate() {
                    if stop() {
                        return Err("cancelled".to_owned());
                    }
                    written.push((
                        export_page(&source, *page, output, *format, *dpi)?,
                        output.clone(),
                    ));
                    progress(done + 1, pages.len());
                }
            }
            ToolJob::ImagesToPdf { images, output } => {
                if images.is_empty() {
                    return Err("no images chosen".to_owned());
                }
                let mut doc = PdfDocument::new();
                let total = images.len() + 1;
                for (done, image) in images.iter().enumerate() {
                    if stop() {
                        return Err("cancelled".to_owned());
                    }
                    add_image_page(&mut doc, image)?;
                    progress(done + 1, total);
                }
                if stop() {
                    return Err("cancelled".to_owned());
                }
                written.push((write_part(&mut doc, output)?, output.clone()));
                progress(total, total);
            }
            ToolJob::Compress {
                input,
                preset,
                output,
            } => {
                let bytes = match input {
                    MergeInput::Current => run
                        .current
                        .as_ref()
                        .map(|b| b.as_slice().to_vec())
                        .ok_or("the open document is not available")?,
                    MergeInput::File(path) => {
                        std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?
                    }
                };
                progress(0, 2);
                let mut doc = PdfDocument::from_bytes(&bytes).map_err(|e| e.to_string())?;
                unlock(&mut doc, run.password.as_deref(), "the document")?;
                if let Some((above, to, quality)) = preset.images() {
                    doc.rewrite_images(above, to, quality)
                        .map_err(|e| e.to_string())?;
                }
                if stop() {
                    return Err("cancelled".to_owned());
                }
                progress(1, 2);
                let part = write_part(&mut doc, output)?;
                let after = std::fs::metadata(&part).map_or(u64::MAX, |m| m.len());
                let before = bytes.len() as u64;
                // Never a larger file: the input itself is the better copy then - unless it is a
                // redacted document's snapshot, which still holds what the redaction removed.
                let redacted_input = run.redacted && *input == MergeInput::Current;
                let kept_original = after >= before && !redacted_input;
                if kept_original {
                    if let Err(e) = std::fs::write(&part, &bytes) {
                        let _ = std::fs::remove_file(&part);
                        return Err(format!("{}: {e}", output.display()));
                    }
                }
                written.push((part, output.clone()));
                sizes = Some(ToolSizes {
                    before,
                    after: if kept_original { before } else { after },
                    kept_original,
                });
                progress(2, 2);
            }
            ToolJob::Print {
                pages,
                target,
                low_res,
            } => {
                if pages.is_empty() {
                    return Err("no pages to print".to_owned());
                }
                let source = open(
                    &MergeInput::Current,
                    run.current.as_ref(),
                    run.password.as_deref(),
                )?;
                match target {
                    PrintTarget::Device(sink) => {
                        let mut sink = sink
                            .0
                            .lock()
                            .map_err(|_| "the printer's job failed before".to_owned())?;
                        print_to_sink(&source, pages, sink.as_mut(), *low_res, &stop, &|d, t| {
                            progress(d, t);
                        })?;
                    }
                    PrintTarget::File(output) => {
                        let mut copy = if *low_res {
                            raster_copy(&source, pages, &stop, &|d, t| progress(d, t))?
                        } else {
                            copy_pages(&source, pages)?
                        };
                        if stop() {
                            return Err("cancelled".to_owned());
                        }
                        written.push((write_part(&mut copy, output)?, output.clone()));
                        progress(pages.len() + 1, pages.len() + 1);
                    }
                }
            }
            #[cfg(not(feature = "docx"))]
            ToolJob::ExportWord { .. } => {
                return Err("Export as Word is not part of this build".to_owned());
            }
            #[cfg(feature = "docx")]
            ToolJob::ExportWord { output } => {
                let source = open(
                    &MergeInput::Current,
                    run.current.as_ref(),
                    run.password.as_deref(),
                )?;
                let part =
                    export_word(&source, output, &stop, &|done, total| progress(done, total))?;
                written.push((part, output.clone()));
                let total = source.page_count().map_or(0, |n| n.max(0) as usize) + 1;
                progress(total, total);
            }
        }
        if stop() {
            return Err("cancelled".to_owned());
        }
        // Every file is complete: they take their names now.
        for (part, output) in &written {
            std::fs::rename(part, output).map_err(|e| format!("{}: {e}", output.display()))?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => Ok((
            written.into_iter().map(|(_, output)| output).collect(),
            sizes,
        )),
        Err(err) => {
            for (part, _) in &written {
                let _ = std::fs::remove_file(part);
            }
            Err(err)
        }
    }
}
