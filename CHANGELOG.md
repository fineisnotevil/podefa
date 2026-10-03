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
- Multi-crate Cargo workspace structure:
  - `crates/core`: Engine-agnostic domain models (`PdfEngine`, `DocumentInfo`, `Rect`, `Bitmap`, `Command`).
  - `crates/engine-mupdf`: MuPDF integration stub implementing `PdfEngine`.
  - `crates/render`: Tile scheduler and rasterization cache crate placeholder.
  - `crates/app`: Declarative Slint UI desktop application scaffold.
- Release compilation profile with binary size optimizations (`opt-level = "s"`, fat LTO, abort panic, symbol stripping).
- Complete REUSE 3.3 specification compliance with SPDX license identifiers and `LICENSES/` catalog.
- Auditing configuration via `deny.toml` enforcing AGPL-3.0 compatibility and dependency bans.
- Architectural Decision Records (`docs/adr/`):
  - `0001-stack-rust-slint-mupdf.md`
  - `0002-license-agpl.md`
- Community and project health standards (`README.md`, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md`, `justfile`).
- GitHub Actions multi-platform CI matrix (Linux, macOS, Windows) with linting, tests, license checks, REUSE compliance, and DCO sign-off validation.
