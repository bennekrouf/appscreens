//! Writing settings into a project's `.env` the way a developer would: a line
//! that's already there (in `.env` or `fastlane/.env`, whichever defines it)
//! is changed in place, anything new goes into `.env`, the file stays private
//! to the user, and git is told to ignore it.

use std::path::Path;

/// `text` with `key` set to `value`: the existing line replaced, else appended.
pub(super) fn upsert(text: &str, key: &str, value: &str) -> String {
    let line = format!("{key}={}", quote(value));
    let mut found = false;
    let mut out: Vec<String> = text
        .lines()
        .map(|l| {
            let t = l.trim_start().strip_prefix("export ").unwrap_or(l.trim_start());
            if !found && t.split_once('=').is_some_and(|(k, _)| k.trim() == key) {
                found = true;
                line.clone()
            } else {
                l.to_string()
            }
        })
        .collect();
    if !found {
        out.push(line);
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

fn quote(v: &str) -> String {
    if v.chars().any(|c| c.is_whitespace() || c == '#' || c == '"') {
        format!("\"{}\"", v.replace('"', "\\\""))
    } else {
        v.to_string()
    }
}

fn defines(text: &str, key: &str) -> bool {
    text.lines().any(|l| {
        let t = l.trim_start().strip_prefix("export ").unwrap_or(l.trim_start());
        !t.starts_with('#') && t.split_once('=').is_some_and(|(k, _)| k.trim() == key)
    })
}

/// Set `pairs` for the project. `fastlane/.env` wins over `.env` when both
/// define a key, so a key it already defines is changed there.
pub(super) fn set_values(project: &Path, pairs: &[(&str, &str)]) -> Result<(), String> {
    let main = project.join(".env");
    let fastlane = project.join("fastlane").join(".env");
    let mut main_text = std::fs::read_to_string(&main).unwrap_or_default();
    let mut fl_text = std::fs::read_to_string(&fastlane).ok();
    for (key, value) in pairs {
        match fl_text.as_mut().filter(|t| defines(t, key)) {
            Some(t) => *t = upsert(t, key, value),
            None => main_text = upsert(&main_text, key, value),
        }
    }
    write_private(&main, &main_text)?;
    if let Some(t) = fl_text {
        write_private(&fastlane, &t)?;
    }
    ignore_in_git(project)
}

fn write_private(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Make sure `.gitignore` covers `.env` files.
fn ignore_in_git(project: &Path) -> Result<(), String> {
    let path = project.join(".gitignore");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let covered = text.lines().map(str::trim).any(|l| matches!(l, ".env" | "/.env" | "*.env" | ".env*" | "**/.env"));
    if covered {
        return Ok(());
    }
    let sep = if text.is_empty() || text.ends_with('\n') { "" } else { "\n" };
    std::fs::write(&path, format!("{text}{sep}.env\n")).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_in_place_and_appends_new_keys() {
        let text = "# App Store\nAPP_STORE_CONNECT_API_KEY_KEY_ID=OLD\nexport OTHER=1\n";
        let t = upsert(text, "APP_STORE_CONNECT_API_KEY_KEY_ID", "4TALSPNY5Y");
        assert_eq!(t, "# App Store\nAPP_STORE_CONNECT_API_KEY_KEY_ID=4TALSPNY5Y\nexport OTHER=1\n");
        let t = upsert(&t, "APP_STORE_CONNECT_API_KEY_KEY_FILEPATH", "/Users/me/my keys/AuthKey.p8");
        assert!(t.ends_with("APP_STORE_CONNECT_API_KEY_KEY_FILEPATH=\"/Users/me/my keys/AuthKey.p8\"\n"));
        assert_eq!(upsert("", "A", "b"), "A=b\n");
    }

    #[test]
    fn writes_where_the_key_already_lives_and_ignores_env_in_git() {
        let dir = std::env::temp_dir().join("appscreens-test-envfile");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("fastlane")).unwrap();
        std::fs::write(dir.join("fastlane/.env"), "APP_STORE_CONNECT_API_KEY_ISSUER_ID=old\n").unwrap();
        std::fs::write(dir.join(".gitignore"), "/target").unwrap();
        set_values(&dir, &[("APP_STORE_CONNECT_API_KEY_ISSUER_ID", "new"), ("APP_STORE_CONNECT_API_KEY_KEY_ID", "K")]).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("fastlane/.env")).unwrap(), "APP_STORE_CONNECT_API_KEY_ISSUER_ID=new\n");
        assert_eq!(std::fs::read_to_string(dir.join(".env")).unwrap(), "APP_STORE_CONNECT_API_KEY_KEY_ID=K\n");
        assert_eq!(std::fs::read_to_string(dir.join(".gitignore")).unwrap(), "/target\n.env\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(dir.join(".env")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // Already ignored: untouched.
        set_values(&dir, &[("X", "1")]).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(".gitignore")).unwrap(), "/target\n.env\n");
    }
}
