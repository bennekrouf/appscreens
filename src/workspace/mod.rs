//! Project workspace: a sidebar of steps in the order the work actually
//! happens — Setup → Build → Store listing → Release — with one screen per
//! step and a job drawer at the bottom for anything long-running.
//!
//! State shared by every step lives in [`Ws`], provided as context by
//! [`ProjectView`]. Step statuses are computed from that state (and from files
//! on disk), never stored, so they can't drift from reality.

use super::*;
use std::time::SystemTime;

mod checks;
mod jobs;
mod steps;
mod stores;

use checks::*;

// ---------------------------------------------------------------------------
// Steps
// ---------------------------------------------------------------------------
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Step {
    App,
    Accounts,
    Version,
    Build,
    Languages,
    Screenshots,
    Metadata,
    Submit,
}

impl Step {
    const ALL: [Step; 8] = [
        Step::App,
        Step::Accounts,
        Step::Version,
        Step::Build,
        Step::Languages,
        Step::Screenshots,
        Step::Metadata,
        Step::Submit,
    ];

    fn group(self) -> &'static str {
        match self {
            Step::App | Step::Accounts => "Setup",
            Step::Version | Step::Build => "Build",
            Step::Languages | Step::Screenshots | Step::Metadata => "Store listing",
            Step::Submit => "Release",
        }
    }

    /// Short label for the sidebar.
    fn label(self) -> &'static str {
        match self {
            Step::App => "App",
            Step::Accounts => "Accounts",
            Step::Version => "Version",
            Step::Build => "Build",
            Step::Languages => "Languages",
            Step::Screenshots => "Screenshots",
            Step::Metadata => "Store text",
            Step::Submit => "Submit",
        }
    }

    fn heading(self) -> &'static str {
        match self {
            Step::App => "App identity",
            Step::Accounts => "Accounts & signing",
            Step::Version => "Version & build numbers",
            Step::Build => "Build",
            Step::Languages => "Languages & source screens",
            Step::Screenshots => "Screenshots",
            Step::Metadata => "Store text",
            Step::Submit => "Submit to stores",
        }
    }

    fn blurb(self) -> &'static str {
        match self {
            Step::App => "Name, identifiers and platforms. They feed the build scripts and the store lookups.",
            Step::Accounts => "Signing identity and store API keys, checked here so nothing fails halfway through a build or an upload.",
            Step::Version => "What this release is called in the stores, and the build numbers the next builds will use.",
            Step::Build => "Produce the bundles you ship. Builds run in the background — you can move on to other steps meanwhile.",
            Step::Languages => "Pick the store languages, then add the raw app screenshots and their captions for each one.",
            Step::Screenshots => "Choose the sizes and a style, then generate the store-ready images.",
            Step::Metadata => "Descriptions, keywords and release notes for each language, sent with the listing.",
            Step::Submit => "Upload the listing, upload the build, then send it to App Review or a Play track.",
        }
    }
}

