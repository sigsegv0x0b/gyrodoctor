//! The write stage: write the repaired orientations into a copy of the video, each at
//! the time slot of its quaternion, and rewrite the stored camera model name.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::decode;
use crate::mp4;
use crate::quat::{conjugate, hamilton_app, normalize, Quat};
use crate::repair::RepairResult;
use crate::telemetry;

const IDENTITY_THRESHOLD_RAD: f64 = 1e-6;
const MINIMUM_ABSOLUTE_DOT_PRODUCT: f64 = 0.999;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WritebackReport {
    pub quaternion_messages_overwritten: u64,
    pub telemetry_frames_rewritten: usize,
    pub video_size_bytes: u64,
}

fn resolved(path: &Path) -> Result<std::path::PathBuf> {
    Ok(std::fs::canonicalize(path).or_else(|_| std::path::absolute(path))?)
}

fn stored_conjugate(stored: &Quat, repaired: &Quat) -> Quat {
    let mut rotation = hamilton_app(&conjugate(stored), repaired);
    if rotation[0] < 0.0 {
        rotation = [-rotation[0], -rotation[1], -rotation[2], -rotation[3]];
    }
    normalize(&rotation)
}

fn stored_to_repaired_rotation_at_times(
    repaired_orientation_times_s: &[f64],
    rotations_wxyz: &[Quat],
    times: &[f64],
) -> Result<Vec<Quat>> {
    let mut at_times = Vec::with_capacity(times.len());
    for &time in times {
        let index = repaired_orientation_times_s.partition_point(|&candidate| candidate < time);
        let among_repaired = index < repaired_orientation_times_s.len()
            && repaired_orientation_times_s[index] == time;
        if !among_repaired {
            let count = times
                .iter()
                .filter(|candidate| {
                    let position = repaired_orientation_times_s
                        .partition_point(|&value| value < **candidate);
                    position >= repaired_orientation_times_s.len()
                        || repaired_orientation_times_s[position] != **candidate
                })
                .count();
            bail!(
                "{count} quaternion times are not times of the repaired orientations"
            );
        }
        at_times.push(rotations_wxyz[index]);
    }
    Ok(at_times)
}

struct PreparedWrite {
    new_telemetry_frames: Vec<(u64, Vec<u8>)>,
    quaternion_messages_overwritten: u64,
    video_size_bytes: u64,
}

fn prepare_write<R: Read + Seek>(
    file: &mut R,
    repair_result: &RepairResult,
    force: bool,
) -> Result<PreparedWrite> {
    let track = telemetry::read_gyro_data_track_from_stream(file, force)?;
    let order = decode::time_order(&track.quaternion_times_s);
    let track_times_ordered: Vec<f64> = order
        .iter()
        .map(|&index| track.quaternion_times_s[index])
        .collect();
    if track_times_ordered.len() != repair_result.quaternion_times_s.len() {
        bail!(
            "the repair result holds {} orientations and the video holds {}",
            repair_result.quaternion_times_s.len(),
            track_times_ordered.len()
        );
    }
    let differing = track_times_ordered
        .iter()
        .zip(repair_result.quaternion_times_s.iter())
        .filter(|(a, b)| a != b)
        .count();
    if differing > 0 {
        bail!("{differing} quaternion times of the repair result differ from the video's");
    }

    let keep = &repair_result.kept_after_time_collisions;
    let mut stored_kept = Vec::new();
    let mut repaired_kept = Vec::new();
    let mut repaired_times = Vec::new();
    for (position, &index) in order.iter().enumerate() {
        if keep[position] {
            stored_kept.push(track.quaternions_wxyz[index]);
            repaired_kept.push(repair_result.quaternions_wxyz[position]);
            repaired_times.push(repair_result.quaternion_times_s[position]);
        }
    }
    let rotations: Vec<Quat> = stored_kept
        .iter()
        .zip(repaired_kept.iter())
        .map(|(stored, repaired)| stored_conjugate(stored, repaired))
        .collect();
    let rotations_at_file_times = stored_to_repaired_rotation_at_times(
        &repaired_times,
        &rotations,
        &track.quaternion_times_s,
    )?;
    let changed: Vec<bool> = rotations_at_file_times
        .iter()
        .map(|rotation| {
            let angle = 2.0 * (rotation[0].abs().clamp(0.0, 1.0)).acos();
            angle > IDENTITY_THRESHOLD_RAD
        })
        .collect();

    file.seek(SeekFrom::Start(0))?;
    let djmd = mp4::locate_djmd_track(file)?;
    let decoded_count = track.quaternions_wxyz.len();
    let mut new_telemetry_frames: Vec<(u64, Vec<u8>)> = Vec::new();
    let mut quaternion_messages_overwritten = 0u64;
    let mut quaternion_count_in_file = 0usize;

    for (telemetry_frame, (&position, &size)) in djmd
        .telemetry_frame_offsets
        .iter()
        .zip(djmd.telemetry_frame_sizes.iter())
        .enumerate()
    {
        file.seek(SeekFrom::Start(position))?;
        let mut bytes = vec![0u8; size as usize];
        file.read_exact(&mut bytes)?;
        let mut modified = false;

        if telemetry_frame == 0 {
            let (start, end, stored_name) = mp4::find_stored_camera_model_name(&bytes)?;
            let new_name = mp4::repaired_camera_model_name(&stored_name)?;
            if end - start != new_name.len() {
                bail!(
                    "the new camera model name {:?} has length {} against the stored length {}",
                    String::from_utf8_lossy(&new_name),
                    new_name.len(),
                    end - start
                );
            }
            if stored_name != new_name {
                bytes[start..end].copy_from_slice(&new_name);
                modified = true;
            }
        }

        let spans = mp4::quaternion_message_spans(&bytes)?;
        if !spans.is_empty() {
            let mut stored = Vec::with_capacity(spans.len());
            for &span in &spans {
                stored.push(mp4::decode_quaternion_message(&bytes, span)?);
            }
            if stored
                .iter()
                .any(|quaternion| quaternion.iter().any(|value| value.is_nan()))
            {
                bail!("a stored orientation in telemetry frame {telemetry_frame} is not a number");
            }
            let camera_frame = mp4::stored_to_camera_frame(&stored);
            if camera_frame
                .iter()
                .any(|quaternion| quaternion.iter().all(|value| *value == 0.0))
            {
                bail!("a stored orientation in telemetry frame {telemetry_frame} is all zeros");
            }
            let count = spans.len();
            if quaternion_count_in_file + count > decoded_count {
                bail!(
                    "the video holds more than the {decoded_count} decoded orientations, at telemetry frame {telemetry_frame}"
                );
            }
            for (in_frame, quaternion) in camera_frame.iter().enumerate() {
                let file_index = quaternion_count_in_file + in_frame;
                let decoded = track.quaternions_wxyz[file_index];
                let mut dot = 0.0f64;
                for component in 0..4 {
                    dot += quaternion[component] * decoded[component];
                }
                if dot.abs() < MINIMUM_ABSOLUTE_DOT_PRODUCT {
                    bail!(
                        "a stored orientation in telemetry frame {telemetry_frame} differs from its decoded orientation: dot {} against minimum {}",
                        dot.abs(),
                        MINIMUM_ABSOLUTE_DOT_PRODUCT
                    );
                }
            }
            for (in_frame, &span) in spans.iter().enumerate() {
                let file_index = quaternion_count_in_file + in_frame;
                if changed[file_index] {
                    let repaired = hamilton_app(&camera_frame[in_frame], &rotations_at_file_times[file_index]);
                    let stored_repaired = mp4::camera_frame_to_stored(&repaired);
                    mp4::encode_quaternion_message(&mut bytes, span, &stored_repaired);
                    quaternion_messages_overwritten += 1;
                    modified = true;
                }
            }
            quaternion_count_in_file += count;
        }

        if modified {
            new_telemetry_frames.push((position, bytes));
        }
    }

    if quaternion_count_in_file == 0 {
        bail!("the gyro data track holds no orientations");
    }
    if quaternion_count_in_file != decoded_count {
        bail!(
            "the video holds {quaternion_count_in_file} stored orientations against {decoded_count} decoded"
        );
    }

    let video_size_bytes = file.seek(SeekFrom::End(0))?;

    Ok(PreparedWrite {
        new_telemetry_frames,
        quaternion_messages_overwritten,
        video_size_bytes,
    })
}

