// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Android (Plan 0008 Phase 1, ADR 0012): the system's document pickers, message boxes and
//! clipboard through the app's activity, `org.fineassociation.podefa.PodefaActivity`
//! (`crates/android/java`), called over JNI.
//!
//! A picked document is a `content://` URI; the activity copies it into the cache and the app
//! opens the copy. [`Documents`] remembers which URI each copy stands for, in a file that outlives
//! the process, so a save or a recent file still writes back to the document. A picked folder is
//! an empty cache folder whose files go to the folder's tree URI when written.
//!
//! The activity answers on its own threads; the answers hop to the event loop, where the
//! callbacks wait in [`PENDING`]. Everything else here runs on the event loop's thread.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use jni::objects::{GlobalRef, JClass, JObject, JObjectArray, JString, JValue};
use jni::sys::jint;
use jni::{JNIEnv, JavaVM, NativeMethod};

use super::{Answer, Buttons, FileDialog, MessageDialog, mime_for};

/// The activity's request kinds (`PodefaActivity.OPEN` and the rest).
const OPEN: i32 = 0;
const OPEN_MANY: i32 = 1;
const FOLDER: i32 = 2;
const CREATE: i32 = 3;

struct Java {
    vm: JavaVM,
    activity: GlobalRef,
}

static JAVA: OnceLock<Java> = OnceLock::new();
/// The app's files folder (settings, the document map) and its cache folder.
static DIRS: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();

/// A picker's or a message box's callback, waiting for the activity's answer.
/// Opens a document in a tab.
type Opener = Box<dyn Fn(PathBuf)>;

enum Pending {
    Picked(Box<dyn FnOnce(Option<Vec<PathBuf>>)>),
    Answer(Box<dyn FnOnce(Answer)>),
}

/// The URI each cache copy stands for, and the tree URI each cache folder stands for.
#[derive(Default)]
struct Documents {
    files: HashMap<PathBuf, String>,
    folders: HashMap<PathBuf, String>,
}

thread_local! {
    static PENDING: RefCell<HashMap<i32, Pending>> = RefCell::new(HashMap::new());
    static NEXT_REQUEST: Cell<i32> = const { Cell::new(0) };
    static DOCUMENTS: RefCell<Documents> = RefCell::new(Documents::default());
    /// Opens a document the system handed over while the app runs ("Open with").
    static ON_OPEN: RefCell<Option<Opener>> = const { RefCell::new(None) };
}

/// Called once from `android_main`, after Slint's backend is up and before the window: keeps the
/// activity, registers its native methods, reads the document map. Returns the document the
/// activity was started to show, already copied into the cache.
pub fn init(app: &slint::android::AndroidApp) -> Option<PathBuf> {
    let files = app.internal_data_path()?;
    // SAFETY: the pointers come from the running activity and stay valid for the process.
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }.ok()?;
    let activity = {
        let mut env = vm.attach_current_thread().ok()?;
        // SAFETY: as above; the global reference keeps the activity object alive.
        let local = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
        let activity = env.new_global_ref(&local).ok()?;
        if let Err(err) = register(&mut env, &local) {
            let _ = env.exception_clear();
            eprintln!("podefa: the activity has no native methods ({err}); dialogs are off");
        }
        activity
    };
    let _ = JAVA.set(Java { vm, activity });
    let cache = activity_string("cacheDir").map_or_else(|| files.join("../cache"), PathBuf::from);
    let _ = DIRS.set((files, cache));
    load_documents();
    let opened = with_env(|env, activity| {
        let pair = env.call_method(activity, "openedDocument", "()[Ljava/lang/String;", &[])?;
        Ok(strings(env, &JObjectArray::from(pair.l()?)))
    })
    .flatten()?;
    let [path, uri] = <[String; 2]>::try_from(opened).ok()?;
    let path = PathBuf::from(path);
    remember_file(&path, uri);
    Some(path)
}

