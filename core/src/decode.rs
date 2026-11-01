//! The decode stage: the decode result derived from the gyro data track.

use std::cmp::Ordering;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::mp4::{self, Quat, Vec3};
use crate::quat::{canonicalize_sequence, equal_exact, relative_rotvec};
use crate::telemetry;

pub const DERIVATION_VERSION: u32 = 1;
pub const PERIOD_TEST_TOLERANCE_S: f64 = 1e-8;
pub const OSMO_ACTION_6_HANDLER_NAME: &str = "CAM meta";
pub const OSMO_ACTION_6_QUATERNIONS_PER_GROUP: usize = 3;
pub const DEG: f64 = 180.0 / std::f64::consts::PI;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoIdentity {
    pub video_name: String,
    pub video_size_bytes: u64,
    pub video_mtime_s: f64,
    pub derivation_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecodeResult {
    pub video_name: String,
    pub video_size_bytes: u64,
    pub video_mtime_s: f64,
    pub derivation_version: u32,
    pub handler_name: String,
    pub video_frame_rate: f64,
    pub quaternion_rate_hz: f64,
    pub measurement_rate_hz: f64,
    pub quaternions_per_video_frame: f64,
    pub quaternion_times_s: Vec<f64>,
    pub quaternions_wxyz: Vec<Quat>,
    pub kept_after_time_collisions: Vec<bool>,
    pub consecutive_quaternion_rate_times_s: Vec<f64>,
    pub consecutive_quaternion_rate_deg_s: Vec<Vec3>,
    pub recorded_rate_times_s: Vec<f64>,
    pub recorded_rate_deg_s: Vec<Vec3>,
    pub group_opening_quaternion_index: Vec<usize>,
    pub quaternion_count_per_telemetry_frame: Vec<u64>,
    pub owning_telemetry_frame: Vec<u32>,
    pub period_s: f64,
    pub normalized_rate_deg_s: Vec<Vec3>,
}

pub fn time_order(times: &[f64]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..times.len()).collect();
    order.sort_by(|&a, &b| {
        times[a]
            .partial_cmp(&times[b])
            .unwrap_or(Ordering::Equal)
    });
    order
}

pub fn kept_after_time_collisions(times: &[f64]) -> Vec<bool> {
    let mut kept = Vec::with_capacity(times.len());
    if let Some(first) = times.first() {
        kept.push(true);
        let _ = first;
    }
    for i in 1..times.len() {
        kept.push(times[i] > times[i - 1]);
    }
    kept
}

pub fn repeats_previous_quaternion(quaternions: &[Quat]) -> Vec<bool> {
    let mut repeats = Vec::with_capacity(quaternions.len().saturating_sub(1));
    for i in 1..quaternions.len() {
        repeats.push(equal_exact(&quaternions[i], &quaternions[i - 1]));
    }
    repeats
}

pub fn interval_rates(times: &[f64], quaternions: &[Quat]) -> (Vec<f64>, Vec<Vec3>) {
    if times.len() < 2 {
        return (Vec::new(), Vec::new());
    }
    let mut canonical = quaternions.to_vec();
    canonicalize_sequence(&mut canonical);
    let mut rate_times = Vec::with_capacity(times.len() - 1);
    let mut rates = Vec::with_capacity(times.len() - 1);
    for i in 0..times.len() - 1 {
        let rotvec = relative_rotvec(&canonical[i], &canonical[i + 1]);
        let elapsed = times[i + 1] - times[i];
        rates.push([
            rotvec[0] / elapsed * DEG,
            rotvec[1] / elapsed * DEG,
            rotvec[2] / elapsed * DEG,
        ]);
        rate_times.push(0.5 * (times[i] + times[i + 1]));
    }
    (rate_times, rates)
}

pub fn consecutive_quaternion_rates(times: &[f64], quaternions: &[Quat]) -> (Vec<f64>, Vec<Vec3>) {
    interval_rates(times, quaternions)
}

pub fn air_unit_group_opening_mask(quaternions: &[Quat]) -> Vec<bool> {
    let repeats = repeats_previous_quaternion(quaternions);
    let mut mask = Vec::with_capacity(quaternions.len());
    mask.push(true);
    mask.extend(repeats.iter().map(|repeat| !repeat));
    mask
}

