//! Long-running work started from the workspace steps: screenshot generation,
//! build scripts and store uploads.
//!
//! Every task is spawned on the workspace scope (see [`Ws::spawn`]) rather than
//! on the step that started it, so a build keeps running when the user moves on
//! to another step. Progress goes to the job's log signal, which the job drawer
//! renders.

use super::*;

// ---- Logo picker ----
#[cfg(not(target_os = "android"))]
pub(super) fn pick_logo(ws: Ws) {
    let mut proj = ws.proj;
    let proj_dir = ws.dir();
    ws.spawn(async move {
        let file = rfd::AsyncFileDialog::new()
            .set_title("Choose App Logo")
            .add_filter("Images", &["png", "jpg", "jpeg"])
            .pick_file()
            .await;
        if let Some(selected) = file {
            let src = selected.path().to_path_buf();
            // Copy the logo into assets/ inside the project so it survives
            // the original file being moved or deleted later.
            let assets_dir = proj_dir.join("assets");
            let logo_path = if let Some(fname) = src.file_name() {
                let dest = assets_dir.join(fname);
                let _ = tokio::fs::create_dir_all(&assets_dir).await;
                match tokio::fs::copy(&src, &dest).await {
                    Ok(_) => dest,         // use the stable local copy
                    Err(_) => src.clone(), // fallback: use original path
                }
            } else {
                src.clone()
            };
            let mut p = proj.write();
            p.logo_path = Some(logo_path);
            save_project_state(&proj_dir, &p);
        }
    });
}

// rfd has no Android backend (see src/android_saf.rs) — the picker there
// hands back bytes directly rather than a path, so this copies from bytes
// instead of from a source file, but lands in the same place.
#[cfg(target_os = "android")]
pub(super) fn pick_logo(ws: Ws) {
    let mut proj = ws.proj;
    let proj_dir = ws.dir();
    ws.spawn(async move {
        if let Some((fname, bytes)) = android_saf::pick_images(false).await.into_iter().next() {
            let assets_dir = proj_dir.join("assets");
            let _ = tokio::fs::create_dir_all(&assets_dir).await;
            let dest = assets_dir.join(&fname);
            if tokio::fs::write(&dest, &bytes).await.is_ok() {
                let mut p = proj.write();
                p.logo_path = Some(dest);
                save_project_state(&proj_dir, &p);
            }
        }
    });
}

// ---- Source image picker — adds images to the active language ----
#[cfg(not(target_os = "android"))]
pub(super) fn pick_sources(ws: Ws) {
    let mut proj = ws.proj;
    let proj_dir = ws.dir();
    let locale_for_pick = ws.active_locale.read().clone();
    ws.spawn(async move {
        let files = rfd::AsyncFileDialog::new()
            .add_filter("Images", &["png", "jpg", "jpeg"])
            .pick_files()
            .await;
        if let Some(selected) = files {
            let mut p = proj.write();
            let srcs = p
                .locale_sources
                .entry(locale_for_pick.clone())
                .or_insert_with(Vec::new);
            for f in selected {
                let path = f.path().to_path_buf();
                if !srcs.contains(&path) {
                    srcs.push(path);
                }
            }
            let n = p.locale_sources[&locale_for_pick].len();
            p.ensure_texts_len(&locale_for_pick, n);
            save_project_state(&proj_dir, &p);
        }
    });
}

// Android has no stable path to reference back to (a content:// URI isn't
// guaranteed to survive past this session), so each picked image is
// copied into the project instead of referenced in place — unlike
// desktop's pick_sources above, which keeps pointing at the original file.
#[cfg(target_os = "android")]
pub(super) fn pick_sources(ws: Ws) {
    let mut proj = ws.proj;
    let proj_dir = ws.dir();
    let locale_for_pick = ws.active_locale.read().clone();
    ws.spawn(async move {
        let picked = android_saf::pick_images(true).await;
        if picked.is_empty() {
            return;
        }
        let sources_dir = proj_dir.join("sources").join(&locale_for_pick);
        let _ = tokio::fs::create_dir_all(&sources_dir).await;

        let mut written = Vec::new();
        for (fname, bytes) in picked {
            let dest = sources_dir.join(&fname);
            if tokio::fs::write(&dest, &bytes).await.is_ok() {
                written.push(dest);
            }
        }

        let mut p = proj.write();
        let srcs = p
            .locale_sources
            .entry(locale_for_pick.clone())
            .or_insert_with(Vec::new);
        for dest in written {
            if !srcs.contains(&dest) {
                srcs.push(dest);
            }
        }
        let n = p.locale_sources[&locale_for_pick].len();
        p.ensure_texts_len(&locale_for_pick, n);
        save_project_state(&proj_dir, &p);
    });
}

