// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Plan 0006 Phase 1 through the engine (rows AI, AJ and AK's engine half): a form's fields are
//! read with their kinds, options and states, every kind is filled as one undo step, the values
//! survive a save in place and a reopen, a field on two pages changes both, a read-only field
//! refuses, and Turkish text fills in milliseconds. Phase 2 (row AL's engine half): the page's
//! tab order, a multi-select list's several choices, resetting a field and the form, and the page
//! Tab goes on to.

use engine_mupdf::{Change, MupdfEngine, MupdfError};
use pdf_core::annot::EditOp;
use pdf_core::form::{FieldInfo, FieldKind};
use std::path::{Path, PathBuf};
use std::time::Instant;

mod form_fixture;
use form_fixture::form_pdf;

fn fixture(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::write(&path, form_pdf()).expect("fixture");
    path
}

fn open(path: &Path) -> MupdfEngine {
    let mut engine = MupdfEngine::new();
    engine
        .open_document_with_password(path, None)
        .expect("open");
    engine
}

fn field<'a>(fields: &'a [FieldInfo], name: &str) -> &'a FieldInfo {
    fields
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("no field {name}"))
}

fn value(engine: &MupdfEngine, page: u32, name: &str) -> String {
    field(&engine.fields(page).expect("fields"), name)
        .value
        .clone()
}

/// The appearance state of each widget of a button field on page 1, as saved.
fn states(engine: &MupdfEngine, name: &str) -> Vec<String> {
    let bytes = engine.snapshot().expect("snapshot");
    let doc = mupdf::pdf::PdfDocument::from_bytes(&bytes).expect("snapshot opens");
    let page = doc.load_pdf_page(0).expect("page");
    page.widgets()
        .filter(|w| w.name().ok().flatten().as_deref() == Some(name))
        .map(|w| {
            let state = w.annotation().object().get_dict("AS").expect("dict");
            String::from_utf8_lossy(&state.expect("AS").as_name().expect("name")).into_owned()
        })
        .collect()
}

fn fill(engine: &mut MupdfEngine, page: u32, name: &str, value: &str) -> Change {
    engine
        .edit(&EditOp::SetField {
            page,
            name: name.to_owned(),
            value: value.to_owned(),
        })
        .unwrap_or_else(|e| panic!("fill {name}: {e}"))
}

#[test]
fn the_fields_are_read_with_their_kinds_options_and_states() {
    let engine = open(&fixture("form_read.pdf"));
    let fields = engine.fields(0).expect("fields");
    let kinds: Vec<(&str, FieldKind)> = fields.iter().map(|f| (f.name.as_str(), f.kind)).collect();
    assert_eq!(
        kinds,
        [
            ("name", FieldKind::Text),
            ("notes", FieldKind::Text),
            ("agree", FieldKind::CheckBox),
            ("choice", FieldKind::Radio),
            ("choice", FieldKind::Radio),
            ("city", FieldKind::ComboBox),
            ("langs", FieldKind::ListBox),
            ("a", FieldKind::Text),
            ("b", FieldKind::Text),
            ("total", FieldKind::Text),
            ("signature", FieldKind::Signature),
            ("shared", FieldKind::Text),
            ("code", FieldKind::Text),
        ]
    );
    let code = field(&fields, "code");
    assert_eq!((code.comb, code.comb_cells()), (true, Some(6)));
    assert!(!field(&fields, "name").comb);
    // Rectangles in page space: "name" is 20 points high, 72 from the top.
    let name = field(&fields, "name");
    assert_eq!(
        (name.rect.x, name.rect.y, name.rect.width, name.rect.height),
        (150.0, 72.0, 250.0, 20.0)
    );
    assert_eq!(name.font_size, 11.0);
    assert!(field(&fields, "notes").multiline);
    assert_eq!(field(&fields, "agree").on_state.as_deref(), Some("Yes"));
    let radios: Vec<_> = fields
        .iter()
        .filter(|f| f.name == "choice")
        .map(|f| f.on_state.clone().unwrap_or_default())
        .collect();
    assert_eq!(radios, ["a", "b"]);
    let city: Vec<&str> = field(&fields, "city")
        .options
        .iter()
        .map(|o| o.label.as_str())
        .collect();
    assert_eq!(city, ["İstanbul", "Ankara", "Muğla"]);
    assert!(field(&fields, "langs").multi_select);
    assert!(field(&fields, "total").read_only);
    let shared = field(&fields, "shared");
    assert_eq!(shared.max_len, Some(20));
    assert_eq!(shared.tooltip.as_deref(), Some("Your city"));
    assert_eq!(engine.fields(1).expect("page 2").len(), 1);

    let summary = engine.form_summary();
    assert!(
        summary.fields && summary.scripts && !summary.xfa,
        "{summary:?}"
    );
}