pub fn osmo_action_6_group_opening_index(
    times: &[f64],
    owning_telemetry_frame: &[u32],
) -> Result<Vec<usize>> {
    let mut order: Vec<usize> = (0..times.len()).collect();
    order.sort_by(|&a, &b| {
        owning_telemetry_frame[a]
            .cmp(&owning_telemetry_frame[b])
            .then_with(|| {
                times[a]
                    .partial_cmp(&times[b])
                    .unwrap_or(Ordering::Equal)
            })
    });
    let group_count = (order.len().saturating_sub(1)) / OSMO_ACTION_6_QUATERNIONS_PER_GROUP;
    if group_count < 2 {
        bail!(
            "the track holds {} quaternions, fewer than two whole measurement groups and the quaternion that closes the second",
            order.len()
        );
    }
    let last = group_count * OSMO_ACTION_6_QUATERNIONS_PER_GROUP;
    Ok((0..=last)
        .step_by(OSMO_ACTION_6_QUATERNIONS_PER_GROUP)
        .map(|index| order[index])
        .collect())
}

pub fn group_boundary_times(result: &DecodeResult) -> Vec<f64> {
    let kept_times: Vec<f64> = result
        .quaternion_times_s
        .iter()
        .zip(result.kept_after_time_collisions.iter())
        .filter_map(|(time, kept)| kept.then_some(*time))
        .collect();
    result
        .group_opening_quaternion_index
        .iter()
        .map(|&index| kept_times[index])
        .collect()
}

pub fn derive_period(boundaries: &[f64], measurement_rate_hz: f64) -> Result<f64> {
    let rate_sample_count = boundaries.len().saturating_sub(1);
    if rate_sample_count == 0 {
        bail!("the track holds fewer than two measurement groups");
    }
    let period = (boundaries[boundaries.len() - 1] - boundaries[0]) / rate_sample_count as f64;
    let counted = 1.0 / measurement_rate_hz;
    if (period - counted).abs() > PERIOD_TEST_TOLERANCE_S {
        bail!(
            "the period T {:.6} us differs from the counted measurement period {:.6} us",
            period * 1e6,
            counted * 1e6
        );
    }
    Ok(period)
}

pub fn normalized_rate(recorded_rate: &[Vec3], boundaries: &[f64], period: f64) -> Vec<Vec3> {
    recorded_rate
        .iter()
        .enumerate()
        .map(|(index, rate)| {
            let factor = (boundaries[index + 1] - boundaries[index]) / period;
            [rate[0] * factor, rate[1] * factor, rate[2] * factor]
        })
        .collect()
}

pub fn counted_rate_hz(times: &[f64]) -> f64 {
    if times.len() < 2 {
        return f64::NAN;
    }
    (times.len() - 1) as f64 / (times[times.len() - 1] - times[0])
}

pub fn video_identity(video: &Path) -> Result<VideoIdentity> {
    let metadata = std::fs::metadata(video)
        .with_context(|| format!("cannot read the video {}", video.display()))?;
    let video_name = video
        .file_stem()
        .map_or_else(String::new, |name| name.to_string_lossy().to_string());
    let video_mtime_s = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| anyhow::anyhow!("the video modification time is before the Unix epoch"))?
        .as_secs_f64();
    Ok(VideoIdentity {
        video_name,
        video_size_bytes: metadata.len(),
        video_mtime_s,
        derivation_version: DERIVATION_VERSION,
    })
}

