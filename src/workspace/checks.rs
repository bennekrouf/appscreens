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
fn plist_value(text: &str, key: &str) -> Option<String> {
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