#[test]
fn every_kind_fills_as_one_step_and_survives_a_save() {
    let path = fixture("form_fill.pdf");
    let mut engine = open(&path);
    let fills = [
        ("name", "Öğüt Şükrü İnce"),
        ("notes", "Birinci satır\nİkinci satır"),
        ("agree", "Yes"),
        ("choice", "b"),
        ("city", "Muğla"),
        ("langs", "en"),
        ("a", "12"),
    ];
    for (name, v) in fills {
        let t = Instant::now();
        let change = fill(&mut engine, 0, name, v);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        assert!(
            matches!(change, Change::Page { page: 0, .. }),
            "{name}: {change:?}"
        );
        assert_eq!(value(&engine, 0, name), v);
        // Row AJ's engine side: no system font search for Turkish letters (plan §3.2 took
        // 2.4-3.5 s per fill); the bound leaves room for a debug build.
        assert!(ms < 1000.0, "{name} took {ms:.0} ms");
    }
    // One step each: undoing them all empties the form again.
    for (name, _) in fills.iter().rev() {
        engine.undo().expect("undo").expect("a step");
        let now = value(&engine, 0, name);
        assert!(now.is_empty() || now == "Off", "{name} after undo: {now:?}");
    }
    assert_eq!(engine.undo().expect("nothing left"), None);
    for _ in fills {
        engine.redo().expect("redo").expect("a step");
    }
    engine.save(None).expect("save in place");
    drop(engine);
    let engine = open(&path);
    for (name, v) in fills {
        assert_eq!(value(&engine, 0, name), v, "{name} after reopening");
    }
}

#[test]
fn a_field_on_two_pages_changes_both() {
    let mut engine = open(&fixture("form_shared.pdf"));
    let change = fill(&mut engine, 1, "shared", "Muğla");
    match change {
        Change::Pages { map, .. } => assert_eq!(map, [None, None]),
        other => panic!("both pages should change: {other:?}"),
    }
    assert_eq!(value(&engine, 0, "shared"), "Muğla");
    assert_eq!(value(&engine, 1, "shared"), "Muğla");
    let back = engine.undo().expect("undo").expect("a step");
    assert!(matches!(back, Change::Pages { .. }), "{back:?}");
    assert_eq!(value(&engine, 0, "shared"), "");
}

#[test]
fn read_only_and_unknown_fields_are_refused_without_a_step() {
    let mut engine = open(&fixture("form_refuse.pdf"));
    let refused = engine.edit(&EditOp::SetField {
        page: 0,
        name: "total".to_owned(),
        value: "99".to_owned(),
    });
    assert!(
        matches!(refused, Err(MupdfError::FieldReadOnly(ref n)) if n == "total"),
        "{refused:?}"
    );
    let missing = engine.edit(&EditOp::SetField {
        page: 1,
        name: "name".to_owned(),
        value: "x".to_owned(),
    });
    assert!(
        matches!(missing, Err(MupdfError::FieldNotFound { page: 1, .. })),
        "{missing:?}"
    );
    assert_eq!(engine.history_state(), (false, false), "no step was added");
}

#[test]
fn a_radio_group_shows_the_chosen_button() {
    let mut engine = open(&fixture("form_radio.pdf"));
    fill(&mut engine, 0, "choice", "b");
    assert_eq!(states(&engine, "choice"), ["Off", "b"]);
    fill(&mut engine, 0, "choice", "a");
    assert_eq!(states(&engine, "choice"), ["a", "Off"]);
    engine.undo().expect("undo").expect("a step");
    assert_eq!(states(&engine, "choice"), ["Off", "b"]);
}

