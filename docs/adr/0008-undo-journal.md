<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# ADR 0008: Undo and Redo Through MuPDF's Journal

## Status

Accepted (Plan 0004 Phase 0 gate, question 2 A, 2026-10-21). Extends ADR 0007.

## Context

v0.2 undoes and redoes edits, and later releases add page operations (v0.3) and form filling (v0.4)
that need the same. MuPDF has an undo journal on `pdf_document`: operations bracketed by
`pdf_begin_operation` / `pdf_end_operation`, then `pdf_undo`, `pdf_redo` and `pdf_undoredo_state`
(`mupdf/include/mupdf/pdf/object.h:380-438`). mupdf-sys 0.8.0 wraps only begin, end and abandon.
`pdf_undo` and `pdf_redo` report errors by `longjmp` (for example at the start of the history), so
calling the raw bindings from Rust would be undefined behaviour. No mupdf-rs release wraps them;
messense/mupdf-rs#267 proposed it and was closed unmerged.

## Decision

1. **Our own `fz_try` wrappers in `patches/mupdf`.**
   - Four C functions in `journal/journal.c`: enable journal, undo, redo, undo/redo state.
   - Each has the shape of mupdf-sys' own wrappers and reports errors through mupdf-sys'
     `mupdf_save_error`, so the Rust side uses the crate's `ffi_try!`.
   - On top of them, `PdfDocument::enable_journal / undo / redo / undo_redo_state / can_undo /
     can_redo`.
2. **Compiled exactly as mupdf-sys compiles its wrappers.**
   - mupdf-sys uses a plain `cc::Build` with `mupdf/include` and no defines (`mupdf-sys-0.8.0/build.rs:212-224`).
   - The one configuration-dependent type `fz_try` uses, `fz_jmp_buf`, is chosen from platform
     macros only (`fitz/system.h:119-153`).
   - The headers are a copy of MuPDF 1.27.2's, the version mupdf-sys 0.8.0 builds, under
     `patches/mupdf/journal/include`, annotated as Artifex's in `REUSE.toml`.
   - mupdf-sys is pinned to `=0.8.0`.
   - Before the journal is enabled, MuPDF itself checks that the copied headers are its version:
     `fz_new_context_imp` refuses a context whose header version differs from the library's
     (`fitz/context.c:252-255`).
3. **Every edit is one journal operation.**
   - With a journal, MuPDF refuses any change outside an operation (`pdf-object.c:1842-1843`).
   - The engine wraps each `EditOp` in `begin_operation` / `end_operation`, so cutting old markup
     and adding new is one undo step. A failed edit is rolled back by `abandon_operation`.
   - MuPDF's own nested operations merge into ours. Its implicit ones (appearance synthesis while
     drawing) merge into the previous step or are dropped (`pdf-object.c:1196-1258`), so the
     journal position counts our edits only.
4. **The engine records the page each step changed** (`MupdfEngine::history`). Undo and redo give
   that page a new revision, drop its display list and send `PageChanged`, then
   `EngineEvent::History { change, can_undo, can_redo }`.
5. **The app counts the unsaved state from the history** (`SaveState::edited / undone / redone`):
   - Undoing back to the saved position is clean again.
   - A new edit after undoing past the save makes that position unreachable.
   - Edit > Undo is Ctrl+Z; Redo is Ctrl+Y or Ctrl+Shift+Z.
   - **Amended 2026-10-23 (Plan 0005 Phase 4):** `journal/tools.c`, compiled the same way, adds
     two more: `pdf_rewrite_images` (lossy compression; its options struct is filled in C, so
     its layout is not copied into Rust) and `fz_save_pixmap_as_jpeg`. Exposed as
     `PdfDocument::rewrite_images` and `Pixmap::save_as_jpeg`, refused under the same header
     check.
6. **An upstream pull request** adds the same wrappers to mupdf-sys and mupdf-rs (messense/mupdf-rs#273, merged into mupdf-rs `main` as `7aa360d3` on 2026-10-22; no release
   carries it yet). When a release
   carries them, `journal/`, `build.rs` and the pin go.

## Consequences

### Positive
- Every kind of edit undoes the same way, now and for v0.3-v0.4, and an undo restores the very
  objects: row S compares annotation ids after 50 edits undone and redone (`undo_test.rs`).
- A save after undoing writes the undone state, and the open document can still redo past it.
- The C code we own is four wrappers in the same shape as mupdf-sys'.

### Negative
- Until upstream has the wrappers, `patches/mupdf` carries 968 KiB of headers, a C file and a
  build script, and mupdf-sys cannot be upgraded without replacing the headers. The version check
  turns a mismatch into "no undo" rather than undefined behaviour.
- The history lives in memory: it ends when the document is closed, and a Save As (which reopens
  the copy, ADR 0007) starts it again.
