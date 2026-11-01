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

use crate::device::{CoreType, CoreUtilization, CpuInfo, CpuReader, CpuSocketInfo};
use crate::utils::system::get_hostname;
use chrono::Local;
use serde::Deserialize;
use std::sync::RwLock;
use sysinfo::{CpuRefreshKind, System};
use wmi::WMIConnection;

// Import the temperature fallback chain
use super::cpu_windows_arm::{classify_platform, core_types_from_identifiers};
use super::windows_temp::TemperatureManager;

/// Read `HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\<index>\Identifier`.
fn read_processor_identifier(index: usize) -> Option<String> {
    read_processor_value(index, "Identifier")
}

/// Read a string value under
/// `HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\<index>`.
fn read_processor_value(index: usize, name: &str) -> Option<String> {
    use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
    use windows::core::HSTRING;

    let subkey = HSTRING::from(format!(
        "HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\{index}"
    ));
    let value = HSTRING::from(name);
    let mut buffer = [0u16; 256];
    let mut size = std::mem::size_of_val(&buffer) as u32;
    // SAFETY: `buffer` is a writable UTF-16 buffer whose byte length is
    // passed in `size`; RegGetValueW writes at most that many bytes,
    // NUL-terminated for RRF_RT_REG_SZ, and updates `size` to the bytes
    // written. The buffer is only read after the call succeeds.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &subkey,
            &value,
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status.is_err() {
        return None;
    }
    let chars = (size as usize / 2).min(buffer.len());
    let text = String::from_utf16_lossy(&buffer[..chars]);
    let text = text.trim_end_matches('\0').trim().to_string();
    (!text.is_empty()).then_some(text)
}

// WMI structures for processor information
#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
struct Win32Processor {
    max_clock_speed: Option<u32>,
    l2_cache_size: Option<u32>,
    l3_cache_size: Option<u32>,
}

// Thread-local WMI connections for reuse within the same thread
thread_local! {
    static WMI_CIMV2_CONNECTION: std::cell::RefCell<Option<WMIConnection>> =
        const { std::cell::RefCell::new(None) };
    static WMI_ROOT_WMI_CONNECTION: std::cell::RefCell<Option<WMIConnection>> =
        const { std::cell::RefCell::new(None) };
}

/// Helper to get or create CIMV2 connection
fn with_cimv2_connection<T, F: FnOnce(&WMIConnection) -> T>(f: F) -> Option<T> {
    WMI_CIMV2_CONNECTION.with(|cell| {
        let mut conn_ref = cell.borrow_mut();
        // A failed connection is left as `None` on purpose: the caller
        // gets `None` back and degrades, and the next call retries.
        if conn_ref.is_none()
            && let Ok(wmi_con) = WMIConnection::new()
        {
            *conn_ref = Some(wmi_con);
        }
        conn_ref.as_ref().map(f)
    })
}

/// Helper to get or create root\WMI connection
fn with_root_wmi_connection<T, F: FnOnce(&WMIConnection) -> T>(f: F) -> Option<T> {
    WMI_ROOT_WMI_CONNECTION.with(|cell| {
        let mut conn_ref = cell.borrow_mut();
        // Same as above: a failed connection stays `None` and is retried.
        if conn_ref.is_none()
            && let Ok(wmi_con) = WMIConnection::with_namespace_path("root\\WMI")
        {
            *conn_ref = Some(wmi_con);
        }
        conn_ref.as_ref().map(f)
    })
}

pub struct WindowsCpuReader {
    system: RwLock<System>,
    first_refresh_done: RwLock<bool>,
    // Cached WMI data (static info)
    cached_max_frequency: RwLock<Option<u32>>,
    cached_cache_size: RwLock<Option<u32>>,
    cached_socket_count: RwLock<Option<u32>>,
    // Temperature manager with fallback chain
    temperature_manager: TemperatureManager,
    // Per-logical-processor registry `Identifier` strings, read once:
    // they name the ARM core design and so the P/E cluster (issue #459).
    processor_identifiers: Vec<Option<String>>,
    // Registry `ProcessorNameString` of processor 0, read once: the brand
    // fallback when sysinfo reports an empty brand (Windows on ARM).
    processor_name: Option<String>,
}

