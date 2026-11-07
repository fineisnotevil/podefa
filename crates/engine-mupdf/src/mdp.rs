// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Whether the changes after a signature are ones it allows (Plan 0009 §3.4).
//! `pdf_validate_change_history` is not enough alone, in two ways. It knows fields but not
//! annotations: as soon as anything is locked it counts a note added to a page as forbidden, also
//! under DocMDP level 3 and FieldMDP, which allow it (ISO 32000-2, 12.8.2). And it misses a signed
//! widget changed in a value written in the widget itself: `check_unchanged_between` marks the
//! change on the value's object number, which for a direct value is 0, and that entry is never
//! read. So at levels 1 and 2 its "not fine" stands and what is signed is compared here as well;
//! at level 3 and with field locks this check decides alone.
//!
//! The check compares the signed version with the file, object by object. Every object that
//! differs must be something the signature leaves free: an annotation and its appearance, the list
//! of a page's annotations, a field that is not locked, and what MuPDF lets regenerate (document
//! information, metadata). Everything else counts as forbidden, so what this code does not
//! recognize errs towards warning the reader.

use crate::signature::{Field, fields};
use mupdf::pdf::{PdfDocument, PdfObject};
use std::collections::{HashMap, HashSet};

/// The most that a page, its fields and kids of a list are looked through.
const LIST_LIMIT: usize = 1_000_000;

/// What the signatures of the signed version lock.
#[derive(Debug, Default, PartialEq, Eq)]
struct Locks {
    /// The lowest DocMDP level.
    level: Option<i32>,
    all: bool,
    includes: Vec<String>,
    excludes: Vec<String>,
}

impl Locks {
    fn read(doc: &PdfDocument, fields: &[Field]) -> Self {
        let mut locks = Self::default();
        for field in fields {
            if let Some(value) = &field.signature {
                locks.add_references(value);
                if let Some(lock) = get(&field.object, "Lock") {
                    locks.add_fields(&lock);
                }
            }
        }
        // A certification named by the catalog, whether or not its field is in the list.
        let certification = doc
            .catalog()
            .ok()
            .and_then(|catalog| get(&catalog, "Perms"))
            .and_then(|perms| get(&perms, "DocMDP"));
        if let Some(value) = certification {
            locks.add_references(&value);
        }
        locks
    }

    fn add_references(&mut self, value: &PdfObject) {
        let Some(references) = get(value, "Reference") else {
            return;
        };
        for reference in items(&references) {
            let Some(params) = get(&reference, "TransformParams") else {
                continue;
            };
            let method = get(&reference, "TransformMethod").and_then(|m| m.as_name().ok());
            match method.as_deref() {
                Some(b"DocMDP") => {
                    // Without /P the level is 2 (Table 257).
                    let level = get(&params, "P").and_then(|p| p.as_int().ok()).unwrap_or(2);
                    self.level = Some(self.level.map_or(level, |old| old.min(level)));
                }
                Some(b"FieldMDP") => self.add_fields(&params),
                _ => {}
            }
        }
    }

    /// A field lock dictionary: `/Action` and `/Fields`.
    fn add_fields(&mut self, lock: &PdfObject) {
        let names = || -> Vec<String> {
            get(lock, "Fields").map_or_else(Vec::new, |list| {
                items(&list)
                    .iter()
                    .filter_map(|name| name.as_bytes().ok())
                    .map(|bytes| crate::form::text_string(&bytes))
                    .collect()
            })
        };
        match get(lock, "Action")
            .and_then(|action| action.as_name().ok())
            .as_deref()
        {
            Some(b"All") => self.all = true,
            Some(b"Include") => self.includes.extend(names()),
            Some(b"Exclude") => {
                self.all = true;
                self.excludes.extend(names());
            }
            _ => {}
        }
    }

    /// Whether `name` is under `list`: a name locks the field and the ones below it.
    fn listed(list: &[String], name: &str) -> bool {
        list.iter().any(|locked| {
            name == locked
                || name
                    .strip_prefix(locked.as_str())
                    .is_some_and(|rest| rest.starts_with('.'))
        })
    }

    fn locks(&self, name: &str) -> bool {
        if self.all {
            return !Self::listed(&self.excludes, name);
        }
        Self::listed(&self.includes, name)
    }
}

