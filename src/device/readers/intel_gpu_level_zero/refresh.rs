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

//! Per-engine and per-power-domain refresh logic. Split out of
//! `intel_gpu_level_zero.rs` so the FFI-heavy core can be tested
//! independently of the public surface and so each file stays under
//! the 500-line budget.

use super::api::LzApi;
use super::loader::cap_handle_count;
use super::{LevelZeroState, ffi, is_tracked_engine, label_order};
use std::collections::HashMap;
use std::ffi::c_void;

const MAX_GPU_POWER_WATTS: f64 = 750.0;

/// Per-engine running snapshot. Same delta-tracking shape as the sysfs
/// `EngineSample` used by `intel_gpu_engine` — `last_active_us` is
/// monotonic until the device is reset.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EngineSample {
    pub(crate) handle: zes_engine_handle_t_send,
    /// `zes_engine_group_t` value the driver reported for this engine.
    pub(crate) group: i32,
    pub(crate) last_active_us: u64,
    pub(crate) last_timestamp_us: u64,
    pub(crate) last_busy_pct: f64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PowerSample {
    pub(crate) handle: zes_pwr_handle_t_send,
    pub(crate) last_energy_uj: u64,
    pub(crate) last_timestamp_us: u64,
}

macro_rules! send_handle {
    ($name:ident, $raw:ty) => {
        #[derive(Clone, Copy)]
        pub(crate) struct $name(pub(crate) $raw);
        unsafe impl Send for $name {}
        unsafe impl Sync for $name {}
        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_tuple(stringify!($name))
                    .field(&(self.0 as usize))
                    .finish()
            }
        }
    };
}

send_handle!(zes_engine_handle_t_send, ffi::zes_engine_handle_t);
send_handle!(zes_pwr_handle_t_send, ffi::zes_pwr_handle_t);

/// Enumerate per-engine handles for the bound L0 device. Filters the
/// raw enumeration to only the engine groups we surface in v1 (see
/// [`super::is_tracked_engine`]) so that aggregated `_ALL` totals do
/// not double-count against the per-engine `_SINGLE` readings.
pub(crate) fn populate_engine_samples(api: &LzApi, state: &mut LevelZeroState) {
    let (Some(enum_engine_groups), Some(get_properties)) = (
        api.zes_device_enum_engine_groups,
        api.zes_engine_get_properties,
    ) else {
        return;
    };
    let device = match state.device {
        Some(d) => d.0,
        None => return,
    };
    let mut count: u32 = 0;
    // SAFETY: null pointer for count-only call per spec.
    let r = unsafe { (enum_engine_groups)(device, &mut count, std::ptr::null_mut()) };
    if r != ffi::ZE_RESULT_SUCCESS || count == 0 {
        return;
    }
    // Cap the driver-reported count to MAX_L0_HANDLES before allocating
    // — a buggy driver returning u32::MAX would otherwise OOM us.
    let (cap, mut count) = cap_handle_count(count, "engine groups");
    let mut handles: Vec<ffi::zes_engine_handle_t> = vec![std::ptr::null_mut::<c_void>(); cap];
    // SAFETY: handles is sized exactly to count (capped).
    let r = unsafe { (enum_engine_groups)(device, &mut count, handles.as_mut_ptr()) };
    if r != ffi::ZE_RESULT_SUCCESS {
        return;
    }
    handles.truncate((count as usize).min(cap));
    for handle in handles.into_iter() {
        if handle.is_null() {
            continue;
        }
        let mut props = ffi::zes_engine_properties_t::default();
        // SAFETY: props is correctly initialised; driver writes the rest.
        let r = unsafe { (get_properties)(handle, &mut props) };
        if r != ffi::ZE_RESULT_SUCCESS {
            continue;
        }
        if !is_tracked_engine(props.type_) {
            continue;
        }
        state.engine_samples.push(EngineSample {
            handle: zes_engine_handle_t_send(handle),
            group: props.type_,
            last_active_us: 0,
            last_timestamp_us: 0,
            last_busy_pct: 0.0,
        });
    }
}

