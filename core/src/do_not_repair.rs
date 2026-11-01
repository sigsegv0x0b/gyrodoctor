//! The grammar of a position, a range, and a duration, and the do not repair list that
//! resolves against the extents of a detection result.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::detect::{self, DetectionResult};

#[derive(Debug, Clone)]
pub struct Position {
    pub seconds: Option<f64>,
    pub video_frame: Option<i64>,
    pub text: String,
}

impl Position {
    pub fn to_video_frame(&self, video_frame_rate: f64) -> i64 {
        if let Some(frame) = self.video_frame {
            return frame;
        }
        (self.seconds.unwrap_or(0.0) * video_frame_rate).floor() as i64
    }
}

#[derive(Debug, Clone)]
pub struct Range {
    pub first: Position,
    pub last: Position,
}

#[derive(Debug, Clone)]
pub struct Duration {
    pub seconds: Option<f64>,
    pub video_frames: Option<i64>,
    pub text: String,
}

fn digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

fn seconds_field(text: &str) -> Option<f64> {
    if text.is_empty() {
        return None;
    }
    let mut seen_dot = false;
    let mut seen_digit = false;
    for byte in text.bytes() {
        if byte.is_ascii_digit() {
            seen_digit = true;
        } else if byte == b'.' && !seen_dot {
            seen_dot = true;
        } else {
            return None;
        }
    }
    if !seen_digit {
        return None;
    }
    text.parse::<f64>().ok()
}

fn clock_field(text: &str, below_sixty: bool) -> Option<f64> {
    let integer = text.split_once('.').map_or(text, |(integer, _)| integer);
    if integer.len() != 2 || !digits(integer) {
        return None;
    }
    let value = seconds_field(text)?;
    if below_sixty && value >= 60.0 {
        return None;
    }
    Some(value)
}

fn seconds_of(text: &str) -> Option<f64> {
    if let Some(rest) = text.strip_suffix('s') {
        return seconds_field(rest);
    }
    let parts: Vec<&str> = text.split(':').collect();
    match parts.len() {
        2 => {
            let first = parts[0];
            if !digits(first) {
                return None;
            }
            let second = clock_field(parts[1], true)?;
            Some(60.0 * first.parse::<u64>().ok()? as f64 + second)
        }
        3 => {
            let first = parts[0];
            if !digits(first) || parts[1].len() != 2 || !digits(parts[1]) {
                return None;
            }
            let minutes = parts[1].parse::<u64>().ok()?;
            if minutes >= 60 {
                return None;
            }
            let second = clock_field(parts[2], true)?;
            Some(
                3600.0 * first.parse::<u64>().ok()? as f64
                    + 60.0 * minutes as f64
                    + second,
            )
        }
        _ => None,
    }
}

pub fn parse_position(text: &str) -> Result<Position> {
    if let Some(rest) = text.strip_prefix('f') {
        if digits(rest) {
            return Ok(Position {
                seconds: None,
                video_frame: Some(rest.parse()?),
                text: text.to_string(),
            });
        }
    }
    if let Some(seconds) = seconds_of(text) {
        return Ok(Position {
            seconds: Some(seconds),
            video_frame: None,
            text: text.to_string(),
        });
    }
    bail!("{text:?} is not a position: expected seconds with s, minutes:seconds, hours:minutes:seconds, or f followed by a video frame")
}

pub fn parse_range(text: &str) -> Result<Range> {
    let parts: Vec<&str> = text.split('-').collect();
    if parts.len() != 2 || parts.iter().any(|part| part.is_empty()) {
        bail!("{text:?} is not a range: expected two positions with a hyphen between them");
    }
    let first = parse_position(parts[0])?;
    let last = parse_position(parts[1])?;
    if let (Some(first_seconds), Some(last_seconds)) = (first.seconds, last.seconds) {
        if last_seconds < first_seconds {
            bail!("the range {text:?} ends before it starts");
        }
    }
    if let (Some(first_frame), Some(last_frame)) = (first.video_frame, last.video_frame) {
        if last_frame < first_frame {
            bail!("the range {text:?} ends before it starts");
        }
    }
    Ok(Range { first, last })
}

