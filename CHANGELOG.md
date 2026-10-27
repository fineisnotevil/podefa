<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **Tools by touch** (Plan 0008 Phase 4): on a phone, Tools shows the pages in one column, each
  with its number, a check box and buttons to rotate, duplicate, extract and delete. A tap makes a
  page current and a second tap opens it; a "+" between pages splits there or inserts a blank page
  or pages from a file; holding a page for half a second picks it up to move it. With pages
  chosen, a bar over the dock acts on all of them. On the phone the tools' dialogs open as sheets
  above the dock or the keyboard.
- **Editing by touch** (Plan 0008 Phase 3): in Edit, the phone shows an edit strip over the dock
  with every Edit tool, rotation and the form's commands, and the chosen tool's colours and ink
  widths over it. A tap fills a form field, places a note with the Note tool, selects an
  annotation, and opens a selected note; a long press on a note moves it; Ink and Crop draw with
  one finger. A field or note being edited moves above the on-screen keyboard and below the
  header. Ink strokes log their frames in the bench trace.
- **A tool's dialog opened in Tools shows** (Watermark, Page Numbers, Merge and the others): it
  was hidden with the pages behind the gallery.
- **Back on the phone** (Plan 0008 Phase 3) closes one thing at a time: a menu, a card, the
  bottom sheet, a dialog, the search bar, an editor (keeping its text) or a selection, then goes
  from Edit or Tools back to Read, and last asks about unsaved changes before the app closes.
  It used to close the app at once and lose unsaved changes.
- **Pinch zoom and double tap** (Plan 0008 Phase 2): two fingers on a touch screen, or a pinch on
  a trackpad, zoom around the fingers; the pages follow them at once and sharpen when they lift. A
  double tap with a finger fits the page's width, and a second one goes back to the zoom before.
  The bench trace logs pointer presses and releases and each pinch's frames.
- **Android app** (Plan 0008 Phase 1, ADR 0012): `scripts/android-build.sh` builds an arm64 APK
  (`org.fineassociation.podefa`) with the mobile interface. Documents come from the system's
  picker or "Open with"; the app works on a copy and writes saves, Save As, tool outputs and
  exported form data back to the document or folder that was picked. Copy uses Android's
  clipboard, settings live in the app's own folder. CI builds the APK.
- **Mobile interface** (Plan 0008): on a phone, and on the desktop with `--mobile`, the window
  shows the concept's mobile shell instead of the desktop islands: a header island with the
  document, search, save, pages and a More menu; a thumb dock with the pager, the three modes and
  the zoom, whose cards open over it; a bottom sheet with pages, contents and open documents.
- **Tabs** (Plan 0007 Phase 4, ADR 0011): open several documents in one window. Open, Open
  Recent, a dropped file and the "+" button (Ctrl+T) open a document in a new tab (or in the
  shown tab when it is empty, or switch to the tab that has it); tabs share the title row down to
  a narrow width, then scroll with the wheel. Close a tab with its close button, a middle click or
  Ctrl+W (the last tab closes the window); Ctrl+Tab, Ctrl+Shift+Tab, Ctrl+Page Down and Ctrl+Page
  Up step through them. Closing the window asks about each tab with unsaved changes in turn.
- **Pages between tabs:** drag pages from the gallery or the thumbnails onto a tab to copy them
  to the end of its document (one undo step there), or onto "+" to open them as a new document.
- **Split in the gallery again:** the scissors in a gap's pill move the pages after it into a new
  tab, with no dialog; the new document is saved with Save As.

### Changed
- **Opening a document keeps the one you have open**: it opens in a tab of its own, so opening no
  longer asks to save the shown document first.
- **The window shares one tile budget and one set of raster workers across its tabs:** only the
  shown tab holds tiles, base layers, thumbnails and display lists.
- **The window's colours come from one set of design tokens** (Plan 0007 Phase 1): the canvas,
  panels, lines and text follow the new design's light and dark themes, and "System" follows the
  operating system. The window's markup moved from `main.rs` into `crates/app/ui/*.slint`.
