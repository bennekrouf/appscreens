//! 2 · Accounts & signing: store credentials, Apple signing (identity,
//! certificate, profile), the Android upload key, secrets tracked by git and
//! the encrypted backup of all signing material.

use super::steps::{check_row, job_status_line};
use super::*;

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

// rfd has no Android backend; there is nothing to sign there anyway.
#[cfg(not(target_os = "android"))]
async fn pick_file(title: &str, filter: (&str, &[&str])) -> Option<PathBuf> {
    rfd::AsyncFileDialog::new()
        .set_title(title)
        .add_filter(filter.0, filter.1)
        .pick_file()
        .await
        .map(|f| f.path().to_path_buf())
}
#[cfg(target_os = "android")]
async fn pick_file(_title: &str, _filter: (&str, &[&str])) -> Option<PathBuf> {
    None
}

#[cfg(not(target_os = "android"))]
async fn save_file(title: &str, name: &str) -> Option<PathBuf> {
    rfd::AsyncFileDialog::new()
        .set_title(title)
        .set_file_name(name)
        .save_file()
        .await
        .map(|f| f.path().to_path_buf())
}
#[cfg(target_os = "android")]
async fn save_file(_title: &str, _name: &str) -> Option<PathBuf> {
    None
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

/// A password field bound to a local signal.
fn password_input(mut value: Signal<String>, placeholder: &'static str) -> Element {
    rsx! {
        input {
            class: "text-input",
            r#type: "password",
            autocomplete: "new-password",
            placeholder: "{placeholder}",
            value: "{value}",
            oninput: move |e: Event<FormData>| value.set(e.value()),
        }
    }
}

#[component]
pub(super) fn AccountsStep() -> Element {
    let ws = use_context::<Ws>();
    let mut refresh = ws.refresh;
    let plat = ws.proj.read().platform_type.clone();

    // .env files, profiles, keys and git may have changed since the last look.
    use_effect(move || refresh.with_mut(|n| *n += 1));

    rsx! {
        if plat.has_ios() {
            AppleCard {}
        }
        if plat.has_android() {
            PlayCard {}
            UploadKeyCard {}
        }
        SecretsCard {}
        BackupCard {}
        LostMacCard {}

        button {
            class: "btn",
            onclick: move |_| refresh.with_mut(|n| *n += 1),
            "Check everything again"
        }
    }
}

// ---------------------------------------------------------------------------
// Apple
// ---------------------------------------------------------------------------
#[component]
fn AppleCard() -> Element {
    let ws = use_context::<Ws>();
    let mut settings = ws.settings;
    let p = ws.proj.read().clone();

    let mut identities = use_signal(Vec::<String>::new);
    let mut profiles = use_signal(Vec::<ProfileInfo>::new);
    use_effect(move || {
        ws.refresh.read(); // re-scan after a certificate or profile is created
        spawn(async move {
            identities.set(tokio::task::spawn_blocking(discover_identities).await.unwrap_or_default());
            profiles.set(tokio::task::spawn_blocking(discover_profiles).await.unwrap_or_default());
        });
    });

    let identity = settings.read().apple_identity.clone();
    let bundle_id = p.ios_bundle_id.trim().to_string();
    let creds = ws.creds.read().clone();
    let selected = ws.profile.read().clone();
    let profile_ok = selected.as_ref().is_some_and(|i| profile_checks(i, &bundle_id, &identity).iter().all(|c| c.ok));
    let has_distribution = identities.read().iter().any(|i| i.starts_with("Apple Distribution"));

    // Profiles for this app first, then App Store ones, then by name.
    let mut sorted = profiles.read().clone();
    sorted.sort_by(|a, b| {
        b.matches_bundle(&bundle_id)
            .cmp(&a.matches_bundle(&bundle_id))
            .then((b.kind == ProfileKind::AppStore).cmp(&(a.kind == ProfileKind::AppStore)))
            .then(a.name.cmp(&b.name))
    });

    // New-certificate form: the name defaults to the one in the identity.
    let default_name = identity
        .strip_prefix("Apple Distribution: ")
        .or_else(|| identity.strip_prefix("Apple Development: "))
        .and_then(|s| s.rsplit_once(" (").map(|(n, _)| n.to_string()))
        .unwrap_or_default();
    let mut cert_name = use_signal(|| default_name.clone());
    let mut cert_email = use_signal(String::new);
    let cert_pw = use_signal(String::new);
    let cert_pw2 = use_signal(String::new);
    let mut cert_error = use_signal(|| Option::<String>::None);
    let cert_state = ws.cert_job.read().clone();
    let profile_state = ws.profile_job.read().clone();
    let asc_ok = creds.app_store_ok();

    rsx! {
        div { class: "card",
            h2 { "Apple" }

            div { class: "settings-field",
                label { "Signing identity" }
                if identities.read().is_empty() {
                    p { class: "settings-hint hint-error", "No signing identity in the keychain — create a Distribution certificate below." }
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

            details { class: "inline-form", open: !has_distribution,
                summary { "Create a Distribution certificate" }
                p { class: "settings-hint",
                    "Makes a new private key on this Mac, asks Apple for the certificate, installs it in the keychain and keeps a password-protected .p12 backup in your keys folder. Needs the App Store Connect key below."
                }
                div { class: "build-config-grid",
                    div { class: "build-config-field",
                        label { class: "build-config-label", "Your name" }
                        input { class: "text-input", value: "{cert_name}", oninput: move |e| cert_name.set(e.value()) }
                    }
                    div { class: "build-config-field",
                        label { class: "build-config-label", "Apple ID email" }
                        input { class: "text-input", r#type: "email", value: "{cert_email}", oninput: move |e| cert_email.set(e.value()) }
                    }
                    div { class: "build-config-field",
                        label { class: "build-config-label", ".p12 backup password" }
                        {password_input(cert_pw, "At least 8 characters")}
                    }
                    div { class: "build-config-field",
                        label { class: "build-config-label", "Repeat password" }
                        {password_input(cert_pw2, "")}
                    }
                }
                if let Some(e) = cert_error() {
                    p { class: "settings-hint hint-error", "{e}" }
                }
                div { class: "conn-test",
                    button {
                        class: "btn btn-sm",
                        disabled: matches!(cert_state, JobState::Running(_)) || !asc_ok,
                        onclick: move |_| {
                            let (pw, pw2) = (cert_pw.peek().clone(), cert_pw2.peek().clone());
                            if cert_name.peek().trim().is_empty() || !cert_email.peek().contains('@') {
                                cert_error.set(Some("Fill in your name and Apple ID email.".into()));
                            } else if pw != pw2 {
                                cert_error.set(Some("The passwords don't match.".into()));
                            } else {
                                cert_error.set(None);
                                jobs::create_distribution_certificate(ws, cert_name.peek().trim().into(), cert_email.peek().trim().into(), pw);
                            }
                        },
                        "Create certificate"
                    }
                    div { class: "publish-status-row", {job_status_line(cert_state.clone(), if asc_ok { "" } else { "Set up the App Store Connect key first" })} }
                }
            }

            div { class: "settings-field",
                label { "Provisioning profile" }
                select {
                    class: "text-input",
                    value: "{p.provisioning_profile}",
                    onchange: move |e: Event<FormData>| ws.update(|p| p.provisioning_profile = e.value()),
                    if p.provisioning_profile.is_empty() {
                        option { value: "", disabled: true, selected: true, "— choose profile —" }
                    }
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
                div { class: "install-actions",
                    button {
                        class: "btn btn-sm",
                        onclick: move |_| {
                            spawn(async move {
                                let Some(path) = pick_file("Add a provisioning profile", ("Provisioning profile", &["mobileprovision"])).await else {
                                    return;
                                };
                                let mut state = ws.profile_job;
                                let installed = std::fs::read(&path).map_err(|e| e.to_string()).and_then(|b| install_profile(&b));
                                match installed.and_then(|p| read_profile(&p).map(|i| (p, i)).ok_or("Installed, but it can't be read".to_string())) {
                                    Ok((installed, info)) => {
                                        ws.update(|p| p.provisioning_profile = installed.to_string_lossy().to_string());
                                        let mut refresh = ws.refresh;
                                        refresh.with_mut(|n| *n += 1);
                                        state.set(JobState::Ok(format!("{} installed and selected", info.name)));
                                    }
                                    Err(e) => state.set(JobState::Failed(e)),
                                }
                            });
                        },
                        "Add profile…"
                    }
                    a { class: "btn btn-sm", href: "https://developer.apple.com/account/resources/profiles/list", target: "_blank", "Profiles on Apple Developer" }
                }
                p { class: "settings-hint",
                    "Per project — profiles belong to one bundle ID. Ones matching this app are listed first. Downloaded one from Apple Developer? Add it here: it's installed for Xcode and the build too."
                }
            }

            if let Some(info) = selected.as_ref() {
                ul { class: "check-list",
                    for c in profile_checks(info, &bundle_id, &identity) {
                        {check_row(ws, c.ok, c.label, c.detail, None)}
                    }
                }
            }
            if !profile_ok {
                div { class: "conn-test",
                    button {
                        class: "btn btn-sm",
                        disabled: matches!(profile_state, JobState::Running(_)) || !asc_ok,
                        onclick: move |_| jobs::repair_profile(ws),
                        "Regenerate App Store profile"
                    }
                    div { class: "publish-status-row",
                        {job_status_line(profile_state.clone(), "Creates a fresh profile for this app on your current certificate and selects it")}
                    }
                }
            }

            p { class: "export-section-label", "App Store Connect API key" }
            ul { class: "check-list",
                for c in creds.app_store.iter().cloned() {
                    {check_row(ws, c.ok, c.label, c.detail, None)}
                }
            }
            if asc_ok {
                details { class: "inline-form",
                    summary { "Use a different key" }
                    AscKeySetup {}
                }
            } else {
                AscKeySetup {}
            }
            {connection_test(ws.asc_test.read().clone(), move |_| jobs::test_app_store(ws))}
        }
    }
}

/// Get the three App Store Connect values into the project without hunting:
/// reuse another project's key, or walk through creating one with the page
/// links, picking up the downloaded .p8 and its Key ID automatically.
#[component]
fn AscKeySetup() -> Element {
    let ws = use_context::<Ws>();
    let mut others = use_signal(Vec::<asckey::AscKey>::new);
    let mut p8s = use_signal(Vec::<(PathBuf, String)>::new);
    let mut key_id = use_signal(String::new);
    let mut issuer = use_signal(String::new);
    let mut p8 = use_signal(|| Option::<PathBuf>::None);
    let mut state = use_signal(|| JobState::Idle);

    use_effect(move || {
        ws.refresh.read();
        let projects = ws.settings.peek().recent_projects.clone();
        let (dir, keys) = (ws.dir(), keys_dir(&ws.settings.peek()));
        spawn(async move {
            let (o, f) = tokio::task::spawn_blocking(move || (asckey::from_other_projects(&projects, &dir), asckey::find_p8s(&keys)))
                .await
                .unwrap_or_default();
            // The Issuer ID is the same for every key of the team.
            if issuer.peek().is_empty() {
                if let Some(k) = o.first() {
                    issuer.set(k.issuer_id.clone());
                }
            }
            // A single downloaded key is almost certainly the one just made.
            if p8.peek().is_none() && f.len() == 1 {
                key_id.set(f[0].1.clone());
                p8.set(Some(f[0].0.clone()));
            }
            others.set(o);
            p8s.set(f);
        });
    });

    // Save into the project's .env, then prove it works.
    let mut save = move |key: asckey::AscKey| {
        let dir = ws.dir();
        let keys = keys_dir(&ws.settings.peek());
        let result = asckey::keep_in_keys_dir(&key.p8, &key.key_id, &keys).and_then(|kept| {
            let path = kept.to_string_lossy().to_string();
            envfile::set_values(
                &dir,
                &[(asckey::KEY_ID_VAR, &key.key_id), (asckey::ISSUER_VAR, &key.issuer_id), (asckey::P8_VAR, &path)],
            )
            .map(|_| kept)
        });
        match result {
            Ok(kept) => {
                state.set(JobState::Ok(format!("Saved in the project's .env · key kept at {}", kept.display())));
                let mut refresh = ws.refresh;
                refresh.with_mut(|n| *n += 1);
                jobs::test_app_store(ws);
            }
            Err(e) => state.set(JobState::Failed(e)),
        }
    };

    let (kid, iss) = (key_id(), issuer());
    let problem = if p8().is_none() {
        Some("Choose the downloaded .p8 key")
    } else if !asckey::valid_key_id(kid.trim()) {
        Some("The Key ID is 10 capital letters and digits")
    } else if !asckey::valid_issuer(iss.trim()) {
        Some("The Issuer ID looks like 69a6de97-00e6-47e3-e053-5b8c7c11a4d1")
    } else {
        None
    };

    rsx! {
        if !others.read().is_empty() {
            p { class: "settings-hint", "One key works for every app of your team — this Mac already has one:" }
            for k in others.read().iter().cloned() {
                div { class: "install-option",
                    div { class: "check-text",
                        span { class: "check-label", "Key {k.key_id}" }
                        span { class: "check-detail", "Used by {k.from} · {k.p8.display()}" }
                    }
                    div { class: "install-actions",
                        button { class: "btn btn-sm", onclick: move |_| save(k.clone()), "Use this key" }
                    }
                }
            }
        }
        details { class: "inline-form", open: others.read().is_empty(),
            summary { if others.read().is_empty() { "Set up a key" } else { "Or create a new key" } }
            ol { class: "lost-steps",
                li {
                    "Open "
                    a { href: asckey::KEYS_PAGE, target: "_blank", "App Store Connect → Users and Access → Integrations" }
                    ". The first time, the Account Holder has to click Request Access."
                }
                li {
                    "Click + to generate a key. Name it AppScreens. Access: Admin lets AppScreens also create certificates and profiles; App Manager is enough for uploads and review."
                }
                li { "Download the key right away — Apple lets you download it only once. AppScreens keeps a copy in your keys folder." }
                li { "The Issuer ID is shown above the list of keys on the same page." }
            }
            div { class: "build-config-grid",
                div { class: "build-config-field",
                    label { class: "build-config-label", "Private key (.p8)" }
                    if p8s.read().len() > 1 {
                        select {
                            class: "text-input",
                            onchange: move |e: Event<FormData>| {
                                let v = e.value();
                                if let Some((path, id)) = p8s.peek().iter().find(|(p, _)| p.to_string_lossy() == v).cloned() {
                                    key_id.set(id);
                                    p8.set(Some(path));
                                }
                            },
                            option { value: "", selected: p8().is_none(), disabled: true, "— downloaded keys —" }
                            for (path, id) in p8s.read().iter() {
                                option { value: "{path.display()}", selected: p8().as_ref() == Some(path), "{id} · {path.display()}" }
                            }
                        }
                    } else if let Some(path) = p8() {
                        p { class: "settings-hint", "{path.display()}" }
                    }
                    button {
                        class: "btn btn-sm",
                        onclick: move |_| {
                            spawn(async move {
                                if let Some(path) = pick_file("Choose the App Store Connect key", ("App Store Connect key", &["p8"])).await {
                                    if let Some(id) = asckey::key_id_from_file(&path) {
                                        key_id.set(id);
                                    }
                                    p8.set(Some(path));
                                }
                            });
                        },
                        "Choose .p8…"
                    }
                }
                div { class: "build-config-field",
                    label { class: "build-config-label", "Key ID" }
                    input { class: "text-input", placeholder: "From the file name, e.g. 4TALSPNY5Y", value: "{kid}", oninput: move |e| key_id.set(e.value().trim().to_uppercase()) }
                }
                div { class: "build-config-field",
                    label { class: "build-config-label", "Issuer ID" }
                    input { class: "text-input", placeholder: "Above the keys list", value: "{iss}", oninput: move |e| issuer.set(e.value().trim().to_string()) }
                }
            }
            div { class: "conn-test",
                button {
                    class: "btn btn-sm",
                    disabled: problem.is_some(),
                    onclick: move |_| {
                        if let Some(path) = p8() {
                            save(asckey::AscKey { key_id: key_id().trim().into(), issuer_id: issuer().trim().into(), p8: path, from: String::new() });
                        }
                    },
                    "Save and test"
                }
                if let Some(pb) = problem {
                    span { class: "settings-hint", "{pb}" }
                }
            }
            p { class: "settings-hint",
                a { href: asckey::HELP_PAGE, target: "_blank", "Apple's guide to API keys" }
            }
        }
        div { class: "publish-status-row", {job_status_line(state(), "")} }
    }
}

// ---------------------------------------------------------------------------
// Google Play
// ---------------------------------------------------------------------------
#[component]
fn PlayCard() -> Element {
    let ws = use_context::<Ws>();
    let creds = ws.creds.read().clone();
    rsx! {
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
}

// ---------------------------------------------------------------------------
// Android upload key
// ---------------------------------------------------------------------------
#[component]
fn UploadKeyCard() -> Element {
    let ws = use_context::<Ws>();
    let mut settings = ws.settings;
    let mut refresh = ws.refresh;
    let p = ws.proj.read().clone();
    let android = ws.android.read().clone();
    let keystore_setting = settings.read().android_keystore_path.clone();
    let slug_alias = p.project_slug.trim().to_lowercase();

    let password = use_signal(String::new);
    let mut save_state = use_signal(|| JobState::Idle);

    let mut dn_name = use_signal(String::new);
    let mut dn_org = use_signal(String::new);
    let mut dn_country = use_signal(String::new);
    let new_pw = use_signal(String::new);
    let new_pw2 = use_signal(String::new);
    let mut create_state = use_signal(|| JobState::Idle);

    let keystore_path = android.keystore.clone();
    let has_keystore = keystore_path.as_ref().is_some_and(|k| k.is_file());

    rsx! {
        div { class: "card",
            h2 { "Android upload key" }
            p { class: "hint card-hint", "Signs the AABs you upload. One keystore for all your apps, one key (alias) per app." }

            div { class: "build-config-grid",
                div { class: "build-config-field",
                    label { class: "build-config-label", "Keystore (all projects)" }
                    div { class: "settings-path-row",
                        input {
                            class: "text-input",
                            placeholder: "~/keys/upload.jks",
                            value: "{keystore_setting}",
                            disabled: android.keystore_from_env,
                            oninput: move |e: Event<FormData>| { settings.write().android_keystore_path = e.value(); save_settings(&settings()); },
                        }
                        button {
                            class: "btn settings-browse-btn",
                            disabled: android.keystore_from_env,
                            onclick: move |_| {
                                spawn(async move {
                                    if let Some(path) = pick_file("Choose upload keystore", ("Keystore", &["jks", "keystore", "p12"])).await {
                                        settings.write().android_keystore_path = path.to_string_lossy().to_string();
                                        save_settings(&settings());
                                    }
                                });
                            },
                            "Browse…"
                        }
                    }
                    if android.keystore_from_env {
                        p { class: "settings-hint", "Set by ANDROID_KEYSTORE_PATH in this project's .env, which takes priority." }
                    }
                }
                div { class: "build-config-field",
                    label { class: "build-config-label", "Key alias (this app)" }
                    input {
                        class: "text-input",
                        placeholder: "{slug_alias}",
                        value: "{p.android_key_alias}",
                        oninput: move |e: Event<FormData>| ws.update(|p| p.android_key_alias = e.value()),
                    }
                }
            }

            ul { class: "check-list",
                for c in android.checks() {
                    {check_row(ws, c.ok, c.label, c.detail, None)}
                }
            }

            if has_keystore {
                div { class: "settings-field",
                    label { "Keystore password" }
                    div { class: "settings-path-row",
                        {password_input(password, "Checked against the keystore, then kept in the Keychain")}
                        button {
                            class: "btn settings-browse-btn",
                            disabled: matches!(save_state(), JobState::Running(_)),
                            onclick: move |_| {
                                let pw = password.peek().clone();
                                let ks = keystore_path.clone().unwrap_or_default();
                                let alias = ws.android.peek().alias.clone();
                                let mut password = password;
                                save_state.set(JobState::Running("Checking…".into()));
                                spawn(async move {
                                    let result = tokio::task::spawn_blocking(move || -> Result<String, String> {
                                        let aliases = signing::keystore_aliases(&ks, &pw)?;
                                        signing::keychain_set(&signing::keystore_account(&ks), &pw)?;
                                        Ok(if aliases.contains(&alias) {
                                            format!("Saved — the keystore has \"{alias}\"")
                                        } else {
                                            format!("Saved, but the keystore has no \"{alias}\" key ({}). Create one below or change the alias.", aliases.join(", "))
                                        })
                                    })
                                    .await
                                    .unwrap_or_else(|e| Err(e.to_string()));
                                    password.set(String::new());
                                    save_state.set(match result {
                                        Ok(m) => JobState::Ok(m),
                                        Err(e) => JobState::Failed(e),
                                    });
                                    refresh.with_mut(|n| *n += 1);
                                });
                            },
                            "Save to Keychain"
                        }
                    }
                    div { class: "publish-status-row", {job_status_line(save_state(), "")} }
                }
            }

            details { class: "inline-form", open: !has_keystore,
                summary { if has_keystore { "Add a key for this app" } else { "Create an upload keystore" } }
                p { class: "settings-hint",
                    if has_keystore {
                        "Adds the \"{android.alias}\" key to your keystore (use its password) and exports the certificate Play Console asks for."
                    } else {
                        "Creates the keystore in your keys folder with the \"{android.alias}\" key, keeps the password in the Keychain and exports the certificate Play Console asks for."
                    }
                }
                div { class: "build-config-grid",
                    div { class: "build-config-field",
                        label { class: "build-config-label", "Name" }
                        input { class: "text-input", value: "{dn_name}", oninput: move |e| dn_name.set(e.value()) }
                    }
                    div { class: "build-config-field",
                        label { class: "build-config-label", "Organisation" }
                        input { class: "text-input", value: "{dn_org}", oninput: move |e| dn_org.set(e.value()) }
                    }
                    div { class: "build-config-field",
                        label { class: "build-config-label", "Country code" }
                        input { class: "text-input settings-short-input", placeholder: "CH", maxlength: 2, value: "{dn_country}", oninput: move |e| dn_country.set(e.value().to_uppercase()) }
                    }
                    div { class: "build-config-field",
                        label { class: "build-config-label", "Keystore password" }
                        {password_input(new_pw, "At least 6 characters")}
                    }
                    div { class: "build-config-field",
                        label { class: "build-config-label", "Repeat password" }
                        {password_input(new_pw2, "")}
                    }
                }
                div { class: "conn-test",
                    button {
                        class: "btn btn-sm",
                        disabled: matches!(create_state(), JobState::Running(_)),
                        onclick: move |_| {
                            let (pw, pw2) = (new_pw.peek().clone(), new_pw2.peek().clone());
                            if dn_name.peek().trim().is_empty() {
                                create_state.set(JobState::Failed("Fill in a name.".into()));
                                return;
                            }
                            if pw != pw2 {
                                create_state.set(JobState::Failed("The passwords don't match.".into()));
                                return;
                            }
                            let ks = ws.android.peek().keystore.clone().unwrap_or_else(|| keys_dir(&settings.peek()).join("upload.jks"));
                            let alias = ws.android.peek().alias.clone();
                            let dn = signing::dname(&dn_name.peek(), &dn_org.peek(), &dn_country.peek());
                            let (mut new_pw, mut new_pw2) = (new_pw, new_pw2);
                            create_state.set(JobState::Running("Creating key…".into()));
                            spawn(async move {
                                let ks2 = ks.clone();
                                let result = tokio::task::spawn_blocking(move || -> Result<String, String> {
                                    let pem = signing::create_upload_key(&ks2, &alias, &pw, &dn)?;
                                    signing::keychain_set(&signing::keystore_account(&ks2), &pw)?;
                                    let fp = signing::upload_key_fingerprint(&ks2, &alias, &pw).unwrap_or_default();
                                    Ok(format!(
                                        "Created \"{alias}\" · SHA-256 {fp} · certificate: {} — for an app already on Play, upload it under App integrity → Request upload key reset; for a new app, the first upload registers it.",
                                        pem.display()
                                    ))
                                })
                                .await
                                .unwrap_or_else(|e| Err(e.to_string()));
                                new_pw.set(String::new());
                                new_pw2.set(String::new());
                                if result.is_ok() && settings.peek().android_keystore_path.trim().is_empty() {
                                    settings.write().android_keystore_path = ks.to_string_lossy().to_string();
                                    save_settings(&settings.peek());
                                }
                                create_state.set(match result {
                                    Ok(m) => JobState::Ok(m),
                                    Err(e) => JobState::Failed(e),
                                });
                                refresh.with_mut(|n| *n += 1);
                            });
                        },
                        if has_keystore { "Add key" } else { "Create keystore" }
                    }
                    div { class: "publish-status-row", {job_status_line(create_state(), "")} }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Secrets tracked by git
// ---------------------------------------------------------------------------
#[component]
fn SecretsCard() -> Element {
    let ws = use_context::<Ws>();
    let mut refresh = ws.refresh;
    let tracked = ws.tracked_secrets.read().clone();
    let mut state = use_signal(|| JobState::Idle);

    rsx! {
        div { class: "card",
            h2 { "Secrets in git" }
            if tracked.is_empty() {
                ul { class: "check-list",
                    {check_row(ws, true, "No secret files tracked", "No .env, keys, keystores or service-account files are committed in this project".into(), None)}
                }
            } else {
                ul { class: "check-list",
                    for f in tracked.iter() {
                        {check_row(ws, false, f, "Tracked by git — anyone with the repository has it".into(), None)}
                    }
                }
                div { class: "conn-test",
                    button {
                        class: "btn btn-sm",
                        disabled: matches!(state(), JobState::Running(_)),
                        onclick: move |_| {
                            let files = ws.tracked_secrets.peek().clone();
                            let dir = ws.dir();
                            spawn(async move {
                                let r = tokio::task::spawn_blocking(move || signing::untrack_secrets(&dir, &files)).await.unwrap_or_else(|e| Err(e.to_string()));
                                state.set(match r {
                                    Ok(()) => JobState::Ok("Untracked and added to .gitignore — commit the change".into()),
                                    Err(e) => JobState::Failed(e),
                                });
                                refresh.with_mut(|n| *n += 1);
                            });
                        },
                        "Stop tracking them"
                    }
                    div { class: "publish-status-row", {job_status_line(state(), "")} }
                }
                p { class: "settings-hint",
                    "The files stay on disk. Copies already committed remain in the history, so treat those keys as exposed and replace them."
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Backup
// ---------------------------------------------------------------------------
#[component]
fn BackupCard() -> Element {
    let ws = use_context::<Ws>();
    let mut settings = ws.settings;
    let mut refresh = ws.refresh;
    let backup = ws.backup.read().clone();
    let keys = keys_dir(&settings.read());
    let pw = use_signal(String::new);
    let pw2 = use_signal(String::new);
    let mut state = use_signal(|| JobState::Idle);

    let (ok, detail) = match &backup {
        BackupState::Never => (false, "No backup yet — if this Mac is lost, so are your keys".to_string()),
        BackupState::Outdated(at) => (false, format!("Last backup {at} UTC, but keys changed since")),
        BackupState::Current(at) => (true, format!("Last backup {at} UTC · {}", settings.read().last_backup_path)),
    };

    rsx! {
        div { class: "card",
            h2 { "Backup" }
            ul { class: "check-list",
                {check_row(ws, ok, "Encrypted backup of your keys", detail, None)}
            }
            div { class: "build-config-grid",
                div { class: "build-config-field",
                    label { class: "build-config-label", "Keys folder" }
                    input {
                        class: "text-input",
                        placeholder: "{signing::default_keys_dir().display()}",
                        value: "{settings.read().keys_dir}",
                        oninput: move |e: Event<FormData>| { settings.write().keys_dir = e.value(); save_settings(&settings()); },
                    }
                    p { class: "settings-hint", "Upload keystore, .p12, .p8 and service-account files — outside every project." }
                }
                div { class: "build-config-field",
                    label { class: "build-config-label", "Backup password" }
                    {password_input(pw, "At least 8 characters")}
                }
                div { class: "build-config-field",
                    label { class: "build-config-label", "Repeat password" }
                    {password_input(pw2, "")}
                }
            }
            div { class: "conn-test",
                button {
                    class: "btn btn-sm",
                    disabled: matches!(state(), JobState::Running(_)),
                    onclick: move |_| {
                        let (p1, p2) = (pw.peek().clone(), pw2.peek().clone());
                        if p1 != p2 {
                            state.set(JobState::Failed("The passwords don't match.".into()));
                            return;
                        }
                        let sources = backup_sources(&settings.peek());
                        let (mut pw, mut pw2) = (pw, pw2);
                        spawn(async move {
                            let name = format!("appscreens-keys-{}.enc", &utc_now()[..10]);
                            let Some(dest) = save_file("Save encrypted backup", &name).await else { return };
                            state.set(JobState::Running("Encrypting…".into()));
                            let dest2 = dest.clone();
                            let r = tokio::task::spawn_blocking(move || signing::export_backup(&sources, &dest2, &p1)).await.unwrap_or_else(|e| Err(e.to_string()));
                            pw.set(String::new());
                            pw2.set(String::new());
                            match r {
                                Ok(()) => {
                                    settings.write().last_backup_at = utc_now();
                                    settings.write().last_backup_path = dest.to_string_lossy().to_string();
                                    save_settings(&settings.peek());
                                    state.set(JobState::Ok("Saved — keep it off this Mac (USB drive, password manager, cloud)".into()));
                                }
                                Err(e) => state.set(JobState::Failed(e)),
                            }
                            refresh.with_mut(|n| *n += 1);
                        });
                    },
                    "Export encrypted backup…"
                }
                div { class: "publish-status-row", {job_status_line(state(), "")} }
            }
            p { class: "settings-hint",
                "Covers {keys.display()}"
                if !settings.read().android_keystore_path.trim().is_empty() { " and your upload keystore" }
                ". Restore with: openssl enc -d -aes-256-cbc -pbkdf2 -in FILE | tar xz"
            }
        }
    }
}

// ---------------------------------------------------------------------------
// If this Mac is lost
// ---------------------------------------------------------------------------

/// Google service-account key file → (project id, key id prefix).
fn service_account_ids(path: &str) -> Option<(String, String)> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(signing::expand_home(path)).ok()?).ok()?;
    Some((v["project_id"].as_str()?.to_string(), v["private_key_id"].as_str()?.chars().take(12).collect()))
}

/// The checklist from the day a Mac is stolen: what to revoke and replace,
/// filled in with this project's own key IDs.
#[component]
fn LostMacCard() -> Element {
    let ws = use_context::<Ws>();
    let p = ws.proj.read().clone();
    let plat = p.platform_type.clone();
    let resolve = env_lookup(&ws.dir());
    let asc_key = resolve("APP_STORE_CONNECT_API_KEY_KEY_ID").unwrap_or_else(|| "(not set)".into());
    let sa = resolve("GOOGLE_PLAY_JSON_KEY").and_then(|p| service_account_ids(&p));
    let identity = ws.settings.read().apple_identity.clone();
    let alias = ws.android.read().alias.clone();

    rsx! {
        details { class: "card inline-form",
            summary { "If this Mac is lost or stolen" }
            p { class: "settings-hint", "In this order — the first ones let someone publish as you." }
            ol { class: "lost-steps",
                li {
                    "Sign the Mac out of your Apple ID and Google accounts, and mark it lost in "
                    a { href: "https://www.icloud.com/find", target: "_blank", "Find My" }
                    "."
                }
                if plat.has_ios() {
                    li {
                        "Revoke the App Store Connect API key "
                        code { "{asc_key}" }
                        " and create a new one: "
                        a { href: "https://appstoreconnect.apple.com/access/integrations/api", target: "_blank", "Integrations" }
                        "."
                    }
                }
                if plat.has_android() {
                    li {
                        "Delete the Google service-account key "
                        code { if let Some((_, k)) = &sa { "{k}…" } else { "(not set)" } }
                        " and create a new one: "
                        a {
                            href: if let Some((proj, _)) = &sa { format!("https://console.cloud.google.com/iam-admin/serviceaccounts?project={proj}") } else { "https://console.cloud.google.com/iam-admin/serviceaccounts".into() },
                            target: "_blank",
                            "Service accounts"
                        }
                        "."
                    }
                }
                if plat.has_ios() {
                    li {
                        "Revoke the Distribution certificate ("
                        if identity.is_empty() { "none selected" } else { "{identity}" }
                        ") in "
                        a { href: "https://developer.apple.com/account/resources/certificates/list", target: "_blank", "Certificates" }
                        ", then create a new one and regenerate the profile above."
                    }
                    li {
                        "Developer ID certificates (macOS apps): don't revoke them yourself — ask "
                        a { href: "https://developer.apple.com/contact/", target: "_blank", "Apple Developer Support" }
                        " to revoke them from the theft date, so apps already downloaded keep opening."
                    }
                }
                if plat.has_android() {
                    li {
                        "Create a new upload key (\"{alias}\") above and request an upload key reset in "
                        a { href: "https://play.google.com/console", target: "_blank", "Play Console" }
                        " → App integrity → App signing. Apps never uploaded need nothing: the first upload registers the new key."
                    }
                }
                li { "Restore the rest from your encrypted backup, then make a new one." }
            }
        }
    }
}
