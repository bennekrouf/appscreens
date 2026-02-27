#!/bin/bash

set -e

APK_PATH="$(find target/dx -name "*.apk" 2>/dev/null | head -1)"
ADB="$HOME/Library/Android/sdk/platform-tools/adb"

echo "=== Android Installation ==="

if [[ ! -f "$APK_PATH" ]]; then
    echo "❌ APK not found. Run: dx bundle --platform android --release"
    exit 1
fi

if [[ ! -f "$ADB" ]]; then
    echo "❌ adb not found at $ADB"
    exit 1
fi

DEVICES=$("$ADB" devices | grep -v "List" | grep "device$" | wc -l)
if [[ $DEVICES -eq 0 ]]; then
    echo "❌ No Android devices connected"
    echo "Enable USB debugging and connect device"
    exit 1
fi

echo "📱 Connected devices:"
"$ADB" devices

echo
echo "🚀 Installing APK..."
"$ADB" install -r "$APK_PATH"

echo "✅ Installation complete"
