// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Encryption and permissions (Plan 0009 Phase 2, rows BA and BB): AES-256 with Turkish
//! passwords, changing and removing them, a print-only document read as print-only and followed,
//! the permissions password unlocking it, and the signatures a rewrite would break.

use engine_mupdf::{EngineHandle, MupdfEngine, MupdfError};
use mupdf::TextPageFlags;
use mupdf::pdf::{Encryption, PdfDocument, PdfWriteOptions, Permission};
use pdf_core::annot::{Colour, EditOp};
use pdf_core::secure::{self, Need, Permissions, Protection, Security};
use pdf_core::tools::{MergeInput, Preset, ToolJob};
use pdf_core::{EngineCmd, EngineEvent, PdfEngine as _, Rect, SaveError};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::time::Duration;

const USER: &str = "açık-ğüşiöç";
const OWNER: &str = "Sahip-İı-ŞĞ";

fn scratch(name: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("secure_{n}_{name}"));
    // A file of an earlier run must not pass for one this run wrote.
    let _ = std::fs::remove_file(&path);
    path
}

/// A PDF of these objects (1-based, object 1 the catalog) with its cross-reference table.
fn pdf(objects: &[String]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /ID [<0123456789abcdef0123456789abcdef> <0123456789abcdef0123456789abcdef>] >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// One page with the line `PLAIN-WORDS`; with `signature`, a signed field and, with `level`, a
/// certifying signature of that DocMDP level.
fn plain(signature: bool, level: Option<u8>) -> Vec<u8> {
    let content = "BT /F1 18 Tf 72 700 Td (PLAIN-WORDS) Tj ET";
    let mut catalog = "<< /Type /Catalog /Pages 2 0 R".to_owned();
    if signature {
        catalog.push_str(" /AcroForm << /Fields [5 0 R] /SigFlags 3 >>");
        if level.is_some() {
            catalog.push_str(" /Perms << /DocMDP 6 0 R >>");
        }
    }
    catalog.push_str(" >>");
    let annots = if signature { " /Annots [5 0 R]" } else { "" };
    let reference = match level {
        Some(p) => format!(
            " /Reference [<< /Type /SigRef /TransformMethod /DocMDP /TransformParams << /Type /TransformParams /P {p} /V /1.2 >> >>]"
        ),
        None => String::new(),
    };
    pdf(&[
        catalog,
        "<< /Type /Pages /Kids [4 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 7 0 R /Resources << /Font << /F1 3 0 R >> >>{annots} >>"
        ),
        "<< /FT /Sig /T (Signature1) /V 6 0 R /Type /Annot /Subtype /Widget /Rect [0 0 0 0] /P 4 0 R /F 132 >>".to_owned(),
        format!(
            "<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange [0 0 0 0] /Contents <00>{reference} >>"
        ),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
    ])
}

/// The plain document written with AES-256 by MuPDF, as another tool would protect it.
fn protected(user: &str, owner: &str, permissions: Permission) -> PathBuf {
    let source = scratch("plain.pdf");
    std::fs::write(&source, plain(false, None)).expect("fixture");
    let doc = PdfDocument::open(source.to_str().expect("utf-8")).expect("open");
    let mut options = PdfWriteOptions::default();
    options
        .set_encryption(Encryption::Aes256)
        .set_user_password(user)
        .set_owner_password(owner)
        .set_permissions(permissions);
    let out = scratch("protected.pdf");
    doc.save_with_options(out.to_str().expect("utf-8"), options)
        .expect("protect");
    out
}

fn print_only() -> PathBuf {
    protected("", OWNER, Permission::PRINT | Permission::PRINT_HQ)
}

fn open(path: &Path, password: Option<&str>) -> MupdfEngine {
    let mut engine = MupdfEngine::new();
    engine
        .open_document_with_password(path, password)
        .expect("open");
    engine
}

/// Whether the open document's page draws its line: a document that lost its keys draws a
/// blank page.
fn inked(engine: &MupdfEngine) -> bool {
    let bitmap = engine.render_page(0, 1.0).expect("render");
    bitmap.data.iter().filter(|&&b| b < 100).count() > 50
}

