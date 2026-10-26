// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

package org.fineassociation.podefa;

import android.app.AlertDialog;
import android.app.NativeActivity;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.os.Bundle;
import android.provider.DocumentsContract;
import android.provider.OpenableColumns;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.ArrayList;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/**
 * The app's activity (Plan 0008 Phase 1, ADR 0012): Rust's NativeActivity plus what only Java can
 * do. The system's document pickers answer through onActivityResult, so the pickers start here
 * and report back to native code; a picked document is copied into the cache, and the app works
 * on the copy. What the app writes is copied back to the document it stands for.
 *
 * Native code calls the public methods from its own thread and registers the native methods
 * itself (RegisterNatives), since NativeActivity loads the library without System.loadLibrary.
 */
public class PodefaActivity extends NativeActivity {
    static final int OPEN = 0;
    static final int OPEN_MANY = 1;
    static final int FOLDER = 2;
    static final int CREATE = 3;

    static final int ANSWER_YES = 0;
    static final int ANSWER_NO = 1;
    static final int ANSWER_CANCEL = 2;

    /** Picks and their copies run here, one at a time, off the UI thread. */
    private final ExecutorService io = Executors.newSingleThreadExecutor();

    /** What each pending request asked for, by request code. */
    private final java.util.HashMap<Integer, Integer> kinds = new java.util.HashMap<>();

    static native void nativePicked(int request, String[] paths, String[] uris);

    static native void nativeAnswer(int request, int answer);

