<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# Plan 0008: Mobile GUI, Android First

## Status

**Phase 0 passed its gate on 2026-10-26; the interface part of Phase 2 (the mobile shell) is
implemented, shown on the desktop with `--mobile` and on the phone from a throwaway build, and
waits for the maintainer's review** (§5, "Phase 2, interface part"). By the maintainer's order the
shell came before Phase 1's platform work (§0).

- Phase 0: passed its gate on 2026-10-26 (§0). The trial build (§3.3): an 11.3 MiB APK, the first
  page in 239 ms, 0 idle frames, 154-218 MiB PSS; Skia is the only renderer on Android and clashes
  with MuPDF at link time; zoom frames take 30-55 ms, the main risk for Phase 2.
- Phase 2, interface part: implemented (2026-10-26), at a review checkpoint.
- Phase 1 (platform), the rest of Phase 2 (touch: pinch, fling, double tap), Phases 3 to 5: not
  started.

Started 2026-10-26 on the maintainer's request ("Konsepte mobil GUI eklendi. Geliştirme için
başlangıcı yap"). Plan 0007 Phase 4 (tabs) was then at its own gate; §9 q1 asked how the
two plans go together (answered in §0).

The design source is the mobile part of the maintainer's TypeScript concept,
`podefa-ui-concept` (`src/components/mobile/`, `DESIGN.md` step 38). As for Plan 0007, it lives
outside this repository and is not a dependency: this plan ports its decisions to Slint, not its
code.

## 0. Gate record

