<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# Plan 0007: GUI redesign (floating islands, three modes, tabs)

## Status

**Phase 2 (own frame and islands) implemented and at its gate - awaiting approval.** Row AV
needs the maintainer's hands (§5, Phase 2 results).

- Phase 0 (research, the design's cost, this plan): passed its gate on 2026-10-25 (§0).
- Phase 1 (`.slint` files, `slint-build`, the `Theme` global): passed its gate on 2026-10-25.
- Phase 2: **implemented, at its gate** (2026-10-25); results in §5 and `docs/benchmarks.md`
  §13.3; ADR 0010.
- Phases 3 to 5: not started.

Started 2026-10-25 on the maintainer's decision to put the GUI before the remaining roadmap
("Gelecek planları erteliyoruz. Sırada GUI güncellemesi var"; v0.5 Secure and later wait). Plan
0006 Phase 3 is still at its own gate; this plan does not depend on it.

The design source is the maintainer's TypeScript concept, `podefa-ui-concept` (React, Tailwind,
Framer Motion), and its `DESIGN.md`. It lives outside this repository and is not a dependency:
this plan ports its decisions to Slint, not its code.

## 0. Gate record

**Phase 0 passed its gate on 2026-10-25** ("Tamam, gerçek GUI oluşturmaya başla"), after the
maintainer had the concept checked against the app's 42 menu commands and placed the missing ones
in the concept (its `DESIGN.md`, step 37). The answers to §9:

1. **Menus:** no menu bar and no menu button with the old menus. Every command lives in the
   island or context it belongs to, as the concept places it: Save As behind an arrow on Save
   (Ctrl+Shift+S); Rotate Left beside every Rotate Right; Reset Field on the field itself (on
   hover); a selected highlight, underline, strike-out or ink stroke shows its colours and Delete;
   Images to PDF in the Tools mode's sidebar; the theme (Light, Dark, System) and About in the
   brand island's menu; Ctrl+W closes the tab. Left out on purpose: Quit (the window's close
   button) and Merge Files without opening them (dragging an open tab into the gallery merges).
2. to 6. The go came with no other answers, so the recommended ones stand: English, every string
   ready for `@tr()`; tabs in this plan (Phase 4); Windows-style controls on Windows and Linux, the
   native frame on macOS; no Snap Layouts flyout for now (`docs/backlog.md` item 19); the theme
   follows the system by default (already so: `pdf_core::settings::Theme::System` is the default).
   The maintainer can still change any of them before Phase 2.

**Phase 1 passed its gate on 2026-10-25** ("Phase 1 onaylandı, Faz 2'ye başla").

## 1. Goal and scope

Replace the current window (a native menu bar, a toolbar, a sidebar, a status line) with the
concept's interface, with no loss of function and within the resource budget (§6):

