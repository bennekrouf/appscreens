//! Which Java AppScreens hands to keytool and Gradle.
//!
//! Setting JAVA_HOME system-wide is different — and fragile — on every OS,
//! so AppScreens doesn't rely on it: it finds every JDK installed, checks each
//! against the Gradle version the project builds with, and uses the chosen
//! (or recommended) one for the commands it runs itself. The developer's
//! shell and system stay as they are.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::RwLock;

#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Jdk {
    pub home: PathBuf,
    /// Feature release, e.g. 21 (8 for "1.8.0_392")
    pub major: u32,
    /// As `java -version` prints it, e.g. "21.0.12.1"
    pub version: String,
    /// Where it was found: "Android Studio", "Homebrew", "JAVA_HOME"…
    pub source: &'static str,
}

/// The oldest Java the Android Gradle plugin (8.x) accepts.
pub(crate) const MIN_JAVA: u32 = 17;
/// Gradle used when the project hasn't been bundled yet (dx 0.7's template).
const DEFAULT_GRADLE: (u32, u32) = (8, 10);

/// "openjdk version \"21.0.12.1\" 2026-08-18" → ("21.0.12.1", 21);
/// "java version \"1.8.0_392\"" → ("1.8.0_392", 8).
pub(crate) fn parse_java_version(output: &str) -> Option<(String, u32)> {
    let line = output.lines().find(|l| l.contains("version \""))?;
    let v = line.split('"').nth(1)?.to_string();
    let mut parts = v.split(['.', '_', '-', '+']);
    let first: u32 = parts.next()?.parse().ok()?;
    let major = if first == 1 { parts.next()?.parse().ok()? } else { first };
    Some((v, major))
}

/// The newest Java a Gradle version can run on (Gradle's compatibility
/// matrix). Unknown future versions get the newest known limit.
pub(crate) fn max_java_for_gradle(gradle: (u32, u32)) -> u32 {
    const MATRIX: &[((u32, u32), u32)] = &[
        ((7, 3), 17),
        ((7, 5), 18),
        ((7, 6), 19),
        ((8, 3), 20),
        ((8, 5), 21),
        ((8, 8), 22),
        ((8, 10), 23),
        ((8, 14), 24),
        ((9, 1), 25),
        ((9, 3), 26),
    ];
    MATRIX.iter().rev().find(|(g, _)| gradle >= *g).map(|(_, j)| *j).unwrap_or(16)
}

/// Gradle version from a wrapper's `distributionUrl=…/gradle-8.10.2-bin.zip`.
pub(crate) fn parse_gradle_wrapper(props: &str) -> Option<(u32, u32)> {
    let url = props.lines().find_map(|l| l.trim().strip_prefix("distributionUrl="))?;
    let name = url.rsplit('/').next()?;
    let ver = name.strip_prefix("gradle-")?.split('-').next()?;
    let mut it = ver.split('.');
    Some((it.next()?.parse().ok()?, it.next().unwrap_or("0").parse().ok()?))
}

/// The Gradle version this project builds with: the wrapper dx generated,
/// else dx's default.
pub(crate) fn project_gradle(project: &Path) -> ((u32, u32), bool) {
    let found = std::fs::read_dir(project.join("target/dx"))
        .ok()
        .into_iter()
        .flat_map(|d| d.flatten())
        .map(|e| e.path().join("release/android/app/gradle/wrapper/gradle-wrapper.properties"))
        .find_map(|p| std::fs::read_to_string(p).ok().and_then(|s| parse_gradle_wrapper(&s)));
    match found {
        Some(v) => (v, true),
        None => (DEFAULT_GRADLE, false),
    }
}

/// The Android Gradle project dx generated, when it has a wrapper to run.
pub(crate) fn gradle_project_dir(project: &Path) -> Option<std::path::PathBuf> {
    std::fs::read_dir(project.join("target/dx"))
        .ok()?
        .flatten()
        .map(|e| e.path().join("release/android/app"))
        .find(|d| d.join(if cfg!(windows) { "gradlew.bat" } else { "gradlew" }).exists())
}

pub(crate) fn compatible(jdk: &Jdk, gradle: (u32, u32)) -> bool {
    jdk.major >= MIN_JAVA && jdk.major <= max_java_for_gradle(gradle)
}

