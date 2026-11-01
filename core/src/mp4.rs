//! The container layer: MP4 box walking, the DJI telemetry track, Protocol
//! Buffers field reads, and the stored/camera-frame quaternion basis transform.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};

use anyhow::{bail, Context, Result};

pub use crate::quat::{Quat, Vec3};
use crate::quat::hamilton_app;

pub const MODEL_NAME_MARKER: &[u8] = b"-Dr.";
const DJI_PREFIX: &[u8] = b"DJI ";
const GYRO_HANDLER_NAMES: [&str; 2] = ["DJI meta", "CAM meta"];
const CONTAINERS: [&[u8; 4]; 7] = [b"moov", b"trak", b"mdia", b"minf", b"stbl", b"udta", b"edts"];

const QUATERNION_MESSAGE_LENGTH: usize = 20;
const QUATERNION_MESSAGE_TAGS: [u8; 4] = [0x0D, 0x15, 0x1D, 0x25];

const BASIS_LEFT: Quat = [0.0, 0.0, 1.0, 0.0];
const BASIS_RIGHT: Quat = [0.5, -0.5, -0.5, 0.5];
const BASIS_LEFT_INVERSE: Quat = [0.0, 0.0, -1.0, 0.0];
const BASIS_RIGHT_INVERSE: Quat = [0.5, 0.5, 0.5, -0.5];

#[derive(Debug, Clone)]
pub struct BoxRef {
    pub path: String,
    pub position: u64,
    pub size: u64,
    pub header_size: u64,
}

#[derive(Debug, Clone, Default)]
pub struct TrackInfo {
    pub handler_type: Option<String>,
    pub handler_name: Option<String>,
    pub stsd_fourcc: Option<String>,
    pub sizes: Option<Vec<u64>>,
    pub offsets: Option<Vec<u64>>,
    pub stsc: Vec<(u32, u32, u32)>,
    pub mdhd_timescale: Option<u32>,
    pub mdhd_duration: Option<u64>,
    pub stts_total_samples: u64,
    pub stts_total_duration: u64,
}

#[derive(Debug, Clone)]
pub struct DjmdTrack {
    pub telemetry_frame_offsets: Vec<u64>,
    pub telemetry_frame_sizes: Vec<u64>,
    pub handler_name: String,
}

#[derive(Debug, Clone, Copy)]
pub struct ProtoField {
    pub number: u32,
    pub wire_type: u8,
    pub value: u64,
    pub span: Option<(usize, usize)>,
}

fn read_box_header<R: Read + Seek>(file: &mut R, position: u64, end: u64) -> Result<Option<(u64, u64, [u8; 4])>> {
    if position + 8 > end {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(position))?;
    let mut header = [0u8; 8];
    file.read_exact(&mut header)?;
    let mut size = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as u64;
    let box_type = [header[4], header[5], header[6], header[7]];
    let mut header_size = 8u64;
    if size == 1 {
        let mut extended = [0u8; 8];
        file.read_exact(&mut extended)?;
        size = u64::from_be_bytes(extended);
        header_size = 16;
    } else if size == 0 {
        size = end - position;
    }
    if size < header_size || position + size > end {
        return Ok(None);
    }
    Ok(Some((size, header_size, box_type)))
}

fn walk_boxes<R: Read + Seek>(file: &mut R, start: u64, end: u64, path: &str, out: &mut Vec<BoxRef>) -> Result<()> {
    let mut position = start;
    while position + 8 <= end {
        let Some((size, header_size, box_type)) = read_box_header(file, position, end)? else {
            break;
        };
        let name = String::from_utf8_lossy(&box_type).to_string();
        let child_path = format!("{path}/{name}");
        out.push(BoxRef {
            path: child_path.clone(),
            position,
            size,
            header_size,
        });
        if CONTAINERS.contains(&&box_type) {
            walk_boxes(file, position + header_size, position + size, &child_path, out)?;
        }
        position += size;
    }
    Ok(())
}

pub fn walk_file<R: Read + Seek>(file: &mut R) -> Result<Vec<BoxRef>> {
    let end = file.seek(SeekFrom::End(0))?;
    let mut boxes = Vec::new();
    walk_boxes(file, 0, end, "", &mut boxes)?;
    Ok(boxes)
}

