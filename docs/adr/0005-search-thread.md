<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# ADR 0005: A Search Thread with Its Own Document

## Status

Accepted (Plan 0003 Phase 0 gate, question 4, 2026-10-20). Extends ADR 0003.

## Context

Searching a document means extracting every page's structured text and matching the query in it:
about 1.1 ms of extraction and 0.45 ms of matching a page on generated text documents, so ~1.5 s
for 1,000 pages and ~31 s for 20,000, plus a one-off first touch of over a second
(`docs/benchmarks.md` §9.1). The engine actor (ADR 0003) owns the only `Document` and prepares every
tile the viewport asks for; a search interleaved there would hold every render request behind
seconds of text extraction. Selection (Plan 0003 Phase 4) needs the same text pages.

## Decision

1. **A dedicated `mupdf-search` thread, started by the actor, with a `Document` of its own** opened on
   the same path (and password) the first time a search needs it. MuPDF gives every thread its own
   context; opening the file a second time is the supported way to read it from two threads.
2. **The actor forwards** `EngineCmd::Search { query, from_page, generation }` and `CancelSearch`, and a
   new document on every successful `Open`. Hits come back on the same event channel as
   `EngineEvent::SearchHits { generation, page, hits }`, one event per page with hits, and
   `SearchDone { generation, pages, ms }`.
3. **Pages are searched from the reader's page onwards, wrapping**, so the first hit near the reader
   arrives first and the rest stream in.
4. **Cancellation is a shared generation counter**, stored by the actor *before* it forwards a search
   and compared by the search loop before every page. A superseded or cancelled search stops within
   one page's text extraction, never sends another hit and never sends `SearchDone`. The app keeps a
   last line of defence: `pdf_core::search::SearchResults` refuses hits of any other generation.
5. **Text pages are not kept**: a hit is a few rectangles; the text page holds every character.

## Consequences

### Positive
- Rendering never waits for a search (row L measures `t_blank` during one).
- A new query stops the old one at the next page boundary; tests check that no old hit follows the
  first new one, and fail if the per-page check is removed.

### Negative
- A second object cache: the search document holds what it has parsed, ~1.4 KiB a page once every
  page has been searched (the per-page open cost of Plan 0002 / backlog item 4). Measured as row O.
- Cancellation cannot interrupt one page's extraction (`to_text_page` takes no cookie); a single huge
  page bounds the latency.
- **The two documents diverge once the document can be edited (v0.2):** the search document would not
  see unsaved changes. v0.2 has to reopen it after a save or route search through the actor; this
  ADR is revisited then.
