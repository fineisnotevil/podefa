// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Row BC of Plan 0009 (Phase 3): signatures made by another tool (OpenSSL's `cms`, in
//! `tests/fixtures/make_signed.py`) read and verified: signer, time, an intact digest; one byte
//! changed in the signed range; changes after signing, allowed and not.

use engine_mupdf::signature::{Roots, verify_file};
use engine_mupdf::{EngineHandle, MupdfEngine};
use pdf_core::Rect;
use pdf_core::annot::EditOp;
use pdf_core::pages::PageOp;
use pdf_core::signature::{CertificateState, ChangedAfter, DigestState, SignatureInfo, Verdict};
use pdf_core::{EngineCmd, EngineEvent, PdfEngine as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn scratch(name: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("signature_{n}_{name}"));
    let _ = std::fs::remove_file(&path);
    path
}

/// A copy of a fixture to change.
fn copy(name: &str) -> PathBuf {
    let path = scratch(name);
    std::fs::copy(fixture(name), &path).expect("copy");
    path
}

fn roots() -> Roots {
    Roots::from_pem(&std::fs::read(fixture("signed_root.pem")).expect("root"))
}

fn one(path: &Path) -> SignatureInfo {
    let mut found = verify_file(path, None, &roots());
    assert_eq!(found.len(), 1, "{found:?}");
    found.remove(0)
}

#[test]
fn a_signature_from_another_tool_is_read_and_valid() {
    for name in ["signed_rsa.pdf", "signed_pss.pdf"] {
        let found = one(&fixture(name));
        assert_eq!(found.digest, DigestState::Intact, "{name}");
        assert_eq!(found.certificate, CertificateState::Trusted, "{name}");
        assert_eq!(found.changed_after, ChangedAfter::None, "{name}");
        assert_eq!(found.verdict(), Verdict::Valid, "{name}");
        // The certificate's name, not the one the dictionary claims.
        assert_eq!(found.signer, "Şule Çağlar Öztürk");
        assert_eq!(found.time, "2030-01-01 12:00 +03:00");
        assert_eq!(found.reason, "Approved");
        assert_eq!(found.location, "Ankara");
        assert_eq!(found.field, "Signature1");
        assert_eq!(found.page, Some(0));
        let rect = found.rect.expect("rect");
        // /Rect [72 600 272 660] on a Letter page, y down.
        assert!((rect.x - 72.0).abs() < 0.01, "{rect:?}");
        assert!((rect.y - 132.0).abs() < 0.01, "{rect:?}");
        assert!((rect.width - 200.0).abs() < 0.01, "{rect:?}");
        assert!((rect.height - 60.0).abs() < 0.01, "{rect:?}");
    }
}

#[test]
fn an_elliptic_curve_signature_is_valid() {
    let found = one(&fixture("signed_ec.pdf"));
    assert_eq!(found.digest, DigestState::Intact);
    assert_eq!(found.certificate, CertificateState::Trusted);
    assert_eq!(found.signer, "Elif Eğri");
}

/// The offset of `text` in a file.
fn offset(data: &[u8], text: &[u8]) -> usize {
    data.windows(text.len())
        .position(|w| w == text)
        .expect("in the file")
}

#[test]
fn one_byte_changed_in_the_signed_range_is_broken() {
    let path = copy("signed_rsa.pdf");
    let mut data = std::fs::read(&path).expect("read");
    // The page's word, in its uncompressed stream: same length, same structure.
    let at = offset(&data, b"SIGNED-WORDS");
    data[at] = b'Z';
    std::fs::write(&path, &data).expect("write");
    let found = one(&path);
    assert!(matches!(found.digest, DigestState::Broken(_)), "{found:?}");
    assert_eq!(found.verdict(), Verdict::Invalid);
}

#[test]
fn a_byte_changed_in_the_signature_itself_is_broken() {
    // Far enough into the blob to be the signature value at the end, not its structure.
    let path = copy("signed_rsa.pdf");
    let mut data = std::fs::read(&path).expect("read");
    let start = offset(&data, b"/Contents <") + b"/Contents <".len();
    // The last non-zero hex digit is the end of the signature value.
    let end = start + data[start..].iter().position(|&b| b == b'>').expect("end");
    let last = data[start..end]
        .iter()
        .rposition(|&b| b != b'0')
        .expect("digit")
        + start;
    data[last] = if data[last] == b'1' { b'2' } else { b'1' };
    std::fs::write(&path, &data).expect("write");
    let found = one(&path);
    assert!(
        matches!(found.digest, DigestState::Broken(_)),
        "{:?}",
        found.digest
    );
}

