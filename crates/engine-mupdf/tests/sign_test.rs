// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Row BD of Plan 0009 (Phase 4): a signature made here, with a key file made by OpenSSL
//! (`tests/fixtures/make_p12.py`), verifies in this app; a second signature keeps the first
//! intact; a wrong password or a refused document changes nothing. That another reader accepts
//! it is checked by hand, and by `openssl cms -verify` (`docs/benchmarks.md` §15.6).

use engine_mupdf::MupdfEngine;
use engine_mupdf::signature::{Roots, verify_file};
use pdf_core::PdfEngine as _;
use pdf_core::Rect;
use pdf_core::annot::EditOp;
use pdf_core::signature::{
    CertificateState, ChangedAfter, DigestState, SignError, SignPlace, SignRequest, SignatureInfo,
    Verdict,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

const PASSWORD: &str = "Şifre 1";

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn scratch(name: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("sign_{n}_{name}"));
    let _ = std::fs::remove_file(&path);
    path
}

fn copy(name: &str) -> PathBuf {
    let path = scratch(name);
    std::fs::copy(fixture(name), &path).expect("copy");
    path
}

/// A 200-page document that takes an incremental update: `large_200p.pdf`'s hand-written cross
/// reference makes MuPDF repair it at open, and a signature is refused on a repaired file
/// (checked by `a_repaired_file_is_refused`), so it is written whole once.
fn plain() -> PathBuf {
    let source = copy("large_200p.pdf");
    let path = scratch("plain.pdf");
    let mut engine = open(&source);
    engine.save(Some(&path)).expect("save as");
    path
}

fn roots() -> Roots {
    Roots::from_pem(&std::fs::read(fixture("signer_root.pem")).expect("root"))
}

fn box_at(y: f32) -> Rect {
    Rect {
        x: 72.0,
        y,
        width: 220.0,
        height: 60.0,
    }
}

fn request(key: &str, password: &str, place: SignPlace) -> SignRequest {
    SignRequest {
        key_file: fixture(key),
        password: password.to_owned(),
        reason: "Onaylıyorum".to_owned(),
        location: "İzmir".to_owned(),
        place,
    }
}

fn new_box(page: u32, y: f32) -> SignPlace {
    SignPlace::New {
        page,
        rect: box_at(y),
    }
}

fn open(path: &Path) -> MupdfEngine {
    let mut engine = MupdfEngine::new();
    engine.open_document(path, None).expect("open");
    engine
}

fn all(path: &Path) -> Vec<SignatureInfo> {
    verify_file(path, None, &roots())
}

fn assert_valid(found: &SignatureInfo, signer: &str) {
    assert_eq!(found.digest, DigestState::Intact, "{found:?}");
    assert_eq!(found.certificate, CertificateState::Trusted, "{found:?}");
    assert_eq!(found.verdict(), Verdict::Valid, "{found:?}");
    assert_eq!(found.signer, signer);
}

#[test]
fn a_signature_made_here_verifies_here() {
    for (key, signer) in [
        ("signer_rsa.p12", "Ayşe Gül Yıldız"),
        ("signer_ec.p12", "Elif Eğri"),
        ("signer_p384.p12", "Çağrı Öz"),
    ] {
        let path = plain();
        let mut engine = open(&path);
        let (written, len, page) = engine
            .sign(&request(key, PASSWORD, new_box(3, 100.0)))
            .unwrap_or_else(|e| panic!("{key}: {e}"));
        assert_eq!(written, path);
        assert_eq!(page, 3);
        assert_eq!(len, std::fs::metadata(&path).expect("len").len());
        let found = all(&path);
        assert_eq!(found.len(), 1, "{key}: {found:?}");
        let found = &found[0];
        assert_valid(found, signer);
        assert_eq!(found.changed_after, ChangedAfter::None, "{key}");
        assert_eq!(found.reason, "Onaylıyorum");
        assert_eq!(found.location, "İzmir");
        assert_eq!(found.field, "Signature1");
        assert_eq!(found.page, Some(3));
        let rect = found.rect.expect("rect");
        assert!(
            (rect.x - 72.0).abs() < 0.5 && (rect.y - 100.0).abs() < 0.5,
            "{rect:?}"
        );
        assert!(
            (rect.width - 220.0).abs() < 0.5 && (rect.height - 60.0).abs() < 0.5,
            "{rect:?}"
        );
        // The document is the signed file now, open and as many pages as before.
        assert_eq!(engine.page_count(), 200);
    }
}