fn note() -> EditOp {
    EditOp::AddNote {
        page: 0,
        rect: Rect::new(300.0, 300.0, 24.0, 24.0),
        contents: "a note".to_owned(),
        colour: Colour {
            r: 1.0,
            g: 0.8,
            b: 0.2,
        },
        author: None,
    }
}

fn text(path: &Path, password: Option<&str>) -> String {
    let mut doc = PdfDocument::open(path.to_str().expect("utf-8")).expect("open");
    if let Some(password) = password {
        assert!(doc.authenticate(password).expect("authenticate"));
    }
    doc.load_page(0)
        .expect("page")
        .to_text_page(TextPageFlags::empty())
        .expect("text")
        .to_text()
        .expect("text")
}

fn encryption(user: &str, owner: &str, permissions: Permissions) -> Protection {
    Protection::Encrypt(secure::Encryption {
        user: user.to_owned(),
        owner: owner.to_owned(),
        permissions,
    })
}

#[test]
fn a_print_only_document_reports_print_only() {
    // Row BB. mupdf-rs read `/P` with `from_bits`, which refuses the reserved bits a real `/P`
    // has, and reported every permission (§3.3 flaw 1; `patches/mupdf` reads it truncated).
    let engine = open(&print_only(), None);
    let security = engine.security();
    assert!(security.encrypted);
    assert_eq!(security.method, "AES-256");
    let p = security.permissions;
    assert!(p.print && p.print_hq, "{p:?}");
    assert!(
        !p.copy && !p.modify && !p.annotate && !p.fill && !p.assemble,
        "{p:?}"
    );
    // A document without encryption allows everything.
    let path = scratch("open.pdf");
    std::fs::write(&path, plain(false, None)).expect("fixture");
    assert_eq!(open(&path, None).security(), Security::default());
}

#[test]
fn the_permissions_are_followed_until_unlocked() {
    let mut engine = open(&print_only(), None);
    match engine.edit(&note()) {
        Err(MupdfError::NotPermitted(Need::Annotate)) => {}
        other => panic!("an annotation on a print-only document: {other:?}"),
    }
    assert_eq!(engine.annotations(0).expect("annotations").len(), 0);
    // A wrong password unlocks nothing, and the document still reads (MuPDF dropped its keys at
    // the failed try; the engine authenticates again).
    assert!(!engine.unlock("yanlış").expect("unlock"));
    assert!(!engine.security().permissions.copy);
    assert!(inked(&engine));
    // The password to open (empty here) is not the permissions password.
    assert!(!engine.unlock("").expect("unlock"));
    assert!(engine.unlock(OWNER).expect("unlock"));
    assert!(engine.security().permissions.is_all());
    engine.edit(&note()).expect("annotate once unlocked");
    assert!(inked(&engine));
}

#[test]
fn aes_256_with_turkish_passwords() {
    // Row BA: the protected file asks for its password, refuses a wrong one, opens with the
    // password to open (print only) and with the permissions password (everything).
    let source = scratch("source.pdf");
    std::fs::write(&source, plain(false, None)).expect("fixture");
    let mut engine = open(&source, None);
    let target = scratch("aes.pdf");
    let print = Permissions {
        print: true,
        print_hq: true,
        ..Permissions::from_bits(0)
    };
    engine
        .protect(Some(&target), &encryption(USER, OWNER, print))
        .expect("protect");
    // The engine switched to the copy, opened with the permissions password.
    assert!(engine.security().permissions.is_all());
    assert!(engine.security().encrypted);
    // The text is encrypted in the file.
    let raw = std::fs::read(&target).expect("read");
    assert!(!raw.windows(11).any(|w| w == b"PLAIN-WORDS"));
    let mut other = MupdfEngine::new();
    assert!(matches!(
        other.open_document_with_password(&target, None),
        Err(MupdfError::PasswordRequired)
    ));
    assert!(matches!(
        other.open_document_with_password(&target, Some("acik-gusioc")),
        Err(MupdfError::InvalidPassword)
    ));
    let mut user = open(&target, Some(USER));
    let security = user.security();
    assert_eq!(security.method, "AES-256");
    assert!(security.permissions.print && !security.permissions.copy);
    // The password to open is right, but it unlocks no permission.
    assert!(!user.unlock(USER).expect("unlock"));
    assert!(!user.security().permissions.copy);
    assert!(open(&target, Some(OWNER)).security().permissions.is_all());
    assert!(text(&target, Some(USER)).contains("PLAIN-WORDS"));
    // The original stays as it was.
    assert!(!open(&source, None).security().encrypted);
}

