//! `gyro-doctor` repairs the gyro data track of a DJI O4 video and saves the repaired
//! video alongside the original.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Parser;

use gyro_doctor::attenuate::{RepairParameters, CENTERLINES, FADE_SHAPES};
use gyro_doctor::cache::{self, CachePaths};
use gyro_doctor::decode::{self, DecodeResult};
use gyro_doctor::detect::{self, DetectionParameters, DetectionResult};
use gyro_doctor::do_not_repair;
use gyro_doctor::mp4::FileStamp;
use gyro_doctor::repair::{self, RepairResult};
use gyro_doctor::report;

#[derive(Debug, Parser)]
#[command(
    name = "gyro-doctor",
    version,
    about = "Repairs the gyro data track of a DJI O4 video and saves the repaired video alongside the original.",
    after_help = "The gyro repair algorithm is based on Gyro Doctor by DredSkyeQuads:\nhttps://github.com/DredSkyeQuads/ds-gyro-doctor\n\nLossless MP4 chunk merging uses mp4-merge, the merge library used by Gyroflow:\nhttps://github.com/gyroflow/gyroflow"
)]
struct Cli {
    /// The video to repair, or two or more chunks to merge and repair.
    #[arg(value_name = "VIDEO", num_args = 1..)]
    videos: Vec<PathBuf>,

    /// The repaired video to write (default: the video's name with -repaired).
    #[arg(short, long, value_name = "PATH")]
    output: Option<PathBuf>,

    /// Repair a video that already carries the repaired model name marker, and replace an existing output.
    #[arg(long)]
    force: bool,

    /// Merge the inputs only, without repairing the gyro data.
    #[arg(long)]
    merge_only: bool,

    /// Print the report of each stage.
    #[arg(long)]
    report: bool,

    /// Print one line for each stage.
    #[arg(long)]
    verbose: bool,

    /// Do not read or write the cache.
    #[arg(long)]
    no_cache: bool,

    /// The location of the cache (default: .gyro-doctor-cache under the current directory).
    #[arg(long, value_name = "PATH")]
    cache_dir: Option<PathBuf>,

    /// Stop the run after the decode stage.
    #[arg(long, group = "stop")]
    decode_only: bool,

    /// Stop the run after the detection stage.
    #[arg(long, group = "stop")]
    detect_only: bool,

    /// Stop the run after the repair stage.
    #[arg(long, group = "stop")]
    repair_only: bool,

    /// The detect axes to process.
    #[arg(long, value_name = "LIST", default_value = "pitch,yaw,roll")]
    detect_axes: String,

    /// The repair axes to process (default: the detected axes).
    #[arg(long, value_name = "LIST")]
    repair_axes: Option<String>,

    /// Do not repair the named extents, time ranges, and frame ranges in LIST.
    #[arg(long, value_name = "LIST")]
    do_not_repair_list: Option<String>,

    /// A text file for the do not repair list, one item or range on each line.
    #[arg(long, value_name = "FILE")]
    do_not_repair: Option<PathBuf>,

    /// Noise threshold for a large amplitude oscillation, one value or three.
    #[arg(long, value_name = "VALUE or LIST", default_value = "100.0")]
    detect_oscillation_threshold: String,

    /// Noise threshold for a mid-amplitude burst, one value or three.
    #[arg(long, value_name = "VALUE or LIST", default_value = "10.0")]
    detect_burst_threshold: String,

    /// Noise threshold for an impulse sample, one value or three.
    #[arg(long, value_name = "VALUE or LIST", default_value = "4.5")]
    detect_impulse_threshold: String,

    /// Ceiling for the repair of a large amplitude oscillation.
    #[arg(long, default_value = "0.0")]
    repair_oscillation_ceiling: f64,

    /// Ceiling for the repair of a mid-amplitude burst.
    #[arg(long, default_value = "2.0")]
    repair_burst_ceiling: f64,

    /// Ceiling for the repair of an impulse event.
    #[arg(long, default_value = "0.5")]
    repair_impulse_ceiling: f64,

