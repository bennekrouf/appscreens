//! One component per workspace step. Each reads and writes the shared
//! [`Ws`] context; anything long-running goes through [`jobs`].

use super::*;

// ---------------------------------------------------------------------------
// Small shared pieces
// ---------------------------------------------------------------------------

/// A text input bound to one `ProjectState` field, saved on every keystroke.
fn project_field(
    ws: Ws,
    label: &'static str,
    placeholder: &'static str,
    hint: &'static str,
    get: fn(&ProjectState) -> &String,
    set: fn(&mut ProjectState, String),
) -> Element {
    let value = get(&ws.proj.read()).clone();
    rsx! {
        div { class: "build-config-field",
            label { class: "build-config-label", "{label}" }
            input {
                class: "text-input",
                placeholder: "{placeholder}",
                value: "{value}",
                oninput: move |e: Event<FormData>| ws.update(|p| set(p, e.value())),
            }
            p { class: "settings-hint", "{hint}" }
        }
    }
}

/// One line of a readiness checklist. `fix` names the step that resolves it.
fn check_row(ws: Ws, ok: bool, label: &str, detail: String, fix: Option<Step>) -> Element {
    rsx! {
        li { class: if ok { "check-row check-ok" } else { "check-row check-bad" },
            span { class: "check-mark",
                if ok { {icon_check()} } else { {icon_alert()} }
            }
            div { class: "check-text",
                div { class: "check-head",
                    span { class: "check-label", "{label}" }
                    if let (false, Some(step)) = (ok, fix) {
                        button {
                            class: "check-fix",
                            title: "Go to the {step.label()} step",
                            onclick: move |_| ws.go(step),
                            "{step.label()} →"
                        }
                    }
                }
                span { class: "check-detail", "{detail}" }
            }
        }
    }
}

fn job_status_line(state: JobState, idle_text: &str) -> Element {
    match state {
        JobState::Idle => rsx! { span { class: "publish-status publish-idle", "{idle_text}" } },
        JobState::Running(t) => rsx! {
            span { class: "spinner spinner-dark" }
            span { class: "publish-status publish-running", "{t}" }
        },
        JobState::Ok(t) => rsx! {
            span { class: "publish-status publish-success", {icon_check()} "{t}" }
        },
        JobState::Failed(e) => rsx! {
            span { class: "publish-status publish-error", {icon_alert()} "{e}" }
        },
    }
}

fn locale_name(code: &str) -> String {
    LOCALES
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, n)| n.to_string())
        .unwrap_or_else(|| code.to_string())
}