/// Steps that apply to this project. Desktop projects have nothing to sign,
/// build or submit here; the Android build of AppScreens can't run the bash
/// build scripts.
fn visible_steps(platform: &PlatformType) -> Vec<Step> {
    let store_app = !platform.has_desktop();
    Step::ALL
        .into_iter()
        .filter(|s| match s {
            Step::Accounts | Step::Version | Step::Metadata | Step::Submit => store_app,
            Step::Build => store_app && cfg!(not(target_os = "android")),
            _ => true,
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum StepStatus {
    Todo,
    Running,
    Attention,
    Done,
}

// ---------------------------------------------------------------------------
// Jobs
// ---------------------------------------------------------------------------
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum JobKind {
    Screenshots,
    Build,
    AppStore,
    AppStoreBuild,
    AppStoreReview,
    GooglePlay,
    PlayRelease,
}

impl JobKind {
    const ALL: [JobKind; 7] = [
        JobKind::Screenshots,
        JobKind::Build,
        JobKind::AppStore,
        JobKind::AppStoreBuild,
        JobKind::AppStoreReview,
        JobKind::GooglePlay,
        JobKind::PlayRelease,
    ];

    fn label(self) -> &'static str {
        match self {
            JobKind::Screenshots => "Screenshots",
            JobKind::Build => "Build",
            JobKind::AppStore => "App Store listing",
            JobKind::AppStoreBuild => "IPA upload",
            JobKind::AppStoreReview => "App Review",
            JobKind::GooglePlay => "Play listing",
            JobKind::PlayRelease => "Play release",
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
enum JobState {
    Idle,
    Running(String),
    Ok(String),
    Failed(String),
}

// ---------------------------------------------------------------------------
// Things read from disk: store credentials and built bundles
// ---------------------------------------------------------------------------
#[derive(Clone, PartialEq, Debug)]
struct CredCheck {
    label: &'static str,
    ok: bool,
    detail: String,
}

#[derive(Clone, PartialEq, Debug, Default)]
struct Creds {
    app_store: Vec<CredCheck>,
    play: Vec<CredCheck>,
}

impl Creds {
    fn app_store_ok(&self) -> bool {
        self.app_store.iter().all(|c| c.ok)
    }
    fn play_ok(&self) -> bool {
        self.play.iter().all(|c| c.ok)
    }
}

/// Same lookup order as the publish jobs: project `.env`, then
/// `fastlane/.env`, then the process environment.
fn read_creds(dir: &std::path::Path) -> Creds {
    let env = load_env(&[dir.join(".env"), dir.join("fastlane").join(".env")]);
    let resolve = |key: &str| -> Option<String> {
        env.get(key)
            .cloned()
            .or_else(|| std::env::var(key).ok())
            .filter(|v| !v.is_empty())
    };
    let value = |label: &'static str, key: &str| match resolve(key) {
        Some(_) => CredCheck { label, ok: true, detail: format!("{key} is set") },
        None => CredCheck { label, ok: false, detail: format!("{key} is not set") },
    };
    let file = |label: &'static str, key: &str| match resolve(key) {
        Some(path) if std::path::Path::new(&path).is_file() => {
            CredCheck { label, ok: true, detail: path }
        }
        Some(path) => CredCheck { label, ok: false, detail: format!("File not found: {path}") },
        None => CredCheck { label, ok: false, detail: format!("{key} is not set") },
    };
    Creds {
        app_store: vec![
            value("Key ID", "APP_STORE_CONNECT_API_KEY_KEY_ID"),
            value("Issuer ID", "APP_STORE_CONNECT_API_KEY_ISSUER_ID"),
            file("Private key (.p8)", "APP_STORE_CONNECT_API_KEY_KEY_FILEPATH"),
        ],
        play: vec![file("Service-account JSON", "GOOGLE_PLAY_JSON_KEY")],
    }
}

#[derive(Clone, PartialEq, Debug)]
struct Artifact {
    path: PathBuf,
    modified: SystemTime,
    size: u64,
}

impl Artifact {
    fn file_name(&self) -> String {
        self.path.file_name().unwrap_or_default().to_string_lossy().to_string()
    }
    fn summary(&self) -> String {
        format!("{} · {:.1} MB · {}", self.file_name(), self.size as f64 / 1_048_576.0, ago(self.modified))
    }
}

#[derive(Clone, PartialEq, Debug, Default)]
struct Artifacts {
    ipa: Option<Artifact>,
    aab: Option<Artifact>,
    apk: Option<Artifact>,
}

/// The build scripts copy their final bundle into the project root
/// (`<App>.ipa`, `<slug>_release.aab`, `<slug>-debug.apk`); the newest file of
/// each kind is the one that counts.
fn scan_artifacts(dir: &std::path::Path) -> Artifacts {
    let mut found = Artifacts::default();
    let Ok(entries) = std::fs::read_dir(dir) else { return found };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let slot = match path.extension().and_then(|e| e.to_str()) {
            Some("ipa") => &mut found.ipa,
            Some("aab") => &mut found.aab,
            Some("apk") => &mut found.apk,
            _ => continue,
        };
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if slot.as_ref().is_none_or(|a| modified > a.modified) {
            *slot = Some(Artifact { path, modified, size: meta.len() });
        }
    }
    found
}

fn ago(t: SystemTime) -> String {
    let secs = SystemTime::now().duration_since(t).map(|d| d.as_secs()).unwrap_or(0);
    match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86_400),
    }
}