fn read_body<R: Read + Seek>(file: &mut R, box_ref: &BoxRef, whole: bool) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(box_ref.position + box_ref.header_size))?;
    let length = (box_ref.size - box_ref.header_size) as usize;
    let length = if whole { length } else { length.min(1 << 16) };
    let mut body = vec![0u8; length];
    file.read_exact(&mut body)?;
    Ok(body)
}

fn be_u32(body: &[u8], offset: usize) -> Result<u32> {
    if offset + 4 > body.len() {
        bail!("a box body is shorter than its fields state");
    }
    Ok(u32::from_be_bytes([
        body[offset],
        body[offset + 1],
        body[offset + 2],
        body[offset + 3],
    ]))
}

fn be_u64(body: &[u8], offset: usize) -> Result<u64> {
    if offset + 8 > body.len() {
        bail!("a box body is shorter than its fields state");
    }
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&body[offset..offset + 8]);
    Ok(u64::from_be_bytes(bytes))
}

fn parse_tracks<R: Read + Seek>(file: &mut R, boxes: &[BoxRef]) -> Result<Vec<TrackInfo>> {
    let mut tracks: Vec<TrackInfo> = Vec::new();
    let mut current: Option<usize> = None;
    for box_ref in boxes {
        let leaf = box_ref.path.rsplit('/').next().unwrap_or("");
        if leaf == "trak" {
            tracks.push(TrackInfo::default());
            current = Some(tracks.len() - 1);
        }
        let Some(index) = current else { continue };
        let whole = matches!(leaf, "stsz" | "stco" | "co64" | "stts");
        let body = read_body(file, box_ref, whole)?;
        let track = &mut tracks[index];
        match leaf {
            "hdlr" => {
                if body.len() >= 12 {
                    track.handler_type =
                        Some(String::from_utf8_lossy(&body[8..12]).to_string());
                }
                if body.len() > 24 {
                    let name = body[24..].split(|b| *b == 0).next().unwrap_or(&[]);
                    track.handler_name =
                        Some(String::from_utf8_lossy(name).to_string());
                }
            }
            "stsd" => {
                if body.len() >= 16 {
                    track.stsd_fourcc =
                        Some(String::from_utf8_lossy(&body[12..16]).to_string());
                }
            }
            "stsz" => {
                let fixed = be_u32(&body, 4)? as u64;
                let count = be_u32(&body, 8)? as usize;
                if fixed != 0 {
                    track.sizes = Some(vec![fixed; count]);
                } else {
                    let mut sizes = Vec::with_capacity(count);
                    for i in 0..count {
                        sizes.push(be_u32(&body, 12 + 4 * i)? as u64);
                    }
                    track.sizes = Some(sizes);
                }
            }
            "stco" => {
                let count = be_u32(&body, 4)? as usize;
                let mut offsets = Vec::with_capacity(count);
                for i in 0..count {
                    offsets.push(be_u32(&body, 8 + 4 * i)? as u64);
                }
                track.offsets = Some(offsets);
            }
            "co64" => {
                let count = be_u32(&body, 4)? as usize;
                let mut offsets = Vec::with_capacity(count);
                for i in 0..count {
                    offsets.push(be_u64(&body, 8 + 8 * i)?);
                }
                track.offsets = Some(offsets);
            }
            "stsc" => {
                let count = be_u32(&body, 4)? as usize;
                let mut rows = Vec::with_capacity(count);
                for i in 0..count {
                    let offset = 8 + 12 * i;
                    rows.push((
                        be_u32(&body, offset)?,
                        be_u32(&body, offset + 4)?,
                        be_u32(&body, offset + 8)?,
                    ));
                }
                track.stsc = rows;
            }
            "mdhd" => {
                if body.len() >= 4 {
                    let version = body[0];
                    if version == 1 {
                        if body.len() >= 32 {
                            track.mdhd_timescale = Some(be_u32(&body, 20)?);
                            track.mdhd_duration = Some(be_u64(&body, 24)?);
                        }
                    } else if body.len() >= 20 {
                        track.mdhd_timescale = Some(be_u32(&body, 12)?);
                        track.mdhd_duration = Some(be_u32(&body, 16)? as u64);
                    }
                }
            }
            "stts" => {
                let count = be_u32(&body, 4)? as usize;
                let mut total_samples = 0u64;
                let mut total_duration = 0u64;
                for i in 0..count {
                    let offset = 8 + 8 * i;
                    let sample_count = be_u32(&body, offset)? as u64;
                    let sample_delta = be_u32(&body, offset + 4)? as u64;
                    total_samples += sample_count;
                    total_duration += sample_count * sample_delta;
                }
                track.stts_total_samples = total_samples;
                track.stts_total_duration = total_duration;
            }
            _ => {}
        }
    }
    Ok(tracks)
}