/// Why a JDK can't be used, or None.
pub(crate) fn incompatibility(jdk: &Jdk, gradle: (u32, u32)) -> Option<String> {
    if jdk.major < MIN_JAVA {
        Some(format!("Java {} is too old — Android builds need {MIN_JAVA} or newer", jdk.major))
    } else if jdk.major > max_java_for_gradle(gradle) {
        Some(format!(
            "Java {} is too new for Gradle {}.{} (up to Java {})",
            jdk.major,
            gradle.0,
            gradle.1,
            max_java_for_gradle(gradle)
        ))
    } else {
        None
    }
}

/// Best compatible JDK: LTS releases first (21, then 17), Android Studio's
/// bundled runtime before others of the same release, then the newest.
pub(crate) fn recommend(jdks: &[Jdk], gradle: (u32, u32)) -> Option<Jdk> {
    let rank = |j: &Jdk| {
        let lts = match j.major {
            21 => 3,
            17 => 2,
            _ => 1,
        };
        (lts, (j.source == "Android Studio") as u8, j.major)
    };
    jdks.iter().filter(|j| compatible(j, gradle)).max_by_key(|j| rank(j)).cloned()
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

fn java_exe(home: &Path) -> PathBuf {
    home.join("bin").join(if cfg!(windows) { "java.exe" } else { "java" })
}

/// Describe the JDK at `home`, if it is one (it must have keytool too).
pub(crate) fn probe(home: &Path, source: &'static str) -> Option<Jdk> {
    let keytool = home.join("bin").join(if cfg!(windows) { "keytool.exe" } else { "keytool" });
    if !java_exe(home).is_file() || !keytool.is_file() {
        return None;
    }
    let out = Command::new(java_exe(home)).arg("-version").output().ok()?;
    // `java -version` writes to stderr.
    let text = format!("{}{}", String::from_utf8_lossy(&out.stderr), String::from_utf8_lossy(&out.stdout));
    let (version, major) = parse_java_version(&text)?;
    let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    Some(Jdk { home, major, version, source })
}

/// Sub-folders of `dir` that are JDK homes, `suffix` appended (e.g. macOS
/// bundles keep the home in `Contents/Home`).
fn homes_in(dir: &Path, suffix: &str) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|d| d.flatten().map(|e| if suffix.is_empty() { e.path() } else { e.path().join(suffix) }).collect())
        .unwrap_or_default()
}

