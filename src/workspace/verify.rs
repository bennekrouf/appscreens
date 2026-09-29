//! What a build actually produced, checked before anything is uploaded: the
//! signer, identifiers, version and target API of the AAB, and the signer,
//! profile and version of the IPA. A mistake here would otherwise surface as
//! a store rejection, or not at all.

use super::*;
use std::path::Path;
use std::process::Command;

fn check(label: &'static str, ok: bool, detail: impl Into<String>) -> CredCheck {
    CredCheck { label, ok, detail: detail.into() }
}

/// SHA-256 fingerprint of the certificate that signed a jar-signed file (AAB).
pub(super) fn signer_fingerprint(file: &Path) -> Option<String> {
    let out = Command::new(signing::jdk_tool("keytool")).args(["-printcert", "-jarfile"]).arg(file).output().ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.trim().strip_prefix("SHA256:").map(|f| f.trim().to_string()))
}

/// A `key = value` / `key = "value"` from the Gradle Kotlin file dx generates.
pub(super) fn gradle_value(gradle: &str, key: &str) -> Option<String> {
    gradle.lines().find_map(|l| {
        let (k, v) = l.trim().split_once('=')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
    })
}

/// Checks for the newest AAB. `expected_fingerprint` is the upload key's,
/// when its password is available to read it.
pub(super) fn verify_aab(
    aab: &Path,
    project: &Path,
    slug: &str,
    package: &str,
    expected_fingerprint: Option<&str>,
) -> Vec<CredCheck> {
    let mut checks = Vec::new();
    match (signer_fingerprint(aab), expected_fingerprint) {
        (None, _) => checks.push(check("Signed", false, "Not signed — Play rejects unsigned bundles")),
        (Some(fp), Some(want)) if fp.eq_ignore_ascii_case(want) => {
            checks.push(check("Signed with your upload key", true, format!("SHA-256 {}…", &fp[..fp.len().min(23)])))
        }
        (Some(fp), Some(_)) => checks.push(check(
            "Signed with your upload key",
            false,
            format!("Signed by a different key (SHA-256 {}…) — Play will reject it", &fp[..fp.len().min(23)]),
        )),
        (Some(fp), None) => checks.push(check(
            "Signed",
            true,
            format!("SHA-256 {}… (save the keystore password to compare it with your upload key)", &fp[..fp.len().min(23)]),
        )),
    }

    // The Gradle file the release script patched is what produced the bundle.
    let gradle_path = project.join(format!("target/dx/{slug}/release/android/app/app/build.gradle.kts"));
    let Ok(gradle) = std::fs::read_to_string(&gradle_path) else {
        checks.push(check("Build settings", false, "Gradle project not found — rebuild to check package, version and target API"));
        return checks;
    };
    let app_id = gradle_value(&gradle, "applicationId").unwrap_or_default();
    checks.push(check(
        "Package",
        !package.is_empty() && app_id == package,
        if app_id == package { app_id.clone() } else { format!("Built as {app_id}, but the app is {package}") },
    ));
    let target: u32 = gradle_value(&gradle, "targetSdk").and_then(|v| v.parse().ok()).unwrap_or(0);
    checks.push(check(
        "Target API",
        target >= 36,
        if target >= 36 { format!("API {target}") } else { format!("API {target} — Google Play requires 36 for updates") },
    ));
    let code = gradle_value(&gradle, "versionCode").unwrap_or_default();
    let name = gradle_value(&gradle, "versionName").unwrap_or_default();
    checks.push(check("Version", !code.is_empty(), format!("{name} ({code})")));
    checks
}

