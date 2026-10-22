// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! The tool thread (Plan 0005, §4.2): merging and extracting pages into new files.
//!
//! Jobs run here, on documents of their own, never on the open one: rewriting a file with garbage
//! collection breaks the open document's undo journal (Plan 0005 §3.4), and a merge of thousands of
//! pages must not hold up rendering. The open document comes in as a snapshot of its current state,
//! unsaved changes included (§0, question 3). Every output is written next to its target as
//! `<name>.part`, and only when the whole job has succeeded are they renamed to the names the user
//! chose: a cancelled or failed job deletes its `.part` files and leaves any file it would have
//! replaced untouched.

use mupdf::pdf::{InsertPdfOptions, PageSelection, PdfDocument, PdfWriteOptions};
use pdf_core::EngineEvent;
use pdf_core::tools::{MergeInput, ToolJob};
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
                    let result = execute(&run, &shared, &events);
                    let _ = events.send(EngineEvent::ToolDone {
                        job_id: run.job_id,
                        result,
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
        ToolJob::Extract { .. } => true,
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

/// Writes a document to the `.part` file of `output`, which it returns.
fn write_part(doc: &PdfDocument, output: &Path) -> Result<PathBuf, String> {
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

fn execute(
    run: &Run,
    cancelled: &AtomicU64,
    events: &Sender<EngineEvent>,
) -> Result<Vec<PathBuf>, String> {
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
                written.push((write_part(&merged, output)?, output.clone()));
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
                    written.push((write_part(&part, output)?, output.clone()));
                    progress(done + 1, parts.len());
                }
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
        Ok(()) => Ok(written.into_iter().map(|(_, output)| output).collect()),
        Err(err) => {
            for (part, _) in &written {
                let _ = std::fs::remove_file(part);
            }
            Err(err)
        }
    }
}