// ---- Publish iOS screenshots to App Store Connect ----
pub(super) fn publish_app_store(ws: Ws) {
    let proj = ws.proj;
    let mut publish_phase = ws.ios_pub;
    let mut publish_log = ws.ios_pub_log;
    let proj_dir = ws.dir();

    publish_phase.set(PublishPhase::Running);
    publish_log.set(Vec::new());
    ws.show_job(JobKind::AppStore);

    // Snapshot project state needed inside the async block.
    let bundle_id = proj.read().ios_bundle_id.clone(); // ASC lookup uses iOS bundle ID
    let locales = proj.read().locales.clone();
    let output_paths = proj.read().output_paths.clone();
    let app_name = proj.read().app_name.clone();
    let store_texts = proj.read().store_texts.clone();
    let support_url = proj.read().support_url.clone();
    let ios_version = proj.read().version.trim().to_string();
    if !valid_version(&ios_version) {
        publish_phase.set(PublishPhase::Error(
            "Set a valid version (e.g. 1.2.0) in the Version step.".into(),
        ));
        ws.show_job(JobKind::AppStore);
        return;
    }
    let proj_dir = proj_dir.clone();
    let fastlane_ios = proj_dir.join("fastlane").join("screenshots").join("ios");

    ws.spawn(async move {
            let mut log = publish_log.clone();
            let mut push = |msg: &str| log.write().push(msg.to_string());

            // ── 1. Credentials → App Store Connect JWT ────────────────────
            push("🔑 Authenticating with App Store Connect…");
            let jwt = match asc_auth(&proj_dir).await {
                Ok(t) => t,
                Err(e) => { publish_phase.set(PublishPhase::Error(e)); return; }
            };
            push("✅ Authenticated");

            // ── 3. Find the app by bundle ID ──────────────────────────────
            push(&format!("🔍 Looking up app: {bundle_id}"));
            let app_id = match asc_find_app(&jwt, &bundle_id).await {
                Ok(id) => id,
                Err(e) => { publish_phase.set(PublishPhase::Error(format!("App lookup failed: {e}"))); return; }
            };
            push(&format!("   App ID: {app_id}"));

            // ── 4. Find or create the latest editable version ─────────────
            push("🔍 Finding (or creating) app version…");
            let version_id = match asc_find_or_create_version(&jwt, &app_id, &ios_version).await {
                Ok(id) => id,
                Err(e) => { publish_phase.set(PublishPhase::Error(format!("Version lookup failed: {e}"))); return; }
            };
            push(&format!("   Version ID: {version_id}"));

            // ── 5. Get localizations for this version ─────────────────────
            push("🌐 Fetching localizations…");
            let mut loc_map = match asc_get_localizations(&jwt, &version_id).await {
                Ok(m) => m,
                Err(e) => { publish_phase.set(PublishPhase::Error(format!("Localization fetch failed: {e}"))); return; }
            };
            push(&format!("   Found localizations: {}", loc_map.keys().cloned().collect::<Vec<_>>().join(", ")));

            // ── 5b. Ensure every localization has the correct app name ─────
            if !app_name.is_empty() {
                push(&format!("✏️  Setting app name to \"{app_name}\" on all localizations…"));
                for (locale, loc_id) in &loc_map {
                    if let Err(e) = asc_set_localization_name(&jwt, loc_id, &app_name).await {
                        push(&format!("   ⚠️  Could not set name for {locale}: {e}"));
                    }
                }
            }

            // ── 5c. Store text; add languages App Store Connect lacks ─────
            for locale in &locales {
                let text = store_texts.get(locale).cloned().unwrap_or_default();
                match loc_map.get(locale.as_str()).cloned() {
                    Some(loc_id) => {
                        let attrs = stores::asc_localization_attributes(&text, &support_url, true);
                        if attrs.as_object().is_some_and(|o| o.is_empty()) {
                            continue;
                        }
                        push(&format!("📝 [{locale}] Updating store text…"));
                        if let Err(e) = stores::asc_update_localization(&jwt, &loc_id, attrs).await {
                            // "What's New" is refused on an app's first version;
                            // save everything else rather than failing the lot.
                            let without = stores::asc_localization_attributes(&text, &support_url, false);
                            if !text.whats_new.trim().is_empty()
                                && stores::asc_update_localization(&jwt, &loc_id, without).await.is_ok()
                            {
                                push(&format!("   ⚠️  [{locale}] What's New not accepted (first version?) — the rest was saved"));
                            } else {
                                publish_phase.set(PublishPhase::Error(format!("[{locale}] Store text: {e}")));
                                return;
                            }
                        }
                    }
                    None => {
                        push(&format!("🌐 [{locale}] Adding language to App Store Connect…"));
                        let attrs = stores::asc_localization_attributes(&text, &support_url, false);
                        match stores::asc_create_localization(&jwt, &version_id, locale, attrs).await {
                            Ok(id) => {
                                loc_map.insert(locale.clone(), id);
                            }
                            Err(e) => push(&format!("   ⚠️  [{locale}] Could not add language: {e}")),
                        }
                    }
                }
            }

            // ── 6. For each locale × device type: upload screenshots ──────
            // Map IOS_TARGETS device folder names to ASC screenshotDisplayType values.
            // https://developer.apple.com/documentation/appstoreconnectapi/screenshotdisplaytype
            let device_types: &[(&str, &str)] = &[
                ("iPhone 6.9\" Display", "APP_IPHONE_69"),
                ("iPhone 6.7\" Display", "APP_IPHONE_67"),
                ("iPhone 6.5\" Display", "APP_IPHONE_65"),
                ("iPad Pro (12.9-inch)", "APP_IPAD_PRO_3GEN_129"),
            ];

            let upload_locales: Vec<String> = if locales.is_empty() {
                vec!["en-US".to_string()]
            } else {
                locales.clone()
            };

            for locale in &upload_locales {
                let loc_id = match loc_map.get(locale.as_str()) {
                    Some(id) => id.clone(),
                    None => {
                        push(&format!("⚠️  No localization found for {locale} — skipping (create it in App Store Connect first)"));
                        continue;
                    }
                };
                push(&format!("─── Locale: {locale} (loc_id: {loc_id}) ───"));

                for (device_name, display_type) in device_types {
                    // Collect screenshot files for this locale + device from output_paths,
                    // then fall back to the fastlane screenshots folder.
                    let mut files: Vec<PathBuf> = output_paths.iter()
                        .filter(|(label, path)| {
                            label.contains(device_name) && label.contains(locale.as_str()) && path.exists()
                        })
                        .map(|(_, p)| p.clone())
                        .collect();

                    if files.is_empty() {
                        // Fallback: fastlane/screenshots/ios/<locale>/<device_name>-*.png
                        let dir = fastlane_ios.join(locale);
                        if dir.exists() {
                            if let Ok(entries) = std::fs::read_dir(&dir) {
                                let mut found: Vec<PathBuf> = entries
                                    .flatten()
                                    .map(|e| e.path())
                                    .filter(|p| {
                                        p.file_name()
                                            .and_then(|n| n.to_str())
                                            .map(|n| n.starts_with(device_name) && n.ends_with(".png"))
                                            .unwrap_or(false)
                                    })
                                    .collect();
                                found.sort();
                                files = found;
                            }
                        }
                    }

                    if files.is_empty() {
                        push(&format!("   ⏭  No files for {device_name} [{locale}] — skipping"));
                        continue;
                    }

                    push(&format!("   📱 {device_name} — {} screenshot(s)", files.len()));

                    // Get or create screenshot set for this localization + display type.
                    let set_id = match asc_get_or_create_screenshot_set(&jwt, &loc_id, display_type).await {
                        Ok(id) => id,
                        Err(e) => {
                            publish_phase.set(PublishPhase::Error(format!("[{locale}] {device_name}: screenshot set error: {e}")));
                            return;
                        }
                    };

                    // Delete existing screenshots in the set before uploading fresh ones.
                    if let Err(e) = asc_delete_all_screenshots_in_set(&jwt, &set_id).await {
                        push(&format!("   ⚠️  Could not clear existing screenshots: {e}"));
                    }

                    // Upload each file.
                    for path in &files {
                        let fname = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                        push(&format!("   ⬆  {fname}…"));
                        match asc_upload_screenshot(&jwt, &set_id, path).await {
                            Ok(_) => push(&format!("   ✅ {fname}")),
                            Err(e) => {
                                publish_phase.set(PublishPhase::Error(format!("[{locale}] {device_name} {fname}: {e}")));
                                return;
                            }
                        }
                    }
                }
            }

            push("🎉 App Store listing uploaded (store text + screenshots)");
            record_release(ws, "ios", &ios_version, 0, "Listing uploaded");
            publish_phase.set(PublishPhase::Success);
        });
}

