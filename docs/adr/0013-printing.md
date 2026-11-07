<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# ADR 0013: Printing - Pixels to Windows, a PDF to CUPS and Android, and the Permissions Held by the Engine

## Status

Accepted (2026-11-07, on the maintainer's word: "yazdırmayı hallet"; `docs/backlog.md` item 32).

## Context

PODEFA had no Print command, so a document's `print` and `print_hq` permissions followed nothing.
The three families of system print differently:

- **Windows** has no call that prints a PDF. A program draws on a printer's device context with
  GDI. Handing the file to the application registered for PDFs would print through another
  program, or not at all.
- **Linux and macOS** have CUPS, which takes a PDF through `lp` and converts it for the printer.
- **Android** has `PrintManager`, whose own dialog asks for the printer, pages and copies, and
  which reads a PDF through a `PrintDocumentAdapter`.

ISO 32000-1 table 22 says a document without bit 12 (`print_hq`) may be printed only "to a
representation from which a faithful digital copy of the PDF content could not be generated". A
PDF of the pages themselves, given to a queue, is exactly such a copy. The standard gives no
number for low resolution.

A printer can be finer than anything else the engine draws for: an A4 sheet at 600 dpi is
4960 x 7016 pixels, 104 MiB of RGB8, and a plotter's sheet is many times that.

## Decision

1. **A print job is a tool job**: `ToolJob::Print { pages, target, low_res }`, on the tool thread,
   over a snapshot of the open document as it is now. Progress, cancelling, the refusal by
   permission and the status bar come with it, and rendering is not held up.
2. **Two targets** (`pdf_core::print::PrintTarget`):
   - `Device(SharedSink)`: the engine draws and a `PrintSink` takes the pixels. On Windows,
     `platform::print_win::GdiSink` over `CreateDCW`, `StartDocW`, `StretchDIBits`, `EndDoc` or
     `AbortDoc`.
   - `File(path)`: a PDF for `lp` or `PrintManager`, in the temporary folder, deleted once the
     system has it. The pages themselves, with their annotations, through the same copy as Extract
     and garbage collected, so a redacted document's removed content does not go out with it.
3. **The engine holds the permissions**, whatever the app asks: `Need::Print` is refused like any
   other need, and without `print_hq` the actor sets `low_res`. With `low_res`, a device gets a
   raster of at most `LOW_RES_DPI` (150, the value Acrobat uses) stretched to the sheet, and a file
   holds images of the pages at that resolution, with no text. The dialog says so beforehand.
4. **Pages are drawn in bands** of at most `BAND_BYTES` (32 MiB), from the page's display list,
   each with the bleed rows of a tile so a clip's anti-aliased edge does not show where two bands
   meet. A page is drawn at no more than `MAX_DPI` (600); a finer printer gets it stretched.
5. **Placement** is pure, in `pdf_core::print`: at its own size when it fits, shrunk to fit the
   printable area when it does not, never enlarged; turned a quarter when it lies the other way;
   centred on the paper. Scaling choices and n-up are not part of this.
6. **Copies:** on Windows the page list is repeated, so copies are collated whatever the driver
   does; `lp` gets `-n`; Android's dialog asks.
7. **The dialog** is the app's own on the desktop (printer, pages, copies, the driver's Properties
   on Windows), and none on Android, whose system dialog asks. `windows-sys` becomes a direct
   dependency for Windows, at the version already locked.

## Consequences

- Printing a large document on Windows sends bitmaps, as SumatraPDF does: spool files are larger
  than a driver's vector commands would be, and a printer's own fonts are not used. In exchange
  every page looks as it does on screen, without a PDF-to-GDI translator.
- Printing draws annotations as the screen does (MuPDF's "View" usage): one marked Print-only does
  not print, and one marked no-print does. `docs/backlog.md` item 32 keeps this open.
- Only PDFs print, like every other tool; `state.pdf_editable` is the same gate.
- The job's sink lives in a `Mutex` behind an `Arc` so a `ToolJob` can stay `Clone + Eq`; jobs
  compare sinks by identity.
