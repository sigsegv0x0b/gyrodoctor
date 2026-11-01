# DJI Gyro Fixer (`gyrodoctor`)

A Rust implementation of the Gyro Doctor gyro-data repair pipeline, with a desktop
command-line tool and an Android application.

The project repairs the gyro data track of DJI O4 videos and can losslessly merge multiple
DJI O4 recording chunks into one repaired video. It does not re-encode the video: only the
`djmd` telemetry quaternions and the stored camera model name change.

## Repository layout

```text
core/       Rust core, desktop CLI, chart and render tools
android/    Android application and JNI bridge
```

### Core

- `core/src/decode.rs` — telemetry decode and rate derivation
- `core/src/detect.rs` — three-pass gyro noise detection
- `core/src/attenuate.rs` — ceiling attenuation
- `core/src/repair.rs` — repair, quaternion rebuild, assertions
- `core/src/merge.rs` — lossless chunk merge with `mp4-merge`
- `core/src/write.rs` — in-place telemetry writeback
- `core/src/pipeline.rs` — shared merge-and-repair pipeline
- `core/src/bin/` — product binaries and validation tools

The desktop binaries are:

```text
gyro-doctor
gyro-doctor-chart
gyro-doctor-render
```

### Android

The Android app is a small Kotlin application with a Jetpack-free native UI. It uses the
Storage Access Framework for multi-select input and folder output, passes raw file
descriptors to Rust through JNI, and runs the merge-and-repair work in a foreground
service.

Application ID:

```text
com.sigsegv0x0b.gyrodoctor
```

Display name:

```text
DJI Gyro Fixer
```

## Build

### Desktop

```sh
cd core
cargo build --release
./target/release/gyro-doctor --help
```

### Android

Requirements:

- Android SDK with platform 35 and build-tools 35.0.0
- Android NDK 27.2.12479018
- Rust targets `x86_64-linux-android` and `aarch64-linux-android`
- `cargo-ndk`
- JDK 17 or newer
- Gradle wrapper is included

```sh
export ANDROID_HOME=/path/to/android-sdk
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/27.2.12479018"

./android/build.sh
```

The APK is written to:

```text
android/app/build/outputs/apk/debug/app-debug.apk
```

Install and launch:

```sh
adb install -r android/app/build/outputs/apk/debug/app-debug.apk
adb shell am start -n com.sigsegv0x0b.gyrodoctor/.MainActivity
```

## Usage

### Repair one video

```sh
gyro-doctor VIDEO.MP4
```

The repaired video is written beside the input as `<stem>-repaired.<extension>`, or to the
path named by `--output`.

### Merge and repair chunks

```sh
gyro-doctor chunk_0001.MP4 chunk_0002.MP4 chunk_0003.MP4 \
  --output joined-repaired.MP4
```

The merge writes directly into the final output. There is no temporary joined file and no
cache in merge mode. The output is then decoded, detected, repaired, and patched in place.
Use `--merge-only` to stop after the lossless merge.

### Android

1. Select two or more video chunks.
2. Choose an output folder.
3. Press **Merge and repair**.

The app creates `joined-repaired.mp4` in the selected folder. It uses SAF descriptors
directly and does not copy the input chunks into app storage.

## Validation

The core was validated against the Python Gyro Doctor implementation:

- decode arrays are bit-identical
- detection local medians, local Robust Standard Deviations, and extents are bit-identical
- repair quaternions and Normalized Rate are bit-identical
- repaired MP4s are byte-for-byte identical with `cmp`

The Android app was validated on an x86_64 emulator with three 30-second chunks from a
Gyroflow-joined O4 recording:

- SAF multi-select and folder output
- 179,834 quaternions, 89,916 rate samples
- output telemetry bit-identical to the desktop merge-and-repair result
- one output document, no temporary joined file and no cache

## Attribution

The gyro repair algorithm is based on **Gyro Doctor** by DredSkyeQuads:

<https://github.com/DredSkyeQuads/ds-gyro-doctor>

Lossless MP4 chunk merging uses **mp4-merge**, the merge library used by **Gyroflow**:

<https://github.com/gyroflow/gyroflow>

The original Gyro Doctor is licensed under GPL-3.0-or-later. This project follows the same
license. See `LICENSE`.

## License

GPL-3.0-or-later.
