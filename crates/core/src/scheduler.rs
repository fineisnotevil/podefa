// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Pure tile cache and request scheduler (Plan 0001, §4.5).
//!
//! No MuPDF and no Slint types: the whole policy — byte budget, eviction order, pending set,
//! model capacity clamp — is plain Rust and therefore unit-testable without a document or a UI.
//!
//! The scheduler owns two parallel views of the same tiles:
//!
//! - the **cache**, a `TileKey -> Entry` map that decides what is worth keeping and when to
//!   evict, and
//! - the **rows**, a fixed-length `Vec` of model slots the UI draws. A row holds an image
//!   whether or not it is currently visible, which is what lets a pan or zoom-back be served
//!   without re-rendering.

use crate::tiling::TileKey;
use std::collections::{HashMap, HashSet};

/// Default bitmap budget on desktop (64 MiB of tile pixels).
pub const TILE_CACHE_MAX_BYTES: usize = 64 * 1024 * 1024;
/// Default bitmap budget on mobile (16 MiB of tile pixels).
pub const TILE_CACHE_MAX_BYTES_MOBILE: usize = 16 * 1024 * 1024;

/// Bitmap budget for the current platform.
pub fn default_cache_bytes() -> usize {
    if cfg!(any(target_os = "android", target_os = "ios")) {
        TILE_CACHE_MAX_BYTES_MOBILE
    } else {
        TILE_CACHE_MAX_BYTES
    }
}

/// What the UI must do to the model after a scheduler call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileAction {
    /// The row already holds the right pixels: draw it.
    Show {
        /// Model row.
        slot: usize,
        /// Tile the row holds.
        key: TileKey,
    },
    /// Stop drawing the row but keep its pixels cached.
    Hide {
        /// Model row.
        slot: usize,
    },
    /// Evicted: stop drawing the row and drop its image to free the memory.
    Release {
        /// Model row.
        slot: usize,
    },
    /// Render this tile and send it back through [`TileScheduler::insert`].
    Request {
        /// Tile to render.
        key: TileKey,
    },
}

/// Result of [`TileScheduler::insert`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct InsertOutcome {
    /// Model row the tile was placed in, or `None` when it was dropped.
    pub slot: Option<usize>,
    /// Row changes caused by the eviction needed to make room.
    pub actions: Vec<TileAction>,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    key: TileKey,
    bytes: usize,
    slot: usize,
    tick: u64,
    visible: bool,
}

/// Tile cache, eviction policy and request planning. Pure data structure.
#[derive(Debug)]
pub struct TileScheduler {
    capacity: usize,
    budget: usize,
    scale_milli: u32,
    tick: u64,
    entries: HashMap<TileKey, Entry>,
    slots: Vec<Option<TileKey>>,
    pending: HashSet<TileKey>,
    desired: HashSet<TileKey>,
    /// Keys requested by [`Self::prefetch`] and not yet arrived: accepted on arrival, and kept
    /// apart from `desired` so that "wanted by the viewport" keeps its exact meaning.
    prefetch: HashSet<TileKey>,
    bytes: usize,
    dropped: u64,
    evicted: u64,
}

impl TileScheduler {
    /// Creates a scheduler with `model_capacity` model rows and a bitmap byte budget.
    ///
    /// Both are clamped to at least 1 so a degenerate window size can never make the scheduler
    /// panic or refuse every tile.
    pub fn new(model_capacity: usize, cache_max_bytes: usize) -> Self {
        let capacity = model_capacity.max(1);
        Self {
            capacity,
            budget: cache_max_bytes.max(1),
            scale_milli: 0,
            tick: 0,
            entries: HashMap::new(),
            slots: vec![None; capacity],
            pending: HashSet::new(),
            desired: HashSet::new(),
            prefetch: HashSet::new(),
            bytes: 0,
            dropped: 0,
            evicted: 0,
        }
    }

