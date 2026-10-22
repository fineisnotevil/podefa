// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Structural edits through the engine (Plan 0005 Phase 1, row AA's engine half): rotate, delete,
//! duplicate, move and insert a blank page, each one undo step. A model of the page order follows
//! every edit through the page map the engine reports, and the document - read back as bytes, the
//! way the search thread reads it - must match the model after each edit, each undo and each redo,
//! and after a save in place and a reopen.

use engine_mupdf::{Change, MupdfEngine};
use mupdf::{Document, TextPageFlags};
use pdf_core::annot::EditOp;
use pdf_core::pages::{PageMap, PageOp, invert, plan};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn text_pdf(pages: usize) -> Vec<u8> {
    let mut objects: Vec<String> = Vec::new();
    objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
    let kids: String = (0..pages).map(|p| format!("{} 0 R ", 4 + 2 * p)).collect();
    objects.push(format!(
        "<< /Type /Pages /Kids [{kids}] /Count {pages} \
         /Resources << /Font << /F1 3 0 R >> >> >>"
    ));
    objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());
    for p in 0..pages {
        let content = format!("BT /F1 10 Tf 54 750 Td (page {p}) Tj ET");
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} 792] /Contents {} 0 R >>",
            width(p as u32),
            5 + 2 * p
        ));
        objects.push(format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ));
    }
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

fn open(path: &Path) -> MupdfEngine {
    let mut engine = MupdfEngine::new();
    engine
        .open_document_with_password(path, None)
        .expect("open");
    engine
}

/// Each page of the fixture is as wide as its number says, so its size tells which page it is;
/// blank pages are Letter, wider than any of them.
fn width(page: u32) -> u32 {
    300 + 10 * page
}

/// Which original page each page is, from its size, and whether it is turned sideways.
fn by_size(sizes: &[pdf_core::PageSize]) -> Vec<(Option<u32>, bool)> {
    sizes
        .iter()
        .map(|size| {
            let short = size.width.min(size.height).round() as u32;
            let origin = (short != 612).then(|| (short - 300) / 10);
            (origin, size.width > size.height)
        })
        .collect()
}

/// A page as the model sees it: the page of the original document it shows (`None` for a blank
/// one) and its rotation.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Model {
    origin: Option<u32>,
    rotation: i32,
}

/// What the document holds: every page's text ("page N", or nothing on a blank page) and whether it
/// is turned sideways.
fn observed(engine: &MupdfEngine, sizes: &[pdf_core::PageSize]) -> Vec<(Option<u32>, bool)> {
    let bytes = engine.snapshot().expect("snapshot");
    let doc = Document::from_bytes(&bytes, "application/pdf").expect("snapshot opens");
    let count = doc.page_count().expect("count");
    assert_eq!(count as usize, sizes.len(), "page sizes cover every page");
    (0..count)
        .map(|p| {
            let text = doc
                .load_page(p)
                .expect("page")
                .to_text_page(TextPageFlags::empty())
                .expect("text")
                .to_text()
                .expect("text");
            let origin = text
                .trim()
                .strip_prefix("page ")
                .and_then(|n| n.parse::<u32>().ok());
            let size = sizes[p as usize];
            (origin, size.width > size.height)
        })
        .collect()
}

fn expected(model: &[Model]) -> Vec<(Option<u32>, bool)> {
    model
        .iter()
        .map(|m| (m.origin, m.rotation.rem_euclid(180) == 90))
        .collect()
}

/// The model after an edit: each page comes from where `map` says, or is the blank page or the
/// turned page the edit made.
fn step(model: &[Model], op: &PageOp, map: &PageMap) -> Vec<Model> {
    let mut next: Vec<Model> = Vec::with_capacity(map.len());
    for (new, old) in map.iter().enumerate() {
        next.push(match (old, op) {
            (Some(old), _) => model[*old as usize],
            (None, PageOp::InsertBlank { .. }) => Model {
                origin: None,
                rotation: 0,
            },
            (None, PageOp::Rotate { quarter_turns, .. }) => {
                let was = model[new];
                Model {
                    origin: was.origin,
                    rotation: was.rotation + 90 * quarter_turns,
                }
            }
            (None, _) => panic!("{op:?} made page {new} new"),
        });
    }
    next
}

fn pages_changed(change: Change) -> (Vec<pdf_core::PageSize>, u32, PageMap) {
    match change {
        Change::Pages {
            page_sizes,
            revision,
            map,
        } => (page_sizes, revision, map),
        other => panic!("a structural edit reported {other:?}"),
    }
}

/// A fixed linear congruential sequence: the same edits on every run and platform.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: u32) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) % u64::from(bound.max(1))) as u32
    }
}

fn random_op(rng: &mut Lcg, count: u32) -> PageOp {
    let some = |rng: &mut Lcg| -> Vec<u32> {
        let n = 1 + rng.next(3);
        (0..n).map(|_| rng.next(count)).collect()
    };
    match rng.next(5) {
        0 => PageOp::Rotate {
            pages: some(rng),
            quarter_turns: [1, -1, 2][rng.next(3) as usize],
        },
        1 if count > 4 => PageOp::Delete { pages: some(rng) },
        2 if count < 30 => PageOp::Duplicate { pages: some(rng) },
        3 => PageOp::InsertBlank {
            at: rng.next(count + 1),
            width: 612.0,
            height: 792.0,
        },
        _ => PageOp::Move {
            pages: some(rng),
            before: rng.next(count + 1),
        },
    }
}

