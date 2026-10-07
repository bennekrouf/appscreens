//! Local signing material: the macOS Keychain for passwords, the Android
//! upload keystore (keytool), secrets accidentally tracked by git, and an
//! encrypted backup of everything in the keys folder.
//!
//! All of it shells out to the tools a developer already has (`security`,
//! `keytool`, `git`, `tar`, `openssl`) rather than reimplementing them.
//! Secrets are passed on stdin or through the environment, never as
//! command-line arguments, which other processes can read.

use super::*;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Keychain item service name for everything AppScreens stores.
const KEYCHAIN_SERVICE: &str = "AppScreens";

/// Default folder for signing material, outside every project.
pub(super) fn default_keys_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join("keys")
}

/// `~/…` → absolute, as the build scripts expand it.
pub(super) fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(path),
    }
}

fn run(cmd: &mut Command) -> Result<String, String> {
    let out = cmd.quiet().output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        // keytool reports its errors on stdout, the others on stderr.
        let err = format!("{}\n{}", String::from_utf8_lossy(&out.stderr), String::from_utf8_lossy(&out.stdout));
        let first = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("failed");
        Err(first.trim().trim_start_matches("keytool error: ").to_string())
    }
}

// ---------------------------------------------------------------------------
// Java (keytool, jarsigner, and Gradle's runtime)
// ---------------------------------------------------------------------------

/// The JDK home AppScreens uses — chosen in the Build step, else the
/// recommended one (see `java`). An app started from the Dock has no shell
/// PATH, and macOS's /usr/bin/keytool is only a stub that asks for Java.
pub(super) fn java_home() -> Option<PathBuf> {
    java::selected().map(|j| j.home)
}

/// A JDK tool ("keytool", "jarsigner"…) from `java_home`, else from PATH.
pub(super) fn jdk_tool(name: &str) -> PathBuf {
    java_home().map(|h| h.join("bin").join(name)).unwrap_or_else(|| PathBuf::from(name))
}

// ---------------------------------------------------------------------------
// Keychain
// ---------------------------------------------------------------------------

/// Keychain account name for a keystore's password.
pub(super) fn keystore_account(keystore: &Path) -> String {
    format!("android-keystore:{}", keystore.display())
}

/// What the password store is called on this OS, for messages.
pub(super) fn password_store_name() -> &'static str {
    if cfg!(windows) {
        "Windows Credential Manager"
    } else {
        "the Keychain"
    }
}

/// Whether this OS has a password store AppScreens can use.
pub(super) fn has_password_store() -> bool {
    cfg!(target_os = "macos") || cfg!(windows)
}

#[cfg(windows)]
pub(super) fn keychain_get(account: &str) -> Option<String> {
    keyring::Entry::new(KEYCHAIN_SERVICE, account).ok()?.get_password().ok().filter(|s| !s.is_empty())
}

#[cfg(not(windows))]
pub(super) fn keychain_get(account: &str) -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    run(Command::new("security").args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-a", account, "-w"]))
        .ok()
        .map(|s| s.trim_end_matches('\n').to_string())
        .filter(|s| !s.is_empty())
}

/// Store (or replace) a secret in Windows Credential Manager.
#[cfg(windows)]
pub(super) fn keychain_set(account: &str, secret: &str) -> Result<(), String> {
    keyring::Entry::new(KEYCHAIN_SERVICE, account)
        .and_then(|e| e.set_password(secret))
        .map_err(|e| format!("Windows Credential Manager refused the password: {e}"))
}

/// Store (or replace) a secret in the Keychain. Fed to `security -i` on
/// stdin so it never appears in the process list.
#[cfg(not(windows))]
pub(super) fn keychain_set(account: &str, secret: &str) -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        return Err("This system has no password store AppScreens can use — put the password in the project's .env instead.".into());
    }
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let mut child = Command::new("security")
        .arg("-i")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    writeln!(
        child.stdin.take().ok_or("no stdin")?,
        "add-generic-password -U -s {} -a {} -w {}",
        quote(KEYCHAIN_SERVICE),
        quote(account),
        quote(secret)
    )
    .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let err = String::from_utf8_lossy(&out.stderr);
    if out.status.success() && !err.contains("error") {
        Ok(())
    } else {
        Err(format!("Keychain refused the password: {}", err.trim()))
    }
}

