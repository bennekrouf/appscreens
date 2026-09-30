# Changelog

What changed in each release of **AppScreens**, the desktop app for making
app-store screenshots, publishing them and building your app.

The public version of this page — with the download for each release — lives at
<https://mayorana.ch/en/apps/appscreens/releases>. It is generated from this
file by `scripts/changelog_to_json.py`, so this file is the only place a
release note is written.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Each heading is dated on the day its tag was pushed. Releases that carried only
build or packaging work say so rather than being hidden: the version numbers a
user sees in the update prompt should all be accounted for.

## [0.1.20] - 2026-09-30

### Added

- Setting up the App Store Connect API key no longer means editing `.env` by
  hand. If another of your projects already has a key, one click reuses it —
  one key works for every app of your team. Otherwise the Apple card walks you
  through creating one, with a link straight to the right App Store Connect
  page. It then picks up the downloaded `.p8`, reads the Key ID from its file
  name, keeps a private copy in your keys folder (Apple lets you download it
  only once), saves everything to the project's `.env` and tests the
  connection.
- "Add profile…" next to the provisioning profile list installs a profile you
  downloaded from Apple Developer and selects it, with a link to your
  profiles there.

### Fixed

- Provisioning profiles downloaded by Xcode 16 now appear in the profile list;
  only the older profiles folder used to be read.

## [0.1.19] - 2026-09-29

### Added

- Opening a project you've already built fills in the App step for you: the
  app name, project slug, bundle ID and Android package come from your build
  scripts, `Dioxus.toml`, `Cargo.toml`, fastlane and your last builds, and a
  project that was only ever built for iOS (or Android) is set up for just
  that platform. Fields you've filled in yourself are never changed.

### Fixed

- AppScreens no longer replaces a build script you wrote yourself. It used to
  rewrite every build script from its template before each build, losing
  changes like a display name or a minimum iOS version. Scripts it wrote are
  still kept up to date.

## [0.1.18] - 2026-09-29

### Added

- The Accounts step can now set up signing without a terminal: create an
  Android upload keystore (or add a key for another app to it) and get the
  certificate Play Console asks for, create an Apple Distribution certificate
  straight into your keychain, and regenerate an invalid App Store profile.
- Passwords for the upload keystore are kept in the macOS Keychain instead of
  a plain-text `.env` file (a `.env` still works and takes priority).
- The Accounts step warns when `.env` files, keys, keystores or service-account
  files are committed in the project, and can stop tracking them for you.
- An encrypted backup of your whole keys folder in one file, with a reminder
  until you have one and again whenever your keys change — so a lost or stolen
  Mac no longer means revoking and recreating every key.
- The Build step checks your build tools before you build: `dx` against the
  project's Dioxus version, the Android SDK, NDK, target platform, build tools
  and Java, the Rust targets, Xcode and your signing identity. Each problem says
  how to fix it, and several have a button that does it for you — instead of
  finding them one failed build at a time.
- Every build is inspected before you upload it: which key signed the AAB
  (compared with your upload key), its package, version and target API; and
  for the IPA its bundle ID, version, distribution signature and profile.
  Problems show under the build and in the Submit checklist.
- Releasing to Google Play stops before the upload when the bundle's
  versionCode isn't higher than one already on Play, instead of after it.
- The App step checks that your app's own code and `.env` use the same Android
  package as the app — store links, data paths and package constants — and
  can correct them in one click. A mismatch used to ship silently: saved data
  in the wrong place, a "rate us" link to a missing page, or an upload to a
  different app.
- The build check flags `Dioxus.toml` settings the installed `dx` ignores
  (`target_sdk_version`, `min_sdk_version`) or rejects (`permissions = [...]`),
  and a target API below what Google Play requires, and can rewrite them.
- The App step warns when the project's code isn't safe from a lost Mac: not
  in git, no remote, or commits that were never pushed.
- The Submit step shows what the stores have right now — version states from
  App Store Connect and every Play track's releases — with links to what only
  the consoles show.
- The Accounts step has an "If this Mac is lost or stolen" checklist: what to
  revoke and replace, in order, filled in with this project's own key IDs and
  linking straight to each console page.

- The Build step shows every Java installed on the machine — Android Studio's,
  Homebrew, SDKMAN, system installs, and on Windows the registry — marks the
  ones too old or too new for the Gradle version your project builds with,
  and lets you pick which one AppScreens uses. Your terminal and system
  settings are left alone, so there is no JAVA_HOME to set up by hand.
- When no suitable Java is installed, the Java card offers to install one that
  fits your project: Homebrew (inside AppScreens, no password) or the Temurin
  installer on macOS, your package manager in a terminal on Linux, winget on
  Windows — and picks it up as soon as it's done.
- Android builds no longer start on a Java the project's Gradle can't run on.
  If the Java you picked, or the one in `JAVA_HOME`, is too old or too new
  (Java 26 with Gradle 9.1, for example), the build uses one that fits and says
  so in the log. The Doctor and the Java card point it out, with a
  one-click "Use Java 21".
- When a build fails on a Java mismatch anyway, the Build step explains it in
  plain words and offers "Build again with Java 21", using a Java you already
  have installed.
- If you want the same Java in your own terminal, the Java card can add it to
  your shell profile (zsh, bash or fish) or your Windows user settings. You see
  the exact lines first, your profile is backed up, and Remove takes them out
  again. "Check my terminal" opens a fresh shell and shows which Java it and
  your project's Gradle really use.

### Fixed

- Signing and Android builds no longer fail with "Unable to locate a Java
  Runtime" when AppScreens is started from the Dock or a terminal without Java
  on its path. It now finds a Java installation by itself — `JAVA_HOME`,
  macOS's registered Java, Android Studio's bundled runtime or Homebrew — and
  hands it to the build.

