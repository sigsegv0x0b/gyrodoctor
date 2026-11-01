//! `gyro-doctor-render`: render the original and the repaired video side by side, or the
//! raw video against a Gyroflow stabilization, and compose them into one MP4.
//!
//! This is the functional Rust port of the render command. It composes the two video
//! panes under a title bar with ffmpeg. The three rate panels and the annotated extents
//! of the Python tool's column are not drawn yet; the rate axes and decorator options are
//! accepted for command line compatibility.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use clap::Parser;

use gyro_doctor::cache::{self, CachePaths};
use gyro_doctor::decode::DecodeResult;
use gyro_doctor::do_not_repair::{parse_duration, parse_position};

#[derive(Debug, Parser)]
#[command(
    name = "gyro-doctor-render",
    version,
    about = "Renders the original and the repaired video side by side using Gyroflow for the stabilization.",
    after_help = "The gyro repair algorithm is based on Gyro Doctor by DredSkyeQuads:\nhttps://github.com/DredSkyeQuads/ds-gyro-doctor\n\nLossless MP4 chunk merging uses mp4-merge, the merge library used by Gyroflow:\nhttps://github.com/gyroflow/gyroflow"
)]
struct Cli {
    /// The original video.
    #[arg(value_name = "VIDEO")]
    video: PathBuf,

    /// original-vs-repair, or raw-vs-stab.
    #[arg(long, value_name = "MODE")]
    mode: Option<String>,

    /// The complete path of the composite.
    #[arg(long, value_name = "PATH")]
    output: Option<PathBuf>,

    /// The first video frame to render: a time or a video frame.
    #[arg(long, value_name = "POSITION")]
    start: Option<String>,

    /// The duration to render from --start: a time or a count of video frames.
    #[arg(long, value_name = "DURATION")]
    duration: Option<String>,

    /// The cache, as gyro-doctor takes it.
    #[arg(long, value_name = "PATH")]
    cache_dir: Option<PathBuf>,

    /// The Gyroflow dials of every Gyroflow render: standard or zooming.
    #[arg(long, default_value = "zooming")]
    preset: String,

    /// The rate axis of the three panels (accepted for compatibility; none are drawn yet).
    #[arg(long, value_name = "VALUE or LIST")]
    rate_axis_scale: Option<String>,

    /// The color of the marks of one classification (accepted for compatibility).
    #[arg(long, value_name = "CLASS=#RRGGBB")]
    decorator_color: Vec<String>,

    /// Print what the render drew.
    #[arg(long)]
    verbose: bool,
}

struct Dials {
    smoothness: f64,
    fov: f64,
    lens: f64,
    zoom_window: f64,
    max_zoom: Option<f64>,
}

fn dials(preset: &str) -> Result<Dials> {
    match preset {
        "standard" => Ok(Dials {
            smoothness: 0.5,
            fov: 0.9,
            lens: 0.25,
            zoom_window: 0.0,
            max_zoom: None,
        }),
        "zooming" => Ok(Dials {
            smoothness: 0.15,
            fov: 1.0,
            lens: 0.20,
            zoom_window: 2.0,
            max_zoom: Some(130.0),
        }),
        other => bail!("--preset takes standard or zooming, not {other}"),
    }
}

fn cache_root(cli: &Cli) -> PathBuf {
    cli.cache_dir
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(".gyro-doctor-cache"))
}

fn read_decode(paths: &CachePaths, video: &Path) -> Result<DecodeResult> {
    cache::read_decode(&paths.decode(), video).with_context(|| {
        format!(
            "the decode result is missing or invalid at {}; run gyro-doctor --decode-only {}",
            paths.decode().display(),
            video.display()
        )
    })
}

fn repaired_video(paths: &CachePaths) -> Result<PathBuf> {
    let record = std::fs::read_to_string(paths.write_record()).with_context(|| {
        format!(
            "the write record is missing at {}; run gyro-doctor {} first",
            paths.write_record().display(),
            "VIDEO"
        )
    })?;
    let record: serde_json::Value = serde_json::from_str(&record)?;
    let path = record["repaired_video"]
        .as_str()
        .context("the write record holds no repaired video path")?;
    Ok(PathBuf::from(path))
}

fn range(
    decode: &DecodeResult,
    start: Option<&str>,
    duration: Option<&str>,
) -> Result<(i64, i64)> {
    let frame_rate = decode.video_frame_rate;
    let frame_count = decode.quaternion_count_per_telemetry_frame.len() as i64;
    let first = match start {
        Some(text) => parse_position(text)?.to_video_frame(frame_rate),
        None => 0,
    };
    if first < 0 || first >= frame_count {
        bail!(
            "--start names video frame {first}, outside the {frame_count} video frames of the video"
        );
    }
    let remaining = frame_count - first;
    let count = match duration {
        Some(text) => {
            let duration = parse_duration(text)?;
            match (duration.seconds, duration.video_frames) {
                (Some(seconds), _) => (seconds * frame_rate).round() as i64,
                (_, Some(frames)) => frames,
                _ => remaining,
            }
        }
        None => remaining,
    };
    if count <= 0 {
        bail!("--duration must select at least one video frame");
    }
    if count > remaining {
        bail!(
            "--start and --duration reach past the last video frame: {count} video frames from f{first} against {remaining} remaining"
        );
    }
    Ok((first, first + count - 1))
}

