<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# Backlog

Work that is known, measured and deliberately **not** done. Each entry is written so it can be
filed as a GitHub issue as-is: title, labels, and the body in the same shape as
`.github/ISSUE_TEMPLATE/`. Nothing here blocks Plan 0001, which is closed
([`plans/0001-tiled-viewport-rendering.md`](./plans/0001-tiled-viewport-rendering.md)); the next
plan is [`plans/0002-continuous-multipage-scroll.md`](./plans/0002-continuous-multipage-scroll.md).

The three entries below are one investigation split by question, not three independent tasks. They
all come out of the same measurement: [`benchmarks.md`](./benchmarks.md) §7.6 samples the process
every 250 ms and finds that the tile cache explains only a small part of the plateau it reaches -
12 MiB of cache against a 160 MiB working set, with the process ~75 MiB above its own empty
baseline, and `private` running 50-100 MiB above `ws`.

---

## 1. Where the ~70 MiB fixed overhead between an empty process and a loaded one comes from

**Labels:** `performance`, `memory`, `investigation`

### Problem Statement

Loaded with one document and nothing else open, the app's working set sits about 75 MiB above its
own empty baseline: `ws` 84.9 MiB at startup against 160.4 MiB median during a §7.6 run, while the
tile cache accounts for 12 MiB of that. The same gap shows on a *minimal* fixture and does not grow
with the page or the zoom level, so it is fixed cost, not cache behaviour. Nobody has looked inside
it.

### Proposed Solution

Measure before changing anything: extend the existing 250 ms sampler (§7.6) to print a `phase` label
so samples can be attributed, then take one run per step and report the delta each adds:

1. process start, before `Document::open` and before the engine actor spawns;
2. after `open` (page count + page sizes, no raster);
3. after the display-list build for page 0 (the known one-off);
4. after the base layer;
5. after a settled viewport of tiles;
6. after a settled pan (cache full), then after `drop`ping the document.

Candidates to check against those deltas, in the order the evidence is likely to favour:

- **MuPDF's `fz_store`** - MuPDF keeps decoded images, fonts and shading caches in a store keyed by
  object, with a default budget. The display list of an A0 page holds the page's decoded resources
  for as long as the list lives, and the list lives for as long as the page does.
- **The display list itself** - one per page, cached with the document; an A0 page's list is a
  parsed command stream, not a raster, so its size should be reported rather than assumed.
- **The base layer** - one stretched page raster, ~2.3 MiB of RGB8 for A0 plus its Slint copy.
- **Slint's texture cache** - keyed per image, drained when the model drops an `Image`; the
  question is whether a released A0 base layer or a superseded tile level drains promptly.

