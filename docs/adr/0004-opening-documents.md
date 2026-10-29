<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# ADR 0004: Opening Documents - File Dialog, Drag-and-Drop and Settings

## Status

Accepted (Plan 0003 Phase 0 gate, 2026-10-20)

## Context

v0.1 has to open documents the way a desktop application does: a menu and Ctrl+O with the
platform's file dialog, dropping a file on the window, and a list of recent files that survives a
restart. Slint 1.18.1, the UI toolkit (ADR 0001), provides none of the three:

- it ships no file dialog;
- its `DropArea` has a stable API for file paths (`DataTransfer::file_paths()`), but the winit
  backend this build uses never delivers a drop from another application, and winit 0.30 has no
  file drops at all on Wayland (Plan 0003 §3.2);
- it has no settings storage.

The project's first priority is a small, light application (CLAUDE.md §1), so each answer is
weighed by what it adds to the binary and to the dependency graph.

## Decision

1. **File dialog: `rfd` 0.17** (MIT), default features. It shows the operating system's own dialog
   on Windows and macOS and goes through the XDG desktop portal on Linux. Measured on Windows with
   this workspace's release profile it adds 1,536 bytes (`docs/benchmarks.md` §9.2); its new crates
   are `rfd` and `pollster`, everything else it needs is already in the graph through Slint. The
   dialog is modal and runs on the UI thread, as a modal dialog does.
2. **Drag-and-drop: Slint's stable `DropArea` only.** The app opens the first `.pdf` of a drop
   through `DataTransfer::file_paths()`. On Slint 1.18.1 that handler never fires; it starts working
   with the Slint release that carries slint-ui/slint#13527 (merged into Slint's `feature/winit-0.31`
   branch, built on rust-windowing/winit#4571), on every platform including Wayland, with no code
   change. Rejected: the `unstable-winit-030` hook (pins Slint to `~1.18`, no Wayland) and our own
   per-platform drop targets (four `unsafe` integrations into a window winit owns).
3. **Settings: a plain-text file and no dependency.** `key=value` lines in the platform's per-user
   configuration directory, found from the environment (`%APPDATA%`, `$XDG_CONFIG_HOME` or
   `~/.config`, `~/Library/Application Support`); parsed by `pdf_core::settings`, which never fails
   and falls back to defaults. Written through a temporary file and a rename. Rejected: `serde` with
   a format crate, and `dirs`/`directories`, for the few lines they would replace.

## Consequences

### Positive
- Native dialogs on every platform for about 1.5 KiB on Windows and two new crates.
- Drag-and-drop arrives with an ordinary Slint upgrade, Wayland included, without unstable API.
- Settings add no dependency, and a broken settings file can never stop the viewer from opening.

### Negative
- Until that Slint release, dropping a file on the window does nothing; Ctrl+O, the recent list and
  the operating system's "open with" (the path arrives as the command-line argument) are the ways
  in.
- On a Linux session without the XDG desktop portal the file dialog cannot open. `rfd`'s behaviour
  in that case and its Linux binary size have not been measured yet (no Linux machine; to be done
  in CI).
- The settings format is ours to keep compatible; unknown keys are ignored so that a newer file can
  still be read by an older build.
