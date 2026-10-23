// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Form fields (Plan 0006): reading a page's AcroForm widgets and setting a field's value.
//!
//! mupdf-rs 0.8 gives each widget's type, name, value and flags (`PdfWidget`); the rest - a choice
//! field's options, a button's "on" state, the maximum length, the text size, the tooltip - is read
//! from the widget's dictionary with the `PdfObject` API (plan §3.5), so no shim is needed.
//! Document JavaScript is not run (plan §0, question 2): values are set with MuPDF's trigger
//! events ignored.

use crate::MupdfError;
use mupdf::pdf::{PdfDocument, PdfObject, PdfPage, PdfWidget, WidgetType};
use pdf_core::Rect;
use pdf_core::form::{ChoiceOption, FieldInfo, FieldKind, FormSummary};

fn kind(widget: &PdfWidget) -> FieldKind {
    match widget.r#type() {
        Ok(WidgetType::Text) => FieldKind::Text,
        Ok(WidgetType::Checkbox) => FieldKind::CheckBox,
        Ok(WidgetType::RadioButton) => FieldKind::Radio,
        Ok(WidgetType::Combobox) => FieldKind::ComboBox,
        Ok(WidgetType::Listbox) => FieldKind::ListBox,
        Ok(WidgetType::Button) => FieldKind::PushButton,
        Ok(WidgetType::Signature) => FieldKind::Signature,
        _ => FieldKind::Unknown,
    }
}

/// A PDF text string: UTF-16BE with its byte order mark, UTF-8 with its mark, or PDFDocEncoding
/// (read as Latin-1, which it matches for letters).
pub(crate) fn text_string(bytes: &[u8]) -> String {
    if let Some(utf16) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = utf16
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(utf8) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(utf8).into_owned();
    }
    bytes.iter().map(|&b| char::from(b)).collect()
}

fn text(obj: Option<PdfObject>) -> Option<String> {
    let obj = obj?;
    if obj.is_string().ok()? {
        Some(text_string(&obj.as_bytes().ok()?))
    } else {
        None
    }
}

/// A choice field's entries: each a string, or an `[export label]` pair.
fn options(dict: &PdfObject) -> Vec<ChoiceOption> {
    let Some(opt) = dict.get_dict_inheritable("Opt").ok().flatten() else {
        return Vec::new();
    };
    let count = opt.len().unwrap_or(0);
    (0..count)
        .filter_map(|i| {
            let entry = opt.get_array(i as i32).ok().flatten()?;
            if entry.is_array().ok()? {
                let export = text(entry.get_array(0).ok().flatten())?;
                let label =
                    text(entry.get_array(1).ok().flatten()).unwrap_or_else(|| export.clone());
                Some(ChoiceOption { export, label })
            } else {
                let label = text(Some(entry))?;
                Some(ChoiceOption {
                    export: label.clone(),
                    label,
                })
            }
        })
        .collect()
}

/// A button widget's "on" state: the name in its normal appearances other than `Off`.
fn on_state(dict: &PdfObject) -> Option<String> {
    let normal = dict.get_dict("AP").ok()??.get_dict("N").ok()??;
    let count = normal.dict_len().ok()?;
    (0..count).find_map(|i| {
        let key = normal.get_dict_key(i as i32).ok()??;
        let name = String::from_utf8_lossy(&key.as_name().ok()?).into_owned();
        (name != "Off").then_some(name)
    })
}

/// The text size in a default appearance string such as `/Helv 11 Tf 0 g`; 0 for "fit".
pub(crate) fn font_size(da: &str) -> f32 {
    let tokens: Vec<&str> = da.split_whitespace().collect();
    tokens
        .iter()
        .position(|t| *t == "Tf")
        .and_then(|i| i.checked_sub(1))
        .and_then(|i| tokens.get(i))
        .and_then(|t| t.parse::<f32>().ok())
        .filter(|s| s.is_finite() && *s >= 0.0)
        .unwrap_or(0.0)
}

/// Every widget of a page, in the page's order.
pub(crate) fn fields(page: &PdfPage) -> Vec<FieldInfo> {
    page.widgets()
        .filter_map(|widget| {
            let dict = widget.annotation().object();
            let flags = widget.field_flags().ok()?;
            use mupdf::pdf::FieldFlags as F;
            let r = widget.annotation().rect().ok()?;
            let kind = kind(&widget);
            let da = text(dict.get_dict_inheritable("DA").ok().flatten()).unwrap_or_default();
            Some(FieldInfo {
                name: widget.name().ok().flatten().unwrap_or_default(),
                kind,
                value: widget.value().ok().flatten().unwrap_or_default(),
                rect: Rect::new(r.x0, r.y0, r.x1 - r.x0, r.y1 - r.y0),
                on_state: matches!(kind, FieldKind::CheckBox | FieldKind::Radio)
                    .then(|| on_state(&dict))
                    .flatten(),
                options: if matches!(kind, FieldKind::ComboBox | FieldKind::ListBox) {
                    options(&dict)
                } else {
                    Vec::new()
                },
                read_only: flags.contains(F::READ_ONLY) || widget.is_readonly().unwrap_or(false),
                required: flags.contains(F::REQUIRED),
                multiline: flags.contains(F::MULTILINE),
                password: flags.contains(F::PASSWORD),
                editable: flags.contains(F::EDIT),
                multi_select: flags.contains(F::MULTI_SELECT),
                max_len: dict
                    .get_dict_inheritable("MaxLen")
                    .ok()
                    .flatten()
                    .and_then(|m| m.as_int().ok())
                    .and_then(|m| u32::try_from(m).ok()),
                font_size: font_size(&da),
                tooltip: text(dict.get_dict_inheritable("TU").ok().flatten()),
            })
        })
        .collect()
}

