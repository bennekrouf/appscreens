use ab_glyph::{FontRef, PxScale};
use dioxus::desktop::wry::http::{Response, StatusCode};
use dioxus::desktop::wry::RequestAsyncResponder;
use dioxus::desktop::{use_asset_handler, LogicalSize, WindowBuilder};
use dioxus::prelude::*;
use image::imageops::FilterType;
use image::{Rgba, RgbaImage};
use imageproc::drawing::{draw_filled_rect_mut, draw_text_mut, text_size};
use imageproc::rect::Rect;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const MAIN_CSS: Asset = asset!("/assets/main.css");
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
    ("ios_ipad_129", "iPad Pro (12.9-inch)", 2048, 2732),
];

// Android targets still go to output/ inside the project
const ANDROID_TARGETS: &[(&str, &str, u32, u32)] = &[
    ("android_phone", "Android Phone", 1080, 2340),
    ("android_feature", "Android Feature", 1024, 500),
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
    #[serde(default)]
    ios_short_version: String,    // e.g. "1.0"
}

fn default_phone_style() -> String {
    "modern smartphone".to_string()
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
    #[serde(default)]
    bundle_id: String,     // iOS + Android bundle ID, e.g. "com.mayorana.tafseel.abjad"
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
}

fn default_true() -> bool { true }
fn default_locale() -> String { "en-US".to_string() }
fn default_locales() -> Vec<String> { vec!["en-US".to_string()] }
fn default_ios_targets() -> Vec<bool> { vec![true; IOS_TARGETS.len()] }
fn default_android_targets() -> Vec<bool> { vec![true; ANDROID_TARGETS.len()] }

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
            ..Default::default()
        }
    }
    /// Ensure ios_targets / android_targets vecs are the right length.
    /// Also migrate legacy `manual_texts` + `locale` into `locale_texts` / `locales`.
    fn normalize_targets(&mut self) {
        while self.ios_targets.len() < IOS_TARGETS.len() { self.ios_targets.push(true); }
        self.ios_targets.truncate(IOS_TARGETS.len());
        while self.android_targets.len() < ANDROID_TARGETS.len() { self.android_targets.push(true); }
        self.android_targets.truncate(ANDROID_TARGETS.len());
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
        // Migrate legacy shared source_paths → en-US locale_sources
        if !self.source_paths.is_empty() {
            let entry = self.locale_sources
                .entry("en-US".to_string())
                .or_insert_with(Vec::new);
            for p in &self.source_paths {
                if !entry.contains(p) { entry.push(p.clone()); }
            }
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
    state.migrate_legacy();
    state
}

fn save_project_state(project_dir: &PathBuf, state: &ProjectState) {
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(project_state_path(project_dir), json);
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
APP_BUNDLE="target/dx/{project_slug}/release/ios/{app_name}.app"
ENTITLEMENTS="Entitlements.plist"

echo "🚀 Starting iOS Build for App Store Distribution..."

# 1. Prerequisite Checks
if ! command -v dx &>/dev/null; then
  echo "❌ Dioxus CLI (dx) not found. Please install it."
  exit 1
fi

# 2. Build Rust Project for iOS (Release)
echo "🧹 Cleaning old builds..."
rm -rf target/dx/{project_slug}/release/ios
rm -rf target/dx/{project_slug}/release/web # Clean web assets cache too

echo "📦 Building Rust project for iOS (Release - Device)..."
# Force aarch64-apple-ios to avoid simulator slices
dx build --platform ios --release --target aarch64-apple-ios

# 3. Locate Generated App Bundle
if [ ! -d "$APP_BUNDLE" ]; then
  echo "❌ Could not find generated .app bundle at $APP_BUNDLE"
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

# 4.5 Generate App Icons (Asset Catalog Method - Required for iOS 11+)
echo "🎨 Generating App Icons via Asset Catalog..."
ICON_SOURCE="assets/icon.png"

if [ -f "$ICON_SOURCE" ]; then
  echo "   Found source icon: $ICON_SOURCE"

  # Create temporary Asset Catalog structure
  ASSETS_DIR="TargetSupport/Assets.xcassets"
  APP_ICON_SET="$ASSETS_DIR/AppIcon.appiconset"
  mkdir -p "$APP_ICON_SET"

  # Write Contents.json
  cat >"$APP_ICON_SET/Contents.json" <<EOF
{{
  "images" : [
    {{ "size" : "20x20", "idiom" : "iphone", "filename" : "Icon-20@2x.png", "scale" : "2x" }},
    {{ "size" : "20x20", "idiom" : "iphone", "filename" : "Icon-20@3x.png", "scale" : "3x" }},
    {{ "size" : "29x29", "idiom" : "iphone", "filename" : "Icon-29@2x.png", "scale" : "2x" }},
    {{ "size" : "29x29", "idiom" : "iphone", "filename" : "Icon-29@3x.png", "scale" : "3x" }},
    {{ "size" : "40x40", "idiom" : "iphone", "filename" : "Icon-40@2x.png", "scale" : "2x" }},
    {{ "size" : "40x40", "idiom" : "iphone", "filename" : "Icon-40@3x.png", "scale" : "3x" }},
    {{ "size" : "60x60", "idiom" : "iphone", "filename" : "Icon-60@2x.png", "scale" : "2x" }},
    {{ "size" : "60x60", "idiom" : "iphone", "filename" : "Icon-60@3x.png", "scale" : "3x" }},
    {{ "size" : "20x20", "idiom" : "ipad", "filename" : "Icon-20.png", "scale" : "1x" }},
    {{ "size" : "20x20", "idiom" : "ipad", "filename" : "Icon-20@2x.png", "scale" : "2x" }},
    {{ "size" : "29x29", "idiom" : "ipad", "filename" : "Icon-29.png", "scale" : "1x" }},
    {{ "size" : "29x29", "idiom" : "ipad", "filename" : "Icon-29@2x.png", "scale" : "2x" }},
    {{ "size" : "40x40", "idiom" : "ipad", "filename" : "Icon-40.png", "scale" : "1x" }},
    {{ "size" : "40x40", "idiom" : "ipad", "filename" : "Icon-40@2x.png", "scale" : "2x" }},
    {{ "size" : "76x76", "idiom" : "ipad", "filename" : "Icon-76.png", "scale" : "1x" }},
    {{ "size" : "76x76", "idiom" : "ipad", "filename" : "Icon-76@2x.png", "scale" : "2x" }},
    {{ "size" : "83.5x83.5", "idiom" : "ipad", "filename" : "Icon-83.5@2x.png", "scale" : "2x" }},
    {{ "size" : "1024x1024", "idiom" : "ios-marketing", "filename" : "Icon-1024.png", "scale" : "1x" }}
  ],
  "info" : {{ "version" : 1, "author" : "xcode" }}
}}
EOF

  # Generate Icons
  sips -z 40 40 "$ICON_SOURCE" --out "$APP_ICON_SET/Icon-20@2x.png" >/dev/null
  sips -z 60 60 "$ICON_SOURCE" --out "$APP_ICON_SET/Icon-20@3x.png" >/dev/null
  sips -z 58 58 "$ICON_SOURCE" --out "$APP_ICON_SET/Icon-29@2x.png" >/dev/null
  sips -z 87 87 "$ICON_SOURCE" --out "$APP_ICON_SET/Icon-29@3x.png" >/dev/null
  sips -z 80 80 "$ICON_SOURCE" --out "$APP_ICON_SET/Icon-40@2x.png" >/dev/null
  sips -z 120 120 "$ICON_SOURCE" --out "$APP_ICON_SET/Icon-40@3x.png" >/dev/null
  sips -z 120 120 "$ICON_SOURCE" --out "$APP_ICON_SET/Icon-60@2x.png" >/dev/null
  sips -z 180 180 "$ICON_SOURCE" --out "$APP_ICON_SET/Icon-60@3x.png" >/dev/null
  sips -z 20 20 "$ICON_SOURCE" --out "$APP_ICON_SET/Icon-20.png" >/dev/null
  sips -z 29 29 "$ICON_SOURCE" --out "$APP_ICON_SET/Icon-29.png" >/dev/null
  sips -z 40 40 "$ICON_SOURCE" --out "$APP_ICON_SET/Icon-40.png" >/dev/null
  sips -z 76 76 "$ICON_SOURCE" --out "$APP_ICON_SET/Icon-76.png" >/dev/null
  sips -z 152 152 "$ICON_SOURCE" --out "$APP_ICON_SET/Icon-76@2x.png" >/dev/null
  sips -z 167 167 "$ICON_SOURCE" --out "$APP_ICON_SET/Icon-83.5@2x.png" >/dev/null
  sips -z 1024 1024 "$ICON_SOURCE" --out "$APP_ICON_SET/Icon-1024.png" >/dev/null

  # Compile Asset Catalog
  echo "   Compiling Assets.car..."
  xcrun actool "$ASSETS_DIR" --compile "$APP_PATH" --platform iphoneos --minimum-deployment-target 13.0 --app-icon AppIcon --output-partial-info-plist "partial_info.plist" >/dev/null

  # Merge Partial Info.plist
  if [ -f "partial_info.plist" ]; then
    echo "   Merging partial Info.plist..."
    /usr/libexec/PlistBuddy -c "Merge partial_info.plist" "$PLIST_PATH"
    rm "partial_info.plist"
  fi

  # Cleanup
  rm -rf "TargetSupport"

  echo "✅ Assets compiled and Info.plist updated"
else
  echo "⚠️  Warning: assets/icon.png not found. Skipping icon generation."
fi

echo "🔧 Fixing Info.plist for App Store Validation..."

# Remove empty lines at start of file
sed -i '' '/^$/d' "$PLIST_PATH"

# Inject Bundle Metadata
plutil -replace CFBundleIdentifier -string "$BUNDLE_ID" "$PLIST_PATH"
plutil -replace CFBundleDisplayName -string "{app_name}" "$PLIST_PATH"

# Auto-increment Build Number
BUILD_NUMBER_FILE="build_number.txt"
if [ ! -f "$BUILD_NUMBER_FILE" ]; then
  echo "2" >"$BUILD_NUMBER_FILE"
fi

BUILD_NUMBER=$(cat "$BUILD_NUMBER_FILE")
BUILD_NUMBER=$((BUILD_NUMBER + 1))
echo "$BUILD_NUMBER" >"$BUILD_NUMBER_FILE"
echo "   Incrementing Build Number to: $BUILD_NUMBER"

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
DOWNLOADS_PROFILE="{profile_path}"

if [ -f "$DOWNLOADS_PROFILE" ]; then
  echo "📄 Found profile: $DOWNLOADS_PROFILE"
  cp "$DOWNLOADS_PROFILE" "$APP_PATH/embedded.mobileprovision"
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

fn script_android_release(app_name: &str, project_slug: &str, bundle_id: &str) -> String {
    // Generate right keystore config based on project_slug
    let is_abjad = project_slug.to_lowercase() == "abjad";
    let keystore_file = if is_abjad { "reset_upload_key.jks" } else { "tafseel-quran-release.keystore" };
    let key_alias_val = if is_abjad { "upload" } else { "tafseel-quran" };
    let key_pass_val = if is_abjad { "android" } else { "TafseelQuran2024!Secure" };

    // Special case for Android package name to bypass Play Console lock
    let android_package = if bundle_id == "com.mayorana.tafseel.abjad" {
        "com.mayorana.abjad"
    } else {
        bundle_id
    };

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
KEYSTORE_PATH="../keystores/{keystore_file}"
KEY_ALIAS="{key_alias_val}"
KEY_PASS="{key_pass_val}"

export ANDROID_HOME="$HOME/Library/Android/sdk"
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk"
NDK_VERSION=$(ls -1 "$ANDROID_NDK_HOME" 2>/dev/null | grep -E '^[0-9]+\.' | sort -V | tail -1)
export ANDROID_NDK_HOME="$ANDROID_NDK_HOME/$NDK_VERSION"

echo "🚀 Starting Android Release Build (AAB)..."
echo "NDK: $NDK_VERSION"

# 0. Check for Keystore
if [ ! -f "$KEYSTORE_PATH" ]; then
    echo "❌ Error: $KEYSTORE_PATH not found!"
    echo "   Please generate it first or place it in the root directory."
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

if grep -q "$OLD_PACKAGE" "$BUILD_GRADLE"; then
    echo "   Updating package to $NEW_PACKAGE..."
    sed -i '' "s/$OLD_PACKAGE/$NEW_PACKAGE/g" "$BUILD_GRADLE"
    find "$BUILD_DIR/app/src" -name "*.kt" -type f -exec sed -i '' "s/$OLD_PACKAGE/$NEW_PACKAGE/g" {{}} \;
    find "$BUILD_DIR/app/src" -name "AndroidManifest.xml" -type f -exec sed -i '' "s/$OLD_PACKAGE/$NEW_PACKAGE/g" {{}} \;
    find "$BUILD_DIR/app/src" -name "AndroidManifest.xml" -type f -exec sed -i '' 's/android:label="@string\/app_name"/android:label="{app_name}"/g' {{}} \;
fi

# 4. Inject Signing Config
echo "✍️  Injecting Signing Configuration..."

if ! grep -q "signingConfigs" "$BUILD_GRADLE"; then
    ABS_KEYSTORE_PATH="$(pwd)/$KEYSTORE_PATH"

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
    else
        echo "❌ Signature Verification FAILED."
        exit 1
    fi
else
    echo "❌ Build Failed: AAB not found."
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
cp -r manual_assets/android_icons/mipmap-* "$RES_DIR/"
echo "✅ Icons copied successfully."

BUILD_DIR="target/dx/$PROJECT_NAME/release/android/app"
BUILD_GRADLE="$BUILD_DIR/app/build.gradle.kts"
OLD_PACKAGE="{old_package}"
NEW_PACKAGE="{bundle_id}"

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

APK_PATH="$(find target/dx -name "*.apk" -type f -exec ls -t {{}} + 2>/dev/null | head -1)"
if [[ -n "$APK_PATH" ]]; then
    echo "📂 Opening APK folder..."
    open "$(dirname "$APK_PATH")"
fi
"##)
}

/// Write scripts to `project_dir` only if they don't already exist.
/// Returns a list of scripts that were newly created.
fn ensure_build_scripts(
    project_dir: &PathBuf,
    app_name: &str,
    project_slug: &str,
    bundle_id: &str,
    identity: &str,
    provisioning_profile: &str,
    short_version: &str,
) -> Vec<String> {
    let scripts: &[(&str, String)] = &[
        (
            "build_ios_distribution.sh",
            script_ios_distribution(app_name, project_slug, bundle_id, identity, provisioning_profile, short_version),
        ),
        (
            "build_android_release.sh",
            script_android_release(app_name, project_slug, bundle_id),
        ),
        (
            "build_android.sh",
            script_android(app_name, project_slug, bundle_id),
        ),
        (
            "build_apk.sh",
            script_build_apk(app_name, project_slug, bundle_id),
        ),
    ];

    let mut created = Vec::new();
    for (name, content) in scripts {
        let path = project_dir.join(name);
        if !path.exists() {
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
    }
    created
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

// Bottom-panel tab
#[derive(Clone, Debug, PartialEq)]
enum OutputTab {
    Progress,
    GeneratedImages,
    SavedScreenshots,
    Publish,
    Build,
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

    let (icon_rgba, icon_width, icon_height) = {
        let img = image::load_from_memory(include_bytes!("../assets/icon.png"))
            .expect("Failed to load icon")
            .into_rgba8();
        let (width, height) = img.dimensions();
        let rgba = img.into_raw();
        (rgba, width, height)
    };

    let icon = dioxus::desktop::tao::window::Icon::from_rgba(icon_rgba, icon_width, icon_height)
        .expect("Failed to create icon");

    let config = dioxus::desktop::Config::new().with_window(
        WindowBuilder::new()
            .with_title("AppScreens")
            .with_window_icon(Some(icon))
            .with_inner_size(LogicalSize::new(1020.0, 860.0))
            .with_focused(true)
            .with_decorations(true)
            .with_transparent(false),
    );

    LaunchBuilder::desktop().with_cfg(config).launch(App);
}

// ---------------------------------------------------------------------------
// Root App
// ---------------------------------------------------------------------------
#[component]
fn App() -> Element {
    // Global settings
    let mut settings = use_signal(load_settings);
    use_context_provider(|| settings);

    // Active project dir (None = show project picker)
    let mut project_dir = use_signal(|| Option::<PathBuf>::None);

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
        document::Link { rel: "stylesheet", href: MAIN_CSS }
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

// ---------------------------------------------------------------------------
// Project Picker (shown on launch)
// ---------------------------------------------------------------------------
#[component]
fn ProjectPicker(on_open: EventHandler<PathBuf>) -> Element {
    let settings = use_context::<Signal<Settings>>();

    rsx! {
        div { class: "picker-screen",
            div { class: "picker-inner",
                div { class: "picker-logo",
                    h1 { "AppScreens" }
                    p { class: "picker-subtitle", "App Store & Play Store screenshot generator" }
                }

                button {
                    class: "btn btn-primary picker-new-btn",
                    onclick: move |_| {
                        spawn(async move {
                            if let Some(folder) = rfd::AsyncFileDialog::new()
                                .set_title("Choose or create a project folder")
                                .pick_folder()
                                .await
                            {
                                on_open.call(folder.path().to_path_buf());
                            }
                        });
                    },
                    "Open / Create Project Folder…"
                }

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
// Project View (main workspace)
// ---------------------------------------------------------------------------
#[component]
fn ProjectView(project_dir: PathBuf, on_close: EventHandler<()>) -> Element {
    let settings = use_context::<Signal<Settings>>();

    // Per-project persistent state
    let mut proj = use_signal(|| load_project_state(&project_dir));

    // Derived: fastlane ios screenshots path lives inside the project
    let fastlane_path = project_dir.join("fastlane").join("screenshots").join("ios");

    let mut phase = use_signal(|| AppPhase::Idle);
    let mut log_lines = use_signal(|| Vec::<String>::new());
    let mut show_settings = use_signal(|| false);
    let mut output_tab = use_signal(|| OutputTab::Progress);
    // Active locale tab shown in the Source Screenshots card for editing per-locale texts
    let mut active_locale_tab: Signal<String> = use_signal(|| {
        load_project_state(&project_dir).locales.first().cloned().unwrap_or_else(|| "en-US".to_string())
    });
    // Whether the "add language" picker dropdown is open
    let mut show_lang_picker = use_signal(|| false);

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
    let mut publish_phase = use_signal(|| PublishPhase::Idle);
    let mut publish_log = use_signal(|| Vec::<String>::new());
    let mut android_publish_phase = use_signal(|| AndroidPublishPhase::Idle);
    let mut android_publish_log = use_signal(|| Vec::<String>::new());
    let mut build_phase = use_signal(|| BuildPhase::Idle);
    let mut build_log = use_signal(|| Vec::<String>::new());

    let proj_dir = project_dir.clone();
    let proj_dir2 = project_dir.clone();
    let proj_dir3 = project_dir.clone();
    let proj_dir4 = project_dir.clone();
    let proj_dir5 = project_dir.clone();
    let proj_dir6 = project_dir.clone();

    // Helper: save project state
    let _save_proj = {
        let proj_dir = project_dir.clone();
        move || save_project_state(&proj_dir, &proj.read())
    };

    // Helper: push log line
    let mut add_log = move |msg: String| {
        log_lines.write().push(msg);
    };

    // ---- Logo picker ----
    let pick_logo = move |_| {
        let proj_dir = proj_dir3.clone();
        spawn(async move {
            let file = rfd::AsyncFileDialog::new()
                .set_title("Choose App Logo")
                .add_filter("Images", &["png", "jpg", "jpeg"])
                .pick_file()
                .await;
            if let Some(selected) = file {
                let mut p = proj.write();
                p.logo_path = Some(selected.path().to_path_buf());
                save_project_state(&proj_dir, &p);
            }
        });
    };

    // ---- Publish to App Store Connect via fastlane ----
    let mut on_publish = {
        let proj_dir = proj_dir4.clone();
        move |_| {
            publish_phase.set(PublishPhase::Running);
            publish_log.set(Vec::new());
            output_tab.set(OutputTab::Publish);

            let fastlane_dir = proj_dir.clone();
            spawn(async move {
                use std::io::{BufRead, BufReader};
                use std::process::Stdio;
                use std::sync::mpsc;

                // Load .env from AppScreens binary dir and project dir (project wins).
                let env_overrides = load_env(&[
                    fastlane_dir.join(".env"),
                    fastlane_dir.join("fastlane").join(".env"),
                ]);

                // Helper: resolve a key from merged .env or the process environment.
                let resolve = |key: &str| -> Option<String> {
                    env_overrides.get(key).cloned()
                        .or_else(|| std::env::var(key).ok())
                        .filter(|v| !v.is_empty())
                };

                // Validate required App Store Connect API key env vars before running fastlane.
                const REQUIRED: &[&str] = &[
                    "APP_STORE_CONNECT_API_KEY_KEY_ID",
                    "APP_STORE_CONNECT_API_KEY_ISSUER_ID",
                    "APP_STORE_CONNECT_API_KEY_KEY_FILEPATH",
                ];
                let missing: Vec<&str> = REQUIRED.iter()
                    .copied()
                    .filter(|k| resolve(k).is_none())
                    .collect();
                if !missing.is_empty() {
                    let msg = format!(
                        "Missing required environment variables:\n{}\n\nSet them in fastlane/.env or export them before launching the app.",
                        missing.iter().map(|k| format!("  • {k}")).collect::<Vec<_>>().join("\n")
                    );
                    publish_phase.set(PublishPhase::Error(msg));
                    return;
                }

                // Spawn the fastlane process on a plain OS thread (Dioxus Signals are
                // !Send so they can't enter tokio::spawn_blocking).  Lines are sent back
                // to this async task via an mpsc channel and pushed into the signal here,
                // on the UI thread, so Dioxus re-renders after every poll interval.
                let (tx, rx) = mpsc::channel::<Result<String, String>>();

                let env_overrides_clone = env_overrides.clone();
                std::thread::spawn(move || {
                    // Resolve the full path to `bundle` so the subprocess finds it even
                    // when launched from a GUI context (launchd strips PATH to /usr/bin etc).
                    // Walk the shell PATH to find `bundle`, falling back to known RVM location.
                    let bundle_bin = std::env::var("PATH")
                        .unwrap_or_default()
                        .split(':')
                        .map(|dir| std::path::PathBuf::from(dir).join("bundle"))
                        .find(|p| p.is_file())
                        .unwrap_or_else(|| {
                            // Hardcoded RVM fallback
                            std::path::PathBuf::from(
                                std::env::var("HOME").unwrap_or_else(|_| "/Users/mb".into())
                            )
                            .join(".rvm/gems/ruby-3.2.2/bin/bundle")
                        });

                    let mut cmd = std::process::Command::new(&bundle_bin);
                    cmd.args(["exec", "fastlane", "upload_screenshots"])
                       .current_dir(&fastlane_dir)
                       .env("FASTLANE_HIDE_CHANGELOG", "1")
                       .env("FASTLANE_SKIP_UPDATE_CHECK", "1")
                       .env("CI", "1")
                       // Forward the current PATH so bundle/ruby/fastlane can find their deps
                       .env("PATH", std::env::var("PATH").unwrap_or_default())
                       .stdout(Stdio::piped())
                       .stderr(Stdio::piped());
                    for (k, v) in &env_overrides_clone {
                        cmd.env(k, v);
                    }

                    let mut child = match cmd.spawn() {
                        Ok(c) => c,
                        Err(e) => { let _ = tx.send(Err(format!("spawn: {e}"))); return; }
                    };

                    // Pipe stderr on its own thread; send lines into the same channel.
                    let tx2 = tx.clone();
                    let stderr_stream = child.stderr.take();
                    std::thread::spawn(move || {
                        if let Some(r) = stderr_stream.map(BufReader::new) {
                            for line in r.lines().map_while(Result::ok) {
                                let _ = tx2.send(Ok(line));
                            }
                        }
                    });

                    if let Some(stdout) = child.stdout.take() {
                        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                            let _ = tx.send(Ok(line));
                        }
                    }

                    match child.wait() {
                        Ok(s) if s.success() => { let _ = tx.send(Err("__ok__".into())); }
                        Ok(s) => { let _ = tx.send(Err(format!("fastlane exited with code {}", s.code().unwrap_or(-1)))); }
                        Err(e) => { let _ = tx.send(Err(format!("wait: {e}"))); }
                    }
                });

                // Poll the channel from the async task so we can write to the Signal
                // (which lives on the UI thread) without crossing thread boundaries.
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    let mut done = false;
                    for msg in rx.try_iter() {
                        match msg {
                            Ok(line) => {
                                let clean = strip_ansi(&line);
                                if !clean.trim().is_empty() {
                                    publish_log.write().push(clean);
                                }
                            }
                            Err(sentinel) if sentinel == "__ok__" => {
                                publish_phase.set(PublishPhase::Success);
                                done = true;
                            }
                            Err(err_msg) => {
                                publish_phase.set(PublishPhase::Error(err_msg));
                                done = true;
                            }
                        }
                    }
                    if done { break; }
                }
            });
        }
    };

    // ---- Publish Android screenshots to Google Play via androidpublisher v3 ----
    let mut on_android_publish = {
        let proj_dir = proj_dir6.clone();
        move |_| {
            android_publish_phase.set(AndroidPublishPhase::Running);
            android_publish_log.set(Vec::new());
            output_tab.set(OutputTab::Publish);

            // Snapshot what we need from project state before moving into async.
            let bundle_id = proj.read().bundle_id.clone();
            let output_paths = proj.read().output_paths.clone();

            let proj_dir = proj_dir.clone();
            spawn(async move {
                let mut log = android_publish_log.clone();
                let mut push = |msg: &str| log.write().push(msg.to_string());

                // ── 1. Resolve env vars ──────────────────────────────────────
                // Load from AppScreens binary dir and project dir (project wins).
                let env = load_env(&[
                    proj_dir.join(".env"),
                    proj_dir.join("fastlane").join(".env"),
                ]);
                let resolve = |key: &str| -> Option<String> {
                    env.get(key).cloned()
                        .or_else(|| std::env::var(key).ok())
                        .filter(|v| !v.is_empty())
                };

                // GOOGLE_PLAY_JSON_KEY  – path to service-account .json file
                // ANDROID_PACKAGE_NAME  – optional override (falls back to bundle_id)
                let json_key_path = match resolve("GOOGLE_PLAY_JSON_KEY") {
                    Some(p) => p,
                    None => {
                        android_publish_phase.set(AndroidPublishPhase::Error(
                            "Missing GOOGLE_PLAY_JSON_KEY env var.\nSet it to the path of your Google service-account JSON file in fastlane/.env or export it before launching the app.".into()
                        ));
                        return;
                    }
                };
                let package_name = resolve("ANDROID_PACKAGE_NAME")
                    .or_else(|| if bundle_id.is_empty() { None } else { Some(bundle_id.clone()) })
                    .unwrap_or_default();
                if package_name.is_empty() {
                    android_publish_phase.set(AndroidPublishPhase::Error(
                        "Cannot determine Android package name.\nSet ANDROID_PACKAGE_NAME in fastlane/.env or fill in Bundle ID in the project settings.".into()
                    ));
                    return;
                }

                push(&format!("📦 Package: {package_name}"));

                // ── 2. Read & parse service-account JSON ─────────────────────
                let sa_json: serde_json::Value = match std::fs::read_to_string(&json_key_path)
                    .map_err(|e| e.to_string())
                    .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
                {
                    Ok(v) => v,
                    Err(e) => {
                        android_publish_phase.set(AndroidPublishPhase::Error(
                            format!("Failed to read service-account JSON at {json_key_path}: {e}")
                        ));
                        return;
                    }
                };
                let client_email = sa_json["client_email"].as_str().unwrap_or("").to_string();
                let private_key_pem = sa_json["private_key"].as_str().unwrap_or("").to_string();
                if client_email.is_empty() || private_key_pem.is_empty() {
                    android_publish_phase.set(AndroidPublishPhase::Error(
                        "Service-account JSON is missing client_email or private_key.".into()
                    ));
                    return;
                }

                // ── 3. Mint a short-lived OAuth2 JWT and exchange for access token ──
                push("🔑 Authenticating with Google…");
                let access_token = match google_play_access_token(&client_email, &private_key_pem).await {
                    Ok(t) => t,
                    Err(e) => {
                        android_publish_phase.set(AndroidPublishPhase::Error(
                            format!("Authentication failed: {e}")
                        ));
                        return;
                    }
                };
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
                // Labels look like "Screen 1 → Android Phone" / "Screen 1 → Android Feature"
                // Android screenshots are locale-agnostic (text is baked in per-locale at generation).
                // We upload to all selected locales using the same screenshot files.
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

                if android_files.is_empty() {
                    android_publish_phase.set(AndroidPublishPhase::Error(
                        "No Android screenshots found. Generate screenshots first.".into()
                    ));
                    let _ = google_play_delete_edit(&access_token, &package_name, &edit_id).await;
                    return;
                }

                push(&format!("🖼  Found {} Android screenshot(s) to upload across {} locale(s)",
                    android_files.len(), play_languages.len()));

                // ── 6. Upload each screenshot per locale ──────────────────────
                for play_language in &play_languages {
                    push(&format!("─── Locale: {play_language} ───"));

                    // Collect paths for this locale (prefer locale-specific files if they exist)
                    let mut phone_paths: Vec<PathBuf> = Vec::new();
                    let mut feature_paths: Vec<PathBuf> = Vec::new();

                    // First try locale-specific paths (generated by multi-locale flow)
                    let locale_phone: Vec<PathBuf> = android_files.iter()
                        .filter(|(l, p)| l.contains("Android Phone") && p.to_string_lossy().contains(play_language.as_str()))
                        .map(|(_, p)| p.clone()).collect();
                    let locale_feature: Vec<PathBuf> = android_files.iter()
                        .filter(|(l, p)| l.contains("Android Feature") && p.to_string_lossy().contains(play_language.as_str()))
                        .map(|(_, p)| p.clone()).collect();

                    if !locale_phone.is_empty() || !locale_feature.is_empty() {
                        phone_paths = locale_phone;
                        feature_paths = locale_feature;
                    } else {
                        // Fall back to any Android screenshots (single-locale generation)
                        for (label, path) in &android_files {
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
                            &access_token, &package_name, &edit_id, play_language, image_type
                        ).await;

                        for path in paths {
                            let fname = path.file_name().unwrap_or_default().to_string_lossy();
                            push(&format!("⬆  [{play_language}] Uploading {fname} ({image_type})…"));
                            if let Err(e) = google_play_upload_image(
                                &access_token, &package_name, &edit_id,
                                play_language, image_type, path
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

                push("🎉 Android screenshots uploaded to Google Play successfully!");
                android_publish_phase.set(AndroidPublishPhase::Success);
            });
        }
    };

    // ---- Run build script ----
    // Ensures scripts exist (generates if missing), then runs the chosen one.
    let on_run_script = {
        let proj_dir = proj_dir5.clone();
        move |script_name: String| {
            let app_name = proj.read().app_name.clone();
            let project_slug = proj.read().project_slug.clone();
            let bundle_id = proj.read().bundle_id.clone();
            let identity = settings.read().apple_identity.clone();
            let profile = settings.read().provisioning_profile.clone();
            let short_version = settings.read().ios_short_version.clone();

            // Validate required fields
            if app_name.trim().is_empty() || project_slug.trim().is_empty() || bundle_id.trim().is_empty() {
                build_phase.set(BuildPhase::Error(
                    "App Name, Project Slug, and Bundle ID are required. Fill them in the Build Config card.".into()
                ));
                output_tab.set(OutputTab::Build);
                return;
            }
            if script_name.contains("ios") && (identity.trim().is_empty() || profile.trim().is_empty()) {
                build_phase.set(BuildPhase::Error(
                    "Apple Identity and Provisioning Profile are required for iOS. Set them in ⚙ Settings.".into()
                ));
                output_tab.set(OutputTab::Build);
                return;
            }

            build_phase.set(BuildPhase::Running(script_name.clone()));
            build_log.set(Vec::new());
            output_tab.set(OutputTab::Build);

            let dir = proj_dir.clone();
            spawn(async move {
                // 1. Ensure scripts exist (create if missing)
                let created = tokio::task::spawn_blocking({
                    let dir = dir.clone();
                    let app_name = app_name.clone();
                    let project_slug = project_slug.clone();
                    let bundle_id = bundle_id.clone();
                    let identity = identity.clone();
                    let profile = profile.clone();
                    let short_version = short_version.clone();
                    move || ensure_build_scripts(&dir, &app_name, &project_slug, &bundle_id, &identity, &profile, &short_version)
                }).await.unwrap_or_default();

                for name in &created {
                    build_log.write().push(format!("📝 Created {name}"));
                }

                // 2. Run the script
                let script_path = dir.join(&script_name);
                if !script_path.exists() {
                    build_phase.set(BuildPhase::Error(format!("{script_name} not found in project directory")));
                    return;
                }

                build_log.write().push(format!("▶ Running {script_name}…"));

                // Spawn the bash script on a plain OS thread (Signals are !Send).
                // Lines flow back via mpsc; we poll from this async task and push
                // to the Signal here on the UI thread so Dioxus re-renders live.
                {
                    use std::io::{BufRead, BufReader};
                    use std::process::Stdio;
                    use std::sync::mpsc;

                    let (tx, rx) = mpsc::channel::<Result<String, String>>();
                    let script_name_for_thread = script_name.clone();

                    std::thread::spawn(move || {
                        let script_name = script_name_for_thread;
                        let mut cmd = std::process::Command::new("bash");
                        cmd.arg(&script_path)
                           .current_dir(&dir)
                           .env("CI", "1")
                           .stdout(Stdio::piped())
                           .stderr(Stdio::piped());
                        for (k, v) in load_env(&[dir.join(".env")]) {
                            cmd.env(k, v);
                        }

                        let mut child = match cmd.spawn() {
                            Ok(c) => c,
                            Err(e) => { let _ = tx.send(Err(format!("spawn: {e}"))); return; }
                        };

                        let tx2 = tx.clone();
                        let stderr_stream = child.stderr.take();
                        std::thread::spawn(move || {
                            if let Some(r) = stderr_stream.map(BufReader::new) {
                                for line in r.lines().map_while(Result::ok) {
                                    let _ = tx2.send(Ok(line));
                                }
                            }
                        });

                        if let Some(stdout) = child.stdout.take() {
                            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                                let _ = tx.send(Ok(line));
                            }
                        }

                        match child.wait() {
                            Ok(s) if s.success() => { let _ = tx.send(Err("__ok__".into())); }
                            Ok(s) => { let _ = tx.send(Err(format!("{script_name} exited with code {}", s.code().unwrap_or(-1)))); }
                            Err(e) => { let _ = tx.send(Err(format!("wait: {e}"))); }
                        }
                    });

                    loop {
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        let mut done = false;
                        for msg in rx.try_iter() {
                            match msg {
                                Ok(line) => {
                                    let clean = strip_ansi(&line);
                                    if !clean.trim().is_empty() {
                                        build_log.write().push(clean);
                                    }
                                }
                                Err(sentinel) if sentinel == "__ok__" => {
                                    build_phase.set(BuildPhase::Success(script_name.clone()));
                                    done = true;
                                }
                                Err(err_msg) => {
                                    build_phase.set(BuildPhase::Error(err_msg));
                                    done = true;
                                }
                            }
                        }
                        if done { break; }
                    }
                }
            });
        }
    };

    // ---- File picker — adds images to the currently active locale tab ----
    let pick_files = move |_| {
        let proj_dir = proj_dir.clone();
        let locale_for_pick = active_locale_tab.read().clone();
        spawn(async move {
            let files = rfd::AsyncFileDialog::new()
                .add_filter("Images", &["png", "jpg", "jpeg"])
                .pick_files()
                .await;
            if let Some(selected) = files {
                let mut p = proj.write();
                let srcs = p.locale_sources
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
    };

    // ---- AI Generate ----
    let mut on_generate_ai = {
        let fastlane_path = fastlane_path.clone();
        let proj_dir = project_dir.clone();
        move |_| {
            let prompt = proj.read().theme_prompt.clone();
            let selected_locales = proj.read().locales.clone();
            // Per-locale sources: locale → Vec<PathBuf>
            let locale_srcs: std::collections::HashMap<String, Vec<PathBuf>> = selected_locales
                .iter()
                .map(|loc| (loc.clone(), proj.read().sources_for(loc)))
                .collect();
            let ios_enabled = proj.read().ios_targets.clone();
            let android_enabled = proj.read().android_targets.clone();
            let export_ios = proj.read().export_ios;
            let export_android = proj.read().export_android;
            let fl_path = fastlane_path.to_string_lossy().to_string();

            // Check every locale has at least one image
            let empty_locales: Vec<&String> = selected_locales.iter()
                .filter(|loc| locale_srcs.get(*loc).map(|v| v.is_empty()).unwrap_or(true))
                .collect();
            if !empty_locales.is_empty() {
                phase.set(AppPhase::Error(format!(
                    "No images for locale(s): {}. Add images in each language tab.",
                    empty_locales.iter().map(|l| l.as_str()).collect::<Vec<_>>().join(", ")
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

            spawn(async move {
                phase.set(AppPhase::GeneratingAi);
                log_lines.set(Vec::new());
                output_tab.set(OutputTab::Progress);
                {
                    let mut p = proj.write();
                    p.generated_urls.clear();
                    p.output_paths.clear();
                }

                if api_key.is_empty() {
                    phase.set(AppPhase::Error(
                        "No fal.ai API key set. Click the gear icon.".into(),
                    ));
                    return;
                }

                let frame_w: u32 = 1290;
                let frame_h: u32 = 2796;
                let mut all_outputs: Vec<(String, PathBuf)> = Vec::new();
                let ios_e: Vec<bool> = if export_ios { ios_enabled.clone() } else { vec![] };
                let android_e: Vec<bool> = if export_android { android_enabled.clone() } else { vec![] };

                // Each locale has its own images — process independently
                for locale in &selected_locales {
                    let srcs = locale_srcs.get(locale).cloned().unwrap_or_default();
                    let total = srcs.len();
                    add_log(format!("─── AI generating locale: {locale} ({total} image(s)) ───"));

                    for (idx, src) in srcs.iter().enumerate() {
                        let screen_num = idx + 1;
                        add_log(format!(
                            "[{locale}] Reading image {screen_num}/{total}: {}",
                            src.file_name().unwrap_or_default().to_string_lossy()
                        ));

                        let img_bytes = match std::fs::read(src) {
                            Ok(b) => b,
                            Err(e) => {
                                phase.set(AppPhase::Error(format!(
                                    "[{locale}] Failed to read image {screen_num}: {e}"
                                )));
                                return;
                            }
                        };
                        let screenshot = match image::load_from_memory(&img_bytes) {
                            Ok(img) => img.to_rgba8(),
                            Err(e) => {
                                phase.set(AppPhase::Error(format!(
                                    "[{locale}] Failed to decode image {screen_num}: {e}"
                                )));
                                return;
                            }
                        };

                        let full_prompt = build_prompt(&prompt, &phone_style, screen_num, total);
                        add_log(format!("[{locale}] Generating frame {screen_num}/{total} via FLUX…"));

                        let frame_url = match call_fal_text_to_image(
                            &api_key, &full_prompt, frame_w, frame_h, steps_val,
                        ).await {
                            Ok(url) => { add_log(format!("Got frame {screen_num}")); url }
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
                        let mut frame_img = match image::load_from_memory(&frame_bytes) {
                            Ok(img) => img.to_rgba8(),
                            Err(e) => {
                                phase.set(AppPhase::Error(format!(
                                    "[{locale}] Failed to decode frame {screen_num}: {e}"
                                )));
                                return;
                            }
                        };

                        phase.set(AppPhase::Resizing);
                        let rect = find_placeholder_rect(&frame_img).unwrap_or_else(|| {
                            fallback_placement(frame_img.width(), frame_img.height())
                        });
                        add_log(format!("[{locale}] Compositing screenshot {screen_num}…"));
                        composite_screenshot(&mut frame_img, &screenshot, rect);

                        let composited_bytes = {
                            let mut buf = std::io::Cursor::new(Vec::new());
                            if let Err(e) = frame_img.write_to(&mut buf, image::ImageFormat::Png) {
                                phase.set(AppPhase::Error(format!(
                                    "[{locale}] Failed to encode image {screen_num}: {e}"
                                )));
                                return;
                            }
                            buf.into_inner()
                        };

                        match resize_to_targets(&composited_bytes, screen_num, total, locale, &fl_path, &ios_e, &android_e) {
                            Ok(paths) => {
                                for (label, _p) in &paths { add_log(format!("Saved: {label}")); }
                                if let Some((_, p)) = paths.iter().find(|(l, _)| l.contains("6.7")) {
                                    let encoded = urlencoding::encode(&p.to_string_lossy().to_string()).into_owned();
                                    let mut pw = proj.write();
                                    if !pw.generated_urls.iter().any(|u| u.contains(&p.to_string_lossy().to_string())) {
                                        pw.generated_urls.push(format!("/localimg/{encoded}"));
                                    }
                                }
                                all_outputs.extend(paths);
                            }
                            Err(e) => {
                                phase.set(AppPhase::Error(format!(
                                    "Resize failed for [{locale}] screen {screen_num}: {e}"
                                )));
                                return;
                            }
                        }
                        phase.set(AppPhase::GeneratingAi);
                    }
                }

                {
                    let mut p = proj.write();
                    p.output_paths = all_outputs;
                    save_project_state(&proj_dir, &p);
                }
                let loc_count = selected_locales.len();
                add_log(format!("Done! AI screenshots generated for {loc_count} locale(s)."));
                phase.set(AppPhase::Done);
                output_tab.set(OutputTab::GeneratedImages);
            });
        }
    };

    // ---- Manual Generate ----
    let mut on_generate_manual = {
        let fastlane_path = fastlane_path.clone();
        let proj_dir = project_dir.clone();
        move |_| {
            let primary = proj.read().primary_color.clone();
            let secondary = proj.read().secondary_color.clone();
            let selected_locales = proj.read().locales.clone();
            // Per-locale sources and texts
            let locale_srcs: std::collections::HashMap<String, Vec<PathBuf>> = selected_locales
                .iter()
                .map(|loc| (loc.clone(), proj.read().sources_for(loc)))
                .collect();
            let locale_texts_map: std::collections::HashMap<String, Vec<(String,String)>> = selected_locales
                .iter()
                .map(|loc| (loc.clone(), proj.read().texts_for(loc)))
                .collect();
            let ios_enabled = proj.read().ios_targets.clone();
            let android_enabled = proj.read().android_targets.clone();
            let export_ios = proj.read().export_ios;
            let export_android = proj.read().export_android;
            let fl_path = fastlane_path.to_string_lossy().to_string();
            let proj_dir = proj_dir.clone();

            // Check every locale has at least one image
            let empty_locales: Vec<&String> = selected_locales.iter()
                .filter(|loc| locale_srcs.get(*loc).map(|v| v.is_empty()).unwrap_or(true))
                .collect();
            if !empty_locales.is_empty() {
                phase.set(AppPhase::Error(format!(
                    "No images for locale(s): {}. Add images in each language tab.",
                    empty_locales.iter().map(|l| l.as_str()).collect::<Vec<_>>().join(", ")
                )));
                return;
            }

            spawn(async move {
                phase.set(AppPhase::GeneratingManual);
                log_lines.set(Vec::new());
                output_tab.set(OutputTab::Progress);
                {
                    let mut p = proj.write();
                    p.generated_urls.clear();
                    p.output_paths.clear();
                }

                let width = 1290u32;
                let height = 2796u32;
                let primary_rgba = parse_hex_color(&primary).unwrap_or(Rgba([59, 130, 246, 255]));
                let secondary_rgba = if !secondary.is_empty() {
                    parse_hex_color(&secondary).unwrap_or(lighten_color(primary_rgba))
                } else {
                    lighten_color(primary_rgba)
                };
                let font = FontRef::try_from_slice(ROBOTO_FONT).expect("Error constructing Font");
                let mut all_outputs: Vec<(String, PathBuf)> = Vec::new();

                // Each locale has its own images — process independently
                for locale in &selected_locales {
                    let srcs = locale_srcs.get(locale).cloned().unwrap_or_default();
                    let total = srcs.len();
                    let texts = locale_texts_map.get(locale).cloned().unwrap_or_default();
                    add_log(format!("─── Generating locale: {locale} ({total} image(s)) ───"));

                    for (idx, src) in srcs.iter().enumerate() {
                        let screen_num = idx + 1;
                        add_log(format!("[{locale}] Processing image {screen_num}/{total}…"));

                        let bg_color = if idx % 2 == 0 {
                            primary_rgba
                        } else {
                            secondary_rgba
                        };
                        let mut img = RgbaImage::from_pixel(width, height, bg_color);
                        let text_color = get_contrast_color(bg_color);

                        let (title, subtitle) = texts.get(idx).cloned().unwrap_or_default();
                        if !title.is_empty() {
                            draw_centered_text(
                                &mut img,
                                &font,
                                &title,
                                PxScale::from(120.0),
                                text_color,
                                200,
                            );
                        }
                        if !subtitle.is_empty() {
                            draw_centered_text(
                                &mut img,
                                &font,
                                &subtitle,
                                PxScale::from(60.0),
                                text_color,
                                350,
                            );
                        }

                        let screenshot_bytes = match std::fs::read(src) {
                            Ok(b) => b,
                            Err(e) => {
                                phase.set(AppPhase::Error(format!(
                                    "Failed to read image {screen_num}: {e}"
                                )));
                                return;
                            }
                        };
                        let screenshot = match image::load_from_memory(&screenshot_bytes) {
                            Ok(i) => i.to_rgba8(),
                            Err(e) => {
                                phase.set(AppPhase::Error(format!(
                                    "Failed to decode image {screen_num}: {e}"
                                )));
                                return;
                            }
                        };

                        let phone_w = (width as f64 * 0.7) as u32;
                        let phone_h = (phone_w as f64 * 2.16) as u32;
                        let phone_x = (width - phone_w) / 2;
                        let phone_y = height - phone_h - 150;
                        draw_phone_frame(&mut img, phone_x, phone_y, phone_w, phone_h);

                        let bezel = 30u32;
                        let resized_screenshot = image::imageops::resize(
                            &screenshot,
                            phone_w - bezel * 2,
                            phone_h - bezel * 2,
                            FilterType::Lanczos3,
                        );
                        image::imageops::overlay(
                            &mut img,
                            &resized_screenshot,
                            (phone_x + bezel) as i64,
                            (phone_y + bezel) as i64,
                        );

                        // Replace any Android status bar with a clean iOS one
                        draw_ios_status_bar(
                            &mut img,
                            &font,
                            phone_x + bezel,
                            phone_y + bezel,
                            phone_w - bezel * 2,
                        );

                        let composited_bytes = {
                            let mut buf = std::io::Cursor::new(Vec::new());
                            img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
                            buf.into_inner()
                        };

                        let ios_e = if export_ios { &ios_enabled[..] } else { &[] };
                        let android_e = if export_android { &android_enabled[..] } else { &[] };
                        match resize_to_targets(&composited_bytes, screen_num, total, locale, &fl_path, ios_e, android_e)
                        {
                            Ok(paths) => {
                                for (label, _p) in &paths {
                                    add_log(format!("Saved: {label}"));
                                }
                                if let Some((_, p)) = paths.iter().find(|(l, _)| l.contains("6.7")) {
                                    let encoded = urlencoding::encode(&p.to_string_lossy().to_string())
                                        .into_owned();
                                    proj.write()
                                        .generated_urls
                                        .push(format!("/localimg/{encoded}"));
                                }
                                all_outputs.extend(paths);
                            }
                            Err(e) => {
                                phase.set(AppPhase::Error(format!(
                                    "Resize failed for [{locale}] screen {screen_num}: {e}"
                                )));
                                return;
                            }
                        }
                    }
                }

                {
                    let mut p = proj.write();
                    p.output_paths = all_outputs;
                    save_project_state(&proj_dir, &p);
                }
                let loc_count = selected_locales.len();
                add_log(format!("Done! Manual screenshots generated for {loc_count} locale(s)."));
                phase.set(AppPhase::Done);
                output_tab.set(OutputTab::GeneratedImages);
            });
        }
    };

    let is_busy = matches!(
        *phase.read(),
        AppPhase::GeneratingAi | AppPhase::GeneratingManual | AppPhase::Resizing
    );
    let proj_name = project_dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let fastlane_display = fastlane_path.to_string_lossy().to_string();

    rsx! {
        div { class: "app-container",
            // ---- Header ----
            div { class: "header-row",
                div { class: "header-left",
                    button {
                        class: "btn btn-icon btn-back",
                        onclick: move |_| on_close.call(()),
                        title: "Back to project picker",
                        "‹"
                    }
                    div {
                        h1 { "{proj_name}" }
                        p { class: "subtitle", "{fastlane_display}" }
                    }
                }
                button {
                    class: "btn btn-icon",
                    onclick: move |_| show_settings.toggle(),
                    title: "Settings",
                    svg {
                        width: "20", height: "20", view_box: "0 0 20 20", fill: "currentColor",
                        path { d: "M11.49 3.17c-.38-1.56-2.6-1.56-2.98 0a1.532 1.532 0 01-2.286.948c-1.372-.836-2.942.734-2.106 2.106.54.886.061 2.042-.947 2.287-1.561.379-1.561 2.6 0 2.978a1.532 1.532 0 01.947 2.287c-.836 1.372.734 2.942 2.106 2.106a1.532 1.532 0 012.287.947c.379 1.561 2.6 1.561 2.978 0a1.533 1.533 0 012.287-.947c1.372.836 2.942-.734 2.106-2.106a1.533 1.533 0 01.947-2.287c1.561-.379 1.561-2.6 0-2.978a1.532 1.532 0 01-.947-2.287c.836-1.372-.734-2.942-2.106-2.106a1.532 1.532 0 01-2.287-.947zM10 13a3 3 0 100-6 3 3 0 000 6z" }
                    }
                }
            }

            if *show_settings.read() {
                SettingsPopup { on_close: move |_| show_settings.set(false) }
            }

            // ---- 1. Build Config ----
            div { class: "card",
                h2 { "1. Build Config" }
                p { class: "hint card-hint",
                    "Used to generate build scripts for this project. Scripts are created once in the project folder and never overwritten."
                }
                div { class: "build-config-grid",
                    div { class: "build-config-field",
                        label { class: "build-config-label", "App Name" }
                        input {
                            class: "text-input",
                            placeholder: "Abjad",
                            value: "{proj.read().app_name}",
                            oninput: {
                                let proj_dir_save = project_dir.clone();
                                move |e: Event<FormData>| {
                                    let mut p = proj.write();
                                    p.app_name = e.value();
                                    save_project_state(&proj_dir_save, &p);
                                }
                            }
                        }
                        p { class: "settings-hint", "Display name, e.g. \"Abjad\"" }
                    }
                    div { class: "build-config-field",
                        label { class: "build-config-label", "Project Slug" }
                        input {
                            class: "text-input",
                            placeholder: "abjad",
                            value: "{proj.read().project_slug}",
                            oninput: {
                                let proj_dir_save = project_dir.clone();
                                move |e: Event<FormData>| {
                                    let mut p = proj.write();
                                    p.project_slug = e.value();
                                    save_project_state(&proj_dir_save, &p);
                                }
                            }
                        }
                        p { class: "settings-hint", "Lowercase dx slug, e.g. \"abjad\"" }
                    }
                    div { class: "build-config-field",
                        label { class: "build-config-label", "Bundle ID" }
                        input {
                            class: "text-input",
                            placeholder: "com.company.app",
                            value: "{proj.read().bundle_id}",
                            oninput: {
                                let proj_dir_save = project_dir.clone();
                                move |e: Event<FormData>| {
                                    let mut p = proj.write();
                                    p.bundle_id = e.value();
                                    save_project_state(&proj_dir_save, &p);
                                }
                            }
                        }
                        p { class: "settings-hint", "iOS & Android bundle ID, e.g. \"com.co.app\"" }
                    }
                }
            }

            // ---- 2. App Logo ----
            div { class: "card",
                h2 { "2. App Logo (optional)" }
                p { class: "hint card-hint", "Upload your app icon to use as a watermark / logo overlay." }
                div { class: "logo-upload-row",
                    button { class: "btn", onclick: pick_logo, "Choose Logo…" }
                    if let Some(logo) = proj.read().logo_path.clone() {
                        {
                            let logo_str = logo.to_string_lossy().to_string();
                            let encoded = urlencoding::encode(&logo_str).into_owned();
                            let thumb_src = format!("/localimg/{encoded}");
                            let proj_dir_save = project_dir.clone();
                            rsx! {
                                div { class: "logo-preview",
                                    img { class: "logo-thumb", src: "{thumb_src}", alt: "Logo preview" }
                                    div { class: "logo-info",
                                        span { class: "logo-path", title: "{logo_str}", "{logo_str}" }
                                        button {
                                            class: "btn-remove",
                                            title: "Remove logo",
                                            onclick: move |_| {
                                                let mut p = proj.write();
                                                p.logo_path = None;
                                                save_project_state(&proj_dir_save, &p);
                                            },
                                            "✕"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ---- 3. Export Settings ----
            div { class: "card export-settings-card",
                h2 { "3. Export Settings" }
                div { class: "export-settings-grid",

                    // iOS targets
                    div { class: "export-section",
                        div { class: "export-platform-header",
                            input {
                                r#type: "checkbox",
                                id: "chk-ios",
                                checked: proj.read().export_ios,
                                onchange: {
                                    let proj_dir_save = project_dir.clone();
                                    move |e: Event<FormData>| {
                                        let mut p = proj.write();
                                        p.export_ios = e.value() == "true";
                                        save_project_state(&proj_dir_save, &p);
                                    }
                                }
                            }
                            label { r#for: "chk-ios", class: "export-platform-label export-ios-label", "iOS" }
                        }
                        if proj.read().export_ios {
                            div { class: "export-targets",
                                for (ti, &(_, tname, tw, th)) in IOS_TARGETS.iter().enumerate() {
                                    {
                                        let ti = ti;
                                        let checked = proj.read().ios_targets.get(ti).copied().unwrap_or(true);
                                        let proj_dir_save = project_dir.clone();
                                        let chk_id = format!("chk-ios-{ti}");
                                        rsx! {
                                            div { class: "export-target-row",
                                                input {
                                                    r#type: "checkbox",
                                                    id: "{chk_id}",
                                                    checked: checked,
                                                    onchange: {
                                                        let proj_dir_save = proj_dir_save.clone();
                                                        move |e: Event<FormData>| {
                                                            let mut p = proj.write();
                                                            if ti < p.ios_targets.len() {
                                                                p.ios_targets[ti] = e.value() == "true";
                                                            }
                                                            save_project_state(&proj_dir_save, &p);
                                                        }
                                                    }
                                                }
                                                label { r#for: "{chk_id}", class: "export-target-label",
                                                    span { class: "export-target-name", "{tname}" }
                                                    span { class: "export-target-dim", "{tw}×{th}" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Android targets
                    div { class: "export-section",
                        div { class: "export-platform-header",
                            input {
                                r#type: "checkbox",
                                id: "chk-android",
                                checked: proj.read().export_android,
                                onchange: {
                                    let proj_dir_save = project_dir.clone();
                                    move |e: Event<FormData>| {
                                        let mut p = proj.write();
                                        p.export_android = e.value() == "true";
                                        save_project_state(&proj_dir_save, &p);
                                    }
                                }
                            }
                            label { r#for: "chk-android", class: "export-platform-label export-android-label", "Android" }
                        }
                        if proj.read().export_android {
                            div { class: "export-targets",
                                for (ti, &(_, tname, tw, th)) in ANDROID_TARGETS.iter().enumerate() {
                                    {
                                        let ti = ti;
                                        let checked = proj.read().android_targets.get(ti).copied().unwrap_or(true);
                                        let proj_dir_save = project_dir.clone();
                                        let chk_id = format!("chk-android-{ti}");
                                        rsx! {
                                            div { class: "export-target-row",
                                                input {
                                                    r#type: "checkbox",
                                                    id: "{chk_id}",
                                                    checked: checked,
                                                    onchange: {
                                                        let proj_dir_save = proj_dir_save.clone();
                                                        move |e: Event<FormData>| {
                                                            let mut p = proj.write();
                                                            if ti < p.android_targets.len() {
                                                                p.android_targets[ti] = e.value() == "true";
                                                            }
                                                            save_project_state(&proj_dir_save, &p);
                                                        }
                                                    }
                                                }
                                                label { r#for: "{chk_id}", class: "export-target-label",
                                                    span { class: "export-target-name", "{tname}" }
                                                    span { class: "export-target-dim", "{tw}×{th}" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // Colors
                    div { class: "export-section",
                        p { class: "export-section-label", "Colors" }
                        div { class: "color-inputs",
                            div { class: "color-field",
                                label { "Primary" }
                                div { class: "color-picker-row",
                                    input {
                                        r#type: "color",
                                        value: "{proj.read().primary_color}",
                                        oninput: {
                                            let proj_dir_save = project_dir.clone();
                                            move |e: Event<FormData>| {
                                                let mut p = proj.write();
                                                p.primary_color = e.value();
                                                save_project_state(&proj_dir_save, &p);
                                            }
                                        },
                                    }
                                    span { "{proj.read().primary_color}" }
                                }
                            }
                            div { class: "color-field",
                                label { "Secondary" }
                                div { class: "color-picker-row",
                                    input {
                                        r#type: "color",
                                        value: "{proj.read().secondary_color}",
                                        oninput: {
                                            let proj_dir_save = project_dir.clone();
                                            move |e: Event<FormData>| {
                                                let mut p = proj.write();
                                                p.secondary_color = e.value();
                                                save_project_state(&proj_dir_save, &p);
                                            }
                                        },
                                    }
                                    span { "{proj.read().secondary_color}" }
                                }
                            }
                        }
                    }
                }
            }

            // ---- 4. Source Screenshots ----
            // ---- 4. Source Screenshots (language-tabbed) ----
            div { class: "card card-screenshots",
                // ── Tab bar ──────────────────────────────────────────────────
                div { class: "lang-tab-bar",
                    // One tab per selected locale
                    for loc in proj.read().locales.clone().iter() {
                        {
                            let loc = loc.clone();
                            let display_name = LOCALES.iter()
                                .find(|(c, _)| *c == loc.as_str())
                                .map(|(_, n)| *n)
                                .unwrap_or(loc.as_str())
                                .to_string();
                            let is_active = *active_locale_tab.read() == loc;
                            let is_default = loc == "en-US";
                            let proj_dir_save = project_dir.clone();
                            rsx! {
                                div {
                                    class: if is_active { "lang-tab lang-tab-active" } else { "lang-tab" },
                                    // Click anywhere on tab body to activate
                                    onclick: {
                                        let loc2 = loc.clone();
                                        move |_| {
                                            show_lang_picker.set(false);
                                            active_locale_tab.set(loc2.clone());
                                        }
                                    },
                                    span { class: "lang-tab-name", "{display_name}" }
                                    // × to remove non-default tabs
                                    if !is_default {
                                        button {
                                            class: "lang-tab-remove",
                                            title: "Remove language",
                                            onclick: {
                                                let loc3 = loc.clone();
                                                move |e: MouseEvent| {
                                                    e.stop_propagation();
                                                    let mut p = proj.write();
                                                    p.locales.retain(|l| l != &loc3);
                                                    if let Some(first) = p.locales.first() {
                                                        p.locale = first.clone();
                                                    }
                                                    // Sync active tab
                                                    if !p.locales.contains(&active_locale_tab.read().clone()) {
                                                        if let Some(first) = p.locales.first() {
                                                            active_locale_tab.set(first.clone());
                                                        }
                                                    }
                                                    save_project_state(&proj_dir_save, &p);
                                                }
                                            },
                                            "×"
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
                            onclick: move |_| show_lang_picker.toggle(),
                            "+"
                        }
                        if *show_lang_picker.read() {
                            // Transparent full-screen backdrop — closes the picker on outside click
                            div {
                                class: "lang-picker-backdrop",
                                onclick: move |_| show_lang_picker.set(false),
                            }
                            div { class: "lang-picker-dropdown",
                                for (code, name) in LOCALES.iter() {
                                    {
                                        let code = *code;
                                        let name = *name;
                                        let already_added = proj.read().locales.contains(&code.to_string());
                                        let proj_dir_save = project_dir.clone();
                                        if already_added {
                                            rsx! { }
                                        } else {
                                            rsx! {
                                                button {
                                                    class: "lang-picker-item",
                                                    onclick: move |_| {
                                                        let mut p = proj.write();
                                                        if !p.locales.contains(&code.to_string()) {
                                                            p.locales.push(code.to_string());
                                                            p.locale_sources.entry(code.to_string()).or_insert_with(Vec::new);
                                                            p.ensure_texts_len(code, 0);
                                                            if let Some(first) = p.locales.first() {
                                                                p.locale = first.clone();
                                                            }
                                                            save_project_state(&proj_dir_save, &p);
                                                        }
                                                        active_locale_tab.set(code.to_string());
                                                        show_lang_picker.set(false);
                                                    },
                                                    span { class: "lang-picker-name", "{name}" }
                                                    span { class: "lang-picker-code", "{code}" }
                                                }
                                            }
                                        }
                                    }
                                }
                                // All languages already added
                                if proj.read().locales.len() == LOCALES.len() {
                                    p { class: "lang-picker-empty", "All languages added" }
                                }
                            }
                        }
                    }

                    // ── Right side: image picker button ──────────────────────
                    div { class: "lang-tab-bar-right",
                        div { class: "lang-tab-bar-right-inner",
                            div { class: "lang-tab-images-row",
                                button { class: "btn btn-sm", onclick: pick_files, "Choose Images…" }
                                {
                                    let n = proj.read().sources_for(&active_locale_tab.read()).len();
                                    if n > 0 {
                                        rsx! { span { class: "source-count", "{n} image(s)" } }
                                    } else {
                                        rsx! {}
                                    }
                                }
                            }
                            p { class: "lang-tab-images-hint",
                                "Images added here are specific to this language tab."
                            }
                        }
                    }
                }

                // ── Tab content: source image list for active locale ─────────
                {
                    let active_sources = proj.read().sources_for(&active_locale_tab.read());
                    if active_sources.is_empty() {
                        rsx! {
                            p { class: "source-empty",
                                "Add source screenshots for this language tab to get started."
                            }
                        }
                    } else {
                        rsx! {
                    div { class: "source-list",
                        for (i, path) in active_sources.iter().enumerate() {
                            {
                                let idx = i;
                                let path = path.clone();
                                let path_str = path.to_string_lossy().to_string();
                                let encoded_path = urlencoding::encode(&path_str).into_owned();
                                let thumb_src = format!("/localimg/{encoded_path}");
                                let proj_dir_save = proj_dir2.clone();
                                let cur_locale = active_locale_tab.read().clone();

                                rsx! {
                                    div { class: "source-item",
                                        img { class: "source-thumb", src: "{thumb_src}" }
                                        div { class: "source-item-right",
                                            div { class: "source-info",
                                                span { class: "source-index", "{idx + 1}." }
                                                span { class: "source-path", title: "{path_str}", "{path_str}" }
                                            }
                                            div { class: "manual-inputs",
                                                // Title input — always per-locale via locale_texts
                                                {
                                                    let proj_dir_save2 = proj_dir_save.clone();
                                                    let loc2 = cur_locale.clone();
                                                    let title_val = proj.read().locale_texts
                                                        .get(&cur_locale)
                                                        .and_then(|v| v.get(idx))
                                                        .map(|t| t.0.clone())
                                                        .unwrap_or_default();
                                                    rsx! {
                                                        input {
                                                            class: "text-input small-input",
                                                            placeholder: "Title (e.g. Welcome)",
                                                            value: "{title_val}",
                                                            oninput: move |e: Event<FormData>| {
                                                                let mut p = proj.write();
                                                                p.ensure_texts_len(&loc2, idx + 1);
                                                                p.locale_texts.get_mut(&loc2).unwrap()[idx].0 = e.value();
                                                                // Keep legacy manual_texts in sync for the first locale
                                                                if p.locales.first().map(|l| l.as_str()) == Some(loc2.as_str()) {
                                                                    if idx < p.manual_texts.len() { p.manual_texts[idx].0 = e.value(); }
                                                                }
                                                                save_project_state(&proj_dir_save2, &p);
                                                            }
                                                        }
                                                    }
                                                }
                                                // Subtitle input — always per-locale via locale_texts
                                                {
                                                    let proj_dir_save3 = proj_dir_save.clone();
                                                    let loc3 = cur_locale.clone();
                                                    let sub_val = proj.read().locale_texts
                                                        .get(&cur_locale)
                                                        .and_then(|v| v.get(idx))
                                                        .map(|t| t.1.clone())
                                                        .unwrap_or_default();
                                                    rsx! {
                                                        input {
                                                            class: "text-input small-input",
                                                            placeholder: "Short description…",
                                                            value: "{sub_val}",
                                                            oninput: move |e: Event<FormData>| {
                                                                let mut p = proj.write();
                                                                p.ensure_texts_len(&loc3, idx + 1);
                                                                p.locale_texts.get_mut(&loc3).unwrap()[idx].1 = e.value();
                                                                if p.locales.first().map(|l| l.as_str()) == Some(loc3.as_str()) {
                                                                    if idx < p.manual_texts.len() { p.manual_texts[idx].1 = e.value(); }
                                                                }
                                                                save_project_state(&proj_dir_save3, &p);
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                            button {
                                                class: "btn-remove",
                                                title: "Remove image",
                                                onclick: {
                                                    let proj_dir_save = proj_dir_save.clone();
                                                    move |_| {
                                                        let mut p = proj.write();
                                                        let cur_loc = active_locale_tab.read().clone();
                                                        if let Some(srcs) = p.locale_sources.get_mut(&cur_loc) {
                                                            if idx < srcs.len() { srcs.remove(idx); }
                                                        }
                                                        if idx < p.manual_texts.len() { p.manual_texts.remove(idx); }
                                                        if let Some(v) = p.locale_texts.get_mut(&cur_loc) {
                                                            if idx < v.len() { v.remove(idx); }
                                                        }
                                                        save_project_state(&proj_dir_save, &p);
                                                    }
                                                },
                                                "✕"
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
            }

            // ---- 5. AI Theme config ----
            div { class: "card",
                h2 { "5. AI Theme" }
                p { class: "hint card-hint", "Describe the visual style for the AI-generated background." }
                textarea {
                    class: "text-input theme-textarea",
                    placeholder: "e.g. dark cyberpunk neon interface with glitch effects…",
                    value: "{proj.read().theme_prompt}",
                    rows: "3",
                    oninput: {
                        let proj_dir_save = project_dir.clone();
                        move |e: Event<FormData>| {
                            let mut p = proj.write();
                            p.theme_prompt = e.value();
                            save_project_state(&proj_dir_save, &p);
                        }
                    },
                }
                if !proj.read().theme_history.is_empty() {
                    div { class: "theme-history",
                        p { class: "theme-history-label", "Recent:" }
                        div { class: "theme-chips",
                            for entry in proj.read().theme_history.clone().iter() {
                                {
                                    let entry_clone = entry.clone();
                                    let display = if entry.len() > 60 { format!("{}…", &entry[..57]) } else { entry.clone() };
                                    let proj_dir_save = project_dir.clone();
                                    rsx! {
                                        button {
                                            class: "theme-chip",
                                            title: "{entry_clone}",
                                            onclick: move |_| {
                                                let mut p = proj.write();
                                                p.theme_prompt = entry_clone.clone();
                                                save_project_state(&proj_dir_save, &p);
                                            },
                                            "{display}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ---- Generate buttons ----
            div { class: "generate-row",
                button {
                    class: "btn btn-primary btn-generate",
                    disabled: is_busy,
                    onclick: move |_| on_generate_ai(()),
                    if matches!(*phase.read(), AppPhase::GeneratingAi | AppPhase::Resizing) {
                        span { class: "spinner" }
                        " Generating AI…"
                    } else {
                        "✦ Generate AI Screenshots"
                    }
                }
                button {
                    class: "btn btn-generate btn-manual",
                    disabled: is_busy,
                    onclick: move |_| on_generate_manual(()),
                    if matches!(*phase.read(), AppPhase::GeneratingManual) {
                        span { class: "spinner spinner-dark" }
                        " Generating…"
                    } else {
                        "⬛ Generate Manual Screenshots"
                    }
                }
            }

            // ---- Error banner ----
            if let AppPhase::Error(ref msg) = *phase.read() {
                div { class: "card error-card",
                    p { "⚠ {msg}" }
                    button {
                        class: "btn btn-dismiss",
                        onclick: move |_| phase.set(AppPhase::Idle),
                        "Dismiss"
                    }
                }
            }

            // ---- Bottom output panel ----
            div { class: "card output-panel",
                // Tabs
                div { class: "output-tabs",
                    button {
                        class: if *output_tab.read() == OutputTab::Progress { "output-tab active" } else { "output-tab" },
                        onclick: move |_| output_tab.set(OutputTab::Progress),
                        "Progress"
                        if !log_lines.read().is_empty() {
                            span { class: "tab-badge", "{log_lines.read().len()}" }
                        }
                    }
                    button {
                        class: if *output_tab.read() == OutputTab::GeneratedImages { "output-tab active" } else { "output-tab" },
                        onclick: move |_| output_tab.set(OutputTab::GeneratedImages),
                        "Generated Images"
                        if !proj.read().generated_urls.is_empty() {
                            span { class: "tab-badge", "{proj.read().generated_urls.len()}" }
                        }
                    }
                    button {
                        class: if *output_tab.read() == OutputTab::SavedScreenshots { "output-tab active" } else { "output-tab" },
                        onclick: move |_| output_tab.set(OutputTab::SavedScreenshots),
                        "Saved Screenshots"
                        if !proj.read().output_paths.is_empty() {
                            span { class: "tab-badge", "{proj.read().output_paths.len()}" }
                        }
                    }
                    button {
                        class: if *output_tab.read() == OutputTab::Publish { "output-tab active output-tab-publish" } else { "output-tab output-tab-publish" },
                        onclick: move |_| output_tab.set(OutputTab::Publish),
                        "🚀 Publish"
                    }
                    button {
                        class: if *output_tab.read() == OutputTab::Build { "output-tab active output-tab-build" } else { "output-tab output-tab-build" },
                        onclick: move |_| output_tab.set(OutputTab::Build),
                        "🔨 Build"
                        if matches!(*build_phase.read(), BuildPhase::Running(_)) {
                            span { class: "spinner spinner-dark" }
                        }
                    }
                }

                // Tab content
                div { class: "output-content",
                    match *output_tab.read() {
                        OutputTab::Progress => rsx! {
                            if log_lines.read().is_empty() {
                                p { class: "output-empty", "Progress will appear here when you generate screenshots." }
                            } else {
                                div { class: "log-scroll",
                                    for line in log_lines.read().iter() {
                                        p { class: "log-line", "{line}" }
                                    }
                                }
                            }
                        },
                        OutputTab::GeneratedImages => rsx! {
                            if proj.read().generated_urls.is_empty() {
                                p { class: "output-empty", "Generated previews will appear here." }
                            } else {
                                div { class: "preview-grid",
                                    for (i, url) in proj.read().generated_urls.clone().iter().enumerate() {
                                        {
                                            let url = url.clone();
                                            rsx! {
                                                div { class: "preview-item",
                                                    p { class: "preview-label", "Screen {i + 1}" }
                                                    img { class: "preview-img", src: "{url}" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        },
                        OutputTab::SavedScreenshots => rsx! {
                            if proj.read().output_paths.is_empty() {
                                p { class: "output-empty", "Saved file paths will appear here." }
                            } else {
                                for (label, path) in proj.read().output_paths.clone().iter() {
                                    {
                                        let label = label.clone();
                                        let path_str = path.to_string_lossy().to_string();
                                        rsx! {
                                            div { class: "output-row",
                                                span { class: "output-label", "{label}" }
                                                span { class: "output-path", "{path_str}" }
                                            }
                                        }
                                    }
                                }
                            }
                        },
                        OutputTab::Publish => rsx! {
                            div { class: "publish-panel",
                                // Status / info
                                div { class: "publish-info",
                                    p { class: "publish-desc",
                                        "Upload screenshots to App Store Connect via "
                                        strong { "fastlane upload_screenshots" }
                                        ". Make sure you have generated screenshots first and set "
                                        code { "APP_STORE_CONNECT_API_KEY_*" }
                                        " environment variables."
                                    }
                                    div { class: "publish-status-row",
                                        match *publish_phase.read() {
                                            PublishPhase::Idle => rsx! {
                                                span { class: "publish-status publish-idle", "Ready" }
                                            },
                                            PublishPhase::Running => rsx! {
                                                span { class: "spinner" }
                                                span { class: "publish-status publish-running", " Uploading…" }
                                            },
                                            PublishPhase::Success => rsx! {
                                                span { class: "publish-status publish-success", "✓ Uploaded successfully" }
                                            },
                                            PublishPhase::Error(ref msg) => rsx! {
                                                span { class: "publish-status publish-error", "✕ {msg}" }
                                            },
                                        }
                                    }
                                }

                                // Publish button
                                button {
                                    class: "btn btn-publish",
                                    disabled: matches!(*publish_phase.read(), PublishPhase::Running),
                                    onclick: move |_| on_publish(()),
                                    if matches!(*publish_phase.read(), PublishPhase::Running) {
                                        span { class: "spinner" }
                                        " Uploading to App Store Connect…"
                                    } else {
                                        "🚀 Upload to App Store Connect"
                                    }
                                }

                                // Output log
                                if !publish_log.read().is_empty() {
                                    div { class: "publish-log",
                                        p { class: "publish-log-label", "fastlane output:" }
                                        div { class: "log-scroll publish-log-scroll",
                                            for line in publish_log.read().iter() {
                                                {
                                                    let line = line.clone();
                                                    // Color error/warning lines differently
                                                    let cls = if line.to_lowercase().contains("error") || line.starts_with("✗") {
                                                        "log-line log-error"
                                                    } else if line.to_lowercase().contains("warning") || line.starts_with("⚠") {
                                                        "log-line log-warn"
                                                    } else {
                                                        "log-line"
                                                    };
                                                    rsx! { p { class: "{cls}", "{line}" } }
                                                }
                                            }
                                        }
                                    }
                                }

                                // ── Android / Google Play section ──────────────
                                hr { class: "publish-divider" }
                                div { class: "publish-info",
                                    p { class: "publish-desc",
                                        "Upload Android screenshots to Google Play via "
                                        strong { "androidpublisher v3" }
                                        ". Set "
                                        code { "GOOGLE_PLAY_JSON_KEY" }
                                        " (path to service-account JSON) in "
                                        code { "fastlane/.env" }
                                        " or the environment."
                                    }
                                    div { class: "publish-status-row",
                                        match *android_publish_phase.read() {
                                            AndroidPublishPhase::Idle => rsx! {
                                                span { class: "publish-status publish-idle", "Ready" }
                                            },
                                            AndroidPublishPhase::Running => rsx! {
                                                span { class: "spinner" }
                                                span { class: "publish-status publish-running", " Uploading…" }
                                            },
                                            AndroidPublishPhase::Success => rsx! {
                                                span { class: "publish-status publish-success", "✓ Uploaded successfully" }
                                            },
                                            AndroidPublishPhase::Error(ref msg) => rsx! {
                                                span { class: "publish-status publish-error", "✕ {msg}" }
                                            },
                                        }
                                    }
                                }
                                button {
                                    class: "btn btn-publish btn-publish-android",
                                    disabled: matches!(*android_publish_phase.read(), AndroidPublishPhase::Running),
                                    onclick: move |_| on_android_publish(()),
                                    if matches!(*android_publish_phase.read(), AndroidPublishPhase::Running) {
                                        span { class: "spinner" }
                                        " Uploading to Google Play…"
                                    } else {
                                        "🤖 Upload to Google Play"
                                    }
                                }
                                if !android_publish_log.read().is_empty() {
                                    div { class: "publish-log",
                                        p { class: "publish-log-label", "androidpublisher output:" }
                                        div { class: "log-scroll publish-log-scroll",
                                            for line in android_publish_log.read().iter() {
                                                {
                                                    let line = line.clone();
                                                    let cls = if line.to_lowercase().contains("error") || line.starts_with("✗") {
                                                        "log-line log-error"
                                                    } else if line.starts_with("✅") || line.starts_with("🎉") {
                                                        "log-line log-success"
                                                    } else {
                                                        "log-line"
                                                    };
                                                    rsx! { p { class: "{cls}", "{line}" } }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        },
                        OutputTab::Build => rsx! {
                            div { class: "build-panel",
                                // Status row
                                div { class: "build-status-row",
                                    match build_phase.read().clone() {
                                        BuildPhase::Idle => rsx! {
                                            span { class: "build-status build-idle", "Ready — scripts will be created in project folder if missing." }
                                        },
                                        BuildPhase::Running(ref name) => rsx! {
                                            span { class: "spinner" }
                                            span { class: "build-status build-running", " Running {name}…" }
                                        },
                                        BuildPhase::Success(ref name) => rsx! {
                                            span { class: "build-status build-success", "✓ {name} completed successfully" }
                                        },
                                        BuildPhase::Error(ref msg) => rsx! {
                                            span { class: "build-status build-error", "✕ {msg}" }
                                        },
                                    }
                                }

                                // Build buttons
                                div { class: "build-buttons",
                                    // iOS
                                    div { class: "build-platform-group",
                                        p { class: "build-platform-title build-ios-title", "iOS" }
                                        button {
                                            class: "btn btn-build btn-build-ios",
                                            disabled: matches!(*build_phase.read(), BuildPhase::Running(_)),
                                            onclick: {
                                                let mut on_run = on_run_script.clone();
                                                move |_| on_run("build_ios_distribution.sh".into())
                                            },
                                            if matches!(*build_phase.read(), BuildPhase::Running(ref n) if n == "build_ios_distribution.sh") {
                                                span { class: "spinner" }
                                                " Building IPA…"
                                            } else {
                                                "📱 Build iOS IPA"
                                            }
                                        }
                                    }

                                    // Android
                                    div { class: "build-platform-group",
                                        p { class: "build-platform-title build-android-title", "Android" }
                                        button {
                                            class: "btn btn-build btn-build-android",
                                            disabled: matches!(*build_phase.read(), BuildPhase::Running(_)),
                                            onclick: {
                                                let mut on_run = on_run_script.clone();
                                                move |_| on_run("build_android_release.sh".into())
                                            },
                                            if matches!(*build_phase.read(), BuildPhase::Running(ref n) if n == "build_android_release.sh") {
                                                span { class: "spinner" }
                                                " Building AAB…"
                                            } else {
                                                "📦 Build AAB (Google Play)"
                                            }
                                        }
                                        button {
                                            class: "btn btn-build btn-build-android-secondary",
                                            disabled: matches!(*build_phase.read(), BuildPhase::Running(_)),
                                            onclick: {
                                                let mut on_run = on_run_script.clone();
                                                move |_| on_run("build_android.sh".into())
                                            },
                                            if matches!(*build_phase.read(), BuildPhase::Running(ref n) if n == "build_android.sh") {
                                                span { class: "spinner spinner-dark" }
                                                " Building…"
                                            } else {
                                                "🔧 Build APK (Release)"
                                            }
                                        }
                                        button {
                                            class: "btn btn-build btn-build-android-secondary",
                                            disabled: matches!(*build_phase.read(), BuildPhase::Running(_)),
                                            onclick: {
                                                let mut on_run = on_run_script.clone();
                                                move |_| on_run("build_apk.sh".into())
                                            },
                                            if matches!(*build_phase.read(), BuildPhase::Running(ref n) if n == "build_apk.sh") {
                                                span { class: "spinner spinner-dark" }
                                                " Building…"
                                            } else {
                                                "🐛 Build APK (Debug/Test)"
                                            }
                                        }
                                    }
                                }

                                // Output log
                                if !build_log.read().is_empty() {
                                    div { class: "publish-log",
                                        p { class: "publish-log-label", "Output:" }
                                        div { class: "log-scroll publish-log-scroll build-log-scroll",
                                            for line in build_log.read().iter() {
                                                {
                                                    let line = line.clone();
                                                    let cls = if line.to_lowercase().contains("error") || line.starts_with("❌") {
                                                        "log-line log-error"
                                                    } else if line.to_lowercase().contains("warning") || line.starts_with("⚠") {
                                                        "log-line log-warn"
                                                    } else if line.starts_with("✅") || line.starts_with("🎉") {
                                                        "log-line log-success"
                                                    } else {
                                                        "log-line"
                                                    };
                                                    rsx! { p { class: "{cls}", "{line}" } }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        },
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
    let apple_identity_val = settings.read().apple_identity.clone();
    let provisioning_profile_val = settings.read().provisioning_profile.clone();
    let ios_short_version_val = settings.read().ios_short_version.clone();

    // Discover signing identities from the system keychain on mount.
    let mut identities = use_signal(|| Vec::<String>::new());
    use_effect(move || {
        spawn(async move {
            let found = tokio::task::spawn_blocking(|| {
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
            }).await.unwrap_or_default();
            identities.set(found);
        });
    });

    // Discover provisioning profiles from the system directory on mount.
    // Each entry is (display_label, full_path).
    let mut profiles = use_signal(|| Vec::<(String, String)>::new());
    use_effect(move || {
        spawn(async move {
            let found = tokio::task::spawn_blocking(|| {
                let dir = dirs::home_dir()
                    .unwrap_or_default()
                    .join("Library/MobileDevice/Provisioning Profiles");
                let Ok(entries) = std::fs::read_dir(&dir) else { return vec![]; };
                let mut list: Vec<(String, String)> = entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("mobileprovision"))
                    .filter_map(|e| {
                        let path = e.path();
                        // Extract Name from the plist embedded inside the binary profile.
                        // The profile is a CMS blob; the XML plist is readable as plain text.
                        let bytes = std::fs::read(&path).ok()?;
                        let text = String::from_utf8_lossy(&bytes);
                        // Find <key>Name</key>\n\t<string>VALUE</string>
                        let name = text.find("<key>Name</key>")
                            .and_then(|i| {
                                let after = &text[i + "<key>Name</key>".len()..];
                                let start = after.find("<string>")? + "<string>".len();
                                let end = after[start..].find("</string>")?;
                                Some(after[start..start + end].trim().to_string())
                            })
                            .unwrap_or_else(|| {
                                path.file_stem()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .to_string()
                            });
                        Some((name, path.to_string_lossy().to_string()))
                    })
                    .collect();
                // Sort: Distribution profiles first, then alphabetically
                list.sort_by(|a, b| {
                    let a_dist = a.0.to_lowercase().contains("distribution") || a.0.to_lowercase().contains("appstore");
                    let b_dist = b.0.to_lowercase().contains("distribution") || b.0.to_lowercase().contains("appstore");
                    b_dist.cmp(&a_dist).then(a.0.cmp(&b.0))
                });
                list
            }).await.unwrap_or_default();
            profiles.set(found);
        });
    });

    rsx! {
        div { class: "settings-backdrop", onclick: move |_| on_close.call(()) }
        div { class: "settings-popup settings-popup-wide",
            div { class: "settings-header",
                h2 { "Settings" }
                button { class: "btn btn-icon", onclick: move |_| on_close.call(()), "✕" }
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

            // ---- iOS Signing (shared across all projects) ----
            p { class: "settings-section-title", "iOS Signing (all projects)" }

            div { class: "settings-field",
                label { "Apple Distribution Identity" }
                if identities.read().is_empty() {
                    // Keychain not yet queried or no identities found — fall back to text input
                    input {
                        class: "text-input",
                        placeholder: "Apple Distribution: Name (TEAMID)",
                        value: "{apple_identity_val}",
                        oninput: move |e: Event<FormData>| { settings.write().apple_identity = e.value(); save_settings(&settings()); },
                    }
                    p { class: "settings-hint", "No signing identities found in keychain." }
                } else {
                    select {
                        class: "text-input",
                        value: "{apple_identity_val}",
                        onchange: move |e: Event<FormData>| {
                            settings.write().apple_identity = e.value();
                            save_settings(&settings());
                        },
                        // Blank sentinel so the dropdown shows "choose…" when nothing is saved yet
                        if apple_identity_val.is_empty() {
                            option { value: "", disabled: true, selected: true, "— choose identity —" }
                        }
                        for id in identities.read().iter() {
                            {
                                let id = id.clone();
                                let selected = id == apple_identity_val;
                                rsx! {
                                    option { value: "{id}", selected: selected, "{id}" }
                                }
                            }
                        }
                    }
                    p { class: "settings-hint",
                        "Loaded from keychain · Distribution identities listed first"
                    }
                }
            }

            div { class: "settings-field",
                label { "Provisioning Profile" }
                if profiles.read().is_empty() {
                    // No profiles discovered — fall back to manual path input + Browse
                    div { class: "settings-path-row",
                        input {
                            class: "text-input",
                            placeholder: "/Users/you/Downloads/YourApp.mobileprovision",
                            value: "{provisioning_profile_val}",
                            oninput: move |e: Event<FormData>| {
                                settings.write().provisioning_profile = e.value();
                                save_settings(&settings());
                            },
                        }
                        button {
                            class: "btn settings-browse-btn",
                            onclick: move |_| {
                                spawn(async move {
                                    if let Some(f) = rfd::AsyncFileDialog::new()
                                        .set_title("Select Provisioning Profile")
                                        .add_filter("Provisioning Profile", &["mobileprovision"])
                                        .pick_file().await
                                    {
                                        settings.write().provisioning_profile = f.path().to_string_lossy().to_string();
                                        save_settings(&settings());
                                    }
                                });
                            },
                            "Browse…"
                        }
                    }
                    p { class: "settings-hint", "No profiles found in ~/Library/MobileDevice/Provisioning Profiles" }
                } else {
                    select {
                        class: "text-input",
                        value: "{provisioning_profile_val}",
                        onchange: move |e: Event<FormData>| {
                            settings.write().provisioning_profile = e.value();
                            save_settings(&settings());
                        },
                        if provisioning_profile_val.is_empty() {
                            option { value: "", disabled: true, selected: true, "— choose profile —" }
                        }
                        for (label, path) in profiles.read().iter() {
                            {
                                let path = path.clone();
                                let label = label.clone();
                                let selected = path == provisioning_profile_val;
                                rsx! {
                                    option { value: "{path}", selected: selected, "{label}" }
                                }
                            }
                        }
                    }
                    p { class: "settings-hint",
                        "Loaded from ~/Library/MobileDevice/Provisioning Profiles · Distribution profiles listed first"
                    }
                }
            }

            div { class: "settings-field",
                label { "iOS App Version" }
                input {
                    class: "text-input settings-short-input",
                    placeholder: "1.0",
                    value: "{ios_short_version_val}",
                    oninput: move |e: Event<FormData>| { settings.write().ios_short_version = e.value(); save_settings(&settings()); },
                }
                p { class: "settings-hint", "CFBundleShortVersionString, e.g. 1.0" }
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
fn resize_to_targets(
    image_bytes: &[u8],
    screen_index: usize,
    total_screens: usize,
    locale: &str,
    fastlane_path: &str,
    ios_enabled: &[bool],
    android_enabled: &[bool],
) -> Result<Vec<(String, PathBuf)>, String> {
    let img = image::load_from_memory(image_bytes)
        .map_err(|e| format!("Failed to decode image: {e}"))?
        .to_rgba8();

    let android_dir = PathBuf::from("output");
    std::fs::create_dir_all(&android_dir)
        .map_err(|e| format!("Failed to create output dir: {e}"))?;

    let mut results = Vec::new();

    for (ti, &(key, device_name, tw, th)) in IOS_TARGETS.iter().enumerate() {
        if !ios_enabled.get(ti).copied().unwrap_or(true) { continue; }
        let resized = fill_and_crop(&img, tw, th);
        let path = if !fastlane_path.is_empty() {
            let locale_dir = PathBuf::from(fastlane_path).join(locale);
            std::fs::create_dir_all(&locale_dir)
                .map_err(|e| format!("Failed to create locale dir: {e}"))?;
            locale_dir.join(format!("{device_name}-{screen_index:02}.png"))
        } else {
            let name = if total_screens == 1 {
                format!("{key}.png")
            } else {
                format!("screen_{screen_index}_{key}.png")
            };
            android_dir.join(name)
        };
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

    for (ti, &(key, label, tw, th)) in ANDROID_TARGETS.iter().enumerate() {
        if !android_enabled.get(ti).copied().unwrap_or(true) { continue; }
        let resized = fill_and_crop(&img, tw, th);
        let name = if total_screens == 1 {
            format!("{key}.png")
        } else {
            format!("screen_{screen_index}_{key}.png")
        };
        let path = android_dir.join(&name);
        resized
            .save(&path)
            .map_err(|e| format!("Failed to save {name}: {e}"))?;
        let full_label = if total_screens == 1 {
            label.to_string()
        } else {
            format!("Screen {screen_index} → {label}")
        };
        results.push((full_label, path));
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

fn draw_phone_frame(img: &mut RgbaImage, x: u32, y: u32, w: u32, h: u32) {
    let rect = Rect::at(x as i32, y as i32).of_size(w, h);
    draw_filled_rect_mut(img, rect, Rgba([50, 50, 50, 255]));
}

/// Draw a clean iOS-style status bar over the top of the placed screenshot
/// to replace any Android status bar that may be visible in the source image.
/// `screen_x/y` is the top-left of the screen content area (inside the bezel),
/// `screen_w` is the width of that area.
fn draw_ios_status_bar(img: &mut RgbaImage, font: &FontRef, screen_x: u32, screen_y: u32, screen_w: u32) {
    // Status bar height: ~4.5% of screen width (matches iOS proportions)
    let bar_h = (screen_w as f64 * 0.075).round() as u32;

    // Sample background color from the center of the status bar area in the composited image
    // (a few px down to avoid bezel edge) and use it as the bar background.
    let sample_y = screen_y + bar_h / 2;
    let sample_x = screen_x + screen_w / 2;
    let bg = if sample_x < img.width() && sample_y < img.height() {
        *img.get_pixel(sample_x, sample_y)
    } else {
        Rgba([255, 255, 255, 255])
    };

    // Paint a solid bar with the sampled background color.
    let bar_rect = Rect::at(screen_x as i32, screen_y as i32).of_size(screen_w, bar_h);
    draw_filled_rect_mut(img, bar_rect, bg);

    // Choose text/icon color based on luminance.
    let fg = get_contrast_color(bg);

    // Time text on the left: "9:41" (classic Apple status bar time)
    let text_scale = PxScale::from(bar_h as f32 * 0.55);
    let time_str = "9:41";
    let padding = (screen_w as f64 * 0.04).round() as i32;
    let text_y = screen_y as i32 + ((bar_h as i32 - (bar_h as f32 * 0.55) as i32) / 2);
    draw_text_mut(img, fg, screen_x as i32 + padding, text_y, text_scale, font, time_str);

    // Draw simple battery icon on the right
    let icon_h = (bar_h as f64 * 0.45).round() as u32;
    let icon_w = (icon_h as f64 * 1.8).round() as u32;
    let icon_y = screen_y + (bar_h - icon_h) / 2;
    let icon_x = screen_x + screen_w - (screen_w as f64 * 0.04).round() as u32 - icon_w;

    // Battery outline
    let border = (icon_h as f64 * 0.12).round().max(1.0) as u32;
    let batt_rect = Rect::at(icon_x as i32, icon_y as i32).of_size(icon_w, icon_h);
    draw_filled_rect_mut(img, batt_rect, fg);
    // Battery fill (inner, slightly inset with background color)
    let inner_rect = Rect::at(
        (icon_x + border) as i32,
        (icon_y + border) as i32,
    )
    .of_size(icon_w - border * 2, icon_h - border * 2);
    draw_filled_rect_mut(img, inner_rect, bg);
    // Battery charge fill (75%)
    let charge_w = ((icon_w - border * 2) as f64 * 0.75).round() as u32;
    let charge_rect = Rect::at(
        (icon_x + border) as i32,
        (icon_y + border) as i32,
    )
    .of_size(charge_w, icon_h - border * 2);
    draw_filled_rect_mut(img, charge_rect, fg);

    // Draw signal dots to the left of the battery
    let dot_r = (bar_h as f64 * 0.09).round().max(2.0) as u32;
    let dot_gap = dot_r;
    let dots_total_w = 4 * dot_r * 2 + 3 * dot_gap;
    let dots_x = icon_x.saturating_sub(dots_total_w + (screen_w as f64 * 0.025).round() as u32);
    let dots_y = screen_y + bar_h / 2;
    for i in 0u32..4 {
        let cx = dots_x + i * (dot_r * 2 + dot_gap) + dot_r;
        // Draw a filled square approximating a dot
        let sq_rect = Rect::at((cx - dot_r) as i32, (dots_y - dot_r) as i32).of_size(dot_r * 2, dot_r * 2);
        draw_filled_rect_mut(img, sq_rect, fg);
    }
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
