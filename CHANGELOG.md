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

### Changed
- Maximum zoom raised from 1000% to 6400%, with memory bounded by the tile cache instead of the whole-page raster (an A0 page at 6400% previously required a 125 GiB buffer, which cannot be allocated).
- `crates/app` re-rasterizes only the tiles affected by a pan or zoom, instead of the entire page.
- Project named **PODEFA**, developed by FINE Association, with repository metadata and contributor contacts filled in.
- `docs/benchmarks.md` extended with Phase 1 before/after measurements (an A0 page at 400% drops from 490 MiB and 541 ms per zoom step to about 6 MiB of tiles and 5 ms).

### Removed
- `crates/render`: empty placeholder crate; its responsibilities live in `crates/core` (`tiling`, `scheduler`).