Acceptance: the measured deltas add up to the observed plateau within ~10 MiB, and whatever is
largest has a named owner (this crate, MuPDF's store budget, or Slint) in this file. A fix is a
follow-up issue; this one is done when the number has an explanation.

### Alternatives Considered

- *Guess and shrink* (`fz_store` limits, `fz_drop_display_list` on page change, smaller base
  layer) without the per-step measurement - rejected: three plausible causes, no evidence which
  one is the 70 MiB, and every guess is a behaviour change.
- *Ignore the gap because the §4.9 target is met* - rejected: the target is stated against the
  working set, so an unexplained 70 MiB is a risk to it, not a footnote under it.

### Additional Context

`benchmarks.md` §7.6, runs A-C; the empty-baseline sample is the first `rss` line of any traced run.
`crates/app/src/bench.rs` already has the sampler (`process_memory`, `sample_memory`) and the
summary's `rss_mib` line, so this is a label and a set of runs rather than new plumbing.

---

## 2. Try a different allocator and measure what it does to the plateau

**Labels:** `performance`, `experiment`

### Problem Statement

`private` (committed private bytes) sits 50-100 MiB above `ws` (resident) through a §7.6 run:
204.0 -> 301.8 MiB at the medians in the RGBA8 revision, and 210.8 -> 352.4 MiB with the larger
cache. Some of that is the tile churn: tiles are 768 KiB allocations, allocated, filled, and freed
again as the ring advances, which is exactly the pattern a general-purpose allocator is worst at -
it keeps the pages rather than returning them, so the commit charge stays high even after the
bitmap is gone.

### Proposed Solution

One global allocator behind a cargo feature, measured against the current default on the same
machine and scenario:

```toml
[features]
alloc-mimalloc = ["dep:mimalloc"]
```

- build with and without it, run §7.6's three configurations unchanged;
- report `private`, `ws` and `peak_ws` per configuration, plus the run's `evicted` and tiles
  rendered so a difference in work is visible, not just in memory;
- acceptance for *keeping* the allocator: `ws` plateau or `private` plateau improves by more than
  run-to-run spread, no `t_blank` regression beyond the 50 ms verdict ceiling, and `cargo deny`
  stays clean (licence + `[bans]`).

No new dependency unless the measurement says so: the feature is opt-in and off by default, so a
negative result costs one commit rather than a permanent dependency.

### Alternatives Considered

- *Pool the tile buffers ourselves* - more code, and it duplicates what an allocator does; only
  worth it if the allocator experiment shows the churn is the cause.
- *Reduce the churn instead of managing it* - that is the direction-aware ring idea recorded on
  `AppState::ring` and in Plan 0001 §7, and it should be measured before an allocator is adopted.

### Additional Context

Depends on issue 1's phase labelling to say *which* phase the commit charge comes from; both can be
measured in the same runs. `docs/benchmarks.md` §7.6 already logs `ws` and `private` side by side
for exactly this comparison.


---

## 3. Explain and then narrow the `private`-vs-`working set` gap

**Labels:** `performance`, `memory`, `investigation`

### Problem Statement

Two process-memory counters are reported, and they disagree by more than the tile cache is large:
median `private` 352.4 MiB against median `ws` 241.4 MiB in §7.6's run B. Only one of them can be
the plan's target metric (working set), and the difference is currently attributed by hand-waving -
"committed pages the allocator has not released". If the gap is instead something structural
(committed-but-not-touched pages, a growing thread stack, a driver allocation, a memory-mapped
resource), then it is a real ceiling on a memory-constrained machine even when `ws` looks fine.

### Proposed Solution

Attribute the gap rather than re-describe it:

1. sample `private` and `ws` at the *same* instants (already the case) and add the derived gap to
   the `rss` line so it is a first-class number in the trace;
2. on Windows, take one probe run that calls `EmptyWorkingSet` on the process between phases and
   report what `private` does when `ws` is forced down - the difference that survives is commit
   that no trim can reclaim;
3. compare the same scenario on Linux (`VmRSS` vs `VmSize` in `/proc/self/status`) with the
   counters handled by a small `cfg`-gated module beside the Windows one;
4. write down which of the two the plan targets and why, so §4.9's target is not silently
   ambiguous.

Acceptance: the gap has a stated composition (paging, allocator retention, other), the size of
each part is measured, and `benchmarks.md` §7.6 says which counter answers which question.

### Alternatives Considered

- *Report only `ws`* - rejected: `private` is where a tile cache's growth actually shows, which is
  why both are logged, and dropping it would hide the commit charge that makes a low-memory machine
  swap.
- *Report only `private`* - rejected for the opposite reason: the plan's user-visible target is the
  resident set, and `private` flatters no cache. Hence the gap, not one of the two.

### Additional Context

§7.6's field table already defines both; the RSS sampler's Windows implementation is a hand-written
`GetProcessMemoryInfo` block in `crates/app/src/bench.rs`, so a second platform is a sibling module
rather than a new dependency.


---

## 4. Memory that grows with the document's page count at open (~1.4 KiB a page)

**Labels:** `performance`, `memory`, `investigation`

### Problem Statement

Plan 0002's virtualization keeps the *visited* pages from costing memory, but opening a document still
costs memory proportional to its page count: the 20,000-page synthetic document starts 26.4-27.0 MiB
above the 1,000-page one before any scrolling (`benchmarks.md` §8.3, B1/B3 and B2/B4), about 1.4 KiB a
page. The app's own page table is ~64 bytes a page, so almost all of it is elsewhere - most likely
MuPDF's object cache holding every page object that `page_sizes_lenient` loaded (`fz_load_page` per
page, plus its annotations) for the life of the document.