// ---- Publish Android screenshots to Google Play via androidpublisher v3 ----
pub(super) fn publish_google_play(ws: Ws) {
    let proj = ws.proj;
    let mut android_publish_phase = ws.play_pub;
    let mut android_publish_log = ws.play_pub_log;
    let proj_dir = ws.dir();

    android_publish_phase.set(AndroidPublishPhase::Running);
    android_publish_log.set(Vec::new());
    ws.show_job(JobKind::GooglePlay);

    // Snapshot what we need from project state before moving into async.
    let bundle_id = proj.read().android_bundle_id.clone(); // Google Play uses Android package name
    let output_paths = proj.read().output_paths.clone();
    let store_texts = proj.read().store_texts.clone();
    let app_name = proj.read().app_name.clone();
    let version = proj.read().version.trim().to_string();

    let proj_dir = proj_dir.clone();
    ws.spawn(async move {
            let mut log = android_publish_log.clone();
            let mut push = |msg: &str| log.write().push(msg.to_string());

            // ── 1. Credentials → Google access token ──────────────────────
            push("🔑 Authenticating with Google…");
            let (access_token, package_name) = match play_auth(&proj_dir, &bundle_id).await {
                Ok(v) => v,
                Err(e) => {
                    android_publish_phase.set(AndroidPublishPhase::Error(e));
                    return;
                }
            };
            push(&format!("📦 Package: {package_name}"));
            push("✅ Authenticated");

            // ── 4. Create an edit ─────────────────────────────────────────
            push("📝 Creating edit…");
            let edit_id = match google_play_create_edit(&access_token, &package_name).await {
                Ok(id) => id,
                Err(e) => {
                    android_publish_phase.set(AndroidPublishPhase::Error(
                        format!("Failed to create edit: {e}")
                    ));
                    return;
                }
            };
            push(&format!("   Edit ID: {edit_id}"));

            // ── 5. Collect Android screenshots from output_paths ──────────
            // output_paths entries: (label, path)
            // Labels look like "Android Phone [fr-FR] #1" / "Android Feature [fr-FR] #1"
            // (older output: "Screen 1 → Android Phone", no language tag)
            // Each language uploads its own tagged files (captions are baked in per language).
            let selected_locales = proj.read().locales.clone();
            let play_languages = if selected_locales.is_empty() {
                vec![proj.read().locale.clone()]
            } else {
                selected_locales.clone()
            };

            let android_files: Vec<(String, PathBuf)> = output_paths
                .iter()
                .filter(|(label, path)| {
                    (label.contains("Android Phone") || label.contains("Android Feature"))
                    && path.exists()
                })
                .map(|(label, path)| (label.clone(), path.clone()))
                .collect();

            let has_text = play_languages.iter().any(|l| store_texts.get(l).is_some_and(|t| !t.description.trim().is_empty()));
            if android_files.is_empty() && !has_text {
                android_publish_phase.set(AndroidPublishPhase::Error(
                    "Nothing to upload: no Android screenshots and no store text. Generate screenshots or fill in the Store text step.".into()
                ));
                let _ = google_play_delete_edit(&access_token, &package_name, &edit_id).await;
                return;
            }

            push(&format!("🖼  Found {} Android screenshot(s) to upload across {} locale(s)",
                android_files.len(), play_languages.len()));

            // ── 6. Upload each screenshot per locale ──────────────────────
            for play_language in &play_languages {
                // Play has its own codes for some languages (zh-CN, not zh-Hans).
                let api_lang = stores::play_language(play_language);
                push(&format!("─── Locale: {play_language} (Play: {api_lang}) ───"));

                let text = store_texts.get(play_language.as_str()).cloned().unwrap_or_default();
                if !text.description.trim().is_empty() && !text.short_description.trim().is_empty() {
                    push(&format!("📝 [{api_lang}] Updating store listing…"));
                    if let Err(e) = stores::play_update_listing(
                        &access_token, &package_name, &edit_id, api_lang, &app_name, &text,
                    ).await {
                        let _ = google_play_delete_edit(&access_token, &package_name, &edit_id).await;
                        android_publish_phase.set(AndroidPublishPhase::Error(format!("[{api_lang}] Listing: {e}")));
                        return;
                    }
                } else if !text.description.trim().is_empty() || !text.short_description.trim().is_empty() {
                    push(&format!("   ⚠️  [{api_lang}] Listing skipped — Play needs both a description and a short description"));
                }

                // Collect paths for this locale (prefer locale-specific files if they exist)
                let mut phone_paths: Vec<PathBuf> = Vec::new();
                let mut feature_paths: Vec<PathBuf> = Vec::new();

                // Files generated for this language carry its [locale] tag.
                let tag = format!("[{play_language}]");
                let locale_phone: Vec<PathBuf> = android_files.iter()
                    .filter(|(l, _)| l.contains("Android Phone") && l.contains(&tag))
                    .map(|(_, p)| p.clone()).collect();
                let locale_feature: Vec<PathBuf> = android_files.iter()
                    .filter(|(l, _)| l.contains("Android Feature") && l.contains(&tag))
                    .map(|(_, p)| p.clone()).collect();

                if !locale_phone.is_empty() || !locale_feature.is_empty() {
                    phone_paths = locale_phone;
                    feature_paths = locale_feature;
                } else {
                    // Output from before language tags existed is untagged and
                    // shared; never borrow another language's tagged files.
                    for (label, path) in android_files.iter().filter(|(l, _)| !l.contains('[')) {
                        if label.contains("Android Phone") { phone_paths.push(path.clone()); }
                        else if label.contains("Android Feature") { feature_paths.push(path.clone()); }
                    }
                }

                // Delete existing then upload for each image type
                for (image_type, paths) in [
                    ("PHONE_SCREENSHOTS", &phone_paths),
                    ("FEATURE_GRAPHIC", &feature_paths),
                ] {
                    if paths.is_empty() { continue; }

                    // Clear existing
                    let _ = google_play_delete_images(
                        &access_token, &package_name, &edit_id, api_lang, image_type
                    ).await;

                    for path in paths {
                        let fname = path.file_name().unwrap_or_default().to_string_lossy();
                        push(&format!("⬆  [{play_language}] Uploading {fname} ({image_type})…"));
                        if let Err(e) = google_play_upload_image(
                            &access_token, &package_name, &edit_id,
                            api_lang, image_type, path
                        ).await {
                            let _ = google_play_delete_edit(&access_token, &package_name, &edit_id).await;
                            android_publish_phase.set(AndroidPublishPhase::Error(
                                format!("[{play_language}] Upload failed for {fname}: {e}")
                            ));
                            return;
                        }
                        push(&format!("   ✅ {fname}"));
                    }
                }
            }

            // ── 7. Commit the edit ────────────────────────────────────────
            push("💾 Committing edit…");
            if let Err(e) = google_play_commit_edit(&access_token, &package_name, &edit_id).await {
                android_publish_phase.set(AndroidPublishPhase::Error(
                    format!("Failed to commit edit: {e}")
                ));
                return;
            }

            push("🎉 Google Play listing uploaded (store text + screenshots)");
            record_release(ws, "android", &version, 0, "Listing uploaded");
            android_publish_phase.set(AndroidPublishPhase::Success);
        });
}

