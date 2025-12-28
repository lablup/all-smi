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

use super::{is_wmi_not_found_error, TemperatureResult};
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
/// Note: WMIConnection is not Send + Sync, so we cannot cache it.
/// Instead, we cache only which namespace is available (if any),
/// and create a new connection on each temperature query.
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
            if WMIConnection::with_namespace_path("root\\LibreHardwareMonitor").is_ok() {
                Some(LhmNamespace::Libre)
            } else if WMIConnection::with_namespace_path("root\\OpenHardwareMonitor").is_ok() {
                Some(LhmNamespace::Open)
            } else {
                None
            }
        })
    }

    /// Create a new WMI connection to the appropriate namespace.
    fn create_connection(&self) -> Option<WMIConnection> {
        match self.get_available_namespace()? {
            LhmNamespace::Libre => {
                WMIConnection::with_namespace_path("root\\LibreHardwareMonitor").ok()
            }
            LhmNamespace::Open => {
                WMIConnection::with_namespace_path("root\\OpenHardwareMonitor").ok()
            }
        }
    }

    /// Get temperature from LibreHardwareMonitor WMI.
    ///
    /// # Returns
    /// * `TemperatureResult::Success(temp)` - Temperature in Celsius
    /// * `TemperatureResult::NotFound` - LibreHardwareMonitor WMI namespace not available
    /// * `TemperatureResult::Error` - Transient error during query
    /// * `TemperatureResult::NoValidReading` - Query succeeded but returned invalid data
    pub fn get_temperature(&self) -> TemperatureResult {
        // Check if any namespace is available (cached)
        if self.get_available_namespace().is_none() {
            return TemperatureResult::NotFound;
        }

        // Create a new connection for this query
        let connection = match self.create_connection() {
            Some(conn) => conn,
            None => return TemperatureResult::Error,
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