| Feature | What "done" means |
| :--- | :--- |
| Floating islands | Brand, title bar with tabs, sidebar, editor toolbar, search bar and status bar as separate rounded islands over the canvas; pages flow under them. Windowed: islands inset from the window edge; maximized: islands meet the edge |
| Own window frame | No system title bar: move by dragging the title island, resize from every edge, minimize, maximize and restore, close; Windows snapping by dragging to an edge and Win+arrows |
| Three modes | **Read** (continuous pages, reading tools), **Edit** (markup, notes, ink, forms and page tools, on a dot grid), **Tools** (pages side by side on a "typesetting desk": select several, drag to reorder, insert, split, delete, rotate) |
| Tabs | Several documents in one window; a tab squeezes down to a minimum width; drag pages onto a tab or the + button |
| Themes | Light and dark from one token set (the concept's dark surface ladder, `DESIGN.md` §5); follows the system unless the user picks one |
| Dialogs | The concept's compact desktop dialogs for properties, stamps, page operations, compress, export, form data, password, about; file pickers stay native (`rfd`) |
| Motion | The concept's spring and ease motions, at most 250 ms, never re-rendering tiles while moving, and off with a "reduce motion" setting |

**Not in scope, not designed out:** new PDF functions (the islands carry what the engine does
today), i18n (but strings are ready for it, §9 question 2), touch layout for Android (the islands
are kept touch-sized so a later mobile layout can reuse them).

## 2. What exists and what has to change

- The whole UI is one `slint::slint!` block of about 2,060 lines inside `crates/app/src/main.rs`
  (lines 61-2119), next to about 4,400 lines of Rust in the same file. The redesign starts by
  moving it into `.slint` files compiled by `slint-build` (Phase 1), one file per island and
  dialog, with a `Theme` global for the tokens.
- Menus are a `MenuBar` with File, Edit, Pages, Tools, Form and View (42 menu items). On Windows
  and macOS Slint 1.18.1 shows it as a native menu through `muda`
  (`i-slint-backend-winit-1.18.1/Cargo.toml:247`). A window without a system frame has no place
  for a native Windows menu bar, so the commands need a new home (§9 question 1).
- One engine actor owns one document (ADR 0003). Tabs need several documents (§3.4).
- What stays: the tile pipeline, the base layer, zoom, the canvas window (Plan 0002), the bench
  harness. Every bench in `docs/benchmarks.md` must still pass after each phase.

## 3. Phase 0 findings (verified in Slint 1.18.1's source)

### 3.1 The window frame

- `Window { no-frame: true; }` removes the system frame (`i-slint-core-1.18.1/items.rs:1268`).
- `WindowMoveArea` starts the system's own move loop when dragged past the drag threshold
  (`items/input_items.rs`, the `WindowMoveArea` item, calling `start_window_move`; on winit,
  `winit_window.drag_window()`, `i-slint-backend-winit-1.18.1/winitwindowadapter.rs:2038`).
  Because it is the system's loop, dragging to a screen edge snaps on Windows; to be confirmed in
  Phase 2 on the maintainer's machine.
- `resize-border-width` on a frameless window lets every edge resize it
  (`items.rs:1269`; the winit backend sets the cursor and calls `drag_resize_window`,
  `drag_resize_window.rs:8-31`).
- **Not available:** Windows 11's Snap Layouts flyout over the maximize button. It needs the
  window procedure to answer `WM_NCHITTEST` with `HTMAXBUTTON`, which winit does not expose.
  Without it, snapping by drag and Win+arrow still work (§9 question 5).

### 3.2 What the concept uses that Slint does not have

| Concept | Slint 1.18.1 | Port |
| :--- | :--- | :--- |
| Backdrop blur (`backdrop-filter`) on two surfaces | None (no match in `i-slint-core` or the renderers) | Opaque surfaces. The concept already made its islands 97% opaque; the two blurred surfaces become flat colours |
| Spring motion (Framer Motion) | `easing: spring(bounce)` with the animation's `duration` (`animations.rs:170-172`, `i-slint-compiler-1.18.1/lookup.rs:860`) | Direct |
| CSS dot and line grids | `Image` with `horizontal-tiling` / `vertical-tiling` (`items/image.rs:207-208`) | A small tiled image, not thousands of drawn dots |
| Box shadows | `drop-shadow-*` on `Rectangle`; femtovg caches them (`BoxShadowCache`, `i-slint-renderer-femtovg-1.18.1/itemrenderer.rs:12,400`). The software renderer draws none (`i-slint-renderer-software-1.18.1/lib.rs:3065`, "TODO") | Islands must stand apart without a shadow too: the token ladder does that (dark islands one step lighter than the canvas) |
| 0.5 px hairlines | Logical lengths | 1 physical pixel: half a pixel on a 1x display is a blurred grey line |
| Layout animations when pages are inserted | `animate x, width` on repeated items | Direct, with stable keys per page |
| Drag and drop between tabs | `DragArea` / `DropArea` (`items/drag_n_drop.rs:109, 291`) | Inside one window only; dropping from the system is still slint-ui/slint#13527 |

The default renderer is femtovg (OpenGL), with the software renderer as the fallback (`slint`
1.18.1 default features: `renderer-femtovg`, `renderer-software`).

### 3.3 The design's cost, and the rules that keep it inside the budget

Nothing in the design needs more memory or CPU per page. The cost is in four places:

1. **Motion.** A frame during an animation repaints the dirty region; tiles are textures, so the
   GPU only composites them. Rule: no tile is rendered while something moves. A mode change
   (Read to Tools, the concept's "camera pulls back") scales the tiles and thumbnails already
   there, as the zoom burst does (Plan 0001), and the crisp ones come after it settles.
2. **Idle.** An endless animation keeps the event loop drawing. Rule: nothing moves while the user
   does nothing. The tab title's marquee runs only under the pointer.
3. **Surfaces.** No blur; shadows only where the renderer caches them; grids as tiled images.
4. **Tabs.** The 64 MiB tile budget must stay one budget for the window, not one per tab (§3.4).

### 3.4 Tabs and memory

Today's budget (`docs/benchmarks.md` §7.6, Plan 0001 §4.9): `peak_ws` <= 250 MiB in the
default configuration, measured 192.7 MiB, of which about 70 MiB is fixed overhead.

A tab is an open document: its MuPDF `Document`, its outline and page sizes. Proposed:

- one engine thread per tab, so a slow document never blocks another, with the render pool
  shared;
- **one tile cache and budget for the window**, keyed by tab as well as page, with the visible
  tab's tiles pinned and other tabs' tiles the first to go;
- a tab out of view drops its display lists and its thumbnails beyond the visible strip;
- measured: the working set with 1, 4 and 10 tabs of `large_200p.pdf` (row AU).

This is an architectural change to ADR 0003, recorded as a new ADR in Phase 4.

### 3.5 Baseline

At `871706c`, release build, Windows (the §1 machine): `app.exe` is 18,919,936 bytes. Startup
and idle CPU are measured in Phase 1 before anything changes, with the commands of
`docs/benchmarks.md` §13.

## 4. Design

- **Files:** `crates/app/ui/` with `theme.slint` (a `Theme` global: the surface ladder, text,
  lines, the active fill, radii, spacing, motion durations, for light and dark), `window.slint`,
  one file per island (`title.slint`, `sidebar.slint`, `statusbar.slint`, `search.slint`,
  `editor_toolbar.slint`), `canvas.slint`, `gallery.slint`, `dialogs/*.slint`. Rust callbacks keep
  their names where they can.
- **Tokens** (from `DESIGN.md` §5): light - canvas `#eceff3`, islands white; dark - shell
  `#141416`, canvas `#1c1c1f`, islands `#2b2b30`, strong lines and hover `#36363b`, active
  control `#4a4a51`. Text and accents are the concept's neutral zinc scale.
- **Modes** are a state of the window, not separate windows. Read and Edit share the canvas;
  Tools is the gallery, built on the thumbnail renderer of Plan 0003 and the page operations of
  Plan 0005 (each drag, insert or delete is the same undoable `EditOp::Pages` as today).
- **Settings:** theme (system, light, dark), reduce motion, island inset; in `settings.rs`.

## 5. Phases

| Phase | Content | Gate checks |
| :--- | :--- | :--- |
| 0 | This research and plan (passed, §0) | §9 answered |
| 1 | `.slint` files and `slint-build`; the `Theme` global with both themes, applied to today's layout; baseline and after: size, startup, idle CPU (passed) | Every bench as before; rows AP, AQ, AR, AS |
| 2 | Own frame and islands: no system frame, move, resize, window controls, windowed and maximized insets, brand, title, sidebar, status bar and search islands; the menu's new home (§9 q1) | Row AV; every command still reachable by mouse and keyboard |
| 3 | The three modes: Edit's toolbar, colour palettes and dot grid; Tools' gallery with multi-select, drag to reorder, dividers that insert and split; the mode transitions | Row AT; Plan 0005's page operations still pass |
| 4 | Tabs: several documents, one tile budget (ADR), squeezing tabs, close, drag pages to a tab or to + | Row AU |
| 5 | Dialogs in the new style; the password dialog; about; reduce motion | Rows AW, AX; the maintainer's review of both themes |

Each phase also runs the library crates' suite on the Android phone (`docs/backlog.md` item 9);
the UI itself is not ported to Android in this plan.

### Phase 1 results (2026-10-25)

- The markup left `main.rs`: `crates/app/ui/main.slint` (the window), `types.slint` (the model
  rows), `theme.slint` (the `Theme` global), compiled by `crates/app/build.rs` through
  `slint-build` 1.18.1 and included with `slint::include_modules!()`. Today's window is one
  component; it is split into one file per island in Phase 2, where the islands replace it, rather
  than cut up here and rewritten there.
- `Theme` holds both token sets (§4): shell, canvas, surface, hover, active, lines, text, accent,
  danger, shadow, scroll bar. Every colour the window draws itself comes from it (the 81 uses of
  `Palette` and the fixed chrome colours); colours on the paper (search hits, note and field
  editors) stay as they were, since the page is white in both themes. The std-widgets keep
  following `Palette`.
- "System" leaves `Palette.color-scheme` at `unknown`, and Slint 1.18.1 keeps the effective scheme
  private (`SlintInternal.color-scheme`, `i-slint-compiler-1.18.1/lookup.rs:1096-1101`), so
  `Theme.dark` reads the Fluent style's background, #1C1C1C in its dark scheme
  (`widgets/fluent/styling.slint:18-22, 36`). Checked by screenshots with the setting at light,
  dark and system (Windows in dark mode): canvas #eceff3, #1c1c1f and #1c1c1f.
- The bench gained `--bench-idle S`, the frame counter and `t_first_page` (`docs/benchmarks.md`
  §13.1), measured before the move as the baseline and after it.

| Row | Before (`5e7c805` + the bench change) | After | Pass |
| :--- | :--- | :--- | :--- |
| AP `peak_ws`, default configuration | 162.7 MiB | 162.4 MiB | yes (<= 250) |
| AQ `app.exe` | 18,927,104 bytes | 19,054,080 bytes, +124 KiB (+131 KiB over the §3.5 figure) | yes (<= +1.5 MiB) |
| AR `t_first_page`, median of 5 | `large_200p` 638 ms, `dense_vector` 804 ms | 638 ms, 822 ms (+2.2%) | yes (<= +10%) |
| AS idle frames in 5 s | 0 (10 of 10 runs) | 0 (10 of 10 runs) | yes |

Every earlier bench run since: the `dense_vector` zoom burst (`t_blank` 23/25/29 ms, PASS), the
A0 memory sweep (PASS), `--bench-nav` on the 1000-page file (20 of 20), `--bench-forms` (35 of 35),
`--bench-pages` (14 of 14). `cargo test --workspace` 229 passed; fmt, clippy, deny and reuse clean.
The library crates did not change in this phase; the Android suite was not rerun (no phone was
connected), and stays at 212 of 212 from `871706c`.

### Phase 2 results (2026-10-25)

The window lost its system frame and its menu bar; ADR 0010 records the decision.

- **Files:** `crates/app/ui/widgets.slint` (the island surface, `IconButton`, menu rows),
  `title.slint` (brand and title islands), `sidebar.slint` (the mode island, the mode's commands,
  the tool flyouts and menus, the drawer), `statusbar.slint` (navigation and zoom, the status
  message), `search.slint`; `main.slint` places them over the canvas. 58 Lucide icons as SVG files
  in `crates/app/ui/icons/` (ISC and MIT: `LICENSES/ISC.txt`, `LICENSES/MIT.txt`, `REUSE.toml`).
  Tooltips are Slint 1.18.1's own `Tooltip` element.
- **Frame:** `no-frame` and `resize-border-width: 6px` while not maximized; `WindowMoveArea` under
  the document's name, a double click there maximizes or restores; minimize, maximize/restore and
  close (close asks about unsaved changes, as Quit did). macOS keeps its native frame
  (`own-frame` false; built in CI, not run). The islands sit 12 px from the window's edge, 8 px
  maximized; a 1 px line marks the edge of a window that is not maximized.
- **Where the menu's 42 items went:**

| Menu | Items | Now |
| :--- | :--- | :--- |
| File | Open, Open Recent | Title island: the folder button (Ctrl+O) and the arrow beside it |
| File | Save, Save As, Save Repaired Copy | Title island: Save (Ctrl+S) and its arrow: Save, Save As (Ctrl+Shift+S), Save Repaired Copy when the file was repaired |
| File | Quit (Ctrl+Q) | Removed (§0): the close button, Ctrl+W |
| Edit | Undo, Redo | Title island (Ctrl+Z, Ctrl+Y) |
| Edit | Copy | Ctrl+C on a selection, as before; no button (the concept has none) |
| Edit | Find | Read mode's search button (Ctrl+F); the search island |
| Edit | Delete Annotation | The bar over a selected annotation (with its colours), and Delete |
| Pages | Rotate Left, Rotate Right | Edit and Tools islands |
| Pages | Duplicate, Delete Pages, Move Earlier, Move Later, Insert Blank Page, Insert Pages from File | Tools island: the Pages menu (until Phase 3's gallery) |
| Tools | Merge Files, Split | Tools island |
| Tools | Extract Pages | Tools island: the Pages menu |
| Tools | Watermark, Page Numbers | Tools island: the Stamp menu |
| Tools | Compress, Export as Images, Images to PDF, Document Properties | Tools island (Export as Word beside Images when built with `docx`) |
| Tools | Crop Pages | Edit island |
| Form | Reset Field | A button beside the focused field; the Form menu |
| Form | Reset Form, Import Data, Export Data, Flatten Form | Edit island, when the document has fields: Reset Form, and the Form menu |
| View | Sidebar (F4), Highlight Form Fields | Read mode's thumbnail and outline buttons (F4, Ctrl+B); Edit island |
| View | Theme | The brand island's menu: Light, Dark, System |

  The old toolbar's commands are in the status island (previous and next page, the page field,
  fit width, fit page, 100%, zoom out and in) and the Edit island (Select, the five markup tools,
  Note, Ink; colours and line widths fly out beside the active tool, as in the concept).
- **Keyboard:** a `FocusScope` around the window takes Ctrl+O, Ctrl+S, Ctrl+Shift+S, Ctrl+W,
  Ctrl+F, Ctrl+B, F4 and the zoom keys before the focused element, so they work from the page
  field and the search field as the native menu's did (`KeyCommand::is_global`, with a test).
  Tab reaches every island button, Enter or Space presses it; a menu opened from the keyboard
  focuses its first row (Open Recent and the theme menu: Tab moves into the rows) and Esc closes
  it. Checked by driving the release build: Tab to Open Recent, Enter, Tab into the list; Save's
  menu by keyboard; F4 and Ctrl+W from the page field.
- **Modes:** the mode island switches the sidebar's commands (Read, Edit, Tools); choosing a tool
  by its key (H, U, K, N, I) moves to Edit, and leaving Edit goes back to Select. The canvas, the
  gallery and the transitions are Phase 3.
- **Deliberate differences from the concept, for now:** the canvas starts under the title row and
  beside the sidebar instead of flowing under them (the islands are opaque, §3.2, and a page put
  at the viewport's top would otherwise sit under the title); the drawer opens in every mode,
  because Tools chooses pages in its thumbnails until the gallery comes; About waits for Phase 5's
  dialogs; the mode is not remembered between runs; the thumbnails' hover actions come with the
  gallery. The preferred window size stays 900 x 700: at 1000 x 760 the first view covers more
  tiles and `t_first_page` rose to 733 ms (median, `large_200p`), which measures the window, not
  the design. The minimum height is 660 px, where Edit's island with the form buttons fits.

Row AV, checked by driving the release build on Windows 11 with `SendInput`-style mouse events
(the window's rectangle read back after each step):

| Check | Result |
| :--- | :--- |
| Move by dragging the document's name | moved by the drag (90, 90 px) |
| Resize: right edge, top edge, left edge, bottom-right corner, top-left corner | each changed the rectangle as dragged |
| Maximize by double click and by the button; restore by the button | yes; on the monitor with a taskbar the maximized client area is 1920 x 1032 at (1920, 0), the work area, not over the taskbar |
| Minimize | the window is iconic |
| Ctrl+W, from the canvas and from the page field | the window closes (no unsaved changes) |

Not checked, for the maintainer: the bottom edge and the two remaining corners by hand, snapping
by dragging to a screen edge and Win+arrow, the frame on Linux (X11 and Wayland, GNOME and KDE)
through the mini PC, and the look in both themes.

| Row | Phase 1 | Phase 2 | Pass |
| :--- | :--- | :--- | :--- |
| AP `peak_ws`, default configuration | 162.4 MiB | 162.6, 159.6, 158.6, 165.0 MiB (4 runs; the Phase 1 binary in the same session: 153.9, 166.0) | yes (<= 250) |
| AQ `app.exe` | 19,054,080 bytes | 19,568,128 bytes, +502 KiB (+633 KiB over the §3.5 figure) | yes (<= +1.5 MiB) |
| AR `t_first_page`, median of 5 | `large_200p` 638 ms, `dense_vector` 822 ms | 678 ms (+6.3% over the 638 ms baseline), 848 ms (+5.5% over 804 ms) | yes (<= +10%) |
| AS idle frames in 5 s | 0 (10 of 10 runs) | 0 (10 of 10 runs) | yes |

The window's working set at the start of the A0 run is about 7 MiB higher than Phase 1's (104.5
against 87.0 to 97.6 MiB); its peak is not. Every earlier bench: the A0 memory sweep (PASS),
`--bench-nav` on the 1000-page file (20 of 20), `--bench-forms` (35 of 35), `--bench-pages` (14 of
14; `peak_ws` 290.2 MiB, the Phase 1 binary 309.2 and 393.6 MiB in the same session). The
`dense_vector` zoom burst passed 3 of 4 runs (worst `t_blank` 25, 26, 28 ms); one run's second
repeat took 127 ms, above the 50 ms ceiling, the only time in this session. `cargo test
--workspace` 230 passed; fmt, clippy, deny and reuse clean. The library crates did not change; the
Android suite was not rerun.

## 6. Acceptance and benchmarks

| Row | Scenario | Pass |
| :--- | :--- | :--- |
| AP | Memory | `peak_ws` <= 250 MiB in the default configuration, the Plan 0001 row, after every phase |
| AQ | Size | `app.exe` grows by at most 1.5 MiB over the 18,919,936-byte baseline (fonts and icons included) |
| AR | Startup | Time to the first page shown no more than 10% over the Phase 1 baseline |
| AS | Idle | With nothing moving, the window draws no frames for 5 s (frame counter in the bench) |
| AT | Motion | During a mode change and a zoom burst, 0 tiles rendered until it settles; no frame over 16.7 ms on the §1 machine |
| AU | Tabs | Working set with 10 tabs of `large_200p.pdf` <= the one-tab figure + 10 x (one document's own memory, measured), and `peak_ws` still <= 250 MiB with 4 tabs |
| AV | Frame | Move, resize from all 8 edges and corners, maximize, restore, minimize, snap by drag and Win+arrow on Windows; the same on Linux (X11 and Wayland) through the mini PC (manual) |
| AW | Themes | Text contrast at least 4.5:1 (WCAG AA) on every surface of both themes, checked from the tokens |
| AX | Reduce motion | With the setting on, no animation runs longer than 0 ms |

Metric definitions (frame counter, startup time) go into `docs/benchmarks.md` §13 before Phase 1
measures them.

## 7. Risks

| Risk | Effect | Mitigation |
| :--- | :--- | :--- |
| A frameless window on Wayland | Some compositors refuse client-side moves without a seat serial | `drag_window` is winit's own Wayland path; Phase 2 checks GNOME and KDE |
| No native menu bar | Alt-key access and screen readers lose the menu | §9 question 1; every command keeps a shortcut and stays in an accessible Slint menu |
| Software renderer has no shadows | Islands look flat there | The token ladder separates them without shadows (§3.2) |
| One tile budget across tabs | Switching to a tab shows its base layer before tiles | The base layer is kept per visible tab; measured in row AU |
| The design grows while it is ported | Phases never close | The concept as of 2026-10-25 is the reference; changes to it go to the next phase or the backlog |

## 8. Deliberate non-goals

- No new PDF functions in this plan.
- No web or Electron code: the concept stays a design reference.
- No custom file pickers: `rfd` keeps the system's.

## 9. Questions for the approver

1. **Where do the menus go?** A frameless window has no place for the native Windows menu bar.
   - **A (recommended):** a menu button in the brand island opens the same File, Edit, Pages,
     Tools, Form and View menus as a Slint popup; Alt and the letters open it from the keyboard;
     macOS keeps its global menu bar.
   - **B:** a compact text menu bar inside the title island.
   - **C:** no menus; commands only in the islands, plus a command palette (Ctrl+K).
2. **Language.** The concept is in Turkish, the app in English. A (recommended): English now,
   every string marked with `@tr()` so a Turkish translation lands with i18n; or B: Turkish now.
3. **Tabs** (Phase 4): in this plan (recommended), or later in their own plan?
4. **Window controls per system.** A (recommended): Windows-style controls on Windows and Linux,
   the native frame and controls on macOS; or B: the same Windows-style frame everywhere.
5. **Snap Layouts flyout** on Windows 11 (§3.1): accept without it for now and record it in the
   backlog (recommended), or add a small Windows-only shim for `WM_NCHITTEST` in Phase 2?
6. **Default theme:** follow the system (recommended), or dark?
