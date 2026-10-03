<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# ADR 0001: Technology Stack Selection (Rust, Slint, MuPDF)

## Status

Accepted

## Context

`[PROJECT_NAME]` aims to provide a lightweight, cross-platform PDF viewer and editor with:
- Minimal startup latency and instantaneous document display.
- Modest memory consumption (avoiding the high baseline RAM demands of web runtimes like Electron or heavy toolkits).
- Robust type safety, memory safety, and cross-platform compilation targets (Windows, Linux, macOS initially; mobile later).
- Complete PDF specification rendering capabilities, including complex vector geometries, font subsets, annotations, and color management.

We evaluated several combinations:
1. **Language**: C++ vs Rust vs Go.
2. **UI Framework**: Qt/QML vs Slint vs Iced vs Tauri/Electron.
3. **PDF Engine**: PDFium vs Poppler vs MuPDF vs pure-Rust pdf crates (e.g. `pdf-rs`, `lopdf`).

## Decision

We choose:
1. **Rust** as the primary programming language for its strong memory-safety guarantees, fearless concurrency, modern tooling (`cargo`), and seamless FFI interoperability.
2. **Slint** for the declarative UI layer. Slint compiles declarative markup directly into native Rust code, uses minimal memory, boots instantly, provides a clean reactive architecture, and supports multiple backends (Qt, Skia, software, Winit).
3. **MuPDF** (via the `mupdf` crate bindings) as the underlying PDF parsing and rendering engine. MuPDF is renowned for speed, small binary footprint, high visual fidelity, and comprehensive support for PDF annotations, form fields, and color rendering.

To isolate these dependencies:
- Domain primitives and traits are isolated in `crates/core` with zero UI or engine dependencies.
- MuPDF is encapsulated behind the `PdfEngine` trait in `crates/engine-mupdf`.
- The UI layer (`crates/app`) consumes only high-level abstractions and rendered bitmaps from `crates/render`.

## Consequences

### Positive
- Sub-100ms startup times and small memory footprint compared to Electron or Chromium-based alternatives.
- Memory safety in core state management, command histories (undo/redo), and tile caching logic.
- Well-tested PDF fidelity powered by MuPDF's decades of production refinement.
- Clean architectural decoupling: backends can be swapped or mocked in unit tests without touching the UI.

### Negative
- Compiling `mupdf` requires native C toolchain and LLVM/`libclang` prerequisites on build machines.
- `mupdf-rs` bindings require platform-specific maintenance (e.g. MSVC header alignments).
