// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Encryption, permissions and the signatures a rewrite would break (Plan 0009 Phase 2, §4.1).

use mupdf::pdf::{Encryption as MuEncryption, PdfDocument, PdfObject, PdfWriteOptions, Permission};
use pdf_core::secure::{Permissions, Protection, ProtectionError, Security};

/// Field trees deeper than this, or with more fields, are taken as broken (a loop).
const FIELD_DEPTH: u32 = 32;
const FIELD_LIMIT: usize = 100_000;

/// What the open document allows, as opened (every permission with its permissions password).
pub(crate) fn permissions(pdf: &PdfDocument) -> Permissions {
    // `permissions()` reads MuPDF's `/P` with `from_bits_truncate` (`patches/mupdf`, §3.3 flaw 1).
    Permissions::from_bits(pdf.permissions().bits())
}

/// The document's protection and signatures.
pub(crate) fn security(pdf: &PdfDocument) -> Security {
    let encrypt = pdf
        .trailer()
        .ok()
        .and_then(|t| t.get_dict("Encrypt").ok().flatten())
        .filter(|e| e.is_dict().unwrap_or(false));
    let (signed, certified) = signatures(pdf);
    Security {
        encrypted: encrypt.is_some(),
        method: encrypt.as_ref().map(method).unwrap_or_default(),
        permissions: permissions(pdf),
        signed,
        certified,
    }
}

/// The encryption in words, from the `/Encrypt` dictionary (ISO 32000-2, 7.6).
fn method(encrypt: &PdfObject) -> String {
    let int = |key: &str| {
        encrypt
            .get_dict(key)
            .ok()
            .flatten()
            .and_then(|v| v.as_int().ok())
    };
    match (int("V").unwrap_or(0), int("Length").unwrap_or(40)) {
        (5, _) => "AES-256".to_owned(),
        (4, _) => {
            // The standard filter's method: AESV2 or RC4 (V2).
            let aes = encrypt
                .get_dict("CF")
                .ok()
                .flatten()
                .and_then(|cf| cf.get_dict("StdCF").ok().flatten())
                .and_then(|std| std.get_dict("CFM").ok().flatten())
                .and_then(|cfm| cfm.as_name().ok())
                .is_some_and(|name| name == b"AESV2");
            if aes { "AES-128" } else { "RC4 128-bit" }.to_owned()
        }
        (1, _) => "RC4 40-bit".to_owned(),
        (_, length) => format!("RC4 {length}-bit"),
    }
}

/// How many signature fields are signed, and the DocMDP level of a certifying signature.
fn signatures(pdf: &PdfDocument) -> (u32, Option<u8>) {
    let Ok(catalog) = pdf.catalog() else {
        return (0, None);
    };
    let mut signed = 0;
    if let Some(fields) = catalog
        .get_dict("AcroForm")
        .ok()
        .flatten()
        .and_then(|form| form.get_dict("Fields").ok().flatten())
    {
        let mut seen = 0;
        count_signed(&fields, false, 0, &mut seen, &mut signed);
    }
    (signed, certification(&catalog))
}

/// Counts signed signature fields under a `/Fields` or `/Kids` array. `/FT` is inherited.
fn count_signed(array: &PdfObject, sig: bool, depth: u32, seen: &mut usize, signed: &mut u32) {
    let Ok(len) = array.len() else {
        return;
    };
    for i in 0..len {
        *seen += 1;
        if depth > FIELD_DEPTH || *seen > FIELD_LIMIT {
            return;
        }
        let Ok(Some(field)) = array.get_array(i as i32) else {
            continue;
        };
        let is_sig = field
            .get_dict("FT")
            .ok()
            .flatten()
            .and_then(|ft| ft.as_name().ok())
            .map_or(sig, |name| name == b"Sig");
        if is_sig
            && field
                .get_dict("V")
                .ok()
                .flatten()
                .is_some_and(|v| v.is_dict().unwrap_or(false))
        {
            *signed += 1;
        }
        if let Ok(Some(kids)) = field.get_dict("Kids") {
            count_signed(&kids, is_sig, depth + 1, seen, signed);
        }
    }
}

/// The `/P` of the certifying signature's DocMDP transform (`/Perms /DocMDP`; 2 when unset).
fn certification(catalog: &PdfObject) -> Option<u8> {
    let sig = catalog.get_dict("Perms").ok()??.get_dict("DocMDP").ok()??;
    let references = sig.get_dict("Reference").ok().flatten();
    let mut level = 2;
    if let Some(references) = references {
        for i in 0..references.len().unwrap_or(0) {
            let Ok(Some(reference)) = references.get_array(i as i32) else {
                continue;
            };
            let docmdp = reference
                .get_dict("TransformMethod")
                .ok()
                .flatten()
                .and_then(|m| m.as_name().ok())
                .is_some_and(|name| name == b"DocMDP");
            if docmdp {
                if let Some(p) = reference
                    .get_dict("TransformParams")
                    .ok()
                    .flatten()
                    .and_then(|params| params.get_dict("P").ok().flatten())
                    .and_then(|p| p.as_int().ok())
                {
                    level = p;
                }
                break;
            }
        }
    }
    Some(level.clamp(1, 3) as u8)
}

/// Sets the write options for a protection; `Keep` leaves MuPDF's default, which keeps the
/// document's encryption.
pub(crate) fn apply(
    options: &mut PdfWriteOptions,
    protection: &Protection,
) -> Result<(), ProtectionError> {
    match protection {
        Protection::Keep => {}
        Protection::Remove => {
            options.set_encryption(MuEncryption::None);
        }
        Protection::Encrypt(encryption) => {
            // Checked again here: mupdf-rs `assert!`s the length (§3.3 flaw 2).
            let encryption = encryption.clone().checked()?;
            options
                .set_encryption(MuEncryption::Aes256)
                .set_user_password(&encryption.user)
                .set_owner_password(&encryption.owner)
                .set_permissions(Permission::from_bits_truncate(
                    encryption.permissions.to_bits(),
                ));
        }
    }
    Ok(())
}
