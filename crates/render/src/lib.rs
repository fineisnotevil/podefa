// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 [YOUR_NAME] <[YOUR_EMAIL]>

//! Tile scheduler and rendering cache for PDF pages.
//!
//! # Intent
//!
//! This crate will manage:
//! - Asynchronous rendering of visible document tiles to decouple UI interaction
//!   from rasterization workloads.
//! - Background prefetching for anticipated viewport scroll and zoom movements.
//! - A memory-bounded LRU (least recently used) cache holding rendered tile
//!   bitmaps to minimize memory footprint while ensuring 60+ FPS navigation.
