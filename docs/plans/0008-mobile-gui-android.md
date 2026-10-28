<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# Plan 0008: Mobile GUI, Android First

## Status

**Phase 0 passed its gate on 2026-10-26; the maintainer approved the mobile shell (the interface
part of Phase 2) the same day; Phase 1 (platform) passed its gate on 2026-10-27** (§5, "Phase 1:
results"); Phase 2 passed its gate on 2026-10-27** (§5, "Phase 2, touch"); **Phase 3 (Edit on
touch) passed its gate on 2026-10-27** (§5, "Phase 3: results"); **Phase 4 (Tools on touch) passed
its gate on 2026-10-28** (§5, "Phase 4: results"); **Phase 5 (landscape, "Open with" and
"Share", icon, release signing) passed its gate on 2026-10-29** after the maintainer's review and
its fixes (§5, "Phase 5: results" and "Phase 5: the maintainer's review"). **Plan 0008 is
closed.** By the maintainer's order the shell came before Phase 1's platform work
(§0).

- Phase 0: passed its gate on 2026-10-26 (§0). The trial build (§3.3): an 11.3 MiB APK, the first
  page in 239 ms, 0 idle frames, 154-218 MiB PSS; Skia is the only renderer on Android and clashes
  with MuPDF at link time; zoom frames take 30-55 ms, the main risk for Phase 2.
- Phase 2, interface part: approved on 2026-10-26 (§0).
- Phase 1 (platform): passed its gate on 2026-10-27 (ADR 0012). Rows MA, MB, MC pass on the phone;
  row MD passes for the system picker, saving in place and Save As, with Google Drive and another
  reader left to the maintainer; the desktop rows are unchanged.
- The rest of Phase 2 (touch): pinch and double tap implemented (§5, "Phase 2, touch"); on the
  phone the scripted pinch (row ME), the fling (MF), idle (MG) and the double tap pass, and the
  library suite (215 of 215); a pinch by hand is the maintainer's. Passed its gate on 2026-10-27.
- Phase 3 (Edit on touch): implemented and at its gate (2026-10-27): the edit strip and palettes,
  taps for fields, notes and annotations, a long press to move a note, editors kept above the
  on-screen keyboard; rows MI and MJ pass on the phone; Back closes one level at a time and asks
  about unsaved changes last. Passed its gate on 2026-10-27; the keyboard's composing word
  (backlog item 26) stays open.
- Phase 4 (Tools on touch): passed its gate on 2026-10-28: the one-column gallery with
  each page's buttons and check box, the gaps' Split, blank page and pages from a file, a long
  press that moves pages, the bar over the dock for the chosen pages, and the tools' dialogs as
  sheets; Plan 0005's page operations pass on the phone (`--bench-pages` 13 of 14, the miss being
  a desktop sidebar drag, and every operation by hand).
- Phase 5: passed its gate on 2026-10-29 (the maintainer checked the review's fixes and
  improvements on the phone: "Kontrol ettim. Problem yok."): landscape (row MK passes on the phone), the
  menus and the tools' sheets scroll when a phone on its side leaves them too little room, "Share"
  next to "Open with", the app's icon, a release key of the maintainer's own whose password
  apksigner asks for (`docs/android-release.md`). The review in both themes is the
  maintainer's.

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

