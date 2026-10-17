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

//! ACPI thermal zones through the performance-counter WMI class.
//!
//! `root\cimv2:Win32_PerfFormattedData_Counters_ThermalZoneInformation`
//! exposes the same ACPI zones as `MSAcpi_ThermalZoneTemperature` but needs
//! no elevation. It is the last source in the fallback chain: on x86_64
//! Intel and AMD machines the vendor and LibreHardwareMonitor sources read
//! the CPU package sensor, which is closer to the cores than an ACPI zone,
//! so this class only answers when none of them does (the RTX Spark N1X,
//! issue #459, where `root\WMI` is access-denied to a normal user).

use super::{TemperatureResult, is_wmi_not_found_error};
use crate::device::windows_thermal_zone::hottest_zone_celsius;
use serde::Deserialize;
use wmi::WMIConnection;

/// `Win32_PerfFormattedData_Counters_ThermalZoneInformation` row.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
struct ThermalZoneInformation {
    /// Whole Kelvin.
    temperature: Option<u32>,
    /// Tenths of Kelvin.
    high_precision_temperature: Option<u32>,
}

/// Thermal-zone performance-counter temperature source.
#[derive(Debug, Default)]
pub struct ThermalZoneInfoSource;

impl ThermalZoneInfoSource {
    /// Create a new source.
    pub fn new() -> Self {
        Self
    }

    /// Hottest plausible thermal zone, in Celsius.
    pub fn get_temperature(&self, cimv2_conn: Option<&WMIConnection>) -> TemperatureResult {
        let Some(conn) = cimv2_conn else {
            return TemperatureResult::Error;
        };
        let results: Result<Vec<ThermalZoneInformation>, _> = conn.raw_query(
            "SELECT Temperature, HighPrecisionTemperature \
             FROM Win32_PerfFormattedData_Counters_ThermalZoneInformation",
        );
        match results {
            Ok(zones) => {
                let readings: Vec<(Option<u32>, Option<u32>)> = zones
                    .iter()
                    .map(|zone| (zone.high_precision_temperature, zone.temperature))
                    .collect();
                match hottest_zone_celsius(&readings) {
                    Some(celsius) => TemperatureResult::Success(celsius),
                    None => TemperatureResult::NoValidReading,
                }
            }
            Err(e) if is_wmi_not_found_error(&e) => TemperatureResult::NotFound,
            Err(_) => TemperatureResult::Error,
        }
    }
}