fn get(dict: &PdfObject, key: &str) -> Option<PdfObject> {
    dict.get_dict(key).ok().flatten()
}

fn items(list: &PdfObject) -> Vec<PdfObject> {
    if !list.is_array().unwrap_or(false) {
        return Vec::new();
    }
    (0..list.len().unwrap_or(0).min(LIST_LIMIT))
        .filter_map(|i| list.get_array(i as i32).ok().flatten())
        .collect()
}

/// The object number of what `object` refers to, when it is a reference.
fn number(object: &PdfObject) -> Option<i32> {
    object
        .is_indirect()
        .unwrap_or(false)
        .then(|| object.as_indirect().ok())
        .flatten()
}

/// Object `num`; `None` when the document has no such object.
fn object(doc: &PdfDocument, num: i32) -> Option<PdfObject> {
    let reference = doc.new_indirect(num, 0).ok()?;
    let resolved = reference.resolve().ok()??;
    if resolved.is_null().unwrap_or(true) {
        return None;
    }
    Some(reference)
}

/// Whether two objects are the same, stream bytes too.
fn same(a: &PdfObject, b: &PdfObject) -> bool {
    let Ok(stream) = a.is_stream() else {
        return false;
    };
    if stream != b.is_stream().unwrap_or(!stream) {
        return false;
    }
    if text(a) != text(b) {
        return false;
    }
    !stream
        || matches!(
            (a.read_raw_stream(), b.read_raw_stream()),
            (Ok(x), Ok(y)) if x == y
        )
}

/// An object in writing, its references as references.
fn text(object: &PdfObject) -> String {
    object
        .resolve()
        .ok()
        .flatten()
        .map_or_else(String::new, |resolved| resolved.to_string())
}

/// The same dictionary but for `keys`.
fn same_except(a: &PdfObject, b: &PdfObject, keys: &[&str]) -> bool {
    let entries = |dict: &PdfObject| -> Option<HashMap<String, String>> {
        let len = dict.dict_len().ok()?;
        let mut out = HashMap::with_capacity(len);
        for i in 0..len {
            let key = dict.get_dict_key(i as i32).ok()??;
            let name = String::from_utf8_lossy(&key.as_name().ok()?).into_owned();
            if keys.contains(&name.as_str()) {
                continue;
            }
            let value = dict.get_dict_val(i as i32).ok()??;
            out.insert(name, value.to_string());
        }
        Some(out)
    };
    matches!((entries(a), entries(b)), (Some(x), Some(y)) if x == y)
}

/// The objects of a version that a change may touch, and those it must not.
#[derive(Default)]
struct Map {
    catalog: Option<i32>,
    acro_form: Option<i32>,
    /// `/Fields`, and `/DR` with its font list when they are objects of their own.
    form_lists: HashSet<i32>,
    regenerated: HashSet<i32>,
    pages: HashSet<i32>,
    annot_lists: HashSet<i32>,
    /// Annotations other than widgets.
    annots: HashSet<i32>,
    /// Appearance streams of annotations and of fields that are free to change.
    appearances: HashSet<i32>,
    /// What page contents use: an appearance here is more than an appearance.
    page_uses: HashSet<i32>,
    /// Fields with their full names.
    fields: HashMap<i32, String>,
    /// Signed or locked fields, which must stay.
    kept: HashSet<i32>,
    /// The signature dictionaries of signed fields.
    values: HashSet<i32>,
}