#[test]
fn change_and_remove_the_password_in_place() {
    let source = scratch("change.pdf");
    std::fs::write(&source, plain(false, None)).expect("fixture");
    let mut engine = open(&source, None);
    engine
        .protect(None, &encryption(USER, "", Permissions::ALL))
        .expect("protect in place");
    assert!(engine.security().encrypted);
    engine
        .protect(None, &encryption("yeni-şifre", "", Permissions::ALL))
        .expect("change");
    let mut other = MupdfEngine::new();
    assert!(matches!(
        other.open_document_with_password(&source, Some(USER)),
        Err(MupdfError::InvalidPassword)
    ));
    assert!(text(&source, Some("yeni-şifre")).contains("PLAIN-WORDS"));
    engine.protect(None, &Protection::Remove).expect("remove");
    assert!(!engine.security().encrypted);
    let plain = open(&source, None);
    assert!(!plain.security().encrypted);
    assert!(inked(&plain));
}

#[test]
fn unsaved_changes_go_into_the_protected_file() {
    let source = scratch("unsaved.pdf");
    std::fs::write(&source, plain(false, None)).expect("fixture");
    let mut engine = open(&source, None);
    engine.edit(&note()).expect("note");
    engine
        .protect(None, &encryption(USER, "", Permissions::ALL))
        .expect("protect");
    assert_eq!(engine.annotations(0).expect("annotations").len(), 1);
    assert_eq!(
        open(&source, Some(USER))
            .annotations(0)
            .expect("annotations")
            .len(),
        1
    );
}

#[test]
fn changing_the_protection_needs_every_permission() {
    let path = print_only();
    let before = std::fs::read(&path).expect("read");
    let mut engine = open(&path, None);
    for protection in [Protection::Remove, encryption(USER, "", Permissions::ALL)] {
        assert!(matches!(
            engine.protect(None, &protection),
            Err(SaveError::Failed(_))
        ));
    }
    assert_eq!(std::fs::read(&path).expect("read"), before);
    assert!(engine.unlock(OWNER).expect("unlock"));
    engine
        .protect(Some(&scratch("unlocked.pdf")), &Protection::Remove)
        .expect("remove once unlocked");
}

#[test]
fn a_bad_protection_writes_nothing() {
    let source = scratch("bad.pdf");
    std::fs::write(&source, plain(false, None)).expect("fixture");
    let mut engine = open(&source, None);
    let target = scratch("bad-out.pdf");
    let too_long = "ş".repeat(64);
    assert!(
        engine
            .protect(Some(&target), &encryption(&too_long, "", Permissions::ALL))
            .is_err()
    );
    assert!(!target.exists());
    let mut part = target.into_os_string();
    part.push(".part");
    assert!(!PathBuf::from(part).exists());
}

fn next(rx: &Receiver<EngineEvent>) -> EngineEvent {
    rx.recv_timeout(Duration::from_secs(30)).expect("event")
}

