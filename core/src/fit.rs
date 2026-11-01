//! The robust local linear regression and the local difference scale, matching
//! `gyro_doctor.core.robust_local_fit`.

pub const MAD_TO_SIGMA: f64 = 1.4826;
pub const BISQUARE_TUNING_CONSTANT: f64 = 4.685;

#[derive(Debug, Clone)]
pub struct Fit {
    pub local_median: Vec<f64>,
    pub windows_without_weight: usize,
    pub zero_scale: usize,
}

/// NumPy's pairwise summation order.
pub fn numpy_sum(values: &[f64]) -> f64 {
    const BLOCK: usize = 128;
    let n = values.len();
    if n < 8 {
        return values.iter().copied().sum();
    }
    if n <= BLOCK {
        let mut r = [0.0f64; 8];
        r.copy_from_slice(&values[..8]);
        let mut i = 8;
        while i + 8 <= n {
            for j in 0..8 {
                r[j] += values[i + j];
            }
            i += 8;
        }
        let mut result = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
        while i < n {
            result += values[i];
            i += 1;
        }
        return result;
    }
    let mut n2 = n / 2;
    n2 -= n2 % 8;
    numpy_sum(&values[..n2]) + numpy_sum(&values[n2..])
}

/// NumPy's median for a slice of finite values. The slice is reordered.
pub fn median_in_place(values: &mut [f64]) -> f64 {
    let n = values.len();
    if n == 0 {
        return f64::NAN;
    }
    let middle = n / 2;
    let (_, upper, _) = values.select_nth_unstable_by(middle, f64::total_cmp);
    let upper = *upper;
    if n % 2 == 1 {
        upper
    } else {
        let lower = values[..middle]
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);
        (lower + upper) / 2.0
    }
}

pub fn median(values: &mut [f64]) -> f64 {
    median_in_place(values)
}

#[inline]
fn tricube(scaled_offset: f64) -> f64 {
    let absolute = scaled_offset.abs();
    if absolute < 1.0 {
        (1.0 - absolute.powf(3.0)).powf(3.0)
    } else {
        0.0
    }
}

#[inline]
fn bisquare(scaled_difference: f64) -> f64 {
    let absolute = scaled_difference.abs();
    if absolute < 1.0 {
        (1.0 - absolute.powf(2.0)).powf(2.0)
    } else {
        0.0
    }
}

fn window_bounds(times: &[f64], center: f64, window_s: f64) -> (usize, usize) {
    let start = times.partition_point(|&time| time < center - window_s);
    let end = times.partition_point(|&time| time <= center + window_s);
    (start, end)
}

fn weighted_local_fit(
    rate: &[f64],
    times: &[f64],
    window_s: f64,
    robustness_weights: &[f64],
) -> (Vec<f64>, usize) {
    let n = rate.len();
    let mut fitted = vec![0.0; n];
    let mut windows_without_weight = 0usize;
    let mut offsets = Vec::new();
    let mut weights = Vec::new();
    let mut window_rate = Vec::new();
    for i in 0..n {
        let (start, end) = window_bounds(times, times[i], window_s);
        let width = end - start;
        offsets.clear();
        weights.clear();
        window_rate.clear();
        offsets.reserve(width);
        weights.reserve(width);
        window_rate.reserve(width);
        for j in start..end {
            let offset = times[j] - times[i];
            offsets.push(offset);
            weights.push(tricube(offset / window_s) * robustness_weights[j]);
            window_rate.push(rate[j]);
        }

        let weight_sum = numpy_sum(&weights);
        let usable = weight_sum > 0.0;
        let mut value = if usable {
            let mut weighted_rate = Vec::with_capacity(width);
            for j in 0..width {
                weighted_rate.push(weights[j] * window_rate[j]);
            }
            numpy_sum(&weighted_rate) / weight_sum
        } else {
            windows_without_weight += 1;
            rate[i]
        };

        if width > 1 {
            let mut weighted_offset = Vec::with_capacity(width);
            let mut weighted_squared_offset = Vec::with_capacity(width);
            let mut weighted_offset_rate = Vec::with_capacity(width);
            for j in 0..width {
                weighted_offset.push(weights[j] * offsets[j]);
                weighted_squared_offset.push(weights[j] * offsets[j] * offsets[j]);
                weighted_offset_rate.push(weights[j] * offsets[j] * window_rate[j]);
            }
            let weighted_offset_sum = numpy_sum(&weighted_offset);
            let weighted_squared_offset_sum = numpy_sum(&weighted_squared_offset);
            let weighted_offset_rate_sum = numpy_sum(&weighted_offset_rate);
            let mut weighted_rate = Vec::with_capacity(width);
            for j in 0..width {
                weighted_rate.push(weights[j] * window_rate[j]);
            }
            let weighted_rate_sum = numpy_sum(&weighted_rate);
            let determinant =
                weight_sum * weighted_squared_offset_sum - weighted_offset_sum * weighted_offset_sum;
            let has_spread = usable && determinant > 0.0;
            if has_spread {
                let linear = (weighted_squared_offset_sum * weighted_rate_sum
                    - weighted_offset_sum * weighted_offset_rate_sum)
                    / determinant;
                value = linear;
            }
        }
        fitted[i] = value;
    }
    (fitted, windows_without_weight)
}

fn local_difference_scale(difference: &[f64], times: &[f64], window_s: f64) -> Vec<f64> {
    let mut scale = vec![0.0; difference.len()];
    let mut absolute = Vec::new();
    for i in 0..difference.len() {
        let (start, end) = window_bounds(times, times[i], window_s);
        absolute.clear();
        absolute.extend(difference[start..end].iter().map(|value| value.abs()));
        scale[i] = MAD_TO_SIGMA * median_in_place(&mut absolute);
    }
    scale
}

pub fn fit(rate: &[f64], times: &[f64], local_median_window_ms: f64, fits: usize) -> Fit {
    let window_s = local_median_window_ms * 1e-3;
    let mut robustness_weights = vec![1.0; rate.len()];
    let (mut local_median, mut windows_without_weight) =
        weighted_local_fit(rate, times, window_s, &robustness_weights);
    let mut zero_scale = 0usize;
    for _ in 1..fits {
        let difference: Vec<f64> = rate
            .iter()
            .zip(local_median.iter())
            .map(|(rate, median)| rate - median)
            .collect();
        let scale = local_difference_scale(&difference, times, window_s);
        zero_scale = scale.iter().filter(|value| **value <= 0.0).count();
        robustness_weights = difference
            .iter()
            .zip(scale.iter())
            .map(|(difference, scale)| {
                if *scale > 0.0 {
                    bisquare(difference / (BISQUARE_TUNING_CONSTANT * scale))
                } else {
                    bisquare(0.0)
                }
            })
            .collect();
        (local_median, windows_without_weight) =
            weighted_local_fit(rate, times, window_s, &robustness_weights);
    }
    Fit {
        local_median,
        windows_without_weight,
        zero_scale,
    }
}
