//! Does the whole project agree on who the app is? The App step's package
//! name is the truth; the app's own code (store links, data paths) and the
//! `.env` override must match it, or the build ships as a different app, or
//! saves its data where the app can't find it. Also: Dioxus.toml settings the
//! installed dx ignores or rejects.

use super::*;
use std::path::{Path, PathBuf};

/// A reference to an Android package name somewhere in the project.
#[derive(Clone, PartialEq, Debug)]
pub(super) struct PackageRef {
    pub file: PathBuf,
    pub line: usize,
    pub value: String,
}

fn is_package_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.'
}

/// Package names in one source text: Play Store links, Android data paths and
/// `PACKAGE_NAME`-style constants.
pub(super) fn package_refs_in(text: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for (n, line) in text.lines().enumerate() {
        for marker in ["details?id=", "/data/data/", "/data/user/0/"] {
            let mut rest = line;
            while let Some(i) = rest.find(marker) {
                let after = &rest[i + marker.len()..];
                let value: String = after.chars().take_while(|c| is_package_char(*c)).collect();
                if value.contains('.') {
                    found.push((n + 1, value));
                }
                rest = &after[..];
            }
        }
        let t = line.trim();
        if t.contains("PACKAGE_NAME") && (t.starts_with("pub const") || t.starts_with("const") || t.starts_with("static")) {
            if let Some(v) = t.split('"').nth(1).filter(|v| v.contains('.') && v.chars().all(is_package_char)) {
                found.push((n + 1, v.to_string()));
            }
        }
    }
    found
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Every reference in `src/` and the `.env` override that names a different
/// Android package than `expected`.
pub(super) fn package_mismatches(project: &Path, expected: &str) -> Vec<PackageRef> {
    let mut files = Vec::new();
    rust_files(&project.join("src"), &mut files);
    let mut bad = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap_or_default();
        for (line, value) in package_refs_in(&text) {
            if value != expected {
                bad.push(PackageRef { file: f.clone(), line, value });
            }
        }
    }
    let env = project.join(".env");
    if let Ok(text) = std::fs::read_to_string(&env) {
        for (n, l) in text.lines().enumerate() {
            if let Some(v) = l.trim().strip_prefix("ANDROID_PACKAGE_NAME=") {
                let v = v.trim().trim_matches('"').to_string();
                if !v.is_empty() && v != expected {
                    bad.push(PackageRef { file: env.clone(), line: n + 1, value: v });
                }
            }
        }
    }
    bad
}

/// Rewrite every mismatched reference to `expected`. Only the exact old
/// package strings found are replaced.
pub(super) fn fix_package_mismatches(expected: &str, refs: &[PackageRef]) -> Result<usize, String> {
    let mut files: Vec<&PathBuf> = refs.iter().map(|r| &r.file).collect();
    files.dedup();
    let mut changed = 0;
    for f in files {
        let mut text = std::fs::read_to_string(f).map_err(|e| e.to_string())?;
        for old in refs.iter().filter(|r| &r.file == f).map(|r| r.value.as_str()) {
            for marker in ["details?id=", "/data/data/", "/data/user/0/", "ANDROID_PACKAGE_NAME=", "\""] {
                text = text.replace(&format!("{marker}{old}"), &format!("{marker}{expected}"));
            }
        }
        std::fs::write(f, text).map_err(|e| e.to_string())?;
        changed += 1;
    }
    Ok(changed)
}

// ---------------------------------------------------------------------------
// Dioxus.toml vs dx 0.7
// ---------------------------------------------------------------------------

/// `[android]` settings dx 0.7 ignores (old names) or rejects, plus a target
/// API Play won't accept. Empty = fine.
pub(super) fn dioxus_toml_issues(toml: &str) -> Vec<String> {
    let mut issues = Vec::new();
    let mut in_android = false;
    let mut target = None;
    for l in toml.lines() {
        let t = l.trim();
        if t.starts_with('[') {
            in_android = t == "[android]";
            continue;
        }
        if !in_android || t.starts_with('#') {
            continue;
        }
        let Some((k, v)) = t.split_once('=') else { continue };
        match k.trim() {
            "target_sdk_version" => issues.push("target_sdk_version is ignored by dx 0.7 — it's target_sdk".into()),
            "min_sdk_version" => issues.push("min_sdk_version is ignored by dx 0.7 — it's min_sdk".into()),
            "permissions" if v.trim().starts_with('[') => {
                issues.push("permissions = [...] is rejected by dx 0.7.10 (INTERNET is added anyway)".into())
            }
            "target_sdk" => target = v.trim().parse::<u32>().ok(),
            _ => {}
        }
    }
    if let Some(t) = target.filter(|t| *t < 36) {
        issues.push(format!("target_sdk = {t} — Google Play requires 36 for updates"));
    }
    issues
}