- **A new window** (Plan 0007 Phase 2): the window draws its own frame (move by the document's
  name, resize from every edge, double click to maximize) and floats its controls in islands over
  the canvas: the title row with Open, Save, Undo and Redo; the brand menu with the theme; a
  sidebar with Read, Edit and Tools modes; the status island with pages and zoom; search under the
  title. There is no menu bar: every menu command moved to its island (ADR 0010). macOS keeps its
  native frame.
- **The thumbnails and outline drawer is Read's only** and ends above the status row.
- **The gallery** sits in the middle while its pages fit, and its rulers carry a scroll crown:
  drag it, or turn the wheel over it, to scroll.
- **The islands float over the pages** (Plan 0007 Phase 3): the pages run under the whole
  window and pass under the title row, the sidebar and the status island, which are slightly
  see-through; the first page opens just under the title, and fit width, fit page, page steps and
  jumps use the room beside the islands.
- **Read, Edit and Tools look and move apart** (Plan 0007 Phase 3): Edit lays a dot grid under the
  pages and tints the active markup tool; its colours and line widths pour out beside it. Tools
  pulls back to a gallery of the pages on a desk, in place of the drawer's thumbnails and the
  Pages menu. The canvas only fades and scales during a change, so no page is drawn again.

### Added
- **The Tools gallery** (Plan 0007 Phase 3): click, Ctrl+click and Shift+click choose pages, the
  tick box adds one, Ctrl+A chooses all; drag one or the chosen pages into a gap to move them;
  each page's hover bar rotates, extracts, duplicates or deletes it, and a bar over several chosen
  pages does the same to them; the gap between two pages inserts a blank page or pages from a file
  there. Pages slide into place after a delete, insert or move
  instead of being drawn again. Double click opens a page in Read; arrows, Home and End move
  through the pages, Delete deletes the chosen ones.
- **Ctrl+W closes the window** (asking about unsaved changes), and the shortcuts Open, Save, Save
  As, Find, the sidebar and zoom work from any text field.

### Removed
- **Quit and Ctrl+Q:** the close button and Ctrl+W do the same.

### Fixed
- **A list box shows its chosen options:** MuPDF 1.27.2 draws a list box without marking its value.
- **Field text with Turkish letters fills in about 2 ms instead of 2.4-3.5 s**, and sits inside a single-line field instead of being clipped (MuPDF asked for a `serif` font through the system font search; MuPDF 1.27.2 placed single-line rich text below the field). Radio groups show the chosen button.
- **Copied pages kept their annotations and links:** duplicating a page lost its markup, notes, ink and links, because MuPDF's page copy leaves `/Annots` out. They are copied now, with page links pointing at the copied pages.
- **Saving in place more than once in a session broke the file.** From the second Ctrl+S, the new section's `/Prev` pointed at the previous save's cross-reference table, which that save had just overwritten (MuPDF 1.27.2's incremental writer moves its notion of the file's table to each section it writes). Readers then lost pages; one file opened with none. Each in-place save now names the file's own table. A document with no readable pages is now refused with a message instead of opening empty.

### Changed
- **Unembedded standard fonts load at once:** MuPDF's built-in copies of the 14 standard PDF fonts are used before a system font search, which cost 1.2-1.7 s per font on Windows (a document's first page, a duplicated page). Incremental saves compress the streams they add.