/// The app's folder in shared storage, which adb reaches without the app being debuggable.
pub fn external_dir() -> Option<PathBuf> {
    activity_string("externalDir").map(PathBuf::from)
}

/// Command-line arguments from the launch intent (`am start ... --es args "..."`).
pub fn launch_args() -> Option<String> {
    activity_string("launchArgs")
}

/// Calls one of the activity's `String name()` methods; `None` for a null answer.
fn activity_string(name: &str) -> Option<String> {
    with_env(|env, activity| {
        let value = env.call_method(activity, name, "()Ljava/lang/String;", &[])?;
        let value = JString::from(value.l()?);
        if value.is_null() {
            return Ok(None);
        }
        Ok(Some(String::from(env.get_string(&value)?)))
    })
    .flatten()
}

/// Sets what a document handed over while the app runs opens in.
pub fn on_open(open: impl Fn(PathBuf) + 'static) {
    ON_OPEN.with(|cell| *cell.borrow_mut() = Some(Box::new(open)));
}

fn register(env: &mut JNIEnv, activity: &JObject) -> jni::errors::Result<()> {
    let class = env.get_object_class(activity)?;
    env.register_native_methods(
        &class,
        &[
            NativeMethod {
                name: "nativePicked".into(),
                sig: "(I[Ljava/lang/String;[Ljava/lang/String;)V".into(),
                fn_ptr: native_picked as *mut c_void,
            },
            NativeMethod {
                name: "nativeAnswer".into(),
                sig: "(II)V".into(),
                fn_ptr: native_answer as *mut c_void,
            },
            NativeMethod {
                name: "nativeOpened".into(),
                sig: "(Ljava/lang/String;Ljava/lang/String;)V".into(),
                fn_ptr: native_opened as *mut c_void,
            },
        ],
    )
}

/// Runs `f` with a JNI environment for this thread and the activity; `None` when there is no
/// activity or the call threw (the exception is cleared).
fn with_env<T>(f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<T>) -> Option<T> {
    let java = JAVA.get()?;
    let mut env = java.vm.attach_current_thread().ok()?;
    match f(&mut env, java.activity.as_obj()) {
        Ok(value) => Some(value),
        Err(err) => {
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_describe();
                let _ = env.exception_clear();
            }
            eprintln!("podefa: JNI call failed: {err}");
            None
        }
    }
}

/// A Java `String[]` as strings; `None` for a null array or a null element.
fn strings(env: &mut JNIEnv, array: &JObjectArray) -> Option<Vec<String>> {
    if array.is_null() {
        return None;
    }
    let len = env.get_array_length(array).ok()?;
    (0..len)
        .map(|i| {
            let item = JString::from(env.get_object_array_element(array, i).ok()?);
            if item.is_null() {
                return None;
            }
            env.get_string(&item).ok().map(String::from)
        })
        .collect()
}

fn string_array<'local>(
    env: &mut JNIEnv<'local>,
    items: &[&str],
) -> jni::errors::Result<JObjectArray<'local>> {
    let len = i32::try_from(items.len()).unwrap_or(i32::MAX);
    let array = env.new_object_array(len, "java/lang/String", JObject::null())?;
    for (i, item) in (0..len).zip(items) {
        let item = env.new_string(item)?;
        env.set_object_array_element(&array, i, item)?;
    }
    Ok(array)
}

/// A request code for `startActivityForResult`, which takes 16 bits.
fn next_request(pending: Pending) -> i32 {
    let request = NEXT_REQUEST.with(|next| {
        let request = next.get() % 0xffff + 1;
        next.set(request);
        request
    });
    PENDING.with(|cell| cell.borrow_mut().insert(request, pending));
    request
}

fn take_pending(request: i32) -> Option<Pending> {
    PENDING.with(|cell| cell.borrow_mut().remove(&request))
}

