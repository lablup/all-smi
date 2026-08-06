// Copyright 2025 Lablup Inc. and Jeongkyu Shin
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Aggregations over GPU fields that can be absent (issue #325).
//!
//! `GpuInfo`'s live metric fields are plain numbers carrying an
//! out-of-range "no reading" encoding rather than `Option`, because roughly
//! sixty call sites consume them as numbers (see
//! [`crate::device::types::GPU_METRIC_UNAVAILABLE`]). The cost of that choice
//! is that every *aggregation* has to skip the non-readings explicitly, or it
//! averages a sentinel into a real number and produces a plausible-looking
//! lie: a two-GPU host where one card stopped reporting would show half its
//! real mean utilization, and summing a `-1.0` into an energy accumulator
//! integrates negative joules.
//!
//! These helpers are the one place that skipping happens, so the TUI
//! dashboard, the header, the sparkline panel, the snapshot writer and the
//! Prometheus-side aggregator cannot each get it subtly different.
//!
//! Every function here treats "nobody reported" as `None` (or `0.0` for the
//! sums, where an empty sum genuinely is zero) rather than inventing a value.

use crate::device::GpuInfo;

/// Sum of the GPU power readings that are actually present, in watts.
///
/// GPUs with no power rail reading contribute nothing instead of dragging
/// the total negative. An empty input, or an input where nothing reported,
/// sums to `0.0` — which is the correct total power of zero reporting rails
/// and matches what callers previously did for an empty GPU list.
pub fn total_power_watts(gpus: &[GpuInfo]) -> f64 {
    gpus.iter()
        .filter_map(GpuInfo::power_consumption_reading)
        .sum()
}

/// Mean utilization across the GPUs that reported one, or `None` when none
/// did.
pub fn mean_utilization(gpus: &[GpuInfo]) -> Option<f64> {
    mean(gpus.iter().filter_map(GpuInfo::utilization_reading))
}

/// Mean temperature in Celsius across the GPUs that reported one, or `None`
/// when no sensor answered on any device.
pub fn mean_temperature(gpus: &[GpuInfo]) -> Option<f64> {
    mean(
        gpus.iter()
            .filter_map(GpuInfo::temperature_reading)
            .map(f64::from),
    )
}

/// Sample standard deviation of the reported temperatures.
///
/// Returns `None` when fewer than two devices reported, since the sample
/// standard deviation is undefined there (the previous code divided by
/// `n - 1` and relied on a `total_gpus > 1` guard at every call site).
pub fn temperature_std_dev(gpus: &[GpuInfo]) -> Option<f64> {
    let temps: Vec<f64> = gpus
        .iter()
        .filter_map(GpuInfo::temperature_reading)
        .map(f64::from)
        .collect();
    if temps.len() < 2 {
        return None;
    }
    let mean = temps.iter().sum::<f64>() / temps.len() as f64;
    let variance = temps.iter().map(|t| (t - mean).powi(2)).sum::<f64>() / (temps.len() - 1) as f64;
    Some(variance.sqrt())
}

/// ANE power in watts for the first Apple Silicon row that reported one.
///
/// There is exactly one GPU row per Apple Silicon host, so "first that
/// reported" is "the one, if it reported".
pub fn first_ane_power_watts(gpus: &[GpuInfo]) -> Option<f64> {
    gpus.iter()
        .find_map(GpuInfo::ane_utilization_reading)
        .map(|mw| mw / 1000.0)
}

fn mean(values: impl Iterator<Item = f64>) -> Option<f64> {
    let mut count = 0usize;
    let mut sum = 0.0;
    for value in values {
        sum += value;
        count += 1;
    }
    (count > 0).then(|| sum / count as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::types::GPU_METRIC_UNAVAILABLE;
    use std::collections::HashMap;

    fn gpu(utilization: f64, temperature: u32, power: f64) -> GpuInfo {
        GpuInfo {
            uuid: "GPU-1".to_string(),
            time: String::new(),
            name: "test".to_string(),
            device_type: "GPU".to_string(),
            host_id: "h".to_string(),
            hostname: "h".to_string(),
            instance: "h".to_string(),
            utilization,
            ane_utilization: GPU_METRIC_UNAVAILABLE,
            dla_utilization: None,
            tensorcore_utilization: None,
            temperature,
            used_memory: 0,
            total_memory: 0,
            frequency: 0,
            power_consumption: power,
            gpu_core_count: None,
            temperature_threshold_slowdown: None,
            temperature_threshold_shutdown: None,
            temperature_threshold_max_operating: None,
            temperature_threshold_acoustic: None,
            performance_state: None,
            numa_node_id: None,
            gsp_firmware_mode: None,
            gsp_firmware_version: None,
            nvlink_remote_devices: Vec::new(),
            gpm_metrics: None,
            detail: HashMap::new(),
        }
    }

    fn unavailable() -> GpuInfo {
        gpu(GPU_METRIC_UNAVAILABLE, 0, GPU_METRIC_UNAVAILABLE)
    }

    #[test]
    fn absent_rows_are_skipped_not_counted_as_zero() {
        let gpus = vec![gpu(80.0, 70, 300.0), unavailable()];
        // The mean is over the one device that reported, not 40.0.
        assert_eq!(mean_utilization(&gpus), Some(80.0));
        assert_eq!(mean_temperature(&gpus), Some(70.0));
        assert_eq!(total_power_watts(&gpus), 300.0);
    }

    #[test]
    fn a_genuine_zero_still_counts() {
        let gpus = vec![gpu(0.0, 30, 0.0), gpu(100.0, 50, 200.0)];
        assert_eq!(mean_utilization(&gpus), Some(50.0));
        assert_eq!(mean_temperature(&gpus), Some(40.0));
        assert_eq!(total_power_watts(&gpus), 200.0);
    }

    #[test]
    fn nothing_reported_yields_none_rather_than_zero() {
        let gpus = vec![unavailable(), unavailable()];
        assert_eq!(mean_utilization(&gpus), None);
        assert_eq!(mean_temperature(&gpus), None);
        assert_eq!(temperature_std_dev(&gpus), None);
        assert_eq!(first_ane_power_watts(&gpus), None);
        // A sum over nothing is genuinely zero.
        assert_eq!(total_power_watts(&gpus), 0.0);
    }

    #[test]
    fn empty_input_is_handled() {
        assert_eq!(mean_utilization(&[]), None);
        assert_eq!(total_power_watts(&[]), 0.0);
        assert_eq!(temperature_std_dev(&[]), None);
    }

    #[test]
    fn std_dev_needs_two_reporting_devices() {
        // Two devices present but only one reporting: undefined, not 0.
        let gpus = vec![gpu(10.0, 60, 5.0), unavailable()];
        assert_eq!(temperature_std_dev(&gpus), None);

        let gpus = vec![gpu(10.0, 60, 5.0), gpu(10.0, 70, 5.0)];
        let sd = temperature_std_dev(&gpus).expect("two readings");
        assert!((sd - 7.0710678).abs() < 1e-6, "got {sd}");
    }

    #[test]
    fn ane_power_converts_milliwatts_to_watts() {
        let mut g = gpu(1.0, 40, 1.0);
        g.ane_utilization = 2500.0;
        assert_eq!(first_ane_power_watts(&[g]), Some(2.5));
    }
}
