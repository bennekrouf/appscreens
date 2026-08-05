//! Storage Access Framework bridge for Android.
//!
//! `rfd` has no Android backend at all — SAF only ever hands out opaque
//! `content://` URIs, never a filesystem path, so rfd's "pick a path" model
//! has nowhere to attach on this platform. This hand-rolls the one thing
//! AppScreens actually needs from it: picking one or more images and
//! reading their bytes, via JNI, so the caller can copy them into the
//! project's own directory (see `app_private_projects_root`) and never touch
//! a `content://` URI again afterward.
//!
//! Launching the picker `Intent` is a plain JNI call and needs nothing
//! special. Receiving the *result* does: `startActivityForResult`'s answer
//! only reaches `Activity.onActivityResult`, which the stock generated
//! `MainActivity` doesn't expose, so `android/MainActivity.kt` subclasses it
//! (via Dioxus.toml's `android_main_activity`) to forward the callback into
//! `nativeOnActivityResult` below.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, OnceLock};

use jni::objects::{JObject, JString, JValue};
use jni::sys::jint;
use jni::{JNIEnv, JavaVM};

const RESULT_OK: jint = -1;

static NEXT_REQUEST_CODE: AtomicI32 = AtomicI32::new(1000);
static PENDING: OnceLock<Mutex<HashMap<jint, tokio::sync::oneshot::Sender<Option<String>>>>> =
    OnceLock::new();

fn pending() -> &'static Mutex<HashMap<jint, tokio::sync::oneshot::Sender<Option<String>>>> {
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Called by `MainActivity.onActivityResult` (see android/MainActivity.kt).
/// `uri` is `\n`-joined when multiple documents were picked, null on cancel.
#[no_mangle]
pub extern "system" fn Java_dev_dioxus_main_MainActivity_nativeOnActivityResult(
    mut env: JNIEnv,
    _this: JObject,
    request_code: jint,
    result_code: jint,
    uri: JString,
) {
    let Some(sender) = pending().lock().unwrap().remove(&request_code) else { return };
    let value = if result_code != RESULT_OK || uri.as_raw().is_null() {
        None
    } else {
        env.get_string(&uri).ok().map(|s| s.into())
    };
    let _ = sender.send(value);
}

/// The process-wide `JavaVM` handle. `ndk-context`'s underlying pointer is
/// stable for the app's whole lifetime (set once at startup by `tao`'s
/// Android backend, underneath `dioxus-desktop`), so this only needs
/// wrapping once rather than on every call.
fn java_vm() -> &'static JavaVM {
    static VM: OnceLock<JavaVM> = OnceLock::new();
    VM.get_or_init(|| {
        let ctx = ndk_context::android_context();
        unsafe { JavaVM::from_raw(ctx.vm().cast()) }.expect("no JavaVM set by the Android runtime")
    })
}

/// Attaches the current thread and hands back the env plus the app's
/// `Activity` (which `ndk-context` exposes as a generic `Context` pointer).
fn attach() -> jni::errors::Result<(jni::AttachGuard<'static>, JObject<'static>)> {
    let env = java_vm().attach_current_thread()?;
    let ctx = ndk_context::android_context();
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
    Ok((env, activity))
}

/// Launches `ACTION_OPEN_DOCUMENT` and returns the request code it was
/// launched under, or `None` if any JNI step failed.
fn launch_picker(mime_type: &str, allow_multiple: bool) -> Option<jint> {
    let (mut env, activity) = attach().ok()?;
    let request_code = NEXT_REQUEST_CODE.fetch_add(1, Ordering::Relaxed);

    let result: jni::errors::Result<()> = (|| {
        let action = env.new_string("android.intent.action.OPEN_DOCUMENT")?;
        let intent_class = env.find_class("android/content/Intent")?;
        let intent = env.new_object(intent_class, "(Ljava/lang/String;)V", &[JValue::Object(&action)])?;

        let mime = env.new_string(mime_type)?;
        env.call_method(
            &intent,
            "setType",
            "(Ljava/lang/String;)Landroid/content/Intent;",
            &[JValue::Object(&mime)],
        )?;

        let category = env.new_string("android.intent.category.OPENABLE")?;
        env.call_method(
            &intent,
            "addCategory",
            "(Ljava/lang/String;)Landroid/content/Intent;",
            &[JValue::Object(&category)],
        )?;

        if allow_multiple {
            let extra = env.new_string("android.intent.extra.ALLOW_MULTIPLE")?;
            env.call_method(
                &intent,
                "putExtra",
                "(Ljava/lang/String;Z)Landroid/content/Intent;",
                &[JValue::Object(&extra), JValue::Bool(1)],
            )?;
        }

        env.call_method(
            &activity,
            "startActivityForResult",
            "(Landroid/content/Intent;I)V",
            &[JValue::Object(&intent), JValue::Int(request_code)],
        )?;
        Ok(())
    })();

    result.ok().map(|_| request_code)
}

