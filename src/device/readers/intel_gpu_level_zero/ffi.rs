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
// is active; otherwise the binary contains zero references to L0 symbols.)

//! Hand-written, minimal FFI bindings for the Intel Level Zero
//! (oneAPI) Sysman API surface that the v1 backend actually calls.
//!
//! This deliberately does **not** vendor the upstream `ze_api.h` /
//! `zes_api.h` headers. We mirror only the typedefs, structs, and enum
//! values needed for two metric categories — engine activity and power
//! via the energy counter — and lean on `libloading` to dynamically
//! resolve symbols at runtime.
//!
//! All FFI signatures and struct layouts here track the Level Zero
//! specification at <https://oneapi-src.github.io/level-zero-spec/>.
//! Enum values are locked in by unit tests in
//! [`super::tests`](../tests/index.html) so a future spec drift cannot
//! silently corrupt the per-engine classification.
//!
//! ## Versioning policy
//!
//! Sysman struct ABI is gated by the `stype` field (a
//! `ze_structure_type_t` constant). For every struct we read we set
//! `stype` to the correct version constant and `pnext` to `null_mut()`
//! before passing the value to the driver, so newer drivers that add
//! optional extensions cannot mis-interpret our smaller struct.

#![allow(non_camel_case_types)]
#![allow(dead_code)]

use std::ffi::c_void;

mod sysman;
pub use sysman::*;

// -- Result code -----------------------------------------------------

/// `ZE_RESULT_SUCCESS` — every Level Zero function returns this on
/// success. Any other value indicates an error; we never need to
/// distinguish individual error codes in v1 (we just degrade), but we
/// surface the numeric value in debug logs for triage.
pub const ZE_RESULT_SUCCESS: i32 = 0;

// -- Init flags ------------------------------------------------------

/// Default `zeInit` flags. The L0 spec accepts `0` ("any device type")
/// or `ZE_INIT_FLAG_GPU_ONLY = 1`. We pass `0` so the loader is free to
/// enumerate non-GPU subdevices in mixed environments.
pub const ZE_INIT_FLAG_DEFAULT: u32 = 0;

// -- ze_structure_type_t (subset) ------------------------------------

/// `ZES_STRUCTURE_TYPE_PCI_PROPERTIES` per the Sysman spec.
pub const ZES_STRUCTURE_TYPE_PCI_PROPERTIES: i32 = 0x0000_0002;
/// `ZES_STRUCTURE_TYPE_ENGINE_PROPERTIES` per the Sysman spec.
pub const ZES_STRUCTURE_TYPE_ENGINE_PROPERTIES: i32 = 0x0000_0005;

// -- zes_engine_group_t (full enum, only some are matched) -----------
//
// Values verified against
// <https://oneapi-src.github.io/level-zero-spec/level-zero/latest/sysman/api.html#zes-engine-group-t>.
// The unit tests in `super::tests` lock these in so any future drift is
// caught by CI rather than producing silently mis-classified telemetry.

pub const ZES_ENGINE_GROUP_ALL: i32 = 0;
pub const ZES_ENGINE_GROUP_COMPUTE_ALL: i32 = 1;
pub const ZES_ENGINE_GROUP_MEDIA_ALL: i32 = 2;
pub const ZES_ENGINE_GROUP_COPY_ALL: i32 = 3;
/// `COMPUTE_SINGLE` — the XMX / AI engine class on Arc / Battlemage.
pub const ZES_ENGINE_GROUP_COMPUTE_SINGLE: i32 = 4;
pub const ZES_ENGINE_GROUP_RENDER_SINGLE: i32 = 5;
pub const ZES_ENGINE_GROUP_MEDIA_DECODE_SINGLE: i32 = 6;
pub const ZES_ENGINE_GROUP_MEDIA_ENCODE_SINGLE: i32 = 7;
pub const ZES_ENGINE_GROUP_COPY_SINGLE: i32 = 8;
pub const ZES_ENGINE_GROUP_MEDIA_ENHANCEMENT_SINGLE: i32 = 9;
/// [DEPRECATED in spec, retained as a value lock.]
pub const ZES_ENGINE_GROUP_3D_SINGLE: i32 = 10;
/// [DEPRECATED in spec, retained as a value lock.]
pub const ZES_ENGINE_GROUP_3D_RENDER_COMPUTE_ALL: i32 = 11;
pub const ZES_ENGINE_GROUP_RENDER_ALL: i32 = 12;
/// [DEPRECATED in spec, retained as a value lock.]
pub const ZES_ENGINE_GROUP_3D_ALL: i32 = 13;
pub const ZES_ENGINE_GROUP_MEDIA_CODEC_SINGLE: i32 = 14;

// -- Opaque handles --------------------------------------------------

pub type ze_driver_handle_t = *mut c_void;
pub type ze_device_handle_t = *mut c_void;
pub type zes_device_handle_t = *mut c_void;
pub type zes_engine_handle_t = *mut c_void;
pub type zes_pwr_handle_t = *mut c_void;

// -- Concrete structs ------------------------------------------------

/// `zes_pci_address_t` — PCI BDF tuple. Matches the Sysman spec
/// exactly: four `uint32_t` fields in this order.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct zes_pci_address_t {
    pub domain: u32,
    pub bus: u32,
    pub device: u32,
    pub function: u32,
}

/// `zes_pci_speed_t` — PCIe link speed tuple. Unused but laid out so
/// the containing `zes_pci_properties_t` stays the spec-correct size.
///
/// The spec field name is `gen`, which is a reserved keyword in the
/// 2024 edition; we expose it as `gen_` to keep the struct buildable
/// while preserving the same `#[repr(C)]` layout the driver writes.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct zes_pci_speed_t {
    pub gen_: i32,
    pub width: i32,
    pub max_bandwidth: i64,
}

