# AppScreens

Dioxus Android app ready for deployment.

## Quick Start

```bash
# Build APK
./build_android.sh

# Install on device
./install_android.sh
```

## Development

```bash
# Build release APK
dx bundle --platform android --release

# Check build
./check_android.sh

# Install
./install_android.sh
```

## Configuration

- Package: `com.mayorana.appscreens`
- Display Name: `AppScreens`
- Min SDK: 24 (Android 7.0)

## Requirements

- Android Studio with NDK
- USB debugging enabled
- Device connected via USB

Built with 🦀 Rust + Dioxus