/// Enumerate per-power-domain handles for the bound L0 device. We do
/// not filter by domain type in v1 — the largest delta wins as the
/// card-level power, which is the correct behaviour on single-tile
/// parts and a conservative approximation on multi-tile ones until a
/// follow-up adds explicit domain-class handling.
pub(crate) fn populate_power_samples(api: &LzApi, state: &mut LevelZeroState) {
    let Some(enum_power_domains) = api.zes_device_enum_power_domains else {
        return;
    };
    let device = match state.device {
        Some(d) => d.0,
        None => return,
    };
    let mut count: u32 = 0;
    // SAFETY: null pointer for count-only call per spec.
    let r = unsafe { (enum_power_domains)(device, &mut count, std::ptr::null_mut()) };
    if r != ffi::ZE_RESULT_SUCCESS || count == 0 {
        return;
    }
    // Cap the driver-reported count to MAX_L0_HANDLES before allocating.
    let (cap, mut count) = cap_handle_count(count, "power domains");
    let mut handles: Vec<ffi::zes_pwr_handle_t> = vec![std::ptr::null_mut::<c_void>(); cap];
    // SAFETY: handles is sized exactly to count (capped).
    let r = unsafe { (enum_power_domains)(device, &mut count, handles.as_mut_ptr()) };
    if r != ffi::ZE_RESULT_SUCCESS {
        return;
    }
    handles.truncate((count as usize).min(cap));
    for handle in handles.into_iter() {
        if handle.is_null() {
            continue;
        }
        state.power_samples.push(PowerSample {
            handle: zes_pwr_handle_t_send(handle),
            last_energy_uj: 0,
            last_timestamp_us: 0,
        });
    }
}

