// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Changes to a document's pages (Plan 0005 Phase 1, §4.1): what each structural edit does to the
//! page order, as pure logic the engine applies and the app follows.
//!
//! A structural edit renumbers pages, so everything the app keeps per page - tiles, base layers,
//! thumbnails, text lines, annotations - has to be told where each page went. That is a
//! [`PageMap`]: for every page after the edit, the page it was before, when it still looks the
//! same. The engine computes it from the edit with [`plan`], before touching the document, and
//! checks the result against the document's new page count.

/// For every page after a structural edit, the index it had before, or `None` when its pixels are
/// new: an inserted page, or a page whose rotation changed. A duplicate maps to its original.
pub type PageMap = Vec<Option<u32>>;

/// A structural edit (Plan 0005, §4.1). Page lists name pages by their index before the edit; the
/// order and repeats in them do not matter.
#[derive(Debug, Clone, PartialEq)]
pub enum PageOp {
    /// Turns pages by a multiple of 90 degrees, clockwise when positive.
    Rotate {
        pages: Vec<u32>,
        quarter_turns: i32,
    },
    Delete {
        pages: Vec<u32>,
    },
    /// Puts a copy of each page right after it.
    Duplicate {
        pages: Vec<u32>,
    },
    /// Takes the pages out, in document order, and puts them back before the page that was at
    /// `before`, or at the end when `before` is the page count. A `before` that is one of the moved
    /// pages means the first page after it that does not move.
    Move {
        pages: Vec<u32>,
        before: u32,
    },
    /// A blank page of this size, in points, inserted before page `at` (at the end for the page
    /// count).
    InsertBlank {
        at: u32,
        width: f32,
        height: f32,
    },
    /// Every page of another PDF, inserted before page `at`. `source_pages` is that file's page
    /// count, which the engine reads from the file (the app sends 0).
    InsertFrom {
        path: std::path::PathBuf,
        at: u32,
        source_pages: u32,
    },
}

/// Why a structural edit cannot be applied.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PageOpError {
    #[error("page {page} is out of range (the document has {count})")]
    OutOfRange { page: u32, count: u32 },
    #[error("no pages chosen")]
    NoPages,
    #[error("a document must keep at least one page")]
    WouldBeEmpty,
    #[error("the page size {width}x{height} pt is not usable")]
    BadSize { width: f32, height: f32 },
}

impl PageOp {
    /// The first page the edit names, or where it inserts.
    pub fn first_page(&self) -> u32 {
        match self {
            Self::Rotate { pages, .. }
            | Self::Delete { pages }
            | Self::Duplicate { pages }
            | Self::Move { pages, .. } => pages.iter().copied().min().unwrap_or(0),
            Self::InsertBlank { at, .. } | Self::InsertFrom { at, .. } => *at,
        }
    }

    /// The name MuPDF's undo journal records the edit under.
    pub fn title(&self) -> &'static str {
        match self {
            Self::Rotate { .. } => "Rotate pages",
            Self::Delete { .. } => "Delete pages",
            Self::Duplicate { .. } => "Duplicate pages",
            Self::Move { .. } => "Move pages",
            Self::InsertBlank { .. } => "Insert blank page",
            Self::InsertFrom { .. } => "Insert pages from file",
        }
    }
}

/// The pages of a list, checked, sorted and without repeats.
pub fn chosen(pages: &[u32], count: u32) -> Result<Vec<u32>, PageOpError> {
    if pages.is_empty() {
        return Err(PageOpError::NoPages);
    }
    let mut out = pages.to_vec();
    out.sort_unstable();
    out.dedup();
    if let Some(&page) = out.iter().find(|&&p| p >= count) {
        return Err(PageOpError::OutOfRange { page, count });
    }
    Ok(out)
}

/// The order of the old pages after a move: the pages that stay, with the moved ones (in document
/// order) put back before `before`.
pub fn moved_order(pages: &[u32], before: u32, count: u32) -> Result<Vec<u32>, PageOpError> {
    let moving = chosen(pages, count)?;
    if before > count {
        return Err(PageOpError::OutOfRange {
            page: before,
            count,
        });
    }
    let staying: Vec<u32> = (0..count)
        .filter(|p| moving.binary_search(p).is_err())
        .collect();
    // Where `before` lands among the pages that stay: before the first staying page at or after it.
    let at = staying.partition_point(|&p| p < before);
    let mut order = Vec::with_capacity(count as usize);
    order.extend_from_slice(&staying[..at]);
    order.extend_from_slice(&moving);
    order.extend_from_slice(&staying[at..]);
    Ok(order)
}

