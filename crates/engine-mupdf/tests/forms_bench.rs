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
use std::path::PathBuf;
use std::time::Instant;

mod form_fixture;
use form_fixture::form_pdf;

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
