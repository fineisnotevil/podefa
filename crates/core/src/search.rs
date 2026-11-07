// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Search results as the find bar sees them (Plan 0003, §3.6 and §4.4).
//!
//! Hits stream in page by page from a search thread, starting at the reader's page and wrapping, so
//! they arrive out of document order and possibly from a search that has since been replaced. This
//! module keeps them in document order, refuses anything from another generation (row M: no hit of
//! an old search is ever shown), and moves a "current hit" that stays on the same hit while others
//! are inserted around it.

use crate::Rect;
use std::cmp::Ordering;

/// One match: its page and the rectangles that cover it (one per line it spans), in MuPDF's page
/// space - points from the page's top-left corner, rotation applied.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub page: u32,
    pub rects: Vec<Rect>,
}

impl SearchHit {
    /// Document order: page, then top, then left of the first rectangle.
    fn order(&self, other: &Self) -> Ordering {
        let first = |hit: &Self| hit.rects.first().map_or((0.0, 0.0), |r| (r.y, r.x));
        let (ay, ax) = first(self);
        let (by, bx) = first(other);
        self.page
            .cmp(&other.page)
            .then(ay.total_cmp(&by))
            .then(ax.total_cmp(&bx))
    }
}

/// The hits of the current search, in document order.
#[derive(Debug, Clone, Default)]
pub struct SearchResults {
    generation: u64,
    hits: Vec<SearchHit>,
    current: Option<usize>,
    done: bool,
    stale: u64,
}

impl SearchResults {
    /// Starts a new search: everything of the previous one is forgotten, and from now on only
    /// `generation` is accepted.
    pub fn begin(&mut self, generation: u64) {
        self.generation = generation;
        self.hits.clear();
        self.current = None;
        self.done = false;
    }

    /// The generation being accepted.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Adds one page's hits. Returns `false`, and counts it as stale, when they belong to another
    /// generation. The current hit stays the same hit; if there was none, the first hit to arrive
    /// becomes current (the search starts at the reader's page, so that is the nearest one).
    pub fn add(&mut self, generation: u64, page: u32, hits: Vec<Vec<Rect>>) -> bool {
        if generation != self.generation {
            self.stale += 1;
            return false;
        }
        let current = self.current.map(|i| self.hits[i].clone());
        let mut first_new = None;
        for rects in hits {
            let hit = SearchHit { page, rects };
            let at = self
                .hits
                .partition_point(|h| h.order(&hit) != Ordering::Greater);
            self.hits.insert(at, hit);
            if first_new.is_none() {
                first_new = Some(at);
            }
        }
        self.current = match current {
            Some(hit) => self.hits.iter().position(|h| *h == hit),
            None => first_new.map(|_| {
                // The earliest-arriving page's first hit, wherever it sorted.
                self.hits.iter().position(|h| h.page == page).unwrap_or(0)
            }),
        };
        true
    }

    /// The search of `generation` finished. Stale completions are ignored.
    pub fn finish(&mut self, generation: u64) {
        if generation == self.generation {
            self.done = true;
        }
    }

    /// Whether the search has covered the whole document.
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Hits refused because they belonged to another generation (`stale_hits`, benchmarks §9.3).
    pub fn stale(&self) -> u64 {
        self.stale
    }

    /// All hits, in document order.
    pub fn hits(&self) -> &[SearchHit] {
        &self.hits
    }

    /// Index of the current hit in [`Self::hits`].
    pub fn current(&self) -> Option<usize> {
        self.current
    }

    /// The current hit.
    pub fn current_hit(&self) -> Option<&SearchHit> {
        self.current.and_then(|i| self.hits.get(i))
    }

    /// Moves to the next hit (wrapping) and returns it.
    pub fn next_hit(&mut self) -> Option<&SearchHit> {
        self.step(true)
    }

