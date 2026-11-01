//! The read of the gyro data track out of the container: every quaternion in
//! file order, converted into the camera frame of reference, with its time.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::mp4::{self, DjmdTrack, Quat};

#[derive(Debug, Clone)]
pub struct GyroDataTrack {
    pub quaternion_times_s: Vec<f64>,
    pub quaternions_wxyz: Vec<Quat>,
    pub quaternion_count_per_telemetry_frame: Vec<u64>,
    pub handler_name: String,
}

struct TelemetryFrame {
    frame_timestamp: i64,
    offset: f64,
    quaternions: Vec<Quat>,
}

fn frame_rates_and_stored_camera_model_name(bytes: &[u8]) -> Result<(f64, f64, Vec<u8>)> {
    let whole = Some((0, bytes.len()));
    let clip_meta = mp4::submessage_span(bytes, whole, 1)?;
    let sensor_fps = mp4::float_field(
        bytes,
        mp4::submessage_span(bytes, clip_meta, 11)?,
        1,
    )?;
    let stream_meta = mp4::submessage_span(bytes, whole, 2)?;
    let framerate = mp4::float_field(
        bytes,
        mp4::submessage_span(bytes, stream_meta, 3)?,
        3,
    )?;
    let (Some(sensor_fps), Some(framerate)) = (sensor_fps, framerate) else {
        let missing = if sensor_fps.is_none() {
            "sensor_fps"
        } else {
            "framerate"
        };
        bail!("the first telemetry frame has no {missing}");
    };
    let (_, _, name) = mp4::find_stored_camera_model_name(bytes)?;
    Ok((sensor_fps as f64, framerate as f64, name))
}

fn telemetry_frame(bytes: &[u8]) -> Result<TelemetryFrame> {
    let whole = Some((0, bytes.len()));
    let frame_meta = mp4::submessage_span(bytes, whole, 3)?;
    let header = mp4::submessage_span(bytes, frame_meta, 1)?;
    let imu_frame_meta = mp4::submessage_span(bytes, frame_meta, 3)?;
    let orientation_record = mp4::submessage_span(bytes, imu_frame_meta, 2)?;
    let frame_timestamp = mp4::varint_field(bytes, header, 2)?.unwrap_or(0) as i64;
    let offset = mp4::float_field(bytes, orientation_record, 4)?.unwrap_or(0.0) as f64;
    let spans = mp4::quaternion_message_spans(bytes)?;
    let mut quaternions = Vec::with_capacity(spans.len());
    for span in spans {
        quaternions.push(mp4::decode_quaternion_message(bytes, span)?);
    }
    Ok(TelemetryFrame {
        frame_timestamp,
        offset,
        quaternions,
    })
}

fn quaternion_times(
    frame_timestamp: &[i64],
    offset: &[f64],
    counts: &[u64],
    sensor_fps: f64,
    framerate: f64,
) -> Vec<f64> {
    let total: usize = counts.iter().map(|count| *count as usize).sum();
    let mut times = Vec::with_capacity(total);
    let mut start = 0u64;
    let first_timestamp = frame_timestamp.first().copied().unwrap_or(0);
    let frame_period_ms = 1000.0 / sensor_fps;
    for (frame_index, &count) in counts.iter().enumerate() {
        let timestamp_from_first_ms = (frame_timestamp[frame_index] - first_timestamp) as f64 / 1000.0;
        let offset = offset[frame_index];
        let count_f = count as f64;
        for position in 0..count {
            let position_f = position as f64;
            let gyro_sensor_clock_ms = timestamp_from_first_ms
                + ((position_f - offset) / count_f) * frame_period_ms;
            times.push(gyro_sensor_clock_ms / (framerate / sensor_fps) / 1000.0);
        }
        start += count;
        let _ = start;
    }
    times
}

