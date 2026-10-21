// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Editing and saving through the engine actor (Plan 0004 Phase 1, rows P and Q).
//!
//! The documents are generated here, so every line's position is known: page `p`, line `l` reads
//! `p{p}l{l} alpha beta`, 10 pt Helvetica, baselines 12 pt apart from 42 pt below the top. The
//! in-place save runs on every CI platform: appending to a file that is open is the one save
//! behaviour measured only on Windows in Phase 0 (`docs/benchmarks.md` §10.1).

use engine_mupdf::EngineHandle;
use mupdf::pdf::{PdfDocument, PdfPage};
use pdf_core::annot::{AnnotKind, Colour, EditOp, MarkupKind, PALETTE};
use pdf_core::select::{PagePoint, PageSelection, SelectMode};
use pdf_core::{EngineCmd, EngineEvent, PageGeometry, Rect, SaveError};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::Duration;

const LINES: usize = 50;

fn text_pdf(pages: usize) -> Vec<u8> {
    let mut objects: Vec<String> = Vec::new();
    objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
    let kids: String = (0..pages).map(|p| format!("{} 0 R ", 4 + 2 * p)).collect();
    objects.push(format!(
        "<< /Type /Pages /Kids [{kids}] /Count {pages} /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 3 0 R >> >> >>"
    ));
    objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());
    for p in 0..pages {
        let mut content = String::from("BT /F1 10 Tf 12 TL 54 750 Td\n");
        for l in 0..LINES {
            let _ = writeln!(content, "(p{p}l{l} alpha beta) Tj T*");
        }
        content.push_str("ET");
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /Contents {} 0 R >>",
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

fn on_line(page: u32, line: usize, x: f32) -> PagePoint {
    PagePoint {
        page,
        x,
        y: 42.0 + 12.0 * line as f32 - 3.0,
    }
}

/// The next event `pick` accepts, skipping the others; panics on an engine error.
fn wait<T>(rx: &Receiver<EngineEvent>, mut pick: impl FnMut(EngineEvent) -> Option<T>) -> T {
    loop {
        let event = rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the engine answers within a minute");
        if let EngineEvent::Error { message } = &event {
            panic!("engine reported: {message}");
        }
        if let Some(found) = pick(event) {
            return found;
        }
    }
}

/// Opens `path` and returns whether the engine calls it editable.
fn open(handle: &EngineHandle, rx: &Receiver<EngineEvent>, path: &Path) -> bool {
    handle
        .send(EngineCmd::Open {
            path: path.to_path_buf(),
            password: None,
        })
        .expect("actor is alive");
    wait(rx, |e| match e {
        EngineEvent::Opened { editable, .. } => Some(editable),
        _ => None,
    })
}

fn edit(handle: &EngineHandle, rx: &Receiver<EngineEvent>, op: EditOp, edit_id: u64) -> EditSeen {
    handle
        .send(EngineCmd::Edit {
            op,
            edit_id,
            generation: 1,
        })
        .expect("send");
    let mut changed = None;
    let result = wait(rx, |e| match e {
        EngineEvent::PageChanged {
            page,
            revision,
            items,
            ..
        } => {
            changed = Some((page, revision, items));
            None
        }
        EngineEvent::Edited {
            edit_id: id,
            result,
            ..
        } if id == edit_id => Some(result),
        _ => None,
    });
    EditSeen { changed, result }
}

struct EditSeen {
    changed: Option<(u32, u32, Vec<pdf_core::annot::AnnotInfo>)>,
    result: Result<(), String>,
}

fn save(
    handle: &EngineHandle,
    rx: &Receiver<EngineEvent>,
    path: Option<PathBuf>,
) -> (PathBuf, Result<u64, SaveError>) {
    handle.send(EngineCmd::Save { path }).expect("send");
    wait(rx, |e| match e {
        EngineEvent::Saved { path, result } => Some((path, result)),
        _ => None,
    })
}

/// A tile of page 0 at 100%, with the revision it was drawn from.
fn tile(
    handle: &EngineHandle,
    rx: &Receiver<EngineEvent>,
    request_id: u64,
) -> (u32, pdf_core::Bitmap) {
    let geometry = PageGeometry::new(0, Rect::new(0.0, 0.0, 612.0, 792.0), 1000);
    handle
        .send(EngineCmd::RenderTile {
            geometry,
            col: 0,
            row: 0,
            request_id,
        })
        .expect("send");
    wait(rx, |e| match e {
        EngineEvent::TileRendered {
            request_id: id,
            revision,
            bitmap,
            ..
        } if id == request_id => Some((revision, bitmap)),
        _ => None,
    })
}

fn rgb(bitmap: &pdf_core::Bitmap, x: u32, y: u32) -> [u8; 3] {
    let at = y as usize * bitmap.stride + x as usize * 3;
    [bitmap.data[at], bitmap.data[at + 1], bitmap.data[at + 2]]
}

/// The top margin of page 0: blank paper above the first line.
const MARGIN: Rect = Rect::new(60.0, 6.0, 120.0, 20.0);
const RED: Colour = Colour::new(1.0, 0.0, 0.0);

#[test]
fn an_edit_gives_the_page_a_new_revision_and_the_next_tile_shows_it() {
    let path = scratch("edit_revision.pdf");
    std::fs::write(&path, text_pdf(3)).expect("write fixture");
    let (handle, rx) = EngineHandle::spawn();
    assert!(open(&handle, &rx, &path));

    let (revision, before) = tile(&handle, &rx, 1);
    assert_eq!(revision, 0);
    assert_eq!(rgb(&before, 90, 16), [255, 255, 255]);

    let seen = edit(
        &handle,
        &rx,
        EditOp::AddMarkup {
            page: 0,
            kind: MarkupKind::Highlight,
            rects: vec![MARGIN],
            colour: RED,
            author: None,
        },
        10,
    );
    assert_eq!(seen.result, Ok(()));
    let (page, revision, items) = seen.changed.expect("PageChanged before Edited");
    assert_eq!((page, revision), (0, 1));
    assert_eq!(items.len(), 1);
    let id = items[0].id;
    assert_eq!(items[0].kind, AnnotKind::Highlight);
    assert_eq!(items[0].quads.len(), 1);
    let colour = items[0].colour.expect("a highlight has a colour");
    assert!(colour.r > 0.99 && colour.g < 0.01 && colour.b < 0.01);

    let (revision, after) = tile(&handle, &rx, 2);
    assert_eq!(revision, 1);
    let px = rgb(&after, 90, 16);
    assert!(
        px[0] > 200 && px[1] < 80 && px[2] < 80,
        "highlight drawn: {px:?}"
    );

    // Recolour, then delete: each is one more revision, and the list follows.
    let seen = edit(
        &handle,
        &rx,
        EditOp::SetColour {
            page: 0,
            id,
            colour: PALETTE[2],
        },
        11,
    );
    assert_eq!(seen.result, Ok(()));
    let (_, revision, items) = seen.changed.expect("changed");
    assert_eq!(revision, 2);
    let colour = items[0].colour.expect("colour");
    assert!((colour.b - PALETTE[2].b).abs() < 0.01);

    let seen = edit(&handle, &rx, EditOp::Delete { page: 0, id }, 12);
    assert_eq!(seen.result, Ok(()));
    let (_, revision, items) = seen.changed.expect("changed");
    assert_eq!(revision, 3);
    assert!(items.is_empty());
    let (_, gone) = tile(&handle, &rx, 3);
    assert_eq!(rgb(&gone, 90, 16), [255, 255, 255]);

    // An edit naming an annotation that is gone fails, and changes nothing.
    let seen = edit(&handle, &rx, EditOp::Delete { page: 0, id }, 13);
    assert!(seen.result.is_err());
    assert!(seen.changed.is_none());
}

#[test]
fn a_highlight_across_two_pages_survives_an_in_place_save() {
    let path = scratch("edit_round_trip.pdf");
    let original = text_pdf(3);
    std::fs::write(&path, &original).expect("write fixture");
    let (handle, rx) = EngineHandle::spawn();
    assert!(open(&handle, &rx, &path));

    // Row Q: select from line 48 of page 0 to line 1 of page 1, then mark each page's part.
    handle
        .send(EngineCmd::Select {
            from: on_line(0, 48, 54.0),
            to: on_line(1, 1, 400.0),
            mode: SelectMode::Chars,
            generation: 1,
        })
        .expect("send");
    let pages: Vec<PageSelection> = wait(&rx, |e| match e {
        EngineEvent::Selection { pages, .. } => Some(pages),
        _ => None,
    });
    assert_eq!(pages.iter().map(|p| p.page).collect::<Vec<_>>(), [0, 1]);
    for (n, part) in pages.iter().enumerate() {
        let seen = edit(
            &handle,
            &rx,
            EditOp::AddMarkup {
                page: part.page,
                kind: MarkupKind::Highlight,
                rects: part.rects.clone(),
                colour: PALETTE[0],
                author: None,
            },
            n as u64,
        );
        assert_eq!(seen.result, Ok(()));
    }

    let (saved_to, result) = save(&handle, &rx, None);
    assert_eq!(saved_to, path);
    let bytes = result.expect("in-place save");
    let on_disk = std::fs::read(&path).expect("read back");
    assert_eq!(on_disk.len() as u64, bytes);
    assert!(on_disk.len() > original.len());
    assert_eq!(
        &on_disk[..original.len()],
        &original[..],
        "original bytes kept"
    );

    // Reopened by another reader, each page has one highlight whose quads are the selection's
    // line boxes.
    let reopened = PdfDocument::open(path.to_str().expect("utf-8")).expect("reopen");
    for part in &pages {
        let page = PdfPage::try_from(reopened.load_page(part.page as i32).expect("page"))
            .expect("pdf page");
        let annots: Vec<_> = page.annotations().collect();
        assert_eq!(annots.len(), 1, "page {}", part.page);
        let quads = annots[0].quad_points().expect("quads");
        assert_eq!(quads.len(), part.rects.len());
        for (quad, rect) in quads.iter().zip(&part.rects) {
            assert!((quad.ul.x - rect.x).abs() < 0.01);
            assert!((quad.ul.y - rect.y).abs() < 0.01);
            assert!((quad.lr.x - (rect.x + rect.width)).abs() < 0.01);
            assert!((quad.lr.y - (rect.y + rect.height)).abs() < 0.01);
        }
    }
}

#[test]
fn save_as_switches_to_the_copy() {
    let path = scratch("edit_save_as_source.pdf");
    let copy = scratch("edit_save_as_copy.pdf");
    let _ = std::fs::remove_file(&copy);
    let original = text_pdf(2);
    std::fs::write(&path, &original).expect("write fixture");
    let (handle, rx) = EngineHandle::spawn();
    assert!(open(&handle, &rx, &path));

    let mark = |page| EditOp::AddMarkup {
        page,
        kind: MarkupKind::Underline,
        rects: vec![MARGIN],
        colour: RED,
        author: Some("Tester".into()),
    };
    assert_eq!(edit(&handle, &rx, mark(0), 1).result, Ok(()));
    let (saved_to, result) = save(&handle, &rx, Some(copy.clone()));
    assert_eq!(saved_to, copy);
    result.expect("save as");
    assert_eq!(
        std::fs::read(&path).expect("source"),
        original,
        "source untouched"
    );
    // The copy as the engine opened it: every later in-place save keeps these bytes.
    let at_open = std::fs::read(&copy).expect("copy");

    // The next edit and in-place save go to the copy.
    assert_eq!(edit(&handle, &rx, mark(1), 2).result, Ok(()));
    let before = std::fs::metadata(&copy).expect("copy").len();
    let (saved_to, result) = save(&handle, &rx, None);
    assert_eq!(saved_to, copy);
    assert!(result.expect("in-place save of the copy") > before);
    assert_eq!(
        std::fs::read(&path).expect("source"),
        original,
        "source still untouched"
    );

    let reopened = PdfDocument::open(copy.to_str().expect("utf-8")).expect("reopen");
    for page in 0..2 {
        let pdf_page =
            PdfPage::try_from(reopened.load_page(page).expect("page")).expect("pdf page");
        let annots: Vec<_> = pdf_page.annotations().collect();
        assert_eq!(annots.len(), 1, "page {page}");
        assert_eq!(annots[0].author().expect("author"), Some("Tester"));
    }

    // Save As naming the open file itself saves incrementally instead of rewriting the file
    // being read. MuPDF writes one section holding every change since the file was opened after
    // the bytes it opened (`pdf-write.c`, `ensure_initial_incremental_contents`), so the bytes at
    // open are what survives, and the reopened file has every edit.
    assert_eq!(edit(&handle, &rx, mark(0), 3).result, Ok(()));
    let (_, result) = save(&handle, &rx, Some(copy.clone()));
    result.expect("save as onto itself");
    let after = std::fs::read(&copy).expect("copy");
    assert_eq!(&after[..at_open.len()], &at_open[..]);
    let reopened = PdfDocument::open(copy.to_str().expect("utf-8")).expect("reopen");
    let counts: Vec<usize> = (0..2)
        .map(|page| {
            PdfPage::try_from(reopened.load_page(page).expect("page"))
                .expect("pdf page")
                .annotations()
                .count()
        })
        .collect();
    assert_eq!(counts, [2, 1]);
}

#[test]
fn a_document_that_is_not_a_pdf_is_view_only() {
    let path = scratch("edit_plain.txt");
    std::fs::write(&path, "plain text, not a PDF\n").expect("write fixture");
    let (handle, rx) = EngineHandle::spawn();
    assert!(!open(&handle, &rx, &path));

    handle
        .send(EngineCmd::Annotations {
            page: 0,
            generation: 7,
        })
        .expect("send");
    let items = wait(&rx, |e| match e {
        EngineEvent::Annotations {
            generation: 7,
            items,
            ..
        } => Some(items),
        _ => None,
    });
    assert!(items.is_empty());

    let seen = edit(
        &handle,
        &rx,
        EditOp::AddMarkup {
            page: 0,
            kind: MarkupKind::Highlight,
            rects: vec![MARGIN],
            colour: RED,
            author: None,
        },
        1,
    );
    assert!(seen.result.is_err());
    assert!(seen.changed.is_none());
    let (_, result) = save(&handle, &rx, None);
    assert_eq!(result, Err(SaveError::NotEditable));
}
