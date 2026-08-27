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

---

## Licence

Source-available under the [PolyForm Noncommercial License 1.0.0](LICENSE).

- **Free** for personal use, learning, research and hobby projects, and for
  charities, schools, universities and government institutions.
- **Commercial use requires a licence** — including a solo consultant using it
  on client work, and an employee using it at their job.
  [Get in touch](https://mayorana.ch/en/contact).

This is deliberately not an OSI-approved open source licence: the source is
public and readable, but companies using it for work buy a licence.

The name, logo and icons are trademarks and are not covered by that licence —
fork it and rebrand it. See [TRADEMARK.md](TRADEMARK.md).
