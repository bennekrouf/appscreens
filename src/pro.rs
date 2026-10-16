//! ShipScreen Pro: the window, and the free version's one app.
//!
//! Without Pro, ShipScreen works on one app (project folder): the first one
//! opened takes the free slot, kept in `free_app.json` beside the licence.
//! Opening another asks for Pro, or offers to make that one the free app
//! instead — the same deal as GitAgent's free repositories.

use std::path::{Path, PathBuf};

use dioxus::prelude::*;

use crate::licence::{self, Status};

const FREE_APP_FILE: &str = "free_app.json";

/// Shared by every part of the UI that can open the window or needs to know
/// whether the app is licensed. Provided once, by `App`.
#[derive(Clone, Copy)]
pub struct Pro {
    pub open: Signal<bool>,
    pub status: Signal<Status>,
    /// The app someone tried to open while the free slot holds another one.
    pub pending: Signal<Option<PathBuf>>,
}

/// The app the free version works on, once one has been opened.
pub fn free_app() -> Option<PathBuf> {
    let json = std::fs::read_to_string(licence::dir().join(FREE_APP_FILE)).ok()?;
    serde_json::from_str(&json).ok()
}

fn set_free_app(dir: &Path) {
    let _ = std::fs::create_dir_all(licence::dir());
    if let Ok(json) = serde_json::to_string(dir) {
        let _ = std::fs::write(licence::dir().join(FREE_APP_FILE), json);
    }
}