#[test]
fn a_range_that_leaves_out_more_than_the_signature_is_broken() {
    // A range moved so that /M and what follows are no longer signed: the classic trick.
    let path = copy("signed_rsa.pdf");
    let mut data = std::fs::read(&path).expect("read");
    let at = offset(&data, b"/ByteRange [") + b"/ByteRange [".len();
    let text = String::from_utf8_lossy(&data[at..at + 43]).into_owned();
    let values: Vec<u64> = text
        .split_whitespace()
        .map(|v| v.parse().expect("int"))
        .collect();
    let forged = format!(
        "{:010} {:010} {:010} {:010}",
        values[0],
        values[1],
        values[2] + 10,
        values[3] - 10
    );
    data[at..at + 43].copy_from_slice(forged.as_bytes());
    std::fs::write(&path, &data).expect("write");
    let found = one(&path);
    assert!(
        matches!(found.digest, DigestState::Broken(_)),
        "{:?}",
        found.digest
    );
}

#[test]
fn a_range_past_the_end_is_broken() {
    let path = copy("signed_rsa.pdf");
    let data = std::fs::read(&path).expect("read");
    std::fs::write(&path, &data[..data.len() - 100]).expect("write");
    let found = one(&path);
    assert!(
        matches!(found.digest, DigestState::Broken(_)),
        "{:?}",
        found.digest
    );
}

#[test]
fn a_self_signed_certificate_is_intact_but_not_trusted() {
    let found = one(&fixture("signed_selfsigned.pdf"));
    assert_eq!(found.digest, DigestState::Intact);
    assert_eq!(
        found.certificate,
        CertificateState::Untrusted("it is self-signed".to_owned())
    );
    assert_eq!(found.verdict(), Verdict::Unsure);
    assert_eq!(found.signer, "Nobody Knows Me");
}

#[test]
fn a_chain_of_other_keys_under_the_same_names_is_not_trusted() {
    let found = one(&fixture("signed_forged.pdf"));
    assert_eq!(found.digest, DigestState::Intact);
    assert_eq!(
        found.certificate,
        CertificateState::Untrusted("it does not lead to a root this computer trusts".to_owned())
    );
}

#[test]
fn a_real_intermediate_that_did_not_issue_the_leaf_is_not_trusted() {
    let found = one(&fixture("signed_forged_mid.pdf"));
    assert_eq!(found.digest, DigestState::Intact);
    assert!(matches!(found.certificate, CertificateState::Untrusted(_)));
}

#[test]
fn a_signature_over_a_range_with_more_left_out_is_broken() {
    // The digest and the signature are right for that range: only the gap gives it away.
    let found = one(&fixture("signed_gap.pdf"));
    assert_eq!(
        found.digest,
        DigestState::Broken("its byte range leaves out more than the signature".to_owned())
    );
}

#[test]
fn a_certificate_not_yet_valid_when_signing_is_not_trusted() {
    let found = one(&fixture("signed_early.pdf"));
    assert_eq!(found.digest, DigestState::Intact);
    assert_eq!(
        found.certificate,
        CertificateState::Untrusted("it was not valid when the document was signed".to_owned())
    );
}

#[test]
fn without_the_root_the_chain_is_not_trusted() {
    let found = verify_file(&fixture("signed_rsa.pdf"), None, &Roots::default());
    assert_eq!(found[0].digest, DigestState::Intact);
    assert_eq!(
        found[0].certificate,
        CertificateState::Untrusted("it does not lead to a root this computer trusts".to_owned())
    );
}

fn note() -> EditOp {
    EditOp::AddNote {
        page: 0,
        rect: Rect {
            x: 100.0,
            y: 100.0,
            width: 20.0,
            height: 20.0,
        },
        contents: "after signing".to_owned(),
        colour: pdf_core::annot::PALETTE[0],
        author: None,
    }
}

fn annotate(path: &Path) {
    let mut engine = MupdfEngine::new();
    engine.open_document(path, None).expect("open");
    engine.edit(&note()).expect("note");
    engine.save(None).expect("save in place");
}

#[test]
fn a_note_after_a_plain_signature_is_an_allowed_change() {
    let path = copy("signed_rsa.pdf");
    annotate(&path);
    let found = one(&path);
    assert_eq!(found.digest, DigestState::Intact, "the signed bytes stay");
    assert_eq!(found.changed_after, ChangedAfter::Allowed);
    assert_eq!(found.verdict(), Verdict::Valid);
}