### Added
- **Form data and flattening** (Plan 0006 Phase 3): Form > Export Data... writes every field's value as XFDF (which Acrobat and most readers import) or JSON; Form > Import Data... fills the form from such a file in one undo step, leaving out fields the form does not have and values a field does not take. Form > Flatten Form draws the fields into the pages, as one undo step.
- **Fill forms with the keyboard** (Plan 0006 Phase 2): Tab and Shift+Tab go from field to field in the page's tab order and on to the next page with a field; Space or Enter sets a check box or radio button; Up, Down and Enter choose in a list. Multi-select lists take several choices. Form > Reset Field and Reset Form put values back to their defaults (one undo step).
- **Form fields are shaded** so they can be seen before they are touched (red edge when required); View > Highlight Form Fields turns it off. **Comb fields** are edited one character per box.
- **Fill forms** (Plan 0006 Phase 1): with the Select tool, a click on a form field fills it - an editor laid over a text field (Enter, Tab or a click elsewhere keeps the text, Esc drops it), check boxes toggle, radio buttons turn on, combo and list boxes open their options. Each change is one undo step and is saved with the document; a field on several pages changes everywhere. Tooltips show on hover; forms with scripts or XFA say so in the status bar (scripts are not run).
- **Export as Word, built only on request** (Plan 0005 Phase 5): with the `docx` Cargo feature (off by default), Tools > Export as Word writes the document, unsaved changes included, as a DOCX file through MuPDF's DOCX writer, with progress and Cancel. It is off because that writer does not keep columns or tables (`docs/backlog.md` item 16).
- **Compress, export as images, images to PDF, repaired copy** (Plan 0005 Phase 4): Tools > Compress writes a smaller copy (Lossless; Balanced: images to 150 dpi JPEG; Strong: 96 dpi), never larger than the original and without touching the open document's undo history; Tools > Export as Images writes pages as PNG or JPEG at a chosen resolution; Tools > Images to PDF makes a PDF of image files, one per page, at their size, in an order set in a list. A file MuPDF had to repair at open says so and File offers Save Repaired Copy. `--bench-convert` checks them through the window.
- **Watermark, page numbers and crop** (Plan 0005 Phase 3): Tools > Watermark writes text across every page (or a range), diagonal or horizontal, in a chosen colour and opacity; Tools > Page Numbers writes a number such as "Page {n} of {total}" in one of six positions; Tools > Crop Pages draws a rectangle on a page and crops that page or all pages to it. Each is one undo step, works on rotated pages, and the stamped text (Turkish letters included) can be selected and searched. `--bench-stamps` checks them through the window.
- **Merge, split, extract, insert and properties** (Plan 0005 Phase 2): a Tools menu merges PDFs in a chosen order (the open document included, unsaved changes and all), splits the document every N pages or by page ranges, extracts the chosen pages, inserts the pages of another file (one undo step) and edits the document properties (title, author, subject, keywords; one undo step). Jobs run on their own thread with progress and Cancel in the status bar, and never replace an existing file unless they finish. `--bench-tools` checks merging and extracting through the window.
- **Organize pages** (Plan 0005 Phase 1): choose thumbnails with a click, Ctrl+click and Shift+click, then rotate, duplicate, delete, move (strip buttons, the Pages menu, or by dragging a thumbnail) and insert a blank page. Each is one undo step; the view stays on the page you were reading. `--bench-pages` checks every operation through the window and reports `t_restructure`.
- **Ink** (Plan 0004 Phase 4): an Ink tool (I) draws free-hand lines with the mouse, a pen or a finger (touch does not pan while it is active), in the chosen colour and one of three widths. The line shows live while drawing and is simplified (0.5 pt) into one PDF `Ink` annotation per stroke; a click selects ink only on its line. Recolour, Delete and undo work as for markup. `--bench-ink` checks it through the window and reports `ink_rebuild_ms`; the edit phase reports `ws_after_edits_n`, and `FA_PDF_NO_JOURNAL` turns the undo journal off for that comparison.
- **Sticky notes** (Plan 0004 Phase 3): a Note tool (N) places a note where you click and opens an editor beside it (Ctrl+Enter or Save keeps the text, Esc or Cancel drops it). A double-click edits a note's text, dragging moves it on its page, and resting the pointer on it shows its text. Notes are PDF `Text` annotations with MuPDF's 16 pt icon and their popup beside the icon; each change is one undo step, and their text, non-ASCII included, survives saving byte for byte. `--bench-note` checks it through the window.
- **Undo and redo** (Plan 0004 Phase 2): Edit > Undo (Ctrl+Z) and Redo (Ctrl+Y, Ctrl+Shift+Z) for every edit, through MuPDF's own undo journal; an edit is one step, cutting old markup included. Undoing back to the last save makes the document clean again. `patches/mupdf` wraps the journal in `fz_try` (`journal/journal.c`, compiled against a copy of MuPDF 1.27.2's headers that MuPDF checks at run time; mupdf-sys pinned to `=0.8.0`) until a mupdf-rs release does (ADR 0008).
- **Annotating and saving** (Plan 0004 Phase 1): a tool strip for PDFs - Select, Highlight (H), Underline (U), Strike-out (K), Squiggly, six colours, Delete. A markup button marks the selected text, or with nothing selected stays on so every drag marks; a click selects an annotation, a swatch recolours it, Delete removes it. File > Save (Ctrl+S) appends an incremental update to the open file, Save As (Ctrl+Shift+S) writes a copy and switches to it, a file MuPDF repaired at open goes to Save As, the title shows `*` while unsaved, and opening another file, quitting or closing the window asks first. Edits run on the engine actor; every page has a revision that tiles, base layers and thumbnails carry, so an edited page keeps its tiles until the new ones arrive and never shows older pixels after newer ones (ADR 0007). Other formats MuPDF opens are view-only. New markup replaces old markup of the same kind where they overlap instead of stacking on it (other kinds stay, markup with a comment is never cut). An optional `author=` setting is written into new markup. `--bench-edit` measures `t_edit` (median 3 ms, `docs/benchmarks.md` §10.3).
- Tiled, viewport-only rendering: pages rasterize as 512 px tiles through a bounded cache, so zoom and scroll cost scales with the visible area rather than the whole page.
- `crates/core`: pure tile geometry (`tiling`) and the tile request/cache scheduler (`scheduler`), including integer permille zoom scales and a configurable tile cache ceiling.
- `crates/engine-mupdf`: tiled rasterization on the engine actor, reusing a single scratch pixmap and caching the page display list.
- `crates/engine-mupdf`: a render worker pool (`clamp(cores - 1, 1, 4)` threads, `FA_PDF_WORKERS` overrides) sharing one cached display list per page as `Arc<DisplayList>`, plus a low-resolution base layer per page for the frames while a new scale's tiles are still in flight; a superseded zoom epoch drops its queued tiles and aborts the tile already inside the rasterizer through MuPDF's cancel cookie, reporting the measured abort latency in `EngineEvent::TileAborted`.
- `crates/app`: draws the base layer as the whole-page backdrop until the new scale's tiles land, and logs `abort_ms` (min/median/max) for superseded tiles in the `--bench-*` report.
- `crates/app`: `--bench-repeat N` repeats a `--bench-*` scenario and reports each run's worst beside the min/median/max across runs, so a gate number one run can only sample once (a rapid burst leaves `t_blank` with the last step alone) has a distribution; each repeat restores the viewport and drops the tile cache, so every run measures the same cold scenario instead of one served by the previous run's cache.
- `crates/engine-mupdf`: pool tests for the cancellation invariants, plus `dense_vector.pdf` (one path node of 1000 page-spanning cubics) to measure them on: an epoch cancelled before its work is dispatched publishes nothing at all, every tile delivered while an epoch is being cancelled is still byte-identical to the serial render, and the abort latency is reported for a page of small nodes and for a page that is a single heavy node.
- `crates/core`: `TileScheduler` accessors for pending requests and the active scale, so the app can tell useful raster work from backlog.
- Implementation plans under `docs/plans/` (`0001-tiled-viewport-rendering.md`, `0002-continuous-multipage-scroll.md`).
- Multi-crate Cargo workspace structure:
  - `crates/core`: Engine-agnostic domain models (`PdfEngine`, `DocumentInfo`, `Rect`, `Bitmap`, `Command`).
  - `crates/engine-mupdf`: MuPDF integration stub implementing `PdfEngine`.
  - `crates/app`: Declarative Slint UI desktop application scaffold.
