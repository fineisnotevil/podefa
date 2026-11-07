// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

// Signing a signature field (Plan 0009 Phase 4, ADR 0015). MuPDF writes the PDF side: the field's
// value, the room for /Contents, the appearance, the byte range at save time. It asks a signer
// (pdf_pkcs7_signer, pdf/form.h) for the CMS blob; MuPDF's own signer is built on OpenSSL, which
// this build does not have. The signer here hashes the signed bytes with MuPDF's own SHA-2 and
// hands the digest to Rust (fa_rust_signer_sign), which builds and signs the CMS. Rust never sees
// an fz_stream, so nothing that can throw runs under a Rust frame: the read is here, in C.

#include <string.h>
#include "mupdf/fitz.h"
#include "mupdf/pdf.h"

typedef struct mupdf_error mupdf_error_t;
void mupdf_save_error(fz_context *ctx, mupdf_error_t **errptr);

/* In Rust (patches/mupdf/src/pdf/sign.rs). The CMS for `digest` into `out`: its length, 0 when
 * signing failed, more than `out_len` when it does not fit. */
size_t fa_rust_signer_sign(void *rust, const unsigned char *digest, size_t digest_len,
                           unsigned char *out, size_t out_len);
void fa_rust_signer_drop(void *rust);

enum { FA_SHA256 = 0, FA_SHA384 = 1, FA_SHA512 = 2 };

typedef struct
{
    pdf_pkcs7_signer base;
    int refs;
    void *rust;
    size_t max_size;
    int hash;
    char *cn, *o, *ou, *email, *c;
} fa_signer;

static pdf_pkcs7_signer *fa_keep(fz_context *ctx, pdf_pkcs7_signer *signer)
{
    return fz_keep_imp(ctx, signer, &((fa_signer *)signer)->refs);
}

static void fa_drop(fz_context *ctx, pdf_pkcs7_signer *signer)
{
    fa_signer *s = (fa_signer *)signer;
    if (fz_drop_imp(ctx, s, &s->refs))
    {
        fa_rust_signer_drop(s->rust);
        fz_free(ctx, s->cn);
        fz_free(ctx, s->o);
        fz_free(ctx, s->ou);
        fz_free(ctx, s->email);
        fz_free(ctx, s->c);
        fz_free(ctx, s);
    }
}

static char *copy_or_null(fz_context *ctx, const char *text)
{
    return text ? fz_strdup(ctx, text) : NULL;
}

static pdf_pkcs7_distinguished_name *fa_name(fz_context *ctx, pdf_pkcs7_signer *signer)
{
    fa_signer *s = (fa_signer *)signer;
    pdf_pkcs7_distinguished_name *dn = fz_malloc_struct(ctx, pdf_pkcs7_distinguished_name);
    fz_try(ctx)
    {
        dn->cn = copy_or_null(ctx, s->cn);
        dn->o = copy_or_null(ctx, s->o);
        dn->ou = copy_or_null(ctx, s->ou);
        dn->email = copy_or_null(ctx, s->email);
        dn->c = copy_or_null(ctx, s->c);
    }
    fz_catch(ctx)
    {
        pdf_signature_drop_distinguished_name(ctx, dn);
        fz_rethrow(ctx);
    }
    return dn;
}

static size_t fa_max_size(fz_context *ctx, pdf_pkcs7_signer *signer)
{
    return ((fa_signer *)signer)->max_size;
}

static int fa_create(fz_context *ctx, pdf_pkcs7_signer *signer, fz_stream *in,
                     unsigned char *out, size_t out_len)
{
    fa_signer *s = (fa_signer *)signer;
    unsigned char buffer[65536];
    unsigned char digest[64];
    size_t digest_len, n, written;

    if (s->hash == FA_SHA256)
    {
        fz_sha256 state;
        fz_sha256_init(&state);
        while ((n = fz_read(ctx, in, buffer, sizeof buffer)) > 0)
            fz_sha256_update(&state, buffer, n);
        fz_sha256_final(&state, digest);
        digest_len = 32;
    }
    else if (s->hash == FA_SHA384)
    {
        fz_sha384 state;
        fz_sha384_init(&state);
        while ((n = fz_read(ctx, in, buffer, sizeof buffer)) > 0)
            fz_sha384_update(&state, buffer, n);
        fz_sha384_final(&state, digest);
        digest_len = 48;
    }
    else
    {
        fz_sha512 state;
        fz_sha512_init(&state);
        while ((n = fz_read(ctx, in, buffer, sizeof buffer)) > 0)
            fz_sha512_update(&state, buffer, n);
        fz_sha512_final(&state, digest);
        digest_len = 64;
    }

    written = fa_rust_signer_sign(s->rust, digest, digest_len, out, out_len);
    if (written == 0)
        fz_throw(ctx, FZ_ERROR_GENERIC, "the signature could not be made");
    if (written > out_len)
        fz_throw(ctx, FZ_ERROR_GENERIC, "the signature is larger than the room left for it");
    return (int)written;
}

