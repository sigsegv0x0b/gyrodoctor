# Message to the original author

Hi Jamie / DredSkyeQuads,

I’m sigsegv0x0b. I’ve been using Gyro Doctor and Gyroflow, and I wanted a version that works well on Android. With a coding agent, I ported your gyro repair pipeline to Rust using your implementation as the reference, and built an Android app around it.

I want to be clear: this is a port based on your code and algorithm, not an attempt to claim the original work as mine. The Rust project is GPL-3.0-or-later and credits you in the README, the Android app’s About page, and the CLI `--help`.

On a 7.7 GB joined DJI O4 file, with fresh caches and both writing to the same filesystem, I measured:

| | Python | Rust |
|---|---:|---:|
| Wall time | 379.62 s (6:19.6) | 165.48 s (2:45.5) |
| Peak RSS | 1.51 GB | 0.74 GB |
| Output | — | — |

That is about 2.3× faster and roughly half the peak memory. The repaired MP4s were byte-for-byte identical with `cmp`, and the decoded quaternion/rate arrays matched exactly. The rough stage breakdown was:

| Stage | Python | Rust |
|---|---:|---:|
| Decode | ~68 s | ~17.5 s |
| Detection | ~20.7 s | ~8.6 s |
| Repair | ~8.2 s | ~0.6 s |
| Write/copy | ~282 s | ~139 s |

These are cold-cache runs on my machine, so the absolute numbers are hardware-dependent, but the direction was consistent.

The main reason I did this was Android. I use Gyroflow on my phone and tablet, and Python is too slow and awkward for that workflow. The Android app selects chunks through SAF, passes file descriptors directly to Rust, merges the chunks losslessly, repairs the gyro data in place, and writes one output file without copying the inputs or creating temporary joined files. It also has a foreground service for long recordings.

My plan is to keep this repository private for my own workflow and sync with your changes when I need them. But if you look at the Rust port and think the performance and memory advantages are worth moving to Rust — and you are comfortable with Rust, or happy to use coding agents — you are welcome to take this codebase if you find it useful. I’m not asking you to do that; I just wanted to put it in front of you. If you want to look at it, I can add you to the private repository.

I’m not trying to take over your project. If you’d rather keep the Rust work separate, I’ll continue it as an independent GPL project with clear attribution. Either way, I’d appreciate your thoughts, especially on licensing and attribution.

Thanks for the original work — the repair pipeline and the documentation made the port possible.

— sigsegv0x0b
