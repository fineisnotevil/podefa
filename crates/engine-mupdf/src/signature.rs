// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Reading and verifying a document's signatures (Plan 0009 Phase 3, §4.1). This works on the file
//! on disk, through a document of its own on a thread of its own: a signature speaks for bytes in
//! the file, not for the open document's unsaved state, and a hash of a large file must not hold up
//! rendering.

pub use crate::verify::Roots;

use crate::form::text_string;
use crate::verify;
use mupdf::pdf::{PdfDocument, PdfObject, PdfPage};
use pdf_core::signature::{ChangedAfter, SignatureInfo, pdf_date, pdf_date_seconds};
use pdf_core::{EngineEvent, Rect};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{SystemTime, UNIX_EPOCH};

const FIELD_DEPTH: u32 = 16;
const FIELD_LIMIT: usize = 100_000;
/// Pages looked through for a widget with no `/P`.
const PAGE_SCAN_LIMIT: i32 = 100_000;

/// A field under `/Fields`, with its full name.
pub(crate) struct Field {
    pub(crate) name: String,
    pub(crate) object: PdfObject,
    /// Its signature, when it is a signed signature field.
    pub(crate) signature: Option<PdfObject>,
}

/// The fields of a document's form.
pub(crate) fn fields(doc: &PdfDocument) -> Vec<Field> {
    let mut out = Vec::new();
    let list = doc
        .catalog()
        .ok()
        .and_then(|catalog| catalog.get_dict("AcroForm").ok().flatten())
        .and_then(|form| form.get_dict("Fields").ok().flatten());
    if let Some(list) = list {
        let mut seen = 0;
        walk(&list, "", false, 0, &mut seen, &mut out);
    }
    out
}

fn walk(
    array: &PdfObject,
    parent: &str,
    sig: bool,
    depth: u32,
    seen: &mut usize,
    out: &mut Vec<Field>,
) {
    let Ok(len) = array.len() else {
        return;
    };
    for i in 0..len {
        *seen += 1;
        if depth > FIELD_DEPTH || *seen > FIELD_LIMIT {
            return;
        }
        let Ok(Some(field)) = array.get_array(i as i32) else {
            continue;
        };
        let is_sig = field
            .get_dict("FT")
            .ok()
            .flatten()
            .and_then(|ft| ft.as_name().ok())
            .map_or(sig, |name| name == b"Sig");
        let part = string(&field, "T");
        let name = match (parent.is_empty(), part.is_empty()) {
            (true, _) => part,
            (false, true) => parent.to_owned(),
            (false, false) => format!("{parent}.{part}"),
        };
        let signature = field
            .get_dict("V")
            .ok()
            .flatten()
            .filter(|value| is_sig && value.is_dict().unwrap_or(false));
        out.push(Field {
            name: name.clone(),
            object: field.clone(),
            signature,
        });
        if let Ok(Some(kids)) = field.get_dict("Kids") {
            walk(&kids, &name, is_sig, depth + 1, seen, out);
        }
    }
}

fn string(dict: &PdfObject, key: &str) -> String {
    dict.get_dict(key)
        .ok()
        .flatten()
        .and_then(|value| value.as_bytes().ok())
        .map_or_else(String::new, |bytes| text_string(&bytes))
}

fn byte_range(value: &PdfObject) -> Option<[u64; 4]> {
    let array = value.get_dict("ByteRange").ok()??;
    if array.len().ok()? != 4 {
        return None;
    }
    let mut out = [0; 4];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u64::try_from(array.get_array(i as i32).ok()??.as_int().ok()?).ok()?;
    }
    Some(out)
}

/// The widget's page and its rectangle on that page, in page space (top left, y down). The field
/// is its own widget, or its first kid is.
fn place(doc: &PdfDocument, field: &PdfObject) -> (Option<u32>, Option<Rect>) {
    let widget = if field.get_dict("Rect").ok().flatten().is_some() {
        Some(field.clone())
    } else {
        field
            .get_dict("Kids")
            .ok()
            .flatten()
            .and_then(|kids| kids.get_array(0).ok().flatten())
    };
    let Some(widget) = widget else {
        return (None, None);
    };
    let page = match widget.get_dict("P").ok().flatten() {
        Some(page_obj) => doc
            .lookup_page_number(&page_obj)
            .ok()
            .and_then(|page| u32::try_from(page).ok()),
        // `/P` is optional (ISO 32000-1 table 164), and MuPDF's new signature fields have none.
        None => page_listing(doc, &widget),
    };
    let Some(page) = page else {
        return (None, None);
    };
    (Some(page), rect(doc, page, &widget))
}