    /// Configured model capacity in rows.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Configured bitmap budget in bytes.
    pub fn cache_max_bytes(&self) -> usize {
        self.budget
    }

    /// Number of cached tiles (visible or not).
    pub fn cache_len(&self) -> usize {
        self.entries.len()
    }

    /// Bytes of cached tile pixels.
    pub fn cache_bytes(&self) -> usize {
        self.bytes
    }

    /// Number of tiles requested but not yet inserted.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// The requested-but-not-arrived keys.
    ///
    /// Exposed so the app can tell useful work from backlog: a key that is pending but no longer
    /// wanted is raster time spent on a viewport the user has already left, which is what makes a
    /// fast pan keep showing blank strips (Plan 0001, §7 - see `docs/benchmarks.md` §7.5).
    pub fn pending_keys(&self) -> impl Iterator<Item = &TileKey> {
        self.pending.iter()
    }

    /// Number of tiles that were never placed.
    ///
    /// Three reasons, and none of them is the byte budget: the tile arrived after its epoch was
    /// superseded, the viewport wanted more cells than the model has rows, or every cached tile
    /// was visible so there was no row to take. A slow pan keeps this near zero; a zoom burst does
    /// not, because each step supersedes the tiles the step before it requested. The byte budget
    /// has its own counter, [`Self::evicted`].
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Number of tiles released to stay inside the byte budget.
    ///
    /// Row recycling for a full model is not counted: that is the capacity, not the budget. This
    /// is the counter that says whether a run's budget ever bound - a cache whose `cache_bytes`
    /// sits at the budget while this climbs is under budget pressure, and one that never climbs is
    /// capped by its row count instead (see `docs/benchmarks.md` §7.6).
    pub fn evicted(&self) -> u64 {
        self.evicted
    }

    /// Whether the tile is cached, visible or not.
    pub fn contains(&self, key: &TileKey) -> bool {
        self.entries.contains_key(key)
    }

    /// Whether the tile has been requested and has not arrived yet.
    pub fn is_pending(&self, key: &TileKey) -> bool {
        self.pending.contains(key)
    }

    /// Whether the tile is cached *and* currently part of the viewport.
    pub fn is_visible(&self, key: &TileKey) -> bool {
        self.entries.get(key).is_some_and(|entry| entry.visible)
    }

    /// Model row holding a tile, if it is cached.
    pub fn slot_of(&self, key: &TileKey) -> Option<usize> {
        self.entries.get(key).map(|entry| entry.slot)
    }

    /// Tile held by a model row, if any.
    pub fn slot_key(&self, slot: usize) -> Option<TileKey> {
        self.slots.get(slot).copied().flatten()
    }

    /// Number of keys the last [`Self::update_view`] wanted.
    pub fn desired_len(&self) -> usize {
        self.desired.len()
    }

    /// Scale of the last [`Self::update_view`], in permille.
    pub fn scale_milli(&self) -> u32 {
        self.scale_milli
    }

    /// Diffs the live viewport against the cache.
    ///
    /// Tiles that are newly wanted are shown, tiles that are no longer wanted are hidden but
    /// kept, and missing tiles are requested once. The desired set is clamped to the model
    /// capacity: surplus keys are dropped rather than placed, so an oversized viewport degrades
    /// into blank strips instead of panicking.
    pub fn update_view(&mut self, desired: &[TileKey], scale_milli: u32) -> Vec<TileAction> {
        self.tick += 1;
        self.scale_milli = scale_milli;

        let wanted: HashSet<TileKey> = desired.iter().copied().collect();
        let tick = self.tick;
        let mut actions = Vec::new();
        let mut visible_cached = 0usize;
        for entry in self.entries.values_mut() {
            let is = wanted.contains(&entry.key);
            if is {
                entry.tick = tick;
                visible_cached += 1;
            }
            if is != entry.visible {
                entry.visible = is;
                actions.push(if is {
                    TileAction::Show {
                        slot: entry.slot,
                        key: entry.key,
                    }
                } else {
                    TileAction::Hide { slot: entry.slot }
                });
            }
        }
        self.desired = wanted;

        // Only rows that are *wanted* are off limits: cached tiles outside the viewport can be
        // evicted on arrival, so they must not stop new tiles from being requested.
        let mut free = self.capacity.saturating_sub(visible_cached);
        for key in desired {
            if self.entries.contains_key(key) || self.pending.contains(key) {
                continue;
            }
            if free == 0 {
                self.dropped += 1;
                continue;
            }
            self.pending.insert(*key);
            free -= 1;
            actions.push(TileAction::Request { key: *key });
        }
        actions
    }

