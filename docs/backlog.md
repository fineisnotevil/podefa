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

