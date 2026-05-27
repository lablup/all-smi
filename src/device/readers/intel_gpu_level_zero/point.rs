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

use super::api::LzApi;
use super::loader::cap_handle_count;
use super::{
    FreshValue, LevelZeroFanReadout, LevelZeroMemoryKind, LevelZeroMemoryReadout, LevelZeroState,
    ffi,
};
use std::ffi::c_void;

const MAX_GPU_TEMP_CELSIUS: u32 = 125;
const MAX_GPU_FREQ_MHZ: u32 = 5000;
const MAX_GPU_MEMORY_BYTES: u64 = 96 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub(crate) struct TemperatureSample {
    pub(crate) handle: zes_temp_handle_t_send,
    pub(crate) sensor_type: i32,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct MemorySample {
    pub(crate) handle: zes_mem_handle_t_send,
    pub(crate) location: i32,
    pub(crate) physical_size: u64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct FrequencySample {
    pub(crate) handle: zes_freq_handle_t_send,
    pub(crate) domain: i32,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct FanSample {
    pub(crate) handle: zes_fan_handle_t_send,
    pub(crate) supported_units: u32,
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

send_handle!(zes_temp_handle_t_send, ffi::zes_temp_handle_t);
send_handle!(zes_mem_handle_t_send, ffi::zes_mem_handle_t);
send_handle!(zes_freq_handle_t_send, ffi::zes_freq_handle_t);
send_handle!(zes_fan_handle_t_send, ffi::zes_fan_handle_t);

macro_rules! enumerate_handles {
    ($device:expr, $enumerate:expr, $ty:ty, $what:expr) => {{
        let mut count: u32 = 0;
        if unsafe { ($enumerate)($device, &mut count, std::ptr::null_mut()) }
            != ffi::ZE_RESULT_SUCCESS
            || count == 0
        {
            Vec::<$ty>::new()
        } else {
            let (cap, mut count) = cap_handle_count(count, $what);
            let mut handles: Vec<$ty> = vec![std::ptr::null_mut::<c_void>() as $ty; cap];
            let r = unsafe { ($enumerate)($device, &mut count, handles.as_mut_ptr()) };
            if r != ffi::ZE_RESULT_SUCCESS {
                Vec::<$ty>::new()
            } else {
                handles.truncate((count as usize).min(cap));
                handles.into_iter().filter(|h| !h.is_null()).collect()
            }
        }
    }};
}

pub(crate) fn populate_point_samples(api: &LzApi, state: &mut LevelZeroState) {
    populate_temperature_samples(api, state);
    populate_memory_samples(api, state);
    populate_frequency_samples(api, state);
    populate_fan_samples(api, state);
}

fn populate_temperature_samples(api: &LzApi, state: &mut LevelZeroState) {
    let (Some(enumerate), Some(get_properties)) = (
        api.zes_device_enum_temperature_sensors,
        api.zes_temperature_get_properties,
    ) else {
        return;
    };
    let Some(device) = state.device.map(|d| d.0) else {
        return;
    };
    for handle in enumerate_handles!(
        device,
        enumerate,
        ffi::zes_temp_handle_t,
        "temperature sensors"
    ) {
        let mut props = ffi::zes_temp_properties_t::default();
        if unsafe { (get_properties)(handle, &mut props) } == ffi::ZE_RESULT_SUCCESS {
            state.temperature_samples.push(TemperatureSample {
                handle: zes_temp_handle_t_send(handle),
                sensor_type: props.type_,
            });
        }
    }
}

fn populate_memory_samples(api: &LzApi, state: &mut LevelZeroState) {
    let (Some(enumerate), Some(get_properties)) = (
        api.zes_device_enum_memory_modules,
        api.zes_memory_get_properties,
    ) else {
        return;
    };
    let Some(device) = state.device.map(|d| d.0) else {
        return;
    };
    for handle in enumerate_handles!(device, enumerate, ffi::zes_mem_handle_t, "memory modules") {
        let mut props = ffi::zes_mem_properties_t::default();
        if unsafe { (get_properties)(handle, &mut props) } == ffi::ZE_RESULT_SUCCESS {
            state.memory_samples.push(MemorySample {
                handle: zes_mem_handle_t_send(handle),
                location: props.location,
                physical_size: props.physical_size,
            });
        }
    }
}

fn populate_frequency_samples(api: &LzApi, state: &mut LevelZeroState) {
    let (Some(enumerate), Some(get_properties)) = (
        api.zes_device_enum_frequency_domains,
        api.zes_frequency_get_properties,
    ) else {
        return;
    };
    let Some(device) = state.device.map(|d| d.0) else {
        return;
    };
    for handle in enumerate_handles!(
        device,
        enumerate,
        ffi::zes_freq_handle_t,
        "frequency domains"
    ) {
        let mut props = ffi::zes_freq_properties_t::default();
        if unsafe { (get_properties)(handle, &mut props) } == ffi::ZE_RESULT_SUCCESS {
            state.frequency_samples.push(FrequencySample {
                handle: zes_freq_handle_t_send(handle),
                domain: props.type_,
            });
        }
    }
}

fn populate_fan_samples(api: &LzApi, state: &mut LevelZeroState) {
    let (Some(enumerate), Some(get_properties)) =
        (api.zes_device_enum_fans, api.zes_fan_get_properties)
    else {
        return;
    };
    let Some(device) = state.device.map(|d| d.0) else {
        return;
    };
    for handle in enumerate_handles!(device, enumerate, ffi::zes_fan_handle_t, "fans") {
        let mut props = ffi::zes_fan_properties_t::default();
        if unsafe { (get_properties)(handle, &mut props) } == ffi::ZE_RESULT_SUCCESS {
            state.fan_samples.push(FanSample {
                handle: zes_fan_handle_t_send(handle),
                supported_units: props.supported_units,
            });
        }
    }
}

pub(crate) fn refresh_temperature(api: &LzApi, state: &LevelZeroState) -> Option<FreshValue<u32>> {
    let get_state = api.zes_temperature_get_state?;
    let mut best: Option<(u8, u32)> = None;
    for sample in &state.temperature_samples {
        let mut temp = 0.0_f64;
        if unsafe { (get_state)(sample.handle.0, &mut temp) } != ffi::ZE_RESULT_SUCCESS {
            continue;
        }
        if !temp.is_finite() || temp <= 0.0 || temp > f64::from(MAX_GPU_TEMP_CELSIUS) {
            continue;
        }
        let value = temp.round() as u32;
        let rank = temperature_rank(sample.sensor_type);
        if best
            .map(|(best_rank, best_value)| {
                rank < best_rank || (rank == best_rank && value > best_value)
            })
            .unwrap_or(true)
        {
            best = Some((rank, value));
        }
    }
    best.map(|(_, value)| FreshValue::level_zero(value))
}

pub(crate) fn refresh_memory(
    api: &LzApi,
    state: &LevelZeroState,
) -> Option<LevelZeroMemoryReadout> {
    let get_state = api.zes_memory_get_state?;
    let mut total = 0_u64;
    let mut used = 0_u64;
    let mut shared_seen = false;
    for sample in &state.memory_samples {
        let mut mem_state = ffi::zes_mem_state_t::default();
        if unsafe { (get_state)(sample.handle.0, &mut mem_state) } != ffi::ZE_RESULT_SUCCESS {
            continue;
        }
        if sample.location != ffi::ZES_MEM_LOC_DEVICE {
            shared_seen = true;
            continue;
        }
        let module_total = if sample.physical_size > 0 {
            sample.physical_size
        } else {
            mem_state.size
        };
        if module_total == 0 || module_total > MAX_GPU_MEMORY_BYTES {
            continue;
        }
        let module_free = mem_state.free.min(module_total);
        total = total.saturating_add(module_total).min(MAX_GPU_MEMORY_BYTES);
        used = used
            .saturating_add(module_total.saturating_sub(module_free))
            .min(total);
    }
    if total > 0 {
        Some(LevelZeroMemoryReadout {
            used_bytes: used,
            total_bytes: total,
            kind: LevelZeroMemoryKind::DedicatedLocal,
            source: "Level Zero Sysman",
        })
    } else if shared_seen {
        Some(LevelZeroMemoryReadout {
            used_bytes: 0,
            total_bytes: 0,
            kind: LevelZeroMemoryKind::SharedSystem,
            source: "Level Zero Sysman",
        })
    } else {
        None
    }
}

pub(crate) fn refresh_frequency(
    api: &LzApi,
    state: &LevelZeroState,
) -> (Option<FreshValue<u32>>, Vec<(&'static str, u32)>) {
    let Some(get_state) = api.zes_frequency_get_state else {
        return (None, Vec::new());
    };
    let mut best: Option<(u8, u32)> = None;
    let mut domains = Vec::new();
    for sample in &state.frequency_samples {
        let mut freq_state = ffi::zes_freq_state_t::default();
        if unsafe { (get_state)(sample.handle.0, &mut freq_state) } != ffi::ZE_RESULT_SUCCESS {
            continue;
        }
        let actual = freq_state.actual;
        if !actual.is_finite() || actual <= 0.0 || actual > f64::from(MAX_GPU_FREQ_MHZ) {
            continue;
        }
        let mhz = actual.round() as u32;
        let label = frequency_label(sample.domain);
        domains.push((label, mhz));
        let rank = frequency_rank(sample.domain);
        if best
            .map(|(best_rank, best_mhz)| rank < best_rank || (rank == best_rank && mhz > best_mhz))
            .unwrap_or(true)
        {
            best = Some((rank, mhz));
        }
    }
    domains.sort_by(|a, b| a.0.cmp(b.0));
    (best.map(|(_, mhz)| FreshValue::level_zero(mhz)), domains)
}

pub(crate) fn refresh_fan(api: &LzApi, state: &LevelZeroState) -> Option<LevelZeroFanReadout> {
    let get_state = api.zes_fan_get_state?;
    let mut rpm: Option<u32> = None;
    let mut percent: Option<u32> = None;
    for sample in &state.fan_samples {
        if fan_unit_supported(sample.supported_units, ffi::ZES_FAN_SPEED_UNITS_RPM) {
            rpm = rpm.max(read_fan_speed(
                get_state,
                sample.handle.0,
                ffi::ZES_FAN_SPEED_UNITS_RPM,
            ));
        }
        if fan_unit_supported(sample.supported_units, ffi::ZES_FAN_SPEED_UNITS_PERCENT) {
            percent = percent.max(read_fan_speed(
                get_state,
                sample.handle.0,
                ffi::ZES_FAN_SPEED_UNITS_PERCENT,
            ));
        }
    }
    if rpm.is_some() || percent.is_some() {
        Some(LevelZeroFanReadout {
            rpm,
            percent,
            source: "Level Zero Sysman",
        })
    } else {
        None
    }
}

fn temperature_rank(sensor_type: i32) -> u8 {
    match sensor_type {
        ffi::ZES_TEMP_SENSORS_GPU => 0,
        ffi::ZES_TEMP_SENSORS_GLOBAL => 1,
        ffi::ZES_TEMP_SENSORS_GPU_BOARD => 2,
        ffi::ZES_TEMP_SENSORS_MEMORY => 3,
        _ => 10,
    }
}

fn frequency_rank(domain: i32) -> u8 {
    match domain {
        ffi::ZES_FREQ_DOMAIN_GPU => 0,
        ffi::ZES_FREQ_DOMAIN_MEDIA => 1,
        ffi::ZES_FREQ_DOMAIN_MEMORY => 2,
        _ => 10,
    }
}

fn frequency_label(domain: i32) -> &'static str {
    match domain {
        ffi::ZES_FREQ_DOMAIN_GPU => "gpu",
        ffi::ZES_FREQ_DOMAIN_MEDIA => "media",
        ffi::ZES_FREQ_DOMAIN_MEMORY => "memory",
        _ => "other",
    }
}

fn fan_unit_supported(supported_units: u32, unit: i32) -> bool {
    supported_units == 0 || (supported_units & (1_u32 << (unit as u32))) != 0
}

fn read_fan_speed(
    get_state: ffi::ZesFanGetState,
    handle: ffi::zes_fan_handle_t,
    units: i32,
) -> Option<u32> {
    let mut speed = -1_i32;
    if unsafe { (get_state)(handle, units, &mut speed) } == ffi::ZE_RESULT_SUCCESS && speed >= 0 {
        Some(speed as u32)
    } else {
        None
    }
}
