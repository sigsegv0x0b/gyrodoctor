//! The cache of the three computed results: the decode result, the detection result, and
//! the repair result, with the run record, the extent tables, the inventory, and the
//! write record.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::decode::{self, DecodeResult, DERIVATION_VERSION, PERIOD_TEST_TOLERANCE_S};
use crate::detect::{self, DetectionParameters, DetectionResult};
use crate::mp4::FileStamp;
use crate::attenuate::RepairParameters;
use crate::repair::RepairResult;

pub const DECODE_RESULT_PARTS: usize = 17;

#[derive(Debug, Clone)]
pub struct CachePaths {
    pub directory: PathBuf,
    pub stem: String,
}

impl CachePaths {
    pub fn new(root: &Path, video: &Path) -> Self {
        let stem = video
            .file_stem()
            .map_or_else(String::new, |stem| stem.to_string_lossy().to_string());
        Self {
            directory: root.join(&stem),
            stem,
        }
    }

    pub fn decode(&self) -> PathBuf {
        self.directory.join(format!("{}-decode-result.bin", self.stem))
    }

    pub fn detection(&self) -> PathBuf {
        self.directory
            .join(format!("{}-detection-result.bin", self.stem))
    }

    pub fn detection_record(&self) -> PathBuf {
        self.directory
            .join(format!("{}-detection-run-record.json", self.stem))
    }

    pub fn extent_table(&self, axis: &str) -> PathBuf {
        self.directory
            .join(format!("{}-{axis}-extent-table.csv", self.stem))
    }

    pub fn repair(&self) -> PathBuf {
        self.directory
            .join(format!("{}-repair-result.bin", self.stem))
    }

    pub fn repair_record(&self) -> PathBuf {
        self.directory
            .join(format!("{}-repair-record.json", self.stem))
    }

    pub fn write_record(&self) -> PathBuf {
        self.directory
            .join(format!("{}-write-record.json", self.stem))
    }

    pub fn readme(&self) -> PathBuf {
        self.directory.join("README.md")
    }
}

fn write_binary<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = bincode::serialize(value).context("cannot encode a cached result")?;
    let partial = path.with_file_name(format!(
        "{}.partial",
        path.file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().to_string())
    ));
    std::fs::write(&partial, bytes)
        .with_context(|| format!("cannot write {}", partial.display()))?;
    std::fs::rename(&partial, path)
        .with_context(|| format!("cannot replace {}", path.display()))?;
    Ok(())
}

fn read_binary<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    bincode::deserialize(&bytes).with_context(|| format!("cannot decode {}", path.display()))
}

pub fn header_test(result: &DecodeResult, video: &Path, path: &Path) -> Result<()> {
    let expected = decode::video_identity(video)?;
    if result.derivation_version != expected.derivation_version {
        bail!(
            "{} was cached by derivation version {} and this build derives version {}",
            path.display(),
            result.derivation_version,
            expected.derivation_version
        );
    }
    let mut differs = Vec::new();
    if result.video_name != expected.video_name {
        differs.push("video_name");
    }
    if result.video_size_bytes != expected.video_size_bytes {
        differs.push("video_size_bytes");
    }
    if result.video_mtime_s != expected.video_mtime_s {
        differs.push("video_mtime_s");
    }
    if !differs.is_empty() {
        bail!(
            "{} names a video that has changed since it was cached ({})",
            path.display(),
            differs.join(", ")
        );
    }
    Ok(())
}

pub fn period_test(result: &DecodeResult) -> Result<()> {
    let boundaries = decode::group_boundary_times(result);
    let derived = decode::derive_period(&boundaries, result.measurement_rate_hz)?;
    if (result.period_s - derived).abs() > PERIOD_TEST_TOLERANCE_S {
        bail!(
            "the stored period {:.6} us differs from the derived period {:.6} us",
            result.period_s * 1e6,
            derived * 1e6
        );
    }
    Ok(())
}

