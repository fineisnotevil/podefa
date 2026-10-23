// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Plan 0006 Phase 1 through the engine (rows AI, AJ and AK's engine half): a form's fields are
//! read with their kinds, options and states, every kind is filled as one undo step, the values
//! survive a save in place and a reopen, a field on two pages changes both, a read-only field
//! refuses, and Turkish text fills in milliseconds.

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
        ]
    );
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
