// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Redaction (Plan 0009 Phase 1, §4.1): marking areas and applying the marks so that nothing under
//! them is left in the document.
//!
//! MuPDF's `pdf_redact_page` removes the glyphs, image pixels and drawing under the marks
//! (§3.1, §3.6). What it leaves is removed here, in the same journal operation:
//!
//! - annotations and form fields that touch a mark (their text and values);
//! - text it cannot filter (inside patterns, soft masks, Type 3 glyphs): found by extracting the
//!   page's text after its pass, and removed by a second pass that takes drawing that touches a
//!   mark; if text is still found, the redaction fails and the operation is abandoned;
//! - the replacement text (`/ActualText`, `/Alt`, `/E`) of marked sequences whose content changed
//!   ([`pdf_core::content::strip_replacement_text`]), in the page's content and the form XObjects
//!   MuPDF made copies of;
//! - resources the page's content no longer names, so a pattern or soft mask that held the text
//!   is not written;
//! - the page's thumbnail (`/Thumb`) and private application data (`/PieceInfo`).
//!
//! With `clean`, the document's hidden information goes too (§0, question 2): its title, author,
//! subject, keywords and creator, XMP metadata, attachments, JavaScript and structure tree.
//!
//! Every removed object stays in the document's object table until a garbage-collecting save, which
//! is why a redacted document is only written that way (`MupdfEngine::save`).

use crate::MupdfError;
use mupdf::pdf::{
    PdfAnnotationType, PdfDocument, PdfObject, PdfPage, PdfRedactImageMethod,
    PdfRedactLineArtMethod, PdfRedactOptions, PdfRedactTextMethod,
};
use mupdf::{Buffer, Rect as MuRect, TextPageFlags};
use pdf_core::Rect;
use pdf_core::annot::RedactionMark;
use pdf_core::content::{self, REPLACEMENT_KEYS, ResourceNames};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// What [`apply`] did.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Applied {
    /// The pages that had marks, ascending.
    pub pages: Vec<u32>,
    /// The words of the text under the marks (three letters or more, lower case), for finding
    /// outline entries that name them.
    pub words: BTreeSet<String>,
    /// Annotations removed because they touched a mark.
    pub annotations: usize,
    /// Form fields removed because a widget of theirs touched a mark.
    pub fields: usize,
    /// Pages that needed the second pass (text MuPDF's filter does not reach).
    pub second_pass: Vec<u32>,
}

fn to_mu(r: &Rect) -> MuRect {
    MuRect::new(r.x, r.y, r.x + r.width, r.y + r.height)
}

fn overlaps(a: &MuRect, b: &MuRect) -> bool {
    a.x0.max(b.x0) < a.x1.min(b.x1) && a.y0.max(b.y0) < a.y1.min(b.y1)
}

fn contains(r: &MuRect, x: f32, y: f32) -> bool {
    x >= r.x0 && x <= r.x1 && y >= r.y0 && y <= r.y1
}

fn edit_failed(page: u32) -> impl Fn(mupdf::Error) -> MupdfError {
    move |source| MupdfError::EditFailed { page, source }
}

fn load(doc: &PdfDocument, page: u32) -> Result<PdfPage, MupdfError> {
    let loaded = doc
        .load_page(page as i32)
        .map_err(|source| MupdfError::PageLoadFailed {
            page_index: page as usize,
            source,
        })?;
    PdfPage::try_from(loaded).map_err(edit_failed(page))
}

/// Adds redaction marks, each on its page covering its rectangles (one rectangle per line of a
/// search hit, or one dragged area). Runs inside the caller's journal operation; returns the pages
/// that got a mark, ascending.
///
/// The marks are written as objects rather than through `create_annotation` and its setters: each
/// of those opens a journal operation of its own, which MuPDF merges into the caller's by comparing
/// every change with every other (`docs/backlog.md` item 15), so 2000 marks took 9.4 s.
pub(crate) fn mark_all(
    doc: &mut PdfDocument,
    marks: &[RedactionMark],
) -> Result<Vec<u32>, MupdfError> {
    let mut by_page: BTreeMap<u32, Vec<&[Rect]>> = BTreeMap::new();
    for m in marks.iter().filter(|m| !m.rects.is_empty()) {
        by_page.entry(m.page).or_default().push(&m.rects);
    }
    for (&page, rects) in &by_page {
        let pdf_page = load(doc, page)?;
        for rects in rects {
            mark(doc, &pdf_page, rects).map_err(edit_failed(page))?;
        }
    }
    Ok(by_page.into_keys().collect())
}