// ---------------------------------------------------------------------------
// Android upload keystore
// ---------------------------------------------------------------------------

/// Key aliases in a keystore, which also proves the password is right.
pub(super) fn keystore_aliases(keystore: &Path, password: &str) -> Result<Vec<String>, String> {
    let out = run(Command::new(jdk_tool("keytool"))
        .args(["-list", "-keystore"])
        .arg(keystore)
        .args(["-storepass:env", "APPSCREENS_KSP"])
        .env("APPSCREENS_KSP", password))
    .map_err(|e| if e.contains("password was incorrect") { "Wrong keystore password".to_string() } else { e })?;
    Ok(out
        .lines()
        .filter(|l| l.contains("PrivateKeyEntry"))
        .filter_map(|l| l.split(',').next())
        .map(|a| a.trim().to_string())
        .collect())
}

/// Distinguished name for a new key, e.g. "CN=Jane Doe, O=Acme, C=CH".
pub(super) fn dname(name: &str, org: &str, country: &str) -> String {
    let clean = |s: &str| s.replace(',', " ").trim().to_string();
    let mut parts = vec![format!("CN={}", clean(name))];
    if !org.trim().is_empty() {
        parts.push(format!("O={}", clean(org)));
    }
    if !country.trim().is_empty() {
        parts.push(format!("C={}", clean(country)));
    }
    parts.join(", ")
}

/// Add an upload key to `keystore` (created if missing) and export its
/// certificate as PEM next to it — the file Play Console asks for. Returns
/// the PEM path. Storing the password is the caller's job.
pub(super) fn create_upload_key(keystore: &Path, alias: &str, password: &str, dname: &str) -> Result<PathBuf, String> {
    if password.chars().count() < 6 {
        return Err("The password must be at least 6 characters (keytool's minimum).".into());
    }
    if keystore.exists() && keystore_aliases(keystore, password)?.iter().any(|a| a == alias) {
        return Err(format!("The keystore already has a key named \"{alias}\"."));
    }
    if let Some(dir) = keystore.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    run(Command::new(jdk_tool("keytool"))
        .args(["-genkeypair", "-noprompt", "-keystore"])
        .arg(keystore)
        .args(["-alias", alias, "-keyalg", "RSA", "-keysize", "2048", "-validity", "10000", "-dname", dname])
        .args(["-storepass:env", "APPSCREENS_KSP", "-keypass:env", "APPSCREENS_KSP"])
        .env("APPSCREENS_KSP", password))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(keystore, std::fs::Permissions::from_mode(0o600));
    }
    let pem = keystore.with_file_name(format!("{alias}_upload_certificate.pem"));
    run(Command::new(jdk_tool("keytool"))
        .args(["-exportcert", "-rfc", "-keystore"])
        .arg(keystore)
        .args(["-alias", alias, "-file"])
        .arg(&pem)
        .args(["-storepass:env", "APPSCREENS_KSP"])
        .env("APPSCREENS_KSP", password))?;
    Ok(pem)
}

/// Upload keystores lying in `folders` (not recursive): `.jks` and `.keystore`
/// files, except Android's debug keystore. Sorted, each listed once.
pub(super) fn find_keystores(folders: &[PathBuf]) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = folders
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flat_map(|entries| entries.flatten().map(|e| e.path()))
        .filter(|p| p.is_file())
        .filter(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("jks" | "keystore")))
        .filter(|p| p.file_name().is_some_and(|n| n != "debug.keystore"))
        .collect();
    found.sort();
    found.dedup();
    found
}

