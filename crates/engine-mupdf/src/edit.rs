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
use mupdf::pdf::{PdfAnnotation, PdfAnnotationType, PdfDocument, PdfPage};
use mupdf::{Quad, Rect as MuRect};
use pdf_core::Rect;
use pdf_core::annot::{AnnotInfo, AnnotKind, Colour, EditOp, MarkupKind, cut};

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
    Some(AnnotInfo {
        id,
        kind,
        rect,
        quads,
        colour,
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
