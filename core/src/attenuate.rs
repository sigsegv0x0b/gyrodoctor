//! The ceiling attenuator of the repair stage: reduce each extent until the largest
//! departure in its interior from its centerline is at or below the ceiling of its
//! classification.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::fit;

pub const RAISED_COSINE: &str = "raised-cosine";
pub const LINEAR: &str = "linear";
pub const FADE_SHAPES: [&str; 2] = [RAISED_COSINE, LINEAR];

pub const LOCAL_MEDIAN: &str = "local-median";
pub const CHORD: &str = "chord";
pub const ZERO: &str = "zero";
pub const CENTERLINES: [&str; 3] = [LOCAL_MEDIAN, CHORD, ZERO];

pub const LARGE_AMPLITUDE_OSCILLATION: &str = "large amplitude oscillation";
pub const MID_AMPLITUDE_BURST: &str = "mid-amplitude burst";
pub const IMPULSE_EVENT: &str = "impulse event";

pub const AT_A_VIDEO_EDGE: &str = "at a video edge";
pub const ON_THE_DO_NOT_REPAIR_LIST: &str = "on the do not repair list";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairParameters {
    pub repair_oscillation_centerline: String,
    pub repair_burst_centerline: String,
    pub repair_impulse_centerline: String,
    pub repair_oscillation_ceiling: f64,
    pub repair_burst_ceiling: f64,
    pub repair_impulse_ceiling: f64,
    pub repair_fade: f64,
    pub repair_fade_shape: String,
    pub repair_edges: bool,
    pub remove_gyro_motion: Vec<String>,
    pub do_not_repair: Vec<String>,
}

impl Default for RepairParameters {
    fn default() -> Self {
        Self {
            repair_oscillation_centerline: CHORD.to_string(),
            repair_burst_centerline: LOCAL_MEDIAN.to_string(),
            repair_impulse_centerline: LOCAL_MEDIAN.to_string(),
            repair_oscillation_ceiling: 0.0,
            repair_burst_ceiling: 2.0,
            repair_impulse_ceiling: 0.5,
            repair_fade: 4.0,
            repair_fade_shape: RAISED_COSINE.to_string(),
            repair_edges: false,
            remove_gyro_motion: Vec::new(),
            do_not_repair: Vec::new(),
        }
    }
}

impl RepairParameters {
    pub fn centerline_of(&self, classification: &str) -> Result<&str> {
        Ok(match classification {
            LARGE_AMPLITUDE_OSCILLATION => &self.repair_oscillation_centerline,
            MID_AMPLITUDE_BURST => &self.repair_burst_centerline,
            IMPULSE_EVENT => &self.repair_impulse_centerline,
            other => bail!(
                "extents carry classifications this attenuator has no treatment for: {other}"
            ),
        })
    }

    pub fn ceiling_of(&self, classification: &str) -> Result<f64> {
        Ok(match classification {
            LARGE_AMPLITUDE_OSCILLATION => self.repair_oscillation_ceiling,
            MID_AMPLITUDE_BURST => self.repair_burst_ceiling,
            IMPULSE_EVENT => self.repair_impulse_ceiling,
            other => bail!(
                "extents carry classifications this attenuator has no treatment for: {other}"
            ),
        })
    }

    pub fn with_infinite_ceilings(&self) -> Self {
        let mut parameters = self.clone();
        parameters.repair_oscillation_ceiling = f64::INFINITY;
        parameters.repair_burst_ceiling = f64::INFINITY;
        parameters.repair_impulse_ceiling = f64::INFINITY;
        parameters
    }
}