pub fn derive_decode_result(
    quaternion_times_s: &[f64],
    quaternions_wxyz: &[Quat],
    video_frame_rate: f64,
    identity: &VideoIdentity,
    quaternion_count_per_telemetry_frame: &[u64],
    owning_telemetry_frame: &[u32],
    handler_name: &str,
    group_opening_quaternion_index: Option<&[usize]>,
) -> Result<DecodeResult> {
    let kept = kept_after_time_collisions(quaternion_times_s);
    let kept_times: Vec<f64> = quaternion_times_s
        .iter()
        .zip(kept.iter())
        .filter_map(|(time, keep)| keep.then_some(*time))
        .collect();
    let kept_quaternions: Vec<Quat> = quaternions_wxyz
        .iter()
        .zip(kept.iter())
        .filter_map(|(quaternion, keep)| keep.then_some(*quaternion))
        .collect();
    let (consecutive_times, consecutive_rates) =
        consecutive_quaternion_rates(&kept_times, &kept_quaternions);

    let opening: Vec<usize> = if let Some(opening) = group_opening_quaternion_index {
        opening.to_vec()
    } else if handler_name == OSMO_ACTION_6_HANDLER_NAME {
        let kept_owners: Vec<u32> = owning_telemetry_frame
            .iter()
            .zip(kept.iter())
            .filter_map(|(owner, keep)| keep.then_some(*owner))
            .collect();
        osmo_action_6_group_opening_index(&kept_times, &kept_owners)?
    } else {
        air_unit_group_opening_mask(&kept_quaternions)
            .iter()
            .enumerate()
            .filter_map(|(index, opening)| opening.then_some(index))
            .collect()
    };

    let opening_times: Vec<f64> = opening.iter().map(|&index| kept_times[index]).collect();
    let opening_quaternions: Vec<Quat> = opening
        .iter()
        .map(|&index| kept_quaternions[index])
        .collect();
    let (recorded_times, recorded_rate) = interval_rates(&opening_times, &opening_quaternions);

    let quaternion_rate_hz = counted_rate_hz(&consecutive_times);
    let measurement_rate_hz = counted_rate_hz(&recorded_times);
    let quaternions_per_video_frame = quaternion_rate_hz / video_frame_rate;

    let boundaries: Vec<f64> = opening.iter().map(|&index| kept_times[index]).collect();
    let period_s = derive_period(&boundaries, measurement_rate_hz)?;
    let normalized_rate_deg_s = normalized_rate(&recorded_rate, &boundaries, period_s);

    Ok(DecodeResult {
        video_name: identity.video_name.clone(),
        video_size_bytes: identity.video_size_bytes,
        video_mtime_s: identity.video_mtime_s,
        derivation_version: identity.derivation_version,
        handler_name: handler_name.to_string(),
        video_frame_rate,
        quaternion_rate_hz,
        measurement_rate_hz,
        quaternions_per_video_frame,
        quaternion_times_s: quaternion_times_s.to_vec(),
        quaternions_wxyz: quaternions_wxyz.to_vec(),
        kept_after_time_collisions: kept,
        consecutive_quaternion_rate_times_s: consecutive_times,
        consecutive_quaternion_rate_deg_s: consecutive_rates,
        recorded_rate_times_s: recorded_times,
        recorded_rate_deg_s: recorded_rate,
        group_opening_quaternion_index: opening,
        quaternion_count_per_telemetry_frame: quaternion_count_per_telemetry_frame.to_vec(),
        owning_telemetry_frame: owning_telemetry_frame.to_vec(),
        period_s,
        normalized_rate_deg_s,
    })
}

pub fn decode(video: &Path, force: bool) -> Result<DecodeResult> {
    let mut file = std::fs::File::open(video)
        .with_context(|| format!("cannot open {}", video.display()))?;
    let identity = video_identity(video)?;
    decode_from_stream(&mut file, &identity, force)
        .with_context(|| format!("cannot decode {}", video.display()))
}

pub fn decode_from_stream<R: Read + Seek>(
    file: &mut R,
    identity: &VideoIdentity,
    force: bool,
) -> Result<DecodeResult> {
    let track = telemetry::read_gyro_data_track_from_stream(file, force)
        .context("cannot read the gyro data track")?;
    let order = time_order(&track.quaternion_times_s);
    let counts = &track.quaternion_count_per_telemetry_frame;
    let mut owning = Vec::with_capacity(counts.iter().sum::<u64>() as usize);
    for (frame, &count) in counts.iter().enumerate() {
        for _ in 0..count {
            owning.push(frame as u32);
        }
    }
    let ordered_times: Vec<f64> = order
        .iter()
        .map(|&index| track.quaternion_times_s[index])
        .collect();
    let ordered_quaternions: Vec<Quat> = order
        .iter()
        .map(|&index| track.quaternions_wxyz[index])
        .collect();
    let ordered_owning: Vec<u32> = order.iter().map(|&index| owning[index]).collect();

    file.seek(SeekFrom::Start(0))?;
    let video_frame_rate = mp4::video_frame_rate(file)
        .context("cannot read the video frame rate")?
        .context("the video reports no video frame rate")?;

    derive_decode_result(
        &ordered_times,
        &ordered_quaternions,
        video_frame_rate,
        identity,
        counts,
        &ordered_owning,
        &track.handler_name,
        None,
    )
}
