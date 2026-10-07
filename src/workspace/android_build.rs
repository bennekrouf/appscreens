//! Android builds, run by AppScreens itself instead of a bash script, so they
//! work the same on macOS, Windows and Linux: `dx` generates the Gradle
//! project, AppScreens fixes what dx gets wrong (icons, package name,
//! version, signing), then Gradle builds the AAB or APK.
//!
//! The steps are the ones `build_android_release.sh` / `build_apk.sh` always
//! did. The text patches are plain functions so they can be tested; the
//! keystore password reaches Gradle through the environment and is never
//! written into the generated project.

use super::*;
use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// Where the SDK and its tools are
// ---------------------------------------------------------------------------

/// The Android SDK: ANDROID_HOME, ANDROID_SDK_ROOT, else where Android Studio
/// installs it on this OS.
pub(super) fn sdk_home() -> PathBuf {
    for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Some(v) = std::env::var_os(var).filter(|v| !v.is_empty()) {
            return PathBuf::from(v);
        }
    }
    default_sdk_home()
}

fn default_sdk_home() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_default();
    if cfg!(windows) {
        dirs::data_local_dir().unwrap_or_else(|| home.join("AppData/Local")).join("Android").join("Sdk")
    } else if cfg!(target_os = "macos") {
        home.join("Library/Android/sdk")
    } else {
        home.join("Android/Sdk")
    }
}

/// "26.1.10909125" → (26, 1, 10909125), for picking the newest version folder.
fn version_key(name: &str) -> Vec<u64> {
    name.split(['.', '-', '_']).map(|p| p.parse().unwrap_or(0)).collect()
}

/// The newest version-numbered folder in `dir` (NDK, build-tools).
pub(super) fn newest_version_dir(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .max_by_key(|n| version_key(n))
        .map(|n| dir.join(n))
}

/// A tool from an SDK folder: `zipalign.exe`, `apksigner.bat` on Windows.
fn sdk_tool(dir: &Path, name: &str, windows_ext: &str) -> PathBuf {
    if cfg!(windows) { dir.join(format!("{name}.{windows_ext}")) } else { dir.join(name) }
}

/// Gradle's wrapper in a generated project.
fn gradlew(build_dir: &Path) -> PathBuf {
    build_dir.join(if cfg!(windows) { "gradlew.bat" } else { "gradlew" })
}

/// The Dioxus CLI. Cargo's own copy first: Homebrew's deno installs a `dx`
/// too, and it can come first on PATH.
pub(super) fn dx_command() -> PathBuf {
    let exe = if cfg!(windows) { "dx.exe" } else { "dx" };
    let cargo_dx = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".cargo"))
        .join("bin")
        .join(exe);
    if cargo_dx.is_file() { cargo_dx } else { PathBuf::from("dx") }
}

/// Show a file in the system's file manager.
pub(super) fn reveal(path: &Path) {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg("-R").arg(path);
        c
    } else if cfg!(windows) {
        let mut c = Command::new("explorer");
        c.arg(format!("/select,{}", path.display()));
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(path.parent().unwrap_or(path));
        c
    };
    let _ = cmd.spawn();
}

// ---------------------------------------------------------------------------
// Text patches
// ---------------------------------------------------------------------------