/// What a structural edit does to a document of `count` pages: the [`PageMap`] of the result.
pub fn plan(op: &PageOp, count: u32) -> Result<PageMap, PageOpError> {
    match op {
        PageOp::Rotate {
            pages,
            quarter_turns,
        } => {
            let turning = chosen(pages, count)?;
            let turns = quarter_turns.rem_euclid(4) != 0;
            Ok((0..count)
                .map(|p| (!turns || turning.binary_search(&p).is_err()).then_some(p))
                .collect())
        }
        PageOp::Delete { pages } => {
            let gone = chosen(pages, count)?;
            if gone.len() as u32 == count {
                return Err(PageOpError::WouldBeEmpty);
            }
            Ok((0..count)
                .filter(|p| gone.binary_search(p).is_err())
                .map(Some)
                .collect())
        }
        PageOp::Duplicate { pages } => {
            let copied = chosen(pages, count)?;
            let mut map = Vec::with_capacity(count as usize + copied.len());
            for p in 0..count {
                map.push(Some(p));
                if copied.binary_search(&p).is_ok() {
                    map.push(Some(p));
                }
            }
            Ok(map)
        }
        PageOp::Move { pages, before } => Ok(moved_order(pages, *before, count)?
            .into_iter()
            .map(Some)
            .collect()),
        PageOp::InsertBlank { at, width, height } => {
            if *at > count {
                return Err(PageOpError::OutOfRange { page: *at, count });
            }
            if !(width.is_finite() && height.is_finite() && *width >= 1.0 && *height >= 1.0) {
                return Err(PageOpError::BadSize {
                    width: *width,
                    height: *height,
                });
            }
            let mut map: PageMap = (0..count).map(Some).collect();
            map.insert(*at as usize, None);
            Ok(map)
        }
        PageOp::InsertFrom {
            at, source_pages, ..
        } => {
            if *at > count {
                return Err(PageOpError::OutOfRange { page: *at, count });
            }
            if *source_pages == 0 {
                return Err(PageOpError::NoPages);
            }
            let mut map: PageMap = (0..count).map(Some).collect();
            let at = *at as usize;
            map.splice(at..at, std::iter::repeat_n(None, *source_pages as usize));
            Ok(map)
        }
    }
}

/// The map that leads back: for each page before the edit, its index after it (the first, for a
/// page that was duplicated), or `None` when it is gone or its pixels changed. `old_count` is the
/// page count before the edit. Undo uses it.
pub fn invert(map: &[Option<u32>], old_count: u32) -> PageMap {
    let mut back = vec![None; old_count as usize];
    for (new, old) in map.iter().enumerate() {
        if let Some(old) = old
            && let Some(slot) = back.get_mut(*old as usize)
            && slot.is_none()
        {
            *slot = Some(new as u32);
        }
    }
    back
}

