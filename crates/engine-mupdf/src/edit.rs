// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Reading and editing a PDF's annotations (Plan 0004, §3.2 and §4.1).
//!
//! The engine actor owns the document; these functions run on it, between two commands, so an
//! edit never races a display-list build. Worker threads keep replaying the display lists they
//! were given - a list is a snapshot, measured safe against concurrent edits in
//! `docs/benchmarks.md` §10.1 - and the page's revision tells the app which pixels are current.

use crate::MupdfError;
use mupdf::color::AnnotationColor;
use mupdf::pdf::{
    InsertPdfOptions, InsertPosition, PdfAnnotation, PdfAnnotationType, PdfDocument, PdfObject,
    PdfPage,
};
use mupdf::{Buffer, Document, Font, Matrix, MetadataName};
use mupdf::{Point, Quad, Rect as MuRect, Size};
use pdf_core::Rect;
use pdf_core::annot::{AnnotInfo, AnnotKind, Colour, EditOp, MarkupKind, cut};
use pdf_core::pages::{self, PageOp, PageOpError};
use pdf_core::stamp::{self, Stamp};
use pdf_core::tools::{DocumentInfo, Metadata, pdf_date};
use std::path::Path;

fn to_rect(r: MuRect) -> Rect {
    Rect::new(r.x0, r.y0, r.x1 - r.x0, r.y1 - r.y0)
}

fn to_mu(r: &Rect) -> MuRect {
    MuRect::new(r.x, r.y, r.x + r.width, r.y + r.height)
}