#[test]
fn a_note_after_docmdp_1_is_a_disallowed_change() {
    let path = copy("signed_docmdp1.pdf");
    let before = one(&path);
    assert_eq!(before.verdict(), Verdict::Valid);
    annotate(&path);
    let found = one(&path);
    assert_eq!(found.digest, DigestState::Intact, "the signed bytes stay");
    assert_eq!(found.changed_after, ChangedAfter::Disallowed);
    assert_eq!(found.verdict(), Verdict::Invalid);
}

#[test]
fn a_note_after_docmdp_3_is_allowed() {
    let path = copy("signed_docmdp3.pdf");
    annotate(&path);
    let found = one(&path);
    assert_eq!(found.changed_after, ChangedAfter::Allowed);
}

/// Saves `op` in place in a copy of `name`, and says what the signature makes of it.
fn after(name: &str, op: &EditOp) -> SignatureInfo {
    let path = copy(name);
    let mut engine = MupdfEngine::new();
    engine.open_document(&path, None).expect("open");
    engine.edit(op).expect("edit");
    engine.save(None).expect("save in place");
    drop(engine);
    one(&path)
}

fn rotate() -> EditOp {
    EditOp::Pages(PageOp::Rotate {
        pages: vec![0],
        quarter_turns: 1,
    })
}

fn fill(name: &str) -> EditOp {
    EditOp::SetField {
        page: 0,
        name: name.to_owned(),
        value: "second".to_owned(),
    }
}

#[test]
fn level_3_allows_annotations_but_not_a_turned_page() {
    let found = after("signed_docmdp3.pdf", &rotate());
    assert_eq!(found.changed_after, ChangedAfter::Disallowed);
    assert_eq!(found.verdict(), Verdict::Invalid);
}

#[test]
fn level_1_refuses_a_turned_page_too() {
    let found = after("signed_docmdp1.pdf", &rotate());
    assert_eq!(found.changed_after, ChangedAfter::Disallowed);
}

#[test]
fn a_plain_signature_allows_whatever_comes_after() {
    // Nothing locked: it speaks only for its own version, which stays intact.
    let found = after("signed_rsa.pdf", &rotate());
    assert_eq!(found.digest, DigestState::Intact);
    assert_eq!(found.changed_after, ChangedAfter::Allowed);
}

#[test]
fn field_locks_are_kept_apart_from_annotations() {
    let name = "signed_fieldmdp.pdf";
    assert_eq!(one(&fixture(name)).verdict(), Verdict::Valid);
    assert_eq!(after(name, &note()).changed_after, ChangedAfter::Allowed);
    assert_eq!(
        after(name, &fill("Free")).changed_after,
        ChangedAfter::Allowed
    );
    assert_eq!(
        after(name, &fill("Locked")).changed_after,
        ChangedAfter::Disallowed
    );
    assert_eq!(
        after(name, &rotate()).changed_after,
        ChangedAfter::Disallowed
    );
}