/// Read every enumerated engine handle once and compute the per-class
/// busy percentage against the previous sample. Returns a sorted list
/// of `(label, percentage)` pairs.
#[derive(Debug, Clone, Default)]
pub(crate) struct EngineRefresh {
    pub(crate) entries: Vec<(&'static str, f64)>,
    pub(crate) primary: Option<f64>,
    pub(crate) seeded: bool,
}

pub(crate) fn refresh_engines(api: &LzApi, state: &mut LevelZeroState) -> EngineRefresh {
    if state.engine_samples.is_empty() {
        return EngineRefresh::default();
    }
    let Some(get_activity) = api.zes_engine_get_activity else {
        return EngineRefresh::default();
    };
    let mut per_group: HashMap<&'static str, f64> = HashMap::new();
    let mut any_sample = false;
    let mut any_fresh = false;
    for sample in state.engine_samples.iter_mut() {
        let mut stats = ffi::zes_engine_stats_t::default();
        // SAFETY: stats is fully initialised; driver writes the fields.
        let r = unsafe { (get_activity)(sample.handle.0, &mut stats) };
        if r != ffi::ZE_RESULT_SUCCESS {
            continue;
        }
        let was_seeded = sample.last_timestamp_us == 0;
        let pct = compute_engine_busy_pct(sample, &stats);
        sample.last_active_us = stats.active_time;
        sample.last_timestamp_us = stats.timestamp;
        sample.last_busy_pct = pct;
        any_sample = true;
        if was_seeded {
            continue;
        }
        any_fresh = true;
        let key = super::engine_label(sample.group);
        let entry = per_group.entry(key).or_insert(0.0);
        if pct > *entry {
            *entry = pct;
        }
    }
    if !any_sample || !any_fresh {
        return EngineRefresh {
            entries: Vec::new(),
            primary: None,
            seeded: any_sample,
        };
    }
    let mut out: Vec<(&'static str, f64)> = per_group.into_iter().collect();
    out.sort_by(|a, b| label_order(a.0).cmp(&label_order(b.0)).then(a.0.cmp(b.0)));
    let primary = super::primary_utilization(&out).map(|p| p.clamp(0.0, 100.0));
    EngineRefresh {
        entries: out,
        primary,
        seeded: false,
    }
}

/// Read every enumerated power-domain handle once and compute the
/// largest watts value across domains as the card-level power.
/// Returns `None` on the seeding call (no previous baseline) or when
/// no domain produced a delta.
pub(crate) fn refresh_power(api: &LzApi, state: &mut LevelZeroState) -> Option<f64> {
    if state.power_samples.is_empty() {
        return None;
    }
    let get_counter = api.zes_power_get_energy_counter?;
    let mut best: Option<f64> = None;
    for sample in state.power_samples.iter_mut() {
        let mut counter = ffi::zes_power_energy_counter_t::default();
        // SAFETY: counter is fully initialised; driver writes its fields.
        let r = unsafe { (get_counter)(sample.handle.0, &mut counter) };
        if r != ffi::ZE_RESULT_SUCCESS {
            continue;
        }
        let watts = compute_power_watts(sample, &counter);
        sample.last_energy_uj = counter.energy;
        sample.last_timestamp_us = counter.timestamp;
        if let Some(w) = watts
            && w <= MAX_GPU_POWER_WATTS
            && (best.is_none() || best.unwrap() < w)
        {
            best = Some(w);
        }
    }
    best
}

/// Convert an engine activity sample pair into a busy percentage. The
/// first call (when `last_timestamp_us == 0`) seeds the baseline and
/// returns `0.0` — matching the seeding semantics of the sysfs engine
/// reader so the two backends behave consistently on the first
/// refresh.
pub(crate) fn compute_engine_busy_pct(
    sample: &EngineSample,
    stats: &ffi::zes_engine_stats_t,
) -> f64 {
    if sample.last_timestamp_us == 0 || stats.timestamp <= sample.last_timestamp_us {
        return 0.0;
    }
    let delta_t = stats.timestamp.saturating_sub(sample.last_timestamp_us);
    if delta_t == 0 {
        return 0.0;
    }
    let delta_active = stats.active_time.saturating_sub(sample.last_active_us);
    ((delta_active as f64) / (delta_t as f64) * 100.0).clamp(0.0, 100.0)
}

/// Convert an energy-counter sample pair into watts. Returns `None` on
/// the seeding call (no previous baseline yet) or when wall time has
/// not advanced.
///
/// Both counters share the same SI-prefix scale: energy is in
/// **microjoules**, timestamp in **microseconds**. The ratio of those
/// units is therefore (µJ / µs) = (10⁻⁶ J) / (10⁻⁶ s) = J/s = **watts**
/// directly, with no further scaling. A common bug is to assume the
/// ratio yields microwatts; that would require energy in joules or
/// timestamp in seconds, neither of which the L0 spec promises.
pub(crate) fn compute_power_watts(
    sample: &PowerSample,
    counter: &ffi::zes_power_energy_counter_t,
) -> Option<f64> {
    if sample.last_timestamp_us == 0 || counter.timestamp <= sample.last_timestamp_us {
        return None;
    }
    let delta_t_us = counter.timestamp.saturating_sub(sample.last_timestamp_us);
    if delta_t_us == 0 {
        return None;
    }
    if counter.energy < sample.last_energy_uj {
        return None;
    }
    let delta_e_uj = counter.energy - sample.last_energy_uj;
    // (µJ / µs) = (10⁻⁶ J) / (10⁻⁶ s) = J/s = W. Do NOT divide by 1e6.
    let watts = (delta_e_uj as f64) / (delta_t_us as f64);
    if watts.is_finite() && watts >= 0.0 {
        Some(watts)
    } else {
        None
    }
}

// -- Test-only constructors -------------------------------------------------

#[cfg(test)]
pub(crate) fn make_engine_sample(group: i32, active: u64, ts: u64) -> EngineSample {
    EngineSample {
        handle: zes_engine_handle_t_send(std::ptr::null_mut()),
        group,
        last_active_us: active,
        last_timestamp_us: ts,
        last_busy_pct: 0.0,
    }
}

#[cfg(test)]
pub(crate) fn make_power_sample(energy: u64, ts: u64) -> PowerSample {
    PowerSample {
        handle: zes_pwr_handle_t_send(std::ptr::null_mut()),
        last_energy_uj: energy,
        last_timestamp_us: ts,
    }
}