pub fn rate_series_test(result: &DecodeResult) -> Result<()> {
    let count = result.recorded_rate_deg_s.len();
    if result.normalized_rate_deg_s.len() != count {
        bail!("the recorded rate and the Normalized Rate differ in length");
    }
    let boundaries = decode::group_boundary_times(result);
    if boundaries.len() != count + 1 {
        bail!("the group boundaries do not hold one more entry than the rate samples");
    }
    if result.recorded_rate_times_s.len() != count {
        bail!("the rate sample times do not hold one entry per rate sample");
    }
    Ok(())
}

pub fn read_decode(path: &Path, video: &Path) -> Result<DecodeResult> {
    let result: DecodeResult = read_binary(path)?;
    header_test(&result, video, path)?;
    period_test(&result)?;
    rate_series_test(&result)?;
    Ok(result)
}

pub fn write_decode(result: &DecodeResult, paths: &CachePaths) -> Result<(PathBuf, PathBuf)> {
    std::fs::create_dir_all(&paths.directory)
        .with_context(|| format!("cannot create {}", paths.directory.display()))?;
    let path = paths.decode();
    write_binary(&path, result)?;
    let readme = update_readme(&path, result)?;
    Ok((path, readme))
}

pub fn same_detection_parameters(
    result: &DetectionResult,
    axes: &[String],
    parameters: &DetectionParameters,
) -> Result<bool> {
    let axes = detect::selected_axes(axes)?;
    if result.axes.iter().map(|axis| &axis.axis).ne(axes.iter()) {
        return Ok(false);
    }
    for axis in &result.axes {
        if axis.settings != detect::axis_settings(parameters, &axis.axis)? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn read_detection(path: &Path) -> Result<DetectionResult> {
    read_binary(path)
}

pub fn write_detection(
    result: &DetectionResult,
    paths: &CachePaths,
    decode_stamp: FileStamp,
) -> Result<BTreeMap<String, PathBuf>> {
    std::fs::create_dir_all(&paths.directory)
        .with_context(|| format!("cannot create {}", paths.directory.display()))?;
    write_binary(&paths.detection(), result)?;
    let mut tables = BTreeMap::new();
    for axis in &result.axes {
        let path = paths.extent_table(&axis.axis);
        std::fs::write(&path, extent_table_text(result, &axis.axis))
            .with_context(|| format!("cannot write {}", path.display()))?;
        tables.insert(axis.axis.clone(), path);
    }
    let record = run_record(result, &paths.detection(), &tables, decode_stamp);
    std::fs::write(
        paths.detection_record(),
        serde_json::to_string_pretty(&record)? + "\n",
    )
    .with_context(|| format!("cannot write {}", paths.detection_record().display()))?;
    Ok(tables)
}

pub fn detection_stamps(paths: &CachePaths) -> Result<BTreeMap<String, FileStamp>> {
    let text = std::fs::read_to_string(paths.detection_record())
        .with_context(|| format!("cannot read {}", paths.detection_record().display()))?;
    let record: serde_json::Value = serde_json::from_str(&text)?;
    let stamps = record
        .get("file_stamps")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    serde_json::from_value(stamps).context("the detection run record holds no file stamps")
}

pub fn same_repair_parameters(
    record: &serde_json::Value,
    axes: &[String],
    parameters: &RepairParameters,
) -> bool {
    let expected_axes: Vec<&str> = detect::AXES
        .iter()
        .copied()
        .filter(|axis| axes.iter().any(|wanted| wanted == axis))
        .collect();
    let stored_axes = record
        .pointer("/run/repair_axes")
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let stored_parameters = record
        .pointer("/run/repair_parameters")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let expected_parameters = serde_json::to_value(parameters).unwrap_or(serde_json::Value::Null);
    stored_axes == expected_axes && stored_parameters == expected_parameters
}

pub fn read_repair(path: &Path) -> Result<RepairResult> {
    read_binary(path)
}

pub fn repair_record(paths: &CachePaths) -> Result<serde_json::Value> {
    let text = std::fs::read_to_string(paths.repair_record())
        .with_context(|| format!("cannot read {}", paths.repair_record().display()))?;
    Ok(serde_json::from_str(&text)?)
}

pub fn write_repair(
    result: &RepairResult,
    paths: &CachePaths,
    decode_stamp: FileStamp,
    detection_stamp: FileStamp,
) -> Result<()> {
    std::fs::create_dir_all(&paths.directory)
        .with_context(|| format!("cannot create {}", paths.directory.display()))?;
    write_binary(&paths.repair(), result)?;
    let mut record: serde_json::Value =
        serde_json::from_str(&result.repair_record).context("the repair record is not JSON")?;
    if let Some(object) = record.as_object_mut() {
        object.insert(
            "file_stamps".to_string(),
            serde_json::json!({
                "decode_result": decode_stamp,
                "detection_result": detection_stamp,
            }),
        );
    }
    let repair_stamp = crate::mp4::file_stamp(&paths.repair())?;
    if let Some(stamps) = record
        .get_mut("file_stamps")
        .and_then(|stamps| stamps.as_object_mut())
    {
        stamps.insert(
            "repair_result".to_string(),
            serde_json::to_value(repair_stamp)?,
        );
    }
    std::fs::write(
        paths.repair_record(),
        serde_json::to_string_pretty(&record)? + "\n",
    )
    .with_context(|| format!("cannot write {}", paths.repair_record().display()))?;
    Ok(())
}

pub fn repair_stamps(paths: &CachePaths) -> Result<BTreeMap<String, FileStamp>> {
    let record = repair_record(paths)?;
    let stamps = record
        .get("file_stamps")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    serde_json::from_value(stamps).context("the repair record holds no file stamps")
}

pub fn file_stamps_test(
    directory: &Path,
    stamps: &BTreeMap<String, FileStamp>,
    keys: &[&str],
) -> Result<()> {
    for key in keys {
        let stamp = stamps
            .get(*key)
            .with_context(|| format!("the cached result holds no file stamp for {key}"))?;
        let path = directory.join(&stamp.name);
        let metadata = std::fs::metadata(&path)
            .with_context(|| format!("the stamped file {} is missing", path.display()))?;
        let mtime_ns = metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| anyhow::anyhow!("the stamped file predates the Unix epoch"))?
            .as_nanos() as u64;
        if metadata.len() != stamp.size || mtime_ns != stamp.mtime_ns {
            bail!(
                "the stamped file {} changed since the shell recorded it",
                path.display()
            );
        }
    }
    Ok(())
}

pub fn write_write_record(
    paths: &CachePaths,
    repaired_video: &Path,
    repair_result_path: &Path,
) -> Result<()> {
    std::fs::create_dir_all(&paths.directory)
        .with_context(|| format!("cannot create {}", paths.directory.display()))?;
    let repaired_video = std::path::absolute(repaired_video)?;
    let record = serde_json::json!({
        "repaired_video": repaired_video,
        "file_stamps": {
            "repaired_video": crate::mp4::file_stamp(&repaired_video)?,
            "repair_result": crate::mp4::file_stamp(repair_result_path)?,
        },
    });
    std::fs::write(
        paths.write_record(),
        serde_json::to_string_pretty(&record)? + "\n",
    )
    .with_context(|| format!("cannot write {}", paths.write_record().display()))?;
    Ok(())
}

pub fn extent_table_text(result: &DetectionResult, axis: &str) -> String {
    let mut text = String::from(
        "identifier,first_sample,last_sample,rate_sample_count,start_s,end_s,duration_ms,found_by,classification,peak_sample,peak_video_frame,peak_rate_deg_s\n",
    );
    if let Some(detection) = result.axes.iter().find(|detection| detection.axis == axis) {
        for row in &detection.rows {
            text.push_str(&format!(
                "{},{},{},{},{:.6},{:.6},{:.4},{},{},{},{:.3},{:.4}\n",
                detect::extent_identifier(axis, row.peak_video_frame),
                row.first_sample,
                row.last_sample,
                row.rate_sample_count,
                row.start_s,
                row.end_s,
                row.duration_ms,
                row.found_by,
                row.classification,
                row.peak_sample,
                row.peak_video_frame,
                row.peak_rate_deg_s
            ));
        }
    }
    text
}

fn run_record(
    result: &DetectionResult,
    result_path: &Path,
    extent_table_paths: &BTreeMap<String, PathBuf>,
    decode_stamp: FileStamp,
) -> serde_json::Value {
    let mut axes = serde_json::Map::new();
    for detection in &result.axes {
        let passes: Vec<serde_json::Value> = detect::PASSES
            .iter()
            .enumerate()
            .map(|(index, pass)| {
                serde_json::json!({
                    "pass": index + 1,
                    "name": pass.name(),
                    "classification": pass.classification(),
                    "robust_standard_deviation_deg_s": detection.pass_robust_standard_deviation_deg_s[index],
                    "threshold": detection.pass_threshold[index],
                    "extent_count": detection.pass_extent_count[index],
                    "share_of_rate_samples_inside_extents": detection.pass_share_of_rate_samples_inside_extents[index],
                })
            })
            .collect();
        axes.insert(
            detection.axis.clone(),
            serde_json::json!({
                "passes": passes,
                "share_of_rate_samples_inside_extents": detection.share_of_rate_samples_inside_extents,
                "extent_table": extent_table_paths
                    .get(&detection.axis)
                    .and_then(|path| path.file_name())
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default(),
            }),
        );
    }
    serde_json::json!({
        "detection_result": result_path.file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_default(),
        "file_stamps": { "decode_result": decode_stamp },
        "axes": axes,
    })
}

fn spaced(count: usize) -> String {
    let text = count.to_string();
    let mut out = String::new();
    let bytes = text.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 && (bytes.len() - index) % 3 == 0 {
            out.push(' ');
        }
        out.push(*byte as char);
    }
    out
}