impl Map {
    fn read(doc: &PdfDocument, all_fields: &[Field], locks: &Locks) -> Option<Self> {
        let mut map = Self::default();
        let catalog = doc.catalog().ok()?;
        let trailer = doc.trailer().ok()?;
        map.catalog = doc
            .trailer()
            .ok()
            .and_then(|trailer| get(&trailer, "Root"))
            .as_ref()
            .and_then(number);
        for (dict, key) in [
            (&trailer, "Info"),
            (&trailer, "Encrypt"),
            (&catalog, "Metadata"),
        ] {
            if let Some(num) = get(dict, key).as_ref().and_then(number) {
                map.regenerated.insert(num);
            }
        }
        if let Some(form) = get(&catalog, "AcroForm") {
            map.acro_form = number(&form);
            for list in [get(&form, "Fields"), get(&form, "DR")]
                .into_iter()
                .flatten()
            {
                map.form_lists.extend(number(&list));
                if let Some(fonts) = get(&list, "Font") {
                    map.form_lists.extend(number(&fonts));
                }
            }
        }
        for field in all_fields {
            let Some(num) = number(&field.object) else {
                continue;
            };
            map.fields.insert(num, field.name.clone());
            let kept = field.signature.is_some() || locks.locks(&field.name);
            map.values.extend(field.signature.as_ref().and_then(number));
            if kept {
                map.kept.insert(num);
            } else {
                map.add_appearances(&field.object);
            }
        }
        for i in 0..doc.page_count().ok()?.min(LIST_LIMIT as i32) {
            map.read_page(&doc.find_page(i).ok()?);
        }
        Some(map)
    }

    fn read_page(&mut self, page: &PdfObject) {
        self.pages.extend(number(page));
        if let Some(contents) = get(page, "Contents") {
            self.page_uses.extend(number(&contents));
            self.page_uses
                .extend(items(&contents).iter().filter_map(number));
        }
        if let Some(resources) = get(page, "Resources") {
            self.page_uses.extend(number(&resources));
            if let Some(xobjects) = get(&resources, "XObject") {
                self.page_uses.extend(number(&xobjects));
                let len = xobjects.dict_len().unwrap_or(0).min(LIST_LIMIT);
                for i in 0..len {
                    if let Ok(Some(value)) = xobjects.get_dict_val(i as i32) {
                        self.page_uses.extend(number(&value));
                    }
                }
            }
        }
        let Some(annots) = get(page, "Annots") else {
            return;
        };
        self.annot_lists.extend(number(&annots));
        for annot in items(&annots) {
            let widget = get(&annot, "Subtype")
                .and_then(|subtype| subtype.as_name().ok())
                .is_some_and(|name| name == b"Widget");
            if let (false, Some(num)) = (widget, number(&annot)) {
                self.annots.insert(num);
                self.add_appearances(&annot);
            }
        }
    }

    /// An annotation's appearance streams, one for each state: `/N`, `/R`, `/D`, each a stream or
    /// a dictionary of streams.
    fn add_appearances(&mut self, annot: &PdfObject) {
        let Some(ap) = get(annot, "AP") else {
            return;
        };
        self.appearances.extend(number(&ap));
        for key in ["N", "R", "D"] {
            let Some(entry) = get(&ap, key) else {
                continue;
            };
            self.appearances.extend(number(&entry));
            if entry.is_stream().unwrap_or(false) || !entry.is_dict().unwrap_or(false) {
                continue;
            }
            let len = entry.dict_len().unwrap_or(0).min(LIST_LIMIT);
            for i in 0..len {
                if let Ok(Some(state)) = entry.get_dict_val(i as i32) {
                    self.appearances.extend(number(&state));
                }
            }
        }
    }

    /// Whether the change of object `num` from `old` to `new` is one the signature leaves free.
    /// `new` is `None` when the object is gone.
    fn allows(&self, num: i32, old: &PdfObject, new: Option<&PdfObject>) -> bool {
        let of_type = get(old, "Type").and_then(|t| t.as_name().ok());
        if self.regenerated.contains(&num)
            || matches!(of_type.as_deref(), Some(b"ObjStm" | b"XRef"))
        {
            return true;
        }
        if self.kept.contains(&num) {
            return false;
        }
        if self.annots.contains(&num) || self.fields.contains_key(&num) {
            return true;
        }
        if self.appearances.contains(&num) {
            return !self.page_uses.contains(&num);
        }
        let Some(new) = new else {
            return false;
        };
        if self.annot_lists.contains(&num) || self.form_lists.contains(&num) {
            return old.is_stream().unwrap_or(true) == new.is_stream().unwrap_or(false);
        }
        if self.pages.contains(&num) {
            return same_except(old, new, &["Annots"]);
        }
        if Some(num) == self.catalog {
            return same_except(old, new, &["AcroForm"]) && same_form(old, new, self.acro_form);
        }
        if Some(num) == self.acro_form {
            return same_except(old, new, FORM_FREE);
        }
        false
    }
}