#[test]
fn the_tools_follow_the_permissions() {
    // Extract and Merge write the pages into an unprotected file: refused without the copy
    // permission. Compress keeps the encryption, so it runs, and its output asks the password.
    let path = protected(
        USER,
        OWNER,
        Permission::PRINT | Permission::COPY.complement(),
    );
    let (handle, rx) = EngineHandle::spawn();
    handle
        .send(EngineCmd::Open {
            path,
            password: Some(USER.to_owned()),
        })
        .expect("alive");
    let security = loop {
        if let EngineEvent::Opened { security, .. } = next(&rx) {
            break security;
        }
    };
    assert!(!security.permissions.copy && security.permissions.modify);
    let (part, compressed) = (scratch("part.pdf"), scratch("compressed.pdf"));
    let jobs = [
        (
            ToolJob::Extract {
                parts: vec![(vec![0], part.clone())],
            },
            false,
        ),
        (
            ToolJob::Merge {
                inputs: vec![MergeInput::Current],
                output: scratch("merged.pdf"),
            },
            false,
        ),
        (
            ToolJob::Compress {
                input: MergeInput::Current,
                preset: Preset::Lossless,
                output: compressed.clone(),
            },
            true,
        ),
    ];
    for (job_id, (job, allowed)) in jobs.into_iter().enumerate() {
        let job_id = job_id as u64 + 1;
        handle
            .send(EngineCmd::RunTool { job_id, job })
            .expect("alive");
        loop {
            if let EngineEvent::ToolDone {
                job_id: id, result, ..
            } = next(&rx)
                && id == job_id
            {
                assert_eq!(result.is_ok(), allowed, "job {job_id}: {result:?}");
                break;
            }
        }
    }
    assert!(!part.exists());
    let mut other = MupdfEngine::new();
    assert!(matches!(
        other.open_document_with_password(&compressed, None),
        Err(MupdfError::PasswordRequired)
    ));
    // Unlocked, the job runs.
    handle
        .send(EngineCmd::Unlock {
            password: OWNER.to_owned(),
            generation: 0,
        })
        .expect("alive");
    loop {
        if let EngineEvent::Security {
            security, wrong, ..
        } = next(&rx)
        {
            assert!(!wrong && security.permissions.is_all());
            break;
        }
    }
    handle
        .send(EngineCmd::RunTool {
            job_id: 9,
            job: ToolJob::Extract {
                parts: vec![(vec![0], part.clone())],
            },
        })
        .expect("alive");
    loop {
        if let EngineEvent::ToolDone {
            job_id: 9, result, ..
        } = next(&rx)
        {
            result.expect("extract once unlocked");
            break;
        }
    }
    assert!(text(&part, None).contains("PLAIN-WORDS"));
}

#[test]
fn signatures_and_certification_are_read() {
    let read = |bytes: Vec<u8>| {
        let path = scratch("signed.pdf");
        std::fs::write(&path, bytes).expect("fixture");
        open(&path, None).security()
    };
    let signed = read(plain(true, None));
    assert_eq!((signed.signed, signed.certified), (1, None));
    for level in 1..=3 {
        let certified = read(plain(true, Some(level)));
        assert_eq!((certified.signed, certified.certified), (1, Some(level)));
    }
    assert_eq!(read(plain(false, None)).signed, 0);
}

#[cfg(windows)]
#[test]
fn a_file_held_elsewhere_keeps_the_unsaved_changes() {
    // Another program holds the file without sharing it for a rename (Windows): the protected
    // file cannot replace it. The changes were saved in place first, so the document opened again
    // still has them, and the file still opens without a password.
    use std::os::windows::fs::OpenOptionsExt as _;
    let source = scratch("held.pdf");
    std::fs::write(&source, plain(false, None)).expect("fixture");
    let mut engine = open(&source, None);
    engine.edit(&note()).expect("note");
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(3) // FILE_SHARE_READ and FILE_SHARE_WRITE, not FILE_SHARE_DELETE
        .open(&source)
        .expect("hold");
    assert!(
        engine
            .protect(None, &encryption(USER, "", Permissions::ALL))
            .is_err()
    );
    drop(held);
    assert_eq!(engine.annotations(0).expect("annotations").len(), 1);
    assert!(!engine.security().encrypted);
    assert_eq!(
        open(&source, None)
            .annotations(0)
            .expect("annotations")
            .len(),
        1
    );
}