#[test]
fn the_signature_is_pades_baseline() {
    let path = plain();
    open(&path)
        .sign(&request("signer_rsa.p12", PASSWORD, new_box(0, 100.0)))
        .expect("sign");
    let data = std::fs::read(&path).expect("read");
    let has = |text: &[u8]| data.windows(text.len()).any(|w| w == text);
    assert!(
        has(b"/SubFilter/ETSI.CAdES.detached"),
        "PAdES names its format"
    );
    assert!(!has(b"adbe.pkcs7.detached"));
    // The signing-certificate-v2 attribute's OID, 1.2.840.113549.1.9.16.2.47, in DER: in the hex
    // of /Contents.
    let hex: Vec<u8> = b"060B2A864886F70D010910022F".to_vec();
    let text = String::from_utf8_lossy(&data).to_uppercase();
    assert!(text.contains(std::str::from_utf8(&hex).expect("ascii")));
}

#[test]
fn legacy_and_empty_password_key_files_sign_too() {
    for (key, password) in [("signer_legacy.p12", "eski"), ("signer_empty.p12", "")] {
        let path = plain();
        open(&path)
            .sign(&request(key, password, new_box(0, 100.0)))
            .unwrap_or_else(|e| panic!("{key}: {e}"));
        let found = all(&path);
        assert_eq!(found.len(), 1, "{key}");
        assert_valid(&found[0], "Ayşe Gül Yıldız");
    }
}

#[test]
fn a_second_signature_keeps_the_first() {
    let path = plain();
    let mut engine = open(&path);
    engine
        .sign(&request("signer_rsa.p12", PASSWORD, new_box(0, 100.0)))
        .expect("first");
    let first = std::fs::read(&path).expect("first file");
    engine
        .sign(&request("signer_ec.p12", PASSWORD, new_box(0, 200.0)))
        .expect("second");
    let second = std::fs::read(&path).expect("second file");
    assert!(
        second.starts_with(&first),
        "the first revision's bytes stay as they were"
    );
    let found = all(&path);
    assert_eq!(found.len(), 2, "{found:?}");
    let by = |name: &str| found.iter().find(|f| f.field == name).expect(name);
    assert_valid(by("Signature1"), "Ayşe Gül Yıldız");
    assert_eq!(by("Signature1").changed_after, ChangedAfter::Allowed);
    assert_valid(by("Signature2"), "Elif Eğri");
    assert_eq!(by("Signature2").changed_after, ChangedAfter::None);
}

fn note() -> EditOp {
    EditOp::AddNote {
        page: 0,
        rect: Rect {
            x: 300.0,
            y: 300.0,
            width: 20.0,
            height: 20.0,
        },
        contents: "later".to_owned(),
        colour: pdf_core::annot::PALETTE[0],
        author: None,
    }
}

#[test]
fn a_save_after_signing_keeps_the_signature() {
    let path = plain();
    let mut engine = open(&path);
    engine
        .sign(&request("signer_rsa.p12", PASSWORD, new_box(0, 100.0)))
        .expect("sign");
    // Two saves in place: the second rewrites the section after the signed bytes, never them.
    for _ in 0..2 {
        engine.edit(&note()).expect("note");
        engine.save(None).expect("save");
    }
    let found = all(&path);
    assert_eq!(found.len(), 1);
    assert_valid(&found[0], "Ayşe Gül Yıldız");
    assert_eq!(found[0].changed_after, ChangedAfter::Allowed);
}

#[test]
fn unsaved_changes_are_saved_before_signing() {
    let path = plain();
    let mut engine = open(&path);
    engine.edit(&note()).expect("note");
    engine
        .sign(&request("signer_rsa.p12", PASSWORD, new_box(0, 100.0)))
        .expect("sign");
    let found = all(&path);
    assert_eq!(
        found[0].changed_after,
        ChangedAfter::None,
        "the note is under the signature"
    );
    let reopened = open(&path);
    assert_eq!(reopened.annotations(0).expect("annotations").len(), 1);
}

#[test]
fn a_wrong_password_changes_nothing() {
    let path = plain();
    let before = std::fs::read(&path).expect("before");
    let mut engine = open(&path);
    engine.edit(&note()).expect("note");
    let error = engine
        .sign(&request("signer_rsa.p12", "wrong", new_box(0, 100.0)))
        .expect_err("refused");
    assert_eq!(error, SignError::WrongPassword);
    assert_eq!(
        std::fs::read(&path).expect("after"),
        before,
        "not even the note was saved"
    );
    assert!(engine.save(None).is_ok(), "the note is still there to save");
    let error = engine
        .sign(&request("signer_legacy.p12", "wrong", new_box(0, 100.0)))
        .expect_err("refused");
    assert_eq!(error, SignError::WrongPassword);
}

