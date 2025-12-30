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

//! Windows CPU temperature fallback chain implementation.
//!
//! This module implements a cascading fallback mechanism to try multiple
//! temperature sources on Windows, handling the case where the standard
//! MSAcpi_ThermalZoneTemperature WMI class is not available (WBEM_E_NOT_FOUND).
//!
//! ## Fallback Order:
//! 1. MSAcpi_ThermalZoneTemperature (ACPI thermal zones)
//! 2. AMD Ryzen Master SDK (AMD CPUs only - if DLL present)
//! 3. Intel WMI (Intel CPUs only - root/Intel namespace)
//! 4. LibreHardwareMonitor WMI (any CPU - if app running)
//! 5. None (graceful fallback with no error spam)

mod acpi_thermal;
mod amd_ryzen;
mod intel_wmi;
mod libre_hwmon;

pub use acpi_thermal::AcpiThermalSource;
pub use amd_ryzen::AmdRyzenSource;
pub use intel_wmi::IntelWmiSource;
pub use libre_hwmon::LibreHardwareMonitorSource;

use once_cell::sync::OnceCell;
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use wmi::WMIConnection;

/// Helper to get read lock, recovering from poisoned state.
/// This prevents the application from panicking if another thread panicked while holding the lock.
fn read_lock<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Helper to get write lock, recovering from poisoned state.
fn write_lock<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// HRESULT error code for WBEM_E_NOT_FOUND (0x8004100C)
/// This error indicates the WMI class doesn't exist in the namespace.
#[allow(dead_code)]
const WBEM_E_NOT_FOUND: i32 = -0x7FFBEFEC_i32; // 0x8004100C as signed

/// Represents the detected CPU vendor for selecting appropriate temperature sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuVendor {
    Intel,
    Amd,
    Unknown,
}

/// Cached CPU vendor, determined once at startup.
static CPU_VENDOR: OnceCell<CpuVendor> = OnceCell::new();

/// Get the cached CPU vendor.
pub fn get_cpu_vendor() -> CpuVendor {
    *CPU_VENDOR.get_or_init(detect_cpu_vendor)
}

/// Detect the CPU vendor from the processor brand string.
fn detect_cpu_vendor() -> CpuVendor {
    use sysinfo::{CpuRefreshKind, System};

    let mut system = System::new();
    system.refresh_cpu_specifics(CpuRefreshKind::everything());

    let cpus = system.cpus();
    if cpus.is_empty() {
        return CpuVendor::Unknown;
    }

    let brand = cpus[0].brand().to_lowercase();
    if brand.contains("intel") {
        CpuVendor::Intel
    } else if brand.contains("amd") {
        CpuVendor::Amd
    } else {
        CpuVendor::Unknown
    }
}

/// Availability status of a temperature source.
/// Once we determine a source is unavailable, we cache that to avoid repeated checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceAvailability {
    /// Not yet checked
    Unknown,
    /// Available and working
    Available,
    /// Permanently unavailable (e.g., WMI class not found)
    Unavailable,
}

/// Manages the temperature fallback chain and caches availability status.
pub struct TemperatureManager {
    /// ACPI thermal zone availability
    acpi_available: RwLock<SourceAvailability>,
    /// AMD Ryzen Master SDK availability
    amd_available: RwLock<SourceAvailability>,
    /// Intel WMI availability
    intel_available: RwLock<SourceAvailability>,
    /// LibreHardwareMonitor WMI availability
    libre_available: RwLock<SourceAvailability>,
    /// ACPI thermal zone source
    acpi_source: AcpiThermalSource,
    /// AMD Ryzen Master SDK source
    amd_source: AmdRyzenSource,
    /// Intel WMI source
    intel_source: IntelWmiSource,
    /// LibreHardwareMonitor source
    libre_source: LibreHardwareMonitorSource,
}

impl Default for TemperatureManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TemperatureManager {
    /// Create a new temperature manager.
    pub fn new() -> Self {
        Self {
            acpi_available: RwLock::new(SourceAvailability::Unknown),
            amd_available: RwLock::new(SourceAvailability::Unknown),
            intel_available: RwLock::new(SourceAvailability::Unknown),
            libre_available: RwLock::new(SourceAvailability::Unknown),
            acpi_source: AcpiThermalSource::new(),
            amd_source: AmdRyzenSource::new(),
            intel_source: IntelWmiSource::new(),
            libre_source: LibreHardwareMonitorSource::new(),
        }
    }