// ---- Run build script ----
// Ensures scripts exist (generates if missing), then runs the chosen one.
pub(super) fn run_build(ws: Ws, script_name: String) {
    let proj = ws.proj;
    let settings = ws.settings;
    let mut build_phase = ws.build_phase;
    let mut build_log = ws.build_log;
    let mut refresh = ws.refresh;
    let proj_dir = ws.dir();

    let app_name = proj.read().app_name.clone();
    let project_slug = proj.read().project_slug.clone();
    let ios_bundle_id = proj.read().ios_bundle_id.clone();
    let android_bundle_id = proj.read().android_bundle_id.clone();
    let identity = settings.read().apple_identity.clone();
    let profile = proj.read().provisioning_profile.clone();
    let version = proj.read().version.trim().to_string();
    let ios_build_number = proj.read().ios_build_number;
    let android_version_code = proj.read().android_version_code;
    let logo_path = proj.read().logo_path.clone();

    // Validate required fields
    if app_name.trim().is_empty()
        || project_slug.trim().is_empty()
        || (ios_bundle_id.trim().is_empty() && android_bundle_id.trim().is_empty())
    {
        build_phase.set(BuildPhase::Error(
                "App Name, Project Slug, and at least one Bundle ID are required. Fill them in the App step.".into()
            ));
        ws.show_job(JobKind::Build);
        return;
    }
    if !valid_version(&version) {
        build_phase.set(BuildPhase::Error(
            "Set a valid version (e.g. 1.2.0) in the Version step.".into(),
        ));
        ws.show_job(JobKind::Build);
        return;
    }
    if script_name.contains("ios") && (identity.trim().is_empty() || profile.trim().is_empty()) {
        build_phase.set(BuildPhase::Error(
                "Apple Identity and Provisioning Profile are required for iOS. Set them in the Accounts step.".into()
            ));
        ws.show_job(JobKind::Build);
        return;
    }

    build_phase.set(BuildPhase::Running(script_name.clone()));
    build_log.set(Vec::new());
    ws.show_job(JobKind::Build);

    let dir = proj_dir.clone();
    ws.spawn(async move {
        // 0. Sync logo → assets/icon.png so the build script always uses the latest logo.
        // Always re-encode as genuine PNG — the source may be a JPEG renamed to .png,
        // which Apple's validator rejects even though the extension looks right.
        if let Some(src) = &logo_path {
            let dest = dir.join("assets").join("icon.png");
            let src2 = src.clone();
            let dest2 = dest.clone();
            match tokio::task::spawn_blocking(move || {
                image::open(&src2)
                    .map_err(|e| e.to_string())
                    .and_then(|img| {
                        // Convert to RGB (drop alpha) and save as real PNG
                        img.to_rgb8()
                            .save_with_format(&dest2, image::ImageFormat::Png)
                            .map_err(|e| e.to_string())
                    })
            })
            .await
            {
                Ok(Ok(_)) => build_log
                    .write()
                    .push("🖼️  Logo saved as true PNG to assets/icon.png".into()),
                Ok(Err(e)) => build_log
                    .write()
                    .push(format!("⚠️  Could not encode logo as PNG: {e}")),
                Err(e) => build_log.write().push(format!("⚠️  Logo task failed: {e}")),
            }
        }

        // 1. Ensure scripts exist (create if missing)
        let created = tokio::task::spawn_blocking({
            let dir = dir.clone();
            let app_name = app_name.clone();
            let project_slug = project_slug.clone();
            let ios_bundle_id = ios_bundle_id.clone();
            let android_bundle_id = android_bundle_id.clone();
            let identity = identity.clone();
            let profile = profile.clone();
            let version = version.clone();
            move || {
                ensure_build_scripts(
                    &dir,
                    &app_name,
                    &project_slug,
                    &ios_bundle_id,
                    &android_bundle_id,
                    &identity,
                    &profile,
                    &version,
                    ios_build_number,
                    android_version_code,
                )
            }
        })
        .await
        .unwrap_or_default();

        for name in &created {
            build_log.write().push(format!("📝 Created {name}"));
        }

        // 2. Run the script
        let script_path = dir.join(&script_name);
        if !script_path.exists() {
            build_phase.set(BuildPhase::Error(format!(
                "{script_name} not found in project directory"
            )));
            return;
        }

        build_log.write().push(format!("▶ Running {script_name}…"));

        let mut cmd = std::process::Command::new("bash");
        cmd.arg(&script_path).current_dir(&dir).env("CI", "1");
        for (k, v) in load_env(&[dir.join(".env")]) {
            cmd.env(k, v);
        }
        match stream_command(cmd, &script_name, build_log).await {
            Ok(()) => {
                build_phase.set(BuildPhase::Success(script_name.clone()));
                // The number is now taken in the store's eyes; the next
                // release build needs a higher one.
                if proj.peek().auto_increment_build {
                    match script_name.as_str() {
                        "build_ios_distribution.sh" => ws.update(|p| p.ios_build_number += 1),
                        "build_android_release.sh" => ws.update(|p| p.android_version_code += 1),
                        _ => {}
                    }
                }
            }
            Err(e) => build_phase.set(BuildPhase::Error(e)),
        }
        refresh.with_mut(|n| *n += 1);
    });
}