/// Checks for the newest IPA: identifiers, version, signer and profile.
pub(super) fn verify_ipa(ipa: &Path, bundle_id: &str) -> Vec<CredCheck> {
    let mut checks = Vec::new();
    let tmp = std::env::temp_dir().join(format!("appscreens-ipa-check-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let unzipped = Command::new("unzip").args(["-q", "-o"]).arg(ipa).arg("-d").arg(&tmp).status().is_ok_and(|s| s.success());
    let app = std::fs::read_dir(tmp.join("Payload"))
        .ok()
        .and_then(|d| d.flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|e| e == "app")));
    let Some(app) = app.filter(|_| unzipped) else {
        let _ = std::fs::remove_dir_all(&tmp);
        return vec![check("Readable", false, "Could not open the IPA")];
    };

    let plist = Command::new("plutil")
        .args(["-convert", "json", "-o", "-"])
        .arg(app.join("Info.plist"))
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok())
        .unwrap_or_default();
    let built_id = plist["CFBundleIdentifier"].as_str().unwrap_or("");
    checks.push(check(
        "Bundle ID",
        built_id == bundle_id,
        if built_id == bundle_id { built_id.to_string() } else { format!("Built as {built_id}, but the app is {bundle_id}") },
    ));
    checks.push(check(
        "Version",
        plist["CFBundleShortVersionString"].is_string() && plist["CFBundleVersion"].is_string(),
        format!(
            "{} ({})",
            plist["CFBundleShortVersionString"].as_str().unwrap_or("?"),
            plist["CFBundleVersion"].as_str().unwrap_or("?")
        ),
    ));

    let sig = Command::new("codesign").args(["-dvv"]).arg(&app).output().ok();
    let authority = sig
        .as_ref()
        .map(|o| String::from_utf8_lossy(&o.stderr).to_string())
        .and_then(|t| t.lines().find_map(|l| l.strip_prefix("Authority=").map(str::to_string)))
        .unwrap_or_default();
    checks.push(check(
        "Signed for distribution",
        authority.starts_with("Apple Distribution") || authority.starts_with("iPhone Distribution"),
        if authority.is_empty() { "Not signed".to_string() } else { authority },
    ));

    match read_profile(&app.join("embedded.mobileprovision")) {
        Some(p) => {
            let ok = p.kind == ProfileKind::AppStore && p.matches_bundle(bundle_id) && !p.expired();
            checks.push(check(
                "Provisioning profile",
                ok,
                format!("{} · {} · expires {}", p.name, p.kind.label(), p.expires),
            ));
        }
        None => checks.push(check("Provisioning profile", false, "No embedded profile")),
    }
    let _ = std::fs::remove_dir_all(&tmp);
    checks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_gradle_values() {
        let g = "android {\n    namespace = \"com.a\"\n    defaultConfig {\n        applicationId = \"com.mayorana.nahw\"\n        targetSdk = 36\n        versionCode = 7\n        versionName = \"1.0.0\"\n    }\n}";
        assert_eq!(gradle_value(g, "applicationId").as_deref(), Some("com.mayorana.nahw"));
        assert_eq!(gradle_value(g, "targetSdk").as_deref(), Some("36"));
        assert_eq!(gradle_value(g, "versionName").as_deref(), Some("1.0.0"));
        assert_eq!(gradle_value(g, "minSdk"), None);
    }

    #[test]
    fn detects_the_signer_of_a_signed_jar() {
        if signing::java_home().is_none() {
            return; // no JDK
        }
        let dir = std::env::temp_dir().join("appscreens-test-verify");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ks = dir.join("k.jks");
        signing::create_upload_key(&ks, "a", "test-pass-123", "CN=Test").unwrap();
        let jar = dir.join("x.aab");
        std::fs::write(dir.join("f.txt"), "x").unwrap();
        assert!(Command::new(signing::jdk_tool("jar")).arg("cf").arg(&jar).arg("-C").arg(&dir).arg("f.txt").status().unwrap().success());
        assert_eq!(signer_fingerprint(&jar), None, "unsigned before jarsigner");
        let signed = Command::new(signing::jdk_tool("jarsigner"))
            .arg("-keystore").arg(&ks)
            .args(["-storepass:env", "P"]).env("P", "test-pass-123")
            .arg(&jar).arg("a")
            .output().unwrap();
        assert!(signed.status.success(), "{}", String::from_utf8_lossy(&signed.stdout));
        let want = signing::upload_key_fingerprint(&ks, "a", "test-pass-123").unwrap();
        assert_eq!(signer_fingerprint(&jar).as_deref(), Some(want.as_str()));
        let checks = verify_aab(&jar, &dir, "none", "com.x", Some(&want));
        assert!(checks[0].ok, "{:?}", checks[0]);
        assert!(!verify_aab(&jar, &dir, "none", "com.x", Some("AA:BB"))[0].ok);
    }
}