    /// Get the CPU temperature using the fallback chain.
    ///
    /// Tries each temperature source in order until one succeeds.
    /// Caches the availability status to avoid repeated failed queries.
    pub fn get_temperature(&self, root_wmi_conn: Option<&WMIConnection>) -> Option<u32> {
        // 1. Try ACPI thermal zones (current method)
        if self.is_source_potentially_available(&self.acpi_available) {
            match self.acpi_source.get_temperature(root_wmi_conn) {
                TemperatureResult::Success(temp) => {
                    self.mark_available(&self.acpi_available);
                    return Some(temp);
                }
                TemperatureResult::NotFound => {
                    self.mark_unavailable(&self.acpi_available);
                }
                TemperatureResult::Error => {
                    // Transient error, don't mark as unavailable
                }
                TemperatureResult::NoValidReading => {
                    // No valid reading but source exists
                }
            }
        }

        let vendor = get_cpu_vendor();

        // 2. Try AMD Ryzen Master SDK (AMD only)
        if vendor == CpuVendor::Amd && self.is_source_potentially_available(&self.amd_available) {
            match self.amd_source.get_temperature() {
                TemperatureResult::Success(temp) => {
                    self.mark_available(&self.amd_available);
                    return Some(temp);
                }
                TemperatureResult::NotFound => {
                    self.mark_unavailable(&self.amd_available);
                }
                TemperatureResult::Error => {
                    // Transient error
                }
                TemperatureResult::NoValidReading => {}
            }
        }

        // 3. Try Intel WMI (Intel only)
        if vendor == CpuVendor::Intel && self.is_source_potentially_available(&self.intel_available)
        {
            match self.intel_source.get_temperature() {
                TemperatureResult::Success(temp) => {
                    self.mark_available(&self.intel_available);
                    return Some(temp);
                }
                TemperatureResult::NotFound => {
                    self.mark_unavailable(&self.intel_available);
                }
                TemperatureResult::Error => {}
                TemperatureResult::NoValidReading => {}
            }
        }

        // 4. Try LibreHardwareMonitor WMI (any CPU)
        if self.is_source_potentially_available(&self.libre_available) {
            match self.libre_source.get_temperature() {
                TemperatureResult::Success(temp) => {
                    self.mark_available(&self.libre_available);
                    return Some(temp);
                }
                TemperatureResult::NotFound => {
                    self.mark_unavailable(&self.libre_available);
                }
                TemperatureResult::Error => {}
                TemperatureResult::NoValidReading => {}
            }
        }

        // All sources failed - return None gracefully (no error spam)
        None
    }

    /// Check if a source is potentially available (not marked as permanently unavailable).
    fn is_source_potentially_available(&self, status: &RwLock<SourceAvailability>) -> bool {
        !matches!(*read_lock(status), SourceAvailability::Unavailable)
    }

    /// Mark a source as available.
    fn mark_available(&self, status: &RwLock<SourceAvailability>) {
        let mut guard = write_lock(status);
        if *guard == SourceAvailability::Unknown {
            *guard = SourceAvailability::Available;
        }
    }

    /// Mark a source as permanently unavailable.
    fn mark_unavailable(&self, status: &RwLock<SourceAvailability>) {
        *write_lock(status) = SourceAvailability::Unavailable;
    }
}

/// Result of attempting to read temperature from a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemperatureResult {
    /// Successfully read temperature (in Celsius)
    Success(u32),
    /// Source permanently not found (e.g., WMI class doesn't exist)
    NotFound,
    /// Transient error (connection issue, etc.)
    Error,
    /// Source exists but returned no valid reading
    NoValidReading,
}

/// Check if a WMI error is WBEM_E_NOT_FOUND.
pub fn is_wmi_not_found_error(error: &wmi::WMIError) -> bool {
    let error_str = format!("{error}");
    // Check for HRESULT error code 0x8004100C
    error_str.contains("0x8004100C")
        || error_str.contains("WBEM_E_NOT_FOUND")
        || error_str.contains(&format!("{WBEM_E_NOT_FOUND}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cpu_vendor_detection() {
        let vendor = detect_cpu_vendor();
        // Should return a valid vendor (actual value depends on test machine)
        assert!(matches!(
            vendor,
            CpuVendor::Intel | CpuVendor::Amd | CpuVendor::Unknown
        ));
    }

    #[test]
    fn test_temperature_manager_creation() {
        let manager = TemperatureManager::new();
        assert!(manager.is_source_potentially_available(&manager.acpi_available));
    }
}