fn candidates() -> Vec<(PathBuf, &'static str)> {
    let home = dirs::home_dir().unwrap_or_default();
    let mut c: Vec<(PathBuf, &'static str)> = Vec::new();
    if let Ok(h) = std::env::var("JAVA_HOME") {
        c.push((h.into(), "JAVA_HOME"));
    }
    // SDKMAN and asdf work the same everywhere they run.
    for p in homes_in(&home.join(".sdkman/candidates/java"), "") {
        c.push((p, "SDKMAN"));
    }
    for p in homes_in(&home.join(".asdf/installs/java"), "") {
        c.push((p, "asdf"));
    }

    #[cfg(target_os = "macos")]
    {
        c.push(("/Applications/Android Studio.app/Contents/jbr/Contents/Home".into(), "Android Studio"));
        c.push((home.join("Applications/Android Studio.app/Contents/jbr/Contents/Home"), "Android Studio"));
        // Every JDK macOS knows about: `java_home -V` lists them on stderr.
        if let Ok(out) = Command::new("/usr/libexec/java_home").arg("-V").output() {
            for line in String::from_utf8_lossy(&out.stderr).lines() {
                if let Some(i) = line.find('/') {
                    c.push((line[i..].trim().into(), "macOS"));
                }
            }
        }
        for p in homes_in(Path::new("/Library/Java/JavaVirtualMachines"), "Contents/Home") {
            c.push((p, "System"));
        }
        for p in homes_in(&home.join("Library/Java/JavaVirtualMachines"), "Contents/Home") {
            c.push((p, "User"));
        }
        for brew in ["/opt/homebrew/opt", "/usr/local/opt"] {
            for p in homes_in(Path::new(brew), "libexec/openjdk.jdk/Contents/Home") {
                if p.to_string_lossy().contains("/openjdk") {
                    c.push((p, "Homebrew"));
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        for p in [home.join("android-studio/jbr"), "/opt/android-studio/jbr".into(), "/snap/android-studio/current/jbr".into()] {
            c.push((p, "Android Studio"));
        }
        for p in homes_in(Path::new("/usr/lib/jvm"), "") {
            c.push((p, "System"));
        }
        // What `java` on the PATH resolves to (update-alternatives).
        if let Ok(real) = std::fs::canonicalize("/usr/bin/java") {
            if let Some(h) = real.parent().and_then(|b| b.parent()) {
                c.push((h.to_path_buf(), "System"));
            }
        }
    }

    #[cfg(windows)]
    {
        for base in [std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into())] {
            let base = PathBuf::from(base);
            c.push((base.join(r"Android\Android Studio\jbr"), "Android Studio"));
            for vendor in ["Java", "Eclipse Adoptium", "Microsoft", "Zulu", "Amazon Corretto", "BellSoft"] {
                for p in homes_in(&base.join(vendor), "") {
                    c.push((p, "Installed"));
                }
            }
        }
        for p in homes_in(&home.join(r"scoop\apps"), "current") {
            if p.to_string_lossy().to_lowercase().contains("jdk") {
                c.push((p, "Scoop"));
            }
        }
        // JDKs registered by their installers.
        for key in [r"HKLM\SOFTWARE\JavaSoft\JDK", r"HKLM\SOFTWARE\Eclipse Adoptium\JDK"] {
            if let Ok(out) = Command::new("reg").args(["query", key, "/s", "/v", "JavaHome"]).output() {
                for line in String::from_utf8_lossy(&out.stdout).lines() {
                    if let Some((_, v)) = line.split_once("REG_SZ") {
                        c.push((v.trim().into(), "Registry"));
                    }
                }
            }
        }
    }
    c
}

/// Every distinct JDK on this machine. Starts a JVM per candidate, so the
/// result is cached; `refresh` re-scans.
static INSTALLED: RwLock<Option<Vec<Jdk>>> = RwLock::new(None);

pub(crate) fn installed() -> Vec<Jdk> {
    if let Some(list) = INSTALLED.read().ok().and_then(|c| c.clone()) {
        return list;
    }
    let mut found: Vec<Jdk> = Vec::new();
    for (path, source) in candidates() {
        if let Some(jdk) = probe(&path, source) {
            if !found.iter().any(|f| f.home == jdk.home) {
                found.push(jdk);
            }
        }
    }
    found.sort_by(|a, b| b.major.cmp(&a.major).then(a.source.cmp(b.source)));
    if let Ok(mut c) = INSTALLED.write() {
        *c = Some(found.clone());
    }
    found
}

/// Forget the cached scan (after installing or removing a JDK).
pub(crate) fn refresh() {
    if let Ok(mut c) = INSTALLED.write() {
        *c = None;
    }
}

// ---------------------------------------------------------------------------
// The JDK AppScreens uses
// ---------------------------------------------------------------------------

static PREFERRED: RwLock<Option<PathBuf>> = RwLock::new(None);

/// The team setting from the picker ("" = automatic). Set at startup and on
/// every change.
pub(crate) fn set_preferred(home: &str) {
    if let Ok(mut p) = PREFERRED.write() {
        *p = (!home.trim().is_empty()).then(|| PathBuf::from(home.trim()));
    }
}

/// The JDK to use: the one chosen in AppScreens if it still exists, else the
/// recommended one for the default Gradle, else any.
pub(crate) fn selected() -> Option<Jdk> {
    let jdks = installed();
    let chosen = PREFERRED.read().ok().and_then(|p| p.clone());
    if let Some(home) = chosen {
        if let Some(j) = jdks.iter().find(|j| j.home == home) {
            return Some(j.clone());
        }
        if let Some(j) = probe(&home, "Chosen") {
            return Some(j);
        }
    }
    recommend(&jdks, DEFAULT_GRADLE).or_else(|| jdks.first().cloned())
}

// ---------------------------------------------------------------------------
// Installing a JDK
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Runner {
    /// Runs inside AppScreens with a live log (no password needed)
    InApp { program: PathBuf, args: Vec<String> },
    /// Needs a password prompt, so it opens in a terminal window
    Terminal,
    /// An installer to download
    Browser(String),
}

#[derive(Clone, PartialEq, Debug)]
pub(crate) struct InstallOption {
    pub title: String,
    pub note: String,
    /// The command as a person would type it
    pub command: String,
    pub runner: Runner,
}

/// The Java release to install for a Gradle version: the newest LTS it runs on.
pub(crate) fn install_major(gradle: (u32, u32)) -> u32 {
    if max_java_for_gradle(gradle) >= 21 { 21 } else { 17 }
}

/// What this machine has to install Java with.
#[derive(Clone, Default, Debug)]
pub(crate) struct Installers {
    pub brew: Option<PathBuf>,
    /// "apt", "dnf", "pacman" or "zypper"
    pub linux_pm: Option<&'static str>,
    pub winget: bool,
    pub android_studio: bool,
}

pub(crate) fn detect_installers() -> Installers {
    let first = |paths: &[&str]| paths.iter().map(PathBuf::from).find(|p| p.is_file());
    Installers {
        // An app started from the Dock has no shell PATH: look where they live.
        brew: first(&["/opt/homebrew/bin/brew", "/usr/local/bin/brew", "/home/linuxbrew/.linuxbrew/bin/brew"]),
        linux_pm: [("apt", "/usr/bin/apt-get"), ("dnf", "/usr/bin/dnf"), ("pacman", "/usr/bin/pacman"), ("zypper", "/usr/bin/zypper")]
            .into_iter()
            .find(|(_, p)| Path::new(p).is_file())
            .map(|(n, _)| n),
        winget: cfg!(windows) && Command::new("winget").arg("--version").output().is_ok_and(|o| o.status.success()),
        android_studio: installed().iter().any(|j| j.source == "Android Studio"),
    }
}

/// Install choices for `os` ("macos" / "linux" / "windows"), best first.
pub(crate) fn install_options(os: &str, major: u32, found: &Installers) -> Vec<InstallOption> {
    let temurin = |os_name: &str| InstallOption {
        title: format!("Eclipse Temurin {major} installer"),
        note: "Download and run the installer from adoptium.net, then scan again.".into(),
        command: String::new(),
        runner: Runner::Browser(format!("https://adoptium.net/temurin/releases/?version={major}&os={os_name}")),
    };
    let mut opts = Vec::new();
    match os {
        "macos" => {
            if let Some(brew) = &found.brew {
                opts.push(InstallOption {
                    title: format!("Homebrew: openjdk@{major}"),
                    note: "Installs in a few minutes, no password needed.".into(),
                    command: format!("brew install openjdk@{major}"),
                    runner: Runner::InApp { program: brew.clone(), args: vec!["install".into(), format!("openjdk@{major}")] },
                });
            }
            opts.push(temurin("mac"));
        }
        "linux" => {
            let cmd = match found.linux_pm {
                Some("apt") => Some(format!("sudo apt-get install -y openjdk-{major}-jdk")),
                Some("dnf") => Some(format!("sudo dnf install -y java-{major}-openjdk-devel")),
                Some("pacman") => Some(format!("sudo pacman -S --noconfirm jdk{major}-openjdk")),
                Some("zypper") => Some(format!("sudo zypper install -y java-{major}-openjdk-devel")),
                _ => None,
            };
            if let Some(command) = cmd {
                opts.push(InstallOption {
                    title: format!("{}: OpenJDK {major}", found.linux_pm.unwrap_or_default()),
                    note: "Asks for your password, so it opens in a terminal.".into(),
                    command,
                    runner: Runner::Terminal,
                });
            }
            opts.push(temurin("linux"));
        }
        "windows" => {
            if found.winget {
                let id = format!("EclipseAdoptium.Temurin.{major}.JDK");
                opts.push(InstallOption {
                    title: format!("winget: Temurin {major}"),
                    note: "Windows may ask to allow the installer.".into(),
                    command: format!("winget install --id {id} -e"),
                    runner: Runner::InApp {
                        program: "winget".into(),
                        args: ["install", "--id", &id, "-e", "--accept-package-agreements", "--accept-source-agreements"]
                            .iter()
                            .map(|s| s.to_string())
                            .collect(),
                    },
                });
            }
            opts.push(temurin("windows"));
        }
        _ => {}
    }
    if !found.android_studio {
        opts.push(InstallOption {
            title: "Android Studio".into(),
            note: "Comes with its own Java — and the SDK and NDK Android builds need anyway.".into(),
            command: String::new(),
            runner: Runner::Browser("https://developer.android.com/studio".into()),
        });
    }
    opts
}

/// Open `command` in a terminal window where the user can type a password.
pub(crate) fn open_in_terminal(command: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let script = command.replace('\\', "\\\\").replace('"', "\\\"");
        let status = Command::new("osascript")
            .args(["-e", "tell application \"Terminal\" to activate"])
            .args(["-e", &format!("tell application \"Terminal\" to do script \"{script}\"")])
            .status()
            .map_err(|e| e.to_string())?;
        return if status.success() { Ok(()) } else { Err("Terminal didn't open".into()) };
    }
    #[cfg(target_os = "linux")]
    {
        let inner = format!("{command}; echo; read -p 'Done — press Enter to close'");
        for term in ["x-terminal-emulator", "gnome-terminal", "konsole", "xfce4-terminal", "xterm"] {
            let args: Vec<&str> = if term == "gnome-terminal" { vec!["--", "bash", "-c", &inner] } else { vec!["-e", "bash", "-c", &inner] };
            if Command::new(term).args(&args).spawn().is_ok() {
                return Ok(());
            }
        }
        return Err("No terminal found — copy the command instead".into());
    }
    #[cfg(windows)]
    {
        Command::new("cmd").args(["/c", "start", "cmd", "/k", command]).spawn().map_err(|e| e.to_string())?;
        return Ok(());
    }
    #[allow(unreachable_code)]
    Err("Not supported on this system".into())
}

/// The OS name `install_options` expects.
pub(crate) fn current_os() -> &'static str {
    if cfg!(target_os = "macos") { "macos" } else if cfg!(windows) { "windows" } else { "linux" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jdk(major: u32, source: &'static str) -> Jdk {
        Jdk { home: format!("/jdk/{source}/{major}").into(), major, version: format!("{major}.0.1"), source }
    }

    #[test]
    fn install_choices_fit_the_machine() {
        assert_eq!(install_major((9, 1)), 21);
        assert_eq!(install_major((8, 3)), 17);

        let brew = Installers { brew: Some("/opt/homebrew/bin/brew".into()), android_studio: true, ..Default::default() };
        let mac = install_options("macos", 21, &brew);
        assert!(matches!(&mac[0].runner, Runner::InApp { args, .. } if args == &["install", "openjdk@21"]));
        assert!(matches!(&mac[1].runner, Runner::Browser(u) if u.contains("version=21&os=mac")));
        assert_eq!(mac.len(), 2, "no Android Studio suggestion when it's installed");

        let bare_mac = install_options("macos", 21, &Installers::default());
        assert!(matches!(bare_mac[0].runner, Runner::Browser(_)));
        assert!(bare_mac.iter().any(|o| o.title == "Android Studio"));

        let apt = Installers { linux_pm: Some("apt"), android_studio: true, ..Default::default() };
        let linux = install_options("linux", 17, &apt);
        assert_eq!(linux[0].command, "sudo apt-get install -y openjdk-17-jdk");
        assert_eq!(linux[0].runner, Runner::Terminal);

        let win = install_options("windows", 21, &Installers { winget: true, android_studio: true, ..Default::default() });
        assert!(win[0].command.contains("EclipseAdoptium.Temurin.21.JDK"));
    }

    #[test]
    fn parses_java_versions() {
        assert_eq!(parse_java_version("openjdk version \"21.0.12.1\" 2026-08-18\nOpenJDK Runtime"), Some(("21.0.12.1".into(), 21)));
        assert_eq!(parse_java_version("java version \"1.8.0_392\""), Some(("1.8.0_392".into(), 8)));
        assert_eq!(parse_java_version("openjdk version \"26\" 2026-03-17"), Some(("26".into(), 26)));
        assert_eq!(parse_java_version("nothing here"), None);
    }

    #[test]
    fn reads_the_gradle_wrapper() {
        let props = "distributionBase=GRADLE_USER_HOME\ndistributionUrl=https\\://services.gradle.org/distributions/gradle-8.10.2-bin.zip\n";
        assert_eq!(parse_gradle_wrapper(props), Some((8, 10)));
        assert_eq!(parse_gradle_wrapper("distributionUrl=https://x/gradle-9.0-all.zip"), Some((9, 0)));
    }

    #[test]
    fn gradle_limits_java() {
        assert_eq!(max_java_for_gradle((8, 10)), 23);
        assert_eq!(max_java_for_gradle((8, 5)), 21);
        assert_eq!(max_java_for_gradle((9, 9)), 26);
        let g = (8, 10);
        assert!(incompatibility(&jdk(11, "x"), g).unwrap().contains("too old"));
        assert!(incompatibility(&jdk(26, "x"), g).unwrap().contains("too new"));
        assert!(incompatibility(&jdk(21, "x"), g).is_none());
    }

    #[test]
    fn recommends_an_lts_preferring_android_studio() {
        let g = (8, 10);
        let list = vec![jdk(26, "Homebrew"), jdk(21, "Homebrew"), jdk(21, "Android Studio"), jdk(17, "System"), jdk(11, "System")];
        assert_eq!(recommend(&list, g).unwrap().source, "Android Studio");
        assert_eq!(recommend(&[jdk(26, "Homebrew"), jdk(17, "System")], g).unwrap().major, 17);
        assert!(recommend(&[jdk(26, "Homebrew"), jdk(11, "System")], g).is_none());
    }
}


