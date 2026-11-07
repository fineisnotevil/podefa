<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# ADR 0009: Structural Edits and Page Maps

## Status

Accepted (Plan 0005 Phase 0 gate, question 2, 2026-10-22). Extends ADR 0007 and ADR 0008.

## Context

v0.3 rotates, deletes, duplicates, moves and inserts pages. Until then the page count and order
were fixed for a document's lifetime, and everything kept per page is keyed by its index:

- in the app, the layout, tiles, base layers, thumbnails, text lines, annotations, search hits,
  the selection and the page revisions (ADR 0007);
- in the engine, the display-list cache and the revisions;
- on the search thread, a second `Document` opened on the *file* (ADR 0005), whose pages stop
  matching the open document's as soon as a page moves.

## Decision

1. **A structural edit is an `EditOp`** (`EditOp::Pages(PageOp)`), one journal operation like an
   annotation edit, so undo and redo cover it (ADR 0008).
2. **It is described by a page map.** `pdf_core::pages::plan` computes, before the engine touches
   the document, for every page after the edit the page it was before, or `None` when its pixels
   are new (an inserted page, a turned page). Undo uses its inverse (`invert`). The engine checks
   the document's page count against the map and abandons the operation on a mismatch.
3. **The engine reports `Restructured { page_sizes, revision, map }`** instead of `PageChanged`,
   after the edit and after its undo or redo. Every page gets the same new revision, above every
   revision before, so no tile drawn under the old numbering passes for a new one; the display
   lists are dropped.
4. **The app rebuilds its layout and follows the map.** Base layers, thumbnails, text lines,
   annotations and the chosen pages move to their new indices; tiles are dropped (their canvas
   places changed); search hits and the text selection are cleared, and a search that was showing
   runs again. The reader stays on their page (`pdf_core::pages::follow`). Answers to requests made
   under the old numbering are recognised by a layout generation and dropped.
5. **The search thread reads a snapshot.** After each structural change the actor writes the
   document as it is now to memory (`write_to`, without garbage collection, which would break the
   journal: Plan 0005 §3.4) and the search thread opens it from those bytes.
6. **An edit that leaves every page as it was is no step**: MuPDF's journal records none for it,
   and the engine's history must not either, or undo would pair the wrong steps.

## Consequences

- Structural edits cost milliseconds on 1000 pages (Plan 0005 §11.4); the snapshot is a copy of the
  document in memory per structural change, as large as the document.
- The search thread's first text extraction from a snapshot costs what any newly opened document
  costs. That was 1.2-1.7 s per unembedded standard font before the font loader of Plan 0005 §3.8.
- Later structural tools (insert from file, stamps that change sizes, crop) only need a `PageOp`
  and its map.
