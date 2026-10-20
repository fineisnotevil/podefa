// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Search through the engine actor and its search thread (Plan 0003, §3.6, rows L and M).

use engine_mupdf::EngineHandle;
use pdf_core::{EngineCmd, EngineEvent};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Duration;

/// `pages` Letter pages of Helvetica text; `podefa` once on every page (line 25) and `rare` only on
/// page 0.
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
        for line in 0..50 {
            let text = match (line, p) {
                (25, _) => "lorem ipsum podefa dolor",
                (30, 0) => "a rare word",
                _ => "lorem ipsum dolor sit amet consectetur",
            };
            let _ = writeln!(content, "({text}) Tj T*");
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

fn next(rx: &Receiver<EngineEvent>) -> EngineEvent {
    rx.recv_timeout(Duration::from_secs(60))
        .expect("the engine answers within a minute")
}

fn open_text(pages: usize) -> (EngineHandle, Receiver<EngineEvent>) {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("search_{pages}p.pdf"));
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

/// Every `SearchHits` page of `generation` up to its `SearchDone`, in arrival order.
fn collect(rx: &Receiver<EngineEvent>, generation: u64) -> (Vec<(u32, usize)>, u32) {
    let mut pages = Vec::new();
    loop {
        match next(rx) {
            EngineEvent::SearchHits {
                generation: g,
                page,
                hits,
            } => {
                assert_eq!(g, generation, "a hit of another search");
                pages.push((page, hits.len()));
            }
            EngineEvent::SearchDone {
                generation: g,
                pages: searched,
                ..
            } => {
                assert_eq!(g, generation);
                return (pages, searched);
            }
            other => panic!("unexpected event {other:?}"),
        }
    }
}

#[test]
fn a_search_starts_at_the_readers_page_and_wraps() {
    let (handle, rx) = open_text(20);
    handle
        .send(EngineCmd::Search {
            query: "PODEFA".into(),
            from_page: 15,
            generation: 1,
        })
        .expect("send");
    let (pages, searched) = collect(&rx, 1);
    assert_eq!(searched, 20);
    let order: Vec<u32> = pages.iter().map(|(p, _)| *p).collect();
    let expected: Vec<u32> = (15..20).chain(0..15).collect();
    assert_eq!(order, expected, "from page 15, wrapping, case-insensitive");
    assert!(pages.iter().all(|(_, n)| *n == 1), "one hit a page");
}

#[test]
fn hit_rectangles_are_in_page_space() {
    let (handle, rx) = open_text(1);
    handle
        .send(EngineCmd::Search {
            query: "rare".into(),
            from_page: 0,
            generation: 7,
        })
        .expect("send");
    let EngineEvent::SearchHits { hits, .. } = next(&rx) else {
        panic!("expected hits");
    };
    let rect = hits[0][0];
    // Line 30 of the text block: the baseline is 750 - 30 x 12 = 390 pt above the bottom, so the
    // word sits about 402 pt below the top of the 792 pt page, 54 pt or more from the left.
    assert!(rect.x >= 54.0 && rect.x < 120.0, "{rect:?}");
    assert!(rect.y > 390.0 && rect.y < 405.0, "{rect:?}");
    assert!(rect.height > 5.0 && rect.height < 15.0, "{rect:?}");
}

#[test]
fn a_new_search_stops_the_old_one_and_no_old_hit_follows() {
    // Plan 0003 §6 row M, at the engine: once the first event of search 2 has arrived, nothing of
    // search 1 may follow, and search 1 never reports itself finished.
    let (handle, rx) = open_text(400);
    handle
        .send(EngineCmd::Search {
            query: "podefa".into(),
            from_page: 0,
            generation: 1,
        })
        .expect("send");
    // Let search 1 get going, then replace it.
    let first = next(&rx);
    assert!(matches!(
        first,
        EngineEvent::SearchHits { generation: 1, .. }
    ));
    handle
        .send(EngineCmd::Search {
            query: "rare".into(),
            from_page: 0,
            generation: 2,
        })
        .expect("send");
    let mut old_after_new = 0;
    let mut seen_new = false;
    let mut old_hits = 1;
    loop {
        match next(&rx) {
            EngineEvent::SearchHits { generation: 1, .. } => {
                old_hits += 1;
                if seen_new {
                    old_after_new += 1;
                }
            }
            EngineEvent::SearchHits { generation: 2, .. } => seen_new = true,
            EngineEvent::SearchDone { generation: 1, .. } => {
                panic!("a superseded search reported itself done")
            }
            EngineEvent::SearchDone {
                generation: 2,
                pages,
                ..
            } => {
                assert_eq!(pages, 400);
                break;
            }
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert!(seen_new);
    assert_eq!(old_after_new, 0);
    assert!(old_hits < 400, "search 1 was stopped, not run to the end");
}

#[test]
fn cancel_stops_a_search_without_starting_another() {
    let (handle, rx) = open_text(400);
    handle
        .send(EngineCmd::Search {
            query: "podefa".into(),
            from_page: 0,
            generation: 3,
        })
        .expect("send");
    assert!(matches!(next(&rx), EngineEvent::SearchHits { .. }));
    handle.send(EngineCmd::CancelSearch).expect("send");
    // Whatever was already in flight may still arrive, but the search ends without a SearchDone.
    let mut hits = 1;
    while let Ok(event) = rx.recv_timeout(Duration::from_millis(500)) {
        match event {
            EngineEvent::SearchHits { generation: 3, .. } => hits += 1,
            EngineEvent::SearchDone { .. } => panic!("a cancelled search reported itself done"),
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert!(hits < 400, "the search was stopped ({hits} pages of hits)");
}
