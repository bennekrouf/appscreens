//! Store API calls for the release half of the Submit step: store text,
//! binary uploads, App Review and Play tracks. The screenshot-era helpers
//! (auth, edits, screenshot sets) live in main.rs; these follow their style.

use super::*;

const ASC: &str = "https://api.appstoreconnect.apple.com/v1";
const PLAY: &str = "https://androidpublisher.googleapis.com/androidpublisher/v3/applications";
const PLAY_UPLOAD: &str = "https://androidpublisher.googleapis.com/upload/androidpublisher/v3/applications";

/// Readable error from an App Store Connect or Google API error body.
fn api_error(status: reqwest::StatusCode, body: &serde_json::Value) -> String {
    if let Some(err) = body["errors"].as_array().and_then(|e| e.first()) {
        let title = err["title"].as_str().unwrap_or("Error");
        let detail = err["detail"].as_str().unwrap_or("");
        return format!("HTTP {status} · {title}: {detail}");
    }
    if let Some(msg) = body["error"]["message"].as_str() {
        return format!("HTTP {status} · {msg}");
    }
    format!("HTTP {status} · {body}")
}

/// Send a request and return its JSON body (Null for an empty 2xx body).
async fn send_json(req: reqwest::RequestBuilder) -> Result<serde_json::Value, String> {
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    let body: serde_json::Value = if text.trim().is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text))
    };
    if status.is_success() {
        Ok(body)
    } else {
        Err(api_error(status, &body))
    }
}

// ---------------------------------------------------------------------------
// App Store Connect
// ---------------------------------------------------------------------------

/// Store text for one App Store version localization. Empty fields are left
/// out rather than clearing what is already in App Store Connect.
pub(super) fn asc_localization_attributes(text: &StoreText, support_url: &str, with_whats_new: bool) -> serde_json::Value {
    let mut attrs = serde_json::Map::new();
    let mut put = |key: &str, value: &str| {
        if !value.trim().is_empty() {
            attrs.insert(key.to_string(), serde_json::Value::String(value.trim().to_string()));
        }
    };
    put("description", &text.description);
    put("keywords", &text.keywords);
    put("promotionalText", &text.promo_text);
    put("supportUrl", support_url);
    if with_whats_new {
        put("whatsNew", &text.whats_new);
    }
    serde_json::Value::Object(attrs)
}

pub(super) async fn asc_update_localization(jwt: &str, localization_id: &str, attributes: serde_json::Value) -> Result<(), String> {
    let body = serde_json::json!({
        "data": { "type": "appStoreVersionLocalizations", "id": localization_id, "attributes": attributes }
    });
    send_json(reqwest::Client::new()
        .patch(format!("{ASC}/appStoreVersionLocalizations/{localization_id}"))
        .bearer_auth(jwt)
        .json(&body))
    .await
    .map(|_| ())
}

/// Add a language to the version. Returns the new localization id.
pub(super) async fn asc_create_localization(
    jwt: &str,
    version_id: &str,
    locale: &str,
    mut attributes: serde_json::Value,
) -> Result<String, String> {
    attributes["locale"] = serde_json::Value::String(locale.to_string());
    let body = serde_json::json!({
        "data": {
            "type": "appStoreVersionLocalizations",
            "attributes": attributes,
            "relationships": {
                "appStoreVersion": { "data": { "type": "appStoreVersions", "id": version_id } }
            }
        }
    });
    let resp = send_json(reqwest::Client::new()
        .post(format!("{ASC}/appStoreVersionLocalizations"))
        .bearer_auth(jwt)
        .json(&body))
    .await?;
    resp["data"]["id"].as_str().map(str::to_string).ok_or_else(|| format!("No localization id in {resp}"))
}

#[derive(Clone, Debug)]
pub(super) struct AscBuild {
    pub id: String,
    /// PROCESSING / FAILED / INVALID / VALID
    pub state: String,
    /// None until export compliance has been answered
    pub uses_non_exempt_encryption: Option<bool>,
}

