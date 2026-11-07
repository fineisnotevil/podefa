use mupdf_sys::*;

use crate::{context, Error};

/// Provide two-way communication between application and library.
/// Intended for multi-threaded applications where one thread is rendering pages and
/// another thread wants to read progress feedback or abort a job that takes a long time to finish.
/// The communication is unsynchronized without locking.
#[derive(Debug)]
pub struct Cookie {
    pub(crate) inner: *mut fz_cookie,
}

impl Cookie {
    pub fn new() -> Result<Self, Error> {
        unsafe { ffi_try!(mupdf_new_cookie(context())) }.map(|inner| Self { inner })
    }

    /// Abort rendering
    pub fn abort(&mut self) {
        unsafe {
            (*self.inner).abort = 1;
        }
    }

    /// Communicates rendering progress back to the application.
    /// Increments as a page is being rendered.
    pub fn progress(&self) -> i32 {
        unsafe { (*self.inner).progress }
    }

    /// Communicates the known upper bound of rendering back to the application
    pub fn max_progress(&self) -> usize {
        unsafe { (*self.inner).progress_max }
    }

    /// count of errors during current rendering
    pub fn errors(&self) -> i32 {
        unsafe { (*self.inner).errors }
    }

    /// Initially should be set to 0.
    /// Will be set to non-zero if a TRYLATER error is thrown during rendering
    pub fn incomplete(&self) -> bool {
        unsafe { (*self.inner).incomplete > 0 }
    }

    pub fn set_incomplete(&mut self, value: bool) {
        let val = if value { 1 } else { 0 };
        unsafe {
            (*self.inner).incomplete = val;
        }
    }

    /// A handle that aborts this cookie from another thread.
    pub fn abort_handle(&self) -> CookieAbort {
        CookieAbort(self.inner)
    }
}

/// An abort handle for a [`Cookie`] that may be used from another thread.
///
/// Upstream documents `fz_cookie` as the channel "for multi-threaded applications where one
/// thread is rendering pages and another thread wants to read progress feedback or abort a job
/// that takes a long time to finish", and the only field this touches is the `abort` flag, which
/// MuPDF polls as a plain `int` while it runs. [`Cookie::abort`] cannot be called from the
/// cancelling thread because it needs `&mut Cookie`, and the rendering thread is inside the
/// raster; this handle is what lets a render pool abort an in-flight raster instead of waiting
/// for it to finish and discarding the result.
#[derive(Debug, Clone, Copy)]
pub struct CookieAbort(*mut fz_cookie);

// SAFETY: the handle is only used while the `Cookie` it came from is alive (the caller owns both),
// and every use is a single write of the `abort` flag - the one cross-thread use upstream
// documents. MuPDF reads that flag without synchronisation by design.
unsafe impl Send for CookieAbort {}
// SAFETY: see the `Send` impl; there is no shared state beyond that flag.
unsafe impl Sync for CookieAbort {}

impl CookieAbort {
    /// Asks the rendering thread to stop.
    pub fn abort(self) {
        unsafe {
            (*self.0).abort = 1;
        }
    }
}

impl Drop for Cookie {
    fn drop(&mut self) {
        if !self.inner.is_null() {
            unsafe { fz_free(context(), self.inner.cast()) }
        }
    }
}
