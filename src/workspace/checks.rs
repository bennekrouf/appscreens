//! Local, offline checks behind the Setup and Build steps: what a
//! provisioning profile actually allows, whether a version string is one the
//! stores accept, and seeding per-project release numbers from the files that
//! used to own them.

use super::*;

// ---------------------------------------------------------------------------
// Provisioning profiles
// ---------------------------------------------------------------------------
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ProfileKind {
    AppStore,
    AdHoc,
    Development,
    Enterprise,
}

impl ProfileKind {
    pub(super) fn label(self) -> &'static str {
        match self {
            ProfileKind::AppStore => "App Store",
            ProfileKind::AdHoc => "Ad Hoc",
            ProfileKind::Development => "Development",
            ProfileKind::Enterprise => "Enterprise",
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub(super) struct ProfileInfo {
    pub path: String,
    pub name: String,
    /// Bundle ID pattern without the team prefix, e.g. "com.acme.app" or "*"
    pub app_id: String,
    pub team_id: String,
    /// "YYYY-MM-DD"
    pub expires: String,
    pub kind: ProfileKind,
}

impl ProfileInfo {
    pub(super) fn matches_bundle(&self, bundle_id: &str) -> bool {
        match self.app_id.strip_suffix('*') {
            Some(prefix) => bundle_id.starts_with(prefix),
            None => self.app_id == bundle_id,
        }
    }

    pub(super) fn expired(&self) -> bool {
        !self.expires.is_empty() && self.expires.as_str() < utc_today().as_str()
    }
}

/// Value of the first `<string>`/`<date>` after `<key>{key}</key>` in the
/// plist embedded in a profile. The profile is a CMS blob, but that plist is
/// stored as plain XML inside it.
pub(super) fn plist_value(text: &str, key: &str) -> Option<String> {
    let after = &text[text.find(&format!("<key>{key}</key>"))?..];
    let (open, close) = ["<string>", "<date>"]
        .into_iter()
        .filter_map(|tag| after.find(tag).map(|i| (i, tag)))
        .min()
        .map(|(_, tag)| if tag == "<string>" { ("<string>", "</string>") } else { ("<date>", "</date>") })?;
    let start = after.find(open)? + open.len();
    let end = after[start..].find(close)?;
    Some(after[start..start + end].trim().to_string())
}

fn plist_true(text: &str, key: &str) -> bool {
    text.find(&format!("<key>{key}</key>"))
        .and_then(|i| text[i..].split_once("</key>").map(|(_, rest)| rest.trim_start().starts_with("<true/>")))
        .unwrap_or(false)
}

pub(super) fn read_profile(path: &std::path::Path) -> Option<ProfileInfo> {
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let team_id = plist_value(&text, "TeamIdentifier").unwrap_or_default();
    let full_app_id = plist_value(&text, "application-identifier").unwrap_or_default();
    let app_id = full_app_id
        .strip_prefix(&format!("{team_id}."))
        .map(str::to_string)
        .unwrap_or(full_app_id);
    let kind = if plist_true(&text, "get-task-allow") {
        ProfileKind::Development
    } else if plist_true(&text, "ProvisionsAllDevices") {
        ProfileKind::Enterprise
    } else if text.contains("<key>ProvisionedDevices</key>") {
        ProfileKind::AdHoc
    } else {
        ProfileKind::AppStore
    };
    Some(ProfileInfo {
        path: path.to_string_lossy().to_string(),
        name: plist_value(&text, "Name")
            .unwrap_or_else(|| path.file_stem().unwrap_or_default().to_string_lossy().to_string()),
        app_id,
        team_id,
        expires: plist_value(&text, "ExpirationDate").map(|d| d.chars().take(10).collect()).unwrap_or_default(),
        kind,
    })
}

/// Every installed profile that could be read.
pub(super) fn discover_profiles() -> Vec<ProfileInfo> {
    let dir = dirs::home_dir()
        .unwrap_or_default()
        .join("Library/MobileDevice/Provisioning Profiles");
    let Ok(entries) = std::fs::read_dir(&dir) else { return vec![] };
    entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("mobileprovision"))
        .filter_map(|p| read_profile(&p))
        .collect()
}

/// Team ID from an identity like "Apple Distribution: Jane Doe (AB12CD34EF)".
fn identity_team(identity: &str) -> Option<&str> {
    let open = identity.rfind('(')?;
    let close = identity.rfind(')')?;
    (close > open).then(|| &identity[open + 1..close])
}

/// Checks for the Accounts step: does this profile fit this app, this
/// identity, and an App Store upload?
pub(super) fn profile_checks(profile: &ProfileInfo, bundle_id: &str, identity: &str) -> Vec<CredCheck> {
    let mut checks = vec![
        CredCheck {
            label: "Matches bundle ID",
            ok: profile.matches_bundle(bundle_id),
            detail: format!("Profile is for {} · app is {}", profile.app_id, if bundle_id.is_empty() { "not set" } else { bundle_id }),
        },
        CredCheck {
            label: "Not expired",
            ok: !profile.expired(),
            detail: if profile.expires.is_empty() { "No expiry date found".into() } else { format!("Expires {}", profile.expires) },
        },
        CredCheck {
            label: "App Store distribution",
            ok: profile.kind == ProfileKind::AppStore,
            detail: format!("{} profile", profile.kind.label()),
        },
    ];
    if let Some(team) = identity_team(identity) {
        checks.push(CredCheck {
            label: "Same team as signing identity",
            ok: team == profile.team_id,
            detail: format!("Profile team {} · identity team {team}", profile.team_id),
        });
    }
    checks
}

/// Today's date in UTC as "YYYY-MM-DD".
fn utc_today() -> String {
    utc_now()[..10].to_string()
}

/// Now in UTC as "YYYY-MM-DD HH:MM" (civil-from-days, no date crate).
pub(super) fn utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let (hh, mm) = ((secs % 86_400) / 3600, (secs % 3600) / 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}")
}