pub fn parse_duration(text: &str) -> Result<Duration> {
    if let Some(rest) = text.strip_suffix('f') {
        if digits(rest) {
            return Ok(Duration {
                seconds: None,
                video_frames: Some(rest.parse()?),
                text: text.to_string(),
            });
        }
    }
    if let Some(seconds) = seconds_of(text) {
        return Ok(Duration {
            seconds: Some(seconds),
            video_frames: None,
            text: text.to_string(),
        });
    }
    bail!("{text:?} is not a duration: expected a time or a count of video frames followed by f")
}

pub fn format_time(seconds: f64) -> String {
    let milliseconds = (seconds * 1000.0).round_ties_even().max(0.0) as u64;
    let mut rest = milliseconds;
    let hours = rest / 3_600_000;
    rest %= 3_600_000;
    let minutes = rest / 60_000;
    rest %= 60_000;
    let whole_seconds = rest / 1000;
    let milliseconds = rest % 1000;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{whole_seconds:02}.{milliseconds:03}")
    } else {
        format!("{minutes}:{whole_seconds:02}.{milliseconds:03}")
    }
}

pub fn reported_video_frame(seconds: f64, video_frame_rate: f64) -> i64 {
    parse_position(&format_time(seconds))
        .map(|position| position.to_video_frame(video_frame_rate))
        .unwrap_or(0)
}

#[derive(Debug, Clone)]
pub struct DoNotRepairElement {
    pub text: String,
    pub file: Option<PathBuf>,
    pub line: Option<usize>,
    pub identifier: Option<String>,
    pub axis_letter: Option<char>,
    pub range: Option<Range>,
}

#[derive(Debug, Clone)]
pub struct Warning {
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct DoNotRepairList {
    pub elements: Vec<DoNotRepairElement>,
    pub warnings: Vec<Warning>,
}

fn element(text: &str, file: Option<&Path>, line: Option<usize>) -> Result<DoNotRepairElement> {
    if !text.contains('-') {
        let bytes = text.as_bytes();
        if bytes.len() < 2
            || !matches!(bytes[0], b'P' | b'Y' | b'R')
            || !bytes[1..].iter().all(u8::is_ascii_digit)
        {
            bail!("{text:?} is not an identifier: expected an axis letter P, Y, or R followed by a video frame");
        }
        return Ok(DoNotRepairElement {
            text: text.to_string(),
            file: file.map(Path::to_path_buf),
            line,
            identifier: Some(text.to_string()),
            axis_letter: None,
            range: None,
        });
    }
    let axis_letter = text
        .chars()
        .next()
        .filter(|letter| matches!(letter, 'P' | 'Y' | 'R'));
    let range_text = if axis_letter.is_some() {
        &text[1..]
    } else {
        text
    };
    let range = parse_range(range_text)
        .map_err(|error| anyhow::anyhow!("{text:?}: {error}"))?;
    Ok(DoNotRepairElement {
        text: text.to_string(),
        file: file.map(Path::to_path_buf),
        line,
        identifier: None,
        axis_letter,
        range: Some(range),
    })
}

fn file_text(path: &Path) -> Result<String> {
    if !path.exists() {
        bail!("the do not repair file {} does not exist", path.display());
    }
    if path.is_dir() {
        bail!("the do not repair file {} is a directory", path.display());
    }
    let bytes = std::fs::read(path)
        .map_err(|error| anyhow::anyhow!("cannot read {}: {error}", path.display()))?;
    let bytes = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        &bytes[3..]
    } else {
        &bytes[..]
    };
    String::from_utf8(bytes.to_vec())
        .map_err(|_| anyhow::anyhow!("the do not repair file {} is not UTF-8", path.display()))
}

pub fn read_list(listed: Option<&str>, path: Option<&Path>) -> Result<DoNotRepairList> {
    let mut given: Vec<(String, Option<PathBuf>, Option<usize>)> = Vec::new();
    if let Some(listed) = listed {
        for text in listed.split(',') {
            given.push((text.to_string(), None, None));
        }
    }
    if let Some(path) = path {
        for (number, line) in file_text(path)?.split('\n').enumerate() {
            let text = line.trim();
            if text.is_empty() || text.starts_with('#') {
                continue;
            }
            given.push((text.to_string(), Some(path.to_path_buf()), Some(number + 1)));
        }
    }
    let mut list = DoNotRepairList::default();
    for (text, file, line) in given {
        if text.is_empty() {
            continue;
        }
        match element(&text, file.as_deref(), line) {
            Ok(element) => list.elements.push(element),
            Err(error) => list.warnings.push(Warning {
                text: format!(
                    "ignored do not repair element {}{}: {error}",
                    text,
                    file.as_deref()
                        .map(|file| format!(" at {}:{}", file.display(), line.unwrap_or(0)))
                        .unwrap_or_default()
                ),
            }),
        }
    }
    Ok(list)
}

