// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Annotations as the app sees them, and the edits it asks the engine for (Plan 0004, §4.1).
//!
//! Pure data and logic: the engine lists a page's annotations as [`AnnotInfo`]s and applies
//! [`EditOp`]s; hit-testing, the colour palette and the unsaved-changes state live here so they are
//! testable without a document. Coordinates are MuPDF's page space (points from the page's
//! top-left), the same space as the text selection's rectangles.

use crate::Rect;

/// What kind of annotation an [`AnnotInfo`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnnotKind {
    Highlight,
    Underline,
    StrikeOut,
    Squiggly,
    /// A sticky note (PDF `Text` annotation).
    Note,
    Ink,
    /// Anything else the document has (stamps, shapes, free text, ...): drawn, selectable and
    /// deletable, but not created by the app.
    Other,
}

/// The text markup the app creates from a selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarkupKind {
    Highlight,
    Underline,
    StrikeOut,
    Squiggly,
}

impl MarkupKind {
    /// The annotation kind a markup of this kind is listed as.
    pub fn kind(self) -> AnnotKind {
        match self {
            Self::Highlight => AnnotKind::Highlight,
            Self::Underline => AnnotKind::Underline,
            Self::StrikeOut => AnnotKind::StrikeOut,
            Self::Squiggly => AnnotKind::Squiggly,
        }
    }
}

/// An RGB colour, each component in `0.0..=1.0`, as PDF annotations store it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Colour {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

impl Colour {
    pub const fn new(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b }
    }
}

/// The six colours the tool strip offers (Plan 0004, §4.2); the first is the default.
pub const PALETTE: [Colour; 6] = [
    Colour::new(1.0, 0.85, 0.0),  // yellow
    Colour::new(0.35, 0.85, 0.3), // green
    Colour::new(0.3, 0.7, 1.0),   // blue
    Colour::new(1.0, 0.45, 0.75), // pink
    Colour::new(1.0, 0.55, 0.1),  // orange
    Colour::new(0.9, 0.1, 0.1),   // red
];

/// One annotation of a page, as the engine lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotInfo {
    /// The annotation's object number in the PDF: stable across edits of other annotations, and
    /// what an [`EditOp`] names it by.
    pub id: u32,
    pub kind: AnnotKind,
    /// Its display bounds, page space.
    pub rect: Rect,
    /// For text markup, one rectangle per marked line: a click selects the markup only on them,
    /// not anywhere in its bounds.
    pub quads: Vec<Rect>,
    /// Its colour, when it has one the app can show and change.
    pub colour: Option<Colour>,
}

/// An edit the app asks the engine for. Each is applied to one page, which gets a new revision.
#[derive(Debug, Clone, PartialEq)]
pub enum EditOp {
    /// Marks text: one rectangle per line, as [`crate::select::PageSelection::rects`] gives them.
    AddMarkup {
        page: u32,
        kind: MarkupKind,
        rects: Vec<Rect>,
        colour: Colour,
        /// Written as the annotation's author (`/T`) only when set (Plan 0004 §0, question 5).
        author: Option<String>,
    },
    Delete {
        page: u32,
        id: u32,
    },
    SetColour {
        page: u32,
        id: u32,
        colour: Colour,
    },
}

impl EditOp {
    /// The page the edit changes.
    pub fn page(&self) -> u32 {
        match self {
            Self::AddMarkup { page, .. }
            | Self::Delete { page, .. }
            | Self::SetColour { page, .. } => *page,
        }
    }
}

/// How far outside an annotation's area a click still selects it, in points.
pub const ANNOT_HIT_PAD_PT: f32 = 2.0;

fn contains(rect: &Rect, x: f32, y: f32, pad: f32) -> bool {
    x >= rect.x - pad
        && x <= rect.x + rect.width + pad
        && y >= rect.y - pad
        && y <= rect.y + rect.height + pad
}

/// The annotation under a page point: text markup by its line rectangles, anything else by its
/// bounds. Annotations later in the list are drawn on top, so the last match wins.
pub fn hit(items: &[AnnotInfo], x: f32, y: f32) -> Option<&AnnotInfo> {
    items.iter().rev().find(|item| {
        if item.quads.is_empty() {
            contains(&item.rect, x, y, ANNOT_HIT_PAD_PT)
        } else {
            item.quads
                .iter()
                .any(|quad| contains(quad, x, y, ANNOT_HIT_PAD_PT))
        }
    })
}

