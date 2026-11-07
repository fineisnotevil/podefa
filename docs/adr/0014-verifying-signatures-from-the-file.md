<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# ADR 0014: Verifying Signatures in Rust from the File's Bytes, and Reading Changes Ourselves

## Status

Accepted (2026-11-07, Plan 0009 Phase 3; the crates follow the maintainer's answers to the plan's
§9 questions 6 and 7).

## Context

MuPDF's verifier is a struct of callbacks whose only implementation is built on OpenSSL, which the
build does not have. Plan 0009 §3.4 expected C trampolines around `pdf_check_digest` and
`pdf_check_certificate`, with the Rust verifier behind them, and took
`pdf_validate_change_history` for the answer about changes after signing.

Two things came out while building it:

1. A signature speaks for bytes in a file. The trampolines would have handed Rust a stream of the
   range, through the open document, whose journal holds unsaved work and whose thread renders;
   the same bytes can be read from the file directly, with less unsafe code.
2. `pdf_validate_change_history` is not enough (`pdf-xref.c`, mupdf-sys 0.8.0). It knows fields but
   not annotations: as soon as anything is locked, `validate_locked_fields` counts a changed page
   dictionary as a "change with no reason", so a note makes a signature with DocMDP level 3 or a
   FieldMDP lock "forbidden", though both allow it (ISO 32000-2, 12.8.2). It also misses a real
   change: `check_unchanged_between` marks a difference on the value's object number, which is 0
   for a value written in the object itself, and the final loop starts at 1; so a signed widget
   moved off its page passes, at level 1 as well (tested by `signature_test`).

## Decision

- **The verifier** (`crates/engine-mupdf/src/verify.rs`) is Rust over the file: `ring` for the
  arithmetic, RustCrypto's `cms`, `x509-cert` and `der` for the structures, no trampolines. MuPDF
  gives only the field's `/ByteRange` and `/SubFilter`. Anything the code cannot read is "unknown",
  never "broken"; "broken" is kept for what was shown wrong.
- **It runs on a thread of its own**, with a document of its own opened from the path, after the
  file was opened or written. `EngineEvent::Signatures` carries the path it is about, so the app
  drops a late answer about another file. An unsaved edit changes nothing it says: the state is
  about the file on disk, which is what another reader will see.
- **The roots** are the system's: Windows' `ROOT` store, the bundle files on Linux and macOS, the
  certificate folders on Android, through `verify::Roots`, with no further crate. Revocation is not
  checked (§9 question 7) and the panel says so each time, so "valid" is not read as more than it
  is.
- **Changes after signing** (`crates/engine-mupdf/src/mdp.rs`) compare the signed version, opened
  from the start of the file, with the whole file, object by object. Each difference must be
  something the signature leaves free, and whatever is not recognized counts as forbidden. MuPDF's
  answer stands at levels 1 and 2, where it reads the fields those levels leave free, and what is
  signed is compared as well; at level 3 and with field locks the comparison decides alone; a
  signature with no lock speaks only for its own version.

## Consequences

- No unsafe beyond the Windows store. The verifier and its tests run on every platform that opens
  a file, Android included, and need no patch of MuPDF but one `fz_try` wrapper.
- `ring` carries C and assembly, already needed for signing in Phase 4; it passes `cargo deny`
  (its licences are on the allow list). Size: `docs/benchmarks.md` §15.5.
- The comparison is ours to keep correct. It errs towards warning: an update it does not recognize
  makes a signature read as invalid, which a reader can look into, and never the reverse.
- `adbe.pkcs7.sha1` and `adbe.x509.rsa_sha1` are reported as not read, not as bad
  (`docs/backlog.md` item 33).

## Alternatives considered

- **Trampolines, as planned**: more unsafe code, Rust called from inside `fz_try`, and a verifier
  bound to the open document's thread, for no gain in what is checked.
- **Taking MuPDF's change check as it is**: a note on a certified form would say "invalid
  signature", and a moved signature would pass. The first alone would teach readers to ignore the
  warning.
- **Patching `pdf-xref.c`**: an upstream change is worth offering for the object number, but the
  annotation rules are a larger change in what the function means, and the app needs the right
  answer now (`docs/backlog.md` item 33).
