// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Plan 0009 Phase 0 measurement: what mupdf-rs 0.8 and MuPDF 1.27.2 do with redaction and
//! encryption (ignored by default; not part of CI).
//!
//! A generated page holds a public line and a secret one (`SECRET-42`, uncompressed, so the saved
//! bytes can be searched), a highlight, a note and a free-text annotation over the secret, each
//! naming it, a link over it, and the secret in the document's title and outline. A redaction
//! covers the secret line.
//!
//! Run: `cargo test --release -p engine-mupdf --test secure_bench -- --ignored --nocapture`

use mupdf::pdf::{
    PdfAnnotationType, PdfDocument, PdfRedactOptions, PdfWriteOptions,
    document::{Encryption, Permission},
};
use mupdf::{MetadataName, Rect, TextPageFlags};
use std::path::PathBuf;
use std::time::Instant;

const SECRET: &[u8] = b"SECRET-42";

fn scratch(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name)
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn fixture() -> Vec<u8> {
    let content = "BT /F1 14 Tf 72 700 Td (PUBLIC LINE) Tj ET\n\
                   BT /F1 14 Tf 72 600 Td (SECRET-42 account) Tj ET\n\
                   0 0 1 rg 72 560 200 20 re f";
    let objects: Vec<String> = vec![
        // 1 catalog, 2 pages, 3 page, 4 font, 5 content.
        "<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R \
         /Resources << /Font << /F1 4 0 R >> >> /Annots [6 0 R 7 0 R 8 0 R 9 0 R] >>"
            .into(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
        // 6 highlight, 7 note, 8 free text, 9 link: all over or naming the secret line.
        "<< /Type /Annot /Subtype /Highlight /Rect [70 595 230 615] \
         /QuadPoints [70 615 230 615 70 595 230 595] /C [1 1 0] /Contents (highlight on SECRET-42) >>"
            .into(),
        "<< /Type /Annot /Subtype /Text /Rect [240 598 260 618] /Contents (note: SECRET-42) >>".into(),
        "<< /Type /Annot /Subtype /FreeText /Rect [270 595 400 615] /DA (/Helv 10 Tf 0 g) \
         /Contents (free text SECRET-42) >>"
            .into(),
        "<< /Type /Annot /Subtype /Link /Rect [72 595 200 615] /Border [0 0 0] \
         /A << /S /URI /URI (https://example.org/SECRET-42) >> >>"
            .into(),
        // 10 outline, 11 its item, 12 info.
        "<< /Type /Outlines /First 11 0 R /Last 11 0 R /Count 1 >>".into(),
        "<< /Title (Chapter SECRET-42) /Parent 10 0 R /Dest [3 0 R /XYZ 0 792 0] >>".into(),
        "<< /Title (Report SECRET-42) >>".into(),
    ];
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
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /Info 12 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

fn count(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

fn page_text(doc: &PdfDocument) -> String {
    doc.load_page(0)
        .expect("page")
        .to_text_page(TextPageFlags::empty())
        .expect("text")
        .to_text()
        .expect("text")
}

/// The annotations left on page 1 as `Type: contents`.
fn annotations(doc: &PdfDocument) -> Vec<String> {
    let page = doc.load_pdf_page(0).expect("page");
    page.annotations()
        .map(|a| {
            format!(
                "{:?}: {}",
                a.r#type().expect("type"),
                a.contents().expect("contents").unwrap_or_default()
            )
        })
        .collect()
}

fn links(doc: &PdfDocument) -> usize {
    doc.load_page(0)
        .expect("page")
        .links()
        .expect("links")
        .count()
}

/// Adds the redaction over the secret line and applies it (MuPDF's own journal operation).
fn redact(doc: &mut PdfDocument, options: PdfRedactOptions) -> f64 {
    let mut page = doc.load_pdf_page(0).expect("page");
    doc.begin_operation("Mark redaction").expect("begin");
    let mut annot = page
        .create_annotation(PdfAnnotationType::Redact)
        .expect("redact");
    annot
        .set_rect(Rect::new(70.0, 175.0, 235.0, 200.0))
        .expect("rect");
    doc.end_operation().expect("end");
    let t = Instant::now();
    assert!(page.redact_with_options(options).expect("apply"));
    ms(t)
}

fn report(label: &str, doc: &PdfDocument) {
    let text = page_text(doc);
    println!(
        "[redact] {label}: text has secret {}, public {}; links {}; title {:?}",
        text.contains("SECRET-42"),
        text.contains("PUBLIC"),
        links(doc),
        doc.metadata(MetadataName::Title).expect("title")
    );
    for a in annotations(doc) {
        println!("[redact]   {a}");
    }
}

fn save(doc: &PdfDocument, name: &str, options: PdfWriteOptions) -> Result<Vec<u8>, String> {
    let path = scratch(name);
    doc.save_with_options(path.to_str().expect("utf-8"), options)
        .map_err(|e| e.to_string())?;
    std::fs::read(&path).map_err(|e| e.to_string())
}

#[test]
#[ignore = "measurement, run by hand"]
fn redaction_bench() {
    let path = scratch("secure_bench.pdf");
    std::fs::write(&path, fixture()).expect("fixture");
    println!(
        "[redact] fixture: {} copies of the secret in the bytes",
        count(&fixture(), SECRET)
    );

    for (name, options) in [
        ("default", PdfRedactOptions::default()),
        ("pymupdf_default", PdfRedactOptions::pymupdf_default()),
    ] {
        let mut doc = PdfDocument::open(path.to_str().expect("utf-8")).expect("open");
        doc.enable_journal().expect("journal");
        report(&format!("{name} before"), &doc);
        let took = redact(&mut doc, options);
        println!("[redact] {name}: applied in {took:.2} ms");
        report(&format!("{name} after"), &doc);

        let mut incremental = PdfWriteOptions::default();
        incremental.set_incremental(true);
        match save(&doc, &format!("secure_{name}_inc.pdf"), incremental) {
            Ok(bytes) => println!(
                "[redact]   incremental save: {} copies",
                count(&bytes, SECRET)
            ),
            Err(e) => println!("[redact]   incremental save refused: {e}"),
        }
        let full = PdfWriteOptions::default();
        if let Ok(bytes) = save(&doc, &format!("secure_{name}_full.pdf"), full) {
            println!(
                "[redact]   full save, no garbage: {} copies",
                count(&bytes, SECRET)
            );
        }
        let mut gc = PdfWriteOptions::default();
        gc.set_garbage_level(4);
        if let Ok(bytes) = save(&doc, &format!("secure_{name}_gc.pdf"), gc) {
            println!(
                "[redact]   full save, garbage 4: {} copies",
                count(&bytes, SECRET)
            );
            let back = PdfDocument::from_bytes(&bytes).expect("reopen");
            report("   reopened", &back);
        }

        // Undo after these saves: `redaction_undo_bench` (the garbage-collecting save above breaks
        // the open document's undo, Plan 0005 §3.4).
    }
}

#[test]
#[ignore = "measurement, run by hand"]
fn encryption_bench() {
    let user = "kullanıcı-şifre";
    let owner = "sahip-ŞİFRE";
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/large_200p.pdf");
    let doc = PdfDocument::open(source.to_str().expect("utf-8")).expect("open");
    let pages = doc.page_count().expect("count");

    for (label, method) in [
        ("AES-256", Encryption::Aes256),
        ("AES-128", Encryption::Aes128),
        ("RC4-128", Encryption::Rc4_128),
    ] {
        let mut options = PdfWriteOptions::default();
        options
            .set_encryption(method)
            .set_user_password(user)
            .set_owner_password(owner)
            .set_permissions(Permission::PRINT | Permission::PRINT_HQ);
        let out = scratch(&format!("secure_enc_{label}.pdf"));
        let t = Instant::now();
        doc.save_with_options(out.to_str().expect("utf-8"), options)
            .expect("save");
        let saved = ms(t);

        let t = Instant::now();
        let mut back = PdfDocument::open(out.to_str().expect("utf-8")).expect("reopen");
        let needs = back.needs_password().expect("needs");
        let wrong = back.authenticate("yanlış").expect("auth");
        let right = back.authenticate(user).expect("auth");
        let opened = ms(t);
        let t = Instant::now();
        let text = back
            .load_page(pages - 1)
            .expect("page")
            .to_text_page(TextPageFlags::empty())
            .expect("text")
            .to_text()
            .expect("text");
        println!(
            "[enc] {label}: {pages} pages saved in {saved:.1} ms, {} bytes; reopen+auth {opened:.2} ms, \
             last page text {:.2} ms ({} chars); needs password {needs}, wrong accepted {wrong}, \
             user accepted {right}, permissions with the user password {:?}",
            std::fs::metadata(&out).expect("size").len(),
            ms(t),
            text.len(),
            back.permissions().bits()
        );
        let mut as_owner = PdfDocument::open(out.to_str().expect("utf-8")).expect("reopen");
        let owner_ok = as_owner.authenticate(owner).expect("auth");
        println!(
            "[enc]   owner accepted {owner_ok}, permissions {:?}",
            as_owner.permissions().bits()
        );

        // An edit saved in place keeps the encryption (`do_encrypt` defaults to KEEP).
        back.enable_journal().expect("journal");
        back.begin_operation("note").expect("begin");
        let mut page = back.load_pdf_page(0).expect("page");
        let mut note = page
            .create_annotation(PdfAnnotationType::Text)
            .expect("note");
        note.set_contents("şifreli not").expect("contents");
        back.end_operation().expect("end");
        let mut inc = PdfWriteOptions::default();
        inc.set_incremental(true);
        let t = Instant::now();
        let res = back.save_with_options(out.to_str().expect("utf-8"), inc);
        println!(
            "[enc]   incremental save with the user password: {res:?} in {:.2} ms",
            ms(t)
        );
        drop(page);
        drop(back);
        let mut again = PdfDocument::open(out.to_str().expect("utf-8")).expect("reopen");
        let still = again.needs_password().expect("needs");
        again.authenticate(user).expect("auth");
        println!(
            "[enc]   after it: needs password {still}, annotations {:?}",
            annotations(&again)
        );

        // Removing the encryption with only the user password.
        let mut plain = PdfWriteOptions::default();
        plain.set_encryption(Encryption::None);
        let plain_out = scratch(&format!("secure_plain_{label}.pdf"));
        let res = again.save_with_options(plain_out.to_str().expect("utf-8"), plain);
        let reopened = PdfDocument::open(plain_out.to_str().expect("utf-8"))
            .map(|d| d.needs_password().unwrap_or(true));
        println!(
            "[enc]   saved without encryption from the user password: {res:?}, needs password after: {reopened:?}"
        );
    }
}

/// Undo and redo of a redaction with no save in between (a garbage-collecting save of the open
/// document breaks undo, Plan 0005 §3.4).
#[test]
#[ignore = "measurement, run by hand"]
fn redaction_undo_bench() {
    let mut doc = PdfDocument::from_bytes(&fixture()).expect("open");
    doc.enable_journal().expect("journal");
    redact(&mut doc, PdfRedactOptions::default());
    report("undo-only after", &doc);
    doc.undo().expect("undo");
    report("undo-only after undo", &doc);
    let mut full = Vec::new();
    doc.write_to(&mut full).expect("write");
    println!(
        "[redact]   undone, full write: {} copies",
        count(&full, SECRET)
    );
    doc.redo().expect("redo");
    report("undo-only after redo", &doc);
}
