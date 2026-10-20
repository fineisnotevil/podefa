// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Text selection over a page's characters (Plan 0003, §3.7 and §4.5).
//!
//! Selection works on the characters of MuPDF's structured text in their reading order, as the
//! engine lists them: a selection is the run of characters between two *boundaries* (the gaps
//! before or after a character), its highlight is one rectangle per line it touches, and its text is
//! those same characters with a line break between lines. Highlight and copied text therefore come
//! from one list and cannot disagree. Pure: the engine turns a text page into [`SelChar`]s, this
//! module does the rest, so all of it is testable without a document.

use crate::Rect;

/// One character of a page, in reading order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelChar {
    /// The character.
    pub ch: char,
    /// Its box in MuPDF's page space (points from the page's top-left).
    pub rect: Rect,
    /// The line it belongs to; consecutive characters of one line share it.
    pub line: u32,
}

/// What a selection snaps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectMode {
    /// Character boundaries: a drag.
    #[default]
    Chars,
    /// Whole words: a double-click (and its drag).
    Words,
    /// Whole lines: a triple-click.
    Lines,
}

/// A point on a page, in page space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PagePoint {
    pub page: u32,
    pub x: f32,
    pub y: f32,
}

/// One page's part of a selection.
#[derive(Debug, Clone, PartialEq)]
pub struct PageSelection {
    pub page: u32,
    /// One rectangle per line (per run of selected characters on a line), page space.
    pub rects: Vec<Rect>,
    /// The selected characters' text, lines separated by `\n`.
    pub text: String,
}

/// Index of the character a point is closest to: on the line whose box is nearest vertically
/// (inside it counts as zero), the character nearest horizontally. `None` for a page without text.
fn nearest(chars: &[SelChar], x: f32, y: f32) -> Option<usize> {
    let gap = |lo: f32, hi: f32, v: f32| {
        if v < lo {
            lo - v
        } else if v > hi {
            v - hi
        } else {
            0.0
        }
    };
    chars
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            let ka = (
                gap(a.rect.y, a.rect.y + a.rect.height, y),
                gap(a.rect.x, a.rect.x + a.rect.width, x),
            );
            let kb = (
                gap(b.rect.y, b.rect.y + b.rect.height, y),
                gap(b.rect.x, b.rect.x + b.rect.width, x),
            );
            ka.0.total_cmp(&kb.0).then(ka.1.total_cmp(&kb.1))
        })
        .map(|(i, _)| i)
}

/// The boundary a point falls on: before the nearest character, or after it when the point is
/// right of its centre. A boundary `i` sits between characters `i - 1` and `i`.
pub fn boundary(chars: &[SelChar], x: f32, y: f32) -> usize {
    match nearest(chars, x, y) {
        Some(i) => {
            let c = chars[i].rect;
            if x > c.x + c.width / 2.0 { i + 1 } else { i }
        }
        None => 0,
    }
}

/// The word around the character nearest a point: the run of non-whitespace characters on its
/// line, as a boundary range.
fn word_at(chars: &[SelChar], x: f32, y: f32) -> (usize, usize) {
    let Some(i) = nearest(chars, x, y) else {
        return (0, 0);
    };
    if chars[i].ch.is_whitespace() {
        return (i, i + 1);
    }
    let line = chars[i].line;
    let same = |j: usize| chars[j].line == line && !chars[j].ch.is_whitespace();
    let mut start = i;
    while start > 0 && same(start - 1) {
        start -= 1;
    }
    let mut end = i + 1;
    while end < chars.len() && same(end) {
        end += 1;
    }
    (start, end)
}

/// The line of the character nearest a point, as a boundary range.
fn line_at(chars: &[SelChar], x: f32, y: f32) -> (usize, usize) {
    let Some(i) = nearest(chars, x, y) else {
        return (0, 0);
    };
    let line = chars[i].line;
    let start = chars.iter().position(|c| c.line == line).unwrap_or(i);
    let end = chars
        .iter()
        .rposition(|c| c.line == line)
        .map_or(i + 1, |j| j + 1);
    (start, end)
}

