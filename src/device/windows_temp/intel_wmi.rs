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

//! Intel WMI temperature source.
//!
//! Queries the root/Intel WMI namespace for thermal zone information.
//! This is available on some Intel systems with proper chipset drivers.

use super::{TemperatureResult, is_wmi_not_found_error};
use crate::device::windows_wmi_worker::INTEL;
use once_cell::sync::OnceCell;
use serde::Deserialize;
use wmi::WMIConnection;

/// WMI structure for Intel thermal zone information.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
struct IntelThermalZone {
    /// Current temperature in Celsius or Kelvin (varies by implementation)
    current_temperature: Option<u32>,
    /// Some Intel implementations use Temperature instead
    #[serde(default)]
    temperature: Option<u32>,
}

/// Intel WMI temperature source.
///
/// The `root\Intel` connection itself is owned and reused by the WMI worker
/// (`windows_wmi_worker`); this source caches only whether the namespace
/// exists at all.
pub struct IntelWmiSource {
    /// Whether the Intel WMI namespace is available
    namespace_available: OnceCell<bool>,
}

impl Default for IntelWmiSource {
    fn default() -> Self {
        Self::new()
    }
}

impl IntelWmiSource {
    /// Create a new Intel WMI source.
    pub fn new() -> Self {
        Self {
            namespace_available: OnceCell::new(),
        }
    }

    /// Check if the Intel WMI namespace is available.
    fn is_namespace_available(&self) -> bool {
        *self
            .namespace_available
            .get_or_init(|| WMIConnection::with_namespace_path(INTEL).is_ok())
    }

    /// Get temperature from Intel WMI over the cached `root\Intel`
    /// connection (`None` when it could not be opened).
    ///
    /// # Returns
    /// * `TemperatureResult::Success(temp)` - Temperature in Celsius
    /// * `TemperatureResult::NotFound` - Intel WMI namespace not available
    /// * `TemperatureResult::Error` - Transient error during query
    /// * `TemperatureResult::NoValidReading` - Query succeeded but returned invalid data
    pub fn get_temperature(&self, connection: Option<&WMIConnection>) -> TemperatureResult {
        // Check if namespace is available (cached)
        if !self.is_namespace_available() {
            return TemperatureResult::NotFound;
        }

        let Some(connection) = connection else {
            return TemperatureResult::Error;
        };
        // A query that failed for a reason other than a missing class: the
        // connection may be stale, so report it and let the owner reconnect.
        let mut query_failed = false;

        // Try different Intel thermal zone classes
        // Intel implementations vary - try common class names
        let queries = [
            "SELECT CurrentTemperature, Temperature FROM ThermalZoneInformation",
            "SELECT CurrentTemperature, Temperature FROM Intel_ThermalZone",
        ];

        for query in queries {
            let results: Result<Vec<IntelThermalZone>, _> = connection.raw_query(query);

            match results {
                Ok(zones) if !zones.is_empty() => {
                    for zone in zones {
                        // Try CurrentTemperature first, then Temperature
                        let temp_value = zone.current_temperature.or(zone.temperature);

                        if let Some(temp) = temp_value {
                            // Intel may report in Celsius directly or in tenths of Kelvin
                            let celsius = if temp > 200 {
                                // Likely tenths of Kelvin
                                (temp as f64 / 10.0) - 273.15
                            } else {
                                // Already in Celsius
                                temp as f64
                            };

                            if celsius > 0.0 && celsius < 150.0 {
                                // Use round() for more accurate conversion
                                return TemperatureResult::Success(celsius.round() as u32);
                            }
                        }
                    }
                }
                Ok(_) => continue, // Empty result, try next query
                Err(e) if is_wmi_not_found_error(&e) => continue,
                Err(_) => query_failed = true,
            }
        }

        if query_failed {
            TemperatureResult::Error
        } else {
            // The namespace exists but has no usable thermal data.
            TemperatureResult::NotFound
        }
    }
}
