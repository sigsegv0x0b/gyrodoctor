//! The merge-and-repair pipeline shared by the command line and the Android library.
//!
//! The inputs are merged losslessly into the output, the output is decoded, detected,
//! repaired, and patched in place. No cache and no temporary joined file are used.

use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::attenuate::RepairParameters;
use crate::decode::{DecodeResult, VideoIdentity, DERIVATION_VERSION};
use crate::detect::{self, DetectionParameters, DetectionResult};
use crate::merge;
use crate::repair::{self, RepairResult};
use crate::write;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum Stage {
    Merge,
    Decode,
    Detection,
    Repair,
    Write,
}

impl Stage {
    pub fn name(self) -> &'static str {
        match self {
            Stage::Merge => "merge",
            Stage::Decode => "decode",
            Stage::Detection => "detection",
            Stage::Repair => "repair",
            Stage::Write => "write",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineOptions {
    pub detect_axes: Option<Vec<String>>,
    pub repair_axes: Option<Vec<String>>,
    pub remove_gyro_motion: Option<Vec<String>>,
    pub force: bool,
    pub do_not_repair: Option<Vec<String>>,
}

impl Default for PipelineOptions {
    fn default() -> Self {
        Self {
            detect_axes: None,
            repair_axes: None,
            remove_gyro_motion: None,
            force: false,
            do_not_repair: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeReport {
    pub files: usize,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub quaternion_count: usize,
    pub rate_sample_count: usize,
    pub extent_count: usize,
    pub quaternion_messages_overwritten: u64,
    pub telemetry_frames_rewritten: usize,
}

/// Merge open input streams into an open output stream, repair the merged file in place,
/// and leave the output open. The caller owns all handles and any cleanup.
pub fn merge_and_repair_streams<F: FnMut(Stage, f64, String)>(
    inputs: &mut [(File, usize)],
    output: &mut File,
    output_name: &str,
    options: &PipelineOptions,
    mut progress: F,
) -> Result<MergeReport> {
    if inputs.len() < 2 {
        bail!("a merge takes at least two input files");
    }
    progress(Stage::Merge, 0.0, "merging inputs".to_string());
    let progress_cell = std::cell::RefCell::new(&mut progress);
    let merge_result = merge::merge_streams(inputs, output, |percent| {
        let mut callback = progress_cell.borrow_mut();
        callback(
            Stage::Merge,
            percent,
            format!("merging {:.0}%", percent * 100.0),
        );
    });
    let input_bytes = merge_result?;
    progress(Stage::Merge, 1.0, "merge complete".to_string());

    let detect_axes = options
        .detect_axes
        .clone()
        .unwrap_or_else(|| detect::AXES.iter().map(|axis| (*axis).to_string()).collect());
    let repair_axes = options
        .repair_axes
        .clone()
        .unwrap_or_else(|| detect_axes.clone());
    let detection_parameters = DetectionParameters::default();
    let mut repair_parameters = RepairParameters::default();
    if let Some(remove) = &options.remove_gyro_motion {
        repair_parameters.remove_gyro_motion = remove.clone();
    }
    if let Some(do_not_repair) = &options.do_not_repair {
        repair_parameters.do_not_repair = do_not_repair.clone();
    }

    let output_bytes = output.metadata()?.len();
    let identity = VideoIdentity {
        video_name: output_name.to_string(),
        video_size_bytes: output_bytes,
        video_mtime_s: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs_f64())
            .unwrap_or(0.0),
        derivation_version: DERIVATION_VERSION,
    };

    progress(Stage::Decode, 0.0, "decoding telemetry".to_string());
    output.seek(SeekFrom::Start(0))?;
    let decode_result: DecodeResult =
        crate::decode::decode_from_stream(output, &identity, options.force)?;
    progress(
        Stage::Decode,
        1.0,
        format!("{} quaternions", decode_result.quaternion_times_s.len()),
    );

    progress(Stage::Detection, 0.0, "detecting noise events".to_string());
    let detection_result: DetectionResult =
        detect::detect(&decode_result, &detect_axes, &detection_parameters)?;
    let extent_count: usize = detection_result
        .axes
        .iter()
        .map(|axis| axis.rows.len())
        .sum();
    progress(Stage::Detection, 1.0, format!("{extent_count} extents"));

    progress(Stage::Repair, 0.0, "repairing orientations".to_string());
    let repair_result: RepairResult = repair::repair(
        &decode_result,
        &detection_result,
        &repair_axes,
        &repair_parameters,
    )?;
    progress(Stage::Repair, 1.0, "repair complete".to_string());

    progress(Stage::Write, 0.0, "writing repaired telemetry".to_string());
    let write_report = write::write_in_place_stream(output, &repair_result, options.force)?;
    progress(Stage::Write, 1.0, "write complete".to_string());

    let output_bytes = output.metadata()?.len();
    Ok(MergeReport {
        files: inputs.len(),
        input_bytes,
        output_bytes,
        quaternion_count: decode_result.quaternion_times_s.len(),
        rate_sample_count: decode_result.recorded_rate_times_s.len(),
        extent_count,
        quaternion_messages_overwritten: write_report.quaternion_messages_overwritten,
        telemetry_frames_rewritten: write_report.telemetry_frames_rewritten,
    })
}

/// Path-based wrapper for the desktop app and the app-storage debug path.
pub fn merge_and_repair<F: FnMut(Stage, f64, String)>(
    inputs: &[PathBuf],
    output: &Path,
    options: &PipelineOptions,
    progress: F,
) -> Result<MergeReport> {
    if inputs.len() < 2 {
        bail!("a merge takes at least two input files");
    }
    if output.exists() && !options.force {
        bail!(
            "the output {} already exists; pass --force to replace it",
            output.display()
        );
    }
    let mut sorted = inputs.to_vec();
    merge::natural_sort(&mut sorted);
    let mut files = Vec::with_capacity(sorted.len());
    for input in &sorted {
        let file = File::open(input)
            .with_context(|| format!("cannot open {}", input.display()))?;
        let size = file
            .metadata()
            .with_context(|| format!("cannot read the size of {}", input.display()))?
            .len();
        files.push((
            file,
            usize::try_from(size).context("an input file is too large for this platform")?,
        ));
    }
    let mut output_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(output)
        .with_context(|| format!("cannot create {}", output.display()))?;
    let name = output
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().to_string());
    let result = merge_and_repair_streams(&mut files, &mut output_file, &name, options, progress);
    if result.is_err() {
        drop(output_file);
        let _ = std::fs::remove_file(output);
    }
    result
}
