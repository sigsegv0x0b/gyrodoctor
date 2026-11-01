//! The detection stage: three passes over each selected axis of the Normalized
//! Rate, the extents they find, and the quantities the reports print.

use std::cmp::Ordering;

use anyhow::{bail, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::decode::{self, DecodeResult};
use crate::fit::{self, MAD_TO_SIGMA};

pub const LARGE_AMPLITUDE_OSCILLATION: &str = "large amplitude oscillation";
pub const MID_AMPLITUDE_BURST: &str = "mid-amplitude burst";
pub const IMPULSE_EVENT: &str = "impulse event";
pub const AXES: [&str; 3] = ["pitch", "yaw", "roll"];
pub const REPORTED_PERCENTILES: [f64; 5] = [50.0, 75.0, 90.0, 99.0, 100.0];
pub const LOCAL_MEDIAN_FITS: usize = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectionParameters {
    pub detect_oscillation_threshold: Vec<f64>,
    pub detect_burst_threshold: Vec<f64>,
    pub detect_impulse_threshold: Vec<f64>,
    pub local_median_window: f64,
    pub detect_noise_window: f64,
    pub detect_event_end_level: f64,
    pub detect_event_end_time: f64,
    pub detect_impulse_min_width: f64,
    pub detect_impulse_max_width: f64,
    pub detect_impulse_shoulder_drop: f64,
    pub detect_impulse_shoulder_time: f64,
    pub detect_impulse_side_end_time: f64,
    pub detect_impulse_widen: f64,
    pub detect_impulse_widen_max: f64,
}

impl Default for DetectionParameters {
    fn default() -> Self {
        Self {
            detect_oscillation_threshold: vec![100.0],
            detect_burst_threshold: vec![10.0],
            detect_impulse_threshold: vec![4.5],
            local_median_window: 100.0,
            detect_noise_window: 100.0,
            detect_event_end_level: 2.0,
            detect_event_end_time: 10.0,
            detect_impulse_min_width: 3.0,
            detect_impulse_max_width: 25.0,
            detect_impulse_shoulder_drop: 1.0,
            detect_impulse_shoulder_time: 12.0,
            detect_impulse_side_end_time: 5.0,
            detect_impulse_widen: 5.0,
            detect_impulse_widen_max: 50.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AxisSettings {
    pub detect_oscillation_threshold: f64,
    pub detect_burst_threshold: f64,
    pub detect_impulse_threshold: f64,
    pub local_median_window: f64,
    pub detect_noise_window: f64,
    pub detect_event_end_level: f64,
    pub detect_event_end_time: f64,
    pub detect_impulse_min_width: f64,
    pub detect_impulse_max_width: f64,
    pub detect_impulse_shoulder_drop: f64,
    pub detect_impulse_shoulder_time: f64,
    pub detect_impulse_side_end_time: f64,
    pub detect_impulse_widen: f64,
    pub detect_impulse_widen_max: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtentRow {
    pub first_sample: usize,
    pub last_sample: usize,
    pub rate_sample_count: usize,
    pub start_s: f64,
    pub end_s: f64,
    pub duration_ms: f64,
    pub found_by: String,
    pub classification: String,
    pub peak_sample: usize,
    pub peak_video_frame: f64,
    pub peak_rate_deg_s: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AxisDetection {
    pub axis: String,
    pub settings: AxisSettings,
    pub local_median_deg_s: Vec<f64>,
    pub local_robust_standard_deviation_deg_s: Vec<f64>,
    pub full_track_robust_standard_deviation_deg_s: f64,
    pub clean_robust_standard_deviation_deg_s: f64,
    pub pass_robust_standard_deviation_deg_s: [f64; 3],
    pub pass_threshold: [f64; 3],
    pub pass_extent_count: [usize; 3],
    pub pass_trigger_count: [usize; 3],
    pub pass_share_of_rate_samples_inside_extents: [f64; 3],
    pub share_of_rate_samples_inside_extents: f64,
    pub pass_peak_departure_percentiles_deg_s: [[f64; 5]; 3],
    pub pass_peak_departure_percentiles_in_robust_standard_deviations: [[f64; 5]; 3],
    pub pass_extent_duration_percentiles_ms: [[f64; 5]; 3],
    pub rows: Vec<ExtentRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectionResult {
    pub video_name: String,
    pub video_size_bytes: u64,
    pub video_mtime_s: f64,
    pub derivation_version: u32,
    pub axes: Vec<AxisDetection>,
    pub rate_sample_times_s: Vec<f64>,
    pub rate_sample_video_frames: Vec<f64>,
}

#[derive(Clone, Copy)]
pub struct Pass {
    name: &'static str,
    classification: &'static str,
    threshold_parameter: ThresholdParameter,
    impulse: bool,
}

#[derive(Clone, Copy)]
enum ThresholdParameter {
    Oscillation,
    Burst,
    Impulse,
}

pub const PASSES: [Pass; 3] = [
    Pass {
        name: "oscillation pass",
        classification: LARGE_AMPLITUDE_OSCILLATION,
        threshold_parameter: ThresholdParameter::Oscillation,
        impulse: false,
    },
    Pass {
        name: "burst pass",
        classification: MID_AMPLITUDE_BURST,
        threshold_parameter: ThresholdParameter::Burst,
        impulse: false,
    },
    Pass {
        name: "impulse pass",
        classification: IMPULSE_EVENT,
        threshold_parameter: ThresholdParameter::Impulse,
        impulse: true,
    },
];

impl Pass {
    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn classification(&self) -> &'static str {
        self.classification
    }
}

pub fn axis_column(axis: &str) -> Result<usize> {
    match axis {
        "pitch" => Ok(0),
        "yaw" => Ok(1),
        "roll" => Ok(2),
        _ => bail!("unknown axis {axis:?}; expected one of {AXES:?}"),
    }
}

pub fn selected_axes(axes: &[String]) -> Result<Vec<String>> {
    let mut seen = Vec::new();
    for axis in axes {
        axis_column(axis)?;
        if seen.contains(axis) {
            bail!("the detection stage takes distinct axes, not {axis} twice");
        }
        seen.push(axis.clone());
    }
    if seen.is_empty() {
        bail!("the detection stage takes one or more axes");
    }
    Ok(AXES
        .iter()
        .filter(|axis| seen.iter().any(|seen| seen == *axis))
        .map(|axis| (*axis).to_string())
        .collect())
}

pub fn threshold_on_axis(values: &[f64], axis: &str, parameter: &str) -> Result<f64> {
    if values.len() == 1 {
        return Ok(values[0]);
    }
    if values.len() != AXES.len() {
        bail!(
            "{parameter} takes one value for all three axes or three in pitch, yaw, roll order, not {}",
            values.len()
        );
    }
    Ok(values[axis_column(axis)?])
}

pub fn axis_settings(parameters: &DetectionParameters, axis: &str) -> Result<AxisSettings> {
    Ok(AxisSettings {
        detect_oscillation_threshold: threshold_on_axis(
            &parameters.detect_oscillation_threshold,
            axis,
            "detect_oscillation_threshold",
        )?,
        detect_burst_threshold: threshold_on_axis(
            &parameters.detect_burst_threshold,
            axis,
            "detect_burst_threshold",
        )?,
        detect_impulse_threshold: threshold_on_axis(
            &parameters.detect_impulse_threshold,
            axis,
            "detect_impulse_threshold",
        )?,
        local_median_window: parameters.local_median_window,
        detect_noise_window: parameters.detect_noise_window,
        detect_event_end_level: parameters.detect_event_end_level,
        detect_event_end_time: parameters.detect_event_end_time,
        detect_impulse_min_width: parameters.detect_impulse_min_width,
        detect_impulse_max_width: parameters.detect_impulse_max_width,
        detect_impulse_shoulder_drop: parameters.detect_impulse_shoulder_drop,
        detect_impulse_shoulder_time: parameters.detect_impulse_shoulder_time,
        detect_impulse_side_end_time: parameters.detect_impulse_side_end_time,
        detect_impulse_widen: parameters.detect_impulse_widen,
        detect_impulse_widen_max: parameters.detect_impulse_widen_max,
    })
}

fn settings_threshold(settings: &AxisSettings, parameter: ThresholdParameter) -> f64 {
    match parameter {
        ThresholdParameter::Oscillation => settings.detect_oscillation_threshold,
        ThresholdParameter::Burst => settings.detect_burst_threshold,
        ThresholdParameter::Impulse => settings.detect_impulse_threshold,
    }
}

pub fn runs_of(mask: &[bool]) -> (Vec<usize>, Vec<usize>) {
    let mut starts = Vec::new();
    let mut ends = Vec::new();
    if mask.is_empty() {
        return (starts, ends);
    }
    let mut in_run = false;
    for (index, value) in mask.iter().enumerate() {
        if *value && !in_run {
            starts.push(index);
            in_run = true;
        } else if !*value && in_run {
            ends.push(index - 1);
            in_run = false;
        }
    }
    if in_run {
        ends.push(mask.len() - 1);
    }
    (starts, ends)
}

fn merge_overlapping(mut extents: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    extents.sort_by_key(|(first, _)| *first);
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (first, last) in extents {
        if let Some(previous) = merged.last_mut() {
            if first <= previous.1 {
                previous.1 = previous.1.max(last);
                continue;
            }
        }
        merged.push((first, last));
    }
    merged
}

fn merge_touching(mut extents: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    extents.sort_by_key(|(first, _)| *first);
    let mut joined: Vec<(usize, usize)> = Vec::new();
    for (first, last) in extents {
        if let Some(previous) = joined.last_mut() {
            if first <= previous.1 + 1 {
                previous.1 = previous.1.max(last);
                continue;
            }
        }
        joined.push((first, last));
    }
    joined
}

fn percentiles(values: &[f64]) -> [f64; 5] {
    if values.is_empty() {
        return [f64::NAN; 5];
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let n = sorted.len();
    let mut result = [0.0; 5];
    for (index, percentile) in REPORTED_PERCENTILES.iter().enumerate() {
        let rank = (n - 1) as f64 * percentile / 100.0;
        let lower = rank.floor() as usize;
        let upper = rank.ceil() as usize;
        result[index] = if lower == upper {
            sorted[lower]
        } else {
            sorted[lower] + (sorted[upper] - sorted[lower]) * (rank - lower as f64)
        };
    }
    result
}

fn robust_standard_deviation_outside_extents(residual: &[f64], claimed: &[bool]) -> f64 {
    let mut absolute: Vec<f64> = residual
        .iter()
        .zip(claimed.iter())
        .filter_map(|(value, claimed)| (!*claimed).then_some(value.abs()))
        .collect();
    MAD_TO_SIGMA * fit::median(&mut absolute)
}

pub fn local_robust_standard_deviations(
    residual: &[f64],
    times: &[f64],
    noise_window_ms: f64,
) -> Vec<f64> {
    let noise_window_s = noise_window_ms * 1e-3;
    let absolute: Vec<f64> = residual.iter().map(|value| value.abs()).collect();
    let mut result = vec![0.0; residual.len()];
    let mut gathered = Vec::new();
    for i in 0..residual.len() {
        let start = times.partition_point(|&time| time < times[i] - noise_window_s);
        let end = times.partition_point(|&time| time <= times[i] + noise_window_s);
        gathered.clear();
        gathered.extend_from_slice(&absolute[start..end]);
        result[i] = MAD_TO_SIGMA * fit::median(&mut gathered);
    }
    result
}

fn runs_within_event_end_level(
    residual: &[f64],
    times: &[f64],
    pass_rsd: f64,
    event_end_level: f64,
    event_end_time_ms: f64,
) -> (Vec<usize>, Vec<usize>) {
    let inside: Vec<bool> = residual
        .iter()
        .map(|value| value.abs() <= event_end_level * pass_rsd)
        .collect();
    let (starts, ends) = runs_of(&inside);
    let mut kept_starts = Vec::new();
    let mut kept_ends = Vec::new();
    for (start, end) in starts.into_iter().zip(ends) {
        if (times[end] - times[start]) * 1e3 >= event_end_time_ms {
            kept_starts.push(start);
            kept_ends.push(end);
        }
    }
    (kept_starts, kept_ends)
}

fn ratio_pass_extents(
    ratio: &[f64],
    residual: &[f64],
    times: &[f64],
    threshold: f64,
    pass_rsd: f64,
    event_end_level: f64,
    event_end_time_ms: f64,
) -> (Vec<(usize, usize)>, Vec<usize>) {
    let past: Vec<bool> = ratio.iter().map(|value| *value > threshold).collect();
    let (trigger_first, trigger_last) = runs_of(&past);
    if trigger_first.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let (quiet_first, quiet_last) = runs_within_event_end_level(
        residual,
        times,
        pass_rsd,
        event_end_level,
        event_end_time_ms,
    );
    let mut start = Vec::with_capacity(trigger_first.len());
    let mut end = Vec::with_capacity(trigger_first.len());
    for (&first, &last) in trigger_first.iter().zip(trigger_last.iter()) {
        let before = quiet_last.iter().filter(|&&quiet| quiet <= first).last();
        start.push(before.copied().unwrap_or(0));
        let after = quiet_first.iter().find(|&&quiet| quiet >= last);
        end.push(after.copied().unwrap_or(residual.len() - 1));
    }
    let merged = merge_touching(merge_overlapping(
        start.into_iter().zip(end).collect(),
    ));
    (merged, trigger_first)
}

fn without_claimed_rate_samples(
    merged: Vec<(usize, usize)>,
    claimed: &[bool],
) -> Vec<(usize, usize)> {
    let mut kept = Vec::new();
    for (first, last) in merged {
        let span: Vec<usize> = (first..=last).collect();
        let free: Vec<bool> = span.iter().map(|&index| !claimed[index]).collect();
        let (runs_first, runs_last) = runs_of(&free);
        for (first_free, last_free) in runs_first.into_iter().zip(runs_last) {
            kept.push((span[first_free], span[last_free]));
        }
    }
    kept.sort_by_key(|(first, _)| *first);
    merge_touching(kept)
}

fn impulse_event_candidates(scaled_residual: &[f64], threshold: f64) -> Vec<usize> {
    let beyond: Vec<bool> = scaled_residual
        .iter()
        .map(|value| value.abs() > threshold)
        .collect();
    let (starts, ends) = runs_of(&beyond);
    starts
        .into_iter()
        .zip(ends)
        .map(|(start, end)| {
            let mut best = start;
            let mut best_value = scaled_residual[start].abs();
            for (index, value) in scaled_residual
                .iter()
                .enumerate()
                .take(end + 1)
                .skip(start + 1)
            {
                if value.abs() > best_value {
                    best = index;
                    best_value = value.abs();
                }
            }
            best
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn follow_side_to_shoulder(
    residual: &[f64],
    boundaries: &[f64],
    peak: usize,
    direction: isize,
    pass_rsd: f64,
    shoulder_drop: f64,
    shoulder_time_ms: f64,
    event_end_level: f64,
    side_end_time_ms: f64,
) -> usize {
    let n = residual.len();
    let sign = if residual[peak] < 0.0 { 1.0 } else { -1.0 };
    let shoulder_time_s = shoulder_time_ms * 1e-3;
    let side_end_time_s = side_end_time_ms * 1e-3;
    let mut farthest_residual = residual[peak];
    let mut farthest = peak;
    let mut within_since: Option<usize> = None;
    let mut i = peak as isize;
    loop {
        i += direction;
        if i < 0 || i >= n as isize {
            return farthest;
        }
        let index = i as usize;
        if (residual[index] / pass_rsd).abs() <= event_end_level {
            if within_since.is_none() {
                within_since = Some(index);
            } else if (boundaries[index] - boundaries[within_since.unwrap()]).abs()
                >= side_end_time_s
            {
                return farthest;
            }
        } else {
            within_since = None;
        }
        if sign * (residual[index] - farthest_residual) > 0.0 {
            farthest_residual = residual[index];
            farthest = index;
            continue;
        }
        let retrace = sign * (farthest_residual - residual[index]);
        let elapsed = (boundaries[index] - boundaries[farthest]).abs();
        if elapsed <= shoulder_time_s {
            if retrace >= shoulder_drop * pass_rsd {
                return farthest;
            }
        } else {
            farthest_residual = residual[index];
            farthest = index;
        }
    }
}

fn widen_impulse_extents(
    first: &[usize],
    last: &[usize],
    scaled_residual: &[f64],
    boundaries: &[f64],
    event_end_level: f64,
    widening_step_ms: f64,
    maximum_widening_ms: f64,
) -> (Vec<usize>, Vec<usize>) {
    let n = scaled_residual.len();
    let mut start = vec![0usize; first.len()];
    let mut end = vec![0usize; first.len()];
    for k in 0..first.len() {
        for (is_start, edge_time, sign) in [
            (true, boundaries[first[k]], -1.0),
            (false, boundaries[last[k] + 1], 1.0),
        ] {
            let mut widened = 0.0;
            let mut outermost = if is_start { first[k] } else { last[k] };
            while widened < maximum_widening_ms - 1e-9 {
                widened = (widened + widening_step_ms).min(maximum_widening_ms);
                let target = edge_time + sign * widened * 1e-3;
                let position = boundaries.partition_point(|&value| value <= target);
                outermost = position.saturating_sub(1).min(n - 1);
                if scaled_residual[outermost].abs() <= event_end_level {
                    break;
                }
            }
            if is_start {
                start[k] = outermost;
            } else {
                end[k] = outermost;
            }
        }
    }
    (start, end)
}

fn peak_sample(first: usize, last: usize, scaled_residual: &[f64]) -> usize {
    let mut best = first;
    let mut best_value = scaled_residual[first].abs();
    for (index, value) in scaled_residual
        .iter()
        .enumerate()
        .take(last + 1)
        .skip(first + 1)
    {
        if value.abs() > best_value {
            best = index;
            best_value = value.abs();
        }
    }
    best
}

fn extent_rows(
    merged: &[(usize, usize)],
    boundaries: &[f64],
    scaled_residual: &[f64],
    video_frames: &[f64],
    rate: &[f64],
    pass: Pass,
) -> Vec<ExtentRow> {
    merged
        .iter()
        .map(|&(first, last)| {
            let start_s = boundaries[first];
            let end_s = boundaries[last + 1];
            let peak = peak_sample(first, last, scaled_residual);
            ExtentRow {
                first_sample: first,
                last_sample: last,
                rate_sample_count: last - first + 1,
                start_s,
                end_s,
                duration_ms: (end_s - start_s) * 1e3,
                found_by: pass.name.to_string(),
                classification: pass.classification.to_string(),
                peak_sample: peak,
                peak_video_frame: video_frames[peak],
                peak_rate_deg_s: rate[peak],
            }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn run_ratio_pass(
    residual: &[f64],
    times: &[f64],
    boundaries: &[f64],
    video_frames: &[f64],
    rate: &[f64],
    local_rsd: &[f64],
    pass_rsd: f64,
    claimed: &[bool],
    threshold: f64,
    parameters: &DetectionParameters,
    pass: Pass,
) -> (Vec<ExtentRow>, Vec<usize>) {
    let ratio: Vec<f64> = local_rsd.iter().map(|value| value / pass_rsd).collect();
    let (merged, mut triggers) = ratio_pass_extents(
        &ratio,
        residual,
        times,
        threshold,
        pass_rsd,
        parameters.detect_event_end_level,
        parameters.detect_event_end_time,
    );
    triggers.retain(|&trigger| !claimed[trigger]);
    let merged = without_claimed_rate_samples(merged, claimed);
    let scaled_residual: Vec<f64> = residual.iter().map(|value| value / pass_rsd).collect();
    (
        extent_rows(&merged, boundaries, &scaled_residual, video_frames, rate, pass),
        triggers,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_impulse_pass(
    residual: &[f64],
    boundaries: &[f64],
    video_frames: &[f64],
    rate: &[f64],
    pass_rsd: f64,
    claimed: &[bool],
    threshold: f64,
    parameters: &DetectionParameters,
    pass: Pass,
) -> (Vec<ExtentRow>, Vec<usize>) {
    let scaled_residual: Vec<f64> = residual.iter().map(|value| value / pass_rsd).collect();
    if !scaled_residual.iter().any(|value| value.abs() > threshold) {
        return (Vec::new(), Vec::new());
    }
    let mut peaks = impulse_event_candidates(&scaled_residual, threshold);
    let left: Vec<usize> = peaks
        .iter()
        .map(|&peak| {
            follow_side_to_shoulder(
                residual,
                boundaries,
                peak,
                -1,
                pass_rsd,
                parameters.detect_impulse_shoulder_drop,
                parameters.detect_impulse_shoulder_time,
                parameters.detect_event_end_level,
                parameters.detect_impulse_side_end_time,
            )
        })
        .collect();
    let right: Vec<usize> = peaks
        .iter()
        .map(|&peak| {
            follow_side_to_shoulder(
                residual,
                boundaries,
                peak,
                1,
                pass_rsd,
                parameters.detect_impulse_shoulder_drop,
                parameters.detect_impulse_shoulder_time,
                parameters.detect_event_end_level,
                parameters.detect_impulse_side_end_time,
            )
        })
        .collect();

    let mut seen = std::collections::HashSet::new();
    let mut unique_indices = Vec::new();
    for index in 0..peaks.len() {
        if seen.insert((left[index], right[index])) {
            unique_indices.push(index);
        }
    }
    unique_indices.sort_unstable();
    peaks = unique_indices.iter().map(|&index| peaks[index]).collect();
    let left: Vec<usize> = unique_indices.iter().map(|&index| left[index]).collect();
    let right: Vec<usize> = unique_indices.iter().map(|&index| right[index]).collect();

    let shoulder_to_shoulder_ms: Vec<f64> = left
        .iter()
        .zip(right.iter())
        .map(|(&left, &right)| (boundaries[right + 1] - boundaries[left]) * 1e3)
        .collect();
    let outside: Vec<bool> = scaled_residual
        .iter()
        .map(|value| value.abs() > parameters.detect_event_end_level)
        .collect();
    let mut departure_ms = vec![0.0; peaks.len()];
    for (k, &peak) in peaks.iter().enumerate() {
        let mut departure_first = peak;
        let mut departure_last = peak;
        while departure_first > 0 && outside[departure_first - 1] {
            departure_first -= 1;
        }
        while departure_last + 1 < residual.len() && outside[departure_last + 1] {
            departure_last += 1;
        }
        departure_ms[k] = (boundaries[departure_last + 1] - boundaries[departure_first]) * 1e3;
    }

    let mut accepted_peaks = Vec::new();
    let mut accepted_left = Vec::new();
    let mut accepted_right = Vec::new();
    for k in 0..peaks.len() {
        if departure_ms[k] >= parameters.detect_impulse_min_width
            && shoulder_to_shoulder_ms[k] <= parameters.detect_impulse_max_width
            && !claimed[peaks[k]]
        {
            accepted_peaks.push(peaks[k]);
            accepted_left.push(left[k]);
            accepted_right.push(right[k]);
        }
    }
    if accepted_peaks.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let (start, end) = widen_impulse_extents(
        &accepted_left,
        &accepted_right,
        &scaled_residual,
        boundaries,
        parameters.detect_event_end_level,
        parameters.detect_impulse_widen,
        parameters.detect_impulse_widen_max,
    );
    let merged = without_claimed_rate_samples(
        merge_overlapping(start.into_iter().zip(end).collect()),
        claimed,
    );
    (
        extent_rows(&merged, boundaries, &scaled_residual, video_frames, rate, pass),
        accepted_peaks,
    )
}

fn axis_result_test(
    axis: &str,
    rows: &[ExtentRow],
    found_at: &[usize],
    fit: &fit::Fit,
    full_track_rsd: f64,
) -> Result<()> {
    for pair in rows.windows(2) {
        let previous = &pair[0];
        let row = &pair[1];
        if row.first_sample <= previous.last_sample {
            bail!(
                "the extents of axis {axis} overlap: {}..{} and {}..{}",
                previous.first_sample,
                previous.last_sample,
                row.first_sample,
                row.last_sample
            );
        }
        if row.first_sample == previous.last_sample + 1
            && row.classification == previous.classification
        {
            bail!(
                "the extents of axis {axis} touch: {}..{} and {}..{}",
                previous.first_sample,
                previous.last_sample,
                row.first_sample,
                row.last_sample
            );
        }
    }
    for row in rows {
        if !(row.first_sample <= row.peak_sample && row.peak_sample <= row.last_sample) {
            bail!(
                "the peak {} of axis {axis} lies outside its extent {}..{}",
                row.peak_sample,
                row.first_sample,
                row.last_sample
            );
        }
    }
    for &sample in found_at {
        if !rows
            .iter()
            .any(|row| row.first_sample <= sample && sample <= row.last_sample)
        {
            bail!(
                "the event at rate sample {sample} of axis {axis} lies outside every extent"
            );
        }
    }
    if fit.windows_without_weight > 0 {
        bail!(
            "{} local median windows of axis {axis} held no weight",
            fit.windows_without_weight
        );
    }
    if fit.zero_scale > 0 {
        bail!(
            "{} differences of axis {axis} had a zero scale",
            fit.zero_scale
        );
    }
    if !full_track_rsd.is_finite() || full_track_rsd <= 0.0 {
        bail!(
            "the full track Robust Standard Deviation of axis {axis} is {full_track_rsd}, not above zero"
        );
    }
    Ok(())
}

pub fn detect_axis(
    decode_result: &DecodeResult,
    axis: &str,
    parameters: &DetectionParameters,
) -> Result<AxisDetection> {
    let settings = axis_settings(parameters, axis)?;
    let column = axis_column(axis)?;
    let rate: Vec<f64> = decode_result
        .normalized_rate_deg_s
        .iter()
        .map(|rate| rate[column])
        .collect();
    let boundaries = decode::group_boundary_times(decode_result);
    let rate_sample_times_s = decode_result.recorded_rate_times_s.clone();
    let video_frames: Vec<f64> = rate_sample_times_s
        .iter()
        .map(|time| time * decode_result.video_frame_rate)
        .collect();

    let fit = fit::fit(
        &rate,
        &rate_sample_times_s,
        parameters.local_median_window,
        LOCAL_MEDIAN_FITS,
    );
    let residual: Vec<f64> = rate
        .iter()
        .zip(fit.local_median.iter())
        .map(|(rate, median)| rate - median)
        .collect();
    let local_rsd = local_robust_standard_deviations(
        &residual,
        &rate_sample_times_s,
        parameters.detect_noise_window,
    );

    let mut claimed = vec![false; rate.len()];
    let mut pass_rsd = [0.0; 3];
    let mut pass_threshold = [0.0; 3];
    let mut pass_extent_count = [0usize; 3];
    let mut pass_trigger_count = [0usize; 3];
    let mut pass_share = [0.0; 3];
    let mut pass_departure = [[f64::NAN; 5]; 3];
    let mut pass_departure_rsd = [[f64::NAN; 5]; 3];
    let mut pass_duration = [[f64::NAN; 5]; 3];
    let mut rows: Vec<ExtentRow> = Vec::new();
    let mut found_at_all: Vec<usize> = Vec::new();

    for (pass_index, pass) in PASSES.iter().enumerate() {
        let rsd = robust_standard_deviation_outside_extents(&residual, &claimed);
        let threshold = settings_threshold(&settings, pass.threshold_parameter);
        let (pass_rows, found_at) = if pass.impulse {
            run_impulse_pass(
                &residual,
                &boundaries,
                &video_frames,
                &rate,
                rsd,
                &claimed,
                threshold,
                parameters,
                *pass,
            )
        } else {
            run_ratio_pass(
                &residual,
                &rate_sample_times_s,
                &boundaries,
                &video_frames,
                &rate,
                &local_rsd,
                rsd,
                &claimed,
                threshold,
                parameters,
                *pass,
            )
        };
        for row in &pass_rows {
            claimed[row.first_sample..=row.last_sample].fill(true);
        }
        let peaks: Vec<f64> = pass_rows
            .iter()
            .map(|row| residual[row.peak_sample].abs())
            .collect();
        pass_departure[pass_index] = percentiles(&peaks);
        pass_departure_rsd[pass_index] = pass_departure[pass_index].map(|value| value / rsd);
        let durations: Vec<f64> = pass_rows.iter().map(|row| row.duration_ms).collect();
        pass_duration[pass_index] = percentiles(&durations);
        pass_rsd[pass_index] = rsd;
        pass_threshold[pass_index] = threshold;
        pass_extent_count[pass_index] = pass_rows.len();
        pass_trigger_count[pass_index] = found_at.len();
        pass_share[pass_index] = pass_rows
            .iter()
            .map(|row| row.rate_sample_count)
            .sum::<usize>() as f64
            / rate.len() as f64;
        found_at_all.extend(found_at);
        rows.extend(pass_rows);
    }

    rows.sort_by_key(|row| row.first_sample);
    let full_track_rsd = pass_rsd[0];
    let clean_rsd = robust_standard_deviation_outside_extents(&residual, &claimed);
    axis_result_test(axis, &rows, &found_at_all, &fit, full_track_rsd)?;

    Ok(AxisDetection {
        axis: axis.to_string(),
        settings,
        local_median_deg_s: fit.local_median,
        local_robust_standard_deviation_deg_s: local_rsd,
        full_track_robust_standard_deviation_deg_s: full_track_rsd,
        clean_robust_standard_deviation_deg_s: clean_rsd,
        pass_robust_standard_deviation_deg_s: pass_rsd,
        pass_threshold,
        pass_extent_count,
        pass_trigger_count,
        pass_share_of_rate_samples_inside_extents: pass_share,
        share_of_rate_samples_inside_extents: claimed.iter().filter(|value| **value).count()
            as f64
            / rate.len() as f64,
        pass_peak_departure_percentiles_deg_s: pass_departure,
        pass_peak_departure_percentiles_in_robust_standard_deviations: pass_departure_rsd,
        pass_extent_duration_percentiles_ms: pass_duration,
        rows,
    })
}

pub fn detect(
    decode_result: &DecodeResult,
    axes: &[String],
    parameters: &DetectionParameters,
) -> Result<DetectionResult> {
    let axes = selected_axes(axes)?;
    let per_axis: Vec<AxisDetection> = axes
        .par_iter()
        .map(|axis| detect_axis(decode_result, axis, parameters))
        .collect::<Result<Vec<_>>>()?;
    let rate_sample_times_s = decode_result.recorded_rate_times_s.clone();
    let rate_sample_video_frames = rate_sample_times_s
        .iter()
        .map(|time| time * decode_result.video_frame_rate)
        .collect();
    Ok(DetectionResult {
        video_name: decode_result.video_name.clone(),
        video_size_bytes: decode_result.video_size_bytes,
        video_mtime_s: decode_result.video_mtime_s,
        derivation_version: decode_result.derivation_version,
        axes: per_axis,
        rate_sample_times_s,
        rate_sample_video_frames,
    })
}

pub fn extent_identifier(axis: &str, peak_video_frame: f64) -> String {
    format!(
        "{}{}",
        axis.chars().next().unwrap_or('?').to_ascii_uppercase(),
        peak_video_frame.floor() as i64
    )
}

pub fn axis_index(result: &DetectionResult, axis: &str) -> Option<usize> {
    result.axes.iter().position(|detection| detection.axis == axis)
}

pub fn all_extents<'a>(result: &'a DetectionResult) -> Vec<(&'a str, &'a ExtentRow)> {
    let mut extents = Vec::new();
    for axis in &result.axes {
        for row in &axis.rows {
            extents.push((axis.axis.as_str(), row));
        }
    }
    extents
}

pub fn extent_identifier_of(axis: &str, row: &ExtentRow) -> String {
    extent_identifier(axis, row.peak_video_frame)
}
