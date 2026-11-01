//! The text of the stage reports and of the lines that `--verbose` prints.

use std::path::Path;

use crate::decode::DecodeResult;
use crate::detect::{self, DetectionResult};
use crate::do_not_repair::{format_time, reported_video_frame};
use crate::write::WritebackReport;

pub const DECODE_REPORT_HEADING: &str = "decode stage";
pub const DETECTION_REPORT_HEADING: &str = "detection stage";
pub const REPAIR_REPORT_HEADING: &str = "repair stage";
pub const WRITE_REPORT_HEADING: &str = "write stage";
pub const FILES_HEADING: &str = "files";

pub const LARGE_AMPLITUDE_OSCILLATION: &str = "large amplitude oscillation";
pub const MID_AMPLITUDE_BURST: &str = "mid-amplitude burst";
pub const IMPULSE_EVENT: &str = "impulse event";

const PASSES: [PassReport; 3] = [
    PassReport {
        number: 1,
        name: "oscillation pass",
        classification: LARGE_AMPLITUDE_OSCILLATION,
        measured_against: "full track Robust Standard Deviation",
        threshold_label: "oscillation threshold, local Robust Standard Deviation over that",
        trigger_label: "trigger runs past the oscillation threshold",
    },
    PassReport {
        number: 2,
        name: "burst pass",
        classification: MID_AMPLITUDE_BURST,
        measured_against: "Robust Standard Deviation of the burst pass",
        threshold_label: "burst threshold, local Robust Standard Deviation over that",
        trigger_label: "trigger runs past the burst threshold that begin outside earlier extents",
    },
    PassReport {
        number: 3,
        name: "impulse pass",
        classification: IMPULSE_EVENT,
        measured_against: "Robust Standard Deviation of the impulse pass",
        threshold_label: "impulse threshold, in Robust Standard Deviations",
        trigger_label: "peaks accepted by the impulse event criteria",
    },
];

fn general(value: f64) -> String {
    if value == 0.0 {
        return "0".to_string();
    }
    if !value.is_finite() {
        return if value.is_nan() {
            "nan".to_string()
        } else if value > 0.0 {
            "inf".to_string()
        } else {
            "-inf".to_string()
        };
    }
    let exponent = value.abs().log10().floor() as i32;
    if exponent < -4 || exponent >= 6 {
        let text = format!("{value:.5e}");
        if let Some((mantissa, power)) = text.split_once('e') {
            let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
            return format!("{mantissa}e{power}");
        }
        return text;
    }
    let decimals = (5 - exponent).max(0) as usize;
    let text = format!("{value:.decimals$}");
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text
    }
}

struct PassReport {
    number: usize,
    name: &'static str,
    classification: &'static str,
    measured_against: &'static str,
    threshold_label: &'static str,
    trigger_label: &'static str,
}

pub fn decode_report(
    result: &DecodeResult,
    path: Option<&Path>,
    readme: Option<&Path>,
) -> Vec<String> {
    let mut lines = vec![DECODE_REPORT_HEADING.to_string()];
    let written = match path {
        Some(path) => format!(
            "  wrote {} ({:.1} MB): ",
            path.display(),
            std::fs::metadata(path)
                .map(|metadata| metadata.len())
                .unwrap_or(0) as f64
                / 1e6
        ),
        None => "  ".to_string(),
    };
    lines.push(format!(
        "{written}{} quaternions, {} rate samples, {:.2} Hz, {:.4} quaternions per video frame, {} telemetry frames, period T {:.4} us",
        result.quaternion_times_s.len(),
        result.recorded_rate_times_s.len(),
        result.quaternion_rate_hz,
        result.quaternions_per_video_frame,
        result.quaternion_count_per_telemetry_frame.len(),
        result.period_s * 1e6
    ));
    if let Some(readme) = readme {
        lines.push(format!("  recorded its row in {}", readme.display()));
    }
    lines
}