/// The boundary range a point covers in a mode: a single boundary for characters, the word or the
/// line for the others. Used for both ends of a selection.
fn span(chars: &[SelChar], p: (f32, f32), mode: SelectMode) -> (usize, usize) {
    match mode {
        SelectMode::Chars => {
            let b = boundary(chars, p.0, p.1);
            (b, b)
        }
        SelectMode::Words => word_at(chars, p.0, p.1),
        SelectMode::Lines => line_at(chars, p.0, p.1),
    }
}

/// Highlight rectangles and text of the characters `start..end`.
pub fn render(page: u32, chars: &[SelChar], start: usize, end: usize) -> PageSelection {
    let end = end.min(chars.len());
    let mut rects: Vec<Rect> = Vec::new();
    let mut text = String::new();
    let mut last_line = None;
    for c in chars.get(start..end).unwrap_or(&[]) {
        if last_line != Some(c.line) {
            if last_line.is_some() {
                text.push('\n');
            }
            rects.push(c.rect);
            last_line = Some(c.line);
        } else if let Some(r) = rects.last_mut() {
            let x0 = r.x.min(c.rect.x);
            let y0 = r.y.min(c.rect.y);
            let x1 = (r.x + r.width).max(c.rect.x + c.rect.width);
            let y1 = (r.y + r.height).max(c.rect.y + c.rect.height);
            *r = Rect::new(x0, y0, x1 - x0, y1 - y0);
        }
        text.push(c.ch);
    }
    PageSelection { page, rects, text }
}

