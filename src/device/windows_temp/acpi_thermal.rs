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

//! ACPI Thermal Zone temperature source.
//!
//! Queries MSAcpi_ThermalZoneTemperature from the root\WMI namespace first.
//! That class is the standard Windows method but is not available on all
//! systems: it is empty, missing, or access-denied for a normal user on
//! many machines (the RTX Spark N1X among them, issue #459). The same ACPI
//! zones are then read from
//! `root\cimv2:Win32_PerfFormattedData_Counters_ThermalZoneInformation`,
//! which needs no elevation.

use super::{TemperatureResult, is_wmi_not_found_error};
use crate::device::windows_thermal_zone::{hottest_zone_celsius, tenths_kelvin_to_celsius};
use serde::Deserialize;
use wmi::WMIConnection;

/// WMI structure for thermal zone temperature.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
struct ThermalZoneTemperature {
    current_temperature: Option<u32>, // Temperature in tenths of Kelvin
}

/// `Win32_PerfFormattedData_Counters_ThermalZoneInformation` row.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
struct ThermalZoneInformation {
    /// Whole Kelvin.
    temperature: Option<u32>,
    /// Tenths of Kelvin.
    high_precision_temperature: Option<u32>,
}

/// ACPI Thermal Zone temperature source.
pub struct AcpiThermalSource {
    // No state needed - uses passed WMI connection
}

impl Default for AcpiThermalSource {
    fn default() -> Self {
        Self::new()
    }
}

impl AcpiThermalSource {
    /// Create a new ACPI thermal source.
    pub fn new() -> Self {
        Self {}
    }

    /// Get temperature from ACPI thermal zones.
    ///
    /// # Arguments
    /// * `wmi_conn` - Optional WMI connection to root\WMI namespace
    /// * `cimv2_conn` - Optional WMI connection to root\cimv2, for the
    ///   performance-counter fallback
    ///
    /// # Returns
    /// * `TemperatureResult::Success(temp)` - Temperature in Celsius
    /// * `TemperatureResult::NotFound` - Neither class exists
    /// * `TemperatureResult::Error` - Transient error (connection issue, etc.)
    /// * `TemperatureResult::NoValidReading` - Class exists but no valid temperature
    pub fn get_temperature(
        &self,
        wmi_conn: Option<&WMIConnection>,
        cimv2_conn: Option<&WMIConnection>,
    ) -> TemperatureResult {
        let primary = Self::msacpi_temperature(wmi_conn);
        if matches!(primary, TemperatureResult::Success(_)) {
            return primary;
        }
        match Self::perf_counter_temperature(cimv2_conn) {
            TemperatureResult::Success(temp) => TemperatureResult::Success(temp),
            // Only call the whole source unavailable when both classes are
            // missing; otherwise report what the primary said.
            TemperatureResult::NotFound => primary,
            fallback => match primary {
                TemperatureResult::NotFound => fallback,
                other => other,
            },
        }
    }

    /// `root\WMI:MSAcpi_ThermalZoneTemperature`.
    fn msacpi_temperature(wmi_conn: Option<&WMIConnection>) -> TemperatureResult {
        let conn = match wmi_conn {
            Some(c) => c,
            None => return TemperatureResult::Error,
        };

        let results: Result<Vec<ThermalZoneTemperature>, _> =
            conn.raw_query("SELECT CurrentTemperature FROM MSAcpi_ThermalZoneTemperature");

        match results {
            Ok(zones) => {
                if zones.is_empty() {
                    // No thermal zones found - this might be a permanent condition
                    return TemperatureResult::NoValidReading;
                }

                for zone in zones {
                    if let Some(temp_tenths_kelvin) = zone.current_temperature {
                        let celsius = tenths_kelvin_to_celsius(temp_tenths_kelvin);
                        if celsius > 0.0 && celsius < 150.0 {
                            // Use round() for more accurate conversion
                            return TemperatureResult::Success(celsius.round() as u32);
                        }
                        // Out of range value, continue to next zone
                    }
                }
                TemperatureResult::NoValidReading
            }
            Err(e) => {
                if is_wmi_not_found_error(&e) {
                    // WBEM_E_NOT_FOUND - class doesn't exist
                    TemperatureResult::NotFound
                } else {
                    // Other WMI error (including access denied) - may be
                    // transient, and the fallback below still gets a try
                    TemperatureResult::Error
                }
            }
        }
    }

    /// `root\cimv2:Win32_PerfFormattedData_Counters_ThermalZoneInformation`.
    fn perf_counter_temperature(cimv2_conn: Option<&WMIConnection>) -> TemperatureResult {
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
