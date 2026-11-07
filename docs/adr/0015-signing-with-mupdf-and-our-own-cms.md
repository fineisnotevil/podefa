<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# ADR 0015: Signing Through MuPDF with Our Own CMS, as a Save, and a Reopen After It

## Status

Accepted (2026-11-07, Plan 0009 Phase 4; the crates follow the maintainer's answer to the plan's
§9 question 6, the key source question 5's first step, a `.p12`/`.pfx` file).

## Context

MuPDF writes a signature's PDF side: `pdf_sign_signature` fills the field's value (`/ByteRange`,
room for `/Contents`, `/M`, a FieldMDP reference), draws the appearance, and records the signature
on the newest cross-reference section; the next incremental save writes the file, works out the
byte range and asks a signer (`pdf_pkcs7_signer`, `pdf/form.h`) for the CMS over the signed bytes
(`complete_signatures`, `pdf-write.c:1608`). MuPDF's own signer is OpenSSL's, which this build does
not have. Four things came out while building on it (mupdf-sys 0.8.0, MuPDF 1.27.2):

1. MuPDF writes `adbe.pkcs7.detached`; PAdES baseline B-B wants `ETSI.CAdES.detached` and a
   signing-certificate-v2 attribute in the CMS, and no signing time there.
2. MuPDF puts the reason and the location only into the appearance, not into the signature
   dictionary, where readers list them.
3. After a signature, MuPDF puts **every** further change into a new incremental section
   (`ensure_incremental_xref`, `pdf-xref.c:526`), and the byte range ends with the signature's own
   section. Changing the signature dictionary after `pdf_sign_signature` (for 1 and 2), or the
   app's `/Producer` stamp in the save, moved the dictionary into the later section: the byte
   range was computed from offsets in two sections, its last length came out negative, and the
   signature was broken.
4. The engine's save in place rewrites one cumulative section after the bytes the file had at
   open (ADR 0007, the startxref accessor). A save after signing, without opening the file again,
   would rewrite the signed section and break the signature.

## Decision

- **The signer** is `fa_signer` in `patches/mupdf/journal/sign.c`: it reads the signed bytes from
  MuPDF's stream and hashes them with MuPDF's own SHA-2 inside `fz_try`, then calls Rust
  (`fa_rust_signer_sign`, `patches/mupdf/src/pdf/sign.rs`) with the digest only. Rust never sees an
  `fz_stream`, nothing that can throw runs under a Rust frame, and the Rust side catches panics.
- **The CMS** (`crates/engine-mupdf/src/cms_sign.rs`) is a detached `SignedData` built with
  RustCrypto's `cms`, `x509-cert` and `der` and signed with `ring`: signed attributes content type,
  message digest and signing-certificate-v2 (the certificate's SHA-256 and its issuer and serial),
  no signing time; RSA PKCS#1 v1.5 with SHA-256, ECDSA P-256 with SHA-256, P-384 with SHA-384; the
  certificates in the key file go with it. `/SubFilter` is `ETSI.CAdES.detached`.
- **The key** comes from a PKCS#12 file read by our own code (`crates/engine-mupdf/src/p12.rs`)
  over RustCrypto's `pkcs12` structures: PBES2 (PBKDF2 and AES) through `pkcs5`, the legacy
  PKCS#12 PBE (3DES, RC2-128, RC2-40) through `des`, `rc2` and `cbc`, the MAC checked with `ring`'s
  HMAC so a wrong password is told from a damaged file. The key is matched to its certificate by
  public key. `p12-keystore`, which reads the same, was tried and dropped: it brings `der` 0.8, a
  `cms` release candidate, `x509-parser` with `nom`, `rand` 0.10 and `getrandom` 0.4 beside what
  Phase 3 already has.
- **The dictionary's own lines** (`/SubFilter`, `/Reason`, `/Location`, and `/P` on a new field) are
  written in the signature's section: `doc->disallow_new_increments` is set around them, as MuPDF
  sets it itself while it writes. The `/Producer` stamp is made before signing.
- **Signing is a save**: `MupdfEngine::sign` checks the permissions (filling), a certifying
  signature (level 1 allows nothing), that the file takes an incremental update, and the key file
  and password, before it changes anything; then saves unsaved changes in place, signs, saves in
  place, and **opens the file again**, after a success so that the next save keeps the signature
  (4), after a failure so that a half-made signature is gone and the document is the file. The
  undo history starts again from the signed file, as after Save As.

## Consequences

- A signature made here verifies in this app and with `openssl cms -verify`, and a second
  signature leaves the first revision's bytes as they were (`crates/engine-mupdf/tests/sign_test.rs`,
  `docs/benchmarks.md` §15.6).
- The appearance is MuPDF's; for text beyond Latin-1 it embeds its built-in Nimbus Sans, about
  27 KB per signature.
- Each signature costs a save and an open of the document.
- `disallow_new_increments` and the struct fields of `pdf_xref` come from MuPDF's headers, which
  `fa_pdf_journal_headers_match` vouches for, as for the other accessors.
- Only key files for now. PKCS#11 tokens (Turkish e-İmza), the Windows certificate store and
  Android's KeyChain are §9 question 5's open part; each is a `CmsSigner` of its own, and the
  signer interface asks only for a digest's signature.
- No timestamp (PAdES B-T) and no revocation data (B-LT): both need the network, against §0
  question 7's offline rule.