### Proposed Solution

Measure first, in the shape of item 1: `ws` after `Document::open`, after the page count, after the
sizes, and after dropping the document, on the 1,000- and 20,000-page synthetic files and on a real
large PDF once the maintainer supplies one. Then, only if the page loads are the cause, compare reading
the sizes through the page object alone (the *object* path of `page_sizes_bench.rs`, which skips the
annotation load) and, if MuPDF allows it, letting the xref drop parsed page objects after the sizes
are read.

### Alternatives Considered

- *Progressive page sizes (Plan 0002 Phase 3)* would spread the same cost over time without reducing
  it; it is about open latency, not this.

### Additional Context

At 20,000 pages the cost is 27 MiB, inside the 250 MiB ceiling; at 100,000 pages it would be ~135 MiB.

---

## 5. A possible slow working-set drift when one document is reopened many times

**Labels:** `performance`, `memory`, `investigation`

### Problem Statement

Reopening the 20,000-page synthetic document 30 times in one process (`benchmarks.md` §9.4) raises
the working set by 8.8-10.4 MiB over the first three opens - the step that makes row H (10 MiB) fail -
and then by about 0.13 MiB an open (160.4 MiB after the fourth, 163.9 MiB after the thirtieth). The
step looks like allocator warm-up; the drift is either noise or a small per-open retention.

### Proposed Solution

Run 200 reopens and fit a slope; if it is not flat, attribute it: the app (rows, base layers, the
recent-files model), the engine (the old `Document` and its display lists, the pool's per-thread
contexts), or MuPDF's global store (fonts and resources that outlive a document). Item 1's phase
labelling applies here as well.

### Alternatives Considered

- *Accept it*: at 0.13 MiB an open it would take hundreds of reopens to matter; but it must be
  measured before it is accepted.

---

## 6. Text pages render slowly above ~2800% zoom

**Labels:** `performance`, `rendering`, `investigation`

### Problem Statement

On `synthetic_text_20000p.pdf` (10 pt Helvetica, 50 lines a page) a zoom step up to 2276% shows its
first tile in 0-12 ms, but from 2845% to 6400% it takes 96-155 ms with nothing else running
(`benchmarks.md` §9.6, control run) - over Plan 0001's 50 ms `t_blank` line. Plan 0001's fixtures are
grids and vector fills, so this was not seen before.

### Proposed Solution

Measure one tile at 2276% and at 2845% with a profiler. The likely cause is MuPDF's glyph cache: glyphs
above a size limit are not cached as bitmaps and are filled as outlines for every tile they touch; a
10 pt glyph at 2845% is ~285 px. If so, the options are MuPDF's glyph-cache size limit (an `fz_context`
setting) or accepting it - text at those zooms is far larger than a screen.

### Alternatives Considered

- *Cap the zoom for text documents* - rejected by CLAUDE.md §4 rule 1.

---

## 7. Turn the mid-raster abort back on once mupdf-rs closes devices safely

**Labels:** `bug`, `performance`, `upstream`

### Problem Statement

`MID_RASTER_ABORT` (`crates/engine-mupdf/src/pool.rs`) is off because an aborted run leaves the draw
device's stack unbalanced and mupdf-rs 0.8 closes the device in `Drop` without a `fz_try`, which makes
MuPDF call `exit(1)` (`benchmarks.md` §9.7). With it off, a zoom burst on a page of expensive nodes
sometimes waits for tiles already rasterizing: `dense_vector.pdf` shows one burst in five at 190-230 ms
`t_blank` (21-26 ms with the abort).

