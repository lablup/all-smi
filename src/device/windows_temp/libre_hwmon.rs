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

//! LibreHardwareMonitor WMI temperature source.
//!
//! LibreHardwareMonitor is an open-source application that can read temperatures
//! from various hardware sensors and exposes them via WMI.
//!
//! Reference: https://github.com/LibreHardwareMonitor/LibreHardwareMonitor
//!
//! Note: LibreHardwareMonitor must be running for this source to work.
//! The user should be advised to run LibreHardwareMonitor if they want
//! temperature monitoring on Windows systems without ACPI thermal zones.

use super::{TemperatureResult, is_wmi_not_found_error};
use crate::device::windows_wmi_worker::{LIBRE_HARDWARE_MONITOR, OPEN_HARDWARE_MONITOR};
use once_cell::sync::OnceCell;
use serde::Deserialize;
use wmi::WMIConnection;

/// WMI structure for LibreHardwareMonitor sensor.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
struct LhmSensor {
    /// Sensor name (e.g., "CPU Package", "CPU Core #1")
    name: Option<String>,
    /// Sensor type (e.g., "Temperature", "Voltage", "Clock")
    /// Note: This field is required for WMI deserialization but not used in code
    /// because we filter by SensorType='Temperature' in the query.
    #[allow(dead_code)]
    sensor_type: Option<String>,
    /// Current sensor value
    value: Option<f32>,
    /// Parent hardware identifier
    #[serde(default)]
    parent: Option<String>,
}

/// Which namespace is available for LibreHardwareMonitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LhmNamespace {
    /// LibreHardwareMonitor namespace (newer versions)
    Libre,
    /// OpenHardwareMonitor namespace (older versions)
    Open,
}

/// LibreHardwareMonitor WMI temperature source.
///
/// The namespace connection itself is owned and reused by the WMI worker
/// (`windows_wmi_worker`); this source caches only which namespace exists.
pub struct LibreHardwareMonitorSource {
    /// Which namespace is available (None if not available, never checked yet, or error)
    available_namespace: OnceCell<Option<LhmNamespace>>,
}

impl Default for LibreHardwareMonitorSource {
    fn default() -> Self {
        Self::new()
    }
}

impl LibreHardwareMonitorSource {
    /// Create a new LibreHardwareMonitor source.
    pub fn new() -> Self {
        Self {
            available_namespace: OnceCell::new(),
        }
    }

    /// Get the available namespace, checking both LibreHardwareMonitor and OpenHardwareMonitor.
    fn get_available_namespace(&self) -> Option<LhmNamespace> {
        *self.available_namespace.get_or_init(|| {
            if WMIConnection::with_namespace_path(LIBRE_HARDWARE_MONITOR).is_ok() {
                Some(LhmNamespace::Libre)
            } else if WMIConnection::with_namespace_path(OPEN_HARDWARE_MONITOR).is_ok() {
                Some(LhmNamespace::Open)
            } else {
                None
            }
        })
    }

    /// WMI namespace path to query, or `None` when neither is registered.
    pub fn namespace_path(&self) -> Option<&'static str> {
        match self.get_available_namespace()? {
            LhmNamespace::Libre => Some(LIBRE_HARDWARE_MONITOR),
            LhmNamespace::Open => Some(OPEN_HARDWARE_MONITOR),
        }
    }

    /// Get temperature from LibreHardwareMonitor WMI over the cached
    /// connection to [`Self::namespace_path`] (`None` when it could not be
    /// opened).
    ///
    /// # Returns
    /// * `TemperatureResult::Success(temp)` - Temperature in Celsius
    /// * `TemperatureResult::NotFound` - LibreHardwareMonitor WMI namespace not available
    /// * `TemperatureResult::Error` - Transient error during query
    /// * `TemperatureResult::NoValidReading` - Query succeeded but returned invalid data
    pub fn get_temperature(&self, connection: Option<&WMIConnection>) -> TemperatureResult {
        // Check if any namespace is available (cached)
        if self.get_available_namespace().is_none() {
            return TemperatureResult::NotFound;
        }

        let Some(connection) = connection else {
            return TemperatureResult::Error;
        };

        // Query for CPU temperature sensors
        let query =
            "SELECT Name, SensorType, Value, Parent FROM Sensor WHERE SensorType='Temperature'";

        let results: Result<Vec<LhmSensor>, _> = connection.raw_query(query);

        match results {
            Ok(sensors) => {
                if sensors.is_empty() {
                    // LibreHardwareMonitor is running but no temperature sensors found
                    return TemperatureResult::NoValidReading;
                }

                // Priority order for CPU temperature sensors:
                // 1. "CPU Package" - Package temperature (most accurate overall CPU temp)
                // 2. "CPU CCD" - Chiplet temperature (AMD)
                // 3. "CPU Core #0" or similar - Individual core temperature
                // 4. Any CPU-related temperature

                let priority_names = ["CPU Package", "CPU CCD", "CPU Core"];

                for priority in priority_names {
                    for sensor in &sensors {
                        if let (Some(name), Some(value)) = (&sensor.name, sensor.value) {
                            // Check if it's a CPU sensor
                            let is_cpu = sensor
                                .parent
                                .as_ref()
                                .map(|p| p.to_lowercase().contains("cpu"))
                                .unwrap_or(false)
                                || name.to_lowercase().contains("cpu");

                            if is_cpu && name.contains(priority) {
                                let temp = value as f64;
                                if temp > 0.0 && temp < 150.0 {
                                    // Use round() for more accurate conversion
                                    return TemperatureResult::Success(temp.round() as u32);
                                }
                            }
                        }
                    }
                }

                // Fallback: any CPU temperature sensor
                for sensor in &sensors {
                    if let (Some(name), Some(value)) = (&sensor.name, sensor.value) {
                        let is_cpu = sensor
                            .parent
                            .as_ref()
                            .map(|p| p.to_lowercase().contains("cpu"))
                            .unwrap_or(false)
                            || name.to_lowercase().contains("cpu");

                        if is_cpu {
                            let temp = value as f64;
                            if temp > 0.0 && temp < 150.0 {
                                // Use round() for more accurate conversion
                                return TemperatureResult::Success(temp.round() as u32);
                            }
                        }
                    }
                }

                TemperatureResult::NoValidReading
            }
            Err(e) => {
                if is_wmi_not_found_error(&e) {
                    TemperatureResult::NotFound
                } else {
                    // LibreHardwareMonitor might not be running
                    // This could be transient (user might start it later)
                    TemperatureResult::Error
                }
            }
        }
    }
}