    /// Requests tiles *ahead* of the viewport: the one-tile ring around it.
    ///
    /// The ring is speculation, so it never changes what is visible. Its tiles are ordinary
    /// evictable cache entries (eviction takes a non-visible tile first), they are not counted as
    /// holes, and a ring tile that never arrives costs nothing but the raster. Only rows the
    /// visible set does not need are taken, and nothing is requested once the cache already holds
    /// its whole budget: the ring can never push the cache past the budget on its own.
    ///
    /// Requested keys are remembered until they arrive, so a tile whose ring has moved on is
    /// cached rather than counted as a stale arrival - the pan that invalidated it is the pan it
    /// was requested for.
    pub fn prefetch(&mut self, ring: &[TileKey]) -> Vec<TileAction> {
        // A cache already holding its budget is not prefetched into: those tiles would be evicted
        // as fast as they arrived.
        //
        // ponytail: the guard is the budget alone, so a *small* budget whose rows cannot hold the
        // visible+ring working set still prefetches - and pays for it. Measured in
        // `docs/benchmarks.md` §7.6 run C (8 MiB budget): `evicted=3203` over a 296-step sweep
        // against `evicted=0` at the default, i.e. most of the ring is rendered and thrown away.
        // The upgrade path is to stop prefetching once the free budget is under one tile, from the
        // observed per-tile size, so a budget that cannot hold a ring does not pay to render it.
        if self.bytes >= self.budget {
            return Vec::new();
        }
        // The rows the viewport itself claims are off limits; every other cached tile - the ring
        // of the step before, a tile a pan left behind - is evictable, and that is what makes room
        // for the ring without the model growing a row for it.
        let mut free = self.capacity.saturating_sub(self.desired.len());
        let mut actions = Vec::new();
        for key in ring {
            if self.entries.contains_key(key) || self.pending.contains(key) {
                continue;
            }
            if free == 0 {
                break;
            }
            self.pending.insert(*key);
            self.prefetch.insert(*key);
            free -= 1;
            actions.push(TileAction::Request { key: *key });
        }
        actions
    }

    /// Records an arrived tile and returns the row it was placed in, or `None` when it was
    /// dropped: stale (no longer wanted), already cached, or with no evictable row left.
    /// [`InsertOutcome::actions`] carries the row changes caused by making room.
    ///
    /// A tile that is cached but not currently visible - one the ring asked for, or one whose
    /// viewport moved on inside the same epoch - lands hidden, and the next [`Self::update_view`]
    /// promotes it with a `Show` if the user reaches it.
    pub fn insert(&mut self, key: TileKey, bytes: usize) -> InsertOutcome {
        self.pending.remove(&key);
        // A prefetched tile carries its acceptance with it: the ring may have moved on between
        // the request and the arrival, which is exactly the pan the tile was requested for.
        let prefetched = self.prefetch.remove(&key);
        let mut out = InsertOutcome::default();

        if let Some(entry) = self.entries.get(&key) {
            out.slot = Some(entry.slot);
            return out;
        }
        // Visible is the viewport's answer, not the queue's: a tile that arrived for the ring is
        // cached, and the next `update_view` promotes it with a `Show` if the user got there.
        let visible = self.desired.contains(&key);
        if !visible && !prefetched {
            // Stale tile from a superseded epoch: never let it evict a live one.
            self.dropped += 1;
            return out;
        }

        match self.evict_to_fit(bytes, &mut out) {
            Some(slot) => {
                // Every insert gets its own tick, so LRU order is a total order and eviction is
                // deterministic even when several tiles were received in the same viewport diff.
                self.tick += 1;
                self.slots[slot] = Some(key);
                self.entries.insert(
                    key,
                    Entry {
                        key,
                        bytes,
                        slot,
                        tick: self.tick,
                        visible,
                    },
                );
                self.bytes += bytes;
                out.slot = Some(slot);
            }
            None => self.dropped += 1,
        }
        out
    }