/// Adds one redaction mark on a loaded page: a `Redact` annotation whose rectangle bounds the
/// rectangles, with one quad per rectangle when there are several.
fn mark(doc: &mut PdfDocument, page: &PdfPage, rects: &[Rect]) -> Result<(), mupdf::Error> {
    let (_, _, back) = crate::edit::visible_space(page)?;
    let mut annot = doc.new_dict()?;
    annot.dict_put("Type", PdfObject::new_name("Annot")?)?;
    annot.dict_put("Subtype", PdfObject::new_name("Redact")?)?;
    // Printable, as MuPDF's own annotations are.
    annot.dict_put("F", PdfObject::new_int(4)?)?;
    let user: Vec<MuRect> = rects.iter().map(|r| to_mu(r).transform(&back)).collect();
    let bounds = user.iter().skip(1).fold(user[0], |u, r| {
        MuRect::new(
            u.x0.min(r.x0),
            u.y0.min(r.y0),
            u.x1.max(r.x1),
            u.y1.max(r.y1),
        )
    });
    let mut rect = doc.new_array()?;
    bounds.encode_into(&mut rect)?;
    annot.dict_put("Rect", rect)?;
    if rects.len() > 1 {
        // Upper left, upper right, lower left, lower right of each, in the PDF's coordinates.
        let mut quads = doc.new_array()?;
        for r in rects.iter().map(to_mu) {
            for (x, y) in [(r.x0, r.y0), (r.x1, r.y0), (r.x0, r.y1), (r.x1, r.y1)] {
                let p = mupdf::Point::new(x, y).transform(&back);
                quads.array_push(PdfObject::new_real(p.x)?)?;
                quads.array_push(PdfObject::new_real(p.y)?)?;
            }
        }
        annot.dict_put("QuadPoints", quads)?;
    }
    let mut page_obj = page.object();
    annot.dict_put("P", page_obj.clone())?;
    let annot = doc.add_object(&annot)?;
    match page_obj.get_dict("Annots")? {
        Some(mut list) if list.is_array()? => list.array_push(annot)?,
        _ => {
            let mut list = doc.new_array()?;
            list.array_push(annot)?;
            page_obj.dict_put("Annots", list)?;
        }
    }
    Ok(())
}

/// Replaces a page's redaction marks by one mark covering the same areas, one quad each. MuPDF
/// tests every glyph, image and drawing against each quad as it does against each mark, but takes
/// each mark away by itself in an operation of its own: 2000 marks on 200 pages cost 0.27 ms a
/// mark that way (`docs/benchmarks.md` §15.3).
fn merge_marks(doc: &mut PdfDocument, page: u32, areas: &[MuRect]) -> Result<(), mupdf::Error> {
    let pdf_page = load(doc, page).map_err(|e| mupdf::Error::InvalidArgument(e.to_string()))?;
    let mut page_obj = pdf_page.object();
    let Some(mut list) = page_obj.get_dict("Annots")? else {
        return Ok(());
    };
    let is_mark = |annot: &PdfObject| {
        annot
            .get_dict("Subtype")
            .ok()
            .flatten()
            .and_then(|s| s.as_name().ok())
            .is_some_and(|s| s == b"Redact")
    };
    let len = list.len()? as i32;
    let marks = (0..len)
        .filter(|&i| {
            list.get_array(i)
                .ok()
                .flatten()
                .is_some_and(|a| is_mark(&a))
        })
        .count();
    if marks < 2 {
        return Ok(());
    }
    for i in (0..len).rev() {
        if list.get_array(i)?.is_some_and(|a| is_mark(&a)) {
            list.array_delete(i)?;
        }
    }
    page_obj.dict_put("Annots", list)?;
    let rects: Vec<Rect> = areas
        .iter()
        .map(|a| Rect::new(a.x0, a.y0, a.x1 - a.x0, a.y1 - a.y0))
        .collect();
    mark(doc, &pdf_page, &rects)
}

/// The areas of a page's redaction marks: each mark's quads, or its rectangle.
fn marks(page: &PdfPage) -> Result<Vec<MuRect>, mupdf::Error> {
    let mut out = Vec::new();
    for annot in page.annotations() {
        if annot.r#type()? != PdfAnnotationType::Redact {
            continue;
        }
        let quads = annot.quad_points().unwrap_or_default();
        if quads.is_empty() {
            out.push(annot.rect()?);
        } else {
            out.extend(quads.iter().map(|q| {
                let xs = [q.ul.x, q.ur.x, q.ll.x, q.lr.x];
                let ys = [q.ul.y, q.ur.y, q.ll.y, q.lr.y];
                MuRect::new(
                    xs.iter().copied().fold(f32::INFINITY, f32::min),
                    ys.iter().copied().fold(f32::INFINITY, f32::min),
                    xs.iter().copied().fold(f32::NEG_INFINITY, f32::max),
                    ys.iter().copied().fold(f32::NEG_INFINITY, f32::max),
                )
            }));
        }
    }
    Ok(out)
}