/// Appends to the file a new version of object `num`, as an incremental save by another writer.
fn append_object(path: &Path, num: u32, body: &[u8]) {
    let mut data = std::fs::read(path).expect("read");
    let tail = offset(&data, b"startxref") + b"startxref".len();
    let previous: u64 = String::from_utf8_lossy(&data[tail..])
        .split_whitespace()
        .next()
        .expect("offset")
        .parse()
        .expect("number");
    let at = data.len();
    data.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
    data.extend_from_slice(body);
    data.extend_from_slice(b"\nendobj\n");
    let xref = data.len();
    data.extend_from_slice(
        format!(
            "xref\n0 1\n0000000000 65535 f \n{num} 1\n{at:010} 00000 n \n\
             trailer\n<< /Size 9 /Root 1 0 R /Prev {previous} >>\nstartxref\n{xref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    std::fs::write(path, data).expect("write");
}

#[test]
fn new_page_contents_after_level_3_are_forbidden() {
    let path = copy("signed_docmdp3.pdf");
    let words = b"BT /F1 24 Tf 72 700 Td (OTHER-WORDS) Tj ET";
    let mut body = format!("<< /Length {} >>\nstream\n", words.len()).into_bytes();
    body.extend_from_slice(words);
    body.extend_from_slice(b"\nendstream");
    append_object(&path, 4, &body);
    let found = one(&path);
    assert_eq!(found.digest, DigestState::Intact, "the signed bytes stay");
    assert_eq!(found.changed_after, ChangedAfter::Disallowed);
}

#[test]
fn a_changed_signature_value_after_level_3_is_forbidden() {
    let path = copy("signed_docmdp3.pdf");
    // The widget itself, moved off the page: signed fields stay as they are.
    append_object(
        &path,
        5,
        b"<< /Type /Annot /Subtype /Widget /FT /Sig /T (Signature1) /F 4 /Rect [0 0 0 0] \
          /P 3 0 R /V 6 0 R >>",
    );
    assert_eq!(one(&path).changed_after, ChangedAfter::Disallowed);
}

#[test]
fn an_appearance_the_page_uses_is_not_free_under_level_3() {
    let path = copy("signed_shared.pdf");
    let drawing = b"0 0 90 40 re S";
    let mut body = format!(
        "<< /Type /XObject /Subtype /Form /BBox [0 0 100 50] /Length {} >>\nstream\n",
        drawing.len()
    )
    .into_bytes();
    body.extend_from_slice(drawing);
    body.extend_from_slice(b"\nendstream");
    append_object(&path, 8, &body);
    assert_eq!(one(&path).changed_after, ChangedAfter::Disallowed);
}

#[test]
fn another_annotation_is_free_under_level_3() {
    let path = copy("signed_shared.pdf");
    append_object(
        &path,
        7,
        b"<< /Type /Annot /Subtype /Square /F 4 /Rect [72 300 172 350] /P 3 0 R /AP << /N 8 0 R >> >>",
    );
    assert_eq!(one(&path).changed_after, ChangedAfter::Allowed);
}

#[test]
fn a_moved_signature_after_level_1_is_forbidden() {
    // MuPDF's own check passes this one.
    let path = copy("signed_docmdp1.pdf");
    append_object(
        &path,
        5,
        b"<< /Type /Annot /Subtype /Widget /FT /Sig /T (Signature1) /F 4 /Rect [0 0 0 0]           /P 3 0 R /V 6 0 R >>",
    );
    assert_eq!(one(&path).changed_after, ChangedAfter::Disallowed);
}

#[test]
fn an_unsigned_document_has_nothing_to_say() {
    assert!(verify_file(&fixture("minimal.pdf"), None, &roots()).is_empty());
}

#[test]
fn the_engine_reports_a_signed_document_when_opened_and_after_a_save() {
    let path = copy("signed_rsa.pdf");
    let (handle, rx) = EngineHandle::spawn();
    handle
        .send(EngineCmd::Open {
            path: path.clone(),
            password: None,
        })
        .expect("alive");
    let wait_for = || loop {
        match rx.recv_timeout(Duration::from_secs(30)).expect("event") {
            EngineEvent::Signatures { path, signatures } => return (path, signatures),
            _ => continue,
        }
    };
    let (from, signatures) = wait_for();
    assert_eq!(from, path);
    assert_eq!(signatures.len(), 1);
    assert_eq!(signatures[0].changed_after, ChangedAfter::None);
    // The test's root is not this computer's, so the state is what the system's roots say.
    assert!(matches!(
        signatures[0].certificate,
        CertificateState::Untrusted(_)
    ));

    handle
        .send(EngineCmd::Edit {
            op: note(),
            edit_id: 1,
            generation: 0,
        })
        .expect("alive");
    handle.send(EngineCmd::Save { path: None }).expect("alive");
    let (_, signatures) = wait_for();
    assert_eq!(signatures[0].changed_after, ChangedAfter::Allowed);
}

#[test]
fn a_document_without_signatures_reports_none() {
    let (handle, rx) = EngineHandle::spawn();
    handle
        .send(EngineCmd::Open {
            path: fixture("minimal.pdf"),
            password: None,
        })
        .expect("alive");
    loop {
        match rx.recv_timeout(Duration::from_secs(30)).expect("event") {
            EngineEvent::Opened { .. } => break,
            EngineEvent::Signatures { .. } => panic!("no signatures to report"),
            _ => {}
        }
    }
    assert!(
        rx.recv_timeout(Duration::from_millis(300))
            .iter()
            .all(|event| !matches!(event, EngineEvent::Signatures { .. }))
    );
}

/// `docs/benchmarks.md` §15.5: `cargo test --release -p engine-mupdf --test signature_test
/// signature_timings -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, not a check"]
fn signature_timings() {
    let start = std::time::Instant::now();
    let system = Roots::system();
    println!(
        "Roots::system: {:?}, {} roots",
        start.elapsed(),
        system.len()
    );
    for name in ["signed_rsa.pdf", "signed_pss.pdf", "signed_ec.pdf"] {
        for _ in 0..3 {
            let start = std::time::Instant::now();
            let found = verify_file(&fixture(name), None, &system);
            println!("{name}: {:?} ({} signature)", start.elapsed(), found.len());
        }
    }
}
