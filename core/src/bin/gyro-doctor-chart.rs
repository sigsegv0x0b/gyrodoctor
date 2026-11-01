//! `gyro-doctor-chart`: chart the Normalized Rate of the three axes before and after the
//! repair, from the results in the cache.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use clap::Parser;

use gyro_doctor::attenuate;
use gyro_doctor::cache::{self, CachePaths};
use gyro_doctor::decode::DecodeResult;
use gyro_doctor::detect::{self, DetectionResult};
use gyro_doctor::do_not_repair::parse_position;
use gyro_doctor::repair::RepairResult;

const PALETTE: [&str; 2] = ["#e8820e", "#009E73"];
const LOCAL_MEDIAN_COLOR: &str = "#b9a7e0";
const THRESHOLD_COLORS: [&str; 3] = ["#0072B2", "#E69F00", "#009E73"];
const AXES: [&str; 3] = ["pitch", "yaw", "roll"];

#[derive(Debug, Parser)]
#[command(
    name = "gyro-doctor-chart",
    version,
    about = "Charts the rotational rate of the camera on each axis before and after the repair.",
    after_help = "The gyro repair algorithm is based on Gyro Doctor by DredSkyeQuads:\nhttps://github.com/DredSkyeQuads/ds-gyro-doctor\n\nLossless MP4 chunk merging uses mp4-merge, the merge library used by Gyroflow:\nhttps://github.com/gyroflow/gyroflow"
)]
struct Cli {
    /// The video whose cached results the chart draws.
    #[arg(value_name = "VIDEO")]
    video: PathBuf,

    /// The middle of the drawn window: a time or a video frame.
    #[arg(long, value_name = "POSITION", required = true)]
    center: String,

    /// The complete path of the chart (default: the video stem, chart, center video frame, and window in frames).
    #[arg(long, value_name = "PATH")]
    output: Option<PathBuf>,

    /// Draw the original Normalized Rate alone.
    #[arg(long, value_name = "SERIES")]
    series: Option<String>,

    /// The cache, as gyro-doctor takes it.
    #[arg(long, value_name = "PATH")]
    cache_dir: Option<PathBuf>,

    /// The duration of the window: normal, wide, or ultra-wide.
    #[arg(long, default_value = "normal")]
    view: String,

    /// The rate axis of the panels, in degrees per second: a value, three values, or peraxis.
    #[arg(long, value_name = "VALUE or LIST")]
    rate_axis_scale: Option<String>,

    /// Draw the local Robust Standard Deviation divided by the full track Robust Standard Deviation.
    #[arg(long)]
    show_noise_ratio: bool,

    /// Words placed before the title.
    #[arg(long, value_name = "WORDS")]
    title_prefix: Option<String>,

    /// Print what the chart drew.
    #[arg(long)]
    verbose: bool,
}

#[derive(Clone, Copy)]
enum View {
    Normal,
    Wide,
    UltraWide,
}

impl View {
    fn window_video_frames(self, video_frame_rate: f64) -> i64 {
        match self {
            View::Normal => 24,
            View::Wide => 120,
            View::UltraWide => (15.0 * video_frame_rate).round() as i64,
        }
    }

    fn canvas_width(self) -> f64 {
        match self {
            View::Normal => 3000.0,
            View::Wide => 15000.0,
            View::UltraWide => 63000.0,
        }
    }
}

#[derive(Clone, Copy)]
enum RateScale {
    One(f64),
    Three([f64; 3]),
    PerAxis,
}

fn parse_rate_scale(text: Option<&str>) -> Result<Option<RateScale>> {
    let Some(text) = text else {
        return Ok(None);
    };
    if text == "peraxis" {
        return Ok(Some(RateScale::PerAxis));
    }
    let values: Vec<f64> = text
        .split(',')
        .map(|value| value.trim().parse::<f64>())
        .collect::<std::result::Result<_, _>>()
        .with_context(|| "the rate axis scale takes numbers or peraxis")?;
    if !matches!(values.len(), 1 | 3) {
        bail!("the rate axis scale takes one value, three values, or peraxis");
    }
    if values.iter().any(|value| !value.is_finite() || *value <= 0.0) {
        bail!("the rate axis scale takes values above zero");
    }
    Ok(Some(if values.len() == 1 {
        RateScale::One(values[0])
    } else {
        RateScale::Three([values[0], values[1], values[2]])
    }))
}

