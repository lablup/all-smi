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
//
// (Feature-gated. This file is only compiled when `--features level_zero`
// is active. Without the feature there are NO Level Zero symbols in the
// binary.)

//! Opt-in Intel Level Zero Sysman backend used as the preferred Intel
//! vendor source when `--features level_zero` is enabled and the loader
//! is present. The default build still compiles no Level Zero symbols.
//!
//! The backend dynamically resolves only the Sysman functions all-smi
//! consumes: temperature, memory state, frequency, fan state,
//! engine-activity deltas, and power energy-counter deltas. Metric
//! families are independent: missing optional symbols or unsupported
//! domains degrade only that field, and seeded delta families do not
//! overwrite OS-specific fallbacks until a fresh second sample exists.
//!
//! ## Coexistence model
//!
//! Linux starts from sysfs/hwmon/fdinfo and Windows starts from WMI.
//! Fresh Sysman values override those baselines per field except Linux
//! fan telemetry, where hwmon keeps priority. `detail["Source: <field>"]`
//! exposes mixed-source results.
//!
//! ## Threading model
//!
//! `IntelGpuReader::get_gpu_info` and
//! `IntelWindowsGpuReader::get_gpu_info` are invoked from a single
//! collector thread today. L0 device handles are not freely shareable
//! across threads, but every call goes through `LevelZeroState`'s
//! per-card `Mutex` so concurrent invocation is safe (just serialised
//! per card).

#![allow(dead_code)] // Some helpers are unused on the non-target OS half.
#![allow(non_camel_case_types)] // FFI handle wrappers mirror C type names.

mod api;
mod apply;
pub(crate) mod ffi;
mod loader;
mod point;
mod refresh;

#[cfg(test)]
mod tests;

pub use apply::{ApplyPlatform, apply_to_gpu_info};
#[allow(unused_imports)]
// `normalise_pci_bdf` is consumed by the per-OS readers wired in commits 3-4.
pub use loader::normalise_pci_bdf;
pub use loader::prepare_sysman_env_for_legacy_runtime;
pub(crate) use loader::with_runtime;
pub(crate) use point::{
    FanSample, FrequencySample, MemorySample, TemperatureSample, populate_point_samples,
    refresh_fan, refresh_frequency, refresh_memory, refresh_temperature,
};
pub(crate) use refresh::{
    EngineSample, PowerSample, populate_engine_samples, populate_power_samples, refresh_engines,
    refresh_power,
};

/// Per-card mutable state held inside a `Mutex` next to the existing
/// `EngineState`. Captures the L0 device handle (resolved on the first
/// successful refresh keyed by PCI BDF) and the previous-tick samples
/// needed for delta-based engine activity and power readings.
#[derive(Debug, Default)]
pub struct LevelZeroState {
    /// `true` once we have at least attempted to bind this card to an
    /// L0 device handle. Avoids re-running the PCI lookup on every
    /// refresh once we've discovered the card is invisible to L0.
    pub(crate) bind_attempted: bool,
    /// Resolved L0 device handle for the card, when binding succeeded.
    pub(crate) device: Option<loader::zes_device_handle_t_send>,
    /// Per-engine running samples (handle + active_time + timestamp).
    /// L0 enumeration order is stable per handle so we resolve the
    /// previous sample by linear search.
    pub(crate) engine_samples: Vec<EngineSample>,
    /// Per-power-domain running samples (handle + energy + timestamp).
    /// Multiple domains are common on multi-tile parts; v1 picks the
    /// largest delta as the card-level total since the spec does not
    /// publish whether the package domain is "domain 0".
    pub(crate) power_samples: Vec<PowerSample>,
    pub(crate) temperature_samples: Vec<TemperatureSample>,
    pub(crate) memory_samples: Vec<MemorySample>,
    pub(crate) frequency_samples: Vec<FrequencySample>,
    pub(crate) fan_samples: Vec<FanSample>,
}