    /// Centerline toward which a large amplitude oscillation is attenuated.
    #[arg(long, default_value = "chord")]
    repair_oscillation_centerline: String,

    /// Centerline toward which a mid-amplitude burst is attenuated.
    #[arg(long, default_value = "local-median")]
    repair_burst_centerline: String,

    /// Centerline toward which an impulse event is attenuated.
    #[arg(long, default_value = "local-median")]
    repair_impulse_centerline: String,

    /// The fade at each end of a repair, in milliseconds.
    #[arg(long, default_value = "4.0")]
    repair_fade: f64,

    /// The shape of the fade at each end of a repair.
    #[arg(long, default_value = "raised-cosine")]
    repair_fade_shape: String,

    /// Repair a gyro noise event at the beginning or end of the video.
    #[arg(long)]
    repair_edges: bool,

    /// Axes whose gyro motion the repair removes.
    #[arg(long, value_name = "LIST")]
    remove_gyro_motion: Option<String>,

    /// The time in milliseconds on either side of the local median window.
    #[arg(long, default_value = "100.0")]
    local_median_window: f64,

    /// The time in milliseconds on either side of the noise window.
    #[arg(long, default_value = "100.0")]
    detect_noise_window: f64,

    /// The event end level, in Robust Standard Deviations.
    #[arg(long, default_value = "2.0")]
    detect_event_end_level: f64,

    /// The event end time, in milliseconds.
    #[arg(long, default_value = "10.0")]
    detect_event_end_time: f64,

    /// The shortest impulse event, in milliseconds.
    #[arg(long, default_value = "3.0")]
    detect_impulse_min_width: f64,

    /// The longest impulse event before widening, in milliseconds.
    #[arg(long, default_value = "25.0")]
    detect_impulse_max_width: f64,

    /// The shoulder drop, in Robust Standard Deviations.
    #[arg(long, default_value = "1.0")]
    detect_impulse_shoulder_drop: f64,

    /// The shoulder time, in milliseconds.
    #[arg(long, default_value = "12.0")]
    detect_impulse_shoulder_time: f64,

    /// The side end time, in milliseconds.
    #[arg(long, default_value = "5.0")]
    detect_impulse_side_end_time: f64,

    /// The widening step of an impulse event, in milliseconds.
    #[arg(long, default_value = "5.0")]
    detect_impulse_widen: f64,

    /// The longest widening of each end of an impulse event, in milliseconds.
    #[arg(long, default_value = "50.0")]
    detect_impulse_widen_max: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    Decode,
    Detection,
    Repair,
    Write,
}

fn parse_axes(text: &str, option: &str) -> Result<Vec<String>> {
    let values: Vec<String> = text
        .split(',')
        .map(|axis| axis.trim().to_string())
        .filter(|axis| !axis.is_empty())
        .collect();
    for axis in &values {
        detect::axis_column(axis)
            .with_context(|| format!("{option} takes one or more axes of pitch, yaw, roll"))?;
    }
    if values.is_empty() {
        bail!("{option} takes one or more axes of pitch, yaw, roll");
    }
    if values.iter().enumerate().any(|(index, axis)| {
        values[..index].iter().any(|previous| previous == axis)
    }) {
        bail!("{option} takes distinct axes");
    }
    Ok(values)
}

fn parse_threshold(text: &str, option: &str) -> Result<Vec<f64>> {
    let values: Vec<f64> = text
        .split(',')
        .map(|value| value.trim().parse::<f64>())
        .collect::<std::result::Result<_, _>>()
        .with_context(|| format!("{option} takes a number or a comma separated list"))?;
    if values.len() != 1 && values.len() != 3 {
        bail!("{option} takes one value for all three axes or three in pitch, yaw, roll order");
    }
    if values.iter().any(|value| !value.is_finite()) {
        bail!("{option} takes finite values");
    }
    Ok(values)
}

