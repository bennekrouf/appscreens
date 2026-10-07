//! Build script — embeds the Windows app icon and version details into the
//! .exe, so it shows in the taskbar, the Start menu, Alt-Tab and the file's
//! Properties. Needs `assets/icon.ico` (release CI makes it from
//! `assets/icon.png`); without it the build still succeeds, icon-less, with a
//! `cargo:warning` so the gap shows in the log.
//!
//! Non-Windows builds are a no-op.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");

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
        res.set("FileDescription", "AppScreens");
        res.set("ProductName", "AppScreens");
        res.set("CompanyName", "Mayorana");
        res.set("LegalCopyright", "© 2026 Mayorana");
        if let Err(e) = res.compile() {
            println!("cargo:warning=Failed to embed the Windows icon: {e} (building without it)");
        }
    }
}