**The mobile shell was approved on 2026-10-26** ("Mobil kabuk onaylandı. Sayfa kapsülündeki 24 px
oklar kalsın, Faz 1'e (platform) başla"): the pager's 24 px wide arrows stay, which settles the
open point of row MH (their touch areas run the dock's full height).

**Phase 1, the Java question, 2026-10-26.** The system's pickers answer only through the
activity's `onActivityResult`, so Phase 1 needs Java code, which §3.2 had not foreseen. Asked how,
the maintainer chose the app's own activity class compiled into the APK ("APK'ya kendi dex
(Önerilen)") over a dex loaded at run time; ADR 0012.

**Phase 1 passed its gate on 2026-10-27** ("go"). Google Drive, another reader and "Open with"
stay the maintainer's checks.

**Phase 2 passed its gate on 2026-10-27** ("Telefonda iki parmakla kıstırmayı denedim, sorun
yok. Faz 2 onaylandı, Faz 3'e başla"): the pinch by hand on the phone is the maintainer's check
of row ME.

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
- **Crate shape.** `crates/app` becomes a library (`app::run(args)`) with the desktop `main`
  as a thin binary. `android_main` lives in its own crate, `crates/android` (`libpodefa.so`), so
  the desktop build does not link the app twice (Phase 1, ADR 0012). `main.rs`'s 5,240
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
  come from the launch intent (`adb shell am start ... --es args "..."`), read through JNI. The results
  go to `out.txt` in the app's folder in shared storage, which `adb` reads without the app being
  debuggable (Phase 1; `logcat` would need a writer thread for nothing more).

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

### Phase 1: results (2026-10-27)

ADR 0012 records the design.

- **Crates.** `crates/app` is a library (`app::run`) plus a thin desktop `main`;
  `crates/android` (`podefa-android`, `libpodefa.so`) holds `android_main`, the activity
  `org.fineassociation.podefa.PodefaActivity` (Java), the manifest (`launchMode="singleTask"`,
  "Open with" for `application/pdf`) and the C++ runtime link. It is empty on other targets.
- **`app::platform`.** File dialogs, message boxes, the clipboard, the settings and temporary
  folders and the write-back hook (`written`) behind one API; every dialog answers through a
  callback (15 call sites changed; on the desktop the callback runs before the call returns, so
  the desktop behaves as before). `rfd` and `arboard` are not built for Android; `jni` 0.21 is.
- **Files.** A picked document is copied into `cache/docs/<hash>/<name>` by the activity; saves,
  Save As, tool outputs and exported form data are written back to their URIs; picked folders
  (Split, Export as Images) map to tree URIs. The copy-to-URI map survives restarts
  (`files/documents.txt`).
- **Linking.** `patches/mupdf-sys-android.patch` (the symbol rename of §3.1) is applied to a copy
  of the registry's mupdf-sys under `target/android` and patched in by a Cargo config there, so
  neither the desktop build nor `Cargo.lock` changes. The C++ runtime comes from build scripts
  (`crates/android/build.rs` for the app, `crates/engine-mupdf/build.rs` for its test binaries),
  not from `RUSTFLAGS`; a `rustc-link-lib` line was tried first and failed, since it lands before
  MuPDF's archives on the link line.
- **Build.** `scripts/android-build.sh` (8 m 22 s from a warm `target` on the mini PC, 15 s when
  only Java changed); CI job `android` builds the APK on Ubuntu with NDK r28c and uploads it.
  `cargo clippy -D warnings` passes for `aarch64-linux-android` too.
- **Rows on the phone** (`docs/benchmarks.md` §14.3):
  - **MA PASS:** 12,038,559 bytes (11.5 MiB) against 16 MiB.
  - **MB PASS:** peak PSS 175.0, 173.4, 161.4 MiB in the zoom bursts and pans of `dense_vector`,
    `large_format_a0` and `large_200p` against 250 MiB. A pinch and a fling by hand wait for
    Phase 2, which adds them; the zoom bench stands in, as in the trial.
  - **MC PASS:** `TotalTime` plus `t_first_page` 327, 442, 381 ms against 1.0 s.
  - **MD:** the system picker opened a PDF from Downloads (a copy of `large_200p.pdf`); cancelling
    the picker left the app as it was; an ink stroke and Save fell back to Save As, as on the
    desktop for this file (`NeedsSaveAs`); Save As created the new document through the picker and
    wrote 22,838 bytes to it; a second stroke and Save updated it in place and wrote it back
    (23,529 bytes, 2 ink annotations in the file pulled from the phone). Not checked here: Google
    Drive's provider and another reader (the maintainer's hands), "Open with" from a file
    manager.
- **Desktop rows unchanged:** `dense_vector` zoom bench PASS (worst `t_blank` 32 ms, `peak_ws`
  153.8 MiB), `t_first_page` 649, 637 ms (median before: 638 ms), 0 idle frames; the first run
  after the build was slower (243 ms `t_blank` in its first repeat, 672 ms first page), the
  cold-binary effect seen before.
- **Not done:** the Edit strip is Phase 3, so the ink tool was chosen with the `I` key over
  `adb`; the library suite on the phone (see §5's note) was built (26 test binaries link without
  `RUSTFLAGS`) but not run: the phone left USB before it could be pushed.

### Phase 5: the maintainer's review (2026-10-28)

The maintainer's review on the phone found eight faults and asked for eight improvements. What
was done:

- **Faults.** (1) The dock's mode island is centred on the window, moved aside only when the
  pager or the right group would touch it. (2) A second finger while drawing (a pinch) cancels
  the stroke instead of painting (`ink_blocked`). (3) Text fields: Slint 1.18.1 on Android draws
  the keyboard's composing word as a selection (backlog item 28), so typing and deleting looked
  as if they selected and broke the text; every line field is now the app's `Field`
  (`widgets.slint`), which draws no selection while a word is composed. (4) Edit's divider
  follows Crop. (5) The islands' side padding matches their top (6 px, or 4 px around 44 px
  circles), and the inner radii follow the outer ones (outer minus padding). (6) Tools' bar has
  "Unchoose" next to "Choose all". (7) Export as Images' Resolution has a row of its own; Export
  as Images, Compress, Merge and Images to PDF moved from More to a Tools card (the wrench at the
  dock's right in Tools). (8) After a split, what stays is named like a part ("name_1-N").
- **Improvements.** (1) Only the page in the middle of the column shows its buttons. (2) The
  concept's history capsule under the header's right end: Undo, Save, Save As (Lucide
  `save-pen`), Redo; Save left the header. (3) Book mode on the desktop and the phone (`book.rs`;
  below). (4) Search sits next to the zoom in the dock. (5) Read on the phone hides its islands
  after 4 s without a touch (`CHROME_HIDE_MS`) unless a menu, card, sheet, editor or selection is
  open; a tap brings them back. (6) Held pages follow the finger as a faded stack of up to 3, the
  page under the finger in front. (7) Saved and written PDFs carry "FINE Association | PODEFA
  {version}" as Producer (`pdf_core::tools::PRODUCER`); a save stamps it as part of the last undo
  step through `pdf_begin_implicit_operation` (an `fz_try` wrapper in the journal shim), so it
  adds no step of its own. (8) A tab dragged into the shown document: on the desktop another tab
  dragged onto a gap in Tools' gallery or onto the sidebar's pages; on the phone, in Tools, a
  document held for half a second in the header's documents menu (the menu fades while it is
  held, a page with its name follows the finger) and let go in a gap. The dragged tab's own
  engine writes its document as it is now (unsaved edits included) while it stays parked
  (`AppState::start_tab_tool`), and the shown document inserts it as one undo step.
- **Book mode.** The layout stays the continuous column with 4096 logical px added above the
  first page and below the last, so any page can sit in the middle of the view; the scroll is
  held to the shown page and only that page is drawn. On the phone a finger pans the page and,
  past its edge, pulls it sideways: let go beyond 18% of the width it turns, and the next page
  slides in. On the desktop the wheel and Page Up/Down scroll within the page and turn at its
  ends; Left/Right turn. The button is in the status bar's zoom group and in the phone's zoom card
  (Scroll / Book). It does not clash with Tools' "Choose all": book mode applies to Read and Edit's
  canvas, and Tools keeps its gallery.
- **Checks.** Workspace tests, clippy and fmt pass. Desktop, release build: `--bench-pages`
  14 of 14, `--bench-tabs 3` PASS, `--bench-modes` PASS (worst frame 16.0 ms),
  `--mobile --bench-rotate 2` PASS. The APK builds (12 MiB) and installs on the phone.
- **Checked by the maintainer** on 2026-10-29: no problems found.

### Phase 5: results (2026-10-28)

- **Landscape** (row MK). The activity already took its own orientation changes
  (`config_changes`), so turning the phone resizes the window and nothing restarts; the scroll
  position and the zoom stay, as on a desktop window's resize. What changed:
  - The mobile shell keeps its islands inside the safe area's sides too (`safe-left`,
    `safe-right` from `Window.safe-area-insets`): on the A34 on its side the camera cutout takes
    27 px on the left. The header, the dock, the cards, the menus, the edit strip, the bottom sheet
    and the tools' sheets start at `left` and run `span` (`shell.slint`, `main.slint`).
  - On its side the phone is 891 x 411 px with about 236 px between the header and the dock. The
    header's two menus (documents, More) are capped at the room under the header and scroll
    (`menu-room`); before, More ran off the screen and its last items could not be reached. The
    tools' sheets (Split, Watermark and Page Numbers, Compress and Export as Images, Crop,
    Properties) scroll their rows (a `Flickable` around each, its layout as tall as the sheet or
    its minimum; on the desktop it does not scroll and its dialogs look as before). With the
    on-screen keyboard up, a phone on its side has about 90 px left: when less than 160 px is
    left under the header, the sheet covers the header (the sheets stand over the mobile shell,
    `z`), so the field being typed in stays in sight. Merge keeps its list, which scrolls already.
  - Tools: a page (325 px) is taller than that room. Revealing a page shows its top under the
    column's bar, and the column scrolls through the rest (`reveal_in_column`); before, it centred
    the page, with its top under the header.
  - The mobile shell's smallest window is 320 x 320 px (was 320 x 480), so `--mobile` on the
    desktop can be laid on its side; `--landscape` opens it that way (a phone-sized wide window on
    the desktop; on the phone the activity asks for landscape).
  - `--bench-rotate N` turns the screen to landscape and back N times through
    `platform::request_orientation` (the activity's `setRequestedOrientation`, so the system's
    rotation setting is not touched; the desktop swaps the window's sides) and checks that the
    page and the zoom are kept and the header and the dock lie inside the safe area. On the
    phone: 6 of 6 turns pass, the view covered again 114-209 ms after each request
    (`docs/benchmarks.md` §14.8).
- **"Open with" and "Share".** "Open with" was Phase 1's (VIEW). "Share" (SEND with a PDF) now opens
  the PDF the same way: the activity takes the stream extra as it takes VIEW's data, and copies
  it in. Checked with `am start -a android.intent.action.SEND` on a cold start (the document
  opens) and while the app runs (it opens in a new tab).
- **The icon and the name.** An adaptive icon (`crates/android/res`): the title island's brand
  mark, the "P" on the dark tile (`#18181B`, light theme's `Theme.text`), drawn as a vector, with
  a monochrome layer for Android 13's themed icons. No PNGs: the app runs from API 26, which has
  adaptive icons. The name stays "PODEFA". `aapt dump badging` shows the icon and
  `versionName='0.1.0'`.
- **Release signing.** `scripts/android-build.sh` signs with the maintainer's release key when
  `PODEFA_KEYSTORE` names it; apksigner then asks for the password on the terminal, so no password
  goes through the environment or a file (the old `PODEFA_KEYSTORE_PASSWORD` is gone); without
  it, the throwaway debug key as before (CI). The script checks the signature at the end.
  `docs/android-release.md` has the steps for making the key and the version rule. Making the
  release key and building the first signed APK are the maintainer's: the key's password is
  theirs to type.
- **Size** (row MA): 11.6 MiB (12,210,819 bytes), within 16 MiB.
- **Checks:** on the phone `--bench-rotate` 6 of 6 and `--bench-pages` 13 of 14 (the same desktop
  sidebar step as in Phase 4); on the desktop `--bench-pages` 14 of 14, `--bench-stamps` 4 of 4,
  `--bench-tools` 3 of 3, `--bench-forms` 35 of 35, `--mobile --bench-rotate 2` 4 of 4; the
  workspace's tests, clippy and fmt pass. Seen on the phone on its side: Read, Tools, the More menu
  scrolled to its end, the Watermark sheet with and without the keyboard (scrolled to Apply), the
  bottom sheet.
- **Not done here:** the review in both themes and the icon on the launcher are the maintainer's;
  the desktop's dialogs were checked by their benches, not by eye.
- **Open for the maintainer:** in Tools on a phone on its side one page fills more than the view.
  Smaller pages would shrink the page's 44 px buttons below row MH; two or three pages side by
  side would be a layout of its own. Kept as it is unless the maintainer wants that layout.

### Phase 4: results (2026-10-28)

**Phase 4 passed its gate on 2026-10-28** ("Faz 4 onaylandı.").

- **The gallery column** (`crates/app/ui/mobile/gallery.slint`, `MobileGallery`, the concept's
  `MobileGalleryView`): in Tools on a phone the pages stand in one column in a Flickable, each in
  the concept's 230 x 325 px box, with a bar under the header ("200 pages", or "2 of 200 pages
  chosen", and Choose all). The desktop's gallery is not shown on a phone. Only the pages near the
  view are built, and Rust draws the pictures of those in view (`AppState::thumb_range` and
  `reveal_in_column`, `MOBILE_GALLERY_*` in `lib.rs`, which match `MobileGalleryMetrics`).
- **Each page** has its number at the top left, a check box at the top right and, along its
  bottom, Rotate Left, Rotate Right, Duplicate, Extract and Delete, always shown (nothing on a
  phone is only on hover; plan §4): 32 px chips in 44 px targets. A page's own button acts on that
  page and leaves nothing chosen that was not, since chosen pages turn the column's taps into
  choosing.
- **Taps:** a tap makes a page the current one and a second tap opens it in Read (a finger's double
  tap zooms elsewhere, so the second tap stands in for the desktop's double-click); while pages are
  chosen, a tap chooses or unchooses.
- **Gaps:** between two pages, and before the first and after the last, a "+" opens into Split
  Here (not at the ends), Insert Blank Page, Insert Pages from File and Close, each a 44 px button;
  they call the desktop gallery's handlers.
- **Moving pages:** a finger held still on a page for 500 ms picks it up, with the other chosen
  pages when it is one of them; the pages fade, the gap nearest the finger shows "Move Here", and
  letting go moves them there (one undo step, the desktop's `PageOp::Move`). Near the top or
  bottom of the view the column scrolls along. A pick-up that chose its page unchooses it after.
  Slint's Flickable stops taking a press's moves once it has been held still for 500 ms
  (`i-slint-core-1.18.1/items/flickable.rs:384, 740-755`), so a quick swipe still flings and a held
  finger reaches the page.
- **The chosen pages' bar** over the dock (the concept's bulk actions): the count, Rotate Left and
  Right, Duplicate, Extract, Delete (not when every page is chosen) and Unchoose.
- **Back** in Tools closes an open gap, then unchooses the chosen pages, then goes to Read.
- **The tools' dialogs as sheets:** Merge, Split, Watermark and Page Numbers, Compress and Export
  as Images, Crop and Document Properties were inside the canvas, which is hidden in Tools, so a
  dialog opened there did not show (seen on the phone: Watermark from the More menu in Tools).
  They now stand over the canvas and the gallery (`main.slint`, `dialogs`). On a phone each is a
  sheet across the width, above the dock, the edit strip or the on-screen keyboard, as tall as
  its content allows under the header, with 44 px rows; on the desktop their place and size are as
  before.
- **On the phone** (`large_200p.pdf` and `pages_bench_200.pdf`, `docs/benchmarks.md` §14.7):
  rotate by a page's button; a blank page from a gap (201 pages, as large as the page before);
  Undo; the check box and a tap choosing a second page; Rotate on both from the bar; Choose all
  (Delete hides); Back unchoosing; a long-press move of page 2 before page 1 (the order changed and
  nothing stayed chosen); Split Here after page 3 (a new tab of 197 pages, the document kept 3);
  Extract opening the system's save picker and Back cancelling it; the Watermark sheet in Tools
  over the keyboard and, once it hid, over the dock, Back closing it. `--bench-pages`: 13 of 14,
  every page operation and its undo; the miss is the step that drags a thumbnail in the desktop's
  sidebar, which the phone shell has no place for (the same on the desktop with `--mobile`).
- **Desktop:** `--bench-pages` 14 of 14, `--bench-forms` 35 of 35.
- **Found, not fixed:** the first change into Edit and the first into Tools after a start take
  13.6-26.8 ms and 21.2-34.6 ms for their slowest frame on the phone (`--bench-modes`), over the
  16.7 ms budget; the same build before this phase took 26.0-28.1 ms into Tools, so the column did
  not bring it. A second change into Tools takes 3.7-5.1 ms. Backlog item 27.

### Phase 3: results (2026-10-27)

- **The edit strip** (`crates/app/ui/mobile/shell.slint`, the concept's `MobileEditorToolbar`):
  over the dock in Edit, one 56 px island whose row scrolls sideways: Select, Highlight,
  Underline, Strike-out, Squiggly, Ink, Note, then Crop and Rotate Left and Right, then, when the
  document has form fields, the field shading and a Form card (Reset Field, Reset Form, Import
  Data, Export Data, Flatten Form: the desktop's form menu). Every button is 44 px; the tinted
  fills are the desktop's (Theme `highlight-fill`, `strike-fill`, `ink-fill`), and the chosen
  markup or ink tool carries a dot in its colour. Leaving Edit by the dock puts the tool back to
  Select, as the desktop's mode buttons do. Squiggly and Crop are not in the concept's strip; they
  are the desktop's Edit commands, and the strip scrolls, so nothing had to go.
- **Palettes:** over the strip, the chosen markup tool's six colours (28 px discs in 44 px
  targets), and for Ink the colours and the three widths. The selected annotation's bar
  (colours and Delete) has 44 px targets on a phone, the go-to-page and zoom cards open over
  the strip and palette.
- **Taps** (`AppState::touch_tap`): a finger's tap does what a click does. It fills a form field
  (on the tap, not on touch down, so a pan that starts on a field only pans), places or opens a
  note with the Note tool, selects the annotation under it, and a second tap on a selected note
  opens it (a finger's double tap zooms, so it cannot open a note as a double-click does).
  Anywhere else it clears the selection and may be the first tap of a double tap.
- **A long press on a note** picks it up and the finger moves it, one undo step (`MoveNote`), as
  a mouse drag does. A long press on text still selects a word, and with a markup tool the
  release marks it (Phase 2's path).
- **Ink and Crop** draw with one finger: the Flickable does not pan while either tool is chosen.
- **No hover on touch:** a finger's move shows no note or field tip (the tips stayed on screen
  after the finger lifted); the floating Reset Field button beside a field is not shown on a
  phone, where at 44 px it covered the next field (a tap on a radio button reset the field
  beside it); field options are 44 px rows; the note editor shows no "Ctrl+Enter".
- **Keyboard (row MI):** the shell's `floor` is the top of the on-screen keyboard or of the
  dock, strip and palette, whichever is higher; `edit-floor` reaches Rust through a `changed`
  callback. When a field editor opens and whenever the floor moves, `keep_editor_above_floor`
  scrolls the document so the field's editor (and its options) end 8 px above the floor, but never
  past the field's top, and a field under the header comes down below it; the note editor, which
  does not follow the page, moves itself. The document's bottom inset includes the keyboard, so
  the last field of a document can scroll above it too.
- **`FA_PDF_TRACE` / bench:** each ink stroke logs its frames (`ink started`, `ink ended` with
  points, the slowest rebuild of the live stroke and the frame window), for row MJ by hand
  (`docs/benchmarks.md` §14.6).
- **On the phone** (`docs/benchmarks.md` §14.6), the APK of this phase on the generated form of
  `form_test.rs`: the strip and palettes show and choose; a text field at the bottom of the view
  scrolls above the keyboard when tapped and takes typed text (row MI); a multi-line field under
  the header comes down below it; the note editor opens above the keyboard; check box, radio
  button and combo box fill on a tap; Save writes the file, and the values are there after a
  restart; strokes follow the finger with frames of 4.0-6.0 ms (once 30.4 ms, the first stroke)
  and one Undo removes one stroke (row MJ); a long press and drag highlights a word; a note is
  placed, selected, opened, deleted and moved by touch.
- **Desktop:** `--bench-forms` 35 of 35, `--bench-note` 5 of 5; `--bench-ink` 3 of 4 with backlog
  item 24's known failure.
- **Back key** (fixed in this phase, by the maintainer's word; was backlog item 25): Back closed
  the app whenever no text field had the keyboard, and an unsaved document was lost without the
  unsaved prompt. Slint 1.18.1 sends Back to the window as `Key.Back` and finishes the activity
  only when nothing accepts it (`i-slint-backend-android-activity-1.18.1/javahelper.rs:788-810`).
  The window's `back()` (`main.slint`) now takes it one level at a time: the shell's menus, card
  or sheet (`MobileShell.back()`), an open dialog, the search bar, an open field or note editor
  (committed, as a tap elsewhere does) or a selection (`AppState::back_close`), Edit or Tools back
  to Read, and last, with unsaved changes, the same question as closing the window
  (`tabs::quit`, which on Android ends with `Activity.finish()`, `platform::close_app`). With
  nothing unsaved it does not take Back, and Slint closes the app.
  - The page takes the keys back before anything closes: a closed editor left the focus nowhere,
    and the next Back skipped the window and closed the app.
  - Field and note editors move the focus away before their text is read: on Android the
    keyboard's composing word reaches `text` only at FocusOut
    (`i-slint-core-1.18.1/items/text.rs:1258-1278`), so a Back right after typing lost the last
    word.
  - On the phone, with "Back" typed into a field: the 1st Back hides the keyboard (the IME takes
    it), the 2nd commits the field (the header's Save turns on), the 3rd goes from Edit to Read,
    the 4th asks "Save the changes to "form.pdf" before closing it?". Cancel keeps the app and
    the changes; No closes it and leaves the file byte for byte as it was; Yes saves, closes,
    and the value is there at the next start. The sheet also closes on Back. No crash.
  - On the way: Android keeps the process after the activity finishes, and a new launch runs
    `android_main` again in it. `platform::android` kept the first launch's activity in a
    `OnceLock`, so the unsaved question of a second launch went to the finished activity and the
    app crashed (`BadTokenException`). Each launch now replaces it. Slint's own `finish()` on
    Back left the process in the same state before.
- **Not checked by script:** typing with the phone's own keyboard. Through `adb shell input text`,
  one run lost the word being composed when Enter was pressed in a single-line field, and a word
  composed for a combo box that takes no text showed up in the next field's editor; with spaces
  typed between words the text arrived whole. The Back tests found why the word moves: typed
  into a field that was empty, "Ab" became "ZqAb", "Zq" being the last word typed in the field
  before (even in the launch before). Slint 1.18.1 restarts the keyboard's input only when the
  input type changes (`SlintAndroidJavaHelper.java:215-225`), so when the focus moves to another
  text input of the same type, the keyboard keeps its composing word and commits it there
  (backlog item 26).
- **Not explained:** one Save did not write the file (the header's Save, after a Reset Field by
  mistake); two later saves did. Not reproduced.

### Phase 2, touch: progress (2026-10-27)

- **Pinch:** `ScaleRotateGestureHandler` (§3.1) around the viewport. While the fingers move,
  Rust only sets a transform on the layer holding the Flickable: a scale around the point where
  they started (`transform-origin`) and a shift by how far that point moved, clamped so the scale
  stays inside 1%-6400%. Nothing is laid out or requested. When they lift, `zoom_moved` zooms to the
  scale shown, anchored where the fingers started and moved by their drift, in one step, and the
  transform goes back to none in the same frame. A trackpad pinch on the desktop takes the same
  path. While a gesture is active the handler holds the pointer, so the Flickable does not pan.
- **Fling:** Slint's Flickable already flings a touch release with its own constant deceleration
  (`i-slint-core-1.18.1/items/flickable.rs:645-668`); the canvas window (Plan 0002) is wide
  enough that a fling does not reach its edge before it stops.
- **Double tap:** two taps of a finger within 300 ms and 24 logical px zoom to fit width for the
  page under them, anchored at the tap; a double tap at fit width (within 2%) goes back to the zoom
  before, or to twice fit width. A mouse double-click still selects a word. The first version used
  the TouchArea's `double-clicked`, which never fired on the phone: Slint 1.18.1 sends `Exit`
  after a finger lifts (`i-slint-core-1.18.1/input.rs:2400-2410`), and the change of the top item
  resets the click count (`window.rs:1087-1090`), so Rust now tells a double tap from two taps
  itself.
- **`--bench-pinch N`** (`docs/benchmarks.md` §14.4): on the desktop 0 tiles during every pinch,
  `frame_max` under 1 ms, the view covered 15-54 ms after the fingers lift.
- **On the phone** (`docs/benchmarks.md` §14.4-14.5): the scripted pinch keeps every frame at
  2-4 ms with 0 tiles (one 48.7 ms frame in 24 pinches); 90 real flings through the 1000-page
  document leave no hole and PSS flat at 208-217 MiB; idle 0 frames.
- **Double tap on the phone** (two `input tap` 114-121 ms apart, on the 1000-page document at
  100%): the trace shows both taps as touches and the zoom going to 58.0% (fit width; the page
  fills the width on the screenshot), and the next double tap back to 100%.
- **Library suite on the phone** at `fb90872`: 215 of 215 tests in 26 executables pass
  (`docs/backlog.md` item 9).
- **Not yet:** a pinch by hand (the `adb` shell may not write the touch screen's input device:
  SELinux), so row ME's last part is the maintainer's. `--bench-ink`'s commit check fails also
  before this phase (backlog item 24).

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
