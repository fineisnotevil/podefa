<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Tiled, viewport-only rendering: pages rasterize as 512 px tiles through a bounded cache, so zoom and scroll cost scales with the visible area rather than the whole page.
- `crates/core`: pure tile geometry (`tiling`) and the tile request/cache scheduler (`scheduler`), including integer permille zoom scales and a configurable tile cache ceiling.
- `crates/engine-mupdf`: tiled rasterization on the engine actor, reusing a single scratch pixmap and caching the page display list.
- `crates/engine-mupdf`: a render worker pool (`clamp(cores - 1, 1, 4)` threads, `FA_PDF_WORKERS` overrides) sharing one cached display list per page as `Arc<DisplayList>`, plus a low-resolution base layer per page for the frames while a new scale's tiles are still in flight; a superseded zoom epoch drops its queued tiles and aborts the tile already inside the rasterizer through MuPDF's cancel cookie, reporting the measured abort latency in `EngineEvent::TileAborted`.
- `crates/app`: draws the base layer as the whole-page backdrop until the new scale's tiles land, and logs `abort_ms` (min/median/max) for superseded tiles in the `--bench-*` report.
- `crates/app`: `--bench-repeat N` repeats a `--bench-*` scenario and reports each run's worst beside the min/median/max across runs, so a gate number one run can only sample once (a rapid burst leaves `t_blank` with the last step alone) has a distribution; each repeat restores the viewport and drops the tile cache, so every run measures the same cold scenario instead of one served by the previous run's cache.
- `crates/engine-mupdf`: pool tests for the cancellation invariants, plus `dense_vector.pdf` (one path node of 1000 page-spanning cubics) to measure them on: an epoch cancelled before its work is dispatched publishes nothing at all, every tile delivered while an epoch is being cancelled is still byte-identical to the serial render, and the abort latency is reported for a page of small nodes and for a page that is a single heavy node.
- `crates/core`: `TileScheduler` accessors for pending requests and the active scale, so the app can tell useful raster work from backlog.
- Implementation plans under `docs/plans/` (`0001-tiled-viewport-rendering.md`, `0002-continuous-multipage-scroll.md`).
- Multi-crate Cargo workspace structure:
  - `crates/core`: Engine-agnostic domain models (`PdfEngine`, `DocumentInfo`, `Rect`, `Bitmap`, `Command`).
  - `crates/engine-mupdf`: MuPDF integration stub implementing `PdfEngine`.
  - `crates/app`: Declarative Slint UI desktop application scaffold.
- Release compilation profile with binary size optimizations (`opt-level = "z"`, fat LTO, abort panic, symbol stripping).
- Complete REUSE 3.3 specification compliance with SPDX license identifiers and `LICENSES/` catalog.
- Auditing configuration via `deny.toml` enforcing AGPL-3.0 compatibility and dependency bans.
- Architectural Decision Records (`docs/adr/`):
  - `0001-stack-rust-slint-mupdf.md`
  - `0002-license-agpl.md`
  - `0003-engine-actor-threading.md`