/// Rename the old keys, drop the list-style permissions and raise the target
/// API to 36, inside `[android]` only.
pub(super) fn fix_dioxus_toml(toml: &str) -> String {
    let mut out = Vec::new();
    let mut in_android = false;
    let mut has_target = toml.lines().any(|l| l.trim().starts_with("target_sdk") && !l.trim().starts_with("target_sdk_version"));
    for l in toml.lines() {
        let t = l.trim();
        if t.starts_with('[') {
            in_android = t == "[android]";
            out.push(l.to_string());
            continue;
        }
        if in_android && !t.starts_with('#') {
            if let Some((k, v)) = t.split_once('=') {
                let indent = &l[..l.len() - l.trim_start().len()];
                match k.trim() {
                    "target_sdk_version" => {
                        if !has_target {
                            out.push(format!("{indent}target_sdk = 36"));
                            out.push(format!("{indent}compile_sdk = 36"));
                            has_target = true;
                        }
                        continue;
                    }
                    "min_sdk_version" => {
                        out.push(format!("{indent}min_sdk = {}", v.trim()));
                        continue;
                    }
                    "permissions" if v.trim().starts_with('[') => continue,
                    "target_sdk" if v.trim().parse::<u32>().is_ok_and(|n| n < 36) => {
                        out.push(format!("{indent}target_sdk = 36"));
                        continue;
                    }
                    "compile_sdk" if v.trim().parse::<u32>().is_ok_and(|n| n < 36) => {
                        out.push(format!("{indent}compile_sdk = 36"));
                        continue;
                    }
                    _ => {}
                }
            }
        }
        out.push(l.to_string());
    }
    let mut s = out.join("\n");
    if toml.ends_with('\n') {
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_package_references() {
        let src = r#"
pub const PACKAGE_NAME: &str = "com.mayorana.abjad";
fn url() -> &'static str { "market://details?id=com.mayorana.abjad" }
let p = format!("/data/data/{}/files", x);
let q = "/data/user/0/com.other.app/files/db";
"#;
        let refs: Vec<String> = package_refs_in(src).into_iter().map(|(_, v)| v).collect();
        assert_eq!(refs, vec!["com.mayorana.abjad", "com.mayorana.abjad", "com.other.app"]);
    }

    #[test]
    fn fixes_mismatches_in_code_and_env() {
        let dir = std::env::temp_dir().join("appscreens-test-consistency");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("src/config.rs"),
            "pub const PACKAGE_NAME: &str = \"com.old.app\";\nconst URL: &str = \"market://details?id=com.old.app\";\n",
        )
        .unwrap();
        std::fs::write(dir.join(".env"), "ANDROID_PACKAGE_NAME=\"com.old.app\"\nOTHER=1\n").unwrap();
        let bad = package_mismatches(&dir, "com.new.app");
        assert_eq!(bad.len(), 3);
        fix_package_mismatches("com.new.app", &bad).unwrap();
        assert!(package_mismatches(&dir, "com.new.app").is_empty());
        assert!(std::fs::read_to_string(dir.join(".env")).unwrap().contains("OTHER=1"));
    }

    #[test]
    fn dioxus_toml_old_keys() {
        let toml = "[android]\npackage = \"x\"\nmin_sdk_version = 24\ntarget_sdk_version = 35\npermissions = [\"INTERNET\"]\n\n[bundle]\nidentifier = \"x\"\n";
        assert_eq!(dioxus_toml_issues(toml).len(), 3);
        let fixed = fix_dioxus_toml(toml);
        assert!(dioxus_toml_issues(&fixed).is_empty(), "{fixed}");
        assert!(fixed.contains("min_sdk = 24") && fixed.contains("target_sdk = 36") && fixed.contains("compile_sdk = 36"));
        assert!(fixed.contains("[bundle]\nidentifier = \"x\""));
        assert!(dioxus_toml_issues("[android]\ntarget_sdk = 34\n")[0].contains("36"));
        assert!(dioxus_toml_issues("[android]\ntarget_sdk = 36\ncompile_sdk = 36\n").is_empty());
    }
}