/// What may change in `/AcroForm`, as in MuPDF's check.
const FORM_FREE: &[&str] = &["Fields", "SigFlags", "DR", "XFA"];

/// Whether a form written in the catalog itself changed only in what is free. One that is an
/// object of its own is checked as that object.
fn same_form(old: &PdfObject, new: &PdfObject, form_object: Option<i32>) -> bool {
    match (get(old, "AcroForm"), get(new, "AcroForm")) {
        (Some(a), Some(b)) => form_object.is_some() || same_except(&a, &b, FORM_FREE),
        (None, None) => true,
        _ => false,
    }
}

/// Whether `new`, a later version of `signed`, has no change the signatures of `signed` forbid.
/// `history_bad` is MuPDF's answer for the whole file.
pub(crate) fn only_allowed_changes(
    signed: &PdfDocument,
    new: &PdfDocument,
    history_bad: bool,
) -> bool {
    let old_fields = fields(signed);
    let locks = Locks::read(signed, &old_fields);
    // Nothing locked: the signature speaks for its own version alone, which its digest covers.
    if locks == Locks::default() {
        return true;
    }
    let Some(map) = Map::read(signed, &old_fields, &locks) else {
        return false;
    };
    // Levels 1 and 2 forbid annotations, and MuPDF reads the fields they leave free.
    if locks.level.is_some_and(|level| level < 3) {
        return !history_bad && what_is_signed_stayed(signed, new, &map);
    }
    let (Ok(old_len), Ok(new_len)) = (signed.xref_len(), new.xref_len()) else {
        return false;
    };
    // Objects are never missing from the new version's table unless freed; the longer table
    // covers both.
    for num in 1..old_len.max(new_len) as i32 {
        let Some(before) = object(signed, num) else {
            // A new object: it counts only through what refers to it, which is checked.
            continue;
        };
        match object(new, num) {
            Some(after) if same(&before, &after) => {}
            after => {
                if !map.allows(num, &before, after.as_ref()) {
                    return false;
                }
            }
        }
    }
    // What is kept must also stay where it was: in the form's list, and on its page.
    let new_fields: HashSet<i32> = fields(new)
        .iter()
        .filter_map(|field| number(&field.object))
        .collect();
    if map.kept.iter().any(|num| !new_fields.contains(num)) {
        return false;
    }
    still_on_pages(signed, new, &map.kept)
}

fn what_is_signed_stayed(signed: &PdfDocument, new: &PdfDocument, map: &Map) -> bool {
    map.kept
        .iter()
        .chain(&map.values)
        .all(|&num| match (object(signed, num), object(new, num)) {
            (Some(before), Some(after)) => same(&before, &after),
            _ => false,
        })
        && still_on_pages(signed, new, &map.kept)
}

/// Whether the widgets of `kept` on the pages of `signed` are on the pages of `new`.
fn still_on_pages(signed: &PdfDocument, new: &PdfDocument, kept: &HashSet<i32>) -> bool {
    let shown = |doc: &PdfDocument| -> Option<HashSet<i32>> {
        let mut out = HashSet::new();
        for i in 0..doc.page_count().ok()?.min(LIST_LIMIT as i32) {
            if let Some(annots) = get(&doc.find_page(i).ok()?, "Annots") {
                out.extend(items(&annots).iter().filter_map(number));
            }
        }
        Some(out)
    };
    let (Some(before), Some(after)) = (shown(signed), shown(new)) else {
        return false;
    };
    kept.iter()
        .all(|num| !before.contains(num) || after.contains(num))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_locks_what_is_under_it() {
        let locks = Locks {
            includes: vec!["form.name".to_owned()],
            ..Locks::default()
        };
        assert!(locks.locks("form.name"));
        assert!(locks.locks("form.name.first"));
        assert!(!locks.locks("form.names"));
        assert!(!locks.locks("form"));
    }

    #[test]
    fn all_but_some() {
        let locks = Locks {
            all: true,
            excludes: vec!["free".to_owned()],
            ..Locks::default()
        };
        assert!(locks.locks("other"));
        assert!(!locks.locks("free"));
        assert!(!locks.locks("free.kid"));
    }
}