- Community and project health standards (`README.md`, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md`, `justfile`).
- GitHub Actions multi-platform CI matrix (Linux, macOS, Windows) with linting, tests, license checks, REUSE compliance, and DCO sign-off validation.
- `crates/app`: the `--bench-*` trace samples the process's resident (`WorkingSetSize`) and committed private (`PrivateUsage`) bytes every 250 ms through the Windows API directly (`GetProcessMemoryInfo` in a hand-written `extern` block, no new dependency), logs them beside the tile cache's bytes, rows, `dropped` and `evicted` on the same timeline as the level changes and aborts, and reports min/median/max plus the process's peak in the summary, so a run's memory plateau can be read against the cache that produced it.
- `crates/app`: `--cache-mib N` sets the tile cache's byte budget (default unchanged: 64 MiB on desktop, 16 MiB on mobile) and `--cache-rows N` the model row count, which is the cache's real ceiling because a scheduler never holds more tiles than it has rows; a 900 px-wide window caps the cache at ~12 MiB of RGB8 tiles before the default budget can bind, so a memory run that needs the budget to bind raises the rows too.
- `crates/core`: `TileScheduler::evicted()` counts the tiles released to stay inside the byte budget, which `dropped()` never did (it counts superseded, over-capacity and row-less tiles); the `dropped` documentation now says which is which.
- `docs/benchmarks.md` §7.6: the RSS sampler's line format, the two cache ceilings told apart, and the long-pan eviction scenario (A0 at 6400%, panned to the page edge) run at the 64 MiB and 8 MiB budgets as well as the default, with the measured plateau following whichever ceiling is lower each time - 12 MiB of rows by default, 63.8 MiB of the 64 MiB budget, 7.5 MiB of the 8 - while `dropped` stays at the zoom burst's 123 in all three. It states the Phase 2 memory target as one metric (peak working set <= 250 MiB in the default configuration, measured 192.7 MiB) and flags what it does not claim: run B, whose rows are raised so the budget binds, peaks at 267.3 MiB.

- `crates/core`: `TileScheduler::prefetch` requests the one-tile ring around the viewport ahead of the user's pan. Its tiles land in the cache as hidden, evictable entries, so they cost no memory beyond the cache's ceiling, are never counted as a hole, never take a row the visible set claims, and are not requested at all once the cache holds its whole byte budget. Measured in `docs/benchmarks.md` §7.6: a 296-step pan at 10,240 px/s shows 0 hole frames against 241 of 296 with the ring blocked, for about three times the tile rasters at the default row count - the ceiling and its upgrade path are recorded on `AppState::ring`.
- `docs/backlog.md`: three issue drafts for work that is measured but deliberately not done - the ~70 MiB fixed overhead between an empty process and a loaded one (MuPDF's store, the display list, the base layer), an allocator experiment, and the `private`-vs-working-set gap - each written in the shape of `.github/ISSUE_TEMPLATE/` so it can be filed as it stands.


### Changed
- Tiles, the base layer and `PdfEngine::render_page` are rasterized **alpha-free - RGB8 on white paper - instead of RGBA8**: `PixelFormat::Rgb8`, a `Bitmap` whose stride is `width * 3`, and `Image::from_rgb8` on the app side, which hands Slint's femtovg renderer the 3-byte buffer as it is. A cached tile drops from 1 MiB to 768 KiB and the UI copy beside it drops with it, so the byte budget now measures what the process actually holds instead of half of it. No visible pixel changes: MuPDF clears an alpha-free pixmap to `0xff`, so the paper is white rather than a transparent hole, and the tests assert that corner per fixture - including a new assertion on `large_200p.pdf` (corner `[255,255,255]`, `data.len() == w * h * 3`) and the A0 @ 6400% acceptance test, which now compares tiles against an independent render on a grid crossing instead of over blank paper (max channel delta 0, 0 differing bytes of 3,145,728).
- The default cache is the **visible tiles plus the one-tile prefetch ring**, which is the row count the viewport already derived; `--cache-mib` / `--cache-rows` still override both. With RGB8 tiles that same row count holds 12 MiB rather than 16, so §7.6's commands raise `--cache-rows` from 80 to 96 to make the 64 MiB budget bind.
- `docs/plans/0001-tiled-viewport-rendering.md`: Phase 2 is **closed** - every item on its list (pool, cancellation, base layer, prefetch ring, the app-boundary check of the byte budget) is implemented and measured, and the RSS half of its acceptance has a measured result. §4.9 states the memory target as one named metric (peak working set, <= 250 MiB in the default configuration, measured 192.7 MiB) and records that the earlier "+50 MiB over the baseline" framing was not met; the sections that quoted RGBA8 sizes, the 3x3 page-centre acceptance block and the byte-identity stride were updated with it. The gate run that replaces §7.5's pre-pool header has since landed, and the next plan is 0002.
- `docs/benchmarks.md` §7.5 is **re-measured on the pooled, RGB8, ring build** (2026-10-06, the same 884 px viewport §7.6 uses), with the pre-pool tables kept beside it as the baseline they replace. Settled zoom steps and six-step bursts land in 1-3 ms `t_blank`; the whole-viewport wait (`t_full`) falls from a pre-pool worst of 22 ms to 9 ms on the A0; panning reports **0 hole frames** at 3900 and 24700 px/s where the pre-ring build reported 3, 9 and 13; `abort_ms` never exceeds 2 ms on any run that sampled one; five repeats of the A0 burst all pass. One row still FAILs and is reported as such: a zoom burst fired 800 ms into the document's own first render takes 714 ms for the new level's first tile - inside the pre-pool 124-929 ms envelope - because the pool parallelizes tile rasters, not the display-list build that this wait is spent on. §7.5 and the plan say so, and the stretched-level item that covers it is Phase 3's.
- Maximum zoom raised from 1000% to 6400%, with memory bounded by the tile cache instead of the whole-page raster (an A0 page at 6400% previously required a 125 GiB buffer, which cannot be allocated).
- `crates/app` re-rasterizes only the tiles affected by a pan or zoom, instead of the entire page.
- Project named **PODEFA**, developed by FINE Association, with repository metadata and contributor contacts filled in.
- `docs/benchmarks.md` extended with Phase 1 before/after measurements (an A0 page at 400% drops from 490 MiB and 541 ms per zoom step to about 6 MiB of tiles and 5 ms).
- `docs/benchmarks.md` §7.5 adds interactive zoom/pan measurements from the new harness: the A0 rapid-zoom burst costs 2-14 ms per step once the document has rendered, and the 929 ms stall reported by the first revision was the document's own first render (§7.2-7.3's one-off) sampled while it was still in flight, not stale tiles queued ahead of the epoch `Cancel`. The harness now reports per-phase `window` lines and keeps the startup render out of the `VERDICT`. Those measurements were the pre-pool baseline, which the Phase 2 gate run has since re-measured in place (§7.5).
- `crates/engine-mupdf`: the pool decides whether a finished raster may be published inside the registry lock that cancellation takes, in the same critical section the epoch flag is set in, so "a cancelled epoch publishes nothing" follows from that lock instead of from how quickly a flag propagates between cores; the flag read before a raster starts is now only a way to skip work that would be discarded. Abort latency is documented (and measured) as bounded by the largest display-list node rather than by a fixed ceiling, since MuPDF tests the cookie at node boundaries.

### Removed
- `crates/render`: empty placeholder crate; its responsibilities live in `crates/core` (`tiling`, `scheduler`).