/// MuPDF 1.27.2 lays single-line Turkish text out below the field (plan, Phase 1 results): the
/// engine moves it into place, by the translation `-7.2` for this 20-point field and 11-point
/// text. The multi-line field is laid out right and is left alone.
#[test]
fn single_line_turkish_text_sits_inside_its_field() {
    let mut engine = open(&fixture("form_rich.pdf"));
    fill(&mut engine, 0, "name", "Öğüt Şükrü İnce");
    fill(&mut engine, 0, "notes", "Birinci satır\nİkinci satır");
    let bytes = engine.snapshot().expect("snapshot");
    let doc = mupdf::pdf::PdfDocument::from_bytes(&bytes).expect("snapshot opens");
    let page = doc.load_pdf_page(0).expect("page");
    let appearance = |name: &str| {
        let widget = page
            .widgets()
            .find(|w| w.name().ok().flatten().as_deref() == Some(name))
            .expect("widget");
        let normal = widget
            .annotation()
            .object()
            .get_dict("AP")
            .expect("dict")
            .expect("AP")
            .get_dict("N")
            .expect("dict")
            .expect("N");
        String::from_utf8_lossy(&normal.read_stream().expect("stream")).into_owned()
    };
    let name = appearance("name");
    assert!(name.contains("\n1 0 0 1 0 -7.2 cm\n"), "{name}");
    let notes = appearance("notes");
    assert!(!notes.contains("1 0 0 1 0 -"), "{notes}");
}

/// A widget's normal appearance stream on page 1, as saved.
fn appearance(engine: &MupdfEngine, name: &str) -> String {
    let bytes = engine.snapshot().expect("snapshot");
    let doc = mupdf::pdf::PdfDocument::from_bytes(&bytes).expect("snapshot opens");
    let page = doc.load_pdf_page(0).expect("page");
    let widget = page
        .widgets()
        .find(|w| w.name().ok().flatten().as_deref() == Some(name))
        .expect("widget");
    let normal = widget
        .annotation()
        .object()
        .get_dict("AP")
        .expect("dict")
        .expect("AP")
        .get_dict("N")
        .expect("dict")
        .expect("N");
    String::from_utf8_lossy(&normal.read_stream().expect("stream")).into_owned()
}

fn choices(engine: &MupdfEngine, name: &str) -> Vec<String> {
    field(&engine.fields(0).expect("fields"), name)
        .values
        .clone()
}

#[test]
fn tab_follows_the_rows_of_a_page_tabbed_by_rows() {
    let engine = open(&fixture("form_tabs.pdf"));
    let fields = engine.fields(0).expect("fields");
    let order: Vec<&str> = pdf_core::form::tab_sequence(&fields)
        .into_iter()
        .map(|i| fields[i].name.as_str())
        .collect();
    // "code" sits on the row of "name", to its right, though it is the page's last annotation;
    // the read-only total and the signature are not visited.
    assert_eq!(
        order,
        [
            "name", "code", "notes", "agree", "choice", "choice", "city", "langs", "a", "b",
            "shared"
        ]
    );
}

#[test]
fn tab_goes_on_to_the_next_page_with_a_field_and_round_the_end() {
    let engine = open(&fixture("form_pages.pdf"));
    assert_eq!(engine.next_field_page(0, false), Some(1));
    assert_eq!(engine.next_field_page(1, false), Some(0));
    assert_eq!(engine.next_field_page(0, true), Some(1));
    assert_eq!(engine.next_field_page(1, true), Some(0));
}

#[test]
fn a_multi_select_list_takes_several_choices_and_shows_them() {
    let path = fixture("form_multi.pdf");
    let mut engine = open(&path);
    let change = engine
        .edit(&EditOp::SetChoices {
            page: 0,
            name: "langs".to_owned(),
            values: vec!["de".to_owned(), "tr".to_owned()],
        })
        .expect("choose");
    assert!(matches!(change, Change::Page { page: 0, .. }), "{change:?}");
    // Kept in the options' order, whatever order they were given in.
    assert_eq!(choices(&engine, "langs"), ["tr", "de"]);
    // The chosen rows are marked: MuPDF 1.27.2 draws a list without its value.
    let ap = appearance(&engine, "langs");
    assert_eq!(ap.matches("0.6 0.75 0.85 rg").count(), 2, "{ap}");
    // One step: undo clears both.
    engine.undo().expect("undo").expect("a step");
    assert!(choices(&engine, "langs").is_empty());
    engine.redo().expect("redo").expect("a step");
    engine.save(None).expect("save in place");
    drop(engine);
    let engine = open(&path);
    assert_eq!(choices(&engine, "langs"), ["tr", "de"]);
    // A value that is not an option is refused.
    let mut engine = engine;
    let refused = engine.edit(&EditOp::SetChoices {
        page: 0,
        name: "langs".to_owned(),
        values: vec!["fr".to_owned(), "tr".to_owned()],
    });
    assert!(
        matches!(refused, Err(MupdfError::FieldRefused(_))),
        "{refused:?}"
    );
}