impl Default for WindowsCpuReader {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowsCpuReader {
    pub fn new() -> Self {
        let mut system = crate::utils::system::new_system();

        // Perform initial CPU refresh during construction to establish baseline.
        // This moves the 100ms blocking delay from the first get_cpu_info() call
        // to initialization time, preventing UI freezing during runtime queries.
        system.refresh_cpu_specifics(CpuRefreshKind::everything());
        std::thread::sleep(std::time::Duration::from_millis(100));
        system.refresh_cpu_specifics(CpuRefreshKind::everything());
        let processor_identifiers = (0..system.cpus().len())
            .map(read_processor_identifier)
            .collect();
        let processor_name = read_processor_value(0, "ProcessorNameString");

        Self {
            system: RwLock::new(system),
            first_refresh_done: RwLock::new(true), // Already initialized
            cached_max_frequency: RwLock::new(None),
            cached_cache_size: RwLock::new(None),
            cached_socket_count: RwLock::new(None),
            temperature_manager: TemperatureManager::new(),
            processor_identifiers,
            processor_name,
        }
    }

    /// Get CPU temperature using the fallback chain.
    ///
    /// Tries multiple temperature sources in order:
    /// 1. ACPI thermal zones (MSAcpi_ThermalZoneTemperature)
    /// 2. AMD Ryzen Master SDK (AMD CPUs only)
    /// 3. Intel WMI (Intel CPUs only)
    /// 4. LibreHardwareMonitor WMI (any CPU)
    /// 5. ThermalZoneInformation performance counters (any CPU, no elevation)
    /// 6. None (graceful fallback)
    fn get_cpu_temperature(&self) -> Option<u32> {
        // Get the root\WMI connection for ACPI thermal zones
        // The cimv2 connection serves the last fallback
        // (Win32_PerfFormattedData_Counters_ThermalZoneInformation).
        with_cimv2_connection(|cimv2| {
            with_root_wmi_connection(|wmi_conn| {
                self.temperature_manager
                    .get_temperature(Some(wmi_conn), Some(cimv2))
            })
            .flatten()
            // If root\WMI connection failed, still try other sources
            .or_else(|| self.temperature_manager.get_temperature(None, Some(cimv2)))
        })
        .flatten()
        .or_else(|| self.temperature_manager.get_temperature(None, None))
    }

    /// Get static CPU info from WMI (max frequency, cache size, socket count)
    /// Uses thread-local connection for efficiency
    fn get_wmi_processor_info(&self) -> (Option<u32>, Option<u32>, u32) {
        // Check cache first
        let cached_freq = *self
            .cached_max_frequency
            .read()
            .expect("cached_max_frequency lock poisoned");
        let cached_cache = *self
            .cached_cache_size
            .read()
            .expect("cached_cache_size lock poisoned");
        let cached_sockets = *self
            .cached_socket_count
            .read()
            .expect("cached_socket_count lock poisoned");

        if cached_freq.is_some() && cached_cache.is_some() && cached_sockets.is_some() {
            return (cached_freq, cached_cache, cached_sockets.unwrap_or(1));
        }

        // Query WMI for processor info using thread-local connection
        // Win32_Processor returns one instance per physical processor (socket)
        let result = with_cimv2_connection(|wmi_con| {
            let results: Result<Vec<Win32Processor>, _> = wmi_con
                .raw_query("SELECT MaxClockSpeed, L2CacheSize, L3CacheSize FROM Win32_Processor");

            if let Ok(procs) = results {
                // Number of Win32_Processor instances = number of physical processors (sockets)
                let socket_count = procs.len().max(1) as u32;

                if let Some(proc) = procs.first() {
                    let max_freq = proc.max_clock_speed.unwrap_or(0);
                    // Cache size in KB, convert to MB
                    let l2 = proc.l2_cache_size.unwrap_or(0);
                    let l3 = proc.l3_cache_size.unwrap_or(0);
                    let cache_mb = (l2 + l3) / 1024;

                    return Some((max_freq, cache_mb, socket_count));
                }
            }
            None
        })
        .flatten();

        if let Some((freq, cache, sockets)) = result {
            *self
                .cached_max_frequency
                .write()
                .expect("cached_max_frequency lock poisoned") = Some(freq);
            *self
                .cached_cache_size
                .write()
                .expect("cached_cache_size lock poisoned") = Some(cache);
            *self
                .cached_socket_count
                .write()
                .expect("cached_socket_count lock poisoned") = Some(sockets);
            (Some(freq), Some(cache), sockets)
        } else {
            // Default to 1 socket if WMI query fails
            (None, None, 1)
        }
    }

