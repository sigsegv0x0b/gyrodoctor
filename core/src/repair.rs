//! The repair stage: attenuate each extent of the detection result with the ceiling
//! attenuator, rebuild the orientations from the repaired rates, and record the repair.

use anyhow::{bail, Context, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::attenuate::{self, RepairParameters};
use crate::decode::{self, DecodeResult, DEG};
use crate::detect::{self, AxisDetection, DetectionResult};
use crate::fit;
use crate::mp4::{Quat, Vec3};
use crate::quat::{
    compose_scipy, conjugate, from_rotvec, hamilton_app, normalize_xyzw, to_rotvec,
};

pub const SERIES: &str = "normalized";
pub const ROUND_TRIP_IDENTITY_TOLERANCE_DEG_S: f64 = 1e-6;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairResult {
    pub video_name: String,
    pub video_size_bytes: u64,
    pub video_mtime_s: f64,
    pub derivation_version: u32,
    pub quaternion_times_s: Vec<f64>,
    pub quaternions_wxyz: Vec<Quat>,
    pub kept_after_time_collisions: Vec<bool>,
    pub recorded_rate_times_s: Vec<f64>,
    pub normalized_rate_deg_s: Vec<Vec3>,
    pub repair_record: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AxisRecord {
    pub axis: String,
    pub repaired: bool,
    pub detected_extents_in: usize,
    pub extents_out: usize,
    pub repaired_rate_deg_s: Vec<f64>,
    pub record: serde_json::Value,
}

pub fn selected_repair_axes(axes: &[String], detection_result: &DetectionResult) -> Result<Vec<String>> {
    let mut seen: Vec<&String> = Vec::new();
    for axis in axes {
        detect::axis_column(axis)?;
        if seen.contains(&axis) {
            bail!(
                "the repair stage takes one or more distinct axes of {:?}, not {} twice",
                detect::AXES,
                axis
            );
        }
        seen.push(axis);
    }
    if seen.is_empty() {
        bail!(
            "the repair stage takes one or more distinct axes of {:?}, not none",
            detect::AXES
        );
    }
    let processed: Vec<&str> = detection_result
        .axes
        .iter()
        .map(|axis| axis.axis.as_str())
        .collect();
    let absent: Vec<&String> = axes
        .iter()
        .filter(|axis| !processed.contains(&axis.as_str()))
        .collect();
    if !absent.is_empty() {
        bail!(
            "the repair stage repairs only the axes of the detection result, {}, not {}",
            processed.join(", "),
            absent
                .iter()
                .map(|axis| axis.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(detect::AXES
        .iter()
        .filter(|axis| axes.iter().any(|wanted| wanted == *axis))
        .map(|axis| (*axis).to_string())
        .collect())
}

pub fn changed_only_inside_extents(
    rate: &[f64],
    repaired: &[f64],
    extents: &[(usize, usize)],
) -> Result<(usize, usize, usize)> {
    let mut inside = vec![false; rate.len()];
    for &(first, last) in extents {
        for flag in &mut inside[first..=last] {
            *flag = true;
        }
    }
    let mut changed = 0usize;
    let mut outside = 0usize;
    let mut first_outside = None;
    let mut inside_count = 0usize;
    for index in 0..rate.len() {
        if inside[index] {
            inside_count += 1;
        }
        if rate[index] != repaired[index] {
            changed += 1;
            if !inside[index] {
                outside += 1;
                if first_outside.is_none() {
                    first_outside = Some(index);
                }
            }
        }
    }
    if outside > 0 {
        bail!(
            "{outside} rate samples changed outside every extent, the first at rate sample {}",
            first_outside.unwrap_or(0)
        );
    }
    Ok((inside_count, changed, outside))
}

fn ceiling_attenuator_tests(
    rate: &[f64],
    repaired: &[f64],
    centerline: &[f64],
    extents: &[(usize, usize)],
    gains: &[f64],
    entries: &[attenuate::ExtentEntry],
    ceilings: &[f64],
) -> Result<serde_json::Value> {
    let mut edges_held = 0usize;
    let mut ratio_held = 0usize;
    let mut over_ceiling: Vec<usize> = Vec::new();
    let mut shortened_fades = 0usize;
    let mut over_anywhere = 0usize;
    for (index, ((&(first, last), &gain), entry)) in extents
        .iter()
        .zip(gains.iter())
        .zip(entries.iter())
        .enumerate()
    {
        if !entry.scaled {
            continue;
        }
        if repaired[first] != rate[first] || repaired[last] != rate[last] {
            bail!("the repair changed a rate sample at the end of the extent at {first}");
        }
        edges_held += 1;
        let mut low = f64::INFINITY;
        let mut high = f64::NEG_INFINITY;
        for sample in first..=last {
            let departure_in = rate[sample] - centerline[sample];
            if departure_in == 0.0 {
                continue;
            }
            let ratio = (repaired[sample] - centerline[sample]) / departure_in;
            low = low.min(ratio);
            high = high.max(ratio);
        }
        if low.is_finite() && (low < gain - 1e-9 || high > 1.0 + 1e-9) {
            bail!(
                "the repair scaled a departure outside the gain of extent {first}: {low} to {high} against gain {gain}"
            );
        }
        ratio_held += 1;
        if entry.fade_shortened == Some(true) {
            shortened_fades += 1;
        } else if entry
            .peak_in_clean_robust_standard_deviations_after_interior
            .unwrap_or(f64::NAN)
            > ceilings[index] + 1e-6
        {
            over_ceiling.push(first);
        }
        if entry
            .peak_in_clean_robust_standard_deviations_after
            .unwrap_or(f64::NAN)
            > ceilings[index] + 1e-6
        {
            over_anywhere += 1;
        }
    }
    if let Some(&first) = over_ceiling.first() {
        bail!(
            "{} extents stand above their ceiling in the interior, the first at rate sample {first}",
            over_ceiling.len()
        );
    }
    Ok(serde_json::json!({
        "extents_identical_at_both_boundaries": edges_held,
        "extents_scaled_within_gain_to_one": ratio_held,
        "extents_over_ceiling_in_the_interior": over_ceiling.len(),
        "extents_over_ceiling_anywhere_in_the_extent": over_anywhere,
        "extents_with_shortened_fades": shortened_fades,
    }))
}

pub fn axis_of_detection_result<'a>(
    detection_result: &'a DetectionResult,
    axis: &str,
) -> Result<&'a AxisDetection> {
    detection_result
        .axes
        .iter()
        .find(|detection| detection.axis == axis)
        .with_context(|| {
            format!(
                "the detection result holds no axis {axis}; it holds {}",
                detection_result
                    .axes
                    .iter()
                    .map(|detection| detection.axis.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

fn extent_report(
    rate: &[f64],
    repaired: &[f64],
    centerline: &[f64],
    boundaries: &[f64],
    period_s: f64,
    first: usize,
    last: usize,
    gain: f64,
    entry: &attenuate::ExtentEntry,
) -> serde_json::Value {
    let removed = fit::numpy_sum(
        &(first..=last)
            .map(|index| (rate[index] - repaired[index]) * period_s)
            .collect::<Vec<_>>(),
    );
    let recorded = fit::numpy_sum(
        &(first..=last)
            .map(|index| rate[index] * period_s)
            .collect::<Vec<_>>(),
    );
    let centerline_rotation = fit::numpy_sum(
        &(first..=last)
            .map(|index| centerline[index] * period_s)
            .collect::<Vec<_>>(),
    );
    let mut report = serde_json::json!({
        "first_sample": first,
        "last_sample": last,
        "rate_sample_count": last - first + 1,
        "start_s": boundaries[first],
        "end_s": boundaries[last + 1],
        "duration_ms": (boundaries[last + 1] - boundaries[first]) * 1e3,
        "gain": gain,
        "recorded_rotation_deg": recorded,
        "centerline_rotation_deg": centerline_rotation,
        "removed_rotation_deg": removed,
        "kept_rotation_deg": recorded - removed,
    });
    let entry_json = serde_json::to_value(entry).unwrap_or(serde_json::Value::Null);
    if let (Some(report_object), serde_json::Value::Object(entry_object)) =
        (report.as_object_mut(), entry_json)
    {
        for (key, value) in entry_object {
            report_object.entry(key).or_insert(value);
        }
    }
    report
}

#[allow(clippy::too_many_arguments)]
pub fn repair_axis(
    rate: &[f64],
    boundaries: &[f64],
    rate_sample_times_s: &[f64],
    measurement_rate_hz: f64,
    period_s: f64,
    detection_result: &DetectionResult,
    axis: &str,
    repair_axes: &[String],
    parameters: &RepairParameters,
) -> Result<AxisRecord> {
    let repaired_here = repair_axes.iter().any(|repaired| repaired == axis);
    let axis_parameters = if repaired_here {
        parameters.clone()
    } else {
        parameters.with_infinite_ceilings()
    };
    let detection = axis_of_detection_result(detection_result, axis)?;
    let extents: Vec<(usize, usize)> = detection
        .rows
        .iter()
        .map(|row| (row.first_sample, row.last_sample))
        .collect();
    let peaks: Vec<usize> = detection.rows.iter().map(|row| row.peak_sample).collect();
    let classifications: Vec<String> = detection
        .rows
        .iter()
        .map(|row| row.classification.clone())
        .collect();
    let identifiers: Vec<String> = detection
        .rows
        .iter()
        .map(|row| detect::extent_identifier(axis, row.peak_video_frame))
        .collect();
    let excluded: Vec<bool> = identifiers
        .iter()
        .map(|identifier| parameters.do_not_repair.contains(identifier))
        .collect();

    let started = std::time::Instant::now();
    let result = attenuate::attenuate(
        rate,
        measurement_rate_hz,
        &extents,
        &peaks,
        &classifications,
        &detection.local_median_deg_s,
        detection.clean_robust_standard_deviation_deg_s,
        rate_sample_times_s,
        &excluded,
        &axis_parameters,
    )?;
    let elapsed_s = started.elapsed().as_secs_f64();
    if !repaired_here {
        for (index, value) in rate.iter().enumerate() {
            if *value != result.repaired_rate_deg_s[index] {
                bail!(
                    "the repair changed rate sample {index} of axis {axis}, which lies outside the repair axes"
                );
            }
        }
    }

    let extent_reports: Vec<serde_json::Value> = identifiers
        .iter()
        .zip(result.extents.iter())
        .zip(result.gains.iter())
        .zip(result.extent_entries.iter())
        .map(|(((identifier, &(first, last)), &gain), entry)| {
            let mut report = serde_json::json!({ "identifier": identifier });
            let detail = extent_report(
                rate,
                &result.repaired_rate_deg_s,
                &result.centerline,
                boundaries,
                period_s,
                first,
                last,
                gain,
                entry,
            );
            if let (Some(object), serde_json::Value::Object(detail)) =
                (report.as_object_mut(), detail)
            {
                for (key, value) in detail {
                    object.entry(key).or_insert(value);
                }
            }
            report
        })
        .collect();

    let mut scaled: Vec<&serde_json::Value> = extent_reports
        .iter()
        .filter(|report| report["scaled"].as_bool() == Some(true))
        .collect();
    scaled.sort_by_key(|report| report["first_sample"].as_u64().unwrap_or(0));
    let gains_of_scaled: Vec<f64> = scaled
        .iter()
        .map(|report| report["gain"].as_f64().unwrap_or(0.0))
        .collect();
    let durations: Vec<f64> = extent_reports
        .iter()
        .filter_map(|report| report["duration_ms"].as_f64())
        .collect();
    let removed: Vec<f64> = scaled
        .iter()
        .map(|report| report["removed_rotation_deg"].as_f64().unwrap_or(0.0))
        .collect();
    let before: Vec<f64> = scaled
        .iter()
        .map(|report| {
            report["peak_in_clean_robust_standard_deviations_before"]
                .as_f64()
                .unwrap_or(f64::NAN)
        })
        .collect();
    let kept_reasons: Vec<&str> = extent_reports
        .iter()
        .filter(|report| report["scaled"].as_bool() != Some(true))
        .filter_map(|report| report["reason"].as_str())
        .collect();
    let (inside, changed, outside) =
        changed_only_inside_extents(rate, &result.repaired_rate_deg_s, &result.extents)?;
    let ceilings: Vec<f64> = classifications
        .iter()
        .map(|classification| axis_parameters.ceiling_of(classification))
        .collect::<Result<Vec<_>>>()?;
    let tests = ceiling_attenuator_tests(
        rate,
        &result.repaired_rate_deg_s,
        &result.centerline,
        &result.extents,
        &result.gains,
        &result.extent_entries,
        &ceilings,
    )?;

    let record = serde_json::json!({
        "clean_robust_standard_deviation_deg_s": detection.clean_robust_standard_deviation_deg_s,
        "repaired": repaired_here,
        "seconds_to_attenuate": elapsed_s,
        "detected_extents_in": extents.len(),
        "extents_out": result.extents.len(),
        "identity_outside_extents": {
            "rate_samples_inside_extents": inside,
            "rate_samples_changed": changed,
            "rate_samples_changed_outside_extents": outside,
        },
        "ceiling_attenuator_tests": tests,
        "extent_summary": {
            "scaled_count": scaled.len(),
            "skipped_count": kept_reasons.iter().filter(|reason| **reason == attenuate::AT_A_VIDEO_EDGE).count(),
            "excluded_count": kept_reasons.iter().filter(|reason| **reason == attenuate::ON_THE_DO_NOT_REPAIR_LIST).count(),
            "duration_ms_min": durations.iter().copied().fold(None, |acc: Option<f64>, value| Some(acc.map_or(value, |acc| acc.min(value)))),
            "duration_ms_median": if durations.is_empty() { None } else { let mut copy = durations.clone(); Some(fit::median(&mut copy)) },
            "duration_ms_max": durations.iter().copied().fold(None, |acc: Option<f64>, value| Some(acc.map_or(value, |acc| acc.max(value)))),
            "gain_min": gains_of_scaled.iter().copied().fold(None, |acc: Option<f64>, value| Some(acc.map_or(value, |acc| acc.min(value)))),
            "gain_median": if gains_of_scaled.is_empty() { None } else { let mut copy = gains_of_scaled.clone(); Some(fit::median(&mut copy)) },
            "gain_max": gains_of_scaled.iter().copied().fold(None, |acc: Option<f64>, value| Some(acc.map_or(value, |acc| acc.max(value)))),
            "passed_through_count": gains_of_scaled.iter().filter(|gain| **gain >= 1.0).count(),
            "peak_in_clean_robust_standard_deviations_before_min": before.iter().copied().fold(None, |acc: Option<f64>, value| Some(acc.map_or(value, |acc| acc.min(value)))),
            "peak_in_clean_robust_standard_deviations_before_median": if before.is_empty() { None } else { let mut copy = before.clone(); Some(fit::median(&mut copy)) },
            "peak_in_clean_robust_standard_deviations_before_max": before.iter().copied().fold(None, |acc: Option<f64>, value| Some(acc.map_or(value, |acc| acc.max(value)))),
            "removed_rotation_deg_abs_total": removed.iter().map(|value| value.abs()).sum::<f64>(),
            "removed_rotation_deg_net": removed.iter().sum::<f64>(),
        },
        "attenuator_statistics": result.statistics,
        "extents": extent_reports,
    });

    Ok(AxisRecord {
        axis: axis.to_string(),
        repaired: repaired_here,
        detected_extents_in: extents.len(),
        extents_out: result.extents.len(),
        repaired_rate_deg_s: result.repaired_rate_deg_s,
        record,
    })
}

fn remove_axis(
    rate: &[f64],
    period_s: f64,
    detection_result: &DetectionResult,
    axis: &str,
) -> Result<(Vec<f64>, serde_json::Value)> {
    let repaired = vec![0.0; rate.len()];
    for (index, value) in repaired.iter().enumerate() {
        if *value != 0.0 {
            bail!("the removed axis {axis} is not zero at rate sample {index}");
        }
    }
    let removed = fit::numpy_sum(
        &rate
            .iter()
            .map(|rate| (rate - 0.0) * period_s)
            .collect::<Vec<_>>(),
    );
    let mut extents_json = Vec::new();
    let mut detected = 0usize;
    if let Some(detection) = detection_result
        .axes
        .iter()
        .find(|detection| detection.axis == axis)
    {
        detected = detection.rows.len();
        for row in &detection.rows {
            extents_json.push(serde_json::json!({
                "identifier": detect::extent_identifier(axis, row.peak_video_frame),
                "first_sample": row.first_sample,
                "last_sample": row.last_sample,
            }));
        }
    }
    Ok((
        repaired,
        serde_json::json!({
            "detected_extents_in": detected,
            "removed_rotation_deg_abs_total": removed.abs(),
            "removed_rotation_deg_net": removed,
            "extents": extents_json,
        }),
    ))
}

fn composed_orientations(first_orientation_wxyz: Quat, rotation_deg: &[Vec3]) -> Vec<Quat> {
    let mut orientations = Vec::with_capacity(rotation_deg.len() + 1);
    orientations.push(first_orientation_wxyz);
    let mut orientation = first_orientation_wxyz;
    for rotation in rotation_deg {
        let rotation_wxyz = from_rotvec(&[
            rotation[0] / DEG,
            rotation[1] / DEG,
            rotation[2] / DEG,
        ]);
        orientation = hamilton_app(&orientation, &rotation_wxyz);
        orientations.push(orientation);
    }
    orientations
}

fn inv(q: &Quat) -> Quat {
    conjugate(q)
}

fn quat_between(opening: &Quat, closing: &Quat) -> Quat {
    compose_scipy(&inv(opening), closing)
}

fn rotation_quat(q: &Quat) -> Quat {
    normalize_xyzw(q)
}

fn redistribute_across_group_quaternions(
    kept_quaternions: &[Quat],
    group_opening_quaternion_index: &[usize],
    rebuilt_orientations_at_group_boundaries: &[Quat],
) -> Vec<Quat> {
    let quaternion_count = kept_quaternions.len();
    let group_count = group_opening_quaternion_index.len() - 1;
    let mut group = vec![0usize; quaternion_count];
    for (index, slot) in group.iter_mut().enumerate() {
        let position = group_opening_quaternion_index.partition_point(|&opening| opening <= index);
        *slot = position.saturating_sub(1).min(group_count - 1);
    }

    let recorded_openings: Vec<Quat> = (0..group_count)
        .map(|index| rotation_quat(&kept_quaternions[group_opening_quaternion_index[index]]))
        .collect();
    let recorded_closings: Vec<Quat> = (1..=group_count)
        .map(|index| rotation_quat(&kept_quaternions[group_opening_quaternion_index[index]]))
        .collect();
    let rebuilt_openings: Vec<Quat> = (0..group_count)
        .map(|index| rotation_quat(&rebuilt_orientations_at_group_boundaries[index]))
        .collect();
    let rebuilt_closings: Vec<Quat> = (1..=group_count)
        .map(|index| rotation_quat(&rebuilt_orientations_at_group_boundaries[index]))
        .collect();

    let recorded_within: Vec<Quat> = (0..quaternion_count)
        .map(|index| quat_between(&recorded_openings[group[index]], &kept_quaternions[index]))
        .collect();
    let recorded_group_rotation: Vec<Quat> = (0..group_count)
        .map(|index| quat_between(&recorded_openings[index], &recorded_closings[index]))
        .collect();
    let repair_change: Vec<Vec3> = (0..group_count)
        .map(|index| {
            let rebuilt = quat_between(&rebuilt_openings[index], &rebuilt_closings[index]);
            to_rotvec(&quat_between(&recorded_group_rotation[index], &rebuilt))
        })
        .collect();

    let recorded_group_rotation_vector: Vec<Vec3> = recorded_group_rotation
        .iter()
        .map(to_rotvec)
        .collect();
    let mut fraction = vec![0.0; quaternion_count];
    for index in 0..quaternion_count {
        let within = to_rotvec(&recorded_within[index]);
        let group_vector = &recorded_group_rotation_vector[group[index]];
        let denominator = dot3(group_vector, group_vector);
        let progressed = dot3(&within, group_vector);
        fraction[index] = if denominator > 0.0 {
            progressed / denominator
        } else {
            0.0
        };
    }

    let mut rebuilt = vec![[0.0; 4]; quaternion_count];
    for index in 0..quaternion_count {
        let change = from_rotvec(&[
            fraction[index] * repair_change[group[index]][0],
            fraction[index] * repair_change[group[index]][1],
            fraction[index] * repair_change[group[index]][2],
        ]);
        let value = compose_scipy(
            &compose_scipy(&rebuilt_openings[group[index]], &recorded_within[index]),
            &change,
        );
        rebuilt[index] = value;
    }

    let last_boundary = *group_opening_quaternion_index.last().unwrap();
    let recorded_at_last =
        rotation_quat(&kept_quaternions[last_boundary]);
    let rebuilt_at_last =
        rotation_quat(&rebuilt_orientations_at_group_boundaries
            [rebuilt_orientations_at_group_boundaries.len() - 1]);
    for index in last_boundary..quaternion_count {
        let relative = quat_between(&recorded_at_last, &rotation_quat(&kept_quaternions[index]));
        rebuilt[index] = compose_scipy(&rebuilt_at_last, &relative);
    }
    rebuilt
}

fn dot3(a: &Vec3, b: &Vec3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn quaternion_track_from_repaired_rates(
    decode_result: &DecodeResult,
    repaired_rate_deg_s: &[Vec3],
    period_s: Option<f64>,
) -> Result<Vec<Quat>> {
    if repaired_rate_deg_s.len() != decode_result.recorded_rate_deg_s.len() {
        bail!(
            "the repaired rates hold {} rate samples and the recorded rate of the decode result holds {}",
            repaired_rate_deg_s.len(),
            decode_result.recorded_rate_deg_s.len()
        );
    }
    let boundaries = decode::group_boundary_times(decode_result);
    let Some(period) = period_s else {
        bail!("the Normalized Rate rebuild requires the period T");
    };
    let derived = decode::derive_period(&boundaries, decode_result.measurement_rate_hz)?;
    if (period - derived).abs() > decode::PERIOD_TEST_TOLERANCE_S {
        bail!(
            "the stated period {:.6} us differs from the derived period {:.6} us",
            period * 1e6,
            derived * 1e6
        );
    }
    let rotations: Vec<Vec3> = repaired_rate_deg_s
        .iter()
        .map(|rate| [rate[0] * period, rate[1] * period, rate[2] * period])
        .collect();
    rebuild_track(decode_result, &rotations)
}

fn rebuild_track(decode_result: &DecodeResult, rotations: &[Vec3]) -> Result<Vec<Quat>> {    let recorded_quaternions = &decode_result.quaternions_wxyz;
    let keep = &decode_result.kept_after_time_collisions;
    let group_opening_quaternion_index = &decode_result.group_opening_quaternion_index;
    let kept_quaternions: Vec<Quat> = recorded_quaternions
        .iter()
        .zip(keep.iter())
        .filter_map(|(quaternion, keep)| keep.then_some(*quaternion))
        .collect();

    let rebuilt_orientations_at_group_boundaries = composed_orientations(
        kept_quaternions[group_opening_quaternion_index[0]],
        rotations,
    );

    let rebuilt_kept_quaternions = if decode_result.handler_name == "CAM meta" {
        redistribute_across_group_quaternions(
            &kept_quaternions,
            group_opening_quaternion_index,
            &rebuilt_orientations_at_group_boundaries,
        )
    } else {
        let mut run_lengths = Vec::with_capacity(group_opening_quaternion_index.len());
        for pair in group_opening_quaternion_index.windows(2) {
            run_lengths.push(pair[1] - pair[0]);
        }
        run_lengths.push(kept_quaternions.len() - group_opening_quaternion_index
            [group_opening_quaternion_index.len() - 1]);
        let mut rebuilt = Vec::with_capacity(kept_quaternions.len());
        for (orientation, run) in rebuilt_orientations_at_group_boundaries
            .iter()
            .zip(run_lengths.iter())
        {
            for _ in 0..*run {
                rebuilt.push(*orientation);
            }
        }
        rebuilt
    };

    let mut rebuilt = recorded_quaternions.clone();
    let kept_indices: Vec<usize> = (0..keep.len()).filter(|&index| keep[index]).collect();
    for (position, &index) in kept_indices.iter().enumerate() {
        rebuilt[index] = rebuilt_kept_quaternions[position];
    }
    if keep.iter().any(|keep| !*keep) {
        let mut source = 0usize;
        for index in 0..keep.len() {
            if keep[index] {
                source = index;
            } else {
                rebuilt[index] = rebuilt[source];
            }
        }
    }
    Ok(rebuilt)
}

pub fn round_trip_identity(decode_result: &DecodeResult) -> Result<serde_json::Value> {
    let boundaries = decode::group_boundary_times(decode_result);
    let rotations: Vec<Vec3> = decode_result
        .recorded_rate_deg_s
        .iter()
        .enumerate()
        .map(|(index, rate)| {
            let interval = boundaries[index + 1] - boundaries[index];
            [rate[0] * interval, rate[1] * interval, rate[2] * interval]
        })
        .collect();
    let rebuilt = rebuild_track(decode_result, &rotations)?;

    let keep = &decode_result.kept_after_time_collisions;
    let kept_times: Vec<f64> = decode_result
        .quaternion_times_s
        .iter()
        .zip(keep.iter())
        .filter_map(|(time, keep)| keep.then_some(*time))
        .collect();
    let rebuilt_kept: Vec<Quat> = rebuilt
        .iter()
        .zip(keep.iter())
        .filter_map(|(quaternion, keep)| keep.then_some(*quaternion))
        .collect();
    let (_, consecutive) = decode::consecutive_quaternion_rates(&kept_times, &rebuilt_kept);
    let opening = &decode_result.group_opening_quaternion_index;
    let opening_times: Vec<f64> = opening.iter().map(|&index| kept_times[index]).collect();
    let opening_quaternions: Vec<Quat> = opening.iter().map(|&index| rebuilt_kept[index]).collect();
    let (_, recorded) = decode::interval_rates(&opening_times, &opening_quaternions);
    let mut max_consecutive: f64 = 0.0;
    let mut max_recorded: f64 = 0.0;
    for (index, rate) in consecutive.iter().enumerate() {
        let expected = decode_result.consecutive_quaternion_rate_deg_s[index];
        for axis in 0..3 {
            max_consecutive = max_consecutive.max((rate[axis] - expected[axis]).abs());
        }
    }
    for (index, rate) in recorded.iter().enumerate() {
        let expected = decode_result.recorded_rate_deg_s[index];
        for axis in 0..3 {
            max_recorded = max_recorded.max((rate[axis] - expected[axis]).abs());
        }
    }
    if max_consecutive > ROUND_TRIP_IDENTITY_TOLERANCE_DEG_S
        || max_recorded > ROUND_TRIP_IDENTITY_TOLERANCE_DEG_S
    {
        bail!(
            "round trip identity failed: consecutive {max_consecutive} deg/s and recorded {max_recorded} deg/s against tolerance {ROUND_TRIP_IDENTITY_TOLERANCE_DEG_S}"
        );
    }
    Ok(serde_json::json!({
        "name": "round trip identity",
        "max_consecutive_quaternion_rate_difference_deg_s": max_consecutive,
        "max_recorded_rate_difference_deg_s": max_recorded,
        "tolerance_deg_s": ROUND_TRIP_IDENTITY_TOLERANCE_DEG_S,
        "passed": true,
    }))
}

pub fn cadence_preservation(
    decode_result: &DecodeResult,
    rebuilt_track: &[Quat],
) -> Result<serde_json::Value> {
    let keep = &decode_result.kept_after_time_collisions;
    let recorded: Vec<Quat> = decode_result
        .quaternions_wxyz
        .iter()
        .zip(keep.iter())
        .filter_map(|(quaternion, keep)| keep.then_some(*quaternion))
        .collect();
    let rebuilt: Vec<Quat> = rebuilt_track
        .iter()
        .zip(keep.iter())
        .filter_map(|(quaternion, keep)| keep.then_some(*quaternion))
        .collect();
    let recorded_repeats = decode::repeats_previous_quaternion(&recorded);
    let rebuilt_repeats = decode::repeats_previous_quaternion(&rebuilt);
    let recorded_count = recorded_repeats.iter().filter(|repeat| **repeat).count();
    let rebuilt_count = rebuilt_repeats.iter().filter(|repeat| **repeat).count();
    if recorded_repeats != rebuilt_repeats {
        let differing = recorded_repeats
            .iter()
            .zip(rebuilt_repeats.iter())
            .filter(|(a, b)| a != b)
            .count();
        bail!(
            "cadence preservation failed: {rebuilt_count} rebuilt repeated pairs against {recorded_count} recorded, {differing} differing"
        );
    }
    Ok(serde_json::json!({
        "name": "cadence preservation",
        "recorded_repeated_pairs": recorded_count,
        "rebuilt_repeated_pairs": rebuilt_count,
        "passed": true,
    }))
}

pub fn repair(
    decode_result: &DecodeResult,
    detection_result: &DetectionResult,
    axes: &[String],
    parameters: &RepairParameters,
) -> Result<RepairResult> {
    let repair_axes = selected_repair_axes(axes, detection_result)?;
    let processed_axes: Vec<String> = detection_result
        .axes
        .iter()
        .map(|axis| axis.axis.clone())
        .collect();
    let removed_axes = &parameters.remove_gyro_motion;
    let repaired_axes: Vec<String> = detect::AXES
        .iter()
        .filter(|axis| processed_axes.contains(&axis.to_string()) || removed_axes.contains(&axis.to_string()))
        .map(|axis| (*axis).to_string())
        .collect();

    let rates = &decode_result.normalized_rate_deg_s;
    let boundaries = decode::group_boundary_times(decode_result);
    let rate_sample_times_s: Vec<f64> = boundaries
        .windows(2)
        .map(|pair| 0.5 * (pair[0] + pair[1]))
        .collect();
    let measurement_rate_hz = decode_result.measurement_rate_hz;
    let period_s = decode_result.period_s;

    let per_axis: Vec<(String, Vec<f64>, serde_json::Value, bool)> = repaired_axes
        .par_iter()
        .map(|axis| -> Result<(String, Vec<f64>, serde_json::Value, bool)> {
            let column = detect::axis_column(axis)?;
            let rate: Vec<f64> = rates.iter().map(|rate| rate[column]).collect();
            if removed_axes.contains(axis) {
                let (repaired, record) =
                    remove_axis(&rate, period_s, detection_result, axis)?;
                Ok((axis.clone(), repaired, record, false))
            } else {
                let axis_repair = repair_axis(
                    &rate,
                    &boundaries,
                    &rate_sample_times_s,
                    measurement_rate_hz,
                    period_s,
                    detection_result,
                    axis,
                    &repair_axes,
                    parameters,
                )?;
                Ok((
                    axis.clone(),
                    axis_repair.repaired_rate_deg_s,
                    axis_repair.record,
                    axis_repair.repaired,
                ))
            }
        })
        .collect::<Result<Vec<_>>>()?;

    let mut repaired_rates = rates.clone();
    let mut axis_records = serde_json::Map::new();
    for (axis, repaired_rate, record, _) in &per_axis {
        let column = detect::axis_column(axis)?;
        for (index, value) in repaired_rate.iter().enumerate() {
            repaired_rates[index][column] = *value;
        }
        axis_records.insert(axis.clone(), record.clone());
    }

    let mut assertions = vec![round_trip_identity(decode_result)?];
    let rebuilt = quaternion_track_from_repaired_rates(
        decode_result,
        &repaired_rates,
        Some(period_s),
    )?;
    if removed_axes.is_empty() {
        assertions.push(cadence_preservation(decode_result, &rebuilt)?);
    }

    let identity = decode::VideoIdentity {
        video_name: decode_result.video_name.clone(),
        video_size_bytes: decode_result.video_size_bytes,
        video_mtime_s: decode_result.video_mtime_s,
        derivation_version: decode_result.derivation_version,
    };
    let recomputed = decode::derive_decode_result(
        &decode_result.quaternion_times_s,
        &rebuilt,
        decode_result.video_frame_rate,
        &identity,
        &decode_result.quaternion_count_per_telemetry_frame,
        &decode_result.owning_telemetry_frame,
        &decode_result.handler_name,
        Some(&decode_result.group_opening_quaternion_index),
    )?;

    let record = serde_json::json!({
        "run": {
            "attenuator_method": "ceiling",
            "attenuator": "ceiling_attenuator.py",
            "baseline_series": "local_median_deg_s",
            "input_series": "normalized_rate_deg_s",
            "input_series_label": "Normalized Rate (rotation / T)",
            "repaired_rate_series": SERIES,
            "composition_multiplier": "the period T",
            "detected_extents": "every extent of the detection result on each axis of the detection result",
            "measurement_rate_source": "measurement_rate_hz of the decode result, counted over the video",
            "scope": "whole video",
            "repair_parameters": parameters,
            "repair_axes": repair_axes,
        },
        "video": {
            "video_name": decode_result.video_name,
            "video_size_bytes": decode_result.video_size_bytes,
            "video_mtime_s": decode_result.video_mtime_s,
            "derivation_version": decode_result.derivation_version,
            "video_frame_rate": decode_result.video_frame_rate,
            "measurement_rate_hz": measurement_rate_hz,
            "period_s": period_s,
            "rate_sample_count": rates.len(),
            "axes": processed_axes,
        },
        "assertions": assertions,
        "axes": axis_records,
    });

    Ok(RepairResult {
        video_name: recomputed.video_name.clone(),
        video_size_bytes: recomputed.video_size_bytes,
        video_mtime_s: recomputed.video_mtime_s,
        derivation_version: recomputed.derivation_version,
        quaternion_times_s: recomputed.quaternion_times_s,
        quaternions_wxyz: recomputed.quaternions_wxyz,
        kept_after_time_collisions: recomputed.kept_after_time_collisions,
        recorded_rate_times_s: recomputed.recorded_rate_times_s,
        normalized_rate_deg_s: recomputed.normalized_rate_deg_s,
        repair_record: serde_json::to_string(&record)?,
    })
}
