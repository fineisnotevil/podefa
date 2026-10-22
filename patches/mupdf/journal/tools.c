// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

// MuPDF functions the v0.3 tools need that mupdf-sys 0.8.0 does not wrap (Plan 0005 §3.2, Phase 4),
// behind fz_try like journal.c: rewriting a document's images (lossy compression) and writing a
// pixmap as JPEG. Compiled the same way, against the same copy of the 1.27.2 headers; the Rust
// side refuses to call them unless fa_pdf_journal_headers_match says the headers are the linked
// library's. pdf_image_rewriter_options is filled here, in C, so its layout never has to be copied
// into Rust. They go away once a mupdf-rs release wraps these functions.

#include <string.h>
#include "mupdf/fitz.h"
#include "mupdf/pdf.h"

typedef struct mupdf_error mupdf_error_t;
void mupdf_save_error(fz_context *ctx, mupdf_error_t **errptr);

/* Colour and grey images above `threshold_dpi` are scaled (bicubic) to `to_dpi` and every
 * colour and grey image is recompressed as JPEG of `quality` (1-100) when that is smaller. Bitonal
 * images (fax, JBIG2) are left as they are: JPEG would only make them larger and blurred.
 * Bicubic, not averaging: averaging only halves (pdf-image-rewriter.c, resample), and not even
 * once when the target is exactly half, so 300 to 150 dpi would not scale at all. */
void fa_pdf_rewrite_images(fz_context *ctx, pdf_document *doc, int threshold_dpi, int to_dpi,
                           int quality, mupdf_error_t **errptr)
{
    pdf_image_rewriter_options opts;
    char q[8];

    memset(&opts, 0, sizeof(opts));
    fz_snprintf(q, sizeof(q), "%d", quality);

    opts.color_lossless_image_subsample_method = FZ_SUBSAMPLE_BICUBIC;
    opts.color_lossy_image_subsample_method = FZ_SUBSAMPLE_BICUBIC;
    opts.color_lossless_image_subsample_threshold = threshold_dpi;
    opts.color_lossless_image_subsample_to = to_dpi;
    opts.color_lossy_image_subsample_threshold = threshold_dpi;
    opts.color_lossy_image_subsample_to = to_dpi;
    opts.color_lossless_image_recompress_method = FZ_RECOMPRESS_JPEG;
    opts.color_lossy_image_recompress_method = FZ_RECOMPRESS_JPEG;
    opts.color_lossless_image_recompress_quality = q;
    opts.color_lossy_image_recompress_quality = q;

    opts.gray_lossless_image_subsample_method = FZ_SUBSAMPLE_BICUBIC;
    opts.gray_lossy_image_subsample_method = FZ_SUBSAMPLE_BICUBIC;
    opts.gray_lossless_image_subsample_threshold = threshold_dpi;
    opts.gray_lossless_image_subsample_to = to_dpi;
    opts.gray_lossy_image_subsample_threshold = threshold_dpi;
    opts.gray_lossy_image_subsample_to = to_dpi;
    opts.gray_lossless_image_recompress_method = FZ_RECOMPRESS_JPEG;
    opts.gray_lossy_image_recompress_method = FZ_RECOMPRESS_JPEG;
    opts.gray_lossless_image_recompress_quality = q;
    opts.gray_lossy_image_recompress_quality = q;

    opts.bitonal_image_recompress_method = FZ_RECOMPRESS_NEVER;
    opts.recompress_when = FZ_RECOMPRESS_WHEN_SMALLER;

    fz_try(ctx)
        pdf_rewrite_images(ctx, doc, &opts);
    fz_catch(ctx)
        mupdf_save_error(ctx, errptr);
}

/* Writes the pixmap to `filename` (UTF-8) as a baseline JPEG of `quality` (1-100). */
void fa_save_pixmap_as_jpeg(fz_context *ctx, fz_pixmap *pix, const char *filename, int quality,
                            mupdf_error_t **errptr)
{
    fz_try(ctx)
        fz_save_pixmap_as_jpeg(ctx, pix, filename, quality);
    fz_catch(ctx)
        mupdf_save_error(ctx, errptr);
}
