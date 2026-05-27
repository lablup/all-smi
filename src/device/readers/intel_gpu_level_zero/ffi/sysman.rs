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

use std::ffi::c_void;

pub const ZES_STRUCTURE_TYPE_FAN_PROPERTIES: i32 = 0x0000_0007;
pub const ZES_STRUCTURE_TYPE_FREQ_PROPERTIES: i32 = 0x0000_0009;
pub const ZES_STRUCTURE_TYPE_MEM_PROPERTIES: i32 = 0x0000_000b;
pub const ZES_STRUCTURE_TYPE_TEMP_PROPERTIES: i32 = 0x0000_0014;
pub const ZES_STRUCTURE_TYPE_FREQ_STATE: i32 = 0x0000_001b;
pub const ZES_STRUCTURE_TYPE_MEM_STATE: i32 = 0x0000_001e;

pub const ZES_TEMP_SENSORS_GLOBAL: i32 = 0;
pub const ZES_TEMP_SENSORS_GPU: i32 = 1;
pub const ZES_TEMP_SENSORS_MEMORY: i32 = 2;
pub const ZES_TEMP_SENSORS_GPU_BOARD: i32 = 6;

pub const ZES_MEM_LOC_SYSTEM: i32 = 0;
pub const ZES_MEM_LOC_DEVICE: i32 = 1;

pub const ZES_FREQ_DOMAIN_GPU: i32 = 0;
pub const ZES_FREQ_DOMAIN_MEMORY: i32 = 1;
pub const ZES_FREQ_DOMAIN_MEDIA: i32 = 2;

pub const ZES_FAN_SPEED_UNITS_RPM: i32 = 0;
pub const ZES_FAN_SPEED_UNITS_PERCENT: i32 = 1;

pub type zes_temp_handle_t = *mut c_void;
pub type zes_mem_handle_t = *mut c_void;
pub type zes_freq_handle_t = *mut c_void;
pub type zes_fan_handle_t = *mut c_void;

#[repr(C)]
pub struct zes_temp_properties_t {
    pub stype: i32,
    pub pnext: *mut c_void,
    pub type_: i32,
    pub on_subdevice: u8,
    pub subdevice_id: u32,
    pub max_temperature: f64,
    pub is_critical_temp_supported: u8,
    pub is_threshold1_supported: u8,
    pub is_threshold2_supported: u8,
}

impl Default for zes_temp_properties_t {
    fn default() -> Self {
        Self {
            stype: ZES_STRUCTURE_TYPE_TEMP_PROPERTIES,
            pnext: std::ptr::null_mut(),
            type_: ZES_TEMP_SENSORS_GLOBAL,
            on_subdevice: 0,
            subdevice_id: 0,
            max_temperature: 0.0,
            is_critical_temp_supported: 0,
            is_threshold1_supported: 0,
            is_threshold2_supported: 0,
        }
    }
}

#[repr(C)]
pub struct zes_mem_properties_t {
    pub stype: i32,
    pub pnext: *mut c_void,
    pub type_: i32,
    pub on_subdevice: u8,
    pub subdevice_id: u32,
    pub location: i32,
    pub physical_size: u64,
    pub bus_width: i32,
    pub num_channels: i32,
}

impl Default for zes_mem_properties_t {
    fn default() -> Self {
        Self {
            stype: ZES_STRUCTURE_TYPE_MEM_PROPERTIES,
            pnext: std::ptr::null_mut(),
            type_: 0,
            on_subdevice: 0,
            subdevice_id: 0,
            location: ZES_MEM_LOC_SYSTEM,
            physical_size: 0,
            bus_width: -1,
            num_channels: -1,
        }
    }
}

#[repr(C)]
pub struct zes_mem_state_t {
    pub stype: i32,
    pub pnext: *const c_void,
    pub health: i32,
    pub free: u64,
    pub size: u64,
}

impl Default for zes_mem_state_t {
    fn default() -> Self {
        Self {
            stype: ZES_STRUCTURE_TYPE_MEM_STATE,
            pnext: std::ptr::null(),
            health: 0,
            free: 0,
            size: 0,
        }
    }
}

#[repr(C)]
pub struct zes_freq_properties_t {
    pub stype: i32,
    pub pnext: *mut c_void,
    pub type_: i32,
    pub on_subdevice: u8,
    pub subdevice_id: u32,
    pub can_control: u8,
    pub is_throttle_event_supported: u8,
    pub min: f64,
    pub max: f64,
}

