# DJI Gyro Fixer for Android

Android UI over the Rust Gyro Doctor merge-and-repair core.

The gyro data repair algorithm is based on Gyro Doctor by DredSkyeQuads:
https://github.com/DredSkyeQuads/ds-gyro-doctor

Lossless MP4 chunk merging uses mp4-merge, the merge library used by Gyroflow:
https://github.com/gyroflow/gyroflow

## What it does

- Select two or more DJI O4 video chunks with the Android Storage Access Framework.
- Choose an output folder; the app creates `joined-repaired.mp4` inside it.
- Merge the chunks losslessly with `mp4-merge`.
- Decode, detect, repair, and patch the merged file in place.
- Save one merged, repaired MP4.

The app uses the SAF file descriptors directly. Input chunks are not copied into app
storage and no temporary joined file or cache is created. The only new file is the final
output document.

## Build

Requirements:

- Android SDK with platform 35 and build-tools 35.0.0
- Android NDK 27.2.12479018
- Rust targets `x86_64-linux-android` and `aarch64-linux-android`
- `cargo-ndk`
- JDK 17 or newer

Run:

```sh
export ANDROID_HOME=/path/to/android-sdk
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/27.2.12479018"
./build.sh
```

or manually:

```sh
cd ../core
ANDROID_NDK_HOME="$ANDROID_NDK_HOME" \
  cargo ndk -t x86_64 -t arm64-v8a \
  -o ../android/app/src/main/jniLibs \
  build --release --lib

cd ../android
./gradlew :app:assembleDebug
```

The APK is at:

```text
app/build/outputs/apk/debug/app-debug.apk
```

It contains both `x86_64` (emulator) and `arm64-v8a` (phone) native libraries.

## Install and run

```sh
adb install -r app/build/outputs/apk/debug/app-debug.apk
adb shell am start -n com.sigsegv0x0b.gyrodoctor/.MainActivity
```

Use **Select video chunks**, then **Choose output folder**, then **Merge and repair**.

Two debug buttons are included:

- **Run files from app storage**: reads `files/input/*.mp4` and writes
  `files/output/joined-repaired.mp4`.
- **Run app files via file descriptors**: exercises the same fd-based native path against
  those files.

## Native interface

`Native.runMergeFds(inputFds, outputFd, optionsJson, callback)` passes raw file descriptors
to Rust. Rust duplicates them, merges directly into the output descriptor, decodes the
merged stream, runs detection and repair, and patches the telemetry in place.

The callback receives `stage`, `message`, and `percent`.

## Validation

Validated on the Android x86_64 emulator with three 30-second O4 chunks:

- merge + repair completed through the SAF picker and `runMergeFds`
- output telemetry was bit-identical to the same merge + repair on desktop:
  - 179,834 quaternions
  - 89,916 rate samples
  - identical times, quaternions, rates, and group openings
- both the app-storage fd test and the full SAF picker flow produced output pieces
- no temporary files or cache
- output written directly to the selected SAF document

## Current limitations

- No chart or render tools on Android yet.
- Long processing runs in a foreground service with a data-sync notification, so it
  survives the activity being backgrounded. Notification permission is requested on
  Android 13+.
- The activity also holds a keep-screen-on flag while visible.
- Chunk order is sorted by display name. DJI's fixed-width `_0001`, `_0002` names sort
  correctly.
- The app currently passes `force=true` to tolerate an existing output document. It does
  not yet expose repair tuning options or a do-not-repair list.
- MP4Box-split test chunks needed `btrt` sample-description normalization before
  `mp4-merge` accepted them. Real DJI chunks from one recording share their sample
  descriptions; if a real set fails only on bitrate metadata, a normalization pass is the
  next fix.
