use ab_glyph::{FontRef, PxScale};
use dioxus::desktop::wry::http::{Response, StatusCode};
use dioxus::desktop::wry::RequestAsyncResponder;
use dioxus::desktop::{use_asset_handler, LogicalSize, WindowBuilder};
use dioxus::prelude::*;
use image::imageops::FilterType;
use image::{Rgba, RgbaImage};
use imageproc::drawing::{draw_text_mut, text_size};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

mod update_check;
mod notice;
mod workspace;

use workspace::ProjectView;
#[cfg(target_os = "android")]
mod android_saf;

// Embedded rather than shipped as a manganis asset: the bundled `.app` /
// `.msi` / `.deb` only carry `assets/` if the packaging step copies them in,
// and when it silently does not (as in the v0.1.2 dmg) the webview 404s the
// stylesheet and renders raw unstyled DOM. Baking it into the binary — like
// ROBOTO_FONT and the window icon — makes that failure mode impossible.
const MAIN_CSS: &str = include_str!("../assets/main.css");
const ROBOTO_FONT: &[u8] = include_bytes!("../assets/Roboto-Bold.ttf");

// ---------------------------------------------------------------------------
// Locales supported by abjad (fastlane locale code -> display name)
// ---------------------------------------------------------------------------
const LOCALES: &[(&str, &str)] = &[
    ("ar-SA", "Arabic"),
    ("en-US", "English"),
    ("fr-FR", "French"),
    ("hi", "Hindi"),
    ("id", "Indonesian"),
    ("ms", "Malay"),
    ("sq", "Albanian"),
    ("tr", "Turkish"),
    ("ur", "Urdu"),
    ("zh-Hans", "Chinese (Simplified)"),
];

// iOS target -> fastlane device folder name
const IOS_TARGETS: &[(&str, &str, u32, u32)] = &[
    ("ios_iphone_69", "iPhone 6.9\" Display", 1320, 2868),
    ("ios_iphone_67", "iPhone 6.7\" Display", 1290, 2796),
    ("ios_iphone_65", "iPhone 6.5\" Display", 1242, 2688),
    ("ios_ipad_129", "iPad Pro (12.9-inch)", 2048, 2732),
];

// Android targets still go to output/ inside the project
const ANDROID_TARGETS: &[(&str, &str, u32, u32)] = &[
    ("android_phone", "Android Phone", 1080, 2340),
    ("android_feature", "Android Feature", 1024, 500),
];

// Desktop screenshot targets (used when platform_type == Desktop)
const DESKTOP_TARGETS: &[(&str, &str, u32, u32)] = &[
    ("desktop_mac",   "macOS (1280×800)",    1280,  800),
    ("desktop_win",   "Windows (1280×720)",  1280,  720),
    ("desktop_wide",  "Widescreen (1920×1080)", 1920, 1080),
];

// ---------------------------------------------------------------------------
// Global settings (gear popup)
// ---------------------------------------------------------------------------
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Settings {
    fal_key: String,
    #[serde(default = "default_phone_style")]
    phone_style: String,
    inference_steps: u32,
    #[serde(default)]
    recent_projects: Vec<PathBuf>,
    // Build / signing config (same developer across all projects)
    #[serde(default)]
    apple_identity: String,   // "Apple Distribution: Name (TEAMID)"
    #[serde(default)]
    provisioning_profile: String, // absolute path to .mobileprovision
    #[serde(default = "default_ios_short_version")]
    ios_short_version: String,    // e.g. "1.0"
}

fn default_phone_style() -> String {
    "modern smartphone".to_string()
}

fn default_ios_short_version() -> String {
    "1.0".to_string()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            fal_key: std::env::var("FAL_KEY").unwrap_or_default(),
            phone_style: default_phone_style(),
            inference_steps: 28,
            recent_projects: Vec::new(),
            apple_identity: String::new(),
            provisioning_profile: String::new(),
            ios_short_version: "1.0".to_string(),
        }
    }
}

fn global_config_path() -> PathBuf {
    let dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("appscreens");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("settings.json")
}

fn load_settings() -> Settings {
    std::fs::read_to_string(global_config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_settings(s: &Settings) {
    if let Ok(json) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(global_config_path(), json);
    }
}

/// Load key=value pairs from a .env file into a map.
fn parse_dotenv(path: &PathBuf) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    if let Ok(contents) = std::fs::read_to_string(path) {
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') { continue; }
            if let Some((k, v)) = line.split_once('=') {
                let v = v.trim().trim_matches('"').trim_matches('\'');
                map.insert(k.trim().to_string(), v.to_string());
            }
        }
    }
    map
}

/// Merge env vars from multiple .env files and the process environment.
/// Later sources win (project .env > app .env > process env).
/// Returns a map and a resolver closure isn't possible here, so callers use
/// the returned map + std::env::var fallback.
fn load_env(dotenv_paths: &[PathBuf]) -> std::collections::HashMap<String, String> {
    let mut merged = std::collections::HashMap::new();

    // Collect candidate directories to look for an AppScreens-level .env:
    //   1. current working directory  (always correct for `cargo run`)
    //   2. next to the executable     (correct for an installed/release binary)
    //   3. walk up from exe until we find a .env (handles target/debug/ nesting)
    let mut search_dirs: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        search_dirs.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        // Walk up from the exe dir looking for a .env
        let mut dir = exe.parent().map(|p| p.to_path_buf());
        while let Some(d) = dir {
            search_dirs.push(d.clone());
            dir = d.parent().map(|p| p.to_path_buf());
        }
    }
    for dir in &search_dirs {
        let candidate = dir.join(".env");
        if candidate.exists() {
            for (k, v) in parse_dotenv(&candidate) {
                merged.entry(k).or_insert(v); // first match wins (cwd beats target/debug)
            }
            break; // stop at the first .env found walking up
        }
    }

    // Then overlay each caller-supplied path (project-level, later = higher priority)
    for path in dotenv_paths {
        for (k, v) in parse_dotenv(path) {
            merged.insert(k, v);
        }
    }
    merged
}

// ---------------------------------------------------------------------------
// Platform type
// ---------------------------------------------------------------------------
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum PlatformType {
    Desktop,
    Ios,
    Android,
    #[default]
    IosAndroid,
}

impl PlatformType {
    fn label(&self) -> &'static str {
        match self {
            PlatformType::Desktop     => "Desktop",
            PlatformType::Ios         => "iOS",
            PlatformType::Android     => "Android",
            PlatformType::IosAndroid  => "iOS + Android",
        }
    }
    fn has_ios(&self) -> bool {
        matches!(self, PlatformType::Ios | PlatformType::IosAndroid)
    }
    fn has_android(&self) -> bool {
        matches!(self, PlatformType::Android | PlatformType::IosAndroid)
    }
    fn has_desktop(&self) -> bool {
        matches!(self, PlatformType::Desktop)
    }
}

// ---------------------------------------------------------------------------
// Per-project state (saved as <project_dir>/appscreens.json)
// ---------------------------------------------------------------------------
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
struct ProjectState {
    /// Legacy shared source paths (kept for backward compat; migrated into locale_sources on load)
    #[serde(default)]
    source_paths: Vec<PathBuf>,
    /// Per-locale source images: locale code → ordered list of image paths
    #[serde(default)]
    locale_sources: std::collections::HashMap<String, Vec<PathBuf>>,
    /// Legacy single-locale texts (kept for backward compat; migrated into locale_texts on load)
    #[serde(default)]
    manual_texts: Vec<(String, String)>, // (title, subtitle) per image
    /// Per-locale texts: locale code → vec of (title, subtitle) per source image
    #[serde(default)]
    locale_texts: std::collections::HashMap<String, Vec<(String, String)>>,
    #[serde(default)]
    primary_color: String,
    #[serde(default)]
    secondary_color: String,
    #[serde(default)]
    theme_prompt: String,
    #[serde(default)]
    theme_history: Vec<String>,
    /// Legacy single locale (kept for backward compat; use `locales` instead)
    #[serde(default = "default_locale")]
    locale: String,
    /// Currently selected locales for generation/publishing
    #[serde(default = "default_locales")]
    locales: Vec<String>,
    #[serde(default)]
    generated_urls: Vec<String>,
    #[serde(default)]
    output_paths: Vec<(String, PathBuf)>,
    #[serde(default)]
    logo_path: Option<PathBuf>,
    // Build configuration (per-project)
    #[serde(default)]
    app_name: String,      // Display name, e.g. "Abjad"
    #[serde(default)]
    project_slug: String,  // Lowercase dx slug, e.g. "abjad"
    /// Legacy single bundle ID field — kept for deserialization of old JSON only.
    /// On load this is migrated into ios_bundle_id / android_bundle_id and then ignored.
    #[serde(default, skip_serializing)]
    bundle_id: String,
    #[serde(default)]
    ios_bundle_id: String,     // iOS bundle ID, e.g. "com.mayorana.tafseel.mufrad"
    #[serde(default)]
    android_bundle_id: String, // Android package, e.g. "com.mayorana.mufrad"
    #[serde(default)]
    platform_type: PlatformType,  // Target platform(s) for this project
    // Export configuration
    #[serde(default = "default_true")]
    export_ios: bool,
    #[serde(default = "default_true")]
    export_android: bool,
    /// One bool per IOS_TARGETS entry (all enabled by default)
    #[serde(default = "default_ios_targets")]
    ios_targets: Vec<bool>,
    /// One bool per ANDROID_TARGETS entry (all enabled by default)
    #[serde(default = "default_android_targets")]
    android_targets: Vec<bool>,
    /// One bool per DESKTOP_TARGETS entry (all enabled by default)
    #[serde(default = "default_desktop_targets")]
    desktop_targets: Vec<bool>,
    // Release numbering (per project). Empty/0 means "not seeded yet" — the
    // workspace seeds them on open from the old sources (global setting,
    // build_number.txt, Dioxus.toml) so numbers never go backwards.
    /// Marketing version, e.g. "1.2.0" (CFBundleShortVersionString / versionName)
    #[serde(default)]
    version: String,
    /// Build number the next iOS build will use (CFBundleVersion)
    #[serde(default)]
    ios_build_number: u32,
    /// versionCode the next Android release build will use
    #[serde(default)]
    android_version_code: u32,
    /// Bump the build number after each successful release build
    #[serde(default = "default_true")]
    auto_increment_build: bool,
    /// Provisioning profile for this app (profiles are per bundle ID)
    #[serde(default)]
    provisioning_profile: String,
    // Store listing text and release settings
    /// Per-locale store text: locale code → texts
    #[serde(default)]
    store_texts: std::collections::HashMap<String, StoreText>,
    /// Support URL sent with every App Store localization (required for review)
    #[serde(default)]
    support_url: String,
    /// App uses only exempt encryption (HTTPS etc.) — answers export compliance
    #[serde(default)]
    exempt_encryption: bool,
    /// Google Play track releases go to: internal / alpha / beta / production
    #[serde(default = "default_play_track")]
    play_track: String,
    /// Google Play release status: draft / completed
    #[serde(default = "default_play_status")]
    play_release_status: String,
    /// What was shipped, newest first
    #[serde(default)]
    releases: Vec<ReleaseRecord>,
}

/// Store text for one locale. Limits are the stores' own.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
struct StoreText {
    /// Both stores (4000)
    #[serde(default)]
    description: String,
    /// App Store keywords, comma-separated (100)
    #[serde(default)]
    keywords: String,
    /// App Store promotional text (170)
    #[serde(default)]
    promo_text: String,
    /// "What's new" — App Store (4000) and Play release notes (500)
    #[serde(default)]
    whats_new: String,
    /// Google Play short description (80)
    #[serde(default)]
    short_description: String,
}

/// One step of shipping a release, for the Submit step's history.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct ReleaseRecord {
    /// UTC, "YYYY-MM-DD HH:MM"
    at: String,
    /// "ios" / "android"
    platform: String,
    version: String,
    build: u32,
    /// e.g. "Uploaded build", "Submitted for review", "Released to internal (draft)"
    action: String,
}

fn default_play_track() -> String { "internal".to_string() }
fn default_play_status() -> String { "draft".to_string() }

fn default_true() -> bool { true }
fn default_locale() -> String { "en-US".to_string() }
fn default_locales() -> Vec<String> { vec!["en-US".to_string()] }
fn default_ios_targets() -> Vec<bool> { vec![true; IOS_TARGETS.len()] }
fn default_android_targets() -> Vec<bool> { vec![true; ANDROID_TARGETS.len()] }
fn default_desktop_targets() -> Vec<bool> { vec![true; DESKTOP_TARGETS.len()] }

impl ProjectState {
    fn with_defaults() -> Self {
        Self {
            primary_color: "#3B82F6".to_string(),
            secondary_color: "#FFFFFF".to_string(),
            locale: default_locale(),
            locales: default_locales(),
            export_ios: true,
            export_android: true,
            ios_targets: default_ios_targets(),
            android_targets: default_android_targets(),
            desktop_targets: default_desktop_targets(),
            auto_increment_build: true,
            play_track: default_play_track(),
            play_release_status: default_play_status(),
            ..Default::default()
        }
    }
    /// Ensure ios_targets / android_targets / desktop_targets vecs are the right length.
    /// Also migrate legacy `manual_texts` + `locale` into `locale_texts` / `locales`.
    fn normalize_targets(&mut self) {
        while self.ios_targets.len() < IOS_TARGETS.len() { self.ios_targets.push(true); }
        self.ios_targets.truncate(IOS_TARGETS.len());
        while self.android_targets.len() < ANDROID_TARGETS.len() { self.android_targets.push(true); }
        self.android_targets.truncate(ANDROID_TARGETS.len());
        while self.desktop_targets.len() < DESKTOP_TARGETS.len() { self.desktop_targets.push(true); }
        self.desktop_targets.truncate(DESKTOP_TARGETS.len());
    }
    /// Migrate old shared data into the per-locale maps.
    fn migrate_legacy(&mut self) {
        // Ensure `locales` is non-empty; default to en-US if blank
        if self.locales.is_empty() {
            self.locales = vec!["en-US".to_string()];
        }
        // Always ensure en-US is present as the default tab
        if !self.locales.contains(&"en-US".to_string()) {
            self.locales.insert(0, "en-US".to_string());
        }
        // Migrate legacy shared source_paths → en-US locale_sources.
        // IMPORTANT: drain() clears source_paths so re-opening the project never
        // re-adds paths the user has already deleted from locale_sources.
        if !self.source_paths.is_empty() {
            let entry = self.locale_sources
                .entry("en-US".to_string())
                .or_insert_with(Vec::new);
            for p in self.source_paths.drain(..) {
                if !entry.contains(&p) { entry.push(p); }
            }
            // source_paths is now empty — caller should persist this to disk.
        }
        // Migrate legacy shared manual_texts → en-US locale_texts
        if self.locale_texts.get("en-US").map(|v| v.is_empty()).unwrap_or(true)
            && !self.manual_texts.is_empty()
        {
            self.locale_texts.insert("en-US".to_string(), self.manual_texts.clone());
        }
        // Seed locale_texts / locale_sources entries for every listed locale
        for loc in self.locales.clone() {
            let n = self.locale_sources.get(&loc).map(|v| v.len()).unwrap_or(0);
            self.ensure_texts_len(&loc, n);
            self.locale_sources.entry(loc).or_insert_with(Vec::new);
        }
        // Keep legacy locale field in sync
        if let Some(first) = self.locales.first() {
            self.locale = first.clone();
        }
        // Migrate legacy single bundle_id → ios_bundle_id / android_bundle_id.
        // Only copy when the new fields are still empty (first open of an old project).
        if !self.bundle_id.is_empty() {
            if self.ios_bundle_id.is_empty() {
                self.ios_bundle_id = self.bundle_id.clone();
            }
            if self.android_bundle_id.is_empty() {
                self.android_bundle_id = self.bundle_id.clone();
            }
        }
    }
    /// Return the source paths for a given locale.
    fn sources_for(&self, locale: &str) -> Vec<PathBuf> {
        self.locale_sources.get(locale).cloned().unwrap_or_default()
    }
    /// Return the texts for a given locale.
    fn texts_for(&self, locale: &str) -> Vec<(String, String)> {
        self.locale_texts.get(locale).cloned().unwrap_or_default()
    }
    /// Ensure the text vec for `locale` has at least `n` entries.
    fn ensure_texts_len(&mut self, locale: &str, n: usize) {
        let v = self.locale_texts.entry(locale.to_string()).or_insert_with(Vec::new);
        while v.len() < n { v.push((String::new(), String::new())); }
    }
}

