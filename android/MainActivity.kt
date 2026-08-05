package dev.dioxus.main

import android.content.Intent

typealias BuildConfig = com.mayorana.appscreens.BuildConfig

// Subclasses the generated WryActivity only to forward onActivityResult into
// Rust — the Storage Access Framework picker in src/android_saf.rs launches
// an Intent via JNI, but the *result* can only be received by overriding this
// method on the Activity itself, so a plain WryActivity() isn't enough here.
class MainActivity : WryActivity() {
    external fun nativeOnActivityResult(requestCode: Int, resultCode: Int, uri: String?)

    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)

        val clip = data?.clipData
        val uri = if (clip != null && clip.itemCount > 0) {
            // Multi-select (EXTRA_ALLOW_MULTIPLE) reports picks via ClipData
            // rather than getData(); join them so the native side only needs
            // to parse one string.
            (0 until clip.itemCount).joinToString("\n") { clip.getItemAt(it).uri.toString() }
        } else {
            data?.data?.toString()
        }

        nativeOnActivityResult(requestCode, resultCode, uri)
    }
}