**The §9 answers, 2026-10-26** ("§9'daki önerilen cevaplar tamam; mini PC'deki indirmeleri
onaylıyorum, deneme derlemesine başla"): every recommended answer stands.

1. Plan 0007 Phase 4 passes its gate; this plan's Phases 0 and 1 run alongside Plan 0007 Phase 5,
   and this plan's Phase 2 starts once Plan 0007 closes.
2. The downloads on the mini PC are approved; the trial build goes ahead.
3. One app, two shells, with `--mobile` on the desktop.
4. Android 8.0 (API 26) and later.
5. Landscape in this plan, with the phone layout (Phase 5).
6. One column of cards in Tools.
7. Rows MA-MC's figures are starting points, fixed after the trial build.

**Phase 0 passed its gate on 2026-10-26** ("Faz 0 onaylandı. Önce mobil kabuğu (Faz 2'nin arayüz
kısmı) --mobile ile masaüstünde ve telefonda göster, platform işleri (Faz 1) sonra gelsin"). The
proposed figures of rows MA-MC stand. The order changes: the interface part of Phase 2 (the shell's
islands, cards, menus and sheet, `--mobile`) comes first, shown on the phone from a throwaway build
like the trial's; then Phase 1; then the rest of Phase 2 (pinch, fling, double tap, its rows).

**While the shell was under way** the maintainer found the dock's side parts too large: the
concept keeps the pager (at most 92 px) and the zoom smaller than the modes in the middle. They are
now the concept's size, 36 px tall, with touch areas the dock's full 56 px height.

## 1. Goal and scope

The app runs on Android phones with the concept's mobile interface: the same document engine,
the same three modes (Read, Edit, Tools), the same tokens and themes as the desktop, laid out for
a thumb on a touch screen. Android first (the maintainer's decision of 2026-10-21); iOS after it,
in a plan of its own (it needs a Mac, and the App Store question of CLAUDE.md §1 is still open).

In scope:

- Building, packaging and running the app on Android: an APK the maintainer can install, the
  build scripted, a CI job.
- The platform layer Android needs: an entry point, files that arrive as `content://` URIs and
  are saved back through the system, the settings' folder, the clipboard, no desktop file dialog.
- The concept's mobile shell (§4): header island, bottom thumb dock, the edit strip, a vertical
  gallery, the bottom-sheet drawer, the search bar; safe areas and the on-screen keyboard.
- Touch input everywhere: one-finger pan and fling, pinch zoom, double tap, long press; ink and
  notes with a finger; form fields with the on-screen keyboard.
- Portrait and landscape on phones.

Out of scope, not designed out: iOS; tablet-specific layouts (a tablet gets the phone shell until
then); stylus pressure; Android's "Open with" and share targets beyond opening a PDF (Phase 5
looks at them); store releases.

## 2. What exists

- **The libraries run on a phone.** `pdf-core` and `engine-mupdf` build for
  `aarch64-linux-android` on the maintainer's mini PC (NDK r28c, API 26) and their tests pass on a
  Samsung Galaxy A34 (Android 16), most recently 204 of 204 at `5abbb83` (`docs/backlog.md` item
  9, which records the environment the build needs).
- **The tile budget already knows mobile:** 16 MiB on Android and iOS
  (`pdf_core::scheduler::default_cache_bytes`, `crates/core/src/scheduler.rs:26-32`).
- **Touch selection by long press** exists (ADR 0006).
- **The app crate is desktop-only** (`docs/backlog.md` item 9, read from the pinned crates):
  - it is a `bin` with `main()`; Slint on Android needs an `android_main` in a `cdylib`;
  - `rfd` 0.17.2 has no Android backend, and `arboard` 3.6.1 excludes Android;
  - the settings folder comes from desktop environment variables (`settings_path`,
    `crates/app/src/main.rs:3713-3721`), so on Android it would be `None` and nothing would be
    remembered;
  - the desktop shell is built for a mouse: hover bars, tooltips, small hit targets, a frameless
    window with its own controls.
- **The test binaries need the C++ runtime linked by hand** (`-lc++_static -lc++abi` through an
  environment variable; backlog item 9 "To do in the mobile plan").

## 3. Phase 0 findings (verified in the pinned sources)

### 3.1 Slint 1.18.1 on Android

- **Backend and entry point:** the `backend-android-activity-06` feature of `slint`, an
  `android_main(app: slint::android::AndroidApp)` marked `#[unsafe(no_mangle)]` in a `cdylib`, and
  `slint::android::init(app)` before the window is built (`slint-1.18.1/android.rs:5-45`,
  `Cargo.toml` features at lines 65-69). The backend crate is
  `i-slint-backend-android-activity` 1.18.1 (`slint-1.18.1/Cargo.toml`, the
  `cfg(target_os = "android")` dependency); the trial build fetched it (renderer below, touch in
  §3.3).
- **Build tools:** Slint's documentation asks for the Android SDK (`ANDROID_HOME`), the NDK
  (`ANDROID_NDK_ROOT`) and `javac` (`JAVA_HOME`), and suggests `cargo-apk` or `xbuild` to make the
  APK (`android.rs:52-80`). The mini PC has the NDK only.
- **Renderer: Skia, and only Skia.** The femtovg renderer the desktop uses is a dependency on
  every platform except Android (`slint-1.18.1/Cargo.toml`,
  `[target.'cfg(not(target_os = "android"))'.dependencies.i-slint-renderer-femtovg]`), and the
  Android backend depends on `i-slint-renderer-skia` alone
  (`i-slint-backend-android-activity-1.18.1/Cargo.toml:93`, `lib.rs:33`). Skia draws shadows, so
  the islands can look as on the desktop. Its Rust bindings (`skia-bindings` 0.153.3) download
  prebuilt Skia binaries from rust-skia's GitHub releases while building (the trial's build log:
  `skia-binaries-...-aarch64-linux-android-ganesh-gl-jpegd-jpege-pdf-vulkan.tar.gz`), so an
  offline or CI build needs that file cached (`SKIA_BINARIES_URL`).