fn inventory_row(path: &Path, result: &DecodeResult) -> (String, String) {
    let quaternion_count = result.quaternion_times_s.len();
    let group_count = result.group_opening_quaternion_index.len();
    let span_s = result
        .consecutive_quaternion_rate_times_s
        .last()
        .copied()
        .unwrap_or(0.0)
        - result
            .consecutive_quaternion_rate_times_s
            .first()
            .copied()
            .unwrap_or(0.0);
    let kept: Vec<_> = result
        .quaternions_wxyz
        .iter()
        .zip(result.kept_after_time_collisions.iter())
        .filter_map(|(quaternion, kept)| kept.then_some(*quaternion))
        .collect();
    let repeated = if kept.len() > 1 {
        decode::repeats_previous_quaternion(&kept)
            .iter()
            .filter(|repeat| **repeat)
            .count() as f64
            / (kept.len() - 1) as f64
    } else {
        0.0
    };
    let size = std::fs::metadata(path).map(|metadata| metadata.len()).unwrap_or(0);
    let cells = [
        format!("`{}`", path.file_name().unwrap_or_default().to_string_lossy()),
        format!("`{}`", result.video_name),
        format!("{:.4} fps", result.video_frame_rate),
        format!(
            "{} over {:.1} s ({:.4} Hz, {:.4} per video frame)",
            spaced(quaternion_count),
            span_s,
            result.quaternion_rate_hz,
            result.quaternions_per_video_frame
        ),
        format!(
            "{} ({:.4} Hz)",
            spaced(group_count),
            result.measurement_rate_hz
        ),
        format!("{:.4} %", repeated * 100.0),
        format!("{:.1} MB", size as f64 / 1e6),
    ];
    (
        path.file_name().unwrap_or_default().to_string_lossy().to_string(),
        format!("| {} |", cells.join(" | ")),
    )
}