fn axis_letter(element: &DoNotRepairElement) -> Option<char> {
    if let Some(identifier) = &element.identifier {
        return identifier.chars().next();
    }
    element.axis_letter
}

fn axis_of(letter: char) -> &'static str {
    match letter {
        'P' => "pitch",
        'Y' => "yaw",
        'R' => "roll",
        _ => "unknown",
    }
}

pub struct Resolution {
    pub excluded: Vec<String>,
    pub warnings: Vec<Warning>,
}

pub fn resolve(
    elements: &[DoNotRepairElement],
    detection: &DetectionResult,
    video_frame_rate: f64,
    removed_axes: &[String],
) -> Resolution {
    let removed_letters: Vec<char> = removed_axes
        .iter()
        .filter_map(|axis| axis.chars().next().map(|letter| letter.to_ascii_uppercase()))
        .collect();
    let resolved: Vec<bool> = elements
        .iter()
        .map(|element| {
            axis_letter(element)
                .map(|letter| !removed_letters.contains(&letter))
                .unwrap_or(true)
        })
        .collect();
    let video_frame_ranges: Vec<Option<(Option<char>, i64, i64)>> = elements
        .iter()
        .zip(resolved.iter())
        .map(|(element, resolved)| {
            if !resolved {
                return None;
            }
            element.range.as_ref().map(|range| {
                (
                    element.axis_letter,
                    range.first.to_video_frame(video_frame_rate),
                    range.last.to_video_frame(video_frame_rate),
                )
            })
        })
        .collect();

    let mut extents: Vec<(String, String, i64, i64)> = Vec::new();
    for axis in &detection.axes {
        for row in &axis.rows {
            extents.push((
                detect::extent_identifier(&axis.axis, row.peak_video_frame),
                axis.axis.clone(),
                reported_video_frame(row.start_s, video_frame_rate),
                reported_video_frame(row.end_s, video_frame_rate),
            ));
        }
    }
    let known: std::collections::BTreeSet<String> =
        extents.iter().map(|(identifier, ..)| identifier.clone()).collect();
    let named: std::collections::BTreeSet<String> = elements
        .iter()
        .zip(resolved.iter())
        .filter_map(|(element, resolved)| {
            resolved.then_some(element.identifier.clone()).flatten()
        })
        .collect();
    let mut excluded: std::collections::BTreeSet<String> = named
        .iter()
        .filter(|identifier| known.contains(*identifier))
        .cloned()
        .collect();
    for (axis_letter, first, last) in video_frame_ranges.iter().flatten() {
        if last < first {
            continue;
        }
        for (identifier, axis, extent_first, extent_last) in &extents {
            let on_axis = axis_letter.is_none_or(|letter| {
                axis.chars()
                    .next()
                    .is_some_and(|axis_letter| axis_letter.to_ascii_uppercase() == letter)
            });
            if on_axis && *extent_first <= *last && *extent_last >= *first {
                excluded.insert(identifier.clone());
            }
        }
    }

    let mut warnings = Vec::new();
    for ((element, resolved), video_frame_range) in elements
        .iter()
        .zip(resolved.iter())
        .zip(video_frame_ranges.iter())
    {
        if !resolved {
            let letter = axis_letter(element).unwrap_or('?');
            warnings.push(Warning {
                text: format!(
                    "ignored do not repair element {} on the removed axis {}",
                    element.text,
                    axis_of(letter)
                ),
            });
        } else if let Some(identifier) = &element.identifier {
            if !known.contains(identifier) {
                warnings.push(Warning {
                    text: format!(
                        "ignored do not repair identifier {identifier}, which names no extent"
                    ),
                });
            }
        } else if let Some((_, first, last)) = video_frame_range {
            if last < first {
                warnings.push(Warning {
                    text: format!(
                        "ignored do not repair element {}: the range ends before it starts",
                        element.text
                    ),
                });
            }
        }
    }
    Resolution {
        excluded: excluded.into_iter().collect(),
        warnings,
    }
}