/// The characters MuPDF extracts from the page whose centre lies under a mark, with a marker
/// (`None`) where a run of them ends.
fn text_under(page: &PdfPage, areas: &[MuRect]) -> Result<Vec<Option<char>>, mupdf::Error> {
    let text = page.to_text_page(TextPageFlags::empty())?;
    let mut out = Vec::new();
    for block in text.blocks() {
        for line in block.lines() {
            for ch in line.chars() {
                let q = ch.quad();
                let cx = (q.ul.x + q.ur.x + q.ll.x + q.lr.x) / 4.0;
                let cy = (q.ul.y + q.ur.y + q.ll.y + q.lr.y) / 4.0;
                if areas.iter().any(|a| contains(a, cx, cy)) {
                    out.push(ch.char());
                } else if out.last().is_some_and(Option::is_some) {
                    out.push(None);
                }
            }
            if out.last().is_some_and(Option::is_some) {
                out.push(None);
            }
        }
    }
    Ok(out)
}

/// Whether any character of `text_under`'s answer is not white space.
fn any_text(chars: &[Option<char>]) -> bool {
    chars.iter().flatten().any(|c| !c.is_whitespace())
}

fn words(chars: &[Option<char>], into: &mut BTreeSet<String>) {
    let mut word = String::new();
    for c in chars.iter().copied().chain([None]) {
        match c {
            Some(c) if c.is_alphanumeric() => word.extend(c.to_lowercase()),
            _ => {
                if word.chars().count() >= 3 {
                    into.insert(std::mem::take(&mut word));
                }
                word.clear();
            }
        }
    }
}

/// A content stream and what its resource names stood for, read before MuPDF's pass.
struct Snapshot {
    content: Vec<u8>,
    /// XObject names to their object numbers, with the content of the forms among them.
    xobjects: HashMap<Vec<u8>, (i32, Option<Snapshot>)>,
    shadings: HashMap<Vec<u8>, i32>,
}

/// How deep forms inside forms are followed.
const FORM_DEPTH: u32 = 6;

fn entries(dict: Option<PdfObject>) -> Result<Vec<(Vec<u8>, PdfObject)>, mupdf::Error> {
    let Some(dict) = dict else {
        return Ok(Vec::new());
    };
    if !dict.is_dict()? {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in dict.dict_iter()? {
        let (key, value) = entry?;
        out.push((key.as_name()?, value));
    }
    Ok(out)
}

fn object_number(obj: &PdfObject) -> i32 {
    if obj.is_indirect().unwrap_or(false) {
        obj.as_indirect().unwrap_or(0)
    } else {
        0
    }
}

fn snapshot(
    content: Vec<u8>,
    resources: Option<PdfObject>,
    depth: u32,
) -> Result<Snapshot, mupdf::Error> {
    let mut xobjects = HashMap::new();
    let mut shadings = HashMap::new();
    if let Some(res) = resources.filter(|r| r.is_dict().unwrap_or(false)) {
        for (name, xobject) in entries(res.get_dict("XObject")?)? {
            let form = if depth < FORM_DEPTH && is_form(&xobject)? {
                let inner = xobject.read_stream().unwrap_or_default();
                Some(snapshot(inner, xobject.get_dict("Resources")?, depth + 1)?)
            } else {
                None
            };
            xobjects.insert(name, (object_number(&xobject), form));
        }
        for (name, shading) in entries(res.get_dict("Shading")?)? {
            shadings.insert(name, object_number(&shading));
        }
    }
    Ok(Snapshot {
        content,
        xobjects,
        shadings,
    })
}

fn is_form(xobject: &PdfObject) -> Result<bool, mupdf::Error> {
    Ok(xobject
        .get_dict("Subtype")?
        .and_then(|s| s.as_name().ok())
        .is_some_and(|s| s == b"Form"))
}

/// A page's content: its stream, or its streams joined.
fn page_content(page: &PdfPage) -> Result<Vec<u8>, mupdf::Error> {
    let Some(contents) = page.contents()? else {
        return Ok(Vec::new());
    };
    if contents.is_array()? {
        let mut out = Vec::new();
        for item in contents.array_iter()? {
            out.extend(item?.read_stream()?);
            out.push(b'\n');
        }
        Ok(out)
    } else {
        contents.read_stream()
    }
}

fn page_resources(page: &PdfPage) -> Result<Option<PdfObject>, mupdf::Error> {
    page.object().get_dict_inheritable("Resources")
}

/// Answers the object a `Do` or `sh` name stood for in a snapshot.
fn resolver(snap: &Snapshot) -> impl Fn(&[u8], &[u8]) -> Option<i64> + '_ {
    move |op, name| {
        let number = match op {
            b"Do" => snap.xobjects.get(name).map(|(n, _)| *n),
            _ => snap.shadings.get(name).copied(),
        }?;
        (number > 0).then_some(i64::from(number))
    }
}

/// Answers the object a `Do` or `sh` name stands for in a resource dictionary now.
fn live_resolver(resources: Option<&PdfObject>) -> impl Fn(&[u8], &[u8]) -> Option<i64> + '_ {
    move |op, name| {
        let res = resources?;
        let category = if op == b"Do" { "XObject" } else { "Shading" };
        let dict = res.get_dict(category).ok()??;
        let key = String::from_utf8(name.to_vec()).ok()?;
        let obj = dict.get_dict(key).ok()??;
        let number = object_number(&obj);
        (number > 0).then_some(i64::from(number))
    }
}