/// Whether the document has changes the file does not (Plan 0004, §3.3: MuPDF's own flag stays
/// set after an incremental save, so the app counts its edits instead).
///
/// `position` counts the edits applied since the document was opened and `saved` is the position
/// the file was last written at. Phase 2's undo moves `position` back and forth, which is why the
/// saved state is a position and not a flag: undoing back to it makes the document clean again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaveState {
    position: u64,
    saved: Option<u64>,
}

impl Default for SaveState {
    fn default() -> Self {
        Self::new()
    }
}

impl SaveState {
    /// A freshly opened document: clean.
    pub const fn new() -> Self {
        Self {
            position: 0,
            saved: Some(0),
        }
    }

    /// An edit was applied.
    pub fn edited(&mut self) {
        self.position += 1;
    }

    /// The document was written to its file.
    pub fn saved(&mut self) {
        self.saved = Some(self.position);
    }

    /// Whether quitting now would lose changes.
    pub fn is_dirty(&self) -> bool {
        self.saved != Some(self.position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: u32, rect: Rect, quads: Vec<Rect>) -> AnnotInfo {
        AnnotInfo {
            id,
            kind: if quads.is_empty() {
                AnnotKind::Note
            } else {
                AnnotKind::Highlight
            },
            rect,
            quads,
            colour: None,
        }
    }

    #[test]
    fn markup_is_hit_on_its_lines_only() {
        // Two marked lines with a gap between them: the gap is inside the bounds but not marked.
        let lines = vec![
            Rect::new(50.0, 100.0, 200.0, 12.0),
            Rect::new(50.0, 130.0, 100.0, 12.0),
        ];
        let items = [info(7, Rect::new(50.0, 100.0, 200.0, 42.0), lines)];
        assert_eq!(hit(&items, 60.0, 105.0).map(|a| a.id), Some(7));
        assert_eq!(hit(&items, 60.0, 135.0).map(|a| a.id), Some(7));
        assert!(hit(&items, 60.0, 120.0).is_none(), "between the lines");
        assert!(
            hit(&items, 190.0, 135.0).is_none(),
            "beyond the second line's end"
        );
        // The pad reaches just outside a line.
        assert!(hit(&items, 60.0, 99.0).is_some());
        assert!(hit(&items, 60.0, 96.0).is_none());
    }

    #[test]
    fn the_topmost_annotation_wins() {
        let items = [
            info(1, Rect::new(0.0, 0.0, 100.0, 100.0), Vec::new()),
            info(2, Rect::new(40.0, 40.0, 20.0, 20.0), Vec::new()),
        ];
        assert_eq!(hit(&items, 50.0, 50.0).map(|a| a.id), Some(2));
        assert_eq!(hit(&items, 10.0, 10.0).map(|a| a.id), Some(1));
        assert!(hit(&items, 300.0, 10.0).is_none());
    }

    #[test]
    fn save_state_follows_edits_and_saves() {
        let mut state = SaveState::new();
        assert!(!state.is_dirty());
        state.edited();
        assert!(state.is_dirty());
        state.saved();
        assert!(!state.is_dirty());
        state.edited();
        state.edited();
        assert!(state.is_dirty());
        state.saved();
        assert!(!state.is_dirty());
        // The default (no document) is clean too, so nothing asks to be saved.
        assert!(!SaveState::default().is_dirty());
    }

    #[test]
    fn every_edit_names_its_page() {
        let colour = PALETTE[0];
        let ops = [
            EditOp::AddMarkup {
                page: 3,
                kind: MarkupKind::Underline,
                rects: Vec::new(),
                colour,
                author: None,
            },
            EditOp::Delete { page: 4, id: 1 },
            EditOp::SetColour {
                page: 5,
                id: 1,
                colour,
            },
        ];
        let pages: Vec<u32> = ops.iter().map(EditOp::page).collect();
        assert_eq!(pages, [3, 4, 5]);
    }
}