    static native void nativeOpened(String path, String uri);

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        final Uri uri = viewedUri(intent);
        if (uri == null) {
            return;
        }
        io.execute(() -> {
            String path = copyIn(uri, "docs");
            if (path != null) {
                nativeOpened(path, uri.toString());
            }
        });
    }

    /** The cache folder, for native code. */
    public String cacheDir() {
        return getCacheDir().getAbsolutePath();
    }

    /**
     * The app's folder in shared storage (Android/data/<package>/files), which adb can read and
     * write without the app being debuggable: the bench harness's documents and output go there.
     */
    public String externalDir() {
        File dir = getExternalFilesDir(null);
        return dir == null ? null : dir.getAbsolutePath();
    }

    /** Command-line arguments from the launch intent (am start --es args "..."), or null. */
    public String launchArgs() {
        Intent intent = getIntent();
        return intent == null ? null : intent.getStringExtra("args");
    }

    /**
     * The document the activity was started to show ("Open with"), copied into the cache:
     * {path, uri}, or null. Runs on the caller's thread.
     */
    public String[] openedDocument() {
        Uri uri = viewedUri(getIntent());
        if (uri == null) {
            return null;
        }
        String path = copyIn(uri, "docs");
        return path == null ? null : new String[] {path, uri.toString()};
    }

    private static Uri viewedUri(Intent intent) {
        if (intent == null || !Intent.ACTION_VIEW.equals(intent.getAction())) {
            return null;
        }
        return intent.getData();
    }

    /** Starts a picker; the answer comes through nativePicked with the same request code. */
    public void pick(final int request, final int kind, final String[] mimes, final String name) {
        runOnUiThread(() -> {
            Intent intent;
            switch (kind) {
                case FOLDER:
                    intent = new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE);
                    break;
                case CREATE:
                    intent = new Intent(Intent.ACTION_CREATE_DOCUMENT);
                    intent.addCategory(Intent.CATEGORY_OPENABLE);
                    intent.setType(mimes.length > 0 ? mimes[0] : "application/octet-stream");
                    if (name != null) {
                        intent.putExtra(Intent.EXTRA_TITLE, name);
                    }
                    break;
                default:
                    intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
                    intent.addCategory(Intent.CATEGORY_OPENABLE);
                    if (mimes.length == 1) {
                        intent.setType(mimes[0]);
                    } else {
                        intent.setType("*/*");
                        if (mimes.length > 1) {
                            intent.putExtra(Intent.EXTRA_MIME_TYPES, mimes);
                        }
                    }
                    intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, kind == OPEN_MANY);
                    break;
            }
            kinds.put(request, kind);
            try {
                startActivityForResult(intent, request);
            } catch (RuntimeException e) {
                kinds.remove(request);
                nativePicked(request, null, null);
            }
        });
    }

    @Override
    protected void onActivityResult(final int request, int result, final Intent data) {
        Integer kind = kinds.remove(request);
        if (kind == null) {
            super.onActivityResult(request, result, data);
            return;
        }
        if (result != RESULT_OK || data == null) {
            nativePicked(request, null, null);
            return;
        }
        final ArrayList<Uri> uris = new ArrayList<>();
        if (data.getClipData() != null) {
            ClipData clip = data.getClipData();
            for (int i = 0; i < clip.getItemCount(); i++) {
                uris.add(clip.getItemAt(i).getUri());
            }
        } else if (data.getData() != null) {
            uris.add(data.getData());
        }
        final int flags = Intent.FLAG_GRANT_READ_URI_PERMISSION
                | (kind == OPEN_MANY ? 0 : Intent.FLAG_GRANT_WRITE_URI_PERMISSION);
        for (Uri uri : uris) {
            try {
                getContentResolver().takePersistableUriPermission(uri, flags);
            } catch (SecurityException e) {
                // A provider that grants no lasting access: this session's access still works.
            }
        }
        final int k = kind;
        io.execute(() -> {
            String[] paths = new String[uris.size()];
            String[] strings = new String[uris.size()];
            for (int i = 0; i < uris.size(); i++) {
                Uri uri = uris.get(i);
                strings[i] = uri.toString();
                switch (k) {
                    case FOLDER:
                        paths[i] = folderIn(uri);
                        break;
                    case CREATE:
                        paths[i] = placeFor(uri, "out");
                        break;
                    default:
                        paths[i] = copyIn(uri, "docs");
                        break;
                }
                if (paths[i] == null) {
                    nativePicked(request, null, null);
                    return;
                }
            }
            nativePicked(request, paths, strings);
        });
    }

    /** The document's display name, or a stand-in. */
    private String displayName(Uri uri) {
        try (Cursor cursor = getContentResolver().query(
                uri, new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (cursor != null && cursor.moveToFirst() && !cursor.isNull(0)) {
                return cursor.getString(0).replace('/', '_');
            }
        } catch (RuntimeException e) {
            // Falls through to the stand-in.
        }
        String last = uri.getLastPathSegment();
        return last == null ? "document.pdf" : last.replace('/', '_').replace(':', '_');
    }

    /** cache/<area>/<hash of the URI>/<name>: the same document always gets the same copy. */
    private String placeFor(Uri uri, String area) {
        File dir = new File(new File(getCacheDir(), area), Integer.toHexString(uri.toString().hashCode()));
        if (!dir.isDirectory() && !dir.mkdirs()) {
            return null;
        }
        return new File(dir, displayName(uri)).getAbsolutePath();
    }

    /** Copies the document into the cache; its path, or null. */
    private String copyIn(Uri uri, String area) {
        String path = placeFor(uri, area);
        if (path == null) {
            return null;
        }
        File part = new File(path + ".part");
        try (InputStream in = getContentResolver().openInputStream(uri);
                OutputStream out = new FileOutputStream(part)) {
            if (in == null) {
                return null;
            }
            pipe(in, out);
        } catch (IOException | RuntimeException e) {
            part.delete();
            return null;
        }
        File file = new File(path);
        file.delete();
        return part.renameTo(file) ? path : null;
    }

    /**
     * An empty cache folder standing for a picked folder, with an empty file for each document
     * already in it, so the app can see which names it would replace.
     */
    private String folderIn(Uri tree) {
        String base = placeFor(tree, "folders");
        if (base == null) {
            return null;
        }
        File dir = new File(base).getParentFile();
        File[] old = dir.listFiles();
        if (old != null) {
            for (File f : old) {
                f.delete();
            }
        }
        Uri children = DocumentsContract.buildChildDocumentsUriUsingTree(
                tree, DocumentsContract.getTreeDocumentId(tree));
        try (Cursor cursor = getContentResolver().query(children,
                new String[] {DocumentsContract.Document.COLUMN_DISPLAY_NAME}, null, null, null)) {
            while (cursor != null && cursor.moveToNext()) {
                String name = cursor.getString(0);
                if (name != null && !name.contains("/")) {
                    new File(dir, name).createNewFile();
                }
            }
        } catch (IOException | RuntimeException e) {
            // Without the names the app just does not warn before replacing.
        }
        return dir.getAbsolutePath();
    }

    /** Copies a file the app wrote back to its document. Queued: returns at once. */
    public void writeBack(final String path, final String uri) {
        io.execute(() -> {
            try (InputStream in = new FileInputStream(path);
                    OutputStream out = getContentResolver().openOutputStream(Uri.parse(uri), "wt")) {
                if (out != null) {
                    pipe(in, out);
                }
            } catch (IOException | RuntimeException e) {
                android.util.Log.e("PODEFA", "could not write " + uri + ": " + e);
            }
        });
    }

    /** Copies a file the app wrote into the picked folder, replacing a document of that name. */
    public void writeToFolder(final String path, final String tree, final String mime) {
        io.execute(() -> {
            Uri treeUri = Uri.parse(tree);
            String name = new File(path).getName();
            try {
                Uri target = childNamed(treeUri, name);
                if (target == null) {
                    Uri parent = DocumentsContract.buildDocumentUriUsingTree(
                            treeUri, DocumentsContract.getTreeDocumentId(treeUri));
                    target = DocumentsContract.createDocument(getContentResolver(), parent, mime, name);
                }
                if (target == null) {
                    return;
                }
                try (InputStream in = new FileInputStream(path);
                        OutputStream out = getContentResolver().openOutputStream(target, "wt")) {
                    if (out != null) {
                        pipe(in, out);
                    }
                }
            } catch (IOException | RuntimeException e) {
                android.util.Log.e("PODEFA", "could not write " + name + ": " + e);
            }
        });
    }

    private Uri childNamed(Uri tree, String name) {
        Uri children = DocumentsContract.buildChildDocumentsUriUsingTree(
                tree, DocumentsContract.getTreeDocumentId(tree));
        try (Cursor cursor = getContentResolver().query(children, new String[] {
                    DocumentsContract.Document.COLUMN_DOCUMENT_ID,
                    DocumentsContract.Document.COLUMN_DISPLAY_NAME},
                null, null, null)) {
            while (cursor != null && cursor.moveToNext()) {
                if (name.equals(cursor.getString(1))) {
                    return DocumentsContract.buildDocumentUriUsingTree(tree, cursor.getString(0));
                }
            }
        }
        return null;
    }

    private static void pipe(InputStream in, OutputStream out) throws IOException {
        byte[] buffer = new byte[64 * 1024];
        int n;
        while ((n = in.read(buffer)) > 0) {
            out.write(buffer, 0, n);
        }
    }

    /**
     * A message box: buttons 0 = OK, 1 = Yes/No, 2 = Yes/No/Cancel. The answer comes through
     * nativeAnswer; closing the box without a button answers Cancel.
     */
    public void message(final int request, final String text, final int buttons) {
        runOnUiThread(() -> {
            final boolean[] answered = {false};
            AlertDialog.Builder builder = new AlertDialog.Builder(this)
                    .setTitle("PODEFA")
                    .setMessage(text)
                    .setOnDismissListener(d -> {
                        if (!answered[0]) {
                            answered[0] = true;
                            nativeAnswer(request, ANSWER_CANCEL);
                        }
                    });
            if (buttons == 0) {
                builder.setPositiveButton(android.R.string.ok, (d, w) -> {
                    answered[0] = true;
                    nativeAnswer(request, ANSWER_YES);
                });
            } else {
                builder.setPositiveButton("Yes", (d, w) -> {
                    answered[0] = true;
                    nativeAnswer(request, ANSWER_YES);
                });
                builder.setNegativeButton("No", (d, w) -> {
                    answered[0] = true;
                    nativeAnswer(request, ANSWER_NO);
                });
                if (buttons == 2) {
                    builder.setNeutralButton("Cancel", (d, w) -> {
                        answered[0] = true;
                        nativeAnswer(request, ANSWER_CANCEL);
                    });
                }
            }
            builder.show();
        });
    }

    /** Puts text on the clipboard. */
    public void copyText(final String text) {
        runOnUiThread(() -> {
            ClipboardManager clipboard = (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
            if (clipboard != null) {
                clipboard.setPrimaryClip(ClipData.newPlainText("PODEFA", text));
            }
        });
    }
}