#[test]
fn a_file_that_is_no_key_file_is_refused() {
    let path = plain();
    let mut engine = open(&path);
    let mut bad = request("signer_rsa.p12", PASSWORD, new_box(0, 100.0));
    bad.key_file = fixture("minimal.pdf");
    assert!(matches!(engine.sign(&bad), Err(SignError::KeyFile(_))));
    bad.key_file = fixture("no_such_file.p12");
    assert!(matches!(engine.sign(&bad), Err(SignError::KeyFile(_))));
}

#[test]
fn a_tiny_box_or_a_missing_page_is_refused_and_the_file_stays() {
    let path = plain();
    let before = std::fs::read(&path).expect("before");
    let mut engine = open(&path);
    let tiny = SignPlace::New {
        page: 0,
        rect: Rect {
            x: 10.0,
            y: 10.0,
            width: 5.0,
            height: 5.0,
        },
    };
    assert!(matches!(
        engine.sign(&request("signer_rsa.p12", PASSWORD, tiny)),
        Err(SignError::Field(_))
    ));
    assert!(matches!(
        engine.sign(&request("signer_rsa.p12", PASSWORD, new_box(500, 100.0))),
        Err(SignError::Field(_))
    ));
    assert_eq!(std::fs::read(&path).expect("after"), before);
    assert!(all(&path).is_empty());
}

/// A copy of `large_200p.pdf` with an empty signature field `Approval` on page 1.
fn with_empty_field() -> PathBuf {
    let path = plain();
    let doc = mupdf::pdf::PdfDocument::open(path.to_str().expect("utf-8")).expect("open");
    let mut page = doc.load_pdf_page(1).expect("page");
    let mut widget = page.add_signature_widget("Approval").expect("field");
    widget
        .set_rect(mupdf::Rect::new(100.0, 400.0, 320.0, 460.0))
        .expect("rect");
    drop(widget);
    drop(page);
    let mut options = mupdf::pdf::PdfWriteOptions::default();
    options.set_incremental(true);
    doc.save_with_options(path.to_str().expect("utf-8"), options)
        .expect("save");
    path
}

#[test]
fn an_empty_field_is_signed_where_it_is() {
    let path = with_empty_field();
    let mut engine = open(&path);
    let place = SignPlace::Field {
        page: 1,
        name: "Approval".to_owned(),
    };
    engine
        .sign(&request("signer_rsa.p12", PASSWORD, place.clone()))
        .expect("sign");
    let found = all(&path);
    assert_eq!(found.len(), 1);
    assert_valid(&found[0], "Ayşe Gül Yıldız");
    assert_eq!(found[0].field, "Approval");
    assert_eq!(found[0].page, Some(1));
    // Signed now: a second try is refused.
    assert!(matches!(
        engine.sign(&request("signer_ec.p12", PASSWORD, place)),
        Err(SignError::Field(_))
    ));
}

#[test]
fn a_document_signed_by_another_tool_takes_a_second_signature() {
    let path = copy("signed_rsa.pdf");
    open(&path)
        .sign(&request("signer_ec.p12", PASSWORD, new_box(0, 300.0)))
        .expect("sign");
    let mut both = verify_file(&path, None, &roots());
    both.sort_by(|a, b| a.field.cmp(&b.field));
    assert_eq!(both.len(), 2, "{both:?}");
    // OpenSSL's: intact, its own root not loaded here, and the change after it allowed.
    assert_eq!(both[0].digest, DigestState::Intact);
    assert_eq!(both[0].changed_after, ChangedAfter::Allowed);
    assert_eq!(both[0].signer, "Şule Çağlar Öztürk");
    assert_valid(&both[1], "Elif Eğri");
}

/// A one-page PDF with exact cross-reference offsets, an `/Info` without `/Producer`.
fn foreign_pdf() -> Vec<u8> {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] >>",
        "<< /Title (Elsewhere) >>",
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{body}\nendobj\n", i + 1).bytes());
    }
    let xref = out.len();
    let size = objects.len() + 1;
    out.extend(format!("xref\n0 {size}\n0000000000 65535 f \n").bytes());
    for offset in offsets {
        out.extend(format!("{offset:010} 00000 n \n").bytes());
    }
    out.extend(
        format!("trailer\n<< /Size {size} /Root 1 0 R /Info 4 0 R >>\nstartxref\n{xref}\n%%EOF\n")
            .bytes(),
    );
    out
}

