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

//! Generic chassis reader for non-Apple Silicon platforms
//!
//! This reader collects chassis-level metrics from:
//! - DMI data (`/sys/class/dmi/id/`) for system identification
//! - Thermal zones (`/sys/class/thermal/`) for board temperatures
//! - Cached GPU power for total power consumption

use crate::device::{ChassisInfo, ChassisReader};
use crate::utils::get_hostname;
use chrono::Local;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Generic chassis reader that collects DMI info, thermal zones, and GPU power
#[allow(dead_code)]
pub struct GenericChassisReader {
    hostname: String,
    /// Cached total GPU power (updated externally)
    cached_gpu_power: Arc<RwLock<Option<f64>>>,
    /// Static DMI detail cached at construction (never changes at runtime)
    dmi_detail: HashMap<String, String>,
}

impl Default for GenericChassisReader {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
impl GenericChassisReader {
    pub fn new() -> Self {
        let mut dmi_detail = HashMap::new();
        #[cfg(target_os = "linux")]
        collect_dmi_info(&mut dmi_detail);
        Self {
            hostname: get_hostname(),
            cached_gpu_power: Arc::new(RwLock::new(None)),
            dmi_detail,
        }
    }

    /// Update the cached GPU power value
    /// This should be called from the data collection loop with aggregated GPU power
    pub fn update_gpu_power(&self, total_gpu_power_watts: f64) {
        if let Ok(mut power) = self.cached_gpu_power.write() {
            *power = Some(total_gpu_power_watts);
        }
    }