/// The page whose `/Annots` lists `widget`.
fn page_listing(doc: &PdfDocument, widget: &PdfObject) -> Option<u32> {
    let number = widget.as_indirect().ok().filter(|&n| n > 0)?;
    let count = doc.page_count().ok()?.min(PAGE_SCAN_LIMIT);
    (0..count).find_map(|page| {
        let annots = doc.find_page(page).ok()?.get_dict("Annots").ok()??;
        let len = annots.len().ok()?.min(FIELD_LIMIT) as i32;
        (0..len)
            .any(|i| {
                annots
                    .get_array(i)
                    .ok()
                    .flatten()
                    .is_some_and(|annot| annot.as_indirect().ok() == Some(number))
            })
            .then(|| u32::try_from(page).ok())
            .flatten()
    })
}

fn rect(doc: &PdfDocument, page: u32, widget: &PdfObject) -> Option<Rect> {
    let array = widget.get_dict("Rect").ok()??;
    let number = |i: i32| array.get_array(i).ok()??.as_float().ok();
    let (x0, y0, x1, y1) = (number(0)?, number(1)?, number(2)?, number(3)?);
    let page = PdfPage::try_from(doc.load_page(page as i32).ok()?).ok()?;
    let ctm = page.ctm().ok()?;
    let a = mupdf::Point::new(x0, y0).transform(&ctm);
    let b = mupdf::Point::new(x1, y1).transform(&ctm);
    Some(Rect {
        x: a.x.min(b.x),
        y: a.y.min(b.y),
        width: (a.x - b.x).abs(),
        height: (a.y - b.y).abs(),
    })
}

/// What the file at `path` says about its signatures. A file that cannot be read has none to tell
/// of: the document that was opened from it has its own error to report.
pub fn verify_file(path: &Path, password: Option<&str>, roots: &Roots) -> Vec<SignatureInfo> {
    let Some(name) = path.to_str() else {
        return Vec::new();
    };
    let Ok(mut doc) = PdfDocument::open(name) else {
        return Vec::new();
    };
    if doc.needs_password().unwrap_or(false)
        && !password.is_some_and(|password| doc.authenticate(password).unwrap_or(false))
    {
        return Vec::new();
    }
    let all = fields(&doc);
    let signed: Vec<(&Field, &PdfObject)> = all
        .iter()
        .filter_map(|field| field.signature.as_ref().map(|value| (field, value)))
        .collect();
    if signed.is_empty() {
        return Vec::new();
    }
    // Whether any later version breaks what an earlier signature locked, by MuPDF's reading; the
    // module `mdp` reads it for each signature.
    let history_bad = doc.validate_change_history().map_or(true, |bad| bad > 0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    signed
        .iter()
        .map(|&(field, value)| {
            let date = string(value, "M");
            let at = pdf_date_seconds(&date).unwrap_or(now);
            let sub_filter = value
                .get_dict("SubFilter")
                .ok()
                .flatten()
                .and_then(|name| name.as_name().ok())
                .unwrap_or_default();
            let range = byte_range(value);
            let checked = match range {
                Some(range) => verify::check(path, range, &sub_filter, at, roots),
                None => verify::unranged(),
            };
            let changed_after = if !checked.saved_since {
                ChangedAfter::None
            } else if range
                .is_some_and(|range| changes_are_free(path, password, range, &doc, history_bad))
            {
                ChangedAfter::Allowed
            } else {
                ChangedAfter::Disallowed
            };
            let (page, rect) = place(&doc, &field.object);
            SignatureInfo {
                field: field.name.clone(),
                page,
                rect,
                signer: checked.signer.unwrap_or_else(|| string(value, "Name")),
                time: if date.is_empty() {
                    String::new()
                } else {
                    pdf_date(&date)
                },
                reason: string(value, "Reason"),
                location: string(value, "Location"),
                digest: checked.digest,
                certificate: checked.certificate,
                changed_after,
            }
        })
        .collect()
}

/// Whether what `new` changed since the version a signature covers is free under that signature.
/// The signed version is the start of the file, which is a document of its own.
fn changes_are_free(
    path: &Path,
    password: Option<&str>,
    [_, _, gap_end, second]: [u64; 4],
    new: &PdfDocument,
    history_bad: bool,
) -> bool {
    let Ok(data) = std::fs::read(path) else {
        return false;
    };
    let Some(version) = usize::try_from(gap_end + second)
        .ok()
        .and_then(|end| data.get(..end))
    else {
        return false;
    };
    let Ok(mut signed) = PdfDocument::from_bytes(version) else {
        return false;
    };
    if signed.needs_password().unwrap_or(false)
        && !password.is_some_and(|password| signed.authenticate(password).unwrap_or(false))
    {
        return false;
    }
    crate::mdp::only_allowed_changes(&signed, new, history_bad)
}

/// Verifies off the engine's thread and sends what it finds. A thread for each request, not a
/// worker: this happens when a signed document is opened or saved, and only then.
pub(crate) fn verify_in_background(
    path: PathBuf,
    password: Option<String>,
    events: Sender<EngineEvent>,
) {
    let _ = std::thread::Builder::new()
        .name("mupdf-signatures".to_owned())
        .spawn(move || {
            let signatures = verify_file(&path, password.as_deref(), &Roots::system());
            let _ = events.send(EngineEvent::Signatures { path, signatures });
        });
}