pub fn locate_djmd_track<R: Read + Seek>(file: &mut R) -> Result<DjmdTrack> {
    let boxes = walk_file(file)?;
    let tracks = parse_tracks(file, &boxes)?;
    for track in &tracks {
        if track.handler_type.as_deref() == Some("meta")
            && track
                .handler_name
                .as_deref()
                .is_some_and(|name| GYRO_HANDLER_NAMES.contains(&name))
        {
            let handler_name = track.handler_name.clone().unwrap_or_default();
            if track.stsd_fourcc.as_deref() != Some("djmd") {
                bail!(
                    "the gyro data track has handler name {:?} and format {:?}, not djmd",
                    handler_name,
                    track.stsd_fourcc
                );
            }
            if track.stsc.is_empty() {
                bail!("the gyro data track has no sample to chunk table");
            }
            if track
                .stsc
                .iter()
                .any(|(_, _, sample_description)| *sample_description != 1)
            {
                bail!(
                    "gyro data track stored with a sample description other than the first: {:?}",
                    track.stsc
                );
            }
            let (Some(sizes), Some(chunks)) = (&track.sizes, &track.offsets) else {
                bail!("the gyro data track has no sample size or chunk offset table");
            };
            let mut offsets = Vec::with_capacity(sizes.len());
            let mut sample = 0usize;
            let mut row = 0usize;
            for (chunk_index, &chunk_offset) in chunks.iter().enumerate() {
                while row + 1 < track.stsc.len()
                    && track.stsc[row + 1].0 as usize <= chunk_index + 1
                {
                    row += 1;
                }
                let per_chunk = track.stsc[row].1 as usize;
                let mut offset = chunk_offset;
                for _ in 0..per_chunk {
                    if sample >= sizes.len() {
                        bail!(
                            "the sample to chunk table names more samples than the {} sample sizes it holds",
                            sizes.len()
                        );
                    }
                    offsets.push(offset);
                    offset += sizes[sample];
                    sample += 1;
                }
            }
            if sample != sizes.len() {
                bail!(
                    "the sample to chunk table names {sample} samples against {} sample sizes",
                    sizes.len()
                );
            }
            return Ok(DjmdTrack {
                telemetry_frame_offsets: offsets,
                telemetry_frame_sizes: sizes.clone(),
                handler_name,
            });
        }
    }
    let names: Vec<String> = tracks
        .iter()
        .filter_map(|track| track.handler_name.clone())
        .collect();
    bail!("the video has no gyro data track; handler names: {:?}", names)
}

/// The video frame rate of the first video stream, as PyAV's `average_rate`
/// reports it. None when there is no video stream or no timing table.
pub fn video_frame_rate<R: Read + Seek>(file: &mut R) -> Result<Option<f64>> {
    let boxes = walk_file(file)?;
    let tracks = parse_tracks(file, &boxes)?;
    for track in &tracks {
        if track.handler_type.as_deref() != Some("vide") {
            continue;
        }
        if let (Some(timescale), true) = (track.mdhd_timescale, track.stts_total_duration > 0) {
            let samples = if track.stts_total_samples > 0 {
                track.stts_total_samples
            } else {
                track.sizes.as_ref().map_or(0, |sizes| sizes.len() as u64)
            };
            if samples > 0 && timescale > 0 {
                return Ok(Some(
                    samples as f64
                        / (track.stts_total_duration as f64 / timescale as f64),
                ));
            }
        }
        if let (Some(timescale), Some(duration)) = (track.mdhd_timescale, track.mdhd_duration) {
            let samples = track.sizes.as_ref().map_or(0, |sizes| sizes.len() as u64);
            if samples > 0 && timescale > 0 && duration > 0 {
                return Ok(Some(samples as f64 / (duration as f64 / timescale as f64)));
            }
        }
        return Ok(None);
    }
    Ok(None)
}

