// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Plan 0003 Phase 0 measurement: what text extraction and search cost (ignored by default; not
//! part of CI).
//!
//! v0.1's search, selection and copy all start from MuPDF's structured text (`fz_stext_page`), so
//! the question is what one costs per page and how long a whole-document search takes. Per page,
//! on a freshly opened document:
//!
//! - **stext**: `Page::to_text_page` (load + text extraction) - what search and selection need;
//! - **search**: `TextPage::search` for a word that occurs on every page, and for one that occurs
//!   nowhere (the full-scan case);
//! - **chars**: walking every block, line and char of the text page, i.e. what a Rust-side
//!   selection-to-text would iterate (Plan 0003 §3).
//!
//! The document is generated here: `pages` Letter pages of 50 lines of Helvetica text each (about
//! 3,700 characters a page), uncompressed. Real documents differ (fonts, compressed streams, CJK),
//! so `FA_PDF_BENCH_PDF` measures a real file too.
//!
//! Environment knobs:
//! - `FA_PDF_BENCH_PAGES`: comma-separated page counts (default `200,1000`).
//! - `FA_PDF_BENCH_PDF`: also measure this file.
//!
//! Run: `cargo test --release -p engine-mupdf --test text_bench -- --ignored --nocapture`

use mupdf::{Document, TextPageFlags};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

const WORDS: &[&str] = &[
    "lorem",
    "ipsum",
    "dolor",
    "sit",
    "amet",
    "consectetur",
    "adipiscing",
    "elit",
    "sed",
    "do",
    "eiusmod",
    "tempor",
    "incididunt",
    "ut",
    "labore",
    "et",
    "dolore",
    "magna",
    "aliqua",
];

/// `pages` pages of text, every page containing the word `podefa` once.
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
            let mut text = String::new();
            for w in 0..12 {
                text.push_str(WORDS[(p * 7 + line * 13 + w) % WORDS.len()]);
                text.push(' ');
            }
            if line == 25 {
                text.push_str("podefa");
            }
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
    let mut offsets = Vec::with_capacity(objects.len());
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

fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1e3
}

fn measure(path: &Path, label: &str, hit: &str) {
    let doc = Document::open(path.to_str().expect("utf-8 path")).expect("open");
    let count = doc.page_count().expect("page count").max(0) as usize;

    let (mut stext_ms, mut hit_ms, mut miss_ms, mut chars_ms) = (0.0, 0.0, 0.0, 0.0);
    let (mut hits, mut chars, mut worst_stext) = (0usize, 0usize, 0.0f64);
    let whole = Instant::now();
    for i in 0..count {
        let t = Instant::now();
        let page = doc.load_page(i as i32).expect("load page");
        let text = page
            .to_text_page(TextPageFlags::empty())
            .expect("text page");
        let one = ms(t);
        stext_ms += one;
        worst_stext = worst_stext.max(one);

        let t = Instant::now();
        hits += text.search(hit).expect("search").len();
        hit_ms += ms(t);

        let t = Instant::now();
        let none = text.search("zzqqxx").expect("search");
        miss_ms += ms(t);
        assert!(none.is_empty());

        let t = Instant::now();
        for block in text.blocks() {
            for line in block.lines() {
                chars += line.chars().filter(|c| c.char().is_some()).count();
            }
        }
        chars_ms += ms(t);
    }
    let total = ms(whole);
    let n = count.max(1) as f64;
    println!(
        "{label:<26} pages={count:>5} chars/page={:>5}  stext={:>6.3} ms/page (worst {:>6.2})  \
         search hit={:>6.3} miss={:>6.3} ms/page  chars walk={:>6.3} ms/page  hits={hits}  \
         whole-document stext+search={total:>8.1} ms",
        chars / count.max(1),
        stext_ms / n,
        worst_stext,
        hit_ms / n,
        miss_ms / n,
        chars_ms / n,
    );
}

#[test]
#[ignore = "plan 0003 phase 0 measurement; run explicitly with --release --ignored --nocapture"]
fn bench_text_extraction_and_search() {
    let counts: Vec<usize> = std::env::var("FA_PDF_BENCH_PAGES")
        .unwrap_or_else(|_| "200,1000".into())
        .split(',')
        .map(|s| s.trim().parse().expect("invalid FA_PDF_BENCH_PAGES entry"))
        .collect();
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    for &pages in &counts {
        let path = dir.join(format!("synthetic_text_{pages}p.pdf"));
        std::fs::write(&path, text_pdf(pages)).expect("write fixture");
        measure(&path, &format!("synthetic text {pages}p"), "podefa");
    }
    if let Ok(path) = std::env::var("FA_PDF_BENCH_PDF") {
        let path = PathBuf::from(path);
        let label = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        measure(&path, &label, "the");
    }
}
