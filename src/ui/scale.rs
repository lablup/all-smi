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

//! Y-axis range helpers for the metric sparklines.
//!
//! Two axis strategies coexist, one layered on the other.
//!
//! ## Fixed domains (the hard bounds)
//!
//! Absolute-magnitude metrics are only comparable over time when their
//! sparkline axis is anchored to a stable, domain-meaningful range. The
//! per-frame `[min(window), max(window)]` auto-ranging this replaced
//! exaggerated noise (a ±1°C wiggle filled the full height), shifted the
//! baseline every time the window slid, and collapsed any near-constant series
//! to the bottom row, making a blazing 90°C indistinguishable from a cool 35°C.
//! The fixed-domain helpers pin each metric to a stable range instead:
//! - temperature is anchored to a `30°C` idle floor and a thermal-threshold
//!   ceiling (falling back to `100°C`) via [`temp_range`],
//! - power is anchored to `0` and the device's enforced power limit (falling
//!   back to a [`nice_ceil`] over the observed peak) via [`power_range`],
//! - ANE is anchored to `0` and a [`nice_ceil`] over the observed peak with a
//!   small minimum via [`ane_range`],
//! - percentage metrics (utilization, memory) use a fixed `(0, 100)`.
//!
//! These remain the hard bounds of every metric. #272 kept them so the
//! multi-row Activity graphs (Part 3, #274) can render on a fixed axis where a
//! dot's height means the same thing every frame; they are still exported for
//! those callers.
//!
//! ## Soft zoom (single-row sparklines)
//!
//! A single-row braille sparkline has just four vertical dot levels, far too
//! little resolution to show texture inside a full fixed domain: a GPU parked
//! at ~41% utilization always lands in the same quantization band of a `0..100`
//! axis, so real variation is invisible. [`soft_range`] zooms the axis into the
//! visible history window for those compact sparklines, with three guardrails
//! so it does not reintroduce the instability the fixed axes were built to
//! avoid:
//!
//! 1. a per-metric **minimum span**, so a near-constant series cannot blow
//!    sensor noise up to full height;
//! 2. bounds **rounded outward to a coarse grid**, a stateless hysteresis: two
//!    overlapping sliding windows whose extremes differ slightly round to the
//!    same axis, so the baseline does not flap frame-to-frame;
//! 3. the result **clamped inside the metric's hard domain**, so the soft axis
//!    can never exceed the fixed bounds above.
//!
//! The fixed-domain helpers double as the domain argument to [`soft_range`]:
//! temperature's soft axis is clamped to `(0, temp_range(gpu).1)` (a `0` floor,
//! not `TEMP_FLOOR_C`, so a genuinely cool sensor can still zoom below 30°C),
//! power's to `(0, power_range(..).1)`, and ANE's to `(0, ane_range(..).1)`.

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

// ---------------------------------------------------------------------------
// Soft auto-range (single-row sparklines)
// ---------------------------------------------------------------------------

/// Minimum soft-axis span for utilization / memory sparklines, in percentage
/// points. Keeps a near-constant series from having its jitter magnified to the
/// full height of the 4-dot row.
pub const PERCENT_SOFT_MIN_SPAN: f64 = 20.0;

/// Coarse grid, in percentage points, that utilization / memory soft-axis
/// bounds round outward to (the stateless-hysteresis step).
pub const PERCENT_SOFT_GRID: f64 = 5.0;

/// Hard domain for utilization / memory soft axes: `0..100` percent.
pub const PERCENT_DOMAIN: (f64, f64) = (0.0, 100.0);

/// Minimum soft-axis span for temperature sparklines, in °C.
pub const TEMP_SOFT_MIN_SPAN: f64 = 10.0;

/// Coarse grid, in °C, that temperature soft-axis bounds round outward to.
pub const TEMP_SOFT_GRID: f64 = 5.0;

/// Minimum soft-axis span for ANE-power sparklines, in W. Small, since the ANE
/// operates in the single-watt range.
pub const ANE_SOFT_MIN_SPAN: f64 = 2.0;

/// Coarse grid, in W, that ANE soft-axis bounds round outward to.
pub const ANE_SOFT_GRID: f64 = 1.0;

/// Absolute floor for the package-power soft-axis minimum span, in W. Applied
/// when a fraction of the ceiling would be smaller (e.g. Apple Silicon, where
/// the ceiling is only ~20 W).
pub const POWER_SOFT_MIN_SPAN_FLOOR: f64 = 2.0;

