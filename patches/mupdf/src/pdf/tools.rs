// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Image rewriting, JPEG output (fa-pdf addition, Plan 0005 Phase 4) and closing a document writer
//! (Phase 5).
//!
//! The wrappers live in `journal/tools.c`, compiled next to the journal's and against the same
//! copy of the MuPDF headers, so they are refused under the same check
//! ([`journal_headers_match`]). A release of mupdf-rs that wraps `pdf_rewrite_images`,
//! `fz_save_pixmap_as_jpeg` and closes its writers inside `fz_try` replaces this module.

use std::ffi::{c_char, c_int, CString};

use mupdf_sys::{fz_context, fz_document_writer, fz_pixmap, mupdf_error_t, pdf_document};

use crate::pdf::journal::journal_headers_match;
use crate::pdf::PdfDocument;
use crate::{context, DocumentWriter, Error, Pixmap};

extern "C" {
    fn fa_pdf_rewrite_images(
        ctx: *mut fz_context,
        doc: *mut pdf_document,
        threshold_dpi: c_int,
        to_dpi: c_int,
        quality: c_int,
        errptr: *mut *mut mupdf_error_t,
    );
    fn fa_save_pixmap_as_jpeg(
        ctx: *mut fz_context,
        pix: *mut fz_pixmap,
        filename: *const c_char,
        quality: c_int,
        errptr: *mut *mut mupdf_error_t,
    );
    fn fa_close_document_writer(
        ctx: *mut fz_context,
        wri: *mut fz_document_writer,
        errptr: *mut *mut mupdf_error_t,
    );
}

fn headers() -> Result<(), Error> {
    if journal_headers_match() {
        Ok(())
    } else {
        Err(Error::InvalidArgument(
            "the fa-pdf wrappers were compiled against another MuPDF version".to_owned(),
        ))
    }
}

impl PdfDocument {
    /// Downsamples colour and grey images above `threshold_dpi` to `to_dpi`, and recompresses
    /// them as JPEG of `quality` (1-100) where that is smaller. Bitonal images stay as they are.
    /// It changes objects in place: use it on a document of its own, never on one whose undo
    /// history matters.
    pub fn rewrite_images(
        &mut self,
        threshold_dpi: u32,
        to_dpi: u32,
        quality: u8,
    ) -> Result<(), Error> {
        headers()?;
        unsafe {
            ffi_try!(fa_pdf_rewrite_images(
                context(),
                self.as_raw(),
                threshold_dpi.min(c_int::MAX as u32) as c_int,
                to_dpi.min(c_int::MAX as u32) as c_int,
                c_int::from(quality.clamp(1, 100))
            ))
        }
    }
}

impl Pixmap {
    /// Writes the pixmap as a JPEG file of `quality` (1-100). The pixmap must be grey, RGB or
    /// CMYK without alpha.
    pub fn save_as_jpeg(&self, filename: &str, quality: u8) -> Result<(), Error> {
        headers()?;
        let c_filename = CString::new(filename)?;
        unsafe {
            ffi_try!(fa_save_pixmap_as_jpeg(
                context(),
                self.inner,
                c_filename.as_ptr(),
                c_int::from(quality.clamp(1, 100))
            ))
        }
    }
}

impl DocumentWriter {
    /// Finishes the document - for DOCX, this is when the file is written - and reports a failure,
    /// which dropping the writer would hide (or turn into undefined behaviour: mupdf-rs' `Drop`
    /// closes it outside `fz_try`). The writer is gone afterwards either way.
    pub fn close(mut self) -> Result<(), Error> {
        headers()?;
        let inner = std::mem::replace(&mut self.inner, std::ptr::null_mut());
        if inner.is_null() {
            return Ok(());
        }
        unsafe { ffi_try!(fa_close_document_writer(context(), inner)) }
    }
}