fn has_replacement_text(dict: &PdfObject) -> bool {
    REPLACEMENT_KEYS.iter().any(|key| {
        std::str::from_utf8(key)
            .ok()
            .and_then(|key| dict.get_dict(key).ok().flatten())
            .is_some()
    })
}

/// Cuts the replacement text out of `content` where its sequence changed and the state nothing is
/// painted with, then the resources to what the result names; recurses into the form XObjects
/// MuPDF replaced. Returns the new content when it differs from `content`, and the new resources
/// when some went.
fn tidy(
    doc: &mut PdfDocument,
    before: &Snapshot,
    content: &[u8],
    resources: Option<&PdfObject>,
    depth: u32,
) -> Result<(Option<Vec<u8>>, Option<PdfObject>), mupdf::Error> {
    let named_has_text = |name: &[u8]| {
        let Some(res) = resources else {
            return true;
        };
        let Ok(key) = String::from_utf8(name.to_vec()) else {
            return true;
        };
        res.get_dict("Properties")
            .ok()
            .flatten()
            .and_then(|props| props.get_dict(key).ok().flatten())
            .is_none_or(|entry| has_replacement_text(&entry))
    };
    let stripped = content::strip_replacement_text(
        &before.content,
        content,
        &resolver(before),
        &live_resolver(resources),
        &named_has_text,
    );
    // Then the graphics states and colours left with nothing painted after them: a soft mask or a
    // pattern whose fill went is named there still.
    let idle = content::drop_idle_state(stripped.as_deref().unwrap_or(content));
    let stripped = idle.or(stripped);
    let now = stripped.as_deref().unwrap_or(content);
    let used = content::resource_names(now);

    // Forms MuPDF replaced with filtered copies: tidy them the same way.
    if depth < FORM_DEPTH
        && let Some(res) = resources
    {
        for (name, form) in entries(res.get_dict("XObject")?)? {
            if !used.x_object.contains(&name) || !is_form(&form)? {
                continue;
            }
            let Some((was, Some(inner))) = before.xobjects.get(&name) else {
                continue;
            };
            if object_number(&form) == *was {
                continue;
            }
            // Through its reference: MuPDF updates a stream only as an indirect object.
            let mut form = form;
            let form_content = form.read_stream()?;
            let form_res = form.get_dict("Resources")?;
            let (new_content, new_res) =
                tidy(doc, inner, &form_content, form_res.as_ref(), depth + 1)?;
            if let Some(new_content) = new_content {
                form.write_stream_buffer(&Buffer::from_bytes(&new_content)?)?;
            }
            if let Some(new_res) = new_res {
                form.dict_put("Resources", new_res)?;
            }
        }
    }

    let pruned = match resources {
        Some(res) => prune(doc, res, &used)?,
        None => None,
    };
    Ok((stripped, pruned))
}

/// A copy of `resources` with only the names `used` lists, or `None` when nothing goes.
fn prune(
    doc: &mut PdfDocument,
    resources: &PdfObject,
    used: &ResourceNames,
) -> Result<Option<PdfObject>, mupdf::Error> {
    let mut out = doc.new_dict()?;
    let mut dropped = false;
    for entry in resources.dict_iter()? {
        let (key, value) = entry?;
        let key_name = key.as_name()?;
        let category = std::str::from_utf8(&key_name)
            .ok()
            .and_then(|k| used.category(k));
        let Some(names) = category else {
            out.dict_put(key, value)?;
            continue;
        };
        let Some(sub) = value.resolve()?.filter(|v| v.is_dict().unwrap_or(false)) else {
            out.dict_put(key, value)?;
            continue;
        };
        let mut kept = doc.new_dict()?;
        for item in sub.dict_iter()? {
            let (name, obj) = item?;
            if names.contains(&name.as_name()?) {
                kept.dict_put(name, obj)?;
            } else {
                dropped = true;
            }
        }
        if kept.dict_len()? > 0 {
            out.dict_put(key, kept)?;
        }
    }
    Ok(dropped.then_some(out))
}

/// Replaces a page's content with `bytes`, as one stream.
fn set_page_content(
    doc: &mut PdfDocument,
    page: &PdfPage,
    bytes: &[u8],
) -> Result<(), mupdf::Error> {
    let buffer = Buffer::from_bytes(bytes)?;
    match page.contents()? {
        Some(mut stream) if stream.is_stream()? => stream.write_stream_buffer(&buffer),
        _ => {
            let stream = doc.add_stream(&buffer, None, false)?;
            page.object().dict_put("Contents", stream)
        }
    }
}

/// Whether two objects are the same indirect object.
fn same_object(a: &PdfObject, b: &PdfObject) -> bool {
    let (x, y) = (object_number(a), object_number(b));
    x > 0 && x == y
}

