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
//! Queries MSAcpi_ThermalZoneTemperature from root\WMI namespace.
//! This is the standard Windows method but is not available on all systems.

use super::{TemperatureResult, is_wmi_not_found_error};
use serde::Deserialize;
use wmi::WMIConnection;

/// WMI structure for thermal zone temperature.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
struct ThermalZoneTemperature {
    current_temperature: Option<u32>, // Temperature in tenths of Kelvin
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
    ///
    /// # Returns
    /// * `TemperatureResult::Success(temp)` - Temperature in Celsius
    /// * `TemperatureResult::NotFound` - MSAcpi_ThermalZoneTemperature class not found
    /// * `TemperatureResult::Error` - Transient error (connection issue, etc.)
    /// * `TemperatureResult::NoValidReading` - Class exists but no valid temperature
    pub fn get_temperature(&self, wmi_conn: Option<&WMIConnection>) -> TemperatureResult {
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
                        // Convert from tenths of Kelvin to Celsius
                        // Formula: (K / 10) - 273.15 = C
                        let celsius = (temp_tenths_kelvin as f64 / 10.0) - 273.15;
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
                    // Other WMI error - likely transient
                    TemperatureResult::Error
                }
            }
        }
    }
}
