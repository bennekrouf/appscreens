//! Lightweight update check.
//!
//! Fetches the `latest.json` published with each GitHub release and compares
//! the version field to this build's `CARGO_PKG_VERSION`. Designed to be
//! cheap and side-effect-free so it can run in the background at startup.
//!
//! The full auto-replace flow (download .dmg, swap binary, relaunch) is
//! platform-specific and noticeably riskier than just notifying the user —
//! we keep it simple: tell the user a newer version exists and link them to
//! the GitHub release page.

use serde::Deserialize;

/// Stable URL — points at whatever the latest GitHub release is, regardless
/// of version number. Updated automatically every release.
/// Served from mayorana.ch alongside the builds it describes, so update
/// checks do not depend on the source repository staying publicly readable.
const LATEST_URL: &str = "https://mayorana.ch/downloads/appscreens/latest/latest.json";

/// Where the user is sent to get the new version. The binaries are
/// distributed from mayorana.ch, not from GitHub.
const RELEASES_URL: &str = "https://mayorana.ch/en/apps";

#[derive(Debug, Deserialize)]
struct LatestJson {
    version: String,
    tag: String,
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
    /// URL to the GitHub release page.
    pub release_url: String,
}

/// Returns `Some(UpdateInfo)` if a newer version than the running binary is
/// available, `None` otherwise. Network or parse errors are swallowed
/// silently — an update check should never disrupt the app.
pub async fn check() -> Option<UpdateInfo> {
    let current = env!("CARGO_PKG_VERSION");
    let body = reqwest::Client::new()
        .get(LATEST_URL)
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
            release_url: RELEASES_URL.to_string(),
        })
    } else {
        None
    }
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
}