    /// Drops every tile and returns the rows to clear (page change).
    pub fn clear_all(&mut self) -> Vec<TileAction> {
        let mut actions = Vec::new();
        for slot in 0..self.slots.len() {
            if self.slots[slot].is_some() {
                actions.push(TileAction::Release { slot });
            }
        }
        self.slots.iter_mut().for_each(|slot| *slot = None);
        self.entries.clear();
        self.pending.clear();
        self.desired.clear();
        self.prefetch.clear();
        self.bytes = 0;
        actions
    }

    /// Forgets the in-flight requests (epoch change: their results are discarded anyway).
    pub fn clear_pending(&mut self) {
        self.pending.clear();
        self.prefetch.clear();
    }

    /// Frees room for a `bytes`-sized tile, evicting as needed.
    ///
    /// Returns `None` when every cached tile is currently visible. The byte budget is a target: a
    /// single tile larger than the whole budget is still inserted, otherwise nothing would ever
    /// be rendered.
    fn evict_to_fit(&mut self, bytes: usize, out: &mut InsertOutcome) -> Option<usize> {
        let mut freed = self.slots.iter().position(Option::is_none);
        if freed.is_none() {
            let slot = self.evict_one()?;
            out.actions.push(TileAction::Release { slot });
            freed = Some(slot);
        }
        while self.bytes + bytes > self.budget {
            match self.evict_one() {
                Some(slot) => {
                    out.actions.push(TileAction::Release { slot });
                    self.evicted += 1;
                }
                None => break,
            }
        }
        freed
    }

