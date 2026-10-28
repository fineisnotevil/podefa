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
use mupdf::{Colorspace, Image, Matrix, Rect, Size};
use pdf_core::EngineEvent;
use pdf_core::tools::{ImageFormat, MergeInput, ToolJob, ToolSizes};
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

    /// Queues a job; `current` is the open document as bytes, for jobs that read it.
    pub(crate) fn run(&self, job_id: u64, job: ToolJob, current: Option<Vec<u8>>) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Run {
                job_id,
                job,
                current: current.map(Arc::new),
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
        ToolJob::Extract { .. } | ToolJob::ExportImages { .. } | ToolJob::ExportWord { .. } => true,
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

fn open(input: &MergeInput, current: Option<&Arc<Vec<u8>>>) -> Result<PdfDocument, String> {
    let doc = match input {
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
    if doc.needs_password().unwrap_or(false) {
        let name = match input {
            MergeInput::Current => "the open document".to_owned(),
            MergeInput::File(path) => path.display().to_string(),
        };
        return Err(format!("{name} is password-protected"));
    }
    Ok(doc)
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
                    let source = open(input, run.current.as_ref())?;
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
                let source = open(&MergeInput::Current, run.current.as_ref())?;
                for (done, (pages, output)) in parts.iter().enumerate() {
                    if stop() {
                        return Err("cancelled".to_owned());
                    }
                    let mut part = PdfDocument::new();
                    let options = InsertPdfOptions {
                        source_pages: PageSelection::Pages(
                            pages.iter().map(|&p| p as usize).collect(),
                        ),
                        ..InsertPdfOptions::default()
                    };
                    part.insert_pdf(&source, options)
                        .map_err(|e| e.to_string())?;
                    // Links to pages left out of the part are dropped.
                    let pairs: Vec<(i32, i32)> = pages
                        .iter()
                        .enumerate()
                        .map(|(to, &from)| (from as i32, to as i32))
                        .collect();
                    crate::edit::copy_annotations(&mut part, Some(&source), &pairs, &|p| {
                        pages.iter().position(|&q| q as i32 == p).map(|i| i as i32)
                    })
                    .map_err(|e| e.to_string())?;
                    written.push((write_part(&mut part, output)?, output.clone()));
                    progress(done + 1, parts.len());
                }
            }
            ToolJob::ExportImages { pages, format, dpi } => {
                let source = open(&MergeInput::Current, run.current.as_ref())?;
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
                if doc.needs_password().unwrap_or(false) {
                    return Err("the document is password-protected".to_owned());
                }
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
                // Never a larger file: the input itself is the better copy then.
                let kept_original = after >= before;
                if kept_original {
                    if let Err(e) = std::fs::write(&part, &bytes) {
                        let _ = std::fs::remove_file(&part);
                        return Err(format!("{}: {e}", output.display()));
                    }
                }
                written.push((part, output.clone()));
                sizes = Some(ToolSizes {
                    before,
                    after: after.min(before),
                    kept_original,
                });
                progress(2, 2);
            }
            #[cfg(not(feature = "docx"))]
            ToolJob::ExportWord { .. } => {
                return Err("Export as Word is not part of this build".to_owned());
            }
            #[cfg(feature = "docx")]
            ToolJob::ExportWord { output } => {
                let source = open(&MergeInput::Current, run.current.as_ref())?;
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