/// Replace every `key = <value>` (spaces optional), as the scripts' sed did.
/// `quoted` values are written as `"value"`.
fn replace_assignments(text: &str, key: &str, value: &str, quoted: bool) -> String {
    let new_value = if quoted { format!("\"{value}\"") } else { value.to_string() };
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(key) {
        let (before, after_key) = rest.split_at(i);
        out.push_str(before);
        let after = &after_key[key.len()..];
        let trimmed = after.trim_start_matches(' ');
        // Only a whole word followed by `=`.
        let word_start = before.chars().last().is_none_or(|c| !c.is_alphanumeric() && c != '_');
        if word_start && trimmed.starts_with('=') {
            let value_part = trimmed[1..].trim_start_matches(' ');
            let old_len = if quoted {
                value_part.strip_prefix('"').and_then(|v| v.find('"').map(|e| e + 2))
            } else {
                Some(value_part.chars().take_while(|c| c.is_ascii_digit()).count())
            };
            if let Some(len) = old_len {
                out.push_str(key);
                out.push_str(" = ");
                out.push_str(&new_value);
                rest = &value_part[len..];
                continue;
            }
        }
        out.push_str(key);
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Dioxus.toml's `version_code` / `version_name`, kept in step with the build.
pub(super) fn set_dioxus_versions(toml: &str, code: u32, name: &str) -> String {
    let t = replace_assignments(toml, "version_code", &code.to_string(), false);
    replace_assignments(&t, "version_name", name, true)
}

/// The generated build.gradle.kts with the app's package and version.
pub(super) fn patch_gradle(gradle: &str, old_package: &str, package: &str, code: u32, name: &str) -> String {
    let g = gradle.replace(old_package, package);
    let g = replace_assignments(&g, "versionCode", &code.to_string(), false);
    replace_assignments(&g, "versionName", name, true)
}

/// The app's name instead of dx's `@string/app_name` placeholder.
pub(super) fn set_label(manifest: &str, app_name: &str) -> String {
    let escaped = app_name.replace('&', "&amp;").replace('"', "&quot;");
    manifest.replace("android:label=\"@string/app_name\"", &format!("android:label=\"{escaped}\""))
}

/// A Kotlin string literal: `\`, `"` and `$` (templates) escaped.
fn kotlin_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('$', "\\$"))
}

/// Add a release signing config that reads the passwords from the
/// environment, and use it for release builds. Unchanged if the project
/// already has one, or has no `buildTypes` block to attach it to.
pub(super) fn inject_signing(gradle: &str, keystore: &Path, alias: &str) -> String {
    if gradle.contains("signingConfigs") || !gradle.contains("buildTypes {") {
        return gradle.to_string();
    }
    // Forward slashes work on every OS and need no escaping.
    let store = keystore.to_string_lossy().replace('\\', "/");
    let mut out = Vec::new();
    for line in gradle.lines() {
        let indent = &line[..line.len() - line.trim_start().len()];
        if line.trim_start().starts_with("buildTypes {") {
            out.push(format!("{indent}signingConfigs {{"));
            out.push(format!("{indent}    create(\"release\") {{"));
            out.push(format!("{indent}        storeFile = file({})", kotlin_string(&store)));
            out.push(format!("{indent}        storePassword = System.getenv(\"ANDROID_KEYSTORE_PASSWORD\")"));
            out.push(format!("{indent}        keyAlias = {}", kotlin_string(alias)));
            out.push(format!(
                "{indent}        keyPassword = System.getenv(\"ANDROID_KEY_PASSWORD\") ?: System.getenv(\"ANDROID_KEYSTORE_PASSWORD\")"
            ));
            out.push(format!("{indent}    }}"));
            out.push(format!("{indent}}}"));
            out.push(line.to_string());
        } else if line.trim_start().starts_with("getByName(\"release\") {") {
            out.push(line.to_string());
            out.push(format!("{indent}    signingConfig = signingConfigs.getByName(\"release\")"));
        } else {
            out.push(line.to_string());
        }
    }
    let mut s = out.join("\n");
    if gradle.ends_with('\n') {
        s.push('\n');
    }
    s
}

/// What dx names the package by default: com.example.<Slug>.
pub(super) fn dx_default_package(slug: &str) -> String {
    let mut c = slug.chars();
    let title = c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default();
    format!("com.example.{title}")
}

// ---------------------------------------------------------------------------
// File fixes in the generated project
// ---------------------------------------------------------------------------

fn files_named(dir: &Path, matches: &dyn Fn(&Path) -> bool, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for p in entries.flatten().map(|e| e.path()) {
        if p.is_dir() {
            files_named(&p, matches, out);
        } else if matches(&p) {
            out.push(p);
        }
    }
}

fn copy_dir(src: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for e in std::fs::read_dir(src)?.flatten() {
        let (from, to) = (e.path(), dest.join(e.file_name()));
        if from.is_dir() { copy_dir(&from, &to)? } else { std::fs::copy(&from, &to).map(|_| ())? }
    }
    Ok(())
}

