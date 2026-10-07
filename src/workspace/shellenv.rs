//! Opt-in: make the developer's own terminal use the same Java as AppScreens.
//!
//! AppScreens doesn't need this — it passes Java to the commands it runs.
//! It's for everything else (running Gradle by hand, other tools). The change
//! is a marked block at the end of the shell profile (or user environment
//! variables on Windows), shown before it's written, backed up, and removed
//! exactly by "Remove".

use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::process::Command;
use super::Quiet;

const BEGIN: &str = "# >>> AppScreens JAVA_HOME >>>";
const END: &str = "# <<< AppScreens JAVA_HOME <<<";

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Flavor {
    /// zsh, bash: `export`
    Posix,
    /// fish: `set -gx`, in its own conf.d file
    Fish,
}

#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Target {
    Profile { path: PathBuf, flavor: Flavor },
    /// User environment variables (Windows)
    WindowsUser,
}

impl Target {
    pub(crate) fn describe(&self) -> String {
        match self {
            Target::Profile { path, .. } => path.display().to_string(),
            Target::WindowsUser => "your Windows user environment variables".into(),
        }
    }
}

/// Where the developer's terminal reads its settings from.
pub(crate) fn detect_target() -> Target {
    if cfg!(windows) {
        return Target::WindowsUser;
    }
    let home = dirs::home_dir().unwrap_or_default();
    let shell = std::env::var("SHELL").unwrap_or_default();
    // An app started from the Dock may not inherit SHELL; macOS defaults to zsh.
    let shell = if shell.is_empty() && cfg!(target_os = "macos") { "/bin/zsh".to_string() } else { shell };
    let name = Path::new(&shell).file_name().and_then(|n| n.to_str()).unwrap_or("");
    match name {
        "fish" => Target::Profile { path: home.join(".config/fish/conf.d/appscreens-java.fish"), flavor: Flavor::Fish },
        // macOS Terminal opens login shells, which read .bash_profile, not .bashrc.
        "bash" if cfg!(target_os = "macos") => Target::Profile { path: home.join(".bash_profile"), flavor: Flavor::Posix },
        "bash" => Target::Profile { path: home.join(".bashrc"), flavor: Flavor::Posix },
        _ => Target::Profile { path: home.join(".zshrc"), flavor: Flavor::Posix },
    }
}

/// The lines AppScreens adds.
pub(crate) fn block(flavor: Flavor, java_home: &Path) -> String {
    let home = java_home.display().to_string().replace('"', "\\\"");
    match flavor {
        Flavor::Posix => format!("{BEGIN}\nexport JAVA_HOME=\"{home}\"\nexport PATH=\"$JAVA_HOME/bin:$PATH\"\n{END}\n"),
        Flavor::Fish => format!("{BEGIN}\nset -gx JAVA_HOME \"{home}\"\nset -gx PATH \"$JAVA_HOME/bin\" $PATH\n{END}\n"),
    }
}

/// `text` without AppScreens' block (and without the blank line before it).
pub(crate) fn remove_block(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if line.trim() == BEGIN {
            inside = true;
            while out.last().is_some_and(|l| l.trim().is_empty()) {
                out.pop();
            }
            continue;
        }
        if inside {
            if line.trim() == END {
                inside = false;
            }
            continue;
        }
        out.push(line);
    }
    let mut s = out.join("\n");
    if !s.is_empty() {
        s.push('\n');
    }
    s
}

/// `text` with AppScreens' block (replacing any earlier one) at the end, so
/// it wins over settings above it.
pub(crate) fn upsert_block(text: &str, block: &str) -> String {
    let base = remove_block(text);
    if base.trim().is_empty() { block.to_string() } else { format!("{base}\n{block}") }
}

/// The JAVA_HOME in AppScreens' block, if the block is there.
pub(crate) fn block_java_home(text: &str) -> Option<String> {
    let start = text.lines().position(|l| l.trim() == BEGIN)?;
    text.lines().skip(start + 1).take_while(|l| l.trim() != END).find_map(|l| {
        let l = l.trim();
        let v = l.strip_prefix("export JAVA_HOME=").or_else(|| l.strip_prefix("set -gx JAVA_HOME "))?;
        Some(v.trim().trim_matches('"').to_string())
    })
}

/// Line numbers (1-based) outside the block that also set JAVA_HOME.
pub(crate) fn other_java_home_lines(text: &str) -> Vec<usize> {
    let mut inside = false;
    let mut lines = Vec::new();
    for (n, l) in text.lines().enumerate() {
        let t = l.trim();
        if t == BEGIN {
            inside = true;
        } else if t == END {
            inside = false;
        } else if !inside && !t.starts_with('#') && t.contains("JAVA_HOME") && (t.contains("export") || t.contains("set -gx") || t.contains('=')) {
            lines.push(n + 1);
        }
    }
    lines
}

#[derive(Clone, PartialEq, Debug, Default)]
pub(crate) struct Status {
    /// JAVA_HOME AppScreens set, if it did
    pub installed: Option<String>,
    /// Other places that set JAVA_HOME (profile line numbers)
    pub others: Vec<usize>,
}

