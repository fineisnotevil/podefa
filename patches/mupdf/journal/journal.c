// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

// MuPDF's undo journal behind fz_try (Plan 0004 Phase 2, ADR 0008).
//
// mupdf-sys 0.8.0 wraps pdf_begin_operation, pdf_end_operation and pdf_abandon_operation but not
// the rest of the journal, and these functions throw (undoing at the start of the history, or
// during an operation), so calling MuPDF's raw bindings from Rust would be undefined behaviour.
// Each wrapper below has the shape of mupdf-sys' own (wrapper/pdf_document.c): it catches MuPDF's
// error and hands it back through mupdf-sys' `mupdf_save_error`, so the Rust side reads it with
// the crate's usual `ffi_try!`.
//
// This file is compiled exactly as mupdf-sys compiles its wrappers (build.rs: a plain cc::Build,
// the MuPDF headers, no extra defines), against a copy of the headers of the MuPDF version
// mupdf-sys builds (journal/include, 1.27.2). fa_pdf_journal_headers_match lets MuPDF itself check
// that they are the linked library's: the crate refuses to enable the journal otherwise.
// It goes away once a mupdf-rs release wraps the journal itself.

#include <stdlib.h>
#include "mupdf/fitz.h"
#include "mupdf/pdf.h"

/* mupdf-sys' error type and its helper (wrapper/internal.h and wrapper/error.c in mupdf-sys). */
typedef struct mupdf_error mupdf_error_t;
void mupdf_save_error(fz_context *ctx, mupdf_error_t **errptr);

/* 1 when the headers this file was compiled with are the linked library's version: MuPDF's
 * fz_new_context_imp compares the version it is given with its own and refuses on a mismatch. */
int fa_pdf_journal_headers_match(void)
{
    fz_context *ctx = fz_new_context_imp(NULL, NULL, 0, FZ_VERSION);
    if (ctx == NULL)
        return 0;
    fz_drop_context(ctx);
    return 1;
}

void fa_pdf_enable_journal(fz_context *ctx, pdf_document *doc, mupdf_error_t **errptr)
{
    fz_try(ctx)
        pdf_enable_journal(ctx, doc);
    fz_catch(ctx)
        mupdf_save_error(ctx, errptr);
}

void fa_pdf_undo(fz_context *ctx, pdf_document *doc, mupdf_error_t **errptr)
{
    fz_try(ctx)
        pdf_undo(ctx, doc);
    fz_catch(ctx)
        mupdf_save_error(ctx, errptr);
}

void fa_pdf_redo(fz_context *ctx, pdf_document *doc, mupdf_error_t **errptr)
{
    fz_try(ctx)
        pdf_redo(ctx, doc);
    fz_catch(ctx)
        mupdf_save_error(ctx, errptr);
}

/* The position in the history (0 = the document as opened) and the number of steps. */
int fa_pdf_undoredo_state(fz_context *ctx, pdf_document *doc, int *steps, mupdf_error_t **errptr)
{
    int position = 0;
    *steps = 0;
    fz_try(ctx)
        position = pdf_undoredo_state(ctx, doc, steps);
    fz_catch(ctx)
        mupdf_save_error(ctx, errptr);
    return position;
}

/* The offset of the cross-reference section the next incremental save names as /Prev, and setting
 * it. MuPDF 1.27.2 moves it to the section each incremental save writes (pdf-write.c, writexref and
 * writexrefstream), although the next incremental save of the same document truncates the file to
 * its size at open and writes one cumulative section again (ensure_initial_incremental_contents):
 * from the second save on, /Prev named a section that was gone, and a reader followed it into the
 * middle of the new data, or into the new section itself. The caller puts back the value the
 * document was opened with before each incremental save. Plain field accesses that cannot throw;
 * the struct layout is the headers', which fa_pdf_journal_headers_match vouches for. */
int64_t fa_pdf_startxref(pdf_document *doc)
{
    return doc->startxref;
}

void fa_pdf_set_startxref(pdf_document *doc, int64_t startxref)
{
    doc->startxref = startxref;
}

