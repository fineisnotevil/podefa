// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Phase 0 baseline and Phase 1 tiled measurement harness (ignored by default; not part of CI).
//!
//! `bench_full_page_render` reproduces the per-zoom-click cost of the **old** full-page rasterizer:
//! for a given PDF it renders the same page at several zoom scales and reports wall-clock time and
//! the size of the RGBA8 buffer the engine returns to the UI. It is kept as the "before" column.
//!
//! `bench_tiled_zoom` measures the **Phase 1** path: for each zoom step it renders only the tiles a
//! 900x700 viewport needs, reusing the engine's display list and scratch raster, and reports the
//! per-step time, the bytes rendered per step and the bytes retained by the tile cache.
//!
//! Environment knobs:
//! - `FA_PDF_BENCH_PDF`: fixture to measure (default `large_200p.pdf`).
//! - `FA_PDF_BENCH_SCALES`: comma-separated zoom scales (default `1,2,4,10`).
//! - `FA_PDF_BENCH_MAX_MB`: skip a scale whose full-page buffer would exceed
//!   this many MiB (default `1024`), to avoid OOM.
//!
//! Run: `cargo test --release -p engine-mupdf --test render_bench -- --ignored --nocapture`

use engine_mupdf::MupdfEngine;
use pdf_core::{PageGeometry, PdfEngine, TileKey, TileScheduler};
use std::path::PathBuf;
use std::time::Instant;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn bytes_for(size: pdf_core::Rect, scale: f32) -> u64 {
    let w = (size.width * scale).ceil() as u64;
    let h = (size.height * scale).ceil() as u64;
    w * h * 4
}

