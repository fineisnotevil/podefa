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