pub(crate) fn status(target: &Target) -> Status {
    match target {
        Target::Profile { path, .. } => {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            Status { installed: block_java_home(&text), others: other_java_home_lines(&text) }
        }
        Target::WindowsUser => windows_status(),
    }
}

/// Write the block (or Windows variables) for `java_home`.
pub(crate) fn apply(target: &Target, java_home: &Path) -> Result<String, String> {
    match target {
        Target::Profile { path, flavor } => {
            let old = std::fs::read_to_string(path).unwrap_or_default();
            if !old.is_empty() {
                // One backup of the file as it was before AppScreens first touched it.
                let backup = path.with_file_name(format!(
                    "{}.appscreens-backup",
                    path.file_name().and_then(|n| n.to_str()).unwrap_or("profile")
                ));
                if !backup.exists() {
                    std::fs::write(&backup, &old).map_err(|e| e.to_string())?;
                }
            }
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            std::fs::write(path, upsert_block(&old, &block(*flavor, java_home))).map_err(|e| e.to_string())?;
            Ok(format!("Added to {} — open a new terminal window to use it", path.display()))
        }
        Target::WindowsUser => windows_apply(java_home),
    }
}

/// Take out exactly what `apply` added.
pub(crate) fn remove(target: &Target) -> Result<String, String> {
    match target {
        Target::Profile { path, flavor } => {
            if *flavor == Flavor::Fish {
                // fish gets its own file; remove it whole.
                let _ = std::fs::remove_file(path);
                return Ok(format!("Removed {}", path.display()));
            }
            let old = std::fs::read_to_string(path).unwrap_or_default();
            std::fs::write(path, remove_block(&old)).map_err(|e| e.to_string())?;
            Ok(format!("Removed from {} — new terminal windows are back to before", path.display()))
        }
        Target::WindowsUser => windows_remove(),
    }
}

/// What a new terminal window actually gets: its JAVA_HOME, its `java`, and
/// the Java the project's Gradle runs on (when the Android project exists).
/// Runs the user's own shell as a login shell, so it reads the profile.
pub(crate) fn prove(target: &Target, gradle_dir: Option<&Path>) -> Vec<(String, String)> {
    let mut script = String::from("echo \"JAVA_HOME=$JAVA_HOME\"; java -version 2>&1");
    if let Some(dir) = gradle_dir {
        let dir = dir.display().to_string().replace('\'', "'\\''");
        script.push_str(&format!("; cd '{dir}' && ./gradlew --version 2>&1"));
    }
    let out = match target {
        Target::Profile { flavor, .. } => {
            let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| {
                if *flavor == Flavor::Fish { "fish".into() } else { "/bin/zsh".into() }
            });
            std::process::Command::new(shell).args(["-l", "-i", "-c", &script]).stdin(std::process::Stdio::null()).output()
        }
        Target::WindowsUser => std::process::Command::new("cmd")
            .quiet()
            .args(["/c", "echo JAVA_HOME=%JAVA_HOME% & java -version 2>&1"])
            .output(),
    };
    let text = match out {
        Ok(o) => format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)),
        Err(e) => return vec![("Terminal".into(), format!("Could not start your shell: {e}"))],
    };
    summarize_proof(&text, gradle_dir.is_some())
}

fn summarize_proof(text: &str, gradle: bool) -> Vec<(String, String)> {
    let line = |pred: &dyn Fn(&str) -> bool| text.lines().map(str::trim).find(|l| pred(l)).map(str::to_string);
    let mut rows = vec![
        ("JAVA_HOME".to_string(), line(&|l| l.starts_with("JAVA_HOME=")).map(|l| {
            let v = l.trim_start_matches("JAVA_HOME=").to_string();
            if v.is_empty() || v == "%JAVA_HOME%" { "not set".into() } else { v }
        }).unwrap_or_else(|| "not set".into())),
        ("java".to_string(), line(&|l| l.contains(" version \"")).unwrap_or_else(|| "not found on PATH".into())),
    ];
    if gradle {
        rows.push((
            "Gradle runs on".to_string(),
            line(&|l| l.starts_with("Launcher JVM:") || l.starts_with("Daemon JVM:") || l.starts_with("JVM:"))
                .map(|l| l.split_once(':').map(|(_, v)| v.trim().to_string()).unwrap_or(l))
                .unwrap_or_else(|| "Gradle did not report — see if it can download its distribution".into()),
        ));
    }
    rows
}