/// The build with this version and build number, if App Store Connect has
/// received it yet.
pub(super) async fn asc_find_build(jwt: &str, app_id: &str, version: &str, build: u32) -> Result<Option<AscBuild>, String> {
    let resp = send_json(reqwest::Client::new()
        .get(format!("{ASC}/builds"))
        .query(&[
            ("filter[app]", app_id),
            ("filter[version]", &build.to_string()),
            ("filter[preReleaseVersion.version]", version),
            ("fields[builds]", "version,processingState,usesNonExemptEncryption"),
            ("limit", "1"),
        ])
        .bearer_auth(jwt))
    .await?;
    Ok(resp["data"].as_array().and_then(|a| a.first()).map(|b| AscBuild {
        id: b["id"].as_str().unwrap_or_default().to_string(),
        state: b["attributes"]["processingState"].as_str().unwrap_or("PROCESSING").to_string(),
        uses_non_exempt_encryption: b["attributes"]["usesNonExemptEncryption"].as_bool(),
    }))
}

/// Answer export compliance for a build: no non-exempt encryption.
pub(super) async fn asc_set_exempt_encryption(jwt: &str, build_id: &str) -> Result<(), String> {
    let body = serde_json::json!({
        "data": { "type": "builds", "id": build_id, "attributes": { "usesNonExemptEncryption": false } }
    });
    send_json(reqwest::Client::new().patch(format!("{ASC}/builds/{build_id}")).bearer_auth(jwt).json(&body))
        .await
        .map(|_| ())
}

pub(super) async fn asc_attach_build(jwt: &str, version_id: &str, build_id: &str) -> Result<(), String> {
    let body = serde_json::json!({ "data": { "type": "builds", "id": build_id } });
    send_json(reqwest::Client::new()
        .patch(format!("{ASC}/appStoreVersions/{version_id}/relationships/build"))
        .bearer_auth(jwt)
        .json(&body))
    .await
    .map(|_| ())
}

/// Submit the version for App Review through a review submission, reusing an
/// open (not yet submitted) one if the app already has it. Returns its id.
pub(super) async fn asc_submit_for_review(jwt: &str, app_id: &str, version_id: &str) -> Result<String, String> {
    let client = reqwest::Client::new();
    let open = send_json(client
        .get(format!("{ASC}/reviewSubmissions"))
        .query(&[("filter[app]", app_id), ("filter[platform]", "IOS"), ("filter[state]", "READY_FOR_REVIEW")])
        .bearer_auth(jwt))
    .await?;
    let submission_id = match open["data"].as_array().and_then(|a| a.first()).and_then(|s| s["id"].as_str()) {
        Some(id) => id.to_string(),
        None => {
            let body = serde_json::json!({
                "data": {
                    "type": "reviewSubmissions",
                    "attributes": { "platform": "IOS" },
                    "relationships": { "app": { "data": { "type": "apps", "id": app_id } } }
                }
            });
            let created = send_json(client.post(format!("{ASC}/reviewSubmissions")).bearer_auth(jwt).json(&body)).await?;
            created["data"]["id"].as_str().map(str::to_string).ok_or_else(|| format!("No submission id in {created}"))?
        }
    };

    let item = serde_json::json!({
        "data": {
            "type": "reviewSubmissionItems",
            "relationships": {
                "reviewSubmission": { "data": { "type": "reviewSubmissions", "id": submission_id } },
                "appStoreVersion": { "data": { "type": "appStoreVersions", "id": version_id } }
            }
        }
    });
    if let Err(e) = send_json(client.post(format!("{ASC}/reviewSubmissionItems")).bearer_auth(jwt).json(&item)).await {
        // Re-running after a partial failure: the version is already an item.
        if !e.contains("409") {
            return Err(e);
        }
    }

    let submit = serde_json::json!({
        "data": { "type": "reviewSubmissions", "id": submission_id, "attributes": { "submitted": true } }
    });
    send_json(client.patch(format!("{ASC}/reviewSubmissions/{submission_id}")).bearer_auth(jwt).json(&submit)).await?;
    Ok(submission_id)
}