/// The field dictionary a widget belongs to: the widget itself when it carries the field's name,
/// or its parent.
fn field_of(widget: &PdfObject) -> Option<PdfObject> {
    if widget.get_dict("T").ok().flatten().is_some() {
        return Some(widget.clone());
    }
    widget.get_dict("Parent").ok().flatten()
}

/// Sets the value of the field `name` from its widget on `page`, and returns every page with a
/// widget of that field (sorted, `page` among them). Runs inside the caller's journal operation.
pub(crate) fn set_field(
    doc: &mut PdfDocument,
    page: u32,
    name: &str,
    value: &str,
) -> Result<Vec<u32>, MupdfError> {
    let fail = |source| MupdfError::EditFailed { page, source };
    let pdf_page = doc.load_pdf_page(page as i32).map_err(fail)?;
    let mut target = None;
    for widget in pdf_page.widgets() {
        if widget.name().ok().flatten().as_deref() == Some(name) {
            target = Some(widget);
            break;
        }
    }
    let mut widget = target.ok_or_else(|| MupdfError::FieldNotFound {
        page,
        name: name.to_owned(),
    })?;
    if widget.is_readonly().unwrap_or(false) {
        return Err(MupdfError::FieldReadOnly(name.to_owned()));
    }
    // Trigger events would run the document's JavaScript, which v0.4 does not (plan §0).
    let accepted = widget.set_value(doc, value, true).map_err(fail)?;
    if !accepted {
        return Err(MupdfError::FieldRefused(name.to_owned()));
    }
    widget.update().map_err(fail)?;

    // Every page with a widget of the field: its kids' pages, or this page alone.
    let mut pages = vec![page];
    if let Some(field) = field_of(&widget.annotation().object()) {
        if let Some(kids) = field.get_dict("Kids").ok().flatten() {
            for i in 0..kids.len().unwrap_or(0) {
                let Some(kid) = kids.get_array(i as i32).ok().flatten() else {
                    continue;
                };
                if let Some(page_obj) = kid.get_dict("P").ok().flatten()
                    && let Ok(n) = doc.lookup_page_number(&page_obj)
                    && n >= 0
                {
                    pages.push(n as u32);
                }
            }
        }
    }
    pages.sort_unstable();
    pages.dedup();
    Ok(pages)
}

/// Whether an object carries a JavaScript action in `/A` or among its additional actions `/AA`.
fn has_script(obj: &PdfObject) -> bool {
    let is_js = |action: Option<PdfObject>| {
        action
            .and_then(|a| a.get_dict("S").ok().flatten())
            .and_then(|s| s.as_name().ok())
            .is_some_and(|s| s == b"JavaScript")
    };
    if is_js(obj.get_dict("A").ok().flatten()) {
        return true;
    }
    let Some(aa) = obj.get_dict("AA").ok().flatten() else {
        return false;
    };
    (0..aa.dict_len().unwrap_or(0)).any(|i| is_js(aa.get_dict_val(i as i32).ok().flatten()))
}

/// What the document's form has (plan §0, question 2): fields, scripts, XFA.
pub(crate) fn summary(doc: &PdfDocument) -> FormSummary {
    let Ok(catalog) = doc.catalog() else {
        return FormSummary::default();
    };
    let mut summary = FormSummary::default();
    if let Some(form) = catalog.get_dict("AcroForm").ok().flatten() {
        summary.xfa = form.get_dict("XFA").ok().flatten().is_some();
        if let Some(fields) = form.get_dict("Fields").ok().flatten() {
            // Every field and widget, depth first, with a bound against loops.
            let mut stack: Vec<PdfObject> = (0..fields.len().unwrap_or(0))
                .filter_map(|i| fields.get_array(i as i32).ok().flatten())
                .collect();
            summary.fields = !stack.is_empty();
            let mut seen = 0;
            while let Some(field) = stack.pop() {
                seen += 1;
                if seen > 100_000 {
                    break;
                }
                if has_script(&field) {
                    summary.scripts = true;
                }
                if let Some(kids) = field.get_dict("Kids").ok().flatten() {
                    stack.extend(
                        (0..kids.len().unwrap_or(0))
                            .filter_map(|i| kids.get_array(i as i32).ok().flatten()),
                    );
                }
            }
        }
    }
    if catalog
        .get_dict("Names")
        .ok()
        .flatten()
        .and_then(|names| names.get_dict("JavaScript").ok().flatten())
        .is_some()
        || has_script(&catalog)
    {
        summary.scripts = true;
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_strings_and_appearance_sizes() {
        assert_eq!(
            text_string(&[0xFE, 0xFF, 0x00, 0x4D, 0x00, 0x75, 0x01, 0x1F]),
            "Muğ"
        );
        assert_eq!(text_string(b"plain"), "plain");
        assert_eq!(text_string(&[0xEF, 0xBB, 0xBF, 0xC5, 0x9F]), "ş");
        assert_eq!(font_size("/Helv 11 Tf 0 g"), 11.0);
        assert_eq!(font_size("0 g /Helv 0 Tf"), 0.0);
        assert_eq!(font_size(""), 0.0);
    }
}
