<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# CLAUDE.md

Guidance for AI coding agents working in this repository. Read this first, then `README.md`, `docs/adr/`, `docs/plans/`, `docs/benchmarks.md` and `docs/backlog.md` before changing anything.

> Status snapshot: 2026-10-26. Sections marked **(verify)** were reported by earlier agent sessions and should be checked against the repo before relying on them.

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

**Disk use of `target/`:** on 2026-10-25 `target/` had grown to 65 GB (40 GB in `target/debug`: full-debug PDBs of 150-300 MB per `app` build, 15 GB of incremental data, six copies of MuPDF's C build; plus 18 GB of side target dirs `base`, `exp`, `docx`, `nodocx`, `up`). Rules since then:

- `[profile.dev]` builds with `debug = "line-tables-only"` and dependencies with no debug info (`Cargo.toml`). For a debugger session, build with `CARGO_PROFILE_DEV_DEBUG=full`.
- Do not leave side target dirs (`CARGO_TARGET_DIR=target/<name>`) behind: delete them when the experiment or comparison ends.
- **Never run `cargo clean`**: `target/tmp` holds the maintainer's sample PDFs and bench files. To reclaim space, delete everything in `target/` except `tmp` (and `CACHEDIR.TAG`), e.g. in PowerShell: `Get-ChildItem target -Force | Where-Object Name -notin 'tmp','CACHEDIR.TAG' | Remove-Item -Recurse -Force`.
- After that cleanup and a rebuild of `cargo build --workspace --all-targets`, the tests, clippy and `cargo build --release -p app`: `target/debug` 6.7 GB, `target/release` 3.6 GB, the project 10.7 GB (from-scratch build: 24 min for debug, 7.5 min for release).

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

**Plan 0005 - v0.3 Organize and tools** (`docs/plans/0005-v0.3-organize-and-tools.md`): Phase 0 (research, measurements, the plan itself) is **done** (2026-10-22) and the maintainer answered its §9 questions (decisions in the plan's §0: structural edits undoable, tools work on the current state, `fz_try` wrappers plus an upstream PR for the missing MuPDF functions, PDF to Word included as Phase 5). Key finding: a garbage-collecting save of the open document silently breaks undo, so tools write from a separate document (§3.4). Measurements in `docs/benchmarks.md` §11. Phase 0 passed its gate on 2026-10-22. Phase 1 (page structure: rotate, delete, duplicate, move by strip or drag, insert blank; ADR 0009) passed its gate on 2026-10-22: results in the plan's §5 and `docs/benchmarks.md` §11.4; the maintainer's checks found MuPDF's wrong `/Prev` on a second in-place save, fixed with a shim accessor (ADR 0007). On the way, unembedded standard fonts were found to cost a 1.2-1.7 s system font search each and now come from MuPDF's built-in copies (plan §3.8). Phase 2 (merge, split, extract, insert from file, document properties; the tool thread) passed its gate on 2026-10-22: results in the plan's §5 and `docs/benchmarks.md` §11.5. On the way, copied pages were found to lose their annotations and links (MuPDF's page graft leaves `/Annots` out, which also hit Phase 1's Duplicate); `edit::copy_annotations` fixes it. Phase 3 (watermark, page numbers, crop) passed its gate on 2026-10-23 after the maintainer's manual checks: results in the plan's §5 and `docs/benchmarks.md` §11.6. On the way, stamping through mupdf-rs' `insert_contents` was found to cost n^3 (nested journal operations; 17.9 s on 1000 pages, now 0.2 s; `docs/backlog.md` item 15). Phase 4 (compress, export as images, images to PDF, repaired copy; `fz_try` wrappers for `pdf_rewrite_images` and JPEG output in `patches/mupdf/journal/tools.c`) passed its gate on 2026-10-23 (after a fix: Images to PDF orders its images in a list): results in the plan's §5 and `docs/benchmarks.md` §11.7; the upstream pull request for the wrappers is messense/mupdf-rs#276 (opened 2026-10-23). Phase 5 (Export as Word through MuPDF's DOCX writer, with a `fz_try` close of the writer) passed its gate on 2026-10-23: results in the plan's §5 and `docs/benchmarks.md` §11.8. The maintainer's first real conversion lost its layout (MuPDF's writer flows columns and tables), so Export as Word is **off**: behind the `docx` Cargo feature of `engine-mupdf` and `app`, not built by default, still built and tested in CI (`docs/backlog.md` item 16). **Plan 0005 is closed.**

**Plan 0006 - v0.4 Forms** (`docs/plans/0006-v0.4-forms.md`): Phase 0 (research, measurements, the plan) passed its gate on 2026-10-23 (answers in the plan's §0: editor over the field, no JavaScript, XFDF + JSON, flatten as an undoable edit, the font fix in Phase 1). Findings: mupdf-rs 0.8 reads and fills every AcroForm field type as journal steps, flattening (`bake`) is an undoable edit; Turkish text in a field cost 2.4-3.5 s per fill because MuPDF asks the font loaders for `serif` and the engine's loader sends it to the system font search (fixed in a prototype: 2 ms); document JavaScript did not calculate on the first try. Phase 1 (filling fields: an editor over text fields, check boxes, radio buttons, choices; each one undo step; after review, fields are shaded and comb fields are edited box by box) passed its gate on 2026-10-24: results in the plan's §5 and `docs/benchmarks.md` §12.2. On the way: radio groups did not show their value (mupdf-rs passes the widget, not the field, to `pdf_set_field_value`; backlog item 17) and MuPDF 1.27.2 placed single-line Turkish field text below the field (fixed in MuPDF 1.28.5; worked around in `form.rs`). Phase 2 (Tab order from `/Tabs` and across pages, keys on check boxes and lists, multi-select lists, Form > Reset Field and Reset Form) passed its gate on 2026-10-24: results in the plan's §5 and `docs/benchmarks.md` §12.3. On the way: MuPDF 1.27.2 draws list boxes without their chosen options; the engine draws the bands (backlog item 18 for rich-text lists). Phase 3 (Form > Export Data and Import Data in XFDF and JSON, `pdf_core::form_data`, `roxmltree` in core; Flatten Form as one undo step through `pdf_bake_document`) is **implemented and at its gate** (2026-10-24): results in the plan's §5 and `docs/benchmarks.md` §12.4. Do not go on before the go; Phase 4 (JavaScript) was dropped, so Plan 0006 closes with Phase 3. Android: the library crates build with NDK r28c on the maintainer's Debian mini PC (192.168.0.139, user `outsu`, SSH through the Windows ssh-agent) and their tests pass on a phone (204 of 204 at `5abbb83`, Plan 0006 Phases 1-2 included; `docs/backlog.md` item 9).

**Plan 0007 - GUI redesign** (`docs/plans/0007-gui-redesign.md`): on 2026-10-25 the maintainer put the GUI before the rest of the roadmap (v0.5 Secure and later wait). The design source is the maintainer's TypeScript concept `podefa-ui-concept` and its `DESIGN.md` (outside this repo; port its decisions, not its code): floating islands, an own window frame, Read / Edit / Tools modes, tabs, light and dark themes from one token set. Phase 0 (Slint 1.18.1 checked: `no-frame`, `WindowMoveArea`, `resize-border-width`, `spring()` easing, tiled images exist; no backdrop blur; the software renderer draws no shadows; a frameless window has no place for the native Windows menu bar) passed its gate on 2026-10-25: no menu bar, every command in its island as the concept places it (plan §0); the other §9 answers are the recommended ones. Phase 1 (the markup in `crates/app/ui/*.slint` through `slint-build` and `build.rs`, a `Theme` global with both token sets; `--bench-idle` for `t_first_page` and idle frames, `docs/benchmarks.md` §13) passed its gate on 2026-10-25. Phase 2 (own frame and islands, no menu bar, ADR 0010: `title.slint`, `sidebar.slint`, `statusbar.slint`, `search.slint`, `widgets.slint`, Lucide icons under ISC/MIT; Ctrl+W; window-wide shortcuts through `KeyCommand::is_global`) passed its gate on 2026-10-25. Phase 3 (Edit's dot grid, tinted tools and pouring palettes; Tools' gallery in `gallery.slint` with multi-select, drag to a gap, gap actions, hover and bulk bars, rows kept across page edits by `pdf_core::pages::kept_rows` so cards slide; the mode motion only fades and scales; a warm-up frame after the first view; `--bench-modes`, `docs/benchmarks.md` §13.4; after review, the canvas runs under the whole window and the islands float over it at 92% opacity, with `pdf_core::Margins` keeping the first page under the title row) passed its gate on 2026-10-25 after five review fixes (plan §5 "Second review"): row AT renders 0 tiles in every change; the first entry into Tools sits at the 16.7 ms budget. Phase 4 (tabs, ADR 0011: one engine actor per tab, one shared `RasterPool`, only the shown tab draws, parked tabs hold no tiles, thumbnails or display lists (`EngineCmd::Trim`) and queue their events; `crates/app/src/tabs.rs`, the tab strip in `title.slint`; gallery pages dragged onto a tab or onto + go through an Extract to `%TEMP%\PODEFA` and Insert from File; the gallery's scissors split the pages after a gap into a new tab; `--bench-tabs N`, `docs/benchmarks.md` §13.5) passed its gate on 2026-10-26 (with Plan 0008's §9 answers): row AU passed (results in the plan's §5). Phase 5 (polish; it also fixes two misalignments the maintainer found: the + over the tab strip and the gap pill on a card's edge, both elements without `x` centred in their parent) runs alongside Plan 0008 Phases 0-1. Plan 0006 Phase 3 is still at its own gate.

Key questions for Phase 0:
- **Coordinate precision:** Slint lengths are f32 (exact integers only up to ~16.7 M). A 1000-page document at 6400% is ~50 M px tall. Design exact scrolling at any zoom (e.g. f64 scroll offset in Rust with a bounded, re-anchored canvas window) without capping document height.
- **Page sizes at open:** measure cost on large documents; design progressive size resolution with scroll anchoring if needed.
- Layout in `core` (prefix sums, binary search, mixed sizes, rotation, MediaBox origin, fit-width/fit-page, focal-point zoom across pages); virtualization (only visible ± margin pages hold tiles/display lists); tile model capacity when many small pages are visible; epochs across pages.
- Navigation in scope: current-page indicator, go-to-page, PgUp/PgDn/Home/End, arrows/space, zoom shortcuts, ctrl+wheel. Out of scope but not designed out: thumbnails, outline, search, text selection, spreads.
- Benchmarks: fling scroll 30 s on 200p and 1000p+, jump-then-zoom, mixed/rotated docs; RSS must not grow with page count.

**Plan 0008 - Mobile GUI, Android first** (`docs/plans/0008-mobile-gui-android.md`): started 2026-10-26 when the maintainer added a mobile interface to the concept (`podefa-ui-concept/src/components/mobile/`, its `DESIGN.md` step 38: header island, bottom thumb dock, edit strip, one-column gallery, bottom-sheet drawer). The §9 answers are the recommended ones (plan §0). Phase 0 (research, the plan, a trial build) passed its gate on 2026-10-26: Slint 1.18.1 has the Android backend (`backend-android-activity-06`, `android_main` in a `cdylib`), recognizes pinch from two fingers itself, and exposes safe-area and on-screen keyboard insets. The trial build (today's window on the phone, throwaway branch `trial/android-0`, plan §3.3, `docs/benchmarks.md` §14): Skia is Android's only renderer; its prebuilt binaries (downloaded from GitHub while building) clash with MuPDF's FreeType and libjpeg on 432 symbols, solved by renaming MuPDF's third-party symbols with `llvm-objcopy` after its `make`; APK 11.3 MiB, first page 239 ms, 0 idle frames, PSS 154-218 MiB, zoom frames 30-55 ms (the main risk). The build setup lives on the mini PC in `/srv/android` (SDK, cargo-apk 0.10.0, a throwaway debug keystore); the phone is on the Windows PC's USB (`adb` from the Windows SDK). By the maintainer's order the interface part of Phase 2 came first: `crates/app/ui/mobile/shell.slint` (`MobileShell`; header island, thumb dock with compact pager and zoom capsules, go-to-page and zoom cards, search bar, document and More menus, a bottom sheet with pages three to a row, contents, documents), on by default on Android and with `--mobile` on the desktop; shown on the phone from the throwaway branch `trial/android-1`. The maintainer approved it on 2026-10-26 (the pager's 24 px arrows stay). Phase 1 (platform, ADR 0012) passed its gate on 2026-10-27: `crates/app` is a library (`app::run`) with a thin desktop `main`; `app::platform` holds file dialogs, message boxes, the clipboard, the settings and temporary folders and the write-back hook, every dialog answering through a callback (`platform::desktop` wraps `rfd`/`arboard`, `platform::android` calls the activity over JNI); `crates/android` (`podefa-android`, `libpodefa.so`, empty on other targets) holds `android_main`, the Java activity `org.fineassociation.podefa.PodefaActivity` (pickers, cache copies of `content://` documents, write-back, message boxes, clipboard, "Open with"; compiled to a dex the build adds to the APK, by the maintainer's choice) and the manifest. Build with `scripts/android-build.sh` (needs `ANDROID_HOME`, `ANDROID_NDK_HOME`, a JDK, cargo-apk 0.10): it patches a copy of mupdf-sys with `patches/mupdf-sys-android.patch` through a Cargo config in `target/android` (the desktop build and `Cargo.lock` stay as they are) and links libc++ from build scripts; CI job `android`. On the phone, bench arguments come from `am start ... --es args "..."` and output goes to `Android/data/org.fineassociation.podefa/files/out.txt`. Rows MA, MB, MC pass, MD passes for the picker, in-place save and Save As (Drive and another reader are the maintainer's); results in the plan's §5 and `docs/benchmarks.md` §14.3. Phase 2 (the rest: pinch through `ScaleRotateGestureHandler` as a GPU transform with one zoom when the fingers lift, `--bench-pinch`; double tap told apart in Rust, since Slint 1.18.1 counts no double-click for touch; fling is the Flickable's own) passed its gate on 2026-10-27 (rows ME, MF, MG on the phone, the maintainer's pinch by hand; library suite 215 of 215; `docs/benchmarks.md` §14.4-14.5). Phase 3 (Edit on touch: the edit strip and palettes in `shell.slint`, taps through `AppState::touch_tap`, a long press moves a note, `keep_editor_above_floor` keeps field and note editors between the header and the keyboard; rows MI and MJ pass on the phone, `docs/benchmarks.md` §14.6) passed its gate on 2026-10-27; by the maintainer's word, Android's Back now closes one level at a time (menus, cards, sheet, dialogs, editors, Edit/Tools to Read) and asks about unsaved changes last; `platform::android` replaces its activity on each launch (Android reuses the process). Found: the keyboard carries a composed word into the next field (Slint restarts the input only on a type change, backlog item 26). Phase 4 (Tools on touch: `crates/app/ui/mobile/gallery.slint`, `MobileGallery`, one column of 230 x 325 px pages with their own 44 px buttons and check box, gaps with Split, blank page and pages from a file, a 500 ms long press that moves pages, the chosen pages' bar over the dock; the tools' dialogs moved out of the canvas, so they show in Tools, and are sheets on a phone) passed its gate on 2026-10-28: `--bench-pages` 13 of 14 on the phone (the miss is the desktop sidebar's drag), every operation by hand; the first change into Tools misses 16.7 ms on the phone, as before the column (backlog item 27). Phase 5 (landscape: the shell inside the safe area's sides, menus and tools' sheets that scroll, a sheet over the header when the keyboard leaves too little room, `--landscape`, `--bench-rotate` for row MK through `platform::request_orientation`; "Share" for PDFs next to "Open with"; an adaptive vector icon in `crates/android/res`; release signing with the maintainer's key, password asked by apksigner, `docs/android-release.md`) is **implemented and at its gate** (2026-10-28): row MK 6 of 6 on the phone; the review in both themes and the first signed release are the maintainer's. Dependency direction: `android` → `app`; nothing depends on `android`.

### Roadmap after that

| Release | Scope |
|---|---|
| v0.1 Viewer | File dialog + drag-drop, thumbnails, outline, text search, selection + copy, recent files, light/dark theme, keyboard shortcuts |
| v0.2 Annotate | Highlight/underline/strikeout, notes, ink, incremental save, undo/redo |
| v0.3 Organize and tools | Rotate/delete/reorder/insert pages, merge/split, metadata; and the everyday "iLovePDF-style" tools (maintainer's decision, 2026-10-22): compress, PDF to images and images to PDF, watermark, page numbers, crop, save a repaired copy. PDF to Word only if Phase 0 measures MuPDF's DOCX output as good enough. Office to PDF is out of scope (it needs LibreOffice-class dependencies, against the resource priority). |
| v0.4 Forms | Fill AcroForms, flatten, export |
| v0.5 Secure | Redaction, encryption, digital signatures |
| Later | OCR (optional feature), object-level text/image editing, CLI companion, packaging (MSI/winget, dmg/Homebrew, Flatpak/AppImage, F-Droid), mobile ports, i18n and accessibility |