// ---- AI Generate ----
pub(super) fn generate_ai(ws: Ws) {
    let mut proj = ws.proj;
    let settings = ws.settings;
    let mut phase = ws.gen_phase;
    let mut log_lines = ws.gen_log;
    let mut add_log = move |msg: String| {
        log_lines.write().push(msg);
    };
    let proj_dir = ws.dir();

    let prompt = proj.read().theme_prompt.clone();
    let selected_locales = proj.read().locales.clone();
    // Per-locale sources: locale → Vec<PathBuf>
    let locale_srcs: std::collections::HashMap<String, Vec<PathBuf>> = selected_locales
        .iter()
        .map(|loc| (loc.clone(), proj.read().sources_for(loc)))
        .collect();
    let targets = ExportTargets::for_project(&proj_dir, &proj.read());
    // Desktop screens are landscape; phone and tablet ones are portrait.
    let desktop = proj.read().platform_type.has_desktop();

    // Check every locale has at least one image
    let empty_locales: Vec<&String> = selected_locales
        .iter()
        .filter(|loc| locale_srcs.get(*loc).map(|v| v.is_empty()).unwrap_or(true))
        .collect();
    if !empty_locales.is_empty() {
        phase.set(AppPhase::Error(format!(
            "No images for locale(s): {}. Add images in each language tab.",
            empty_locales
                .iter()
                .map(|l| l.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )));
        return;
    }
    if prompt.trim().is_empty() {
        phase.set(AppPhase::Error(
            "Please enter a theme prompt for AI generation.".into(),
        ));
        return;
    }

    // Save prompt to history
    {
        let trimmed = prompt.trim().to_string();
        let mut p = proj.write();
        p.theme_history.retain(|h| h != &trimmed);
        p.theme_history.insert(0, trimmed);
        p.theme_history.truncate(MAX_THEME_HISTORY);
        save_project_state(&proj_dir, &p);
    }

    let api_key = settings.read().fal_key.clone();
    let phone_style = settings.read().phone_style.clone();
    let steps_val = settings.read().inference_steps;
    let proj_dir = proj_dir.clone();

    ws.spawn(async move {
        phase.set(AppPhase::GeneratingAi);
        log_lines.set(Vec::new());
        ws.show_job(JobKind::Screenshots);
        {
            let mut p = proj.write();
            p.generated_urls.clear();
            p.output_paths.clear();
        }

        if api_key.is_empty() {
            phase.set(AppPhase::Error(
                "No fal.ai API key set. Add it in Settings (gear icon in the sidebar).".into(),
            ));
            return;
        }

        let (frame_w, frame_h): (u32, u32) = if desktop { (1920, 1200) } else { (1290, 2796) };
        let device = if desktop { "modern laptop computer".to_string() } else { phone_style.clone() };
        let mut all_outputs: Vec<(String, PathBuf)> = Vec::new();

        // Each locale has its own images — process independently
        for locale in &selected_locales {
            let srcs = locale_srcs.get(locale).cloned().unwrap_or_default();
            let total = srcs.len();
            // Drop last run's phone screenshots so fewer screens this time
            // doesn't leave old ones behind in the fastlane folder.
            if !targets.android.is_empty() {
                let _ = std::fs::remove_dir_all(
                    targets.android_root.join(stores::play_language(locale)).join("images").join("phoneScreenshots"),
                );
            }
            if !targets.desktop.is_empty() {
                let _ = std::fs::remove_dir_all(targets.desktop_dir.join(locale));
            }
            add_log(format!(
                "─── AI generating locale: {locale} ({total} image(s)) ───"
            ));

            for (idx, src) in srcs.iter().enumerate() {
                let screen_num = idx + 1;
                add_log(format!(
                    "[{locale}] Reading image {screen_num}/{total}: {}",
                    src.file_name().unwrap_or_default().to_string_lossy()
                ));

                // Async file read — does not block the UI.
                let img_bytes = match tokio::fs::read(src).await {
                    Ok(b) => b,
                    Err(e) => {
                        phase.set(AppPhase::Error(format!(
                            "[{locale}] Failed to read image {screen_num}: {e}"
                        )));
                        return;
                    }
                };
                // Decode source screenshot (fast, fine on async thread).
                let screenshot = match image::load_from_memory(&img_bytes) {
                    Ok(img) => img.to_rgba8(),
                    Err(e) => {
                        phase.set(AppPhase::Error(format!(
                            "[{locale}] Failed to decode image {screen_num}: {e}"
                        )));
                        return;
                    }
                };

                let full_prompt = build_prompt(&prompt, &device, screen_num, total);
                add_log(format!(
                    "[{locale}] Generating frame {screen_num}/{total} via FLUX…"
                ));

                let frame_url = match call_fal_text_to_image(
                    &api_key,
                    &full_prompt,
                    frame_w,
                    frame_h,
                    steps_val,
                )
                .await
                {
                    Ok(url) => {
                        add_log(format!("Got frame {screen_num}"));
                        url
                    }
                    Err(e) => {
                        phase.set(AppPhase::Error(format!(
                            "[{locale}] Frame generation failed for screen {screen_num}: {e}"
                        )));
                        return;
                    }
                };

                add_log(format!("[{locale}] Downloading frame {screen_num}…"));
                let frame_bytes = match download_image(&frame_url).await {
                    Ok(b) => b,
                    Err(e) => {
                        phase.set(AppPhase::Error(format!(
                            "[{locale}] Download failed for frame {screen_num}: {e}"
                        )));
                        return;
                    }
                };

                // Heavy CPU work (decode AI frame, composite, encode, resize) on a
                // blocking thread so the async executor — and the UI — stay free.
                phase.set(AppPhase::Resizing);
                add_log(format!("[{locale}] Compositing screenshot {screen_num}…"));
                let locale2 = locale.clone();
                let targets2 = targets.clone();
                let play_locale = stores::play_language(locale).to_string();
                let cpu = tokio::task::spawn_blocking(
                    move || -> Result<(Vec<(String, PathBuf)>, Option<String>), String> {
                        let mut frame_img = image::load_from_memory(&frame_bytes)
                            .map_err(|e| {
                                format!("[{locale2}] Failed to decode frame {screen_num}: {e}")
                            })?
                            .to_rgba8();
                        let rect = find_placeholder_rect(&frame_img).unwrap_or_else(|| {
                            fallback_placement(frame_img.width(), frame_img.height())
                        });
                        composite_screenshot(&mut frame_img, &screenshot, rect);

                        let composited_bytes = {
                            let mut buf = std::io::Cursor::new(Vec::new());
                            frame_img
                                .write_to(&mut buf, image::ImageFormat::Png)
                                .map_err(|e| {
                                    format!("[{locale2}] Failed to encode image {screen_num}: {e}")
                                })?;
                            buf.into_inner()
                        };

                        let paths = resize_to_targets(
                            &composited_bytes,
                            screen_num,
                            &locale2,
                            &play_locale,
                            &targets2,
                        )
                        .map_err(|e| {
                            format!("Resize failed for [{locale2}] screen {screen_num}: {e}")
                        })?;

                        let preview =
                            paths.iter().find(|(l, _)| l.contains("6.7")).or_else(|| paths.first()).map(|(_, p)| {
                                urlencoding::encode(&p.to_string_lossy().to_string()).into_owned()
                            });

                        Ok((paths, preview))
                    },
                );

                let (paths, preview) = match cpu.await {
                    Ok(Ok(v)) => v,
                    Ok(Err(e)) => {
                        phase.set(AppPhase::Error(e));
                        return;
                    }
                    Err(e) => {
                        phase.set(AppPhase::Error(format!("Thread panic: {e}")));
                        return;
                    }
                };

                for (label, _) in &paths {
                    add_log(format!("Saved: {label}"));
                }
                if let Some(enc) = preview {
                    let mut pw = proj.write();
                    if !pw.generated_urls.iter().any(|u| u.contains(&enc)) {
                        pw.generated_urls.push(format!("/localimg/{enc}"));
                    }
                }
                all_outputs.extend(paths);
                phase.set(AppPhase::GeneratingAi);
            }
        }

        {
            let mut p = proj.write();
            p.output_paths = all_outputs;
            save_project_state(&proj_dir, &p);
        }
        let loc_count = selected_locales.len();
        add_log(format!(
            "Done! AI screenshots generated for {loc_count} locale(s)."
        ));
        phase.set(AppPhase::Done);
    });
}

// ---- Manual Generate ----
pub(super) fn generate_manual(ws: Ws) {
    let mut proj = ws.proj;
    let mut phase = ws.gen_phase;
    let mut log_lines = ws.gen_log;
    let mut add_log = move |msg: String| {
        log_lines.write().push(msg);
    };
    let proj_dir = ws.dir();

    let primary = proj.read().primary_color.clone();
    let secondary = proj.read().secondary_color.clone();
    let selected_locales = proj.read().locales.clone();
    // Per-locale sources and texts
    let locale_srcs: std::collections::HashMap<String, Vec<PathBuf>> = selected_locales
        .iter()
        .map(|loc| (loc.clone(), proj.read().sources_for(loc)))
        .collect();
    let locale_texts_map: std::collections::HashMap<String, Vec<(String, String)>> =
        selected_locales
            .iter()
            .map(|loc| (loc.clone(), proj.read().texts_for(loc)))
            .collect();
    let targets = ExportTargets::for_project(&proj_dir, &proj.read());
    // Desktop screens are landscape; phone and tablet ones are portrait.
    let desktop = proj.read().platform_type.has_desktop();
    let proj_dir = proj_dir.clone();

    // Check every locale has at least one image
    let empty_locales: Vec<&String> = selected_locales
        .iter()
        .filter(|loc| locale_srcs.get(*loc).map(|v| v.is_empty()).unwrap_or(true))
        .collect();
    if !empty_locales.is_empty() {
        phase.set(AppPhase::Error(format!(
            "No images for locale(s): {}. Add images in each language tab.",
            empty_locales
                .iter()
                .map(|l| l.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )));
        return;
    }

    ws.spawn(async move {
        phase.set(AppPhase::GeneratingManual);
        log_lines.set(Vec::new());
        ws.show_job(JobKind::Screenshots);
        {
            let mut p = proj.write();
            p.generated_urls.clear();
            p.output_paths.clear();
        }

        let primary_rgba = parse_hex_color(&primary).unwrap_or(Rgba([59, 130, 246, 255]));
        let secondary_rgba = if !secondary.is_empty() {
            parse_hex_color(&secondary).unwrap_or(lighten_color(primary_rgba))
        } else {
            lighten_color(primary_rgba)
        };
        let mut all_outputs: Vec<(String, PathBuf)> = Vec::new();

        // Each locale has its own images — process independently
        for locale in &selected_locales {
            let srcs = locale_srcs.get(locale).cloned().unwrap_or_default();
            let total = srcs.len();
            // Drop last run's phone screenshots so fewer screens this time
            // doesn't leave old ones behind in the fastlane folder.
            if !targets.android.is_empty() {
                let _ = std::fs::remove_dir_all(
                    targets.android_root.join(stores::play_language(locale)).join("images").join("phoneScreenshots"),
                );
            }
            if !targets.desktop.is_empty() {
                let _ = std::fs::remove_dir_all(targets.desktop_dir.join(locale));
            }
            let texts = locale_texts_map.get(locale).cloned().unwrap_or_default();
            add_log(format!(
                "─── Generating locale: {locale} ({total} image(s)) ───"
            ));

            for (idx, src) in srcs.iter().enumerate() {
                let screen_num = idx + 1;
                add_log(format!("[{locale}] Processing image {screen_num}/{total}…"));

                // Clone everything the blocking thread needs.
                let src2 = src.clone();
                let locale2 = locale.clone();
                let targets2 = targets.clone();
                let play_locale = stores::play_language(locale).to_string();
                let bg_color = if idx % 2 == 0 {
                    primary_rgba
                } else {
                    secondary_rgba
                };
                let (title, subtitle) = texts.get(idx).cloned().unwrap_or_default();

                // All CPU-heavy work (read, decode, composite, encode, resize) runs on
                // a blocking thread so the async executor — and the UI — stay free.
                let cpu = tokio::task::spawn_blocking(
                    move || -> Result<(Vec<(String, PathBuf)>, Option<String>), String> {
                        let (w, h): (u32, u32) = if desktop { (2560, 1600) } else { (1290, 2796) };
                        let font = FontRef::try_from_slice(ROBOTO_FONT).expect("font");

                        let screenshot_bytes = std::fs::read(&src2)
                            .map_err(|e| format!("Failed to read image {screen_num}: {e}"))?;
                        let screenshot = image::load_from_memory(&screenshot_bytes)
                            .map_err(|e| format!("Failed to decode image {screen_num}: {e}"))?
                            .to_rgba8();

                        let mut img = RgbaImage::from_pixel(w, h, bg_color);
                        let text_color = get_contrast_color(bg_color);
                        // (title size, title y, subtitle size, subtitle y)
                        let (ts, ty, ss, sy) = if desktop { (110.0, 90, 56.0, 230) } else { (120.0, 200, 60.0, 350) };
                        if !title.is_empty() {
                            draw_centered_text(&mut img, &font, &title, PxScale::from(ts), text_color, ty);
                        }
                        if !subtitle.is_empty() {
                            draw_centered_text(&mut img, &font, &subtitle, PxScale::from(ss), text_color, sy);
                        }

                        if desktop {
                            // Fit the whole window below the captions, keeping
                            // its proportions — a desktop screenshot can be any shape.
                            let (area_x, area_y) = (200u32, 360u32);
                            let (area_w, area_h) = (w - 2 * area_x, h - area_y - 120);
                            let scale = (area_w as f64 / screenshot.width() as f64)
                                .min(area_h as f64 / screenshot.height() as f64);
                            let fit_w = ((screenshot.width() as f64 * scale) as u32).max(1);
                            let fit_h = ((screenshot.height() as f64 * scale) as u32).max(1);
                            let resized = image::imageops::resize(&screenshot, fit_w, fit_h, FilterType::Lanczos3);
                            image::imageops::overlay(
                                &mut img,
                                &resized,
                                (area_x + (area_w - fit_w) / 2) as i64,
                                (area_y + (area_h - fit_h) / 2) as i64,
                            );
                        } else {
                            let phone_w = (w as f64 * 0.7) as u32;
                            let phone_h = (phone_w as f64 * 2.16) as u32;
                            let phone_x = (w - phone_w) / 2;
                            let phone_y = h - phone_h - 150;
                            let resized = image::imageops::resize(&screenshot, phone_w, phone_h, FilterType::Lanczos3);
                            image::imageops::overlay(&mut img, &resized, phone_x as i64, phone_y as i64);
                        }

                        let composited_bytes = {
                            let mut buf = std::io::Cursor::new(Vec::new());
                            img.write_to(&mut buf, image::ImageFormat::Png)
                                .map_err(|e| format!("Failed to encode image {screen_num}: {e}"))?;
                            buf.into_inner()
                        };

                        let paths = resize_to_targets(
                            &composited_bytes,
                            screen_num,
                            &locale2,
                            &play_locale,
                            &targets2,
                        )
                        .map_err(|e| {
                            format!("Resize failed for [{locale2}] screen {screen_num}: {e}")
                        })?;

                        let preview =
                            paths.iter().find(|(l, _)| l.contains("6.7")).or_else(|| paths.first()).map(|(_, p)| {
                                urlencoding::encode(&p.to_string_lossy().to_string()).into_owned()
                            });

                        Ok((paths, preview))
                    },
                );

                let (paths, preview) = match cpu.await {
                    Ok(Ok(v)) => v,
                    Ok(Err(e)) => {
                        phase.set(AppPhase::Error(e));
                        return;
                    }
                    Err(e) => {
                        phase.set(AppPhase::Error(format!("Thread panic: {e}")));
                        return;
                    }
                };

                for (label, _) in &paths {
                    add_log(format!("Saved: {label}"));
                }
                if let Some(enc) = preview {
                    proj.write().generated_urls.push(format!("/localimg/{enc}"));
                }
                all_outputs.extend(paths);
            }
        }

        {
            let mut p = proj.write();
            p.output_paths = all_outputs;
            save_project_state(&proj_dir, &p);
        }
        let loc_count = selected_locales.len();
        add_log(format!(
            "Done! Manual screenshots generated for {loc_count} locale(s)."
        ));
        phase.set(AppPhase::Done);
    });
}

/// Run a command on a plain OS thread (Signals are !Send), streaming its
/// stdout and stderr into `log` line by line. Lines flow back over mpsc and are
/// pushed from this task, on the UI thread, so Dioxus re-renders live.
async fn stream_command(
    mut cmd: std::process::Command,
    name: &str,
    mut log: Signal<Vec<String>>,
) -> Result<(), String> {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    use std::sync::mpsc;

    enum Msg {
        Line(String),
        Done(Result<(), String>),
    }
    let (tx, rx) = mpsc::channel::<Msg>();
    let name_for_thread = name.to_string();
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    std::thread::spawn(move || {
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(Msg::Done(Err(format!("Could not start {name_for_thread}: {e}"))));
                return;
            }
        };
        let tx2 = tx.clone();
        let stderr_stream = child.stderr.take();
        std::thread::spawn(move || {
            if let Some(r) = stderr_stream.map(BufReader::new) {
                for line in r.lines().map_while(Result::ok) {
                    let _ = tx2.send(Msg::Line(line));
                }
            }
        });
        if let Some(stdout) = child.stdout.take() {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = tx.send(Msg::Line(line));
            }
        }
        let result = match child.wait() {
            Ok(s) if s.success() => Ok(()),
            Ok(s) => Err(format!("{name_for_thread} exited with code {}", s.code().unwrap_or(-1))),
            Err(e) => Err(format!("wait: {e}")),
        };
        let _ = tx.send(Msg::Done(result));
    });

    loop {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        for msg in rx.try_iter() {
            match msg {
                Msg::Line(line) => {
                    let clean = strip_ansi(&line);
                    if !clean.trim().is_empty() {
                        log.write().push(clean);
                    }
                }
                Msg::Done(result) => return result,
            }
        }
    }
}

