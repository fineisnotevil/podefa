<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# Performance Benchmarks & Measurements

This document records real empirical measurements taken during the technical spike (`spike/render-pipeline`) verifying the core rendering and execution pipeline.

## 1. Environment & Reproduction Setup

- **Operating System**: Windows 11 Pro 64-bit (Build 26100)
- **CPU**: AMD Ryzen 5 3500X 6-Core Processor @ 3.60 GHz (6 cores, 6 logical threads)
- **Rust Toolchain**: `rustc 1.98.0 (88d9e12ae 2026-08-18)`, `cargo 1.98.0 (797e8a9bc 2026-08-05)`
- **Slint Version**: `1.18.1`
- **MuPDF Crate / Version**: `0.8.0` (with MSVC `max_align_t` patch via local path `patches/mupdf`)

### Exact Commands Used

```powershell
# Build release binary (default profile: opt-level = "s")
cargo build --release -p app

# Check binary file size
Get-Item target\release\app.exe | Select-Object Name, Length

# Measure startup time and memory for small PDF
$sw = [System.Diagnostics.Stopwatch]::StartNew();
$p = Start-Process -FilePath "target\release\app.exe" -ArgumentList "crates\engine-mupdf\tests\fixtures\minimal.pdf" -PassThru;
Start-Sleep -Seconds 2;
$ws = (Get-Process -Id $p.Id).WorkingSet64;
Stop-Process -Id $p.Id -Force;
Write-Host "Minimal PDF: $($sw.ElapsedMilliseconds) ms, RSS: $([Math]::Round($ws / 1MB, 2)) MB"

# Measure startup time and memory for 200-page PDF
$sw = [System.Diagnostics.Stopwatch]::StartNew();
$p = Start-Process -FilePath "target\release\app.exe" -ArgumentList "crates\engine-mupdf\tests\fixtures\large_200p.pdf" -PassThru;
Start-Sleep -Seconds 2;
$ws = (Get-Process -Id $p.Id).WorkingSet64;
Stop-Process -Id $p.Id -Force;
Write-Host "200-page PDF: $($sw.ElapsedMilliseconds) ms, RSS: $([Math]::Round($ws / 1MB, 2)) MB"
```

## 2. Release Binary Size

All builds are compiled with `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, and `strip = true`.

| Optimization Profile | Binary Size (bytes) | Binary Size (MiB) | Notes |
| :--- | :--- | :--- | :--- |
| `opt-level = "s"` (size-optimized) | **17,105,920** | **16.31 MiB** | Default workspace release configuration |
| `opt-level = "z"` (aggressive size) | **16,421,888** | **15.66 MiB** | ~668 KiB smaller (-3.99% binary size) |

## 3. PE Binary Section Breakdown (`dumpbin`)

Analysis of raw section allocations in `app.exe` (`opt-level = "s"`):

| Section | Size (Hex) | Raw Size (Bytes) | Proportion | Purpose |
| :--- | :--- | :--- | :--- | :--- |
| `.text` | `0x782000` | 7,872,512 bytes | ~46.0% | Executable machine code (MuPDF C engine + Slint UI + Rust runtime) |
| `.rdata` | `0x745A00` | 7,625,216 bytes | ~44.5% | Read-only data (embedded font tables, ICU tables, MuPDF static tables) |
| `.data` | `0x14B800` | 1,357,824 bytes | ~7.9% | Initialized global/static writable state |
| `.pdata` | `0x031600` | 202,240 bytes | ~1.2% | Exception handling tables (MSVC ABI) |
| `.reloc` | `0x00B800` | 47,104 bytes | ~0.3% | Base relocation information |

### Top Contributors to Binary Size

1. **Embedded Font & Unicode Data**: Slint bundles ICU data tables (`icu_locale_core`, `icu_segmenter_data`, `icu_normalizer_data`) and font parsing runtimes (`read-fonts`, `skrifa`, `fontdb`).
2. **MuPDF C Engine (`libmupdf`)**: Fitz rendering core, font engine, colorspace conversions, and geometry math compiled into `.text` and `.rdata`.
3. **Slint GUI Backend**: Femtovg / software renderer, windowing interfaces (`winit`, `accesskit`), and UI widget state management.

## 4. Latency & Memory Footprint (RSS)

Measurements were taken after initial window draw and page 1 rasterization:

| Test Case | Page Count | Latency to First Page | Stable RSS (Working Set) |
| :--- | :--- | :--- | :--- |
| **Small PDF (`minimal.pdf`)** | 1 page | **~369 ms** | **95.01 MB** |
| **Large PDF (`large_200p.pdf`)** | 200 pages | **~606 ms** | **101.83 MB** |

*Note: The 200-page document (`crates/engine-mupdf/tests/fixtures/large_200p.pdf`) was generated programmatically with 200 distinct page catalog dictionaries and vector content streams.*

### Observations

- **Startup & Render Speed**: Cold launch to initial interactive window with page 1 rendered completed in under 400 ms on a 6-core desktop processor.
- **Memory Scaling**: Scaling from 1 page to 200 pages increased resident memory by only **~6.8 MB**, proving that the single-threaded actor model successfully prevents duplicating document structures or leaking unrendered page buffers.

## 5. Phase 0 Baseline: Zoom Re-rasterization Cost

Measured at commit `31819cf` on the same machine as section 1, release profile.

The current implementation re-rasterizes the **entire page** on every zoom change
(`EngineCmd::RenderPage { page, scale, .. }`), so the cost of a single zoom step is the
cost of a full-page RGBA8 raster at the new scale, plus the copy of that raster into a
Slint `Image`. These numbers are the baseline that the tiled renderer must beat.

### 5.1 Engine-level: full-page raster cost

Harness: `crates/engine-mupdf/tests/render_bench.rs` (marked `#[ignore]`, so it is
compiled but not executed by CI).

```powershell
# Letter, 200-page fixture (default)
cargo test --release -p engine-mupdf --test render_bench -- --ignored --nocapture

# Large format (A0) fixture
$env:FA_PDF_BENCH_PDF = "$PWD\crates\engine-mupdf\tests\fixtures\large_format_a0.pdf"
cargo test --release -p engine-mupdf --test render_bench -- --ignored --nocapture
```

`cold` is the first render at that scale, `warm` is the immediately repeated render of the
same page at the same scale (what a repeated zoom click costs).

**`large_200p.pdf`, page 0 = 612x792 pt (8.50x11.00 in, US Letter):**

| Zoom | Raster size (px) | RGBA8 buffer | Cold | Warm |
| :--- | :--- | ---: | ---: | ---: |
| 100% | 612x792 | 1.85 MiB | 0.8 ms | 0.7 ms |
| 200% | 1224x1584 | 7.40 MiB | 3.4 ms | 3.0 ms |
| 400% | 2448x3168 | 29.58 MiB | 10.9 ms | 12.0 ms |
| 1000% | 6120x7920 | 184.90 MiB | 63.3 ms | 75.0 ms |
| 6400% | 39168x50688 | 7,573.5 MiB | *not attempted* | *not attempted* |

**`large_format_a0.pdf`, page 0 = 2384x3370 pt (33.11x46.81 in, A0 / 841x1189 mm):**

| Zoom | Raster size (px) | RGBA8 buffer | Cold | Warm |
| :--- | :--- | ---: | ---: | ---: |
| 100% | 2384x3370 | 30.65 MiB | 4313.7 ms | 34.6 ms |
| 200% | 4768x6740 | 122.59 MiB | 131.4 ms | 128.5 ms |
| 400% | 9536x13480 | 490.36 MiB | 431.9 ms | 541.3 ms |
| 1000% | 23840x33700 | 3,064.8 MiB | *skipped* | *skipped* |
| 6400% | 152576x215680 | 125,532.5 MiB | *exceeds physical RAM* | *exceeds physical RAM* |

*The 4.3 s `cold` figure at A0 100% is one-time process initialization (MuPDF store, font
machinery, first device allocation) and is not representative; the 200% and 400% rows are
the steady-state cost of a zoom step.*

### 5.2 App-level: resident memory and CPU

Window ~900x700 logical px. RSS is `WorkingSet64`.

| Scenario | Fixture | Steady RSS | Idle CPU |
| :--- | :--- | ---: | ---: |
| 100% (default zoom) | `minimal.pdf` | 87.07 MB (private 129.29 MB) | 0.21% |
| 100% (default zoom) | `large_200p.pdf` | 95.00 MB (private 143.14 MB) | 0.05% |
| **1000%** | `large_200p.pdf` | **294.66 MB steady / 398.95 MB max / 474.24 MB peak** | 1188 ms CPU over 12.9 s wall |

Idling at 0.05-0.21% CPU confirms the actor/event-loop design does no background work; the
cost is entirely in the zoom path.

The 1000% row was captured by temporarily setting `ZOOM_DEFAULT = 10.0` in
`crates/core/src/lib.rs` and rebuilding, because the app exposes no CLI zoom flag and the
zoom path is otherwise only reachable through GUI input. The constant was reverted
immediately after the measurement.

**Memory model, confirmed.** At 1000% the RGBA8 buffer alone is 184.90 MiB, and the
measurement shows ~200-290 MB above the 87 MB baseline. That is consistent with the buffer
being resident *twice*: once as the `SharedPixelBuffer` owned by the Slint `Image` property
(`main.rs` builds it with `Image::from_rgba8` and stores it in `page_image`), and once as
the GPU texture uploaded by the femtovg renderer, plus the transient MuPDF pixmap during
the render itself. The same accounting explains the reported ~382 MB figure from the
earlier spike.

### 5.3 Interactive zoom CPU (manual procedure)

There is no CLI or scripted path to drive the zoom buttons, so this one figure is captured
manually. Start the viewer, then in a separate PowerShell window:

```powershell
$p = Get-Process app | Select-Object -First 1
$c0 = $p.TotalProcessorTime.TotalMilliseconds
$w0 = $p.WorkingSet64
# ... click "+ Zoom" repeatedly (or hold Enter/click) for 5 seconds ...
Start-Sleep -Seconds 5
$p.Refresh()
$cores = [Environment]::ProcessorCount
$cpu = (($p.TotalProcessorTime.TotalMilliseconds - $c0) / 1000) / (5 * $cores) * 100
Write-Host ("CPU during zoom: {0:N1}%, RSS {1:N1} MB -> {2:N1} MB" -f `
    $cpu, ($w0 / 1MB), ($p.WorkingSet64 / 1MB))
```

## 6. Conclusion: tiling is mandatory, not an optimization

For a US Letter page the current design reaches 1000% at a real but survivable cost
(~185 MiB per buffer, ~75 ms per zoom step). For a large-format page it does not survive at
all: a single 400% zoom step costs **490 MiB and ~540 ms**, 1000% needs **3.0 GiB**, and the
6400% target of the tiled renderer needs **125 GiB** for one buffer, which is beyond any
plausible machine.

Independently, a 1000% A0 raster is 23840x33700 px, which exceeds the typical GPU
`MAX_TEXTURE_SIZE` (16384) and therefore cannot be uploaded as a single texture at all.

The fix is to rasterize only the visible region at the current zoom, cached in fixed-size
tiles, so that the cost of a zoom step scales with the *viewport* (e.g. 900x700 physical px
= 2.4 MiB) instead of the *page*. See
[`docs/plans/0001-tiled-viewport-rendering.md`](plans/0001-tiled-viewport-rendering.md).


## 7. Phase 1: tiled rendering (after)

Measured on the same machine, same fixtures, with the tiled renderer in place. The harness grew
two tiled benchmarks, both `#[ignore]`d like the rest:

```powershell
# Per-scale tiled zoom step (cold = first pass, warm = repeat)
cargo test --release -p engine-mupdf --test render_bench bench_tiled_zoom -- --ignored --nocapture

# Twenty zoom clicks in one second, through the real tile cache
cargo test --release -p engine-mupdf --test render_bench bench_twenty -- --ignored --nocapture
```

Viewport 900x700 device pixels, 512 px tiles, 64 MiB tile-cache budget. "MiB/step" is the tile
pixels rasterized for one zoom step.

### 7.1 `large_200p.pdf` (Letter), page 0

| Zoom | Buffer (before) | Step (before) | Tiles | Cold (after) | Warm (after) | MiB/step |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 100% | 1.85 MiB | 0.8 ms | 4 | 0.8 ms | 0.5 ms | 1.85 |
| 200% | 7.40 MiB | 3.4 ms | 9 | 3.3 ms | 1.9 ms | 7.17 |
| 400% | 29.58 MiB | 10.9 ms | 6 | 2.6 ms | 2.3 ms | 6.00 |
| 1000% | 184.90 MiB | 63.3 ms | 4 | 1.2 ms | 1.8 ms | 4.00 |
| 6400% | 7,573.5 MiB | *impossible* | 9 | 2.9 ms | 4.6 ms | 9.00 |

### 7.2 `large_format_a0.pdf`, page 0

| Zoom | Buffer (before) | Step (before) | Tiles | Cold (after) | Warm (after) | MiB/step |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 100% | 30.65 MiB | 4313.7 ms (one-off) | 6 | 1024.4 ms (one-off) | 5.3 ms | 6.00 |
| 400% | 490.36 MiB | 541.3 ms | 6 | 4.4 ms | 5.7 ms | 6.00 |
| 1000% | 3,064.8 MiB | *impossible* | 6 | 16.1 ms | 5.6 ms | 6.00 |
| 6400% | 125,532.5 MiB | *impossible* | 6 | 6.3 ms | 5.5 ms | 6.00 |

The 4313.7 ms "before" figure at A0 100% and the 845-1623 ms "after" cold figure at A0 100% are
the same one-off process initialization documented in §5.1 - a fresh process pays it once,
whichever renderer runs. Measured directly: the first display-list build in a process takes
1.8 s on the A0 fixture, the second takes 0.1 ms, and the first full-page render takes 29 ms.
The user's very first interaction with an A0 document therefore stalls ~1-2 s; tiling neither
causes nor fixes that. Every later zoom step is ~5 ms at any zoom level.

### 7.3 Twenty zoom clicks in one second (rapid zoom)

Twenty `+ Zoom` clicks issued as fast as the harness can, through the real tile cache (16 rows,
64 MiB budget). The session's first tile is timed separately because it pays the one-off above:

| Fixture | First tile | 20 clicks | Slowest step | Final zoom | Rasterized | Peak cache | Dropped |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `large_200p.pdf` | 1 ms | **37 ms** | 3.8 ms | 6400% | 121.5 MiB | 16.0 MiB | 0 |
| `large_format_a0.pdf` | 1314 ms | **97 ms** | 7.5 ms | 6400% | 122.0 MiB | 16.0 MiB | 0 |

All twenty clicks reach 6400% well inside a second: the A0 case averages 4.9 ms per click at up
to 6400%, where the previous renderer needed 541 ms for a *single* 400% click. Peak cache
residency is 16 MiB, i.e. exactly one tile per model row (that run predates the RGB8 tiles of §7.6,
which make the same row 768 KiB), well inside the 64 MiB budget - with a 900x700 window the
viewport-derived row capacity binds before the byte budget, as §4.6 of the plan predicted. `dropped`
counts tiles that found no free row, and stayed at zero.

### 7.4 Memory and pixel-identity summary

| Quantity | Before | After |
| :--- | ---: | ---: |
| A0 at 400%, one zoom step | 490 MiB buffer, 541 ms | 6 MiB of tiles, ~5 ms |
| A0 at 6400%, one zoom step | 125 GiB buffer (impossible) | 6 MiB of tiles, ~6 ms |
| Raster scratch | one full-page pixmap per step | 786 KiB (RGB8), allocated once |
| Tile, 512x512 | 1.00 MiB RGBA8 + a 1.00 MiB UI copy | 768 KiB RGB8 + a 768 KiB UI copy |
| Tile cache ceiling | - | 64 MiB of bitmaps (viewport-sized by default: 12 MiB, §7.6) |