/// SHA-256 fingerprint of one key's certificate, as Play Console shows it.
pub(super) fn upload_key_fingerprint(keystore: &Path, alias: &str, password: &str) -> Result<String, String> {
    let out = run(Command::new(jdk_tool("keytool"))
        .args(["-list", "-v", "-keystore"])
        .arg(keystore)
        .args(["-alias", alias, "-storepass:env", "APPSCREENS_KSP"])
        .env("APPSCREENS_KSP", password))?;
    out.lines()
        .find_map(|l| l.trim().strip_prefix("SHA256:").map(|f| f.trim().to_string()))
        .ok_or_else(|| "No SHA-256 fingerprint in keytool output".into())
}

// ---------------------------------------------------------------------------
// Secrets tracked by git
// ---------------------------------------------------------------------------

/// Tracked files that hold secrets: `.env`, private keys and keystores, and
/// JSON files carrying a `private_key` (Google service accounts).
pub(super) fn tracked_secrets(project: &Path) -> Vec<String> {
    let Ok(out) = run(Command::new("git").arg("-C").arg(project).args(["ls-files", "-z"])) else {
        return vec![]; // not a git repo, or git missing
    };
    out.split('\0')
        .filter(|f| !f.is_empty())
        .filter(|f| {
            let name = Path::new(f).file_name().and_then(|n| n.to_str()).unwrap_or("");
            let ext = Path::new(f).extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
            name == ".env"
                || name.starts_with(".env.") && name != ".env.example"
                || matches!(ext.as_str(), "p8" | "p12" | "jks" | "keystore" | "key")
                || ext == "json"
                    && std::fs::read_to_string(project.join(f)).is_ok_and(|s| s.contains("\"private_key\""))
        })
        .map(str::to_string)
        .collect()
}