/// Removes `item` from a field array (`/Fields` or a `/Kids`) or anywhere below it; returns
/// whether it was found.
fn remove_from_tree(array: &mut PdfObject, item: &PdfObject, depth: u32) -> bool {
    if depth > 32 || !array.is_array().unwrap_or(false) {
        return false;
    }
    let len = array.len().unwrap_or(0) as i32;
    for i in 0..len {
        let Ok(Some(child)) = array.get_array(i) else {
            continue;
        };
        if same_object(&child, item) {
            return array.array_delete(i).is_ok();
        }
        if let Ok(Some(mut kids)) = child.get_dict("Kids")
            && remove_from_tree(&mut kids, item, depth + 1)
        {
            return true;
        }
    }
    false
}

/// The widgets of a field: its own `/Kids` (recursively), or the field itself when it has none.
fn widgets_of(field: &PdfObject, out: &mut Vec<PdfObject>, depth: u32) {
    match field.get_dict("Kids") {
        Ok(Some(kids)) if depth < 32 && kids.is_array().unwrap_or(false) => {
            for kid in kids.array_iter().into_iter().flatten().flatten() {
                widgets_of(&kid, out, depth + 1);
            }
        }
        _ => out.push(field.clone()),
    }
}

/// The field a widget's value belongs to: the topmost ancestor with a name, since a value is
/// inherited down the tree.
fn field_root(widget: &PdfObject) -> PdfObject {
    let mut field = widget.clone();
    let mut node = widget.clone();
    for _ in 0..32 {
        if node.get_dict("T").ok().flatten().is_some() {
            field = node.clone();
        }
        match node.get_dict("Parent") {
            Ok(Some(parent)) => node = parent,
            _ => break,
        }
    }
    field
}

/// Removes a whole field: every widget from its page and the field from the form's tree.
fn remove_field(doc: &mut PdfDocument, field: &PdfObject) -> Result<(), MupdfError> {
    let failed = |source| MupdfError::Backend(format!("remove field: {source}"));
    let mut widgets = Vec::new();
    widgets_of(field, &mut widgets, 0);
    let count = doc.page_count().map_err(failed)?;
    for widget in &widgets {
        let number = object_number(widget);
        for page in 0..count.max(0) as u32 {
            let pdf_page = load(doc, page)?;
            let on_page = pdf_page
                .object()
                .get_dict("Annots")
                .ok()
                .flatten()
                .is_some_and(|annots| {
                    annots
                        .array_iter()
                        .into_iter()
                        .flatten()
                        .flatten()
                        .any(|a| object_number(&a) == number)
                });
            if !on_page {
                continue;
            }
            let mut pdf_page = pdf_page;
            if let Some(w) = pdf_page.load_widget(number).map_err(failed)? {
                pdf_page.delete_widget(w).map_err(failed)?;
            }
        }
    }
    // MuPDF takes each widget out of the tree; the field above them, which holds the value, stays.
    let root = doc.catalog().map_err(failed)?;
    if let Some(form) = root.get_dict("AcroForm").map_err(failed)?
        && let Some(mut fields) = form.get_dict("Fields").map_err(failed)?
    {
        remove_from_tree(&mut fields, field, 0);
    }
    Ok(())
}

/// Removes the annotations and form fields of `page` that touch a mark; returns how many of each.
fn remove_touching(
    doc: &mut PdfDocument,
    page: u32,
    areas: &[MuRect],
) -> Result<(usize, usize), MupdfError> {
    let failed = edit_failed(page);
    let mut pdf_page = load(doc, page)?;
    // Annotations: not the marks, not links (MuPDF removes those), not popups (they go with
    // their parent, and show its text, not their own).
    let mut doomed = Vec::new();
    for annot in pdf_page.annotations() {
        let kind = annot.r#type().map_err(&failed)?;
        if matches!(
            kind,
            PdfAnnotationType::Redact | PdfAnnotationType::Link | PdfAnnotationType::Popup
        ) {
            continue;
        }
        let rect = annot.rect().map_err(&failed)?;
        if areas.iter().any(|a| overlaps(a, &rect)) {
            doomed.push(object_number(&annot.object()));
        }
    }
    let annotations = doomed.len();
    for number in doomed {
        let found = pdf_page
            .annotations()
            .find(|a| object_number(&a.object()) == number);
        if let Some(annot) = found {
            pdf_page.delete_annotation(annot).map_err(&failed)?;
        }
    }
    // Fields: the whole field of any widget that touches, with its value and all its widgets.
    let mut fields = Vec::new();
    for widget in pdf_page.widgets() {
        let rect = widget.annotation().rect().map_err(&failed)?;
        if areas.iter().any(|a| overlaps(a, &rect)) {
            let root = field_root(&widget.annotation().object());
            if !fields.iter().any(|f| same_object(f, &root)) {
                fields.push(root);
            }
        }
    }
    drop(pdf_page);
    for field in &fields {
        remove_field(doc, field)?;
    }
    Ok((annotations, fields.len()))
}