// ---- Store credentials ----
// Shared by the uploads and the Accounts step's connection tests. Lookup
// order: project .env, fastlane/.env, then the process environment.

fn env_resolver(dir: &std::path::Path) -> impl Fn(&str) -> Option<String> {
    let env = load_env(&[dir.join(".env"), dir.join("fastlane").join(".env")]);
    move |key: &str| {
        env.get(key)
            .cloned()
            .or_else(|| std::env::var(key).ok())
            .filter(|v| !v.is_empty())
    }
}

/// The project's App Store Connect API key: (key id, issuer id, .p8 path).
fn asc_key(dir: &std::path::Path) -> Result<(String, String, String), String> {
    let resolve = env_resolver(dir);
    let missing = |key: &str| format!("Missing {key} in .env");
    let key_id = resolve("APP_STORE_CONNECT_API_KEY_KEY_ID")
        .ok_or_else(|| missing("APP_STORE_CONNECT_API_KEY_KEY_ID"))?;
    let issuer_id = resolve("APP_STORE_CONNECT_API_KEY_ISSUER_ID")
        .ok_or_else(|| missing("APP_STORE_CONNECT_API_KEY_ISSUER_ID"))?;
    let key_path = resolve("APP_STORE_CONNECT_API_KEY_KEY_FILEPATH")
        .ok_or_else(|| missing("APP_STORE_CONNECT_API_KEY_KEY_FILEPATH"))?;
    Ok((key_id, issuer_id, key_path))
}