/// Reads the bytes behind a `content://` URI plus a best-guess file
/// extension (from the document's reported MIME type), via
/// `ContentResolver.openInputStream`.
fn read_uri(uri_str: &str) -> Option<(String, Vec<u8>)> {
    let (mut env, activity) = attach().ok()?;

    let result: jni::errors::Result<(String, Vec<u8>)> = (|| {
        let uri_jstr = env.new_string(uri_str)?;
        let uri_class = env.find_class("android/net/Uri")?;
        let uri = env
            .call_static_method(
                uri_class,
                "parse",
                "(Ljava/lang/String;)Landroid/net/Uri;",
                &[JValue::Object(&uri_jstr)],
            )?
            .l()?;

        let resolver = env
            .call_method(
                &activity,
                "getContentResolver",
                "()Landroid/content/ContentResolver;",
                &[],
            )?
            .l()?;

        let mime = env
            .call_method(&resolver, "getType", "(Landroid/net/Uri;)Ljava/lang/String;", &[JValue::Object(&uri)])?
            .l()?;
        let ext = if mime.as_raw().is_null() {
            "png".to_string()
        } else {
            let mime: String = env.get_string(&JString::from(mime))?.into();
            match mime.as_str() {
                "image/jpeg" => "jpg".to_string(),
                "image/webp" => "webp".to_string(),
                "image/gif" => "gif".to_string(),
                _ => "png".to_string(),
            }
        };

        let stream = env
            .call_method(
                &resolver,
                "openInputStream",
                "(Landroid/net/Uri;)Ljava/io/InputStream;",
                &[JValue::Object(&uri)],
            )?
            .l()?;

        let mut bytes = Vec::new();
        let buf = env.new_byte_array(8192)?;
        let mut chunk = [0i8; 8192];
        loop {
            let n = env
                .call_method(&stream, "read", "([B)I", &[JValue::Object(&buf)])?
                .i()?;
            if n < 0 {
                break;
            }
            env.get_byte_array_region(&buf, 0, &mut chunk[..n as usize])?;
            bytes.extend(chunk[..n as usize].iter().map(|&b| b as u8));
        }
        env.call_method(&stream, "close", "()V", &[])?;

        Ok((ext, bytes))
    })();

    let (ext, bytes) = result.ok()?;
    let name = format!(
        "picked_{}.{ext}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default()
    );
    Some((name, bytes))
}

/// Picks one (`allow_multiple: false`) or more image documents and returns
/// each as `(synthetic filename, bytes)`. Empty on cancel or any failure.
pub async fn pick_images(allow_multiple: bool) -> Vec<(String, Vec<u8>)> {
    let Some(request_code) = launch_picker("image/*", allow_multiple) else {
        return Vec::new();
    };

    let (tx, rx) = tokio::sync::oneshot::channel();
    pending().lock().unwrap().insert(request_code, tx);

    let Ok(Some(joined)) = rx.await else {
        return Vec::new();
    };

    joined.lines().filter_map(read_uri).collect()
}

/// A directory the app fully owns (Android's per-app private storage —
/// `Context.getFilesDir()`), used as the root all projects live under on
/// Android instead of a user-picked folder. Plain `std::fs` works on it
/// exactly like any other path; no SAF/JNI needed past this point.
pub fn app_private_projects_root() -> PathBuf {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let path: jni::errors::Result<String> = (|| {
            let (mut env, activity) = attach()?;
            let files_dir = env.call_method(&activity, "getFilesDir", "()Ljava/io/File;", &[])?.l()?;
            let path_jstr = env
                .call_method(&files_dir, "getAbsolutePath", "()Ljava/lang/String;", &[])?
                .l()?;
            Ok(env.get_string(&JString::from(path_jstr))?.into())
        })();

        // getFilesDir() cannot realistically fail on a running app; this
        // fallback only matters if it somehow does.
        let root = PathBuf::from(path.unwrap_or_else(|_| "/data/data/com.mayorana.appscreens/files".into()))
            .join("projects");
        let _ = std::fs::create_dir_all(&root);
        root
    })
    .clone()
}
