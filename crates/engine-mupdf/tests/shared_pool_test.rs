// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! One render pool for several engines (Plan 0007 Phase 4, ADR 0011).
//!
//! Two engines, each with its own document, rasterize on one shared [`RasterPool`]. Each tile must
//! come back on the channel of the engine that asked for it and be byte-identical to the serial
//! render of that engine's document; a `Trim` (the tab went out of view) must not change what the
//! next tile looks like; and dropping one engine must leave the pool working for the other.

use engine_mupdf::{EngineHandle, MupdfEngine, RasterPool};
use pdf_core::{EngineCmd, EngineEvent, PageGeometry, PdfEngine, RequestId};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(60);
/// Long enough for a tile sent to the wrong channel to have arrived.
const QUIET: Duration = Duration::from_millis(500);
const SCALE: u32 = 2_000;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn next(rx: &Receiver<EngineEvent>) -> EngineEvent {
    let event = rx.recv_timeout(TIMEOUT).expect("an engine event");
    if let EngineEvent::Error { message } = &event {
        panic!("engine reported: {message}");
    }
    event
}

/// Opens `name` on the actor and waits for `Opened`, the outline and the history.
fn open(handle: &EngineHandle, rx: &Receiver<EngineEvent>, name: &str) {
    handle
        .send(EngineCmd::Open {
            path: fixture_path(name),
            password: None,
        })
        .expect("actor is alive");
    assert!(matches!(next(rx), EngineEvent::Opened { .. }));
    assert!(matches!(next(rx), EngineEvent::Outline { .. }));
    assert!(matches!(next(rx), EngineEvent::History { .. }));
}

/// The serial render of tile (0, 0) of page 0 of `name`, and its geometry.
fn serial_tile(name: &str) -> (PageGeometry, Vec<u8>) {
    let mut serial = MupdfEngine::new();
    serial
        .open_document(&fixture_path(name), None)
        .expect("open serial");
    let geometry = PageGeometry::new(0, serial.page_size(0).expect("page size"), SCALE);
    let tile = serial.render_tile(&geometry, 0, 0).expect("serial tile");
    (geometry, tile.data)
}

fn ask(handle: &EngineHandle, geometry: &PageGeometry, request_id: RequestId) {
    handle
        .send(EngineCmd::RenderTile {
            geometry: *geometry,
            col: 0,
            row: 0,
            request_id,
        })
        .expect("actor is alive");
}

/// The one tile `request_id` asked for, from `rx`.
fn tile(rx: &Receiver<EngineEvent>, request_id: RequestId) -> Vec<u8> {
    match next(rx) {
        EngineEvent::TileRendered {
            request_id: got,
            bitmap,
            ..
        } => {
            assert_eq!(got, request_id, "a tile of another request on this channel");
            bitmap.data
        }
        other => panic!("expected a tile, got {other:?}"),
    }
}

/// Nothing more arrives on `rx`.
fn quiet(rx: &Receiver<EngineEvent>, which: &str) {
    if let Ok(event) = rx.recv_timeout(QUIET) {
        panic!("{which} got an event nobody asked it for: {event:?}");
    }
}

#[test]
fn engines_on_one_pool_get_their_own_tiles() {
    let (geometry_a, serial_a) = serial_tile("dense_vector.pdf");
    let (geometry_b, serial_b) = serial_tile("large_200p.pdf");
    assert_ne!(
        serial_a, serial_b,
        "the two documents must draw differently"
    );

    let pool = RasterPool::new();
    let (a, rx_a) = EngineHandle::spawn_with(pool.clone());
    let (b, rx_b) = EngineHandle::spawn_with(pool.clone());
    open(&a, &rx_a, "dense_vector.pdf");
    open(&b, &rx_b, "large_200p.pdf");

    // Both at once, so the pool has both in its queue.
    ask(&a, &geometry_a, 1);
    ask(&b, &geometry_b, 2);
    assert_eq!(tile(&rx_a, 1), serial_a, "engine A's tile is not A's page");
    assert_eq!(tile(&rx_b, 2), serial_b, "engine B's tile is not B's page");
    quiet(&rx_a, "engine A");
    quiet(&rx_b, "engine B");

    // A tab out of view drops its display lists; shown again, it draws the same pixels.
    a.send(EngineCmd::Trim).expect("actor is alive");
    ask(&a, &geometry_a, 3);
    assert_eq!(
        tile(&rx_a, 3),
        serial_a,
        "a trimmed engine draws differently"
    );

    // A tab closes: the pool keeps working for the other.
    drop(a);
    ask(&b, &geometry_b, 4);
    assert_eq!(
        tile(&rx_b, 4),
        serial_b,
        "the pool stopped with the first engine"
    );
}