fn options(second_pass: bool) -> PdfRedactOptions {
    PdfRedactOptions {
        // Both passes draw the boxes: the second removes drawing that touches a mark, the first
        // pass's boxes among it.
        black_boxes: true,
        image_method: PdfRedactImageMethod::Pixels,
        line_art: if second_pass {
            PdfRedactLineArtMethod::RemoveIfTouched
        } else {
            PdfRedactLineArtMethod::RemoveIfCovered
        },
        text: PdfRedactTextMethod::Remove,
    }
}

/// Applies the marks of every page (§4.1) inside the caller's journal operation. Fails with
/// [`MupdfError::NothingToRedact`] when no page has a mark, and with
/// [`MupdfError::RedactionIncomplete`] when text is still found under a mark after both passes; the
/// caller abandons the operation then.
pub(crate) fn apply(doc: &mut PdfDocument, clean: bool) -> Result<Applied, MupdfError> {
    let count = doc
        .page_count()
        .map_err(|e| MupdfError::Backend(e.to_string()))?
        .max(0) as u32;
    let mut applied = Applied::default();
    for page in 0..count {
        let failed = edit_failed(page);
        let pdf_page = load(doc, page)?;
        let areas = marks(&pdf_page).map_err(&failed)?;
        if areas.is_empty() {
            continue;
        }
        applied.pages.push(page);
        words(
            &text_under(&pdf_page, &areas).map_err(&failed)?,
            &mut applied.words,
        );
        let before = snapshot(
            page_content(&pdf_page).map_err(&failed)?,
            page_resources(&pdf_page).map_err(&failed)?,
            0,
        )
        .map_err(&failed)?;
        drop(pdf_page);

        let (annotations, fields) = remove_touching(doc, page, &areas)?;
        applied.annotations += annotations;
        applied.fields += fields;

        merge_marks(doc, page, &areas).map_err(&failed)?;
        let mut pdf_page = load(doc, page)?;
        pdf_page
            .redact_with_options(options(false))
            .map_err(&failed)?;
        drop(pdf_page);

        // Text MuPDF's filter does not reach (patterns, soft masks, Type 3 glyphs).
        let pdf_page = load(doc, page)?;
        if any_text(&text_under(&pdf_page, &areas).map_err(&failed)?) {
            drop(pdf_page);
            applied.second_pass.push(page);
            let rects: Vec<Rect> = areas
                .iter()
                .map(|a| Rect::new(a.x0, a.y0, a.x1 - a.x0, a.y1 - a.y0))
                .collect();
            // Loaded again after the new marks, so that MuPDF reads them with the page.
            let marked = load(doc, page)?;
            for rect in &rects {
                mark(doc, &marked, std::slice::from_ref(rect)).map_err(&failed)?;
            }
            drop(marked);
            let mut pdf_page = load(doc, page)?;
            pdf_page
                .redact_with_options(options(true))
                .map_err(&failed)?;
            drop(pdf_page);
            let pdf_page = load(doc, page)?;
            if any_text(&text_under(&pdf_page, &areas).map_err(&failed)?) {
                return Err(MupdfError::RedactionIncomplete { page });
            }
        }

        // Replacement text, forms and resources.
        let pdf_page = load(doc, page)?;
        let now = page_content(&pdf_page).map_err(&failed)?;
        let resources = page_resources(&pdf_page).map_err(&failed)?;
        let (new_content, new_resources) =
            tidy(doc, &before, &now, resources.as_ref(), 0).map_err(&failed)?;
        if let Some(bytes) = new_content {
            set_page_content(doc, &pdf_page, &bytes).map_err(&failed)?;
        }
        let mut page_obj = pdf_page.object();
        if let Some(res) = new_resources {
            page_obj.dict_put("Resources", res).map_err(&failed)?;
        }
        for key in ["Thumb", "PieceInfo"] {
            page_obj.dict_delete(key).map_err(&failed)?;
        }
    }
    if applied.pages.is_empty() {
        return Err(MupdfError::NothingToRedact);
    }
    if clean {
        clean_document(doc).map_err(|e| MupdfError::Backend(format!("clean-up: {e}")))?;
    }
    Ok(applied)
}