#[derive(Debug, Clone)]
pub struct ExtentInput {
    pub first_sample: usize,
    pub last_sample: usize,
    pub peak_sample: usize,
    pub classification: String,
    pub identifier: String,
    pub excluded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtentEntry {
    pub first_sample: usize,
    pub last_sample: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_sample_count: Option<usize>,
    pub scaled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub gain: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak_sample: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak_departure_deg_s: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clean_robust_standard_deviation_deg_s: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak_in_clean_robust_standard_deviations_before: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak_in_clean_robust_standard_deviations_after: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak_in_clean_robust_standard_deviations_after_interior: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fade_samples: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fade_used_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fade_shortened: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub energy_ratio: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AttenuatorStatistics {
    pub extent_count: usize,
    pub attenuated_count: usize,
    pub passed_through_count: usize,
    pub skipped_count: usize,
    pub excluded_count: usize,
    pub shortened_fade_count: usize,
    pub repair_oscillation_centerline: String,
    pub repair_burst_centerline: String,
    pub repair_impulse_centerline: String,
    pub repair_oscillation_ceiling: f64,
    pub repair_burst_ceiling: f64,
    pub repair_impulse_ceiling: f64,
    pub gain_min: Option<f64>,
    pub gain_median: Option<f64>,
    pub gain_max: Option<f64>,
    pub peak_in_clean_robust_standard_deviations_before_median: Option<f64>,
    pub energy_ratio_median: Option<f64>,
    pub share_of_rate_samples_inside_scaled_extents: f64,
}

#[derive(Debug, Clone)]
pub struct AttenuationResult {
    pub repaired_rate_deg_s: Vec<f64>,
    pub centerline: Vec<f64>,
    pub gains: Vec<f64>,
    pub extents: Vec<(usize, usize)>,
    pub extent_entries: Vec<ExtentEntry>,
    pub statistics: AttenuatorStatistics,
}

pub fn departure_gain(
    peak_departure: f64,
    clean_robust_standard_deviation: f64,
    ceiling: f64,
) -> (f64, f64) {
    if !clean_robust_standard_deviation.is_finite() || clean_robust_standard_deviation <= 0.0 {
        return (
            1.0,
            if peak_departure > 0.0 {
                f64::INFINITY
            } else {
                0.0
            },
        );
    }
    let departure =
        peak_departure / clean_robust_standard_deviation;
    if departure <= ceiling {
        return (1.0, departure);
    }
    (ceiling / departure, departure)
}

fn extent_centerline(
    choice: &str,
    rate: &[f64],
    local_median: &[f64],
    times: &[f64],
    start: usize,
    end: usize,
) -> Result<Vec<f64>> {
    if choice == LOCAL_MEDIAN {
        return Ok(local_median[start..end].to_vec());
    }
    if choice == ZERO {
        return Ok(vec![0.0; end - start]);
    }
    if choice != CHORD {
        bail!("centerline must be one of {CENTERLINES:?}, not {choice:?}");
    }
    let span_times = &times[start..end];
    let first_value = rate[start];
    let last_value = rate[end - 1];
    let width = span_times[span_times.len() - 1] - span_times[0];
    if width <= 0.0 {
        return Ok(vec![0.5 * (first_value + last_value); end - start]);
    }
    Ok(span_times
        .iter()
        .map(|time| first_value + (last_value - first_value) * (time - span_times[0]) / width)
        .collect())
}

fn extent_gain(
    rate: &[f64],
    centerline: &[f64],
    clean_rsd: f64,
    start: usize,
    end: usize,
    ceiling: f64,
) -> (f64, f64, f64, f64) {
    let departure: Vec<f64> = rate[start..end]
        .iter()
        .zip(centerline.iter())
        .map(|(rate, centerline)| (rate - centerline).abs())
        .collect();
    let mut largest_index = 0usize;
    let mut largest_ratio = f64::NEG_INFINITY;
    let mut any = false;
    if clean_rsd.is_finite() && clean_rsd > 0.0 {
        any = true;
        for (index, value) in departure.iter().enumerate() {
            let ratio = value / clean_rsd;
            if index == 0 || ratio > largest_ratio {
                largest_ratio = ratio;
                largest_index = index;
            }
        }
    }
    let largest = if any {
        departure[largest_index]
    } else {
        departure
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max)
    };
    let rsd_at_largest = if any { clean_rsd } else { f64::NAN };
    let (gain, departure_in_rsd) = departure_gain(largest, rsd_at_largest, ceiling);
    (gain, departure_in_rsd, largest, rsd_at_largest)
}

/// Python's `round`: round half to even.
fn python_round(value: f64) -> f64 {
    value.round_ties_even()
}

pub fn gain_at_each_rate_sample(
    rate_sample_count: usize,
    gain: f64,
    fade_samples: usize,
    fade_shape: &str,
) -> Result<Vec<f64>> {
    if !FADE_SHAPES.contains(&fade_shape) {
        bail!("fade_shape must be one of {FADE_SHAPES:?}, not {fade_shape:?}");
    }
    let mut gains = vec![gain; rate_sample_count];
    if fade_samples == 0 {
        if rate_sample_count > 0 {
            gains[0] = 1.0;
            gains[rate_sample_count - 1] = 1.0;
        }
        return Ok(gains);
    }
    let fade = fade_samples.min(rate_sample_count);
    let mut entry = Vec::with_capacity(fade);
    for index in 0..fade {
        let position = (index as f64 + 1.0) / (fade_samples as f64 + 1.0);
        let ramp = if fade_shape == RAISED_COSINE {
            0.5 * (1.0 + (std::f64::consts::PI * position).cos())
        } else {
            1.0 - position
        };
        entry.push(gain + (1.0 - gain) * ramp);
    }
    for index in 0..fade {
        gains[index] = entry[index];
        gains[rate_sample_count - fade + index] = entry[fade - 1 - index];
    }
    if rate_sample_count > 0 {
        gains[0] = 1.0;
        gains[rate_sample_count - 1] = 1.0;
    }
    Ok(gains)
}

fn median_of(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut copy = values.to_vec();
    Some(fit::median(&mut copy))
}

#[allow(clippy::too_many_arguments)]
pub fn attenuate(
    normalized_rate_deg_s: &[f64],
    measurement_rate_hz: f64,
    detected_extents: &[(usize, usize)],
    peak_samples: &[usize],
    classifications: &[String],
    local_median_deg_s: &[f64],
    clean_robust_standard_deviation_deg_s: f64,
    rate_sample_times_s: &[f64],
    excluded: &[bool],
    parameters: &RepairParameters,
) -> Result<AttenuationResult> {
    let rate = normalized_rate_deg_s;
    let times = rate_sample_times_s;
    let local_median = local_median_deg_s;
    let rate_sample_count = rate.len();
    if local_median.len() != rate_sample_count {
        bail!(
            "the local median holds {} rate samples against a Normalized Rate of {}",
            local_median.len(),
            rate_sample_count
        );
    }
    if detected_extents.len() != peak_samples.len()
        || detected_extents.len() != classifications.len()
        || detected_extents.len() != excluded.len()
    {
        bail!("the extents, peaks, classifications, and exclusions differ in count");
    }
    let clean_rsd = clean_robust_standard_deviation_deg_s;

    let mut order: Vec<usize> = (0..detected_extents.len()).collect();
    order.sort_by_key(|&index| detected_extents[index].0);

    let fade_samples_nominal =
        python_round(parameters.repair_fade * 1e-3 * measurement_rate_hz) as i64;
    let mut repaired = rate.to_vec();
    let mut effective_centerline = local_median.to_vec();
    let mut gains = Vec::with_capacity(detected_extents.len());
    let mut extent_entries = Vec::with_capacity(detected_extents.len());
    let mut used_extents = Vec::with_capacity(detected_extents.len());

    for &index in &order {
        let (first_sample, last_sample) = detected_extents[index];
        let peak_sample = peak_samples[index];
        let classification = &classifications[index];
        let is_excluded = excluded[index];
        let start = first_sample;
        let end = last_sample + 1;
        let span = end - start;

        let touches_edge = start == 0 || end == rate_sample_count;
        let reason = if is_excluded {
            Some(ON_THE_DO_NOT_REPAIR_LIST)
        } else if touches_edge && !parameters.repair_edges {
            Some(AT_A_VIDEO_EDGE)
        } else {
            None
        };
        if let Some(reason) = reason {
            gains.push(1.0);
            extent_entries.push(ExtentEntry {
                first_sample: start,
                last_sample: end - 1,
                rate_sample_count: None,
                scaled: false,
                reason: Some(reason.to_string()),
                gain: 1.0,
                peak_sample: None,
                peak_departure_deg_s: None,
                clean_robust_standard_deviation_deg_s: None,
                peak_in_clean_robust_standard_deviations_before: None,
                peak_in_clean_robust_standard_deviations_after: None,
                peak_in_clean_robust_standard_deviations_after_interior: None,
                fade_samples: None,
                fade_used_ms: None,
                fade_shortened: None,
                energy_ratio: None,
            });
            used_extents.push((start, end - 1));
            continue;
        }

        let centerline = extent_centerline(
            parameters.centerline_of(classification)?,
            rate,
            local_median,
            times,
            start,
            end,
        )?;
        let (gain, departure_in_rsd, peak_departure, rsd_at_largest) =
            extent_gain(rate, &centerline, clean_rsd, start, end, parameters.ceiling_of(classification)?);
        let deviation: Vec<f64> = rate[start..end]
            .iter()
            .zip(centerline.iter())
            .map(|(rate, centerline)| rate - centerline)
            .collect();
        effective_centerline[start..end].copy_from_slice(&centerline);

        let fade_nominal = fade_samples_nominal.max(0) as usize;
        let fade_samples = fade_nominal.min(span / 2);
        let fade_shortened = fade_samples < fade_nominal;
        let rate_sample_gains = gain_at_each_rate_sample(
            span,
            gain,
            fade_samples,
            &parameters.repair_fade_shape,
        )?;
        for offset in 0..span {
            let value = rate[start + offset]
                - (1.0 - rate_sample_gains[offset]) * deviation[offset];
            repaired[start + offset] = value;
        }

        let attenuated_deviation: Vec<f64> = repaired[start..end]
            .iter()
            .zip(centerline.iter())
            .map(|(rate, centerline)| rate - centerline)
            .collect();
        let input_energy = fit::numpy_sum(
            &deviation.iter().map(|value| value * value).collect::<Vec<_>>(),
        );
        let output_energy = fit::numpy_sum(
            &attenuated_deviation
                .iter()
                .map(|value| value * value)
                .collect::<Vec<_>>(),
        );
        let usable_scale = clean_rsd.is_finite() && clean_rsd > 0.0;
        let mut after = vec![0.0; span];
        if usable_scale {
            for offset in 0..span {
                after[offset] = attenuated_deviation[offset].abs() / clean_rsd;
            }
        }
        let peak_after = if usable_scale {
            after.iter().copied().fold(0.0f64, f64::max)
        } else {
            f64::INFINITY
        };
        let excluded_at_each_end = fade_samples.max(1);
        let interior_after = if span > 2 * excluded_at_each_end && usable_scale {
            after[excluded_at_each_end..span - excluded_at_each_end]
                .iter()
                .copied()
                .fold(0.0f64, f64::max)
        } else {
            0.0
        };

        gains.push(gain);
        extent_entries.push(ExtentEntry {
            first_sample: start,
            last_sample: end - 1,
            rate_sample_count: Some(span),
            scaled: true,
            reason: None,
            gain,
            peak_sample: Some(peak_sample),
            peak_departure_deg_s: Some(peak_departure),
            clean_robust_standard_deviation_deg_s: Some(rsd_at_largest),
            peak_in_clean_robust_standard_deviations_before: Some(departure_in_rsd),
            peak_in_clean_robust_standard_deviations_after: Some(peak_after),
            peak_in_clean_robust_standard_deviations_after_interior: Some(interior_after),
            fade_samples: Some(fade_samples),
            fade_used_ms: Some(fade_samples as f64 / measurement_rate_hz * 1e3),
            fade_shortened: Some(fade_shortened),
            energy_ratio: Some(if input_energy > 0.0 {
                output_energy / input_energy
            } else {
                1.0
            }),
        });
        used_extents.push((start, end - 1));
    }

    let scaled_entries: Vec<&ExtentEntry> =
        extent_entries.iter().filter(|entry| entry.scaled).collect();
    let kept_reasons: Vec<&str> = extent_entries
        .iter()
        .filter(|entry| !entry.scaled)
        .filter_map(|entry| entry.reason.as_deref())
        .collect();
    let mut statistics = AttenuatorStatistics {
        extent_count: detected_extents.len(),
        attenuated_count: scaled_entries.iter().filter(|entry| entry.gain < 1.0).count(),
        passed_through_count: scaled_entries.iter().filter(|entry| entry.gain >= 1.0).count(),
        skipped_count: kept_reasons.iter().filter(|reason| **reason == AT_A_VIDEO_EDGE).count(),
        excluded_count: kept_reasons
            .iter()
            .filter(|reason| **reason == ON_THE_DO_NOT_REPAIR_LIST)
            .count(),
        shortened_fade_count: scaled_entries
            .iter()
            .filter(|entry| entry.fade_shortened == Some(true))
            .count(),
        repair_oscillation_centerline: parameters.repair_oscillation_centerline.clone(),
        repair_burst_centerline: parameters.repair_burst_centerline.clone(),
        repair_impulse_centerline: parameters.repair_impulse_centerline.clone(),
        repair_oscillation_ceiling: parameters.repair_oscillation_ceiling,
        repair_burst_ceiling: parameters.repair_burst_ceiling,
        repair_impulse_ceiling: parameters.repair_impulse_ceiling,
        gain_min: gains.iter().copied().fold(None, |acc: Option<f64>, value| {
            Some(acc.map_or(value, |acc| acc.min(value)))
        }),
        gain_median: median_of(&gains),
        gain_max: gains.iter().copied().fold(None, |acc: Option<f64>, value| {
            Some(acc.map_or(value, |acc| acc.max(value)))
        }),
        peak_in_clean_robust_standard_deviations_before_median: median_of(
            &scaled_entries
                .iter()
                .map(|entry| {
                    entry
                        .peak_in_clean_robust_standard_deviations_before
                        .unwrap_or(f64::NAN)
                })
                .collect::<Vec<_>>(),
        ),
        energy_ratio_median: median_of(
            &scaled_entries
                .iter()
                .map(|entry| entry.energy_ratio.unwrap_or(f64::NAN))
                .collect::<Vec<_>>(),
        ),
        share_of_rate_samples_inside_scaled_extents: scaled_entries
            .iter()
            .map(|entry| entry.rate_sample_count.unwrap_or(0))
            .sum::<usize>() as f64
            / rate_sample_count as f64,
    };
    if statistics.extent_count == 0 {
        statistics.gain_min = None;
        statistics.gain_median = None;
        statistics.gain_max = None;
    }

    Ok(AttenuationResult {
        repaired_rate_deg_s: repaired,
        centerline: effective_centerline,
        gains,
        extents: used_extents,
        extent_entries,
        statistics,
    })
}
