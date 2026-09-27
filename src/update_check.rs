//! Lightweight update check.
//!
//! Fetches the `latest.json` published with each GitHub release and compares
//! the version field to this build's `CARGO_PKG_VERSION`. Designed to be
//! cheap and side-effect-free so it can run in the background at startup.
//!
//! The full auto-replace flow (download .dmg, swap binary, relaunch) is
//! platform-specific and noticeably riskier than just notifying the user —
//! we keep it simple: tell the user a newer version exists and link them
//! straight to the build for their OS, which their browser downloads.

use serde::Deserialize;
use std::collections::BTreeMap;

/// Stable URL — points at whatever the latest GitHub release is, regardless
/// of version number. Updated automatically every release.
/// Served from mayorana.ch alongside the builds it describes, so update
/// checks do not depend on the source repository staying publicly readable.
const LATEST_URL: &str = "https://mayorana.ch/downloads/appscreens/latest/latest.json";

/// Fallback when `latest.json` has no build for this OS (e.g. an Intel Mac —
/// only Apple Silicon is built): the page listing every build, instead of a
/// link that would 404. The binaries are distributed from mayorana.ch, not
/// from GitHub.
const RELEASES_URL: &str = "https://mayorana.ch/en/apps";

/// Sent on the update check so the download logs can tell an existing user
/// updating from a new install, and which versions are still in use. Read by
/// scripts/downloads-stats.py on mayorana.ch.
const USER_AGENT: &str = concat!("appscreens/", env!("CARGO_PKG_VERSION"), " (updater)");

#[derive(Debug, Deserialize)]
struct LatestJson {
    version: String,
    tag: String,
    #[serde(default)]
    platforms: Platforms,
}

/// Builds per OS, keyed by package format (`dmg`, `msi`, `deb`,
/// `appimage`…) — not by CPU architecture. A `BTreeMap` so the fallback pick
/// in `platform_url` is the same on every launch.
#[derive(Debug, Default, Deserialize)]
struct Platforms {
    #[serde(default)]
    macos: BTreeMap<String, Artifact>,
    #[serde(default)]
    windows: BTreeMap<String, Artifact>,
    #[serde(default)]
    linux: BTreeMap<String, Artifact>,
}

#[derive(Debug, Deserialize)]
struct Artifact {
    url: String,
}

/// Result of a successful check.
#[derive(Debug, Clone)]
pub struct UpdateInfo {
    /// The newest version published.
    pub latest_version: String,
    /// Tag of the latest release (e.g. "v0.1.4"). Reserved for direct deep-
    /// links to a specific tag's downloads later.
    #[allow(dead_code)]
    pub latest_tag: String,
    /// Direct link to this OS's build, so the banner's "Download" gets the
    /// file itself rather than a page to pick one from. Falls back to that
    /// page when there is no build for this OS.
    ///
    /// The download happens in the user's browser, so there is nothing here
    /// to verify `latest.json`'s `sha256` against; it is deliberately not
    /// deserialised.
    pub release_url: String,
}

/// Returns `Some(UpdateInfo)` if a newer version than the running binary is
/// available, `None` otherwise. Network or parse errors are swallowed
/// silently — an update check should never disrupt the app.
pub async fn check() -> Option<UpdateInfo> {
    let current = env!("CARGO_PKG_VERSION");
    let body = reqwest::Client::new()
        .get(LATEST_URL)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;

    let latest: LatestJson = serde_json::from_str(&body).ok()?;
    if is_newer(&latest.version, current) {
        Some(UpdateInfo {
            latest_version: latest.version,
            latest_tag: latest.tag,
            release_url: platform_url(std::env::consts::OS, &latest.platforms),
        })
    } else {
        None
    }
}

/// Formats to offer, best first, per OS. AppImage before deb on Linux: it
/// runs on any distribution without installing anything as root.
fn preferred_formats(os: &str) -> &'static [&'static str] {
    match os {
        "macos" => &["dmg"],
        "windows" => &["msi", "exe", "exe_or_msi"],
        "linux" => &["appimage", "deb"],
        _ => &[],
    }
}