/// Where a page went: its index after the edit, or, when it is gone, the page now at its place
/// (the last page when it was beyond the end). Used to keep the reader on their page.
pub fn follow(map: &[Option<u32>], old: u32) -> u32 {
    if let Some(new) = map.iter().position(|o| *o == Some(old)) {
        return new as u32;
    }
    old.min(map.len().saturating_sub(1) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(map: &PageMap) -> Vec<i64> {
        map.iter().map(|o| o.map_or(-1, i64::from)).collect()
    }

    #[test]
    fn rotation_changes_the_turned_pages_only() {
        let op = PageOp::Rotate {
            pages: vec![1, 3, 1],
            quarter_turns: 1,
        };
        assert_eq!(ids(&plan(&op, 5).unwrap()), [0, -1, 2, -1, 4]);
        // A full turn changes nothing.
        let op = PageOp::Rotate {
            pages: vec![1],
            quarter_turns: -4,
        };
        assert_eq!(ids(&plan(&op, 3).unwrap()), [0, 1, 2]);
    }

    #[test]
    fn delete_keeps_the_rest_in_order_and_one_page_at_least() {
        let op = PageOp::Delete { pages: vec![0, 2] };
        assert_eq!(ids(&plan(&op, 4).unwrap()), [1, 3]);
        let all = PageOp::Delete {
            pages: vec![0, 1, 2],
        };
        assert_eq!(plan(&all, 3), Err(PageOpError::WouldBeEmpty));
        let none = PageOp::Delete { pages: vec![] };
        assert_eq!(plan(&none, 3), Err(PageOpError::NoPages));
        let far = PageOp::Delete { pages: vec![7] };
        assert_eq!(
            plan(&far, 3),
            Err(PageOpError::OutOfRange { page: 7, count: 3 })
        );
    }

    #[test]
    fn a_duplicate_follows_its_original() {
        let op = PageOp::Duplicate { pages: vec![2, 0] };
        assert_eq!(ids(&plan(&op, 3).unwrap()), [0, 0, 1, 2, 2]);
    }

    #[test]
    fn move_puts_the_pages_before_the_target() {
        let mv = |pages: Vec<u32>, before| ids(&plan(&PageOp::Move { pages, before }, 6).unwrap());
        // Forwards and backwards.
        assert_eq!(mv(vec![0], 3), [1, 2, 0, 3, 4, 5]);
        assert_eq!(mv(vec![4], 1), [0, 4, 1, 2, 3, 5]);
        // To the end, and several pages keeping their document order.
        assert_eq!(mv(vec![1], 6), [0, 2, 3, 4, 5, 1]);
        assert_eq!(mv(vec![4, 1], 0), [1, 4, 0, 2, 3, 5]);
        // A target among the moved pages: before the next page that stays.
        assert_eq!(mv(vec![2, 3], 3), [0, 1, 2, 3, 4, 5]);
        assert_eq!(
            plan(
                &PageOp::Move {
                    pages: vec![0],
                    before: 7
                },
                6
            ),
            Err(PageOpError::OutOfRange { page: 7, count: 6 })
        );
    }

    #[test]
    fn a_blank_page_is_new() {
        let op = PageOp::InsertBlank {
            at: 1,
            width: 612.0,
            height: 792.0,
        };
        assert_eq!(ids(&plan(&op, 2).unwrap()), [0, -1, 1]);
        let end = PageOp::InsertBlank {
            at: 2,
            width: 612.0,
            height: 792.0,
        };
        assert_eq!(ids(&plan(&end, 2).unwrap()), [0, 1, -1]);
        let bad = PageOp::InsertBlank {
            at: 0,
            width: 0.0,
            height: 792.0,
        };
        assert!(matches!(plan(&bad, 2), Err(PageOpError::BadSize { .. })));
    }

    #[test]
    fn pages_from_a_file_are_new() {
        let op = PageOp::InsertFrom {
            path: "other.pdf".into(),
            at: 1,
            source_pages: 2,
        };
        assert_eq!(ids(&plan(&op, 3).unwrap()), [0, -1, -1, 1, 2]);
        let empty = PageOp::InsertFrom {
            path: "other.pdf".into(),
            at: 0,
            source_pages: 0,
        };
        assert_eq!(plan(&empty, 3), Err(PageOpError::NoPages));
    }

    #[test]
    fn invert_leads_back_and_follow_keeps_the_reader_on_their_page() {
        let op = PageOp::Move {
            pages: vec![0],
            before: 3,
        };
        let map = plan(&op, 4).unwrap(); // [1, 2, 0, 3]
        assert_eq!(ids(&invert(&map, 4)), [2, 0, 1, 3]);
        assert_eq!(follow(&map, 0), 2);
        // A deleted page: the page now at its place.
        let map = plan(&PageOp::Delete { pages: vec![3] }, 4).unwrap();
        assert_eq!(follow(&map, 3), 2);
        assert_eq!(ids(&invert(&map, 4)), [0, 1, 2, -1]);
        // A duplicate leads back to the original's first copy.
        let map = plan(&PageOp::Duplicate { pages: vec![1] }, 3).unwrap();
        assert_eq!(ids(&invert(&map, 3)), [0, 1, 3]);
    }

    #[test]
    fn moved_order_is_a_permutation() {
        for before in 0..=6 {
            for mask in 1u32..63 {
                let pages: Vec<u32> = (0..6).filter(|p| mask & (1 << p) != 0).collect();
                let mut order = moved_order(&pages, before, 6).unwrap();
                order.sort_unstable();
                assert_eq!(order, [0, 1, 2, 3, 4, 5], "{pages:?} before {before}");
            }
        }
    }
}
