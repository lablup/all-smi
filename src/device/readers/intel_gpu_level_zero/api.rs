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

//! Dynamic Level Zero symbol resolution.

use super::ffi;
use libloading::{Library, Symbol};
use tracing::debug;

/// Function pointer table extracted from the loaded library once.
#[derive(Clone, Copy)]
pub(crate) struct LzApi {
    pub(crate) ze_init: ffi::ZeInit,
    pub(crate) zes_init: Option<ffi::ZesInit>,
    pub(crate) ze_driver_get: ffi::ZeDriverGet,
    pub(crate) ze_device_get: ffi::ZeDeviceGet,
    pub(crate) zes_device_pci_get_properties: ffi::ZesDevicePciGetProperties,
    pub(crate) zes_device_enum_engine_groups: Option<ffi::ZesDeviceEnumEngineGroups>,
    pub(crate) zes_engine_get_properties: Option<ffi::ZesEngineGetProperties>,
    pub(crate) zes_engine_get_activity: Option<ffi::ZesEngineGetActivity>,
    pub(crate) zes_device_enum_power_domains: Option<ffi::ZesDeviceEnumPowerDomains>,
    pub(crate) zes_power_get_energy_counter: Option<ffi::ZesPowerGetEnergyCounter>,
    pub(crate) zes_device_enum_temperature_sensors: Option<ffi::ZesDeviceEnumTemperatureSensors>,
    pub(crate) zes_temperature_get_properties: Option<ffi::ZesTemperatureGetProperties>,
    pub(crate) zes_temperature_get_state: Option<ffi::ZesTemperatureGetState>,
    pub(crate) zes_device_enum_memory_modules: Option<ffi::ZesDeviceEnumMemoryModules>,
    pub(crate) zes_memory_get_properties: Option<ffi::ZesMemoryGetProperties>,
    pub(crate) zes_memory_get_state: Option<ffi::ZesMemoryGetState>,
    pub(crate) zes_device_enum_frequency_domains: Option<ffi::ZesDeviceEnumFrequencyDomains>,
    pub(crate) zes_frequency_get_properties: Option<ffi::ZesFrequencyGetProperties>,
    pub(crate) zes_frequency_get_state: Option<ffi::ZesFrequencyGetState>,
    pub(crate) zes_device_enum_fans: Option<ffi::ZesDeviceEnumFans>,
    pub(crate) zes_fan_get_properties: Option<ffi::ZesFanGetProperties>,
    pub(crate) zes_fan_get_state: Option<ffi::ZesFanGetState>,
}

/// Wrapper returned by [`try_load_library`] and owned by the static runtime cell.
pub struct LoadedLibrary {
    pub(crate) library: Library,
    pub(crate) api: LzApi,
}

/// Attempt to load the Level Zero loader at the given path and resolve symbols.
///
/// Required symbols cover process init, driver/device enumeration, and PCI BDF
/// lookup. Metric-family symbols are optional so older loaders can still serve
/// whichever Sysman domains they expose.
pub unsafe fn try_load_library(path: &str) -> Option<LoadedLibrary> {
    unsafe {
        debug!("Level Zero: trying to load loader at {path}");
        let lib = match Library::new(path) {
            Ok(l) => l,
            Err(e) => {
                debug!("Level Zero: failed to load {path}: {e}");
                return None;
            }
        };

        let ze_init: Symbol<ffi::ZeInit> = lib.get(b"zeInit\0").ok()?;
        let ze_driver_get: Symbol<ffi::ZeDriverGet> = lib.get(b"zeDriverGet\0").ok()?;
        let ze_device_get: Symbol<ffi::ZeDeviceGet> = lib.get(b"zeDeviceGet\0").ok()?;
        let zes_device_pci_get_properties: Symbol<ffi::ZesDevicePciGetProperties> =
            lib.get(b"zesDevicePciGetProperties\0").ok()?;

        let api = LzApi {
            ze_init: *ze_init,
            zes_init: optional(&lib, b"zesInit\0"),
            ze_driver_get: *ze_driver_get,
            ze_device_get: *ze_device_get,
            zes_device_pci_get_properties: *zes_device_pci_get_properties,
            zes_device_enum_engine_groups: optional(&lib, b"zesDeviceEnumEngineGroups\0"),
            zes_engine_get_properties: optional(&lib, b"zesEngineGetProperties\0"),
            zes_engine_get_activity: optional(&lib, b"zesEngineGetActivity\0"),
            zes_device_enum_power_domains: optional(&lib, b"zesDeviceEnumPowerDomains\0"),
            zes_power_get_energy_counter: optional(&lib, b"zesPowerGetEnergyCounter\0"),
            zes_device_enum_temperature_sensors: optional(
                &lib,
                b"zesDeviceEnumTemperatureSensors\0",
            ),
            zes_temperature_get_properties: optional(&lib, b"zesTemperatureGetProperties\0"),
            zes_temperature_get_state: optional(&lib, b"zesTemperatureGetState\0"),
            zes_device_enum_memory_modules: optional(&lib, b"zesDeviceEnumMemoryModules\0"),
            zes_memory_get_properties: optional(&lib, b"zesMemoryGetProperties\0"),
            zes_memory_get_state: optional(&lib, b"zesMemoryGetState\0"),
            zes_device_enum_frequency_domains: optional(&lib, b"zesDeviceEnumFrequencyDomains\0"),
            zes_frequency_get_properties: optional(&lib, b"zesFrequencyGetProperties\0"),
            zes_frequency_get_state: optional(&lib, b"zesFrequencyGetState\0"),
            zes_device_enum_fans: optional(&lib, b"zesDeviceEnumFans\0"),
            zes_fan_get_properties: optional(&lib, b"zesFanGetProperties\0"),
            zes_fan_get_state: optional(&lib, b"zesFanGetState\0"),
        };

        Some(LoadedLibrary { library: lib, api })
    }
}

unsafe fn optional<T: Copy>(lib: &Library, name: &[u8]) -> Option<T> {
    unsafe { lib.get::<T>(name).ok().map(|sym| *sym) }
}
