<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# ADR 0007: Editing Annotations and Saving

## Status

Accepted (Plan 0004 Phase 0 gate, questions 1, 3, 4 and 5, 2026-10-21). Extends ADR 0003.

## Context

v0.2 annotates PDFs: it draws the annotations a document already has, creates text markup from a
selection, selects, recolours and deletes annotations, and saves. Four things had to be decided:

- **Who edits.** The engine actor owns the document (ADR 0003) while the render pool replays the
  display lists it was handed, concurrently. The search thread has a second `Document` on the same
  file (ADR 0005).
- **How the screen learns that a page changed** without blanking it, and without ever showing a
  tile drawn before an edit after one drawn after it - the pool finishes jobs in any order.
- **How to write the file.** Measured on Windows (`docs/benchmarks.md` §10.1): an incremental save
  to the open file works while both documents read it; a rename over it does not (`os error 5`).
- **What a non-PDF document does**: MuPDF also opens images, EPUB and text, which it cannot save.

## Decision

1. **The actor edits.** A PDF is held as a `PdfDocument`; any other format as a plain `Document`,
   view-only (`Opened { editable }`). `EngineCmd::Edit { op, edit_id, generation }` applies one
   `pdf_core::annot::EditOp` - add markup, recolour, delete - between two commands, so an edit never
   races a display-list build. Worker threads keep replaying the lists they were given: a display
   list is a snapshot, and 600 edits against three rendering workers gave no crash and no changed
   pixel on untouched pages (§10.1).
2. **Every page has a revision.** An edit drops the page's cached display list and bumps its
   revision; every job carries the revision of the list it was built from, and `TileRendered`,
   `BaseRendered` and `ThumbRendered` report it. The actor sends `PageChanged { page, revision,
   items }` before it prepares anything else, so the app knows a revision before it can see pixels
   of it, and drops a tile, base layer or thumbnail of an older revision on arrival. The scheduler
   marks the page's visible tiles stale - they stay on screen - asks for them again, and releases
   its hidden ones; other pages are untouched (`TileScheduler::invalidate_page`).
3. **Save appends to the open file; Save As writes a copy and switches to it** (question 3 A).
   Save is MuPDF's incremental save to the opened path: the bytes the file had when it was opened
   stay as they are, which keeps existing signatures valid. MuPDF writes one section with every
   change since the open after those bytes, truncating whatever an earlier save of the same session
   appended (`pdf-write.c`, `ensure_initial_incremental_contents`), so the file is always the opened
   bytes plus one section. That section's `/Prev` must name the file's own cross-reference
   section; MuPDF 1.27.2 names the previous save's section instead (`writexref` moves
   `doc->startxref` to each section it writes), which the next save overwrites, so from the second
   save in a session the file lost pages for every reader (found 2026-10-22 on a maintainer's
   file that then opened with none). The engine puts the offset the file was opened with back
   before each incremental save, through the `fz_try` shim of ADR 0008
   (`fa_pdf_set_startxref`), and does not save in place without it. A file MuPDF repaired at open cannot take an incremental update: Save
   answers `SaveError::NeedsSaveAs` and the app asks where to write a copy. Save As writes a full
   copy and reopens it in the actor and the search thread; the pages keep their revisions. A Save As
   naming the open file is saved incrementally instead, because a full rewrite would destroy the file
   MuPDF reads from.
4. **The app counts its own unsaved state** (`pdf_core::annot::SaveState`): MuPDF's
   `has_unsaved_changes` stays set after an incremental save. The title shows `*`; opening another
   file, File > Quit and closing the window ask "Save the changes?" (Yes / No / Cancel) through
   `rfd::MessageDialog`, and Yes runs the action once the save succeeded.
5. **Markup both ways** (question 4): with text selected, a markup button (or H, U, K) marks it; with
   nothing selected the button turns the tool on, and every drag over text marks when it ends. A
   click without a drag on an annotation selects it. **New markup replaces old markup of the same
   kind where they overlap** (Phase 1 review, option B): the old annotation is cut to the parts of
   its lines outside the new one, or deleted when nothing is left, in the same edit; other kinds
   stay stacked, and markup with a comment or with quads that are not upright rectangles is never
   cut.
6. **No author unless one is set** (question 5): `author=` in the settings file; nothing personal is
   written otherwise.

## Consequences

### Positive
- An edit is on screen in milliseconds (`t_edit` median 3-10 ms, worst 16 ms, `docs/benchmarks.md`
  §10.3) and the page never blanks while it re-renders.
- A tile drawn before an edit can never be shown after it, by construction: the revision travels
  with the job.
- Saving is fast (1 ms for a 17 KB file) and never rewrites the bytes the document was opened with.

### Negative
- An in-place save is not atomic. A crash while MuPDF writes its section - after it truncated the
  file to the opened length - loses every change of the session that the file held, though never the
  opened document itself. The atomic alternative (write a copy, close both documents, rename,
  reopen: §9 question 3 B, which costs a reopen per save and invalidates signatures) was not
  chosen at the gate.
- Save As reopens the document, so MuPDF state that belongs to the open document - Phase 2's undo
  journal - starts again from the copy.
- A file changed by another program while open is overwritten by the next save (backlog).
