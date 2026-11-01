#!/usr/bin/env bash
set -euo pipefail

# Build the Rust native libraries and the Android APK.
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ANDROID_DIR="$(cd "$(dirname "$0")" && pwd)"
RUST_DIR="${RUST_DIR:-$ROOT/core}"
ANDROID_HOME="${ANDROID_HOME:-$HOME/android-sdk}"
NDK="${ANDROID_NDK_HOME:-$ANDROID_HOME/ndk/27.2.12479018}"
JNI_DIR="$ANDROID_DIR/app/src/main/jniLibs"

if [ ! -d "$NDK" ]; then
    echo "Android NDK not found at $NDK" >&2
    echo "Set ANDROID_NDK_HOME or install NDK 27.2.12479018" >&2
    exit 1
fi

cd "$RUST_DIR"
ANDROID_NDK_HOME="$NDK" cargo ndk \
    -t x86_64 -t arm64-v8a \
    -o "$JNI_DIR" \
    build --release --lib

cd "$ANDROID_DIR"
export ANDROID_HOME
./gradlew :app:assembleDebug