/* A signer that calls `rust` (owned from now on: dropped with the signer, also on an error here). */
pdf_pkcs7_signer *fa_new_signer(fz_context *ctx, void *rust, size_t max_size, int hash,
                                const char *cn, const char *o, const char *ou, const char *email,
                                const char *c, mupdf_error_t **errptr)
{
    fa_signer *s = NULL;
    fz_var(s);
    fz_try(ctx)
    {
        s = fz_malloc_struct(ctx, fa_signer);
        s->refs = 1;
        s->rust = rust;
        s->max_size = max_size;
        s->hash = hash;
        s->base.keep = fa_keep;
        s->base.drop = fa_drop;
        s->base.get_signing_name = fa_name;
        s->base.max_digest_size = fa_max_size;
        s->base.create_digest = fa_create;
        s->cn = copy_or_null(ctx, cn);
        s->o = copy_or_null(ctx, o);
        s->ou = copy_or_null(ctx, ou);
        s->email = copy_or_null(ctx, email);
        s->c = copy_or_null(ctx, c);
    }
    fz_catch(ctx)
    {
        if (s)
        {
            s->rust = rust;
            fa_drop(ctx, &s->base);
        }
        else
            fa_rust_signer_drop(rust);
        mupdf_save_error(ctx, errptr);
        return NULL;
    }
    return &s->base;
}

void fa_drop_signer(fz_context *ctx, pdf_pkcs7_signer *signer)
{
    if (signer)
        signer->drop(ctx, signer);
}

/* Signs `widget` with MuPDF's appearance (`flags`, PDF_SIGNATURE_SHOW_*), as `adbe.pkcs7.detached`
 * or, with `cades`, as `ETSI.CAdES.detached` (PAdES baseline): MuPDF writes the first, and the
 * name is the only difference in the PDF. MuPDF puts the reason and the location only in the
 * appearance; they are written into the signature dictionary too (/Reason, /Location, ISO
 * 32000-1 table 252), where readers look for them. The signature is completed when the document
 * is saved incrementally. */
void fa_pdf_sign_signature(fz_context *ctx, pdf_annot *widget, pdf_pkcs7_signer *signer, int flags,
                           const char *reason, const char *location, int cades,
                           mupdf_error_t **errptr)
{
    pdf_document *doc = pdf_annot_page(ctx, widget)->doc;
    int open = 0;
    int disallowed = doc->disallow_new_increments;
    fz_var(open);
    fz_try(ctx)
    {
        pdf_obj *wobj = pdf_annot_obj(ctx, widget);
        /* The page the widget is on, which MuPDF's new signature fields leave out and readers
         * use to place a signature in their lists. */
        if (!pdf_dict_get(ctx, wobj, PDF_NAME(P)))
        {
            pdf_begin_operation(ctx, doc, "Sign signature");
            open = 1;
            pdf_dict_put(ctx, wobj, PDF_NAME(P), pdf_annot_page(ctx, widget)->obj);
            open = 0;
            pdf_end_operation(ctx, doc);
        }
        pdf_sign_signature(ctx, widget, signer, flags, NULL, reason, location);
        /* After a signature MuPDF puts every change in a new incremental section
         * (ensure_incremental_xref, pdf-xref.c), and the signature's byte range ends with the
         * signature's own section. A change to the signature dictionary there would leave the
         * dictionary outside its own range: the next lines belong to the signature, so they go
         * into its section. */
        doc->disallow_new_increments = 1;
        /* The dictionary's own step, so a journal records it as it records MuPDF's. */
        pdf_begin_operation(ctx, doc, "Sign signature");
        open = 1;
        pdf_obj *v = pdf_dict_get(ctx, pdf_annot_obj(ctx, widget), PDF_NAME(V));
        if (cades)
            pdf_dict_put_drop(ctx, v, PDF_NAME(SubFilter), pdf_new_name(ctx, "ETSI.CAdES.detached"));
        if (reason && *reason)
            pdf_dict_put_text_string(ctx, v, PDF_NAME(Reason), reason);
        if (location && *location)
            pdf_dict_put_text_string(ctx, v, PDF_NAME(Location), location);
        open = 0;
        pdf_end_operation(ctx, doc);
    }
    fz_always(ctx)
        doc->disallow_new_increments = disallowed;
    fz_catch(ctx)
    {
        if (open)
            pdf_abandon_operation(ctx, doc);
        mupdf_save_error(ctx, errptr);
    }
}

