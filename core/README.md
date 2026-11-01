# gyro-doctor (Rust)

A Rust port of the Gyro Doctor repair pipeline for the gyro data track of DJI O4 videos.

The core repair pipeline is validated against the Python product: on the 30-second
reference clip and on a 9 GB joined clip it produces a repaired MP4 that is byte-for-byte
identical to Python's, including the same 1,062,787 rewritten quaternions and the same
31,854 rewritten telemetry frames. The decode arrays, the detection arrays, and the
repaired arrays are bit-identical to Python's NumPy results.

## Build

```sh
cargo build --release
```

The three product binaries are:

```sh
target/release/gyro-doctor
target/release/gyro-doctor-chart
target/release/gyro-doctor-render
```

## Repair

```sh
target/release/gyro-doctor [options] VIDEO
```

By default the repaired video is written beside the input as
`<video stem>-repaired.<extension>`. Use `--output PATH` to choose another path.

The command runs the decode, detection, repair, and write stages in that order. It writes
and reuses the cache in `.gyro-doctor-cache/<video stem>/` unless `--no-cache` is given or
another `--cache-dir` is chosen.

Useful options:

```text
--output PATH
--force
--report
--verbose
--cache-dir PATH
--no-cache
--decode-only / --detect-only / --repair-only
--detect-axes LIST
--repair-axes LIST
--remove-gyro-motion LIST
--do-not-repair-list LIST
--do-not-repair FILE
--detect-*-threshold VALUE[,VALUE,VALUE]
--repair-*-ceiling VALUE
--repair-*-centerline local-median|chord|zero
--repair-fade MS
--repair-fade-shape raised-cosine|linear
--repair-edges
--help
```

The do-not-repair list accepts identifiers such as `P210`, time ranges such as
`0:06.7-0:07.6`, and video frame ranges such as `Rf4500-f4620`. A range may carry an
axis letter `P`, `Y`, or `R`.

## Cache

The cache holds one directory per video, keyed by the video file stem:

```text
<cache>/<stem>/
  <stem>-decode-result.bin
  <stem>-detection-result.bin
  <stem>-detection-run-record.json
  <stem>-<axis>-extent-table.csv
  <stem>-repair-result.bin
  <stem>-repair-record.json
  <stem>-write-record.json
  README.md
```

Results are stored with `bincode`, not the Python `.npz` format. The JSON sidecars hold the
file stamps and the run records. A cached result is reused only when its header matches the
video on disk, its upstream file stamps still match, and its parameters match the current
run. A cache that fails a test is logged and the stage runs again.

The decode cache keeps the inventory table in `README.md`, and the detection cache keeps
the per-axis extent tables and run record.


## Merging camera chunks

Two or more DJI O4 chunks can be merged and repaired in one operation:

```sh
target/release/gyro-doctor chunk_0001.MP4 chunk_0002.MP4 chunk_0003.MP4 \
  --output joined-repaired.MP4
```

The output is one merged MP4 with repaired gyro data. The merge is lossless and uses
`mp4-merge`, the same crate Gyroflow uses. It writes directly into the output file, which
is then rewound, decoded, detected, repaired, and patched in place.

There is no temporary joined file, no `.partial`, and no cache in merge mode. Peak
storage is the input chunks plus the final output. Source chunks are never modified or
deleted. If any stage fails, the incomplete output is removed.

Useful merge options:

```text
--merge-only          merge without repairing
--output PATH         the final merged file
--force               replace an existing output
```

The inputs are naturally sorted by filename. `mp4-merge` validates the tracks and sample
descriptions before writing. `--decode-only`, `--detect-only`, and `--repair-only` are not
supported in merge mode.

## Chart

```sh
target/release/gyro-doctor-chart --center f4500 VIDEO
```

It draws the Normalized Rate of pitch, yaw, and roll from the cache, with the original and
repaired series, the detected extents, the local median, and the pass thresholds. The
output is a PNG by default; `.svg` output is also accepted. PNG conversion uses the
ImageMagick `convert` command.

This is a functional Rust chart rather than a pixel-identical matplotlib reproduction.

## Render

```sh
target/release/gyro-doctor-render [options] VIDEO
```

`--mode original-vs-repair` composes the original video and the repaired video named by the
cache's write record side by side under a title bar. `--mode raw-vs-stab` renders a
Gyroflow stabilization of the selected range and composes it against the raw video; it
requires the `GYRO_DOCTOR_GYROFLOW` environment variable to name the Gyroflow executable.

Both modes use `ffmpeg` to encode the composite. This port currently draws the two video
panes and the title bar; the Python tool's three scrolling rate panels and annotated
extents are not drawn yet.

## Validation

The core pipeline was compared with Python on the reference clip and on the 9 GB joined
clip:

- decode result: bit-identical rate arrays and quaternion arrays;
- detection result: bit-identical local medians, local Robust Standard Deviations, and
  extent tables;
- repair result: bit-identical repaired quaternions and Normalized Rate;
- repaired MP4: byte-for-byte identical to Python's `cmp`.

The exactness required matching three details of the Python stack: scipy normalizes
quaternions in `(x, y, z, w)` order and normalizes each composed rotation, scipy's
`as_rotvec` does not renormalize, and NumPy's `x**3` uses libm `pow` rather than `x*x*x`.

## Android

The Android UI lives in `../android`. It uses the same core through
JNI:

- SAF multi-select opens the chunks as file descriptors.
- `Native.runMergeFds` passes those descriptors to Rust.
- Rust merges directly into the output document descriptor, decodes the merged stream,
  repairs it, and patches it in place.

No input copies, no temporary joined file, and no cache are used. See the Android
project's `README.md` for build and validation details.
