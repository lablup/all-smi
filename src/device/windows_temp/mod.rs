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
//! 5. Win32_PerfFormattedData_Counters_ThermalZoneInformation (the ACPI
//!    zones again, readable without elevation; last so it never pre-empts
//!    the CPU package sensors above, issue #459)
//! 6. None (graceful fallback with no error spam)

mod acpi_thermal;
mod amd_ryzen;
mod intel_wmi;
mod libre_hwmon;
mod thermal_zone_info;

pub use acpi_thermal::AcpiThermalSource;
pub use amd_ryzen::AmdRyzenSource;
pub use intel_wmi::IntelWmiSource;
pub use libre_hwmon::LibreHardwareMonitorSource;
pub use thermal_zone_info::ThermalZoneInfoSource;

pub use crate::device::windows_temp_chain::{CpuVendor, TemperatureResult};

use crate::device::windows_temp_chain::{ChainState, TempSource, source_order};
use crate::device::windows_wmi_worker::{CIMV2, INTEL, ROOT_WMI, WmiConnections};
use once_cell::sync::OnceCell;
use std::sync::Mutex;
use std::time::Instant;

/// HRESULT error code for WBEM_E_NOT_FOUND (0x8004100C)
/// This error indicates the WMI class doesn't exist in the namespace.
#[allow(dead_code)]
const WBEM_E_NOT_FOUND: i32 = -0x7FFBEFEC_i32; // 0x8004100C as signed

/// Cached CPU vendor, determined once at startup.
static CPU_VENDOR: OnceCell<CpuVendor> = OnceCell::new();

/// Get the cached CPU vendor.
pub fn get_cpu_vendor() -> CpuVendor {
    *CPU_VENDOR.get_or_init(detect_cpu_vendor)
}

/// Detect the CPU vendor from the processor brand string.
fn detect_cpu_vendor() -> CpuVendor {
    use sysinfo::CpuRefreshKind;

    let mut system = crate::utils::system::new_system();
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

/// Runs the temperature fallback chain over the WMI worker's connections.
///
/// Availability and backoff live in one [`ChainState`]: `NotFound` drops a
/// source for good, repeated `Error`/`NoValidReading` backs it off
/// (`windows_temp_chain`).
pub struct TemperatureManager {
    chain: Mutex<ChainState>,
    /// ACPI thermal zone source
    acpi_source: AcpiThermalSource,
    /// AMD Ryzen Master SDK source
    amd_source: AmdRyzenSource,
    /// Intel WMI source
    intel_source: IntelWmiSource,
    /// LibreHardwareMonitor source
    libre_source: LibreHardwareMonitorSource,
    /// ThermalZoneInformation performance-counter source
    thermal_zone_info_source: ThermalZoneInfoSource,
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
            chain: Mutex::new(ChainState::new()),
            acpi_source: AcpiThermalSource::new(),
            amd_source: AmdRyzenSource::new(),
            intel_source: IntelWmiSource::new(),
            libre_source: LibreHardwareMonitorSource::new(),
            thermal_zone_info_source: ThermalZoneInfoSource::new(),
        }
    }

    /// Get the CPU temperature using the fallback chain, once.
    ///
    /// Must run on the WMI worker (`with_wmi`), which owns `connections`.
    /// Each source takes its own namespace connection, so one namespace that
    /// cannot be connected no longer hides the sources behind it.
    pub fn get_temperature(&self, connections: &mut WmiConnections) -> Option<u32> {
        self.run(Instant::now(), |source| self.query(source, connections))
    }

    /// One pass of the chain in priority order, with `query` reading a source.
    fn run(&self, now: Instant, query: impl FnMut(TempSource) -> TemperatureResult) -> Option<u32> {
        let mut chain = self.chain.lock().unwrap_or_else(|e| e.into_inner());
        chain.run_chain(&source_order(get_cpu_vendor()), now, query)
    }

    /// Query one source over its cached connection.
    fn query(&self, source: TempSource, connections: &mut WmiConnections) -> TemperatureResult {
        match source {
            TempSource::Acpi => query_over(connections, ROOT_WMI, |conn| {
                self.acpi_source.get_temperature(conn)
            }),
            TempSource::AmdRyzen => self.amd_source.get_temperature(),
            TempSource::IntelWmi => query_over(connections, INTEL, |conn| {
                self.intel_source.get_temperature(conn)
            }),
            TempSource::LibreHardwareMonitor => match self.libre_source.namespace_path() {
                Some(namespace) => query_over(connections, namespace, |conn| {
                    self.libre_source.get_temperature(conn)
                }),
                None => TemperatureResult::NotFound,
            },
            TempSource::ThermalZoneInfo => query_over(connections, CIMV2, |conn| {
                self.thermal_zone_info_source.get_temperature(conn)
            }),
        }
    }
}

