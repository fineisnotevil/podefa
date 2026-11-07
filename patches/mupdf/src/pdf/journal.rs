// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! MuPDF's undo journal (fa-pdf addition, ADR 0008).
//!
//! The wrappers live in `journal/journal.c`, compiled by this crate's `build.rs` against a copy of
//! the MuPDF headers; every call goes through `fz_try` there, so MuPDF's errors come back as
//! [`Error`] like the rest of the crate's. Before the journal is enabled, MuPDF checks that those
//! headers are its own version ([`journal_headers_match`]). A release of mupdf-rs that wraps the
//! journal replaces this module.

use std::ffi::c_int;
use std::sync::OnceLock;

use mupdf_sys::{fz_context, mupdf_error_t, pdf_document};

use crate::pdf::PdfDocument;
use crate::{context, Error};

extern "C" {
    fn fa_pdf_journal_headers_match() -> c_int;
    fn fa_pdf_enable_journal(
        ctx: *mut fz_context,
        doc: *mut pdf_document,
        errptr: *mut *mut mupdf_error_t,
    );
    fn fa_pdf_undo(ctx: *mut fz_context, doc: *mut pdf_document, errptr: *mut *mut mupdf_error_t);
    fn fa_pdf_redo(ctx: *mut fz_context, doc: *mut pdf_document, errptr: *mut *mut mupdf_error_t);
    fn fa_pdf_begin_implicit_operation(
        ctx: *mut fz_context,
        doc: *mut pdf_document,
        errptr: *mut *mut mupdf_error_t,
    );
    fn fa_pdf_undoredo_state(
        ctx: *mut fz_context,
        doc: *mut pdf_document,
        steps: *mut c_int,
        errptr: *mut *mut mupdf_error_t,
    ) -> c_int;
    fn fa_pdf_startxref(doc: *mut pdf_document) -> i64;
    fn fa_pdf_set_startxref(doc: *mut pdf_document, startxref: i64);
}

/// Whether the headers `journal/journal.c` was compiled against are the linked MuPDF's version.
/// Asked once: MuPDF creates (and drops) a context with the headers' version and refuses one
/// whose version is not its own.
pub fn journal_headers_match() -> bool {
    static MATCH: OnceLock<bool> = OnceLock::new();
    *MATCH.get_or_init(|| unsafe { fa_pdf_journal_headers_match() } != 0)
}

impl PdfDocument {
    /// Starts recording an undo history. Edits made after this, each inside one
    /// [`begin_operation`](Self::begin_operation) / [`end_operation`](Self::end_operation) pair,
    /// can be undone and redone one operation at a time. Calling it again does nothing. Refused,
    /// with nothing called, when the wrappers' headers are not the linked MuPDF's version.
    pub fn enable_journal(&mut self) -> Result<(), Error> {
        if !journal_headers_match() {
            return Err(Error::InvalidArgument(
                "the journal wrappers were compiled against another MuPDF version".to_owned(),
            ));
        }
        unsafe { ffi_try!(fa_pdf_enable_journal(context(), self.as_raw())) }
    }

    /// Undoes the last operation; fails at the start of the history or during an operation.
    pub fn undo(&mut self) -> Result<(), Error> {
        unsafe { ffi_try!(fa_pdf_undo(context(), self.as_raw())) }
    }

    /// Redoes the last undone operation; fails at the end of the history or during an operation.
    pub fn redo(&mut self) -> Result<(), Error> {
        unsafe { ffi_try!(fa_pdf_redo(context(), self.as_raw())) }
    }

    /// Starts an operation that joins the last applied one: what changes in it is undone and
    /// redone with that step, not as a step of its own (with no step before it, it is not
    /// recorded). Ended with [`end_operation`](Self::end_operation) like any other.
    pub fn begin_implicit_operation(&mut self) -> Result<(), Error> {
        unsafe { ffi_try!(fa_pdf_begin_implicit_operation(context(), self.as_raw())) }
    }

    /// `(position, steps)`: how many operations of the history are applied, and how many it has.
    /// `(0, 0)` without a journal; fails during an operation.
    pub fn undo_redo_state(&self) -> Result<(u32, u32), Error> {
        let mut steps: c_int = 0;
        let position =
            unsafe { ffi_try!(fa_pdf_undoredo_state(context(), self.as_raw(), &mut steps)) }?;
        Ok((position.max(0) as u32, steps.max(0) as u32))
    }

    /// Whether [`undo`](Self::undo) would succeed.
    pub fn can_undo(&self) -> Result<bool, Error> {
        Ok(self.undo_redo_state()?.0 > 0)
    }

    /// Whether [`redo`](Self::redo) would succeed.
    pub fn can_redo(&self) -> Result<bool, Error> {
        let (position, steps) = self.undo_redo_state()?;
        Ok(position < steps)
    }

    /// The offset of the cross-reference section the next incremental save will name as `/Prev`;
    /// `None` when the wrappers' headers are not the linked MuPDF's (the field's place in the
    /// struct is theirs).
    pub fn startxref(&self) -> Option<i64> {
        journal_headers_match().then(|| unsafe { fa_pdf_startxref(self.as_raw()) })
    }

    /// Sets that offset. MuPDF 1.27.2 moves it to each incremental save's own section, although
    /// the next incremental save writes over that section again from the file's size at open; put
    /// the value the document was opened with back before each incremental save
    /// (`journal/journal.c`). Does nothing when the headers do not match.
    pub fn set_startxref(&mut self, startxref: i64) {
        if journal_headers_match() {
            unsafe { fa_pdf_set_startxref(self.as_raw(), startxref) }
        }
    }
}