const INVENTORY_HEADING: &str = "## Inventory";
const INVENTORY_COLUMNS: [&str; 7] = [
    "file",
    "video",
    "video frame rate",
    "quaternions",
    "measurement groups",
    "identical consecutive quaternions",
    "size",
];

const NEW_README_PREAMBLE: &str = "# Decode results\n\nOne `.bin` per video holding the quaternions of its gyro data track as the camera\nstored them, and every series derived from them: the recorded rate, the Normalized\nRate, and the period T. Its header carries the identity of the video and the derivation\nversion.\n\nThe decode stage writes the inventory below each time it writes a decode result. A row\nwhose `.bin` no longer exists is dropped when the next decode result is written.\n\n";

fn inventory_table(rows: &BTreeMap<String, String>, directory: &Path) -> Vec<String> {
    let kept: BTreeMap<&String, &String> = rows
        .iter()
        .filter(|(name, _)| directory.join(name).exists())
        .collect();
    let ordered: Vec<&String> = kept.values().copied().collect();
    let total_mb: f64 = kept
        .keys()
        .map(|name| {
            std::fs::metadata(directory.join(name))
                .map(|metadata| metadata.len())
                .unwrap_or(0) as f64
        })
        .sum::<f64>()
        / 1e6;
    let blanks = vec![""; INVENTORY_COLUMNS.len() - 2].join(" | ");
    let results = if kept.len() == 1 {
        "decode result"
    } else {
        "decode results"
    };
    let mut lines = vec![
        format!("| {} |", INVENTORY_COLUMNS.join(" | ")),
        format!("|{}|", INVENTORY_COLUMNS.map(|_| "---").join("|")),
    ];
    lines.extend(ordered.into_iter().cloned());
    lines.push(format!(
        "| **{} {}** | {} | **{:.1} MB** |",
        kept.len(),
        results,
        blanks,
        total_mb
    ));
    lines
}

