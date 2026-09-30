//! Setting up the App Store Connect API key without hunting for three values:
//! reuse the key another project already has (one team, one key), or pick up
//! a downloaded `AuthKey_<KEY ID>.p8` — its name carries the Key ID — and
//! keep it in the keys folder, since Apple only lets you download it once.

use std::path::{Path, PathBuf};

pub(super) const KEYS_PAGE: &str = "https://appstoreconnect.apple.com/access/integrations/api";
pub(super) const HELP_PAGE: &str =
    "https://developer.apple.com/documentation/appstoreconnectapi/creating-api-keys-for-app-store-connect-api";

pub(super) const KEY_ID_VAR: &str = "APP_STORE_CONNECT_API_KEY_KEY_ID";
pub(super) const ISSUER_VAR: &str = "APP_STORE_CONNECT_API_KEY_ISSUER_ID";
pub(super) const P8_VAR: &str = "APP_STORE_CONNECT_API_KEY_KEY_FILEPATH";

/// A complete key, ready to save into a project.
#[derive(Clone, PartialEq, Debug)]
pub(super) struct AscKey {
    pub key_id: String,
    pub issuer_id: String,
    pub p8: PathBuf,
    /// Where it was found, e.g. the other project's name
    pub from: String,
}

/// "ABCDE12345": ten capital letters and digits.
pub(super) fn valid_key_id(s: &str) -> bool {
    s.len() == 10 && s.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// A UUID, as App Store Connect shows the Issuer ID.
pub(super) fn valid_issuer(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && parts.iter().zip([8, 4, 4, 4, 12]).all(|(p, n)| p.len() == n && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// `AuthKey_ABCDE12345.p8` → `ABCDE12345`.
pub(super) fn key_id_from_file(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let id = name.strip_prefix("AuthKey_")?.strip_suffix(".p8")?;
    valid_key_id(id).then(|| id.to_string())
}

/// Is this file an App Store Connect private key?
pub(super) fn is_p8(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|t| t.contains("-----BEGIN PRIVATE KEY-----"))
}

/// `.p8` keys in the places they usually end up: the keys folder, Downloads,
/// and where Apple's own tools look.
pub(super) fn find_p8s(keys_dir: &Path) -> Vec<(PathBuf, String)> {
    let home = dirs::home_dir().unwrap_or_default();
    let dirs = [
        keys_dir.to_path_buf(),
        home.join("Downloads"),
        home.join(".appstoreconnect/private_keys"),
        home.join("private_keys"),
    ];
    let mut found: Vec<(PathBuf, String)> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for p in entries.flatten().map(|e| e.path()) {
            if let Some(id) = key_id_from_file(&p) {
                // One entry per key: the copy in the keys folder wins.
                if !found.iter().any(|(_, k)| *k == id) && is_p8(&p) {
                    found.push((p, id));
                }
            }
        }
    }
    found
}

/// Keys other projects already use (their `.env`), whose `.p8` still exists.
pub(super) fn from_other_projects(projects: &[PathBuf], current: &Path) -> Vec<AscKey> {
    let mut keys: Vec<AscKey> = Vec::new();
    for dir in projects.iter().filter(|d| d.as_path() != current && d.is_dir()) {
        let resolve = super::env_lookup(dir);
        let (Some(key_id), Some(issuer_id), Some(p8)) = (resolve(KEY_ID_VAR), resolve(ISSUER_VAR), resolve(P8_VAR)) else {
            continue;
        };
        let p8 = super::signing::expand_home(&p8);
        if p8.is_file() && !keys.iter().any(|k| k.key_id == key_id) {
            let from = dir.file_name().unwrap_or_default().to_string_lossy().to_string();
            keys.push(AscKey { key_id, issuer_id, p8, from });
        }
    }
    keys
}

/// Keep the key in the keys folder (a copy — Apple won't let you download it
/// again), readable only by you. Returns where it now lives.
pub(super) fn keep_in_keys_dir(src: &Path, key_id: &str, keys_dir: &Path) -> Result<PathBuf, String> {
    if !is_p8(src) {
        return Err(format!("{} isn't an App Store Connect key (.p8)", src.display()));
    }
    if src.parent() == Some(keys_dir) {
        return Ok(src.to_path_buf());
    }
    std::fs::create_dir_all(keys_dir).map_err(|e| e.to_string())?;
    let dest = keys_dir.join(format!("AuthKey_{key_id}.p8"));
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

    const P8: &str = "-----BEGIN PRIVATE KEY-----\nMIGT\n-----END PRIVATE KEY-----\n";

    #[test]
    fn validates_what_app_store_connect_shows() {
        assert!(valid_key_id("4TALSPNY5Y"));
        assert!(!valid_key_id("4talspny5y") && !valid_key_id("4TALSPNY5") && !valid_key_id(""));
        assert!(valid_issuer("69a6de97-00e6-47e3-e053-5b8c7c11a4d1"));
        assert!(!valid_issuer("69a6de97-00e6-47e3-e053") && !valid_issuer("not-a-uuid-at-all-x"));
        assert_eq!(key_id_from_file(Path::new("/x/AuthKey_4TALSPNY5Y.p8")).as_deref(), Some("4TALSPNY5Y"));
        assert_eq!(key_id_from_file(Path::new("/x/AuthKey_4TALSPNY5Y (1).p8")), None);
        assert_eq!(key_id_from_file(Path::new("/x/key.p8")), None);
    }

    #[test]
    fn keeps_a_private_copy_in_the_keys_folder() {
        let dir = std::env::temp_dir().join("appscreens-test-asckey");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Downloads")).unwrap();
        let src = dir.join("Downloads/AuthKey_ABCDE12345.p8");
        std::fs::write(&src, P8).unwrap();
        let keys = dir.join("keys");
        let kept = keep_in_keys_dir(&src, "ABCDE12345", &keys).unwrap();
        assert_eq!(kept, keys.join("AuthKey_ABCDE12345.p8"));
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), P8);
        assert!(src.exists(), "a copy, the download stays");
        // Already in the keys folder: used where it is.
        assert_eq!(keep_in_keys_dir(&kept, "ABCDE12345", &keys).unwrap(), kept);
        assert_eq!(find_p8s(&keys)[0], (kept.clone(), "ABCDE12345".to_string()));
        std::fs::write(dir.join("Downloads/not-a-key.p8"), "hello").unwrap();
        assert!(keep_in_keys_dir(&dir.join("Downloads/not-a-key.p8"), "ABCDE12345", &keys).is_err());
    }

    #[test]
    fn reuses_a_key_another_project_has() {
        let root = std::env::temp_dir().join("appscreens-test-asckey-projects");
        let _ = std::fs::remove_dir_all(&root);
        let (a, b) = (root.join("nahw"), root.join("quran"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let p8 = root.join("AuthKey_ABCDE12345.p8");
        std::fs::write(&p8, P8).unwrap();
        std::fs::write(
            a.join(".env"),
            format!("{KEY_ID_VAR}=ABCDE12345\n{ISSUER_VAR}=69a6de97-00e6-47e3-e053-5b8c7c11a4d1\n{P8_VAR}={}\n", p8.display()),
        )
        .unwrap();
        let keys = from_other_projects(&[a.clone(), b.clone()], &b);
        assert_eq!(keys.len(), 1);
        assert_eq!((keys[0].key_id.as_str(), keys[0].from.as_str()), ("ABCDE12345", "nahw"));
        assert!(from_other_projects(std::slice::from_ref(&a), &a).is_empty(), "not the project itself");
    }
}
