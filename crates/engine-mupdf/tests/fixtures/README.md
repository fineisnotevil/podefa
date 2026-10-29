# Test fixtures

PDFs used by the `engine-mupdf` integration tests and the Phase 0 measurement
harness (`tests/render_bench.rs`).

| File | Page size | Notes |
| :--- | :--- | :--- |
| `minimal.pdf` | 100x100 pt | Hand-written PDF, no font resources, single tiny page. Fastest smoke test. |
| `large_200p.pdf` | 612x792 pt (Letter) x 200 pages | Hand-written, 200 distinct page dictionaries. Used for pagination and RSS-scaling checks. |
| `large_format_a0.pdf` | 2384x3370 pt (A0, 841x1189 mm) | Single page with a 100 pt grid and vector/text content. Exercises the large-format path where a full-page raster buffer is prohibitive: 30.6 MiB at 100%, 490 MiB at 400%, 3.0 GiB at 1000%. |
| `corrupt.pdf` | - | Empty file used to assert open-failure error handling. |
| `dense_vector.pdf` | 2384x3370 pt (A0) | Single page, one `rg` and one nonzero fill (`f`) of 1000 cubics, each sweeping the page's full width with control points 400 pt off the sweep line, so the curves overlap and the page is nearly solid ink. Not meant to be legible: it exists to make one *node* expensive. A tile of it at 6400% is ~24 ms serially on the reference machine against ~1 ms for `large_format_a0.pdf`, which is what the abort-latency test in `crates/engine-mupdf/tests/pool_test.rs` measures - MuPDF checks its cancel cookie at node boundaries, so this is the fixture where a cancel cannot land quickly. |

All fixtures are hand-written PDFs with byte-exact `xref` tables (offsets computed,
not hand-edited), and none is compressed, so no generator script or external tool is
needed to reproduce or modify them: `dense_vector.pdf`'s stream is 1000 repetitions of
the one cubic form its row describes, regenerable from any text editor or a single loop.

