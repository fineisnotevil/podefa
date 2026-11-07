// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Signing the open document (Plan 0009 Phase 4, ADR 0015).
//!
//! MuPDF writes the signature's PDF side (`patches/mupdf/src/pdf/sign.rs`); the CMS is
//! [`crate::cms_sign::Cms`], with the key from a `.p12`/`.pfx` file ([`crate::p12`]). A signature
//! exists only once an incremental save has written it, so signing is a save: the document's
//! unsaved changes are saved in place first (so a failure loses nothing: the file is opened
//! again), the field is signed, the document is saved in place again, and the file is opened
//! again. The reopen is needed, not only tidy: every save in place rewrites one section after the
//! bytes the file had at open (`MupdfEngine::save`), so a later save without it would write over
//! the signed revision and break the signature.

use crate::cms_sign::Cms;
use crate::{MupdfEngine, edit, p12, secure, signature, stamp_producer};
use mupdf::Rect as MuRect;
use mupdf::pdf::sign::{PdfSigner, appearance};
use mupdf::pdf::{PdfPage, PdfWidget, WidgetType};
use pdf_core::SaveError;
use pdf_core::secure::Need;
use pdf_core::signature::{SignError, SignPlace, SignRequest};
use std::path::PathBuf;

/// A key file is small; more is not one.
const MAX_KEY_FILE_BYTES: u64 = 1024 * 1024;

/// What the appearance shows: the name large on the left, then "Digitally signed by", the date
/// and, when given, the reason and the location. MuPDF's logo is left out.
const APPEARANCE: i32 =
    appearance::LABELS | appearance::DATE | appearance::TEXT_NAME | appearance::GRAPHIC_NAME;

/// The smallest new field, in points: smaller cannot hold a legible appearance.
const MIN_FIELD: f32 = 24.0;

impl From<p12::P12Error> for SignError {
    fn from(error: p12::P12Error) -> Self {
        match error {
            p12::P12Error::WrongPassword => SignError::WrongPassword,
            other => SignError::KeyFile(other.to_string()),
        }
    }
}

fn save_error(error: SaveError) -> SignError {
    match error {
        SaveError::NeedsSaveAs => SignError::NeedsSaveAs,
        SaveError::NotEditable => SignError::Failed("the document cannot be edited".to_owned()),
        SaveError::Failed(why) => SignError::Failed(why),
    }
}

/// The identity in a key file, as a signer.
pub fn load_identity(request: &SignRequest) -> Result<Cms, SignError> {
    let read =
        |e: std::io::Error| SignError::KeyFile(format!("{}: {e}", request.key_file.display()));
    let len = std::fs::metadata(&request.key_file).map_err(read)?.len();
    if len > MAX_KEY_FILE_BYTES {
        return Err(SignError::KeyFile(p12::P12Error::NotPkcs12.to_string()));
    }
    let bytes = std::fs::read(&request.key_file).map_err(read)?;
    Ok(Cms::new(p12::read(&bytes, &request.password)?))
}

/// The name a key file signs with (its certificate's common name), or why it cannot sign: what
/// the sign dialog shows before the document is touched.
pub fn signer_name(key_file: &std::path::Path, password: &str) -> Result<String, SignError> {
    let request = SignRequest {
        key_file: key_file.to_path_buf(),
        password: password.to_owned(),
        reason: String::new(),
        location: String::new(),
        place: SignPlace::New {
            page: 0,
            rect: pdf_core::Rect::new(0.0, 0.0, 0.0, 0.0),
        },
    };
    let cms = load_identity(&request)?;
    Ok(cms.name().common_name.unwrap_or_default())
}