pub fn decode_report_line(result: &DecodeResult) -> String {
    format!(
        "{DECODE_REPORT_HEADING}: {} quaternions, {} rate samples, {:.2} Hz, period T {:.4} us",
        result.quaternion_times_s.len(),
        result.recorded_rate_times_s.len(),
        result.quaternion_rate_hz,
        result.period_s * 1e6
    )
}

pub fn write_report(
    video: &Path,
    output: &Path,
    report: &WritebackReport,
) -> Vec<String> {
    vec![
        WRITE_REPORT_HEADING.to_string(),
        format!("  input video                 {}", video.display()),
        format!(
            "  size of the input video     {} bytes",
            report.video_size_bytes
        ),
        format!("  repaired video              {}", output.display()),
        format!(
            "  quaternions overwritten     {}",
            report.quaternion_messages_overwritten
        ),
        format!(
            "  telemetry frames rewritten  {}",
            report.telemetry_frames_rewritten
        ),
    ]
}

pub fn write_report_line(output: &Path, report: &WritebackReport) -> String {
    format!(
        "{WRITE_REPORT_HEADING}: wrote {}, {} quaternions overwritten",
        output.display(),
        report.quaternion_messages_overwritten
    )
}

pub fn axis_report(
    result: &DetectionResult,
    axis: &str,
    extent_table_path: Option<&Path>,
    video_frame_rate: f64,
) -> Vec<String> {
    let Some(detection) = result.axes.iter().find(|detection| detection.axis == axis) else {
        return Vec::new();
    };
    let settings = &detection.settings;
    let rate_sample_count = result.rate_sample_times_s.len();
    let mut lines = vec![
        format!("axis {axis}   Normalized Rate"),
        format!(
            "local median: {}, local median window +/-{} ms",
            "robust local linear regression", general(settings.local_median_window)
        ),
        "Robust Standard Deviation of a pass: 1.4826 x median(|residual|) over every".to_string(),
        "       rate sample of the axis that no earlier pass placed inside an extent".to_string(),
        String::new(),
        format!("  {:<72} {:>14}", "rate samples", rate_sample_count),
        format!(
            "  {:<72} {:>14}",
            "noise window, +/- ms",
            general(settings.detect_noise_window)
        ),
        format!(
            "  {:<72} {:>14}",
            "event end level, in Robust Standard Deviations",
            general(settings.detect_event_end_level)
        ),
        format!(
            "  {:<72} {:>14}",
            "event end time, ms",
            general(settings.detect_event_end_time)
        ),
        String::new(),
    ];

    for (index, pass) in PASSES.iter().enumerate() {
        let pass_rsd = detection.pass_robust_standard_deviation_deg_s[index];
        let extent_count = detection.pass_extent_count[index];
        let share = detection.pass_share_of_rate_samples_inside_extents[index];
        lines.push(format!(
            "  pass {}, looking for: {}",
            pass.number, pass.classification
        ));
        lines.push(format!(
            "  {:<72} {:>14.4}",
            format!("  measures against {}, deg/s", pass.measured_against),
            pass_rsd
        ));
        lines.push(format!(
            "  {:<72} {:>14}",
            format!("  {}", pass.threshold_label),
            detection.pass_threshold[index]
        ));
        lines.push(format!(
            "  {:<72} {:>14}",
            format!("  {}", pass.trigger_label),
            detection.pass_trigger_count[index]
        ));
        lines.push(format!(
            "  {:<72} {:>14}",
            "  extents after merging those that overlap or touch", extent_count
        ));
        lines.push(format!(
            "  {:<72} {:>13.4}%",
            "  share of samples inside an extent",
            100.0 * share
        ));
        if extent_count > 0 {
            lines.push(format!(
                "    {:>10} {:>21} {:>45} {:>20}",
                "percentile",
                "peak departure deg/s",
                "peak departure in Robust Standard Deviations",
                "extent duration ms"
            ));
            for (percentile, departure, in_rsd, duration) in [
                detect::REPORTED_PERCENTILES[0],
                detect::REPORTED_PERCENTILES[1],
                detect::REPORTED_PERCENTILES[2],
                detect::REPORTED_PERCENTILES[3],
                detect::REPORTED_PERCENTILES[4],
            ]
            .iter()
            .enumerate()
            .map(|(position, percentile)| {
                (
                    *percentile,
                    detection.pass_peak_departure_percentiles_deg_s[index][position],
                    detection
                        .pass_peak_departure_percentiles_in_robust_standard_deviations[index]
                        [position],
                    detection.pass_extent_duration_percentiles_ms[index][position],
                )
            }) {
                lines.push(format!(
                    "    {:>10.0} {:>21.4} {:>45.4} {:>20.4}",
                    percentile, departure, in_rsd, duration
                ));
            }
        } else {
            lines.push(format!(
                "    the {} found no extent on this axis, so there is no distribution to report",
                pass.name
            ));
        }
        lines.push(String::new());
    }

    lines.push(format!(
        "  {:<72} {:>14.4}",
        "clean Robust Standard Deviation, deg/s",
        detection.clean_robust_standard_deviation_deg_s
    ));
    lines.push(format!(
        "  {:<72} {:>13.4}%",
        "share of samples inside any extent",
        100.0 * detection.share_of_rate_samples_inside_extents
    ));
    lines.push(String::new());

    if !detection.rows.is_empty() {
        lines.push(format!(
            "    {:<10} {:<27} {:<23} start to end video frame",
            "identifier", "classification", "start to end time"
        ));
        for row in &detection.rows {
            let times = format!(
                "{}-{}",
                format_time(row.start_s),
                format_time(row.end_s)
            );
            let frames = format!(
                "f{}-f{}",
                reported_video_frame(row.start_s, video_frame_rate),
                reported_video_frame(row.end_s, video_frame_rate)
            );
            lines.push(format!(
                "    {:<10} {:<27} {:<23} {}",
                detect::extent_identifier(axis, row.peak_video_frame),
                row.classification,
                times,
                frames
            ));
        }
        lines.push(String::new());
    }
    if let Some(path) = extent_table_path {
        lines.push(format!(
            "  extent table  {}  ({} rows, {} samples)",
            path.display(),
            detection.rows.len(),
            rate_sample_count
        ));
        lines.push(String::new());
    }
    lines
}

