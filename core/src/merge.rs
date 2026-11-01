//! The lossless merge of MP4 chunks with `mp4-merge`, the same crate Gyroflow uses.
//!
//! The chunks are merged straight into the caller's output stream. No temporary joined
//! file is created here; the caller opens the final output and owns its cleanup.

use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// A natural filename order: runs of digits compare by value, so `_0010` follows `_0009`.
pub fn natural_sort(paths: &mut [PathBuf]) {
    paths.sort_by(|a, b| {
        let a_name = a
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().to_string());
        let b_name = b
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().to_string());
        human_compare(&a_name, &b_name).then_with(|| a.cmp(b))
    });
}

fn runs(text: &str) -> Vec<(bool, &str)> {
    let mut result = Vec::new();
    let mut start = 0;
    let mut digits = false;
    for (index, character) in text.char_indices() {
        let is_digit = character.is_ascii_digit();
        if index == 0 {
            digits = is_digit;
            continue;
        }
        if is_digit != digits {
            result.push((digits, &text[start..index]));
            start = index;
            digits = is_digit;
        }
    }
    if start < text.len() {
        result.push((digits, &text[start..]));
    }
    result
}

fn human_compare(a: &str, b: &str) -> std::cmp::Ordering {
    let a_runs = runs(a);
    let b_runs = runs(b);
    for (a_run, b_run) in a_runs.iter().zip(b_runs.iter()) {
        let order = match (a_run, b_run) {
            ((true, a_digits), (true, b_digits)) => {
                let a_trimmed = a_digits.trim_start_matches('0');
                let b_trimmed = b_digits.trim_start_matches('0');
                a_trimmed
                    .len()
                    .cmp(&b_trimmed.len())
                    .then_with(|| a_trimmed.cmp(b_trimmed))
            }
            ((false, a_text), (false, b_text)) => a_text.cmp(b_text),
            ((true, _), (false, _)) => std::cmp::Ordering::Less,
            ((false, _), (true, _)) => std::cmp::Ordering::Greater,
        };
        if order != std::cmp::Ordering::Equal {
            return order;
        }
    }
    a_runs.len().cmp(&b_runs.len())
}

fn absolute(path: &Path) -> Result<PathBuf> {
    Ok(std::fs::canonicalize(path).or_else(|_| std::path::absolute(path))?)
}

/// Validate that the inputs are files with a gyro data track and that the output is not
/// one of them. Full track compatibility is checked by `mp4-merge` itself.
pub fn validate(inputs: &[PathBuf], output: &Path) -> Result<()> {
    if inputs.len() < 2 {
        bail!("a merge takes at least two input files");
    }
    let output_absolute = absolute(output)?;
    for input in inputs {
        if !input.is_file() {
            bail!("the input {} is not a file", input.display());
        }
        if absolute(input)? == output_absolute {
            bail!(
                "the output path {} is one of the files being merged",
                output.display()
            );
        }
        let mut file = File::open(input)
            .with_context(|| format!("cannot open {}", input.display()))?;
        crate::mp4::locate_djmd_track(&mut file)
            .with_context(|| format!("{} has no readable gyro data track", input.display()))?;
    }
    Ok(())
}

/// Merge `inputs` directly into `output` and return the total input size in bytes.
///
/// The output is truncated first. If `output` exists and `force` is false, the merge is
/// refused before anything is written. No temporary file is created.
pub fn merge<F: Fn(f64)>(
    inputs: &[PathBuf],
    output: &Path,
    force: bool,
    progress: F,
) -> Result<u64> {
    validate(inputs, output)?;
    if output.exists() && !force {
        bail!(
            "the output {} already exists; pass --force to replace it",
            output.display()
        );
    }
    let mut files: Vec<(File, usize)> = Vec::with_capacity(inputs.len());
    let mut total = 0u64;
    for input in inputs {
        let file = File::open(input)
            .with_context(|| format!("cannot open {}", input.display()))?;
        let size = file
            .metadata()
            .with_context(|| format!("cannot read the size of {}", input.display()))?
            .len();
        total += size;
        files.push((
            file,
            usize::try_from(size)
                .context("an input file is too large for this platform")?,
        ));
    }
    let mut output_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(output)
        .with_context(|| format!("cannot create {}", output.display()))?;
    mp4_merge::join_file_streams(&mut files, &mut output_file, progress)
        .with_context(|| format!("cannot merge into {}", output.display()))?;
    output_file
        .sync_all()
        .with_context(|| format!("cannot flush {}", output.display()))?;
    Ok(total)
}


/// Merge open input streams into an open output stream. The output is truncated first.
/// The caller owns all handles and is responsible for cleanup.
pub fn merge_streams<F: Fn(f64)>(
    files: &mut [(File, usize)],
    output: &mut File,
    progress: F,
) -> Result<u64> {
    if files.len() < 2 {
        bail!("a merge takes at least two input files");
    }
    output.set_len(0)?;
    output.seek(SeekFrom::Start(0))?;
    let total = files.iter().map(|(_, size)| *size as u64).sum();
    mp4_merge::join_file_streams(files, &mut *output, progress)?;
    output.sync_all()?;
    Ok(total)
}