pub fn update_readme(path: &Path, result: &DecodeResult) -> Result<PathBuf> {
    let directory = path.parent().context("the decode result has no directory")?;
    let readme = directory.join("README.md");
    let text = if readme.exists() {
        std::fs::read_to_string(&readme)
            .with_context(|| format!("cannot read {}", readme.display()))?
    } else {
        NEW_README_PREAMBLE.to_string()
    };
    let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    let mut heading = lines
        .iter()
        .position(|line| line.trim() == INVENTORY_HEADING);
    if heading.is_none() {
        while lines.last().is_some_and(|line| line.trim().is_empty()) {
            lines.pop();
        }
        lines.push(String::new());
        lines.push(INVENTORY_HEADING.to_string());
        lines.push(String::new());
        heading = Some(lines.len() - 2);
    }
    let heading = heading.unwrap();
    let start = lines
        .iter()
        .enumerate()
        .skip(heading)
        .find(|(_, line)| line.starts_with('|'))
        .map(|(index, _)| index);
    let (start, stop, mut rows) = match start {
        Some(start) => {
            let mut stop = start;
            while stop + 1 < lines.len() && lines[stop + 1].starts_with('|') {
                stop += 1;
            }
            stop += 1;
            let mut rows = BTreeMap::new();
            for line in &lines[start + 2..stop] {
                let cells: Vec<&str> = line.trim().trim_matches('|').split('|').collect();
                if let Some(name) = cells.first() {
                    rows.insert(name.trim().trim_matches('`').to_string(), line.clone());
                }
            }
            (start, stop, rows)
        }
        None => {
            let mut start = heading + 1;
            while start < lines.len() && lines[start].trim().is_empty() {
                start += 1;
            }
            (start, start, BTreeMap::new())
        }
    };
    let (key, row) = inventory_row(path, result);
    rows.insert(key, row);
    let table = inventory_table(&rows, directory);
    lines.splice(start..stop, table);
    let mut text = lines.join("\n");
    if !text.ends_with('\n') {
        text.push('\n');
    }
    std::fs::write(&readme, text).with_context(|| format!("cannot write {}", readme.display()))?;
    Ok(readme)
}

pub fn decode_result_stamp(paths: &CachePaths) -> Result<FileStamp> {
    crate::mp4::file_stamp(&paths.decode())
}

pub fn detection_result_stamp(paths: &CachePaths) -> Result<FileStamp> {
    crate::mp4::file_stamp(&paths.detection())
}

pub fn repair_result_stamp(paths: &CachePaths) -> Result<FileStamp> {
    crate::mp4::file_stamp(&paths.repair())
}

pub fn version() -> u32 {
    DERIVATION_VERSION
}