#[test]
fn a_field_resets_to_its_default_as_one_step() {
    let mut engine = open(&fixture("form_reset_field.pdf"));
    fill(&mut engine, 0, "city", "Muğla");
    fill(&mut engine, 0, "name", "Öğüt");
    let reset = |engine: &mut MupdfEngine, name: &str| {
        engine
            .edit(&EditOp::ResetField {
                page: 0,
                name: name.to_owned(),
            })
            .expect("reset")
    };
    reset(&mut engine, "city");
    assert_eq!(value(&engine, 0, "city"), "Ankara", "its /DV");
    reset(&mut engine, "name");
    assert_eq!(value(&engine, 0, "name"), "", "no /DV: empty");
    engine.undo().expect("undo").expect("a step");
    assert_eq!(value(&engine, 0, "name"), "Öğüt");
}

#[test]
fn the_form_resets_as_one_step_and_a_second_reset_adds_none() {
    let mut engine = open(&fixture("form_reset.pdf"));
    fill(&mut engine, 0, "name", "Öğüt");
    fill(&mut engine, 0, "agree", "Yes");
    fill(&mut engine, 1, "shared", "Muğla");
    engine
        .edit(&EditOp::SetChoices {
            page: 0,
            name: "langs".to_owned(),
            values: vec!["tr".to_owned(), "en".to_owned()],
        })
        .expect("choose");
    let change = engine.edit(&EditOp::ResetForm).expect("reset");
    // "shared" is on both pages, so both changed.
    assert!(matches!(change, Change::Pages { .. }), "{change:?}");
    assert_eq!(value(&engine, 0, "name"), "");
    assert_eq!(value(&engine, 0, "agree"), "Off");
    assert_eq!(value(&engine, 1, "shared"), "");
    assert!(choices(&engine, "langs").is_empty());
    assert_eq!(value(&engine, 0, "city"), "Ankara");
    // Already reset: refused, no step.
    let again = engine.edit(&EditOp::ResetForm);
    assert!(
        matches!(again, Err(MupdfError::NothingToReset)),
        "{again:?}"
    );
    // One undo brings every value back.
    engine.undo().expect("undo").expect("a step");
    assert_eq!(value(&engine, 0, "name"), "Öğüt");
    assert_eq!(value(&engine, 0, "agree"), "Yes");
    assert_eq!(value(&engine, 1, "shared"), "Muğla");
    assert_eq!(choices(&engine, "langs"), ["tr", "en"]);
}

/// The text of page `index` as saved, the way a reader extracts it.
fn saved_text(engine: &MupdfEngine, index: i32) -> String {
    let bytes = engine.snapshot().expect("snapshot");
    let doc = mupdf::pdf::PdfDocument::from_bytes(&bytes).expect("snapshot opens");
    doc.load_page(index)
        .expect("page")
        .to_text_page(mupdf::TextPageFlags::empty())
        .expect("text")
        .to_text()
        .expect("text")
}

/// How many widgets the saved document has, over all pages.
fn saved_widgets(engine: &MupdfEngine) -> usize {
    let bytes = engine.snapshot().expect("snapshot");
    let doc = mupdf::pdf::PdfDocument::from_bytes(&bytes).expect("snapshot opens");
    (0..doc.page_count().expect("pages"))
        .map(|p| doc.load_pdf_page(p).expect("page").widgets().count())
        .sum()
}

#[test]
fn flattening_draws_the_fields_into_the_pages_as_one_step() {
    let path = fixture("form_flatten.pdf");
    let mut engine = open(&path);
    fill(&mut engine, 0, "name", "Öğüt Şükrü");
    fill(&mut engine, 0, "agree", "Yes");
    fill(&mut engine, 1, "shared", "Muğla");
    assert!(
        !saved_text(&engine, 0).contains("Öğüt"),
        "a field is no page text yet"
    );

    let change = engine.edit(&EditOp::FlattenForm).expect("flatten");
    assert!(matches!(change, Change::Pages { .. }), "{change:?}");
    assert!(engine.fields(0).expect("fields").is_empty());
    assert!(engine.fields(1).expect("fields").is_empty());
    assert_eq!(saved_widgets(&engine), 0);
    // The values are the pages' own text now, on both pages, Turkish letters included.
    let text = saved_text(&engine, 0);
    assert!(text.contains("Öğüt Şükrü"), "{text}");
    assert!(saved_text(&engine, 1).contains("Muğla"));
    // Nothing left to flatten: refused, no step.
    let again = engine.edit(&EditOp::FlattenForm);
    assert!(matches!(again, Err(MupdfError::NoForm)), "{again:?}");

    // One undo brings the form back, values and all; redo flattens it again.
    engine.undo().expect("undo").expect("a step");
    assert_eq!(value(&engine, 0, "name"), "Öğüt Şükrü");
    assert_eq!(value(&engine, 0, "agree"), "Yes");
    assert_eq!(value(&engine, 1, "shared"), "Muğla");
    assert!(saved_widgets(&engine) > 0);
    engine.redo().expect("redo").expect("a step");
    assert_eq!(saved_widgets(&engine), 0);

    // Saved in place and reopened: still flat.
    engine.save(None).expect("save");
    let engine = open(&path);
    assert!(engine.fields(0).expect("fields").is_empty());
    assert!(saved_text(&engine, 0).contains("Öğüt Şükrü"));
}