/// Stop tracking those files (they stay on disk) and ignore them from now on.
/// Nothing is committed — the change is left staged for the developer.
pub(super) fn untrack_secrets(project: &Path, files: &[String]) -> Result<(), String> {
    let gitignore = project.join(".gitignore");
    let existing = std::fs::read_to_string(&gitignore).unwrap_or_default();
    let missing: Vec<&String> = files.iter().filter(|f| !existing.lines().any(|l| l.trim() == f.as_str())).collect();
    if !missing.is_empty() {
        let mut add = String::from(if existing.ends_with('\n') || existing.is_empty() { "" } else { "\n" });
        add.push_str("\n# Secrets — never commit (added by AppScreens)\n");
        for f in missing {
            add.push_str(f);
            add.push('\n');
        }
        std::fs::write(&gitignore, existing + &add).map_err(|e| e.to_string())?;
    }
    run(Command::new("git").arg("-C").arg(project).args(["rm", "--cached", "-q", "--"]).args(files))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Encrypted backup
// ---------------------------------------------------------------------------

/// Newest modification time among the files that would be backed up — a
/// backup older than this is out of date.
pub(super) fn newest_mtime(paths: &[PathBuf]) -> Option<std::time::SystemTime> {
    let mut newest = None;
    for p in paths {
        let entries: Vec<PathBuf> = if p.is_dir() {
            std::fs::read_dir(p).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default()
        } else {
            vec![p.clone()]
        };
        for e in entries {
            if let Ok(m) = std::fs::metadata(&e).and_then(|m| m.modified()) {
                if newest.is_none_or(|n| m > n) {
                    newest = Some(m);
                }
            }
        }
    }
    newest
}

/// Write `sources` (files or folders) into one AES-256 encrypted archive.
/// Restore with: `openssl enc -d -aes-256-cbc -pbkdf2 -in FILE | tar xz`
pub(super) fn export_backup(sources: &[PathBuf], dest: &Path, password: &str) -> Result<(), String> {
    if password.chars().count() < 8 {
        return Err("Use a backup password of at least 8 characters.".into());
    }
    let existing: Vec<&PathBuf> = sources.iter().filter(|p| p.exists()).collect();
    if existing.is_empty() {
        return Err("Nothing to back up yet.".into());
    }
    let mut tar = Command::new("tar");
    tar.arg("-czf").arg("-");
    for p in &existing {
        // Relative to the parent keeps paths short and restorable anywhere.
        tar.arg("-C").arg(p.parent().unwrap_or(Path::new("/"))).arg(p.file_name().unwrap_or_default());
    }
    let mut tar = tar.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    let out = Command::new("openssl")
        .args(["enc", "-aes-256-cbc", "-pbkdf2", "-salt", "-pass", "env:APPSCREENS_BKP", "-out"])
        .arg(dest)
        .env("APPSCREENS_BKP", password)
        .stdin(tar.stdout.take().ok_or("no tar output")?)
        .output()
        .map_err(|e| e.to_string())?;
    let tar_ok = tar.wait().map(|s| s.success()).unwrap_or(false);
    if !out.status.success() || !tar_ok {
        let _ = std::fs::remove_file(dest);
        return Err(format!("Backup failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Apple: private key + certificate request, and keychain import
// ---------------------------------------------------------------------------

/// A new RSA key and its certificate signing request, both written to
/// `keys_dir` (readable only by the user). Returns (key path, CSR PEM text).
pub(super) fn apple_key_and_csr(keys_dir: &Path, stem: &str, email: &str, name: &str) -> Result<(PathBuf, String), String> {
    std::fs::create_dir_all(keys_dir).map_err(|e| e.to_string())?;
    let key = keys_dir.join(format!("{stem}.key"));
    let csr = keys_dir.join(format!("{stem}.certSigningRequest"));
    let subject = format!("/emailAddress={email}/CN={}", name.replace('/', " "));
    run(Command::new("openssl")
        .args(["req", "-new", "-newkey", "rsa:2048", "-nodes", "-keyout"])
        .arg(&key)
        .arg("-out")
        .arg(&csr)
        .args(["-subj", &subject]))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for f in [&key, &csr] {
            let _ = std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o600));
        }
    }
    let text = std::fs::read_to_string(&csr).map_err(|e| e.to_string())?;
    Ok((key, text))
}

/// Bundle key + certificate (DER) into a password-protected `.p12` in
/// `keys_dir` — the backup — and import it into the login keychain so
/// `codesign` can use it. Returns the `.p12` path.
pub(super) fn import_apple_identity(key: &Path, cert_der: &[u8], label: &str, p12_password: &str) -> Result<PathBuf, String> {
    if p12_password.chars().count() < 8 {
        return Err("Use a .p12 password of at least 8 characters.".into());
    }
    let cer = key.with_extension("cer");
    std::fs::write(&cer, cert_der).map_err(|e| e.to_string())?;
    let pem = key.with_extension("crt.pem");
    run(Command::new("openssl").args(["x509", "-inform", "der", "-in"]).arg(&cer).arg("-out").arg(&pem))?;
    let p12 = key.with_extension("p12");
    let exported = run(Command::new("openssl")
        .args(["pkcs12", "-export", "-legacy", "-inkey"])
        .arg(key)
        .arg("-in")
        .arg(&pem)
        .args(["-name", label, "-passout", "env:APPSCREENS_P12", "-out"])
        .arg(&p12)
        .env("APPSCREENS_P12", p12_password));
    let _ = std::fs::remove_file(&pem);
    exported?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for f in [&p12, &cer] {
            let _ = std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o600));
        }
    }
    let keychain = dirs::home_dir().unwrap_or_default().join("Library/Keychains/login.keychain-db");
    // `security import -P` takes the password as an argument; hand it over
    // through `security -i` on stdin instead.
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let mut child = Command::new("security")
        .arg("-i")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    writeln!(
        child.stdin.take().ok_or("no stdin")?,
        "import {} -k {} -P {} -T /usr/bin/codesign",
        quote(&p12.to_string_lossy()),
        quote(&keychain.to_string_lossy()),
        quote(p12_password)
    )
    .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let msg = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if msg.contains("error") && !msg.contains("already exists") {
        return Err(format!("Keychain import failed: {}", msg.trim()));
    }
    Ok(p12)
}