/// The same folder, however it was reached (a symlink, a trailing slash).
fn same_folder(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

#[derive(Debug, PartialEq)]
enum Decision {
    Open,
    /// Open it, and it becomes the free app.
    TakeSlot,
    Locked,
}

fn decide(status: &Status, slot: Option<&Path>, dir: &Path) -> Decision {
    if status.unlimited() {
        return Decision::Open;
    }
    match slot {
        None => Decision::TakeSlot,
        Some(f) if same_folder(f, dir) => Decision::Open,
        Some(_) => Decision::Locked,
    }
}

/// Whether `dir` may be opened now, taking the free slot for it if the slot
/// is still empty.
pub fn may_open(status: &Status, dir: &Path) -> bool {
    match decide(status, free_app().as_deref(), dir) {
        Decision::Open => true,
        Decision::TakeSlot => {
            set_free_app(dir);
            true
        }
        Decision::Locked => false,
    }
}

fn folder_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

fn open_url(url: &str) {
    use std::process::Command;
    let _ = if cfg!(target_os = "macos") {
        Command::new("open").arg(url).spawn()
    } else if cfg!(windows) {
        Command::new("cmd").args(["/c", "start", "", url]).spawn()
    } else {
        Command::new("xdg-open").arg(url).spawn()
    };
}

/// "Get Pro…" while free, "✦ PRO" once licensed; opens the window either way.
/// Nothing at all in a build that can't check licences.
#[component]
pub fn ProButton() -> Element {
    let mut pro = use_context::<Pro>();
    let status = pro.status.read().clone();
    let (label, class, title) = match &status {
        Status::Unavailable => return rsx! {},
        Status::Pro(l) => (
            "✦ PRO",
            "pro-btn active",
            format!("ShipScreen Pro, licensed to {}", l.email),
        ),
        Status::Renew(_) => (
            "✦ RENEW PRO",
            "pro-btn renew",
            "Your Pro licence doesn't cover this version".to_owned(),
        ),
        Status::Free => (
            "Get Pro…",
            "pro-btn",
            "The free version works on one app; Pro on any number".to_owned(),
        ),
    };
    rsx! {
        button {
            class,
            title,
            onclick: move |_| {
                pro.pending.set(None);
                pro.open.set(true);
            },
            "{label}"
        }
    }
}

/// The Pro window. `on_open` opens a project folder; it is called with the
/// app that was waiting once Pro is activated or it becomes the free app.
#[component]
pub fn ProWindow(on_open: EventHandler<PathBuf>) -> Element {
    let mut pro = use_context::<Pro>();
    let mut key = use_signal(String::new);
    let mut error = use_signal(|| Option::<String>::None);
    if !(pro.open)() {
        return rsx! {};
    }
    let status = pro.status.read().clone();
    let pending = pro.pending.read().clone();
    let free = free_app();
    let mut close = move || {
        pro.open.set(false);
        pro.pending.set(None);
        error.set(None);
    };
    let mut activate = move || {
        let pasted = key.read().clone();
        match licence::activate(&pasted) {
            Ok(s) => {
                let unlocked = s.unlimited();
                pro.status.set(s);
                key.set(String::new());
                error.set(None);
                if unlocked {
                    if let Some(dir) = pro.pending.take() {
                        pro.open.set(false);
                        on_open.call(dir);
                    }
                }
            }
            Err(e) => error.set(Some(e)),
        }
    };
    let free_name = free.as_deref().map(folder_name);
    let pending_name = pending.as_deref().map(folder_name);

    rsx! {
        div { class: "settings-backdrop", onclick: move |_| close() }
        div { class: "settings-popup pro-popup",
            div { class: "settings-header",
                h2 { "ShipScreen Pro" }
            }
            match &status {
                Status::Free => rsx! {
                    if let (Some(p), Some(f)) = (&pending_name, &free_name) {
                        p {
                            "The free version works on one app, and yours is "
                            strong { "{f}" }
                            ". To open "
                            strong { "{p}" }
                            " too, get ShipScreen Pro, or make it your free app instead."
                        }
                    } else {
                        p {
                            "The free version works on one app"
                            if let Some(f) = &free_name { ": " strong { "{f}" } }
                            ", with every step from build to store submission. "
                            "Pro works on any number of apps, and is the licence for commercial use."
                        }
                    }
                },
                Status::Pro(l) => rsx! {
                    p { class: "pro-ok",
                        "Pro is active on this computer, licensed to "
                        strong { "{l.email}" }
                        ". Updates included until {l.updates_until}."
                    }
                },
                Status::Renew(l) => rsx! {
                    p {
                        "Your licence ("
                        strong { "{l.email}" }
                        ") covers versions released until {l.updates_until}. This version is newer, so it runs as the free version: renew Pro to use it here, or keep using a version from before that date."
                    }
                },
                Status::Unavailable => rsx! {
                    p { "This build of ShipScreen can't check licences, so nothing is limited. Builds from mayorana.ch can." }
                },
            }
            if matches!(status, Status::Free | Status::Renew(_)) {
                button {
                    class: "btn btn-primary",
                    onclick: move |_| open_url(licence::BUY_URL),
                    if matches!(status, Status::Renew(_)) { "Renew ShipScreen Pro" } else { "Buy ShipScreen Pro" }
                }
                p { class: "settings-hint", "Already bought it? Paste the licence key from your email:" }
                textarea {
                    class: "licence-key",
                    rows: 4,
                    placeholder: "Licence key",
                    value: "{key}",
                    oninput: move |e| key.set(e.value()),
                }
                if let Some(e) = error.read().as_ref() {
                    p { class: "pro-error", "{e}" }
                }
                p { class: "settings-hint",
                    "The key is checked on this computer: no account, nothing sent anywhere."
                }
            }
            div { class: "pro-actions",
                if matches!(status, Status::Free | Status::Renew(_)) {
                    button {
                        class: "btn",
                        disabled: key.read().trim().is_empty(),
                        onclick: move |_| activate(),
                        "Activate"
                    }
                }
                if let (Some(dir), Some(name)) = (pending.clone(), pending_name.clone()) {
                    button {
                        class: "btn",
                        title: "The other app stays on disk; only the free slot moves",
                        onclick: move |_| {
                            set_free_app(&dir);
                            pro.open.set(false);
                            pro.pending.set(None);
                            on_open.call(dir.clone());
                        },
                        "Make {name} my free app"
                    }
                }
                if matches!(status, Status::Pro(_) | Status::Renew(_)) {
                    button {
                        class: "btn",
                        title: "To move the licence to another computer",
                        onclick: move |_| {
                            licence::deactivate();
                            pro.status.set(licence::current());
                        },
                        "Remove the licence from this computer"
                    }
                }
                button { class: "btn", onclick: move |_| close(), "Close" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_app_takes_the_slot_and_a_second_is_locked() {
        let a = Path::new("/nowhere/app-a");
        let b = Path::new("/nowhere/app-b");
        assert_eq!(decide(&Status::Free, None, a), Decision::TakeSlot);
        assert_eq!(decide(&Status::Free, Some(a), a), Decision::Open);
        assert_eq!(decide(&Status::Free, Some(a), b), Decision::Locked);
    }

    #[test]
    fn pro_and_keyless_builds_open_anything() {
        let a = Path::new("/nowhere/app-a");
        let b = Path::new("/nowhere/app-b");
        assert_eq!(decide(&Status::Unavailable, Some(a), b), Decision::Open);
    }
}
