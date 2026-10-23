// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Plan 0006 Phase 0 measurement: what mupdf-rs 0.8 and MuPDF 1.27.2 do with an AcroForm (ignored
//! by default; not part of CI). A generated form has every field type: a text field, a multi-line
//! one, a check box, a radio group, a combo box with Turkish options, a multi-select list, two
//! numbers and a total calculated by JavaScript, and an unsigned signature field.
//!
//! Run: `cargo test --release -p engine-mupdf --test forms_bench -- --ignored --nocapture`
//! `FA_PDF_FORM_INPUT=path` also lists a real form's fields; `FA_PDF_FORM_JS` replaces the total's
//! calculation script; `FA_PDF_FORM_NO_LOADER=1` leaves the engine's font loader out.

use mupdf::TextPageFlags;
use mupdf::pdf::{PdfDocument, PdfWriteOptions};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Instant;

/// A PDF text string as UTF-16BE hex, for names and options with Turkish letters.
fn utf16(s: &str) -> String {
    let mut out = String::from("<FEFF");
    for unit in s.encode_utf16() {
        let _ = write!(out, "{unit:04X}");
    }
    out.push('>');
    out
}

/// A PDF hex string of bytes, for scripts with characters a literal string would need escaped.
fn hex(s: &str) -> String {
    let mut out = String::from("<");
    for b in s.bytes() {
        let _ = write!(out, "{b:02X}");
    }
    out.push('>');
    out
}

