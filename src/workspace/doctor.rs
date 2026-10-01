//! Build prerequisites, checked before a build instead of discovered one
//! failed build at a time: dx vs the project's Dioxus version, the Android
//! SDK / NDK / platform / Java, the Rust targets, Xcode — each with a fix
//! where one can be applied from here.

use super::*;
use std::path::Path;
use std::process::Command;

#[derive(Clone, PartialEq, Debug)]
pub(super) enum Fix {
    /// `rustup target add …`
    RustTargets(Vec<&'static str>),
    /// `cargo update -p dioxus --precise <version>` in the project
    DioxusVersion(String),
    /// Open Android Studio (SDK Manager is on its welcome screen)
    AndroidStudio,
    /// Rewrite Dioxus.toml's `[android]` for dx 0.7
    DioxusToml,
    /// Handled in another step
    GoTo(Step),
    /// Switch the Java card to this JDK ("" = Automatic); applied by the UI
    UseJava(String, u32),
}

impl Fix {
    pub(super) fn label(&self) -> String {
        match self {
            Fix::RustTargets(_) => "Install".into(),
            Fix::DioxusVersion(v) => format!("Use Dioxus {v}"),
            Fix::AndroidStudio => "Open Android Studio".into(),
            Fix::DioxusToml => "Fix Dioxus.toml".into(),
            Fix::GoTo(step) => format!("{} →", step.label()),
            Fix::UseJava(_, major) => format!("Use Java {major}"),
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub(super) struct DoctorItem {
    /// "Tools", "Android" or "iOS"
    pub section: &'static str,
    pub label: &'static str,
    pub ok: bool,
    pub detail: String,
    pub fix: Option<Fix>,
}

const ANDROID_RUST_TARGETS: [&str; 4] =
    ["aarch64-linux-android", "armv7-linux-androideabi", "x86_64-linux-android", "i686-linux-android"];
const IOS_RUST_TARGETS: [&str; 1] = ["aarch64-apple-ios"];

fn output(cmd: &mut Command) -> Option<String> {
    let out = cmd.output().ok()?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    out.status.success().then_some(text)
}

/// What `dx --version` found.
#[derive(Debug, PartialEq)]
pub(super) enum Dx {
    Missing,
    /// Another program named `dx` — Homebrew's deno ships one.
    Other(String),
    /// The Dioxus CLI, with its version.
    Dioxus(String),
}

/// "dioxus 0.7.10 (57d6794)" → `Dioxus("0.7.10")`; "deno 2.9.7 (…)" → `Other("deno")`
pub(super) fn dx_from(version_output: Option<&str>) -> Dx {
    let mut words = version_output.unwrap_or_default().split_whitespace();
    match (words.next(), words.next()) {
        (Some("dioxus"), Some(version)) => Dx::Dioxus(version.to_string()),
        (Some(name), _) => Dx::Other(name.to_string()),
        (None, _) => Dx::Missing,
    }
}

/// The `dioxus` version pinned in a Cargo.lock.
pub(super) fn locked_dioxus(lock: &str) -> Option<String> {
    let mut lines = lock.lines();
    while let Some(l) = lines.next() {
        if l.trim() == "name = \"dioxus\"" {
            return lines.next()?.trim().strip_prefix("version = ")?.trim_matches('"').to_string().into();
        }
    }
    None
}

/// Target API level from Dioxus.toml's `[android]` (dx 0.7 key), else 36 —
/// the level Google Play requires for updates.
pub(super) fn target_sdk(dioxus_toml: &str) -> u32 {
    let mut in_android = false;
    for l in dioxus_toml.lines() {
        let t = l.trim();
        if t.starts_with('[') {
            in_android = t == "[android]";
        } else if in_android {
            if let Some((k, v)) = t.split_once('=') {
                if k.trim() == "target_sdk" {
                    if let Ok(n) = v.trim().parse() {
                        return n;
                    }
                }
            }
        }
    }
    36
}

fn android_home() -> std::path::PathBuf {
    std::env::var("ANDROID_HOME")
        .map(Into::into)
        .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join("Library/Android/sdk"))
}

fn dir_entries(path: &Path) -> Vec<String> {
    std::fs::read_dir(path)
        .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| !n.starts_with('.')).collect())
        .unwrap_or_default()
}

/// Run every check that applies to this project. Blocking — call off the UI thread.
pub(super) fn run(dir: &Path, platform: &PlatformType, apple_identity: &str) -> Vec<DoctorItem> {
    let mut items = Vec::new();
    let installed_targets: Vec<String> = output(Command::new("rustup").args(["target", "list", "--installed"]))
        .map(|o| o.lines().map(str::to_string).collect())
        .unwrap_or_default();
    let missing = |wanted: &[&'static str]| -> Vec<&'static str> {
        wanted.iter().copied().filter(|t| !installed_targets.iter().any(|i| i == t)).collect()
    };

    // ── dx and the project's Dioxus version ──────────────────────────────
    let dx = dx_from(output(Command::new("dx").arg("--version")).as_deref());
    let lib = std::fs::read_to_string(dir.join("Cargo.lock")).ok().and_then(|l| locked_dioxus(&l));
    items.push(match (&dx, &lib) {
        (Dx::Missing, _) => DoctorItem {
            section: "Tools",
            label: "Dioxus CLI (dx)",
            ok: false,
            detail: "dx not found — install it with: cargo install dioxus-cli --locked".into(),
            fix: None,
        },
        (Dx::Other(name), _) => DoctorItem {
            section: "Tools",
            label: "Dioxus CLI (dx)",
            ok: false,
            detail: format!(
                "the dx found is {name}'s, not the Dioxus CLI — install it with: cargo install dioxus-cli --locked"
            ),
            fix: None,
        },
        (Dx::Dioxus(d), Some(l)) if d != l => DoctorItem {
            section: "Tools",
            label: "dx matches the project",
            ok: false,
            detail: format!("dx is {d}, the project uses dioxus {l} — dx refuses to bundle a different version"),
            fix: Some(Fix::DioxusVersion(d.clone())),
        },
        (Dx::Dioxus(d), _) => DoctorItem {
            section: "Tools",
            label: "dx matches the project",
            ok: true,
            detail: format!("dx {d}{}", lib.as_ref().map(|l| format!(" · dioxus {l}")).unwrap_or_default()),
            fix: None,
        },
    });

    // ── Android ──────────────────────────────────────────────────────────
    if platform.has_android() {
        let issues = consistency::dioxus_toml_issues(&std::fs::read_to_string(dir.join("Dioxus.toml")).unwrap_or_default());
        items.push(DoctorItem {
            section: "Android",
            label: "Dioxus.toml",
            ok: issues.is_empty(),
            detail: if issues.is_empty() { "[android] settings fine for dx 0.7".into() } else { issues.join(" · ") },
            fix: (!issues.is_empty()).then_some(Fix::DioxusToml),
        });
        let sdk = android_home();
        let target = target_sdk(&std::fs::read_to_string(dir.join("Dioxus.toml")).unwrap_or_default());
        let ndk = dir_entries(&sdk.join("ndk"));
        let platforms = dir_entries(&sdk.join("platforms"));
        let build_tools = dir_entries(&sdk.join("build-tools"));
        let (gradle, _) = java::project_gradle(dir);
        let env = load_env(&[dir.join(".env"), dir.join("fastlane").join(".env")]);
        let env_java = env.get("JAVA_HOME").cloned().or_else(|| std::env::var("JAVA_HOME").ok());
        let (jdk, java_notes) = java::for_project(dir, env_java.as_deref());
        let java_problem = match &jdk {
            None => Some(format!(
                "No Java found — install Java {} in the Java card below (Android Studio also ships one)",
                java::install_major(gradle)
            )),
            Some(j) => java::incompatibility(j, gradle)
                .map(|p| format!("{p}, and no installed Java fits — install Java {} in the Java card below", java::install_major(gradle))),
        };
        // A picked Java the build has to skip: one click puts the card back
        // on one that works.
        let java_fix = java::chosen()
            .filter(|c| java::incompatibility(c, gradle).is_some())
            .and(jdk.as_ref().filter(|_| java_problem.is_none()))
            .map(|j| Fix::UseJava(String::new(), j.major));
        let rust_missing = missing(&ANDROID_RUST_TARGETS);

        items.push(DoctorItem {
            section: "Android",
            label: "Android SDK",
            ok: sdk.is_dir(),
            detail: if sdk.is_dir() { sdk.display().to_string() } else { format!("Not found at {} — install Android Studio", sdk.display()) },
            fix: (!sdk.is_dir()).then_some(Fix::AndroidStudio),
        });
        items.push(DoctorItem {
            section: "Android",
            label: "NDK",
            ok: !ndk.is_empty(),
            detail: if ndk.is_empty() { "Missing — SDK Manager → SDK Tools → NDK (Side by side)".into() } else { ndk.join(", ") },
            fix: ndk.is_empty().then_some(Fix::AndroidStudio),
        });
        let has_platform = platforms.iter().any(|p| p == &format!("android-{target}") || p.starts_with(&format!("android-{target}.")));
        items.push(DoctorItem {
            section: "Android",
            label: "Target platform",
            ok: has_platform,
            detail: if has_platform {
                format!("Android API {target} installed")
            } else {
                format!("API {target} missing — SDK Manager → SDK Platforms")
            },
            fix: (!has_platform).then_some(Fix::AndroidStudio),
        });
        items.push(DoctorItem {
            section: "Android",
            label: "Build tools",
            ok: !build_tools.is_empty(),
            detail: if build_tools.is_empty() { "Missing — SDK Manager → SDK Tools".into() } else { build_tools.join(", ") },
            fix: build_tools.is_empty().then_some(Fix::AndroidStudio),
        });
        items.push(DoctorItem {
            section: "Android",
            label: "Java",
            ok: java_problem.is_none(),
            detail: match (&jdk, &java_problem) {
                (_, Some(p)) => p.clone(),
                (Some(j), None) if java_notes.is_empty() => {
                    format!("Java {} · {} · works with Gradle {}.{}", j.version, j.source, gradle.0, gradle.1)
                }
                (Some(_), None) => java_notes.join(" · "),
                (None, None) => unreachable!(),
            },
            fix: java_fix,
        });
        items.push(DoctorItem {
            section: "Android",
            label: "Rust Android targets",
            ok: rust_missing.is_empty(),
            detail: if rust_missing.is_empty() { "Installed".into() } else { format!("Missing: {}", rust_missing.join(", ")) },
            fix: (!rust_missing.is_empty()).then_some(Fix::RustTargets(rust_missing)),
        });
    }

    // ── iOS ──────────────────────────────────────────────────────────────
    if platform.has_ios() && cfg!(target_os = "macos") {
        let sdk = output(Command::new("xcrun").args(["--sdk", "iphoneos", "--show-sdk-version"]));
        let selected = output(Command::new("xcode-select").arg("-p")).unwrap_or_default();
        let detail = match &sdk {
            Some(v) => format!("iOS SDK {}", v.trim()),
            // A fresh Mac often has Xcode installed but the command line
            // still pointed at the standalone Command Line Tools.
            None if selected.contains("CommandLineTools") && Path::new("/Applications/Xcode.app").exists() => {
                "Xcode is installed but not selected — run: sudo xcode-select -s /Applications/Xcode.app/Contents/Developer".into()
            }
            None if Path::new("/Applications/Xcode.app").exists() => {
                "Xcode has no iOS platform — open Xcode → Settings → Components and install iOS".into()
            }
            None => "Xcode not found — install it from the App Store".into(),
        };
        items.push(DoctorItem { section: "iOS", label: "Xcode", ok: sdk.is_some(), detail, fix: None });
        let rust_missing = missing(&IOS_RUST_TARGETS);
        items.push(DoctorItem {
            section: "iOS",
            label: "Rust iOS target",
            ok: rust_missing.is_empty(),
            detail: if rust_missing.is_empty() { "Installed".into() } else { format!("Missing: {}", rust_missing.join(", ")) },
            fix: (!rust_missing.is_empty()).then_some(Fix::RustTargets(rust_missing)),
        });
        let identities = output(Command::new("security").args(["find-identity", "-v", "-p", "codesigning"])).unwrap_or_default();
        let found = !apple_identity.trim().is_empty() && identities.contains(&format!("\"{}\"", apple_identity.trim()));
        items.push(DoctorItem {
            section: "iOS",
            label: "Signing identity",
            ok: found,
            detail: if found {
                apple_identity.to_string()
            } else if apple_identity.trim().is_empty() {
                "None selected".into()
            } else {
                format!("\"{apple_identity}\" isn't a valid identity in the keychain")
            },
            fix: (!found).then_some(Fix::GoTo(Step::Accounts)),
        });
    }
    items
}

/// Apply a fix. Blocking; returns what happened.
pub(super) fn apply(dir: &Path, fix: &Fix) -> Result<String, String> {
    let done = |cmd: &mut Command| -> Result<(), String> {
        let out = cmd.output().map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("failed").to_string())
        }
    };
    match fix {
        Fix::RustTargets(targets) => {
            done(Command::new("rustup").arg("target").arg("add").args(targets))?;
            Ok(format!("Installed {}", targets.join(", ")))
        }
        Fix::DioxusVersion(v) => {
            done(Command::new("cargo").current_dir(dir).args(["update", "-p", "dioxus", "--precise", v]))?;
            Ok(format!("Project now uses dioxus {v} (Cargo.lock updated — commit it)"))
        }
        Fix::AndroidStudio => {
            done(Command::new("open").args(["-a", "Android Studio"]))?;
            Ok("Android Studio opened — use More Actions → SDK Manager".into())
        }
        Fix::DioxusToml => {
            let path = dir.join("Dioxus.toml");
            let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            std::fs::write(&path, consistency::fix_dioxus_toml(&text)).map_err(|e| e.to_string())?;
            Ok("Dioxus.toml updated".into())
        }
        Fix::GoTo(_) | Fix::UseJava(..) => Ok(String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions_and_target_sdk() {
        assert_eq!(dx_from(Some("dioxus 0.7.10 (57d6794)")), Dx::Dioxus("0.7.10".into()));
        assert_eq!(dx_from(Some("deno 2.9.7 (stable, release, aarch64-apple-darwin)")), Dx::Other("deno".into()));
        assert_eq!(dx_from(None), Dx::Missing);
        let lock = "[[package]]\nname = \"dioxus-core\"\nversion = \"0.7.4\"\n\n[[package]]\nname = \"dioxus\"\nversion = \"0.7.10\"\n";
        assert_eq!(locked_dioxus(lock).as_deref(), Some("0.7.10"));
        assert_eq!(locked_dioxus("").as_deref(), None);
        assert_eq!(target_sdk("[android]\nmin_sdk = 24\ntarget_sdk = 35\n[bundle]\ntarget_sdk = 99\n"), 35);
        assert_eq!(target_sdk("[bundle]\nidentifier = \"x\"\n"), 36);
    }
}