impl LevelZeroState {
    pub fn empty() -> Self {
        Self::default()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MetricStatus {
    #[default]
    Unsupported,
    Unavailable,
    Seeded,
    Fresh,
}

#[derive(Debug, Clone, Default)]
pub struct LevelZeroDiagnostics {
    pub engine: MetricStatus,
    pub power: MetricStatus,
    pub temperature: MetricStatus,
    pub memory: MetricStatus,
    pub frequency: MetricStatus,
    pub fan: MetricStatus,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FreshValue<T> {
    pub value: T,
    pub source: &'static str,
}

impl<T> FreshValue<T> {
    pub(crate) fn level_zero(value: T) -> Self {
        Self {
            value,
            source: "Level Zero Sysman",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelZeroMemoryKind {
    DedicatedLocal,
    SharedSystem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LevelZeroMemoryReadout {
    pub used_bytes: u64,
    pub total_bytes: u64,
    pub kind: LevelZeroMemoryKind,
    pub source: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LevelZeroFanReadout {
    pub rpm: Option<u32>,
    pub percent: Option<u32>,
    pub source: &'static str,
}

/// Aggregated outcome of one [`refresh`] call. Each metric family has
/// independent freshness so a seeded power/engine sample cannot hide a
/// valid OS-specific fallback.
#[derive(Debug, Clone, Default)]
pub struct LevelZeroReadout {
    /// Per-engine percentages keyed by short, stable human-readable
    /// label (`"compute (XMX)"`, `"render"`, etc.). Cached for the
    /// `detail` map only — the primary `GpuInfo.utilization` is driven
    /// by `max(render, compute (XMX))` (see [`apply_to_gpu_info`]).
    pub engines: Vec<(&'static str, f64)>,
    pub primary_engine_utilization: Option<FreshValue<f64>>,
    pub power_watts: Option<FreshValue<f64>>,
    pub temperature_celsius: Option<FreshValue<u32>>,
    pub memory: Option<LevelZeroMemoryReadout>,
    pub frequency_mhz: Option<FreshValue<u32>>,
    pub frequency_domains: Vec<(&'static str, u32)>,
    pub fan: Option<LevelZeroFanReadout>,
    pub diagnostics: LevelZeroDiagnostics,
}

impl LevelZeroReadout {
    pub fn has_fresh_data(&self) -> bool {
        !self.engines.is_empty()
            || self.primary_engine_utilization.is_some()
            || self.power_watts.is_some()
            || self.temperature_celsius.is_some()
            || self.memory.is_some()
            || self.frequency_mhz.is_some()
            || self.fan.is_some()
    }
}

/// Drive one refresh for a card. Returns `None` when L0 is unavailable
/// or this card is not visible to L0, in which case the caller leaves
/// the existing sysfs / WMI metrics untouched.
///
/// `pci_bdf` must be the canonical lowercase string per
/// [`normalise_pci_bdf`].
pub fn refresh(state: &mut LevelZeroState, pci_bdf: &str) -> Option<LevelZeroReadout> {
    with_runtime(|runtime| {
        // On first use for this card, look up its L0 handle by PCI BDF.
        if !state.bind_attempted {
            state.bind_attempted = true;
            state.device = runtime.devices_by_pci.get(pci_bdf).copied();
            if state.device.is_some() {
                // Enumerate engine handles + power domains lazily on
                // bind success. Failures here flip the state into "no
                // L0 data for this card" without retrying.
                populate_engine_samples(&runtime.api, state);
                populate_power_samples(&runtime.api, state);
                populate_point_samples(&runtime.api, state);
            }
        }
        state.device?;

        let mut out = LevelZeroReadout::default();

        let engines = refresh_engines(&runtime.api, state);
        if let Some(primary) = engines.primary {
            out.primary_engine_utilization = Some(FreshValue::level_zero(primary));
            out.diagnostics.engine = MetricStatus::Fresh;
        } else if engines.seeded {
            out.diagnostics.engine = MetricStatus::Seeded;
        } else if state.engine_samples.is_empty() {
            out.diagnostics.engine = MetricStatus::Unsupported;
        } else {
            out.diagnostics.engine = MetricStatus::Unavailable;
        }
        if !engines.entries.is_empty() {
            out.engines = engines.entries;
        }

        if let Some(watts) = refresh_power(&runtime.api, state) {
            out.power_watts = Some(FreshValue::level_zero(watts));
            out.diagnostics.power = MetricStatus::Fresh;
        } else if !state.power_samples.is_empty() {
            out.diagnostics.power = MetricStatus::Seeded;
        }

        if let Some(temp) = refresh_temperature(&runtime.api, state) {
            out.temperature_celsius = Some(temp);
            out.diagnostics.temperature = MetricStatus::Fresh;
        } else if !state.temperature_samples.is_empty() {
            out.diagnostics.temperature = MetricStatus::Unavailable;
        }

        if let Some(memory) = refresh_memory(&runtime.api, state) {
            out.memory = Some(memory);
            out.diagnostics.memory = match memory.kind {
                LevelZeroMemoryKind::DedicatedLocal => MetricStatus::Fresh,
                LevelZeroMemoryKind::SharedSystem => MetricStatus::Unavailable,
            };
        } else if !state.memory_samples.is_empty() {
            out.diagnostics.memory = MetricStatus::Unavailable;
        }

        let (frequency, domains) = refresh_frequency(&runtime.api, state);
        out.frequency_domains = domains;
        if let Some(freq) = frequency {
            out.frequency_mhz = Some(freq);
            out.diagnostics.frequency = MetricStatus::Fresh;
        } else if !state.frequency_samples.is_empty() {
            out.diagnostics.frequency = MetricStatus::Unavailable;
        }

        if let Some(fan) = refresh_fan(&runtime.api, state) {
            out.fan = Some(fan);
            out.diagnostics.fan = MetricStatus::Fresh;
        } else if !state.fan_samples.is_empty() {
            out.diagnostics.fan = MetricStatus::Unavailable;
        }

        Some(out)
    })
    .flatten()
}

/// Map a `zes_engine_group_t` value to the short, stable label we
/// surface in the `detail` map. The "compute (XMX)" label is explicit
/// about the role of the `COMPUTE_SINGLE` group on Arc / Battlemage:
/// it is the dedicated AI / XMX engine, distinct from the
/// `RENDER_SINGLE` engine that handles general compute on the same
/// hardware.
pub(crate) fn engine_label(group: i32) -> &'static str {
    match group {
        ffi::ZES_ENGINE_GROUP_COMPUTE_SINGLE => "compute (XMX)",
        ffi::ZES_ENGINE_GROUP_RENDER_SINGLE => "render",
        ffi::ZES_ENGINE_GROUP_COPY_SINGLE => "copy",
        ffi::ZES_ENGINE_GROUP_MEDIA_DECODE_SINGLE => "media-decode",
        ffi::ZES_ENGINE_GROUP_MEDIA_ENCODE_SINGLE => "media-encode",
        _ => "other",
    }
}

/// Engine groups we surface in v1. Aggregated `_ALL` groups are
/// excluded to avoid double-counting against the per-engine `_SINGLE`
/// readings the same handle list also exposes.
pub(crate) fn is_tracked_engine(group: i32) -> bool {
    matches!(
        group,
        ffi::ZES_ENGINE_GROUP_COMPUTE_SINGLE
            | ffi::ZES_ENGINE_GROUP_RENDER_SINGLE
            | ffi::ZES_ENGINE_GROUP_COPY_SINGLE
            | ffi::ZES_ENGINE_GROUP_MEDIA_DECODE_SINGLE
            | ffi::ZES_ENGINE_GROUP_MEDIA_ENCODE_SINGLE
    )
}

pub(crate) fn label_order(class: &str) -> u8 {
    match class {
        "render" => 0,
        "compute (XMX)" => 1,
        "copy" => 2,
        "media-decode" => 3,
        "media-encode" => 4,
        _ => 5,
    }
}

/// Pick the busiest engine percentage to drive `GpuInfo.utilization`
/// on the Windows path (where WMI gives us nothing). Prefer the
/// busier of render / XMX compute, fall back to the max across the
/// whole readout if neither is present.
pub(crate) fn primary_utilization(engines: &[(&'static str, f64)]) -> Option<f64> {
    if engines.is_empty() {
        return None;
    }
    let busy_compute = engines
        .iter()
        .filter(|(l, _)| *l == "render" || *l == "compute (XMX)")
        .map(|(_, p)| *p)
        .fold(f64::NEG_INFINITY, f64::max);
    if busy_compute.is_finite() {
        return Some(busy_compute);
    }
    Some(engines.iter().map(|(_, p)| *p).fold(0.0_f64, f64::max))
}

/// Convenience used by external diagnostics — surfaced as a `detail`
/// entry when callers want to expose the raw enumerated engine count
/// without invoking a full refresh.
pub fn engine_count(state: &LevelZeroState) -> usize {
    state.engine_samples.len()
}

/// Snapshot the BDF strings the L0 runtime knows about, sorted to
/// give the caller a deterministic ordinal mapping. Used by the
/// Windows reader to pair L0 device handles with WMI Intel video
/// controllers when no shared per-card identifier is available
/// (`Win32_VideoController.PNPDeviceID` does not expose the BDF in a
/// stable, parseable form across driver versions).
///
/// Returns an empty list when the L0 runtime is unavailable.
pub fn enumerated_pci_bdfs() -> Vec<String> {
    with_runtime(|runtime| {
        let mut keys: Vec<String> = runtime.devices_by_pci.keys().cloned().collect();
        keys.sort();
        keys
    })
    .unwrap_or_default()
}

/// Convenience for diagnostics: number of power domains the L0 layer
/// discovered for this card.
pub fn power_domain_count(state: &LevelZeroState) -> usize {
    state.power_samples.len()
}

/// Convenience for diagnostics: did the L0 layer bind this card to a
/// device handle?
pub fn is_bound(state: &LevelZeroState) -> bool {
    state.device.is_some()
}

/// Build a stable, deterministic ordering of engine labels for the
/// detail map. Exposed via `Vec<(&'static str, f64)>` everywhere; this
/// helper is exported only for testability.
pub(crate) fn sort_engine_entries(engines: &mut [(&'static str, f64)]) {
    engines.sort_by(|a, b| label_order(a.0).cmp(&label_order(b.0)).then(a.0.cmp(b.0)));
}
