<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# ADR 0006: Clipboard and Text Selection

## Status

Accepted (Plan 0003 Phase 0 gate, questions 3, 5 and 6, 2026-10-20).

## Context

v0.1 copies selected text. Three things had to be decided:

- **The clipboard.** Slint 1.18.1 has no public API for writing the clipboard from Rust: only
  `TextInput.copy()` and the `Platform::set_clipboard_text` hook, which application code cannot reach
  (`i-slint-core-1.18.1/platform.rs:142`). `arboard` 3.6.1 is already linked at run time through
  Slint's winit backend on Windows and Linux, with `default-features = false`.
- **Where the selected text comes from.** MuPDF has `fz_copy_selection`, but neither `mupdf-sys`'
  guarded wrappers nor mupdf-rs expose it, and calling the raw binding is undefined behaviour,
  because MuPDF reports errors by `longjmp`. mupdf-rs' `TextPage::highlight_selection` writes its
  result through a `&[Quad]`, a shared slice, which is unsound to rely on.
- **What a mouse drag does.** It panned the page until now.

## Decision

1. **`arboard` 3.6 directly**, with the features Slint already uses (`default-features = false`,
   `wayland-data-control` on Linux), so `Cargo.lock` gains no crate. One `arboard::Clipboard` is
   created on the first copy and kept outside the thread-local app state; `main` drops it explicitly
   after the event loop, because on X11 that drop hands the copied text to the clipboard manager
   (ICCCM `SAVE_TARGETS`, `arboard-3.6.1/src/platform/linux/x11.rs:711-795, 1082-1096`) and Rust does
   not promise to run the main thread's thread-local destructors.
2. **Selection is computed in Rust** (`pdf_core::select`) over the characters of MuPDF's structured
   text in reading order, extracted on the search thread (ADR 0005). A selection is the run of
   characters between two boundaries; its highlight (one rectangle per line) and its text come from
   the same characters, so they cannot disagree. A pull request to mupdf-rs adds a guarded
   `TextPage::copy_selection` / `copy_rectangle` (messense/mupdf-rs#272, opened 2026-10-20, merged into mupdf-rs `main` as `cb732cb3` on
   2026-10-22; no release carries it yet); if it
   is merged and released, the app can switch to MuPDF's own selection text.
3. **A mouse drag selects; touch pans.** The `Flickable`'s `mouse-drag-pan-enabled` is off, so a mouse
   drag selects text (double-click: a word, triple-click: a line, Shift+click: extend), while touch
   still pans; a 500 ms long press starts a selection on touch, during which the `Flickable` stops
   panning. Mouse and touch are told apart by `PointerEvent.touch_finger_id`. A selection starts
   only **on text**: the app keeps one box per text line for the pages near the view and shows the
   text cursor, or starts a selection, only within 3 pt of one (added after the maintainer's review,
   2026-10-21); elsewhere a press clears the selection.

## Consequences

### Positive
- No new crate for the clipboard, and a copy survives quitting on X11 where a clipboard manager runs.
- Highlight and copied text always match, and the selection logic is tested without a document.
- A selection is answered between two pages of a running search, so a drag never waits for one.

### Negative
- Our reading order is MuPDF's structured-text order; complex layouts (columns, vertical text, mixed
  directions) select in that order, which may differ from what `fz_copy_selection` would give.
- With the mouse, panning is by wheel, scrollbar or keyboard only; there is no hand tool yet.
- Touch selection is untested here (no touchscreen); it is a manual check.