#[test]
#[ignore = "phase 0 baseline measurement; run explicitly with --release --ignored --nocapture"]
fn bench_full_page_render() {
    let path = std::env::var("FA_PDF_BENCH_PDF")
        .map(PathBuf::from)
        .unwrap_or_else(|_| fixture_path("large_200p.pdf"));
    let scales: Vec<f32> = std::env::var("FA_PDF_BENCH_SCALES")
        .unwrap_or_else(|_| "1,2,4,10".into())
        .split(',')
        .map(|s| s.trim().parse().expect("invalid FA_PDF_BENCH_SCALES entry"))
        .collect();
    let max_mb: f64 = std::env::var("FA_PDF_BENCH_MAX_MB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1024.0);

    let mut engine = MupdfEngine::new();
    engine
        .open_document(&path, None)
        .expect("open benchmark pdf");
    let size = engine.page_size(0).expect("page size");
    println!(
        "\nfixture={} page0={:.0}x{:.0} pt ({:.2}x{:.2} in)",
        path.file_name().unwrap().to_string_lossy(),
        size.width,
        size.height,
        size.width / 72.0,
        size.height / 72.0,
    );
    println!(
        "{:>6}  {:>11}  {:>10}  {:>10}  {:>10}",
        "scale", "pixels", "RGBA8", "cold", "warm"
    );

    for scale in scales {
        let mb = bytes_for(size, scale) as f64 / (1024.0 * 1024.0);
        if mb > max_mb {
            println!(
                "{scale:>5}x  SKIPPED: full-page buffer would be {mb:.0} MiB (> {max_mb:.0} MiB)"
            );
            continue;
        }

        let t0 = Instant::now();
        let bmp = match engine.render_page(0, scale) {
            Ok(b) => b,
            Err(e) => {
                println!("{scale:>5}x  FAILED: {e}");
                continue;
            }
        };
        let cold_ms = t0.elapsed().as_secs_f64() * 1000.0;

        let t1 = Instant::now();
        let _ = engine.render_page(0, scale).expect("warm render");
        let warm_ms = t1.elapsed().as_secs_f64() * 1000.0;

        println!(
            "{scale:>5}x  {:>5}x{:<5}  {:>7.2} MiB  {cold_ms:>7.1} ms  {warm_ms:>7.1} ms",
            bmp.width,
            bmp.height,
            bmp.data.len() as f64 / (1024.0 * 1024.0),
        );
    }

    println!("\nAnalytical full-page cost (RGBA8, one buffer, and 2x for pixmap + UI copy):");
    for scale in [1.0_f32, 4.0, 10.0, 64.0] {
        let mb = bytes_for(size, scale) as f64 / (1024.0 * 1024.0);
        println!(
            "  zoom {:>5.0}%: {:>9.1} MiB/buffer, {:>9.1} MiB for two",
            scale * 100.0,
            mb,
            mb * 2.0
        );
    }
}

/// Viewport measured by the tiled benchmarks, in device pixels at a scale factor of 1: the same
/// window the plan's budget table assumes.
const VIEW_W: f32 = 900.0;
const VIEW_H: f32 = 700.0;

/// Desktop tile cache budget used by the benchmarks.
const CACHE_BYTES: usize = pdf_core::scheduler::TILE_CACHE_MAX_BYTES;

fn bench_scales() -> (PathBuf, Vec<u32>) {
    let path = std::env::var("FA_PDF_BENCH_PDF")
        .map(PathBuf::from)
        .unwrap_or_else(|_| fixture_path("large_200p.pdf"));
    let scales: Vec<u32> = std::env::var("FA_PDF_BENCH_SCALES")
        .unwrap_or_else(|_| "1000,2000,4000,10000,64000".into())
        .split(',')
        .map(|s| s.trim().parse().expect("invalid FA_PDF_BENCH_SCALES entry"))
        .collect();
    (path, scales)
}

/// The tiles a 900x700 viewport needs, with the viewport centred on the page.
fn viewport_tiles(geo: &PageGeometry) -> Vec<TileKey> {
    let (device_w, device_h) = geo.device_size();
    let offset_x = (device_w as f32 / 2.0 - VIEW_W / 2.0).max(0.0);
    let offset_y = (device_h as f32 / 2.0 - VIEW_H / 2.0).max(0.0);
    geo.visible_cells(pdf_core::Rect::new(offset_x, offset_y, VIEW_W, VIEW_H))
        .map(|(col, row)| geo.key(col, row))
        .collect()
}

fn render_keys(
    engine: &mut MupdfEngine,
    geo: &PageGeometry,
    keys: &[TileKey],
) -> (std::time::Duration, usize) {
    let started = Instant::now();
    let mut bytes = 0usize;
    for key in keys {
        let tile = engine
            .render_tile(geo, key.col, key.row)
            .expect("render tile");
        bytes += tile.data.len();
    }
    (started.elapsed(), bytes)
}

/// Phase 1: cost of one zoom step with viewport-only tiling, at each scale.
#[test]
#[ignore = "phase 1 measurement; run explicitly with --release --ignored --nocapture"]
fn bench_tiled_zoom() {
    let (path, scales) = bench_scales();
    let mut engine = MupdfEngine::new();
    engine
        .open_document(&path, None)
        .expect("open benchmark pdf");
    let size = engine.page_size(0).expect("page size");

    println!(
        "\ntiled zoom step, {} page0={:.0}x{:.0} pt, viewport {VIEW_W:.0}x{VIEW_H:.0} device px",
        path.file_name().unwrap().to_string_lossy(),
        size.width,
        size.height,
    );
    println!(
        "{:>7}  {:>5}  {:>11}  {:>10}  {:>10}  {:>10}",
        "zoom", "tiles", "page px", "cold ms", "warm ms", "MiB/step"
    );

    // The display list is built once per page and then reused by every tile at every zoom level,
    // so its build time is a one-off first-tile cost worth measuring on its own. This uses a
    // separate document handle, so it does not warm the engine under test.
    {
        let doc = mupdf::Document::open(path.to_str().expect("utf-8 path")).expect("open");
        let page = doc.load_page(0).expect("load page");
        let mut timings = Vec::new();
        for _ in 0..2 {
            let started = Instant::now();
            let _list = page.to_display_list(false).expect("display list");
            timings.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        let started = Instant::now();
        let _full = page
            .to_pixmap(
                &mupdf::Matrix::new_scale(1.0, 1.0),
                &mupdf::Colorspace::device_rgb(),
                true,
                false,
            )
            .expect("full page");
        let full_ms = started.elapsed().as_secs_f64() * 1000.0;
        println!(
            "one-off per-page costs: display list {:.1} ms then {:.1} ms, first full-page render \
             {full_ms:.1} ms",
            timings[0], timings[1]
        );
    }

    for scale_milli in scales {
        let geo = PageGeometry::new(0, size, scale_milli);
        let keys = viewport_tiles(&geo);
        if keys.is_empty() {
            println!("{scale_milli:>6}%  no tiles intersect the viewport");
            continue;
        }
        let (device_w, device_h) = geo.device_size();
        let (cold, cold_bytes) = render_keys(&mut engine, &geo, &keys);
        let (warm, _) = render_keys(&mut engine, &geo, &keys);
        println!(
            "{:>6}%  {:>5}  {:>11}  {:>10.1}  {:>10.1}  {:>10.2}",
            scale_milli / 10,
            keys.len(),
            format!("{device_w}x{device_h}"),
            cold.as_secs_f64() * 1000.0,
            warm.as_secs_f64() * 1000.0,
            cold_bytes as f64 / (1024.0 * 1024.0),
        );
    }
}

/// Twenty zoom clicks inside one second, on both fixtures, through the real tile cache.
///
/// This is the automated half of the rapid-zoom acceptance test: it reports wall-clock time, the
/// bytes rasterized and the peak cache residency, and asserts the budget and capacity invariants.
/// The GUI responsiveness half remains a manual check, since the app has no scriptable input.
#[test]
#[ignore = "phase 1 measurement; run explicitly with --release --ignored --nocapture"]
fn bench_twenty_zoom_clicks_in_one_second() {
    for fixture in ["large_200p.pdf", "large_format_a0.pdf"] {
        let path = fixture_path(fixture);
        let mut engine = MupdfEngine::new();
        engine
            .open_document(&path, None)
            .expect("open benchmark pdf");
        let size = engine.page_size(0).expect("page size");

        let capacity = 16;
        let mut scheduler = TileScheduler::new(capacity, CACHE_BYTES);
        let mut scale_milli = 1_000u32;
        let mut rasterized_bytes = 0usize;
        let mut peak_cache = 0usize;
        let mut slowest = 0f64;

        // Warm up exactly what the first click of a real session would pay: page load, the one-off
        // MuPDF initialization and the display list build. Phase 0 measured the same one-off at
        // A0 100% (see benchmarks/0001 section 5), and it is not a per-click cost.
        let first_tile = Instant::now();
        let first_geo = PageGeometry::new(0, size, scale_milli);
        let _ = engine.render_tile(&first_geo, 0, 0).expect("warm-up tile");
        let first_tile_ms = first_tile.elapsed().as_secs_f64() * 1000.0;

        let started = Instant::now();
        for _ in 0..20 {
            scale_milli = pdf_core::zoom_in_milli(scale_milli);
            let geo = PageGeometry::new(0, size, scale_milli);
            let desired = viewport_tiles(&geo);

            let step_started = Instant::now();
            for action in scheduler.update_view(&desired, scale_milli) {
                let pdf_core::TileAction::Request { key } = action else {
                    continue;
                };
                let tile = engine
                    .render_tile(&geo, key.col, key.row)
                    .expect("render tile");
                rasterized_bytes += tile.data.len();
                let outcome = scheduler.insert(key, tile.data.len());
                for evicted in outcome.actions {
                    if let pdf_core::TileAction::Release { slot } = evicted {
                        assert!(slot < capacity, "released row out of range");
                    }
                }
                assert!(outcome.slot.is_some(), "no row for a requested tile");
            }
            slowest = slowest.max(step_started.elapsed().as_secs_f64() * 1000.0);
            peak_cache = peak_cache.max(scheduler.cache_bytes());
            assert!(
                scheduler.cache_bytes() <= CACHE_BYTES,
                "cache budget exceeded"
            );
            assert!(scheduler.cache_len() <= capacity, "model capacity exceeded");
        }
        let elapsed = started.elapsed();

        println!(
            "{fixture}: first tile {first_tile_ms:.0} ms, then 20 clicks in {:.0} ms (slowest step \
             {slowest:.1} ms), final zoom {}%, rasterized {:.1} MiB, peak cache {:.1} MiB of {} MiB, \
             dropped {}",
            elapsed.as_secs_f64() * 1000.0,
            scale_milli / 10,
            rasterized_bytes as f64 / (1024.0 * 1024.0),
            peak_cache as f64 / (1024.0 * 1024.0),
            CACHE_BYTES / (1024 * 1024),
            scheduler.dropped(),
        );
    }
}