// ---------------------------------------------------------------------------
// Versions
// ---------------------------------------------------------------------------

/// One to three dot-separated integers — what both CFBundleShortVersionString
/// and App Store Connect accept.
pub(crate) fn valid_version(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    (1..=3).contains(&parts.len())
        && parts.iter().all(|p| !p.is_empty() && p.len() <= 9 && p.chars().all(|c| c.is_ascii_digit()))
}

/// Bump component `idx` (0 = major) of a version, zeroing the ones after it.
pub(super) fn bump_version(v: &str, idx: usize) -> String {
    let mut parts: Vec<u64> = v.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    parts.resize(3, 0);
    parts[idx] += 1;
    for p in parts.iter_mut().skip(idx + 1) {
        *p = 0;
    }
    parts.iter().map(u64::to_string).collect::<Vec<_>>().join(".")
}

/// `key = value` from a TOML file, unquoted — enough for Dioxus.toml's
/// `version_code` / `version_name`.
fn toml_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
    })
}

/// `key = value` inside `[section]` only, unquoted.
fn toml_in(text: &str, section: &str, key: &str) -> Option<String> {
    let mut inside = false;
    text.lines().find_map(|line| {
        let t = line.trim();
        if t.starts_with('[') {
            inside = t == format!("[{section}]");
            return None;
        }
        let (k, v) = t.split_once('=')?;
        (inside && k.trim() == key).then(|| v.split('#').next().unwrap_or("").trim().trim_matches('"').to_string())
    })
    .filter(|v| !v.is_empty())
}

/// `NAME="value"` from a shell script, when it's a plain value (no `$…`).
fn shell_var(text: &str, name: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let v = line.trim().strip_prefix(&format!("{name}="))?;
        let v = v.split(" #").next().unwrap_or(v).trim().trim_matches('"').trim_matches('\'');
        (!v.is_empty() && !v.contains('$') && !v.contains('{')).then(|| v.to_string())
    })
}

