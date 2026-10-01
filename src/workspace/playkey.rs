//! Setting up the Google Play service-account key: reuse the one another
//! project uses (one key can release every app of the developer account), or
//! pick up the JSON file Google Cloud just downloaded and keep a private copy
//! in the keys folder.

use std::path::{Path, PathBuf};

pub(super) const KEY_VAR: &str = "GOOGLE_PLAY_JSON_KEY";
pub(super) const API_PAGE: &str = "https://console.cloud.google.com/apis/library/androidpublisher.googleapis.com";
pub(super) const ACCOUNTS_PAGE: &str = "https://console.cloud.google.com/iam-admin/serviceaccounts";
pub(super) const PLAY_USERS_PAGE: &str = "https://play.google.com/console/users-and-permissions";
pub(super) const HELP_PAGE: &str = "https://developers.google.com/android-publisher/getting_started";

/// A service-account key file.
#[derive(Clone, PartialEq, Debug)]
pub(super) struct PlayKey {
    pub path: PathBuf,
    /// The address to invite in Play Console
    pub client_email: String,
    pub project_id: String,
    /// Where it was found, e.g. the other project's name
    pub from: String,
}

/// Read a key file; None unless it's a Google service-account key.
pub(super) fn read_key(path: &Path) -> Option<PlayKey> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    if v["type"].as_str() != Some("service_account") || !v["private_key"].is_string() {
        return None;
    }
    Some(PlayKey {
        path: path.to_path_buf(),
        client_email: v["client_email"].as_str()?.to_string(),
        project_id: v["project_id"].as_str().unwrap_or_default().to_string(),
        from: String::new(),
    })
}

/// Service-account keys in the keys folder and Downloads, one per key.
pub(super) fn find_keys(keys_dir: &Path) -> Vec<PlayKey> {
    let home = dirs::home_dir().unwrap_or_default();
    let mut found: Vec<PlayKey> = Vec::new();
    for dir in [keys_dir.to_path_buf(), home.join("Downloads")] {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for p in entries.flatten().map(|e| e.path()) {
            let small = p.metadata().is_ok_and(|m| m.len() < 64 * 1024);
            if p.extension().is_some_and(|e| e == "json") && small {
                if let Some(k) = read_key(&p) {
                    // The same file name is the same key: the keys folder's copy wins.
                    if !found.iter().any(|f| f.path.file_name() == k.path.file_name()) {
                        found.push(k);
                    }
                }
            }
        }
    }
    found
}

/// Keys other projects already use, whose file still exists.
pub(super) fn from_other_projects(projects: &[PathBuf], current: &Path) -> Vec<PlayKey> {
    let mut keys: Vec<PlayKey> = Vec::new();
    for dir in projects.iter().filter(|d| d.as_path() != current && d.is_dir()) {
        let Some(path) = super::env_lookup(dir)(KEY_VAR) else { continue };
        if let Some(mut k) = read_key(&super::signing::expand_home(&path)) {
            if !keys.iter().any(|o| o.path == k.path) {
                k.from = dir.file_name().unwrap_or_default().to_string_lossy().to_string();
                keys.push(k);
            }
        }
    }
    keys
}

/// Keep a private copy in the keys folder. Returns where it now lives.
pub(super) fn keep_in_keys_dir(src: &Path, keys_dir: &Path) -> Result<PathBuf, String> {
    if read_key(src).is_none() {
        return Err(format!("{} isn't a Google service-account key (JSON)", src.display()));
    }
    if src.parent() == Some(keys_dir) {
        return Ok(src.to_path_buf());
    }
    std::fs::create_dir_all(keys_dir).map_err(|e| e.to_string())?;
    let dest = keys_dir.join(src.file_name().ok_or("No file name")?);
    if !dest.exists() {
        std::fs::copy(src, &dest).map_err(|e| e.to_string())?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600));
    }
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_json(email: &str) -> String {
        format!(
            r#"{{"type":"service_account","project_id":"tafseel-488419","private_key_id":"77ff","private_key":"-----BEGIN PRIVATE KEY-----\nx\n-----END PRIVATE KEY-----\n","client_email":"{email}"}}"#
        )
    }

    #[test]
    fn recognises_service_account_keys_only() {
        let dir = std::env::temp_dir().join("appscreens-test-playkey");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Downloads")).unwrap();
        let src = dir.join("Downloads/tafseel-488419-77ff.json");
        std::fs::write(&src, key_json("play@tafseel-488419.iam.gserviceaccount.com")).unwrap();
        std::fs::write(dir.join("Downloads/package.json"), r#"{"name":"x"}"#).unwrap();
        let k = read_key(&src).unwrap();
        assert_eq!(k.client_email, "play@tafseel-488419.iam.gserviceaccount.com");
        assert_eq!(k.project_id, "tafseel-488419");
        assert!(read_key(&dir.join("Downloads/package.json")).is_none());

        let keys = dir.join("keys");
        let kept = keep_in_keys_dir(&src, &keys).unwrap();
        assert_eq!(kept, keys.join("tafseel-488419-77ff.json"));
        assert!(src.exists(), "a copy, the download stays");
        assert_eq!(keep_in_keys_dir(&kept, &keys).unwrap(), kept);
        assert_eq!(find_keys(&keys)[0].path, kept);
        assert!(keep_in_keys_dir(&dir.join("Downloads/package.json"), &keys).is_err());
    }

    #[test]
    fn reuses_the_key_another_project_has() {
        let root = std::env::temp_dir().join("appscreens-test-playkey-projects");
        let _ = std::fs::remove_dir_all(&root);
        let (a, b) = (root.join("nahw"), root.join("quran"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let key = root.join("sa.json");
        std::fs::write(&key, key_json("play@x.iam.gserviceaccount.com")).unwrap();
        std::fs::write(a.join(".env"), format!("{KEY_VAR}={}\n", key.display())).unwrap();
        let keys = from_other_projects(&[a.clone(), b.clone()], &b);
        assert_eq!(keys.len(), 1);
        assert_eq!((keys[0].from.as_str(), keys[0].path.clone()), ("nahw", key));
    }
}
