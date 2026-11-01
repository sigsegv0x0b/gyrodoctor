# AGENTS.md

Instructions for agents working in the `gyrodoctor` repository.

## Project

`gyrodoctor` is a Rust implementation of the Gyro Doctor gyro-data repair pipeline, with a
desktop CLI and an Android app.

- `core/` — Rust core and desktop binaries
- `android/` — Kotlin/Android app and JNI bridge

The app ID is `com.sigsegv0x0b.gyrodoctor`. The visible name is `DJI Gyro Fixer`.

## Core invariants

Do not break these without an explicit request:

1. **Single-file repair stays bit-identical to the Python reference.**
   - Reference clip: `../ds-gyro-doctor/test.mp4` when the Python checkout is present.
   - Expected output: `../ds-gyro-doctor/test-repaired.mp4`.
   - Compare with `cmp`, not just decoded values.
2. **Merge mode writes directly into the final output.**
   - No temporary joined file.
   - No `.partial`.
   - No cache in merge mode.
   - Source chunks are never modified or deleted.
   - Use `mp4_merge::join_file_streams`, not `join_files` (which creates a temp).
3. **Repair runs on the whole merged series.**
   - Never repair each chunk independently and then merge.
   - Detection statistics, extents, and the quaternion rebuild depend on the full timeline.
4. **The write stage patches telemetry in place.**
   - `write::write_in_place` / `write_in_place_stream` must not copy the merged file.
   - Only telemetry quaternion messages and the camera model marker change.
5. **Attribution stays in both user interfaces.**
   - Android About page must name Gyro Doctor and the original repository.
   - `gyro-doctor --help` must include the attribution footer.
   - Keep the GPL-3.0-or-later license.

## Rust core

Build:

```sh
cd core
cargo build --release
```

Important modules:

- `decode.rs` — telemetry ordering, collision handling, group openings, period test
- `detect.rs` — oscillation, burst, and impulse passes
- `fit.rs` — robust local linear regression and NumPy-compatible sums/medians
- `attenuate.rs` — ceiling attenuator
- `repair.rs` — repair orchestration and quaternion rebuild
- `merge.rs` — natural sort, validation, direct stream merge
- `write.rs` — in-place telemetry patch
- `pipeline.rs` — shared merge-and-repair pipeline for desktop/Android
- `mp4.rs` — MP4 box parser, including multi-sample-per-chunk `stsc`
- `telemetry.rs` — DJI `djmd` protobuf reading
- `android.rs` — JNI entry points (`runMerge`, `runMergeFds`)

### Numerical compatibility

The Python reference uses scipy and NumPy. Preserve these details:

- scipy normalizes quaternions in `(x, y, z, w)` order.
- scipy normalizes each composed rotation.
- scipy `as_rotvec` canonicalizes but does not renormalize.
- NumPy `x**3` uses libm `pow`; `x*x*x` is not equivalent.
- Use the NumPy-compatible pairwise sum and median helpers in `fit.rs`.

A one-ulp change in these paths can eventually flip float32 values in the repaired MP4.

### Merge validation

- Inputs are naturally sorted by filename.
- `mp4-merge` validates track and sample-description compatibility.
- If `mp4-merge` rejects only differing `btrt` metadata on metadata tracks, that is a
  sample-description issue. Real DJI chunks from one recording normally match; MP4Box
  splits may need normalization.
- A split that cuts a measurement group can fail the strict period test. Real O4 chunks
  generally align; investigate boundary handling if a real set fails.

## Android

Package structure:

```text
android/app/src/main/java/com/sigsegv0x0b/gyrodoctor/
```

Key files:

- `MainActivity.kt` — UI, SAF pickers, background thread, progress
- `Native.kt` — `System.loadLibrary` and external methods
- `ProcessingService.kt` — data-sync foreground service
- `AndroidManifest.xml` — permissions, service type, label

JNI symbol names must match the package:

```text
Java_com_sigsegv0x0b_gyrodoctor_Native_runMerge
Java_com_sigsegv0x0b_gyrodoctor_Native_runMergeFds
```

If the package or `Native` class changes, update `core/src/android.rs` too.

### Android build

```sh
export ANDROID_HOME=/path/to/android-sdk
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/27.2.12479018"
./android/build.sh
```

or:

```sh
cd core
cargo ndk -t x86_64 -t arm64-v8a \
  -o ../android/app/src/main/jniLibs \
  build --release --lib
cd ../android
./gradlew :app:assembleDebug
```

The app needs both `x86_64` (emulator) and `arm64-v8a` (phone) libraries.

### Android file handling

- Inputs: `ACTION_OPEN_DOCUMENT` with `EXTRA_ALLOW_MULTIPLE`, opened as seekable
  `ParcelFileDescriptor`s.
- Output: `ACTION_OPEN_DOCUMENT_TREE`, then `DocumentsContract.createDocument` for
  `joined-repaired.mp4`.
- Pass raw descriptors to `runMergeFds`; do not copy chunks into app storage.
- Sort selected inputs by display name before opening them.
- Keep descriptors open until the native call finishes, then close them.
- On failure, delete the created output document.
- Use a `dataSync` foreground service for long runs.

## Do not commit

- `core/target/`
- `android/.gradle/`, `android/.kotlin/`, `android/build/`, `android/app/build/`
- `android/local.properties`
- `android/app/src/main/jniLibs/` (generated `.so` files)
- video files, caches, APKs, or test media

## Testing checklist

Before claiming a core change is safe:

1. `cargo build --release`
2. Repair the reference clip and `cmp` against the Python output.
3. Merge real or split chunks and compare repaired telemetry arrays with the direct
   single-file repair.
4. On Android, test both the app-storage debug path and the SAF picker path.
5. Confirm the About page and `--help` attribution remain.