/// (CFBundleShortVersionString, CFBundleVersion) read from an IPA, via the
/// system `unzip` and `plutil` (macOS — the only place IPAs are uploaded from).
pub(super) fn ipa_version(ipa: &std::path::Path) -> Option<(String, u32)> {
    let listing = std::process::Command::new("unzip").arg("-Z1").arg(ipa).output().ok()?;
    let plist_entry = String::from_utf8_lossy(&listing.stdout)
        .lines()
        .find(|l| l.starts_with("Payload/") && l.ends_with(".app/Info.plist") && l.matches('/').count() == 2)?
        .to_string();
    let plist = std::process::Command::new("unzip").arg("-p").arg(ipa).arg(&plist_entry).output().ok()?;
    let mut plutil = std::process::Command::new("plutil")
        .args(["-convert", "json", "-o", "-", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    {
        use std::io::Write;
        plutil.stdin.take()?.write_all(&plist.stdout).ok()?;
    }
    let out = plutil.wait_with_output().ok()?;
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    Some((
        json["CFBundleShortVersionString"].as_str()?.to_string(),
        json["CFBundleVersion"].as_str()?.parse().ok()?,
    ))
}

// ---------------------------------------------------------------------------
// Google Play
// ---------------------------------------------------------------------------

/// Play's language codes differ from App Store / fastlane ones for some
/// locales (it has no "zh-Hans", "ar-SA" or bare "hi").
pub(crate) fn play_language(locale: &str) -> &str {
    match locale {
        "ar-SA" => "ar",
        "hi" => "hi-IN",
        "tr" => "tr-TR",
        "zh-Hans" => "zh-CN",
        other => other,
    }
}

pub(super) async fn play_update_listing(
    token: &str,
    package: &str,
    edit_id: &str,
    language: &str,
    title: &str,
    text: &StoreText,
) -> Result<(), String> {
    let body = serde_json::json!({
        "language": language,
        "title": title,
        "fullDescription": text.description.trim(),
        "shortDescription": text.short_description.trim(),
    });
    send_json(reqwest::Client::new()
        .put(format!("{PLAY}/{package}/edits/{edit_id}/listings/{language}"))
        .bearer_auth(token)
        .json(&body))
    .await
    .map(|_| ())
}

/// Upload an AAB into the edit. Returns its versionCode.
pub(super) async fn play_upload_bundle(token: &str, package: &str, edit_id: &str, aab: &std::path::Path) -> Result<u32, String> {
    let bytes = tokio::fs::read(aab).await.map_err(|e| format!("Cannot read {}: {e}", aab.display()))?;
    let resp = send_json(reqwest::Client::new()
        .post(format!("{PLAY_UPLOAD}/{package}/edits/{edit_id}/bundles"))
        .query(&[("uploadType", "media")])
        .bearer_auth(token)
        .header("Content-Type", "application/octet-stream")
        .body(bytes))
    .await?;
    resp["versionCode"]
        .as_u64()
        .or_else(|| resp["versionCode"].as_str().and_then(|s| s.parse().ok()))
        .map(|v| v as u32)
        .ok_or_else(|| format!("No versionCode in {resp}"))
}

/// One versionCode, released on one track.
pub(super) struct TrackRelease<'a> {
    pub track: &'a str,
    pub name: String,
    pub version_code: u32,
    /// "draft" or "completed"
    pub status: &'a str,
    /// (Play language, release notes)
    pub notes: &'a [(String, String)],
}

/// Replace a track's releases with this one.
pub(super) async fn play_update_track(token: &str, package: &str, edit_id: &str, release: TrackRelease<'_>) -> Result<(), String> {
    let release_notes: Vec<serde_json::Value> = release
        .notes
        .iter()
        .map(|(lang, text)| serde_json::json!({ "language": lang, "text": text }))
        .collect();
    let track = release.track;
    let body = serde_json::json!({
        "track": track,
        "releases": [{
            "name": release.name,
            "versionCodes": [release.version_code.to_string()],
            "status": release.status,
            "releaseNotes": release_notes,
        }]
    });
    send_json(reqwest::Client::new()
        .put(format!("{PLAY}/{package}/edits/{edit_id}/tracks/{track}"))
        .bearer_auth(token)
        .json(&body))
    .await
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_codes_for_store_locales() {
        assert_eq!(play_language("zh-Hans"), "zh-CN");
        assert_eq!(play_language("ar-SA"), "ar");
        assert_eq!(play_language("hi"), "hi-IN");
        assert_eq!(play_language("tr"), "tr-TR");
        assert_eq!(play_language("en-US"), "en-US");
        assert_eq!(play_language("fr-FR"), "fr-FR");
    }

    #[test]
    fn empty_store_text_is_left_out() {
        let text = StoreText { description: "An app".into(), whats_new: "Fixes".into(), ..Default::default() };
        let attrs = asc_localization_attributes(&text, "", true);
        assert_eq!(attrs["description"], "An app");
        assert_eq!(attrs["whatsNew"], "Fixes");
        assert!(attrs.get("keywords").is_none());
        assert!(attrs.get("supportUrl").is_none());
        let first_version = asc_localization_attributes(&text, "https://x.y", false);
        assert!(first_version.get("whatsNew").is_none());
        assert_eq!(first_version["supportUrl"], "https://x.y");
    }
}
