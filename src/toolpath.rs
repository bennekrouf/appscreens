//! Find the developer's tools (`dx`, `cargo`, `rustup`) however AppScreens was opened.
//!
//! Opened from the Dock or Finder, a macOS app gets only `/usr/bin:/bin:/usr/sbin:/sbin`,
//! so nothing installed by rustup or Homebrew is found. And when Homebrew's deno is
//! installed, its own `dx` can shadow the Dioxus CLI. At startup, before any thread
//! exists, PATH becomes: cargo's bin folder, then the login shell's PATH, then the
//! inherited one. Every command AppScreens runs, build scripts included, inherits it.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

/// Rewrites this process's PATH. Call first thing in `main`, before any thread starts.
pub fn init() {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let path = tool_path(&current, cargo_bin(), login_shell_path());
    if path != current {
        tracing::info!("PATH for tools: {}", path.to_string_lossy());
        std::env::set_var("PATH", path);
    }
}

/// `cargo_bin`, then `login`, then `current`, each folder once, in that order.
fn tool_path(current: &OsStr, cargo_bin: Option<PathBuf>, login: Option<OsString>) -> OsString {
    let mut folders: Vec<PathBuf> = Vec::new();
    let candidates = cargo_bin
        .into_iter()
        .chain(login.iter().flat_map(|p| std::env::split_paths(p).collect::<Vec<_>>()))
        .chain(std::env::split_paths(current));
    for folder in candidates {
        if !folder.as_os_str().is_empty() && !folders.contains(&folder) {
            folders.push(folder);
        }
    }
    std::env::join_paths(folders).unwrap_or_else(|_| current.to_os_string())
}

/// `$CARGO_HOME/bin`, else `~/.cargo/bin`, if it exists.
fn cargo_bin() -> Option<PathBuf> {
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".cargo")))?;
    Some(home.join("bin")).filter(|bin| bin.is_dir())
}

const MARKER: &str = "__APPSCREENS_PATH__";

/// PATH as the developer's login shell sets it (reads `.zprofile`, `.bash_profile`…).
/// `None` on Windows and Android, or if the shell fails or takes over 3 seconds.
#[cfg(all(unix, not(target_os = "android")))]
fn login_shell_path() -> Option<OsString> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".into());
    let mut child = Command::new(shell)
        .args(["-l", "-c", &format!("printf '{MARKER}%s' \"$PATH\"")])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    // A profile that waits for input or hangs must not block startup.
    let deadline = Instant::now() + Duration::from_secs(3);
    while child.try_wait().ok()?.is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            tracing::warn!("Login shell took too long; using the inherited PATH");
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().ok()?;
    path_after_marker(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(not(all(unix, not(target_os = "android"))))]
fn login_shell_path() -> Option<OsString> {
    None
}

/// The text after the marker: profiles sometimes print banners before it.
#[cfg_attr(not(all(unix, not(target_os = "android"))), allow(dead_code))]
fn path_after_marker(output: &str) -> Option<OsString> {
    let path = output.rsplit_once(MARKER)?.1.trim();
    (!path.is_empty()).then(|| OsString::from(path))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn cargo_bin_comes_first_then_login_then_inherited() {
        let path = tool_path(
            OsStr::new("/usr/bin:/bin"),
            Some(PathBuf::from("/Users/me/.cargo/bin")),
            Some(OsString::from("/opt/homebrew/bin:/usr/bin:/Users/me/.cargo/bin")),
        );
        assert_eq!(path, OsString::from("/Users/me/.cargo/bin:/opt/homebrew/bin:/usr/bin:/bin"));
    }

    #[test]
    fn keeps_inherited_path_when_nothing_else_is_known() {
        assert_eq!(tool_path(OsStr::new("/usr/bin:/bin"), None, None), OsString::from("/usr/bin:/bin"));
    }

    #[test]
    fn reads_path_after_profile_banners() {
        let out = format!("Welcome!\n{MARKER}/opt/homebrew/bin:/usr/bin\n");
        assert_eq!(path_after_marker(&out), Some(OsString::from("/opt/homebrew/bin:/usr/bin")));
        assert_eq!(path_after_marker("no marker"), None);
        assert_eq!(path_after_marker(MARKER), None);
    }
}