### Proposed Solution

**Already fixed upstream, not released yet.** mupdf-rs `main` has a `fz_try`-guarded
`mupdf_close_device` wrapper in `mupdf-sys` and uses it in `Drop for Device` - commit `ffaf28f5`,
"Audit and fix correctness and soundness" (messense/mupdf-rs#249, 2026-07-08). The latest release,
`v0.8.0` (2026-06-22), predates it. Checked 2026-10-20; no pull request was sent, since it would
duplicate #249.

Once a release after `v0.8.0` carries it: update the `mupdf` dependency (dropping or rebasing
`patches/mupdf`), set `MID_RASTER_ABORT = true`, and check that `abort_safety_test.rs` passes with it
on and that `dense_vector.pdf` is back at §7.5's numbers. Depending on upstream `main` by git revision
before that release is possible, but needs `deny.toml` to allow the git source and `patches/mupdf`'s
MSVC change to be rebased.

### Alternatives Considered

- *A C shim in `patches/mupdf`*: keeps the abort now, but needs MuPDF's headers, which `mupdf-sys`
  does not expose to dependents, and compile flags matching its build; rejected by the maintainer.
- *Vendoring `mupdf-sys`*: 65 MB of MuPDF sources in this repository; rejected.

---

## 8. Notice when the open file is changed by another program

**Labels:** `enhancement`, `editing`

### Problem Statement

An in-place save (ADR 0007) writes after the bytes the document was opened with. If another
program changed the file meanwhile, MuPDF's `ensure_initial_incremental_contents` finds the bytes
different and copies the opened content into the file before its section, so the other program's
change is overwritten without a word (Plan 0004, §7).

### Proposed Solution

Before an in-place save, compare the file's length and modification time with the ones at open;
if they differ, ask whether to overwrite, Save As, or cancel. Optionally watch the file and offer to
reload while there are no unsaved changes.

### Alternatives Considered

- *Always Save As when the file changed*: safe, but surprising when the change was the app's own
  earlier save; the open-time values have to be updated after each save either way.

### Additional Context

Out of scope for v0.2 (Plan 0004 §7). Found while reading MuPDF's incremental writer in Phase 1.

---

## 9. Build and test on Android (first) and iOS

**Labels:** `platform`, `mobile`, `investigation`

### Problem Statement

Android and then iOS are targets (CLAUDE.md §1; the maintainer set Android first on 2026-10-21).
Read from the pinned crates on 2026-10-21, then checked by a trial Android build of the two library
crates (below); the app crate has not been built for either:

- **Certain compile blockers, in this repository's app crate:**
  - `rfd` 0.17.2 has file-dialog backends for Windows, macOS, Linux/BSD and wasm only
    (`rfd-0.17.2/src/backend.rs`); on Android and iOS its dialog traits have no implementation.
  - `arboard` 3.6.1 has platform modules for Windows, macOS and Linux only, and excludes Android
    explicitly (`arboard-3.6.1/src/platform/mod.rs:1-16`).
  - The app is a `bin` with `main()`. Slint on Android needs the `backend-android-activity-06`
    feature and an `android_main` entry point in a `cdylib` (`slint-1.18.1/android.rs:5-16`), plus
    packaging (an APK/AAB tool such as `cargo-apk` or `xbuild`).
- **Trial build, 2026-10-21: `pdf-core` and `engine-mupdf` build for Android, and their test
  binaries link.**

  **Setup:**
  - Host: the maintainer's Debian mini PC (x86_64, forky/sid, 4 cores, 15 GiB).
  - Toolchain: Rust stable 1.99.1 (the repository pins `stable`), target `aarch64-linux-android`.
  - NDK: r28c, API level 26, GNU make from Debian.

  **Environment the build needs** (the NDK's own tools, nothing installed system-wide):
  - `CC`/`CXX`/`AR_aarch64_linux_android`: the NDK's `aarch64-linux-android26-clang`,
    `aarch64-linux-android26-clang++` and `llvm-ar`.
  - `CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER`: the same clang.
  - `LIBCLANG_PATH`: the NDK's `toolchains/llvm/prebuilt/linux-x86_64/lib`. mupdf-sys runs
    `bindgen` (0.72.1), which needs a host libclang.
  - `BINDGEN_EXTRA_CLANG_ARGS_aarch64_linux_android`: `--sysroot=<ndk>/toolchains/llvm/prebuilt/linux-x86_64/sysroot -isystem <ndk>/toolchains/llvm/prebuilt/linux-x86_64/lib/clang/19/include`.
    Without the `-isystem`, bindgen fails on `'stddef.h' file not found`.

  **Results:**
  - `cargo build -p pdf-core --target aarch64-linux-android`: 1.4 s, no change needed.
  - `cargo build -p engine-mupdf --target aarch64-linux-android`: **succeeds**, 3 min 56 s. Most of
    that is MuPDF's C sources compiled by `make`; it is rebuilt each time the build script reruns.
  - `cargo test -p engine-mupdf -p pdf-core --target aarch64-linux-android --no-run`: **fails to
    link** without the C++ runtime (`__cxa_throw`, `std::length_error`, ...). Part of MuPDF's
    bundled third-party code is C++, and mupdf-sys links a C++ library only with its `tesseract`
    feature, as `stdc++` (`mupdf-sys-0.8.0/make.rs:253-258`); on Android that has to be the NDK's
    libc++.
  - With `CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS="-C link-arg=-lc++_static -C link-arg=-lc++abi"`,
    all **15 test executables link** as AArch64 PIE binaries. Their dynamic dependencies are
    `libc`, `libm`, `libdl`, `liblog` and `libstdc++`, all system libraries on Android.
  - **Run on a phone, 2026-10-22: 124 of 124 tests pass.**
    - Device: Samsung Galaxy A34 (SM-A346E), Android 16 (API 36), arm64-v8a.
    - Method: the test binaries were pushed with `adb` and run from `/data/local/tmp`.
    - Per binary: `pdf_core` 82; `edit_test` 7; `tile_render_test` 8; `pool_test` 5;
      `integration_test` 5; `search_test` 4; `selection_test` 4; `annotation_render_test` 3;
      `sidebar_test` 3; `engine_mupdf` 2; `abort_safety_test` 1.
    - The 7 ignored measurement tests were not run.
    - Paths: the tests read fixtures and write scratch files at absolute paths fixed at build time
      (`CARGO_MANIFEST_DIR`, `CARGO_TARGET_TMPDIR`). So the build ran from a checkout at
      `/data/local/tmp/fa-pdf` on the build host, the same path the phone uses. The fixtures went
      to that path too, plus `target/aarch64-linux-android/tmp`, which is where a cross build puts
      `CARGO_TARGET_TMPDIR` instead of `target/tmp`.
    - Notable: `edit_test` includes the in-place incremental save, which therefore works on
      Android's file system as well. `abort_safety_test` took 19 s on the phone.
  - **Run again on the same phone, 2026-10-22, at `817113a` (Plan 0004 Phases 2-4): 152 of 152
    tests pass** in 18 test executables, the same method.
    - New since the first run: `undo_test` 5 (the journal shim on Android), `note_test` 3,
      `ink_test` 2, and 8 more `pdf_core` unit tests (90).
    - The 7 ignored measurement tests were not run. `abort_safety_test` took 18 s.

  **To do in the mobile plan:**
  - Make the libc++ link part of the build, not an environment variable: a `build.rs` line in
    `engine-mupdf` for `target_os = "android"`, or upstream in mupdf-sys.
  - Put the environment above in a script or a CI job.

  **Already handled:** `patches/mupdf` leaves `font-kit` out on Android (`Cargo.toml:177`).

  **iOS:** Slint's winit backend has iOS code (for example `i-slint-backend-winit-1.18.1/clipboard.rs:117`,
  a `UIPasteboard` clipboard). No iOS build was tried: it needs a Mac.
- **Design work, not compile errors:**
  - Android hands the app `content://` URIs, not paths. MuPDF and the search thread open by path,
    and an in-place incremental save (ADR 0007) cannot append to a URI. A cache copy, plus a full
    write back through the Storage Access Framework, is the likely shape.
  - The settings file path comes from desktop environment variables. Without them it is `None`,
    which keeps the defaults rather than failing.
  - The UI is desktop-shaped: a menu bar, a wide tool strip, and hover. Touch selection by long
    press already exists (ADR 0006).
- **Already portable:** `pdf-core` (pure Rust, no I/O) and the engine logic. The tile cache budget
  is already 16 MiB on Android/iOS (`pdf_core::scheduler::default_cache_bytes`).

### Proposed Solution

A plan of its own when the maintainer starts mobile work:

- **Phase 0:**
  1. Script the device run above (build at the device path, push, run) or move the tests to
     relative paths, so it is one command.
  2. Build the app with `rfd`/`arboard` behind a desktop `cfg`, an `android_main` entry point and
     Slint's `backend-android-activity-06`.
  3. Add an Android CI job (GitHub's Ubuntu runners ship an NDK).
- **After that:** design file access and the mobile UI.

### Alternatives Considered

- *Gate `rfd` and `arboard` behind a desktop `cfg` now*: cheap, but untestable until a mobile build
  exists, so it waits for that plan's Phase 0.

### Additional Context

Found while answering the maintainer's question on 2026-10-21. The trial build changed nothing in
this repository: everything it needed was environment variables on the build host.

---

## 10. Move to the mupdf-rs release that has our upstream changes

**Labels:** `dependencies`, `upstream`

### Problem Statement

Three upstream changes are on mupdf-rs `main` but in no release:

- `ffaf28f5`: devices closed inside `fz_try`, messense/mupdf-rs#249.
- `cb732cb3`: `TextPage::copy_selection` and `copy_rectangle`, #272.
- `7aa360d3`: the PDF undo journal, #273.

The last two were merged on 2026-10-22. crates.io still has 0.8.0 (2026-06-22), and `main` is 17
commits past it. Until a release, fa-pdf carries:

- `patches/mupdf` with the MSVC fix;
- the journal shim with its 968 KiB of copied MuPDF headers (ADR 0008);
- `MID_RASTER_ABORT = false` (item 7);
- its own selection text (ADR 0006).

### Proposed Solution

When a release after 0.8.0 is published:

1. Depend on it and drop `patches/mupdf` (journal, `build.rs`, the headers, the `=0.8.0` pin) and
   the `[patch.crates-io]` entry.
2. Set `MID_RASTER_ABORT = true` and check `abort_safety_test.rs` and `dense_vector.pdf` (item 7).
3. Decide whether to take MuPDF's `copy_selection` for the copied text (ADR 0006).
4. Adapt to the API changes between 0.8.0 and the release. Known so far:
   - `from_bytes` was renamed `from_copied_bytes` (#259);
   - contexts are per thread family (#263, #264), which the render pool's threads depend on.
5. Run the whole suite, the bench gates and the Android tests again.

### Alternatives Considered

- *Depend on `main` by git revision now*: the shim, the patch and the abort switch could go today.
  Against:
  - `deny.toml` must allow a git source;
  - every clean build fetches mupdf-rs with MuPDF's submodules;
  - dependabot does not follow a git revision;
  - it takes 17 unreleased commits, including the context changes, at once.

  Offered to the maintainer on 2026-10-22.