pub fn write(
    video: &Path,
    repair_result: &RepairResult,
    output_path: &Path,
    force: bool,
) -> Result<WritebackReport> {
    if resolved(video)? == resolved(output_path)? {
        bail!(
            "the output path {} resolves to the input video {}",
            output_path.display(),
            video.display()
        );
    }

    let mut input = File::open(video)
        .with_context(|| format!("cannot open {}", video.display()))?;
    let prepared = prepare_write(&mut input, repair_result, force)?;
    std::fs::copy(video, output_path).with_context(|| {
        format!(
            "cannot copy {} to {}",
            video.display(),
            output_path.display()
        )
    })?;
    {
        let mut repaired_file = std::fs::OpenOptions::new()
            .write(true)
            .open(output_path)
            .with_context(|| format!("cannot open {} for writing", output_path.display()))?;
        for (position, bytes) in &prepared.new_telemetry_frames {
            repaired_file.seek(SeekFrom::Start(*position))?;
            repaired_file.write_all(bytes)?;
        }
    }
    let repaired_size_bytes = std::fs::metadata(output_path)
        .with_context(|| format!("cannot read {}", output_path.display()))?
        .len();
    if repaired_size_bytes != prepared.video_size_bytes {
        bail!(
            "the repaired video is {repaired_size_bytes} bytes against the input's {}",
            prepared.video_size_bytes
        );
    }

    Ok(WritebackReport {
        quaternion_messages_overwritten: prepared.quaternion_messages_overwritten,
        telemetry_frames_rewritten: prepared.new_telemetry_frames.len(),
        video_size_bytes: prepared.video_size_bytes,
    })
}

/// Write the repaired orientations into the video at `video` in place. The caller owns
/// the file and is responsible for deleting it if the write fails.
pub fn write_in_place(
    video: &Path,
    repair_result: &RepairResult,
    force: bool,
) -> Result<WritebackReport> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(video)
        .with_context(|| format!("cannot open {} for writing", video.display()))?;
    write_in_place_stream(&mut file, repair_result, force)
}

/// Write the repaired orientations into an open file in place.
pub fn write_in_place_stream<R: Read + Write + Seek>(
    file: &mut R,
    repair_result: &RepairResult,
    force: bool,
) -> Result<WritebackReport> {
    let prepared = prepare_write(file, repair_result, force)?;
    for (position, bytes) in &prepared.new_telemetry_frames {
        file.seek(SeekFrom::Start(*position))?;
        file.write_all(bytes)?;
    }
    let size_bytes = file.seek(SeekFrom::End(0))?;
    if size_bytes != prepared.video_size_bytes {
        bail!(
            "the repaired video is {size_bytes} bytes against the input's {}",
            prepared.video_size_bytes
        );
    }
    Ok(WritebackReport {
        quaternion_messages_overwritten: prepared.quaternion_messages_overwritten,
        telemetry_frames_rewritten: prepared.new_telemetry_frames.len(),
        video_size_bytes: prepared.video_size_bytes,
    })
}