// ---------------------------------------------------------------------------
// Workspace context
// ---------------------------------------------------------------------------
#[derive(Clone, Copy)]
pub(crate) struct Ws {
    dir: Signal<PathBuf>,
    proj: Signal<ProjectState>,
    settings: Signal<Settings>,
    step: Signal<Step>,
    /// Scope that owns the jobs — the workspace itself, so leaving a step
    /// doesn't cancel what it started.
    scope: ScopeId,
    /// Bumped to re-read credentials and artifacts from disk.
    refresh: Signal<u32>,
    creds: Memo<Creds>,
    artifacts: Memo<Artifacts>,
    /// The project's provisioning profile, parsed (None if unset/unreadable).
    profile: Memo<Option<ProfileInfo>>,
    /// Results of the Accounts step's "Test connection" buttons.
    asc_test: Signal<JobState>,
    play_test: Signal<JobState>,
    /// Language currently being edited in the Languages step.
    active_locale: Signal<String>,

    gen_phase: Signal<AppPhase>,
    gen_log: Signal<Vec<String>>,
    build_phase: Signal<BuildPhase>,
    build_log: Signal<Vec<String>>,
    ios_pub: Signal<PublishPhase>,
    ios_pub_log: Signal<Vec<String>>,
    play_pub: Signal<AndroidPublishPhase>,
    play_pub_log: Signal<Vec<String>>,
    ios_build_job: Signal<JobState>,
    ios_build_log: Signal<Vec<String>>,
    ios_review_job: Signal<JobState>,
    ios_review_log: Signal<Vec<String>>,
    play_release_job: Signal<JobState>,
    play_release_log: Signal<Vec<String>>,

    drawer_open: Signal<bool>,
    drawer_job: Signal<JobKind>,
}

impl Ws {
    fn dir(&self) -> PathBuf {
        self.dir.read().clone()
    }

    /// Mutate the project state and persist it.
    fn update(mut self, f: impl FnOnce(&mut ProjectState)) {
        let dir = self.dir();
        let mut p = self.proj.write();
        f(&mut p);
        save_project_state(&dir, &p);
    }

    fn spawn(&self, fut: impl std::future::Future<Output = ()> + 'static) {
        dioxus::core::Runtime::current().spawn(self.scope, fut);
    }

    fn go(mut self, step: Step) {
        self.step.set(step);
        document::eval("window.scrollTo(0, 0);");
    }

    fn show_job(mut self, kind: JobKind) {
        self.drawer_job.set(kind);
        self.drawer_open.set(true);
    }

    fn job_state(&self, kind: JobKind) -> JobState {
        match kind {
            JobKind::Screenshots => match &*self.gen_phase.read() {
                AppPhase::Idle => JobState::Idle,
                AppPhase::GeneratingAi | AppPhase::GeneratingManual | AppPhase::Resizing => {
                    JobState::Running("Generating…".into())
                }
                AppPhase::Done => JobState::Ok("Generated".into()),
                AppPhase::Error(e) => JobState::Failed(e.clone()),
            },
            JobKind::Build => match &*self.build_phase.read() {
                BuildPhase::Idle => JobState::Idle,
                BuildPhase::Running(name) => JobState::Running(format!("Running {name}…")),
                BuildPhase::Success(name) => JobState::Ok(format!("{name} finished")),
                BuildPhase::Error(e) => JobState::Failed(e.clone()),
            },
            JobKind::AppStore => match &*self.ios_pub.read() {
                PublishPhase::Idle => JobState::Idle,
                PublishPhase::Running => JobState::Running("Uploading…".into()),
                PublishPhase::Success => JobState::Ok("Uploaded".into()),
                PublishPhase::Error(e) => JobState::Failed(e.clone()),
            },
            JobKind::GooglePlay => match &*self.play_pub.read() {
                AndroidPublishPhase::Idle => JobState::Idle,
                AndroidPublishPhase::Running => JobState::Running("Uploading…".into()),
                AndroidPublishPhase::Success => JobState::Ok("Uploaded".into()),
                AndroidPublishPhase::Error(e) => JobState::Failed(e.clone()),
            },
            JobKind::AppStoreBuild => self.ios_build_job.read().clone(),
            JobKind::AppStoreReview => self.ios_review_job.read().clone(),
            JobKind::PlayRelease => self.play_release_job.read().clone(),
        }
    }