impl MupdfEngine {
    /// Signs the open document as `request` says and returns the file, its size and the page the
    /// signature is on. On a failure after the first change the file is opened again, so the
    /// document is the file as it was.
    pub fn sign(&mut self, request: &SignRequest) -> Result<(PathBuf, u64, u32), SignError> {
        let current = self
            .path
            .clone()
            .ok_or_else(|| SignError::Failed("no document is open".to_owned()))?;
        let pdf = self.pdf().map_err(|e| SignError::Failed(e.to_string()))?;
        if !secure::permissions(pdf).allows(Need::Fill) {
            return Err(SignError::NotPermitted);
        }
        if secure::security(pdf).certified == Some(1) {
            return Err(SignError::Certified);
        }
        if self.redacted || self.startxref.is_none() || !pdf.can_be_saved_incrementally() {
            return Err(SignError::NeedsSaveAs);
        }
        let page = match &request.place {
            SignPlace::Field { page, .. } | SignPlace::New { page, .. } => *page,
        };
        if page as usize >= self.revisions.len() {
            return Err(SignError::Field(format!("there is no page {}", page + 1)));
        }
        // The key first: a wrong password changes nothing.
        let cms = load_identity(request)?;
        if pdf.has_unsaved_changes() {
            self.save(None).map_err(save_error)?;
        }
        let password = self.password.clone();
        let revisions = self.revisions.clone();
        let result = self.sign_and_save(request, cms, page);
        // Opened again in either case: after a signature so the next save keeps it, after a
        // failure so a half-made signature (MuPDF keeps one to complete at the next save, also
        // across an undo) is gone.
        let reopened = self.open_document_with_password(&current, password.as_deref());
        if revisions.len() == self.revisions.len() {
            self.revisions = revisions;
        }
        let len = result?;
        reopened.map_err(|e| SignError::Failed(e.to_string()))?;
        self.touch(page);
        Ok((current, len, page))
    }

    fn sign_and_save(
        &mut self,
        request: &SignRequest,
        cms: Cms,
        page: u32,
    ) -> Result<u64, SignError> {
        let failed = |e: mupdf::Error| SignError::Failed(e.to_string());
        let name = cms.name();
        let max_size = cms.max_size();
        let digest = cms.digest();
        let pdf = self
            .pdf_mut()
            .map_err(|e| SignError::Failed(e.to_string()))?;
        // Before the signature: a change after it would go into a section of its own, outside the
        // signed range (`journal/sign.c`).
        stamp_producer(pdf);
        let taken: Vec<String> = signature::fields(pdf).into_iter().map(|f| f.name).collect();
        let mut pdf_page = edit::load(pdf, page).map_err(|e| SignError::Failed(e.to_string()))?;
        let mut widget = match &request.place {
            SignPlace::Field { name, .. } => empty_field(&pdf_page, name)?,
            SignPlace::New { rect, .. } => {
                if rect.width < MIN_FIELD || rect.height < MIN_FIELD {
                    return Err(SignError::Field(
                        "the signature's box is too small".to_owned(),
                    ));
                }
                let mut widget = pdf_page
                    .add_signature_widget(&new_field_name(&taken))
                    .map_err(failed)?;
                widget
                    .set_rect(MuRect::new(
                        rect.x,
                        rect.y,
                        rect.x + rect.width,
                        rect.y + rect.height,
                    ))
                    .map_err(failed)?;
                widget
            }
        };
        let signer = PdfSigner::new(Box::new(cms), max_size, digest, &name).map_err(failed)?;
        widget
            .sign_with(
                &signer,
                APPEARANCE,
                &request.reason,
                &request.location,
                true,
            )
            .map_err(failed)?;
        drop(widget);
        drop(pdf_page);
        let (_, len) = self.save(None).map_err(save_error)?;
        Ok(len)
    }
}

/// The unsigned, writable signature field `name` on `page`.
fn empty_field(page: &PdfPage, name: &str) -> Result<PdfWidget, SignError> {
    for widget in page.widgets() {
        if widget.name().ok().flatten().as_deref() != Some(name) {
            continue;
        }
        if !matches!(widget.r#type(), Ok(WidgetType::Signature)) {
            return Err(SignError::Field(format!("{name} is not a signature field")));
        }
        if widget.is_signed().unwrap_or(true) {
            return Err(SignError::Field(format!("{name} is signed already")));
        }
        if widget.is_readonly().unwrap_or(true) {
            return Err(SignError::Field(format!("{name} is locked")));
        }
        return Ok(widget);
    }
    Err(SignError::Field(format!(
        "there is no field {name} on this page"
    )))
}

/// `Signature1`, `Signature2`...: the first not taken.
fn new_field_name(taken: &[String]) -> String {
    (1..)
        .map(|n| format!("Signature{n}"))
        .find(|name| !taken.iter().any(|t| t == name))
        .unwrap_or_else(|| "Signature".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_names_skip_taken_ones() {
        assert_eq!(new_field_name(&[]), "Signature1");
        let taken = vec!["Signature1".to_owned(), "Signature3".to_owned()];
        assert_eq!(new_field_name(&taken), "Signature2");
    }
}
