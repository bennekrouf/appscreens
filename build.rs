//! Build script — embeds the Windows app icon and version details into the
//! .exe, so it shows in the taskbar, the Start menu, Alt-Tab and the file's
//! Properties. Needs `assets/icon.ico` (release CI makes it from
//! `assets/icon.png`); without it the build still succeeds, icon-less, with a
//! `cargo:warning` so the gap shows in the log.
//!
//! Non-Windows builds skip the icon. Every build gets its release date, which
//! a Pro licence is checked against.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");

    // What a Pro licence's `updates_until` is compared with: SHIPSCREEN_RELEASE_DATE
    // if set, else the date of the commit being built (the release commit, in
    // CI), else empty (unknown: every licence covers it). The public key is read
    // by the app with `option_env!`; listed here so changing it rebuilds.
    println!("cargo:rerun-if-env-changed=SHIPSCREEN_RELEASE_DATE");
    println!("cargo:rerun-if-env-changed=SHIPSCREEN_LICENSE_PUBLIC_KEY");
    let date = std::env::var("SHIPSCREEN_RELEASE_DATE")
        .ok()
        .filter(|d| !d.is_empty())
        .or_else(commit_date);
    println!(
        "cargo:rustc-env=SHIPSCREEN_RELEASE_DATE={}",
        date.unwrap_or_default()
    );

    #[cfg(target_os = "windows")]
    {
        let icon = std::path::Path::new("assets/icon.ico");
        if !icon.exists() {
            println!(
                "cargo:warning=assets/icon.ico not found — the .exe will ship without an embedded icon. \
                 Make one from assets/icon.png to brand the Windows build."
            );
            return;
        }
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("FileDescription", "ShipScreen");
        res.set("ProductName", "ShipScreen");
        res.set("CompanyName", "Mayorana");
        res.set("LegalCopyright", "© 2026 Mayorana");
        if let Err(e) = res.compile() {
            println!("cargo:warning=Failed to embed the Windows icon: {e} (building without it)");
        }
    }
}

fn commit_date() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["log", "-1", "--format=%cs"])
        .output()
        .ok()?;
    let date = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && date.len() == 10).then_some(date)
}