/// Removes the document's hidden information (§0, question 2): the information fields that
/// describe it (title, author, subject, keywords, creator, and any of its own), the XMP metadata,
/// attachments, JavaScript (document scripts, open actions, additional actions of the document,
/// its pages and annotations) and the structure tree. The producer and the dates stay.
pub(crate) fn clean_document(doc: &mut PdfDocument) -> Result<(), mupdf::Error> {
    let mut trailer = doc.trailer()?;
    if let Some(info) = trailer.get_dict("Info")? {
        let mut kept = doc.new_dict()?;
        for key in ["Producer", "CreationDate", "ModDate"] {
            if let Some(value) = info.get_dict(key)? {
                kept.dict_put(key, value)?;
            }
        }
        let kept = doc.add_object(&kept)?;
        trailer.dict_put("Info", kept)?;
    }
    let mut root = doc.catalog()?;
    for key in [
        "Metadata",
        "StructTreeRoot",
        "MarkInfo",
        "AA",
        "PieceInfo",
        "Collection",
    ] {
        root.dict_delete(key)?;
    }
    if let Some(action) = root.get_dict("OpenAction")?
        && is_script(&action)
    {
        root.dict_delete("OpenAction")?;
    }
    if let Some(mut names) = root.get_dict("Names")? {
        for key in ["EmbeddedFiles", "JavaScript"] {
            names.dict_delete(key)?;
        }
    }
    let count = doc.page_count()?.max(0);
    for page in 0..count {
        let pdf_page = PdfPage::try_from(doc.load_page(page)?)?;
        let mut page_obj = pdf_page.object();
        for key in ["AA", "StructParents", "Metadata", "PieceInfo"] {
            page_obj.dict_delete(key)?;
        }
        let mut attachments = Vec::new();
        if let Some(annots) = page_obj.get_dict("Annots")? {
            for annot in annots.array_iter()? {
                let Some(mut annot) = annot?.resolve()? else {
                    continue;
                };
                annot.dict_delete("AA")?;
                annot.dict_delete("StructParent")?;
                if let Some(action) = annot.get_dict("A")?
                    && is_script(&action)
                {
                    annot.dict_delete("A")?;
                }
                if annot
                    .get_dict("Subtype")?
                    .and_then(|s| s.as_name().ok())
                    .is_some_and(|s| s == b"FileAttachment")
                {
                    attachments.push(object_number(&annot));
                }
            }
        }
        if !attachments.is_empty() {
            let mut pdf_page = pdf_page;
            let doomed: Vec<_> = pdf_page
                .annotations()
                .filter(|a| attachments.contains(&object_number(&a.object())))
                .collect();
            for annot in doomed {
                pdf_page.delete_annotation(annot)?;
            }
        }
    }
    // Scripts in the form's fields (calculations, formats, keystrokes).
    if let Some(form) = root.get_dict("AcroForm")? {
        if let Some(fields) = form.get_dict("Fields")? {
            strip_field_actions(&fields, 0)?;
        }
        let mut form = form;
        form.dict_delete("XFA")?;
    }
    Ok(())
}

/// The outline's entries in the order MuPDF lists them (depth-first, `/First` then `/Next`), the
/// order of [`crate::MupdfEngine::outline`]'s flattened list.
fn outline_nodes(first: Option<PdfObject>, depth: u32, out: &mut Vec<PdfObject>) {
    let mut node = first;
    // A broken outline may loop; it cannot have more entries than this.
    while let Some(item) = node {
        if depth > 64 || out.len() > 100_000 || out.iter().any(|seen| same_object(seen, &item)) {
            return;
        }
        out.push(item.clone());
        outline_nodes(item.get_dict("First").ok().flatten(), depth + 1, out);
        node = item.get_dict("Next").ok().flatten();
    }
}

/// Removes outline entries, by their index in the flattened outline, with everything under them
/// (Plan 0009, §0 question 2: the entries that name redacted text, when the user says so).
/// Returns how many of `items` were found.
pub(crate) fn remove_outline(doc: &mut PdfDocument, items: &[u32]) -> Result<usize, mupdf::Error> {
    let root = doc.catalog()?;
    let Some(outlines) = root.get_dict("Outlines")? else {
        return Ok(0);
    };
    let mut nodes = Vec::new();
    outline_nodes(outlines.get_dict("First")?, 0, &mut nodes);
    let doomed: Vec<PdfObject> = items
        .iter()
        .filter_map(|&i| nodes.get(i as usize).cloned())
        .collect();
    let mut gone: Vec<PdfObject> = Vec::new();
    for mut node in doomed {
        // Already gone with an entry above it.
        let mut up = Some(node.clone());
        let mut under_gone = false;
        for _ in 0..64 {
            let Some(item) = up else {
                break;
            };
            if gone.iter().any(|g| same_object(g, &item)) {
                under_gone = true;
                break;
            }
            up = item.get_dict("Parent")?;
        }
        if under_gone {
            continue;
        }
        let Some(mut parent) = node.get_dict("Parent")? else {
            continue;
        };
        let prev = node.get_dict("Prev")?;
        let next = node.get_dict("Next")?;
        match (&prev, &next) {
            (Some(p), Some(n)) => {
                p.clone().dict_put("Next", n.clone())?;
                n.clone().dict_put("Prev", p.clone())?;
            }
            (Some(p), None) => {
                p.clone().dict_delete("Next")?;
                parent.dict_put("Last", p.clone())?;
            }
            (None, Some(n)) => {
                n.clone().dict_delete("Prev")?;
                parent.dict_put("First", n.clone())?;
            }
            (None, None) => {
                parent.dict_delete("First")?;
                parent.dict_delete("Last")?;
            }
        }
        // The open ancestors showed the entry and its open descendants; a closed one counts them
        // as a negative number, and the entries above it never showed them.
        let own = node
            .get_dict("Count")?
            .and_then(|c| c.as_int().ok())
            .unwrap_or(0);
        let mut shown = 1 + own.max(0);
        let mut ancestor = Some(parent);
        for _ in 0..64 {
            let Some(mut up) = ancestor else {
                break;
            };
            let count = up.get_dict("Count")?.and_then(|c| c.as_int().ok());
            match count {
                Some(c) if c > 0 => up.dict_put("Count", doc.new_int((c - shown).max(0))?)?,
                Some(c) if c < 0 => {
                    up.dict_put("Count", doc.new_int((c + shown).min(0))?)?;
                    shown = 0;
                }
                _ => {}
            }
            if shown == 0 {
                break;
            }
            ancestor = up.get_dict("Parent")?;
        }
        node.dict_delete("Parent")?;
        node.dict_delete("Prev")?;
        node.dict_delete("Next")?;
        gone.push(node);
    }
    Ok(gone.len())
}

