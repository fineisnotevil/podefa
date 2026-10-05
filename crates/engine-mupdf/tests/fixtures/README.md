# Test fixtures

PDFs used by the `engine-mupdf` integration tests and the Phase 0 measurement
harness (`tests/render_bench.rs`).

| File | Page size | Notes |
| :--- | :--- | :--- |
| `minimal.pdf` | 100x100 pt | Hand-written PDF, no font resources, single tiny page. Fastest smoke test. |
| `large_200p.pdf` | 612x792 pt (Letter) x 200 pages | Hand-written, 200 distinct page dictionaries. Used for pagination and RSS-scaling checks. |
| `large_format_a0.pdf` | 2384x3370 pt (A0, 841x1189 mm) | Single page with a 100 pt grid and vector/text content. Exercises the large-format path where a full-page raster buffer is prohibitive: 30.6 MiB at 100%, 490 MiB at 400%, 3.0 GiB at 1000%. |
| `corrupt.pdf` | - | Empty file used to assert open-failure error handling. |

All fixtures are hand-written PDFs with byte-exact `xref` tables (offsets computed,
not hand-edited) so that no generator script or external tool is needed to
reproduce or modify them.