fn clamp_center(requested: i64, window_frames: i64, video_frames: i64) -> (i64, i64, i64) {
    let window_frames = window_frames.min(video_frames);
    let first = (requested - window_frames / 2)
        .max(0)
        .min(video_frames - window_frames);
    (
        first + window_frames / 2,
        first,
        first + window_frames - 1,
    )
}

fn load_detection(paths: &CachePaths) -> Option<DetectionResult> {
    if !paths.detection().exists() {
        return None;
    }
    cache::read_detection(&paths.detection()).ok()
}

fn load_repair(paths: &CachePaths) -> Option<RepairResult> {
    if !paths.repair().exists() {
        return None;
    }
    cache::read_repair(&paths.repair()).ok()
}

fn largest_absolute_rate(series: &[&Vec<Vec<f64>>], column: usize, first: usize, last: usize) -> f64 {
    let mut values = Vec::new();
    for series in series {
        for rate in &series[first..=last] {
            values.push(rate[column].abs());
        }
    }
    if values.is_empty() {
        return 1.0;
    }
    let mut sorted = values;
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = sorted[sorted.len() / 2];
    let absolute_deviations: Vec<f64> = sorted
        .iter()
        .map(|value| (value - median).abs())
        .collect::<Vec<_>>();
    let mut deviations = absolute_deviations;
    deviations.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rsd = 1.4826 * deviations[deviations.len() / 2];
    let limit = median + 8.0 * rsd;
    sorted
        .iter()
        .copied()
        .filter(|value| *value <= limit)
        .fold(0.0, f64::max)
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn write_svg(
    output: &Path,
    decode: &DecodeResult,
    detection: Option<&DetectionResult>,
    repair: Option<&RepairResult>,
    draw_repaired: bool,
    window_start_s: f64,
    window_end_s: f64,
    first_video_frame: i64,
    last_video_frame: i64,
    requested_video_frame: i64,
    drawn_center: i64,
    canvas_width: f64,
    rate_scale: Option<RateScale>,
    show_ratio: bool,
    title_prefix: Option<&str>,
    video: &Path,
) -> Result<()> {
    let width = canvas_width.max(1000.0);
    let height = 1200.0;
    let left = 90.0;
    let right = 50.0;
    let top = 240.0;
    let bottom = 110.0;
    let panel_gap = 34.0;
    let panel_height = (height - top - bottom - 2.0 * panel_gap) / 3.0;
    let plot_width = width - left - right;
    let span = (window_end_s - window_start_s).max(1e-9);

    let times = &decode.recorded_rate_times_s;
    let first = times.partition_point(|time| *time < window_start_s);
    let last = times.partition_point(|time| *time <= window_end_s);
    let last = last.saturating_sub(1).max(first);
    let original: Vec<Vec<f64>> = decode
        .normalized_rate_deg_s
        .iter()
        .map(|rate| vec![rate[0], rate[1], rate[2]])
        .collect();
    let repaired: Option<Vec<Vec<f64>>> = repair.map(|repair| {
        repair
            .normalized_rate_deg_s
            .iter()
            .map(|rate| vec![rate[0], rate[1], rate[2]])
            .collect()
    });

    let mut halves = [1.0f64; 3];
    match rate_scale {
        Some(RateScale::One(value)) => halves = [value; 3],
        Some(RateScale::Three(values)) => halves = values,
        Some(RateScale::PerAxis) => {
            let mut series: Vec<&Vec<Vec<f64>>> = vec![&original];
            if draw_repaired {
                if let Some(repaired) = &repaired {
                    series.push(repaired);
                }
            }
            for column in 0..3 {
                halves[column] = 1.3 * largest_absolute_rate(&series, column, first, last).max(1e-6);
            }
        }
        None => {
            let mut series: Vec<&Vec<Vec<f64>>> = vec![&original];
            if draw_repaired {
                if let Some(repaired) = &repaired {
                    series.push(repaired);
                }
            }
            let largest = (0..3)
                .map(|column| largest_absolute_rate(&series, column, first, last))
                .fold(0.0f64, f64::max)
                .max(1e-6);
            halves = [1.3 * largest; 3];
        }
    }

    let title = if draw_repaired {
        "Normalized Rate Quaternion Gyro Chart: A=Original, B=Repaired"
    } else {
        "Normalized Rate Quaternion Gyro Chart"
    };
    let title = match title_prefix {
        Some(prefix) => format!("{prefix} {title}"),
        None => title.to_string(),
    };

    let mut svg = String::new();
    writeln!(
        svg,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width:.0}" height="{height:.0}" viewBox="0 0 {width:.0} {height:.0}">"##
    )?;
    writeln!(svg, r##"<rect width="100%" height="100%" fill="#ffffff"/>"##)?;
    writeln!(
        svg,
        r##"<text x="{:.0}" y="46" font-family="DejaVu Sans, sans-serif" font-size="26" font-weight="bold" fill="#222">{}</text>"##,
        left,
        escape(&title)
    )?;
    writeln!(
        svg,
        r##"<text x="{:.0}" y="80" font-family="DejaVu Sans, sans-serif" font-size="15" fill="#444">{}  ·  {:.2} fps  ·  measurement rate {:.4} Hz  ·  period T {:.4} us</text>"##,
        left,
        escape(&video.file_stem().map_or_else(String::new, |stem| stem.to_string_lossy().to_string())),
        decode.video_frame_rate,
        decode.measurement_rate_hz,
        decode.period_s * 1e6
    )?;
    writeln!(
        svg,
        r##"<text x="{:.0}" y="108" font-family="DejaVu Sans, sans-serif" font-size="14" fill="#555">video frames f{}-f{}  ·  requested center f{}  ·  drawn center f{}{}</text>"##,
        left,
        first_video_frame,
        last_video_frame,
        requested_video_frame,
        drawn_center,
        if drawn_center == requested_video_frame {
            String::new()
        } else {
            " (window slid to fit the video)".to_string()
        }
    )?;

    let x_of = |time: f64| left + ((time - window_start_s) / span) * plot_width;
    let sample_step = ((last - first + 1) as f64 / 5000.0).ceil().max(1.0) as usize;

    for (panel, axis) in AXES.iter().enumerate() {
        let panel_top = top + panel as f64 * (panel_height + panel_gap);
        let panel_bottom = panel_top + panel_height;
        let half = halves[panel].max(1e-6);
        let y_of = |value: f64| {
            panel_top + (half - value.clamp(-half, half)) / (2.0 * half) * panel_height
        };

        writeln!(
            svg,
            r##"<rect x="{left:.0}" y="{panel_top:.1}" width="{plot_width:.1}" height="{panel_height:.1}" fill="#fafafa" stroke="#cccccc" stroke-width="1"/>"##
        )?;
        writeln!(
            svg,
            r##"<text x="{:.0}" y="{:.1}" font-family="DejaVu Sans, sans-serif" font-size="16" font-weight="bold" fill="#333">{}  Normalized Rate (deg/s)</text>"##,
            left,
            panel_top - 8.0,
            axis
        )?;

        for tick in 0..=4 {
            let value = half - (tick as f64 / 4.0) * 2.0 * half;
            let y = panel_top + (tick as f64 / 4.0) * panel_height;
            writeln!(
                svg,
                r##"<line x1="{left:.0}" y1="{y:.1}" x2="{:.1}" y2="{y:.1}" stroke="#e2e2e2" stroke-width="0.8"/>"##,
                left + plot_width
            )?;
            writeln!(
                svg,
                r##"<text x="{:.0}" y="{:.1}" text-anchor="end" font-family="DejaVu Sans, sans-serif" font-size="12" fill="#555">{:.3}</text>"##,
                left - 8.0,
                y + 4.0,
                value
            )?;
        }

        // Extents of the classification on this axis.
        if let Some(detection) = detection {
            if let Some(axis_detection) = detection.axes.iter().find(|detection| detection.axis == *axis) {
                for row in &axis_detection.rows {
                    let start = row.start_s.max(window_start_s);
                    let end = row.end_s.min(window_end_s);
                    if end < start {
                        continue;
                    }
                    let fill = match row.classification.as_str() {
                        attenuate::LARGE_AMPLITUDE_OSCILLATION => "#FBDDDD",
                        attenuate::MID_AMPLITUDE_BURST => "#E7DDF4",
                        _ => "#FBF0D5",
                    };
                    let x = x_of(start);
                    let extent_width = (x_of(end) - x).max(1.0);
                    writeln!(
                        svg,
                        r##"<rect x="{x:.1}" y="{panel_top:.1}" width="{extent_width:.1}" height="{panel_height:.1}" fill="{fill}" opacity="0.85"/>"##
                    )?;
                    if extent_width > 36.0 {
                        writeln!(
                            svg,
                            r##"<text x="{:.1}" y="{:.1}" font-family="DejaVu Sans, sans-serif" font-size="10" fill="#6b6b6b">{}</text>"##,
                            x + 3.0,
                            panel_top + 13.0,
                            escape(&detect::extent_identifier(axis, row.peak_video_frame))
                        )?;
                    }
                }
                // Local median.
                let median = &axis_detection.local_median_deg_s;
                let mut path = String::new();
                let mut started = false;
                for index in (first..=last).step_by(sample_step) {
                    let x = x_of(times[index]);
                    let y = y_of(median[index]);
                    if !started {
                        write!(path, "M {x:.1} {y:.1}")?;
                        started = true;
                    } else {
                        write!(path, " L {x:.1} {y:.1}")?;
                    }
                }
                if started {
                    writeln!(
                        svg,
                        r##"<path d="{path}" fill="none" stroke="{LOCAL_MEDIAN_COLOR}" stroke-width="1.2" stroke-dasharray="5 4" opacity="0.7"/>"##
                    )?;
                }
                // Threshold lines of the passes, in Robust Standard Deviations.
                for (pass, color) in THRESHOLD_COLORS.iter().enumerate() {
                    let value = axis_detection.pass_threshold[pass]
                        * axis_detection.pass_robust_standard_deviation_deg_s[pass];
                    if value <= half {
                        for sign in [-1.0, 1.0] {
                            let y = y_of(sign * value);
                            writeln!(
                                svg,
                                r##"<line x1="{left:.0}" y1="{y:.1}" x2="{:.1}" y2="{y:.1}" stroke="{color}" stroke-width="0.8" opacity="0.65"/>"##,
                                left + plot_width
                            )?;
                        }
                    }
                }
                if show_ratio {
                    let full = axis_detection.full_track_robust_standard_deviation_deg_s;
                    if full > 0.0 {
                        let ratio_values: Vec<f64> = (first..=last)
                            .map(|index| axis_detection.local_robust_standard_deviation_deg_s[index] / full)
                            .collect();
                        let limit = ratio_values
                            .iter()
                            .copied()
                            .fold(0.0f64, f64::max)
                            .max(axis_detection.pass_threshold[0])
                            .max(axis_detection.pass_threshold[1])
                            .max(1e-6);
                        let ratio_y = |value: f64| panel_bottom - (value / limit).clamp(0.0, 1.0) * panel_height;
                        let mut path = String::new();
                        let mut started = false;
                        for index in (first..=last).step_by(sample_step) {
                            let x = x_of(times[index]);
                            let y = ratio_y(ratio_values[index - first]);
                            if !started {
                                write!(path, "M {x:.1} {y:.1}")?;
                                started = true;
                            } else {
                                write!(path, " L {x:.1} {y:.1}")?;
                            }
                        }
                        if started {
                            writeln!(
                                svg,
                                r##"<path d="{path}" fill="none" stroke="#7a0f52" stroke-width="1.0" opacity="0.8"/>"##
                            )?;
                        }
                        for (pass, color) in THRESHOLD_COLORS.iter().enumerate().take(2) {
                            let value = axis_detection.pass_threshold[pass];
                            if value <= limit {
                                let y = ratio_y(value);
                                writeln!(
                                    svg,
                                    r##"<line x1="{left:.0}" y1="{y:.1}" x2="{:.1}" y2="{y:.1}" stroke="{color}" stroke-width="0.8" opacity="0.5"/>"##,
                                    left + plot_width
                                )?;
                            }
                        }
                        writeln!(
                            svg,
                            r##"<text x="{:.0}" y="{:.1}" text-anchor="end" font-family="DejaVu Sans, sans-serif" font-size="10" fill="#7a0f52">Local/Full RSD 0-{:.2}</text>"##,
                            left + plot_width,
                            panel_top + 12.0,
                            limit
                        )?;
                    }
                }
            }
        }

        // Series. The repaired series first so the original shows where they differ.
        if draw_repaired {
            if let Some(repaired) = &repaired {
                draw_series(&mut svg, repaired, times, first, last, sample_step, panel, &x_of, &y_of, PALETTE[1])?;
            }
        }
        draw_series(&mut svg, &original, times, first, last, sample_step, panel, &x_of, &y_of, PALETTE[0])?;

        // X ticks: video frames across the window.
        for tick in 0..=6 {
            let frame = first_video_frame
                + ((last_video_frame - first_video_frame) as f64 * tick as f64 / 6.0).round() as i64;
            let time = frame as f64 / decode.video_frame_rate;
            let x = x_of(time);
            writeln!(
                svg,
                r##"<line x1="{x:.1}" y1="{panel_bottom:.1}" x2="{x:.1}" y2="{:.1}" stroke="#c8c8c8" stroke-width="0.8"/>"##,
                panel_bottom + 6.0
            )?;
            writeln!(
                svg,
                r##"<text x="{x:.1}" y="{:.1}" text-anchor="middle" font-family="DejaVu Sans, sans-serif" font-size="12" fill="#555">f{frame}</text>"##,
                panel_bottom + 22.0
            )?;
        }
    }

    // Legend and data box.
    let legend_y = height - 62.0;
    let mut legend_x = left;
    for (label, color) in [
        ("A Normalized Rate (original)", PALETTE[0]),
        ("B Normalized Rate (repaired)", PALETTE[1]),
        ("local median", LOCAL_MEDIAN_COLOR),
        ("large amplitude oscillation", "#FBDDDD"),
        ("mid-amplitude burst", "#E7DDF4"),
        ("impulse event", "#FBF0D5"),
    ] {
        if label.starts_with('B') && !draw_repaired {
            continue;
        }
        writeln!(
            svg,
            r##"<rect x="{legend_x:.1}" y="{:.1}" width="16" height="10" fill="{color}" stroke="#999" stroke-width="0.6"/>"##,
            legend_y - 9.0
        )?;
        writeln!(
            svg,
            r##"<text x="{:.1}" y="{legend_y:.1}" font-family="DejaVu Sans, sans-serif" font-size="12" fill="#444">{}</text>"##,
            legend_x + 22.0,
            escape(label)
        )?;
        legend_x += 30.0 + label.len() as f64 * 6.3;
    }
    writeln!(
        svg,
        r##"<text x="{:.0}" y="150" font-family="DejaVu Sans, sans-serif" font-size="13" fill="#555">window {:.3} s to {:.3} s  ·  {:.0} px  ·  rate axis ±{:.2} deg/s</text>"##,
        left,
        window_start_s,
        window_end_s,
        width,
        halves.iter().copied().fold(0.0, f64::max)
    )?;

    if let Some(detection) = detection {
        if let Some(axis_detection) = detection.axes.first() {
            writeln!(
                svg,
                r##"<text x="{:.0}" y="172" font-family="DejaVu Sans, sans-serif" font-size="12" fill="#666">local median: robust local linear regression  ·  clean RSD pitch/yaw/roll {:.4} / {:.4} / {:.4} deg/s</text>"##,
                left,
                detection.axes.first().map_or(0.0, |axis| axis.clean_robust_standard_deviation_deg_s),
                detection.axes.get(1).map_or(0.0, |axis| axis.clean_robust_standard_deviation_deg_s),
                detection.axes.get(2).map_or(0.0, |axis| axis.clean_robust_standard_deviation_deg_s)
            )?;
            let _ = axis_detection;
        }
    }

    writeln!(svg, "</svg>")?;
    std::fs::write(output, svg)
        .with_context(|| format!("cannot write {}", output.display()))?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn draw_series(
    svg: &mut String,
    series: &[Vec<f64>],
    times: &[f64],
    first: usize,
    last: usize,
    step: usize,
    column: usize,
    x_of: &impl Fn(f64) -> f64,
    y_of: &impl Fn(f64) -> f64,
    color: &str,
) -> Result<()> {
    let mut path = String::new();
    let mut started = false;
    for index in (first..=last).step_by(step) {
        let x = x_of(times[index]);
        let y = y_of(series[index][column]);
        if !started {
            write!(path, "M {x:.1} {y:.1}")?;
            started = true;
        } else {
            write!(path, " L {x:.1} {y:.1}")?;
        }
    }
    if started {
        writeln!(
            svg,
            r##"<path d="{path}" fill="none" stroke="{color}" stroke-width="1.5" stroke-linejoin="round"/>"##
        )?;
    }
    Ok(())
}

fn convert_to_png(svg_path: &Path, output: &Path) -> Result<()> {
    let status = Command::new("convert")
        .arg(svg_path)
        .arg(output)
        .status()
        .with_context(|| "cannot run ImageMagick convert; write an .svg output instead")?;
    if !status.success() {
        bail!("ImageMagick convert failed to write {}", output.display());
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("gyro-doctor-chart: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    if !cli.video.is_file() {
        bail!("the video {} is not a file", cli.video.display());
    }
    if let Some(series) = &cli.series {
        if series != "original" {
            bail!("--series takes original");
        }
    }
    let view = match cli.view.as_str() {
        "normal" => View::Normal,
        "wide" => View::Wide,
        "ultra-wide" => View::UltraWide,
        other => bail!("--view takes normal, wide, or ultra-wide, not {other}"),
    };
    let rate_scale = parse_rate_scale(cli.rate_axis_scale.as_deref())?;
    let cache_root = match &cli.cache_dir {
        Some(directory) => directory.clone(),
        None => std::env::current_dir()?.join(".gyro-doctor-cache"),
    };
    let paths = CachePaths::new(&cache_root, &cli.video);
    let decode = cache::read_decode(&paths.decode(), &cli.video).with_context(|| {
        format!(
            "the decode result is missing or invalid at {}; run gyro-doctor --decode-only {}",
            paths.decode().display(),
            cli.video.display()
        )
    })?;
    let detection = load_detection(&paths);
    if cli.show_noise_ratio && detection.is_none() {
        bail!(
            "--show-noise-ratio requires a detection result at {}",
            paths.detection().display()
        );
    }
    let repair = if cli.series.as_deref() == Some("original") {
        None
    } else {
        load_repair(&paths)
    };
    let draw_repaired = repair.is_some();

    let video_frame_rate = decode.video_frame_rate;
    let video_frame_count = decode.quaternion_count_per_telemetry_frame.len() as i64;
    let window_video_frames = view.window_video_frames(video_frame_rate);
    let requested_video_frame = parse_position(&cli.center)?.to_video_frame(video_frame_rate);
    let (drawn_center, first_video_frame, last_video_frame) =
        clamp_center(requested_video_frame, window_video_frames, video_frame_count);
    let window_start_s = first_video_frame as f64 / video_frame_rate;
    let window_end_s = (last_video_frame + 1) as f64 / video_frame_rate;
    let output = cli.output.unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(format!(
                "{}-chart-f{}-{}f.png",
                cli.video
                    .file_stem()
                    .map_or_else(String::new, |stem| stem.to_string_lossy().to_string()),
                drawn_center,
                last_video_frame - first_video_frame + 1
            ))
    });
    let extension = output
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase());
    if !matches!(extension.as_deref(), Some("png") | Some("svg")) {
        bail!("the chart output must be a .png or .svg file, not {}", output.display());
    }
    let svg_output = if extension.as_deref() == Some("png") {
        let temp = std::env::temp_dir().join(format!(
            "gyro-doctor-chart-{}.svg",
            std::process::id()
        ));
        temp
    } else {
        output.clone()
    };

    let show_ratio = cli.show_noise_ratio
        || detection.as_ref().is_some_and(|detection| {
            detection.axes.iter().any(|axis| {
                axis.rows.iter().any(|row| {
                    row.end_s >= window_start_s
                        && row.start_s <= window_end_s
                        && matches!(
                            row.classification.as_str(),
                            attenuate::LARGE_AMPLITUDE_OSCILLATION | attenuate::MID_AMPLITUDE_BURST
                        )
                })
            })
        });
    write_svg(
        &svg_output,
        &decode,
        detection.as_ref(),
        repair.as_ref(),
        draw_repaired,
        window_start_s,
        window_end_s,
        first_video_frame,
        last_video_frame,
        requested_video_frame,
        drawn_center,
        view.canvas_width(),
        rate_scale,
        show_ratio,
        cli.title_prefix.as_deref(),
        &cli.video,
    )?;
    if extension.as_deref() == Some("png") {
        convert_to_png(&svg_output, &output)?;
        let _ = std::fs::remove_file(&svg_output);
    }

    if cli.verbose {
        println!(
            "  video                 {}  ·  {:.4} fps  ·  {} video frames",
            cli.video.display(),
            video_frame_rate,
            video_frame_count
        );
        println!(
            "  window                f{}-f{}  ·  {:.3} s to {:.3} s  ·  requested center f{}, drawn center f{}",
            first_video_frame,
            last_video_frame,
            window_start_s,
            window_end_s,
            requested_video_frame,
            drawn_center
        );
        println!(
            "  series                {}{}",
            "A Normalized Rate (original)",
            if draw_repaired {
                ", B Normalized Rate (repaired)"
            } else {
                ""
            }
        );
        if let Some(detection) = &detection {
            let total: usize = detection.axes.iter().map(|axis| axis.rows.len()).sum();
            println!("  extents               {total}");
        }
        println!("  wrote {}", output.display());
    }
    Ok(())
}