impl Default for zes_freq_properties_t {
    fn default() -> Self {
        Self {
            stype: ZES_STRUCTURE_TYPE_FREQ_PROPERTIES,
            pnext: std::ptr::null_mut(),
            type_: ZES_FREQ_DOMAIN_GPU,
            on_subdevice: 0,
            subdevice_id: 0,
            can_control: 0,
            is_throttle_event_supported: 0,
            min: 0.0,
            max: 0.0,
        }
    }
}

#[repr(C)]
pub struct zes_freq_state_t {
    pub stype: i32,
    pub pnext: *const c_void,
    pub current_voltage: f64,
    pub request: f64,
    pub tdp: f64,
    pub efficient: f64,
    pub actual: f64,
    pub throttle_reasons: u32,
}

impl Default for zes_freq_state_t {
    fn default() -> Self {
        Self {
            stype: ZES_STRUCTURE_TYPE_FREQ_STATE,
            pnext: std::ptr::null(),
            current_voltage: -1.0,
            request: -1.0,
            tdp: -1.0,
            efficient: -1.0,
            actual: -1.0,
            throttle_reasons: 0,
        }
    }
}

#[repr(C)]
pub struct zes_fan_properties_t {
    pub stype: i32,
    pub pnext: *mut c_void,
    pub on_subdevice: u8,
    pub subdevice_id: u32,
    pub can_control: u8,
    pub supported_modes: u32,
    pub supported_units: u32,
    pub max_rpm: i32,
    pub max_points: i32,
}

impl Default for zes_fan_properties_t {
    fn default() -> Self {
        Self {
            stype: ZES_STRUCTURE_TYPE_FAN_PROPERTIES,
            pnext: std::ptr::null_mut(),
            on_subdevice: 0,
            subdevice_id: 0,
            can_control: 0,
            supported_modes: 0,
            supported_units: 0,
            max_rpm: -1,
            max_points: -1,
        }
    }
}

pub type ZesDeviceEnumTemperatureSensors = unsafe extern "C" fn(
    h_device: super::zes_device_handle_t,
    p_count: *mut u32,
    p_sensors: *mut zes_temp_handle_t,
) -> i32;
pub type ZesTemperatureGetProperties = unsafe extern "C" fn(
    h_temperature: zes_temp_handle_t,
    p_properties: *mut zes_temp_properties_t,
) -> i32;
pub type ZesTemperatureGetState =
    unsafe extern "C" fn(h_temperature: zes_temp_handle_t, p_temperature: *mut f64) -> i32;

pub type ZesDeviceEnumMemoryModules = unsafe extern "C" fn(
    h_device: super::zes_device_handle_t,
    p_count: *mut u32,
    p_memory: *mut zes_mem_handle_t,
) -> i32;
pub type ZesMemoryGetProperties = unsafe extern "C" fn(
    h_memory: zes_mem_handle_t,
    p_properties: *mut zes_mem_properties_t,
) -> i32;
pub type ZesMemoryGetState =
    unsafe extern "C" fn(h_memory: zes_mem_handle_t, p_state: *mut zes_mem_state_t) -> i32;

pub type ZesDeviceEnumFrequencyDomains = unsafe extern "C" fn(
    h_device: super::zes_device_handle_t,
    p_count: *mut u32,
    p_frequency: *mut zes_freq_handle_t,
) -> i32;
pub type ZesFrequencyGetProperties = unsafe extern "C" fn(
    h_frequency: zes_freq_handle_t,
    p_properties: *mut zes_freq_properties_t,
) -> i32;
pub type ZesFrequencyGetState =
    unsafe extern "C" fn(h_frequency: zes_freq_handle_t, p_state: *mut zes_freq_state_t) -> i32;

pub type ZesDeviceEnumFans = unsafe extern "C" fn(
    h_device: super::zes_device_handle_t,
    p_count: *mut u32,
    p_fans: *mut zes_fan_handle_t,
) -> i32;
pub type ZesFanGetProperties =
    unsafe extern "C" fn(h_fan: zes_fan_handle_t, p_properties: *mut zes_fan_properties_t) -> i32;
pub type ZesFanGetState =
    unsafe extern "C" fn(h_fan: zes_fan_handle_t, units: i32, p_speed: *mut i32) -> i32;