pub fn read_varint(bytes: &[u8], mut position: usize, end: usize) -> Result<(u64, usize)> {
    let mut value = 0u64;
    let mut shift = 0u32;
    loop {
        if position >= end {
            bail!("a telemetry frame ends inside a varint");
        }
        let byte = bytes[position];
        value |= ((byte & 0x7F) as u64) << shift;
        position += 1;
        if byte & 0x80 == 0 {
            return Ok((value, position));
        }
        shift += 7;
        if shift >= 64 {
            bail!("a telemetry frame holds a varint longer than 64 bits");
        }
    }
}

pub fn protobuf_fields(bytes: &[u8], start: usize, end: usize) -> Result<Vec<ProtoField>> {
    let mut fields = Vec::new();
    let mut position = start;
    while position < end {
        let (key, next) = read_varint(bytes, position, end)?;
        position = next;
        let number = (key >> 3) as u32;
        let wire_type = (key & 7) as u8;
        match wire_type {
            0 => {
                let (value, next) = read_varint(bytes, position, end)?;
                position = next;
                fields.push(ProtoField {
                    number,
                    wire_type,
                    value,
                    span: None,
                });
            }
            1 => {
                if position + 8 > end {
                    bail!("a telemetry frame ends inside a fixed64 field");
                }
                fields.push(ProtoField {
                    number,
                    wire_type,
                    value: 0,
                    span: Some((position, position + 8)),
                });
                position += 8;
            }
            2 => {
                let (length, next) = read_varint(bytes, position, end)?;
                position = next;
                let stop = position
                    .checked_add(length as usize)
                    .filter(|stop| *stop <= end)
                    .context("a telemetry frame ends inside a length-delimited field")?;
                fields.push(ProtoField {
                    number,
                    wire_type,
                    value: 0,
                    span: Some((position, stop)),
                });
                position = stop;
            }
            5 => {
                if position + 4 > end {
                    bail!("a telemetry frame ends inside a fixed32 field");
                }
                fields.push(ProtoField {
                    number,
                    wire_type,
                    value: 0,
                    span: Some((position, position + 4)),
                });
                position += 4;
            }
            other => bail!("a telemetry frame holds wire type {other}"),
        }
    }
    Ok(fields)
}

pub fn submessage_span(
    bytes: &[u8],
    span: Option<(usize, usize)>,
    field_number: u32,
) -> Result<Option<(usize, usize)>> {
    let Some((start, end)) = span else {
        return Ok(None);
    };
    for field in protobuf_fields(bytes, start, end)? {
        if field.number == field_number && field.wire_type == 2 {
            return Ok(field.span);
        }
    }
    Ok(None)
}

pub fn varint_field(bytes: &[u8], span: Option<(usize, usize)>, field_number: u32) -> Result<Option<u64>> {
    let Some((start, end)) = span else {
        return Ok(None);
    };
    for field in protobuf_fields(bytes, start, end)? {
        if field.number == field_number && field.wire_type == 0 {
            return Ok(Some(field.value));
        }
    }
    Ok(None)
}

pub fn float_field(bytes: &[u8], span: Option<(usize, usize)>, field_number: u32) -> Result<Option<f32>> {
    let Some((start, end)) = span else {
        return Ok(None);
    };
    for field in protobuf_fields(bytes, start, end)? {
        if field.number == field_number && field.wire_type == 5 {
            let (value_start, _) = field.span.unwrap();
            return Ok(Some(f32::from_le_bytes([
                bytes[value_start],
                bytes[value_start + 1],
                bytes[value_start + 2],
                bytes[value_start + 3],
            ])));
        }
    }
    Ok(None)
}