/// Fills one of every kind the data files carry, Turkish text and a multi-line note included.
fn fill_everything(engine: &mut MupdfEngine) {
    fill(engine, 0, "name", "Öğüt \"Şükrü\" <İnce> & co");
    fill(engine, 0, "notes", "Birinci satır\nİkinci satır");
    fill(engine, 0, "agree", "Yes");
    fill(engine, 0, "choice", "b");
    fill(engine, 0, "city", "Muğla");
    fill(engine, 0, "code", "AB12CD");
    fill(engine, 1, "shared", "İzmir");
    engine
        .edit(&EditOp::SetChoices {
            page: 0,
            name: "langs".to_owned(),
            values: vec!["tr".to_owned(), "de".to_owned()],
        })
        .expect("choose");
}

#[test]
fn values_go_out_and_into_an_empty_copy_in_xfdf_and_json() {
    use pdf_core::form_data::{self, DataFormat};
    let mut engine = open(&fixture("form_export.pdf"));
    fill_everything(&mut engine);
    let values = engine.form_values().expect("values");
    let names: Vec<&str> = values.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "name", "notes", "agree", "choice", "city", "langs", "a", "b", "total", "shared",
            "code"
        ],
        "each field once, in page and annotation order"
    );
    for (format, file) in [
        (DataFormat::Xfdf, "form_import_xfdf.pdf"),
        (DataFormat::Json, "form_import_json.pdf"),
    ] {
        let text = form_data::write(&values, format, Some("form_export.pdf"));
        let read = form_data::read(&text, format).expect("read back");
        let mut empty = open(&fixture(file));
        let change = empty
            .edit(&EditOp::ImportForm { values: read })
            .unwrap_or_else(|e| panic!("{format:?}: {e}"));
        // "shared" is on page 2 too.
        assert!(matches!(change, Change::Pages { .. }), "{change:?}");
        assert_eq!(empty.form_values().expect("values"), values, "{format:?}");
        // The whole import is one step.
        empty.undo().expect("undo").expect("a step");
        assert_eq!(value(&empty, 0, "name"), "", "{format:?}");
        assert_eq!(value(&empty, 1, "shared"), "", "{format:?}");
        assert!(empty.undo().expect("undo").is_none(), "{format:?}");
    }
}

#[test]
fn an_import_leaves_out_what_the_form_does_not_take() {
    use pdf_core::form_data::FieldValue;
    let v = |name: &str, value: &str| FieldValue::new(name, vec![value.to_owned()]);
    let mut engine = open(&fixture("form_import_bad.pdf"));
    let junk = vec![
        v("nope", "x"),
        v("total", "99"),
        v("city", "Paris"),
        v("agree", "Maybe"),
        v("choice", "c"),
        FieldValue::new("langs", vec!["tr".to_owned(), "fr".to_owned()]),
        v("signature", "me"),
    ];
    let refused = engine.edit(&EditOp::ImportForm {
        values: junk.clone(),
    });
    assert!(
        matches!(refused, Err(MupdfError::NothingToImport)),
        "{refused:?}"
    );
    assert!(engine.undo().expect("undo").is_none());

    // One value the form takes among the junk: only it is set, as a page edit.
    let mut some = junk;
    some.push(v("name", "Öğüt"));
    let change = engine
        .edit(&EditOp::ImportForm {
            values: some.clone(),
        })
        .expect("import");
    assert!(matches!(change, Change::Page { page: 0, .. }), "{change:?}");
    assert_eq!(value(&engine, 0, "name"), "Öğüt");
    assert_eq!(value(&engine, 0, "city"), "");
    assert_eq!(value(&engine, 0, "agree"), "Off");
    assert!(choices(&engine, "langs").is_empty());
    // The same file again changes nothing.
    let again = engine.edit(&EditOp::ImportForm { values: some });
    assert!(
        matches!(again, Err(MupdfError::NothingToImport)),
        "{again:?}"
    );
}
