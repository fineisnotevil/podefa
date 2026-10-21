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
    /// Its text (`/Contents`): what a note says, or a comment left on any other annotation.
    pub contents: Option<String>,
}

/// The size of a sticky note's icon, in points (Plan 0004 Phase 3): MuPDF gives every `Text`
/// annotation a 16 pt icon and its rectangle that size (`pdf-annot.c`, `pdf_create_annot`).
pub const NOTE_SIZE_PT: f32 = 16.0;

/// The rectangle of a note whose icon has its top-left corner at a page point, kept inside a page
/// of the given size (page space, origin at the page's top-left).
pub fn note_rect(x: f32, y: f32, page_width: f32, page_height: f32) -> Rect {
    let max_x = (page_width - NOTE_SIZE_PT).max(0.0);
    let max_y = (page_height - NOTE_SIZE_PT).max(0.0);
    Rect::new(
        x.clamp(0.0, max_x),
        y.clamp(0.0, max_y),
        NOTE_SIZE_PT,
        NOTE_SIZE_PT,
    )
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
    /// Places a sticky note (PDF `Text` annotation) with its text.
    AddNote {
        page: u32,
        rect: Rect,
        contents: String,
        colour: Colour,
        author: Option<String>,
    },
    /// Replaces an annotation's text.
    SetContents {
        page: u32,
        id: u32,
        contents: String,
    },
    /// Moves a note to a new rectangle on its page.
    MoveNote {
        page: u32,
        id: u32,
        rect: Rect,
    },
}

impl EditOp {
    /// The name MuPDF's undo journal records the edit under.
    pub fn title(&self) -> &'static str {
        match self {
            Self::AddMarkup { kind, .. } => match kind {
                MarkupKind::Highlight => "Highlight",
                MarkupKind::Underline => "Underline",
                MarkupKind::StrikeOut => "Strike-out",
                MarkupKind::Squiggly => "Squiggly",
            },
            Self::Delete { .. } => "Delete annotation",
            Self::SetColour { .. } => "Change colour",
            Self::AddNote { .. } => "Add note",
            Self::SetContents { .. } => "Edit note",
            Self::MoveNote { .. } => "Move note",
        }
    }

    /// The page the edit changes.
    pub fn page(&self) -> u32 {
        match self {
            Self::AddMarkup { page, .. }
            | Self::Delete { page, .. }
            | Self::SetColour { page, .. }
            | Self::AddNote { page, .. }
            | Self::SetContents { page, .. }
            | Self::MoveNote { page, .. } => *page,
        }
    }
}

/// Pieces of a cut line narrower than this, in points, are dropped rather than kept as slivers.
pub const CUT_MIN_WIDTH_PT: f32 = 0.5;

/// Whether two line boxes are on the same text line: they overlap vertically by at least half of
/// the shorter one's height.
fn same_line(a: &Rect, b: &Rect) -> bool {
    let top = a.y.max(b.y);
    let bottom = (a.y + a.height).min(b.y + b.height);
    bottom - top >= 0.5 * a.height.min(b.height)
}