- **Skia and MuPDF clash at link time.** The prebuilt Skia carries its own FreeType and
  libjpeg-turbo, and MuPDF carries its own FreeType and libjpeg: 432 global symbols are defined in
  both (`FT_*`, `ft_*`, `tt_*`, `t1_*`, `cff_*`, `ps_*`, `jpeg_*`, `jinit_*`, ...), and the first
  link failed (`ld.lld: error: duplicate symbol: jpeg_finish_output`). The two copies are different
  versions with different configurations, so one cannot simply serve both. The trial renamed every
  global symbol MuPDF's third-party archive defines (except `fz_`/`pdf_`) with a prefix, in both
  MuPDF archives, with `llvm-objcopy --redefine-syms` after MuPDF's `make` (a patched copy of
  `mupdf-sys` 0.8.0's `make.rs`); the link then succeeded and the app ran. Phase 1 makes this
  permanent (§4, "Linking").
- **Pinch and rotation from touch:** Slint's core recognizes two-finger gestures itself and turns
  them into the same `PinchGesture` and `RotationGesture` events a trackpad sends; one finger is
  forwarded as mouse events (`i-slint-core-1.18.1/input.rs:2123-2160`, thresholds 8 px and 5
  degrees). The `ScaleRotateGestureHandler` element exposes the cumulative `scale`, `center` and
  `started`/`updated`/`ended`/`cancelled` (`items/input_items.rs:1107-1135`), and
  `SwipeGestureHandler` exists for swipes (line 840). So pinch zoom needs no platform code: it
  drives the zoom burst Plan 0001 already has (scale tiles on the GPU, render after the debounce).
- **Safe areas and the on-screen keyboard:** the window has `safe-area-insets`,
  `virtual-keyboard-position` and `virtual-keyboard-size` (`i-slint-core-1.18.1/items.rs:1263-1265`),
  updated when the platform reports them (`window.rs:309-311, 766-768`). The header and the dock
  stay inside the safe area; a field being edited scrolls above the keyboard.
- **Not in Slint:** backdrop blur (as on desktop, Plan 0007 §3.2): the concept's frosted glass is
  the same 92% opaque island the desktop uses.

### 3.2 Files on Android

Android hands the app a `content://` URI (the system picker, "Open with"), not a path, and MuPDF,
the search thread and the in-place incremental save (ADR 0007) all work on paths. Proposed, to be
recorded as an ADR in Phase 1:

- Opening: copy the document from the URI into the app's cache folder and open the copy by path;
  keep the URI with a persisted permission (`takePersistableUriPermission`) for saving.
- Saving: save the copy as today (incremental, in place), then write the whole file back to the
  URI (`ContentResolver.openOutputStream(uri, "wt")`). Save As: the system's create-document
  picker, then the same.
- The pickers and the content resolver are Java APIs: they are reached through JNI from Rust with
  the `AndroidApp` and its activity, on the UI thread. That is the only Java-side code this plan
  needs, apart from what Slint's backend brings.
- Settings live in the app's internal data folder (`AndroidApp::internal_data_path`,
  `android-activity-0.6.1/src/lib.rs:1272`); JNI reaches the activity through `vm_as_ptr` and
  `activity_as_ptr` (lines 810, 848).