/// `app_identifier("…")` / `package_name("…")` from a fastlane Appfile.
fn appfile_value(text: &str, call: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| l.starts_with(&format!("{call}(")) || l.starts_with(&format!("{call} ")))
        .find_map(|l| l.split(['"', '\'']).nth(1).map(str::to_string))
        .filter(|v| !v.is_empty())
}

/// What a project already says about itself, from the files it has: build
/// scripts, Dioxus.toml, Cargo.toml, fastlane, `.env` and the last builds.
#[derive(Clone, PartialEq, Debug, Default)]
pub(super) struct Detected {
    pub app_name: Option<String>,
    pub slug: Option<String>,
    pub ios_bundle_id: Option<String>,
    pub android_bundle_id: Option<String>,
    pub version: Option<String>,
    pub has_ios: bool,
    pub has_android: bool,
}

pub(super) fn detect_identity(dir: &std::path::Path) -> Detected {
    let read = |rel: &str| std::fs::read_to_string(dir.join(rel)).unwrap_or_default();
    let dioxus = read("Dioxus.toml");
    let cargo = read("Cargo.toml");
    let ios_script = read("build_ios_distribution.sh");
    let android_script = read("build_android_release.sh");
    let appfile = read("fastlane/Appfile");
    let env = load_env(&[dir.join(".env"), dir.join("fastlane").join(".env")]);
    let first = |candidates: Vec<Option<String>>| candidates.into_iter().flatten().find(|v| !v.trim().is_empty());

    // dx builds into target/dx/<Cargo package name>; the folder is the proof.
    let dx_dirs: Vec<std::path::PathBuf> = std::fs::read_dir(dir.join("target/dx"))
        .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.join("release").is_dir()).collect())
        .unwrap_or_default();
    let cargo_name = toml_in(&cargo, "package", "name");
    let slug = first(vec![
        cargo_name.clone().filter(|n| dx_dirs.is_empty() || dx_dirs.iter().any(|d| d.ends_with(n))),
        (dx_dirs.len() == 1).then(|| dx_dirs[0].file_name().unwrap_or_default().to_string_lossy().to_string()),
        cargo_name,
    ]);
    let built = |platform: &str| slug.as_ref().is_some_and(|s| dir.join(format!("target/dx/{s}/release/{platform}")).is_dir());
    let gradle = slug
        .as_ref()
        .and_then(|s| std::fs::read_to_string(dir.join(format!("target/dx/{s}/release/android/app/app/build.gradle.kts"))).ok())
        .unwrap_or_default();
    let bundle_identifier = first(vec![
        toml_in(&dioxus, "bundle", "identifier"),
        toml_in(&dioxus, "mobile", "bundle_identifier"),
    ]);
    let root_files: Vec<String> = std::fs::read_dir(dir)
        .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect())
        .unwrap_or_default();
    let has_file = |ext: &str| root_files.iter().any(|f| f.ends_with(ext));

    Detected {
        app_name: first(vec![
            shell_var(&ios_script, "DISPLAY_NAME"),
            toml_in(&dioxus, "mobile", "title"),
            toml_in(&dioxus, "bundle", "name"),
            shell_var(&ios_script, "APP_NAME"),
            toml_in(&dioxus, "application", "name"),
        ]),
        ios_bundle_id: first(vec![
            shell_var(&ios_script, "BUNDLE_ID"),
            appfile_value(&appfile, "app_identifier"),
            env.get("APP_IDENTIFIER").cloned(),
            bundle_identifier.clone(),
        ]),
        // What the last Android build actually used beats what files say.
        android_bundle_id: first(vec![
            super::verify::gradle_value(&gradle, "applicationId"),
            env.get("ANDROID_PACKAGE_NAME").cloned(),
            appfile_value(&appfile, "package_name"),
            shell_var(&android_script, "NEW_PACKAGE"),
            toml_in(&dioxus, "android", "package"),
            bundle_identifier,
        ]),
        version: first(vec![
            shell_var(&ios_script, "VERSION"),
            toml_in(&dioxus, "mobile.ios", "version"),
            toml_in(&dioxus, "android", "version_name"),
            shell_var(&android_script, "V_NAME"),
        ])
        .filter(|v| valid_version(v)),
        has_ios: built("ios") || has_file(".ipa") || !ios_script.is_empty() || root_files.iter().any(|f| f == "Entitlements.plist"),
        has_android: built("android") || has_file(".aab") || has_file(".apk") || !android_script.is_empty(),
        slug,
    }
}