    /// Evicts the least valuable non-visible tile and frees its row, or returns `None` when every
    /// cached tile is visible (a visible tile is never evicted).
    ///
    /// Old-scale tiles go first: after a zoom change they can no longer be shown, and they are
    /// exactly what a burst of new-scale buffers collides with.
    fn evict_one(&mut self) -> Option<usize> {
        let scale = self.scale_milli;
        let victim = self
            .entries
            .values()
            .filter(|entry| !entry.visible)
            // Wrong-scale first, then least recently used, then the key itself so the choice is
            // deterministic rather than dependent on hash-map iteration order.
            .min_by_key(|entry| (entry.key.scale_milli == scale, entry.tick, entry.key))
            .map(|entry| entry.key)?;
        let entry = self
            .entries
            .remove(&victim)
            .expect("victim was read from the same map");
        self.slots[entry.slot] = None;
        self.bytes = self.bytes.saturating_sub(entry.bytes);
        Some(entry.slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiling::{ZOOM_MAX_MILLI, zoom_in_milli};
    use std::time::{Duration, Instant};

    /// One 512x512 RGB8 tile: 768 KiB, the size [`crate::TILE_SIZE_PX`] actually produces.
    const TILE: usize = 512 * 512 * 3;

    fn key(col: i32, row: i32, scale: u32) -> TileKey {
        TileKey {
            page: 0,
            col,
            row,
            scale_milli: scale,
        }
    }

    /// Inserts every key as if it had just arrived, returning the row-change actions.
    fn insert_all(sched: &mut TileScheduler, keys: &[TileKey]) -> Vec<TileAction> {
        let mut actions = Vec::new();
        for k in keys {
            let out = sched.insert(*k, TILE);
            assert!(out.slot.is_some(), "expected a row for {k:?}");
            actions.extend(out.actions);
        }
        actions
    }

    fn requests(actions: &[TileAction]) -> Vec<TileKey> {
        actions
            .iter()
            .filter_map(|action| match action {
                TileAction::Request { key } => Some(*key),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn missing_tiles_are_requested_once() {
        let mut sched = TileScheduler::new(16, TILE_CACHE_MAX_BYTES);
        let desired = [key(0, 0, 1_000), key(1, 0, 1_000)];

        let actions = sched.update_view(&desired, 1_000);
        assert_eq!(requests(&actions), desired.to_vec());
        // The same viewport must not re-request anything.
        assert!(sched.update_view(&desired, 1_000).is_empty());
        assert_eq!(sched.pending_len(), 2);

        insert_all(&mut sched, &desired);
        assert_eq!(sched.pending_len(), 0);
        assert_eq!(sched.cache_len(), 2);
        assert_eq!(sched.cache_bytes(), 2 * TILE);
        assert!(sched.is_visible(&key(0, 0, 1_000)));
        assert_eq!(sched.slot_of(&key(1, 0, 1_000)), Some(1));
    }

    #[test]
    fn panning_keeps_tiles_cached_and_hides_them() {
        let mut sched = TileScheduler::new(8, TILE_CACHE_MAX_BYTES);
        let first = [key(0, 0, 1_000), key(1, 0, 1_000)];
        sched.update_view(&first, 1_000);
        insert_all(&mut sched, &first);

        let second = [key(1, 0, 1_000), key(2, 0, 1_000)];
        let actions = sched.update_view(&second, 1_000);
        assert_eq!(requests(&actions), vec![key(2, 0, 1_000)]);
        assert!(
            !sched.is_visible(&key(0, 0, 1_000)),
            "panned-away tile hides"
        );
        assert!(sched.contains(&key(0, 0, 1_000)), "but stays cached");
        assert!(sched.is_visible(&key(1, 0, 1_000)));

        insert_all(&mut sched, &[key(2, 0, 1_000)]);
        assert_eq!(sched.cache_len(), 3);
    }

    #[test]
    fn byte_budget_releases_the_oldest_hidden_tile() {
        // Two tiles fit, the third does not.
        let mut sched = TileScheduler::new(8, 2 * TILE);
        for k in [key(0, 0, 1_000), key(1, 0, 1_000)] {
            sched.update_view(&[k], 1_000);
            insert_all(&mut sched, &[k]);
        }
        assert_eq!(sched.cache_bytes(), 2 * TILE);

        assert_eq!(sched.evicted(), 0, "both tiles took a free row");
        sched.update_view(&[key(2, 0, 1_000)], 1_000);
        let out = sched.insert(key(2, 0, 1_000), TILE);
        assert!(out.slot.is_some(), "a row is found for the new tile");
        assert_eq!(
            out.actions,
            vec![TileAction::Release { slot: 0 }],
            "the oldest hidden tile is released"
        );
        assert_eq!(sched.slot_key(0), None, "the evicted row is freed");
        assert_eq!(out.slot, Some(2), "the new tile takes an unused row");
        assert!(!sched.contains(&key(0, 0, 1_000)));
        assert!(sched.contains(&key(1, 0, 1_000)));
        assert_eq!(sched.cache_bytes(), 2 * TILE);
        assert!(sched.cache_bytes() <= sched.cache_max_bytes());
        assert_eq!(
            sched.evicted(),
            1,
            "the budget, not the row count, freed the room"
        );
    }

    #[test]
    fn old_scale_tiles_are_evicted_before_current_scale_tiles() {
        let mut sched = TileScheduler::new(4, TILE_CACHE_MAX_BYTES);
        let old = [key(0, 0, 1_000), key(1, 0, 1_000)];
        sched.update_view(&old, 1_000);
        insert_all(&mut sched, &old);

        // Zoom in: the old-scale tiles hide, the new scale takes the remaining rows.
        let new = [key(0, 0, 1_250), key(1, 0, 1_250)];
        sched.update_view(&new, 1_250);
        insert_all(&mut sched, &new);
        assert_eq!(sched.cache_len(), 4);

        // The model is full: one more tile must take an old-scale row, not a current-scale one.
        let third = key(2, 0, 1_250);
        sched.update_view(&[new[0], new[1], third], 1_250);
        let out = sched.insert(third, TILE);
        assert!(out.slot.is_some());
        assert_eq!(out.actions.len(), 1);
        assert!(!sched.contains(&old[0]), "oldest old-scale tile goes first");
        assert!(sched.contains(&old[1]) && sched.contains(&new[0]) && sched.contains(&new[1]));
    }

    #[test]
    fn capacity_overflow_is_graceful() {
        let mut sched = TileScheduler::new(1, TILE_CACHE_MAX_BYTES);
        let desired = [key(0, 0, 1_000), key(1, 0, 1_000)];

        // Only one key can be requested; the surplus is counted, never panicked over.
        let actions = sched.update_view(&desired, 1_000);
        assert_eq!(requests(&actions), vec![key(0, 0, 1_000)]);
        assert_eq!(sched.dropped(), 1);

        assert_eq!(sched.insert(key(0, 0, 1_000), TILE).slot, Some(0));

        // The single row is visible, so a second arrival has nowhere to go: `None`, no panic.
        let out = sched.insert(key(1, 0, 1_000), TILE);
        assert_eq!(out.slot, None);
        assert_eq!(sched.dropped(), 2);
        assert_eq!(sched.cache_len(), 1);
        assert!(sched.cache_bytes() <= TILE);
    }

    #[test]
    fn pending_keys_track_requests_and_arrivals() {
        let mut sched = TileScheduler::new(4, TILE_CACHE_MAX_BYTES);
        let wanted = [key(0, 0, 1_000), key(1, 0, 1_000)];
        let actions = sched.update_view(&wanted, 1_000);
        assert_eq!(requests(&actions), wanted.to_vec());

        let mut pending: Vec<TileKey> = sched.pending_keys().copied().collect();
        pending.sort_unstable();
        assert_eq!(pending, wanted.to_vec());

        // An arrival leaves the pending set.
        insert_all(&mut sched, &[key(0, 0, 1_000)]);
        assert_eq!(sched.pending_keys().next(), Some(&key(1, 0, 1_000)));

        // The viewport moved: the outstanding request for the old viewport is still pending but no
        // longer wanted, which is exactly what the app's `unwanted` counter reports.
        let moved = [key(2, 0, 1_000)];
        sched.update_view(&moved, 1_000);
        assert_eq!(sched.pending_keys().count(), 2);
        assert_eq!(
            sched.pending_keys().filter(|k| !moved.contains(k)).count(),
            1
        );

        sched.clear_pending();
        assert_eq!(sched.pending_keys().count(), 0);
    }

    #[test]
    fn stale_tiles_are_dropped_without_evicting_live_ones() {
        let mut sched = TileScheduler::new(4, TILE_CACHE_MAX_BYTES);
        sched.update_view(&[key(0, 0, 1_000)], 1_000);
        insert_all(&mut sched, &[key(0, 0, 1_000)]);

        // A tile from a zoom level nobody wants any more arrives late.
        sched.update_view(&[key(0, 0, 1_250)], 1_250);
        let out = sched.insert(key(9, 9, 1_000), TILE);
        assert_eq!(out.slot, None);
        assert_eq!(sched.cache_len(), 1);
        assert!(sched.contains(&key(0, 0, 1_000)));
    }

    #[test]
    fn clear_all_releases_every_row() {
        let mut sched = TileScheduler::new(4, TILE_CACHE_MAX_BYTES);
        let keys = [key(0, 0, 1_000), key(1, 0, 1_000)];
        sched.update_view(&keys, 1_000);
        insert_all(&mut sched, &keys);

        let actions = sched.clear_all();
        assert_eq!(
            actions,
            vec![
                TileAction::Release { slot: 0 },
                TileAction::Release { slot: 1 }
            ]
        );
        assert_eq!(sched.cache_len(), 0);
        assert_eq!(sched.cache_bytes(), 0);
        assert_eq!(sched.pending_len(), 0);
    }

    /// 20 zoom clicks inside one second: every invariant the app relies on, at every step.
    #[test]
    fn rapid_zoom_stays_within_budget_and_capacity() {
        let capacity = 16;
        let mut sched = TileScheduler::new(capacity, TILE_CACHE_MAX_BYTES);
        let started = Instant::now();
        let mut scale = 1_000;

        for step in 0..20 {
            scale = zoom_in_milli(scale);

            // A full viewport of tiles at the new scale.
            let desired: Vec<TileKey> = (0..capacity as i32)
                .map(|i| key(i % 4, i / 4, scale))
                .collect();
            let want = requests(&sched.update_view(&desired, scale));
            assert!(want.len() <= capacity, "step {step}");

            for k in &want {
                let out = sched.insert(*k, TILE);
                assert!(out.slot.is_some(), "step {step}: {k:?} must fit");
            }

            // A late tile from the previous scale must not displace anything.
            let stale = key(0, 0, scale.saturating_sub(1));
            assert_eq!(sched.insert(stale, TILE).slot, None, "step {step}");

            assert!(sched.cache_len() <= capacity, "step {step}");
            assert!(sched.pending_len() <= capacity, "step {step}");
            assert_eq!(sched.cache_bytes(), sched.cache_len() * TILE);
        }

        assert_eq!(scale, ZOOM_MAX_MILLI);
        assert!(sched.cache_bytes() <= sched.cache_max_bytes());
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "20 zoom steps must complete within a second"
        );
    }

    /// The ring is requested ahead of the viewport but never marks anything visible, and a tile
    /// whose ring has already moved on is cached rather than counted as a stale arrival.
    #[test]
    fn prefetch_requests_ahead_without_marking_tiles_visible() {
        let mut sched = TileScheduler::new(6, TILE_CACHE_MAX_BYTES);
        let visible = [key(0, 0, 1_000)];
        sched.update_view(&visible, 1_000);
        insert_all(&mut sched, &visible);

        let ring = [key(1, 0, 1_000), key(0, 1, 1_000), key(1, 1, 1_000)];
        assert_eq!(requests(&sched.prefetch(&ring)), ring.to_vec());
        assert_eq!(sched.pending_len(), ring.len());
        assert!(sched.prefetch(&ring).is_empty(), "already in flight");

        insert_all(&mut sched, &ring);
        assert_eq!(sched.cache_len(), 4);
        assert_eq!(sched.dropped(), 0);
        for k in &ring {
            assert!(sched.contains(k), "{k:?} is cached");
            assert!(!sched.is_visible(k), "{k:?} is prefetched, not visible");
        }

        // The ring moved on while the tile was in flight: the arrival is still wanted, and it
        // lands hidden because nothing shows it yet.
        let moved = [key(2, 0, 1_000)];
        assert_eq!(requests(&sched.prefetch(&moved)), moved.to_vec());
        assert!(sched.insert(key(2, 0, 1_000), TILE).slot.is_some());
        assert_eq!(sched.dropped(), 0, "a prefetched arrival is not stale");
        assert!(!sched.is_visible(&moved[0]));
    }

    /// The visible set has first claim on the rows, and the budget caps the ring.
    #[test]
    fn prefetch_takes_only_the_rows_the_viewport_does_not_need() {
        let ring: Vec<TileKey> = (0..6).map(|i| key(i, 0, 1_000)).collect();

        let mut sched = TileScheduler::new(4, TILE_CACHE_MAX_BYTES);
        sched.update_view(&[ring[0]], 1_000);
        insert_all(&mut sched, &[ring[0]]);
        assert_eq!(requests(&sched.prefetch(&ring)).len(), 3, "four rows, one");
        assert_eq!(sched.cache_len() + sched.pending_len(), 4);

        // A full visible viewport lends the ring nothing.
        let full: Vec<TileKey> = (0..4).map(|i| key(i, 1, 1_000)).collect();
        let mut sched = TileScheduler::new(4, TILE_CACHE_MAX_BYTES);
        sched.update_view(&full, 1_000);
        insert_all(&mut sched, &full);
        assert!(sched.prefetch(&ring).is_empty());

        // A cache holding its whole budget is not prefetched into.
        let mut sched = TileScheduler::new(64, 2 * TILE);
        assert_eq!(requests(&sched.prefetch(&ring)).len(), ring.len());
        insert_all(&mut sched, &ring[..2]);
        assert_eq!(sched.cache_bytes(), 2 * TILE);
        assert!(sched.prefetch(&ring).is_empty());
    }

    /// Ring tiles already in the cache do not stop the ring from advancing: the new ones take
    /// rows the old ones hold, because every non-visible tile is evictable.
    #[test]
    fn the_ring_advances_on_a_full_cache() {
        let mut sched = TileScheduler::new(4, TILE_CACHE_MAX_BYTES);
        sched.update_view(&[key(0, 0, 1_000)], 1_000);
        insert_all(&mut sched, &[key(0, 0, 1_000)]);

        let first: Vec<TileKey> = (1..4).map(|i| key(i, 0, 1_000)).collect();
        assert_eq!(requests(&sched.prefetch(&first)).len(), 3);
        insert_all(&mut sched, &first);
        assert_eq!(
            sched.cache_len(),
            4,
            "the cache is full of viewport and ring"
        );

        // One step later: the same three rows are asked for the *next* column.
        let next = [key(4, 0, 1_000), key(5, 0, 1_000), key(6, 0, 1_000)];
        assert_eq!(requests(&sched.prefetch(&next)), next.to_vec());
        insert_all(&mut sched, &next);
        assert_eq!(sched.dropped(), 0, "evictions, not drops");
        assert_eq!(sched.cache_len(), 4);
        assert!(
            sched.is_visible(&key(0, 0, 1_000)),
            "the viewport is untouched"
        );
        for k in &next {
            assert!(sched.contains(k), "the new ring replaced the old one");
        }
    }

    /// A prefetched tile is an ordinary evictable entry: a visible arrival takes its row instead
    /// of the ring pinning the cache, and nothing is dropped to make the room.
    #[test]
    fn prefetched_tiles_do_not_displace_visible_ones() {
        let mut sched = TileScheduler::new(4, TILE_CACHE_MAX_BYTES);
        let visible = [key(0, 0, 1_000)];
        sched.update_view(&visible, 1_000);
        insert_all(&mut sched, &visible);
        let ring = [key(1, 0, 1_000), key(2, 0, 1_000), key(3, 0, 1_000)];
        sched.prefetch(&ring);
        insert_all(&mut sched, &ring);
        assert_eq!(sched.cache_len(), 4);

        let moved = [key(4, 0, 1_000)];
        sched.update_view(&moved, 1_000);
        let out = sched.insert(key(4, 0, 1_000), TILE);
        assert!(out.slot.is_some(), "a hidden row was recycled");
        assert_eq!(out.actions.len(), 1, "one release, of a hidden entry");
        assert_eq!(sched.dropped(), 0, "nothing was lost: it was evictable");
        assert!(sched.is_visible(&moved[0]));
        assert_eq!(sched.cache_len(), 4);
        // Least recently used goes first, so the column that scrolled away gives up its row while
        // the ring - which points at where the user is heading - stays.
        assert!(!sched.contains(&visible[0]));
        for k in &ring {
            assert!(sched.contains(k), "the ring stayed cached");
        }
    }
}