fn is_script(action: &PdfObject) -> bool {
    action
        .get_dict("S")
        .ok()
        .flatten()
        .and_then(|s| s.as_name().ok())
        .is_some_and(|s| s == b"JavaScript")
}

fn strip_field_actions(fields: &PdfObject, depth: u32) -> Result<(), mupdf::Error> {
    if depth > 32 || !fields.is_array()? {
        return Ok(());
    }
    for field in fields.array_iter()? {
        let Some(mut field) = field?.resolve()? else {
            continue;
        };
        field.dict_delete("AA")?;
        if let Some(kids) = field.get_dict("Kids")? {
            strip_field_actions(&kids, depth + 1)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_of_the_text_under_the_marks() {
        let chars: Vec<Option<char>> = "Gizli Şirket ab"
            .chars()
            .map(Some)
            .chain([None])
            .chain("x-42 rapor".chars().map(Some))
            .collect();
        let mut out = BTreeSet::new();
        words(&chars, &mut out);
        let got: Vec<&str> = out.iter().map(String::as_str).collect();
        assert_eq!(got, ["gizli", "rapor", "şirket"]);
        assert!(!any_text(&[Some(' '), None, Some('\n')]));
        assert!(any_text(&[None, Some('a')]));
    }

    #[test]
    fn overlap_needs_area() {
        let a = MuRect::new(0.0, 0.0, 10.0, 10.0);
        assert!(overlaps(&a, &MuRect::new(9.0, 9.0, 20.0, 20.0)));
        // Touching edges is not overlapping: a neighbour line is left alone.
        assert!(!overlaps(&a, &MuRect::new(10.0, 0.0, 20.0, 10.0)));
    }

    #[test]
    fn marks_merged_on_a_page_need_no_second_pass() {
        let content = "BT /F1 14 Tf 72 700 Td (FIRST-SECRET) Tj ET
                       BT /F1 14 Tf 72 400 Td (SECOND-SECRET) Tj ET
                       BT /F1 14 Tf 72 100 Td (PUBLIC) Tj ET";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R              /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_owned(),
            format!("<< /Length {} >>
stream
{content}
endstream", content.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        ];
        let mut bytes = b"%PDF-1.7
"
        .to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            bytes.extend_from_slice(
                format!(
                    "{} 0 obj
{body}
endobj
",
                    i + 1
                )
                .as_bytes(),
            );
        }
        let xref = bytes.len();
        bytes.extend_from_slice(
            b"xref
0 6
0000000000 65535 f 
",
        );
        for o in offsets {
            bytes.extend_from_slice(
                format!(
                    "{o:010} 00000 n 
"
                )
                .as_bytes(),
            );
        }
        bytes.extend_from_slice(
            format!(
                "trailer
<< /Size 6 /Root 1 0 R >>
startxref
{xref}
%%EOF
"
            )
            .as_bytes(),
        );
        let mut doc = PdfDocument::from_bytes(&bytes).expect("open");
        let page = load(&doc, 0).expect("page");
        let mut marks = Vec::new();
        for word in ["FIRST-SECRET", "SECOND-SECRET"] {
            let hits = page.search(word, 1).expect("search");
            let q = &hits[0];
            let r = MuRect::new(
                q.ul.x.min(q.ll.x),
                q.ul.y.min(q.ur.y),
                q.ur.x.max(q.lr.x),
                q.ll.y.max(q.lr.y),
            );
            marks.push(RedactionMark {
                page: 0,
                rects: vec![Rect::new(r.x0, r.y0, r.x1 - r.x0, r.y1 - r.y0)],
            });
        }
        drop(page);
        assert_eq!(mark_all(&mut doc, &marks).expect("mark"), [0]);
        let applied = apply(&mut doc, false).expect("apply");
        // Every mark reached MuPDF's pass: the safety net had nothing to catch.
        assert!(applied.second_pass.is_empty(), "{applied:?}");
        let text = load(&doc, 0)
            .expect("page")
            .to_text_page(TextPageFlags::empty())
            .expect("text")
            .to_text()
            .expect("text");
        assert!(
            text.contains("PUBLIC") && !text.contains("SECRET"),
            "{text}"
        );
    }
}