    fn get_cpu_info_from_system(&self) -> Result<CpuInfo, Box<dyn std::error::Error>> {
        // On first call, do two refreshes to establish baseline for delta calculation
        if !*self
            .first_refresh_done
            .read()
            .expect("first_refresh_done lock poisoned")
        {
            self.system
                .write()
                .expect("system lock poisoned")
                .refresh_cpu_specifics(CpuRefreshKind::everything());
            std::thread::sleep(std::time::Duration::from_millis(100));
            *self
                .first_refresh_done
                .write()
                .expect("first_refresh_done lock poisoned") = true;
        }

        // Regular refresh for current data
        self.system
            .write()
            .expect("system lock poisoned")
            .refresh_cpu_specifics(CpuRefreshKind::everything());

        let hostname = get_hostname();
        let instance = hostname.clone();
        let time = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

        let system = self.system.read().expect("system lock poisoned");

        // Get CPU information
        let cpus = system.cpus();
        let total_threads = cpus.len() as u32;

        // Get CPU model from the first CPU. sysinfo returns an empty brand
        // on Windows on ARM (the N1X, issue #459), where the registry's
        // ProcessorNameString still names the part.
        let cpu_model = match cpus.first().map(|cpu| cpu.brand().trim()) {
            Some(brand) if !brand.is_empty() => brand.to_string(),
            _ => self
                .processor_name
                .clone()
                .unwrap_or_else(|| "Unknown CPU".to_string()),
        };

        // Determine platform type from the CPU brand, the registry core
        // identifier and the build target (NVIDIA ARM SoCs name neither
        // "arm" nor a known vendor in the brand string, issue #459).
        let platform_type = classify_platform(
            &cpu_model,
            self.processor_identifiers
                .first()
                .and_then(|id| id.as_deref()),
            cfg!(target_arch = "aarch64"),
        );

        // Get architecture
        let architecture = if cfg!(target_arch = "x86_64") {
            "x86_64".to_string()
        } else if cfg!(target_arch = "x86") {
            "x86".to_string()
        } else if cfg!(target_arch = "aarch64") {
            "arm64".to_string()
        } else {
            std::env::consts::ARCH.to_string()
        };

        // Get physical core count using sysinfo (static method)
        let total_cores = System::physical_core_count().unwrap_or(total_threads as usize) as u32;

        // Get frequency from the first CPU (in MHz)
        let base_frequency = if !cpus.is_empty() {
            cpus[0].frequency() as u32
        } else {
            0
        };

        // Get overall CPU utilization
        let overall_utilization = system.global_cpu_usage() as f64;

        // Build per-core utilization. Heterogeneous ARM parts get their
        // P/E split from the registry identifiers; everything else stays
        // Standard.
        let core_types = core_types_from_identifiers(&self.processor_identifiers);
        let mut per_core_utilization = Vec::new();
        for (i, cpu) in cpus.iter().enumerate() {
            per_core_utilization.push(CoreUtilization {
                core_id: i as u32,
                core_type: core_types.get(i).cloned().unwrap_or(CoreType::Standard),
                utilization: cpu.usage() as f64,
            });
        }

        // Get CPU temperature using fallback chain (no more error spam)
        let temperature = self.get_cpu_temperature();

        // Get static info from WMI (max frequency, cache size, socket count)
        let (wmi_max_freq, wmi_cache_size, socket_count) = self.get_wmi_processor_info();
        let max_frequency = wmi_max_freq.unwrap_or(base_frequency);
        let cache_size_mb = wmi_cache_size.unwrap_or(0);

        // Calculate per-socket values
        let cores_per_socket = total_cores / socket_count;
        let threads_per_socket = total_threads / socket_count;

        // Create per-socket info
        let per_socket_info: Vec<CpuSocketInfo> = (0..socket_count)
            .map(|socket_id| CpuSocketInfo {
                socket_id,
                utilization: overall_utilization, // Approximate: use overall for each socket
                cores: cores_per_socket,
                threads: threads_per_socket,
                temperature, // Temperature is typically system-wide on Windows
                frequency_mhz: base_frequency,
            })
            .collect();

        Ok(CpuInfo {
            index: 0, // Overwritten by AllSmi::get_cpu_info when flattening readers
            host_id: hostname.clone(),
            hostname,
            instance,
            cpu_model,
            architecture,
            platform_type,
            socket_count,
            total_cores,
            total_threads,
            base_frequency_mhz: base_frequency,
            max_frequency_mhz: max_frequency,
            cache_size_mb,
            utilization: overall_utilization,
            temperature,
            power_consumption: None,
            per_socket_info,
            apple_silicon_info: None,
            per_core_utilization,
            time,
        })
    }
}

impl CpuReader for WindowsCpuReader {
    fn get_cpu_info(&self) -> Vec<CpuInfo> {
        match self.get_cpu_info_from_system() {
            Ok(cpu_info) => vec![cpu_info],
            Err(_) => {
                // Silently return empty - errors are expected on some systems
                vec![]
            }
        }
    }
}