fn output_path(video: &Path, output: Option<PathBuf>) -> Result<PathBuf> {
    if !video.is_file() {
        bail!("the video {} is not a file", video.display());
    }
    let output = output.unwrap_or_else(|| {
        let stem = video
            .file_stem()
            .map_or_else(String::new, |stem| stem.to_string_lossy().to_string());
        let extension = video
            .extension()
            .map_or_else(String::new, |extension| {
                format!(".{}", extension.to_string_lossy())
            });
        video.with_file_name(format!("{stem}-repaired{extension}"))
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
    Ok(output)
}

fn header_matches(
    video: &Path,
    name: &str,
    size: u64,
    mtime: f64,
    version: u32,
) -> Result<()> {
    let expected = decode::video_identity(video)?;
    if version != expected.derivation_version {
        bail!(
            "the cached result was derived by version {version} and this build derives version {}",
            expected.derivation_version
        );
    }
    let mut differs = Vec::new();
    if name != expected.video_name {
        differs.push("video_name");
    }
    if size != expected.video_size_bytes {
        differs.push("video_size_bytes");
    }
    if (mtime - expected.video_mtime_s).abs() > 1e-6 {
        differs.push("video_mtime_s");
    }
    if !differs.is_empty() {
        bail!("the video has changed since the result was cached ({})", differs.join(", "));
    }
    Ok(())
}

fn cached_decode(paths: &CachePaths, video: &Path) -> Result<Option<DecodeResult>> {
    if !paths.decode().exists() {
        return Ok(None);
    }
    Ok(Some(cache::read_decode(&paths.decode(), video)?))
}

fn cached_detection(
    paths: &CachePaths,
    video: &Path,
    axes: &[String],
    parameters: &DetectionParameters,
) -> Result<Option<DetectionResult>> {
    if !paths.detection().exists() {
        return Ok(None);
    }
    let result = cache::read_detection(&paths.detection())?;
    header_matches(
        video,
        &result.video_name,
        result.video_size_bytes,
        result.video_mtime_s,
        result.derivation_version,
    )?;
    if !cache::same_detection_parameters(&result, axes, parameters)? {
        return Ok(None);
    }
    let stamps = cache::detection_stamps(paths)?;
    cache::file_stamps_test(&paths.directory, &stamps, &["decode_result"])?;
    Ok(Some(result))
}

fn cached_repair(
    paths: &CachePaths,
    video: &Path,
    axes: &[String],
    parameters: &RepairParameters,
) -> Result<Option<RepairResult>> {
    if !paths.repair().exists() {
        return Ok(None);
    }
    let record = cache::repair_record(paths)?;
    let video_record = &record["video"];
    header_matches(
        video,
        video_record["video_name"].as_str().unwrap_or(""),
        video_record["video_size_bytes"].as_u64().unwrap_or(0),
        video_record["video_mtime_s"].as_f64().unwrap_or(f64::NAN),
        video_record["derivation_version"]
            .as_u64()
            .unwrap_or(0) as u32,
    )?;
    if !cache::same_repair_parameters(&record, axes, parameters) {
        return Ok(None);
    }
    let stamps = cache::repair_stamps(paths)?;
    cache::file_stamps_test(
        &paths.directory,
        &stamps,
        &["decode_result", "detection_result"],
    )?;
    Ok(Some(cache::read_repair(&paths.repair())?))
}

fn print_lines(lines: &[String]) {
    for line in lines {
        println!("{line}");
    }
}

struct RunOptions {
    detect_axes: Vec<String>,
    repair_axes: Vec<String>,
    detection_parameters: DetectionParameters,
    repair_parameters: RepairParameters,
    list: do_not_repair::DoNotRepairList,
}

fn options(cli: &Cli) -> Result<RunOptions> {
    let detect_axes = parse_axes(&cli.detect_axes, "--detect-axes")?;
    let repair_axes = match &cli.repair_axes {
        Some(axes) => parse_axes(axes, "--repair-axes")?,
        None => detect_axes.clone(),
    };
    let remove_gyro_motion = match &cli.remove_gyro_motion {
        Some(axes) => parse_axes(axes, "--remove-gyro-motion")?,
        None => Vec::new(),
    };
    for axis in &repair_axes {
        if !detect_axes.contains(axis) {
            bail!(
                "--repair-axes names {axis}, which --detect-axes does not; the repair stage repairs only the detected axes"
            );
        }
    }

    let detection_parameters = DetectionParameters {
        detect_oscillation_threshold: parse_threshold(
            &cli.detect_oscillation_threshold,
            "--detect-oscillation-threshold",
        )?,
        detect_burst_threshold: parse_threshold(
            &cli.detect_burst_threshold,
            "--detect-burst-threshold",
        )?,
        detect_impulse_threshold: parse_threshold(
            &cli.detect_impulse_threshold,
            "--detect-impulse-threshold",
        )?,
        local_median_window: cli.local_median_window,
        detect_noise_window: cli.detect_noise_window,
        detect_event_end_level: cli.detect_event_end_level,
        detect_event_end_time: cli.detect_event_end_time,
        detect_impulse_min_width: cli.detect_impulse_min_width,
        detect_impulse_max_width: cli.detect_impulse_max_width,
        detect_impulse_shoulder_drop: cli.detect_impulse_shoulder_drop,
        detect_impulse_shoulder_time: cli.detect_impulse_shoulder_time,
        detect_impulse_side_end_time: cli.detect_impulse_side_end_time,
        detect_impulse_widen: cli.detect_impulse_widen,
        detect_impulse_widen_max: cli.detect_impulse_widen_max,
    };
    for (name, value) in [
        ("--detect-event-end-level", detection_parameters.detect_event_end_level),
        ("--detect-impulse-shoulder-drop", detection_parameters.detect_impulse_shoulder_drop),
    ] {
        if !(value > 0.0) {
            bail!("{name} takes a value above zero");
        }
    }
    for (name, value) in [
        ("--local-median-window", detection_parameters.local_median_window),
        ("--detect-noise-window", detection_parameters.detect_noise_window),
        ("--detect-impulse-widen", detection_parameters.detect_impulse_widen),
    ] {
        if !(value > 0.0) {
            bail!("{name} takes a time above zero");
        }
    }
    for (name, value) in [
        ("--detect-event-end-time", detection_parameters.detect_event_end_time),
        ("--detect-impulse-min-width", detection_parameters.detect_impulse_min_width),
        ("--detect-impulse-max-width", detection_parameters.detect_impulse_max_width),
        ("--detect-impulse-shoulder-time", detection_parameters.detect_impulse_shoulder_time),
        ("--detect-impulse-side-end-time", detection_parameters.detect_impulse_side_end_time),
        ("--detect-impulse-widen-max", detection_parameters.detect_impulse_widen_max),
        ("--repair-fade", cli.repair_fade),
    ] {
        if value < 0.0 {
            bail!("{name} takes a time at or above zero");
        }
    }
    if !CENTERLINES.contains(&cli.repair_oscillation_centerline.as_str())
        || !CENTERLINES.contains(&cli.repair_burst_centerline.as_str())
        || !CENTERLINES.contains(&cli.repair_impulse_centerline.as_str())
    {
        bail!("a centerline must be one of {}", CENTERLINES.join(", "));
    }
    if !FADE_SHAPES.contains(&cli.repair_fade_shape.as_str()) {
        bail!("a fade shape must be one of {}", FADE_SHAPES.join(", "));
    }
    for (name, value) in [
        ("--repair-oscillation-ceiling", cli.repair_oscillation_ceiling),
        ("--repair-burst-ceiling", cli.repair_burst_ceiling),
        ("--repair-impulse-ceiling", cli.repair_impulse_ceiling),
    ] {
        if value < 0.0 || value.is_nan() {
            bail!("{name} takes a ceiling at or above zero");
        }
    }

    let repair_parameters = RepairParameters {
        repair_oscillation_centerline: cli.repair_oscillation_centerline.clone(),
        repair_burst_centerline: cli.repair_burst_centerline.clone(),
        repair_impulse_centerline: cli.repair_impulse_centerline.clone(),
        repair_oscillation_ceiling: cli.repair_oscillation_ceiling,
        repair_burst_ceiling: cli.repair_burst_ceiling,
        repair_impulse_ceiling: cli.repair_impulse_ceiling,
        repair_fade: cli.repair_fade,
        repair_fade_shape: cli.repair_fade_shape.clone(),
        repair_edges: cli.repair_edges,
        remove_gyro_motion,
        do_not_repair: Vec::new(),
    };

    let list = do_not_repair::read_list(
        cli.do_not_repair_list.as_deref(),
        cli.do_not_repair.as_deref(),
    )?;
    Ok(RunOptions {
        detect_axes,
        repair_axes,
        detection_parameters,
        repair_parameters,
        list,
    })
}

fn merge_output_path(
    first: &Path,
    output: Option<PathBuf>,
    merge_only: bool,
) -> Result<PathBuf> {
    let output = output.unwrap_or_else(|| {
        let stem = first
            .file_stem()
            .map_or_else(String::new, |stem| stem.to_string_lossy().to_string());
        let extension = first
            .extension()
            .map_or_else(String::new, |extension| {
                format!(".{}", extension.to_string_lossy())
            });
        let suffix = if merge_only { "-joined" } else { "-joined-repaired" };
        first.with_file_name(format!("{stem}{suffix}{extension}"))
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
    Ok(output)
}

fn run_merge(cli: Cli) -> Result<()> {
    let RunOptions {
        detect_axes,
        repair_axes,
        detection_parameters,
        mut repair_parameters,
        list,
    } = options(&cli)?;
    if cli.decode_only || cli.detect_only || cli.repair_only {
        bail!("--decode-only, --detect-only, and --repair-only cannot be combined with a merge");
    }
    let mut inputs = cli.videos.clone();
    gyro_doctor::merge::natural_sort(&mut inputs);
    if inputs.len() < 2 {
        bail!("a merge takes at least two input files");
    }
    let output = merge_output_path(&inputs[0], cli.output.clone(), cli.merge_only)?;
    if output.exists() && !cli.force {
        bail!(
            "the output {} already exists; pass --force to replace it",
            output.display()
        );
    }
    for warning in &list.warnings {
        eprintln!("gyro-doctor: {}", warning.text);
    }
    if !cli.force {
        gyro_doctor::telemetry::already_repaired_test(&inputs[0])?;
    }

    let total_bytes = {
        use std::sync::atomic::{AtomicU64, Ordering};
        let last_tenth = AtomicU64::new(u64::MAX);
        let progress = |percent: f64| {
            if cli.verbose {
                let tenth = (percent * 10.0).floor() as u64;
                if last_tenth.swap(tenth, Ordering::Relaxed) != tenth {
                    eprint!("\rmerge {:.0}%", percent * 100.0);
                    let _ = std::io::Write::flush(&mut std::io::stderr());
                }
            }
        };
        match gyro_doctor::merge::merge(&inputs, &output, cli.force, progress) {
            Ok(total) => total,
            Err(error) => {
                let _ = std::fs::remove_file(&output);
                return Err(error);
            }
        }
    };
    if cli.verbose {
        eprintln!();
    }

    if cli.merge_only {
        if cli.report {
            print_lines(&[
                "merge stage".to_string(),
                format!("  input files                 {}", inputs.len()),
                format!("  total input size            {:.2} GB", total_bytes as f64 / 1e9),
                format!("  merged video                {}", output.display()),
            ]);
        } else if cli.verbose {
            println!(
                "merge stage: {} files, {:.2} GB, wrote {}",
                inputs.len(),
                total_bytes as f64 / 1e9,
                output.display()
            );
        }
        return Ok(());
    }

    let result = (|| -> Result<()> {
        let decode_started = std::time::Instant::now();
        let decode_result = decode::decode(&output, cli.force)?;
        if cli.report {
            print_lines(&[
                "merge stage".to_string(),
                format!("  input files                 {}", inputs.len()),
                format!("  total input size            {:.2} GB", total_bytes as f64 / 1e9),
                format!("  merged video                {}", output.display()),
            ]);
            print_lines(&report::decode_report(&decode_result, None, None));
        } else if cli.verbose {
            println!(
                "merge stage: {} files, {:.2} GB, wrote {}",
                inputs.len(),
                total_bytes as f64 / 1e9,
                output.display()
            );
            println!("{}", report::decode_report_line(&decode_result));
        }
        let _ = decode_started;

        let detection_result = detect::detect(
            &decode_result,
            &detect_axes,
            &detection_parameters,
        )?;
        if cli.report {
            let mut lines = vec![report::DETECTION_REPORT_HEADING.to_string()];
            for axis in &detect_axes {
                lines.extend(report::axis_report(
                    &detection_result,
                    axis,
                    None,
                    decode_result.video_frame_rate,
                ));
            }
            print_lines(&lines);
        } else if cli.verbose {
            println!("{}", report::detection_report_line(&detection_result));
        }

        let resolution = do_not_repair::resolve(
            &list.elements,
            &detection_result,
            decode_result.video_frame_rate,
            &repair_parameters.remove_gyro_motion,
        );
        repair_parameters.do_not_repair = resolution.excluded;
        for warning in &resolution.warnings {
            eprintln!("gyro-doctor: {}", warning.text);
        }

        let repair_result = repair::repair(
            &decode_result,
            &detection_result,
            &repair_axes,
            &repair_parameters,
        )?;
        if cli.report {
            let record: serde_json::Value =
                serde_json::from_str(&repair_result.repair_record)?;
            print_lines(&report::repair_report(&record));
        } else if cli.verbose {
            let record: serde_json::Value =
                serde_json::from_str(&repair_result.repair_record)?;
            println!("{}", report::repair_report_line(&record));
        }

        let write_report = gyro_doctor::write::write_in_place(&output, &repair_result, cli.force)?;
        if cli.report {
            print_lines(&report::write_report(&output, &output, &write_report));
        } else if cli.verbose {
            println!("{}", report::write_report_line(&output, &write_report));
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&output);
    }
    result
}

fn run(cli: Cli) -> Result<()> {
    let RunOptions {
        detect_axes,
        repair_axes,
        detection_parameters,
        mut repair_parameters,
        list,
    } = options(&cli)?;
    let video = cli.videos[0].clone();
    let output = output_path(&video, cli.output.clone())?;
    let last_stage = if cli.decode_only {
        Stage::Decode
    } else if cli.detect_only {
        Stage::Detection
    } else if cli.repair_only {
        Stage::Repair
    } else {
        Stage::Write
    };
    if cli.no_cache && last_stage != Stage::Write {
        bail!("--no-cache cannot be combined with --decode-only, --detect-only, or --repair-only");
    }
    for warning in &list.warnings {
        eprintln!("gyro-doctor: {}", warning.text);
    }

    if !cli.force {
        gyro_doctor::telemetry::already_repaired_test(&video)?;
    }

    let cache_on = !cli.no_cache;
    let cache_root = match &cli.cache_dir {
        Some(directory) => directory.clone(),
        None => std::env::current_dir()
            .context("cannot read the current directory")?
            .join(".gyro-doctor-cache"),
    };
    let paths = CachePaths::new(&cache_root, &video);

    let mut run_from = Stage::Decode;
    let mut decode_result: Option<DecodeResult> = None;
    let mut detection_result: Option<DetectionResult> = None;
    let mut repair_result: Option<RepairResult> = None;
    let mut extent_tables: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut decode_stamp: Option<FileStamp> = None;
    let mut detection_stamp: Option<FileStamp> = None;
    let mut resolution_warnings: Vec<do_not_repair::Warning> = Vec::new();
    let mut detection_ran = false;
    let print_cached = cli.report || cli.verbose;

    if cache_on {
        match cached_decode(&paths, &video) {
            Ok(Some(result)) => {
                decode_result = Some(result);
                run_from = Stage::Detection;
                if last_stage >= Stage::Detection {
                    match cached_detection(&paths, &video, &detect_axes, &detection_parameters) {
                        Ok(Some(detection)) => {
                            detection_result = Some(detection);
                            run_from = Stage::Repair;
                            if last_stage >= Stage::Repair {
                                let frame_rate = decode_result
                                    .as_ref()
                                    .context("the cached decode result is missing")?
                                    .video_frame_rate;
                                let resolution = do_not_repair::resolve(
                                    &list.elements,
                                    detection_result.as_ref().unwrap(),
                                    frame_rate,
                                    &repair_parameters.remove_gyro_motion,
                                );
                                resolution_warnings = resolution.warnings;
                                repair_parameters.do_not_repair = resolution.excluded;
                                match cached_repair(
                                    &paths,
                                    &video,
                                    &repair_axes,
                                    &repair_parameters,
                                ) {
                                    Ok(Some(repair)) => {
                                        repair_result = Some(repair);
                                        run_from = Stage::Write;
                                    }
                                    Ok(None) => {}
                                    Err(error) => {
                                        eprintln!(
                                            "gyro-doctor: repair stage: {error:#}; running the stage again"
                                        );
                                    }
                                }
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            eprintln!(
                                "gyro-doctor: detection stage: {error:#}; running the stage again"
                            );
                        }
                    }
                }
            }
            Ok(None) => {}
            Err(error) => {
                eprintln!("gyro-doctor: decode stage: {error:#}; running the stage again");
            }
        }
    }

    if cache_on && decode_result.is_some() && decode_stamp.is_none() {
        decode_stamp = Some(gyro_doctor::mp4::file_stamp(&paths.decode())?);
    }
    if cache_on && detection_result.is_some() && detection_stamp.is_none() {
        detection_stamp = Some(gyro_doctor::mp4::file_stamp(&paths.detection())?);
    }

    if run_from >= Stage::Repair && last_stage >= Stage::Repair && !detection_ran {
        for warning in &resolution_warnings {
            eprintln!("gyro-doctor: {}", warning.text);
        }
    }

    let mut decode_path: Option<PathBuf> = None;
    let mut readme_path: Option<PathBuf> = None;
    let mut repair_path: Option<PathBuf> = None;

    if run_from == Stage::Decode {
        let result = decode::decode(&video, cli.force)?;
        decode_result = Some(result);
        if cache_on {
            let (path, readme) =
                cache::write_decode(decode_result.as_ref().unwrap(), &paths)?;
            decode_stamp = Some(gyro_doctor::mp4::file_stamp(&path)?);
            decode_path = Some(path);
            readme_path = Some(readme);
        }
        if cli.report {
            print_lines(&report::decode_report(
                decode_result.as_ref().unwrap(),
                decode_path.as_deref(),
                readme_path.as_deref(),
            ));
        } else if cli.verbose {
            println!(
                "{}",
                report::decode_report_line(decode_result.as_ref().unwrap())
            );
        }
    } else if print_cached {
        println!(
            "{}: result read from the cache: {}",
            report::DECODE_REPORT_HEADING,
            paths.decode().display()
        );
    }

    if last_stage >= Stage::Detection && run_from <= Stage::Detection {
        let result = detect::detect(
            decode_result.as_ref().context("the decode result is missing")?,
            &detect_axes,
            &detection_parameters,
        )?;
        detection_result = Some(result);
        detection_ran = true;
        if cache_on {
            let stamp = decode_stamp
                .clone()
                .context("the decode result was not stamped")?;
            extent_tables = cache::write_detection(
                detection_result.as_ref().unwrap(),
                &paths,
                stamp,
            )?;
            detection_stamp = Some(gyro_doctor::mp4::file_stamp(&paths.detection())?);
        }
        if cli.report {
            let mut lines = vec![report::DETECTION_REPORT_HEADING.to_string()];
            for axis in &detect_axes {
                lines.extend(report::axis_report(
                    detection_result.as_ref().unwrap(),
                    axis,
                    extent_tables.get(axis).map(PathBuf::as_path),
                    decode_result.as_ref().unwrap().video_frame_rate,
                ));
            }
            print_lines(&lines);
        } else if cli.verbose {
            println!(
                "{}",
                report::detection_report_line(detection_result.as_ref().unwrap())
            );
        }
    } else if last_stage >= Stage::Detection && print_cached {
        println!(
            "{}: result read from the cache: {}",
            report::DETECTION_REPORT_HEADING,
            paths.detection().display()
        );
    }

    if last_stage >= Stage::Repair && run_from <= Stage::Repair {
        if detection_ran {
            let resolution = do_not_repair::resolve(
                &list.elements,
                detection_result.as_ref().unwrap(),
                decode_result.as_ref().unwrap().video_frame_rate,
                &repair_parameters.remove_gyro_motion,
            );
            resolution_warnings = resolution.warnings;
            repair_parameters.do_not_repair = resolution.excluded;
            for warning in &resolution_warnings {
                eprintln!("gyro-doctor: {}", warning.text);
            }
        }
        let result = repair::repair(
            decode_result.as_ref().context("the decode result is missing")?,
            detection_result
                .as_ref()
                .context("the detection result is missing")?,
            &repair_axes,
            &repair_parameters,
        )?;
        repair_result = Some(result);
        if cache_on {
            let decode_stamp = decode_stamp
                .clone()
                .context("the decode result was not stamped")?;
            let detection_stamp = detection_stamp
                .clone()
                .context("the detection result was not stamped")?;
            cache::write_repair(
                repair_result.as_ref().unwrap(),
                &paths,
                decode_stamp,
                detection_stamp,
            )?;
            repair_path = Some(paths.repair());
        }
        if cli.report {
            let record: serde_json::Value = serde_json::from_str(
                &repair_result.as_ref().unwrap().repair_record,
            )?;
            print_lines(&report::repair_report(&record));
        } else if cli.verbose {
            let record: serde_json::Value = serde_json::from_str(
                &repair_result.as_ref().unwrap().repair_record,
            )?;
            println!("{}", report::repair_report_line(&record));
        }
    } else if last_stage >= Stage::Repair && print_cached {
        println!(
            "{}: result read from the cache: {}",
            report::REPAIR_REPORT_HEADING,
            paths.repair().display()
        );
    }

    if last_stage == Stage::Write {
        let result = repair_result
            .as_ref()
            .context("the repair result is missing")?;
        let write_report = gyro_doctor::write::write(&video, result, &output, cli.force)?;
        if cache_on {
            let repair_path = repair_path
                .clone()
                .unwrap_or_else(|| paths.repair());
            cache::write_write_record(&paths, &output, &repair_path)?;
        }
        if cli.report {
            print_lines(&report::write_report(&video, &output, &write_report));
        } else if cli.verbose {
            println!(
                "{}",
                report::write_report_line(&output, &write_report)
            );
        }
    }

    if cache_on && cli.report {
        let mut files = vec![paths.decode(), paths.readme()];
        if last_stage >= Stage::Detection {
            files.push(paths.detection());
            files.push(paths.detection_record());
            for axis in &detect_axes {
                files.push(paths.extent_table(axis));
            }
        }
        if last_stage >= Stage::Repair {
            files.push(paths.repair());
            files.push(paths.repair_record());
        }
        if last_stage == Stage::Write {
            files.push(paths.write_record());
        }
        files.retain(|file| file.exists());
        let mut lines = vec![report::FILES_HEADING.to_string()];
        lines.extend(files.iter().map(|file| file.display().to_string()));
        print_lines(&lines);
    }

    Ok(())
}

fn main() {
    let cli = Cli::parse();
    let result = if cli.videos.len() > 1 {
        run_merge(cli)
    } else {
        if cli.merge_only {
            eprintln!("gyro-doctor: --merge-only takes two or more input files");
            std::process::exit(2);
        }
        run(cli)
    };
    if let Err(error) = result {
        eprintln!("gyro-doctor: {error:#}");
        std::process::exit(1);
    }
}