Pixel identity, from `crates/engine-mupdf/tests/tile_render_test.rs` ("0 differing bytes" means
exact equality, not a tolerance):

- Letter at 100% and at 400%: every tile blitted into place reproduces the full-page raster
  **byte for byte**, so there is no seam anywhere on the page.
- A0 at 6400% (a 298x422 grid, 152,576x215,680 px): a 2x2 block of tiles on a grid crossing - the
  block is chosen so the comparison is over antialiased edges rather than over paper - matches an
  independent `Page::run` render of the same region **exactly** (max channel delta 0, 0 differing
  bytes of 3,145,728), and re-rendering a tile is byte-identical. No precision jitter, so the
  per-page virtual-extent guard is not needed.

The GUI-level capture of §5.3 has been re-run only in its non-interactive half: with the A0
fixture open at the default zoom, the tiled app settles at **137 MB RSS and 0.21% idle CPU**
(debug build; the Phase 0 idle figure was 0.05-0.21%). The idle figure matters as a correctness
check on amendment 1: viewport tracking runs from `changed content-x/y` and programmatic
scroll writes re-enter it, so a wiring mistake there would spin. It does not.

Still manual, because it needs repeated GUI input: the 1000% interactive zoom CPU capture of §5.3.
The animated-wheel-scroll and programmatic-viewport-write checks in the plan are now driven
non-interactively by the `--bench-*` harness of §7.5, which supersedes the status-bar
`tiles rendered / tiles requested` counters for those checks.

The engine-level accounting above is the hard bound (scratch + cache rows). The app's Slint
`Image` copies are 3 bytes per pixel too (§7.6), so unlike the RGBA8 revision they no longer roughly
double the cached bitmap bytes; what the app adds on top is the base layer (about 2 MiB) and MuPDF's
own store, which is where the gap §7.6 measures between the cache and the process comes from.

### 7.5 Interactive zoom and pan through the real app (Plan 0001, Phase 2 gate)

> **Gate run (post-pool).** Every figure in this section was measured on the **pooled** engine with
> RGB8 tiles and the prefetch ring - the release binary of Plan 0001's Phase 2 - on 2026-10-06, in
> the same 884 px-wide viewport §7.6 measures, one run per row unless the row says otherwise. The
> measurement this one replaces was the **serial** engine with 1 MiB RGBA8 tiles and no ring; where
> a number from it still helps it is quoted as *pre-pool*. Three things the gate run settles, over
> and above re-measuring the tables: the pool's own contribution to a zoom step, the ring's effect
> on the pan gaps the tables were built around, and the `abort_ms` line the harness reports.
> Two metric definitions carry over unchanged: `holes` counts *visible* cells with no cached tile
> (a ring tile is cached but not visible, so it is not a hole), and `unwanted` counts work in flight
> that is neither visible nor part of the ring, i.e. a viewport the user has already left. The
> memory half of the same gate item is §7.6's, which samples the process every 250 ms through this
> harness and shows the cache plateau following whichever ceiling is lower.

Sections 7.1-7.4 measure the engine and the tile cache in isolation. This section measures the
*app*: the `+ Zoom` button, the wheel-style scroll and the `Flickable` viewport, driven end to end
in a real window by the diagnostic harness in `crates/app/src/bench.rs`. Same machine as §1. The
absolute numbers do not reproduce across machines; the failure mode and the ratio between a settled
step and a burst do.

Two opt-in pieces, both inert without a flag:

| Flag | Effect |
| :--- | :--- |
| `FA_PDF_TRACE=1` | prints the trace: every level change, the first tile of the new level (`t_blank`), full coverage (`t_full`), one line per pan step carrying its offset and the viewport's reach (the source of the pan table's travel and speed below), a throttled frame line carrying `holes` / `pending` / `unwanted`, and a 250 ms `rss` line carrying the process's resident and private bytes beside the cache's bytes, rows, `dropped` and `evicted` (§7.6) |
| `--bench-zoom N` | `N` zoom-in steps through `AppState::zoom`, the call the `+ Zoom` button makes, anchored at the viewport centre |
| `--bench-pan PX --bench-pan-steps N` | after a settle, `N` rightward steps of `PX` device pixels, written to `scroll-x`, the property the wheel drives |
| `--bench-step-ms MS` | time between steps: 1500 ms is a settled single step, 16 ms is a wheel animation |
| `--bench-settle-ms MS` | idle time before the first zoom step and after each phase, so no phase inherits the previous one's backlog (`0` is itself a case: see below) |
| `--bench-max-blank-ms MS` | ceiling the `VERDICT` line compares `t_blank` against (default 50) |
| `--bench-repeat N` | runs the whole scenario `N` times, reporting each run's worst beside the min/median/max across runs (default 1) |
| `--cache-mib N` | tile cache byte budget in MiB, for the scheduler the app builds (default: the platform default, 64 MiB on desktop and 16 MiB on mobile). Not a step flag: it applies with or without a run, so a manual `FA_PDF_TRACE=1` session can carry it too |
| `--cache-rows N` | model rows, replacing the viewport-derived count. The row count is the cache's *real* ceiling - a scheduler never holds more tiles than it has rows - so a byte budget above it cannot bind; §7.6 needs this flag to see a 64 MiB plateau, and with 768 KiB RGB8 tiles that takes 96 rows where the RGBA8 revision needed 80 |

A run is an initial settle, a zoom phase, a settle, an optional pan phase, a second settle, then a
summary. The metrics are counted per level change, per frame and per aborted tile, and each one
measures a specific event rather than a region of the screen:

- **`t_blank`** - from a level change to the **first tile of the new level the model accepts**, i.e.
  how long the new scale had no crisp pixels at all. It is not "the page showed nothing": what the
  user sees in the meantime is the base layer, because a hole is a cell with no *tile*, not a cell
  with no *image*. Only a tile that reaches the model logs a sample, so a step whose first tile is
  cancelled by the next level change - or dropped for lack of a model row - contributes nothing to
  the count. `t_full` is the same clock stopped when the viewport has zero holes.
- **`holes`** - visible cells of the *current* scale with no cached tile. A cell covered only by the
  stretched base layer is a hole: the count is cells that are still soft, which is what a pan shows
  as a moving strip of low-resolution page and a zoom shows as a blur that sharpens cell by cell.
- **`abort_ms`** - from a superseded epoch's `Cancel` to the rasterizer noticing it, one sample per
  aborted tile; the largest display-list node bounds it (see §5 Phase 2 of the plan).
- **`unwanted`** - pending requests the viewport has already left, i.e. raster time that cannot
  reach the screen.

A burst at a 16 ms step on a page whose tiles take tens of milliseconds leaves `t_blank` with a
**single sample**, and the sample belongs to the last step of the burst: every earlier step's first
tile is cancelled by the step that follows it before it can land, so no earlier level ever had a
first tile to time. That single number is the one worth having - it is the wait the user actually
feels - but one sample is not a distribution, which is what `--bench-repeat` is for: `N` runs of the
same scenario, each one's worst reported beside the min/median/max across runs, verdict on the
worst of any run. Between runs the harness returns the viewport to where the scenario started *and
drops the cached tiles*, so a repeat measures a cold scenario rather than one served by the previous
run's cache (a warm cache would find every level already rendered and measure nothing). The metric
lines above the repeat lines always describe the last run; the `per-run` and `across` lines are the
repeats.

Each phase boundary prints a `window <name>` line with that window's counters and its
`worst_blank`. The opening `run` window is the stretch before the first zoom step, and on the A0 it
carries the document's own first render - the §7.2-7.3 one-off, not a zoom cost. The summary
therefore drops the blank and full samples logged inside it; the number itself stays visible on
that window line. Later windows accumulate rather than reset, so a pan after a zoom cannot hide the
zoom's figures.

```powershell
# Isolated zoom step: everything settles between steps
target\release\app.exe crates\engine-mupdf\tests\fixtures\large_200p.pdf --bench-zoom 2 --bench-step-ms 1500 --bench-settle-ms 2000

# Rapid zoom, the wheel-animation case. The initial settle has to outlast the document's own first
# render, which the closing `run` window line reports: 1.2-1.7 s on the A0 here.
target\release\app.exe crates\engine-mupdf\tests\fixtures\large_format_a0.pdf --bench-zoom 6 --bench-step-ms 16 --bench-settle-ms 3000

# The same burst fired while that first render is still running, i.e. a cold open the user
# interrupts before anything has been drawn
target\release\app.exe crates\engine-mupdf\tests\fixtures\large_format_a0.pdf --bench-zoom 6 --bench-step-ms 16 --bench-settle-ms 0

# Fast pan, after the zoom settles
target\release\app.exe crates\engine-mupdf\tests\fixtures\large_format_a0.pdf --bench-zoom 6 --bench-pan 400 --bench-pan-steps 40 --bench-step-ms 16 --bench-settle-ms 3000

# The same burst five times in one process. Each repeat restores the viewport and drops the cached
# tiles, so all five runs measure the same cold scenario; `per-run` lists each run's worst and
# `across` their min/median/max, and the VERDICT takes the worst of the five.
target\release\app.exe crates\engine-mupdf\tests\fixtures\large_format_a0.pdf --bench-zoom 6 --bench-step-ms 16 --bench-settle-ms 3000 --bench-repeat 5
```

Zoom, `t_blank` in ms. The A0 `open` one-off is not a row here: it lands in the `run` window
before any zoom step. Rows are `--bench-zoom N --bench-step-ms MS --bench-settle-ms MS`, one run
each except the last, which is `--bench-repeat 5`; that row's `t_blank` and `t_full` are
min/median/max across the five runs' worsts.

| Fixture | Levels | Reset before the steps | t_blank min/med/max | t_full max | Worst frame | Worst pending | Unwanted | Verdict |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | :--- |
| `large_200p.pdf` | 2 | 1500 ms settle | 1 / 2 / 2 | 3 | 4 | 6 | 0 | PASS |
| `large_200p.pdf` | 8 | 16 ms steps | 1 / 2 / 3 | 13 | 9 | 25 | 8 | PASS |
| `large_format_a0.pdf` | 1 | 1500 ms settle | 2 / 2 / 2 | 6 | 4 | 9 | 0 | PASS |
| `large_format_a0.pdf` | 2 | 1500 ms settle | 2 / 3 / 3 | 5 | 6 | 12 | 0 | PASS |
| `large_format_a0.pdf` | 6 | settled | 1 / 2 / 2 | 9 | 9 | 24 | 4 | PASS |
| `large_format_a0.pdf` | 6 | 800 ms, open still running | 714 / 714 / 714 | 721 | 9 | 24 | 4 | FAIL |
| `large_format_a0.pdf` | 6 | 300 ms, open still running | - (no sample) | - | 9 | 24 | 4 | n/a |
| `large_format_a0.pdf` | 6 | 0 ms, burst at the open | - (no sample) | - | 9 | 24 | 4 | n/a |
| `large_format_a0.pdf` | 6 | settled, 5 runs | 2 / 2 / 10 across | 9 / 10 / 20 | 9 | 24 | 4 | PASS |

The pre-pool run of the same commands, for the rows where the difference is the finding: `t_blank`
was `2 / 4 / 4` and `1 / 1 / 2` on the 200p, `3 / 3 / 3` and `2 / 2 / 2` on the A0, `2 / 3 / 11`
for the settled burst and `295 / 295 / 295` for the 800 ms one; `t_full` max was 8, 7, 18, 11, 22
and 306 in that order.

The document's own first render, from the closing `run` window's `worst_blank`: **1.1-1.3 s** on
the A0 (1057-1315 ms across this section's A0 runs), **3-5 ms** on the 200p. That figure is the wait
for the render's first tile, and the pool does not move it - pre-pool it measured 1193-1675 ms on
the same fixture - because what it waits on, one page's display list, is the one part of a render
that is not parallel work.

A settled A0 zoom step lands in **2-3 ms**, and six of them fired 16 ms apart - the wheel-animation
case - land in **1-2 ms**: under one step interval in every case. The 200p fixture behaves the same
(`t_blank` 1-3 ms in both shapes, its eight-step burst included). Spacing is not what costs, and
neither is the level count.

What the pool changes is the tail, not the first tile. `t_full` - the wait for the new level to
cover the *whole* viewport rather than one cell - falls from a pre-pool worst of **22 ms** to
**9 ms** on the A0's six-step burst and from 18 to 6 on a single settled step, because the
viewport's tiles rasterize on `clamp(cores - 1, 1, 4)` workers instead of one. The 200p's `t_full`
worst is 13 ms in its eight-step burst, the run where the ring keeps 25 requests in flight.

The burst only gets expensive while it overlaps the document's own first render, and then the cost
is the *remainder* of that render, spent once, not a per-step cost. The three overlap runs above,
same command but different initial settle:

- settle 300 ms: the render outlives the whole burst, **no burst level ever lands** (`n=0`,
  `VERDICT n/a`, `tiles rendered=0`, cache 0 of 16 rows), and the viewport shows the first render's
  tiles throughout;
- settle 800 ms: the render is still running when the burst ends, the newest level's first tile
  takes **714 ms** - the remainder of that render - and the run FAILs;
- settle 3000 ms: the render landed, the burst is **1-2 ms**.

The reason is the display-list build, not queue order. `zoom` bumps the epoch, sends the `Cancel`
and enqueues only the new epoch's tiles (§4.5 of the plan, steps 1-4), which is correct; but the
first render's tiles are *already* in the engine's queue when the burst starts, and they carry the
epoch the burst replaced, not the one it cancelled. Cancellation is per epoch - the pool drops the
queued jobs of the epoch a `Cancel` names and aborts the rasters of that epoch already inside the
rasterizer (`crates/engine-mupdf/src/pool.rs`) - so it cannot touch the render the burst
interrupted, which is still what the viewport has to show until the new level lands. So a burst
waits for whatever the first render still owes and no longer. `worst_unwanted=4` appears in the
settled burst too, so it counts superseded burst steps (each step cancels the previous step's tile
before it lands), not the stall. The `abort_ms` samples in the failing run are the pool's side of
that story: **3 aborts, every one of them 0 ms** - a superseded level notices the `Cancel`
immediately, so what the user waits on is the display list, not abort latency.

What varies between the two cases is how expensive the tiles ahead of the wanted one are, and that
is a one-off: the page's first display-list build (1.8 s the first time in a process, 0.1 ms
afterwards, §7.2-7.3) plus that render's own tiles. Once the display list is cached, a tile
rasterizes in 2-4 ms, which is why a settled burst is milliseconds and why the same shape never
shows on the 200p, whose tiles were milliseconds from the start.

§7.3's engine-level harness measures the same shape through the same fixture and agrees: its twenty
rapid clicks, settled before the burst exactly like the settled row above, take 97 ms in total with
a 7.5 ms slowest step, and its separate 1314 ms "first tile" column is the one-off again.