/// Fraction of the fixed power ceiling used as the soft-axis minimum span,
/// floored at [`POWER_SOFT_MIN_SPAN_FLOOR`].
pub const POWER_SOFT_MIN_SPAN_FRACTION: f64 = 0.2;

/// Soft-axis minimum span (W) for package power, derived from the fixed
/// `ceiling`: 20% of the ceiling, never below [`POWER_SOFT_MIN_SPAN_FLOOR`].
///
/// A non-finite ceiling degrades to the floor (`f64::max` ignores `NaN`).
#[must_use]
pub fn power_soft_min_span(ceiling: f64) -> f64 {
    (POWER_SOFT_MIN_SPAN_FRACTION * ceiling).max(POWER_SOFT_MIN_SPAN_FLOOR)
}

/// Soft-axis grid step (W) for package power, chosen as a "nice" step for the
/// magnitude of the fixed `ceiling`:
/// - `1 W` for small budgets (`≤ 20 W`, e.g. a single Apple Silicon package),
/// - `5 W` up to `100 W` (a single discrete GPU),
/// - `25 W` above that (multi-GPU nodes with kilowatt budgets).
///
/// A non-finite ceiling degrades to the coarsest step.
#[must_use]
pub fn power_soft_grid(ceiling: f64) -> f64 {
    if ceiling <= 20.0 {
        1.0
    } else if ceiling <= 100.0 {
        5.0
    } else {
        25.0
    }
}