/// The axis-aligned box of a quad: markup quads are upright rectangles for horizontal text.
fn quad_box(q: &Quad) -> Rect {
    let xs = [q.ul.x, q.ur.x, q.ll.x, q.lr.x];
    let ys = [q.ul.y, q.ur.y, q.ll.y, q.lr.y];
    let (x0, x1) = (
        xs.iter().copied().fold(f32::INFINITY, f32::min),
        xs.iter().copied().fold(f32::NEG_INFINITY, f32::max),
    );
    let (y0, y1) = (
        ys.iter().copied().fold(f32::INFINITY, f32::min),
        ys.iter().copied().fold(f32::NEG_INFINITY, f32::max),
    );
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

fn to_colour(colour: AnnotationColor) -> Colour {
    match colour {
        AnnotationColor::Gray(g) => Colour::new(g, g, g),
        AnnotationColor::Rgb { red, green, blue } => Colour::new(red, green, blue),
        AnnotationColor::Cmyk {
            cyan,
            magenta,
            yellow,
            key,
        } => Colour::new(
            (1.0 - cyan) * (1.0 - key),
            (1.0 - magenta) * (1.0 - key),
            (1.0 - yellow) * (1.0 - key),
        ),
    }
}

fn to_annotation_colour(colour: Colour) -> AnnotationColor {
    AnnotationColor::Rgb {
        red: colour.r,
        green: colour.g,
        blue: colour.b,
    }
}

/// The kind the app lists an annotation as; `None` for the ones it does not list at all: links
/// (navigation, not marks), form widgets (v0.4) and popups (part of their parent note).
fn kind_of(annot_type: PdfAnnotationType) -> Option<AnnotKind> {
    use PdfAnnotationType as T;
    Some(match annot_type {
        T::Highlight => AnnotKind::Highlight,
        T::Underline => AnnotKind::Underline,
        T::StrikeOut => AnnotKind::StrikeOut,
        T::Squiggly => AnnotKind::Squiggly,
        T::Text => AnnotKind::Note,
        T::Ink => AnnotKind::Ink,
        T::Link | T::Widget | T::Popup => return None,
        _ => AnnotKind::Other,
    })
}

fn load(doc: &PdfDocument, page: u32) -> Result<PdfPage, MupdfError> {
    let loaded = doc
        .load_page(page as i32)
        .map_err(|source| MupdfError::PageLoadFailed {
            page_index: page as usize,
            source,
        })?;
    PdfPage::try_from(loaded).map_err(|source| MupdfError::EditFailed { page, source })
}

fn describe(annot: &PdfAnnotation) -> Option<AnnotInfo> {
    let kind = kind_of(annot.r#type().ok()?)?;
    let id = u32::try_from(annot.xref().ok()?).ok()?;
    let rect = to_rect(annot.bounds().ok()?);
    let quads = match kind {
        AnnotKind::Highlight
        | AnnotKind::Underline
        | AnnotKind::StrikeOut
        | AnnotKind::Squiggly => annot
            .quad_points()
            .map(|quads| quads.iter().map(quad_box).collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let colour = annot.color().ok().flatten().map(to_colour);
    let contents = annot
        .contents()
        .ok()
        .flatten()
        .filter(|text| !text.is_empty())
        .map(str::to_owned);
    let strokes = match kind {
        AnnotKind::Ink => annot
            .ink_list()
            .map(|strokes| {
                strokes
                    .iter()
                    .map(|stroke| stroke.iter().map(|p| (p.x, p.y)).collect())
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    Some(AnnotInfo {
        id,
        kind,
        rect,
        quads,
        colour,
        contents,
        strokes,
    })
}

/// A page's annotations in drawing order, skipping the kinds [`kind_of`] does not list and any
/// annotation MuPDF cannot describe.
pub(crate) fn annotations(doc: &PdfDocument, page: u32) -> Result<Vec<AnnotInfo>, MupdfError> {
    let pdf_page = load(doc, page)?;
    Ok(pdf_page
        .annotations()
        .filter_map(|annot| describe(&annot))
        .collect())
}

/// Whether a quad is an upright rectangle, as markup of horizontal text is. Anything else (rotated
/// text, a hand-made quad) is never cut: interval arithmetic on its box would mark the wrong area.
fn upright(q: &Quad) -> bool {
    const EPS: f32 = 0.01;
    (q.ul.y - q.ur.y).abs() < EPS
        && (q.ll.y - q.lr.y).abs() < EPS
        && (q.ul.x - q.ll.x).abs() < EPS
        && (q.ur.x - q.lr.x).abs() < EPS
}

fn subtype(kind: MarkupKind) -> PdfAnnotationType {
    match kind {
        MarkupKind::Highlight => PdfAnnotationType::Highlight,
        MarkupKind::Underline => PdfAnnotationType::Underline,
        MarkupKind::StrikeOut => PdfAnnotationType::StrikeOut,
        MarkupKind::Squiggly => PdfAnnotationType::Squiggly,
    }
}

/// Cuts the page's markup of the same kind where `rects` will cover it (Plan 0004 Phase 1 review,
/// option B), so new markup replaces old instead of stacking on it: each overlapped annotation
/// keeps the parts of its lines outside `rects`, or is deleted when nothing is left. Other kinds
/// stay - an underline under a highlight means something - and so does markup that carries a
/// comment, or whose quads are not upright rectangles.
fn cut_same_kind(
    pdf_page: &mut PdfPage,
    page: u32,
    kind: MarkupKind,
    rects: &[Rect],
) -> Result<(), MupdfError> {
    let fail = |source| MupdfError::EditFailed { page, source };
    let wanted = subtype(kind);
    // Decide first, then edit: the page's annotation list must not change under the iterator.
    let mut plan: Vec<(i32, Vec<Rect>)> = Vec::new();
    for annot in pdf_page.annotations() {
        if annot.r#type().ok() != Some(wanted) {
            continue;
        }
        if annot
            .contents()
            .ok()
            .flatten()
            .is_some_and(|c| !c.trim().is_empty())
        {
            continue;
        }
        let Ok(quads) = annot.quad_points() else {
            continue;
        };
        if quads.is_empty() || !quads.iter().all(upright) {
            continue;
        }
        let lines: Vec<Rect> = quads.iter().map(quad_box).collect();
        if let (Some(rest), Ok(xref)) = (cut(&lines, rects), annot.xref()) {
            plan.push((xref, rest));
        }
    }
    for (xref, rest) in plan {
        let Some(mut annot) = pdf_page
            .annotations()
            .find(|annot| annot.xref().ok() == Some(xref))
        else {
            continue;
        };
        if rest.is_empty() {
            pdf_page.delete_annotation(annot).map_err(fail)?;
        } else {
            let quads: Vec<Quad> = rest.iter().map(|r| Quad::from(to_mu(r))).collect();
            annot.set_quad_points(quads).map_err(fail)?;
            annot.update().map_err(fail)?;
        }
    }
    Ok(())
}

/// Where another reader opens a note's text: beside its icon, not at the page corner MuPDF puts a
/// new note's popup (`pdf_create_annot`).
fn popup_beside(note: &Rect) -> MuRect {
    let x = note.x + note.width + 4.0;
    MuRect::new(x, note.y, x + 200.0, note.y + 100.0)
}

fn find(pdf_page: &PdfPage, id: u32) -> Option<PdfAnnotation> {
    pdf_page
        .annotations()
        .find(|annot| annot.xref().ok() == Some(id as i32))
}

/// Applies one edit to the document; the caller bumps the page's revision.
pub(crate) fn apply(doc: &PdfDocument, op: &EditOp) -> Result<(), MupdfError> {
    let page = op.page();
    let mut pdf_page = load(doc, page)?;
    let fail = |source| MupdfError::EditFailed { page, source };
    match op {
        EditOp::AddMarkup {
            kind,
            rects,
            colour,
            author,
            ..
        } => {
            cut_same_kind(&mut pdf_page, page, *kind, rects)?;
            let quads: Vec<Quad> = rects.iter().map(|r| Quad::from(to_mu(r))).collect();
            let mut annot = match kind {
                MarkupKind::Highlight => pdf_page.add_highlight_annotation(quads),
                MarkupKind::Underline => pdf_page.add_underline_annotation(quads),
                MarkupKind::StrikeOut => pdf_page.add_strikeout_annotation(quads),
                MarkupKind::Squiggly => pdf_page.add_squiggly_annotation(quads),
            }
            .map_err(fail)?;
            annot
                .set_color(to_annotation_colour(*colour))
                .map_err(fail)?;
            if let Some(author) = author.as_deref().filter(|a| !a.is_empty()) {
                annot.set_author(author).map_err(fail)?;
            }
            annot.update().map_err(fail)?;
        }
        EditOp::Delete { id, .. } => {
            let annot =
                find(&pdf_page, *id).ok_or(MupdfError::AnnotationNotFound { page, id: *id })?;
            pdf_page.delete_annotation(annot).map_err(fail)?;
        }
        EditOp::AddNote {
            rect,
            contents,
            colour,
            author,
            ..
        } => {
            let mut annot = pdf_page
                .add_text_annotation(to_mu(rect), contents)
                .map_err(fail)?;
            annot
                .set_color(to_annotation_colour(*colour))
                .map_err(fail)?;
            if let Some(author) = author.as_deref().filter(|a| !a.is_empty()) {
                annot.set_author(author).map_err(fail)?;
            }
            annot.set_popup(popup_beside(rect)).map_err(fail)?;
            annot.update().map_err(fail)?;
        }
        EditOp::SetContents { id, contents, .. } => {
            let mut annot =
                find(&pdf_page, *id).ok_or(MupdfError::AnnotationNotFound { page, id: *id })?;
            annot.set_contents(contents).map_err(fail)?;
            annot.update().map_err(fail)?;
        }
        EditOp::MoveNote { id, rect, .. } => {
            let mut annot =
                find(&pdf_page, *id).ok_or(MupdfError::AnnotationNotFound { page, id: *id })?;
            annot.set_rect(to_mu(rect)).map_err(fail)?;
            annot.set_popup(popup_beside(rect)).map_err(fail)?;
            annot.update().map_err(fail)?;
        }
        EditOp::AddInk {
            strokes,
            colour,
            width,
            author,
            ..
        } => {
            let strokes = strokes
                .iter()
                .map(|stroke| stroke.iter().map(|&(x, y)| Point::new(x, y)));
            let mut annot = pdf_page.add_ink_annotation(strokes).map_err(fail)?;
            annot
                .set_color(to_annotation_colour(*colour))
                .map_err(fail)?;
            annot.set_border_width(*width).map_err(fail)?;
            if let Some(author) = author.as_deref().filter(|a| !a.is_empty()) {
                annot.set_author(author).map_err(fail)?;
            }
            annot.update().map_err(fail)?;
        }
        // Structural edits go through `apply_pages`, and the information fields through
        // `set_metadata`; the engine never sends them here.
        EditOp::Pages(_) | EditOp::SetMetadata(_) => {}
        EditOp::SetColour { id, colour, .. } => {
            let mut annot =
                find(&pdf_page, *id).ok_or(MupdfError::AnnotationNotFound { page, id: *id })?;
            annot
                .set_color(to_annotation_colour(*colour))
                .map_err(fail)?;
            annot.update().map_err(fail)?;
        }
    }
    Ok(())
}

/// Applies a structural edit to a document of `count` pages (Plan 0005, §4.1), inside the caller's
/// journal operation; [`pages::plan`] has checked it already.
pub(crate) fn apply_pages(
    doc: &mut PdfDocument,
    op: &PageOp,
    count: u32,
) -> Result<(), MupdfError> {
    let fail = |source| MupdfError::EditFailed {
        page: op.first_page(),
        source,
    };
    match op {
        PageOp::Rotate {
            pages,
            quarter_turns,
        } => {
            for page in pages::chosen(pages, count)? {
                let mut pdf_page = doc.load_pdf_page(page as i32).map_err(fail)?;
                let rotation = pdf_page.rotation().map_err(fail)?;
                pdf_page
                    .set_rotation((rotation + 90 * quarter_turns).rem_euclid(360))
                    .map_err(fail)?;
            }
        }
        PageOp::Delete { pages } => {
            let gone: Vec<usize> = pages::chosen(pages, count)?
                .into_iter()
                .map(|p| p as usize)
                .collect();
            doc.delete_pages(gone).map_err(fail)?;
        }
        PageOp::Duplicate { pages } => {
            // From the back, so the pages still to copy keep their indices. Links keep pointing
            // where they did: in the same document, a page's index is looked up afresh.
            for page in pages::chosen(pages, count)?.into_iter().rev() {
                doc.copy_page(page as usize, InsertPosition::After(page as usize))
                    .map_err(fail)?;
                let at = page as i32;
                copy_annotations(doc, None, &[(at, at + 1)], &|n| Some(n)).map_err(fail)?;
            }
        }
        PageOp::Move { pages, before } => {
            let order = pages::moved_order(pages, *before, count)?;
            // Puts each position's page in place from the front; MuPDF's move takes a page out and
            // inserts it at the target index of the result.
            let mut current: Vec<u32> = (0..count).collect();
            for (target, page) in order.iter().enumerate() {
                let Some(at) = current.iter().position(|p| p == page) else {
                    continue;
                };
                if at != target {
                    doc.move_page(at, target).map_err(fail)?;
                    let moved = current.remove(at);
                    current.insert(target, moved);
                }
            }
        }
        PageOp::InsertFrom { path, at, .. } => {
            let source = open_source(path)?;
            let options = InsertPdfOptions {
                target: InsertPosition::Before(*at as usize),
                ..InsertPdfOptions::default()
            };
            doc.insert_pdf(&source, options).map_err(fail)?;
            let n = source.page_count().map_err(fail)?;
            let at = *at as i32;
            let pairs: Vec<(i32, i32)> = (0..n).map(|p| (p, at + p)).collect();
            copy_annotations(doc, Some(&source), &pairs, &|p| (p < n).then_some(at + p))
                .map_err(fail)?;
        }
        PageOp::InsertBlank { at, width, height } => {
            let index = if *at >= count { -1 } else { *at as i32 };
            doc.new_page_at(index, Size::new(*width, *height))
                .map_err(fail)?;
        }
        PageOp::Crop { pages, rect } => {
            for page in pages::chosen(pages, count)? {
                crop(doc, page, *rect)?;
            }
        }
        PageOp::Stamp { pages, stamp } => add_stamp(doc, &pages::chosen(pages, count)?, stamp)?,
    }
    Ok(())
}

/// The page's visible space (points from its top left, after rotation and crop) and the matrix
/// that leads from it back to the PDF's own coordinates.
fn visible_space(page: &PdfPage) -> Result<(f32, f32, Matrix), mupdf::Error> {
    let bounds = page.bounds()?;
    let mut to_visible = page.ctm()?;
    // `bounds` starts at (0, 0) for a PDF page; anything else would be moved there first.
    to_visible.concat(Matrix::new_translate(-bounds.x0, -bounds.y0));
    let back = to_visible
        .invert()
        .ok_or_else(|| mupdf::Error::InvalidArgument("the page's matrix is singular".to_owned()))?;
    Ok((bounds.x1 - bounds.x0, bounds.y1 - bounds.y0, back))
}

/// Sets one page's crop box from a rectangle in its visible space (Plan 0005 Phase 3). mupdf-rs'
/// `set_crop_box` is not used: it takes the rectangle as if the page were neither rotated nor
/// offset (`mupdf-sys` `wrapper/pdf_page.c`, `mupdf_pdf_page_set_crop_box`).
fn crop(doc: &mut PdfDocument, page: u32, rect: [f32; 4]) -> Result<(), MupdfError> {
    let fail = |source| MupdfError::EditFailed { page, source };
    let pdf_page = doc.load_pdf_page(page as i32).map_err(fail)?;
    let (width, height, back) = visible_space(&pdf_page).map_err(fail)?;
    let [x0, y0, x1, y1] =
        stamp::clip_crop(rect, width, height).ok_or(PageOpError::CropOutside { page })?;
    let user = MuRect::new(x0, y0, x1, y1).transform(&back);
    let mut array = doc.new_array().map_err(fail)?;
    user.encode_into(&mut array).map_err(fail)?;
    pdf_page.object().dict_put("CropBox", array).map_err(fail)?;
    Ok(())
}

/// Writes a watermark or page numbers into the pages' content, over what is there (Plan 0005
/// Phase 3). The text is set in MuPDF's built-in Helvetica, embedded once as a CID font so any
/// letter it has - Turkish ones included - can be written and extracted again.
fn add_stamp(doc: &mut PdfDocument, pages: &[u32], stamp: &Stamp) -> Result<(), MupdfError> {
    let first = pages.first().copied().unwrap_or(0);
    let fail = |source| MupdfError::EditFailed {
        page: first,
        source,
    };
    let font = Font::new("Helvetica").map_err(fail)?;
    let font_obj = doc.add_font(&font).map_err(fail)?;
    let state = if stamp.opacity() < 1.0 {
        let mut dict = doc.new_dict().map_err(fail)?;
        dict.dict_put("Type", PdfObject::new_name("ExtGState").map_err(fail)?)
            .map_err(fail)?;
        for key in ["ca", "CA"] {
            dict.dict_put(key, PdfObject::new_real(stamp.opacity()).map_err(fail)?)
                .map_err(fail)?;
        }
        Some(doc.add_object(&dict).map_err(fail)?)
    } else {
        None
    };
    // One `q` stream, shared by every page, opens the wrap around the page's own content.
    let open = doc
        .add_stream(&Buffer::from_bytes(b"q\n").map_err(fail)?, None, false)
        .map_err(fail)?;
    let colour = stamp.colour();
    let count = pages.len() as u32;
    for (index, &page) in pages.iter().enumerate() {
        let fail = |source| MupdfError::EditFailed { page, source };
        let text = stamp.text(index as u32, count);
        let mut glyphs = String::with_capacity(text.len() * 4);
        let mut advance = 0.0;
        for c in text.chars() {
            let glyph = font.encode_character(c as i32).map_err(fail)?;
            if glyph <= 0 && !c.is_whitespace() {
                return Err(PageOpError::MissingGlyph(c).into());
            }
            advance += font.advance_glyph(glyph).map_err(fail)?;
            glyphs.push_str(&format!("{glyph:04X}"));
        }
        let pdf_page = doc.load_pdf_page(page as i32).map_err(fail)?;
        let (width, height, back) = visible_space(&pdf_page).map_err(fail)?;
        let placement = stamp.placement(width, height, advance);
        let font_name = resource(doc, &pdf_page, "Font", &font_obj).map_err(fail)?;
        let state_name = match &state {
            Some(state) => Some(resource(doc, &pdf_page, "ExtGState", state).map_err(fail)?),
            None => None,
        };
        let [a, b, c, d, e, f] = placement.matrix;
        let mut content = format!(
            "Q\nq {} {} {} {} {} {} cm ",
            back.a, back.b, back.c, back.d, back.e, back.f
        );
        if let Some(name) = state_name {
            content.push_str(&format!("/{name} gs "));
        }
        content.push_str(&format!(
            "{} {} {} rg BT /{font_name} 1 Tf {a} {b} {c} {d} {e} {f} Tm <{glyphs}> Tj ET Q\n",
            colour.r, colour.g, colour.b
        ));
        let stamp_stream = doc
            .add_stream(
                &Buffer::from_bytes(content.as_bytes()).map_err(fail)?,
                None,
                false,
            )
            .map_err(fail)?;
        wrap_contents(doc, &pdf_page, &open, stamp_stream).map_err(fail)?;
    }
    Ok(())
}

/// Puts `open` before the page's own content streams and `stamp` after them, so whatever state the
/// page's content leaves behind (a matrix, a clip) does not move or hide the stamp, which begins
/// with the matching `Q`.
///
/// mupdf-rs' `insert_contents` is not used: it opens a journal operation of its own, and MuPDF
/// merges a nested operation into the outer one by comparing every change of the outer one with
/// every other (`resolve_undo`, `pdf-object.c`), so stamping n pages that way costs n^3 (17.9 s
/// for 1000 pages, `docs/benchmarks.md` §11.6).
fn wrap_contents(
    doc: &mut PdfDocument,
    page: &PdfPage,
    open: &PdfObject,
    stamp: PdfObject,
) -> Result<(), mupdf::Error> {
    let mut page_obj = page.object();
    let mut array = doc.new_array()?;
    array.array_push(open.clone())?;
    if let Some(old) = page_obj.get_dict("Contents")?
        && !old.is_null()?
    {
        if old.is_array()? {
            for index in 0..old.len()? {
                if let Some(item) = old.get_array(index as i32)? {
                    array.array_push(item)?;
                }
            }
        } else {
            array.array_push(old)?;
        }
    }
    array.array_push(stamp)?;
    page_obj.dict_put("Contents", array)
}

/// Puts `obj` into the page's resources of `kind` and returns its name there. A name already
/// given to the same object (resources shared between pages) is used again.
fn resource(
    doc: &mut PdfDocument,
    page: &PdfPage,
    kind: &str,
    obj: &PdfObject,
) -> Result<String, mupdf::Error> {
    let mut resources = page.resources()?;
    let mut dict = match resources.get_dict(kind)? {
        Some(dict) if dict.is_dict()? => dict,
        _ => {
            let dict = doc.new_dict()?;
            resources.dict_put(kind, dict)?;
            resources.get_dict(kind)?.ok_or_else(|| {
                mupdf::Error::InvalidArgument(format!("no /{kind} in the resources"))
            })?
        }
    };
    let xref = obj.as_indirect()?;
    for n in 0..10_000 {
        let name = format!("FaStamp{n}");
        match dict.get_dict(name.as_str())? {
            None => {
                dict.dict_put(name.as_str(), obj.clone())?;
                return Ok(name);
            }
            Some(existing) if existing.as_indirect().ok() == Some(xref) => return Ok(name),
            Some(_) => {}
        }
    }
    Err(mupdf::Error::InvalidArgument(format!(
        "no free name in the page's /{kind}"
    )))
}

/// Another PDF to take pages from; a password-protected one is refused.
fn open_source(path: &Path) -> Result<PdfDocument, MupdfError> {
    let name = path
        .to_str()
        .ok_or_else(|| MupdfError::InvalidPath(path.to_path_buf()))?;
    let doc = PdfDocument::open(name).map_err(|source| MupdfError::OpenFailed {
        path: path.to_path_buf(),
        source,
    })?;
    if doc.needs_password().unwrap_or(false) {
        return Err(MupdfError::PasswordRequired);
    }
    Ok(doc)
}

/// How many pages a PDF to insert has (Plan 0005 Phase 2).
pub(crate) fn source_page_count(path: &Path) -> Result<u32, MupdfError> {
    let doc = open_source(path)?;
    let count = doc
        .page_count()
        .map_err(|e| MupdfError::Backend(e.to_string()))?;
    Ok(count.max(0) as u32)
}

/// Writes the information fields into the trailer's `/Info` dictionary, created when the document
/// has none, and stamps `/ModDate`. MuPDF's own `pdf_set_metadata` does the same and, like it, this
/// leaves an XMP metadata stream as it is (Plan 0005 §3.2).
pub(crate) fn set_metadata(doc: &mut PdfDocument, fields: &Metadata) -> Result<(), MupdfError> {
    let fail = |source| MupdfError::EditFailed { page: 0, source };
    let existing = doc
        .trailer()
        .map_err(fail)?
        .get_dict("Info")
        .map_err(fail)?
        .filter(|info| info.is_dict().unwrap_or(false));
    let mut info = match existing {
        Some(info) => info,
        None => {
            let dict = doc.new_dict().map_err(fail)?;
            let info = doc.add_object(&dict).map_err(fail)?;
            // The trailer is fetched after the new object: on a document with no change yet,
            // adding it starts the section of changes, which has a trailer of its own, and a key
            // put into the trailer fetched before would be lost.
            let mut trailer = doc.trailer().map_err(fail)?;
            trailer.dict_put("Info", info.clone()).map_err(fail)?;
            info
        }
    };
    for (key, value) in [
        ("Title", &fields.title),
        ("Author", &fields.author),
        ("Subject", &fields.subject),
        ("Keywords", &fields.keywords),
    ] {
        match value.as_deref() {
            None => {}
            Some("") => info.dict_delete(key).map_err(fail)?,
            Some(text) => {
                let text = doc.new_string(text).map_err(fail)?;
                info.dict_put(key, text).map_err(fail)?;
            }
        }
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let date = doc.new_string(&pdf_date(now)).map_err(fail)?;
    info.dict_put("ModDate", date).map_err(fail)?;
    Ok(())
}

fn field(doc: &Document, name: MetadataName) -> String {
    doc.metadata(name).unwrap_or_default()
}

/// What MuPDF reports of any document's information.
pub(crate) fn plain_info(doc: &Document) -> DocumentInfo {
    let text = |name| {
        let value = field(doc, name);
        Some(value)
    };
    DocumentInfo {
        fields: Metadata {
            title: text(MetadataName::Title),
            author: text(MetadataName::Author),
            subject: text(MetadataName::Subject),
            keywords: text(MetadataName::Keywords),
        },
        creator: field(doc, MetadataName::Creator),
        producer: field(doc, MetadataName::Producer),
        created: field(doc, MetadataName::CreationDate),
        modified: field(doc, MetadataName::ModDate),
        format: field(doc, MetadataName::Format),
        has_xmp: false,
    }
}

/// A PDF's information, and whether it carries an XMP stream the app leaves as it is.
pub(crate) fn document_info(doc: &PdfDocument) -> DocumentInfo {
    let has_xmp = doc
        .catalog()
        .ok()
        .and_then(|catalog| catalog.get_dict("Metadata").ok().flatten())
        .is_some();
    DocumentInfo {
        has_xmp,
        ..plain_info(doc)
    }
}

/// Gives grafted pages their annotations and links back (Plan 0005 Phase 2).
///
/// MuPDF's `pdf_graft_page`, behind `insert_pdf` and `copy_page`, copies a page's contents,
/// resources, boxes and rotation but not its `/Annots` (`pdf-graft.c`, `copy_list`), so a
/// duplicated, inserted, merged or extracted page lost its markup, notes, ink and links. This copies
/// each annotation of `from` (in `src`, or in `dst` itself when `src` is `None`) onto page `to` of
/// `dst`, as a new object:
///
/// - `/P` names the new page; `/Popup`, `/Parent` and `/IRT` are left out, since grafting them
///   would drag the source page and its other annotations along (a note opens without its popup
///   window object, which readers create);
/// - a link to a page of the source document is pointed at that page's copy, found through
///   `target` (source page index to `dst` page index); a link whose page was not copied is
///   dropped; links by name and to other files are kept as they are;
/// - form widgets are left out: their fields live in the document's form, which v0.4 handles.
pub(crate) fn copy_annotations(
    dst: &mut PdfDocument,
    src: Option<&PdfDocument>,
    pairs: &[(i32, i32)],
    target: &dyn Fn(i32) -> Option<i32>,
) -> Result<(), mupdf::Error> {
    let mut map = dst.new_graft_map()?;
    for &(from, to) in pairs {
        let src_page = match src {
            Some(doc) => doc.find_page(from)?,
            None => dst.find_page(from)?,
        };
        let Some(annots) = src_page.get_dict("Annots")? else {
            continue;
        };
        if !annots.is_array()? {
            continue;
        }
        let mut dst_page = dst.find_page(to)?;
        let mut list = dst.new_array()?;
        for item in annots.array_iter()? {
            let annot = item?;
            if !annot.is_dict()? {
                continue;
            }
            let subtype = annot
                .get_dict("Subtype")?
                .and_then(|s| s.as_name().ok())
                .unwrap_or_default();
            if subtype == b"Popup" || subtype == b"Widget" {
                continue;
            }
            let mut copy = dst.new_dict()?;
            let mut keep = true;
            for entry in annot.dict_iter()? {
                let (key, value) = entry?;
                let value = match key.as_name()?.as_slice() {
                    b"P" | b"Popup" | b"Parent" | b"IRT" => continue,
                    b"Dest" => match retarget(dst, src, &mut map, &value, target)? {
                        Some(dest) => dest,
                        None => {
                            keep = false;
                            break;
                        }
                    },
                    b"A" if is_goto(&value)? => {
                        let Some(d) = value.get_dict("D")? else {
                            keep = false;
                            break;
                        };
                        match retarget(dst, src, &mut map, &d, target)? {
                            Some(dest) => {
                                let mut action = dst.new_dict()?;
                                action.dict_put("S", PdfObject::new_name("GoTo")?)?;
                                action.dict_put("D", dest)?;
                                action
                            }
                            None => {
                                keep = false;
                                break;
                            }
                        }
                    }
                    _ => map.graft_object(&value)?,
                };
                copy.dict_put(key, value)?;
            }
            if !keep {
                continue;
            }
            copy.dict_put("P", dst_page.try_clone()?)?;
            list.array_push(dst.add_object(&copy)?)?;
        }
        if list.len()? > 0 {
            dst_page.dict_put("Annots", list)?;
        }
    }
    Ok(())
}

fn is_goto(action: &PdfObject) -> Result<bool, mupdf::Error> {
    Ok(action.is_dict()?
        && action
            .get_dict("S")?
            .and_then(|s| s.as_name().ok())
            .is_some_and(|s| s == b"GoTo"))
}

/// A link destination for the copy: an explicit one (`[page /XYZ ...]`) gets the page's copy, or
/// `None` when that page was not copied; a named one is kept as it is.
fn retarget(
    dst: &PdfDocument,
    src: Option<&PdfDocument>,
    map: &mut mupdf::pdf::PdfGraftMap,
    dest: &PdfObject,
    target: &dyn Fn(i32) -> Option<i32>,
) -> Result<Option<PdfObject>, mupdf::Error> {
    if !dest.is_array()? {
        return map.graft_object(dest).map(Some);
    }
    let Some(page) = dest.get_array(0)? else {
        return Ok(None);
    };
    if !page.is_indirect()? {
        // A page number: a link into another file (`GoToR`), kept as it is.
        return map.graft_object(dest).map(Some);
    }
    let number = match src {
        Some(doc) => doc.lookup_page_number(&page),
        None => dst.lookup_page_number(&page),
    };
    let Some(to) = number.ok().and_then(target) else {
        return Ok(None);
    };
    let mut copy = dst.new_array()?;
    copy.array_push(dst.find_page(to)?)?;
    for item in dest.array_iter()?.skip(1) {
        copy.array_push(map.graft_object(&item?)?)?;
    }
    Ok(Some(copy))
}