/// SHA-1 of a DER certificate, uppercase hex — what `security find-identity`
/// prints for each signing identity.
pub(super) fn cert_sha1(der: &[u8]) -> Option<String> {
    let mut child = Command::new("shasum").args(["-a", "1"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().ok()?;
    child.stdin.take()?.write_all(der).ok()?;
    let out = child.wait_with_output().ok()?;
    String::from_utf8_lossy(&out.stdout).split_whitespace().next().map(|h| h.to_uppercase())
}

/// SHA-1 hashes of the valid code-signing identities in the keychain.
pub(super) fn keychain_identity_hashes() -> Vec<String> {
    run(Command::new("security").args(["find-identity", "-v", "-p", "codesigning"]))
        .map(|out| {
            out.lines()
                .filter_map(|l| l.split_whitespace().nth(1))
                .filter(|h| h.len() == 40)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_upload_keystores_but_not_debug_or_apple_files() {
        let dir = std::env::temp_dir().join("appscreens-test-find-keystores");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        for name in ["upload.jks", "old.keystore", "debug.keystore", "apple.p12", "notes.txt", "nested/deep.jks"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        let found = find_keystores(&[dir.clone(), dir.clone(), dir.join("missing")]);
        assert_eq!(found, vec![dir.join("old.keystore"), dir.join("upload.jks")]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn distinguished_names_drop_commas() {
        assert_eq!(dname("Jane Doe", "Acme, Inc", "CH"), "CN=Jane Doe, O=Acme  Inc, C=CH");
        assert_eq!(dname("Jane", "", ""), "CN=Jane");
    }

    fn git_repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("appscreens-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap();
        dir
    }

    #[test]
    fn finds_and_untracks_committed_secrets() {
        let dir = git_repo("secrets");
        std::fs::write(dir.join(".env"), "A=1\n").unwrap();
        std::fs::write(dir.join("AuthKey_X.p8"), "k").unwrap();
        std::fs::write(dir.join("svc.json"), r#"{"private_key": "x"}"#).unwrap();
        std::fs::write(dir.join("package.json"), r#"{"name": "x"}"#).unwrap();
        std::fs::write(dir.join(".env.example"), "A=\n").unwrap();
        Command::new("git").arg("-C").arg(&dir).args(["add", "."]).status().unwrap();

        let mut found = tracked_secrets(&dir);
        found.sort();
        assert_eq!(found, vec![".env", "AuthKey_X.p8", "svc.json"]);

        untrack_secrets(&dir, &found).unwrap();
        assert!(tracked_secrets(&dir).is_empty());
        assert!(dir.join(".env").exists(), "files stay on disk");
        let ignore = std::fs::read_to_string(dir.join(".gitignore")).unwrap();
        assert!(ignore.contains(".env\n") && ignore.contains("svc.json\n"));
    }

    #[test]
    fn creates_an_upload_key_and_lists_it() {
        if java_home().is_none() {
            return; // no JDK on this machine
        }
        let dir = std::env::temp_dir().join("appscreens-test-keystore");
        let _ = std::fs::remove_dir_all(&dir);
        let ks = dir.join("upload.jks");
        let pem = create_upload_key(&ks, "demo", "test-pass-123", &dname("Test", "", "CH")).unwrap();
        assert!(pem.exists());
        assert_eq!(keystore_aliases(&ks, "test-pass-123").unwrap(), vec!["demo"]);
        assert!(keystore_aliases(&ks, "wrong-pass").unwrap_err().contains("Wrong"));
        assert!(create_upload_key(&ks, "demo", "test-pass-123", "CN=x").unwrap_err().contains("already"));
        assert_eq!(upload_key_fingerprint(&ks, "demo", "test-pass-123").unwrap().split(':').count(), 32);
    }

    #[test]
    fn backup_round_trips() {
        let dir = std::env::temp_dir().join("appscreens-test-backup");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("keys")).unwrap();
        std::fs::write(dir.join("keys/a.p8"), "secret").unwrap();
        let dest = dir.join("backup.enc");
        export_backup(&[dir.join("keys")], &dest, "backup-pass-1").unwrap();
        let out = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "openssl enc -d -aes-256-cbc -pbkdf2 -pass pass:backup-pass-1 -in '{}' | tar xzO keys/a.p8",
                dest.display()
            ))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "secret");
        assert!(export_backup(&[dir.join("keys")], &dest, "short").is_err());
    }
}
