<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# Plan 0008: Mobile GUI, Android First

## Status

**Phase 0 (research, the plan itself) in progress - awaiting the maintainer's answers to §9.**
The research that could be done from the pinned sources is done (§3). The trial build on the
phone (§5, Phase 0, step 3) needs downloads that wait for the maintainer's OK (§9 q2).

- Phase 0: in progress (2026-10-26).
- Phases 1 to 5: not started.

Started 2026-10-26 on the maintainer's request ("Konsepte mobil GUI eklendi. Geliştirme için
başlangıcı yap"). Plan 0007 Phase 4 (tabs) is at its own gate and Phase 5 of that plan is not
started; §9 q1 asks how the two plans go together.

The design source is the mobile part of the maintainer's TypeScript concept,
`podefa-ui-concept` (`src/components/mobile/`, `DESIGN.md` step 38). As for Plan 0007, it lives
outside this repository and is not a dependency: this plan ports its decisions to Slint, not its
code.

## 0. Gate record

Phase 0 has not passed its gate.

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
  `cfg(target_os = "android")` dependency); it is not in the local registry yet, so what it does
  inside (renderer, touch events, the Java helper) is checked in the trial build.
- **Build tools:** Slint's documentation asks for the Android SDK (`ANDROID_HOME`), the NDK
  (`ANDROID_NDK_ROOT`) and `javac` (`JAVA_HOME`), and suggests `cargo-apk` or `xbuild` to make the
  APK (`android.rs:52-80`). The mini PC has the NDK only.
- **Renderer:** the femtovg renderer the desktop uses is a dependency on every platform except
  Android (`slint-1.18.1/Cargo.toml`, `[target.'cfg(not(target_os = "android"))'.dependencies.i-slint-renderer-femtovg]`).
  Which renderer the Android backend draws with (software or Skia) decides what the islands can
  look like (the software renderer draws no shadows, Plan 0007 §3.2) and what a frame costs: the
  trial build measures it.
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
- The clipboard goes through Slint on Android instead of `arboard` (whether Slint's Android backend
  provides a clipboard is checked in the trial build); `rfd` and `arboard` move behind a desktop
  `cfg`.

### 3.3 The concept's mobile interface

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

### Phase 0 steps

1. Read the concept's mobile interface and Slint's Android support (§3): done.
2. Write this plan: done.
3. The trial build (after §9 q2): today's desktop window, unchanged, behind a minimal
   `android_main` on a throwaway branch, built on the mini PC, installed on the phone. Measured:
   APK size, the process's memory after opening `large_200p.pdf` and after a pinch zoom, the time
   to the first page, whether one-finger pan and two-finger pinch reach the canvas, which renderer
   draws. Nothing of it is merged; the figures go into §3 and `docs/benchmarks.md` §14.

## 6. Acceptance and benchmarks

Rows on the maintainer's phone (Samsung Galaxy A34, Android 16), release build. The figures marked
"proposed" are set from the trial build's measurements before Phase 1 starts.

| Row | Scenario | Pass |
| :--- | :--- | :--- |
| MA | Size | The release APK (arm64-v8a) at most a proposed 25 MiB |
| MB | Memory | Peak resident memory with `large_200p.pdf`, a pinch to 400% and a fling, at most a proposed 200 MiB; it follows the 16 MiB tile budget like the desktop's §7.6 |
| MC | Startup | From launch to the first page covered, at most a proposed 1.5 s |
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

- **The Android backend's renderer** may be the software one: no shadows, and every frame drawn by
  the CPU, which a pinch over many tiles may not keep inside 16.7 ms. The trial build finds out;
  if so, the options are Slint's Skia renderer on Android (size and build cost), or fewer layers
  in the mobile shell.
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