/// Compute a soft-zoom sparkline axis `(min, max)` from the visible `history`
/// window, for single-row sparklines.
///
/// This is a pure function of its arguments with no hidden state: the same
/// inputs always yield the same axis, so it can be called every frame without
/// storing anything in `AppState`.
///
/// The algorithm, in order:
/// 1. Take the finite min/max of `history`, clamped into the hard `domain`
///    (a sample can stray outside it, e.g. a mis-scaled remote reading, and
///    must not drag the axis out with it). Empty history or all-non-finite
///    input yields a `min_span`-wide window anchored at the domain floor.
/// 2. Enforce `min_span`: if the window is narrower, expand it symmetrically
///    around the data center, then slide it back inside the domain (preserving
///    the span) if an edge is crossed. If the domain is narrower than
///    `min_span`, the window collapses to the whole domain.
/// 3. Round the bounds outward to `grid` (floor the low bound, ceil the high
///    bound). This is the stateless hysteresis: overlapping windows whose
///    extremes differ slightly round to the same axis.
/// 4. Clamp to the hard `domain`. Because rounding only ever widens the axis
///    and step 2 kept it inside the domain, clamping cannot shrink the span
///    below `min_span` unless the domain itself is narrower.
///
/// Degenerate inputs degrade gracefully and never panic or invert the axis:
/// `min_span <= 0` (or non-finite) disables step 2, `grid <= 0` (or
/// non-finite) disables step 3, and an inverted / zero-width / non-finite
/// `domain` disables steps 2's shifting and step 4's clamping (a data-driven
/// axis is still returned).
#[must_use]
pub fn soft_range(history: &[f64], min_span: f64, grid: f64, domain: (f64, f64)) -> (f64, f64) {
    let (dlo, dhi) = domain;
    // A usable hard domain needs finite, correctly-ordered bounds. When it is
    // degenerate or inverted we still return a sensible data-driven axis, just
    // without domain shifting / clamping.
    let domain_valid = dlo.is_finite() && dhi.is_finite() && dhi > dlo;
    // Non-positive / non-finite guardrail parameters simply disable their step.
    let span = if min_span.is_finite() && min_span > 0.0 {
        min_span
    } else {
        0.0
    };
    let use_grid = grid.is_finite() && grid > 0.0;

    // 1. Finite min/max over the window.
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for &v in history {
        if v.is_finite() {
            if v < lo {
                lo = v;
            }
            if v > hi {
                hi = v;
            }
        }
    }

    if !(lo.is_finite() && hi.is_finite()) {
        // Empty history or all-non-finite: anchor a min-span window at the
        // domain floor (0 when the domain is unusable).
        let base = if dlo.is_finite() { dlo } else { 0.0 };
        lo = base;
        hi = base + span;
    }

    // Clamp the raw extrema into the domain before span enforcement: when
    // every sample lies outside the domain and the raw span already meets
    // `min_span`, step 2 would be skipped and step 4 alone would collapse
    // the axis to a zero-width range *outside* the domain (e.g. samples
    // [95, 105] against a 0-90 domain). Clamping here keeps the axis
    // in-domain and lets step 2 rebuild a min-span window from the edge.
    if domain_valid {
        lo = lo.clamp(dlo, dhi);
        hi = hi.clamp(dlo, dhi);
    }

    // 2. Minimum-span enforcement.
    if hi - lo < span {
        let center = (lo + hi) / 2.0;
        lo = center - span / 2.0;
        hi = center + span / 2.0;
        if domain_valid {
            if lo < dlo {
                let shift = dlo - lo;
                lo = dlo;
                hi += shift;
            }
            if hi > dhi {
                let shift = hi - dhi;
                hi = dhi;
                lo -= shift;
                // Domain narrower than the span: collapse to the domain.
                if lo < dlo {
                    lo = dlo;
                }
            }
        }
    }

    // 3. Round bounds outward to the coarse grid.
    if use_grid {
        lo = (lo / grid).floor() * grid;
        hi = (hi / grid).ceil() * grid;
    }

    // 4. Clamp to the hard domain.
    if domain_valid {
        lo = lo.max(dlo);
        hi = hi.min(dhi);
    }

    // Final safety: never emit a non-finite or inverted axis.
    if !(lo.is_finite() && hi.is_finite()) {
        return (0.0, 1.0);
    }
    if hi < lo {
        return (lo, lo);
    }
    (lo, hi)
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

    // --- soft_range: minimum-span enforcement ------------------------------

    #[test]
    fn soft_range_min_span_percent_near_constant() {
        // A near-constant utilization series is expanded to at least the
        // per-metric minimum span so its jitter is not magnified to full height.
        // center 41.3 -> expand to [31.3, 51.3] -> round outward to [30, 55].
        let (lo, hi) = soft_range(
            &[41.3, 41.3, 41.3],
            PERCENT_SOFT_MIN_SPAN,
            PERCENT_SOFT_GRID,
            PERCENT_DOMAIN,
        );
        assert!(hi - lo >= PERCENT_SOFT_MIN_SPAN);
        assert_eq!((lo, hi), (30.0, 55.0));
    }

    #[test]
    fn soft_range_min_span_hugging_zero() {
        // Data hugging 0 slides the min-span window up so it stays inside the
        // domain while keeping the full span. center 1 -> [-9, 11] -> shift up
        // to [0, 20] -> grid [0, 20].
        let (lo, hi) = soft_range(
            &[0.0, 1.0, 2.0],
            PERCENT_SOFT_MIN_SPAN,
            PERCENT_SOFT_GRID,
            PERCENT_DOMAIN,
        );
        assert_eq!((lo, hi), (0.0, 20.0));
        assert!(hi - lo >= PERCENT_SOFT_MIN_SPAN);
    }

    #[test]
    fn soft_range_min_span_hugging_hundred() {
        // Data hugging 100 slides the min-span window down. center 99 ->
        // [89, 109] -> shift down to [80, 100] -> grid [80, 100].
        let (lo, hi) = soft_range(
            &[98.0, 99.0, 100.0],
            PERCENT_SOFT_MIN_SPAN,
            PERCENT_SOFT_GRID,
            PERCENT_DOMAIN,
        );
        assert_eq!((lo, hi), (80.0, 100.0));
        assert!(hi - lo >= PERCENT_SOFT_MIN_SPAN);
    }

    #[test]
    fn soft_range_min_span_temperature() {
        // Near-constant 50°C: span 1 -> expand to [45, 55] -> grid [45, 55].
        let (lo, hi) = soft_range(
            &[49.5, 50.0, 50.5],
            TEMP_SOFT_MIN_SPAN,
            TEMP_SOFT_GRID,
            (0.0, 100.0),
        );
        assert!(hi - lo >= TEMP_SOFT_MIN_SPAN);
        assert_eq!((lo, hi), (45.0, 55.0));
    }

    #[test]
    fn soft_range_min_span_ane() {
        // Idle-ish ANE: span 0.5 -> expand around 0.25 to [-0.75, 1.25] ->
        // shift up to [0, 2] -> grid (1 W) [0, 2].
        let (lo, hi) = soft_range(
            &[0.0, 0.3, 0.5],
            ANE_SOFT_MIN_SPAN,
            ANE_SOFT_GRID,
            (0.0, 10.0),
        );
        assert!(hi - lo >= ANE_SOFT_MIN_SPAN);
        assert_eq!((lo, hi), (0.0, 2.0));
    }

    // --- power soft-axis parameter ladders ---------------------------------

    #[test]
    fn power_soft_min_span_ladder() {
        assert_eq!(power_soft_min_span(20.0), 4.0); // 20% of 20
        assert_eq!(power_soft_min_span(5.0), 2.0); // 20% of 5 = 1 -> floored to 2
        assert_eq!(power_soft_min_span(1000.0), 200.0);
        assert_eq!(power_soft_min_span(f64::NAN), POWER_SOFT_MIN_SPAN_FLOOR);
    }

    #[test]
    fn power_soft_grid_ladder() {
        assert_eq!(power_soft_grid(10.0), 1.0);
        assert_eq!(power_soft_grid(20.0), 1.0);
        assert_eq!(power_soft_grid(80.0), 5.0);
        assert_eq!(power_soft_grid(100.0), 5.0);
        assert_eq!(power_soft_grid(700.0), 25.0);
        assert_eq!(power_soft_grid(f64::NAN), 25.0);
    }

    #[test]
    fn soft_range_min_span_power_apple_silicon() {
        // Apple Silicon ceiling ~20 W -> min span 4 W, grid 1 W. Near-constant
        // 12.5 W: span 0 -> expand to [10.5, 14.5] -> grid [10, 15].
        let ceiling = 20.0;
        let (lo, hi) = soft_range(
            &[12.5, 12.5],
            power_soft_min_span(ceiling),
            power_soft_grid(ceiling),
            (0.0, ceiling),
        );
        assert!(hi - lo >= power_soft_min_span(ceiling));
        assert_eq!((lo, hi), (10.0, 15.0));
    }

    // --- grid-rounding stability (stateless hysteresis) --------------------

    #[test]
    fn soft_range_grid_rounding_is_stable_under_jitter() {
        // Two overlapping windows whose extremes differ slightly round to the
        // same axis, so the sparkline baseline does not flap frame-to-frame.
        // Both spans (7) already exceed the small min-span used here, so no
        // expansion occurs and the raw min/max round to identical bounds.
        let a = soft_range(&[41.0, 44.0, 48.0], 5.0, 5.0, (0.0, 100.0));
        let b = soft_range(&[42.0, 45.0, 49.0], 5.0, 5.0, (0.0, 100.0));
        assert_eq!(a, b);
        assert_eq!(a, (40.0, 50.0));
    }

    // --- hard-domain clamping ----------------------------------------------

    #[test]
    fn soft_range_clamps_to_percent_domain() {
        // Out-of-range data (e.g. a bad remote scrape) is clamped to [0, 100].
        // min -5 max 130 -> grid [-5, 130] -> clamp [0, 100].
        let (lo, hi) = soft_range(
            &[-5.0, 40.0, 130.0],
            PERCENT_SOFT_MIN_SPAN,
            PERCENT_SOFT_GRID,
            PERCENT_DOMAIN,
        );
        assert!(lo >= 0.0 && hi <= 100.0);
        assert_eq!((lo, hi), (0.0, 100.0));
    }

    #[test]
    fn soft_range_clamps_to_temperature_domain() {
        // Ceiling 90°C: the 95°C sample is first clamped into the domain
        // ([85, 90], span 5), then the min span (10) is rebuilt from the edge
        // ([80, 90]), so clamping never shrinks the span below the minimum.
        let (lo, hi) = soft_range(
            &[85.0, 95.0],
            TEMP_SOFT_MIN_SPAN,
            TEMP_SOFT_GRID,
            (0.0, 90.0),
        );
        assert!(hi <= 90.0);
        assert_eq!((lo, hi), (80.0, 90.0));
    }

    #[test]
    fn soft_range_clamps_to_power_domain() {
        // Ceiling 100 W -> min span 20 W, grid 5 W. Data [95, 98] near the top:
        // expand around 96.5 to [86.5, 106.5] -> shift down to [80, 100] ->
        // grid [80, 100] -> clamp keeps hi at 100.
        let ceiling = 100.0;
        let (lo, hi) = soft_range(
            &[95.0, 98.0],
            power_soft_min_span(ceiling),
            power_soft_grid(ceiling),
            (0.0, ceiling),
        );
        assert!(hi <= ceiling);
        assert_eq!((lo, hi), (80.0, 100.0));
    }

    // --- degenerate inputs -------------------------------------------------

    #[test]
    fn soft_range_empty_history_anchors_at_domain_floor() {
        // Empty history -> a min-span window anchored at the domain floor.
        let (lo, hi) = soft_range(
            &[],
            PERCENT_SOFT_MIN_SPAN,
            PERCENT_SOFT_GRID,
            PERCENT_DOMAIN,
        );
        assert_eq!((lo, hi), (0.0, 20.0));
    }

    #[test]
    fn soft_range_all_nan_history_anchors_at_domain_floor() {
        let (lo, hi) = soft_range(
            &[f64::NAN, f64::INFINITY, f64::NEG_INFINITY],
            TEMP_SOFT_MIN_SPAN,
            TEMP_SOFT_GRID,
            (0.0, 90.0),
        );
        assert_eq!((lo, hi), (0.0, 10.0));
        assert!(hi > lo);
    }

    #[test]
    fn soft_range_min_span_wider_than_domain_returns_full_domain() {
        // A min_span larger than the domain collapses to the whole domain
        // rather than overflowing it (the allowed narrower-domain exception).
        let (lo, hi) = soft_range(&[40.0, 50.0], 200.0, 5.0, (0.0, 100.0));
        assert_eq!((lo, hi), (0.0, 100.0));
    }

    #[test]
    fn soft_range_non_positive_grid_skips_rounding() {
        // grid <= 0 disables rounding; the axis is still valid and honours the
        // min span. min 41 max 42 span 1 -> expand around 41.5 to [31.5, 51.5].
        let (lo, hi) = soft_range(&[41.0, 42.0], PERCENT_SOFT_MIN_SPAN, 0.0, PERCENT_DOMAIN);
        assert!(hi - lo >= PERCENT_SOFT_MIN_SPAN);
        assert!(lo >= 0.0 && hi <= 100.0);
        assert_eq!((lo, hi), (31.5, 51.5));
    }

    #[test]
    fn soft_range_never_inverts_on_degenerate_domain() {
        // Inverted or zero-width domains must not produce an inverted axis; a
        // data-driven axis is returned without clamping.
        let (lo, hi) = soft_range(&[10.0, 20.0], 5.0, 5.0, (100.0, 0.0));
        assert!(hi >= lo);
        assert_eq!((lo, hi), (10.0, 20.0));
        let (lo2, hi2) = soft_range(&[10.0, 20.0], 5.0, 5.0, (50.0, 50.0));
        assert!(hi2 >= lo2);
    }

    #[test]
    fn soft_range_non_finite_domain_degrades_gracefully() {
        // A non-finite domain bound disables clamping but still returns a
        // finite, non-inverted axis.
        let (lo, hi) = soft_range(&[10.0, 20.0], 5.0, 5.0, (0.0, f64::INFINITY));
        assert!(lo.is_finite() && hi.is_finite());
        assert!(hi >= lo);
    }

    #[test]
    fn soft_range_out_of_domain_data_stays_inside_domain() {
        // All samples above the domain ceiling with a raw span already >=
        // min_span: without the pre-clamp, span enforcement is skipped and
        // the final clamp collapses to a zero-width axis outside the domain
        // ((95, 95) here). The axis must instead land inside the domain with
        // its minimum span rebuilt from the edge.
        assert_eq!(
            soft_range(&[95.0, 105.0], 10.0, 5.0, (0.0, 90.0)),
            (80.0, 90.0)
        );
        // Symmetric case below the domain floor.
        assert_eq!(
            soft_range(&[-20.0, -5.0], 10.0, 5.0, (0.0, 90.0)),
            (0.0, 10.0)
        );
        // Mixed in/out-of-domain samples keep the in-domain part.
        assert_eq!(
            soft_range(&[85.0, 105.0], 10.0, 5.0, (0.0, 90.0)),
            (80.0, 90.0)
        );
    }

    // --- badge formatting for soft ranges ----------------------------------

    #[test]
    fn scale_badge_shows_soft_range() {
        // The badge must reflect the actual soft range in use, not a fixed one.
        let (lo, hi) = soft_range(
            &[41.3, 41.3, 41.3, 41.3],
            PERCENT_SOFT_MIN_SPAN,
            PERCENT_SOFT_GRID,
            PERCENT_DOMAIN,
        );
        assert_eq!(scale_badge(lo, hi), "30-55");
    }
}