// ---------------------------------------------------------------------------
// Windows: user environment, through .NET (setx truncates long PATHs)
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn powershell(script: &str) -> Result<String, String> {
    let out = Command::new("powershell").quiet().args(["-NoProfile", "-NonInteractive", "-Command", script]).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[cfg(windows)]
fn windows_status() -> Status {
    let marker = powershell("[Environment]::GetEnvironmentVariable('APPSCREENS_JAVA_HOME','User')").unwrap_or_default();
    Status { installed: (!marker.is_empty()).then_some(marker), others: vec![] }
}

#[cfg(windows)]
fn windows_apply(java_home: &Path) -> Result<String, String> {
    let home = java_home.display().to_string().replace('\'', "''");
    // APPSCREENS_JAVA_HOME records that AppScreens made the change, so Remove
    // only undoes its own.
    powershell(&format!(
        "$h='{home}'; [Environment]::SetEnvironmentVariable('JAVA_HOME',$h,'User'); \
         [Environment]::SetEnvironmentVariable('APPSCREENS_JAVA_HOME',$h,'User'); \
         $p=[Environment]::GetEnvironmentVariable('Path','User'); if ($p -eq $null) {{ $p='' }}; \
         if (($p -split ';') -notcontains '%JAVA_HOME%\\bin') {{ \
           [Environment]::SetEnvironmentVariable('Path', ('%JAVA_HOME%\\bin;' + $p).TrimEnd(';'), 'User') }}"
    ))?;
    Ok("Set for your Windows user — open a new terminal window to use it".into())
}

#[cfg(windows)]
fn windows_remove() -> Result<String, String> {
    powershell(
        "[Environment]::SetEnvironmentVariable('JAVA_HOME',$null,'User'); \
         [Environment]::SetEnvironmentVariable('APPSCREENS_JAVA_HOME',$null,'User'); \
         $p=[Environment]::GetEnvironmentVariable('Path','User'); if ($p) { \
           $kept=($p -split ';' | Where-Object { $_ -ne '%JAVA_HOME%\\bin' }) -join ';'; \
           [Environment]::SetEnvironmentVariable('Path',$kept,'User') }",
    )?;
    Ok("Removed from your Windows user environment".into())
}

#[cfg(not(windows))]
fn windows_status() -> Status {
    Status::default()
}
#[cfg(not(windows))]
fn windows_apply(_: &Path) -> Result<String, String> {
    Err("Windows only".into())
}
#[cfg(not(windows))]
fn windows_remove() -> Result<String, String> {
    Err("Windows only".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const JDK: &str = "/Applications/Android Studio.app/Contents/jbr/Contents/Home";

    #[test]
    fn adds_replaces_and_removes_exactly_its_block() {
        let original = "export PATH=\"/opt/homebrew/bin:$PATH\"\nexport JAVA_HOME=/old/jdk\n";
        let b = block(Flavor::Posix, Path::new(JDK));
        let once = upsert_block(original, &b);
        assert!(once.starts_with(original) && once.ends_with(&b));
        assert_eq!(block_java_home(&once).as_deref(), Some(JDK));
        assert_eq!(other_java_home_lines(&once), vec![2], "the user's own line is reported, not ours");

        // Applying again (another JDK) replaces, never duplicates.
        let twice = upsert_block(&once, &block(Flavor::Posix, Path::new("/other/jdk")));
        assert_eq!(twice.matches(BEGIN).count(), 1);
        assert_eq!(block_java_home(&twice).as_deref(), Some("/other/jdk"));

        // Remove restores the file exactly.
        assert_eq!(remove_block(&twice), original);
        assert_eq!(block_java_home(original), None);
    }

    #[test]
    fn fish_block_and_empty_profiles() {
        let b = block(Flavor::Fish, Path::new(JDK));
        assert!(b.contains("set -gx JAVA_HOME"));
        assert_eq!(upsert_block("", &b), b);
        assert_eq!(block_java_home(&b).as_deref(), Some(JDK));
    }

    #[test]
    fn summarizes_what_the_terminal_uses() {
        let out = "JAVA_HOME=/jdk21\nopenjdk version \"21.0.8\" 2025-07-15\nOpenJDK Runtime\n\nGradle 9.1.0\nLauncher JVM:  21.0.8 (Eclipse Adoptium 21.0.8+9)\n";
        let rows = summarize_proof(out, true);
        assert_eq!(rows[0].1, "/jdk21");
        assert!(rows[1].1.contains("21.0.8"));
        assert_eq!(rows[2].1, "21.0.8 (Eclipse Adoptium 21.0.8+9)");
        let none = summarize_proof("JAVA_HOME=\nzsh: command not found: java\n", false);
        assert_eq!(none[0].1, "not set");
        assert_eq!(none[1].1, "not found on PATH");
        assert_eq!(none.len(), 2);
    }

    #[test]
    fn applies_to_a_profile_with_a_backup() {
        let dir = std::env::temp_dir().join("appscreens-test-shellenv");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".zshrc");
        std::fs::write(&path, "alias ll='ls -l'\n").unwrap();
        let target = Target::Profile { path: path.clone(), flavor: Flavor::Posix };

        apply(&target, Path::new(JDK)).unwrap();
        assert_eq!(status(&target).installed.as_deref(), Some(JDK));
        assert_eq!(std::fs::read_to_string(dir.join(".zshrc.appscreens-backup")).unwrap(), "alias ll='ls -l'\n");

        remove(&target).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "alias ll='ls -l'\n");
        assert_eq!(status(&target).installed, None);
    }
}