    /// Moves to the previous hit (wrapping) and returns it.
    pub fn previous_hit(&mut self) -> Option<&SearchHit> {
        self.step(false)
    }

    fn step(&mut self, forward: bool) -> Option<&SearchHit> {
        let n = self.hits.len();
        if n == 0 {
            return None;
        }
        self.current = Some(match self.current {
            None => 0,
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
        });
        self.current_hit()
    }

    /// The counter the find bar shows: "3 of 41", "3 of 41+" while the search still runs,
    /// "No results" once it finished with none, "" before anything is known.
    pub fn counter(&self) -> String {
        match (self.hits.len(), self.current, self.done) {
            (0, _, true) => "No results".to_owned(),
            (0, _, false) => String::new(),
            (n, current, done) => format!(
                "{} of {n}{}",
                current.map_or(0, |i| i + 1),
                if done { "" } else { "+" }
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(y: f32) -> Vec<Rect> {
        vec![Rect::new(10.0, y, 30.0, 10.0)]
    }

    #[test]
    fn hits_arriving_out_of_order_are_kept_in_document_order() {
        let mut results = SearchResults::default();
        results.begin(1);
        // The search starts at page 5 and wraps: 5, 6, then 0.
        assert!(results.add(1, 5, vec![rect(100.0), rect(50.0)]));
        assert!(results.add(1, 6, vec![rect(10.0)]));
        assert!(results.add(1, 0, vec![rect(700.0)]));
        let order: Vec<(u32, f32)> = results
            .hits()
            .iter()
            .map(|h| (h.page, h.rects[0].y))
            .collect();
        assert_eq!(order, vec![(0, 700.0), (5, 50.0), (5, 100.0), (6, 10.0)]);
    }

    #[test]
    fn the_first_page_to_arrive_holds_the_current_hit_and_it_stays_put() {
        let mut results = SearchResults::default();
        results.begin(1);
        results.add(1, 5, vec![rect(50.0)]);
        assert_eq!(results.current_hit().map(|h| h.page), Some(5));
        // Hits sorting before it shift its index, not which hit is current.
        results.add(1, 0, vec![rect(1.0), rect(2.0)]);
        assert_eq!(results.current(), Some(2));
        assert_eq!(results.current_hit().map(|h| h.page), Some(5));
        assert_eq!(results.counter(), "3 of 3+");
    }

    #[test]
    fn next_and_previous_wrap() {
        let mut results = SearchResults::default();
        results.begin(1);
        results.add(1, 0, vec![rect(1.0), rect(2.0), rect(3.0)]);
        assert_eq!(results.current(), Some(0));
        results.next_hit();
        results.next_hit();
        assert_eq!(results.current(), Some(2));
        results.next_hit();
        assert_eq!(results.current(), Some(0), "wraps forwards");
        results.previous_hit();
        assert_eq!(results.current(), Some(2), "wraps backwards");
        let mut empty = SearchResults::default();
        assert!(empty.next_hit().is_none());
    }

    #[test]
    fn hits_of_another_generation_are_refused_and_counted() {
        // Plan 0003 §6 row M: no hit of an old search ever reaches the results.
        let mut results = SearchResults::default();
        results.begin(1);
        results.add(1, 0, vec![rect(1.0)]);
        results.begin(2);
        assert!(!results.add(1, 3, vec![rect(1.0)]), "old generation");
        assert!(results.hits().is_empty());
        assert_eq!(results.stale(), 1);
        results.finish(1);
        assert!(
            !results.is_done(),
            "an old completion does not finish the new search"
        );
        assert!(results.add(2, 4, vec![rect(1.0)]));
        results.finish(2);
        assert_eq!(results.counter(), "1 of 1");
    }

    #[test]
    fn the_counter_reads_like_the_find_bar() {
        let mut results = SearchResults::default();
        results.begin(7);
        assert_eq!(results.counter(), "");
        results.finish(7);
        assert_eq!(results.counter(), "No results");
    }
}