pub fn quaternion_message_spans(bytes: &[u8]) -> Result<Vec<(usize, usize)>> {
    let frame_meta = submessage_span(bytes, Some((0, bytes.len())), 3)?;
    let Some(frame_meta) = frame_meta else {
        return Ok(Vec::new());
    };
    let imu_frame_meta = submessage_span(bytes, Some(frame_meta), 3)?;
    let Some(imu_frame_meta) = imu_frame_meta else {
        return Ok(Vec::new());
    };
    let orientation_record = submessage_span(bytes, Some(imu_frame_meta), 2)?;
    let Some(orientation_record) = orientation_record else {
        return Ok(Vec::new());
    };
    let mut spans = Vec::new();
    for field in protobuf_fields(bytes, orientation_record.0, orientation_record.1)? {
        if field.number == 3 && field.wire_type == 2 {
            if let Some(span) = field.span {
                spans.push(span);
            }
        }
    }
    Ok(spans)
}

pub fn find_stored_camera_model_name(bytes: &[u8]) -> Result<(usize, usize, Vec<u8>)> {
    let clip_meta = submessage_span(bytes, Some((0, bytes.len())), 1)?
        .context("the first telemetry frame has no clip_meta")?;
    let header = submessage_span(bytes, Some(clip_meta), 1)?
        .context("the first telemetry frame has no clip_meta_header")?;
    for field in protobuf_fields(bytes, header.0, header.1)? {
        if field.number == 10 && field.wire_type == 2 {
            let (start, end) = field.span.unwrap();
            return Ok((start, end, bytes[start..end].to_vec()));
        }
    }
    bail!("the first telemetry frame has no product_name")
}

pub fn decode_quaternion_message(bytes: &[u8], span: (usize, usize)) -> Result<Quat> {
    let (start, end) = span;
    if end - start != QUATERNION_MESSAGE_LENGTH {
        bail!(
            "a stored orientation message is {} bytes, not 20",
            end - start
        );
    }
    let tags = [
        bytes[start],
        bytes[start + 5],
        bytes[start + 10],
        bytes[start + 15],
    ];
    if tags != QUATERNION_MESSAGE_TAGS {
        bail!("a stored orientation message has tags {tags:?}, not [13, 21, 29, 37]");
    }
    let mut values = [0f32; 4];
    for (k, value) in values.iter_mut().enumerate() {
        let offset = start + 1 + 5 * k;
        *value = f32::from_le_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]);
    }
    Ok([
        values[0] as f64,
        values[1] as f64,
        values[2] as f64,
        values[3] as f64,
    ])
}

pub fn encode_quaternion_message(bytes: &mut [u8], span: (usize, usize), quaternion: &Quat) {
    let start = span.0;
    for (k, value) in quaternion.iter().enumerate() {
        let offset = start + 1 + 5 * k;
        bytes[offset..offset + 4].copy_from_slice(&(*value as f32).to_le_bytes());
    }
}

pub fn stored_to_camera_frame(quaternions: &[Quat]) -> Vec<Quat> {
    quaternions
        .iter()
        .map(|q| hamilton_app(&BASIS_LEFT, &hamilton_app(q, &BASIS_RIGHT)))
        .collect()
}

pub fn camera_frame_to_stored(quaternion: &Quat) -> Quat {
    hamilton_app(
        &BASIS_LEFT_INVERSE,
        &hamilton_app(quaternion, &BASIS_RIGHT_INVERSE),
    )
}

pub fn repaired_camera_model_name(stored: &[u8]) -> Result<Vec<u8>> {
    let mut name = stored;
    if name.ends_with(MODEL_NAME_MARKER) {
        name = &name[..name.len() - MODEL_NAME_MARKER.len()];
    }
    if name.starts_with(DJI_PREFIX) {
        name = &name[DJI_PREFIX.len()..];
    }
    if name.len() == stored.len() {
        bail!(
            "the stored camera model name {:?} carries neither a DJI prefix nor the repaired marker",
            String::from_utf8_lossy(stored)
        );
    }
    let mut result = name.to_vec();
    result.extend_from_slice(MODEL_NAME_MARKER);
    Ok(result)
}

pub fn file_stamp(path: &std::path::Path) -> Result<FileStamp> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("cannot read the file stamp of {}", path.display()))?;
    let mtime_ns = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| anyhow::anyhow!("file modification time is before the Unix epoch"))?
        .as_nanos() as u64;
    Ok(FileStamp {
        name: path
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().to_string()),
        size: metadata.len(),
        mtime_ns,
    })
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct FileStamp {
    pub name: String,
    pub size: u64,
    pub mtime_ns: u64,
}

pub type FileStamps = BTreeMap<String, FileStamp>;