    /// Get the cached GPU power value
    fn get_cached_gpu_power(&self) -> Option<f64> {
        self.cached_gpu_power.read().ok().and_then(|p| *p)
    }
}

/// Read a single DMI field from `/sys/class/dmi/id/`.
/// Returns `None` if the file doesn't exist or is unreadable (e.g., permission denied).
fn read_dmi_field(field: &str) -> Option<String> {
    let path = format!("/sys/class/dmi/id/{field}");
    std::fs::read_to_string(&path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Collect DMI information into the detail map.
fn collect_dmi_info(detail: &mut HashMap<String, String>) {
    if let Some(v) = read_dmi_field("product_name") {
        detail.insert("Product Name".to_string(), v);
    }
    if let Some(v) = read_dmi_field("sys_vendor") {
        detail.insert("Vendor".to_string(), v);
    }
    if let Some(v) = read_dmi_field("board_name") {
        detail.insert("Board".to_string(), v);
    }
    if let Some(v) = read_dmi_field("product_version") {
        detail.insert("Version".to_string(), v);
    }
    if let Some(v) = read_dmi_field("bios_version") {
        detail.insert("BIOS Version".to_string(), v);
    }
}

/// Read thermal zones from `/sys/class/thermal/` and return (inlet, outlet) temperatures.
///
/// On systems like DGX Spark, ACPI thermal zones provide board-level temperatures.
/// We use the minimum temperature as "inlet" and maximum as "outlet" — a reasonable
/// approximation when specific zone roles aren't labeled.
fn read_thermal_zones() -> (Option<f64>, Option<f64>) {
    read_thermal_zones_from("/sys/class/thermal")
}

/// Testable version that accepts a base path.
fn read_thermal_zones_from(base_path: &str) -> (Option<f64>, Option<f64>) {
    let entries = match std::fs::read_dir(base_path) {
        Ok(e) => e,
        Err(_) => return (None, None),
    };

    let mut temps: Vec<f64> = Vec::new();

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !name_str.starts_with("thermal_zone") {
            continue;
        }

        let zone_path = entry.path();

        // Only use ACPI thermal zones (most reliable for board-level temps)
        let type_path = zone_path.join("type");
        if let Ok(zone_type) = std::fs::read_to_string(&type_path) {
            let zone_type = zone_type.trim();
            if zone_type != "acpitz" {
                continue;
            }
        }

        let temp_path = zone_path.join("temp");
        if let Ok(temp_str) = std::fs::read_to_string(&temp_path)
            && let Ok(millidegrees) = temp_str.trim().parse::<i64>()
        {
            // Kernel reports in millidegrees Celsius
            let celsius = millidegrees as f64 / 1000.0;
            // Sanity check: ignore obviously wrong readings
            if celsius > -40.0 && celsius < 150.0 {
                temps.push(celsius);
            }
        }
    }

    if temps.is_empty() {
        return (None, None);
    }

    let min = temps.iter().cloned().reduce(f64::min);
    let max = temps.iter().cloned().reduce(f64::max);
    (min, max)
}

impl ChassisReader for GenericChassisReader {
    fn get_chassis_info(&self) -> Option<ChassisInfo> {
        // Start with cached DMI detail (read once at construction)
        let mut detail = self.dmi_detail.clone();

        // Platform identifier
        #[cfg(target_os = "linux")]
        detail.insert("platform".to_string(), "Linux".to_string());
        #[cfg(target_os = "windows")]
        detail.insert("platform".to_string(), "Windows".to_string());

        // Read thermal zones (Linux only)
        #[cfg(target_os = "linux")]
        let (inlet_temperature, outlet_temperature) = read_thermal_zones();
        #[cfg(not(target_os = "linux"))]
        let (inlet_temperature, outlet_temperature) = (None, None);

        // Get total power from cached GPU power
        let total_power_watts = self.get_cached_gpu_power();

        let hostname = self.hostname.clone();
        Some(ChassisInfo {
            host_id: hostname.clone(),
            hostname: hostname.clone(),
            instance: hostname,
            total_power_watts,
            inlet_temperature,
            outlet_temperature,
            thermal_pressure: None, // Not applicable for non-Apple platforms
            fan_speeds: Vec::new(),
            psu_status: Vec::new(),
            detail,
            time: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generic_chassis_reader_creation() {
        let reader = GenericChassisReader::new();
        assert!(!reader.hostname.is_empty());
    }

    #[test]
    fn test_update_gpu_power() {
        let reader = GenericChassisReader::new();
        reader.update_gpu_power(350.5);

        let chassis_info = reader.get_chassis_info();
        assert!(chassis_info.is_some());

        let info = chassis_info.unwrap();
        assert_eq!(info.total_power_watts, Some(350.5));
    }

    #[test]
    fn test_chassis_info_without_gpu_power() {
        let reader = GenericChassisReader::new();
        let chassis_info = reader.get_chassis_info();

        assert!(chassis_info.is_some());
        let info = chassis_info.unwrap();
        assert!(info.total_power_watts.is_none());
        assert!(!info.hostname.is_empty());
    }

    #[test]
    fn test_read_dmi_field_nonexistent() {
        // A non-existent DMI field should return None
        assert!(read_dmi_field("nonexistent_field_xyz").is_none());
    }

    #[test]
    fn test_read_thermal_zones_from_nonexistent_path() {
        let (inlet, outlet) = read_thermal_zones_from("/nonexistent/thermal/path");
        assert!(inlet.is_none());
        assert!(outlet.is_none());
    }

    #[test]
    fn test_read_thermal_zones_from_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let (inlet, outlet) = read_thermal_zones_from(dir.path().to_str().unwrap());
        assert!(inlet.is_none());
        assert!(outlet.is_none());
    }

    #[test]
    fn test_read_thermal_zones_from_mock_zones() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        // Create two mock thermal zones
        for (i, temp) in [(0, 39700), (1, 42300)] {
            let zone = base.join(format!("thermal_zone{i}"));
            std::fs::create_dir_all(&zone).unwrap();
            std::fs::write(zone.join("type"), "acpitz\n").unwrap();
            std::fs::write(zone.join("temp"), format!("{temp}\n")).unwrap();
        }

        // Create a non-ACPI zone that should be ignored
        let zone_other = base.join("thermal_zone2");
        std::fs::create_dir_all(&zone_other).unwrap();
        std::fs::write(zone_other.join("type"), "x86_pkg_temp\n").unwrap();
        std::fs::write(zone_other.join("temp"), "99000\n").unwrap();

        let (inlet, outlet) = read_thermal_zones_from(base.to_str().unwrap());
        assert!((inlet.unwrap() - 39.7).abs() < 0.01);
        assert!((outlet.unwrap() - 42.3).abs() < 0.01);
    }

    #[test]
    fn test_read_thermal_zones_from_single_zone() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        let zone = base.join("thermal_zone0");
        std::fs::create_dir_all(&zone).unwrap();
        std::fs::write(zone.join("type"), "acpitz\n").unwrap();
        std::fs::write(zone.join("temp"), "40500\n").unwrap();

        let (inlet, outlet) = read_thermal_zones_from(base.to_str().unwrap());
        // With a single zone, inlet == outlet
        assert!((inlet.unwrap() - 40.5).abs() < 0.01);
        assert!((outlet.unwrap() - 40.5).abs() < 0.01);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_chassis_info_has_dmi_on_linux() {
        let reader = GenericChassisReader::new();
        let info = reader.get_chassis_info().unwrap();
        // On a real Linux system, at least one DMI field should be present
        // (product_name is almost always available)
        let has_any_dmi = info.detail.contains_key("Product Name")
            || info.detail.contains_key("Vendor")
            || info.detail.contains_key("Board");
        assert!(
            has_any_dmi,
            "Expected at least one DMI field on Linux, got detail: {:?}",
            info.detail
        );
    }
}