pub fn already_repaired_test(path: &Path) -> Result<()> {
    let mut file = File::open(path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    already_repaired_test_from_stream(&mut file)
}

pub fn already_repaired_test_from_stream<R: Read + Seek>(file: &mut R) -> Result<()> {
    let track = mp4::locate_djmd_track(file)?;
    let (Some(&offset), Some(&size)) = (
        track.telemetry_frame_offsets.first(),
        track.telemetry_frame_sizes.first(),
    ) else {
        bail!("the video's gyro data track holds no telemetry frames");
    };
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0u8; size as usize];
    file.read_exact(&mut bytes)?;
    let (_, _, stored_name) = mp4::find_stored_camera_model_name(&bytes)?;
    if stored_name.ends_with(mp4::MODEL_NAME_MARKER) {
        bail!(
            "the video is already marked as repaired: camera model name {:?}",
            String::from_utf8_lossy(&stored_name)
        );
    }
    Ok(())
}

pub fn read_gyro_data_track(path: &Path, force: bool) -> Result<GyroDataTrack> {
    let mut file = File::open(path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    read_gyro_data_track_from_stream(&mut file, force)
}

pub fn read_gyro_data_track_from_stream<R: Read + Seek>(
    file: &mut R,
    force: bool,
) -> Result<GyroDataTrack> {
    let track = mp4::locate_djmd_track(file)?;
    let mut frames = Vec::with_capacity(track.telemetry_frame_offsets.len());
    let mut sensor_fps = 0.0;
    let mut framerate = 0.0;
    for (position, size) in track
        .telemetry_frame_offsets
        .iter()
        .zip(track.telemetry_frame_sizes.iter())
    {
        file.seek(SeekFrom::Start(*position))?;
        let mut bytes = vec![0u8; *size as usize];
        file.read_exact(&mut bytes)?;
        if frames.is_empty() {
            let (frame_sensor_fps, frame_rate, stored_name) =
                frame_rates_and_stored_camera_model_name(&bytes)?;
            sensor_fps = frame_sensor_fps;
            framerate = frame_rate;
            if !force && stored_name.ends_with(mp4::MODEL_NAME_MARKER) {
                bail!(
                    "the video is already marked as repaired: camera model name {:?}",
                    String::from_utf8_lossy(&stored_name)
                );
            }
        }
        frames.push(telemetry_frame(&bytes)?);
    }

    let counts: Vec<u64> = frames
        .iter()
        .map(|frame| frame.quaternions.len() as u64)
        .collect();
    if counts.iter().sum::<u64>() == 0 {
        bail!("the video's gyro data track holds no orientations");
    }
    let frame_timestamp: Vec<i64> = frames.iter().map(|frame| frame.frame_timestamp).collect();
    let offset: Vec<f64> = frames.iter().map(|frame| frame.offset).collect();
    let mut stored = Vec::with_capacity(counts.iter().sum::<u64>() as usize);
    for frame in &frames {
        stored.extend_from_slice(&frame.quaternions);
    }

    let mut owner = 0usize;
    let mut count_in_frame = 0u64;
    for (quaternion_index, quaternion) in stored.iter().enumerate() {
        while owner < counts.len() && count_in_frame >= counts[owner] {
            owner += 1;
            count_in_frame = 0;
        }
        if quaternion.iter().any(|value| value.is_nan()) {
            bail!(
                "a stored orientation in telemetry frame {owner} is not a number"
            );
        }
        let _ = quaternion_index;
        count_in_frame += 1;
    }

    let quaternions = mp4::stored_to_camera_frame(&stored);
    for (owner, quaternion) in quaternions.iter().enumerate() {
        if quaternion.iter().all(|value| *value == 0.0) {
            let mut frame_index = 0usize;
            let mut running = 0u64;
            for (index, count) in counts.iter().enumerate() {
                if (owner as u64) < running + count {
                    frame_index = index;
                    break;
                }
                running += count;
            }
            bail!(
                "a stored orientation in telemetry frame {frame_index} is all zeros"
            );
        }
    }

    Ok(GyroDataTrack {
        quaternion_times_s: quaternion_times(
            &frame_timestamp,
            &offset,
            &counts,
            sensor_fps,
            framerate,
        ),
        quaternions_wxyz: quaternions,
        quaternion_count_per_telemetry_frame: counts,
        handler_name: track.handler_name,
    })
}

pub fn read_telemetry_frame<R: Read + Seek>(
    file: &mut R,
    track: &DjmdTrack,
    index: usize,
) -> Result<Vec<u8>> {
    let offset = track.telemetry_frame_offsets[index];
    let size = track.telemetry_frame_sizes[index];
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0u8; size as usize];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}