extern "system" fn native_picked<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    request: jint,
    paths: JObjectArray<'local>,
    uris: JObjectArray<'local>,
) {
    let paths = strings(&mut env, &paths);
    let uris = strings(&mut env, &uris);
    let _ = slint::invoke_from_event_loop(move || picked(request, paths, uris));
}

extern "system" fn native_answer<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    request: jint,
    answer: jint,
) {
    let answer = match answer {
        0 => Answer::Yes,
        1 => Answer::No,
        _ => Answer::Cancel,
    };
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(Pending::Answer(then)) = take_pending(request) {
            then(answer);
        }
    });
}

extern "system" fn native_opened<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    path: JString<'local>,
    uri: JString<'local>,
) {
    let (Ok(path), Ok(uri)) = (env.get_string(&path), env.get_string(&uri)) else {
        return;
    };
    let (path, uri) = (PathBuf::from(String::from(path)), String::from(uri));
    let _ = slint::invoke_from_event_loop(move || {
        remember_file(&path, uri);
        ON_OPEN.with(|cell| {
            if let Some(open) = cell.borrow().as_ref() {
                open(path);
            }
        });
    });
}

/// The activity's answer to a picker, on the event loop.
fn picked(request: i32, paths: Option<Vec<String>>, uris: Option<Vec<String>>) {
    let Some(Pending::Picked(then)) = take_pending(request) else {
        return;
    };
    let (Some(paths), Some(uris)) = (paths, uris) else {
        then(None);
        return;
    };
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    DOCUMENTS.with(|cell| {
        let mut docs = cell.borrow_mut();
        for (path, uri) in paths.iter().zip(uris) {
            if path.is_dir() {
                docs.folders.insert(path.clone(), uri);
            } else {
                docs.files.insert(path.clone(), uri);
            }
        }
    });
    save_documents();
    then(Some(paths));
}

fn start_pick(spec: &FileDialog, kind: i32, then: Box<dyn FnOnce(Option<Vec<PathBuf>>)>) {
    let mimes = spec.mime_types();
    let name = spec.file_name.clone();
    let request = next_request(Pending::Picked(then));
    let started = with_env(|env, activity| {
        let mimes = string_array(env, &mimes)?;
        let name = match &name {
            Some(name) => JObject::from(env.new_string(name)?),
            None => JObject::null(),
        };
        env.call_method(
            activity,
            "pick",
            "(II[Ljava/lang/String;Ljava/lang/String;)V",
            &[
                JValue::Int(request),
                JValue::Int(kind),
                JValue::Object(&mimes),
                JValue::Object(&name),
            ],
        )?;
        Ok(())
    });
    if started.is_none()
        && let Some(Pending::Picked(then)) = take_pending(request)
    {
        then(None);
    }
}

fn first(then: Box<dyn FnOnce(Option<PathBuf>)>) -> Box<dyn FnOnce(Option<Vec<PathBuf>>)> {
    Box::new(move |paths| then(paths.and_then(|paths| paths.into_iter().next())))
}

pub fn pick_file(spec: FileDialog, then: Box<dyn FnOnce(Option<PathBuf>)>) {
    start_pick(&spec, OPEN, first(then));
}

pub fn pick_files(spec: FileDialog, then: Box<dyn FnOnce(Option<Vec<PathBuf>>)>) {
    start_pick(&spec, OPEN_MANY, then);
}

pub fn pick_folder(spec: FileDialog, then: Box<dyn FnOnce(Option<PathBuf>)>) {
    start_pick(&spec, FOLDER, first(then));
}

pub fn save_file(spec: FileDialog, then: Box<dyn FnOnce(Option<PathBuf>)>) {
    start_pick(&spec, CREATE, first(then));
}