/// Replace dx's launcher icons with the project's own (manual_assets/android_icons).
/// The adaptive-icon XMLs go too: on Android 8+ they win over the PNGs and
/// show the default robot.
fn use_project_icons(project: &Path, res: &Path) -> Result<Option<String>, String> {
    let icons = project.join("manual_assets/android_icons");
    if !res.is_dir() || !icons.is_dir() {
        return Ok(None);
    }
    let mut stale = Vec::new();
    files_named(
        res,
        &|p| p.file_name().is_some_and(|n| n == "ic_launcher.webp" || n == "ic_launcher_round.webp"),
        &mut stale,
    );
    for f in stale {
        let _ = std::fs::remove_file(f);
    }
    for xml in ["ic_launcher.xml", "ic_launcher_round.xml"] {
        let _ = std::fs::remove_file(res.join("mipmap-anydpi-v26").join(xml));
    }
    for e in std::fs::read_dir(&icons).map_err(|e| e.to_string())?.flatten() {
        if e.path().is_dir() && e.file_name().to_string_lossy().starts_with("mipmap-") {
            copy_dir(&e.path(), &res.join(e.file_name())).map_err(|e| e.to_string())?;
        }
    }
    Ok(Some("Icons replaced with manual_assets/android_icons".into()))
}

/// Package name in Kotlin sources and the manifest, and the app's label.
fn fix_sources(src: &Path, old_package: &str, package: &str, app_name: &str) -> Result<(), String> {
    let mut files = Vec::new();
    files_named(
        src,
        &|p| p.extension().is_some_and(|e| e == "kt") || p.file_name().is_some_and(|n| n == "AndroidManifest.xml"),
        &mut files,
    );
    for f in files {
        let text = std::fs::read_to_string(&f).map_err(|e| e.to_string())?;
        let mut new = text.replace(old_package, package);
        if f.file_name().is_some_and(|n| n == "AndroidManifest.xml") && !app_name.is_empty() {
            new = set_label(&new, app_name);
        }
        if new != text {
            std::fs::write(&f, new).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The builds
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Kind {
    /// Signed AAB for Google Play
    Release,
    /// Debug-signed APK to install on a device
    DebugApk,
}

impl Kind {
    /// The script this build replaces — what the Build step's buttons name.
    pub(super) fn from_script(name: &str) -> Option<Kind> {
        match name {
            "build_android_release.sh" => Some(Kind::Release),
            "build_apk.sh" => Some(Kind::DebugApk),
            _ => None,
        }
    }
}

pub(super) struct Job {
    pub kind: Kind,
    pub app_name: String,
    pub slug: String,
    pub package: String,
    pub version_name: String,
    pub version_code: u32,
    pub keystore: Option<PathBuf>,
    pub alias: String,
    /// Environment for dx and Gradle: the project's .env, signing, Java.
    pub env: Vec<(String, String)>,
}

impl Job {
    fn env(&self, key: &str) -> Option<&str> {
        self.env.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v.as_str()).filter(|v| !v.is_empty())
    }
}

fn command(program: &Path, dir: &Path, job: &Job, sdk: &Path, ndk: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.quiet();
    cmd.current_dir(dir).env("CI", "1");
    for (k, v) in &job.env {
        cmd.env(k, v);
    }
    cmd.env("ANDROID_HOME", sdk).env("ANDROID_SDK_ROOT", sdk).env("ANDROID_NDK_HOME", ndk);
    cmd
}

/// Run the build, logging as it goes. Returns the file it produced.
pub(super) async fn run(project: PathBuf, job: Job, mut log: Signal<Vec<String>>) -> Result<PathBuf, String> {
    let mut say = move |s: String| log.write().push(s);
    let sdk = sdk_home();
    let ndk = newest_version_dir(&sdk.join("ndk"))
        .ok_or_else(|| format!("Android NDK not found in {} — install it in Android Studio: SDK Manager → SDK Tools → NDK (Side by side).", sdk.join("ndk").display()))?;
    say(format!("🚀 Android {} build", if job.kind == Kind::Release { "release (AAB)" } else { "debug (APK)" }));
    say(format!("   SDK {} · NDK {}", sdk.display(), ndk.file_name().unwrap_or_default().to_string_lossy()));

    // dx refuses to bundle a project whose Dioxus library is a different version.
    let dx = dx_command();
    let version = Command::new(&dx).quiet().arg("--version").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).to_string());
    let lib = std::fs::read_to_string(project.join("Cargo.lock")).ok().and_then(|l| doctor::locked_dioxus(&l));
    match (doctor::dx_from(version.as_deref()), &lib) {
        (doctor::Dx::Missing, _) => return Err("dx not found — install it with: cargo install dioxus-cli --locked".into()),
        (doctor::Dx::Other(name), _) => {
            return Err(format!("the dx found is {name}'s, not the Dioxus CLI — install it with: cargo install dioxus-cli --locked"))
        }
        (doctor::Dx::Dioxus(d), Some(l)) if d != *l => {
            return Err(format!("dx is {d} but this project uses dioxus {l} — the Doctor above can switch the project to {d}."))
        }
        _ => {}
    }

    let keystore = if job.kind == Kind::Release {
        let ks = job.keystore.clone().filter(|_| job.env("ANDROID_KEYSTORE_PASSWORD").is_some()).ok_or(
            "Android signing is not set up — Accounts → Android upload key (the password stays in your system's password store).",
        )?;
        if !ks.is_file() {
            return Err(format!("Keystore not found: {}", ks.display()));
        }
        Some(ks)
    } else {
        None
    };

    let android_dir = project.join(format!("target/dx/{}/release/android", job.slug));
    let build_dir = android_dir.join("app");
    let gradle_file = build_dir.join("app/build.gradle.kts");
    say("🧹 Cleaning the previous Android build…".into());
    let _ = std::fs::remove_dir_all(&android_dir);

    // 1. dx generates the Gradle project. For the release build its own final
    //    Gradle step can fail (duplicate icons) — that's fixed and rebuilt
    //    below; what has to exist is the project.
    let mut dx_cmd = command(&dx, &project, &job, &sdk, &ndk);
    match job.kind {
        Kind::Release => {
            dx_cmd.args(["bundle", "--platform", "android", "--release"]);
            say("📦 dx bundle --platform android --release".into());
            if let Err(e) = jobs::stream_command(dx_cmd, "dx bundle", log).await {
                say(format!("   ({e} — continuing, the Gradle project is fixed and rebuilt below)"));
            }
        }
        Kind::DebugApk => {
            dx_cmd.args(["build", "--platform", "android", "--release"]);
            say("📦 dx build --platform android --release".into());
            jobs::stream_command(dx_cmd, "dx build", log).await?;
        }
    }
    if !gradle_file.is_file() {
        return Err("dx stopped before generating the Android project — see its error above.".into());
    }

    // 2. Icons, package name, version and signing.
    if let Some(m) = use_project_icons(&project, &build_dir.join("app/src/main/res"))? {
        say(format!("🎨 {m}"));
    }
    let old_package = dx_default_package(&job.slug);
    let mut gradle = std::fs::read_to_string(&gradle_file).map_err(|e| e.to_string())?;
    if job.kind == Kind::Release {
        gradle = patch_gradle(&gradle, &old_package, &job.package, job.version_code, &job.version_name);
        let toml_path = project.join("Dioxus.toml");
        if let Ok(toml) = std::fs::read_to_string(&toml_path) {
            let new = set_dioxus_versions(&toml, job.version_code, &job.version_name);
            if new != toml {
                std::fs::write(&toml_path, new).map_err(|e| e.to_string())?;
            }
        }
        if let Some(ks) = &keystore {
            gradle = inject_signing(&gradle, ks, &job.alias);
        }
        say(format!("🔧 {} · version {} ({})", job.package, job.version_name, job.version_code));
    } else {
        gradle = gradle.replace(&old_package, &job.package);
        say(format!("🔧 {}", job.package));
    }
    std::fs::write(&gradle_file, gradle).map_err(|e| e.to_string())?;
    fix_sources(&build_dir.join("app/src"), &old_package, &job.package, &job.app_name)?;

    // 3. Gradle.
    let mut gradle_cmd = command(&gradlew(&build_dir), &build_dir, &job, &sdk, &ndk);
    match job.kind {
        Kind::Release => gradle_cmd.arg("bundleRelease"),
        Kind::DebugApk => gradle_cmd.args(["clean", "assembleDebug"]),
    };
    say(format!("🏗️  Gradle {}", if job.kind == Kind::Release { "bundleRelease" } else { "assembleDebug" }));
    jobs::stream_command(gradle_cmd, "Gradle", log).await?;

    // 4. The result, next to the project's other bundles.
    match job.kind {
        Kind::Release => {
            let aab = build_dir.join("app/build/outputs/bundle/release/app-release.aab");
            if !aab.is_file() {
                return Err("Gradle finished but app-release.aab is missing".into());
            }
            let target = project.join(format!("{}_release.aab", job.slug));
            std::fs::copy(&aab, &target).map_err(|e| e.to_string())?;
            let verified = Command::new(signing::jdk_tool("jarsigner"))
                .quiet()
                .args(["-verify"])
                .arg(&target)
                .output()
                .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("jar verified"));
            if !verified {
                return Err(format!("{} was built but its signature doesn't verify", target.display()));
            }
            say(format!("✅ Signed AAB: {}", target.display()));
            reveal(&target);
            Ok(target)
        }
        Kind::DebugApk => {
            let mut apks = Vec::new();
            files_named(&build_dir.join("app/build/outputs/apk"), &|p| p.extension().is_some_and(|e| e == "apk"), &mut apks);
            apks.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
            let apk = apks.pop().ok_or("Gradle finished but no APK was produced")?;
            let target = project.join(format!("{}-debug.apk", job.slug));
            let signed = sign_debug_apk(&apk, &sdk, &target, &mut say);
            if !signed {
                std::fs::copy(&apk, &target).map_err(|e| e.to_string())?;
            }
            say(format!("✅ APK: {}", target.display()));
            reveal(&target);
            Ok(target)
        }
    }
}

