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

//! Opt-in Intel Level Zero (oneAPI) backend used to augment the
//! sysfs-based Linux reader and to fill the Windows WMI gap. v1 surface:
//!
//! 1. **Per-engine activity** via
//!    [`ffi::ZesDeviceEnumEngineGroups`](ffi::ZesDeviceEnumEngineGroups) +
//!    [`ffi::ZesEngineGetActivity`](ffi::ZesEngineGetActivity). We
//!    surface `RENDER_SINGLE`, `COMPUTE_SINGLE` (the XMX class on Arc /
//!    Battlemage), `COPY_SINGLE`, `MEDIA_DECODE_SINGLE`, and
//!    `MEDIA_ENCODE_SINGLE`. Anything else the driver enumerates is
//!    silently ignored.
//! 2. **Power** via
//!    [`ffi::ZesDeviceEnumPowerDomains`](ffi::ZesDeviceEnumPowerDomains) +
//!    [`ffi::ZesPowerGetEnergyCounter`](ffi::ZesPowerGetEnergyCounter).
//!    The counter is monotonic in microjoules; we delta-track it to
//!    derive instantaneous watts. The very first sample seeds the
//!    baseline and reports `None`.
//!
//! Explicitly **deferred** to follow-up issues (do not add here):
//! temperature, frequency, memory state, performance factor, RAS /
//! error reporting, per-process L0 stats, fine-grained power-limit
//! control. The v1 scope is intentionally narrow so the PR stays
//! reviewable.
//!
//! ## Coexistence model
//!
//! L0 **augments** rather than replaces the existing per-OS readers:
//!
//! * Linux: PR #249's sysfs engine counters keep producing
//!   `GpuInfo.utilization` and per-engine `detail` entries. L0 adds the
//!   XMX `COMPUTE_SINGLE` activity that sysfs cannot reach plus the
//!   energy-counter-derived `Power (L0)` reading, then flips
//!   `detail["Metrics Source"]` from `"sysfs (engine counters)"` to
//!   `"sysfs + Level Zero"`.
//! * Windows: WMI keeps producing the name + (truncated) `AdapterRAM`.
//!   L0 fills `GpuInfo.utilization` and `GpuInfo.power_consumption`
//!   (both zero on the WMI-only path) and flips
//!   `detail["Metrics Source"]` from `"WMI"` to `"WMI + Level Zero"`.
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

pub(crate) mod ffi;
mod loader;
mod refresh;

#[cfg(test)]
mod tests;

#[allow(unused_imports)]
// `normalise_pci_bdf` is consumed by the per-OS readers wired in commits 3-4.
pub use loader::normalise_pci_bdf;
pub(crate) use loader::with_runtime;
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
}

impl LevelZeroState {
    pub fn empty() -> Self {
        Self::default()
    }
}

/// Aggregated outcome of one [`refresh`] call.
///
/// Kept intentionally narrow: the rest of the reader pipeline does not
/// need to know about L0 handle layout, only the human-presentable
/// numbers we wish to surface.
#[derive(Debug, Clone, Default)]
pub struct LevelZeroReadout {
    /// Per-engine percentages keyed by short, stable human-readable
    /// label (`"compute (XMX)"`, `"render"`, etc.). Cached for the
    /// `detail` map only — the primary `GpuInfo.utilization` is driven
    /// by `max(render, compute (XMX))` (see [`apply_to_gpu_info`]).
    pub engines: Vec<(&'static str, f64)>,
    /// Card-level power in watts derived from the energy counter delta.
    /// `None` on the seeding call (no prior sample yet).
    pub power_watts: Option<f64>,
    /// `true` when the refresh produced at least one fresh data point —
    /// callers use this to decide whether to flip
    /// `detail["Metrics Source"]` to indicate the augmented backend.
    pub had_any_data: bool,
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
            }
        }
        state.device?;

        let mut out = LevelZeroReadout::default();

        let engines = refresh_engines(&runtime.api, state);
        if !engines.is_empty() {
            out.engines = engines;
            out.had_any_data = true;
        }

        if let Some(watts) = refresh_power(&runtime.api, state) {
            out.power_watts = Some(watts);
            out.had_any_data = true;
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

/// Fold a [`LevelZeroReadout`] into an existing `GpuInfo` produced by
/// the sysfs / WMI baseline. On Linux this **augments** the detail map
/// without touching `utilization` (the sysfs engine counters remain
/// the source of truth for the primary number); on Windows it
/// **overwrites** `utilization` and `power_consumption` because the
/// WMI baseline reports zero for both.
///
/// The `Metrics Source` detail flips from the baseline string
/// (`"sysfs (engine counters)"` / `"WMI"`) to the augmented one
/// (`"sysfs + Level Zero"` / `"WMI + Level Zero"`) only when the
/// readout actually carried fresh data.
pub fn apply_to_gpu_info(
    gpu_info: &mut crate::device::types::GpuInfo,
    readout: &LevelZeroReadout,
    platform: ApplyPlatform,
) {
    if !readout.had_any_data {
        return;
    }

    // Per-engine percentages — always added on both platforms.
    for (label, pct) in &readout.engines {
        let key = format!("Engine: {label} (L0)");
        gpu_info.detail.insert(key, format!("{pct:.2}%"));
    }

    // Power (L0) detail — kept regardless of platform so the operator
    // can see the energy-counter-derived reading even when the sysfs
    // path produces its own slightly-different number.
    if let Some(watts) = readout.power_watts {
        gpu_info
            .detail
            .insert("Power (L0)".to_string(), format!("{watts:.2} W"));
    }

    match platform {
        ApplyPlatform::Linux => {
            gpu_info.detail.insert(
                "Metrics Source".to_string(),
                "sysfs + Level Zero".to_string(),
            );
        }
        ApplyPlatform::Windows => {
            // Overwrite the zeros WMI produced. Use the busiest engine
            // as the primary utilization (max across render + XMX
            // compute, matching the Linux sysfs convention).
            if let Some(primary) = primary_utilization(&readout.engines) {
                gpu_info.utilization = primary.clamp(0.0, 100.0);
            }
            if let Some(watts) = readout.power_watts {
                gpu_info.power_consumption = watts.max(0.0);
            }
            gpu_info
                .detail
                .insert("Metrics Source".to_string(), "WMI + Level Zero".to_string());
        }
    }
}

/// Selector for [`apply_to_gpu_info`]'s platform-specific behaviour.
#[derive(Debug, Clone, Copy)]
pub enum ApplyPlatform {
    Linux,
    Windows,
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
