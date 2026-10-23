<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# CLAUDE.md

Guidance for AI coding agents working in this repository. Read this first, then `README.md`, `docs/adr/`, `docs/plans/`, `docs/benchmarks.md` and `docs/backlog.md` before changing anything.

> Status snapshot: 2026-10-21. Sections marked **(verify)** were reported by earlier agent sessions and should be checked against the repo before relying on them.

## 1. What this project is

A fully open-source, cross-platform PDF viewer and editor (repo: `fa-pdf`).

- **Targets:** Windows 10+, Linux, macOS first; Android and iOS later. Windows 7 is out of scope.
- **Priorities, in order:** light on resources (RAM, binary/installed size, CPU, startup), modern declarative UI, a complete PDF toolset over time.
- **Stack:** Rust + [Slint](https://slint.dev) (declarative UI) + MuPDF via the `mupdf` crate (mupdf-rs, vendored/patched under `patches/mupdf`).
- **License:** AGPL-3.0-or-later (MuPDF is AGPL; Slint is used under its GPLv3 option). Contributions use the DCO (`Signed-off-by:`), no CLA. The repo is REUSE-compliant. Open question for later: App Store distribution vs. GPL-family licenses (decide before iOS work).

## 2. Architecture

Cargo workspace. Dependency direction is a hard rule:

```
app  ──►  core
 │
 └──►  engine-mupdf  ──►  core
```

| Crate | Package | Role |
|---|---|---|
| `crates/core` | `pdf-core` | Engine-agnostic types and pure logic: `PdfEngine` trait, messages (`EngineCmd`/`EngineEvent`), `Bitmap`/`PixelFormat`, tile geometry, zoom math, cache/scheduler logic, layout, navigation, search results, selection, settings. (There is no undo `Command` trait yet; Plan 0004 §3.4.) **Must not depend on `slint` or `mupdf`.** |
| `crates/engine-mupdf` | `engine-mupdf` | The **only** crate that touches `mupdf`. Engine actor thread, display-list cache, tile rasterizer, render worker pool (`pool.rs`). |
| `crates/app` | `app` | Slint UI and binary: Flickable virtual canvas, tile model, base layer, zoom input, bench harness (`bench.rs`). |

(An earlier separate `render` crate was deliberately removed; its pure logic lives in `core`.)

### Rendering pipeline (Plan 0001, done)

- **Engine actor:** one engine thread owns the MuPDF `Document`. The UI never calls the engine directly; it sends commands over a channel and receives events via `slint::invoke_from_event_loop` (holding a `slint::Weak`).
- **Tiled viewport:** a Flickable whose viewport size is page size × zoom. Only tiles intersecting the viewport (plus a margin) are rendered. Tiles are 512 physical px (`TILE_SIZE_PX`), rendered at physical resolution for HiDPI.
- **Display lists:** each page is interpreted once into an `Arc<DisplayList>`; tiles replay it with a scale+translate matrix and a device-space scissor.
- **Bleed band:** tiles rasterize into a TILE+2·`TILE_BLEED_PX` pixmap and copy out the inner region, so anti-aliased clip edges never show as a grid. Seam tests guard this.
- **Render worker pool:** display lists are `Send + Sync`, so a pool (default `clamp(cores-1, 1, 4)`, `FA_PDF_WORKERS=1` = serial) renders tiles in parallel.
- **Cancellation:** generation/epoch ids; stale requests are skipped before rendering. Stopping in-flight renders with MuPDF's cookie is implemented but **switched off** (`MID_RASTER_ABORT = false` in `pool.rs`) since 2026-10-20: with mupdf-rs 0.8 an aborted run can make MuPDF call `exit(1)` (`docs/benchmarks.md` §9.7); upstream mupdf-rs `main` already closes devices inside `fz_try` (`ffaf28f5`, #249) but no release carries it yet; the abort comes back with that release (`docs/backlog.md` item 7). MuPDF's display-list runner stops on the cookie by breaking out of its loop and *returns normally*, so aborts are detected from the pool's registry, not from an `Err`. The publish decision (`Aborted` / `Superseded` / `Live`) is made under the registry lock so a cancelled epoch can never publish pixels.
- **Abort latency** is bounded by the largest single drawing node (cookie is checked per node), not by a fixed ceiling.
- **Zoom UX:** during a zoom burst nothing renders; existing tiles are scaled on the GPU, a low-res base layer sits underneath, and crisp tiles arrive after a debounce. Zoom anchors on the cursor/viewport centre. `ZOOM_MAX` = 6400%.
- **Cache:** LRU bounded by bytes (default budget 64 MiB), keyed by integer scale (permille), visible tiles pinned. Overrides: `--cache-mib`, `--cache-rows`.

## 3. Commands

```sh
cargo build --workspace
cargo run -p app -- path/to/file.pdf
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace            # or: cargo nextest run --workspace
cargo deny check
reuse lint
cargo build --release -p app
```

A `justfile` wraps the common tasks.

**Bench harness** (release build, real window; set `FA_PDF_TRACE=1` for traces):

```powershell
target\release\app.exe crates\engine-mupdf\tests\fixtures\dense_vector.pdf --bench-zoom 6 --bench-step-ms 16 --bench-settle-ms 3000 --bench-repeat 5
target\release\app.exe crates\engine-mupdf\tests\fixtures\large_format_a0.pdf --bench-zoom 6 --bench-pan 400 --bench-pan-steps 40 --bench-step-ms 16 --bench-settle-ms 3000
```

Metric definitions (`t_blank`, `t_full`, `t_abort`, holes, rebuilds, RSS) live in `docs/benchmarks.md` §7.5–7.6. Define a metric there before measuring it.

**PowerShell trap:** do not append comma-separated literal flags after an array splat (`app.exe $fx $flags '--cache-mib','64'`); they are joined into one token and taken as the document path, so the app idles with no document. Put all flags in one `@()` array or write them literally.

**Fixtures** (`crates/engine-mupdf/tests/fixtures/`, licenses in the README there): `minimal.pdf`, `large_200p.pdf`, `large_format_a0.pdf`, `dense_vector.pdf` (one heavy node per tile, worst case for abort latency).

## 4. Rules for agents

1. **Never scale down the goal to make a problem go away.** No zoom caps, page-count caps or scope cuts as a "fix". If something is blocked (Slint limitation, mupdf-rs API gap, wrong assumption), **stop and report** what and why, with options.
2. **Respect plan gates.** Plans in `docs/plans/` have phases with explicit gates. Stop at each gate and wait for the maintainer's "go".
3. **Never fabricate numbers.** Benchmarks come from real runs; if you cannot run something (e.g. it needs a real window), say so and give the exact command.
4. **Do not guess APIs.** Read the docs/source for the pinned `slint` and `mupdf` versions. Cite file:line for non-obvious claims.
5. Keep the dependency direction. No `unwrap`/`expect` in non-test code; typed errors (`thiserror`).
6. Tests must be able to fail: prefer deterministic tests, and use mutation checks for important invariants (pixel identity, no publish after cancel).
7. Small commits, conventional commit messages, `Signed-off-by:` on each. Keep CI green (fmt, clippy `-D warnings`, tests, deny, reuse).
8. Update `CHANGELOG.md`, the relevant plan's status block, `docs/benchmarks.md` and ADRs when behaviour or decisions change. Record new architectural decisions as ADRs in `docs/adr/`.
9. Prefer "good enough, documented" over endless optimization. Out-of-scope improvements go to `docs/backlog.md`, not into the current change.

## 5. What has been done

| Step | Result |
|---|---|
| Bootstrap | Workspace, AGPL + REUSE, DCO, community files (README, CONTRIBUTING, CODE_OF_CONDUCT, SECURITY), ADR-0001 (stack), ADR-0002 (license), CI matrix (Windows/Ubuntu/macOS), cargo-deny, dependabot. |
| Technical spike | MuPDF page → Slint image through the engine actor (ADR-0003). Found full-page re-rendering on zoom too costly (RAM 58 → 382 MB at 1000%, ~541 ms per zoom step on A0). |
| Plan 0001 — tiled viewport rendering | Phases 0–2 implemented: tiles, display lists, bleed band, worker pool, base layer, cookie aborts, byte-budgeted cache, bench harness with `--bench-repeat`. |

**Measured results (one machine, real runs):**

- Heavy-tile zoom burst (`dense_vector.pdf`, 5 runs): `t_blank` 21/22/26 ms, `t_full` 48/50/59 ms, worst abort 18–22 ms; ~80% of requested tiles cancelled before rendering.
- A0 at 6400% with a long pan: RSS plateaus and follows the cache budget (working set peaks ≈ 156 MiB default, ≈ 209 MiB at 64 MiB budget, ≈ 155 MiB at 8 MiB budget, from an ≈ 85 MiB start). Eviction is proven to work.
- About 70 MiB of fixed overhead is independent of the tile budget (not investigated; see backlog).

The maintainer judged this efficient enough and stopped optimizing.

## 6. Where we are now

Plan 0001 is closed (verified 2026-10-20 against the code, tests and docs):

- [x] RGB8 / `alpha = false` tiles: white paper and 25% less tile memory; `large_200p.pdf` corner `[255,255,255]` and `data.len() == w*h*3` asserted in `crates/engine-mupdf/tests/tile_render_test.rs`.
- [x] Default cache = visible tiles plus the one-tile prefetch ring within the 64 MiB budget (`--cache-mib` / `--cache-rows` kept).
- [x] Memory target: one metric, `peak_ws` <= 250 MiB in the default configuration, measured 192.7 MiB (Plan 0001 §4.9, `docs/benchmarks.md` §7.6).
- [x] `docs/backlog.md`: fixed ~70 MiB overhead, allocator experiment, private-vs-working-set gap.
- [x] Phases 0-2 closed. **Phase 3 (ctrl+wheel/pinch, stretched second tile level) is deferred** by the maintainer, not cancelled; Plan 0002 does not depend on it.

## 7. What comes next

**Plan 0002 — continuous multi-page scroll with page virtualization** (`docs/plans/0002-continuous-multipage-scroll.md`). Phase 0 passed its gate (decisions in the plan's §0). Phase 1 (layout, canvas window, virtualization) passed its gate with row B's `ws_growth` FAIL accepted as documented (`docs/benchmarks.md` §8.3). Phase 2 (navigation and input) passed its gate (2026-10-20; results in the plan's §5 and `docs/benchmarks.md` §8.4). Phase 3 (progressive page sizes) is conditional on real-world documents the maintainer will supply.

**Plan 0003 — v0.1 viewer** (`docs/plans/0003-v0.1-viewer.md`). Phase 0 passed its gate (decisions in the plan's §0, ADR 0004). Phase 1 (open, recent files, theme) passed its gate with row H's FAIL accepted (`docs/benchmarks.md` §9.4). Phase 2 (sidebar: thumbnails and outline) passed its gate. Phase 3 (search) is **implemented and at its gate** (2026-10-20): results in the plan's §5 and `docs/benchmarks.md` §9.6. The maintainer's review found a crash on fast zoom (fixed by switching off the mid-raster abort, §9.7) and two find-bar issues (fixed); the guarded device close it needs is already on mupdf-rs `main` (unreleased), so no pull request was sent. Phase 3 passed its gate. Phase 4 (selection and copy) passed its gate on 2026-10-21 after a review fix (text cursor and selection only over text). **Plan 0003 is closed**; open upstream items: Slint drag-and-drop (slint-ui/slint#13527), the mupdf-rs release with `ffaf28f5` (to re-enable `MID_RASTER_ABORT`), and messense/mupdf-rs#272 (merged into mupdf-rs `main` on 2026-10-22, unreleased).

**Plan 0004 — v0.2 annotate** (`docs/plans/0004-v0.2-annotate.md`). Phase 0 passed its gate (2026-10-21; decisions in the plan's §0; the annotation-drawing fix landed first). Phase 1 (show annotations, text markup, select/recolour/delete, Save/Save As, unsaved prompt; after review, a click clears the selection's highlight and new markup cuts old markup of the same kind) passed its gate on 2026-10-21: results in the plan's §5, `docs/benchmarks.md` §10.3, ADR 0007. Phase 2 (undo and redo through MuPDF's journal, `patches/mupdf/journal/` + `build.rs`, ADR 0008) passed its gate on 2026-10-22: results in the plan's §5 and `docs/benchmarks.md` §10.4; upstream PR messense/mupdf-rs#273 (merged into mupdf-rs `main` on 2026-10-22, unreleased: the shim stays until a release, `docs/backlog.md` item 10). Phase 3 (notes: Note tool, editor, move by drag, hover preview; `docs/benchmarks.md` §10.5). Phase 4 (ink: Ink tool, live stroke, widths, rows V and X; `docs/benchmarks.md` §10.6). Both gates passed on 2026-10-22 after the maintainer's manual checks, confirmed in another PDF reader too. **Plan 0004 is closed.**

**Plan 0005 - v0.3 Organize and tools** (`docs/plans/0005-v0.3-organize-and-tools.md`): Phase 0 (research, measurements, the plan itself) is **done** (2026-10-22) and the maintainer answered its §9 questions (decisions in the plan's §0: structural edits undoable, tools work on the current state, `fz_try` wrappers plus an upstream PR for the missing MuPDF functions, PDF to Word included as Phase 5). Key finding: a garbage-collecting save of the open document silently breaks undo, so tools write from a separate document (§3.4). Measurements in `docs/benchmarks.md` §11. Phase 0 passed its gate on 2026-10-22. Phase 1 (page structure: rotate, delete, duplicate, move by strip or drag, insert blank; ADR 0009) passed its gate on 2026-10-22: results in the plan's §5 and `docs/benchmarks.md` §11.4; the maintainer's checks found MuPDF's wrong `/Prev` on a second in-place save, fixed with a shim accessor (ADR 0007). On the way, unembedded standard fonts were found to cost a 1.2-1.7 s system font search each and now come from MuPDF's built-in copies (plan §3.8). Phase 2 (merge, split, extract, insert from file, document properties; the tool thread) passed its gate on 2026-10-22: results in the plan's §5 and `docs/benchmarks.md` §11.5. On the way, copied pages were found to lose their annotations and links (MuPDF's page graft leaves `/Annots` out, which also hit Phase 1's Duplicate); `edit::copy_annotations` fixes it. Phase 3 (watermark, page numbers, crop) passed its gate on 2026-10-23 after the maintainer's manual checks: results in the plan's §5 and `docs/benchmarks.md` §11.6. On the way, stamping through mupdf-rs' `insert_contents` was found to cost n^3 (nested journal operations; 17.9 s on 1000 pages, now 0.2 s; `docs/backlog.md` item 15). Phase 4 (compress, export as images, images to PDF, repaired copy; `fz_try` wrappers for `pdf_rewrite_images` and JPEG output in `patches/mupdf/journal/tools.c`) passed its gate on 2026-10-23 (after a fix: Images to PDF orders its images in a list): results in the plan's §5 and `docs/benchmarks.md` §11.7; the upstream pull request for the wrappers is drafted, not sent. Phase 5 (Export as Word through MuPDF's DOCX writer, with a `fz_try` close of the writer) is **implemented and at its gate** (2026-10-23): results in the plan's §5 and `docs/benchmarks.md` §11.8; row AH is the maintainer's judgement of real conversions. Do not close Plan 0005 before the go. Android: the library crates build with NDK r28c on the maintainer's Debian mini PC (192.168.0.139, user `outsu`, SSH through the Windows ssh-agent) and their tests pass on a phone (152 of 152 at `817113a`, Phases 2-4 included; `docs/backlog.md` item 9).

Key questions for Phase 0:
- **Coordinate precision:** Slint lengths are f32 (exact integers only up to ~16.7 M). A 1000-page document at 6400% is ~50 M px tall. Design exact scrolling at any zoom (e.g. f64 scroll offset in Rust with a bounded, re-anchored canvas window) without capping document height.
- **Page sizes at open:** measure cost on large documents; design progressive size resolution with scroll anchoring if needed.
- Layout in `core` (prefix sums, binary search, mixed sizes, rotation, MediaBox origin, fit-width/fit-page, focal-point zoom across pages); virtualization (only visible ± margin pages hold tiles/display lists); tile model capacity when many small pages are visible; epochs across pages.
- Navigation in scope: current-page indicator, go-to-page, PgUp/PgDn/Home/End, arrows/space, zoom shortcuts, ctrl+wheel. Out of scope but not designed out: thumbnails, outline, search, text selection, spreads.
- Benchmarks: fling scroll 30 s on 200p and 1000p+, jump-then-zoom, mixed/rotated docs; RSS must not grow with page count.

### Roadmap after that

| Release | Scope |
|---|---|
| v0.1 Viewer | File dialog + drag-drop, thumbnails, outline, text search, selection + copy, recent files, light/dark theme, keyboard shortcuts |
| v0.2 Annotate | Highlight/underline/strikeout, notes, ink, incremental save, undo/redo |
| v0.3 Organize and tools | Rotate/delete/reorder/insert pages, merge/split, metadata; and the everyday "iLovePDF-style" tools (maintainer's decision, 2026-10-22): compress, PDF to images and images to PDF, watermark, page numbers, crop, save a repaired copy. PDF to Word only if Phase 0 measures MuPDF's DOCX output as good enough. Office to PDF is out of scope (it needs LibreOffice-class dependencies, against the resource priority). |
| v0.4 Forms | Fill AcroForms, flatten, export |
| v0.5 Secure | Redaction, encryption, digital signatures |
| Later | OCR (optional feature), object-level text/image editing, CLI companion, packaging (MSI/winget, dmg/Homebrew, Flatpak/AppImage, F-Droid), mobile ports, i18n and accessibility |
