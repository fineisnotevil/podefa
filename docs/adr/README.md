<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# Architecture Decision Records (ADRs)

This directory documents key architectural, technical, and licensing decisions made throughout the lifecycle of `PODEFA`.

## Record Format

Each ADR follows a standardized format:
- **Title**: Number and descriptive title.
- **Status**: Proposed, Accepted, Superceded, Deprecated.
- **Context**: The background problem, constraints, and motivations.
- **Decision**: The specific choice made and rationale.
- **Consequences**: Positive and negative trade-offs resulting from the decision.

## Index of Records

- [0001: Technology Stack - Rust, Slint, and MuPDF](0001-stack-rust-slint-mupdf.md)
- [0002: Licensing - AGPL-3.0-or-later and DCO Contributions](0002-license-agpl.md)
- [0003: Dedicated Engine Actor Threading Model & Render Cancellation](0003-engine-actor-threading.md) (amended 2026-10-20)
- [0004: Opening Documents - File Dialog, Drag-and-Drop and Settings](0004-opening-documents.md)
- [0005: A Search Thread with Its Own Document](0005-search-thread.md)
- [0006: Clipboard and Text Selection](0006-clipboard-and-selection.md)
- [0007: Editing Annotations and Saving](0007-editing-and-saving.md)
- [0008: Undo and Redo Through MuPDF's Journal](0008-undo-journal.md)
- [0009: Structural Edits and Page Maps](0009-structural-edits-and-page-maps.md)
- [0010: The Window Draws Its Own Frame; Commands Live in Islands](0010-own-frame-and-islands.md)