fn run_command(command: &mut Command, what: &str) -> Result<()> {
    let output = command
        .output()
        .with_context(|| format!("cannot run {what}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{what} failed: {}", stderr.lines().rev().take(8).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"));
    }
    Ok(())
}

fn ffmpeg_path() -> PathBuf {
    PathBuf::from("ffmpeg")
}

fn filter_chain(first: i64, last: i64, frame_rate: f64, title: &str, left_title: &str, right_title: &str) -> String {
    let start = first as f64 / frame_rate;
    let duration = (last - first + 1) as f64 / frame_rate;
    format!(
        "[0:v]trim=start={start:.9}:duration={duration:.9},setpts=PTS-STARTPTS,scale=1280:720:force_original_aspect_ratio=increase,crop=1280:720,setsar=1[a];\
         [1:v]trim=start={start:.9}:duration={duration:.9},setpts=PTS-STARTPTS,scale=1280:720:force_original_aspect_ratio=increase,crop=1280:720,setsar=1[b];\
         [a][b]hstack=inputs=2[stack];\
         [stack]pad=iw:ih+92:0:92:color=0x202020,\
         drawtext=font=sans:text='{title}':fontcolor=white:fontsize=34:x=(w-text_w)/2:y=20,\
         drawtext=font=sans:text='{left_title}':fontcolor=0xd8d8d8:fontsize=22:x=w/4-text_w/2:y=64,\
         drawtext=font=sans:text='{right_title}':fontcolor=0xd8d8d8:fontsize=22:x=3*w/4-text_w/2:y=64[v]"
    )
}

fn hstack(
    left: &Path,
    right: &Path,
    output: &Path,
    first: i64,
    last: i64,
    frame_rate: f64,
    title: &str,
    left_title: &str,
    right_title: &str,
) -> Result<()> {
    let filter = filter_chain(first, last, frame_rate, title, left_title, right_title);
    run_command(
        Command::new(ffmpeg_path())
            .arg("-y")
            .arg("-i")
            .arg(left)
            .arg("-i")
            .arg(right)
            .arg("-filter_complex")
            .arg(filter)
            .arg("-map")
            .arg("[v]")
            .arg("-an")
            .arg("-c:v")
            .arg("libx264")
            .arg("-preset")
            .arg("medium")
            .arg("-crf")
            .arg("18")
            .arg("-pix_fmt")
            .arg("yuv420p")
            .arg("-movflags")
            .arg("+faststart")
            .arg(output),
        "ffmpeg",
    )
}

fn gyroflow_render(
    video: &Path,
    output: &Path,
    first: i64,
    last: i64,
    frame_rate: f64,
    dials: &Dials,
) -> Result<PathBuf> {
    let executable = std::env::var_os("GYRO_DOCTOR_GYROFLOW")
        .map(PathBuf::from)
        .context("GYRO_DOCTOR_GYROFLOW is not set; point it at the Gyroflow executable")?;
    if !executable.is_file() {
        bail!(
            "GYRO_DOCTOR_GYROFLOW names {}, which is not a file",
            executable.display()
        );
    }
    let probe = Command::new("ffprobe")
        .arg("-v")
        .arg("error")
        .arg("-select_streams")
        .arg("v:0")
        .arg("-show_entries")
        .arg("stream=width,height,nb_frames")
        .arg("-of")
        .arg("json")
        .arg(video)
        .output()
        .context("cannot run ffprobe")?;
    if !probe.status.success() {
        bail!("ffprobe cannot read {}", video.display());
    }
    let info: serde_json::Value = serde_json::from_slice(&probe.stdout)?;
    let stream = &info["streams"][0];
    let width = stream["width"].as_u64().unwrap_or(0);
    let height = stream["height"].as_u64().unwrap_or(0);
    let frame_count = stream["nb_frames"]
        .as_str()
        .and_then(|frames| frames.parse::<u64>().ok())
        .unwrap_or(0);
    let trim = serde_json::json!([[
        (first as f64 / frame_rate * 1000.0 - 0.001).max(0.0),
        (last + 1) as f64 / frame_rate * 1000.0
    ]]);
    let project = serde_json::json!({
        "videofile": file_url(video),
        "video_info": {
            "width": width,
            "height": height,
            "num_frames": frame_count,
            "fps": frame_rate,
            "duration_ms": frame_count as f64 / frame_rate * 1000.0,
        },
        "stabilization": {
            "method": "Default",
            "smoothing_params": [{"name": "smoothness", "value": dials.smoothness}],
            "lens_correction_amount": dials.lens,
            "fov": dials.fov,
            "adaptive_zoom_window": dials.zoom_window,
            "max_zoom": dials.max_zoom,
            "max_zoom_iterations": 5,
        },
        "gyro_source": {
            "filepath": file_url(video),
            "integration_method": 0,
        },
        "trim_ranges_ms": trim,
        "output": {
            "codec": "H.265/HEVC",
            "output_folder": file_url(output.parent().unwrap_or(Path::new("."))).to_string() + "/",
            "output_filename": output.file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_default(),
            "output_width": 1280,
            "output_height": 720,
            "use_gpu": true,
        },
    });
    let project_path = output.with_extension("gyroflow");
    std::fs::write(&project_path, serde_json::to_string_pretty(&project)?)?;
    let cwd = executable.parent().unwrap_or(Path::new("."));
    let rendered = Command::new(&executable)
        .current_dir(cwd)
        .arg(std::path::absolute(&project_path)?)
        .arg("-f")
        .arg("--stdout-progress")
        .output()
        .with_context(|| format!("cannot run {}", executable.display()))?;
    if !output.exists() {
        bail!(
            "Gyroflow wrote no render; its output ends: {}",
            String::from_utf8_lossy(&rendered.stdout)
                .lines()
                .rev()
                .take(8)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    Ok(output.to_path_buf())
}

fn file_url(path: &Path) -> String {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    format!("file:///{}", path.to_string_lossy().replace('\\', "/").trim_start_matches('/'))
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    if !cli.video.is_file() {
        bail!("the video {} is not a file", cli.video.display());
    }
    let dials = dials(&cli.preset)?;
    let root = cache_root(&cli);
    let paths = CachePaths::new(&root, &cli.video);
    let decode = read_decode(&paths, &cli.video)?;
    let (first, last) = range(&decode, cli.start.as_deref(), cli.duration.as_deref())?;
    let mode = match cli.mode.as_deref() {
        Some("original-vs-repair") => "original-vs-repair",
        Some("raw-vs-stab") => "raw-vs-stab",
        Some(other) => bail!("--mode takes original-vs-repair or raw-vs-stab, not {other}"),
        None => {
            if paths.repair().exists() && paths.write_record().exists() {
                "original-vs-repair"
            } else if paths.detection().exists() {
                "raw-vs-stab"
            } else {
                bail!("the cache holds neither a repair result nor a detection result; run gyro-doctor first")
            }
        }
    };
    let output = cli.output.clone().unwrap_or_else(|| {
        let center = first + (last - first) / 2;
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(format!(
                "{}-{}-f{}-{}f.MP4",
                cli.video
                    .file_stem()
                    .map_or_else(String::new, |stem| stem.to_string_lossy().to_string()),
                mode,
                center,
                last - first + 1
            ))
    });
    if output.is_dir() {
        bail!("the output path {} is a directory", output.display());
    }
    if !output
        .parent()
        .is_none_or(|parent| parent.as_os_str().is_empty() || parent.is_dir())
    {
        bail!(
            "the output directory {} does not exist",
            output.parent().unwrap_or(Path::new("")).display()
        );
    }
    if output.exists() && output.canonicalize().ok() == cli.video.canonicalize().ok() {
        bail!("the output path {} resolves to the video", output.display());
    }

    if mode == "original-vs-repair" {
        let repaired = repaired_video(&paths)?;
        if !repaired.is_file() {
            bail!(
                "the repaired video {} named by the write record is missing",
                repaired.display()
            );
        }
        hstack(
            &cli.video,
            &repaired,
            &output,
            first,
            last,
            decode.video_frame_rate,
            "Original vs repaired",
            "A original",
            "B repaired",
        )?;
    } else {
        let render_path = output.with_file_name(format!(
            "{}-gyroflow.mp4",
            output
                .file_stem()
                .map_or_else(String::new, |stem| stem.to_string_lossy().to_string())
        ));
        let stabilized = gyroflow_render(
            &cli.video,
            &render_path,
            first,
            last,
            decode.video_frame_rate,
            &dials,
        )?;
        hstack(
            &cli.video,
            &stabilized,
            &output,
            first,
            last,
            decode.video_frame_rate,
            "Raw vs stabilized",
            "A raw",
            "B stabilized",
        )?;
    }

    if cli.verbose {
        println!("mode                  {mode}");
        println!(
            "video frames          f{}-f{}  ({} frames)",
            first,
            last,
            last - first + 1
        );
        println!("wrote {}", output.display());
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("gyro-doctor-render: {error:#}");
        std::process::exit(1);
    }
}