/// Fill the App step from what the project already says, for fields still
/// empty. A project nobody has set up yet also gets its platforms and
/// version from its files. Returns whether anything changed.
pub(super) fn seed_identity(p: &mut ProjectState, dir: &std::path::Path) -> bool {
    let unconfigured = [&p.app_name, &p.project_slug, &p.ios_bundle_id, &p.android_bundle_id]
        .iter()
        .all(|f| f.trim().is_empty());
    let d = detect_identity(dir);
    let mut changed = false;
    for (field, found) in [
        (&mut p.app_name, &d.app_name),
        (&mut p.project_slug, &d.slug),
        (&mut p.ios_bundle_id, &d.ios_bundle_id),
        (&mut p.android_bundle_id, &d.android_bundle_id),
    ] {
        if field.trim().is_empty() {
            if let Some(v) = found {
                *field = v.clone();
                changed = true;
            }
        }
    }
    if unconfigured && changed {
        let platform = match (d.has_ios, d.has_android) {
            (true, false) => Some(PlatformType::Ios),
            (false, true) => Some(PlatformType::Android),
            _ => None,
        };
        if let Some(platform) = platform.filter(|pl| *pl != p.platform_type) {
            p.platform_type = platform;
        }
        // The version may have been guessed from the global default before
        // the project's own was readable.
        if let Some(v) = d.version.filter(|v| *v != p.version) {
            p.version = v;
        }
    }
    changed
}