/// `zes_pci_properties_t`. Only the `address` field is read.
///
/// `have_bandwidth_counters`, `have_packet_counters`, and
/// `have_replay_counters` are spec-typed `ze_bool_t` which is a
/// `uint8_t` upstream. Declaring them as `u8` keeps the Rust struct
/// layout in lock-step with the 56-byte C struct the driver writes; a
/// previous version of this file declared them as `u32`, which
/// inflated the struct to 64 bytes and could leave the trailing bytes
/// of a driver-written buffer reading garbage. A `#[cfg(test)]` size
/// assertion locks the layout to 56 bytes.
#[repr(C)]
pub struct zes_pci_properties_t {
    pub stype: i32,
    pub pnext: *mut c_void,
    pub address: zes_pci_address_t,
    pub max_speed: zes_pci_speed_t,
    pub have_bandwidth_counters: u8,
    pub have_packet_counters: u8,
    pub have_replay_counters: u8,
}

impl Default for zes_pci_properties_t {
    fn default() -> Self {
        Self {
            stype: ZES_STRUCTURE_TYPE_PCI_PROPERTIES,
            pnext: std::ptr::null_mut(),
            address: zes_pci_address_t::default(),
            max_speed: zes_pci_speed_t::default(),
            have_bandwidth_counters: 0,
            have_packet_counters: 0,
            have_replay_counters: 0,
        }
    }
}

/// `zes_engine_properties_t`. Only `type_` is consumed; the rest is
/// laid out per spec so the driver does not overrun our buffer.
///
/// `on_subdevice` is spec-typed `ze_bool_t` (a `uint8_t` upstream).
/// Total struct size on x86_64 LP64 is 32 bytes; a `#[cfg(test)]` size
/// assertion locks this in.
#[repr(C)]
pub struct zes_engine_properties_t {
    pub stype: i32,
    pub pnext: *mut c_void,
    /// `zes_engine_group_t` enum value — one of the `ZES_ENGINE_GROUP_*`
    /// constants above.
    pub type_: i32,
    /// `ze_bool_t` — non-zero when the engine is bound to a subdevice
    /// (multi-tile parts). We treat any non-zero value as "yes".
    pub on_subdevice: u8,
    pub subdevice_id: u32,
}

impl Default for zes_engine_properties_t {
    fn default() -> Self {
        Self {
            stype: ZES_STRUCTURE_TYPE_ENGINE_PROPERTIES,
            pnext: std::ptr::null_mut(),
            type_: ZES_ENGINE_GROUP_ALL,
            on_subdevice: 0,
            subdevice_id: 0,
        }
    }
}

/// `zes_engine_stats_t` — `active_time` and `timestamp` both in
/// microseconds. Per the spec, divide `delta_active / delta_timestamp`
/// for the engine-busy ratio.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct zes_engine_stats_t {
    pub active_time: u64,
    pub timestamp: u64,
}

/// `zes_power_energy_counter_t` — `energy` in **microjoules**,
/// `timestamp` in microseconds. Both counters are monotonic until the
/// device is reset; we delta-track them between refreshes to derive
/// watts.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct zes_power_energy_counter_t {
    pub energy: u64,
    pub timestamp: u64,
}

// -- Function-pointer typedefs --------------------------------------
//
// One typedef per L0 symbol the v1 backend resolves. Keeping these as
// `unsafe extern "C" fn` lets us store them as plain function pointers
// inside the loader struct without any trait-object indirection.

pub type ZeInit = unsafe extern "C" fn(flags: u32) -> i32;

/// Optional modern Sysman initialiser. Newer Level Zero loaders expose
/// this so callers do not need to mutate `ZES_ENABLE_SYSMAN` before
/// `zeInit`; older loaders may not export it, so the dynamic resolver
/// treats this symbol as optional.
pub type ZesInit = unsafe extern "C" fn(flags: u32) -> i32;

pub type ZeDriverGet =
    unsafe extern "C" fn(p_count: *mut u32, p_drivers: *mut ze_driver_handle_t) -> i32;

pub type ZeDeviceGet = unsafe extern "C" fn(
    h_driver: ze_driver_handle_t,
    p_count: *mut u32,
    p_devices: *mut ze_device_handle_t,
) -> i32;

pub type ZesDevicePciGetProperties = unsafe extern "C" fn(
    h_device: zes_device_handle_t,
    p_properties: *mut zes_pci_properties_t,
) -> i32;

pub type ZesDeviceEnumEngineGroups = unsafe extern "C" fn(
    h_device: zes_device_handle_t,
    p_count: *mut u32,
    p_engines: *mut zes_engine_handle_t,
) -> i32;

pub type ZesEngineGetProperties = unsafe extern "C" fn(
    h_engine: zes_engine_handle_t,
    p_properties: *mut zes_engine_properties_t,
) -> i32;

pub type ZesEngineGetActivity =
    unsafe extern "C" fn(h_engine: zes_engine_handle_t, p_stats: *mut zes_engine_stats_t) -> i32;

pub type ZesDeviceEnumPowerDomains = unsafe extern "C" fn(
    h_device: zes_device_handle_t,
    p_count: *mut u32,
    p_power: *mut zes_pwr_handle_t,
) -> i32;

pub type ZesPowerGetEnergyCounter = unsafe extern "C" fn(
    h_power: zes_pwr_handle_t,
    p_energy: *mut zes_power_energy_counter_t,
) -> i32;