pub fn detection_report_line(result: &DetectionResult) -> String {
    let axes: Vec<&str> = result.axes.iter().map(|axis| axis.axis.as_str()).collect();
    let extent_counts = result
        .axes
        .iter()
        .map(|axis| axis.rows.len().to_string())
        .collect::<Vec<_>>()
        .join("/");
    let classifications = PASSES
        .iter()
        .enumerate()
        .map(|(index, pass)| {
            format!(
                "{} {}",
                pass.classification,
                result
                    .axes
                    .iter()
                    .map(|axis| axis.pass_extent_count[index].to_string())
                    .collect::<Vec<_>>()
                    .join("/")
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{DETECTION_REPORT_HEADING}: {} extents {extent_counts}, {classifications}",
        axes.join("/")
    )
}

fn removed_axes_of(record: &serde_json::Value) -> Vec<String> {
    let removed: Vec<&str> = record
        .pointer("/run/repair_parameters/remove_gyro_motion")
        .and_then(|value| value.as_array())
        .map(|values| values.iter().filter_map(|value| value.as_str()).collect())
        .unwrap_or_default();
    record
        .get("axes")
        .and_then(|axes| axes.as_object())
        .map(|axes| {
            axes.keys()
                .filter(|axis| removed.contains(&axis.as_str()))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

pub fn repair_report(record: &serde_json::Value) -> Vec<String> {
    let video = &record["video"];
    let mut lines = vec![
        REPAIR_REPORT_HEADING.to_string(),
        format!("video               {}", video["video_name"].as_str().unwrap_or("")),
        format!(
            "video frame rate    {:.4} fps",
            video["video_frame_rate"].as_f64().unwrap_or(0.0)
        ),
        format!(
            "measurement rate    {:.4} Hz  (counted)",
            video["measurement_rate_hz"].as_f64().unwrap_or(0.0)
        ),
    ];
    let removed_axes = removed_axes_of(record);
    if let Some(axes) = record.get("axes").and_then(|axes| axes.as_object()) {
        let mut ordered: Vec<&String> = Vec::new();
        for axis in detect::AXES {
            if axes.contains_key(axis) {
                ordered.push(axes.get_key_value(axis).unwrap().0);
            }
        }
        for (axis, _) in axes {
            if !ordered.iter().any(|name| *name == axis) {
                ordered.push(axis);
            }
        }
        for axis in ordered {
            let block = &axes[axis];
            if removed_axes.contains(axis) {
                lines.extend([
                    String::new(),
                    format!("axis {axis}"),
                    "  gyro motion removed by --remove-gyro-motion".to_string(),
                    format!(
                        "  {:<55} {:.4} deg",
                        "rotation removed, absolute",
                        block["removed_rotation_deg_abs_total"].as_f64().unwrap_or(0.0)
                    ),
                    format!(
                        "  {:<55} {:.4} deg",
                        "rotation removed, net",
                        block["removed_rotation_deg_net"].as_f64().unwrap_or(0.0)
                    ),
                ]);
                continue;
            }
            let summary = &block["extent_summary"];
            let identity = &block["identity_outside_extents"];
            let tests = &block["ceiling_attenuator_tests"];
            lines.extend([
                String::new(),
                format!("axis {axis}"),
                format!(
                    "  {:<55} {} ({} scaled, {} skipped, {} excluded)",
                    "detected extents in",
                    block["detected_extents_in"].as_u64().unwrap_or(0),
                    summary["scaled_count"].as_u64().unwrap_or(0),
                    summary["skipped_count"].as_u64().unwrap_or(0),
                    summary["excluded_count"].as_u64().unwrap_or(0)
                ),
                match summary["duration_ms_min"].as_f64() {
                    Some(minimum) => format!(
                        "  {:<55} {:.2} / {:.2} / {:.2}",
                        "duration ms min/median/max",
                        minimum,
                        summary["duration_ms_median"].as_f64().unwrap_or(0.0),
                        summary["duration_ms_max"].as_f64().unwrap_or(0.0)
                    ),
                    None => format!("  {:<55} none", "duration ms min/median/max"),
                },
                match summary["gain_min"].as_f64() {
                    Some(minimum) => format!(
                        "  {:<55} {:.4} / {:.4} / {:.4}",
                        "gain min/median/max",
                        minimum,
                        summary["gain_median"].as_f64().unwrap_or(0.0),
                        summary["gain_max"].as_f64().unwrap_or(0.0)
                    ),
                    None => format!("  {:<55} none", "gain min/median/max"),
                },
                format!(
                    "  {:<55} {}",
                    "extents left at a gain of one",
                    summary["passed_through_count"].as_u64().unwrap_or(0)
                ),
                format!(
                    "  {:<55} {:.4} deg",
                    "rotation removed, absolute",
                    summary["removed_rotation_deg_abs_total"].as_f64().unwrap_or(0.0)
                ),
                format!(
                    "  {:<55} {:.4} deg",
                    "rotation removed, net",
                    summary["removed_rotation_deg_net"].as_f64().unwrap_or(0.0)
                ),
                format!(
                    "  {:<55} {}",
                    "samples inside extents",
                    identity["rate_samples_inside_extents"].as_u64().unwrap_or(0)
                ),
                format!(
                    "  {:<55} {}",
                    "samples changed",
                    identity["rate_samples_changed"].as_u64().unwrap_or(0)
                ),
                match summary["peak_in_clean_robust_standard_deviations_before_min"].as_f64() {
                    Some(minimum) => format!(
                        "  {:<55} {:.2} / {:.2} / {:.2}",
                        "peak in clean Robust Standard Deviations min/median/max",
                        minimum,
                        summary["peak_in_clean_robust_standard_deviations_before_median"]
                            .as_f64()
                            .unwrap_or(0.0),
                        summary["peak_in_clean_robust_standard_deviations_before_max"]
                            .as_f64()
                            .unwrap_or(0.0)
                    ),
                    None => format!(
                        "  {:<55} none",
                        "peak in clean Robust Standard Deviations min/median/max"
                    ),
                },
                format!(
                    "  {:<55} {}",
                    "over ceiling anywhere in the extent",
                    tests["extents_over_ceiling_anywhere_in_the_extent"]
                        .as_u64()
                        .unwrap_or(0)
                ),
                format!(
                    "  {:<55} {}",
                    "extents with shortened fades",
                    tests["extents_with_shortened_fades"].as_u64().unwrap_or(0)
                ),
            ]);
            if let Some(extents) = block["extents"].as_array() {
                if !extents.is_empty() {
                    lines.push(format!("    {:<10} gain", "identifier"));
                    for extent in extents {
                        let value = if extent["scaled"].as_bool() == Some(true) {
                            format!("{:.4}", extent["gain"].as_f64().unwrap_or(0.0))
                        } else if extent["reason"].as_str() == Some("at a video edge") {
                            "skipped at a video edge".to_string()
                        } else {
                            "excluded".to_string()
                        };
                        lines.push(format!(
                            "    {:<10} {}",
                            extent["identifier"].as_str().unwrap_or(""),
                            value
                        ));
                    }
                }
            }
        }
    }
    lines
}

pub fn repair_report_line(record: &serde_json::Value) -> String {
    let removed_axes = removed_axes_of(record);
    let mut claimed: Vec<&String> = Vec::new();
    let mut counted: Vec<&String> = Vec::new();
    if let Some(axes) = record.get("axes").and_then(|axes| axes.as_object()) {
        for axis in axes.keys() {
            if removed_axes.contains(axis) {
                claimed.push(axis);
            } else {
                counted.push(axis);
            }
        }
    }
    let counted = detect::AXES
        .iter()
        .filter(|axis| counted.iter().any(|counted| counted.as_str() == **axis))
        .collect::<Vec<_>>();
    let removed: Vec<&str> = detect::AXES
        .iter()
        .copied()
        .filter(|axis| removed_axes.iter().any(|removed| removed == axis))
        .collect();
    let mut parts = Vec::new();
    if !counted.is_empty() {
        let summaries: Vec<&serde_json::Value> = counted
            .iter()
            .map(|axis| &record["axes"][**axis]["extent_summary"])
            .collect();
        let attenuated = summaries
            .iter()
            .map(|summary| {
                summary["scaled_count"].as_u64().unwrap_or(0)
                    - summary["passed_through_count"].as_u64().unwrap_or(0)
            })
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join("/");
        let at_one = summaries
            .iter()
            .map(|summary| summary["passed_through_count"].as_u64().unwrap_or(0).to_string())
            .collect::<Vec<_>>()
            .join("/");
        let skipped = summaries
            .iter()
            .map(|summary| summary["skipped_count"].as_u64().unwrap_or(0).to_string())
            .collect::<Vec<_>>()
            .join("/");
        let excluded = summaries
            .iter()
            .map(|summary| summary["excluded_count"].as_u64().unwrap_or(0).to_string())
            .collect::<Vec<_>>()
            .join("/");
        parts.push(format!(
            "{} attenuated {attenuated}, at a gain of one {at_one}, skipped at a video edge {skipped}, excluded {excluded}",
            counted
                .iter()
                .map(|axis| axis.to_string())
                .collect::<Vec<_>>()
                .join("/")
        ));
    }
    if !removed.is_empty() {
        parts.push(format!("gyro motion removed from {}", removed.join("/")));
    }
    format!("{REPAIR_REPORT_HEADING}: {}", parts.join("; "))
}