/// A selection from `a` to `b` (either order) in a mode, given each page's characters.
///
/// `chars_of(page)` returns the characters of a page; it is called only for the pages from the first
/// to the last end. The first page is selected from its end's start onwards, the pages in between
/// entirely, the last page up to its end's end. The text joins pages with a line break.
pub fn select(
    a: PagePoint,
    b: PagePoint,
    mode: SelectMode,
    mut chars_of: impl FnMut(u32) -> Vec<SelChar>,
) -> (Vec<PageSelection>, String) {
    // Order the ends by page, then by position on the page.
    let (first, last) = if a.page < b.page {
        (a, b)
    } else if b.page < a.page {
        (b, a)
    } else {
        let chars = chars_of(a.page);
        let sa = span(&chars, (a.x, a.y), mode);
        let sb = span(&chars, (b.x, b.y), mode);
        let start = sa.0.min(sb.0);
        let end = sa.1.max(sb.1);
        let page = render(a.page, &chars, start, end);
        let text = page.text.clone();
        return (
            if page.rects.is_empty() {
                Vec::new()
            } else {
                vec![page]
            },
            text,
        );
    };
    let mut pages = Vec::new();
    for p in first.page..=last.page {
        let chars = chars_of(p);
        let (start, end) = if p == first.page {
            (span(&chars, (first.x, first.y), mode).0, chars.len())
        } else if p == last.page {
            (0, span(&chars, (last.x, last.y), mode).1)
        } else {
            (0, chars.len())
        };
        let page = render(p, &chars, start, end);
        if !page.rects.is_empty() {
            pages.push(page);
        }
    }
    let text = pages
        .iter()
        .map(|p| p.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (pages, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page of `lines`, each character 10 x 12 pt, lines 20 pt apart starting at y = 100.
    fn page(lines: &[&str]) -> Vec<SelChar> {
        let mut chars = Vec::new();
        for (l, text) in lines.iter().enumerate() {
            for (i, ch) in text.chars().enumerate() {
                chars.push(SelChar {
                    ch,
                    rect: Rect::new(50.0 + 10.0 * i as f32, 100.0 + 20.0 * l as f32, 10.0, 12.0),
                    line: l as u32,
                });
            }
        }
        chars
    }

    fn at(page: u32, x: f32, y: f32) -> PagePoint {
        PagePoint { page, x, y }
    }

    #[test]
    fn a_drag_selects_between_character_boundaries() {
        let chars = page(&["hello world", "second line"]);
        // From the left half of 'w' (x 110..120) to the right half of 'r' (x 130..140).
        let (pages, text) = select(
            at(0, 112.0, 105.0),
            at(0, 137.0, 105.0),
            SelectMode::Chars,
            |_| chars.clone(),
        );
        assert_eq!(text, "wor");
        assert_eq!(pages[0].rects, vec![Rect::new(110.0, 100.0, 30.0, 12.0)]);
        // Backwards gives the same.
        let (_, back) = select(
            at(0, 137.0, 105.0),
            at(0, 112.0, 105.0),
            SelectMode::Chars,
            |_| chars.clone(),
        );
        assert_eq!(back, "wor");
    }

    #[test]
    fn a_selection_across_lines_breaks_the_text_and_the_rectangles_per_line() {
        let chars = page(&["hello world", "second line"]);
        let (pages, text) = select(
            at(0, 112.0, 105.0),
            at(0, 108.0, 125.0),
            SelectMode::Chars,
            |_| chars.clone(),
        );
        assert_eq!(text, "world\nsecond");
        assert_eq!(pages[0].rects.len(), 2);
    }

    #[test]
    fn a_point_beside_a_line_snaps_to_it() {
        let chars = page(&["abc"]);
        // Left of the line and right of it, at the line's height.
        assert_eq!(boundary(&chars, 0.0, 105.0), 0);
        assert_eq!(boundary(&chars, 500.0, 105.0), 3);
        // Far above the text: still the nearest line.
        assert_eq!(boundary(&chars, 67.0, 0.0), 2);
        assert_eq!(boundary(&[], 1.0, 1.0), 0);
    }

    #[test]
    fn double_and_triple_click_snap_to_words_and_lines() {
        let chars = page(&["hello world", "second line"]);
        let p = at(0, 125.0, 105.0); // inside "world"
        let (_, word) = select(p, p, SelectMode::Words, |_| chars.clone());
        assert_eq!(word, "world");
        let (_, line) = select(p, p, SelectMode::Lines, |_| chars.clone());
        assert_eq!(line, "hello world");
        // A word drag extends by whole words.
        let (_, words) = select(p, at(0, 55.0, 125.0), SelectMode::Words, |_| chars.clone());
        assert_eq!(words, "world\nsecond");
    }

    #[test]
    fn a_selection_across_three_pages_takes_the_middle_page_whole() {
        // Plan 0003 §6 row N, in miniature: the engine test does it on a real document.
        let pages = [
            page(&["one two", "three"]),
            page(&["middle"]),
            page(&["last page"]),
        ];
        let (sel, text) = select(
            at(0, 95.0, 105.0),
            at(2, 88.0, 105.0),
            SelectMode::Chars,
            |p| pages[p as usize].clone(),
        );
        assert_eq!(text, "two\nthree\nmiddle\nlast");
        assert_eq!(
            sel.iter().map(|p| p.page).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        // The ends given in reverse select the same.
        let (_, back) = select(
            at(2, 88.0, 105.0),
            at(0, 95.0, 105.0),
            SelectMode::Chars,
            |p| pages[p as usize].clone(),
        );
        assert_eq!(back, text);
    }

    #[test]
    fn an_empty_page_contributes_nothing() {
        let pages = [page(&["a"]), Vec::new(), page(&["b"])];
        let (sel, text) = select(
            at(0, 0.0, 105.0),
            at(2, 500.0, 105.0),
            SelectMode::Chars,
            |p| pages[p as usize].clone(),
        );
        assert_eq!(text, "a\nb");
        assert_eq!(sel.len(), 2);
    }
}