/// The file has no `/Producer`: the stamp has to land inside the signed revision, not in a
/// section after it.
#[test]
fn a_file_without_our_producer_is_signed_whole() {
    let path = scratch("no_producer.pdf");
    std::fs::write(&path, foreign_pdf()).expect("write");
    let mut engine = open(&path);
    engine
        .sign(&request("signer_ec.p12", PASSWORD, new_box(0, 100.0)))
        .expect("sign");
    let found = all(&path);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_valid(&found[0], "Elif Eğri");
    assert_eq!(found[0].changed_after, ChangedAfter::None);
}

#[test]
fn a_repaired_file_is_refused() {
    let path = copy("large_200p.pdf");
    let before = std::fs::read(&path).expect("before");
    let error = open(&path)
        .sign(&request("signer_rsa.p12", PASSWORD, new_box(0, 100.0)))
        .expect_err("refused");
    assert_eq!(error, SignError::NeedsSaveAs);
    assert_eq!(std::fs::read(&path).expect("after"), before);
}

#[test]
fn a_certified_document_that_allows_no_change_is_not_signed() {
    let path = copy("signed_docmdp1.pdf");
    let before = std::fs::read(&path).expect("before");
    let error = open(&path)
        .sign(&request("signer_rsa.p12", PASSWORD, new_box(0, 300.0)))
        .expect_err("refused");
    assert_eq!(error, SignError::Certified);
    assert_eq!(std::fs::read(&path).expect("after"), before);
}

#[test]
fn a_key_file_with_a_damaged_mac_is_refused() {
    let mut bytes = std::fs::read(fixture("signer_rsa.p12")).expect("p12");
    assert!(engine_mupdf::p12::read(&bytes, PASSWORD).is_ok());
    // The MAC's digest is the last OCTET STRING of 32 bytes before the salt and the iterations,
    // near the end of the file: change one byte of it.
    let at = bytes
        .windows(2)
        .rposition(|w| w == [0x04, 0x20])
        .expect("the MAC digest");
    bytes[at + 2] ^= 1;
    assert_eq!(
        engine_mupdf::p12::read(&bytes, PASSWORD).map(|_| ()),
        Err(engine_mupdf::p12::P12Error::WrongPassword)
    );
}

#[test]
fn the_key_is_matched_to_its_own_certificate() {
    // The file holds the leaf and the intermediate; the identity is the leaf, by its public key.
    for key in [
        "signer_rsa.p12",
        "signer_legacy.p12",
        "signer_ec.p12",
        "signer_p384.p12",
    ] {
        let password = if key == "signer_legacy.p12" {
            "eski"
        } else {
            PASSWORD
        };
        let identity = engine_mupdf::p12::read(&std::fs::read(fixture(key)).expect(key), password)
            .unwrap_or_else(|e| panic!("{key}: {e}"));
        assert_eq!(identity.others.len(), 1, "{key}");
        assert_ne!(
            identity.certificate.tbs_certificate.subject,
            identity.others[0].tbs_certificate.subject,
            "{key}"
        );
        assert_eq!(
            identity.certificate.tbs_certificate.issuer,
            identity.others[0].tbs_certificate.subject
        );
    }
}

/// `t_key`, `t_sign` and the file's growth (`docs/benchmarks.md` §15.6). Release build:
/// `cargo test --release -p engine-mupdf --test sign_test signing_timings -- --ignored --nocapture`
#[test]
#[ignore = "a measurement"]
fn signing_timings() {
    for (key, password) in [
        ("signer_rsa.p12", PASSWORD),
        ("signer_legacy.p12", "eski"),
        ("signer_ec.p12", PASSWORD),
        ("signer_p384.p12", PASSWORD),
    ] {
        for _ in 0..3 {
            let started = std::time::Instant::now();
            engine_mupdf::signer_name(&fixture(key), password).expect("key");
            let t_key = started.elapsed();
            let path = plain();
            let size = std::fs::metadata(&path).expect("size").len();
            let mut engine = open(&path);
            let started = std::time::Instant::now();
            let (_, len, _) = engine
                .sign(&request(key, password, new_box(0, 100.0)))
                .expect("sign");
            let t_sign = started.elapsed();
            println!(
                "{key}: t_key {:.1} ms, t_sign {:.1} ms, +{} bytes",
                t_key.as_secs_f64() * 1e3,
                t_sign.as_secs_f64() * 1e3,
                len - size
            );
        }
    }
}