Nothing here contradicts the plan's §4.5 reasoning (lines 441-450); it holds as written. The 929 ms
in this section's first revision was this same one-off, measured in the same overlapping
configuration (124-929 ms across the pre-pool runs, depending on how much of the render was left),
and read as a per-burst cost because the harness then had no window line to separate the startup
from the zoom phase. The gate run puts another sample inside that same envelope - **714 ms** - which
is also the honest answer to what the pool contributes *here*: **nothing**. The pool parallelizes the
raster of tiles, and that shows up in `t_full` above (22 ms to 9 ms on the A0's viewport sweep); the
cold open's first tile waits instead on one page's display list, built by one thread on the engine
actor, so the wait is the same before and after. This section's earlier revision expected it to
shrink by roughly the worker count; the measurement does not show that. What is left is a cold open a
user can interrupt 0.3-0.8 s in, costing the remainder of that render once, and the item that
addresses it is Phase 3's stretched lower-resolution level, which covers the first frames so even
that render is not a blank rectangle (`zoom_anchor` already quantises to permille, so such a level
keys the same way).

Pan, rightward from wherever the anchored zoom (six steps, 3815 permille) left the viewport, to the
page edge. Reach is `page_w - view_w`: 8219 px on the A0 (page_w 9103, view_w 884), 1453 px on the
200p (page_w 2337). Each row is `--bench-pan PX --bench-pan-steps N --bench-step-ms 16`; `Steps` is
how many the phase actually took before the edge stopped it, and `Travel` how far the viewport
moved from where the zoom left it.

| Fixture | Step | Steps | Travel | Speed | Hole episodes | Hole frames | Worst frame | Worst pending | Unwanted |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `large_200p.pdf` | 64 px / 16 ms | 10 | 637 px | 4275 px/s | 0 | 0 | 0 | 4 | 0 |
| `large_format_a0.pdf` | 64 px / 16 ms | 109 | 6912 px | 3912 px/s | 0 | 0 | 0 | 7 | 0 |
| `large_format_a0.pdf` | 400 px / 16 ms | 18 | 6800 px | 24727 px/s | 0 | 0 | 0 | 7 | 0 |

The wheel path is clean, and the ring is why. Every pan run ends with `unwanted=0` - the app never
rasterizes a region it has already scrolled past - and with **0 hole episodes and 0 hole frames**:
not one frame of a 6912 px pan at 3900 px/s or of a 6800 px pan at 24700 px/s showed a soft cell, and
`worst_frame` is 0 in all three rows. `pending` stays at the tiles one step can uncover, at most 7
across the rows. The pre-ring run of the same three commands is the baseline this replaced: **3, 9
and 13 hole frames**, in episodes of 2-3 of the 6 visible cells missing, i.e. the moving strip of
low-resolution page the ring exists to remove. §7.6's faster sweep (512 px per 50 ms, 10240 px/s)
agrees at a higher rate: **0 hole frames** with the ring against **241 of 296 frames** without it,
and the cost is the ring cells the model cannot hold (~3x the tile rasters at the default row
count).

The A0 rows fire six zoom steps before the pan, so the summary's `t_blank` in those runs still
describes the zoom burst (1-5 ms across the runs here) - by design, since a pan window does not
clear it, which is also what stops a zoom burst hiding inside a pan run. The pan's own numbers are
on its `window pan` line, and they are the point of the row: `holes episodes=0 frames=0` and
`worst_frame=0` at 3912 and 24727 px/s, against `holes episodes=1 frames=18` for the zoom that
preceded them - the burst, not the pan, is where soft cells are.

**Phase 2 gate.** Against the 50 ms default ceiling: the 200p fixture passes settled and in a burst;
the A0 passes settled (2 ms) and passes a six-step burst 16 ms apart (1-2 ms) once the document has
rendered once, and five repeats of that same burst pass too (across the five runs, worst `t_blank`
2/2/10 ms and worst `t_full` 9/10/20 ms). `abort_ms` stays at or below 2 ms wherever the harness
sampled one - 2/2/2 ms across the five repeats, 0 ms on each of the failing run's 3 aborts - so
cancellation is not what any row pays for. Panning shows no holes at all, at 3900 or 24700 px/s.
The single FAIL is a burst fired while the document's own first render is still running: it costs the
remainder of that render once per open (714 ms in this run, inside the pre-pool 124-929 ms
envelope), not once per step, and it is §7.2-7.3's one-off, which a cold open pays whether or not the
user zooms - the pool does not move it, because the build it waits on is serial. So the gate turns on
one question: is that one-off, on the heaviest fixture, acceptable for Phase 2 now that it is not a
zoom-scaling defect - or does the stretched-level item belong in this phase? With these numbers in
hand the plan's answer is Phase 3 (§5): what is left is the document's own cold open, spent once per
open and unmoved by the pool, and Phase 3's stretched level is the item that covers those first
frames.

### 7.6 Resident memory: what a plateau follows

§7.5's harness samples the process, not only the frame. With any trace on, `crates/app/src/bench.rs`
reads the Windows process-memory counters every 250 ms - `GetProcessMemoryInfo` through a
hand-written `extern` block, no new dependency - and logs them on the same timeline as the level
changes and the aborts:

```
[bench] rss t=3350ms ws=160.8MiB private=210.4MiB cache=12.0MiB/16 rows dropped=2 evicted=4
```

| Field | What it is |
| :--- | :--- |
| `ws` | `WorkingSetSize`: resident bytes, the number a task manager shows as the process's memory |
| `private` | `PrivateUsage`: committed private bytes, the part a tile cache can actually grow |
| `cache` / `N rows` | the scheduler's own `cache_bytes` / `cache_len` at that instant |
| `dropped` | tiles never placed: a superseded epoch, more wanted cells than rows, or no free row |
| `evicted` | tiles released **to stay inside the byte budget**; `dropped` does not count these |

The summary adds `rss_mib` with `ws` and `private` as min/median/max over the samples, `peak_ws`
(a monotonic high-water mark, so a spike between two samples still shows) and `n`, the sample
count. Off Windows the counters have no implementation: the `rss` lines carry `-` and the summary
says `n/a`.

**The cache has two ceilings, and the byte budget is the higher one.** A `TileScheduler` never holds
more tiles than it has model rows (§4.6), so on an 884 px-wide viewport with a 16-row model - four
to nine visible cells, depending on where the viewport sits - each cached 512 px tile is 768 KiB of
RGB8 and the cache stops at **12 MiB**
whatever the budget says. The 64 MiB default never binds there, `evicted` stays at zero, and a long
pan of a 6400% page plateaus at 12 MiB *of cache* - run A below, to the byte - because that is the
row count, not the budget. A run whose plateau has to follow the budget raises the rows with it,
which is what `--cache-rows` is for; 0.75 MiB tiles need more rows than 1 MiB ones did, so the flag's
value in the commands below is 96 (72 MiB of tiles) rather than 80.

**The prefetch ring is why the cache sits at its ceiling.** `AppState::refresh` follows the viewport
diff with a ring one tile around it (`TileScheduler::prefetch`, §4.5), so the column a pan is about to
uncover is already rendered when the pan takes it: run A's pan phase reports **0 hole episodes and 0
hole frames** across its 296 steps, where the same 512 px / 50 ms sweep showed holes in 241 of them
while a full cache still blocked the ring from being requested. The ring is paid for in raster work,
not in memory: run A renders **1937** tiles where the pre-ring measurement of this same sweep rendered
601, because at the default row count the model holds the visible cells plus only part of the ring, and
the rest is rendered, evicted and rendered again. Run B, with 96 rows, holds more of it and renders
fewer tiles (1523); run C, with the budget cutting the cache to 10 rows, holds almost none of it and
renders 3213. The `ponytail:` note on `AppState::ring` names that ceiling and its fix; what the user
buys with it is a fast pan that never shows a blank column.

One scenario, three configurations: `large_format_a0.pdf` at 6400% (19 zoom steps from 100%, the
298x422 grid of 125,756 tiles at 768 KiB each of §7.4), then a rightward sweep of 512 px device-pixel
steps - one tile column per step - until the page edge. Reach is `page_w - view_w`, 151,692 device px
on the 884 px viewport, so a few hundred steps uncover hundreds of distinct tiles across the visible
tile rows; all three runs below reach the edge at 151,692 px, `--bench-pan-steps 320` is an upper bound
rather than a count, and both the step count and the tile counts scale with the window.

```
:: A. the default: the row count caps the cache at 12 MiB, so the budget cannot bind
target\release\app.exe crates\engine-mupdf\tests\fixtures\large_format_a0.pdf ^
  --bench-zoom 19 --bench-step-ms 50 --bench-settle-ms 2000 ^
  --bench-pan 512 --bench-pan-steps 320

:: B. 64 MiB, with the rows the budget needs to bind (96 rows == 72 MiB of tiles)
target\release\app.exe crates\engine-mupdf\tests\fixtures\large_format_a0.pdf ^
  --bench-zoom 19 --bench-step-ms 50 --bench-settle-ms 2000 ^
  --bench-pan 512 --bench-pan-steps 320 --cache-mib 64 --cache-rows 96

:: C. the same sweep with a budget well under the row count
target\release\app.exe crates\engine-mupdf\tests\fixtures\large_format_a0.pdf ^
  --bench-zoom 19 --bench-step-ms 50 --bench-settle-ms 2000 ^
  --bench-pan 512 --bench-pan-steps 320 --cache-mib 8 --cache-rows 96
```

| Run | `cache` plateau | `evicted` | `dropped` | tiles rendered | `rss_mib` ws min/med/max | `peak_ws` |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| A. default (64 MiB budget, 16 rows) | 12.0 MiB, 16/16 rows | 0 | 123 | 1937 | 84.9 / 160.4 / 163.3 | 192.7 |
| B. `--cache-mib 64 --cache-rows 96` | 63.8 MiB, 85/96 rows | 1438 | 123 | 1523 | 84.9 / 241.4 / 255.7 | 267.3 |
| C. `--cache-mib 8 --cache-rows 96` | 7.5 MiB, 10/96 rows | 3203 | 123 | 3213 | 84.7 / 154.1 / 161.1 | 185.6 |

*Release build (RGB8 tiles and the prefetch ring), one run each, measured 2026-10-05 on the machine
§1's tables come from, on an 884 px-wide viewport (a 16-row model, which is why A stops at 16 rows) -
each run is a 21-23 s trace with 58-62 samples and `VERDICT PASS` at `t_blank` 1-16 ms. The sampled
columns are single runs, so treat the plateaus as the finding and the min/med/max as one run's spread.
The Phase 2 gate run (§7.5) has since filled in that section's header from the same build; these
three commands stay as measured, and a run re-measured in a differently sized window differs in the
row count, not in the shape.*

The three plateaus follow their ceilings, which is the point: A stops at the row count (12 MiB,
`evicted=0`), B at the budget (63.8 MiB of the 64), C at the budget (7.5 MiB of the 8). `dropped` does
not move with the budget - 123 in all three, the 19-step zoom's superseded tiles - because the sweep is
slow enough for the pool to keep up; `evicted` is the counter that tracks the budget, and C's 3203 is
the ring being rendered and thrown away under a budget that cannot hold it (the `ponytail:` note on
`TileScheduler::prefetch`). The 52 MiB A and B differ by shows up in `ws`, not only in `private`: the
median moves 160.4 to 241.4 MiB, so freed tile bitmaps do go back to the allocator. `private` moves
further, 210.8 to 352.4 MiB at the medians, because `PrivateUsage` counts committed pages the allocator
has not released - which is why both are logged rather than only the one that flatters the cache.

**What that means against the plan's memory target.** The plan's Phase 2 target is one metric - the
process *working set* - and an absolute ceiling of 250 MiB for the acceptance configuration, which is
the default cache run A uses: the measured `peak_ws` is **192.7 MiB**, so it holds, and nothing grows
without bound across 19 zoom steps and a 296-step pan. Two honest qualifications. Run B raises the rows
so the 64 MiB budget can bind, and there `peak_ws` is **267.3 MiB**: a deliberately larger cache buys a
larger process, so the 250 MiB ceiling is a property of the default configuration rather than of every
configuration. And the cache explains only 12-64 MiB of a 160-244 MiB plateau: with the default cache
the process sits about **75 MiB above its own empty baseline** (84.9 MiB `ws`) while the cache accounts
for 12 of those, which is the fixed overhead the plan's §4.9 does not explain and
[`backlog.md`](./backlog.md) tracks as work rather than as a result.



## 8. Plan 0002: continuous multi-page scroll

### 8.1 Page sizes at open (Phase 0)

`crates/engine-mupdf/tests/page_sizes_bench.rs`, release build, 2026-10-20, the §1 machine
(`rustc 1.98.0`). One run per row. The documents are generated by the test into Cargo's
`CARGO_TARGET_TMPDIR`: a root `Pages` node carrying `MediaBox [0 0 612 792]`, 64 kids per
intermediate node, one shared content stream, every 7th page A4 landscape, every 11th `/Rotate 90`,
every 13th cropped, and optionally one link annotation per page. *open* is `Document::open` plus the
page count; *load* is `MupdfEngine::all_page_sizes` (one `fz_load_page` + `fz_bound_page` per page);
*object* reads the page object and MuPDF's page CTM without loading the page. Each path runs on its own
freshly opened document, and the test asserts the two agree on every page before it prints a time.

```powershell
cargo test --release -p engine-mupdf --test page_sizes_bench -- --ignored --nocapture
# a real file as well:
$env:FA_PDF_BENCH_PDF = "path\to\real.pdf"; cargo test --release -p engine-mupdf --test page_sizes_bench -- --ignored --nocapture
```

| Document | open | load | load / page | object | object / page | mismatches |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1,000 pages | 18.8 ms | 7.6 ms | 7.59 us | 7.8 ms | 7.76 us | 0 |
| 1,000 pages, annotated | 12.3 ms | 13.3 ms | 13.29 us | 10.6 ms | 10.64 us | 0 |
| 5,000 pages | 13.3 ms | 36.5 ms | 7.29 us | 39.0 ms | 7.80 us | 0 |
| 5,000 pages, annotated | 14.3 ms | 67.8 ms | 13.56 us | 54.4 ms | 10.88 us | 0 |
| 20,000 pages | 21.9 ms | 145.9 ms | 7.29 us | 148.7 ms | 7.43 us | 0 |
| 20,000 pages, annotated | 22.8 ms | 251.7 ms | 12.59 us | 220.7 ms | 11.03 us | 0 |

Reading: linear in the page count, 7-13 us a page; the page load adds at most ~20% over resolving
the page object, so it is not worth avoiding. These are **best cases** - uncompressed objects, a flat
page tree, a correct xref - and no real-world document has been measured yet (Plan 0002 §3.5, §9).

### 8.2 Metrics for the Phase 1 and 2 runs (defined, not yet measured)

The §7.5-7.6 fields (`t_blank`, `t_full`, `holes`, `hole_frames`, `unwanted`, `abort_ms`, `ws`,
`peak_ws`, `private`) keep their definitions. Added by Plan 0002:

| Field | Definition |
| :--- | :--- |
| `pages_resident` | Pages that hold anything evictable at the sample instant - a cached tile, a page image, a base layer or an engine display list - counted once per page. Logged on the `rss` line. The virtualization claim is that it never exceeds the visible pages plus the margin. |
| `pages_visited` | Distinct pages that were visible at least once during the run. |
| `ws_growth` | `peak_ws` of the run minus `peak_ws` of the same scenario on a one-page document of the same page size; the "RSS does not grow with page count" acceptance compares this against 10 MiB. |
| `reanchors` | Canvas-window re-anchors (Plan 0002 §4.2) during the run, each with the frame time of the frame it happened in and whether a fling was running. |
| `t_page_table` | From `EngineCmd::Open` to the layout holding every page size; the open budget of Plan 0002 §6 row G (100 ms) is stated against it. |

> **Amended before the first run (2026-10-20):** `ws_growth` was first defined against "the same
> scenario on a one-page document", which cannot be run - a one-page document ends a 30 s scroll
> after one page. It is measured instead as the scroll phase's last 5 s against its first 5 s (medians
> of the 250 ms samples) in the same run. `reanchors` reports the time the rewrite itself took
> (`reanchor_us`), not the frame time, which the harness cannot see. Both changes were made before any
> Phase 1 number existed.

### 8.3 Phase 1 gate run

Release build of `07e3403` plus the summary-line fix, 2026-10-20, the §1 machine, a real 900x700
window (scale factor 1), `FA_PDF_TRACE=1`, `--bench-step-ms 16 --bench-settle-ms 3000`, one run per
row unless stated. The synthetic documents are §8.1's annotated ones, written by `page_sizes_bench.rs`
into `target/tmp/`. `--bench-zoom 10` lands at 931% and `--bench-zoom 19` at 6400% (permille steps of
1.25 from 100%). `--bench-scroll` writes the scroll position every tick through `AppState::scroll_to`,
so it measures layout, virtualization and re-anchoring, not the `Flickable`'s own fling animation.

