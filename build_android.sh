#!/bin/bash

set -e

export ANDROID_HOME="$HOME/Library/Android/sdk"
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk"
NDK_VERSION=$(ls -1 "$ANDROID_NDK_HOME" 2>/dev/null | grep -E '^[0-9]+\.' | sort -V | tail -1)
export ANDROID_NDK_HOME="$ANDROID_NDK_HOME/$NDK_VERSION"

echo "=== Building Android APK ==="
echo "NDK: $NDK_VERSION"
echo

dx bundle --platform android --release

echo
echo "✅ Build complete"
./check_android.sh