    fn job_log(&self, kind: JobKind) -> Signal<Vec<String>> {
        match kind {
            JobKind::Screenshots => self.gen_log,
            JobKind::Build => self.build_log,
            JobKind::AppStore => self.ios_pub_log,
            JobKind::GooglePlay => self.play_pub_log,
            JobKind::AppStoreBuild => self.ios_build_log,
            JobKind::AppStoreReview => self.ios_review_log,
            JobKind::PlayRelease => self.play_release_log,
        }
    }

    fn status(&self, step: Step) -> StepStatus {
        let p = self.proj.read();
        let plat = &p.platform_type;
        match step {
            Step::App => {
                let ids_ok = (!plat.has_ios() || !p.ios_bundle_id.trim().is_empty())
                    && (!plat.has_android() || !p.android_bundle_id.trim().is_empty());
                if !p.app_name.trim().is_empty() && !p.project_slug.trim().is_empty() && ids_ok {
                    StepStatus::Done
                } else {
                    StepStatus::Todo
                }
            }
            Step::Accounts => {
                let s = self.settings.read();
                let creds = self.creds.read();
                let mut checks = Vec::new();
                let untested_or_ok = |t: &JobState| !matches!(t, JobState::Failed(_));
                if plat.has_ios() {
                    checks.push(!s.apple_identity.trim().is_empty());
                    checks.push(self.profile.read().as_ref().is_some_and(|info| {
                        profile_checks(info, &p.ios_bundle_id, &s.apple_identity).iter().all(|c| c.ok)
                    }));
                    checks.push(creds.app_store_ok() && untested_or_ok(&self.asc_test.read()));
                }
                if plat.has_android() {
                    checks.push(creds.play_ok() && untested_or_ok(&self.play_test.read()));
                }
                match checks.iter().filter(|ok| **ok).count() {
                    n if n == checks.len() => StepStatus::Done,
                    0 => StepStatus::Todo,
                    _ => StepStatus::Attention,
                }
            }
            Step::Version => {
                if valid_version(p.version.trim()) && p.ios_build_number > 0 && p.android_version_code > 0 {
                    StepStatus::Done
                } else {
                    StepStatus::Attention
                }
            }
            Step::Build => {
                let a = self.artifacts.read();
                match &*self.build_phase.read() {
                    BuildPhase::Running(_) => return StepStatus::Running,
                    BuildPhase::Error(_) => return StepStatus::Attention,
                    _ => {}
                }
                let ios_ok = !plat.has_ios() || a.ipa.is_some();
                let android_ok = !plat.has_android() || a.aab.is_some();
                if ios_ok && android_ok {
                    StepStatus::Done
                } else if a.ipa.is_some() || a.aab.is_some() {
                    StepStatus::Attention
                } else {
                    StepStatus::Todo
                }
            }
            Step::Languages => {
                let filled = p.locales.iter().filter(|l| !p.sources_for(l).is_empty()).count();
                if filled == 0 {
                    StepStatus::Todo
                } else if filled == p.locales.len() {
                    StepStatus::Done
                } else {
                    StepStatus::Attention
                }
            }
            Step::Screenshots => match &*self.gen_phase.read() {
                AppPhase::GeneratingAi | AppPhase::GeneratingManual | AppPhase::Resizing => StepStatus::Running,
                AppPhase::Error(_) => StepStatus::Attention,
                _ if p.output_paths.iter().any(|(_, path)| path.exists()) => StepStatus::Done,
                _ => StepStatus::Todo,
            },
            Step::Metadata => {
                let complete = |loc: &String| {
                    p.store_texts.get(loc).is_some_and(|t| {
                        !t.description.trim().is_empty()
                            && (!plat.has_android() || !t.short_description.trim().is_empty())
                    })
                };
                let started = |loc: &String| p.store_texts.get(loc).is_some_and(|t| *t != StoreText::default());
                if p.locales.iter().all(complete) {
                    StepStatus::Done
                } else if p.locales.iter().any(started) {
                    StepStatus::Attention
                } else {
                    StepStatus::Todo
                }
            }
            Step::Submit => {
                let mut kinds = Vec::new();
                if plat.has_ios() {
                    kinds.extend([JobKind::AppStore, JobKind::AppStoreBuild, JobKind::AppStoreReview]);
                }
                if plat.has_android() {
                    kinds.extend([JobKind::GooglePlay, JobKind::PlayRelease]);
                }
                let states: Vec<JobState> = kinds.into_iter().map(|k| self.job_state(k)).collect();
                // Shipped = the current version reached review (iOS) or a track (Play).
                let version = p.version.trim();
                let shipped = |platform: &str, action: &str| {
                    p.releases.iter().any(|r| r.platform == platform && r.version == version && r.action.starts_with(action))
                };
                if states.iter().any(|s| matches!(s, JobState::Running(_))) {
                    StepStatus::Running
                } else if states.iter().any(|s| matches!(s, JobState::Failed(_))) {
                    StepStatus::Attention
                } else if (!plat.has_ios() || shipped("ios", "Submitted for review"))
                    && (!plat.has_android() || shipped("android", "Released to"))
                {
                    StepStatus::Done
                } else {
                    StepStatus::Todo
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Project View (workspace shell)
// ---------------------------------------------------------------------------
#[component]
pub(crate) fn ProjectView(project_dir: PathBuf, on_close: EventHandler<()>) -> Element {
    let settings = use_context::<Signal<Settings>>();

    let dir = use_signal(|| project_dir.clone());
    let proj = use_signal(|| {
        let mut state = load_project_state(&project_dir);
        if seed_release_fields(&mut state, &project_dir, &settings.peek()) {
            save_project_state(&project_dir, &state);
        }
        state
    });
    let refresh = use_signal(|| 0u32);
    let creds = use_memo(move || {
        refresh.read();
        read_creds(&dir.read())
    });
    let artifacts = use_memo(move || {
        refresh.read();
        scan_artifacts(&dir.read())
    });
    let profile = use_memo(move || {
        refresh.read();
        let path = proj.read().provisioning_profile.trim().to_string();
        (!path.is_empty()).then(|| read_profile(std::path::Path::new(&path))).flatten()
    });
    let active_locale = use_signal(|| {
        proj.peek().locales.first().cloned().unwrap_or_else(|| "en-US".to_string())
    });

    let mut show_settings = use_signal(|| false);
    // Lock body scroll whenever the settings popup is open
    use_effect(move || {
        let locked = *show_settings.read();
        let js = if locked {
            "document.body.style.overflow = 'hidden';"
        } else {
            "document.body.style.overflow = '';"
        };
        spawn(async move { document::eval(js).await.ok(); });
    });

    let ws = Ws {
        dir,
        proj,
        settings,
        step: use_signal(|| Step::App),
        scope: dioxus::core::current_scope_id(),
        refresh,
        creds,
        artifacts,
        profile,
        asc_test: use_signal(|| JobState::Idle),
        play_test: use_signal(|| JobState::Idle),
        active_locale,
        gen_phase: use_signal(|| AppPhase::Idle),
        gen_log: use_signal(Vec::new),
        build_phase: use_signal(|| BuildPhase::Idle),
        build_log: use_signal(Vec::new),
        ios_pub: use_signal(|| PublishPhase::Idle),
        ios_pub_log: use_signal(Vec::new),
        play_pub: use_signal(|| AndroidPublishPhase::Idle),
        play_pub_log: use_signal(Vec::new),
        ios_build_job: use_signal(|| JobState::Idle),
        ios_build_log: use_signal(Vec::new),
        ios_review_job: use_signal(|| JobState::Idle),
        ios_review_log: use_signal(Vec::new),
        play_release_job: use_signal(|| JobState::Idle),
        play_release_log: use_signal(Vec::new),
        drawer_open: use_signal(|| false),
        drawer_job: use_signal(|| JobKind::Screenshots),
    };
    use_context_provider(|| ws);

    // Open on the first step that still needs work, once.
    use_hook(move || {
        let steps = visible_steps(&proj.peek().platform_type);
        let first = steps
            .iter()
            .copied()
            .find(|s| ws.status(*s) != StepStatus::Done)
            .unwrap_or(Step::App);
        let mut step = ws.step;
        step.set(first);
    });

    let steps = visible_steps(&proj.read().platform_type);
    // A platform change can hide the current step (e.g. switching to Desktop).
    let current = {
        let s = *ws.step.read();
        if steps.contains(&s) { s } else { Step::App }
    };
    let idx = steps.iter().position(|s| *s == current).unwrap_or(0);
    let prev = idx.checked_sub(1).map(|i| steps[i]);
    let next = steps.get(idx + 1).copied();

    let proj_name = {
        let name = proj.read().app_name.trim().to_string();
        if name.is_empty() {
            project_dir.file_name().unwrap_or_default().to_string_lossy().to_string()
        } else {
            name
        }
    };
    let dir_display = project_dir.to_string_lossy().to_string();

    rsx! {
        div { class: "ws",
            // ---- Sidebar ----
            nav { class: "ws-side", aria_label: "Project steps",
                div { class: "ws-side-head",
                    button {
                        class: "btn btn-icon btn-back",
                        onclick: move |_| on_close.call(()),
                        title: "Back to projects",
                        aria_label: "Back to projects",
                        {icon_chevron_left()}
                    }
                    div { class: "ws-proj",
                        p { class: "ws-proj-name", "{proj_name}" }
                        // LRM keeps the leading "/" in place under the rtl clipping (see .ws-proj-path)
                        p { class: "ws-proj-path", title: "{dir_display}", "\u{200E}{dir_display}" }
                    }
                }

                div { class: "ws-nav",
                    for (i, s) in steps.iter().copied().enumerate() {
                        if i == 0 || steps[i - 1].group() != s.group() {
                            p { class: "ws-group-label", "{s.group()}" }
                        }
                        StepLink { step: s, number: i + 1, active: s == current }
                    }
                }

                div { class: "ws-side-foot",
                    button {
                        class: "btn ws-settings-btn",
                        onclick: move |_| show_settings.toggle(),
                        svg {
                            class: "icon icon-fill", view_box: "0 0 20 20", "aria-hidden": "true",
                            path { d: "M11.49 3.17c-.38-1.56-2.6-1.56-2.98 0a1.532 1.532 0 01-2.286.948c-1.372-.836-2.942.734-2.106 2.106.54.886.061 2.042-.947 2.287-1.561.379-1.561 2.6 0 2.978a1.532 1.532 0 01.947 2.287c-.836 1.372.734 2.942 2.106 2.106a1.532 1.532 0 012.287.947c.379 1.561 2.6 1.561 2.978 0a1.533 1.533 0 012.287-.947c1.372.836 2.942-.734 2.106-2.106a1.533 1.533 0 01.947-2.287c1.561-.379 1.561-2.6 0-2.978a1.532 1.532 0 01-.947-2.287c.836-1.372-.734-2.942-2.106-2.106a1.532 1.532 0 01-2.287-.947zM10 13a3 3 0 100-6 3 3 0 000 6z" }
                        }
                        "Settings"
                    }
                }
            }

            if *show_settings.read() {
                SettingsPopup { on_close: move |_| show_settings.set(false) }
            }

            // ---- Step screen ----
            main { class: "ws-main",
                div { class: "ws-content",
                    header { class: "step-head",
                        p { class: "step-eyebrow", "Step {idx + 1} of {steps.len()} · {current.group()}" }
                        h1 { "{current.heading()}" }
                        p { class: "step-blurb", "{current.blurb()}" }
                    }

                    match current {
                        Step::App => rsx! { steps::AppStep {} },
                        Step::Accounts => rsx! { steps::AccountsStep {} },
                        Step::Version => rsx! { steps::VersionStep {} },
                        Step::Build => rsx! { steps::BuildStep {} },
                        Step::Languages => rsx! { steps::LanguagesStep {} },
                        Step::Screenshots => rsx! { steps::ScreenshotsStep {} },
                        Step::Metadata => rsx! { steps::MetadataStep {} },
                        Step::Submit => rsx! { steps::SubmitStep {} },
                    }

                    div { class: "step-nav",
                        if let Some(p) = prev {
                            button {
                                class: "btn",
                                onclick: move |_| ws.go(p),
                                {icon_chevron_left()}
                                "{p.label()}"
                            }
                        }
                        if let Some(n) = next {
                            button {
                                class: "btn btn-primary step-nav-next",
                                onclick: move |_| ws.go(n),
                                "Continue to {n.label()}"
                                {icon_chevron_right()}
                            }
                        }
                    }
                }

                JobDrawer {}
            }
        }
    }
}

#[component]
fn StepLink(step: Step, number: usize, active: bool) -> Element {
    let ws = use_context::<Ws>();
    let status = ws.status(step);
    let (mark_class, status_label) = match status {
        StepStatus::Done => ("ws-step-mark mark-done", "done"),
        StepStatus::Attention => ("ws-step-mark mark-attn", "needs attention"),
        StepStatus::Running => ("ws-step-mark mark-run", "running"),
        StepStatus::Todo => ("ws-step-mark", "to do"),
    };
    rsx! {
        button {
            class: if active { "ws-step ws-step-active" } else { "ws-step" },
            aria_current: if active { "step" },
            title: "{step.label()} — {status_label}",
            onclick: move |_| ws.go(step),
            span { class: "{mark_class}",
                match status {
                    StepStatus::Done => rsx! { {icon_check()} },
                    StepStatus::Attention => rsx! { "!" },
                    StepStatus::Running => rsx! { span { class: "spinner spinner-dark" } },
                    StepStatus::Todo => rsx! { "{number}" },
                }
            }
            span { class: "ws-step-label", "{step.label()}" }
        }
    }
}

// ---------------------------------------------------------------------------
// Job drawer — one place for every log
// ---------------------------------------------------------------------------
#[component]
fn JobDrawer() -> Element {
    let ws = use_context::<Ws>();
    let mut drawer_open = ws.drawer_open;
    let mut drawer_job = ws.drawer_job;

    let jobs: Vec<(JobKind, JobState)> = JobKind::ALL
        .into_iter()
        .map(|k| (k, ws.job_state(k)))
        .filter(|(_, s)| *s != JobState::Idle)
        .collect();

    let selected = *drawer_job.read();
    let log = ws.job_log(selected);

    // Keep the open log pinned to its newest line.
    use_effect(move || {
        let _ = log.read().len();
        let _ = drawer_open.read();
        spawn(async move {
            document::eval(
                "setTimeout(() => { for (const el of document.querySelectorAll('.drawer-log')) el.scrollTop = el.scrollHeight; }, 50);",
            )
            .await
            .ok();
        });
    });

    if jobs.is_empty() {
        return rsx! {};
    }
    let open = *drawer_open.read();

    rsx! {
        section { class: if open { "drawer drawer-open" } else { "drawer" }, aria_label: "Jobs",
            div { class: "drawer-bar",
                span { class: "drawer-title", "Jobs" }
                div { class: "drawer-chips",
                    for (kind, state) in jobs.iter().cloned() {
                        {
                            let (dot, text) = match &state {
                                JobState::Running(t) => ("drawer-dot dot-run", t.clone()),
                                JobState::Ok(t) => ("drawer-dot dot-ok", t.clone()),
                                JobState::Failed(_) => ("drawer-dot dot-err", "Failed".to_string()),
                                JobState::Idle => ("drawer-dot", String::new()),
                            };
                            let is_sel = open && kind == selected;
                            rsx! {
                                button {
                                    class: if is_sel { "drawer-chip drawer-chip-active" } else { "drawer-chip" },
                                    onclick: move |_| {
                                        if open && kind == selected {
                                            drawer_open.set(false);
                                        } else {
                                            drawer_job.set(kind);
                                            drawer_open.set(true);
                                        }
                                    },
                                    span { class: "{dot}" }
                                    strong { "{kind.label()}" }
                                    span { class: "drawer-chip-text", "{text}" }
                                }
                            }
                        }
                    }
                }
                button {
                    class: "btn btn-sm drawer-toggle",
                    onclick: move |_| drawer_open.toggle(),
                    if open { "Hide log" } else { "Show log" }
                }
            }
            if open {
                div { class: "drawer-body",
                    if let JobState::Failed(msg) = ws.job_state(selected) {
                        p { class: "drawer-error", role: "alert",
                            {icon_alert()}
                            "{msg}"
                        }
                    }
                    if log.read().is_empty() {
                        p { class: "output-empty", "No output yet." }
                    } else {
                        div { class: "log-scroll drawer-log",
                            for line in log.read().iter() {
                                p { class: log_class(line), "{line}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn log_class(line: &str) -> &'static str {
    let lower = line.to_lowercase();
    if lower.contains("error") || line.starts_with('❌') || line.starts_with('✗') {
        "log-line log-error"
    } else if lower.contains("warning") || line.starts_with('⚠') {
        "log-line log-warn"
    } else if line.starts_with('✅') || line.starts_with('🎉') {
        "log-line log-success"
    } else {
        "log-line"
    }
}