// ---------------------------------------------------------------------------
// 1 · App
// ---------------------------------------------------------------------------
#[component]
pub(super) fn AppStep() -> Element {
    let ws = use_context::<Ws>();
    let plat = ws.proj.read().platform_type.clone();

    rsx! {
        div { class: "card",
            h2 { "Identity" }
            div { class: "build-config-grid",
                {project_field(ws, "App Name", "Abjad", "Display name shown under the icon and in the stores",
                    |p| &p.app_name, |p, v| p.app_name = v)}
                {project_field(ws, "Project Slug", "abjad", "Lowercase dx slug, e.g. \"abjad\"",
                    |p| &p.project_slug, |p, v| p.project_slug = v)}
                if plat.has_ios() {
                    {project_field(ws, "iOS Bundle ID", "com.company.app", "Must match the app in App Store Connect",
                        |p| &p.ios_bundle_id, |p, v| p.ios_bundle_id = v)}
                }
                if plat.has_android() {
                    {project_field(ws, "Android Package", "com.company.app", "Must match the app in Google Play Console",
                        |p| &p.android_bundle_id, |p, v| p.android_bundle_id = v)}
                }
            }
        }

        div { class: "card",
            h2 { "Platforms" }
            div { class: "platform-selector",
                for option in [PlatformType::IosAndroid, PlatformType::Ios, PlatformType::Android, PlatformType::Desktop] {
                    button {
                        class: if plat == option { "platform-btn platform-btn-active" } else { "platform-btn" },
                        onclick: {
                            let option = option.clone();
                            move |_| ws.update(|p| {
                                p.platform_type = option.clone();
                                // Keep screenshot export targets in step with the platforms
                                p.export_ios = p.platform_type.has_ios();
                                p.export_android = p.platform_type.has_android();
                            })
                        },
                        "{option.label()}"
                    }
                }
            }
            p { class: "settings-hint platform-hint",
                if plat.has_desktop() {
                    "Desktop projects only use the screenshot steps — there is nothing to sign, build or submit here."
                } else {
                    "Decides which bundles you build, which screenshot sizes you export and which stores you submit to."
                }
            }
        }

        div { class: "card",
            h2 { "App icon" }
            p { class: "hint card-hint", "Copied into the project and written to assets/icon.png before every build." }
            div { class: "logo-upload-row",
                button { class: "btn", onclick: move |_| jobs::pick_logo(ws), "Choose Icon…" }
                if let Some(logo) = ws.proj.read().logo_path.clone() {
                    {
                        let logo_str = logo.to_string_lossy().to_string();
                        let thumb_src = format!("/localimg/{}", urlencoding::encode(&logo_str));
                        rsx! {
                            div { class: "logo-preview",
                                img { class: "logo-thumb", src: "{thumb_src}", alt: "Icon preview" }
                                div { class: "logo-info",
                                    span { class: "logo-path", title: "{logo_str}", "{logo_str}" }
                                    button {
                                        class: "btn-remove",
                                        title: "Remove icon",
                                        aria_label: "Remove icon",
                                        onclick: move |_| ws.update(|p| p.logo_path = None),
                                        {icon_close()}
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 2 · Accounts & signing
// ---------------------------------------------------------------------------

/// Signing identities from the login keychain, Distribution/Development only.
fn discover_identities() -> Vec<String> {
    std::process::Command::new("security")
        .args(["find-identity", "-v", "-p", "codesigning"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|out| {
            out.lines()
                .filter_map(|line| {
                    // Lines look like:  1) HASH "Identity Name (TEAMID)"
                    let q = line.find('"')?;
                    let rest = &line[q + 1..];
                    let end = rest.rfind('"')?;
                    Some(rest[..end].to_string())
                })
                .filter(|id| id.starts_with("Apple Distribution") || id.starts_with("Apple Development"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

// rfd has no Android backend, and iOS signing is meaningless there anyway.
#[cfg(not(target_os = "android"))]
fn profile_browse_button(ws: Ws) -> Element {
    rsx! {
        button {
            class: "btn settings-browse-btn",
            onclick: move |_| {
                ws.spawn(async move {
                    if let Some(f) = rfd::AsyncFileDialog::new()
                        .set_title("Select Provisioning Profile")
                        .add_filter("Provisioning Profile", &["mobileprovision"])
                        .pick_file()
                        .await
                    {
                        let path = f.path().to_string_lossy().to_string();
                        ws.update(|p| p.provisioning_profile = path);
                    }
                });
            },
            "Browse…"
        }
    }
}
#[cfg(target_os = "android")]
fn profile_browse_button(_ws: Ws) -> Element {
    rsx! {}
}

/// Connection-test button plus its latest result.
fn connection_test(state: JobState, on_test: impl FnMut(MouseEvent) + 'static) -> Element {
    let running = matches!(state, JobState::Running(_));
    rsx! {
        div { class: "conn-test",
            button {
                class: "btn btn-sm",
                disabled: running,
                onclick: on_test,
                if running {
                    span { class: "spinner spinner-dark" }
                    " Testing…"
                } else {
                    "Test connection"
                }
            }
            div { class: "publish-status-row", {job_status_line(state, "Not tested yet")} }
        }
    }
}

#[component]
pub(super) fn AccountsStep() -> Element {
    let ws = use_context::<Ws>();
    let mut settings = ws.settings;
    let mut refresh = ws.refresh;
    let p = ws.proj.read().clone();
    let plat = p.platform_type.clone();

    // .env files and profiles may have changed since the last look.
    use_effect(move || refresh.with_mut(|n| *n += 1));

    let mut identities = use_signal(Vec::<String>::new);
    let mut profiles = use_signal(Vec::<ProfileInfo>::new);
    use_effect(move || {
        spawn(async move {
            identities.set(tokio::task::spawn_blocking(discover_identities).await.unwrap_or_default());
            profiles.set(tokio::task::spawn_blocking(discover_profiles).await.unwrap_or_default());
        });
    });

    let identity = settings.read().apple_identity.clone();
    let bundle_id = p.ios_bundle_id.trim().to_string();
    let creds = ws.creds.read().clone();
    let selected = ws.profile.read().clone();

    // Profiles for this app first, then App Store ones, then by name.
    let mut sorted = profiles.read().clone();
    sorted.sort_by(|a, b| {
        b.matches_bundle(&bundle_id)
            .cmp(&a.matches_bundle(&bundle_id))
            .then((b.kind == ProfileKind::AppStore).cmp(&(a.kind == ProfileKind::AppStore)))
            .then(a.name.cmp(&b.name))
    });

    rsx! {
        if plat.has_ios() {
            div { class: "card",
                h2 { "Apple" }

                div { class: "settings-field",
                    label { "Signing identity" }
                    if identities.read().is_empty() {
                        input {
                            class: "text-input",
                            placeholder: "Apple Distribution: Name (TEAMID)",
                            value: "{identity}",
                            oninput: move |e: Event<FormData>| { settings.write().apple_identity = e.value(); save_settings(&settings()); },
                        }
                        p { class: "settings-hint", "No signing identities found in the keychain." }
                    } else {
                        select {
                            class: "text-input",
                            value: "{identity}",
                            onchange: move |e: Event<FormData>| { settings.write().apple_identity = e.value(); save_settings(&settings()); },
                            if identity.is_empty() {
                                option { value: "", disabled: true, selected: true, "— choose identity —" }
                            }
                            for id in identities.read().iter() {
                                option { value: "{id}", selected: *id == identity, "{id}" }
                            }
                        }
                        p { class: "settings-hint", "From the keychain · shared by every project on this Mac" }
                    }
                }

                div { class: "settings-field",
                    label { "Provisioning profile" }
                    if sorted.is_empty() {
                        div { class: "settings-path-row",
                            input {
                                class: "text-input",
                                placeholder: "/Users/you/Downloads/YourApp.mobileprovision",
                                value: "{p.provisioning_profile}",
                                oninput: move |e: Event<FormData>| ws.update(|p| p.provisioning_profile = e.value()),
                            }
                            {profile_browse_button(ws)}
                        }
                        p { class: "settings-hint", "No profiles found in ~/Library/MobileDevice/Provisioning Profiles" }
                    } else {
                        div { class: "settings-path-row",
                            select {
                                class: "text-input",
                                value: "{p.provisioning_profile}",
                                onchange: move |e: Event<FormData>| ws.update(|p| p.provisioning_profile = e.value()),
                                if p.provisioning_profile.is_empty() {
                                    option { value: "", disabled: true, selected: true, "— choose profile —" }
                                }
                                // A browsed-to profile outside the system folder stays selectable.
                                if !p.provisioning_profile.is_empty() && !sorted.iter().any(|i| i.path == p.provisioning_profile) {
                                    option { value: "{p.provisioning_profile}", selected: true, "{p.provisioning_profile}" }
                                }
                                for info in sorted.iter() {
                                    option {
                                        value: "{info.path}",
                                        selected: info.path == p.provisioning_profile,
                                        if info.matches_bundle(&bundle_id) {
                                            "{info.name} · {info.kind.label()} · {info.app_id}"
                                        } else {
                                            "{info.name} · {info.kind.label()} · {info.app_id} (other app)"
                                        }
                                    }
                                }
                            }
                            {profile_browse_button(ws)}
                        }
                        p { class: "settings-hint", "Per project — profiles belong to one bundle ID. Ones matching this app are listed first." }
                    }
                }

                if let Some(info) = selected.as_ref() {
                    ul { class: "check-list",
                        for c in profile_checks(info, &bundle_id, &identity) {
                            {check_row(ws, c.ok, c.label, c.detail, None)}
                        }
                    }
                } else if !p.provisioning_profile.trim().is_empty() {
                    ul { class: "check-list",
                        {check_row(ws, false, "Profile readable", format!("Cannot read {}", p.provisioning_profile), None)}
                    }
                }

                p { class: "export-section-label", "App Store Connect API key" }
                ul { class: "check-list",
                    for c in creds.app_store.iter().cloned() {
                        {check_row(ws, c.ok, c.label, c.detail, None)}
                    }
                }
                {connection_test(ws.asc_test.read().clone(), move |_| jobs::test_app_store(ws))}
                p { class: "settings-hint",
                    "Set the keys in the project's .env or fastlane/.env. Create the API key under Users and Access → Integrations in App Store Connect."
                }
            }
        }

        if plat.has_android() {
            div { class: "card",
                h2 { "Google Play" }
                ul { class: "check-list",
                    for c in creds.play.iter().cloned() {
                        {check_row(ws, c.ok, c.label, c.detail, None)}
                    }
                }
                {connection_test(ws.play_test.read().clone(), move |_| jobs::test_google_play(ws))}
                p { class: "settings-hint",
                    "Set GOOGLE_PLAY_JSON_KEY to the service account's JSON key file, in the project's .env or fastlane/.env. The account needs release access to this app in Play Console."
                }
            }
        }

        button {
            class: "btn",
            onclick: move |_| refresh.with_mut(|n| *n += 1),
            "Re-read .env and profiles"
        }
    }
}

// ---------------------------------------------------------------------------
// 3 · Version & build numbers
// ---------------------------------------------------------------------------

/// A build-number field. Numbers only go up in the stores, so lowering one
/// is allowed (you may know better) but flagged.
fn build_number_field(
    ws: Ws,
    label: &'static str,
    hint: &'static str,
    value: u32,
    set: fn(&mut ProjectState, u32),
) -> Element {
    rsx! {
        div { class: "settings-field",
            label { "{label}" }
            div { class: "number-row",
                input {
                    class: "text-input settings-short-input",
                    r#type: "number",
                    min: "1",
                    value: "{value}",
                    oninput: move |e: Event<FormData>| {
                        if let Ok(n) = e.value().trim().parse::<u32>() {
                            if n > 0 {
                                ws.update(|p| set(p, n));
                            }
                        }
                    },
                }
            }
            p { class: "settings-hint", "{hint}" }
        }
    }
}

#[component]
pub(super) fn VersionStep() -> Element {
    let ws = use_context::<Ws>();
    let p = ws.proj.read().clone();
    let plat = p.platform_type.clone();
    let version = p.version.clone();
    let valid = valid_version(version.trim());

    rsx! {
        div { class: "card",
            h2 { "Version" }
            div { class: "settings-field",
                div { class: "number-row",
                    input {
                        class: if valid { "text-input settings-short-input" } else { "text-input settings-short-input input-invalid" },
                        placeholder: "1.0.0",
                        value: "{version}",
                        oninput: move |e: Event<FormData>| ws.update(|p| p.version = e.value()),
                    }
                    if valid {
                        for (i, name) in ["Major", "Minor", "Patch"].into_iter().enumerate() {
                            button {
                                class: "btn btn-sm",
                                title: "Bump to {bump_version(version.trim(), i)}",
                                onclick: {
                                    let next = bump_version(version.trim(), i);
                                    move |_| {
                                        let next = next.clone();
                                        ws.update(|p| p.version = next)
                                    }
                                },
                                {icon_plus()}
                                "{name}"
                            }
                        }
                    }
                }
                if valid {
                    p { class: "settings-hint",
                        "What users see in the stores. Used as "
                        if plat.has_ios() { "CFBundleShortVersionString and the App Store Connect version" }
                        if plat.has_ios() && plat.has_android() { ", and " }
                        if plat.has_android() { "Android versionName" }
                        "."
                    }
                } else {
                    p { class: "settings-hint hint-error", "Use one to three numbers separated by dots, e.g. 1.2 or 1.2.0." }
                }
            }
        }

        div { class: "card",
            h2 { "Next build numbers" }
            p { class: "hint card-hint", "Each upload needs a higher number than the last, even for the same version." }
            div { class: "build-config-grid",
                if plat.has_ios() {
                    {build_number_field(ws, "iOS build (CFBundleVersion)", "Used by the next Build iOS IPA",
                        p.ios_build_number, |p, n| p.ios_build_number = n)}
                }
                if plat.has_android() {
                    {build_number_field(ws, "Android versionCode", "Used by the next Build AAB",
                        p.android_version_code, |p, n| p.android_version_code = n)}
                }
            }
            label { class: "toggle-row",
                input {
                    r#type: "checkbox",
                    checked: p.auto_increment_build,
                    onchange: move |e: Event<FormData>| ws.update(|p| p.auto_increment_build = e.value() == "true"),
                }
                span { "Increase the build number after each successful release build" }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 4 · Build
// ---------------------------------------------------------------------------
#[component]
pub(super) fn BuildStep() -> Element {
    let ws = use_context::<Ws>();
    let p = ws.proj.read().clone();
    let plat = p.platform_type.clone();
    let artifacts = ws.artifacts.read().clone();
    let running = matches!(*ws.build_phase.read(), BuildPhase::Running(_));
    let running_script = match &*ws.build_phase.read() {
        BuildPhase::Running(name) => name.clone(),
        _ => String::new(),
    };

    let artifact_line = |a: &Option<Artifact>, what: &str| match a {
        Some(a) => rsx! { p { class: "artifact-line", title: "{a.path.display()}", {icon_check()} "{a.summary()}" } },
        None => rsx! { p { class: "artifact-line artifact-none", "No {what} built yet" } },
    };
    let next_line = |label: String| rsx! {
        div { class: "next-build",
            span { "{label}" }
            button { class: "btn btn-sm", onclick: move |_| ws.go(Step::Version), "Change" }
        }
    };

    rsx! {
        div { class: "card build-status-card",
            div { class: "publish-status-row",
                {job_status_line(ws.job_state(JobKind::Build), "Ready — build scripts are regenerated in the project folder before each build.")}
            }
            if !ws.build_log.read().is_empty() {
                button { class: "btn btn-sm", onclick: move |_| ws.show_job(JobKind::Build), "Show log" }
            }
        }

        div { class: "platform-cards",
            if plat.has_ios() {
                div { class: "card platform-card",
                    h2 { class: "build-ios-title", {icon_phone()} "iOS" }
                    {next_line(format!("Next: {} ({})", p.version.trim(), p.ios_build_number))}
                    {artifact_line(&artifacts.ipa, "IPA")}
                    button {
                        class: "btn btn-build btn-build-ios",
                        disabled: running,
                        onclick: move |_| jobs::run_build(ws, "build_ios_distribution.sh".into()),
                        if running_script == "build_ios_distribution.sh" {
                            span { class: "spinner" }
                            " Building IPA…"
                        } else {
                            {icon_phone()}
                            "Build iOS IPA"
                        }
                    }
                }
            }

            if plat.has_android() {
                div { class: "card platform-card",
                    h2 { class: "build-android-title", {icon_package()} "Android" }
                    {next_line(format!("Next: {} ({})", p.version.trim(), p.android_version_code))}
                    {artifact_line(&artifacts.aab, "AAB")}
                    button {
                        class: "btn btn-build btn-build-android",
                        disabled: running,
                        onclick: move |_| jobs::run_build(ws, "build_android_release.sh".into()),
                        if running_script == "build_android_release.sh" {
                            span { class: "spinner" }
                            " Building AAB…"
                        } else {
                            {icon_package()}
                            "Build AAB (Google Play)"
                        }
                    }
                    {artifact_line(&artifacts.apk, "APK")}
                    button {
                        class: "btn btn-build btn-build-android-secondary",
                        disabled: running,
                        onclick: move |_| jobs::run_build(ws, "build_apk.sh".into()),
                        if running_script == "build_apk.sh" {
                            span { class: "spinner spinner-dark" }
                            " Building APK…"
                        } else {
                            {icon_download()}
                            "Build APK (test device)"
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 5 · Languages & source screens
// ---------------------------------------------------------------------------
#[component]
pub(super) fn LanguagesStep() -> Element {
    let ws = use_context::<Ws>();
    let proj = ws.proj;
    let mut active_locale = ws.active_locale;
    let mut show_lang_picker = use_signal(|| false);

    let locales = proj.read().locales.clone();
    let active = active_locale.read().clone();
    let active_sources = proj.read().sources_for(&active);

    rsx! {
        div { class: "card card-screenshots",
            // ── Tab bar: one tab per language ────────────────────────────
            div { class: "lang-tab-bar",
                for loc in locales.iter().cloned() {
                    {
                        let display_name = locale_name(&loc);
                        let count = proj.read().sources_for(&loc).len();
                        let is_active = active == loc;
                        let is_default = loc == "en-US";
                        rsx! {
                            div {
                                class: if is_active { "lang-tab lang-tab-active" } else { "lang-tab" },
                                onclick: {
                                    let loc = loc.clone();
                                    move |_| {
                                        show_lang_picker.set(false);
                                        active_locale.set(loc.clone());
                                    }
                                },
                                span { class: "lang-tab-name", "{display_name}" }
                                span { class: if count == 0 { "tab-badge tab-badge-empty" } else { "tab-badge" }, "{count}" }
                                // × to remove non-default languages
                                if !is_default {
                                    button {
                                        class: "lang-tab-remove",
                                        title: "Remove language",
                                        aria_label: "Remove {display_name}",
                                        onclick: {
                                            let loc = loc.clone();
                                            move |e: MouseEvent| {
                                                e.stop_propagation();
                                                ws.update(|p| {
                                                    p.locales.retain(|l| l != &loc);
                                                    if let Some(first) = p.locales.first() {
                                                        p.locale = first.clone();
                                                    }
                                                });
                                                if *active_locale.peek() == loc {
                                                    let first = proj.peek().locales.first().cloned().unwrap_or_else(|| "en-US".into());
                                                    active_locale.set(first);
                                                }
                                            }
                                        },
                                        {icon_close()}
                                    }
                                }
                            }
                        }
                    }
                }

                // ── + button with language picker dropdown ──────────────
                div { class: "lang-tab-add-wrapper",
                    button {
                        class: if *show_lang_picker.read() { "lang-tab-add lang-tab-add-open" } else { "lang-tab-add" },
                        title: "Add language",
                        aria_label: "Add language",
                        onclick: move |_| show_lang_picker.toggle(),
                        {icon_plus()}
                    }
                    if *show_lang_picker.read() {
                        // Transparent full-screen backdrop — closes the picker on outside click
                        div {
                            class: "lang-picker-backdrop",
                            onclick: move |_| show_lang_picker.set(false),
                        }
                        div { class: "lang-picker-dropdown",
                            for (code, name) in LOCALES.iter().copied().filter(|(c, _)| !locales.iter().any(|l| l == c)) {
                                button {
                                    class: "lang-picker-item",
                                    onclick: move |_| {
                                        ws.update(|p| {
                                            if !p.locales.iter().any(|l| l == code) {
                                                p.locales.push(code.to_string());
                                                p.locale_sources.entry(code.to_string()).or_insert_with(Vec::new);
                                                p.ensure_texts_len(code, 0);
                                            }
                                        });
                                        active_locale.set(code.to_string());
                                        show_lang_picker.set(false);
                                    },
                                    span { class: "lang-picker-name", "{name}" }
                                    span { class: "lang-picker-code", "{code}" }
                                }
                            }
                            if locales.len() == LOCALES.len() {
                                p { class: "lang-picker-empty", "All languages added" }
                            }
                        }
                    }
                }

                // ── Right side: image picker ─────────────────────────────
                div { class: "lang-tab-bar-right",
                    div { class: "lang-tab-bar-right-inner",
                        div { class: "lang-tab-images-row",
                            button { class: "btn btn-sm", onclick: move |_| jobs::pick_sources(ws), "Add Images…" }
                        }
                        p { class: "lang-tab-images-hint", "Images here belong to {locale_name(&active)} only." }
                    }
                }
            }

            // ── Source images + captions for the active language ─────────
            if active_sources.is_empty() {
                p { class: "source-empty",
                    "Add the raw app screenshots for {locale_name(&active)} to get started. Titles and descriptions become the captions on manual screenshots."
                }
            } else {
                div { class: "source-list",
                    for (idx, path) in active_sources.iter().cloned().enumerate() {
                        {
                            let path_str = path.to_string_lossy().to_string();
                            let thumb_src = format!("/localimg/{}", urlencoding::encode(&path_str));
                            let (title_val, sub_val) = proj.read().locale_texts
                                .get(&active)
                                .and_then(|v| v.get(idx))
                                .cloned()
                                .unwrap_or_default();
                            let loc_t = active.clone();
                            let loc_s = active.clone();
                            let loc_r = active.clone();
                            rsx! {
                                div { class: "source-item",
                                    img { class: "source-thumb", src: "{thumb_src}", alt: "Source screenshot {idx + 1}" }
                                    div { class: "source-item-right",
                                        div { class: "source-info",
                                            span { class: "source-index", "{idx + 1}." }
                                            span { class: "source-path", title: "{path_str}", "{path_str}" }
                                        }
                                        div { class: "manual-inputs",
                                            input {
                                                class: "text-input small-input",
                                                placeholder: "Title (e.g. Welcome)",
                                                value: "{title_val}",
                                                oninput: move |e: Event<FormData>| ws.update(|p| {
                                                    p.ensure_texts_len(&loc_t, idx + 1);
                                                    p.locale_texts.get_mut(&loc_t).unwrap()[idx].0 = e.value();
                                                    // Keep legacy manual_texts in sync for the first locale
                                                    if p.locales.first() == Some(&loc_t) && idx < p.manual_texts.len() {
                                                        p.manual_texts[idx].0 = e.value();
                                                    }
                                                }),
                                            }
                                            input {
                                                class: "text-input small-input",
                                                placeholder: "Short description…",
                                                value: "{sub_val}",
                                                oninput: move |e: Event<FormData>| ws.update(|p| {
                                                    p.ensure_texts_len(&loc_s, idx + 1);
                                                    p.locale_texts.get_mut(&loc_s).unwrap()[idx].1 = e.value();
                                                    if p.locales.first() == Some(&loc_s) && idx < p.manual_texts.len() {
                                                        p.manual_texts[idx].1 = e.value();
                                                    }
                                                }),
                                            }
                                        }
                                    }
                                    button {
                                        class: "btn-remove",
                                        title: "Remove image",
                                        aria_label: "Remove image {idx + 1}",
                                        onclick: move |_| ws.update(|p| {
                                            if let Some(srcs) = p.locale_sources.get_mut(&loc_r) {
                                                if idx < srcs.len() { srcs.remove(idx); }
                                            }
                                            if idx < p.manual_texts.len() { p.manual_texts.remove(idx); }
                                            if let Some(v) = p.locale_texts.get_mut(&loc_r) {
                                                if idx < v.len() { v.remove(idx); }
                                            }
                                        }),
                                        {icon_close()}
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 6 · Screenshots
// ---------------------------------------------------------------------------

/// Checkbox list for one platform's export sizes.
fn target_checks(
    ws: Ws,
    prefix: &'static str,
    targets: &'static [(&'static str, &'static str, u32, u32)],
    enabled: Vec<bool>,
    set: fn(&mut ProjectState, usize, bool),
) -> Element {
    rsx! {
        div { class: "export-targets",
            for (ti, &(_, tname, tw, th)) in targets.iter().enumerate() {
                div { class: "export-target-row",
                    input {
                        r#type: "checkbox",
                        id: "chk-{prefix}-{ti}",
                        checked: enabled.get(ti).copied().unwrap_or(true),
                        onchange: move |e: Event<FormData>| ws.update(|p| set(p, ti, e.value() == "true")),
                    }
                    label { r#for: "chk-{prefix}-{ti}", class: "export-target-label",
                        span { class: "export-target-name", "{tname}" }
                        span { class: "export-target-dim", "{tw}×{th}" }
                    }
                }
            }
        }
    }
}

/// Colour swatch + hex field bound to one `ProjectState` colour.
fn color_field(
    ws: Ws,
    label: &'static str,
    placeholder: &'static str,
    get: fn(&ProjectState) -> &String,
    set: fn(&mut ProjectState, String),
) -> Element {
    let value = get(&ws.proj.read()).clone();
    rsx! {
        div { class: "color-field",
            label { "{label}" }
            div { class: "color-picker-row",
                input {
                    r#type: "color",
                    value: "{value}",
                    oninput: move |e: Event<FormData>| ws.update(|p| set(p, e.value())),
                }
                input {
                    r#type: "text",
                    class: "hex-input",
                    value: "{value}",
                    placeholder: "{placeholder}",
                    maxlength: 7,
                    oninput: move |e: Event<FormData>| {
                        let v = e.value();
                        let hex = if v.starts_with('#') { v } else { format!("#{v}") };
                        if hex.len() == 7 && hex[1..].chars().all(|c| c.is_ascii_hexdigit()) {
                            ws.update(|p| set(p, hex));
                        }
                    },
                }
            }
        }
    }
}

#[component]
pub(super) fn ScreenshotsStep() -> Element {
    let ws = use_context::<Ws>();
    let mut gen_phase = ws.gen_phase;
    let p = ws.proj.read().clone();
    let plat = p.platform_type.clone();

    let phase = gen_phase.read().clone();
    let is_busy = matches!(phase, AppPhase::GeneratingAi | AppPhase::GeneratingManual | AppPhase::Resizing);
    let missing: Vec<String> = p.locales.iter().filter(|l| p.sources_for(l).is_empty()).map(|l| locale_name(l)).collect();

    rsx! {
        if !missing.is_empty() {
            div { class: "card notice-card",
                p { {icon_alert()} "No source images yet for: {missing.join(\", \")}." }
                button { class: "btn btn-sm", onclick: move |_| ws.go(Step::Languages), "Go to Languages" }
            }
        }

        // ---- Sizes ----
        div { class: "card export-settings-card",
            h2 { "Sizes" }
            div { class: "export-settings-grid",
                if plat.has_desktop() {
                    div { class: "export-section",
                        div { class: "export-platform-header",
                            span { class: "export-platform-label", {icon_monitor()} "Desktop" }
                        }
                        {target_checks(ws, "desktop", DESKTOP_TARGETS, p.desktop_targets.clone(), |p, i, v| {
                            if let Some(t) = p.desktop_targets.get_mut(i) { *t = v; }
                        })}
                        p { class: "settings-hint", "Run your desktop app, take screenshots, and add them in the Languages step." }
                    }
                }

                if plat.has_ios() || p.export_ios {
                    div { class: "export-section",
                        div { class: "export-platform-header",
                            input {
                                r#type: "checkbox",
                                id: "chk-ios",
                                checked: p.export_ios,
                                onchange: move |e: Event<FormData>| ws.update(|p| p.export_ios = e.value() == "true"),
                            }
                            label { r#for: "chk-ios", class: "export-platform-label export-ios-label", "iOS" }
                        }
                        if p.export_ios {
                            {target_checks(ws, "ios", IOS_TARGETS, p.ios_targets.clone(), |p, i, v| {
                                if let Some(t) = p.ios_targets.get_mut(i) { *t = v; }
                            })}
                        }
                    }
                }

                if plat.has_android() || p.export_android {
                    div { class: "export-section",
                        div { class: "export-platform-header",
                            input {
                                r#type: "checkbox",
                                id: "chk-android",
                                checked: p.export_android,
                                onchange: move |e: Event<FormData>| ws.update(|p| p.export_android = e.value() == "true"),
                            }
                            label { r#for: "chk-android", class: "export-platform-label export-android-label", "Android" }
                        }
                        if p.export_android {
                            {target_checks(ws, "android", ANDROID_TARGETS, p.android_targets.clone(), |p, i, v| {
                                if let Some(t) = p.android_targets.get_mut(i) { *t = v; }
                            })}
                        }
                    }
                }
            }
        }

        // ---- Style ----
        div { class: "style-cards",
            div { class: "card",
                h2 { "Manual style" }
                p { class: "hint card-hint", "Captions from the Languages step on your colours. Screens alternate between the two." }
                div { class: "color-inputs",
                    {color_field(ws, "Primary", "#3B82F6", |p| &p.primary_color, |p, v| p.primary_color = v)}
                    {color_field(ws, "Secondary", "#FFFFFF", |p| &p.secondary_color, |p, v| p.secondary_color = v)}
                }
            }

            div { class: "card",
                h2 { "AI style" }
                p { class: "hint card-hint", "Describe the background; fal.ai draws a device frame and your screenshot goes inside it." }
                textarea {
                    class: "text-input theme-textarea",
                    placeholder: "e.g. dark cyberpunk neon interface with glitch effects…",
                    value: "{p.theme_prompt}",
                    rows: "3",
                    oninput: move |e: Event<FormData>| ws.update(|p| p.theme_prompt = e.value()),
                }
                if !p.theme_history.is_empty() {
                    div { class: "theme-history",
                        p { class: "theme-history-label", "Recent:" }
                        div { class: "theme-chips",
                            for entry in p.theme_history.iter().cloned() {
                                button {
                                    class: "theme-chip",
                                    title: "{entry}",
                                    onclick: {
                                        let entry = entry.clone();
                                        move |_| ws.update(|p| p.theme_prompt = entry.clone())
                                    },
                                    if entry.chars().count() > 60 {
                                        "{entry.chars().take(57).collect::<String>()}…"
                                    } else {
                                        "{entry}"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // ---- Generate ----
        div { class: "generate-row",
            button {
                class: "btn btn-generate btn-manual",
                disabled: is_busy,
                onclick: move |_| jobs::generate_manual(ws),
                if matches!(phase, AppPhase::GeneratingManual) {
                    span { class: "spinner spinner-dark" }
                    " Generating…"
                } else {
                    {icon_image()}
                    "Generate Manual"
                }
            }
            button {
                class: "btn btn-primary btn-generate",
                disabled: is_busy,
                onclick: move |_| jobs::generate_ai(ws),
                if matches!(phase, AppPhase::GeneratingAi | AppPhase::Resizing) {
                    span { class: "spinner" }
                    " Generating AI…"
                } else {
                    {icon_sparkle()}
                    "Generate with AI"
                }
            }
        }

        if let AppPhase::Error(msg) = &phase {
            div { class: "card error-card", role: "alert",
                p { {icon_alert()} "{msg}" }
                button { class: "btn btn-dismiss", onclick: move |_| gen_phase.set(AppPhase::Idle), "Dismiss" }
            }
        }

        // ---- Results ----
        div { class: "card",
            h2 { "Results" }
            if p.generated_urls.is_empty() {
                p { class: "output-empty", "Generated screenshots will appear here." }
            } else {
                div { class: "preview-grid",
                    for (i, url) in p.generated_urls.iter().cloned().enumerate() {
                        div { class: "preview-item",
                            p { class: "preview-label", "Screen {i + 1}" }
                            img { class: "preview-img", src: "{url}", alt: "Generated screenshot {i + 1}" }
                        }
                    }
                }
            }
            if !p.output_paths.is_empty() {
                details { class: "saved-files",
                    summary { "{p.output_paths.len()} saved files" }
                    for (label, path) in p.output_paths.iter().cloned() {
                        div { class: "output-row",
                            span { class: "output-label", "{label}" }
                            span { class: "output-path", "{path.display()}" }
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 7 · Store text
// ---------------------------------------------------------------------------

/// A store-text field for the active language, with the store's length limit.
#[allow(clippy::too_many_arguments)]
fn store_text_field(
    ws: Ws,
    locale: String,
    label: &'static str,
    hint: &'static str,
    limit: usize,
    rows: u32,
    get: fn(&StoreText) -> &String,
    set: fn(&mut StoreText, String),
) -> Element {
    let value = ws.proj.read().store_texts.get(&locale).map(|t| get(t).clone()).unwrap_or_default();
    let used = value.chars().count();
    rsx! {
        div { class: "settings-field",
            div { class: "field-head",
                label { "{label}" }
                span { class: if used > limit { "char-count char-over" } else { "char-count" }, "{used}/{limit}" }
            }
            if rows > 1 {
                textarea {
                    class: "text-input",
                    rows: "{rows}",
                    value: "{value}",
                    oninput: move |e: Event<FormData>| {
                        let v = e.value();
                        ws.update(|p| set(p.store_texts.entry(locale.clone()).or_default(), v));
                    },
                }
            } else {
                input {
                    class: "text-input",
                    value: "{value}",
                    oninput: move |e: Event<FormData>| {
                        let v = e.value();
                        ws.update(|p| set(p.store_texts.entry(locale.clone()).or_default(), v));
                    },
                }
            }
            if !hint.is_empty() {
                p { class: "settings-hint", "{hint}" }
            }
        }
    }
}

#[component]
pub(super) fn MetadataStep() -> Element {
    let ws = use_context::<Ws>();
    let mut active_locale = ws.active_locale;
    let p = ws.proj.read().clone();
    let plat = p.platform_type.clone();
    let active = {
        let a = active_locale.read().clone();
        if p.locales.contains(&a) { a } else { p.locales.first().cloned().unwrap_or_else(|| "en-US".into()) }
    };
    let complete = |loc: &String| {
        p.store_texts.get(loc).is_some_and(|t| {
            !t.description.trim().is_empty() && (!plat.has_android() || !t.short_description.trim().is_empty())
        })
    };
    let first = p.locales.first().cloned().unwrap_or_default();
    let can_copy = active != first && p.store_texts.get(&first).is_some_and(|t| *t != StoreText::default());
    let title_too_long = p.app_name.chars().count() > 30;

    rsx! {
        div { class: "card card-screenshots",
            div { class: "lang-tab-bar",
                for loc in p.locales.iter().cloned() {
                    div {
                        class: if loc == active { "lang-tab lang-tab-active" } else { "lang-tab" },
                        onclick: {
                            let loc = loc.clone();
                            move |_| active_locale.set(loc.clone())
                        },
                        span { class: "lang-tab-name", "{locale_name(&loc)}" }
                        if complete(&loc) {
                            span { class: "tab-done", {icon_check()} }
                        }
                    }
                }
                div { class: "lang-tab-bar-right",
                    if can_copy {
                        button {
                            class: "btn btn-sm",
                            title: "Start from the {locale_name(&first)} text, then translate",
                            onclick: {
                                let (first, active) = (first.clone(), active.clone());
                                move |_| ws.update(|p| {
                                    if let Some(src) = p.store_texts.get(&first).cloned() {
                                        p.store_texts.insert(active.clone(), src);
                                    }
                                })
                            },
                            "Copy from {locale_name(&first)}"
                        }
                    }
                }
            }

            div { class: "meta-body",
                {store_text_field(ws, active.clone(), "Description", "Shown on the product page in both stores.", 4000, 8,
                    |t| &t.description, |t, v| t.description = v)}
                {store_text_field(ws, active.clone(), "What's new",
                    if plat.has_android() { "App Store release notes, and Play release notes (Play keeps the first 500 characters)." } else { "Release notes for this version." },
                    4000, 4, |t| &t.whats_new, |t, v| t.whats_new = v)}
            }
        }

        if plat.has_ios() {
            div { class: "card",
                h2 { "App Store" }
                {store_text_field(ws, active.clone(), "Keywords", "Comma-separated, no spaces needed. Not shown to users.", 100, 1,
                    |t| &t.keywords, |t, v| t.keywords = v)}
                {store_text_field(ws, active.clone(), "Promotional text", "Can be changed any time without a new version.", 170, 2,
                    |t| &t.promo_text, |t, v| t.promo_text = v)}
                div { class: "settings-field",
                    label { "Support URL" }
                    input {
                        class: "text-input",
                        placeholder: "https://example.com/support",
                        value: "{p.support_url}",
                        oninput: move |e: Event<FormData>| ws.update(|p| p.support_url = e.value()),
                    }
                    p { class: "settings-hint", "Same for every language. Required before App Review." }
                }
            }
        }

        if plat.has_android() {
            div { class: "card",
                h2 { "Google Play" }
                {store_text_field(ws, active.clone(), "Short description", "The one-liner under the app name.", 80, 1,
                    |t| &t.short_description, |t, v| t.short_description = v)}
                p { class: if title_too_long { "settings-hint hint-error" } else { "settings-hint" },
                    "The Play title is the App Name from the App step (\"{p.app_name}\", {p.app_name.chars().count()}/30)."
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 8 · Submit
// ---------------------------------------------------------------------------

/// One numbered stage inside a store card: title, what it does, status, action.
fn release_stage(title: &str, hint: String, state: JobState, idle: &str, body: Element) -> Element {
    rsx! {
        div { class: "release-stage",
            h3 { "{title}" }
            p { class: "settings-hint", "{hint}" }
            div { class: "publish-status-row", {job_status_line(state, idle)} }
            {body}
        }
    }
}

fn job_button(label: &str, running_label: &str, running: bool, disabled: bool, class: &str, onclick: impl FnMut(MouseEvent) + 'static) -> Element {
    rsx! {
        button {
            class: "{class}",
            disabled: running || disabled,
            onclick: onclick,
            if running {
                span { class: "spinner" }
                " {running_label}"
            } else {
                {icon_upload()}
                "{label}"
            }
        }
    }
}

#[component]
pub(super) fn SubmitStep() -> Element {
    let ws = use_context::<Ws>();
    let p = ws.proj.read().clone();
    let plat = p.platform_type.clone();
    let creds = ws.creds.read().clone();
    let artifacts = ws.artifacts.read().clone();
    let version = p.version.trim().to_string();

    let existing = |pred: fn(&str) -> bool| -> Vec<(String, PathBuf)> {
        p.output_paths.iter().filter(|(l, path)| pred(l) && path.exists()).cloned().collect()
    };
    let ios_shots = existing(|l| l.starts_with("iOS "));
    let ios_langs = p.locales.iter().filter(|loc| ios_shots.iter().any(|(l, _)| l.contains(&format!("[{loc}]")))).count();
    let android_shots = existing(|l| l.contains("Android Phone") || l.contains("Android Feature"));
    let described = |need_short: bool| {
        p.locales
            .iter()
            .filter(|l| p.store_texts.get(*l).is_some_and(|t| !t.description.trim().is_empty() && (!need_short || !t.short_description.trim().is_empty())))
            .count()
    };
    let running = |k: JobKind| matches!(ws.job_state(k), JobState::Running(_));
    let is_mac = cfg!(target_os = "macos");

    rsx! {
        div { class: "platform-cards",
            if plat.has_ios() {
                div { class: "card platform-card",
                    h2 { class: "build-ios-title", {icon_phone()} "App Store" }
                    ul { class: "check-list",
                        {check_row(ws, !p.ios_bundle_id.trim().is_empty() && creds.app_store_ok(), "Account",
                            if creds.app_store_ok() { p.ios_bundle_id.clone() } else { "API key missing or incomplete".into() }, Some(Step::Accounts))}
                        {check_row(ws, valid_version(&version), "Version",
                            if valid_version(&version) { version.clone() } else { "Not a valid version".into() }, Some(Step::Version))}
                        {check_row(ws, described(false) == p.locales.len() && !p.support_url.trim().is_empty(), "Store text",
                            format!("{} of {} languages described{}", described(false), p.locales.len(),
                                if p.support_url.trim().is_empty() { " · no support URL" } else { "" }), Some(Step::Metadata))}
                        {check_row(ws, !ios_shots.is_empty(), "Screenshots",
                            format!("{} files · {ios_langs} of {} languages", ios_shots.len(), p.locales.len()), Some(Step::Screenshots))}
                        {check_row(ws, artifacts.ipa.is_some(), "IPA",
                            artifacts.ipa.as_ref().map(|a| a.summary()).unwrap_or_else(|| "Not built yet".into()), Some(Step::Build))}
                    }

                    {release_stage("1 · Listing",
                        "Store text and screenshots for every language. Languages missing in App Store Connect are added.".into(),
                        ws.job_state(JobKind::AppStore), "Ready",
                        job_button("Upload listing", "Uploading…", running(JobKind::AppStore), false, "btn stage-btn",
                            move |_| jobs::publish_app_store(ws)))}

                    {release_stage("2 · Build",
                        if is_mac { "Uploads the newest IPA with xcrun altool, using the API key.".to_string() }
                        else { "IPA upload needs macOS (xcrun altool).".to_string() },
                        ws.job_state(JobKind::AppStoreBuild), "Ready",
                        job_button("Upload IPA", "Uploading IPA…", running(JobKind::AppStoreBuild), !is_mac || artifacts.ipa.is_none(), "btn stage-btn",
                            move |_| jobs::upload_ipa(ws)))}

                    {release_stage("3 · App Review",
                        "Waits for the uploaded build to finish processing, attaches it to the version and submits it.".into(),
                        ws.job_state(JobKind::AppStoreReview), "Ready",
                        rsx! {
                            label { class: "toggle-row",
                                input { r#type: "checkbox", checked: p.exempt_encryption, onchange: move |e: Event<FormData>| ws.update(|p| p.exempt_encryption = e.value() == "true") }
                                span { "The app uses only exempt encryption (HTTPS, OS crypto)" }
                            }
                            {job_button("Submit for review", "Submitting…", running(JobKind::AppStoreReview), false, "btn btn-publish",
                                move |_| jobs::submit_for_review(ws))}
                        })}
                }
            }

            if plat.has_android() {
                div { class: "card platform-card",
                    h2 { class: "build-android-title", {icon_play()} "Google Play" }
                    ul { class: "check-list",
                        {check_row(ws, !p.android_bundle_id.trim().is_empty() && creds.play_ok(), "Account",
                            if creds.play_ok() { p.android_bundle_id.clone() } else { "Service account missing".into() }, Some(Step::Accounts))}
                        {check_row(ws, described(true) == p.locales.len(), "Store text",
                            format!("{} of {} languages described", described(true), p.locales.len()), Some(Step::Metadata))}
                        {check_row(ws, !android_shots.is_empty(), "Screenshots",
                            format!("{} files", android_shots.len()), Some(Step::Screenshots))}
                        {check_row(ws, artifacts.aab.is_some(), "AAB",
                            artifacts.aab.as_ref().map(|a| a.summary()).unwrap_or_else(|| "Not built yet".into()), Some(Step::Build))}
                    }

                    {release_stage("1 · Listing",
                        "Store text, phone screenshots and feature graphic for every language.".into(),
                        ws.job_state(JobKind::GooglePlay), "Ready",
                        job_button("Upload listing", "Uploading…", running(JobKind::GooglePlay), false, "btn stage-btn",
                            move |_| jobs::publish_google_play(ws)))}

                    {release_stage("2 · Release",
                        "Uploads the newest AAB and puts it on a track, with What's new as release notes.".into(),
                        ws.job_state(JobKind::PlayRelease), "Ready",
                        rsx! {
                            div { class: "release-options",
                                select {
                                    class: "text-input",
                                    value: "{p.play_track}",
                                    onchange: move |e: Event<FormData>| ws.update(|p| p.play_track = e.value()),
                                    for (value, label) in [("internal", "Internal test"), ("alpha", "Closed test"), ("beta", "Open test"), ("production", "Production")] {
                                        option { value: "{value}", selected: p.play_track == value, "{label}" }
                                    }
                                }
                                select {
                                    class: "text-input",
                                    value: "{p.play_release_status}",
                                    onchange: move |e: Event<FormData>| ws.update(|p| p.play_release_status = e.value()),
                                    option { value: "draft", selected: p.play_release_status == "draft", "Draft" }
                                    option { value: "completed", selected: p.play_release_status == "completed", "Roll out" }
                                }
                            }
                            p { class: "settings-hint", "Apps that were never published only accept Draft." }
                            {job_button("Release AAB", "Releasing…", running(JobKind::PlayRelease), artifacts.aab.is_none(), "btn btn-publish btn-publish-android",
                                move |_| jobs::release_aab(ws))}
                        })}
                }
            }
        }

        div { class: "card",
            h2 { "History" }
            if p.releases.is_empty() {
                p { class: "output-empty", "Uploads and releases will be listed here." }
            } else {
                table { class: "history",
                    thead {
                        tr { th { "When (UTC)" } th { "Store" } th { "Version" } th { "What" } }
                    }
                    tbody {
                        for r in p.releases.iter().take(20) {
                            tr {
                                td { "{r.at}" }
                                td { if r.platform == "ios" { "App Store" } else { "Google Play" } }
                                td { if r.build > 0 { "{r.version} ({r.build})" } else { "{r.version}" } }
                                td { "{r.action}" }
                            }
                        }
                    }
                }
            }
        }
    }
}
