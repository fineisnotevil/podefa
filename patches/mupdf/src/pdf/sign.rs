// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Signing a signature field (fa-pdf addition, Plan 0009 Phase 4, ADR 0015).
//!
//! MuPDF does the PDF part (`pdf_sign_signature`: the value, the appearance, the byte range when
//! the document is saved incrementally) and asks a signer for the CMS blob. The signer lives in
//! `journal/sign.c`: it reads and hashes the signed bytes with MuPDF's SHA-2, inside MuPDF's
//! `fz_try`, and calls [`CmsSigner::sign`] with the digest. Rust is called from C there, so the
//! call catches any panic: nothing unwinds into C.

use std::ffi::{c_char, c_int, c_void, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr::NonNull;

use mupdf_sys::{fz_context, mupdf_error_t, pdf_annot, pdf_pkcs7_signer};

use crate::pdf::journal::journal_headers_match;
use crate::pdf::PdfWidget;
use crate::{context, Error};

extern "C" {
    fn fa_new_signer(
        ctx: *mut fz_context,
        rust: *mut c_void,
        max_size: usize,
        hash: c_int,
        cn: *const c_char,
        o: *const c_char,
        ou: *const c_char,
        email: *const c_char,
        c: *const c_char,
        errptr: *mut *mut mupdf_error_t,
    ) -> *mut pdf_pkcs7_signer;
    fn fa_drop_signer(ctx: *mut fz_context, signer: *mut pdf_pkcs7_signer);
    fn fa_pdf_sign_signature(
        ctx: *mut fz_context,
        widget: *mut pdf_annot,
        signer: *mut pdf_pkcs7_signer,
        flags: c_int,
        reason: *const c_char,
        location: *const c_char,
        cades: c_int,
        errptr: *mut *mut mupdf_error_t,
    );
}

/// Makes the CMS blob for the digest of the signed bytes.
pub trait CmsSigner: Send {
    /// The DER of a CMS `ContentInfo` over `digest`, or `None` when it cannot be made.
    fn sign(&self, digest: &[u8]) -> Option<Vec<u8>>;
}

/// The hash of the signed bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureDigest {
    Sha256,
    Sha384,
    Sha512,
}

/// The signer's name, as MuPDF writes it into the appearance.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignerName {
    pub common_name: Option<String>,
    pub organization: Option<String>,
    pub unit: Option<String>,
    pub email: Option<String>,
    pub country: Option<String>,
}

/// What the appearance shows (`PDF_SIGNATURE_SHOW_*`, `pdf/form.h`).
pub mod appearance {
    pub const LABELS: i32 = 1;
    pub const DN: i32 = 2;
    pub const DATE: i32 = 4;
    pub const TEXT_NAME: i32 = 8;
    pub const GRAPHIC_NAME: i32 = 16;
    pub const LOGO: i32 = 32;
}

/// A signer MuPDF can call.
pub struct PdfSigner {
    inner: NonNull<pdf_pkcs7_signer>,
}

// SAFETY: the signer is MuPDF's struct around a `Send` Rust value; it is used from one thread at
// a time, the document's.
unsafe impl Send for PdfSigner {}

fn c_string(text: &Option<String>) -> Result<Option<CString>, Error> {
    Ok(match text {
        Some(text) => Some(CString::new(text.as_str())?),
        None => None,
    })
}

fn ptr(text: &Option<CString>) -> *const c_char {
    text.as_ref().map_or(std::ptr::null(), |t| t.as_ptr())
}