/// Fill in version, build numbers and profile for a project that predates
/// them, from what used to own each value: the global version setting,
/// `build_number.txt` (last iOS build used), `Dioxus.toml` (last Android
/// versionCode used), and the global profile if it fits this app.
/// Returns whether anything changed.
pub(super) fn seed_release_fields(p: &mut ProjectState, dir: &std::path::Path, settings: &Settings) -> bool {
    let mut changed = false;
    let toml = std::fs::read_to_string(dir.join("Dioxus.toml")).unwrap_or_default();

    if p.version.trim().is_empty() {
        p.version = toml_value(&toml, "version_name")
            .filter(|v| valid_version(v))
            .unwrap_or_else(|| settings.ios_short_version.clone());
        if !valid_version(&p.version) {
            p.version = "1.0.0".into();
        }
        changed = true;
    }
    if p.ios_build_number == 0 {
        let last = std::fs::read_to_string(dir.join("build_number.txt"))
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(0);
        p.ios_build_number = last + 1;
        changed = true;
    }
    if p.android_version_code == 0 {
        let last = toml_value(&toml, "version_code").and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
        p.android_version_code = last + 1;
        changed = true;
    }
    if p.provisioning_profile.is_empty() && !settings.provisioning_profile.is_empty() {
        let fits = read_profile(std::path::Path::new(&settings.provisioning_profile))
            .is_some_and(|info| info.matches_bundle(&p.ios_bundle_id));
        if fits {
            p.provisioning_profile = settings.provisioning_profile.clone();
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROFILE: &str = r#"garbage-cms-header<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
    <key>CreationDate</key>
    <date>2025-01-01T10:00:00Z</date>
    <key>Entitlements</key>
    <dict>
        <key>application-identifier</key>
        <string>AB12CD34EF.com.acme.app</string>
        <key>get-task-allow</key>
        <false/>
    </dict>
    <key>ExpirationDate</key>
    <date>2026-01-01T10:00:00Z</date>
    <key>Name</key>
    <string>Acme App Store</string>
    <key>TeamIdentifier</key>
    <array>
        <string>AB12CD34EF</string>
    </array>
</dict></plist>trailing-signature"#;

    fn write_profile(name: &str, body: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("appscreens-test-{name}.mobileprovision"));
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn reads_app_store_profile() {
        let info = read_profile(&write_profile("appstore", PROFILE)).unwrap();
        assert_eq!(info.name, "Acme App Store");
        assert_eq!(info.team_id, "AB12CD34EF");
        assert_eq!(info.app_id, "com.acme.app");
        assert_eq!(info.expires, "2026-01-01");
        assert_eq!(info.kind, ProfileKind::AppStore);
        assert!(info.matches_bundle("com.acme.app"));
        assert!(!info.matches_bundle("com.acme.other"));
    }

    #[test]
    fn classifies_profile_kinds() {
        let dev = PROFILE.replace("<key>get-task-allow</key>\n        <false/>", "<key>get-task-allow</key>\n        <true/>");
        assert_eq!(read_profile(&write_profile("dev", &dev)).unwrap().kind, ProfileKind::Development);
        let adhoc = PROFILE.replace("<key>Name</key>", "<key>ProvisionedDevices</key><array><string>x</string></array><key>Name</key>");
        assert_eq!(read_profile(&write_profile("adhoc", &adhoc)).unwrap().kind, ProfileKind::AdHoc);
    }

    #[test]
    fn wildcard_profiles_match_prefix() {
        let wild = PROFILE.replace("AB12CD34EF.com.acme.app", "AB12CD34EF.com.acme.*");
        let info = read_profile(&write_profile("wild", &wild)).unwrap();
        assert!(info.matches_bundle("com.acme.anything"));
        assert!(!info.matches_bundle("org.other.app"));
    }

    #[test]
    fn checks_team_against_identity() {
        let info = read_profile(&write_profile("team", PROFILE)).unwrap();
        let checks = profile_checks(&info, "com.acme.app", "Apple Distribution: Jane (ZZ99ZZ99ZZ)");
        let team = checks.iter().find(|c| c.label == "Same team as signing identity").unwrap();
        assert!(!team.ok);
    }

    #[test]
    fn today_is_a_plausible_date() {
        let t = utc_today();
        assert_eq!(t.len(), 10);
        assert!(t.as_str() > "2024-01-01" && t.as_str() < "2100-01-01");
    }

    #[test]
    fn version_rules() {
        assert!(valid_version("1"));
        assert!(valid_version("1.2"));
        assert!(valid_version("1.2.3"));
        assert!(!valid_version(""));
        assert!(!valid_version("1.2.3.4"));
        assert!(!valid_version("1.2-beta"));
        assert!(!valid_version("1..2"));
        assert_eq!(bump_version("1.2.3", 0), "2.0.0");
        assert_eq!(bump_version("1.2.3", 1), "1.3.0");
        assert_eq!(bump_version("1.2", 2), "1.2.1");
    }

    #[test]
    fn android_output_is_per_language_inside_the_project() {
        let project = std::env::temp_dir().join("appscreens-test-android-out");
        let _ = std::fs::remove_dir_all(&project);
        let ios_dir = project.join("fastlane/screenshots/ios");
        let android_root = project.join("fastlane/metadata/android");
        let png = {
            let mut buf = std::io::Cursor::new(Vec::new());
            RgbaImage::from_pixel(40, 80, Rgba([10, 20, 30, 255]))
                .write_to(&mut buf, image::ImageFormat::Png)
                .unwrap();
            buf.into_inner()
        };
        let targets = ExportTargets {
            ios_dir,
            android_root: android_root.clone(),
            desktop_dir: project.join("fastlane/screenshots/desktop"),
            ios: vec![],
            android: vec![true; 2],
            desktop: vec![],
        };
        let mut all = Vec::new();
        for (locale, play) in [("en-US", "en-US"), ("zh-Hans", "zh-CN")] {
            for screen in 1..=2 {
                all.extend(
                    resize_to_targets(&png, screen, locale, play, &targets)
                        .unwrap(),
                );
            }
        }
        // 2 languages × (2 phone + 1 feature graphic), all distinct files in the project.
        assert_eq!(all.len(), 6);
        let paths: std::collections::HashSet<_> = all.iter().map(|(_, p)| p.clone()).collect();
        assert_eq!(paths.len(), 6);
        assert!(paths.iter().all(|p| p.starts_with(&android_root) && p.exists()));
        assert!(android_root.join("zh-CN/images/phoneScreenshots/02.png").exists());
        assert!(android_root.join("en-US/images/featureGraphic.png").exists());
        assert!(all.iter().all(|(l, _)| l.contains("[en-US]") || l.contains("[zh-Hans]")));
    }

    #[test]
    fn desktop_sizes_are_exported_and_unticked_platforms_are_not() {
        let project = std::env::temp_dir().join("appscreens-test-desktop-out");
        let _ = std::fs::remove_dir_all(&project);
        let mut p = ProjectState::with_defaults();
        p.platform_type = PlatformType::Desktop;
        p.export_ios = false;
        p.export_android = false;
        p.desktop_targets = vec![true, false, true];
        let targets = ExportTargets::for_project(&project, &p);
        let png = {
            let mut buf = std::io::Cursor::new(Vec::new());
            RgbaImage::from_pixel(160, 100, Rgba([10, 20, 30, 255]))
                .write_to(&mut buf, image::ImageFormat::Png)
                .unwrap();
            buf.into_inner()
        };
        let out = resize_to_targets(&png, 1, "en-US", "en-US", &targets).unwrap();
        // Only the two ticked desktop sizes — no iOS or Android files.
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|(l, _)| l.starts_with("Desktop ") && l.contains("[en-US]")));
        let mac = project.join("fastlane/screenshots/desktop/en-US/desktop_mac-01.png");
        assert_eq!((image::open(&mac).unwrap().width(), image::open(&mac).unwrap().height()), (1280, 800));
        let wide = project.join("fastlane/screenshots/desktop/en-US/desktop_wide-01.png");
        assert_eq!((image::open(&wide).unwrap().width(), image::open(&wide).unwrap().height()), (1920, 1080));
    }

    #[test]
    fn android_release_script_takes_signing_from_the_environment() {
        let script = script_android_release("Nahw", "nahw", "com.example.nahw", "1.0.0", 6);
        assert!(!script.contains("Salma"), "no password in the generated script");
        assert!(!script.contains("mayorana-release.keystore"));
        assert!(script.contains(r#"KEY_ALIAS="${ANDROID_KEY_ALIAS:-nahw}""#));
        assert!(script.contains(r#"KEYSTORE_PATH="${ANDROID_KEYSTORE_PATH:-}""#));
        // The template is format!-escaped bash; make sure it still parses.
        let path = std::env::temp_dir().join("appscreens-test-android-release.sh");
        std::fs::write(&path, &script).unwrap();
        let check = std::process::Command::new("bash").arg("-n").arg(&path).output().unwrap();
        assert!(check.status.success(), "{}", String::from_utf8_lossy(&check.stderr));
    }

    #[test]
    fn detects_an_existing_project_from_its_files() {
        let dir = std::env::temp_dir().join("appscreens-test-detect");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("target/dx/tafseel-quran/release/ios")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"tafseel-quran\"\nversion = \"1.1.0\"\n").unwrap();
        std::fs::write(
            dir.join("Dioxus.toml"),
            "[application]\nname = \"TafseelQuran\"\n\n[mobile]\ntitle = \"Tafseel\"\nbundle_identifier = \"com.mayorana.tafseel\"\n\n\
             [mobile.ios]\n# live is 1.0\nversion = \"1.1\"\nbuild_number = 12\n\n[bundle]\nidentifier = \"com.mayorana.tafseel\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("TafseelQuran.ipa"), "x").unwrap();
        std::fs::write(dir.join("build_number.txt"), "12\n").unwrap();

        // What AppScreens had saved before it could detect anything.
        let mut p = ProjectState::with_defaults();
        p.version = "1.0".into();
        assert!(seed_identity(&mut p, &dir));
        assert_eq!(p.app_name, "Tafseel");
        assert_eq!(p.project_slug, "tafseel-quran", "dx's folder, not Dioxus.toml's name");
        assert_eq!(p.ios_bundle_id, "com.mayorana.tafseel");
        assert_eq!(p.android_bundle_id, "com.mayorana.tafseel");
        assert_eq!(p.platform_type, PlatformType::Ios, "only iOS was ever built");
        assert_eq!(p.version, "1.1");

        // Once set, nothing is overwritten.
        p.app_name = "Tafseel Quran".into();
        p.version = "1.2".into();
        assert!(!seed_identity(&mut p, &dir));
        assert_eq!((p.app_name.as_str(), p.version.as_str()), ("Tafseel Quran", "1.2"));
    }

    #[test]
    fn a_build_script_and_the_last_android_build_win() {
        let dir = std::env::temp_dir().join("appscreens-test-detect-scripts");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("target/dx/nahw/release/android/app/app")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"nahw\"\n").unwrap();
        std::fs::write(dir.join("Dioxus.toml"), "[bundle]\nidentifier = \"com.example.nahw\"\n").unwrap();
        std::fs::write(
            dir.join("build_ios_distribution.sh"),
            "#!/bin/bash\nAPP_NAME=\"Nahw\"\nVERSION=\"2.0.1\"\nBUNDLE_ID=\"com.mayorana.tafseel.nahw\"\nDX=\"${DX:-dx}\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("target/dx/nahw/release/android/app/app/build.gradle.kts"),
            "android {\n    defaultConfig {\n        applicationId = \"com.mayorana.nahw\"\n    }\n}\n",
        )
        .unwrap();
        let d = detect_identity(&dir);
        assert_eq!(d.app_name.as_deref(), Some("Nahw"));
        assert_eq!(d.ios_bundle_id.as_deref(), Some("com.mayorana.tafseel.nahw"));
        assert_eq!(d.android_bundle_id.as_deref(), Some("com.mayorana.nahw"));
        assert_eq!(d.version.as_deref(), Some("2.0.1"));
        assert!(d.has_ios && d.has_android);
        assert_eq!(shell_var("DX=\"${DX:-dx}\"", "DX"), None, "not a plain value");
    }

    #[test]
    fn never_replaces_a_build_script_it_did_not_write() {
        let dir = std::env::temp_dir().join("appscreens-test-own-scripts");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mine = "#!/bin/bash\n# my tuned script\nDISPLAY_NAME=\"Tafseel\"\n";
        std::fs::write(dir.join("build_ios_distribution.sh"), mine).unwrap();
        let written = ensure_build_scripts(&dir, "Tafseel", "tafseel-quran", "com.a", "com.a", "id", "", "1.1", 13, 1);
        assert!(!written.contains(&"build_ios_distribution.sh".to_string()));
        assert_eq!(std::fs::read_to_string(dir.join("build_ios_distribution.sh")).unwrap(), mine);
        // The ones it wrote carry the marker, so they're replaced next time.
        let apk = std::fs::read_to_string(dir.join("build_apk.sh")).unwrap();
        assert!(apk.starts_with(&format!("#!/bin/bash\n{SCRIPT_MARKER}\n")));
        let again = ensure_build_scripts(&dir, "Tafseel", "tafseel-quran", "com.a", "com.a", "id", "", "1.1", 14, 2);
        assert_eq!(again.len(), 3);
    }

    #[test]
    fn seeds_numbers_after_the_last_used_ones() {
        let dir = std::env::temp_dir().join("appscreens-test-seed");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("build_number.txt"), "14\n").unwrap();
        std::fs::write(dir.join("Dioxus.toml"), "[android]\nversion_code = 36\nversion_name = \"2.1.0\"\n").unwrap();
        let mut p = ProjectState::with_defaults();
        assert!(seed_release_fields(&mut p, &dir, &Settings::default()));
        assert_eq!(p.version, "2.1.0");
        assert_eq!(p.ios_build_number, 15);
        assert_eq!(p.android_version_code, 37);
        // Already seeded: nothing changes.
        assert!(!seed_release_fields(&mut p, &dir, &Settings::default()));
    }
}