- Release compilation profile with binary size optimizations (`opt-level = "z"`, fat LTO, abort panic, symbol stripping).
- Complete REUSE 3.3 specification compliance with SPDX license identifiers and `LICENSES/` catalog.
- Auditing configuration via `deny.toml` enforcing AGPL-3.0 compatibility and dependency bans.
- Architectural Decision Records (`docs/adr/`):
  - `0001-stack-rust-slint-mupdf.md`
  - `0002-license-agpl.md`
  - `0003-engine-actor-threading.md`
- Community and project health standards (`README.md`, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md`, `justfile`).
- GitHub Actions multi-platform CI matrix (Linux, macOS, Windows) with linting, tests, license checks, REUSE compliance, and DCO sign-off validation.
- `crates/app`: the `--bench-*` trace samples the process's resident (`WorkingSetSize`) and committed private (`PrivateUsage`) bytes every 250 ms through the Windows API directly (`GetProcessMemoryInfo` in a hand-written `extern` block, no new dependency), logs them beside the tile cache's bytes, rows, `dropped` and `evicted` on the same timeline as the level changes and aborts, and reports min/median/max plus the process's peak in the summary, so a run's memory plateau can be read against the cache that produced it.
- `crates/app`: `--cache-mib N` sets the tile cache's byte budget (default unchanged: 64 MiB on desktop, 16 MiB on mobile) and `--cache-rows N` the model row count, which is the cache's real ceiling because a scheduler never holds more tiles than it has rows; a 900 px-wide window caps the cache at ~12 MiB of RGB8 tiles before the default budget can bind, so a memory run that needs the budget to bind raises the rows too.
- `crates/core`: `TileScheduler::evicted()` counts the tiles released to stay inside the byte budget, which `dropped()` never did (it counts superseded, over-capacity and row-less tiles); the `dropped` documentation now says which is which.
- `docs/benchmarks.md` §7.6: the RSS sampler's line format, the two cache ceilings told apart, and the long-pan eviction scenario (A0 at 6400%, panned to the page edge) run at the 64 MiB and 8 MiB budgets as well as the default, with the measured plateau following whichever ceiling is lower each time - 12 MiB of rows by default, 63.8 MiB of the 64 MiB budget, 7.5 MiB of the 8 - while `dropped` stays at the zoom burst's 123 in all three. It states the Phase 2 memory target as one metric (peak working set <= 250 MiB in the default configuration, measured 192.7 MiB) and flags what it does not claim: run B, whose rows are raised so the budget binds, peaks at 267.3 MiB.

- `crates/core`: `TileScheduler::prefetch` requests the one-tile ring around the viewport ahead of the user's pan. Its tiles land in the cache as hidden, evictable entries, so they cost no memory beyond the cache's ceiling, are never counted as a hole, never take a row the visible set claims, and are not requested at all once the cache holds its whole byte budget. Measured in `docs/benchmarks.md` §7.6: a 296-step pan at 10,240 px/s shows 0 hole frames against 241 of 296 with the ring blocked, for about three times the tile rasters at the default row count - the ceiling and its upgrade path are recorded on `AppState::ring`.
- `docs/backlog.md`: three issue drafts for work that is measured but deliberately not done - the ~70 MiB fixed overhead between an empty process and a loaded one (MuPDF's store, the display list, the base layer), an allocator experiment, and the `private`-vs-working-set gap - each written in the shape of `.github/ISSUE_TEMPLATE/` so it can be filed as it stands.


### Added
- `docs/plans/0004-v0.2-annotate.md`: Phase 0 of the v0.2 annotate plan, at its gate - showing existing annotations, text markup, notes, ink, incremental save and undo/redo, with the mupdf-rs, MuPDF and Slint APIs each depends on verified in source, four phases with acceptance rows, and six questions for the approver. No feature code. `crates/engine-mupdf/tests/annot_bench.rs` (ignored) and `docs/benchmarks.md` §10 measure what annotating and saving cost; they show that an incremental save in place works on Windows with the file open twice while a rename over the open file does not, and that edits made while the workers render leave 0 pixel mismatches. It also found that v0.1 does not draw the annotations a document already has.
- **The v0.1 viewer plan is complete** (`docs/plans/0003-v0.1-viewer.md`, closed 2026-10-21): opening documents, recent files and themes, a thumbnail and outline sidebar, search, and text selection with copy.
- **Text selection and copy** (Plan 0003 Phase 4): a mouse drag selects text (double-click a word, triple-click a line, Shift+click extends, dragging past an edge scrolls), touch pans and a long press selects; Ctrl+C or Edit > Copy copies through `arboard` (the copy Slint already links). Selection is computed in `pdf_core::select` from MuPDF's structured text on the search thread, so the highlight and the copied text always match (ADR 0006). `--bench-select` checks the pointer path; a pull request to mupdf-rs adds a safe `copy_selection` (messense/mupdf-rs#272).

### Fixed
- A click (or a touch tap) now clears the selection's highlight: the selected text was dropped, but the blue rectangles stayed on screen.
- **Annotations a document already has are drawn** (highlights, notes, stamps, form-field appearances): tiles, the base layer, thumbnails and the full-page render used to draw the page's content stream alone, so they were invisible (Plan 0004 §3.1). On pages without annotations the cost is unchanged (`docs/benchmarks.md` §10.1); `crates/engine-mupdf/tests/annotation_render_test.rs` checks a highlight in a tile, the full-page render and a thumbnail.
- The text cursor shows only over text, and a click or drag away from text no longer selects the nearest word (it clears the selection instead).
- **A crash when some PDFs were zoomed quickly** (exit code 1, no message): a cancelled tile aborted inside MuPDF left the draw device unbalanced, and mupdf-rs 0.8 closes devices without catching MuPDF's error, so MuPDF ended the process. The mid-raster abort is switched off until mupdf-rs closes devices safely (`MID_RASTER_ABORT` in `crates/engine-mupdf/src/pool.rs`); queued tiles of a cancelled zoom are still dropped and their pixels never shown. `crates/engine-mupdf/tests/abort_safety_test.rs` reproduces the crash; `docs/benchmarks.md` §9.7 measures the cost on the worst-case fixture.
- The find bar closes when another document is opened, and its placeholder names Enter / Shift+Enter.

### Changed
- **Search** (Plan 0003 Phase 3): Ctrl+F opens a find bar; results stream in from a search thread with its own document (ADR 0005), starting at the reader's page, highlighted on the pages and stepped through with Enter/F3 and Shift+Enter/Shift+F3. The first hit arrives within a millisecond because the search document is opened and warmed up when a document opens; a search runs at ~1.4 ms a page and a new query stops the old one at the next page. `--bench-search` measures it (`docs/benchmarks.md` §9.6), which also found text rendering slow above ~2800% zoom (`docs/backlog.md` item 6).
- **Sidebar** (Plan 0003 Phase 2): page thumbnails in a virtualized list (at most 64 images held, the current page marked and kept in view) and the document's outline as an expandable tree that jumps to each entry's position; toggled with F4, Ctrl+B, a toolbar button or View > Sidebar, and remembered. The engine sends the flattened outline after `Opened` and renders thumbnails from display lists it does not cache, so the sidebar cannot evict the viewport's. `--bench-thumbs` and `--bench-outline` measure it (`docs/benchmarks.md` §9.5: 64 thumbnails at most over 20,000 pages, `t_blank` worst 2 ms; 4 of 4 outline entries).
- **Opening documents** (Plan 0003 Phase 1): a menu bar (File: Open... Ctrl+O, Open Recent, Quit Ctrl+Q; View: Theme), the platform's file dialog through `rfd` 0.17, replacing the open document (a failed open keeps the previous one), the ten most recent files and the theme (system, light, dark) persisted in a plain-text settings file, and the app's colours from Slint's `Palette`. A `DropArea` opens a dropped PDF through Slint's stable `DataTransfer` API; it becomes active with the Slint release that delivers external drops on winit (slint-ui/slint#13527). `crates/core` gains `settings`; ADR 0004 records the decisions and ADR 0003 is amended to the tile pipeline. `--bench-reopen` measures reopening; `docs/benchmarks.md` §9.4 reports row H as a FAIL (+11.7 MiB after ten opens, limit +10).
- `docs/plans/0003-v0.1-viewer.md`: Phase 0 of the v0.1 viewer plan, at its gate - open/recent/theme, thumbnails and outline, background search, selection and copy, with the Slint and MuPDF APIs each depends on verified in source, four phases with acceptance rows, and seven questions for the approver. No feature code. `crates/engine-mupdf/tests/text_bench.rs` (ignored) and `docs/benchmarks.md` §9 measure text extraction (~1.1 ms a page) and search (~0.45 ms a page) on generated documents and the `rfd` file dialog's size on Windows (+1,536 bytes).
- **Navigation** (Plan 0002 Phase 2): arrows, PgUp/PgDn, Space/Shift+Space, Home/End and Ctrl `+`/`-`/`0` on the viewport; ctrl+wheel zooms at the cursor (one notch per zoom step, a touchpad pinch by fractions of one); a go-to-page field, Fit width and Fit page buttons, and a scrollbar for the whole document. `crates/core` gains `nav` (scroll targets, fit zooms, wheel zoom, scrollbar math); the `--bench-nav` harness checks all of it through the window's own input path (`docs/benchmarks.md` §8.4: 20 of 20 steps).
- **Continuous multi-page scroll** (Plan 0002 Phase 1): the whole document scrolls as one column of pages with an 8 px gap, centred, opening at 100%. The scroll position is an `f64` in document pixels owned by Rust, and Slint's `Flickable` is given only a canvas window around the viewport (re-anchored about every 786 k px of travel), so positions stay exact at any page count and zoom despite Slint's `f32` lengths. Only the pages near the viewport hold tiles or base layers; a page that fits in one tile is its own image. A zoom keeps the page point under its anchor still across pages, and Previous/Next jump to the neighbouring page.
- `crates/core`: `layout` - `DocLayout` (exact `u64` page offsets, page-in-band and page-near queries, cross-page visible cells, page-relative zoom anchor) and `CanvasWindow`; `TileScheduler::grow` adds model rows without dropping the cache.
- `crates/engine-mupdf`: display lists are cached for the last eight pages instead of one; `EngineEvent::Opened` carries `unsized_pages`, and a page whose bounds cannot be read is laid out as Letter instead of emptying the whole page table.
- `crates/app`: `--bench-scroll`, `--bench-scroll-s` and `--bench-jump`, plus `pages_resident`, `pages_visited`, re-anchor and `ws_growth` reporting. `docs/benchmarks.md` §8.3 records the Phase 1 gate run, including one acceptance row that fails its line (`ws_growth` at 6400% and 200,000 px/s) and the finding that opening a document costs ~1.4 KiB a page (`docs/backlog.md` item 4).
- `docs/plans/0002-continuous-multipage-scroll.md`: Phase 0 done and at its gate - verified findings (Slint's `f32` coordinates and the `Flickable`'s own scroll animations, what the single-page app does that continuous scroll cannot keep, page bounds already carrying rotation and crop box), a design (an exact integer layout in `core`, the document position in Rust over a re-anchored canvas window, tiled pages vs single page images for small pages, a display-list LRU, epochs across pages, a page-relative zoom anchor), phases with acceptance rows, and five questions for the approver. No feature code.
- `crates/engine-mupdf/tests/page_sizes_bench.rs` (ignored, not in CI) and `docs/benchmarks.md` §8: reading every page size at open costs 7-13 us a page on generated 1,000-20,000-page documents (8-13 ms per 1,000 pages), and skipping the page load saves at most ~20%; the metrics the Plan 0002 runs will report are defined in §8.2 before any run.
- Plan 0001 is closed through Phase 2; its Phase 3 (ctrl+wheel/pinch zoom, stretched second tile level) is deferred rather than cancelled, and Plan 0002 no longer waits on it.
- `deny.toml` ignores RUSTSEC-2026-0370/0371/0372 (`zvariant`, `zvariant_derive`, `zbus_names` deprecated in favour of zbus 6), which reach the Linux build only through slint 1.18.1's zbus 5 dependency; slint 1.18.1 is the latest release, so there is no upgrade to take yet.
- `CLAUDE.md`: agent guidance for this repository, REUSE-annotated.
- Tiles, the base layer and `PdfEngine::render_page` are rasterized **alpha-free - RGB8 on white paper - instead of RGBA8**: `PixelFormat::Rgb8`, a `Bitmap` whose stride is `width * 3`, and `Image::from_rgb8` on the app side, which hands Slint's femtovg renderer the 3-byte buffer as it is. A cached tile drops from 1 MiB to 768 KiB and the UI copy beside it drops with it, so the byte budget now measures what the process actually holds instead of half of it. No visible pixel changes: MuPDF clears an alpha-free pixmap to `0xff`, so the paper is white rather than a transparent hole, and the tests assert that corner per fixture - including a new assertion on `large_200p.pdf` (corner `[255,255,255]`, `data.len() == w * h * 3`) and the A0 @ 6400% acceptance test, which now compares tiles against an independent render on a grid crossing instead of over blank paper (max channel delta 0, 0 differing bytes of 3,145,728).
- The default cache is the **visible tiles plus the one-tile prefetch ring**, which is the row count the viewport already derived; `--cache-mib` / `--cache-rows` still override both. With RGB8 tiles that same row count holds 12 MiB rather than 16, so §7.6's commands raise `--cache-rows` from 80 to 96 to make the 64 MiB budget bind.
- `docs/plans/0001-tiled-viewport-rendering.md`: Phase 2 is **closed** - every item on its list (pool, cancellation, base layer, prefetch ring, the app-boundary check of the byte budget) is implemented and measured, and the RSS half of its acceptance has a measured result. §4.9 states the memory target as one named metric (peak working set, <= 250 MiB in the default configuration, measured 192.7 MiB) and records that the earlier "+50 MiB over the baseline" framing was not met; the sections that quoted RGBA8 sizes, the 3x3 page-centre acceptance block and the byte-identity stride were updated with it. The gate run that replaces §7.5's pre-pool header has since landed, and the next plan is 0002.
- `docs/benchmarks.md` §7.5 is **re-measured on the pooled, RGB8, ring build** (2026-10-06, the same 884 px viewport §7.6 uses), with the pre-pool tables kept beside it as the baseline they replace. Settled zoom steps and six-step bursts land in 1-3 ms `t_blank`; the whole-viewport wait (`t_full`) falls from a pre-pool worst of 22 ms to 9 ms on the A0; panning reports **0 hole frames** at 3900 and 24700 px/s where the pre-ring build reported 3, 9 and 13; `abort_ms` never exceeds 2 ms on any run that sampled one; five repeats of the A0 burst all pass. One row still FAILs and is reported as such: a zoom burst fired 800 ms into the document's own first render takes 714 ms for the new level's first tile - inside the pre-pool 124-929 ms envelope - because the pool parallelizes tile rasters, not the display-list build that this wait is spent on. §7.5 and the plan say so, and the stretched-level item that covers it is Phase 3's.
- Maximum zoom raised from 1000% to 6400%, with memory bounded by the tile cache instead of the whole-page raster (an A0 page at 6400% previously required a 125 GiB buffer, which cannot be allocated).
- `crates/app` re-rasterizes only the tiles affected by a pan or zoom, instead of the entire page.
- Project named **PODEFA**, developed by FINE Association, with repository metadata and contributor contacts filled in.
- `docs/benchmarks.md` extended with Phase 1 before/after measurements (an A0 page at 400% drops from 490 MiB and 541 ms per zoom step to about 6 MiB of tiles and 5 ms).
- `docs/benchmarks.md` §7.5 adds interactive zoom/pan measurements from the new harness: the A0 rapid-zoom burst costs 2-14 ms per step once the document has rendered, and the 929 ms stall reported by the first revision was the document's own first render (§7.2-7.3's one-off) sampled while it was still in flight, not stale tiles queued ahead of the epoch `Cancel`. The harness now reports per-phase `window` lines and keeps the startup render out of the `VERDICT`. Those measurements were the pre-pool baseline, which the Phase 2 gate run has since re-measured in place (§7.5).
- `crates/engine-mupdf`: the pool decides whether a finished raster may be published inside the registry lock that cancellation takes, in the same critical section the epoch flag is set in, so "a cancelled epoch publishes nothing" follows from that lock instead of from how quickly a flag propagates between cores; the flag read before a raster starts is now only a way to skip work that would be discarded. Abort latency is documented (and measured) as bounded by the largest display-list node rather than by a fixed ceiling, since MuPDF tests the cookie at node boundaries.

### Removed
- `crates/render`: empty placeholder crate; its responsibilities live in `crates/core` (`tiling`, `scheduler`).
