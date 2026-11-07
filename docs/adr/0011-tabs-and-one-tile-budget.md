<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# ADR 0011: Tabs - One Engine per Document, One Render Pool and One Tile Budget per Window

## Status

Accepted (Plan 0007 Phase 4, 2026-10-25; proposed in the plan's §3.4). Amends ADR 0003: one engine
actor still owns one document, but a window now has several, and the render pool is no longer
the actor's own.

## Context

Tabs (Plan 0007 Phase 4) put several open documents in one window. Until now one engine actor
owned the window's only document and started its own render pool (`clamp(cores - 1, 1, 4)`
workers, each with a tile-sized raster buffer), its search thread and its tool thread, and the
app had one tile cache with a 64 MiB budget (Plan 0001 §4.9). Copied per tab, ten tabs would mean
up to forty raster workers and ten budgets, against the resource priority (`peak_ws` <= 250 MiB in
the default configuration, `docs/benchmarks.md` §7.6).

## Decision

1. **One engine actor per tab.** Each tab's actor owns its MuPDF document, its search thread and
   its tool thread, as ADR 0003 and ADR 0005 describe, so a slow document (a long search, a tool
   job, a heavy page) never holds up another tab.
2. **One render pool for the window.** `engine_mupdf::RasterPool` is started once and shared by
   every engine (`EngineHandle::spawn_with`). A queued job carries the event channel of the engine
   that queued it, so its pixels go back to that document. It is safe because every MuPDF context
   in the process is a clone of one base context (`patches/mupdf/src/context.rs:66-80`): a display
   list one engine built can be replayed on any worker. Epochs come from one counter for the
   window, so a `Cancel` names one tab's work.
3. **One tile budget: only the shown tab draws.** The active tab's state is the app's state, as
   before; the other tabs are parked. Parking releases everything drawn for the tab - its tiles,
   base layers, thumbnails, text-line boxes and overlays - and sends `EngineCmd::Trim`, which drops
   the engine's display lists and raster buffer. The tile cache, the window's models and the
   settings belong to the window and go to whichever tab is shown. A parked tab keeps its document,
   page sizes, scroll position, zoom, outline and undo history, and draws again when shown. This
   is the plan's "other tabs' tiles are the first to go" taken to its end: they go at once, so the
   budget is one budget whatever the number of tabs.
4. **A parked tab's events wait.** Every engine's events are forwarded to the UI thread with its
   tab's id. The shown tab's are applied at once; a parked tab's are queued, pixels excepted (it
   asks for them again when shown), and applied in order when it is shown. A tool job that a tab
   started finishes on that tab even if another is shown meanwhile.
5. **Pages between tabs go through a file.** Dragging pages onto a tab copies them to the end of
   its document, dragging them onto the new-tab button opens them as a new document, and Split in
   the gallery moves the pages after a gap into a new tab. Each writes the pages to a file of the
   app's in the temporary folder on the source's tool thread (the existing Extract job, which
   reads the document as it is now, unsaved edits included), then inserts that file into the
   target (the existing Insert from File, one undo step) or opens it in a new tab. A document made
   this way has no file of its own until its first Save As; the app deletes its files when the tab
   closes or the app ends.

## Consequences

- A tab shown again redraws its view from nothing: its first view costs what an open's first view
  costs after the document is read (row AU reports it as `t_switch`).
- Each tab still costs a document's own memory - MuPDF's document and its object cache, the search
  thread's second copy of the document (ADR 0005), page sizes and outline - and three threads
  (actor, search, tools). Row AU measures it.
- MuPDF's resource store (fonts, images) is shared by the process's contexts; a parked tab's
  entries stay until the store evicts them. Not trimmed per tab.
- `Opened` on a parked tab, a long tool job or a save finishing in the background are applied when
  that tab is shown; its tab keeps its name and unsaved mark meanwhile.
- Opening a file no longer asks to save the shown document: it opens in a tab of its own, or in
  the shown tab when that one is empty, or switches to the tab that has it.