```powershell
$fx = "crates\engine-mupdf\tests\fixtures"; $t = "target\tmp"
$env:FA_PDF_TRACE = 1
target\release\app.exe $fx\large_200p.pdf --bench-zoom 10 --bench-scroll 10000 --bench-scroll-s 30 --bench-step-ms 16 --bench-settle-ms 3000                    # A
target\release\app.exe $t\synthetic_1000p_annots_true.pdf --bench-scroll 10000 --bench-scroll-s 30 --bench-step-ms 16 --bench-settle-ms 3000                    # B1
target\release\app.exe $t\synthetic_1000p_annots_true.pdf --bench-zoom 19 --bench-scroll 200000 --bench-scroll-s 30 --bench-step-ms 16 --bench-settle-ms 3000  # B2
target\release\app.exe $t\synthetic_20000p_annots_true.pdf --bench-scroll 10000 --bench-scroll-s 30 --bench-step-ms 16 --bench-settle-ms 3000                   # B3
target\release\app.exe $t\synthetic_20000p_annots_true.pdf --bench-zoom 19 --bench-scroll 200000 --bench-scroll-s 30 --bench-step-ms 16 --bench-settle-ms 3000 # B4
target\release\app.exe $t\synthetic_1000p_annots_true.pdf --bench-jump 900 --bench-zoom 19 --bench-repeat 3 --bench-step-ms 16 --bench-settle-ms 3000           # D
```

| Row | Scenario | pages visited | `pages_resident` max | hole frames (scroll) | `t_blank` worst | re-anchors (from scroll) | `ws` start / median / `peak_ws` (MiB) | `ws_growth` |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | :--- | ---: |
| A | 200p @ 931%, 10,000 px/s, 30 s | 42 | 4 | 682 of ~1875, worst 3 cells | 5 ms | 0 | 88.8 / 150.6 / 165.5 | +3.3 |
| B1 | 1000p @ 100%, 10,000 px/s, 30 s | 398 | 5 | 0 | - | 0 | 90.0 / 132.8 / 140.7 | -1.2 |
| B2 | 1000p @ 6400%, 200,000 px/s, 30 s | 126 | 4 | 3750 (every tick) | 6 ms | 7 | 90.2 / 168.2 / 179.7 | **+22.0** |
| B3 | 20000p @ 100%, 10,000 px/s, 30 s | 398 | 5 | 0 | - | 0 | 116.4 / 161.9 / 169.9 | -1.6 |
| B4 | 20000p @ 6400%, 200,000 px/s, 30 s | 126 | 3 | 3750 (every tick) | 6 ms | 7 | 117.2 / 194.3 / 204.3 | **+16.6** |
| D | 1000p, jump to 900, zoom to 6400%, 3 repeats | 3 | 6 | - | 6 ms (5/5/6 per run) | - | 89.8 / 141.1 / 170.5 | n/a |

Re-anchor rewrites took 1-44 us each (B4 worst). The model grew 2-3 times per run and was never
rebuilt; `dropped` stayed 0 except D's 16 superseded zoom tiles; `evicted` stayed 0.

Reading, row by row against Plan 0002 §6:

- **Virtualization holds.** `pages_resident` never exceeded 6 while 42-398 pages went past, and the
  same scroll over 1000 and 20,000 pages (B1/B3) rendered the identical 1594 tiles.
- **RSS does not grow with pages *visited*** at 100% (B1/B3 `ws_growth` -1.2 / -1.6 MiB) or at 931%
  (A, +3.3). At 6400% and 200,000 px/s (B2/B4) the metric reads **+22.0 / +16.6 MiB - over the 10 MiB
  line, reported as a FAIL**. Splitting each scroll phase into thirds shows where it comes from: the
  median `ws` of the thirds is 159.7 / 171.1 / 171.0 MiB in B2 and 189.5 / 198.0 / 195.8 MiB in B4, so
  the rise is in the first third and the last two are level. Inside them `ws` moves in a band (B2
  144.0-172.8 MiB after t=11.7 s, B4 176.1-201.2 MiB after t=17 s) without a trend. The first-5 s
  window catches the warm-up of a scroll that renders 1330 tiles a second; the band is the allocator
  holding and returning freed tile buffers, the question `backlog.md` item 2 already asks. Two 30 s
  runs cannot rule out a slower leak; a longer run is the way to settle it.
- **RSS does grow with the document's page count, at open.** B3 starts 26.4 MiB above B1 and B4 27.0
  above B2 before a single scroll step: ~1.4 KiB a page for 19,000 more pages, far more than the
  ~64 bytes of page table the plan's §4.3 counted. It is MuPDF's object cache holding every page
  object `page_sizes_lenient` loaded at open. Recorded in `backlog.md` item 4; it does not grow after
  open.
- **Holes:** 0 hole frames at 100%. At 931% a tick moves 160 px and 682 of the ~1875 ticks had 1-3
  soft cells; at 6400% and 200,000 px/s a tick moves
  3200 px, more than five viewports, so every tick is a fresh viewport and the row exists to exercise
  re-anchoring, not to be read as a hole rate.
- **Row D passes:** after the jump and 19 zoom steps the page under the viewport centre is still page
  900 in all three repeats, and the worst `t_blank` is 6 ms against the 50 ms ceiling. The sub-pixel
  half of row D is the `core` test `a_zoom_keeps_the_anchored_page_point_still_across_the_range`.
- **Row C** is the `core` test `crossing_a_boundary_only_adds_the_newly_exposed_cells`.
- **Row E is half measured.** Re-anchors happen where §4.2 says (7 in 6 M px of travel, none in
  300 k px) and cost microseconds. Whether one interrupts a real *fling* - the `Flickable`'s own
  animation - cannot be scripted by this harness, which writes positions; it needs a manual check
  (Plan 0002 Phase 1 results).
- **Every row stays under the Plan 0001 ceiling** (`peak_ws` <= 250 MiB): worst 204.3 MiB (B4).

### 8.4 Phase 2: navigation (row F)

Release build of `a641933`, 2026-10-20, the §1 machine; the run header reports the viewport as
884x612 logical px at scale factor 1, which also confirms §8.3's window.

```powershell
$env:FA_PDF_TRACE = 1
target\release\app.exe target\tmp\synthetic_1000p_annots_true.pdf --bench-nav --bench-step-ms 200 --bench-settle-ms 2000
```

`--bench-nav` performs 20 steps, one every 200 ms, starting from the top at 100%. Keys and ctrl+wheel
are sent with `Window::dispatch_event` (the same `FocusScope` and `TouchArea` path as real input,
modifiers included); the page field, the fit buttons and the scrollbar through the callbacks their
widgets invoke. Before each step the harness computes the expected zoom and page from `core::nav` on
the state the step will act on, and for scroll steps the exact document position too; on the next
tick it compares with what the window shows.

| Step | Expected = observed |
| :--- | :--- |
| End / Home | page 1000, y=754982 / page 1, y=0 |
| Go to page 900 (field) | page 900 |
| PgDn, Down, Up, PgUp | page 901 y=680025, 901 y=680065, 901 y=680025, 900 y=679474 |
| Space, Shift+Space | page 901 y=680025, page 900 y=679474 |
| Ctrl+=, Ctrl+-, Ctrl+-, Ctrl+0 | zoom 1250, 1000, 800, 1000; page 900 throughout |
| Ctrl+wheel +60 / -30 px | zoom 1250 (one notch = one step) / 1118 (half a step back) |
| Fit width / Fit page | zoom 1029 / 1000 (page 900 is A4 landscape: 868 px of room over 842 pt wide, 596 over 595 tall) |
| Scrollbar to end / to top | page 1000 / page 1 |

**20 of 20 steps pass, `VERDICT PASS`.** The check can fail: with `PageDown` mapped to a line step
in `key_command`, the same run reports `nav PgDn expected ... page=901 y=680025 got ... page=900
y=679514 FAIL`, `19 of 20`, `VERDICT FAIL`, exit code 1 (mutation reverted before commit).

## 9. Plan 0003: v0.1 viewer

### 9.1 Text extraction and search (Phase 0)

`crates/engine-mupdf/tests/text_bench.rs`, release build, 2026-10-20, the §1 machine, one run. The
documents are generated by the test into `CARGO_TARGET_TMPDIR`: Letter pages of 50 lines of 10 pt
Helvetica (a base-14 Type 1 font, not embedded), 3,827 characters a page, uncompressed, the word
`podefa` once on every page. Per page, on a freshly opened document: *stext* is `load_page` plus
`to_text_page(TextPageFlags::empty())`; *search* is `TextPage::search` for `podefa` (hit) and for
`zzqqxx` (miss); *chars walk* iterates every block, line and char.

```powershell
cargo test --release -p engine-mupdf --test text_bench -- --ignored --nocapture
```

| Document | stext mean / worst | search hit / miss | chars walk | hits | whole document |
| :--- | ---: | ---: | ---: | ---: | ---: |
| 200 pages | 10.225 / 1828.59 ms | 0.460 / 0.426 ms | 0.011 ms | 200 | 2248.0 ms |
| 1000 pages | 2.211 / 1133.22 ms | 0.459 / 0.428 ms | 0.014 ms | 1000 | 3211.4 ms |

The worst page is the first one in each document - MuPDF's one-off first touch, the same effect
§7.2 measured for the first display list (1813 ms) - and it dominates the *mean*. Without it, the
steady-state extraction is (10.225 x 200 - 1828.6) / 199 = **1.09 ms** and (2.211 x 1000 - 1133.2) /
999 = **1.08 ms** a page. One query is extraction plus one search, **~1.5 ms a page**: about 1.5 s for
1000 pages and ~31 s for 20,000, plus the first touch. (The *whole document* column runs both searches
and the char walk, ~2.1 ms a page after the first.) These are best cases (one non-embedded font, no
compression); real documents are measured with `FA_PDF_BENCH_PDF`.

### 9.2 File dialog binary size (Phase 0)