impl PdfSigner {
    /// `max_size` is the most bytes `signer` ever returns: MuPDF leaves that much room in the
    /// file before it knows the signature.
    pub fn new(
        signer: Box<dyn CmsSigner>,
        max_size: usize,
        digest: SignatureDigest,
        name: &SignerName,
    ) -> Result<Self, Error> {
        if !journal_headers_match() {
            return Err(Error::InvalidArgument(
                "the fa-pdf wrappers were compiled against another MuPDF version".to_owned(),
            ));
        }
        let cn = c_string(&name.common_name)?;
        let o = c_string(&name.organization)?;
        let ou = c_string(&name.unit)?;
        let email = c_string(&name.email)?;
        let c = c_string(&name.country)?;
        let hash = match digest {
            SignatureDigest::Sha256 => 0,
            SignatureDigest::Sha384 => 1,
            SignatureDigest::Sha512 => 2,
        };
        // The C signer owns it from here, also when it cannot be made.
        let rust = Box::into_raw(Box::new(signer)).cast::<c_void>();
        let raw = unsafe {
            ffi_try!(fa_new_signer(
                context(),
                rust,
                max_size,
                hash,
                ptr(&cn),
                ptr(&o),
                ptr(&ou),
                ptr(&email),
                ptr(&c)
            ))
        }?;
        NonNull::new(raw)
            .map(|inner| Self { inner })
            .ok_or(Error::UnexpectedNullPtr)
    }
}

impl Drop for PdfSigner {
    fn drop(&mut self) {
        // SAFETY: made by fa_new_signer, dropped once; MuPDF keeps its own reference while a
        // signature it holds is unsaved.
        unsafe { fa_drop_signer(context(), self.inner.as_ptr()) }
    }
}

impl PdfWidget {
    /// Signs this signature field with `signer` and MuPDF's appearance (`flags` from
    /// [`appearance`]); `cades` names the format `ETSI.CAdES.detached` instead of
    /// `adbe.pkcs7.detached`. The signature is made when the document is next saved
    /// incrementally, and only then.
    pub fn sign_with(
        &mut self,
        signer: &PdfSigner,
        flags: i32,
        reason: &str,
        location: &str,
        cades: bool,
    ) -> Result<(), Error> {
        let reason = CString::new(reason)?;
        let location = CString::new(location)?;
        unsafe {
            ffi_try!(fa_pdf_sign_signature(
                context(),
                self.annot.inner.as_ptr(),
                signer.inner.as_ptr(),
                flags,
                reason.as_ptr(),
                location.as_ptr(),
                c_int::from(cades)
            ))
        }
    }

    /// Moves the widget to `rect`, for a signature field just added (its default place is
    /// MuPDF's `{12, 12, 112, 62}`).
    pub fn set_rect(&mut self, rect: crate::Rect) -> Result<(), Error> {
        self.annot.set_rect(rect)
    }
}

/// Called by `journal/sign.c` with the digest; see `fa_rust_signer_sign` there.
#[no_mangle]
extern "C" fn fa_rust_signer_sign(
    rust: *mut c_void,
    digest: *const u8,
    digest_len: usize,
    out: *mut u8,
    out_len: usize,
) -> usize {
    if rust.is_null() || digest.is_null() || out.is_null() {
        return 0;
    }
    // SAFETY: `rust` is the box fa_new_signer was given, alive until fa_rust_signer_drop; the
    // buffers are MuPDF's, of the lengths it passes.
    let signer = unsafe { &*rust.cast::<Box<dyn CmsSigner>>() };
    let digest = unsafe { std::slice::from_raw_parts(digest, digest_len) };
    let made = catch_unwind(AssertUnwindSafe(|| signer.sign(digest)));
    match made {
        Ok(Some(blob)) if blob.len() <= out_len => {
            let out = unsafe { std::slice::from_raw_parts_mut(out, out_len) };
            out[..blob.len()].copy_from_slice(&blob);
            blob.len()
        }
        Ok(Some(blob)) => blob.len().max(out_len.saturating_add(1)),
        _ => 0,
    }
}

/// Called by `journal/sign.c` when MuPDF drops the signer.
#[no_mangle]
extern "C" fn fa_rust_signer_drop(rust: *mut c_void) {
    if rust.is_null() {
        return;
    }
    // SAFETY: the box fa_new_signer was given, dropped here once.
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        drop(Box::from_raw(rust.cast::<Box<dyn CmsSigner>>()));
    }));
}
