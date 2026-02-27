#!/bin/bash

set -e

APK_PATH="$(find target/dx -name "*.apk" 2>/dev/null | head -1)"

echo "=== Android Build Verification ==="
echo

if [[ ! -f "$APK_PATH" ]]; then
    echo "❌ APK not found. Run: dx bundle --platform android --release"
    exit 1
fi

echo "✅ APK: $APK_PATH"

APK_SIZE=$(du -h "$APK_PATH" | cut -f1)
echo "📦 Size: $APK_SIZE"

if command -v aapt &> /dev/null; then
    PACKAGE=$(aapt dump badging "$APK_PATH" 2>/dev/null | grep "package: name" | sed "s/.*name='\([^']*\)'.*/\1/")
    VERSION=$(aapt dump badging "$APK_PATH" 2>/dev/null | grep "versionName" | sed "s/.*versionName='\([^']*\)'.*/\1/")
    echo "📱 Package: $PACKAGE"
    echo "🔢 Version: $VERSION"
fi

echo
echo "✅ Ready for installation"
echo "Run: ./install_android.sh"
