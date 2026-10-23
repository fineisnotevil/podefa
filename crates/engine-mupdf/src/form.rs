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
use mupdf::Buffer;
use mupdf::pdf::{PdfDocument, PdfObject, PdfPage, PdfWidget, WidgetType};
use pdf_core::Rect;
use pdf_core::form::{ChoiceOption, FieldInfo, FieldKind, FormSummary, OFF, TabOrder, tab_places};
use pdf_core::form_data::FieldValue;
use std::collections::HashMap;

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

/// A choice field's chosen export values (`/V`): a string, or an array of them for a multi-select
/// list.
fn choice_values(dict: &PdfObject) -> Vec<String> {
    let Some(v) = dict.get_dict_inheritable("V").ok().flatten() else {
        return Vec::new();
    };
    let one = |obj: PdfObject| {
        if obj.is_name().unwrap_or(false) {
            obj.as_name()
                .ok()
                .map(|n| String::from_utf8_lossy(&n).into_owned())
        } else {
            text(Some(obj))
        }
    };
    if v.is_array().unwrap_or(false) {
        (0..v.len().unwrap_or(0))
            .filter_map(|i| v.get_array(i as i32).ok().flatten().and_then(one))
            .collect()
    } else {
        one(v).into_iter().filter(|s| !s.is_empty()).collect()
    }
}