/// The download link for `os`: the preferred format that is published, else
/// any build for that OS, else the page listing every build.
fn platform_url(os: &str, platforms: &Platforms) -> String {
    let by_format = match os {
        "macos" => &platforms.macos,
        "windows" => &platforms.windows,
        "linux" => &platforms.linux,
        _ => return RELEASES_URL.to_string(),
    };
    preferred_formats(os)
        .iter()
        .find_map(|format| by_format.get(*format))
        .or_else(|| by_format.values().next())
        .map(|artifact| artifact.url.as_str())
        .filter(|url| !url.is_empty())
        // Marks the hit as coming from an existing install. The browser, not
        // this app, fetches the file, so without the marker it looks like a
        // first-time download off the website. nginx ignores the query.
        .map(|url| format!("{url}?src=updater"))
        .unwrap_or_else(|| RELEASES_URL.to_string())
}

/// Compares two semver-like strings (`MAJOR.MINOR.PATCH`). Returns true if
/// `a` is strictly newer than `b`. Treats anything that fails to parse as
/// equal — so unexpected input never falsely advertises an update.
fn is_newer(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Option<(u32, u32, u32)> {
        let mut parts = s.trim_start_matches('v').split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts
            .next()?
            // Strip pre-release tag (`-beta.1`) and build metadata (`+build`).
            .split(|c: char| c == '-' || c == '+')
            .next()?
            .parse()
            .ok()?;
        Some((major, minor, patch))
    };
    match (parse(a), parse(b)) {
        (Some(av), Some(bv)) => av > bv,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_comparison() {
        assert!(is_newer("0.1.3", "0.1.2"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(!is_newer("0.1.2", "0.1.2"));
        assert!(!is_newer("0.1.1", "0.1.2"));
        // Pre-release suffix is stripped → equal.
        assert!(!is_newer("0.1.2-beta.1", "0.1.2"));
        // Leading "v" tolerated.
        assert!(is_newer("v0.2.0", "0.1.0"));
        // Malformed → no false positive.
        assert!(!is_newer("garbage", "0.1.2"));
    }

    fn platforms(json: &str) -> Platforms {
        serde_json::from_str::<LatestJson>(json).unwrap().platforms
    }

    const FEED: &str = r#"{
        "version": "0.1.14", "tag": "v0.1.14",
        "platforms": {
            "macos": { "dmg": { "url": "https://x/appscreens-macos-arm64.dmg", "sha256": "a" } },
            "linux": {
                "deb": { "url": "https://x/appscreens-linux-x86_64.deb", "sha256": "b" },
                "appimage": { "url": "https://x/appscreens-linux-x86_64.AppImage", "sha256": "c" }
            },
            "windows": { "msi": { "url": "https://x/appscreens-windows-setup.msi", "sha256": "d" } }
        }
    }"#;

    #[test]
    fn links_straight_to_the_build_for_each_os() {
        let p = platforms(FEED);
        assert_eq!(platform_url("macos", &p), "https://x/appscreens-macos-arm64.dmg?src=updater");
        assert_eq!(platform_url("windows", &p), "https://x/appscreens-windows-setup.msi?src=updater");
        // AppImage wins over deb, every time.
        assert_eq!(platform_url("linux", &p), "https://x/appscreens-linux-x86_64.AppImage?src=updater");
    }

    #[test]
    fn falls_back_to_the_apps_page() {
        let p = platforms(FEED);
        assert_eq!(platform_url("freebsd", &p), RELEASES_URL);
        // A feed without platforms (older releases) still parses.
        let bare = platforms(r#"{ "version": "0.1.14", "tag": "v0.1.14" }"#);
        assert_eq!(platform_url("macos", &bare), RELEASES_URL);
    }

    #[test]
    fn unknown_format_still_downloads() {
        let p = platforms(r#"{ "version": "1.0.0", "tag": "v1.0.0",
            "platforms": { "windows": { "zip": { "url": "https://x/a.zip" } } } }"#);
        assert_eq!(platform_url("windows", &p), "https://x/a.zip?src=updater");
    }
}