## [0.1.17] - 2026-09-28

### Changed

- Android release builds take their signing key from the project's `.env`
  (`ANDROID_KEYSTORE_PATH`, `ANDROID_KEYSTORE_PASSWORD`, and optionally
  `ANDROID_KEY_ALIAS`) instead of a fixed keystore location, so you can use
  your own upload key and keep its password out of the build script. The
  Accounts step shows whether the keystore is found and the password is set.

### Fixed

- Android release builds stop straight away with a clear message when the
  Android NDK is not installed, when the installed `dx` and the project's
  Dioxus version do not match (with the command that fixes it), or when `dx`
  fails before creating the Android project. They used to carry on and end in
  a cascade of unrelated "No such file" errors.

## [0.1.16] - 2026-09-28

### Changed

- Packaging only — no user-visible change.

## [0.1.15] - 2026-09-27

### Changed

- The update banner now links directly to the OS-specific build, making it
  easier to download the latest version.

## [0.1.14] - 2026-09-25

### Changed

- Packaging only — no user-visible change.

## [0.1.13] - 2026-09-24

### Fixed

- On wide windows the step content now fills the space beside the sidebar
  instead of sitting in a narrow centred column with an empty band next to it.

## [0.1.12] - 2026-09-24

### Changed

- Each project now opens in a workspace with one screen per step, in the order
  the work happens: App, Accounts, Version, Build, Languages, Screenshots,
  Store text and Submit. The sidebar shows each step's status (to do, needs
  attention, running, done).
- Builds, screenshot generation and uploads run in a jobs drawer at the bottom
  and keep going while you move between steps.
- The signing identity and provisioning profile moved from Settings to the
  Accounts step, and the app version to the Version step. The provisioning
  profile is now chosen per project, since each profile belongs to one app;
  Settings keeps the fal.ai key, device style and inference steps.

### Fixed

- Android screenshots are saved inside the project, one set per language, in
  fastlane's folder layout. They used to land wherever AppScreens was started
  from, and each language overwrote the one before, so every language got the
  same images on Google Play.
- Google Play gets a single feature graphic per language, taken from the first
  screen. Every screen used to produce one, but Play keeps only one.
- Google Play uploads for Arabic, Hindi, Turkish and Chinese (Simplified) now
  use the language codes Play accepts; they were rejected before.
- Desktop screenshot sizes are exported. Desktop projects get a landscape
  layout: captions on top with the whole window fitted below, or an AI-drawn
  laptop frame.
- Unticking iOS or Android under Sizes stops that platform from being exported.

### Added

- A Store text step: description, what's new, App Store keywords and
  promotional text, Play short description and support URL per language, with
  each store's character limits shown as you type and "copy from English" to
  start a translation.
- The Submit step runs each store as numbered stages, with a readiness
  checklist and a release history. For the App Store it can wait for the build
  to process, answer export compliance and submit the version for review; for
  Google Play it uploads the newest AAB to the track you choose, as a draft or
  a rollout.
- The provisioning profile is checked against the app: bundle ID, expiry,
  distribution type and team. **Test connection** checks App Store Connect and
  Google Play credentials before you rely on them.
- Each project keeps its own version, next iOS build number and next Android
  versionCode.

## [0.1.11] - 2026-09-14

### Added

- A banner at startup for occasional messages from us, such as a request for
  feedback. It is fetched once from mayorana.ch, stays until you dismiss it and
  is not shown again after that. If the notice cannot be fetched, no banner
  appears and startup is not slowed.

## [0.1.10] - 2026-09-06

### Changed

- Packaging only — no user-visible change.

## [0.1.9] - 2026-09-06

### Changed

- A redesigned interface: a consistent type scale, spacing and colours in both
  themes, and line icons in place of the emoji that used to label buttons and
  tabs.
- Screenshot previews show the whole phone screen instead of cropping its top
  and bottom.
- Placeholder text is readable, and every control shows keyboard focus.

### Fixed

- On Android, layouts meant for small screens now apply. They were being
  ignored, so the app was laid out as if on a desktop-width page.

## [0.1.8] - 2026-08-30

### Changed

- Packaging only — the release pipeline can now sign the Windows installer.

## [0.1.7] - 2026-08-28

### Changed

- Packaging only — no user-visible change.

## [0.1.6] - 2026-08-27

### Changed

- Packaging only — no user-visible change.

## [0.1.5] - 2026-08-27

### Changed

- Downloads and update checks now come from mayorana.ch instead of GitHub. The
  update banner links to the download page there.
- AppScreens is source-available under the PolyForm Noncommercial licence:
  free for personal, educational and noncommercial use.

## [0.1.4] - 2026-08-27

### Changed

- Packaging only — no user-visible change.

## [0.1.3] - 2026-08-27

### Added

- AppScreens runs on Android, picking images through the system file picker.
- A banner when a newer version is available.
- A light and dark theme toggle, following the system theme until you choose.
- A new app icon.

### Fixed

- The packaged app no longer loses its styling. The stylesheet is now built
  into the app instead of being loaded from beside it.

## [0.1.2] - 2026-06-06

### Added

- A signed and notarized macOS build that opens with a normal double-click.

## [0.1.1] - 2026-06-06

### Added

- First release. Create a new Dioxus project from a wizard or open an existing
  one, for iOS, Android or desktop.
- Screenshot generation per language, with a title and subtitle on each image:
  either drawn over your own colours, or placed in a device frame generated by
  fal.ai from a style you describe.
- Export at the sizes each store requires for iPhone, iPad, Android phone, the
  Play feature graphic and desktop.
- Upload screenshots to App Store Connect and Google Play, and build signed iOS
  and Android releases, from the app.