/// Run a WMI-backed source over the cached `namespace` connection, dropping
/// that connection when the query fails with a transient error so the next
/// attempt reconnects (the provider may have restarted).
fn query_over(
    connections: &mut WmiConnections,
    namespace: &'static str,
    read: impl FnOnce(Option<&wmi::WMIConnection>) -> TemperatureResult,
) -> TemperatureResult {
    let result = read(connections.get(namespace));
    if result == TemperatureResult::Error {
        connections.invalidate(namespace);
    }
    result
}

/// Check if a WMI error is WBEM_E_NOT_FOUND.
pub fn is_wmi_not_found_error(error: &wmi::WMIError) -> bool {
    let error_str = format!("{error}");
    // Check for HRESULT error code 0x8004100C
    error_str.contains("0x8004100C")
        || error_str.contains("WBEM_E_NOT_FOUND")
        || error_str.contains(&format!("{WBEM_E_NOT_FOUND}"))
}

/// `WBEM_E_ACCESS_DENIED` (0x80041003).
const WBEM_E_ACCESS_DENIED: i32 = 0x8004_1003_u32 as i32;
/// `E_ACCESSDENIED` (0x80070005).
const E_ACCESSDENIED: i32 = 0x8007_0005_u32 as i32;

/// Check if a WMI error is an access-denied failure.
///
/// The process token does not gain rights while all-smi runs, so a source
/// that is denied once stays denied. Treating it as permanently unavailable
/// spares every later poll a failing WMI round trip (the RTX Spark N1X
/// denies `root\WMI:MSAcpi_ThermalZoneTemperature` to a normal user,
/// issue #459).
pub fn is_wmi_access_denied_error(error: &wmi::WMIError) -> bool {
    matches!(
        error,
        wmi::WMIError::HResultError { hres }
            if *hres == WBEM_E_ACCESS_DENIED || *hres == E_ACCESSDENIED
    )
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
    fn access_denied_errors_are_recognised() {
        for hres in [WBEM_E_ACCESS_DENIED, E_ACCESSDENIED] {
            assert!(is_wmi_access_denied_error(&wmi::WMIError::HResultError {
                hres
            }));
        }
        assert!(!is_wmi_access_denied_error(&wmi::WMIError::HResultError {
            hres: WBEM_E_NOT_FOUND
        }));
        assert!(!is_wmi_access_denied_error(&wmi::WMIError::ResultEmpty));
    }

    /// The chain runs once per call: every source in the vendor order is
    /// queried exactly once when none answers (the old nested fallback ran
    /// most of them three times).
    #[test]
    fn manager_queries_each_source_once_per_call() {
        let manager = TemperatureManager::new();
        let mut queried = Vec::new();
        let reading = manager.run(Instant::now(), |source| {
            queried.push(source);
            TemperatureResult::NoValidReading
        });
        assert_eq!(reading, None);
        assert_eq!(queried, source_order(get_cpu_vendor()));
    }
}