fn project_state_path(project_dir: &PathBuf) -> PathBuf {
    project_dir.join("appscreens.json")
}

fn load_project_state(project_dir: &PathBuf) -> ProjectState {
    let mut state = std::fs::read_to_string(project_state_path(project_dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(ProjectState::with_defaults);
    state.normalize_targets();
    // Remember whether we have legacy paths to migrate.
    let had_legacy_paths = !state.source_paths.is_empty();
    state.migrate_legacy();
    // If legacy source_paths were migrated, persist the cleaned state immediately.
    // This ensures source_paths is saved as [] so future loads won't re-add images
    // that the user has already deleted from locale_sources.
    if had_legacy_paths {
        save_project_state(project_dir, &state);
    }
    state
}

fn save_project_state(project_dir: &PathBuf, state: &ProjectState) {
    match serde_json::to_string_pretty(state) {
        Ok(json) => {
            if let Err(e) = std::fs::write(project_state_path(project_dir), &json) {
                eprintln!("[AppScreens] ⚠️  Failed to save project state to {:?}: {e}", project_state_path(project_dir));
            }
        }
        Err(e) => {
            eprintln!("[AppScreens] ⚠️  Failed to serialize project state: {e}");
        }
    }
}

const MAX_THEME_HISTORY: usize = 20;
const MAX_RECENT_PROJECTS: usize = 8;

// ---------------------------------------------------------------------------
// Build script templates
// Only the top variable block differs between projects — the rest is preserved
// exactly as the working originals.
// ---------------------------------------------------------------------------

/// Generate the script content for `build_ios_distribution.sh`.
/// Only the 4 top variables are substituted; everything else is verbatim.
fn script_ios_distribution(
    app_name: &str,
    project_slug: &str,
    bundle_id: &str,
    identity: &str,
    profile_path: &str,
    short_version: &str,
    build_number: u32,
) -> String {
    format!(r##"#!/bin/bash

# build_ios_distribution.sh - Consolidated Build Script for {app_name}
# Handles building, validation fixes, signing (with Entitlements), and packaging.

set -e

APP_NAME="{app_name}"
# Bundle Identifier
BUNDLE_ID="{bundle_id}"

# Signing Identity (Distribution)
IDENTITY="{identity}"

# Paths
OUTPUT_DIR="target/ios/ipa"
DX_IOS_DIR="target/dx/{project_slug}/release/ios"
ENTITLEMENTS="Entitlements.plist"

echo "🚀 Starting iOS Build for App Store Distribution..."

# 1. Prerequisite Checks
if ! command -v dx &>/dev/null; then
  echo "❌ Dioxus CLI (dx) not found. Please install it."
  exit 1
fi

# 1.5 Normalize icon to genuine PNG format
# Apple's validator reads the actual file format, not just the extension.
# A JPEG renamed to .png will pass the filename check but fail validation.
if [ -f "assets/icon.png" ]; then
  sips -s format png "assets/icon.png" --out "assets/icon.png" >/dev/null 2>&1
  echo "✅ Icon normalized to true PNG format."
fi

# 2. Build Rust Project for iOS (Release)
echo "🧹 Cleaning old builds..."
rm -rf target/dx/{project_slug}/release/ios
rm -rf target/dx/{project_slug}/release/web # Clean web assets cache too

echo "📦 Building Rust project for iOS (Release - Device)..."
# Force aarch64-apple-ios to avoid simulator slices
dx build --platform ios --release --target aarch64-apple-ios

# 3. Locate Generated App Bundle (must be after dx build)
APP_BUNDLE=$(find "$DX_IOS_DIR" -maxdepth 1 -name "*.app" -type d 2>/dev/null | head -1)
if [ -z "$APP_BUNDLE" ] || [ ! -d "$APP_BUNDLE" ]; then
  echo "❌ Could not find generated .app bundle in $DX_IOS_DIR"
  ls "$DX_IOS_DIR" 2>/dev/null || echo "   (directory does not exist)"
  exit 1
fi
echo "✅ Found App Bundle: $APP_BUNDLE"

# 4. Prepare Payload Directory
echo "📦 Packaging IPA structure..."
mkdir -p "$OUTPUT_DIR"
rm -rf "$OUTPUT_DIR/Payload"
mkdir -p "$OUTPUT_DIR/Payload"

# Copy App Bundle to Payload
cp -R "$APP_BUNDLE" "$OUTPUT_DIR/Payload/"

# Define paths for the copied app
APP_PATH="$OUTPUT_DIR/Payload/$(basename "$APP_BUNDLE")"
PLIST_PATH="$APP_PATH/Info.plist"

# 4.5 Generate App Icons via actool → Assets.car + correct plist
# The root cause of "Missing icon" validation failures was that actool requires
# the output directory to exist before running, and was silently failing.
echo "🎨 Generating App Icons..."
ICON_SOURCE="assets/icon.png"

if [ -f "$ICON_SOURCE" ]; then
  echo "   Found source icon: $ICON_SOURCE"

  # Convert to true PNG and strip alpha (Apple rejects transparency and JPEG-as-PNG)
  FLAT_ICON="/tmp/icon_flat_$$.png"
  sips -s format png "$ICON_SOURCE" --out "$FLAT_ICON" >/dev/null 2>&1
  echo "   Converted to true PNG."

  # Build asset catalog in temp dir
  CATALOG_DIR="/tmp/appscreens_icons_$$"
  ICON_SET="$CATALOG_DIR/Assets.xcassets/AppIcon.appiconset"
  mkdir -p "$ICON_SET"

  # iPhone sizes
  sips -z 40   40   "$FLAT_ICON" --out "$ICON_SET/Icon-20@2x.png"      >/dev/null
  sips -z 60   60   "$FLAT_ICON" --out "$ICON_SET/Icon-20@3x.png"      >/dev/null
  sips -z 58   58   "$FLAT_ICON" --out "$ICON_SET/Icon-29@2x.png"      >/dev/null
  sips -z 87   87   "$FLAT_ICON" --out "$ICON_SET/Icon-29@3x.png"      >/dev/null
  sips -z 80   80   "$FLAT_ICON" --out "$ICON_SET/Icon-40@2x.png"      >/dev/null
  sips -z 120  120  "$FLAT_ICON" --out "$ICON_SET/Icon-40@3x.png"      >/dev/null
  sips -z 120  120  "$FLAT_ICON" --out "$ICON_SET/Icon-60@2x.png"      >/dev/null
  sips -z 180  180  "$FLAT_ICON" --out "$ICON_SET/Icon-60@3x.png"      >/dev/null
  # iPad sizes
  sips -z 20   20   "$FLAT_ICON" --out "$ICON_SET/Icon-ipad-20@1x.png" >/dev/null
  sips -z 40   40   "$FLAT_ICON" --out "$ICON_SET/Icon-ipad-20@2x.png" >/dev/null
  sips -z 29   29   "$FLAT_ICON" --out "$ICON_SET/Icon-ipad-29@1x.png" >/dev/null
  sips -z 58   58   "$FLAT_ICON" --out "$ICON_SET/Icon-ipad-29@2x.png" >/dev/null
  sips -z 40   40   "$FLAT_ICON" --out "$ICON_SET/Icon-ipad-40@1x.png" >/dev/null
  sips -z 80   80   "$FLAT_ICON" --out "$ICON_SET/Icon-ipad-40@2x.png" >/dev/null
  sips -z 76   76   "$FLAT_ICON" --out "$ICON_SET/Icon-ipad-76@1x.png" >/dev/null
  sips -z 152  152  "$FLAT_ICON" --out "$ICON_SET/Icon-ipad-76@2x.png" >/dev/null
  sips -z 167  167  "$FLAT_ICON" --out "$ICON_SET/Icon-ipad-83@2x.png" >/dev/null
  # App Store
  sips -z 1024 1024 "$FLAT_ICON" --out "$ICON_SET/Icon-1024.png"       >/dev/null
  rm -f "$FLAT_ICON"

  cat > "$ICON_SET/Contents.json" << 'ICONEOF'
{{
  "images": [
    {{"idiom":"iphone","scale":"2x","size":"20x20","filename":"Icon-20@2x.png"}},
    {{"idiom":"iphone","scale":"3x","size":"20x20","filename":"Icon-20@3x.png"}},
    {{"idiom":"iphone","scale":"2x","size":"29x29","filename":"Icon-29@2x.png"}},
    {{"idiom":"iphone","scale":"3x","size":"29x29","filename":"Icon-29@3x.png"}},
    {{"idiom":"iphone","scale":"2x","size":"40x40","filename":"Icon-40@2x.png"}},
    {{"idiom":"iphone","scale":"3x","size":"40x40","filename":"Icon-40@3x.png"}},
    {{"idiom":"iphone","scale":"2x","size":"60x60","filename":"Icon-60@2x.png"}},
    {{"idiom":"iphone","scale":"3x","size":"60x60","filename":"Icon-60@3x.png"}},
    {{"idiom":"ipad","scale":"1x","size":"20x20","filename":"Icon-ipad-20@1x.png"}},
    {{"idiom":"ipad","scale":"2x","size":"20x20","filename":"Icon-ipad-20@2x.png"}},
    {{"idiom":"ipad","scale":"1x","size":"29x29","filename":"Icon-ipad-29@1x.png"}},
    {{"idiom":"ipad","scale":"2x","size":"29x29","filename":"Icon-ipad-29@2x.png"}},
    {{"idiom":"ipad","scale":"1x","size":"40x40","filename":"Icon-ipad-40@1x.png"}},
    {{"idiom":"ipad","scale":"2x","size":"40x40","filename":"Icon-ipad-40@2x.png"}},
    {{"idiom":"ipad","scale":"1x","size":"76x76","filename":"Icon-ipad-76@1x.png"}},
    {{"idiom":"ipad","scale":"2x","size":"76x76","filename":"Icon-ipad-76@2x.png"}},
    {{"idiom":"ipad","scale":"2x","size":"83.5x83.5","filename":"Icon-ipad-83@2x.png"}},
    {{"idiom":"ios-marketing","scale":"1x","size":"1024x1024","filename":"Icon-1024.png"}}
  ],
  "info": {{"author":"xcode","version":1}}
}}
ICONEOF

  # APP_PATH already exists (bundle was copied there). actool writes Assets.car into it.
  PARTIAL_PLIST="/tmp/partial_info_$$.plist"
  echo "   Compiling asset catalog with actool..."
  xcrun actool "$CATALOG_DIR/Assets.xcassets" \
    --compile "$APP_PATH" \
    --platform iphoneos \
    --minimum-deployment-target 14.0 \
    --app-icon AppIcon \
    --output-partial-info-plist "$PARTIAL_PLIST" 2>&1 | grep -v "^$" || true

  if [ -f "$PARTIAL_PLIST" ]; then
    # Remove existing icon keys then merge actool-generated ones
    /usr/libexec/PlistBuddy -c "Delete :CFBundleIcons" "$PLIST_PATH" 2>/dev/null || true
    /usr/libexec/PlistBuddy -c "Delete :CFBundleIcons~ipad" "$PLIST_PATH" 2>/dev/null || true
    /usr/libexec/PlistBuddy -c "Delete :CFBundleIconName" "$PLIST_PATH" 2>/dev/null || true
    /usr/libexec/PlistBuddy -c "Merge $PARTIAL_PLIST" "$PLIST_PATH"
    rm -f "$PARTIAL_PLIST"
    echo "✅ Assets.car generated and Info.plist updated."
  else
    echo "⚠️  actool did not produce a partial plist."
  fi

  rm -rf "$CATALOG_DIR"
else
  echo "⚠️  Warning: assets/icon.png not found. Skipping icon generation."
fi

echo "🔧 Fixing Info.plist for App Store Validation..."

# Remove empty lines at start of file
sed -i '' '/^$/d' "$PLIST_PATH"

# Inject Bundle Metadata
plutil -replace CFBundleIdentifier -string "$BUNDLE_ID" "$PLIST_PATH"
plutil -replace CFBundleDisplayName -string "{app_name}" "$PLIST_PATH"

# Build Number — owned by AppScreens (Version step), injected per build
BUILD_NUMBER="{build_number}"
echo "   Version {short_version} ($BUILD_NUMBER)"

plutil -replace CFBundleVersion -string "$BUILD_NUMBER" "$PLIST_PATH"
plutil -replace CFBundleShortVersionString -string "{short_version}" "$PLIST_PATH"
plutil -replace MinimumOSVersion -string "14.0" "$PLIST_PATH"
plutil -replace CFBundlePackageType -string "APPL" "$PLIST_PATH"

# Enforce Single Supported Platform (Fixes Validation Error 90562)
plutil -replace CFBundleSupportedPlatforms -xml "<array><string>iPhoneOS</string></array>" "$PLIST_PATH"

# Encryption Export Compliance (Fixes Missing Compliance Warning)
plutil -replace ITSAppUsesNonExemptEncryption -bool NO "$PLIST_PATH"

# Inject UILaunchScreen (Requires iOS 14.0+)
plutil -remove UILaunchStoryboardName "$PLIST_PATH" || true
plutil -replace UILaunchScreen -xml "<dict>
        <key>UIColorName</key>
        <string>LaunchBackgroundColor</string>
        <key>UIImageName</key>
        <string>LaunchImage</string>
    </dict>" "$PLIST_PATH"

# Dynamic SDK/Platform Metadata
SDK_VERSION=$(xcrun --sdk iphoneos --show-sdk-version)
SDK_BUILD=$(xcrun --sdk iphoneos --show-sdk-build-version)

XCODE_VERSION_RAW=$(xcodebuild -version | grep "Xcode" | awk '{{print $2}}')
XCODE_BUILD=$(xcodebuild -version | grep "Build version" | awk '{{print $3}}')

MAJOR=$(echo "$XCODE_VERSION_RAW" | cut -d. -f1)
MINOR=$(echo "$XCODE_VERSION_RAW" | cut -d. -f2)
DT_XCODE="${{MAJOR}}${{MINOR}}0"

echo "   Detected Xcode: $XCODE_VERSION_RAW ($XCODE_BUILD) -> DTXcode: $DT_XCODE"
echo "   Detected SDK: iOS $SDK_VERSION ($SDK_BUILD)"

plutil -replace DTPlatformName -string "iphoneos" "$PLIST_PATH"
plutil -replace DTPlatformVersion -string "$SDK_VERSION" "$PLIST_PATH"
plutil -replace DTSDKName -string "iphoneos$SDK_VERSION" "$PLIST_PATH"
plutil -replace DTSDKBuild -string "$SDK_BUILD" "$PLIST_PATH"
plutil -replace DTPlatformBuild -string "$SDK_BUILD" "$PLIST_PATH"

plutil -replace DTXcode -string "$DT_XCODE" "$PLIST_PATH"
plutil -replace DTXcodeBuild -string "$XCODE_BUILD" "$PLIST_PATH"
plutil -replace DTCompiler -string "com.apple.compilers.llvm.clang.1_0" "$PLIST_PATH"

# 6. Embed Provisioning Profile
# Priority order:
#   1. ~/Downloads/<AppName>.mobileprovision  (project-specific, freshly downloaded)
#   2. AppScreens global profile selection
#   3. embedded.mobileprovision in project root
#   4. Any .mobileprovision in project root
GLOBAL_PROFILE="{profile_path}"
NAMED_PROFILE=$(ls "$HOME/Downloads/{app_name}.mobileprovision" 2>/dev/null | head -1)

if [ -f "$NAMED_PROFILE" ]; then
  echo "📄 Using project profile from Downloads: $NAMED_PROFILE"
  cp "$NAMED_PROFILE" "$APP_PATH/embedded.mobileprovision"
elif [ -f "$GLOBAL_PROFILE" ]; then
  echo "📄 Using global profile: $GLOBAL_PROFILE"
  cp "$GLOBAL_PROFILE" "$APP_PATH/embedded.mobileprovision"
elif [ -f "embedded.mobileprovision" ]; then
  echo "📄 Found embedded.mobileprovision in root"
  cp "embedded.mobileprovision" "$APP_PATH/embedded.mobileprovision"
else
  PROVISION_PROFILE=$(find . -maxdepth 1 -name "*.mobileprovision" | head -n 1)
  if [ -f "$PROVISION_PROFILE" ]; then
    echo "📄 Found profile: $PROVISION_PROFILE"
    cp "$PROVISION_PROFILE" "$APP_PATH/embedded.mobileprovision"
  else
    echo "❌  ERROR: No .mobileprovision file found. Signing will fail!"
    exit 1
  fi
fi

# 6.5 Remove stale/duplicate assets
echo "🧹 Removing stale assets from app bundle..."
CSS_COUNT=$(find "$APP_PATH/assets" -name "*.css" 2>/dev/null | wc -l | tr -d ' ')
if [ "$CSS_COUNT" -gt 1 ]; then
  echo "   ⚠️  Found $CSS_COUNT CSS files - keeping only the newest"
  find "$APP_PATH/assets" -name "*.css" -print0 | xargs -0 ls -t | tail -n +2 | xargs rm -f
  echo "   ✅ Cleaned stale CSS files"
fi

# 7. Codesign with Entitlements
echo "✍️  Signing with identity: $IDENTITY"
echo "   Entitlements: $ENTITLEMENTS"

if [ ! -f "$ENTITLEMENTS" ]; then
  echo "❌ Entitlements file not found at $ENTITLEMENTS"
  exit 1
fi

# Remove existing signature
rm -rf "$APP_PATH/_CodeSignature"

# Codesign
codesign --force --deep --sign "$IDENTITY" --entitlements "$ENTITLEMENTS" --timestamp "$APP_PATH"

# Verify signature
echo "🔍 Verifying signature..."
codesign --verify --deep --strict --verbose=4 "$APP_PATH" 2>&1 || {{
  echo "❌ Signature verification failed"
  exit 1
}}

# 8. Create IPA
echo "📦 Creating .ipa..."
rm -f "$OUTPUT_DIR/$APP_NAME.ipa"
rm -f "./$APP_NAME.ipa"
pushd "$OUTPUT_DIR" >/dev/null
zip -r "$APP_NAME.ipa" Payload >/dev/null
popd >/dev/null

rm -rf "$OUTPUT_DIR/Payload"

echo "✅ Build Complete!"
echo "📂 IPA location: $OUTPUT_DIR/$APP_NAME.ipa"

cp "$OUTPUT_DIR/$APP_NAME.ipa" "./$APP_NAME.ipa"
echo "✅ IPA copied to project root: ./$APP_NAME.ipa"
open -R "./$APP_NAME.ipa"
"##)
}

fn script_android_release(
    app_name: &str,
    project_slug: &str,
    bundle_id: &str,
    version_name: &str,
    version_code: u32,
) -> String {
    // Generate right keystore config based on project_slug
    let slug_lower = project_slug.to_lowercase();
    let is_abjad = slug_lower == "abjad";
    // abjad has its own legacy keystore; all other apps share mayorana-release.keystore.
    let keystore_path = if is_abjad {
        "$HOME/code/temp_antigravity_abjad/keystores/reset_upload_key.jks"
    } else {
        "$HOME/code/dioxus/keystores/mayorana-release.keystore"
    };
    let key_alias_val = if is_abjad { "upload".to_string() } else { slug_lower.clone() };
    // Password for mayorana-release.keystore; abjad uses its own keystore with "android".
    let key_pass_snippet = if is_abjad { r#"KEY_PASS="android""# } else { r#"KEY_PASS="Salma2026!""# };

    // android_bundle_id is passed directly — callers already resolved iOS vs Android split.
    let android_package = bundle_id;

    // OLD_PACKAGE is what dx generates by default (com.example.Title-cased slug)
    let title_case = {
        let mut c = project_slug.chars();
        match c.next() {
            None => String::new(),
            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        }
    };
    let old_package = format!("com.example.{title_case}");
    format!(r##"#!/bin/bash

# build_android_release.sh - Build Signed Android App Bundle (AAB) for Google Play
set -e

PROJECT_NAME="{project_slug}"
KEYSTORE_PATH="{keystore_path}"
KEY_ALIAS="{key_alias_val}"
{key_pass_snippet}

export ANDROID_HOME="$HOME/Library/Android/sdk"
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk"
NDK_VERSION=$(ls -1 "$ANDROID_NDK_HOME" 2>/dev/null | grep -E '^[0-9]+\.' | sort -V | tail -1)
export ANDROID_NDK_HOME="$ANDROID_NDK_HOME/$NDK_VERSION"

echo "🚀 Starting Android Release Build (AAB)..."
echo "NDK: $NDK_VERSION"

# 0. Check keystore
if [ ! -f "$KEYSTORE_PATH" ]; then
    echo "❌ Keystore not found: $KEYSTORE_PATH"
    echo "   Create it with:"
    echo "   keytool -genkey -v -keystore \$KEYSTORE_PATH -alias $KEY_ALIAS -keyalg RSA -keysize 2048 -validity 10000"
    exit 1
fi

# 1. Start Build
echo "🧹 Cleaning previous release build..."
rm -rf "target/dx/$PROJECT_NAME/release/android"

echo "📦 Running dx bundle..."
dx bundle --platform android --release || true

# 2. Resource & Icon Fixes
echo "🎨 Fixing resources and icons..."
RES_DIR="target/dx/$PROJECT_NAME/release/android/app/app/src/main/res"
SOURCE_ICONS="manual_assets/android_icons"

if [ -d "$RES_DIR" ] && [ -d "$SOURCE_ICONS" ]; then
    find "$RES_DIR" -name "ic_launcher.webp" -delete
    find "$RES_DIR" -name "ic_launcher_round.webp" -delete
    # Remove adaptive icon XMLs — they override PNGs on Android 8+ and show the default robot icon
    rm -f "$RES_DIR/mipmap-anydpi-v26/ic_launcher.xml"
    rm -f "$RES_DIR/mipmap-anydpi-v26/ic_launcher_round.xml"
    cp -r "$SOURCE_ICONS/mipmap-"* "$RES_DIR/"
    echo "✅ Icons updated."
else
    echo "⚠️  Warning: Resources directory not found."
fi

# 3. Package Name Fixes
echo "🔧 Fixing Package Name..."
BUILD_DIR="target/dx/$PROJECT_NAME/release/android/app"
BUILD_GRADLE="$BUILD_DIR/app/build.gradle.kts"
OLD_PACKAGE="{old_package}"
NEW_PACKAGE="{android_package}"

# Version info — owned by AppScreens (Version step), injected per build.
# Mirrored into Dioxus.toml so the project's own config stays in step.
V_CODE={version_code}
V_NAME="{version_name}"
sed -i '' "s/version_code *= *[0-9]*/version_code = $V_CODE/g" Dioxus.toml
sed -i '' "s/version_name *= *\".*\"/version_name = \"$V_NAME\"/g" Dioxus.toml

echo "   Package: $NEW_PACKAGE"
echo "   Version: $V_NAME ($V_CODE)"

if grep -q "$OLD_PACKAGE" "$BUILD_GRADLE" || grep -q "versionCode" "$BUILD_GRADLE"; then
    echo "   Updating build.gradle.kts..."
    # Fix package name
    sed -i '' "s/$OLD_PACKAGE/$NEW_PACKAGE/g" "$BUILD_GRADLE"
    
    # Fix Version Code
    sed -i '' "s/versionCode *= *[0-9]*/versionCode = $V_CODE/g" "$BUILD_GRADLE"
    
    # Fix Version Name
    sed -i '' "s/versionName *= *\".*\"/versionName = \"$V_NAME\"/g" "$BUILD_GRADLE"

    find "$BUILD_DIR/app/src" -name "*.kt" -type f -exec sed -i '' "s/$OLD_PACKAGE/$NEW_PACKAGE/g" {{}} \;
    find "$BUILD_DIR/app/src" -name "AndroidManifest.xml" -type f -exec sed -i '' "s/$OLD_PACKAGE/$NEW_PACKAGE/g" {{}} \;
    find "$BUILD_DIR/app/src" -name "AndroidManifest.xml" -type f -exec sed -i '' "s/android:label=\\\"@string\\/app_name\\\"/android:label=\\\"{app_name}\\\"/g" {{}} \;
fi

# 4. Inject Signing Config
echo "✍️  Injecting Signing Configuration..."

if ! grep -q "signingConfigs" "$BUILD_GRADLE"; then
    ABS_KEYSTORE_PATH="$KEYSTORE_PATH"

    if [[ "$(cat "$BUILD_GRADLE")" == *"buildTypes"* ]]; then
        echo "   Patching existing buildTypes..."
        sed -i '' "/buildTypes {{/i \\
        signingConfigs {{\\
            create(\"release\") {{\\
                storeFile = file(\"$ABS_KEYSTORE_PATH\")\\
                storePassword = \"$KEY_PASS\"\\
                keyAlias = \"$KEY_ALIAS\"\\
                keyPassword = \"$KEY_PASS\"\\
            }}\\
        }}\\
" "$BUILD_GRADLE"

        sed -i '' "/getByName(\"release\") {{/a \\
            signingConfig = signingConfigs.getByName(\"release\")
" "$BUILD_GRADLE"
    else
        echo "⚠️  Could not find buildTypes block."
    fi
else
    echo "ℹ️  Signing config appears already present."
fi

# 5. Build AAB with Gradle
echo "🏗️  Building Android App Bundle..."
cd "$BUILD_DIR"
./gradlew bundleRelease

# 6. Verify and Move
# Standard Gradle output for signed release is 'app-release.aab'
# Do NOT use 'find' because it picks up the unsigned architecture-specific bundles instead of the final signed universal bundle.
OUTPUT_AAB="app/build/outputs/bundle/release/app-release.aab"
if [ -f "$OUTPUT_AAB" ]; then
    echo "✅ AAB Generated: $OUTPUT_AAB"
    cd - > /dev/null
    TARGET_NAME="${{PROJECT_NAME}}_release.aab"
    cp "$BUILD_DIR/$OUTPUT_AAB" "./$TARGET_NAME"
    echo "📦 Final Bundle copied to: ./$TARGET_NAME"

    echo "🔍 Verifying Signature..."
    if jarsigner -verify -verbose -certs "$TARGET_NAME" | grep -q "jar verified"; then
        echo "✅ Signature Verified!"
        echo "🎉 Ready for Google Play Console upload."
        open -R "./$TARGET_NAME"
        open "$(pwd)"
    else
        echo "❌ Signature Verification FAILED."
        exit 1
    fi
else
    echo "❌ Build Failed: AAB not found in app/build/outputs/bundle/release/"
    ls "$BUILD_DIR/app/build/outputs/bundle/release/" 2>/dev/null || echo "(directory does not exist)"
    exit 1
fi
"##)
}

/// Generate `build_android.sh` (release APK via Gradle assembleRelease)
fn script_android(_app_name: &str, project_slug: &str, _bundle_id: &str) -> String {
    format!(r##"#!/bin/bash

# Exit on error
set -e

export ANDROID_HOME="$HOME/Library/Android/sdk"
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk"
NDK_VERSION=$(ls -1 "$ANDROID_NDK_HOME" 2>/dev/null | grep -E '^[0-9]+\.' | sort -V | tail -1)
export ANDROID_NDK_HOME="$ANDROID_NDK_HOME/$NDK_VERSION"

echo "=== Building Android APK ==="
echo "NDK: $NDK_VERSION"
echo

if ! dx bundle --platform android --release; then
    echo "⚠️ dx bundle failed. This is expected due to duplicate icons."
    echo "Attempting to resolve duplicate resources..."
fi

TARGET_RES="target/dx/{project_slug}/release/android/app/app/src/main/res"
SOURCE_ICONS="assets/icons/android"

if [ -d "$TARGET_RES" ] && [ -d "$SOURCE_ICONS" ]; then
    echo "🧹 Cleaning up ALL existing launcher icons in target..."
    find "$TARGET_RES" -name "ic_launcher.webp" -delete
    find "$TARGET_RES" -name "ic_launcher.png" -delete
    find "$TARGET_RES" -name "ic_launcher_round.webp" -delete
    find "$TARGET_RES" -name "ic_launcher_round.png" -delete
    echo "📂 Copying correct icons from $SOURCE_ICONS..."
    cp -R "$SOURCE_ICONS/"* "$TARGET_RES/"
else
    echo "❌ Target resources or source icons not found. Build may fail."
fi

echo "Resuming build with Gradle..."
cd target/dx/{project_slug}/release/android/app
./gradlew assembleRelease
cd -

echo
echo "✅ Build complete"
"##)
}

/// Generate `build_apk.sh` (debug/installable APK)
fn script_build_apk(app_name: &str, project_slug: &str, bundle_id: &str) -> String {
    // android_bundle_id is passed directly — callers already resolved iOS vs Android split.
    let android_package = bundle_id;
    let title_case = {
        let mut c = project_slug.chars();
        match c.next() {
            None => String::new(),
            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        }
    };
    let old_package = format!("com.example.{title_case}");
    format!(r##"#!/bin/bash

# build_apk.sh - Build Android APK (Debug/Installable) for {app_name}
# Use this for local testing on a device/emulator

set -e

export ANDROID_HOME="$HOME/Library/Android/sdk"
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk"
NDK_VERSION=$(ls -1 "$ANDROID_NDK_HOME" 2>/dev/null | grep -E '^[0-9]+\.' | sort -V | tail -1)
export ANDROID_NDK_HOME="$ANDROID_NDK_HOME/$NDK_VERSION"
BUILD_TOOLS_HOME="$ANDROID_HOME/build-tools"
LATEST_BUILD_TOOLS=$(ls -1 "$BUILD_TOOLS_HOME" 2>/dev/null | sort -V | tail -1)
ANDROID_BUILD_TOOLS="$BUILD_TOOLS_HOME/$LATEST_BUILD_TOOLS"
ZIPALIGN="$ANDROID_BUILD_TOOLS/zipalign"
APKSIGNER="$ANDROID_BUILD_TOOLS/apksigner"

PROJECT_NAME="{project_slug}"

echo "=== Building Android APK (Debug) for $PROJECT_NAME ==="
echo "NDK: $NDK_VERSION"
echo "Build Tools: $LATEST_BUILD_TOOLS"
echo

echo "🧹 Cleaning previous build..."
rm -rf "target/dx/$PROJECT_NAME/release/android"

dx build --platform android --release

echo "🔧 Copying custom icons..."
RES_DIR="target/dx/$PROJECT_NAME/release/android/app/app/src/main/res"
find "$RES_DIR" -name "ic_launcher.webp" -delete
find "$RES_DIR" -name "ic_launcher_round.webp" -delete
# Remove adaptive icon XMLs — they override PNGs on Android 8+ and show the default robot icon
rm -f "$RES_DIR/mipmap-anydpi-v26/ic_launcher.xml"
rm -f "$RES_DIR/mipmap-anydpi-v26/ic_launcher_round.xml"
cp -r manual_assets/android_icons/mipmap-* "$RES_DIR/"
echo "✅ Icons copied successfully."

BUILD_DIR="target/dx/$PROJECT_NAME/release/android/app"
BUILD_GRADLE="$BUILD_DIR/app/build.gradle.kts"
OLD_PACKAGE="{old_package}"
NEW_PACKAGE="{android_package}"

if grep -q "$OLD_PACKAGE" "$BUILD_GRADLE"; then
    echo "⚠️  Incorrect package name detected ($OLD_PACKAGE)."
    echo "🔧 Applying automatic fix to set package to: $NEW_PACKAGE"
    sed -i '' "s/$OLD_PACKAGE/$NEW_PACKAGE/g" "$BUILD_GRADLE"
    find "$BUILD_DIR/app/src" -name "*.kt" -type f -exec sed -i '' "s/$OLD_PACKAGE/$NEW_PACKAGE/g" {{}} \;
    find "$BUILD_DIR/app/src" -name "AndroidManifest.xml" -type f -exec sed -i '' "s/$OLD_PACKAGE/$NEW_PACKAGE/g" {{}} \;
else
    echo "✅ Package name appears correct."
fi

echo "🔧 Ensuring App Name is '{app_name}'..."
find "$BUILD_DIR/app/src" -name "AndroidManifest.xml" -type f -exec sed -i '' 's/android:label="@string\/app_name"/android:label="{app_name}"/g' {{}} \;

echo "↻ Rebuilding with Gradle..."
CURRENT_DIR=$(pwd)
cd "$BUILD_DIR"
./gradlew clean assembleDebug --configuration-cache
cd "$CURRENT_DIR"

echo
echo "✅ Gradle Build complete"

GENERATED_APK="$(find target/dx -name "*.apk" -type f -exec ls -t {{}} + 2>/dev/null | head -1)"
ALIGNED_APK="$(dirname "$GENERATED_APK")/app-aligned.apk"
SIGNED_APK="$(dirname "$GENERATED_APK")/{project_slug}-signed.apk"

if [[ -f "$GENERATED_APK" ]]; then
    echo "🔧 Optimizing APK..."
    echo "  > Aligning..."
    rm -f "$ALIGNED_APK"
    "$ZIPALIGN" -v -p 4 "$GENERATED_APK" "$ALIGNED_APK" > /dev/null
    echo "  > Signing (v1 + v2)..."
    "$APKSIGNER" sign --ks "$HOME/.android/debug.keystore" \
                      --ks-pass pass:android \
                      --key-pass pass:android \
                      --out "$SIGNED_APK" \
                      "$ALIGNED_APK"
    echo "  > Verifying..."
    "$APKSIGNER" verify "$SIGNED_APK"
    rm "$ALIGNED_APK"
    echo "✅ Signed APK created: $SIGNED_APK"
    touch "$SIGNED_APK"
fi

if [[ -f "$SIGNED_APK" ]]; then
    TARGET_APK="./{project_slug}-debug.apk"
    cp "$SIGNED_APK" "$TARGET_APK"
    echo "📦 APK copied to: $TARGET_APK"
    open -R "$TARGET_APK"
    open "$(pwd)"
else
    APK_PATH="$(find target/dx -name "*.apk" -type f -exec ls -t {{}} + 2>/dev/null | head -1)"
    if [[ -n "$APK_PATH" ]]; then
        TARGET_APK="./{project_slug}-debug.apk"
        cp "$APK_PATH" "$TARGET_APK"
        echo "📦 APK copied to: $TARGET_APK"
        open -R "$TARGET_APK"
        open "$(pwd)"
    fi
fi
"##)
}

/// Always (re)write build scripts from the latest template.
/// This ensures config changes and template fixes are always picked up.
fn ensure_build_scripts(
    project_dir: &PathBuf,
    app_name: &str,
    project_slug: &str,
    ios_bundle_id: &str,
    android_bundle_id: &str,
    identity: &str,
    provisioning_profile: &str,
    version: &str,
    ios_build_number: u32,
    android_version_code: u32,
) -> Vec<String> {
    let scripts: &[(&str, String)] = &[
        (
            "build_ios_distribution.sh",
            script_ios_distribution(app_name, project_slug, ios_bundle_id, identity, provisioning_profile, version, ios_build_number),
        ),
        (
            "build_android_release.sh",
            script_android_release(app_name, project_slug, android_bundle_id, version, android_version_code),
        ),
        (
            "build_android.sh",
            script_android(app_name, project_slug, android_bundle_id),
        ),
        (
            "build_apk.sh",
            script_build_apk(app_name, project_slug, android_bundle_id),
        ),
    ];

    let mut created = Vec::new();
    for (name, content) in scripts {
        let path = project_dir.join(name);
        if std::fs::write(&path, content).is_ok() {
            // Make executable
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
            }
            created.push((*name).to_string());
        }
    }
    created
}

// ---------------------------------------------------------------------------
// Scaffold a brand-new Dioxus project from AppScreens (no pre-run scaffolder needed)
// ---------------------------------------------------------------------------

/// Create the minimal Rust/Dioxus project structure for a new app.
/// Returns Ok(()) or an Err with a description.
fn scaffold_new_project(
    dir: &PathBuf,
    name: &str,
    slug: &str,
    bundle_id: &str,
    platform: &PlatformType,
) -> Result<(), String> {
    let src_dir = dir.join("src");
    let assets_dir = dir.join("assets");

    std::fs::create_dir_all(&src_dir).map_err(|e| format!("Cannot create src/: {e}"))?;
    std::fs::create_dir_all(&assets_dir).map_err(|e| format!("Cannot create assets/: {e}"))?;

    // ── Cargo.toml ────────────────────────────────────────────────────────────
    let features = match platform {
        PlatformType::Desktop    => r#"["desktop"]"#,
        PlatformType::Ios        => r#"["mobile"]"#,
        PlatformType::Android    => r#"["mobile"]"#,
        PlatformType::IosAndroid => r#"["mobile", "router"]"#,
    };
    let cargo_toml = format!(
        "[package]\nname = \"{slug}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [dependencies]\ndioxus = {{ version = \"0.6\", features = {features} }}\n"
    );
    std::fs::write(dir.join("Cargo.toml"), &cargo_toml)
        .map_err(|e| format!("Cannot write Cargo.toml: {e}"))?;

    // ── Dioxus.toml ───────────────────────────────────────────────────────────
    let default_platform = match platform {
        PlatformType::Desktop => "desktop",
        _                     => "mobile",
    };
    let dioxus_toml = format!(
        "[application]\nname = \"{slug}\"\ndefault_platform = \"{default_platform}\"\n\n\
         [web.app]\ntitle = \"{name}\"\n\n\
         [web.resource]\nstyle = []\nscript = []\n\n\
         [bundle]\nidentifier = \"{bundle_id}\"\npublisher = \"Mayorana\"\n\
         icon = [\"assets/icon.png\"]\nname = \"{name}\"\n"
    );
    std::fs::write(dir.join("Dioxus.toml"), &dioxus_toml)
        .map_err(|e| format!("Cannot write Dioxus.toml: {e}"))?;

    // ── src/main.rs ───────────────────────────────────────────────────────────
    let main_rs = format!(
        "use dioxus::prelude::*;\n\nfn main() {{\n    dioxus::launch(App);\n}}\n\n\
         #[component]\nfn App() -> Element {{\n    rsx! {{\n        div {{\n\
             h1 {{ \"{name}\" }}\n            p {{ \"Hello, world!\" }}\n        }}\n    }}\n}}\n"
    );
    std::fs::write(src_dir.join("main.rs"), &main_rs)
        .map_err(|e| format!("Cannot write src/main.rs: {e}"))?;

    // ── .gitignore ────────────────────────────────────────────────────────────
    let gitignore = "/target\nappscreens.json\n.env\n";
    // Only create if not already present
    let gi_path = dir.join(".gitignore");
    if !gi_path.exists() {
        let _ = std::fs::write(gi_path, gitignore);
    }

    // ── Entitlements.plist (iOS only) ─────────────────────────────────────────
    if platform.has_ios() {
        let entitlements = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\"\n\
             \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n<dict>\n\
             \t<key>application-identifier</key>\n\
             \t<string>{bundle_id}</string>\n\
             \t<key>get-task-allow</key>\n\
             \t<false/>\n\
             </dict>\n</plist>\n"
        );
        let _ = std::fs::write(dir.join("Entitlements.plist"), &entitlements);
        let _ = std::fs::write(dir.join("build_number.txt"), "1\n");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// App phase
// ---------------------------------------------------------------------------
#[derive(Clone, Debug, PartialEq)]
enum AppPhase {
    Idle,
    GeneratingAi,
    GeneratingManual,
    Resizing,
    Done,
    Error(String),
}

// Build script phase
#[derive(Clone, Debug, PartialEq)]
enum BuildPhase {
    Idle,
    Running(String), // which script is running
    Success(String),
    Error(String),
}

// Publish phase (iOS / App Store Connect)
#[derive(Clone, Debug, PartialEq)]
enum PublishPhase {
    Idle,
    Running,
    Success,
    Error(String),
}

// Android publish phase (Google Play via androidpublisher v3)
#[derive(Clone, Debug, PartialEq)]
enum AndroidPublishPhase {
    Idle,
    Running,
    Success,
    Error(String),
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------
fn main() {
    tracing_subscriber::fmt::init();

    let config = dioxus::desktop::Config::new()
        // Injected into index.html's <head> before first paint, so there is no
        // flash of unstyled content and no runtime asset lookup.
        //
        // The viewport meta goes in with it: without it the webview lays the
        // page out in a 980px virtual viewport on Android, so every
        // `@media (max-width: …)` rule in main.css silently never matches on
        // the one platform that needs them.
        .with_custom_head(format!(
            "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0, viewport-fit=cover\">\
             <style>{MAIN_CSS}</style>"
        ))
        .with_window(
            WindowBuilder::new()
                .with_title("AppScreens")
                .with_window_icon(make_icon())
                .with_inner_size(LogicalSize::new(1020.0, 860.0))
                .with_focused(true)
                .with_decorations(true)
                .with_transparent(false),
        );

    LaunchBuilder::desktop().with_cfg(config).launch(App);
}

// ── Procedural window icon (mirrors ais-runner's style) ─────────────────────
// Circular mask + blue→purple gradient + white symbol. Drawn in pure Rust so
// the binary stays self-contained — no PNG asset to ship.
fn make_icon() -> Option<dioxus::desktop::tao::window::Icon> {
    const SIZE: u32 = 64;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            // Circular mask
            let cx = x as f32 - SIZE as f32 / 2.0 + 0.5;
            let cy = y as f32 - SIZE as f32 / 2.0 + 0.5;
            let r_sq = cx * cx + cy * cy;
            let radius = SIZE as f32 / 2.0;
            let alpha = if r_sq > (radius * radius) { 0u8 } else { 255u8 };

            // Gradient: top-left blue → bottom-right purple (AppScreens identity)
            let t = (x + y) as f32 / (SIZE * 2) as f32;
            let r = (0.0_f32 + t * 120.0) as u8;
            let g = (120.0 - t * 60.0) as u8;
            let b = (212.0 - t * 20.0) as u8;

            // White phone-screens shape: two slightly-offset rounded rectangles
            // suggesting the App Store / Play Store dual export.
            let in_shape = is_screens_shape(x, y, SIZE);
            let (r, g, b) = if in_shape { (255u8, 255u8, 255u8) } else { (r, g, b) };

            rgba.extend_from_slice(&[r, g, b, alpha]);
        }
    }
    dioxus::desktop::tao::window::Icon::from_rgba(rgba, SIZE, SIZE).ok()
}

fn is_screens_shape(x: u32, y: u32, size: u32) -> bool {
    let s = size as f32;
    let fx = x as f32;
    let fy = y as f32;

    // Front phone: rounded rect, slight tilt right
    let phone = |cx: f32, cy: f32, w: f32, h: f32, corner: f32| -> bool {
        let left   = cx - w / 2.0;
        let right  = cx + w / 2.0;
        let top    = cy - h / 2.0;
        let bottom = cy + h / 2.0;
        if fx < left || fx > right || fy < top || fy > bottom { return false; }
        // Round the corners
        let dx = if fx < left + corner { left + corner - fx }
                 else if fx > right - corner { fx - (right - corner) }
                 else { 0.0 };
        let dy = if fy < top + corner { top + corner - fy }
                 else if fy > bottom - corner { fy - (bottom - corner) }
                 else { 0.0 };
        dx * dx + dy * dy <= corner * corner
    };

    // Back phone (offset up-left)
    if phone(s * 0.42, s * 0.45, s * 0.32, s * 0.48, s * 0.05) { return true; }
    // Front phone (offset down-right)
    if phone(s * 0.58, s * 0.55, s * 0.32, s * 0.48, s * 0.05) { return true; }
    false
}

// ---------------------------------------------------------------------------
// Root App
// ---------------------------------------------------------------------------
// dark-light has no Android backend (see Cargo.toml) — there's no per-app
// system light/dark signal to read there the way desktop OSes expose one, so
// this just keeps the app on its existing light-by-default behavior (the
// desktop check below also treats "can't tell" as light, via `!= Dark`).
#[cfg(not(target_os = "android"))]
fn system_prefers_light() -> bool {
    dark_light::detect() != dark_light::Mode::Dark
}
#[cfg(target_os = "android")]
fn system_prefers_light() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Icons
//
// These replaced the emoji that used to label buttons and tabs (🚀 Publish,
// 🔨 Build, 📱 Build iOS IPA, 🐛 Build APK, …). Emoji render as full-colour
// glyphs in an otherwise monochrome indigo UI, so they were the loudest thing
// on screen while carrying no meaning, and they vary per platform — a 🐛 on a
// build button reads as "report a bug". Stroke geometry on a 24×24 grid,
// coloured by `currentColor` so each icon inherits whatever accent its control
// already owns. Sizing and stroke live in `.icon` in main.css.
// ---------------------------------------------------------------------------
macro_rules! icon {
    ($name:ident, $($d:expr),+ $(,)?) => {
        fn $name() -> Element {
            rsx! {
                svg { class: "icon", view_box: "0 0 24 24", "aria-hidden": "true",
                    $( path { d: $d } )+
                }
            }
        }
    };
}

icon!(icon_sparkle, "M12 3l1.9 5.1L19 10l-5.1 1.9L12 17l-1.9-5.1L5 10l5.1-1.9z");
icon!(icon_image, "M4 4h16v16H4z", "M4 16l4.5-4.5 3 3 3.5-3.5L20 15");
icon!(icon_upload, "M12 16V4", "M7 9l5-5 5 5", "M4 16v3a2 2 0 002 2h12a2 2 0 002-2v-3");
icon!(icon_phone, "M6 2h12v20H6z", "M11 18.5h2");
icon!(
    icon_package,
    "M21 16V8a2 2 0 00-1-1.73l-7-4a2 2 0 00-2 0l-7 4A2 2 0 003 8v8a2 2 0 001 1.73l7 4a2 2 0 002 0l7-4A2 2 0 0021 16z",
    "M3.3 7l8.7 5 8.7-5",
    "M12 22V12",
);
icon!(icon_download, "M21 15v4a2 2 0 01-2 2H5a2 2 0 01-2-2v-4", "M7 10l5 5 5-5", "M12 15V3");
icon!(icon_play, "M6 3l14 9-14 9z");
icon!(icon_monitor, "M3 4h18v12H3z", "M8 20h8", "M12 16v4");
icon!(
    icon_alert,
    "M10.3 3.9L2 18a2 2 0 001.7 3h16.6a2 2 0 001.7-3L13.7 3.9a2 2 0 00-3.4 0z",
    "M12 9v4.5",
    "M12 17.2h.01",
);
icon!(icon_close, "M18 6L6 18", "M6 6l12 12");
icon!(icon_plus, "M12 5v14", "M5 12h14");
icon!(icon_chevron_left, "M15 5l-7 7 7 7");
icon!(icon_chevron_right, "M9 5l7 7-7 7");
icon!(icon_check, "M20 6L9 17l-5-5");
icon!(icon_sun, "M12 4V2", "M12 22v-2", "M4 12H2", "M22 12h-2", "M5.6 5.6L4.2 4.2", "M19.8 19.8l-1.4-1.4", "M18.4 5.6l1.4-1.4", "M4.2 19.8l1.4-1.4", "M12 7.5a4.5 4.5 0 100 9 4.5 4.5 0 000-9z");
icon!(icon_moon, "M20.5 13.3A8.5 8.5 0 1110.7 3.5a6.6 6.6 0 009.8 9.8z");
icon!(icon_folder_plus, "M21 19a2 2 0 01-2 2H5a2 2 0 01-2-2V5a2 2 0 012-2h4.5l2 3H19a2 2 0 012 2z", "M12 11.5v5", "M9.5 14h5");

#[component]
fn App() -> Element {
    // Global settings
    let mut settings = use_signal(load_settings);
    use_context_provider(|| settings);

    // Active project dir (None = show project picker)
    let mut project_dir = use_signal(|| Option::<PathBuf>::None);

    // ── Theme (matches ais-runner) ────────────────────────────────────────
    // Initialise from system, then keep in sync — but stop syncing once the
    // user manually toggles via the button.
    let mut is_light          = use_signal(system_prefers_light);
    let mut theme_overridden  = use_signal(|| false);

    use_effect(move || {
        let cls = if *is_light.read() { "light" } else { "" };
        document::eval(&format!("document.body.className = '{}';", cls));
    });

    use_coroutine(move |_rx: dioxus::prelude::UnboundedReceiver<()>| async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
            if *theme_overridden.read() { continue; }
            let light = tokio::task::spawn_blocking(system_prefers_light)
                .await
                .unwrap_or(*is_light.read());
            if light != *is_light.read() {
                is_light.set(light);
            }
        }
    });

    // ── Auto-update check ──────────────────────────────────────────────────
    // Background fetch latest.json from GitHub releases on startup; if a newer
    // version is published, surface a small banner. Dismissable per session.
    // ── Notice from mayorana.ch ────────────────────────────────────────────
    // A message to the people running this build (see notice.rs). Same
    // posture as the update check: delayed, best-effort, silent on failure.
    let mut mayorana_notice = use_signal(|| Option::<notice::Notice>::None);
    use_coroutine(move |_rx: dioxus::prelude::UnboundedReceiver<()>| async move {
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        if let Some(n) = notice::fetch().await {
            mayorana_notice.set(Some(n));
        }
    });

    let mut update_info       = use_signal(|| Option::<update_check::UpdateInfo>::None);
    let mut update_dismissed  = use_signal(|| false);

    use_coroutine(move |_rx: dioxus::prelude::UnboundedReceiver<()>| async move {
        // Small delay so we don't compete with the app's own boot work.
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        if let Some(info) = update_check::check().await {
            update_info.set(Some(info));
        }
    });

    // Asset handler for local images (thumbnails)
    use_asset_handler("localimg", |request, responder: RequestAsyncResponder| {
        let encoded = request
            .uri()
            .path()
            .strip_prefix("/localimg/")
            .unwrap_or("")
            .to_string();
        let decoded = urlencoding::decode(&encoded)
            .unwrap_or_default()
            .into_owned();
        let file_path = PathBuf::from(decoded);
        tokio::spawn(async move {
            match tokio::fs::read(&file_path).await {
                Ok(bytes) => {
                    let mime = match file_path.extension().and_then(|e| e.to_str()) {
                        Some("png") => "image/png",
                        Some("jpg") | Some("jpeg") => "image/jpeg",
                        Some("webp") => "image/webp",
                        _ => "image/png",
                    };
                    responder.respond(
                        Response::builder()
                            .status(StatusCode::OK)
                            .header("Content-Type", mime)
                            .header("Access-Control-Allow-Origin", "*")
                            .body(bytes)
                            .unwrap(),
                    );
                }
                Err(_) => {
                    responder.respond(
                        Response::builder()
                            .status(StatusCode::NOT_FOUND)
                            .body(vec![])
                            .unwrap(),
                    );
                }
            }
        });
    });

    rsx! {
        // ── Update banner ──────────────────────────────────────────────────
        // Renders only when a newer release exists AND the user hasn't dismissed
        // it this session. Click "Download" to open the GitHub releases page in
        // the system browser (Dioxus opens external links via the OS handler).
        if let (Some(info), false) = (update_info.read().clone(), *update_dismissed.read()) {
            div { class: "update-banner",
                span { class: "update-banner-text",
                    "AppScreens "
                    strong { "{info.latest_version}" }
                    " is available (you have {env!(\"CARGO_PKG_VERSION\")})."
                }
                a {
                    class: "update-banner-link",
                    href: "{info.release_url}",
                    target: "_blank",
                    "Download"
                }
                button {
                    class: "update-banner-dismiss",
                    title: "Dismiss",
                    aria_label: "Dismiss update notice",
                    onclick: move |_| update_dismissed.set(true),
                    {icon_close()}
                }
            }
        }

        // Notice from mayorana.ch — shown until dismissed, then remembered.
        if let Some(n) = mayorana_notice.read().clone() {
            {
                let id = n.id.clone();
                let link_text = n.link_text.clone().unwrap_or_else(|| "Open".to_string());
                rsx! {
                    div { class: "update-banner notice-banner",
                        span { class: "update-banner-text", "{n.text}" }
                        if let Some(url) = n.url.clone() {
                            a {
                                class: "update-banner-link",
                                href: "{url}",
                                target: "_blank",
                                "{link_text}"
                            }
                        }
                        button {
                            class: "update-banner-dismiss",
                            onclick: move |_| {
                                notice::dismiss(&id);
                                mayorana_notice.set(None);
                            },
                            "×"
                        }
                    }
                }
            }
        }

        // Floating theme toggle, top-right. Same UX as ais-runner.
        button {
            class: "btn-theme",
            title: if *is_light.read() { "Switch to dark mode" } else { "Switch to light mode" },
            aria_label: if *is_light.read() { "Switch to dark mode" } else { "Switch to light mode" },
            onclick: move |_| {
                let next = !*is_light.read();
                theme_overridden.set(true);
                is_light.set(next);
                document::eval(&format!(
                    "document.body.className = '{}';",
                    if next { "light" } else { "" }
                ));
            },
            if *is_light.read() { {icon_moon()} } else { {icon_sun()} }
        }

        if project_dir.read().is_none() {
            ProjectPicker {
                on_open: move |dir: PathBuf| {
                    // Record in recents
                    let mut s = settings.write();
                    s.recent_projects.retain(|p| p != &dir);
                    s.recent_projects.insert(0, dir.clone());
                    s.recent_projects.truncate(MAX_RECENT_PROJECTS);
                    save_settings(&s);
                    drop(s);
                    project_dir.set(Some(dir));
                }
            }
        } else {
            ProjectView {
                project_dir: project_dir.read().clone().unwrap(),
                on_close: move |_| project_dir.set(None),
            }
        }
    }
}

// Android has no folder-browse equivalent (see src/android_saf.rs) — every
// project the user could open was created through this same picker, on the
// one fixed app-private root, so it's already in Recent Projects. Nothing
// external to browse to, so this renders nothing there.
//
// (`#[cfg]` doesn't parse directly on an rsx! element, hence pulling this out
// into its own function rather than cfg'ing the `button {}` in place.)
#[cfg(not(target_os = "android"))]
fn open_existing_button(on_open: EventHandler<PathBuf>) -> Element {
    rsx! {
        button {
            class: "btn picker-open-btn",
            onclick: move |_| {
                spawn(async move {
                    if let Some(folder) = rfd::AsyncFileDialog::new()
                        .set_title("Open existing project folder")
                        .pick_folder()
                        .await
                    {
                        on_open.call(folder.path().to_path_buf());
                    }
                });
            },
            "📂  Open Existing…"
        }
    }
}
#[cfg(target_os = "android")]
fn open_existing_button(_on_open: EventHandler<PathBuf>) -> Element {
    rsx! {}
}

// Meaningless on Android, where new_parent is always already Some (see its
// #[cfg]'d initializer in ProjectPicker).
#[cfg(not(target_os = "android"))]
fn parent_folder_field(mut new_parent: Signal<Option<PathBuf>>) -> Element {
    rsx! {
        div { class: "build-config-field",
            label { class: "build-config-label", "Location" }
            div { class: "folder-pick-row",
                button {
                    class: "btn",
                    onclick: move |_| {
                        spawn(async move {
                            if let Some(folder) = rfd::AsyncFileDialog::new()
                                .set_title("Choose parent folder for new project")
                                .pick_folder()
                                .await
                            {
                                new_parent.set(Some(folder.path().to_path_buf()));
                            }
                        });
                    },
                    "Choose Folder…"
                }
                if let Some(p) = new_parent.read().clone() {
                    span { class: "folder-pick-path", "{p.to_string_lossy()}" }
                } else {
                    span { class: "folder-pick-hint", "No folder selected" }
                }
            }
        }
    }
}
#[cfg(target_os = "android")]
fn parent_folder_field(_new_parent: Signal<Option<PathBuf>>) -> Element {
    rsx! {}
}

// ---------------------------------------------------------------------------
// Project Picker (shown on launch)
// ---------------------------------------------------------------------------
#[component]
fn ProjectPicker(on_open: EventHandler<PathBuf>) -> Element {
    let settings = use_context::<Signal<Settings>>();

    // "New Project" wizard state
    let mut show_new = use_signal(|| false);
    let mut new_name     = use_signal(|| String::new());
    let mut new_slug     = use_signal(|| String::new());
    let mut new_bundle   = use_signal(|| String::new());
    let mut new_platform = use_signal(|| PlatformType::IosAndroid);
    // Desktop: nothing until the user picks a folder. Android: there's no
    // per-project folder choice at all (see src/android_saf.rs) — every
    // project lives under one fixed, app-private root, so this is never
    // empty and the "Location" field below doesn't render.
    #[cfg(not(target_os = "android"))]
    let new_parent = use_signal(|| Option::<PathBuf>::None);
    #[cfg(target_os = "android")]
    let new_parent = use_signal(|| Some(android_saf::app_private_projects_root()));
    let mut create_error = use_signal(|| Option::<String>::None);

    // Derive slug and bundle from name automatically (user can override)
    let auto_slug = {
        let n = new_name.read().to_lowercase();
        n.chars().filter(|c| c.is_alphanumeric() || *c == '_').collect::<String>()
    };
    let auto_bundle = {
        let s = if new_slug.read().is_empty() { auto_slug.clone() } else { new_slug.read().clone() };
        if s.is_empty() { "com.company.app".to_string() } else { format!("com.mayorana.{s}") }
    };

    rsx! {
        div { class: "picker-screen",
            div { class: "picker-inner",
                div { class: "picker-logo",
                    img {
                        class: "picker-logo-mark",
                        src: asset!("/assets/icon.png"),
                        alt: "",
                    }
                    div {
                        h1 { "AppScreens" }
                        p { class: "picker-subtitle", "App Store & Play Store screenshot generator" }
                    }
                }

                // ── Action buttons row ──────────────────────────────────────
                div { class: "picker-actions",
                    button {
                        // Only "New Project…" is primary. When this toggles to
                        // "Cancel" it demotes to a plain button — the strongest
                        // blue on the first screen should never be the dismiss.
                        class: if *show_new.read() { "btn picker-new-btn" } else { "btn btn-primary picker-new-btn" },
                        onclick: move |_| {
                            let currently = *show_new.read();
                            show_new.set(!currently);
                            create_error.set(None);
                        },
                        if *show_new.read() {
                            {icon_close()}
                            "Cancel"
                        } else {
                            {icon_sparkle()}
                            "New Project…"
                        }
                    }
                    {open_existing_button(on_open.clone())}
                }

                // ── New Project wizard (inline) ──────────────────────────────
                if *show_new.read() {
                    div { class: "new-project-panel",
                        h3 { class: "new-project-title", "Create New Project" }

                        // App Name
                        div { class: "build-config-field",
                            label { class: "build-config-label", "App Name" }
                            input {
                                class: "text-input",
                                placeholder: "My App",
                                value: "{new_name.read()}",
                                oninput: move |e: Event<FormData>| {
                                    let v = e.value();
                                    // Auto-derive slug when empty
                                    if new_slug.read().is_empty() {
                                        // leave blank so auto_slug kicks in
                                    }
                                    new_name.set(v);
                                }
                            }
                        }

                        // Slug (auto-derived, editable)
                        div { class: "build-config-field",
                            label { class: "build-config-label", "Project Slug" }
                            input {
                                class: "text-input",
                                placeholder: "{auto_slug}",
                                value: "{new_slug.read()}",
                                oninput: move |e: Event<FormData>| new_slug.set(e.value())
                            }
                            p { class: "settings-hint", "Lowercase identifier — leave blank to auto-derive from App Name" }
                        }

                        // Bundle ID
                        div { class: "build-config-field",
                            label { class: "build-config-label", "Bundle ID" }
                            input {
                                class: "text-input",
                                placeholder: "{auto_bundle}",
                                value: "{new_bundle.read()}",
                                oninput: move |e: Event<FormData>| new_bundle.set(e.value())
                            }
                            p { class: "settings-hint", "Leave blank for com.mayorana.&lt;slug&gt;" }
                        }

                        // Platform selector
                        div { class: "build-config-field",
                            label { class: "build-config-label", "Platform" }
                            div { class: "platform-selector",
                                for plat in [PlatformType::IosAndroid, PlatformType::Ios, PlatformType::Android, PlatformType::Desktop] {
                                    {
                                        let label = plat.label();
                                        let is_selected = *new_platform.read() == plat;
                                        let plat2 = plat.clone();
                                        rsx! {
                                            button {
                                                class: if is_selected { "platform-btn platform-btn-active" } else { "platform-btn" },
                                                onclick: move |_| new_platform.set(plat2.clone()),
                                                "{label}"
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        {parent_folder_field(new_parent)}

                        // Error banner
                        if let Some(err) = create_error.read().clone() {
                            p { class: "new-project-error", "{err}" }
                        }

                        // Create button
                        button {
                            class: "btn btn-primary",
                            onclick: {
                                let on_open2 = on_open.clone();
                                move |_| {
                                    let name_val  = new_name.read().trim().to_string();
                                    let slug_raw  = new_slug.read().trim().to_string();
                                    let slug_val  = if slug_raw.is_empty() { auto_slug.clone() } else { slug_raw };
                                    let bundle_raw = new_bundle.read().trim().to_string();
                                    let bundle_val = if bundle_raw.is_empty() { auto_bundle.clone() } else { bundle_raw };
                                    let plat_val  = new_platform.read().clone();
                                    let parent    = new_parent.read().clone();

                                    if name_val.is_empty() {
                                        create_error.set(Some("App Name is required.".into()));
                                        return;
                                    }
                                    if slug_val.is_empty() {
                                        create_error.set(Some("Could not derive a slug — please fill in Project Slug.".into()));
                                        return;
                                    }
                                    let Some(parent_dir) = parent else {
                                        create_error.set(Some("Please choose a parent folder.".into()));
                                        return;
                                    };

                                    let project_dir = parent_dir.join(&slug_val);
                                    match scaffold_new_project(&project_dir, &name_val, &slug_val, &bundle_val, &plat_val) {
                                        Ok(()) => {
                                            // Pre-populate appscreens.json with the config so
                                            // ProjectView opens with all fields filled in.
                                            let mut state = ProjectState::with_defaults();
                                            state.app_name          = name_val;
                                            state.project_slug      = slug_val;
                                            state.ios_bundle_id     = bundle_val.clone();
                                            state.android_bundle_id = bundle_val;
                                            state.platform_type = plat_val;
                                            // Disable iOS/Android targets for desktop-only projects
                                            if state.platform_type.has_desktop() && !state.platform_type.has_ios() {
                                                state.export_ios     = false;
                                                state.export_android = false;
                                            }
                                            save_project_state(&project_dir, &state);
                                            show_new.set(false);
                                            create_error.set(None);
                                            on_open2.call(project_dir);
                                        }
                                        Err(e) => create_error.set(Some(e)),
                                    }
                                }
                            },
                            {icon_folder_plus()}
                            "Create Project"
                        }
                    }
                }

                // ── Recent Projects ──────────────────────────────────────────
                if !settings.read().recent_projects.is_empty() {
                    div { class: "picker-recents",
                        p { class: "picker-recents-label", "Recent Projects" }
                        for proj in settings.read().recent_projects.clone().iter() {
                            {
                                let proj = proj.clone();
                                let proj2 = proj.clone();
                                let name = proj.file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .to_string();
                                let path_str = proj.to_string_lossy().to_string();
                                rsx! {
                                    button {
                                        class: "picker-recent-item",
                                        onclick: move |_| on_open.call(proj2.clone()),
                                        div { class: "picker-recent-name", "{name}" }
                                        div { class: "picker-recent-path", "{path_str}" }
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
// Settings popup (gear)
// ---------------------------------------------------------------------------
#[component]
fn SettingsPopup(on_close: EventHandler<()>) -> Element {
    let mut settings = use_context::<Signal<Settings>>();

    let fal_key_val = settings.read().fal_key.clone();
    let phone_style_val = settings.read().phone_style.clone();
    let inference_steps_val = settings.read().inference_steps;

    rsx! {
        div { class: "settings-backdrop", onclick: move |_| on_close.call(()) }
        div {
            class: "settings-popup settings-popup-wide",
            role: "dialog",
            aria_modal: "true",
            aria_label: "Settings",
            // Esc closes it. Backdrop-click was the only way out before, which
            // was the sole escape whenever the window was short enough to push
            // the close button off-screen.
            tabindex: "-1",
            onmounted: move |e: Event<MountedData>| {
                spawn(async move { let _ = e.set_focus(true).await; });
            },
            onkeydown: move |e: KeyboardEvent| {
                if e.key() == Key::Escape {
                    e.stop_propagation();
                    on_close.call(());
                }
            },
            div { class: "settings-header",
                h2 { "Settings" }
                button {
                    class: "btn btn-icon",
                    title: "Close settings",
                    aria_label: "Close settings",
                    onclick: move |_| on_close.call(()),
                    {icon_close()}
                }
            }

            // ---- Screenshot Generation ----
            p { class: "settings-section-title", "Screenshot Generation" }

            div { class: "settings-field",
                label { "fal.ai API Key" }
                input {
                    class: "text-input",
                    r#type: "password",
                    placeholder: "Enter your fal.ai API key",
                    value: "{fal_key_val}",
                    oninput: move |e: Event<FormData>| { settings.write().fal_key = e.value(); save_settings(&settings()); },
                }
                p { class: "settings-hint", "Get a key at fal.ai/dashboard/keys" }
            }

            div { class: "settings-field",
                label { "Device Style" }
                input {
                    class: "text-input",
                    placeholder: "modern smartphone",
                    value: "{phone_style_val}",
                    oninput: move |e: Event<FormData>| { settings.write().phone_style = e.value(); save_settings(&settings()); },
                }
                p { class: "settings-hint", "e.g. modern smartphone, iPhone 16, iPad Pro" }
            }

            div { class: "settings-field",
                label { "Inference Steps" }
                input {
                    class: "text-input",
                    r#type: "number",
                    min: "10",
                    max: "50",
                    value: "{inference_steps_val}",
                    oninput: move |e: Event<FormData>| {
                        if let Ok(v) = e.value().parse::<u32>() {
                            settings.write().inference_steps = v.clamp(10, 50);
                            save_settings(&settings());
                        }
                    },
                }
                p { class: "settings-hint", "More steps = better quality but slower (10–50)" }
            }

            div { class: "settings-field settings-path",
                p { class: "settings-hint", "Config: {global_config_path().display()}" }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Prompt builder
// ---------------------------------------------------------------------------
fn build_prompt(
    user_style: &str,
    phone_style: &str,
    screen_index: usize,
    total_screens: usize,
) -> String {
    let consistency_note = if total_screens > 1 {
        format!(
            " This is frame {screen_index} of {total_screens} in a set. \
             ALL frames MUST share the exact same visual theme: same background style, \
             same color palette, same decorative elements, same lighting and mood."
        )
    } else {
        String::new()
    };

    format!(
        "Generate a beautiful app store screenshot presentation frame. \
         Show a {phone_style} device in a front-facing view, centered in the image. \
         The device screen area must be filled with a perfectly uniform solid grey color \
         exactly #808080 with absolutely no texture, gradient, reflections, or content inside it — \
         just a flat solid grey rectangle with crisp sharp edges. \
         The background around the device should be themed in the style of: {user_style}. \
         Make the background beautiful, decorative, and eye-catching. \
         The device frame/bezel should look realistic and premium. \
         High quality, photorealistic, app store marketing material.{consistency_note}"
    )
}

// ---------------------------------------------------------------------------
// fal.ai API
// ---------------------------------------------------------------------------
#[derive(Serialize)]
struct FalTextToImageRequest {
    prompt: String,
    image_size: FalImageSize,
    num_inference_steps: u32,
    guidance_scale: f32,
    num_images: u32,
    output_format: String,
}
#[derive(Serialize)]
struct FalImageSize {
    width: u32,
    height: u32,
}
#[derive(Deserialize, Debug)]
struct FalResponse {
    images: Vec<FalImage>,
}
#[derive(Deserialize, Debug)]
struct FalImage {
    url: String,
}

async fn call_fal_text_to_image(
    api_key: &str,
    prompt: &str,
    width: u32,
    height: u32,
    num_inference_steps: u32,
) -> Result<String, String> {
    let body = FalTextToImageRequest {
        prompt: prompt.to_string(),
        image_size: FalImageSize { width, height },
        num_inference_steps,
        guidance_scale: 3.5,
        num_images: 1,
        output_format: "png".to_string(),
    };
    let resp = reqwest::Client::new()
        .post("https://fal.run/fal-ai/flux/dev")
        .header("Authorization", format!("Key {api_key}"))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("API returned {status}: {text}"));
    }
    let fal_resp: FalResponse = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {e}"))?;
    fal_resp
        .images
        .first()
        .map(|i| i.url.clone())
        .ok_or_else(|| "No images in response".to_string())
}

// ---------------------------------------------------------------------------
// Placeholder detection
// ---------------------------------------------------------------------------
fn find_placeholder_rect(frame: &RgbaImage) -> Option<(u32, u32, u32, u32)> {
    let (fw, fh) = frame.dimensions();
    let is_grey = |x: u32, y: u32| -> bool {
        let p = frame.get_pixel(x, y);
        let (r, g, b) = (p[0] as i32, p[1] as i32, p[2] as i32);
        let max_diff = (r - g).abs().max((r - b).abs()).max((g - b).abs());
        let avg = (r + g + b) / 3;
        max_diff < 30 && avg > 90 && avg < 170
    };
    let mut min_x = fw;
    let mut max_x = 0u32;
    let mut min_y = fh;
    let mut max_y = 0u32;
    let step = 2u32;
    for y in (0..fh).step_by(step as usize) {
        for x in (0..fw).step_by(step as usize) {
            if is_grey(x, y) {
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
            }
        }
    }
    if max_x <= min_x || max_y <= min_y {
        return None;
    }
    let threshold = 0.7;
    for y in min_y..=max_y {
        let gc = (min_x..=max_x)
            .step_by(step as usize)
            .filter(|&x| is_grey(x, y))
            .count();
        let tot = ((max_x - min_x) / step + 1) as usize;
        if (gc as f64 / tot as f64) >= threshold {
            min_y = y;
            break;
        }
    }
    for y in (min_y..=max_y).rev() {
        let gc = (min_x..=max_x)
            .step_by(step as usize)
            .filter(|&x| is_grey(x, y))
            .count();
        let tot = ((max_x - min_x) / step + 1) as usize;
        if (gc as f64 / tot as f64) >= threshold {
            max_y = y;
            break;
        }
    }
    for x in min_x..=max_x {
        let gc = (min_y..=max_y)
            .step_by(step as usize)
            .filter(|&y| is_grey(x, y))
            .count();
        let tot = ((max_y - min_y) / step + 1) as usize;
        if (gc as f64 / tot as f64) >= threshold {
            min_x = x;
            break;
        }
    }
    for x in (min_x..=max_x).rev() {
        let gc = (min_y..=max_y)
            .step_by(step as usize)
            .filter(|&y| is_grey(x, y))
            .count();
        let tot = ((max_y - min_y) / step + 1) as usize;
        if (gc as f64 / tot as f64) >= threshold {
            max_x = x;
            break;
        }
    }
    let w = max_x.saturating_sub(min_x);
    let h = max_y.saturating_sub(min_y);
    let area = w as u64 * h as u64;
    let frame_area = fw as u64 * fh as u64;
    if area > frame_area / 20 && w > 50 && h > 50 {
        Some((min_x, min_y, w, h))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Composite
// ---------------------------------------------------------------------------
fn composite_screenshot(frame: &mut RgbaImage, screenshot: &RgbaImage, rect: (u32, u32, u32, u32)) {
    let (rx, ry, rw, rh) = rect;
    let resized = image::imageops::resize(screenshot, rw, rh, FilterType::Lanczos3);
    let mut saved: Vec<(u32, u32, image::Rgba<u8>)> = Vec::new();
    for sy in 0..rh {
        for sx in 0..rw {
            let (fx, fy) = (rx + sx, ry + sy);
            if fx < frame.width() && fy < frame.height() {
                let fp = *frame.get_pixel(fx, fy);
                let (r, g, b) = (fp[0] as i32, fp[1] as i32, fp[2] as i32);
                let max_diff = (r - g).abs().max((r - b).abs()).max((g - b).abs());
                let avg = (r + g + b) / 3;
                if !(max_diff < 30 && avg > 90 && avg < 170) {
                    saved.push((fx, fy, fp));
                }
            }
        }
    }
    image::imageops::overlay(frame, &resized, rx as i64, ry as i64);
    for (fx, fy, pixel) in saved {
        frame.put_pixel(fx, fy, pixel);
    }
}

fn fallback_placement(frame_w: u32, frame_h: u32) -> (u32, u32, u32, u32) {
    let sw = (frame_w as f64 * 0.55) as u32;
    let sh = (frame_h as f64 * 0.72) as u32;
    ((frame_w - sw) / 2, (frame_h - sh) / 2, sw, sh)
}

async fn download_image(url: &str) -> Result<Vec<u8>, String> {
    reqwest::get(url)
        .await
        .map_err(|e| format!("Download failed: {e}"))?
        .bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| format!("Read bytes failed: {e}"))
}

// ---------------------------------------------------------------------------
// Resize to targets
// ---------------------------------------------------------------------------
/// Where a generation run saves its images, and which sizes are ticked
/// (one bool per entry of IOS_TARGETS / ANDROID_TARGETS / DESKTOP_TARGETS;
/// an empty list exports nothing for that platform).
#[derive(Clone)]
struct ExportTargets {
    /// `fastlane/screenshots/ios`
    ios_dir: PathBuf,
    /// `fastlane/metadata/android`
    android_root: PathBuf,
    /// `fastlane/screenshots/desktop`
    desktop_dir: PathBuf,
    ios: Vec<bool>,
    android: Vec<bool>,
    desktop: Vec<bool>,
}

impl ExportTargets {
    fn for_project(project_dir: &std::path::Path, p: &ProjectState) -> Self {
        let fastlane = project_dir.join("fastlane");
        Self {
            ios_dir: fastlane.join("screenshots").join("ios"),
            android_root: fastlane.join("metadata").join("android"),
            desktop_dir: fastlane.join("screenshots").join("desktop"),
            ios: if p.export_ios { p.ios_targets.clone() } else { vec![] },
            android: if p.export_android { p.android_targets.clone() } else { vec![] },
            desktop: if p.platform_type.has_desktop() { p.desktop_targets.clone() } else { vec![] },
        }
    }
}

/// Resize one composited screen to every enabled target and save it.
///
/// iOS goes to `fastlane/screenshots/ios/<locale>/`, desktop to
/// `fastlane/screenshots/desktop/<locale>/`, Android to fastlane supply's
/// layout: `<play-lang>/images/phoneScreenshots/NN.png` and, from the first
/// screen only (Play shows one), `<play-lang>/images/featureGraphic.png`.
/// Every label carries `[<locale>]` so uploads can pick each language's files.
fn resize_to_targets(
    image_bytes: &[u8],
    screen_index: usize,
    locale: &str,
    play_locale: &str,
    targets: &ExportTargets,
) -> Result<Vec<(String, PathBuf)>, String> {
    let img = image::load_from_memory(image_bytes)
        .map_err(|e| format!("Failed to decode image: {e}"))?
        .to_rgba8();

    let mut results = Vec::new();

    for (ti, &(_, device_name, tw, th)) in IOS_TARGETS.iter().enumerate() {
        if !targets.ios.get(ti).copied().unwrap_or(false) { continue; }
        let resized = fill_and_crop(&img, tw, th);
        let locale_dir = targets.ios_dir.join(locale);
        std::fs::create_dir_all(&locale_dir)
            .map_err(|e| format!("Failed to create locale dir: {e}"))?;
        let path = locale_dir.join(format!("{device_name}-{screen_index:02}.png"));
        // App Store Connect rejects PNGs with an alpha channel (IMAGE_ALPHA_NOT_ALLOWED).
        // Flatten alpha onto white before saving iOS screenshots.
        flatten_alpha_onto_white(&resized)
            .save(&path)
            .map_err(|e| format!("Failed to save {}: {e}", path.display()))?;
        results.push((
            format!("iOS {device_name} [{locale}] #{screen_index}"),
            path,
        ));
    }

    let images_dir = targets.android_root.join(play_locale).join("images");
    for (ti, &(key, label, tw, th)) in ANDROID_TARGETS.iter().enumerate() {
        if !targets.android.get(ti).copied().unwrap_or(false) { continue; }
        let path = match key {
            "android_feature" if screen_index == 1 => images_dir.join("featureGraphic.png"),
            "android_feature" => continue,
            _ => images_dir.join("phoneScreenshots").join(format!("{screen_index:02}.png")),
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("Failed to create {}: {e}", dir.display()))?;
        }
        fill_and_crop(&img, tw, th)
            .save(&path)
            .map_err(|e| format!("Failed to save {}: {e}", path.display()))?;
        results.push((format!("{label} [{locale}] #{screen_index}"), path));
    }

    for (ti, &(key, name, tw, th)) in DESKTOP_TARGETS.iter().enumerate() {
        if !targets.desktop.get(ti).copied().unwrap_or(false) { continue; }
        let locale_dir = targets.desktop_dir.join(locale);
        std::fs::create_dir_all(&locale_dir)
            .map_err(|e| format!("Failed to create {}: {e}", locale_dir.display()))?;
        let path = locale_dir.join(format!("{key}-{screen_index:02}.png"));
        // The Mac App Store refuses alpha just like iOS does.
        flatten_alpha_onto_white(&fill_and_crop(&img, tw, th))
            .save(&path)
            .map_err(|e| format!("Failed to save {}: {e}", path.display()))?;
        results.push((format!("Desktop {name} [{locale}] #{screen_index}"), path));
    }

    Ok(results)
}

/// Composite an RGBA image onto a white background, returning an opaque RGB-equivalent RgbaImage.
/// App Store Connect rejects PNGs that carry an alpha channel (IMAGE_ALPHA_NOT_ALLOWED).
fn flatten_alpha_onto_white(src: &RgbaImage) -> image::RgbImage {
    let (w, h) = src.dimensions();
    let mut out = image::RgbImage::new(w, h);
    for (x, y, pixel) in src.enumerate_pixels() {
        let a = pixel[3] as f32 / 255.0;
        let r = (pixel[0] as f32 * a + 255.0 * (1.0 - a)) as u8;
        let g = (pixel[1] as f32 * a + 255.0 * (1.0 - a)) as u8;
        let b = (pixel[2] as f32 * a + 255.0 * (1.0 - a)) as u8;
        out.put_pixel(x, y, image::Rgb([r, g, b]));
    }
    out
}

fn fill_and_crop(src: &RgbaImage, target_w: u32, target_h: u32) -> RgbaImage {
    let (sw, sh) = (src.width() as f64, src.height() as f64);
    let scale = (target_w as f64 / sw).max(target_h as f64 / sh);
    let new_w = (sw * scale).round().max(1.0) as u32;
    let new_h = (sh * scale).round().max(1.0) as u32;
    let resized = image::imageops::resize(src, new_w, new_h, FilterType::Lanczos3);
    let crop_x = (new_w.saturating_sub(target_w)) / 2;
    let crop_y = (new_h.saturating_sub(target_h)) / 2;
    image::imageops::crop_imm(&resized, crop_x, crop_y, target_w, target_h).to_image()
}

// ---------------------------------------------------------------------------
// Color utilities
// ---------------------------------------------------------------------------
fn parse_hex_color(hex: &str) -> Option<Rgba<u8>> {
    let hex = hex.trim_start_matches('#');
    let rgb = hex::decode(hex).ok()?;
    if rgb.len() >= 3 {
        Some(Rgba([rgb[0], rgb[1], rgb[2], 255]))
    } else {
        None
    }
}

fn lighten_color(color: Rgba<u8>) -> Rgba<u8> {
    let Rgba([r, g, b, a]) = color;
    let mix = |c: u8| -> u8 { ((c as f32 * 0.7) + (255.0 * 0.3)) as u8 };
    Rgba([mix(r), mix(g), mix(b), a])
}

fn get_contrast_color(bg: Rgba<u8>) -> Rgba<u8> {
    let Rgba([r, g, b, _]) = bg;
    let l = 0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32;
    if l > 128.0 {
        Rgba([0, 0, 0, 255])
    } else {
        Rgba([255, 255, 255, 255])
    }
}

fn draw_centered_text(
    img: &mut RgbaImage,
    font: &FontRef,
    text: &str,
    scale: PxScale,
    color: Rgba<u8>,
    y_pos: i32,
) {
    let (w, _) = img.dimensions();
    let (text_width, _) = text_size(scale, font, text);
    draw_text_mut(
        img,
        color,
        (w as i32 - text_width as i32) / 2,
        y_pos,
        scale,
        font,
        text,
    );
}


// ---------------------------------------------------------------------------
// Strip ANSI escape codes from terminal output for clean log display
// ---------------------------------------------------------------------------
fn strip_ansi(s: &str) -> String {
    // Matches ESC[ ... m  (SGR) and other common escape sequences
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Consume the escape sequence: ESC followed by '[' then up to final byte in 0x40–0x7E
            if chars.peek() == Some(&'[') {
                chars.next(); // consume '['
                for nc in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&nc) { break; }
                }
            } else {
                // Non-CSI escape: consume one more char
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Google Play androidpublisher v3 helpers
// ---------------------------------------------------------------------------

/// Mint a short-lived JWT and exchange it for a Google OAuth2 access token.
async fn google_play_access_token(client_email: &str, private_key_pem: &str) -> Result<String, String> {
    use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};

    #[derive(serde::Serialize)]
    struct Claims {
        iss: String,
        scope: String,
        aud: String,
        iat: u64,
        exp: u64,
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();

    let claims = Claims {
        iss: client_email.to_string(),
        scope: "https://www.googleapis.com/auth/androidpublisher".to_string(),
        aud: "https://oauth2.googleapis.com/token".to_string(),
        iat: now,
        exp: now + 3600,
    };

    let key = EncodingKey::from_rsa_pem(private_key_pem.as_bytes())
        .map_err(|e| format!("Invalid private key: {e}"))?;
    let jwt = encode(&Header::new(Algorithm::RS256), &claims, &key)
        .map_err(|e| format!("JWT encode error: {e}"))?;

    let client = reqwest::Client::new();
    let resp = client
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
            ("assertion", &jwt),
        ])
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    body["access_token"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| format!("No access_token in response: {body}"))
}

/// Create a new edit and return its ID.
async fn google_play_create_edit(token: &str, package_name: &str) -> Result<String, String> {
    let url = format!(
        "https://androidpublisher.googleapis.com/androidpublisher/v3/applications/{package_name}/edits"
    );
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .bearer_auth(token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = resp.status();
    let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        // Surface the activation URL when the API is disabled in the GCP project.
        let activation_url = body["error"]["details"]
            .as_array()
            .and_then(|details| {
                details.iter().find_map(|d| d["metadata"]["activationUrl"].as_str())
            });
        if let Some(url) = activation_url {
            return Err(format!(
                "Google Play Android Developer API is disabled.\n\
                Enable it here, wait ~1 min, then retry:\n{url}"
            ));
        }
        let message = body["error"]["message"].as_str()
            .unwrap_or("unknown error").to_string();
        // 404 "Package not found" means the app hasn't been created in Play Console yet.
        if status.as_u16() == 404 && message.contains("Package not found") {
            return Err(format!(
                "{message}\n\n\
                The app must exist in Google Play Console before the API can manage it.\n\
                Go to https://play.google.com/console and create the app, then upload\n\
                at least one APK/AAB manually (even as a draft internal release).\n\
                After that, this upload will work."
            ));
        }
        return Err(format!("HTTP {status}: {message}"));
    }
    body["id"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| format!("No edit id in response: {body}"))
}

/// Delete all existing images for a given language + imageType (clears before re-upload).
async fn google_play_delete_images(
    token: &str,
    package_name: &str,
    edit_id: &str,
    language: &str,
    image_type: &str,
) -> Result<(), String> {
    let url = format!(
        "https://androidpublisher.googleapis.com/androidpublisher/v3/applications/{package_name}/edits/{edit_id}/listings/{language}/{image_type}"
    );
    let client = reqwest::Client::new();
    let resp = client
        .delete(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    // 204 No Content or 404 (nothing to delete) are both fine
    if !resp.status().is_success() && resp.status().as_u16() != 404 {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Delete images HTTP {status}: {body}"));
    }
    Ok(())
}

/// Upload a single image file.
async fn google_play_upload_image(
    token: &str,
    package_name: &str,
    edit_id: &str,
    language: &str,
    image_type: &str,
    path: &std::path::Path,
) -> Result<(), String> {
    let url = format!(
        "https://androidpublisher.googleapis.com/upload/androidpublisher/v3/applications/{package_name}/edits/{edit_id}/listings/{language}/{image_type}"
    );
    let bytes = std::fs::read(path).map_err(|e| format!("Read {}: {e}", path.display()))?;
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .bearer_auth(token)
        .query(&[("uploadType", "media")])
        .header("Content-Type", "image/png")
        .body(bytes)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("HTTP {status}: {body}"));
    }
    Ok(())
}

/// Commit an edit to make it live.
async fn google_play_commit_edit(token: &str, package_name: &str, edit_id: &str) -> Result<(), String> {
    let url = format!(
        "https://androidpublisher.googleapis.com/androidpublisher/v3/applications/{package_name}/edits/{edit_id}:commit"
    );
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .bearer_auth(token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("HTTP {status}: {body}"));
    }
    Ok(())
}

/// Silently delete (abort) an edit — used for cleanup on error.
async fn google_play_delete_edit(token: &str, package_name: &str, edit_id: &str) -> Result<(), String> {
    let url = format!(
        "https://androidpublisher.googleapis.com/androidpublisher/v3/applications/{package_name}/edits/{edit_id}"
    );
    let _ = reqwest::Client::new()
        .delete(&url)
        .bearer_auth(token)
        .send()
        .await;
    Ok(())
}

// ---------------------------------------------------------------------------
// App Store Connect API v1 helpers
// ---------------------------------------------------------------------------

/// Mint a short-lived ES256 JWT for App Store Connect API authentication.
/// key_id: the key ID from App Store Connect (e.g. "53U79ZJ29Y")
/// issuer_id: the issuer UUID from App Store Connect
/// p8_pem: contents of the downloaded .p8 private key file
fn asc_mint_jwt(key_id: &str, issuer_id: &str, p8_pem: &str) -> Result<String, String> {
    use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};

    #[derive(serde::Serialize)]
    struct AscClaims {
        iss: String,
        iat: u64,
        exp: u64,
        aud: String,
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();

    let claims = AscClaims {
        iss: issuer_id.to_string(),
        iat: now,
        exp: now + 1200, // 20 minutes — ASC max is 20 min
        aud: "appstoreconnect-v1".to_string(),
    };

    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some(key_id.to_string());

    let key = EncodingKey::from_ec_pem(p8_pem.as_bytes())
        .map_err(|e| format!("Invalid .p8 key: {e}"))?;

    encode(&header, &claims, &key).map_err(|e| format!("JWT encode error: {e}"))
}

/// Find an app in App Store Connect by bundle ID, return its app ID.
async fn asc_find_app(jwt: &str, bundle_id: &str) -> Result<String, String> {
    let url = format!(
        "https://api.appstoreconnect.apple.com/v1/apps?filter[bundleId]={bundle_id}&fields[apps]=bundleId"
    );
    let resp: serde_json::Value = reqwest::Client::new()
        .get(&url)
        .bearer_auth(jwt)
        .send().await.map_err(|e| e.to_string())?
        .json().await.map_err(|e| e.to_string())?;

    if let Some(err) = resp["errors"].as_array().and_then(|e| e.first()) {
        let title = err["title"].as_str().unwrap_or("App Store Connect error");
        let detail = err["detail"].as_str().unwrap_or("");
        return Err(format!("{title}: {detail}"));
    }
    resp["data"].as_array()
        .and_then(|arr| arr.first())
        .and_then(|app| app["id"].as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("App with bundle ID '{bundle_id}' not found in App Store Connect"))
}

/// Find the latest iOS App Store version (any state), or create one if none exists.
///
/// Strategy:
///   1. GET all versions without a platform filter (the filter can hide READY_FOR_SALE etc.)
///   2. Prefer an editable state; fall back to any existing version.
///   3. Only attempt POST when the list is truly empty. If POST returns
///      "duplicate" or "invalid state" errors, do a second wider GET and use whatever comes back.
async fn asc_find_or_create_version(jwt: &str, app_id: &str, version_string: &str) -> Result<String, String> {
    // ── Step 1: fetch all versions (no platform filter, no field pruning) ──────
    if let Some(id) = asc_fetch_any_version(jwt, app_id).await? {
        return Ok(id);
    }

    // ── Step 2: truly no version found — try to create one ────────────────────
    let version_string = if version_string.is_empty() { "1.0" } else { version_string };
    tracing::info!("No iOS version found; creating v{version_string} via ASC API");
    let body = serde_json::json!({
        "data": {
            "type": "appStoreVersions",
            "attributes": {
                "platform": "IOS",
                "versionString": version_string
            },
            "relationships": {
                "app": {
                    "data": { "type": "apps", "id": app_id }
                }
            }
        }
    });
    let http_resp = reqwest::Client::new()
        .post("https://api.appstoreconnect.apple.com/v1/appStoreVersions")
        .bearer_auth(jwt)
        .json(&body)
        .send().await.map_err(|e| e.to_string())?;
    let create_resp: serde_json::Value = http_resp.json().await.map_err(|e| e.to_string())?;

    // Happy path: POST succeeded and returned the new version.
    if let Some(id) = create_resp["data"]["id"].as_str() {
        return Ok(id.to_string());
    }

    // POST failed — if the error indicates the version already exists or the app
    // is in an incompatible state, fall back to a second wider GET.
    let error_codes: Vec<&str> = create_resp["errors"]
        .as_array()
        .map(|arr| arr.iter()
            .filter_map(|e| e["code"].as_str())
            .collect())
        .unwrap_or_default();

    let already_exists = error_codes.iter().any(|c| {
        c.contains("DUPLICATE") || c.contains("INVALID")
    });

    if already_exists {
        // The version exists but wasn't returned by our first GET.
        // Try again without any query parameters.
        tracing::warn!("POST said version exists; retrying GET with no filters");
        if let Some(id) = asc_fetch_any_version(jwt, app_id).await? {
            return Ok(id);
        }
    }

    Err(format!("Failed to find or create version: {create_resp}"))
}

/// GET /v1/apps/{id}/appStoreVersions with no filters and return the ID of the
/// best available version (editable states preferred, any state as fallback).
async fn asc_fetch_any_version(jwt: &str, app_id: &str) -> Result<Option<String>, String> {
    let url = format!(
        "https://api.appstoreconnect.apple.com/v1/apps/{app_id}/appStoreVersions\
         ?sort=-createdDate&limit=20"
    );
    let resp: serde_json::Value = reqwest::Client::new()
        .get(&url)
        .bearer_auth(jwt)
        .send().await.map_err(|e| e.to_string())?
        .json().await.map_err(|e| e.to_string())?;

    let versions = resp["data"].as_array().map(|a| a.clone()).unwrap_or_default();

    let editable_states = [
        "PREPARE_FOR_SUBMISSION",
        "WAITING_FOR_REVIEW",
        "IN_REVIEW",
        "REJECTED",
        "DEVELOPER_REJECTED",
        "METADATA_REJECTED",
        "INVALID_BINARY",
        "READY_FOR_REVIEW",
    ];

    // Prefer editable.
    for v in versions.iter() {
        let state = v["attributes"]["appStoreState"].as_str().unwrap_or("");
        if editable_states.contains(&state) {
            if let Some(id) = v["id"].as_str() {
                return Ok(Some(id.to_string()));
            }
        }
    }

    // Fall back to any version.
    Ok(versions.first().and_then(|v| v["id"].as_str()).map(|s| s.to_string()))
}

/// Return a map of locale → localization ID for a given version.
async fn asc_get_localizations(jwt: &str, version_id: &str) -> Result<std::collections::HashMap<String, String>, String> {
    let url = format!(
        "https://api.appstoreconnect.apple.com/v1/appStoreVersions/{version_id}/appStoreVersionLocalizations\
         ?fields[appStoreVersionLocalizations]=locale\
         &limit=50"
    );
    let resp: serde_json::Value = reqwest::Client::new()
        .get(&url)
        .bearer_auth(jwt)
        .send().await.map_err(|e| e.to_string())?
        .json().await.map_err(|e| e.to_string())?;

    let mut map = std::collections::HashMap::new();
    if let Some(arr) = resp["data"].as_array() {
        for item in arr {
            if let (Some(id), Some(locale)) = (
                item["id"].as_str(),
                item["attributes"]["locale"].as_str(),
            ) {
                map.insert(locale.to_string(), id.to_string());
            }
        }
    }
    Ok(map)
}

/// PATCH an appStoreVersionLocalization to set the app name.
/// ASC requires `name` to be non-empty for the version to pass review.
async fn asc_set_localization_name(jwt: &str, localization_id: &str, name: &str) -> Result<(), String> {
    let body = serde_json::json!({
        "data": {
            "type": "appStoreVersionLocalizations",
            "id": localization_id,
            "attributes": {
                "name": name
            }
        }
    });
    let resp = reqwest::Client::new()
        .patch(format!(
            "https://api.appstoreconnect.apple.com/v1/appStoreVersionLocalizations/{localization_id}"
        ))
        .bearer_auth(jwt)
        .json(&body)
        .send().await.map_err(|e| e.to_string())?;

    if resp.status().is_success() {
        Ok(())
    } else {
        let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        Err(format!("{body}"))
    }
}

/// Get the screenshot set ID for a given localization + display type,
/// creating the set if it doesn't exist yet.
async fn asc_get_or_create_screenshot_set(
    jwt: &str,
    localization_id: &str,
    display_type: &str,
) -> Result<String, String> {
    // List existing sets for this localization.
    let list_url = format!(
        "https://api.appstoreconnect.apple.com/v1/appStoreVersionLocalizations/{localization_id}/appScreenshotSets\
         ?fields[appScreenshotSets]=screenshotDisplayType\
         &limit=40"
    );
    let resp: serde_json::Value = reqwest::Client::new()
        .get(&list_url)
        .bearer_auth(jwt)
        .send().await.map_err(|e| e.to_string())?
        .json().await.map_err(|e| e.to_string())?;

    if let Some(arr) = resp["data"].as_array() {
        for set in arr {
            if set["attributes"]["screenshotDisplayType"].as_str() == Some(display_type) {
                if let Some(id) = set["id"].as_str() {
                    return Ok(id.to_string());
                }
            }
        }
    }

    // Create the set.
    let body = serde_json::json!({
        "data": {
            "type": "appScreenshotSets",
            "attributes": { "screenshotDisplayType": display_type },
            "relationships": {
                "appStoreVersionLocalization": {
                    "data": { "type": "appStoreVersionLocalizations", "id": localization_id }
                }
            }
        }
    });
    let create_resp: serde_json::Value = reqwest::Client::new()
        .post("https://api.appstoreconnect.apple.com/v1/appScreenshotSets")
        .bearer_auth(jwt)
        .json(&body)
        .send().await.map_err(|e| e.to_string())?
        .json().await.map_err(|e| e.to_string())?;

    create_resp["data"]["id"].as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| format!("Failed to create screenshot set: {create_resp}"))
}

/// Delete all screenshots currently in a screenshot set.
async fn asc_delete_all_screenshots_in_set(jwt: &str, set_id: &str) -> Result<(), String> {
    let list_url = format!(
        "https://api.appstoreconnect.apple.com/v1/appScreenshotSets/{set_id}/appScreenshots\
         ?fields[appScreenshots]=fileName\
         &limit=50"
    );
    let resp: serde_json::Value = reqwest::Client::new()
        .get(&list_url)
        .bearer_auth(jwt)
        .send().await.map_err(|e| e.to_string())?
        .json().await.map_err(|e| e.to_string())?;

    if let Some(arr) = resp["data"].as_array() {
        for screenshot in arr {
            if let Some(id) = screenshot["id"].as_str() {
                let del_url = format!("https://api.appstoreconnect.apple.com/v1/appScreenshots/{id}");
                let _ = reqwest::Client::new()
                    .delete(&del_url)
                    .bearer_auth(jwt)
                    .send().await;
            }
        }
    }
    Ok(())
}

/// Reserve + upload a single screenshot to an appScreenshotSet.
/// Follows the ASC two-phase upload: POST to reserve → PUT to upload bytes → PATCH to commit.
async fn asc_upload_screenshot(jwt: &str, set_id: &str, path: &PathBuf) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Read file: {e}"))?;
    let file_size = bytes.len() as u64;
    let file_name = path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("screenshot.png")
        .to_string();

    // Phase 1: Reserve the upload.
    let reserve_body = serde_json::json!({
        "data": {
            "type": "appScreenshots",
            "attributes": {
                "fileSize": file_size,
                "fileName": file_name,
            },
            "relationships": {
                "appScreenshotSet": {
                    "data": { "type": "appScreenshotSets", "id": set_id }
                }
            }
        }
    });
    let reserve_resp: serde_json::Value = reqwest::Client::new()
        .post("https://api.appstoreconnect.apple.com/v1/appScreenshots")
        .bearer_auth(jwt)
        .json(&reserve_body)
        .send().await.map_err(|e| e.to_string())?
        .json().await.map_err(|e| e.to_string())?;

    let screenshot_id = reserve_resp["data"]["id"].as_str()
        .ok_or_else(|| format!("No screenshot ID in reserve response: {reserve_resp}"))?
        .to_string();

    // Phase 2: Upload bytes to each part URL provided by ASC.
    let upload_ops = reserve_resp["data"]["attributes"]["uploadOperations"]
        .as_array()
        .ok_or_else(|| "No uploadOperations in reserve response".to_string())?;

    let client = reqwest::Client::new();
    for op in upload_ops {
        let url = op["url"].as_str()
            .ok_or_else(|| "Upload operation missing url".to_string())?;
        let offset = op["offset"].as_u64().unwrap_or(0) as usize;
        let length = op["length"].as_u64().unwrap_or(file_size) as usize;
        let chunk = bytes.get(offset..offset + length)
            .ok_or_else(|| format!("Chunk out of range: offset={offset} length={length} file_size={file_size}"))?
            .to_vec();

        // Build request with any required headers from the operation.
        let mut req = client.put(url).body(chunk);
        if let Some(headers) = op["requestHeaders"].as_array() {
            for h in headers {
                if let (Some(name), Some(value)) = (h["name"].as_str(), h["value"].as_str()) {
                    req = req.header(name, value);
                }
            }
        }
        let put_resp = req.send().await.map_err(|e| format!("Upload PUT failed: {e}"))?;
        if !put_resp.status().is_success() {
            return Err(format!("Upload PUT returned HTTP {}", put_resp.status()));
        }
    }

    // Phase 3: Commit — tell ASC the upload is complete.
    let commit_body = serde_json::json!({
        "data": {
            "type": "appScreenshots",
            "id": screenshot_id,
            "attributes": { "uploaded": true }
        }
    });
    let commit_url = format!("https://api.appstoreconnect.apple.com/v1/appScreenshots/{screenshot_id}");
    let commit_resp = reqwest::Client::new()
        .patch(&commit_url)
        .bearer_auth(jwt)
        .json(&commit_body)
        .send().await.map_err(|e| format!("Commit PATCH failed: {e}"))?;

    if !commit_resp.status().is_success() {
        let body = commit_resp.text().await.unwrap_or_default();
        return Err(format!("Commit failed: {body}"));
    }
    Ok(())
}