- The clipboard goes through Slint on Android instead of `arboard` (the trial replaced `arboard` with a
  stand-in and did not try copying; Phase 1 checks Slint's Android clipboard); `rfd` and `arboard` move behind a desktop
  `cfg`.

### 3.3 The trial build (2026-10-26)

Today's desktop window, unchanged, behind a minimal `android_main` on the throwaway branch
`trial/android-0` (not merged): `rfd` and `arboard` replaced by stand-ins that do nothing, the
app built as a `cdylib` too, the arguments read from a file in the app's data folder and stdout
written to one (Android sends stdout nowhere). Built on the mini PC with cargo-apk 0.10.0 (the last
release, 2023-11-30; `xbuild` is marked unmaintained), the Android SDK's platform 35 and build
tools 35.0.1, NDK r28c and JDK 21; signed with a throwaway debug key; installed with `adb` on the
Samsung Galaxy A34 (Android 16, 1080 x 2340, 450 dpi). The build after the rename change took
7.7 min on the mini PC. Figures, measured as `docs/benchmarks.md` §14 defines them:

| | Measured |
| :--- | :--- |
| APK (arm64-v8a, release) | 11,825,507 bytes; `libapp.so` inside it 22,378,688 bytes uncompressed |
| Cold launch (`am start -W`, `TotalTime`) | 436, 467 ms with a document (740 ms on the very first launch, without one) |
| `t_first_page` (from `run()` to the first view covered, `large_200p.pdf`) | 239 ms |
| Idle frames over 5 s | 0 (`--bench-idle 5`, PASS) |
| Memory, `large_200p.pdf` open, idle | PSS 154 MiB (RSS 245 MiB) |
| Memory after one swipe | PSS 175 MiB |
| `dense_vector.pdf` zoom burst (`--bench-zoom 6 --bench-repeat 3`) | `t_blank` 48, 51, 46 ms (the 50 ms ceiling missed by 1 ms once); `t_full` 131, 98, 98 ms; `t_abort` 31-47 ms; peak PSS 201 MiB |
| Frames during that zoom burst | `frame_max` 39-55 ms; the first window's 136 ms |
| A0 sweep (§7.6 run A) | PASS, `t_blank` 49 ms; peak PSS 218 MiB (sampled once a second); zoom window `frame_max` 38.5 ms |

What it showed:

- **It works:** the window opens, the first page draws in 239 ms, a one-finger swipe scrolls the
  canvas (to page 2), nothing draws while idle.
- **Frames are the problem:** frames of a zoom burst took 30-55 ms on the phone: about 20-30
  frames a second while zooming, at 1080 x 2340 with Skia's GL backend. Row ME (16.7 ms) is not
  met by today's window; Phase 2 has to find where the time goes (the full-screen window at
  device resolution, the number of tile images, Skia's image upload) before the mobile shell adds
  layers.
- **Memory:** PSS 154-218 MiB; RSS is higher because it counts shared system libraries in full.
- **Pinch** reaches nothing yet: the canvas has no pinch handler (Plan 0001 Phase 3 deferred it),
  so it was not tried; Phase 2 adds it. `adb` cannot inject two fingers, so the pinch rows need
  the maintainer's hand.
- **The desktop window on a phone:** the title row runs under the status bar and off the right
  edge, the status island sits on the gesture bar: the safe area and the width are what the mobile
  shell is for.
- **Exit:** leaving a bench run logs `FORTIFY: pthread_mutex_lock called on a destroyed mutex`
  and an EGL display not terminated: the process ends while threads still hold locks. Android
  kills apps rather than letting them exit, but Phase 1 checks the shutdown path.

### 3.4 The concept's mobile interface

Read from `podefa-ui-concept/src/components/mobile/*.tsx` and `DESIGN.md` step 38:

- **Header island** (`MobileHeader`): brand mark, the document's name with an unsaved dot and
  "page x / y" under it (tap: the open documents), Search, Save, the drawer, and a "more" menu:
  Document Properties, Watermark and Page Numbers, Compress, Export as Images, About.
- **Bottom thumb dock** (`MobileBottomDock`): icons only. Left: the pager `< x / y >` in a capsule at
  most 92 px wide, long numbers moving back and forth inside it; centre: the three modes; right:
  the zoom percentage and Search. Tapping the page or the zoom opens a small card above the dock
  (go to page; zoom, fit width, fit page, continuous scroll). With pages chosen in Tools, the dock
  carries the bulk actions (rotate, duplicate, delete).
- **Edit strip** (`MobileEditorToolbar`): above the dock in Edit, a horizontally scrolling row:
  Highlight, Underline, Strike-out, Ink, Note, Rotate Left and Right, the form fields' shading; a
  colour and width palette opens from the chosen tool.
- **Tools** (`MobileGalleryView`): page cards in one column in the middle of the screen
  (230 x 325 px), each with its own small buttons (rotate either way, duplicate, extract,
  delete), the tick for choosing; between pages, the same pill as the desktop (Split, Blank Page,
  Pages from File) with a close button. `DESIGN.md`'s component map says "2-column grid" for the
  same file while step 38 and the code show one column: §9 q6.
- **Drawer** (`MobileDrawer`): a bottom sheet up to 80% of the height with three tabs: page
  thumbnails, the outline, the open documents (switch, close, open a file, Images to PDF).
- **Search** (`MobileSearchBar`): a floating bar under the header with the count and previous and
  next.
- **Canvas** (`MobileCanvas`): continuous pages; ink with one finger when the Ink tool is chosen,
  a tap places a note, form fields as on the desktop.

## 4. Design

- **One app, two shells.** The engine, `AppState` and every handler stay shared; the window
  differs. `crates/app/ui/mobile/*.slint` holds the mobile shell (`mobile.slint` with the header,
  dock, edit strip, gallery column, drawer, search), built from the same `Theme` global and
  `widgets.slint`. Android builds the mobile shell; the desktop builds the desktop shell and, for
  development, can show the mobile one in a phone-sized window (`--mobile`, the concept's
  `MobileDeviceFrame` idea), so most mobile UI work is tried on the desktop first and checked on
  the phone after.
- **Crate shape.** `crates/app` becomes a library (`app::run(platform)`) with the desktop `main`
  as a thin binary and `android_main` in the same library built as a `cdylib`. `main.rs`'s 5,240
  lines move into modules on the way, without behaviour changes, in their own commits.
- **Touch rules.** Hit targets at least 44 logical px (the concept's dock and buttons respect it;
  Material's figure is 48 dp); nothing only on hover (the desktop's hover bars become always-shown
  buttons or a long press); one finger pans, two pinch; a double tap zooms to fit width and back;
  a long press selects text (ADR 0006) or picks up a page in Tools.
- **Memory and power.** The 16 MiB tile budget; the same "only the shown tab draws" rule
  (ADR 0011); no animation runs while nothing changes (row AS's rule), which on a phone is battery.
- **Linking.** MuPDF's third-party symbols are renamed after its `make` on Android (§3.1), in
  a `[patch]`ed `mupdf-sys` like the `mupdf` one already under `patches/`, with an upstream
  issue or pull request to mupdf-rs for a supported way (a symbol prefix for the bundled
  libraries). The libc++ link moves into the build too (backlog item 9).
- **Bench harness on the phone.** The desktop harness takes command-line flags; on Android they
  come from the launch intent (`adb shell am start ... --es args "..."`), read through JNI, and
  the results go to the log (`logcat`) instead of stdout.

## 5. Phases

| Phase | Content | Gate checks |
| :--- | :--- | :--- |
| 0 | This research and plan; the trial build: today's desktop window as an APK on the phone (needs §9 q2) | §9 answered; the trial's figures recorded |
| 1 | Platform: the app as a library with `android_main`; `rfd`/`arboard` behind a desktop `cfg`; files through `content://` (open, save back, Save As; ADR); settings folder; libc++ linked by the build, not the environment; a build script and an Android CI job; the bench harness on the phone | Rows MA, MB, MC, MD on the phone; every desktop row unchanged |
| 2 | The mobile shell and Read: header, dock, pager and zoom cards, safe areas, pan and fling, pinch zoom, double tap, the drawer (thumbnails, outline, documents), search; `--mobile` on the desktop | Rows ME, MF, MG, MH |
| 3 | Edit on touch: the edit strip and palettes, text markup by long press and drag, ink with a finger, notes, form fields with the on-screen keyboard | Rows MI, MJ; Plan 0004 and 0006 checks on the phone |
| 4 | Tools on touch: the gallery column, card buttons, the gap pill, choosing by tap, moving pages by long press and drag, the bulk actions in the dock; the tools' dialogs as bottom sheets | Plan 0005's page operations on the phone |
| 5 | Landscape, "Open with" for PDFs, the app icon and name, a signed release APK; the maintainer's review in both themes | Row MK; the maintainer's review |

Each phase also runs the library crates' suite on the phone (`docs/backlog.md` item 9) and every
desktop bench, so the desktop does not change by accident.

### Phase 2, interface part: results (2026-10-26)

`crates/app/ui/mobile/shell.slint` (`MobileShell`), placed over the canvas by `main.slint` when
`mobile` is set; the desktop's islands are hidden then and the document's insets follow the
shell's header and dock. On by default on Android; `--mobile` on the desktop opens a 384 x 832
window (the A34's logical size) with the system frame.

- **Header island:** the document's name, a dot for unsaved changes and the page; a tap lists the
  open documents (switch, open a file). Search, Save, the pages sheet and More (theme; Undo, Redo,
  Save As; properties, watermark, page numbers, compress, export as images) at the right.
- **Thumb dock:** the pager (`<  n / N  >`, the number opens a go-to-page card), the three modes,
  the zoom (opens a card with smaller, larger, Fit width, Fit page). The go-to-page card sits over
  the on-screen keyboard (`virtual-keyboard-position`) and opens with the number keyboard.
- **Search bar** under the header, with the count, previous, next and close.
- **Bottom sheet:** Pages (three to a row; Rust asks for the rows in view of the grid,
  `MOBILE_THUMB_ROW_PX`), Contents (the outline), Docs (switch, close, open a file, images to
  PDF). Opening it does not change the desktop's saved sidebar setting.
- **Safe area:** the header starts under the status bar (`safe-area-insets`), checked on the phone.
- On the phone, from `trial/android-1` (the trial branch on top of this commit, not merged):
  everything above draws and answers taps; with `large_200p.pdf` open PSS was 180.5 MiB once
  (one sample, not a bench run). The go-to-page card's text entry could not be checked over `adb`:
  the keyboard had been left on its emoji page, so the typed digits never reached the field.
- **Not in this part:** pinch, fling, double tap (rest of Phase 2); the Edit strip (Phase 3); a
  one-column gallery and the tools' dialogs as sheets (Phase 4: Tools shows the desktop gallery,
  and a dialog opens as the desktop's panel, under the header).
- **Row MH:** every header, card and sheet control is 44 px or more; the dock's side capsules are
  smaller by the maintainer's word, and their touch areas run the dock's 56 px height but the
  pager's arrows are 24 px wide: row MH needs the maintainer's call on them before Phase 2 closes.

### Phase 0 steps

1. Read the concept's mobile interface and Slint's Android support (§3): done.
2. Write this plan: done.
3. The trial build: done (§3.3, `docs/benchmarks.md` §14). Memory after a pinch was not
   measured: the canvas has no pinch handler yet, and `adb` cannot inject two fingers; a swipe
   and the zoom bench stood in for it.

## 6. Acceptance and benchmarks

Rows on the maintainer's phone (Samsung Galaxy A34, Android 16), release build. The figures marked
"proposed" come from the trial build (§3.3) and wait for the maintainer's word at the Phase 0
gate.

| Row | Scenario | Pass |
| :--- | :--- | :--- |
| MA | Size | The release APK (arm64-v8a) at most a proposed 16 MiB (trial: 11.3 MiB) |
| MB | Memory | Peak PSS with `large_200p.pdf`, a pinch to 400% and a fling at most a proposed 250 MiB, the desktop's `peak_ws` ceiling (trial: 154-218 MiB); it follows the tile budget like the desktop's §7.6 |
| MC | Startup | Cold launch (`TotalTime`) plus `t_first_page` at most a proposed 1.0 s (trial: about 0.7 s) |
| MD | Files | A PDF opened from the system picker (also from Google Drive's provider), annotated, saved, reopened in another reader with the annotation; Save As to a new file |
| ME | Pinch | During a pinch 0 tiles rendered until it settles, and frames inside the display's budget (16.7 ms at 60 Hz) on the phone |
| MF | Fling | A 30 s fling through 1000 pages: no holes after settle, memory flat (the Plan 0002 row on the phone) |
| MG | Idle | With nothing moving, no frames for 5 s |
| MH | Touch targets | Every control in the mobile shell at least 44 logical px in both directions |
| MI | Keyboard | A form field being edited is never under the on-screen keyboard |
| MJ | Ink | A stroke follows the finger with no visible lag at 60 Hz, one undo step |
| MK | Landscape | Rotating the phone keeps the page and zoom; header and dock stay in the safe area |

Metric definitions go into `docs/benchmarks.md` §14 before Phase 1 measures them.

## 7. Risks

- **Frame time on the phone.** The trial's zoom frames took 30-55 ms with Skia (§3.3), over row
  ME's 16.7 ms. Phase 2 measures where the time goes and tries fewer, larger tile images, no
  layer over the canvas while it moves, and fewer pixels drawn during the motion. If that is not
  enough, it stops and reports with options; row ME is not relaxed quietly.
- **Two copies of FreeType and libjpeg** stay in the app (Skia's and MuPDF's), renamed apart, and
  a patched `mupdf-sys` stays until upstream offers a prefix (§4, "Linking").
- **Skia's prebuilt binaries** come from GitHub while building: a build without network needs
  them cached.
- **JNI for the file pickers** is new code in a language boundary the project has not had. Kept to
  one module, with the calls checked against Android's documentation, and tested on the phone
  (row MD).
- **Build time and size:** MuPDF's C build takes about 4 minutes on the mini PC each time its build
  script reruns (backlog item 9); the APK carries all of MuPDF (backlog item 11 would shrink it).
- **One app, two shells** could let the mobile shell drift from the desktop's handlers. Mitigated by
  keeping every handler in shared Rust and the shells to layout.

## 8. Deliberate non-goals

- No WebView or Java UI: the interface is Slint on both.
- No separate mobile engine: the engine, its threads and its budgets are the desktop's.
- No blur (not in Slint); no tablet layout in this plan.

## 9. Questions for the approver

1. **Order with Plan 0007:** Plan 0007 Phase 4 is at its gate and Phase 5 (dialogs, password
   dialog, about, reduce motion) is not started. Recommended: pass Phase 4, then run this plan's
   Phases 0 and 1 (platform work, no UI) alongside Plan 0007 Phase 5, and start this plan's
   Phase 2 once Plan 0007 closes, so the mobile dialogs (Phase 4) reuse Plan 0007 Phase 5's
   style. Or: this plan only after Plan 0007 closes.
2. **Downloads for the trial build, on the mini PC** (each needs your OK; `sudo` steps are yours to
   run): the Android SDK command-line tools with one platform and its build tools (from Google,
   about 150-300 MB); a JDK 17 (Debian's `openjdk-17-jdk-headless`, `sudo apt install`); the
   APK tool, `cargo-apk` or `xbuild` (`cargo install`, built from crates.io; I will check which
   one is maintained before choosing); and Slint's Android backend crates (fetched by cargo). Is
   that fine, and should the phone be connected to the mini PC by USB as before?
3. **One app, two shells** (§4), with `--mobile` to run the mobile shell in a phone-sized desktop
   window during development: yes (recommended), or a separate mobile app crate?
4. **Lowest Android version:** API 26, Android 8.0 (what the library build already uses;
   recommended), or higher?
5. **Landscape:** in this plan with the phone layout (recommended, Phase 5), or portrait only?
6. **Tools' layout on a phone:** one column of cards, as the concept's code and `DESIGN.md` step 38
   show (recommended), or the two-column grid `DESIGN.md`'s component map mentions?
7. **The proposed rows MA-MC** (25 MiB APK, 200 MiB peak memory, 1.5 s to the first page): keep
   them as starting points to be fixed after the trial build (recommended), or other figures?