fn form_pdf() -> Vec<u8> {
    let ap = |body: &str| {
        format!(
            "<< /Type /XObject /Subtype /Form /BBox [0 0 14 14] /Length {} >>\nstream\n{body}\nendstream",
            body.len()
        )
    };
    let tick = "0 g 2 7 m 6 3 l 12 12 l S";
    let dot = "0 g 7 7 4 0 360 re f";
    let objects: Vec<String> = vec![
        // 1 catalog, 2 pages, 3 form, 4 page.
        "<< /Type /Catalog /Pages 2 0 R /AcroForm 3 0 R >>".into(),
        "<< /Type /Pages /Kids [4 0 R] /Count 1 >>".into(),
        "<< /Fields [5 0 R 6 0 R 7 0 R 19 0 R 10 0 R 11 0 R 12 0 R 13 0 R 14 0 R 20 0 R] \
         /DR << /Font << /Helv 15 0 R >> >> /DA (/Helv 0 Tf 0 g) /CO [14 0 R] >>"
            .into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 16 0 R \
         /Resources << /Font << /Helv 15 0 R >> >> \
         /Annots [5 0 R 6 0 R 7 0 R 8 0 R 9 0 R 10 0 R 11 0 R 12 0 R 13 0 R 14 0 R 20 0 R] >>"
            .into(),
        // 5 name, 6 notes (multi-line), 7 agree (check box).
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /Rect [150 700 400 720] /P 4 0 R /F 4 \
         /DA (/Helv 11 Tf 0 g) >>"
            .into(),
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (notes) /Ff 4096 /Rect [150 600 400 690] \
         /P 4 0 R /F 4 /DA (/Helv 10 Tf 0 g) >>"
            .into(),
        "<< /Type /Annot /Subtype /Widget /FT /Btn /T (agree) /Rect [150 570 164 584] /P 4 0 R /F 4 \
         /V /Off /AS /Off /AP << /N << /Yes 17 0 R /Off 18 0 R >> >> /MK << /CA (4) >> \
         /DA (/ZaDb 0 Tf 0 g) >>"
            .into(),
        // 8, 9: the radio group's two buttons, children of 19.
        "<< /Type /Annot /Subtype /Widget /Parent 19 0 R /Rect [150 540 164 554] /P 4 0 R /F 4 \
         /AS /Off /AP << /N << /a 21 0 R /Off 18 0 R >> >> >>"
            .into(),
        "<< /Type /Annot /Subtype /Widget /Parent 19 0 R /Rect [200 540 214 554] /P 4 0 R /F 4 \
         /AS /Off /AP << /N << /b 22 0 R /Off 18 0 R >> >> >>"
            .into(),
        // 10 city (combo box, Turkish options), 11 langs (multi-select list).
        format!(
            "<< /Type /Annot /Subtype /Widget /FT /Ch /T (city) /Ff 131072 /Rect [150 500 400 520] \
             /P 4 0 R /F 4 /DA (/Helv 11 Tf 0 g) /Opt [{} {} {}] >>",
            utf16("İstanbul"),
            utf16("Ankara"),
            utf16("Muğla")
        ),
        "<< /Type /Annot /Subtype /Widget /FT /Ch /T (langs) /Ff 2097152 /Rect [150 420 400 480] \
         /P 4 0 R /F 4 /DA (/Helv 10 Tf 0 g) /Opt [(tr) (en) (de)] >>"
            .into(),
        // 12 a, 13 b, 14 total = a + b in JavaScript.
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (a) /Rect [150 380 250 400] /P 4 0 R /F 4 \
         /DA (/Helv 11 Tf 0 g) >>"
            .into(),
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (b) /Rect [260 380 360 400] /P 4 0 R /F 4 \
         /DA (/Helv 11 Tf 0 g) >>"
            .into(),
        format!(
            "<< /Type /Annot /Subtype /Widget /FT /Tx /T (total) /Ff 1 /Rect [370 380 470 400] \
             /P 4 0 R /F 4 /DA (/Helv 11 Tf 0 g) /AA << /C << /S /JavaScript /JS {} >> >> >>",
            hex(&std::env::var("FA_PDF_FORM_JS").unwrap_or_else(|_| {
                "AFSimple_Calculate(\"SUM\", new Array(\"a\", \"b\"));".to_owned()
            }))
        ),
        // 15 font, 16 page content.
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".into(),
        {
            let c = "BT /Helv 11 Tf 54 705 Td (Name) Tj 0 -95 Td (Notes) Tj 0 -30 Td (Agree) Tj \
                     0 -30 Td (Choice) Tj 0 -40 Td (City) Tj 0 -60 Td (Languages) Tj \
                     0 -40 Td (a + b) Tj 0 -60 Td (Signature) Tj ET";
            format!("<< /Length {} >>\nstream\n{c}\nendstream", c.len())
        },
        // 17, 18: check box on and off; 19 the radio group; 20 a signature field.
        ap(tick),
        ap(""),
        "<< /FT /Btn /T (choice) /Ff 49152 /Kids [8 0 R 9 0 R] /V /Off >>".into(),
        "<< /Type /Annot /Subtype /Widget /FT /Sig /T (signature) /Rect [150 260 400 300] \
         /P 4 0 R /F 4 >>"
            .into(),
        // 21, 22: radio buttons on.
        ap(dot),
        ap(dot),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    let mut table = format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
    for offset in offsets {
        let _ = writeln!(table, "{offset:010} 00000 n ");
    }
    let _ = write!(
        table,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    );
    out.extend_from_slice(table.as_bytes());
    out
}

fn scratch(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name)
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

/// Every field as `name type = value [flags]`.
fn fields(doc: &PdfDocument) -> Vec<String> {
    let mut out = Vec::new();
    for p in 0..doc.page_count().expect("count") {
        let page = doc.load_pdf_page(p).expect("page");
        for widget in page.widgets() {
            let rect = widget.annotation().rect().expect("rect");
            out.push(format!(
                "p{p} {:?} {:?} = {:?} flags={:?} rect=[{:.0} {:.0} {:.0} {:.0}]",
                widget.name().expect("name").unwrap_or_default(),
                widget.r#type().expect("type"),
                widget.value().expect("value").unwrap_or_default(),
                widget.field_flags().expect("flags"),
                rect.x0,
                rect.y0,
                rect.x1,
                rect.y1
            ));
        }
    }
    out
}

/// Sets one field by name, as one journal operation; returns whether MuPDF accepted the value.
fn set(doc: &mut PdfDocument, name: &str, value: &str) -> bool {
    doc.begin_operation(&format!("Fill {name}")).expect("begin");
    let page = doc.load_pdf_page(0).expect("page");
    let mut accepted = None;
    for mut widget in page.widgets() {
        if widget.name().expect("name").as_deref() == Some(name) {
            accepted = Some(widget.set_value(doc, value, false).expect("set"));
            widget.update().expect("update");
            break;
        }
    }
    doc.end_operation().expect("end");
    accepted.expect("field exists")
}

fn value(doc: &PdfDocument, name: &str) -> String {
    let page = doc.load_pdf_page(0).expect("page");
    for widget in page.widgets() {
        if widget.name().expect("name").as_deref() == Some(name) {
            return widget.value().expect("value").unwrap_or_default();
        }
    }
    panic!("no field {name}");
}

fn page_text(doc: &PdfDocument) -> String {
    doc.load_page(0)
        .expect("page")
        .to_text_page(TextPageFlags::empty())
        .expect("text")
        .to_text()
        .expect("text")
}

#[test]
#[ignore = "measurement, run by hand"]
fn forms_bench() {
    // The engine registers its font loader (the 14 standard fonts first, `fonts.rs`), as the app
    // runs; `FA_PDF_FORM_NO_LOADER=1` measures without it.
    let _engine = std::env::var_os("FA_PDF_FORM_NO_LOADER")
        .is_none()
        .then(engine_mupdf::MupdfEngine::new);
    let path = scratch("forms_bench.pdf");
    std::fs::write(&path, form_pdf()).expect("fixture");
    let mut doc = PdfDocument::open(path.to_str().expect("utf-8")).expect("open");
    let t = Instant::now();
    let found = fields(&doc);
    println!("[forms] {} widgets read in {:.2} ms:", found.len(), ms(t));
    for f in &found {
        println!("[forms]   {f}");
    }
    doc.enable_journal().expect("journal");

    // Filling each kind of field.
    for (name, v) in [
        // Plain ASCII first: what a fill costs without letters beyond the field font.
        ("name", "Plain ASCII"),
        ("name", "Plain ASCII again"),
        // A letter Helvetica has, then the Turkish ones it does not encode.
        ("name", "Öz"),
        ("name", "Öğüt Şükrü İnce"),
        ("notes", "Birinci satır\nİkinci satır"),
        ("agree", "Yes"),
        ("choice", "b"),
        ("city", "Muğla"),
        ("langs", "en"),
        ("a", "12"),
        ("b", "30"),
    ] {
        let t = Instant::now();
        let accepted = set(&mut doc, name, v);
        println!(
            "[forms] set {name} = {v:?}: accepted={accepted}, now {:?}, {:.2} ms",
            value(&doc, name),
            ms(t)
        );
    }
    println!(
        "[forms] total without JavaScript: {:?}",
        value(&doc, "total")
    );
    let text = page_text(&doc);
    println!(
        "[forms] the page's text shows the name: {}, the city: {}",
        text.contains("Öğüt Şükrü İnce"),
        text.contains("Muğla")
    );

    // Undo and redo of a fill.
    let (position, steps) = doc.undo_redo_state().expect("state");
    doc.undo().expect("undo");
    println!(
        "[forms] journal {position}/{steps}; after one undo b = {:?}, after redo b = {:?}",
        value(&doc, "b"),
        {
            doc.redo().expect("redo");
            value(&doc, "b")
        }
    );

    // JavaScript: calculation of the total.
    let js = doc.is_js_supported().expect("js?");
    let t = Instant::now();
    doc.enable_js().expect("enable js");
    let enable_ms = ms(t);
    let t = Instant::now();
    set(&mut doc, "a", "13");
    let set_js_ms = ms(t);
    println!(
        "[forms] JavaScript supported={js}, enable {enable_ms:.2} ms; set a = 13 with JS {set_js_ms:.2} ms; total = {:?}",
        value(&doc, "total")
    );
    doc.begin_operation("Calculate").expect("begin");
    let t = Instant::now();
    doc.calculate_form().expect("calculate");
    doc.end_operation().expect("end");
    println!(
        "[forms] calculate_form {:.2} ms; total = {:?}",
        ms(t),
        value(&doc, "total")
    );

    // Saved in place and opened again.
    let mut incremental = PdfWriteOptions::default();
    incremental.set_incremental(true).set_compress(true);
    let t = Instant::now();
    doc.save_with_options(path.to_str().expect("utf-8"), incremental)
        .expect("save");
    println!("[forms] incremental save {:.2} ms", ms(t));
    drop(doc);
    let doc = PdfDocument::open(path.to_str().expect("utf-8")).expect("reopen");
    println!("[forms] after reopening:");
    for f in fields(&doc) {
        println!("[forms]   {f}");
    }

    // Flatten (bake) inside an operation, then undo.
    let mut doc = doc;
    doc.enable_journal().expect("journal");
    doc.begin_operation("Flatten").expect("begin");
    let t = Instant::now();
    doc.bake(false, true).expect("bake");
    doc.end_operation().expect("end");
    let bake_ms = ms(t);
    let after_bake = fields(&doc).len();
    let baked_text = page_text(&doc);
    doc.undo().expect("undo bake");
    println!(
        "[forms] flatten {bake_ms:.2} ms: {after_bake} widgets left, name still in the text: {}; \
         after undo {} widgets",
        baked_text.contains("Öğüt Şükrü İnce"),
        fields(&doc).len()
    );

    // A real form, when given.
    if let Some(input) = std::env::var_os("FA_PDF_FORM_INPUT") {
        let doc = PdfDocument::open(input.to_str().expect("utf-8")).expect("open input");
        let t = Instant::now();
        let found = fields(&doc);
        println!(
            "[forms] input: {} widgets read in {:.2} ms",
            found.len(),
            ms(t)
        );
        for f in found.iter().take(40) {
            println!("[forms]   {f}");
        }
    }
}
