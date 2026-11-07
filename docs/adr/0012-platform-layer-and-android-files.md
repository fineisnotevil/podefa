<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# ADR 0012: A Platform Layer, and Documents on Android as Cache Copies of `content://` URIs

## Status

Accepted (Plan 0008 Phase 1, 2026-10-26; proposed in the plan's §3.2). Amends ADR 0004 (opening
documents) and ADR 0006 (the clipboard): the file dialogs, message boxes and clipboard now go
through `app::platform`, and on Android they answer later instead of blocking.

## Context

The desktop app asks for files with `rfd`'s modal dialogs and copies text with `arboard`. Neither
exists on Android, where:

- The system's document pickers (`ACTION_OPEN_DOCUMENT`, `ACTION_CREATE_DOCUMENT`,
  `ACTION_OPEN_DOCUMENT_TREE`) are started from an activity and answer only through that
  activity's `onActivityResult`. Native code cannot receive that callback: `NativeActivity` does
  not forward it, so some Java has to run in the app.
- A picker answers with a `content://` URI, not a path. MuPDF, the search thread, the tool
  thread and the in-place incremental save (ADR 0007) all work on paths.
- `android_main` runs on its own thread, which is also Slint's event loop. If it waited for a
  picker, the activity's lifecycle events, which reach native code through that same loop, would
  wait too: the app would hang.
- Android sends stdout and stderr nowhere.

## Decision

1. **`app::platform`** holds what differs: `FileDialog` (open one or several files, a folder,
   save), `MessageDialog`, `copy_text`, `settings_path`, `temp_dir`, `written` and `shutdown`.
   `platform::desktop` wraps `rfd` and `arboard`; `platform::android` calls the activity over JNI.
   The rest of the app uses only `platform`, and `rfd` and `arboard` are not built for Android.
2. **Dialogs answer through a callback.** On the desktop the callback runs before the call
   returns, as the modal dialogs always did; on Android it runs on the event loop once the
   activity answers. Callers hold a `slint::Weak` to the window and no borrow of the app state
   across the call. A chain of questions (a folder, then "replace these files?") is a chain of
   callbacks.
3. **The app's own activity.** `org.fineassociation.podefa.PodefaActivity` (`crates/android/java`)
   extends `NativeActivity`: it starts the pickers, copies documents, shows message boxes
   (`AlertDialog`) and sets the clipboard. The build compiles it with `javac` and `d8` and adds
   the `classes.dex` to the APK cargo-apk makes, then aligns and signs it again
   (`scripts/android-build.sh`). Native code registers the activity's native methods itself
   (`RegisterNatives`), because `NativeActivity` loads the library without `System.loadLibrary`.
   The maintainer chose this over a dex loaded at run time ("APK'ya kendi dex", 2026-10-26).
4. **Documents are cache copies.** A picked document is copied into
   `cache/docs/<hash of the URI>/<display name>`, and the app opens the copy by path; the same
   document always gets the same copy, so recent files stay one entry each. The URI's read and
   write permission is persisted (`takePersistableUriPermission`). A map from copy to URI, kept in
   `files/documents.txt`, outlives the process, so a recent file still writes back.
5. **Writing back.** Whatever the app writes to a path a dialog gave it (a save, Save As, a tool's
   outputs, exported form data) is reported to `platform::written`, which copies the file to its
   URI (`openOutputStream(uri, "wt")`) on the activity's I/O thread. Save As first creates the
   document with the create-document picker, then saves the copy, then writes it back.
6. **Folders.** A picked folder (Split, Export as Images) is an empty cache folder holding an
   empty file for each document already in the folder, so "replace these files?" still works;
   each file the job writes there goes to the folder's tree URI, replacing a document of that
   name.
7. **"Open with".** The activity takes `ACTION_VIEW` for `application/pdf`
   (`launchMode="singleTask"`): the document it was started with is copied and opened at start; one
   that arrives while the app runs (`onNewIntent`) opens in a tab.
8. **Settings, temporary files and output.** Settings live in the app's files folder, temporary
   files in its cache folder. `android_main` (`crates/android`) sends stdout and stderr to
   `out.txt` in the app's folder in shared storage and takes command-line arguments from the
   launch intent's `args` extra, which is how the bench harness runs on the phone without a
   debuggable build.
9. **Crates.** `crates/app` is a library (`app::run`) with the desktop binary as a thin `main`.
   `crates/android` (`podefa-android`, library `libpodefa.so`) holds `android_main`, the Java
   class, the manifest and the link of the NDK's C++ runtime into the library (as `engine-mupdf`'s
   build script does for its test binaries); it depends on `app`, nothing depends on it, and on
   other targets it is empty.

## Consequences

- Opening a document from a slow provider (a cloud drive) waits for the copy before the first
  page; large documents are copied once per open, and the cache holds one copy per document until
  Android clears it.
- A save writes the file twice (the copy, then the URI). Another app changing the document at the
  same time is not detected: the last write wins, as with two desktop apps.
- The app trusts Android's providers to honour `"wt"` (truncate). A provider that does not, or
  that grants no lasting permission, still works for the session in which the file was picked.
- Java is now part of the build (`javac` and `d8`); CI builds the APK on Linux.
- Every new dialog goes through `platform` and is written as a callback, also on the desktop.
