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

//! Fixed Y-axis range helpers for the metric sparklines.
//!
//! Absolute-magnitude metrics (temperature, power) are only comparable over
//! time when their sparkline axis is anchored to a stable, domain-meaningful
//! range. The braille sparkline has just four vertical levels, so the old
//! per-frame `[min(window), max(window)]` auto-ranging exaggerated noise
//! (a ±1°C wiggle filled the full height), shifted the baseline every time
//! the window slid, and collapsed any near-constant series to the bottom row
//! — making a blazing 90°C indistinguishable from a cool 35°C.
//!
//! These helpers replace that with fixed ranges:
//! - temperature is anchored to a `30°C` idle floor and a thermal-threshold
//!   ceiling (falling back to `100°C`),
//! - power is anchored to `0` and the device's enforced power limit (falling
//!   back to a [`nice_ceil`] over the observed peak),
//! - ANE is anchored to `0` and a [`nice_ceil`] over the observed peak with a
//!   small minimum so an idle Neural Engine reads as near-empty.
//!
//! Percentage metrics (utilization, memory) already use a fixed `(0, 100)`
//! range at their call sites and do not need a helper here.

use crate::device::GpuInfo;

/// Idle floor for temperature sparklines, in °C.
///
/// Silicon rarely idles below ambient; anchoring the axis here keeps the
/// meaningful `30 .. ceiling` band visible instead of magnifying jitter.
pub const TEMP_FLOOR_C: f64 = 30.0;

/// Fallback temperature ceiling, in °C, used when no thermal threshold is
/// reported (CPU sensors, Apple Silicon, non-NVIDIA GPUs, older drivers).
pub const TEMP_FALLBACK_CEIL_C: f64 = 100.0;

/// Minimum ANE power ceiling, in W. Keeps an idle Neural Engine reading near
/// the bottom of the axis rather than amplifying sub-watt jitter.
pub const ANE_MIN_CEIL_W: f64 = 8.0;

/// Minimum package-power ceiling, in W, for the [`nice_ceil`] fallback used
/// when the device exposes no enforced power limit (e.g. Apple Silicon).
pub const POWER_MIN_CEIL_W: f64 = 10.0;

/// `gpu.detail` keys that may carry an enforced/board power limit in watts,
/// in preference order. Populated by the NVIDIA and Gaudi readers.
const POWER_LIMIT_KEYS: [&str; 3] = [
    "power_limit_current",
    "power_limit_max",
    "power_limit_default",
];

/// Round `v` up to a visually pleasant ceiling of the form `1`, `2`, or
/// `5 × 10ⁿ`.
///
/// This keeps a fallback axis stable: small fluctuations in the observed peak
/// (e.g. 280 W ↔ 295 W) round to the same ceiling (300 W), so the sparkline
/// shape no longer drifts as the window slides.
///
/// Non-finite or non-positive inputs return `1.0` as a harmless degenerate
/// ceiling; callers typically apply their own minimum floor beforehand. The
/// returned ceiling is likewise guaranteed finite even for pathologically
/// large inputs (near `f64::MAX`), where the rounded value would otherwise
/// overflow to infinity.
#[must_use]
pub fn nice_ceil(v: f64) -> f64 {
    if !v.is_finite() || v <= 0.0 {
        return 1.0;
    }
    let exp = v.log10().floor();
    let pow = 10_f64.powf(exp);
    let frac = v / pow; // in [1.0, 10.0)
    let nice = if frac <= 1.0 {
        1.0
    } else if frac <= 2.0 {
        2.0
    } else if frac <= 5.0 {
        5.0
    } else {
        10.0
    };
    let ceil = nice * pow;
    // Guard the rare overflow at the very top of the f64 range (e.g. a
    // malformed remote power reading near f64::MAX): a non-finite ceiling
    // would surface downstream as a "0-inf" axis badge. Fall back to the
    // (finite) input rather than overflow.
    if ceil.is_finite() { ceil } else { v }
}

/// Fixed temperature axis `(floor, ceiling)` in °C.
///
/// The ceiling is the first reported GPU thermal threshold
/// (slowdown → max-operating → shutdown); when none is available — including
/// for CPU/system temperature, which carries no threshold — it falls back to
/// [`TEMP_FALLBACK_CEIL_C`]. A reported threshold at or below the floor is
/// ignored in favour of the fallback so the range never inverts.
#[must_use]
pub fn temp_range(gpu: Option<&GpuInfo>) -> (f64, f64) {
    let ceil = gpu
        .and_then(|g| {
            g.temperature_threshold_slowdown
                .or(g.temperature_threshold_max_operating)
                .or(g.temperature_threshold_shutdown)
        })
        .map(f64::from)
        .filter(|&c| c > TEMP_FLOOR_C)
        .unwrap_or(TEMP_FALLBACK_CEIL_C);
    (TEMP_FLOOR_C, ceil)
}