/// The page's tab order (`/Tabs`).
fn tab_order(page: &PdfPage) -> TabOrder {
    let tabs = page
        .object()
        .get_dict("Tabs")
        .ok()
        .flatten()
        .and_then(|t| t.as_name().ok());
    match tabs.as_deref() {
        Some(b"R") => TabOrder::Rows,
        Some(b"C") => TabOrder::Columns,
        _ => TabOrder::Annotations,
    }
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

/// Every widget of a page, in the page's order, each with its place in the page's tab order.
pub(crate) fn fields(page: &PdfPage) -> Vec<FieldInfo> {
    let mut items = widgets(page);
    let rects: Vec<Rect> = items.iter().map(|f| f.rect).collect();
    for (item, tab) in items.iter_mut().zip(tab_places(&rects, tab_order(page))) {
        item.tab = tab;
    }
    items
}

fn widgets(page: &PdfPage) -> Vec<FieldInfo> {
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
                values: if matches!(kind, FieldKind::ComboBox | FieldKind::ListBox) {
                    choice_values(&dict)
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
                comb: flags.contains(F::COMB),
                font_size: font_size(&da),
                tooltip: text(dict.get_dict_inheritable("TU").ok().flatten()),
                tab: 0,
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
    set_field_values(doc, page, name, &[value.to_owned()])
}

/// [`set_field`] with any number of values: a multi-select list takes them all (as an array in
/// `/V` and their indices in `/I`), every other field the first one, and none clears the field.
pub(crate) fn set_field_values(
    doc: &mut PdfDocument,
    page: u32,
    name: &str,
    values: &[String],
) -> Result<Vec<u32>, MupdfError> {
    let value = values.first().map_or("", String::as_str);
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
    let is_list = matches!(widget.r#type(), Ok(WidgetType::Listbox));
    // A list's values are written here unless there is exactly one, which MuPDF wrote: several
    // (an array), or none (MuPDF leaves an array it does not replace).
    let chosen: Vec<String> = values.iter().filter(|v| !v.is_empty()).cloned().collect();
    if is_list
        && chosen.len() != 1
        && let Some(field) = field_of(&widget.annotation().object())
    {
        set_list_values(doc, field, page, name, &chosen)?;
    }
    // A check box or radio group shows its value through each widget's appearance state. MuPDF
    // sets the states from the object it is given down through `/Kids`, and mupdf-rs gives it the
    // widget, not the field, so a group's other widgets would keep their old state: the field's
    // widgets are set here, each on when the value is its own "on" state.
    if matches!(
        widget.r#type(),
        Ok(WidgetType::Checkbox | WidgetType::RadioButton)
    ) && let Some(field) = field_of(&widget.annotation().object())
        && let Some(kids) = field.get_dict("Kids").ok().flatten()
    {
        for i in 0..kids.len().unwrap_or(0) {
            let Some(mut kid) = kids.get_array(i as i32).ok().flatten() else {
                continue;
            };
            let state = if on_state(&kid).as_deref() == Some(value) {
                value
            } else {
                OFF
            };
            kid.dict_put("AS", PdfObject::new_name(state).map_err(fail)?)
                .map_err(fail)?;
        }
    }

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
    drop(widget);

    // Every widget of the field gets its new appearance now, inside this journal operation, so
    // undo takes it back too and the single-line fix below reaches all of them.
    for &p in &pages {
        let pdf_page = doc.load_pdf_page(p as i32).map_err(fail)?;
        for mut widget in pdf_page.widgets() {
            if widget.name().ok().flatten().as_deref() != Some(name) {
                continue;
            }
            widget.update().map_err(fail)?;
            let multiline = widget
                .field_flags()
                .is_ok_and(|f| f.contains(mupdf::pdf::FieldFlags::MULTILINE));
            let object = widget.annotation().object();
            if is_list {
                mark_list_choices(&object).map_err(fail)?;
            } else if !multiline {
                fix_single_line_rich_text(&object).map_err(fail)?;
            }
        }
    }
    Ok(pages)
}

/// A list's values other than one: `/V` an array of the chosen options' export strings, copied
/// from `/Opt` so they keep the file's encoding, and `/I` their indices in order; with none, both
/// are removed. A value that is not one of the options is refused.
fn set_list_values(
    doc: &mut PdfDocument,
    mut field: PdfObject,
    page: u32,
    name: &str,
    values: &[String],
) -> Result<(), MupdfError> {
    let fail = |source| MupdfError::EditFailed { page, source };
    let entries = options(&field);
    let Some(opt) = field.get_dict_inheritable("Opt").map_err(fail)? else {
        return Err(MupdfError::FieldRefused(name.to_owned()));
    };
    let mut chosen: Vec<usize> = Vec::new();
    for value in values {
        match entries.iter().position(|o| &o.export == value) {
            Some(i) if !chosen.contains(&i) => chosen.push(i),
            Some(_) => {}
            None => return Err(MupdfError::FieldRefused(name.to_owned())),
        }
    }
    chosen.sort_unstable();
    let mut write = || -> Result<(), mupdf::Error> {
        if chosen.is_empty() {
            field.dict_delete("V")?;
            field.dict_delete("I")?;
            return Ok(());
        }
        let mut v = doc.new_array()?;
        let mut indices = doc.new_array()?;
        for &i in &chosen {
            let entry = opt
                .get_array(i as i32)?
                .ok_or(mupdf::Error::UnexpectedNullPtr)?;
            let export = if entry.is_array()? {
                entry.get_array(0)?.ok_or(mupdf::Error::UnexpectedNullPtr)?
            } else {
                entry
            };
            v.array_push(export)?;
            indices.array_push(PdfObject::new_int(i as i32)?)?;
        }
        field.dict_put("V", v)?;
        field.dict_put("I", indices)?;
        Ok(())
    };
    write().map_err(fail)
}

/// Marks a list's chosen rows in its appearance. MuPDF 1.27.2 writes a list box as all its options,
/// one per line, and leaves the chosen ones unmarked (`pdf_write_ch_widget_appearance`: "TODO:
/// Highlight selected items"), so a list shows no value. A band in the colour readers use for a
/// list's selection is drawn under each chosen row, inside the field's clip.
///
/// MuPDF's list text (`write_variable_text`, multi-line) starts at `X Y Td` (`Y` the top of the
/// text box) and moves down one line height, 1.116 times the text size, before each option, from
/// the top index `/TI`. An appearance that does not have that shape (a list laid out as rich text,
/// or another producer's) is left as it is.
fn mark_list_choices(widget: &PdfObject) -> Result<(), mupdf::Error> {
    let Some(field) = field_of(widget) else {
        return Ok(());
    };
    let entries = options(&field);
    let chosen: Vec<usize> = choice_values(&field)
        .iter()
        .filter_map(|v| entries.iter().position(|o| &o.export == v))
        .collect();
    if chosen.is_empty() {
        return Ok(());
    }
    let top = widget
        .get_dict_inheritable("TI")?
        .and_then(|t| t.as_int().ok())
        .unwrap_or(0)
        .max(0) as usize;
    let Some(mut normal) = widget
        .get_dict("AP")?
        .and_then(|ap| ap.get_dict("N").ok().flatten())
        .filter(|n| n.is_stream().unwrap_or(false))
    else {
        return Ok(());
    };
    let content = String::from_utf8_lossy(&normal.read_stream()?).into_owned();
    if let Some(marked) = list_with_bands(&content, top, &chosen) {
        normal.write_stream_buffer(&Buffer::from_bytes(marked.as_bytes())?)?;
    }
    Ok(())
}

/// The list appearance of [`mark_list_choices`] with a band under each chosen option's row, or
/// `None` when it does not have the expected shape.
pub(crate) fn list_with_bands(content: &str, top: usize, chosen: &[usize]) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    let numbers = |line: &str, op: &str| -> Option<Vec<f64>> {
        let rest = line.trim().strip_suffix(op)?;
        rest.split_whitespace().map(|t| t.parse().ok()).collect()
    };
    let bt = lines.iter().position(|l| l.trim() == "BT")?;
    // The clip just before the text: `B B W H re`, then `W`, `n`.
    let clip = lines[..bt]
        .iter()
        .rev()
        .find_map(|l| numbers(l, " re").filter(|n| n.len() == 4))?;
    let size = lines[bt..].iter().find_map(|l| {
        let t: Vec<&str> = l.split_whitespace().collect();
        let at = t.iter().position(|x| *x == "Tf")?;
        t.get(at.checked_sub(1)?)?.parse::<f64>().ok()
    })?;
    let first = lines[bt..]
        .iter()
        .find_map(|l| numbers(l, " Td").filter(|n| n.len() == 2))?;
    if !(size.is_finite() && size > 0.0) {
        return None;
    }
    // Rounded so the stream says 11.16, not 11.160000000000002.
    let round = |x: f64| (x * 1000.0).round() / 1000.0;
    let line = round(size * 1.116);
    let mut bands = String::new();
    for &i in chosen {
        let Some(row) = i.checked_sub(top) else {
            continue;
        };
        let baseline = first[1] - (row as f64 + 1.0) * line;
        let y = round(baseline - 0.25 * size);
        if y + line < clip[1] {
            continue;
        }
        bands.push_str(&format!(
            "q\n0.6 0.75 0.85 rg\n{} {y} {} {line} re\nf\nQ\n",
            clip[0], clip[2]
        ));
    }
    if bands.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(content.len() + bands.len());
    for (i, l) in lines.iter().enumerate() {
        if i == bt {
            out.push_str(&bands);
        }
        out.push_str(l);
        out.push('\n');
    }
    Some(out)
}

/// A field's value for comparing and resetting: a choice's values, a text or button value as one,
/// with an empty text and a button's off state as none.
fn normalised(kind: FieldKind, value: &str, values: Vec<String>) -> Vec<String> {
    match kind {
        FieldKind::ComboBox | FieldKind::ListBox => values,
        FieldKind::CheckBox | FieldKind::Radio if value == OFF => Vec::new(),
        _ if value.is_empty() => Vec::new(),
        _ => vec![value.to_owned()],
    }
}

/// A field's default value (`/DV`), as [`normalised`] gives values.
fn default_values(dict: &PdfObject, kind: FieldKind) -> Vec<String> {
    let Some(dv) = dict.get_dict_inheritable("DV").ok().flatten() else {
        return Vec::new();
    };
    let one = |obj: PdfObject| {
        if obj.is_name().unwrap_or(false) {
            obj.as_name()
                .ok()
                .map(|n| String::from_utf8_lossy(&n).into_owned())
        } else {
            text(Some(obj))
        }
    };
    let values: Vec<String> = if dv.is_array().unwrap_or(false) {
        (0..dv.len().unwrap_or(0))
            .filter_map(|i| dv.get_array(i as i32).ok().flatten().and_then(one))
            .collect()
    } else {
        one(dv).into_iter().collect()
    };
    let first = values.first().cloned().unwrap_or_default();
    normalised(kind, &first, values)
}

/// Values in the form [`set_field_values`] takes for a field of this kind: an empty text, a button
/// off, a choice cleared.
fn to_set(kind: FieldKind, values: Vec<String>) -> Vec<String> {
    match kind {
        FieldKind::CheckBox | FieldKind::Radio if values.is_empty() => vec![OFF.to_owned()],
        _ if values.is_empty() => vec![String::new()],
        _ => values,
    }
}

/// Puts the field `name` (its widget on `page`) back to its default value. Returns the changed
/// pages, as [`set_field`] does.
pub(crate) fn reset_field(
    doc: &mut PdfDocument,
    page: u32,
    name: &str,
) -> Result<Vec<u32>, MupdfError> {
    let fail = |source| MupdfError::EditFailed { page, source };
    let pdf_page = doc.load_pdf_page(page as i32).map_err(fail)?;
    let (kind, defaults) = pdf_page
        .widgets()
        .find(|w| w.name().ok().flatten().as_deref() == Some(name))
        .map(|w| {
            let kind = kind(&w);
            (kind, default_values(&w.annotation().object(), kind))
        })
        .ok_or_else(|| MupdfError::FieldNotFound {
            page,
            name: name.to_owned(),
        })?;
    drop(pdf_page);
    set_field_values(doc, page, name, &to_set(kind, defaults))
}

/// Puts every field that can be filled and does not have its default value back to it. Returns
/// every changed page, sorted; none when the form already has its defaults.
pub(crate) fn reset_form(doc: &mut PdfDocument) -> Result<Vec<u32>, MupdfError> {
    let count = doc
        .page_count()
        .map_err(|source| MupdfError::EditFailed { page: 0, source })?;
    let mut seen = std::collections::HashSet::new();
    let mut resets = Vec::new();
    for p in 0..count.max(0) as u32 {
        let fail = |source| MupdfError::EditFailed { page: p, source };
        let pdf_page = doc.load_pdf_page(p as i32).map_err(fail)?;
        for info in widgets(&pdf_page) {
            if !info.fillable() || !seen.insert(info.name.clone()) {
                continue;
            }
            let Some(widget) = pdf_page
                .widgets()
                .find(|w| w.name().ok().flatten().as_deref() == Some(info.name.as_str()))
            else {
                continue;
            };
            let defaults = default_values(&widget.annotation().object(), info.kind);
            if normalised(info.kind, &info.value, info.values.clone()) != defaults {
                resets.push((p, info.name, to_set(info.kind, defaults)));
            }
        }
    }
    let mut pages = Vec::new();
    for (p, name, values) in resets {
        pages.extend(set_field_values(doc, p, &name, &values)?);
    }
    pages.sort_unstable();
    pages.dedup();
    Ok(pages)
}

/// Every field's value, for a data file (Plan 0006 Phase 3): each field once, in page order, as
/// [`FieldValue`] holds it - a text (empty too) or a button's state (`Off` when off) as one value,
/// a choice's chosen values. Read-only fields are written too; push buttons and signatures have no
/// value to write.
pub(crate) fn form_values(doc: &PdfDocument) -> Result<Vec<FieldValue>, MupdfError> {
    let count = doc
        .page_count()
        .map_err(|source| MupdfError::EditFailed { page: 0, source })?;
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for p in 0..count.max(0) as u32 {
        let page = doc
            .load_pdf_page(p as i32)
            .map_err(|source| MupdfError::EditFailed { page: p, source })?;
        for info in widgets(&page) {
            let values = match info.kind {
                FieldKind::Text => vec![info.value],
                FieldKind::CheckBox | FieldKind::Radio if info.value.is_empty() => {
                    vec![OFF.to_owned()]
                }
                FieldKind::CheckBox | FieldKind::Radio => vec![info.value],
                FieldKind::ComboBox | FieldKind::ListBox => info.values,
                FieldKind::PushButton | FieldKind::Signature | FieldKind::Unknown => continue,
            };
            if seen.insert(info.name.clone()) {
                out.push(FieldValue::new(info.name, values));
            }
        }
    }
    Ok(out)
}

/// What an import needs to know of one field: where a widget of it is, and what it takes.
struct Target {
    page: u32,
    kind: FieldKind,
    current: Vec<String>,
    /// A check box's or radio group's "on" states, over all its widgets.
    states: Vec<String>,
    /// A choice field's export values.
    options: Vec<String>,
    editable: bool,
    multi_select: bool,
}

/// The values a field takes from `wanted`, as [`normalised`] gives them, or `None` when it takes
/// none of them: a state the button does not have, an entry the list does not have.
fn accepted(target: &Target, wanted: &[String]) -> Option<Vec<String>> {
    let first = wanted.first().map_or("", String::as_str);
    match target.kind {
        FieldKind::Text => Some(normalised(target.kind, first, Vec::new())),
        FieldKind::CheckBox | FieldKind::Radio => {
            (first == OFF || first.is_empty() || target.states.iter().any(|s| s == first))
                .then(|| normalised(target.kind, first, Vec::new()))
        }
        FieldKind::ComboBox | FieldKind::ListBox => {
            let mut chosen: Vec<String> =
                wanted.iter().filter(|v| !v.is_empty()).cloned().collect();
            if !target.multi_select {
                chosen.truncate(1);
            }
            // An editable combo box takes any text.
            let free = target.kind == FieldKind::ComboBox && target.editable;
            chosen
                .iter()
                .all(|v| free || target.options.contains(v))
                .then_some(chosen)
        }
        FieldKind::PushButton | FieldKind::Signature | FieldKind::Unknown => None,
    }
}

/// Sets fields from a data file (Plan 0006 Phase 3), inside the caller's journal operation. A
/// value for a field the form does not have, a read-only field, or a value the field does not
/// take is left out; so is one the field already has. Returns every changed page, sorted.
pub(crate) fn import_values(
    doc: &mut PdfDocument,
    values: &[FieldValue],
) -> Result<Vec<u32>, MupdfError> {
    let count = doc
        .page_count()
        .map_err(|source| MupdfError::EditFailed { page: 0, source })?;
    let mut targets: HashMap<String, Target> = HashMap::new();
    for p in 0..count.max(0) as u32 {
        let page = doc
            .load_pdf_page(p as i32)
            .map_err(|source| MupdfError::EditFailed { page: p, source })?;
        for info in widgets(&page) {
            if !info.fillable() {
                continue;
            }
            let target = targets.entry(info.name.clone()).or_insert_with(|| Target {
                page: p,
                kind: info.kind,
                current: normalised(info.kind, &info.value, info.values.clone()),
                states: Vec::new(),
                options: info.options.iter().map(|o| o.export.clone()).collect(),
                editable: info.editable,
                multi_select: info.multi_select,
            });
            if let Some(state) = info.on_state {
                target.states.push(state);
            }
        }
    }
    let mut pages = Vec::new();
    for value in values {
        let Some(target) = targets.get(&value.name) else {
            continue;
        };
        let Some(wanted) = accepted(target, &value.values) else {
            continue;
        };
        if wanted != target.current {
            let set = to_set(target.kind, wanted);
            pages.extend(set_field_values(doc, target.page, &value.name, &set)?);
        }
    }
    pages.sort_unstable();
    pages.dedup();
    Ok(pages)
}

/// Draws every field into its page and removes the form (Plan 0006 Phase 3), inside the caller's
/// journal operation: MuPDF's `pdf_bake_document` with widgets only, so other annotations stay
/// annotations. Returns the pages that had a widget, sorted; none when there is no form.
pub(crate) fn flatten(doc: &mut PdfDocument) -> Result<Vec<u32>, MupdfError> {
    let count = doc
        .page_count()
        .map_err(|source| MupdfError::EditFailed { page: 0, source })?;
    let mut pages = Vec::new();
    for p in 0..count.max(0) as u32 {
        let page = doc
            .load_pdf_page(p as i32)
            .map_err(|source| MupdfError::EditFailed { page: p, source })?;
        if page.widgets().next().is_some() {
            pages.push(p);
        }
    }
    if !pages.is_empty() {
        doc.bake(false, true)
            .map_err(|source| MupdfError::EditFailed { page: 0, source })?;
    }
    Ok(pages)
}

/// The next page after `from` (before it when `back`) with a field that can be filled, going round
/// the document's end and coming to `from` itself last.
pub(crate) fn next_field_page(doc: &PdfDocument, from: u32, back: bool) -> Option<u32> {
    let count = doc.page_count().ok()?.max(0) as u32;
    if count == 0 {
        return None;
    }
    (1..=count)
        .map(|step| {
            if back {
                (from + count - step % count) % count
            } else {
                (from + step) % count
            }
        })
        .find(|&p| {
            doc.load_pdf_page(p as i32)
                .is_ok_and(|page| widgets(&page).iter().any(FieldInfo::fillable))
        })
}

/// Works around MuPDF 1.27.2's placement of single-line field text it lays out as rich text -
/// which it does for any letter outside WinAnsi, Greek and KOI8-U, Turkish ğ, ş, ı, İ among them
/// (`text_needs_rich_layout`, `pdf-appearance.c`). `write_rich_content` computes the text's box
/// as if y grew upwards, in a stream where it grows downwards, so the text sits below the field
/// and is clipped: only its upper part shows (Plan 0006 Phase 1). MuPDF fixed it after 1.27.2
/// (Bug 709471, in 1.28.5); this goes once mupdf-rs builds a MuPDF with that fix
/// (`docs/backlog.md` item 10).
///
/// The appearance has this shape, all numbers MuPDF's: `1 0 0 -1 -0 H cm` (the flip to y down,
/// `H` the field's height), then `q`, then `B Y0 W 2S re` (the story's content box: `B` the border,
/// `Y0` its wrong top, `2S` twice the text size). The right top is `B + (H - 2B - S) / 2`, so a
/// translation by the difference, put right after the flip, moves the box and the text with it.
/// Anything else is left as it is, and nothing is done unless the wrappers' headers are 1.27.2's
/// (`journal_headers_match`).
fn fix_single_line_rich_text(widget: &PdfObject) -> Result<(), mupdf::Error> {
    if !mupdf::pdf::journal::journal_headers_match() {
        return Ok(());
    }
    let Some(mut normal) = widget
        .get_dict("AP")?
        .and_then(|ap| ap.get_dict("N").ok().flatten())
        .filter(|n| n.is_stream().unwrap_or(false))
    else {
        return Ok(());
    };
    let content = String::from_utf8_lossy(&normal.read_stream()?).into_owned();
    if let Some(fixed) = shift_rich_content_box(&content) {
        normal.write_stream_buffer(&Buffer::from_bytes(fixed.as_bytes())?)?;
    }
    Ok(())
}

/// The appearance with the translation of [`fix_single_line_rich_text`] put in, or `None` when it
/// does not have the shape that needs it.
pub(crate) fn shift_rich_content_box(content: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    let flip = lines.iter().position(|l| {
        let t: Vec<&str> = l.split_whitespace().collect();
        t.len() == 7 && t[..4] == ["1", "0", "0", "-1"] && t[6] == "cm"
    })?;
    let height: f32 = lines[flip].split_whitespace().nth(5)?.parse().ok()?;
    if lines.get(flip + 1).map(|l| l.trim()) != Some("q") {
        return None;
    }
    let rect: Vec<f32> = lines
        .get(flip + 2)?
        .strip_suffix(" re")?
        .split_whitespace()
        .map(|t| t.parse().ok())
        .collect::<Option<_>>()?;
    let [b, y0, _w, h2] = rect[..] else {
        return None;
    };
    let size = h2 / 2.0;
    let right = b + (height - 2.0 * b - size) / 2.0;
    let shift = right - y0;
    if !shift.is_finite() || shift.abs() < 0.01 {
        return None;
    }
    let mut out = String::with_capacity(content.len() + 32);
    for (i, line) in lines.iter().enumerate() {
        out.push_str(line);
        out.push('\n');
        if i == flip {
            out.push_str(&format!("1 0 0 1 0 {shift} cm\n"));
        }
    }
    Some(out)
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
        // MuPDF 1.27.2's single-line rich text: the box starts at 11.7, it belongs at 4.5.
        let ap = "/Tx BMC\nq\n1 1 248 18 re\nW\nn\n1 0 0 -1 -0 20 cm\nq\n2 11.7 244 22 re\nW n\nBT\nET\nQ\nQ\nEMC\n";
        let fixed = shift_rich_content_box(ap).expect("shifted");
        assert!(
            fixed.contains("1 0 0 -1 -0 20 cm\n1 0 0 1 0 -7.2"),
            "{fixed}"
        );
        assert_eq!(shift_rich_content_box("q\nBT\nET\nQ\n"), None);
        // A list of 10-point options from y 58 down, the second and third chosen, the first row
        // scrolled away (/TI 1): bands under rows 0 and 1, one line (11.16) apart.
        let list = "/Tx BMC\nq\n1 1 248 58 re\nW\nn\nBT\n/Helv 10 Tf 0 g\n2 58 Td\n0 -11.16 Td\n(en) Tj\nET\nQ\nEMC\n";
        let banded = list_with_bands(list, 1, &[1, 2]).expect("banded");
        assert!(banded.contains("1 44.34 248 11.16 re"), "{banded}");
        assert!(banded.contains("1 33.18 248 11.16 re"), "{banded}");
        assert!(banded.find("rg").expect("band") < banded.find("BT").expect("text"));
        assert_eq!(list_with_bands(list, 3, &[1]), None, "scrolled away");
        assert_eq!(font_size("0 g /Helv 0 Tf"), 0.0);
        assert_eq!(font_size(""), 0.0);
    }
}
