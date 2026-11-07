<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# ADR 0010: The Window Draws Its Own Frame; Commands Live in Islands, Not a Menu Bar

## Status

Accepted (Plan 0007 Phase 0 gate, question 1 and 4, 2026-10-25; implemented in Phase 2).
Supersedes the menu bar of ADR 0004's window.

## Context

The redesign (Plan 0007) follows the maintainer's concept: islands floating over the canvas, a
title row with the window's own controls, three modes. A window without the system frame has no
place for a native menu bar on Windows (Slint 1.18.1 shows `MenuBar` through `muda` inside the
system frame), and the concept has no menu bar. The app had 42 menu items, and their shortcuts
worked from any field because the native menu took them first.

## Decision

1. **Own frame, except on macOS.** `Window.no-frame` is bound to an `own-frame` property, true
   unless the build is for macOS (`cfg!(target_os = "macos")`), which keeps its native frame and
   traffic lights. With its own frame the window resizes from every edge
   (`resize-border-width: 6px` while not maximized), moves through `WindowMoveArea` (the system's
   own move loop, so snapping by dragging to an edge is the system's), maximizes or restores on a
   double click there, and draws minimize, maximize/restore and close. Not maximized, a 1 px line
   marks its edge.
2. **No menu bar.** Every command lives where the concept puts it: Open, Open Recent, Save, Save
   As, Save Repaired Copy, Undo and Redo in the title island; the theme in the brand island's menu;
   the mode's commands in the sidebar (Read: thumbnails, outline, find; Edit: markup, notes, ink,
   crop, rotation, the form; Tools: pages, merge, split, stamps, compress, export, images to PDF,
   properties); page navigation and zoom in the status island; find in its own island; a selected
   annotation's colours and Delete beside it; Reset Field beside the focused field. Quit is gone
   (the close button and Ctrl+W ask about unsaved changes as before).
3. **Shortcuts from anywhere.** A `FocusScope` around the whole window sees each key before the
   focused element (`capture-key-pressed`) and hands Open, Save, Save As, Close (Ctrl+W), Find,
   the sidebar (Ctrl+B, F4) and the zoom keys to the viewport's key handler, so they work from a
   text field as the menu's did. Keys a field needs (copy, undo, arrows, Delete, Esc) stay with it.
4. **Keyboard reach.** Every island button is a `FocusScope` that Tab reaches and Enter or Space
   presses, with a focus ring only for keyboard focus; a menu opened from the keyboard puts the
   focus on its first row (in Open Recent and the theme menu, whose rows are repeated, Tab moves
   into them), and Esc closes it. Clicks do not take the keyboard from the canvas.
5. **Icons are Lucide's** (ISC, with Feather's MIT parts), copied as SVG files into
   `crates/app/ui/icons/` and drawn through Slint's SVG support (resvg, already linked), coloured by
   `colorize` from the theme.

## Consequences

- Windows 11's Snap Layouts flyout over the maximize button is missing: it needs `WM_NCHITTEST`
  answers that winit does not let the application give (`docs/backlog.md` item 19). Snapping by
  drag and Win+arrow stay.
- Linux (X11, Wayland) moves and resizes through winit's `drag_window` and `drag_resize_window`;
  some Wayland compositors may refuse a client-side move. Checked by hand (Plan 0007 row AV).
- A command without a shortcut is now one or two clicks away in its island instead of in a menu,
  and Tab order is the islands' order. Menu accelerators with Alt are gone.
- The window keeps its title (`Window.title`) for the taskbar and the window switcher.