/// Fixed package-power axis `(0, ceiling)` in watts.
///
/// "Package power" is summed across **all** GPUs on the host (see
/// `package_power` in `gpu_sparkline_panel`), so the ceiling is the aggregate
/// enforced power limit — the sum of every GPU's reported limit. The summed
/// limit is used only when *every* GPU reports a valid one; if any GPU lacks a
/// limit (Apple Silicon, or a heterogeneous node with an older driver) the sum
/// would understate the budget and clip the sparkline, so it falls back to
/// [`nice_ceil`] over the observed history peak, floored at
/// [`POWER_MIN_CEIL_W`], which still tracks real usage.
#[must_use]
pub fn power_range(gpus: &[GpuInfo], history: &[f64]) -> (f64, f64) {
    let mut total_limit = 0.0_f64;
    let mut all_have_limit = !gpus.is_empty();
    for g in gpus {
        match gpu_power_limit(g) {
            Some(w) => total_limit += w,
            None => {
                all_have_limit = false;
                break;
            }
        }
    }
    let ceil = if all_have_limit && total_limit.is_finite() && total_limit > 0.0 {
        total_limit
    } else {
        nice_ceil(history_peak(history).max(POWER_MIN_CEIL_W))
    };
    (0.0, ceil)
}

/// First valid enforced power limit (W) a GPU reports, trying each
/// [`POWER_LIMIT_KEYS`] entry in `current → max → default` priority order.
///
/// A power limit scraped from a remote endpoint is untrusted, and `f64`
/// parsing accepts "inf"/"NaN"; each candidate must parse to a positive,
/// *finite* value or the next key is tried (so a present-but-bogus
/// `power_limit_current` does not mask a valid `power_limit_max`). Returns
/// `None` when no key yields a usable value.
fn gpu_power_limit(g: &GpuInfo) -> Option<f64> {
    POWER_LIMIT_KEYS.iter().find_map(|k| {
        g.detail
            .get(*k)
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|&w| w.is_finite() && w > 0.0)
    })
}

/// Fixed ANE-power axis `(0, ceiling)` in watts.
///
/// Apple publishes no ANE power cap, so the ceiling is [`nice_ceil`] over the
/// observed history peak with an [`ANE_MIN_CEIL_W`] floor.
#[must_use]
pub fn ane_range(history: &[f64]) -> (f64, f64) {
    (0.0, nice_ceil(history_peak(history).max(ANE_MIN_CEIL_W)))
}

/// Largest finite sample in `history`, or `0.0` when empty / all non-finite.
fn history_peak(history: &[f64]) -> f64 {
    history
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .fold(0.0_f64, f64::max)
}

