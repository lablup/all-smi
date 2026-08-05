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

//! Intel Mac chassis reader using the SMC and NSProcessInfo
//!
//! Provides chassis-level metrics for Intel Macs without sudo:
//! - Approximate total system power from the SMC `PSTR` key
//! - CPU package power from the SMC power key family, where the model has it
//! - Fan speeds from the SMC `FNum` / `F<i>Ac` / `F<i>Mx` keys
//! - Thermal pressure from NSProcessInfo.thermalState
//!
//! Deliberately does not use the native metrics manager: that is built on
//! IOReport's "Energy Model" channel group, which only exists on Apple
//! Silicon. Everything here is architecture-agnostic macOS API surface.
//!
//! `powermetrics` would give metered rather than approximate power, but it
//! requires sudo, which this project does not ask for on macOS.

use crate::device::macos_native::get_thermal_state;
use crate::device::macos_native::smc::SmcConnection;
use crate::device::{ChassisInfo, ChassisReader, FanInfo};
use crate::utils::get_hostname;
use chrono::Local;
use std::collections::HashMap;
use std::sync::Mutex;

/// Chassis reader for Intel Macs using the SMC (no sudo required)
pub struct IntelMacChassisReader {
    hostname: String,
    /// Opened on first poll and reused. The reader runs on the UI refresh
    /// interval, so reconnecting per cycle would mean an IOKit service lookup
    /// and connection open every second.
    smc: Mutex<SmcConnection>,
}

impl Default for IntelMacChassisReader {
    fn default() -> Self {
        Self::new()
    }
}

impl IntelMacChassisReader {
    pub fn new() -> Self {
        Self {
            hostname: get_hostname(),
            smc: Mutex::new(SmcConnection::default()),
        }
    }
}

/// What a single SMC poll produced. Every field is optional because the key
/// set varies across Intel Mac generations.
#[derive(Debug, Default)]
struct SmcSnapshot {
    total_power_watts: Option<f64>,
    cpu_power_watts: Option<f64>,
    fans: Vec<FanInfo>,
}

impl ChassisReader for IntelMacChassisReader {
    fn get_chassis_info(&self) -> Option<ChassisInfo> {
        let snapshot = self.read_smc();

        // Thermal pressure comes from NSProcessInfo, which works on every Mac
        // from macOS 10.10.3 onward and needs no SMC connection. It is
        // therefore the one field that is always present, and it is what makes
        // a chassis block worth rendering even when the SMC is unreachable.
        let thermal_pressure = get_thermal_state().as_str().to_string();

        let mut detail = HashMap::new();
        detail.insert("platform".to_string(), "Intel Mac".to_string());
        detail.insert("api".to_string(), "Native (SMC)".to_string());

        if let Some(cpu_power) = snapshot.cpu_power_watts {
            detail.insert("cpu_power_watts".to_string(), format!("{cpu_power:.2}"));
        }

        if snapshot.total_power_watts.is_some() {
            // The SMC's own estimate of whole-machine draw. It is not metered
            // and its accuracy is model-dependent, so say so in the payload
            // rather than letting consumers assume it is measured.
            detail.insert(
                "power_source".to_string(),
                "SMC PSTR (approximate)".to_string(),
            );
        }

        let hostname = self.hostname.clone();
        Some(ChassisInfo {
            host_id: hostname.clone(),
            hostname: hostname.clone(),
            instance: hostname,
            total_power_watts: snapshot.total_power_watts,
            inlet_temperature: None,  // No BMC on a Mac
            outlet_temperature: None, // No BMC on a Mac
            thermal_pressure: Some(thermal_pressure),
            fan_speeds: snapshot.fans,
            psu_status: Vec::new(), // Not applicable for laptops/desktops
            detail,
            time: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        })
    }
}

impl IntelMacChassisReader {
    /// Read power and fan data over the cached SMC connection.
    ///
    /// Returns an empty snapshot when the SMC is unreachable, so a chassis
    /// block with thermal pressure alone is still produced.
    fn read_smc(&self) -> SmcSnapshot {
        let Ok(mut guard) = self.smc.lock() else {
            return SmcSnapshot::default();
        };
        let Some(smc) = guard.get() else {
            return SmcSnapshot::default();
        };

        SmcSnapshot {
            total_power_watts: smc.get_system_power(),
            cpu_power_watts: smc.get_cpu_package_power(),
            fans: smc
                .get_fan_readings()
                .into_iter()
                .map(|fan| FanInfo {
                    id: fan.index,
                    name: fan.name(),
                    speed_rpm: fan.actual_rpm,
                    max_rpm: fan.max_rpm,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_intel_mac_chassis_reader_creation() {
        let reader = IntelMacChassisReader::new();
        assert!(!reader.hostname.is_empty());
    }

    /// The reader must always produce a chassis block: thermal pressure needs
    /// no SMC, so an unreachable SMC degrades the payload instead of removing
    /// it. This also exercises the code path on Apple Silicon CI hosts, where
    /// the reader is never selected but must still not panic.
    #[test]
    fn always_reports_thermal_pressure_even_without_smc_power() {
        let reader = IntelMacChassisReader::new();
        let info = reader
            .get_chassis_info()
            .expect("Intel Mac chassis info is always produced");

        assert!(info.thermal_pressure.is_some());
        assert_eq!(
            info.detail.get("platform").map(String::as_str),
            Some("Intel Mac")
        );
        assert!(info.inlet_temperature.is_none());
        assert!(info.psu_status.is_empty());
    }

    /// `power_source` documents that total power is an SMC estimate. It must
    /// only appear alongside an actual reading.
    #[test]
    fn power_source_detail_tracks_the_power_reading() {
        let reader = IntelMacChassisReader::new();
        let info = reader.get_chassis_info().expect("chassis info");

        assert_eq!(
            info.total_power_watts.is_some(),
            info.detail.contains_key("power_source")
        );
    }
}