/// zipalign + apksigner with the debug keystore Gradle created, for an APK
/// that installs on any device. False if the tools or key aren't there.
fn sign_debug_apk(apk: &Path, sdk: &Path, target: &Path, say: &mut impl FnMut(String)) -> bool {
    let Some(tools) = newest_version_dir(&sdk.join("build-tools")) else { return false };
    let debug_ks = dirs::home_dir().unwrap_or_default().join(".android/debug.keystore");
    if !debug_ks.is_file() {
        return false;
    }
    let aligned = apk.with_file_name("app-aligned.apk");
    let _ = std::fs::remove_file(&aligned);
    let ok = |c: &mut Command| c.output().is_ok_and(|o| o.status.success());
    if !ok(Command::new(sdk_tool(&tools, "zipalign", "exe")).quiet().args(["-p", "4"]).arg(apk).arg(&aligned)) {
        return false;
    }
    let signed = ok(Command::new(sdk_tool(&tools, "apksigner", "bat"))
        .quiet()
        .args(["sign", "--ks"])
        .arg(&debug_ks)
        .args(["--ks-pass", "pass:android", "--key-pass", "pass:android", "--out"])
        .arg(target)
        .arg(&aligned));
    let _ = std::fs::remove_file(&aligned);
    if signed {
        say("✍️  Aligned and signed with the debug key".into());
    }
    signed
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRADLE: &str = r#"android {
    namespace = "com.example.Nahw"
    defaultConfig {
        applicationId = "com.example.Nahw"
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
    }
    buildTypes {
        getByName("release") {
            isMinifyEnabled = false
        }
    }
}
"#;

    #[test]
    fn patches_package_and_version_like_the_scripts_did() {
        let g = patch_gradle(GRADLE, &dx_default_package("nahw"), "com.mayorana.nahw", 7, "1.2.0");
        assert!(g.contains("applicationId = \"com.mayorana.nahw\"") && g.contains("namespace = \"com.mayorana.nahw\""));
        assert!(g.contains("versionCode = 7") && g.contains("versionName = \"1.2.0\""));
        assert!(!g.contains("com.example"));
        let toml = "[android]\nversion_code=3\nversion_name = \"1.0\"\nmin_sdk = 24\n";
        assert_eq!(set_dioxus_versions(toml, 7, "1.2.0"), "[android]\nversion_code = 7\nversion_name = \"1.2.0\"\nmin_sdk = 24\n");
        // Other keys that merely contain the name are left alone.
        assert_eq!(replace_assignments("myversionCode = 3", "versionCode", "9", false), "myversionCode = 3");
    }

    #[test]
    fn signing_reads_passwords_from_the_environment() {
        let g = inject_signing(GRADLE, Path::new(r"C:\Users\me\keys\upload.jks"), "nahw");
        assert!(g.contains("storeFile = file(\"C:/Users/me/keys/upload.jks\")"), "{g}");
        assert!(g.contains("storePassword = System.getenv(\"ANDROID_KEYSTORE_PASSWORD\")"));
        assert!(g.contains("keyAlias = \"nahw\""));
        let config = g.find("signingConfigs {").unwrap();
        assert!(config < g.find("buildTypes {").unwrap(), "declared before it's used");
        assert!(g.contains("getByName(\"release\") {\n            signingConfig = signingConfigs.getByName(\"release\")"));
        // Idempotent, and a `$` can't open a Kotlin template.
        assert_eq!(inject_signing(&g, Path::new("/x"), "a"), g);
        assert!(inject_signing(GRADLE, Path::new("/k/$HOME.jks"), "a").contains("file(\"/k/\\$HOME.jks\")"));
    }

    #[test]
    fn labels_and_default_packages() {
        assert_eq!(dx_default_package("nahw"), "com.example.Nahw");
        assert_eq!(
            set_label(r#"<application android:label="@string/app_name">"#, "Tafseel & Co"),
            r#"<application android:label="Tafseel &amp; Co">"#
        );
    }

    #[test]
    fn picks_the_newest_version_folder() {
        let dir = std::env::temp_dir().join("appscreens-test-ndk");
        let _ = std::fs::remove_dir_all(&dir);
        for v in ["25.2.9519653", "26.1.10909125", "26.10.1", "notes"] {
            std::fs::create_dir_all(dir.join(v)).unwrap();
        }
        assert_eq!(newest_version_dir(&dir), Some(dir.join("26.10.1")));
        assert_eq!(newest_version_dir(&dir.join("missing")), None);
    }

    #[test]
    fn replaces_icons_and_fixes_sources() {
        let p = std::env::temp_dir().join("appscreens-test-android-fix");
        let _ = std::fs::remove_dir_all(&p);
        let res = p.join("gen/res");
        std::fs::create_dir_all(res.join("mipmap-hdpi")).unwrap();
        std::fs::create_dir_all(res.join("mipmap-anydpi-v26")).unwrap();
        std::fs::write(res.join("mipmap-hdpi/ic_launcher.webp"), "dx").unwrap();
        std::fs::write(res.join("mipmap-anydpi-v26/ic_launcher.xml"), "adaptive").unwrap();
        std::fs::create_dir_all(p.join("manual_assets/android_icons/mipmap-hdpi")).unwrap();
        std::fs::write(p.join("manual_assets/android_icons/mipmap-hdpi/ic_launcher.png"), "mine").unwrap();
        assert!(use_project_icons(&p, &res).unwrap().is_some());
        assert!(!res.join("mipmap-hdpi/ic_launcher.webp").exists());
        assert!(!res.join("mipmap-anydpi-v26/ic_launcher.xml").exists());
        assert_eq!(std::fs::read_to_string(res.join("mipmap-hdpi/ic_launcher.png")).unwrap(), "mine");

        let src = p.join("gen/src");
        std::fs::create_dir_all(src.join("main/kotlin")).unwrap();
        std::fs::write(src.join("main/kotlin/Main.kt"), "package com.example.Nahw\n").unwrap();
        std::fs::write(src.join("main/AndroidManifest.xml"), r#"<manifest package="com.example.Nahw"><application android:label="@string/app_name"/></manifest>"#).unwrap();
        fix_sources(&src, "com.example.Nahw", "com.mayorana.nahw", "Nahw").unwrap();
        assert_eq!(std::fs::read_to_string(src.join("main/kotlin/Main.kt")).unwrap(), "package com.mayorana.nahw\n");
        let m = std::fs::read_to_string(src.join("main/AndroidManifest.xml")).unwrap();
        assert!(m.contains("com.mayorana.nahw") && m.contains("android:label=\"Nahw\""));
    }
}