#[test]
fn page_edits_follow_their_maps_and_undo_and_redo_exactly() {
    const PAGES: u32 = 12;
    let path = scratch("pages_round_trip.pdf");
    std::fs::write(&path, text_pdf(PAGES as usize)).expect("write fixture");
    let mut engine = open(&path);
    let opened: Vec<Model> = (0..PAGES)
        .map(|p| Model {
            origin: Some(p),
            rotation: 0,
        })
        .collect();

    let mut rng = Lcg(5);
    let mut states = vec![opened];
    let mut ops = Vec::new();
    let mut titles = Vec::new();
    let mut last_revision = 0;
    for n in 0..40 {
        let model = states.last().cloned().expect("state");
        let op = random_op(&mut rng, model.len() as u32);
        let planned = plan(&op, model.len() as u32).expect("valid op");
        let (sizes, revision, map) = pages_changed(
            engine
                .edit(&EditOp::Pages(op.clone()))
                .unwrap_or_else(|e| panic!("edit {n} {op:?}: {e}")),
        );
        assert_eq!(map, planned, "edit {n}: the engine's map is the plan's");
        assert!(
            revision > last_revision,
            "edit {n}: a new revision for every page"
        );
        last_revision = revision;
        let next = step(&model, &op, &map);
        assert_eq!(by_size(&sizes), expected(&next), "after edit {n} {op:?}");
        // An edit that leaves every page where it was is no step at all.
        if map.iter().enumerate().all(|(i, o)| *o == Some(i as u32)) && map.len() == model.len() {
            assert_eq!(next, model);
            continue;
        }
        states.push(next);
        ops.push((map, model.len() as u32));
        titles.push(op.title());
    }

    // The sequence exercises every kind of edit.
    let kinds: std::collections::BTreeSet<&str> = titles.iter().copied().collect();
    assert_eq!(kinds.len(), 5, "{kinds:?}");
    assert!(ops.len() >= 30, "{} steps", ops.len());

    // Undo every edit: each reports the inverse map, and the document returns step by step.
    for (n, (map, old_count)) in ops.iter().enumerate().rev() {
        let (sizes, _, back) = pages_changed(engine.undo().expect("undo").expect("a step"));
        assert_eq!(back, invert(map, *old_count), "undo {n}: the inverse map");
        assert_eq!(by_size(&sizes), expected(&states[n]), "after undoing {n}");
    }
    assert_eq!(engine.undo().expect("nothing left"), None);

    // Redo all of them.
    for (n, (map, _)) in ops.iter().enumerate() {
        let (sizes, _, forward) = pages_changed(engine.redo().expect("redo").expect("a step"));
        assert_eq!(&forward, map, "redo {n}: the forward map");
        assert_eq!(
            by_size(&sizes),
            expected(&states[n + 1]),
            "after redoing {n}"
        );
    }
    // The bytes the search thread reads have the same pages, text included.
    let (sizes, _) = engine.page_sizes_lenient();
    assert_eq!(
        observed(&engine, &sizes),
        expected(states.last().expect("state"))
    );

    // Saved in place and reopened, the file has the final pages.
    let last = states.last().cloned().expect("state");
    engine.save(None).expect("in-place save");
    let reopened = open(&path);
    let (sizes, _) = reopened.page_sizes_lenient();
    assert_eq!(observed(&reopened, &sizes), expected(&last), "reopened");
}

#[test]
fn bad_page_edits_change_nothing() {
    let path = scratch("pages_bad.pdf");
    std::fs::write(&path, text_pdf(3)).expect("write fixture");
    let mut engine = open(&path);
    for op in [
        PageOp::Delete {
            pages: vec![0, 1, 2],
        },
        PageOp::Move {
            pages: vec![5],
            before: 0,
        },
        PageOp::Rotate {
            pages: vec![],
            quarter_turns: 1,
        },
    ] {
        assert!(engine.edit(&EditOp::Pages(op.clone())).is_err(), "{op:?}");
    }
    assert_eq!(engine.history_state(), (false, false), "no step was added");
    let (sizes, _) = engine.page_sizes_lenient();
    assert_eq!(sizes.len(), 3);
}

/// A duplicated page brings its own copy of its fonts. An unembedded standard font (Helvetica
/// here) has to come from MuPDF's built-in copy, not from a system font search: on Windows that
/// search took 1.2 s for the copy alone (Plan 0005 §3.8).
#[test]
fn a_duplicated_page_with_a_standard_font_draws_at_once() {
    let path = scratch("pages_font.pdf");
    std::fs::write(&path, text_pdf(2)).expect("write fixture");
    let mut engine = open(&path);
    engine.render_thumb(0, 64).expect("first page");
    engine
        .edit(&EditOp::Pages(PageOp::Duplicate { pages: vec![0] }))
        .expect("duplicate");
    let start = std::time::Instant::now();
    engine.render_thumb(1, 64).expect("the copy");
    let ms = start.elapsed().as_millis();
    assert!(ms < 500, "the copy's display list took {ms} ms");
}

/// Writes the documents `--bench-pages` runs on (Plan 0005 Phase 1, rows AA and AB): 200 and 1000
/// pages, each 612 points wide and 800 + its number tall, so the page order can be read from the
/// page sizes alone.
#[test]
#[ignore = "writes bench documents, run by hand"]
fn write_pages_bench_documents() {
    for pages in [200, 1000] {
        let bytes = text_pdf(pages);
        let doc = mupdf::pdf::PdfDocument::from_bytes(&bytes).expect("open");
        for p in 0..pages as i32 {
            let page = doc.load_pdf_page(p).expect("page");
            let height = 800.0 + p as f32;
            page.object()
                .dict_put(
                    "MediaBox",
                    doc.new_object_from_str(&format!("[0 0 612 {height}]"))
                        .expect("box"),
                )
                .expect("media box");
        }
        let path = scratch(&format!("pages_bench_{pages}.pdf"));
        doc.save(path.to_str().expect("utf-8")).expect("save");
        println!("{}", path.display());
    }
}