/// Format a fixed range as a compact scale badge, e.g. `30-83`.
///
/// This replaces the old observed-window min/max badge: showing the fixed
/// axis turns the badge into a stable legend that explains the sparkline's
/// height, rather than a number that jitters every frame.
#[must_use]
pub fn scale_badge(min: f64, max: f64) -> String {
    format!("{min:.0}-{max:.0}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::GpuInfo;
    use std::collections::HashMap;

    /// Minimal GPU with the given thresholds / detail map for range tests.
    fn gpu_with(
        slowdown: Option<u32>,
        max_operating: Option<u32>,
        shutdown: Option<u32>,
        detail: HashMap<String, String>,
    ) -> GpuInfo {
        GpuInfo {
            uuid: "gpu-0".to_string(),
            time: String::new(),
            name: "Test GPU".to_string(),
            device_type: "GPU".to_string(),
            host_id: "localhost".to_string(),
            hostname: "localhost".to_string(),
            instance: "localhost".to_string(),
            utilization: 0.0,
            ane_utilization: 0.0,
            dla_utilization: None,
            tensorcore_utilization: None,
            temperature: 50,
            used_memory: 0,
            total_memory: 0,
            frequency: 0,
            power_consumption: 0.0,
            gpu_core_count: None,
            temperature_threshold_slowdown: slowdown,
            temperature_threshold_shutdown: shutdown,
            temperature_threshold_max_operating: max_operating,
            temperature_threshold_acoustic: None,
            performance_state: None,
            numa_node_id: None,
            gsp_firmware_mode: None,
            gsp_firmware_version: None,
            nvlink_remote_devices: Vec::new(),
            gpm_metrics: None,
            detail,
        }
    }

    #[test]
    fn nice_ceil_rounds_to_1_2_5_decades() {
        assert_eq!(nice_ceil(1.0), 1.0);
        assert_eq!(nice_ceil(1.5), 2.0);
        assert_eq!(nice_ceil(2.0), 2.0);
        assert_eq!(nice_ceil(3.5), 5.0);
        assert_eq!(nice_ceil(5.0), 5.0);
        assert_eq!(nice_ceil(7.0), 10.0);
        assert_eq!(nice_ceil(10.0), 10.0);
        assert_eq!(nice_ceil(17.5), 20.0);
        assert_eq!(nice_ceil(158.0), 200.0);
        assert_eq!(nice_ceil(287.0), 500.0);
    }

    #[test]
    fn nice_ceil_handles_degenerate_input() {
        assert_eq!(nice_ceil(0.0), 1.0);
        assert_eq!(nice_ceil(-5.0), 1.0);
        assert_eq!(nice_ceil(f64::NAN), 1.0);
        assert_eq!(nice_ceil(f64::INFINITY), 1.0);
    }

    #[test]
    fn nice_ceil_result_is_always_finite() {
        // Pathologically large but finite inputs (e.g. a malformed remote power
        // reading near f64::MAX) must not overflow the rounded ceiling to inf,
        // which would otherwise surface as a "0-inf" axis badge.
        assert!(nice_ceil(f64::MAX).is_finite());
        assert!(nice_ceil(1.0e308).is_finite());
        assert!(nice_ceil(8.0e307).is_finite());
    }

    #[test]
    fn temp_range_uses_threshold_priority() {
        // slowdown wins over the others
        let g = gpu_with(Some(83), Some(90), Some(95), HashMap::new());
        assert_eq!(temp_range(Some(&g)), (30.0, 83.0));
        // max_operating used when slowdown absent
        let g = gpu_with(None, Some(90), Some(95), HashMap::new());
        assert_eq!(temp_range(Some(&g)), (30.0, 90.0));
        // shutdown used when the others are absent
        let g = gpu_with(None, None, Some(95), HashMap::new());
        assert_eq!(temp_range(Some(&g)), (30.0, 95.0));
    }

    #[test]
    fn temp_range_falls_back_without_thresholds() {
        // No GPU (e.g. CPU temperature) -> fallback ceiling
        assert_eq!(temp_range(None), (30.0, TEMP_FALLBACK_CEIL_C));
        // GPU without thresholds -> fallback ceiling
        let g = gpu_with(None, None, None, HashMap::new());
        assert_eq!(temp_range(Some(&g)), (30.0, TEMP_FALLBACK_CEIL_C));
    }

    #[test]
    fn temp_range_ignores_threshold_at_or_below_floor() {
        // A bogus threshold below the floor must not invert the range.
        let g = gpu_with(Some(20), None, None, HashMap::new());
        assert_eq!(temp_range(Some(&g)), (30.0, TEMP_FALLBACK_CEIL_C));
    }

    #[test]
    fn power_range_prefers_enforced_limit() {
        let mut detail = HashMap::new();
        detail.insert("power_limit_current".to_string(), "350.00".to_string());
        let g = gpu_with(None, None, None, detail);
        // History peak is ignored when an enforced limit exists.
        assert_eq!(
            power_range(std::slice::from_ref(&g), &[100.0, 200.0, 320.0]),
            (0.0, 350.0)
        );
    }

    #[test]
    fn power_range_limit_key_priority() {
        let mut detail = HashMap::new();
        detail.insert("power_limit_max".to_string(), "450".to_string());
        detail.insert("power_limit_default".to_string(), "400".to_string());
        let g = gpu_with(None, None, None, detail);
        // current absent -> max preferred over default
        assert_eq!(power_range(std::slice::from_ref(&g), &[]), (0.0, 450.0));
    }

    #[test]
    fn power_range_tries_next_key_when_first_invalid() {
        // A present-but-invalid power_limit_current must not mask a valid
        // power_limit_max: each key is parsed/validated independently.
        let mut detail = HashMap::new();
        detail.insert("power_limit_current".to_string(), "0".to_string());
        detail.insert("power_limit_max".to_string(), "450".to_string());
        let g = gpu_with(None, None, None, detail);
        assert_eq!(power_range(std::slice::from_ref(&g), &[40.0]), (0.0, 450.0));
    }

    #[test]
    fn power_range_sums_multi_gpu_limits() {
        // Package power is summed across GPUs, so the ceiling is the summed
        // per-GPU limits (4 × 350 W = 1400 W), not a single GPU's limit. A peak
        // exceeding one GPU's limit must therefore not clip the sparkline.
        let mut detail = HashMap::new();
        detail.insert("power_limit_current".to_string(), "350".to_string());
        let gpus: Vec<GpuInfo> = (0..4)
            .map(|_| gpu_with(None, None, None, detail.clone()))
            .collect();
        assert_eq!(power_range(&gpus, &[900.0, 1200.0]), (0.0, 1400.0));
    }

    #[test]
    fn power_range_multi_gpu_falls_back_when_any_limit_missing() {
        // If even one GPU lacks a valid limit, the summed ceiling would
        // understate the budget, so fall back to the nice-rounded peak.
        let mut detail = HashMap::new();
        detail.insert("power_limit_current".to_string(), "350".to_string());
        let with_limit = gpu_with(None, None, None, detail);
        let without_limit = gpu_with(None, None, None, HashMap::new());
        let gpus = [with_limit, without_limit];
        // peak 600 -> nice_ceil 1000 (not the partial 350 sum)
        assert_eq!(power_range(&gpus, &[500.0, 600.0]), (0.0, nice_ceil(600.0)));
    }

    #[test]
    fn power_range_falls_back_to_nice_ceil_peak() {
        // No GPU detail -> nice_ceil over the observed peak.
        let g = gpu_with(None, None, None, HashMap::new());
        // peak 158 -> nice_ceil 200
        assert_eq!(
            power_range(std::slice::from_ref(&g), &[120.0, 140.0, 158.0]),
            (0.0, 200.0)
        );
        // No GPUs at all -> fallback; peak below the floor clamps up to
        // POWER_MIN_CEIL_W's nice_ceil.
        assert_eq!(
            power_range(&[], &[2.0, 3.0]),
            (0.0, nice_ceil(POWER_MIN_CEIL_W))
        );
    }

    #[test]
    fn power_range_ignores_nonpositive_limit() {
        let mut detail = HashMap::new();
        detail.insert("power_limit_current".to_string(), "0".to_string());
        let g = gpu_with(None, None, None, detail);
        // A zero limit is invalid -> fall back to nice_ceil over peak.
        assert_eq!(
            power_range(std::slice::from_ref(&g), &[40.0]),
            (0.0, nice_ceil(40.0))
        );
    }

    #[test]
    fn power_range_ignores_non_finite_limit() {
        // A power limit can originate from an untrusted remote Prometheus
        // scrape, and `f64` parsing accepts "inf"/"NaN". Such a value must not
        // become the axis ceiling (which would render a "0-inf" badge); it must
        // fall back to the nice-rounded observed peak.
        for bogus in ["inf", "Inf", "infinity", "-inf", "NaN", "nan"] {
            let mut detail = HashMap::new();
            detail.insert("power_limit_current".to_string(), bogus.to_string());
            let g = gpu_with(None, None, None, detail);
            assert_eq!(
                power_range(std::slice::from_ref(&g), &[40.0]),
                (0.0, nice_ceil(40.0)),
                "limit {bogus:?} should fall back to the peak"
            );
        }
    }

    #[test]
    fn ane_range_floors_at_min_ceiling() {
        // Idle/low ANE -> floored ceiling (nice_ceil(8) == 10)
        assert_eq!(
            ane_range(&[0.0, 0.5, 3.8]),
            (0.0, nice_ceil(ANE_MIN_CEIL_W))
        );
        assert_eq!(ane_range(&[]), (0.0, nice_ceil(ANE_MIN_CEIL_W)));
        // Higher peak rounds up past the floor
        assert_eq!(ane_range(&[2.0, 12.0]), (0.0, nice_ceil(12.0)));
    }

    #[test]
    fn power_range_is_stable_under_window_shift() {
        // Two overlapping windows with different peaks that round to the same
        // nice ceiling must yield the same axis (no per-frame drift).
        let g = gpu_with(None, None, None, HashMap::new());
        let a = power_range(std::slice::from_ref(&g), &[280.0, 290.0]);
        let b = power_range(std::slice::from_ref(&g), &[290.0, 295.0]);
        assert_eq!(a, b);
        assert_eq!(a, (0.0, 500.0));
    }

    #[test]
    fn scale_badge_formats_without_decimals() {
        assert_eq!(scale_badge(30.0, 83.0), "30-83");
        assert_eq!(scale_badge(0.0, 350.0), "0-350");
        assert_eq!(scale_badge(0.0, 100.0), "0-100");
    }

    #[test]
    fn history_peak_ignores_non_finite() {
        assert_eq!(history_peak(&[1.0, f64::NAN, 5.0, f64::INFINITY]), 5.0);
        assert_eq!(history_peak(&[]), 0.0);
        assert_eq!(history_peak(&[f64::NAN]), 0.0);
    }
}