/// Mint an App Store Connect JWT from the project's API key.
async fn asc_auth(dir: &std::path::Path) -> Result<String, String> {
    let (key_id, issuer_id, key_path) = asc_key(dir)?;
    let p8_pem = tokio::fs::read_to_string(&key_path)
        .await
        .map_err(|e| format!("Cannot read .p8 key at {key_path}: {e}"))?;
    asc_mint_jwt(&key_id, &issuer_id, &p8_pem).map_err(|e| format!("JWT error: {e}"))
}

/// Exchange the project's service account for a Google access token.
/// Returns (access token, package name).
async fn play_auth(dir: &std::path::Path, bundle_id: &str) -> Result<(String, String), String> {
    let resolve = env_resolver(dir);
    // GOOGLE_PLAY_JSON_KEY  – path to service-account .json file
    // ANDROID_PACKAGE_NAME  – optional override (falls back to bundle_id)
    let json_key_path = resolve("GOOGLE_PLAY_JSON_KEY").ok_or(
        "Missing GOOGLE_PLAY_JSON_KEY env var.\nSet it to the path of your Google service-account JSON file in fastlane/.env or export it before launching the app.",
    )?;
    let package_name = resolve("ANDROID_PACKAGE_NAME")
        .or_else(|| (!bundle_id.is_empty()).then(|| bundle_id.to_string()))
        .ok_or("Cannot determine Android package name.\nSet ANDROID_PACKAGE_NAME in fastlane/.env or fill in the Android package in the App step.")?;

    let sa_json: serde_json::Value = tokio::fs::read_to_string(&json_key_path)
        .await
        .map_err(|e| e.to_string())
        .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
        .map_err(|e| format!("Failed to read service-account JSON at {json_key_path}: {e}"))?;
    let client_email = sa_json["client_email"].as_str().unwrap_or("");
    let private_key_pem = sa_json["private_key"].as_str().unwrap_or("");
    if client_email.is_empty() || private_key_pem.is_empty() {
        return Err("Service-account JSON is missing client_email or private_key.".into());
    }
    let token = google_play_access_token(client_email, private_key_pem)
        .await
        .map_err(|e| format!("Authentication failed: {e}"))?;
    Ok((token, package_name))
}

// ---- Connection tests (Accounts step) ----

/// Authenticate, then look the app up by bundle ID.
pub(super) fn test_app_store(ws: Ws) {
    let mut state = ws.asc_test;
    let bundle_id = ws.proj.read().ios_bundle_id.trim().to_string();
    let dir = ws.dir();
    state.set(JobState::Running("Connecting…".into()));
    ws.spawn(async move {
        let result: Result<String, String> = async {
            let jwt = asc_auth(&dir).await?;
            if bundle_id.is_empty() {
                return Ok("Authenticated. Set the iOS bundle ID to check the app.".to_string());
            }
            let app_id = asc_find_app(&jwt, &bundle_id).await?;
            Ok(format!("Connected · {bundle_id} found (app {app_id})"))
        }
        .await;
        state.set(match result {
            Ok(m) => JobState::Ok(m),
            Err(e) => JobState::Failed(e),
        });
    });
}

/// Authenticate, then open and discard an edit — proves the service account
/// can actually change this app's listing.
pub(super) fn test_google_play(ws: Ws) {
    let mut state = ws.play_test;
    let bundle_id = ws.proj.read().android_bundle_id.trim().to_string();
    let dir = ws.dir();
    state.set(JobState::Running("Connecting…".into()));
    ws.spawn(async move {
        let result: Result<String, String> = async {
            let (token, package) = play_auth(&dir, &bundle_id).await?;
            let edit_id = google_play_create_edit(&token, &package)
                .await
                .map_err(|e| format!("Authenticated, but cannot edit {package}: {e}"))?;
            let _ = google_play_delete_edit(&token, &package, &edit_id).await;
            Ok(format!("Connected · can edit {package}"))
        }
        .await;
        state.set(match result {
            Ok(m) => JobState::Ok(m),
            Err(e) => JobState::Failed(e),
        });
    });
}

// ---- Release history ----

fn record_release(ws: Ws, platform: &str, version: &str, build: u32, action: &str) {
    let record = ReleaseRecord {
        at: utc_now(),
        platform: platform.to_string(),
        version: version.to_string(),
        build,
        action: action.to_string(),
    };
    ws.update(|p| {
        p.releases.insert(0, record);
        p.releases.truncate(100);
    });
}

// ---- iOS: upload the IPA ----

/// Upload the newest IPA with `xcrun altool`, authenticated with the
/// project's App Store Connect API key.
pub(super) fn upload_ipa(ws: Ws) {
    let mut state = ws.ios_build_job;
    let mut log = ws.ios_build_log;
    let dir = ws.dir();
    ws.show_job(JobKind::AppStoreBuild);
    let Some(ipa) = ws.artifacts.read().ipa.clone() else {
        state.set(JobState::Failed("No IPA yet — build one in the Build step.".into()));
        return;
    };
    state.set(JobState::Running("Uploading IPA…".into()));
    log.set(Vec::new());

    ws.spawn(async move {
        let result: Result<(String, u32), String> = async {
            let (key_id, issuer_id, key_path) = asc_key(&dir)?;
            let ipa_path = ipa.path.clone();
            let (version, build) = tokio::task::spawn_blocking(move || stores::ipa_version(&ipa_path))
                .await
                .ok()
                .flatten()
                .ok_or("Could not read the version from the IPA's Info.plist")?;
            log.write().push(format!("📦 {} — version {version} ({build})", ipa.file_name()));

            // altool only finds keys named AuthKey_<id>.p8 in a known folder;
            // hand it a private temporary one rather than touching ~/.
            let keys_dir = std::env::temp_dir().join(format!("appscreens-keys-{}", std::process::id()));
            std::fs::create_dir_all(&keys_dir).map_err(|e| e.to_string())?;
            let key_copy = keys_dir.join(format!("AuthKey_{key_id}.p8"));
            std::fs::copy(&key_path, &key_copy).map_err(|e| format!("Cannot read .p8 key at {key_path}: {e}"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&key_copy, std::fs::Permissions::from_mode(0o600));
            }

            log.write().push("⬆  Uploading with xcrun altool (this can take a few minutes)…".into());
            let mut cmd = std::process::Command::new("xcrun");
            cmd.args(["altool", "--upload-app", "--type", "ios", "--file"])
                .arg(&ipa.path)
                .args(["--apiKey", &key_id, "--apiIssuer", &issuer_id])
                .env("API_PRIVATE_KEYS_DIR", &keys_dir);
            let uploaded = stream_command(cmd, "altool", log).await;
            let _ = std::fs::remove_dir_all(&keys_dir);
            uploaded?;
            Ok((version, build))
        }
        .await;

        match result {
            Ok((version, build)) => {
                record_release(ws, "ios", &version, build, "Uploaded build");
                log.write().push("🎉 Uploaded. App Store Connect needs a few minutes to process it before it can be submitted.".into());
                state.set(JobState::Ok(format!("Uploaded {version} ({build})")));
            }
            Err(e) => state.set(JobState::Failed(e)),
        }
    });
}

