// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Text selection through the engine (Plan 0003, §4.5, row N).
//!
//! The document is generated here so the expected text is known line by line: page `p`, line `l`
//! reads `p{p}l{l} alpha beta`, 10 pt Helvetica, baselines 12 pt apart from 42 pt below the top.

use engine_mupdf::EngineHandle;
use pdf_core::select::{PagePoint, SelectMode};
use pdf_core::{EngineCmd, EngineEvent};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Duration;

const LINES: usize = 50;

fn line_text(page: usize, line: usize) -> String {
    format!("p{page}l{line} alpha beta")
}

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
            let _ = writeln!(content, "({}) Tj T*", line_text(p, l));
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

/// A point inside line `l` of `page` (just above its baseline), at `x` points from the left.
fn on_line(page: u32, line: usize, x: f32) -> PagePoint {
    PagePoint {
        page,
        x,
        y: 42.0 + 12.0 * line as f32 - 3.0,
    }
}

fn next(rx: &Receiver<EngineEvent>) -> EngineEvent {
    rx.recv_timeout(Duration::from_secs(60))
        .expect("the engine answers within a minute")
}

fn open(pages: usize) -> (EngineHandle, Receiver<EngineEvent>) {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("select_{pages}p.pdf"));
    std::fs::write(&path, text_pdf(pages)).expect("write fixture");
    let (handle, rx) = EngineHandle::spawn();
    handle
        .send(EngineCmd::Open {
            path,
            password: None,
        })
        .expect("actor is alive");
    assert!(matches!(next(&rx), EngineEvent::Opened { .. }));
    assert!(matches!(next(&rx), EngineEvent::Outline { .. }));
    (handle, rx)
}

fn select(
    handle: &EngineHandle,
    rx: &Receiver<EngineEvent>,
    from: PagePoint,
    to: PagePoint,
    mode: SelectMode,
    generation: u64,
) -> (Vec<u32>, String) {
    handle
        .send(EngineCmd::Select {
            from,
            to,
            mode,
            generation,
        })
        .expect("send");
    loop {
        if let EngineEvent::Selection {
            generation: g,
            pages,
            text,
        } = next(rx)
            && g == generation
        {
            return (pages.iter().map(|p| p.page).collect(), text);
        }
    }
}

#[test]
fn a_selection_across_three_pages_copies_exactly_the_lines_it_covers() {
    // Plan 0003 §6 row N: from the start of page 1's line 48 to the end of page 3's line 1.
    let (handle, rx) = open(3);
    let (pages, text) = select(
        &handle,
        &rx,
        on_line(0, 48, 20.0),
        on_line(2, 1, 600.0),
        SelectMode::Chars,
        1,
    );
    let mut expected = vec![line_text(0, 48), line_text(0, 49)];
    expected.extend((0..LINES).map(|l| line_text(1, l)));
    expected.extend([line_text(2, 0), line_text(2, 1)]);
    assert_eq!(pages, vec![0, 1, 2]);
    assert_eq!(text, expected.join("\n"));
}

#[test]
fn words_and_lines_snap_and_a_drag_inside_a_word_selects_part_of_it() {
    let (handle, rx) = open(1);
    // "p0l7 alpha beta": the word "alpha" starts after "p0l7 " (5 characters of 10 pt Helvetica,
    // about 25 pt), so x = 54 + 40 is inside it.
    let inside = on_line(0, 7, 94.0);
    let (_, word) = select(&handle, &rx, inside, inside, SelectMode::Words, 1);
    assert_eq!(word, "alpha");
    let (_, line) = select(&handle, &rx, inside, inside, SelectMode::Lines, 2);
    assert_eq!(line, line_text(0, 7));
    let (_, nothing) = select(&handle, &rx, inside, inside, SelectMode::Chars, 3);
    assert_eq!(nothing, "", "a click without a drag selects nothing");
}

#[test]
fn a_selection_is_answered_while_a_long_search_runs() {
    // Selection is served between two pages of a search, not after it.
    let (handle, rx) = open(400);
    handle
        .send(EngineCmd::Search {
            query: "beta".into(),
            from_page: 0,
            generation: 1,
        })
        .expect("send");
    handle
        .send(EngineCmd::Select {
            from: on_line(5, 0, 20.0),
            to: on_line(5, 0, 600.0),
            mode: SelectMode::Chars,
            generation: 9,
        })
        .expect("send");
    let mut search_done_first = false;
    loop {
        match next(&rx) {
            EngineEvent::Selection {
                generation: 9,
                text,
                ..
            } => {
                assert_eq!(text, line_text(5, 0));
                break;
            }
            EngineEvent::SearchDone { .. } => search_done_first = true,
            EngineEvent::SearchHits { .. } => {}
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert!(
        !search_done_first,
        "the selection waited for the whole search"
    );
}