pub fn message(spec: MessageDialog, then: Box<dyn FnOnce(Answer)>) {
    let buttons = match spec.buttons {
        Buttons::Ok => 0,
        Buttons::YesNo => 1,
        Buttons::YesNoCancel => 2,
    };
    let request = next_request(Pending::Answer(then));
    let shown = with_env(|env, activity| {
        let text = env.new_string(&spec.description)?;
        env.call_method(
            activity,
            "message",
            "(ILjava/lang/String;I)V",
            &[
                JValue::Int(request),
                JValue::Object(&text),
                JValue::Int(buttons),
            ],
        )?;
        Ok(())
    });
    if shown.is_none() {
        eprintln!("podefa: {}", spec.description);
        if let Some(Pending::Answer(then)) = take_pending(request) {
            then(Answer::Cancel);
        }
    }
}

pub fn copy_text(text: &str) -> bool {
    with_env(|env, activity| {
        let text = env.new_string(text)?;
        env.call_method(
            activity,
            "copyText",
            "(Ljava/lang/String;)V",
            &[JValue::Object(&text)],
        )?;
        Ok(())
    })
    .is_some()
}

pub fn settings_dir() -> Option<PathBuf> {
    DIRS.get().map(|(files, _)| files.clone())
}

pub fn temp_dir() -> PathBuf {
    DIRS.get()
        .map_or_else(std::env::temp_dir, |(_, cache)| cache.clone())
}

/// Writes `path` back to the document it stands for, or into the picked folder it sits in.
pub fn written(path: &Path) {
    let target = DOCUMENTS.with(|cell| {
        let docs = cell.borrow();
        if let Some(uri) = docs.files.get(path) {
            return Some((uri.clone(), false));
        }
        let folder = path.parent()?;
        docs.folders.get(folder).map(|tree| (tree.clone(), true))
    });
    let Some((uri, in_folder)) = target else {
        return;
    };
    let path_text = path.to_string_lossy().into_owned();
    let mime = path.extension().map_or("application/octet-stream", |ext| {
        mime_for(&ext.to_string_lossy())
    });
    with_env(|env, activity| {
        let path = env.new_string(&path_text)?;
        let uri = env.new_string(&uri)?;
        if in_folder {
            let mime = env.new_string(mime)?;
            env.call_method(
                activity,
                "writeToFolder",
                "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V",
                &[
                    JValue::Object(&path),
                    JValue::Object(&uri),
                    JValue::Object(&mime),
                ],
            )?;
        } else {
            env.call_method(
                activity,
                "writeBack",
                "(Ljava/lang/String;Ljava/lang/String;)V",
                &[JValue::Object(&path), JValue::Object(&uri)],
            )?;
        }
        Ok(())
    });
}

pub fn shutdown() {}

fn remember_file(path: &Path, uri: String) {
    DOCUMENTS.with(|cell| cell.borrow_mut().files.insert(path.to_path_buf(), uri));
    save_documents();
}

fn documents_path() -> Option<PathBuf> {
    DIRS.get().map(|(files, _)| files.join("documents.txt"))
}

/// The map as lines of `F` or `D`, the cache path and the URI, tab-separated.
fn load_documents() {
    let Some(text) = documents_path().and_then(|path| std::fs::read_to_string(path).ok()) else {
        return;
    };
    DOCUMENTS.with(|cell| {
        let mut docs = cell.borrow_mut();
        for line in text.lines() {
            let mut parts = line.splitn(3, '\t');
            let (Some(kind), Some(path), Some(uri)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let map = if kind == "D" {
                &mut docs.folders
            } else {
                &mut docs.files
            };
            map.insert(PathBuf::from(path), uri.to_owned());
        }
    });
}

fn save_documents() {
    let Some(path) = documents_path() else {
        return;
    };
    let text = DOCUMENTS.with(|cell| {
        let docs = cell.borrow();
        let mut text = String::new();
        for (kind, map) in [("F", &docs.files), ("D", &docs.folders)] {
            for (path, uri) in map {
                text.push_str(&format!("{kind}\t{}\t{uri}\n", path.display()));
            }
        }
        text
    });
    if let Err(err) = std::fs::write(&path, text) {
        eprintln!("podefa: could not save {}: {err}", path.display());
    }
}