/// The iOS build to submit: the last one uploaded from here, else the IPA on disk.
fn ios_build_to_submit(ws: Ws) -> Option<(String, u32)> {
    let last_uploaded = ws
        .proj
        .read()
        .releases
        .iter()
        .find(|r| r.platform == "ios" && r.action == "Uploaded build")
        .map(|r| (r.version.clone(), r.build));
    last_uploaded.or_else(|| ws.artifacts.read().ipa.as_ref().and_then(|a| stores::ipa_version(&a.path)))
}

// ---- iOS: attach the build and submit for review ----

pub(super) fn submit_for_review(ws: Ws) {
    let mut state = ws.ios_review_job;
    let mut log = ws.ios_review_log;
    let dir = ws.dir();
    let bundle_id = ws.proj.read().ios_bundle_id.trim().to_string();
    let exempt = ws.proj.read().exempt_encryption;
    ws.show_job(JobKind::AppStoreReview);
    let Some((version, build)) = ios_build_to_submit(ws) else {
        state.set(JobState::Failed("No build to submit — upload an IPA first.".into()));
        return;
    };
    state.set(JobState::Running(format!("Submitting {version} ({build})…")));
    log.set(Vec::new());

    ws.spawn(async move {
        let mut push = move |msg: String| log.write().push(msg);
        let result: Result<(), String> = async {
            push("🔑 Authenticating with App Store Connect…".into());
            let jwt = asc_auth(&dir).await?;
            let app_id = asc_find_app(&jwt, &bundle_id).await?;

            // A fresh upload takes a while to process; wait for it (≤ 30 min).
            push(format!("🔍 Looking for build {version} ({build})…"));
            let mut found = None;
            for attempt in 0..60 {
                match stores::asc_find_build(&jwt, &app_id, &version, build).await? {
                    Some(b) if b.state == "VALID" => {
                        found = Some(b);
                        break;
                    }
                    Some(b) if b.state == "FAILED" || b.state == "INVALID" => {
                        return Err(format!("App Store Connect marked build {build} as {} — check the email Apple sent.", b.state));
                    }
                    Some(_) => push(format!("   ⏳ Processing… (checked {} time(s))", attempt + 1)),
                    None => push(format!("   ⏳ Not received yet… (checked {} time(s))", attempt + 1)),
                }
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            }
            let b = found.ok_or("Build still not processed after 30 minutes — try again later.")?;
            push(format!("✅ Build ready ({})", b.id));

            if b.uses_non_exempt_encryption.is_none() {
                if !exempt {
                    return Err("Export compliance isn't answered for this build. Tick \"only exempt encryption\" if that's true, or answer it in App Store Connect.".into());
                }
                push("🔐 Declaring exempt encryption…".into());
                stores::asc_set_exempt_encryption(&jwt, &b.id).await?;
            }

            push(format!("🔗 Attaching build to version {version}…"));
            let version_id = asc_find_or_create_version(&jwt, &app_id, &version).await?;
            stores::asc_attach_build(&jwt, &version_id, &b.id).await?;

            push("📨 Submitting for review…".into());
            let submission = stores::asc_submit_for_review(&jwt, &app_id, &version_id).await?;
            push(format!("🎉 Submitted for review (submission {submission})"));
            Ok(())
        }
        .await;

        match result {
            Ok(()) => {
                record_release(ws, "ios", &version, build, "Submitted for review");
                state.set(JobState::Ok(format!("{version} ({build}) submitted for review")));
            }
            Err(e) => state.set(JobState::Failed(e)),
        }
    });
}

// ---- Android: upload the AAB and release it to a track ----

pub(super) fn release_aab(ws: Ws) {
    let mut state = ws.play_release_job;
    let mut log = ws.play_release_log;
    let dir = ws.dir();
    let p = ws.proj.read().clone();
    ws.show_job(JobKind::PlayRelease);
    let Some(aab) = ws.artifacts.read().aab.clone() else {
        state.set(JobState::Failed("No AAB yet — build one in the Build step.".into()));
        return;
    };
    let track = p.play_track.clone();
    let status = p.play_release_status.clone();
    let version = p.version.trim().to_string();
    // Play caps release notes at 500 characters per language.
    let notes: Vec<(String, String)> = p
        .locales
        .iter()
        .filter_map(|l| {
            let text = p.store_texts.get(l)?.whats_new.trim().to_string();
            (!text.is_empty()).then(|| (stores::play_language(l).to_string(), text.chars().take(500).collect()))
        })
        .collect();
    state.set(JobState::Running(format!("Releasing to {track}…")));
    log.set(Vec::new());

    ws.spawn(async move {
        let mut push = move |msg: String| log.write().push(msg);
        let result: Result<u32, String> = async {
            push("🔑 Authenticating with Google…".into());
            let (token, package) = play_auth(&dir, &p.android_bundle_id).await?;
            let edit_id = google_play_create_edit(&token, &package).await?;

            let released: Result<u32, String> = async {
                push(format!("⬆  Uploading {}…", aab.summary()));
                let code = stores::play_upload_bundle(&token, &package, &edit_id, &aab.path).await?;
                push(format!("✅ Bundle accepted — versionCode {code}"));
                push(format!("🚦 Setting {track} track to {version} ({code}), status {status}…"));
                stores::play_update_track(&token, &package, &edit_id, stores::TrackRelease {
                    track: &track,
                    name: format!("{version} ({code})"),
                    version_code: code,
                    status: &status,
                    notes: &notes,
                })
                .await?;
                push("💾 Committing edit…".into());
                google_play_commit_edit(&token, &package, &edit_id).await?;
                Ok(code)
            }
            .await;
            if released.is_err() {
                let _ = google_play_delete_edit(&token, &package, &edit_id).await;
            }
            released
        }
        .await;

        match result {
            Ok(code) => {
                record_release(ws, "android", &version, code, &format!("Released to {track} ({status})"));
                state.set(JobState::Ok(format!("{version} ({code}) on {track}")));
            }
            Err(e) => state.set(JobState::Failed(e)),
        }
    });
}