/// Cuts a markup's line boxes where new markup of the same kind covers them (Plan 0004 Phase 1
/// review, option B): each old box loses the horizontal span of every new box on its line, which
/// leaves it whole, shortened, split in two, or gone. Returns `None` when nothing overlaps - the old
/// markup stays as it is - and otherwise the boxes that remain, possibly none.
pub fn cut(old: &[Rect], new: &[Rect]) -> Option<Vec<Rect>> {
    let mut changed = false;
    let mut out = Vec::with_capacity(old.len());
    for line in old {
        // The spans of this line that are still unmarked, left to right.
        let mut spans = vec![(line.x, line.x + line.width)];
        for cover in new.iter().filter(|cover| same_line(line, cover)) {
            let (c0, c1) = (cover.x, cover.x + cover.width);
            let mut next = Vec::with_capacity(spans.len() + 1);
            for (s0, s1) in spans {
                if c1 <= s0 || c0 >= s1 {
                    next.push((s0, s1));
                    continue;
                }
                changed = true;
                if c0 > s0 {
                    next.push((s0, c0));
                }
                if c1 < s1 {
                    next.push((c1, s1));
                }
            }
            spans = next;
        }
        out.extend(
            spans
                .into_iter()
                .filter(|(s0, s1)| s1 - s0 >= CUT_MIN_WIDTH_PT)
                .map(|(s0, s1)| Rect::new(s0, line.y, s1 - s0, line.height)),
        );
    }
    changed.then_some(out)
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
/// `position` counts the edits applied since the document was opened, less the ones undone, and
/// `saved` is the position the file was last written at. Undo and redo move `position` back and
/// forth, which is why the saved state is a position and not a flag: undoing back to it makes the
/// document clean again. A new edit after an undo replaces the history beyond it, so a saved
/// position in that part can never be reached again (`saved` becomes `None`).
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
        if self.saved.is_some_and(|saved| saved > self.position) {
            self.saved = None;
        }
        self.position += 1;
    }

    /// The last edit was undone.
    pub fn undone(&mut self) {
        self.position = self.position.saturating_sub(1);
    }

    /// The last undone edit was redone.
    pub fn redone(&mut self) {
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
            contents: None,
        }
    }

    #[test]
    fn a_note_stays_on_its_page() {
        let n = NOTE_SIZE_PT;
        assert_eq!(
            note_rect(100.0, 50.0, 612.0, 792.0),
            Rect::new(100.0, 50.0, n, n)
        );
        // Past the right and bottom edges: pulled back so the whole icon is on the page.
        assert_eq!(
            note_rect(605.0, 790.0, 612.0, 792.0),
            Rect::new(612.0 - n, 792.0 - n, n, n)
        );
        // Above and left of the page.
        assert_eq!(
            note_rect(-30.0, -1.0, 612.0, 792.0),
            Rect::new(0.0, 0.0, n, n)
        );
        // A page smaller than the icon.
        assert_eq!(note_rect(5.0, 5.0, 10.0, 10.0), Rect::new(0.0, 0.0, n, n));
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
    fn a_cut_shortens_splits_or_removes_the_old_lines() {
        let line = |x: f32, w: f32, y: f32| Rect::new(x, y, w, 12.0);
        // Two marked lines; new markup over the middle of the first only.
        let old = [line(50.0, 200.0, 100.0), line(50.0, 200.0, 112.0)];
        let rest = cut(&old, &[line(100.0, 50.0, 100.0)]).expect("overlaps");
        assert_eq!(
            rest,
            vec![
                line(50.0, 50.0, 100.0),
                line(150.0, 100.0, 100.0),
                line(50.0, 200.0, 112.0)
            ]
        );
        // Over the start: shortened from the left.
        let rest = cut(&old[..1], &[line(0.0, 80.0, 100.0)]).expect("overlaps");
        assert_eq!(rest, vec![line(80.0, 170.0, 100.0)]);
        // Over everything: nothing remains.
        let rest = cut(&old, &[line(0.0, 300.0, 100.0), line(0.0, 300.0, 112.0)]);
        assert_eq!(rest, Some(Vec::new()));
        // A sliver under the minimum width is dropped, not kept.
        let rest = cut(&old[..1], &[line(50.2, 199.8, 100.0)]).expect("overlaps");
        assert!(rest.is_empty());
    }

    #[test]
    fn a_cut_leaves_other_lines_and_untouched_markup_alone() {
        let old = [Rect::new(50.0, 100.0, 200.0, 12.0)];
        // The next line down, touching the old one's bottom edge, is a different line.
        assert_eq!(cut(&old, &[Rect::new(50.0, 112.0, 200.0, 12.0)]), None);
        // Same line, but beside it.
        assert_eq!(cut(&old, &[Rect::new(260.0, 100.0, 40.0, 12.0)]), None);
        assert_eq!(cut(&old, &[]), None);
        // A line box of slightly different height on the same line still counts as the same line.
        assert!(cut(&old, &[Rect::new(60.0, 101.0, 20.0, 11.0)]).is_some());
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
    fn undo_and_redo_move_through_the_saved_position() {
        let mut state = SaveState::new();
        state.edited();
        state.edited();
        state.saved();
        state.undone();
        assert!(state.is_dirty(), "one step before the save");
        state.redone();
        assert!(!state.is_dirty(), "back at the save");
        state.undone();
        state.undone();
        assert!(
            state.is_dirty(),
            "at the opened document, which the file no longer is"
        );
        // Undoing past the opened document stays there.
        state.undone();
        state.redone();
        state.redone();
        assert!(!state.is_dirty());
    }

    #[test]
    fn an_edit_after_undoing_past_the_save_loses_it() {
        let mut state = SaveState::new();
        state.edited();
        state.edited();
        state.saved();
        state.undone();
        state.undone();
        // A new branch: the saved position (2) is gone for good, even when the count reaches 2.
        state.edited();
        state.edited();
        assert!(state.is_dirty());
        state.saved();
        assert!(!state.is_dirty());
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
            EditOp::AddNote {
                page: 6,
                rect: note_rect(0.0, 0.0, 100.0, 100.0),
                contents: "x".into(),
                colour,
                author: None,
            },
            EditOp::SetContents {
                page: 7,
                id: 1,
                contents: String::new(),
            },
            EditOp::MoveNote {
                page: 8,
                id: 1,
                rect: Rect::default(),
            },
        ];
        let pages: Vec<u32> = ops.iter().map(EditOp::page).collect();
        assert_eq!(pages, [3, 4, 5, 6, 7, 8]);
    }
}