A throwaway crate outside the repository, built with this workspace's release profile
(`opt-level = "z"`, `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, `strip = true`), Windows
x86_64 MSVC, `rustc 1.98.0`, 2026-10-20:

| Binary | Bytes |
| :--- | ---: |
| `main` printing its first argument | 116,224 |
| `main` calling `rfd::FileDialog::new().add_filter("PDF", &["pdf"]).pick_file()` (`rfd = "=0.17.2"`) | 117,760 |

**+1,536 bytes** on Windows, where the dialog itself is the operating system's. `cargo deny check
licenses bans` with this repository's `deny.toml` passed for every `rfd` dependency (it failed only on
the unlicensed probe crate). Linux and macOS sizes were not measured.

### 9.3 Metrics for the Plan 0003 runs (defined, not yet measured)

| Field | Definition |
| :--- | :--- |
| `t_first_hit` | From the moment a search is sent to the search thread (after the find bar's debounce) to the first `SearchHits` event the app accepts. |
| `t_search` | From the same moment to `SearchDone`, with the page count searched; reported with the per-page cost so it can be compared with §9.1. |
| `stale_hits` | Hits of a superseded search generation that reached the model. Must be 0. |
| `thumbs_resident` | Thumbnail images held by the app at the sample instant; logged on the `rss` line like `pages_resident`. |
| `ws_after_open_n` | Working set sampled 2 s after the n-th open of the same document in one process (row H). |
| `ws_after_search` | Working set just before a search starts, right after `SearchDone`, and 10 s later, with the search thread's document still open (row O). |

### 9.4 Phase 1: reopening a document (row H)

Release build of `e289558` plus the `--bench-reopen` harness, 2026-10-20, the §1 machine, viewport
884x612 logical px at scale factor 1. The 20,000-page annotated synthetic document of §8.1 is opened
at startup, then reopened through the same `open_path` the File menu uses; each sample is the working
set 3 s after the open before it.

```powershell
$env:FA_PDF_TRACE = 1
target\release\app.exe target\tmp\synthetic_20000p_annots_true.pdf --bench-reopen 10 --bench-step-ms 3000 --bench-settle-ms 3000
target\release\app.exe target\tmp\synthetic_20000p_annots_true.pdf --bench-reopen 30 --bench-step-ms 3000 --bench-settle-ms 3000
```

| Run | `ws_after_open_n` (MiB) |
| :--- | :--- |
| 10 opens | 150.5, 157.8, 160.9, 161.8, 160.7, 161.9, 162.6, 161.9, 161.6, 162.2 |
| 30 opens | 150.7, 157.1, 159.5, 160.4, 160.3, 160.6, 161.5, 162.0, 162.9, 161.1, 160.8, 161.7, 161.2, 161.8, 161.2, 161.5, 161.2, 161.3, 161.2, 161.5, 162.7, 162.8, 162.1, 161.0, 161.4, 161.5, 163.6, 164.5, 163.6, 163.9 |

**Row H fails its line:** the tenth open sits **+11.7 MiB** above the first (limit +10 MiB), the
thirtieth +13.2 MiB. The series shows where it comes from: the second and third opens add +10.4 MiB (10-open run) and +8.8 MiB (30-open run),
then a band - opens 3-26 stay within 159.5-162.9 MiB. Across the 27 opens from the fourth to the
thirtieth it rises 3.5 MiB (about 0.13 MiB an open), with the last four samples the highest of the
run; two runs cannot tell that drift from noise, so a slow per-open leak is **not ruled out**
(`backlog.md` item 5). Every sample stays under the 250 MiB ceiling; the run's `peak_ws` was 165.4
MiB.

### 9.5 Phase 2: thumbnails and outline (rows J and K)

Release build of `fd921fd` with the inward-only zoom in `--bench-thumbs`, 2026-10-20, the §1
machine, viewport 884x612 logical px at scale factor 1, `--bench-step-ms 16 --bench-settle-ms 3000`
for row J and `--bench-step-ms 300` for row K.

```powershell
$env:FA_PDF_TRACE = 1
target\release\app.exe target\tmp\synthetic_1000p_annots_true.pdf --bench-thumbs 20000 --bench-scroll-s 30 --bench-step-ms 16 --bench-settle-ms 3000
target\release\app.exe target\tmp\synthetic_20000p_annots_true.pdf --bench-thumbs 200000 --bench-scroll-s 30 --bench-step-ms 16 --bench-settle-ms 3000
target\release\app.exe target\tmp\outline.pdf --bench-outline --bench-step-ms 300 --bench-settle-ms 1000
```

`--bench-thumbs` opens the sidebar, scrolls the thumbnail list down at the given speed (logical px/s)
until its end or `--bench-scroll-s`, and zooms the viewport in one step every second, so `t_blank` is
measured while the pool also renders thumbnails. (A first version alternated zoom in and out; the
levels it went back to came from the tile cache and logged a single `t_blank` sample, so it was
replaced before these runs.) `outline.pdf` is written by `crates/engine-mupdf/tests/sidebar_test.rs`.

| Row | Document | Result |
| :--- | :--- | :--- |
| J | 1,000 pages, 20,000 px/s | list scrolled to row 997 of 1000; `thumbs_resident` max **64** (cap 64); `t_blank` 0/1/2 ms (min/med/max, n=11), `t_full` 2/3/11 ms; `peak_ws` 180.6 MiB; **PASS** |
| J | 20,000 pages, 200,000 px/s | list scrolled to row 19997 of 20000; `thumbs_resident` max **64**; `t_blank` 0/1/2 ms (n=19), `t_full` 1/4/5 ms; `peak_ws` 209.2 MiB; **PASS** |
| K | outline.pdf, 4 entries | `Chapter 1` y=108, `Section 1.1` y=1100, `Chapter 2` y=1600 (page top), `Website` (no page) does not move: **4 of 4** land where expected. Re-run after `7ef83cf` with the tree opened only by title clicks: 4 of 4, the row count going from 3 to 4 when `Chapter 1` is clicked; with that expansion removed the run fails (`rows=3`, exit code 1). |

Row K's expected positions are computed in the harness from the layout and the outline entry, not by
the app's `go_to_dest`; the entries' `top` values themselves (100 and 292 pt below the page top, from
`/XYZ 0 692` and `/FitH 500` on a 792 pt page) are asserted in `sidebar_test.rs`.

### 9.6 Phase 3: search (rows L, M, O)

Release builds of `bd894f8` (first runs) and `2a8ae47` (search document warmed up at open), 2026-10-20,
the §1 machine, viewport 884x612 logical px at scale factor 1, `--bench-step-ms 16`. The text
documents are §9.1's generator (`synthetic_text_1000p.pdf`, and `synthetic_text_20000p.pdf` from
`FA_PDF_BENCH_PAGES=20000`), whose run also re-measured the per-page costs at 20,000 pages: 0.884 ms
extraction and 0.371 / 0.358 ms search (hit / miss) a page, first page 1435.63 ms.
`search_400p.pdf` is written by `crates/engine-mupdf/tests/search_test.rs` (`podefa` on every page,
`rare` on page 1 only).

```powershell
$env:FA_PDF_TRACE = 1
target\release\app.exe target\tmp\synthetic_text_1000p.pdf --bench-search podefa --bench-search-zoom --bench-step-ms 16 --bench-settle-ms 3000   # L
target\release\app.exe target\tmp\search_400p.pdf --bench-search podefa --bench-search-replace rare --bench-search-expect 1 --bench-step-ms 16 --bench-settle-ms 3000   # M
target\release\app.exe target\tmp\synthetic_text_20000p.pdf --bench-search podefa --bench-step-ms 16 --bench-settle-ms 10000   # O
target\release\app.exe target\tmp\synthetic_text_20000p.pdf --bench-zoom 19 --bench-step-ms 1000 --bench-settle-ms 3000   # control, no search
```

| Run | `t_first_hit` | search (thread / wall) | hits | `ws` before / done / at report |
| :--- | ---: | :--- | ---: | :--- |
| L, `bd894f8` | 816 ms | 2184 / 2187 ms, 2.18 ms/page | 1000 | 129.6 / 172.5 / 172.5 MiB (zooming) |
| L, `2a8ae47` | **1 ms** | 1390 / 1396 ms, **1.39 ms/page** | 1000 | 154.0 / 170.0 / 170.0 MiB (zooming) |
| M, `bd894f8` | 761 ms | 243 / 1004 ms (second search) | 1 of 1 expected; 1 stale hit refused | 145.9 / 183.3 / 183.2 MiB (zooming) |
| M, `2a8ae47` | 0 / 1 ms | 237 / 238 ms (second search) | **1 of 1 expected**; 3 stale hits refused | 159.0 / 162.8 / 162.7 MiB |
| O, `bd894f8` with zoom | 905 ms | 29563 ms, 1.48 ms/page | 20000 | not usable: zoom steps grew the tile cache meanwhile |
| O, `2a8ae47`, no zoom | **1 ms** | 27545 / 27549 ms, **1.38 ms/page** | 20000 | **167.8 / 180.4 / 180.4 MiB** |

- **`t_first_hit`**: MuPDF's first text extraction of a *document* costs about a second (§9.1 shows
  it on every generated document, not once per process), and the first search paid it: 761-905 ms.
  Since `2a8ae47` the search document is opened and its first page extracted when the document opens,
  and the first hit arrives 0-1 ms after the search is sent.
- **Search speed** is 1.38-1.39 ms a page in the app, against ~1.5 ms in §9.1 and the 2x limit of row L.
- **Row M**: the app refused 3 hits of the replaced search that were already in flight; the final
  result is exactly the one `rare` hit. The engine-side guarantee - no hit of the old search after
  the first of the new one, and the old search never reports done - is `search_test.rs`, which fails
  when the per-page generation check is removed (mutation run, reverted).
- **Row O**: a full search of 20,000 pages adds **12.6 MiB** of working set, still there 10 s later
  (the search document's object cache). Opening and warming the search document is included in the
  "before" figure; the whole second document is therefore about 1.2 KiB a page or less.

**`t_blank` during a search.** The L run with zoom took only one zoom step (the search lasted 1.4 s:
11 ms). The first O run zoomed 19 steps during a 30 s search: up to 2276% every step took 0-12 ms;
from 2845% to 6400% they took 128-190 ms. The control run - the same zoom steps on the same document
with **no search** - took 96-155 ms at those levels and 0-10 ms below them. So the slow levels are the
cost of rendering this text at very high zoom, not of searching (search adds ~20-35 ms there); this
is a new finding, since §7's fixtures have little text, and it is `backlog.md` item 6.

### 9.7 Mid-raster abort switched off (crash fix)

During the Phase 3 review the maintainer found that some PDFs end the process (exit code 1, no panic
message) when zoomed quickly. Cause, from the sources: a cancel aborts a tile through MuPDF's cookie;
`fz_run_display_list` then breaks out of its loop and leaves the clips and groups it had pushed on the
draw device's stack (`mupdf-sys-0.8.0/mupdf/source/fitz/list-device.c:2109-2114`); closing that device
throws "items left on stack in draw device" (`draw-device.c:3091-3092`), `fz_close_device` rethrows
it (`device.c:82-83`), and mupdf-rs 0.8 closes devices in `Drop for Device` with no `fz_try` around the
call (`patches/mupdf/src/device.rs:641-647`), so MuPDF reaches its uncaught-error path and calls
`exit(EXIT_FAILURE)` (`error.c:208-224`).

Evidence, release build, the §1 machine, `--bench-zoom 19 --bench-step-ms 16 --bench-settle-ms 3000`
on the maintainer's PDF: exit code 1 in 3 of 3 runs with the cookie, 0 in 3 of 3 with it off.
`crates/engine-mupdf/tests/abort_safety_test.rs` reproduces it with a generated page (2000 fills
inside two nested clips): the test binary exits with code 1 when the cookie is used and passes when it
is not.

Fix (maintainer's choice, a stop-gap): `MID_RASTER_ABORT = false` in `crates/engine-mupdf/src/pool.rs`.
A cancel still drops every queued tile of the epoch and still keeps a finished raster of a cancelled
epoch from being published; a tile already inside the rasterizer now runs to its end, and `t_abort`
becomes "cancel to end of that raster". Measured after the fix:

| Run | Per-run worst `t_blank` (ms) | `t_abort` (ms) | Result |
| :--- | :--- | :--- | :--- |
| maintainer's PDF, 19 steps, 3 runs | 19, 38, 14 | max 21, 73, 18 | no crash; PASS x3 |
| `large_format_a0.pdf`, 6 steps x5 | 4/4/14 (min/med/max of the runs' worst) | 0 | PASS |
| `dense_vector.pdf`, 6 steps x5, run 1 | across runs 26/36/193 | 20/23/28 | **FAIL** (one run over 50) |
| `dense_vector.pdf`, 6 steps x5, run 2 | 26, 229, 35, 30, 21 | 20, 30, 23, 21, 18 | **FAIL** (one run over 50) |

`dense_vector.pdf` is the worst case by design (one expensive node a tile). With the cookie it stayed
at 21-26 ms (`benchmarks.md` §7.5); without it, about one burst in five waits for heavy tiles already
rasterizing and its first new tile takes 190-230 ms. That is the measured cost of the stop-gap,
recorded in `backlog.md` item 7 together with the condition for undoing it: a mupdf-rs release that
carries `ffaf28f5` (messense/mupdf-rs#249, already on upstream `main`).

### 9.8 Phase 4: selection (row N)

Row N is a test, not a timing: `crates/engine-mupdf/tests/selection_test.rs` generates three Letter
pages whose line `l` of page `p` reads `p{p}l{l} alpha beta`, selects from the start of page 1's
line 48 to the end of page 3's line 1 through the engine, and compares the copied text with the 54
expected lines joined by `\n`, byte for byte: **equal**. The same file checks word and line snapping
and that a selection sent during a 400-page search is answered before the search finishes.

The pointer path in a real window, release build of `0a55e70`, 2026-10-20, the §1 machine, viewport
884x612 logical px at scale factor 1:

```powershell
$env:FA_PDF_TRACE = 1
target\release\app.exe target\tmp\select_3p.pdf --bench-select --bench-step-ms 400 --bench-settle-ms 1500
```

`--bench-select` sends `PointerPressed` / `PointerMoved` / `PointerReleased` through
`Window::dispatch_event` at window positions computed from the page layout:

| Step | Selected text | Result |
| :--- | :--- | :--- |
| drag from line 2 start to line 4 end | lines 2-4 | PASS |
| Shift+click at line 6 end | lines 2-6 | PASS |
| double-click inside `alpha` of line 9 | `alpha` | PASS |
| triple-click on line 11 | line 11 | PASS |

Ctrl+C is not scripted, so that a run never overwrites the user's clipboard; it is a manual check.

After the review fix (`2e4df61`, the first drag now starting on the first character of its line):

| Step | Expected | Result |
| :--- | :--- | :--- |
| the four steps above | as above | PASS x4 |
| click in the left margin (x = 20 pt, text starts at 54) | selection cleared | PASS |
| drag starting in the margin | nothing selected | PASS |
| pointer over text / in the margin / below the text | text cursor / arrow / arrow | PASS x3 |

9 of 9. With the off-text guard removed, the margin drag selects `p0l2 alpha beta` and the run ends
`VERDICT FAIL`, exit code 1 (mutation reverted).

## 10. Plan 0004: v0.2 annotate

### 10.1 Annotating and saving (Phase 0)

`crates/engine-mupdf/tests/annot_bench.rs`, release build, 2026-10-21, the §1 machine, two runs (the
numbers below are the second; the first agrees within 10%). The document is generated by the test:
1000 Letter pages of 50 lines of 10 pt Helvetica, 2,786,703 bytes, uncompressed.

```powershell
cargo test --release -p engine-mupdf --test annot_bench -- --ignored --nocapture --test-threads 1
```

| Question | Result |
| :--- | :--- |
| Display list, page contents only (`to_display_list(false)`, what the viewer does today) vs page with annotations (`true`), on 200 pages that have none | 0.290 vs 0.286 ms a page: no measurable cost |
| Does `to_display_list(false)` hide annotations? | Yes: the pixel under a new highlight is `[146,146,146]` (grey text) from the contents-only list and `[146,131,0]` from the full list |
| Create + appearance (`update()`): highlight / 200-point ink stroke / text note | 0.144 / 0.121 / 0.024 ms |
| Display list of a page with those 3 annotations | 0.374 ms |
| `has_unsaved_changes` fresh / after an edit / **after an incremental save** | false / true / **true** |
| `can_be_saved_incrementally` on the generated file | true |
| Incremental save **to the same path**, with this document and a second `Document` on the same file open | **ok**, 9.6 ms; file 2,786,703 -> 2,794,191 bytes; the original bytes are kept unchanged as a prefix |
| Reopened after that save | page 0 has its 3 annotations |
| Both open documents read the last page after the save | yes (the second one still sees the old revision) |
| A second edit and incremental save on the same open document | ok, 5.2 ms; the file is then 2,794,840 bytes, +649 over the first save. Read in Phase 1 (§10.3): MuPDF does not append a second section but truncates the file to its length at open and writes one section with every change since then, so this is the original plus one section holding both edits |
| Full (non-incremental) save to another path | ok, 15.4 ms, 2,789,569 bytes |
| `std::fs::rename` of that copy over the open file | **fails on Windows**: `os error 5` (access denied) |

Edits while workers render, `edits_while_workers_render` in the same file: 60 pages; three threads
replay the display lists of pages 0-29 in a loop while the test thread adds 600 highlights
(20 rounds over pages 30-59), calls `update()` and rebuilds each edited page's display list. Result:
600 edits in 474.9 ms (**0.792 ms an edit**, list rebuild included), 421 renders of the untouched
pages, **0 pixel mismatches** against renders taken before any edit, no crash.

Not measured here: Linux and macOS (appending to an open file is ordinary there, but it is first
checked by a CI test in Phase 1), real documents with existing annotations, and documents MuPDF
repairs at open (`can_be_saved_incrementally` false).

### 10.2 Metrics for the Plan 0004 runs (defined, not yet measured)

| Field | Definition |
| :--- | :--- |
| `t_edit` | From the moment the app sends an edit command to the moment the last visible tile of the edited page at the new page revision is published. |
| `t_save` | From the moment the app sends a save command to the `Saved` event, with the file size before and after. |
| `stale_tiles` | Tiles of an older page revision that reached the model after a tile of the newer revision of the same page and scale. Must be 0. |
| `ws_after_edits_n` | Working set sampled 2 s after the n-th scripted edit in one process, n = 0, 100, 1000. |
| `ink_points` / `ink_rebuild_ms` | Points in the stroke being drawn, and the time to rebuild its live preview path on one pointer move. |

### 10.3 Phase 1: editing and saving (rows P, Q, R, T, W)

Rows P and Q are tests, not timings:

- **Row P** (`crates/engine-mupdf/tests/annotation_render_test.rs`): a highlight, an ink stroke, a
  note and a stamp added to `large_200p.pdf`'s page 1 are drawn in a tile, the full-page render, a
  thumbnail and the base layer (the base layer through the actor), each checked against the page's
  contents-only render; all 200 pages of the unannotated fixture render byte-identically to the
  contents-only render at 50%. With `DRAW_ANNOTATIONS = false` the two annotated-page tests fail and
  the identity test passes (mutation check, 2026-10-21).
- **Row Q** (`crates/engine-mupdf/tests/edit_test.rs`): a selection from line 48 of page 1 to line 1
  of page 2 of a generated text document, marked through the actor and saved in place; reopened by
  a separate `PdfDocument`, each page has one highlight whose quads equal the selection's line boxes
  within 0.01 pt, and the original file is an unchanged prefix of the saved one. The same file
  checks revisions, recolour, delete, Save As (and Save As onto the open file), a non-PDF document,
  and a file MuPDF repaired at open (`large_200p.pdf`: `can_be_saved_incrementally` is false, so
  Save answers `NeedsSaveAs` and writes nothing). These run in CI on Windows, Linux and macOS.

**How MuPDF writes an in-place save.** `pdf_save_document` with `do_incremental` compares the file
on disk with the bytes it opened; if they match, it seeks to the opened length, truncates there and
writes one section holding every object changed since the open
(`mupdf/source/pdf/pdf-write.c`, `ensure_initial_incremental_contents`, read 2026-10-21). A second
save of the same session therefore replaces the first one's section instead of appending after it;
`edit_test.rs` asserts the opened bytes, not the previous save's, as the prefix.

Rows R and W through a real window, release build of `46147a9` plus `64bf648`'s harness change,
2026-10-21, the §1 machine:

```powershell
$env:FA_PDF_TRACE = 1
# R: 20 edits on large_200p.pdf (no text: one direct edit a tick at a line box)
target\release\app.exe target\tmp\edit_200p.pdf --bench-edit 20 --bench-edit-burst 1 --bench-step-ms 400 --bench-settle-ms 1500
# R/T path: 20 highlights by drag with the Highlight tool, then a save, on a select_3p.pdf copy
target\release\app.exe target\tmp\edit_bench.pdf --bench-edit 20 --bench-step-ms 400 --bench-settle-ms 1500 --bench-edit-save
# W: 100 edits in bursts of 5, scrolling 24 px down and back between bursts
target\release\app.exe target\tmp\edit_bench.pdf --bench-edit 100 --bench-edit-burst 5 --bench-edit-pan 24 --bench-step-ms 30 --bench-settle-ms 1500
```

After the edits every run clicks the highlight of line 2 through the window, picks the third
swatch and presses Delete, checking the selection, the colour and the annotation count after each
step (`t_edit` counts those two edits too, hence n = 22).

| Run | `t_edit` min / med / max | n | old-revision tiles dropped | tile requests during the edits | checks |
| :--- | ---: | ---: | ---: | :--- | :--- |
| R, `large_200p.pdf` | 2 / 3 / 12 ms | 22 | 0 | page 1 only (80 = 4 tiles x 20) | 4 of 4 |
| drag + save, `select_3p.pdf` | 2 / 3 / 12 ms | 22 | 0 | page 1 only (80) | 5 of 5; `t_save` 1 ms, 17,153 bytes |
| W, bursts + scroll | 3 / 10 / 16 ms | 22 | 2 | page 1 only (400) | 4 of 4; 100 highlights, then 99 |

- **Row R: pass** - median 3 ms against 100 ms.
- **Row W: pass** - the bursts did produce the race the revision exists for: 2 tiles drawn before
  a later edit of their page arrived after the app knew the newer revision and were dropped, so
  `stale_tiles` (one that reached the model) is 0; it is 0 by construction, because the check runs
  before the scheduler sees the tile. No page but the edited one was asked for again. Without bursts
  the edits finish before the next one and no tile is ever late (0 dropped in the other two runs).
- **Row T**: the save state is unit-tested (`pdf_core::annot`), the runs check `dirty=true` after the
  edits and `dirty=false` after the save; the dialogs are manual checks.

**Re-run after the review** (`8aa20cc`: new markup cuts old markup of the same kind; `e09a2b4`: a
click clears the selection's highlight), same commands, 2026-10-21:

| Run | `t_edit` min / med / max | n | old-revision tiles dropped | checks |
| :--- | ---: | ---: | ---: | :--- |
| drag + save, `select_3p.pdf` | 2 / 3 / 11 ms | 22 | 0 | 5 of 5; `t_save` 1 ms, 17,153 bytes |
| W, bursts + scroll | 4 / 8 / 14 ms | 22 | 0 | 4 of 4; 40 highlights (100 edits over 40 lines, each line's newest replacing the one before), then 39 |

No late tile arrived in this W run: whether one does depends on how the bursts fall against the
workers, which is why the earlier runs saw 5 and 2. `--bench-select` with the highlight rows
checked: 11 of 11 (8 of 11 with the old `show_selection`).

### 10.4 Phase 2: undo and redo (row S)

Row S is a test (`crates/engine-mupdf/tests/undo_test.rs`):

- **Setup:** 50 edits from a fixed linear congruential sequence over a generated five-page text
  document that already has two annotations. The mix is 36 markup (9 highlight, 8 underline, 9
  strike-out, 10 squiggly), 9 deletes and 5 recolours.
- **Undo and redo:** each step is compared with a snapshot of every page's annotations, ids
  included. All 50 undo back to the opened state and redo forward to the last edit. **Pass.**
- **Save after undo:** 20 edits, 8 undone, saved in place. The reopened file has the undone
  annotations (compared without object numbers), and the open document can still redo. **Pass.**
- **Mutation check:** without the journal operations around an edit, MuPDF refuses the change and
  3 of the 5 tests fail.
- **Android:** the same test passes 5 of 5 on the Galaxy A34 (Android 16), built with NDK r28c on
  the Debian mini PC.

Through a real window, release build of `3cac9de`, 2026-10-22, the §1 machine:

```powershell
target\release\app.exe target\tmp\edit_bench.pdf --bench-edit 20 --bench-step-ms 400 --bench-settle-ms 1500 --bench-edit-save
```

7 of 7 checks pass:

- The 20 highlights.
- The click that selects one, the recolour and the Delete.
- **Ctrl+Z** brings the deleted annotation back (20 again, `can_redo=true`).
- **Ctrl+Y** removes it again (19, `can_redo=false`).
- The save leaves nothing unsaved.

`t_edit` median 4 ms, worst 19 ms (n = 22, edits only). `t_save` 0 ms (17,153 bytes; below the
millisecond the harness reports).

### 10.5 Phase 3: notes (row U)

Row U is a test (`crates/engine-mupdf/tests/note_test.rs`):

- **Text round trip:** a note whose text mixes Turkish (`Öğrenci notu — çşığüÖ`), CJK, a check mark,
  an emoji and a line break is saved in place. A separate `PdfDocument` reads its `/Contents` back
  byte for byte. **Pass.**
- **Edit, move, undo, delete:** editing the text, moving the note, undoing each, deleting it and
  undoing the delete keep the note's id and text.
- **Empty text:** clearing the text clears the comment.

Through a real window, release build of `a4455dd`, 2026-10-22, the §1 machine:

```powershell
target\release\app.exe target\tmp\note_bench.pdf --bench-note --bench-step-ms 400 --bench-settle-ms 1500
```

On a copy of `select_3p.pdf`, 5 of 5 checks pass:

1. The Note tool opens the editor.
2. The note is placed at (300, 399) with its Turkish text.
3. Hovering shows the text.
4. A drag moves the note to (400, 159).
5. Ctrl+Z puts it back.

`--bench-edit` (7 of 7, `t_edit` median 3 ms) and `--bench-select` (11 of 11) were re-run on the
same build without a change.

The first run of `--bench-note` placed the note on line 45 (579 pt). That is below the viewport of
the default 900x700 window, so the scripted click never reached the page: 0 of 5. The run now uses
line 30.

### 10.6 Phase 4: ink (rows V, X)

Row V's engine half is a test (`crates/engine-mupdf/tests/ink_test.rs`): a 2000-point wave with
sub-point jitter is simplified at 0.5 pt (to under a quarter of its points), committed, saved in
place, and its ink list read back by a separate `PdfDocument` equals the simplified points within
0.01 pt. The line width reads back as set. Recolour, delete and undo keep the stroke.

Through a real window, release build of `83fff9e`, 2026-10-22, the §1 machine, viewport 884x612
logical, scale factor 1:

```powershell
target\release\app.exe target\tmp\ink_bench.pdf --bench-ink --bench-step-ms 400 --bench-settle-ms 1500
```

On a copy of `select_3p.pdf`, a 2000-point stroke drawn with pointer events:

- `ink_rebuild_ms` median **0.184**, max **0.570** over 2000 moves (`ink_points` = 2000). **Pass**
  (<= 4 ms).
- 4 of 4 checks: the committed stroke equals the simplified points the app sent and the live path
  is gone; Ctrl+Z removes it; Ctrl+Y brings it back; a click on the line selects it, one 100 pt
  away inside its bounds does not.

Row X, same build, one run each:

```powershell
target\release\app.exe target\tmp\mem_on.pdf --bench-edit 1000 --bench-edit-burst 10 --bench-step-ms 50 --bench-settle-ms 2000
$env:FA_PDF_NO_JOURNAL=1; target\release\app.exe target\tmp\mem_off.pdf --bench-edit 1000 --bench-edit-burst 10 --bench-step-ms 50 --bench-settle-ms 2000
```

| Journal | n = 0 | n = 100 | n = 1000 |
| :--- | ---: | ---: | ---: |
| on | 149.1 MiB | 156.0 MiB | 160.9 MiB |
| off | 152.1 MiB | 157.8 MiB | 163.4 MiB |

The edits cycle over 40 lines, so each highlight after the 40th replaces one (Plan 0004 Phase 1
review, option B) and the page keeps 40 annotations. 1000 edits cost about 12 MiB of working set
either way; the journal's share is smaller than the difference between two runs. Without the
journal the edit phase's "Ctrl+Z undoes the delete" check fails, as it must. `t_edit` median was
10 ms in both runs, under load of 10 edits per 50 ms tick.

`--bench-note` (5 of 5), `--bench-select` (11 of 11) and `--bench-edit 20` (6 of 6, `t_edit` median
3 ms) were re-run on the same build without a change.

## 11. Plan 0005: v0.3 organize and tools

### 11.1 Phase 0: page operations and tools through mupdf-rs 0.8

`crates/engine-mupdf/tests/tools_bench.rs`, release build, 2026-10-22, the §1 machine, one run.
The document is generated: 200 Letter pages of 50 Helvetica lines (540 KiB). Every operation that
changes the document runs with MuPDF's journal on and is undone afterwards; the test checks the
page order, rotation or text after each undo.

```powershell
cargo test --release -p engine-mupdf --test tools_bench -- --ignored --nocapture --test-threads 1
```

| Operation | Result |
| :--- | :--- |
| Move page 1 to the end | 0.49 ms; undo 0.01 ms |
| Rotate all 200 pages in one operation | 4.03 ms; undo 0.01 ms |
| Delete 100 pages | 1.41 ms; undone |
| Duplicate a page | 0.06 ms; undone |
| Crop all 200 pages | 4.44 ms (page 540x720 pt); undone |
| Merge 200 + 200 pages | 39.6 ms; full compressed save 53.6 ms, 105.5 KiB |
| Split pages 11-20 into a new file | 1.3 ms, 4.7 KiB |
| Lossless compress, text | 539.7 to 77.2 KiB in 23.2 ms |
| Export 10 pages as PNG at 150 dpi | 150.3 ms and 250.9 KiB per page |
| Import those 10 PNGs as pages, default save | 265.9 ms, **61 640.9 KiB** (streams stored uncompressed) |
| Lossless compress of that document | 61 640.9 to 2 472.7 KiB in 1949.3 ms |
| Watermark and page number on 200 pages, one operation | 13.5 ms (0.068 ms per page); undone and redone |

`a_garbage_collecting_save_breaks_undo` (same file): an edit, then a full save, then undo.

| Save | Change undone |
| :--- | :--- |
| Full, garbage collection level 4 | **no** (undo reports success) |
| Full, compressed, no garbage collection | yes |
| Full, default options | yes |
| Incremental, in place | yes |

### 11.2 Metrics for the Plan 0005 runs (defined, not yet measured)

| Field | Definition |
| :--- | :--- |
| `t_restructure` | From the moment the app sends a structural edit to the moment every visible tile of the new layout is shown. |
| `t_tool` | From the moment a tool starts on the tool thread to its output file being complete, with the page count. |

### 11.3 Phase 0: PDF to Word

`crates/engine-mupdf/tests/docx_bench.rs`, release build, 2026-10-22, the §1 machine:

| Input | Pages | Result |
| :--- | ---: | :--- |
| `minimal.pdf`, `large_200p.pdf`, `dense_vector.pdf` | 1, 200, 1 | 9.6 KiB each: no text to carry (`extract` reports one unsupported path on `dense_vector.pdf`) |
| `select_3p.pdf` | 3 | 10.1 KiB in 1445 ms (the writer's first use in the process) |
| 400-page merge of §11.1 | 400 | 60.7 KiB in 1465 ms (3.7 ms per page) |

In `word/document.xml` every page is one paragraph: its lines joined by spaces, no line or page
breaks, text in reading order.

Size: mupdf-rs' default features already compile the DOCX writer. The `docx_bench` test binary is
11 155 456 bytes; built with `docx-output` turned off (mupdf's other default features kept, a
temporary change to the workspace manifest) it is 10 909 184 bytes, so the DOCX writer and
`extract` add **246 272 bytes (0.23 MiB)**. A binary that uses no document writer at all (the same
test reading text instead) is 8 557 056 bytes: using `DocumentWriter` brings 2.48 MiB of writers.

### 11.4 Phase 1: page structure (rows AA, AB, AG)

Row AA's engine half is a test (`crates/engine-mupdf/tests/pages_test.rs`). It makes 40 edits from a
fixed random sequence on a 12-page document whose pages tell themselves apart by width: rotate, delete,
duplicate, move and insert blank. After every edit, undo and redo, the page sizes match a model
that follows the reported page maps. The search thread's snapshot and a reopened in-place save
also have the right text on every page. Mutation check: without the journal operation the undo
half fails.

Through a real window, release build of `879bfbb` plus the bench fixes before `a0506eb`, 2026-10-22,
the §1 machine, viewport 884x612 logical. The documents are written by
`write_pages_bench_documents` (same file): 612 pt wide, 800 + their number tall, so the page order
is read from the page sizes.

```powershell
cargo test --release -p engine-mupdf --test pages_test write_pages -- --ignored
target\release\app.exe target\tmp\pb200.pdf --bench-pages --bench-step-ms 400 --bench-settle-ms 1500
target\release\app.exe target\tmp\pb1000.pdf --bench-pages --bench-step-ms 400 --bench-settle-ms 1500
```

The run rotates page 1, deletes three pages chosen with a click and a Shift+click, duplicates, moves
by the strip, inserts a blank page and drags thumbnail 1 into the lower half of thumbnail 2 with
mouse events. It undoes each with Ctrl+Z. 14 of 14 checks pass on both documents.

| Document | `t_restructure` min / med / max | Layout rebuild med | `ws` before / after 100 edits |
| :--- | ---: | ---: | ---: |
| 200 pages | 1 / 5 / 11 ms | 152 us | 138.3 / 138.4 MiB |
| 1000 pages | 1 / 15 / 28 ms | 479 us | 138.9 / 141.9 MiB |

`t_restructure` is timed for the six operations sent from the strip and the drag; undos by Ctrl+Z and
the 100-edit burst are not timed.

The first drag attempt never dropped: a text payload made Slint hand the drag to the platform,
whose drops the winit backend does not deliver back. The payload is application data now. Before
that, the click area lay over the drag area as a sibling and took the press.

**Unembedded standard fonts** (Plan 0005 §3.8), probes in a release test build, the same day:

| Display list of | System font search (before) | Built-in standard fonts (after) |
| :--- | ---: | ---: |
| A fresh document's page 1 (Helvetica, not embedded) | 1696 ms | 1.0 ms |
| A duplicated page (its own copy of the font) | 1189 ms | 0.69 ms |

`text_bench.rs` (§9.1) calls MuPDF without the engine and so without the loader. Re-run the same
day, its first page still takes 1470 ms (200 pages) and 845 ms (1000 pages): this is the one-off
of §7.2 and §9.1.

### 11.5 Phase 2: merge, split, extract (rows AC, AG)

Row AC's engine half is `crates/engine-mupdf/tests/tools_test.rs` (8 tests, plan §5 Phase 2
results).

Through a real window, release build of `2a3945c`, 2026-10-22, the §1 machine:

```powershell
target\release\app.exe target\tmp\pt200.pdf --bench-tools --bench-step-ms 400 --bench-settle-ms 1500
target\release\app.exe target\tmp\pt1000.pdf --bench-tools --bench-step-ms 400 --bench-settle-ms 1500
```

The run merges the open document with itself on the tool thread and opens the result. It then
extracts two parts (last page then first; page 6 alone) and opens each. Every output's page order
is checked from the page sizes (`pages_bench_*.pdf`, §11.4). 3 of 3 checks on both.

| Document | Merge with itself: `t_tool` | `ws` before / at the end | Extract 2 parts: `t_tool` |
| :--- | ---: | ---: | ---: |
| 200 pages | 33 ms | 122.1 / 123.8 MiB | 11 ms |
| 1000 pages | 627 ms | 123.1 / 125.2 MiB | 30-37 ms |

`t_tool` (§11.2) runs from the job's start on the tool thread to its outputs being complete. The
merged 2000-page file is 421 KiB.

### 11.6 Phase 3: watermark, page numbers, crop (row AD)

Row AD's engine half is `crates/engine-mupdf/tests/stamps_test.rs` (4 tests, plan §5 Phase 3
results). Release build, 1000 Letter pages, 2026-10-23, the §1 machine:

| Stamp | Time | Per page |
| :--- | ---: | ---: |
| Watermark (diagonal, opacity 30%) | 200.0 ms | 0.20 ms |
| Page numbers | 144.8 ms | 0.14 ms |

The first version, through mupdf-rs' `insert_contents` (a nested journal operation per call, merged
into the outer one by MuPDF's quadratic `resolve_undo`), took 17 953 ms and 10 012 ms for the same
two stamps on 1000 pages, and 128.6 / 66.9 ms on 200. Measured in isolation, one `insert_contents`
per page inside one operation took 28 / 269 / 2166 ms on 250 / 500 / 1000 pages; a plain
`add_stream` per page 7 / 27 / 107 ms.

Through a real window, release build of `396778f`:

```powershell
target\release\app.exe target\tmp\st_pt200.pdf --bench-stamps --bench-step-ms 400 --bench-settle-ms 1500
target\release\app.exe target\tmp\st_pt1000.pdf --bench-stamps --bench-step-ms 400 --bench-settle-ms 1500
```

(copies of `pages_bench_200.pdf` and `pages_bench_1000.pdf`, §11.4). The run applies a watermark and
page numbers through the stamp panel, draws a crop on page 1 with the pointer and applies it to
that page, then undoes all three. 4 of 4 checks on both.

| Document | `t_restructure` watermark / numbers / crop |
| :--- | ---: |
| 200 pages | 28 / 21 / 14 ms |
| 1000 pages | 205 / 155 / 25 ms |

### 11.7 Phase 4: compress, export, images to PDF (rows AE, AF)

Rows AE and AF's engine half is `crates/engine-mupdf/tests/convert_test.rs` (5 tests, plan §5
Phase 4 results). Release build, 2026-10-23, the §1 machine:

| Run | Result |
| :--- | ---: |
| Lossless, 40 text pages (uncompressed input) | 161 170 to 21 943 bytes |
| Balanced, 4-page 300 dpi JPEG scan | 2826 to 763 KiB, 1087 ms |
| Strong, same scan | 2826 to 325 KiB, 817 ms |
| Balanced with MuPDF's averaging downsampler (first version) | 2826 to 2102 KiB |
| PNG at 150 dpi | 69.4 ms per page |
| Images to PDF, 3 images | 741 KiB in, 741 KiB out |

Through a real window, release build of `8ec028d`:

```powershell
target\release\app.exe target\tmp\cv_pt200.pdf --bench-convert --bench-step-ms 400 --bench-settle-ms 1500
target\release\app.exe target\tmp\cv_pt1000.pdf --bench-convert --bench-step-ms 400 --bench-settle-ms 1500
```

(copies of `pages_bench_200.pdf` and `pages_bench_1000.pdf`, §11.4, already compressed: Lossless
keeps them as they are.)

| Document | Compress `t_tool` | Export 2 pages | Images to PDF | `ws` at the end |
| :--- | ---: | ---: | ---: | ---: |
| 200 pages | 21 ms | 83 ms | 105 ms | 120.7 MiB |
| 1000 pages | 643 ms | 103 ms | 117 ms | 108.5 MiB |

2 of 2 checks on both: the compressed copy is not larger; pages 6 and 1 exported, made into a PDF
and opened come back in that order.

### 11.8 Phase 5: export as Word (row AH)

`crates/engine-mupdf/tests/word_test.rs`, release build, 2026-10-23, the §1 machine: 100 text pages
(a heading and ten lines each) exported in 29 ms, 0.3 ms per page, 12 KiB of DOCX. Phase 0 measured
3.7 ms per page and a 1.4 s first document (§11.3) through a test binary; the engine's run has the
writer's start-up behind it already (the standard fonts come from MuPDF's own copies, plan §3.8).

Reading a DOCX back with MuPDF (only the test does) is slow: about 170 s for the 100-page file in
a release build, through its HTML layout. The test reads back a 4-page export instead.

Through a real window, release build of `8826918`, the `--bench-convert` run of §11.7 on 200 pages:
the PDF made of two images exported as Word in `t_tool` 17 ms; the working set went from 130.7 to
149.2 MiB.

Release `app.exe`: 18 697 216 bytes at `8ec028d`, 20 677 632 bytes with the DOCX writer
(+1.89 MiB).

## 12. Plan 0006: v0.4 forms

### 12.1 Phase 0: an AcroForm through mupdf-rs 0.8

`crates/engine-mupdf/tests/forms_bench.rs` (ignored), release build, 2026-10-23, the §1 machine.
The form is generated: a text field, a multi-line one, a check box, a radio group of two, a combo
box with Turkish options, a multi-select list, two numbers and a total with a calculation script,
and an unsigned signature field (11 widgets). The engine's font loader is installed, as in the app.

```powershell
cargo test --release -p engine-mupdf --test forms_bench -- --ignored --nocapture
```

| Step | Time |
| :--- | ---: |
| Read the 11 widgets (type, name, value, flags, rectangle) | 2.1 ms |
| Fill with ASCII text, check box, radio, list, numbers (one journal operation each) | 0.3-0.8 ms |
| Fill "Öz" (letters in WinAnsi) | 0.4 ms |
| Fill "Öğüt Şükrü İnce" | 3540 ms (2756 ms in a second run) |
| Fill two Turkish lines | 2484 ms |
| Fill "Muğla" in the combo box | 2676 ms |
| Flatten the filled form (`bake`) | 917 ms |
| The same three Turkish fills with `serif` / `sans-serif` / `monospace` mapped to built-in fonts (prototype) | 2.12 / 1.37 / 1.32 ms |
| Flatten with that mapping | 1.32 ms |
| Incremental save | 2.0 ms |
| `enable_js` | 1.5-2.7 ms |

Without the engine's loader (`FA_PDF_FORM_NO_LOADER=1`) the Turkish fills took 5643, 2982 and
2636 ms. The cause is MuPDF asking for a font named `serif` three times per fill (plan 0006
§3.2). The calculation script did not run (plan §3.3).

### 12.2 Phase 1: filling fields (rows AI, AJ, AK)

`t_fill` is `t_edit` (§10.2) of an `EditOp::SetField`: from sending the fill to every tile of the
changed page showing its new revision.

Engine: `crates/engine-mupdf/tests/form_test.rs` (7 tests, plan §5 Phase 1 results). A Turkish
single-line fill takes about 2 ms (the font fix of plan §3.2; 3046 ms without it, in the debug
test build).

Through a real window, release build of `92478dc`, 2026-10-23, the §1 machine:

```powershell
target\release\app.exe target\tmp\bench_form.pdf --bench-forms --bench-step-ms 400 --bench-settle-ms 1500
```

(`bench_form.pdf` is a copy of `form_read.pdf`, which `form_test.rs` writes from the generated form
of `tests/form_fixture`.) Clicking the check box, the second radio button, typing
"Öğüt Şükrü İnce" into the editor over the name field and choosing the third city, then undoing all
four: 8 of 8 checks. `t_fill` min 3, median 7, max 13 ms (n = 4).

After the maintainer's review (same day): the bench clicks and types through the window's input
path (`dispatch_event`, the button held 150 ms on the text field) and checks the field shading and
a comb field: 11 of 11, in three runs.

### 12.3 Phase 2: the keyboard, lists, reset (row AL)

Engine: `crates/engine-mupdf/tests/form_test.rs`, 11 tests (plan §5, Phase 2 results).

Through a real window, release build, 2026-10-24, the §1 machine:

```powershell
target\release\app.exe target\tmp\bench_form2.pdf --bench-forms --bench-step-ms 400 --bench-settle-ms 1500
```

(`bench_form2.pdf` is a copy of `form_read.pdf`, now tabbed by rows and with a default value for
"city".) Every key goes through `dispatch_event`. The run passed 27 of 27 checks, in three runs:

- Phase 1's 11;
- Tab, Space and Enter on the check box, a radio button and the combo box;
- two ticks in the multi-select list;
- Reset Form and its undo;
- Tab and Shift+Tab across pages;
- Tab from one text field to the next, then typing.

### 12.4 Phase 3: flatten, export and import (rows AM, AN)

Engine: `crates/engine-mupdf/tests/form_test.rs`, 14 tests; core: `crates/core/src/form_data.rs`,
5 tests (plan §5, Phase 3 results).

Through a real window, release build, 2026-10-24, the §1 machine, with the §12.3 command and file.
The bench writes `bench_form_data.json` beside the document. It passed 35 of 35 checks, in three
runs:

- Phases 1 and 2's 27;
- Export Data (JSON) and the file holding the typed value;
- Reset Form, then Import Data: the text, the check box and the multi-select list come back;
- Flatten Form: no field left and none shaded; one undo brings the form back.

## 13. Plan 0007: GUI redesign

### 13.1 Metrics: `t_first_page` and idle frames (rows AR, AS)

- **`t_first_page`:** milliseconds from the start of `main` (before the window is built) to the
  first full coverage of the opening view: every visible tile of the first level is in the
  scheduler, the trace's first `full` line. Measured by `--bench-idle`, which prints it.
- **`frames`:** frames the window drew, counted by a rendering notifier
  (`RenderingState::AfterRendering`) installed when tracing.
- **Idle frames:** `--bench-idle S` waits for `t_first_page`, lets the window settle for 1 s, then
  counts `frames` over S seconds with no input. Passes with 0, exit code 0.

```
target\release\app.exe crates\engine-mupdf\tests\fixtures\large_200p.pdf --bench-idle 5
target\release\app.exe crates\engine-mupdf\tests\fixtures\dense_vector.pdf --bench-idle 5
```

Each run five times in a row; the figure is the median. A run draws 6 frames before the count
(the window appearing and the first tiles), which the count leaves out.

### 13.2 Phase 1: the markup in `.slint` files, the `Theme` global (rows AP-AS)

Release build, the §1 machine, 2026-10-25. Before: `5e7c805` with the bench change only; after:
the Phase 1 commit.

| | Before | After |
| :--- | :--- | :--- |
| `app.exe` | 18,927,104 bytes | 19,054,080 bytes |
| `t_first_page` `large_200p` (5 runs) | 732, 962, 638, 635, 623 ms; median 638 | 646, 831, 633, 638, 638 ms; median 638 |
| `t_first_page` `dense_vector` (5 runs) | 796, 811, 809, 799, 804 ms; median 804 | 837, 815, 812, 822, 903 ms; median 822 |
| idle `frames` over 5 s | 0 in every run | 0 in every run |
| `peak_ws`, §7.6 run A | 162.7 MiB | 162.4 MiB |

The 124 KiB of growth is the theme's tokens and `slint-build`'s generated code for the same
window. The A0 run's `peak_ws` is lower than §7.6's 192.7 MiB of 2026-10-20 on both sides of the
change; the window is the same one, so the difference is not this phase's.

### 13.3 Phase 2: own frame and islands (rows AP-AS)

Release build, the §1 machine, 2026-10-25, the commands of §13.1 and §7.6 run A.

| | Phase 1 | Phase 2 |
| :--- | :--- | :--- |
| `app.exe` | 19,054,080 bytes | 19,568,128 bytes |
| `t_first_page` `large_200p` (5 runs) | median 638 | 659, 678, 679, 682, 675 ms; median 678 |
| `t_first_page` `dense_vector` (5 runs) | median 822 | 778, 848, 881, 801, 864 ms; median 848 |
| idle `frames` over 5 s | 0 in every run | 0 in every run |
| `peak_ws`, §7.6 run A | 162.4 MiB | 162.6, 159.6, 158.6, 165.0 MiB |

With the preferred window size at 1000 x 760 instead of 900 x 700, `t_first_page` was 732, 738,
729, 752, 733 ms (`large_200p`) and 873, 902, 856, 843, 860 ms (`dense_vector`): a larger first
view, more tiles. The window keeps 900 x 700.

### 13.4 Phase 3: mode changes (row AT)

Metrics, measured by the rendering notifier `count_frames` installs when tracing:

- **`frame_ms`:** from Slint's `RenderingState::BeforeRendering` to its `AfterRendering`: the
  drawing and the hand-over to the GPU, without the wait for the display
  (`i-slint-renderer-femtovg-1.18.1/lib.rs:260, 333`). `frame_max` is the longest in a window,
  `frame_first` the first one.
- **`gap_max`:** the longest time between the ends of two frames in a window, the first frame's
  end measured from the window's start.
- **A change's window:** from setting `mode` to the end of the motion plus its settle margin
  (`MODE_MOTION_MS` 250 + `MODE_SETTLE_MS` 30 = 280 ms). `tiles_requested` and `tiles_rendered`
  are the window counters' growth over it.
- **`--bench-modes N`:** waits for `t_first_page`, settles for 1 s, then changes the mode `N`
  rounds of Read > Edit > Tools > Read > Tools > Edit > Read, 800 ms apart. Passes when no change
  requested or rendered a tile and no `frame_ms` was over 16.7 ms; exit code 0.

The harness's other windows (`--bench-zoom`, `--bench-pan`) now print `frames`, `frame_max`,
`frame_first` and `gap_max` too.

```
target\release\app.exe crates\engine-mupdf\tests\fixtures\large_200p.pdf --bench-modes 2
target\release\app.exe crates\engine-mupdf\tests\fixtures\dense_vector.pdf --bench-modes 2
```

Release build, the §1 machine, 2026-10-25. First round (canvas beside the islands, a busy
machine), 5 runs each: 0 tiles in all 120 changes; the first change into Tools took 12.0-21.2 ms
in its first frame; 3 of 10 runs passed. At the gate (the canvas under the whole window), 16
runs: 0 tiles in all 192 changes; the first change into Tools 12.2-15.6 ms (the detailed batch),
every other change's median `frame_max` 7.8-9.9 ms per run; 13 of 16 runs passed, the others'
worst frames 18.5, 19.3 and 19.8 ms. Before the warm-up frame (plan §5, Phase 3 results), the
first change into Edit took 19.5-20 ms and the first into Tools 34-39 ms. The other rows are in
the plan's §5.

### 13.5 Phase 4: tabs (row AU)

Metrics, printed by `--bench-tabs N` (run alone, exit code 0 on a pass):

- **Steps:** once the opening view is covered (no holes among the visible tiles) the harness
  waits 1.5 s and reads the working set; then it opens the same document in a new tab and does
  the same, until there are `N` tabs; then it shows tabs 1 to `N - 1` again, one by one, the
  same way.
- **`ws_1`, `ws_N`:** the working set with one tab, and with `N` tabs, after their view settled.
  **`ws_after_round`:** after the last tab shown again.
- **`doc_mem`:** `ws_2 - ws_1`, what a second tab of the document added: one document's own
  memory (its MuPDF document, the search thread's copy, page sizes, its three threads).
- **`ceiling`:** `ws_1 + N x doc_mem`. Row AU passes when `ws_N` and `ws_after_round` are at most
  the ceiling, and, for `N <= 4`, `peak_ws` is at most 250 MiB.
- **`t_open`:** for each tab, the time from asking for it (the open, or the first view for tab
  1) to its view covered. **`t_switch`:** the same for a tab shown again: its first view drawn
  from nothing, as a parked tab holds no pixels (ADR 0011).

```
target\release\app.exe crates\engine-mupdf\tests\fixtures\large_200p.pdf --bench-tabs 4
target\release\app.exe crates\engine-mupdf\tests\fixtures\large_200p.pdf --bench-tabs 10
```

Release build, the §1 machine, 2026-10-25, 3 runs each: with 4 tabs `peak_ws` 152.7-155.5 MiB and
`ws_4` 146.2-148.9 MiB under ceilings of 156.1-168.2 MiB; with 10 tabs `ws_10` 157.8-158.2 MiB
under ceilings of 175.4-216.1 MiB, 2.0-2.1 MiB per extra tab on average. `t_open` 64-82 ms for
tabs 2 to N, `t_switch` 19-21 ms. Row AU passes in 6 of 6 runs; the table and the other rows are
in the plan's §5, Phase 4 results.

### 13.6 Phase 5: contrast and reduce motion (rows AW, AX)

Metrics:

- **Contrast (row AW):** WCAG 2.x contrast ratio, `(L1 + 0.05) / (L2 + 0.05)` from the sRGB
  relative luminance of the two colours, computed by `crates/app/tests/theme_test.rs` from the
  token values in `crates/app/ui/theme.slint` (both branches of each `dark ? ... : ...`). Pairs:
  `text`, `text-muted`, `text-faint` and `danger` on `shell`, `canvas`, `surface` and `hover`, and
  `text` on `active`. Passes when every pair is at least 4.5:1.
- **Reduce motion (row AX):** the same test checks that every `animate` duration and delay in
  `crates/app/ui` names a `Theme` token and that every duration token reads
  `reduce-motion ? 0ms : ...`. On a real window, `--bench-modes` counts the frames a mode change
  draws (§13.4): with the setting on (`reduce_motion=true` in the settings file) a change draws
  only the frames of its new layout.

```
cargo test -p app --test theme_test
target\release\app.exe crates\engine-mupdf\tests\fixtures\large_200p.pdf --bench-modes 1
```

Release build, the §1 machine, 2026-10-29: the test passes (lowest pair 4.52:1, dark
`text-faint` on `hover`; before the token change `text-faint` was 2.22-3.81:1). `--bench-modes 1`,
6 changes, 0 tiles: 21 frames with reduce motion on, worst 12.9 ms; 289 frames with it off, worst
16.1 ms. The other rows are in the plan's §5, Phase 5 results.

## 14. Plan 0008: the phone

The maintainer's phone: Samsung Galaxy A34 (SM-A346E), Android 16, 1080 x 2340 at 450 dpi,
connected by USB to the §1 machine and driven with the SDK's `adb`. Release builds, arm64-v8a.

### 14.1 Metrics on the phone

- **APK size:** bytes of the signed release APK; `libapp.so` uncompressed as listed inside it.
- **Cold launch:** `TotalTime` of `adb shell am start -W -n <package>/<activity>` after
  `am force-stop`: Android's own figure, from the launch intent to the activity's first frame.
  The activity was `android.app.NativeActivity` in the trial and is
  `org.fineassociation.podefa.PodefaActivity` from Phase 1.
- **`t_first_page`:** as in §13 (`--bench-idle`), from the start of the app's `run()` to the first
  view covered; on Android `run()` starts after the activity, so cold launch plus `t_first_page`
  is the time from the tap to the page.
- **Memory:** `TOTAL PSS` of `adb shell dumpsys meminfo <package>`, sampled once a second during a
  run; the peak is the largest sample. PSS shares each shared library among the processes using
  it, so it is the phone's figure closest to the desktop's working set; RSS is given beside it.
- **Bench verdicts and `frame_max`:** the desktop harness unchanged (§7.5, §13). In the trial the
  arguments went into `args.txt` in the app's data folder and the output into `out.txt` there,
  through `run-as`. From Phase 1 the release APK is not debuggable, so the arguments come from the
  launch intent (`am start ... --es args "'{files}/large_200p.pdf --bench-idle 5'"`, `{files}`
  standing for the app's folder in shared storage, `Android/data/<package>/files`) and the output
  goes to `out.txt` in that folder, both reached with plain `adb push` and `adb shell cat`.

### 14.2 Phase 0 trial build (2026-10-26)

The desktop window unchanged on the phone (Plan 0008 §3.3; branch `trial/android-0`, not merged).
One run of each unless counted:

| | Result |
| :--- | :--- |
| APK | 11,825,507 bytes; `libapp.so` 22,378,688 bytes |
| Cold launch | 740 ms first launch without a document; 436, 467 ms with `large_200p.pdf` |
| `t_first_page`, `large_200p.pdf` | 239 ms |
| `--bench-idle 5` | 0 frames in 5015 ms, PASS |
| PSS, `large_200p.pdf` open | 154.0 MiB (RSS 245 MiB; native heap 66 MiB, graphics 32 MiB) |
| PSS after one swipe (`input swipe`) | 174.6 MiB; the canvas scrolled to page 2 |
| `dense_vector.pdf --bench-zoom 6 --bench-step-ms 16 --bench-settle-ms 3000 --bench-repeat 3` | `t_blank` 48/51/46 ms, `t_full` 131/98/98 ms, `t_abort` 31/47/42 ms; VERDICT FAIL (51 ms against the 50 ms ceiling); zoom windows' `frame_max` 135.6 (first), 54.9, 39.4 ms; peak PSS 200.9 MiB |
| `large_format_a0.pdf --bench-zoom 19 --bench-step-ms 50 --bench-settle-ms 2000 --bench-pan 512 --bench-pan-steps 320` | VERDICT PASS, worst `t_blank` 49 ms; zoom window `frame_max` 38.5 ms, first frame 130.4 ms; peak PSS 218 MiB |

Pinch was not measured: the canvas has no pinch handler yet and `adb` cannot inject two fingers.

### 14.3 Phase 1: the platform build (2026-10-27)

The APK from `scripts/android-build.sh` (`org.fineassociation.podefa`, the mobile shell, ADR 0012),
same phone, documents pushed to `{files}`. PSS sampled once a second for 25 s per run.

| Row | Run | Result |
| :--- | :--- | :--- |
| MA | APK | 12,038,559 bytes (11.5 MiB); `libpodefa.so` 22,804,456 bytes, `classes.dex` 15,068 bytes. PASS (16 MiB) |
| MC | `large_200p.pdf --bench-idle 5`, 3 cold launches | `TotalTime` 234, 236, 205 ms; `t_first_page` 93, 206, 176 ms; sum 327, 442, 381 ms. PASS (1.0 s). Idle: 0 frames in 5 s each time. The first launch after installing, without a document: 1116 ms |
| MB | `dense_vector.pdf --bench-zoom 6 --bench-step-ms 16 --bench-settle-ms 3000 --bench-repeat 3` | peak PSS 175.0 MiB; `t_blank` 40/49/43 ms, `t_full` 85/101/114 ms, `t_abort` 33/33/35 ms, VERDICT PASS (49 ms); zoom windows' `frame_max` 44.9, 51.8 ms |
| MB | `large_format_a0.pdf --bench-zoom 6 --bench-pan 400 --bench-pan-steps 40 --bench-step-ms 16 --bench-settle-ms 3000` | peak PSS 173.4 MiB; VERDICT PASS (`t_blank` 24 ms); zoom window `frame_max` 33.8 ms |
| MB | `large_200p.pdf --bench-zoom 4 --bench-pan 400 --bench-pan-steps 40 --bench-step-ms 16 --bench-settle-ms 3000` | peak PSS 161.4 MiB; VERDICT PASS (`t_blank` 19 ms) |

Row MB's peak PSS (at most 175.0 MiB) passes its 250 MiB ceiling for these runs; its pinch and
fling by hand come with Phase 2. Row MD is a manual check, recorded in the plan's §5.

### 14.4 Phase 2: pinch (row ME)

**`--bench-pinch N`** (defined 2026-10-27, before it was measured): once the opening view is
covered and has settled for a second, the harness makes `N` pinches through the window's `pinch`
callback, the one Slint's `ScaleRotateGestureHandler` calls for two fingers or a trackpad. Each
pinch is 30 updates 16 ms apart, centred on the view, its centre drifting by (30, 20) logical px;
the odd ones scale to x2, the even ones to x0.5. Over each pinch, from `started` to the last
update before `ended`:

- **`tiles_requested`, `tiles_rendered`:** the change of the status counters; row ME wants 0.
- **`frame_max`, `gap_max`:** as in §13.4, over the same window; row ME wants `frame_max` at most
  16.7 ms.
- **`t_cover`:** from `ended` (the one zoom) until every visible cell holds a tile of the new
  scale (`bench::holes` = 0), sampled every 16 ms.

PASS when no pinch requested or rendered a tile while moving and no frame took longer than
16.7 ms. A scripted pinch drives the same Rust and Slint path as fingers, but not Slint's touch
recognizer; on the phone a real two-finger pinch is written to the touch screen's input device
(`/dev/input/event3`, which the `adb` shell may write) and timed with `FA_PDF_TRACE`'s frame lines.

Desktop (§1 machine, release build, `FA_PDF_TRACE=1`, 2026-10-27):

| Document | Result |
| :--- | :--- |
| `large_200p.pdf --bench-pinch 4` | tiles 0 in every pinch; `frame_max` 0.5-0.6 ms; `t_cover` 15-16 ms. PASS |
| `dense_vector.pdf --bench-pinch 4` | tiles 0 in every pinch; `frame_max` 0.5-0.8 ms; `t_cover` 31-54 ms. PASS |

Phone (§14, the APK of `af14fc0` and later, 2026-10-27):

| Run | Result |
| :--- | :--- |
| `large_200p.pdf --bench-pinch 4` | tiles 0; `frame_max` 2.0-2.5 ms, once 48.7 ms (the first frame of the second pinch, a zoom-out from 200%); `t_cover` 30-59 ms. FAIL on that one frame |
| `dense_vector.pdf --bench-pinch 4` | tiles 0; `frame_max` 2.0-4.1 ms; `t_cover` 49-96 ms. PASS |
| `large_200p.pdf --bench-pinch 8`, twice | tiles 0; `frame_max` at most 3.4 and 3.8 ms; `t_cover` at most 56 and 63 ms. PASS both |

So the 48.7 ms frame came once in 24 pinches on that document and not again; the frames while
the fingers move are otherwise 2-4 ms, against 30-55 ms for the zoom frames of the trial build
(§14.2), because only a transform changes. A real two-finger pinch could not be injected: SELinux
keeps the `adb` shell from writing to `/dev/input/event3` although the device's group allows it,
so row ME's pinch by hand is the maintainer's (with `FA_PDF_TRACE`'s `pinch ended` line, which
carries the frames of the pinch).

### 14.5 Phase 2: fling and idle (rows MF, MG)

The 1000-page document is a synthetic Letter document (a title, 20 text lines and 6 filled boxes
per page, 2.4 MB) written for this run; PSS sampled once a second with `dumpsys meminfo`.

| Row | Run | Result |
| :--- | :--- | :--- |
| MF | `--bench-scroll 10000 --bench-scroll-s 30 --bench-step-ms 16 --bench-settle-ms 3000` (Plan 0002 row B1 on the phone; scroll writes, not the Flickable's fling) | 376 pages visited, 0 hole frames while scrolling, `resident_max` 5 pages, 1 re-anchor; PSS 190-196 MiB from the 3rd second to the end (first 5 s against last 5 s of the scroll: flat). PASS |
| MF | 90 real flings (`adb shell input swipe 540 1900 540 300 60`, back to back for 30 s, through the Flickable's own fling) | reached page 196; after 4 s still, the screen showed the page drawn with no hole (screenshot); PSS rose from 180 to 208 MiB in the first 10 s, then stayed between 208 and 217 MiB; peak 217.1 MiB. PASS (no holes after settle, memory flat after the first 10 s, under row MB's 250 MiB) |
| MG | `large_200p.pdf --bench-idle 5` | 0 frames in 5014 ms; `t_first_page` 106 ms. PASS |

### 14.8 Phase 5: landscape (row MK, 2026-10-28)

**Metric.** `--bench-rotate N` (Plan 0008 §5, "Phase 5: results"): after the opening view is
covered and settled, page 5 at 150%; then N turns to landscape and back through the activity's
`setRequestedOrientation`. For each turn, `t_cover` is the time from the request until the window
has its new shape and every visible tile is drawn; the current page (the page under the view's
centre) and the zoom must be the ones before the turns, and the header and the dock inside the
safe area. PASS when every turn keeps them.

**Phone** (`large_200p.pdf`, release build): 6 of 6 turns pass in one run of `--bench-rotate 3`,
4 of 4 in an earlier `--bench-rotate 2`; `t_cover` 189-209 ms to landscape (2340 x 1080 px, safe
insets left 27, top 30, bottom 15), 114-127 ms back to portrait (safe insets top 27, bottom 15).
**Desktop** (`--mobile --bench-rotate 2`): 4 of 4, `t_cover` 11-15 ms (832 x 384 and back).

**APK** (row MA): 12,210,819 bytes (11.6 MiB) with the icon and the scrolling sheets.

### 14.7 Phase 4: tools on touch (2026-10-28)

**Page operations.** `--bench-pages` on `pages_bench_200.pdf` (§11.4), the phone's mobile shell:
13 of 14 steps pass, every page operation and its undo; the miss is the step that drags a
thumbnail in the desktop's sidebar with the mouse, which the phone shell does not show (the same
on the desktop with `--mobile`). `t_restructure` min 9, median 49, max 86 ms (n=5); the desktop's
run: 14 of 14, median 13 ms. The column's own move (a long press and drag) was checked by hand
(plan §5, "Phase 4: results").

**Mode changes.** `--bench-modes` on `pages_bench_200.pdf` (§13.4), the screen kept awake with
`KEYCODE_WAKEUP` (two first runs with the screen off counted no frames and are left out). The
slowest frame of each change, three runs each, before this phase (HEAD `cd233ad`, Tools showing
the desktop's gallery) and with the column:

| Change | Before (3 runs) | With the column (3 runs) |
| :--- | ---: | ---: |
| 0>1 (first into Edit) | 13.9-18.6 ms | 13.6-26.8 ms |
| 1>2 (first into Tools) | 26.0-28.1 ms | 21.2-34.6 ms |
| 2>0 | 5.2-16.1 ms | 5.1-13.1 ms |
| 0>2 (Tools again) | 4.2-4.6 ms | 3.7-5.1 ms |
| 2>1 | 11.7-11.9 ms | 11.4-15.5 ms |
| 1>0 | 4.2-5.5 ms | 3.9-4.1 ms |

Tiles rendered in every change: 0. The first change into Tools misses the 16.7 ms budget before
and after; its slowest frame is not the first (`frame_first` 4.8-12.8 ms) but one during the
motion. Backlog item 27. On the desktop (`--mobile`, 3 runs) the column's worst frame was
16.9-18.6 ms; the desktop shell's own gallery 14.5-17.2 ms in 5 runs (2 over the budget, as in §13.4
"at the budget").

### 14.6 Phase 3: edit on touch (rows MI, MJ)

**Stroke frames (row MJ).** With a trace on (`FA_PDF_TRACE=1`, or any `--bench-*` run such as
`--bench-idle 900` on the phone), each ink stroke prints `ink started` at the finger's press and
`ink ended points=N rebuild_max_us=R frames=F frame_max=M frame_first=A gap_max=G` at its release:
the frame window of §13 between the two, so `frame_max` is the slowest frame while the stroke was
drawn and `gap_max` the longest time between two frames; `rebuild_max_us` is the slowest rebuild of
the live stroke's path (`ink_rebuild_ms`, §10.2). A stroke passes when `frame_max` and `gap_max`
stay inside 16.7 ms (60 Hz) and the stroke is one undo step.

Phone (§14, the APK of this phase, 2026-10-27), the generated form of `form_test.rs`, the Ink
tool chosen from the edit strip, strokes drawn with `adb shell input swipe x1 y1 x2 y2 D` (one
finger, about 120 move events a second):

| Stroke | Points | `frame_max` | `gap_max` | `rebuild_max_us` |
| :--- | ---: | ---: | ---: | ---: |
| 1 (2 s, the first stroke after start) | 238 | 30.4 ms | 36.5 ms | 2202 |
| 2 (4 s) | 476 | 6.0 ms | 16.9 ms | 668 |
| 3-7 (2 s each) | 239-240 | 4.0-5.0 ms | 14.1-17.9 ms | 95-134 |

So a frame of a stroke takes 4-6 ms, one frame per touch event; the first stroke after the app
starts pays once for 30.4 ms. `gap_max` follows the touch events (about 8 ms apart), with a few
gaps of 17-18 ms. One Undo from the More menu removed exactly the last of five strokes
(screenshots). Row MJ: PASS, with the first stroke's one slow frame noted.

**Keyboard (row MI).** On the phone, with the Samsung keyboard (its top at 1454 of 2340 px):

| Case | Result |
| :--- | :--- |
| The text field at the bottom of the view (under the keyboard's place) tapped | the document scrolled; the editor ends just above the keyboard; typed text shows in it. PASS |
| A multi-line field scrolled under the header island, tapped | the document scrolled down; the editor starts under the header. PASS (after a fix: the first build only kept the bottom clear) |
| The Note tool on a point under the keyboard's place | the note editor opened above the keyboard. PASS |
| A combo box tapped | its options open over the page. Once the keyboard came up with them and took typed text that showed nowhere; once it did not. Left to the maintainer's typing by hand |

While the keyboard slides in, each frame changes the document's bottom inset and lays it out
again: 4-26 us each (`reanchor` lines), about 20 times.
