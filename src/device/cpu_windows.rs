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

use crate::device::cpu_topology::tier_utilization;
use crate::device::{CoreType, CoreUtilization, CpuInfo, CpuReader, CpuSocketInfo, CpuTier};
use crate::utils::cached_probe::CachedProbe;
use crate::utils::system::get_hostname;
use chrono::Local;
use serde::Deserialize;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use sysinfo::{CpuRefreshKind, System};

// Import the temperature fallback chain
use super::cpu_windows_arm::{
    classify_platform, core_types_from_identifiers, tier_topology_from_identifiers,
};
use super::windows_temp::TemperatureManager;
use super::windows_wmi_worker::{CIMV2, with_wmi};

/// `(max clock MHz, L2+L3 cache MB, socket count)` from `Win32_Processor`.
type ProcessorInfo = (u32, u32, u32);

/// How long a failed `Win32_Processor` query waits before it is retried.
const PROCESSOR_INFO_RETRY: Duration = Duration::from_secs(60);

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

pub struct WindowsCpuReader {
    system: RwLock<System>,
    first_refresh_done: RwLock<bool>,
    // Static `Win32_Processor` info: kept forever once read, a failed
    // query retried at most once per PROCESSOR_INFO_RETRY (issue #501).
    processor_info: Mutex<CachedProbe<ProcessorInfo>>,
    // Temperature manager with fallback chain. Shared with the WMI worker
    // thread, which runs the chain against its cached connections.
    temperature_manager: Arc<TemperatureManager>,
    // Per-logical-processor registry `Identifier` strings, read once:
    // they name the ARM core design and so the P/E cluster (issue #459).
    processor_identifiers: Vec<Option<String>>,
    // Static tier topology derived from those identifiers once, never per
    // tick (issue #473).
    cpu_tiers: Vec<CpuTier>,
    tier_index_by_core: Vec<Option<u8>>,
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
        let processor_identifiers: Vec<Option<String>> = (0..system.cpus().len())
            .map(read_processor_identifier)
            .collect();
        let tier_topology = tier_topology_from_identifiers(&processor_identifiers);
        let tier_index_by_core = tier_topology.tier_index_by_core();
        let cpu_tiers = tier_topology.tiers;
        let processor_name = read_processor_value(0, "ProcessorNameString");

        Self {
            system: RwLock::new(system),
            first_refresh_done: RwLock::new(true), // Already initialized
            processor_info: Mutex::new(CachedProbe::new()),
            temperature_manager: Arc::new(TemperatureManager::new()),
            processor_identifiers,
            cpu_tiers,
            tier_index_by_core,
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
    ///
    /// The chain runs exactly once per call, on the WMI worker thread, and
    /// each source takes its own namespace connection there.
    fn get_cpu_temperature(&self) -> Option<u32> {
        let manager = Arc::clone(&self.temperature_manager);
        with_wmi(move |connections| manager.get_temperature(connections)).flatten()
    }

    /// Get static CPU info from WMI (max frequency, cache size, socket count)
    fn get_wmi_processor_info(&self) -> (Option<u32>, Option<u32>, u32) {
        processor_info_from_probe(&self.processor_info, Instant::now(), query_processor_info)
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
        // Standard. The exact tier index is separate from the coarse class
        // (issue #473).
        let core_types = core_types_from_identifiers(&self.processor_identifiers);
        let mut per_core_utilization = Vec::new();
        for (i, cpu) in cpus.iter().enumerate() {
            per_core_utilization.push(CoreUtilization {
                core_id: i as u32,
                core_type: core_types.get(i).copied().unwrap_or(CoreType::Standard),
                utilization: cpu.usage() as f64,
                tier_index: self.tier_index_by_core.get(i).copied().flatten(),
            });
        }

        // Only utilization changes per tick; labels, counts and the index map
        // were fixed at construction.
        let mut cpu_tiers = self.cpu_tiers.clone();
        for (position, tier) in cpu_tiers.iter_mut().enumerate() {
            tier.utilization = tier_utilization(&per_core_utilization, position);
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
            cpu_tiers,
            per_core_utilization,
            time,
        })
    }
}

/// Static processor info through `probe`, querying with `query` when the
/// probe's policy allows. Defaults to one socket when nothing is known.
fn processor_info_from_probe(
    probe: &Mutex<CachedProbe<ProcessorInfo>>,
    now: Instant,
    query: impl FnOnce() -> Option<ProcessorInfo>,
) -> (Option<u32>, Option<u32>, u32) {
    let info = probe.lock().unwrap_or_else(|e| e.into_inner()).get_or_try(
        now,
        None,
        PROCESSOR_INFO_RETRY,
        query,
    );
    match info {
        Some((max_freq, cache_mb, sockets)) => (Some(max_freq), Some(cache_mb), sockets),
        None => (None, None, 1),
    }
}

/// Query `Win32_Processor` on the WMI worker.
fn query_processor_info() -> Option<ProcessorInfo> {
    with_wmi(|connections| {
        let queried: Result<Vec<Win32Processor>, _> = connections
            .get(CIMV2)?
            .raw_query("SELECT MaxClockSpeed, L2CacheSize, L3CacheSize FROM Win32_Processor");
        let Ok(procs) = queried else {
            // The connection may be stale (winmgmt restarted): reconnect
            // on the next attempt instead of failing against it forever.
            connections.invalidate(CIMV2);
            return None;
        };
        // One Win32_Processor instance per physical processor (socket).
        let socket_count = procs.len().max(1) as u32;
        let proc = procs.first()?;
        let max_freq = proc.max_clock_speed.unwrap_or(0);
        // Cache size in KB, convert to MB
        let cache_mb = (proc.l2_cache_size.unwrap_or(0) + proc.l3_cache_size.unwrap_or(0)) / 1024;
        Some((max_freq, cache_mb, socket_count))
    })
    .flatten()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn failed_processor_query_is_not_retried_inside_a_minute() {
        let probe = Mutex::new(CachedProbe::new());
        let calls = Cell::new(0);
        let t0 = Instant::now();
        let failing = || {
            calls.set(calls.get() + 1);
            None
        };
        for tick in 0..30 {
            let at = t0 + Duration::from_secs(tick * 2);
            assert_eq!(
                processor_info_from_probe(&probe, at, failing),
                (None, None, 1)
            );
        }
        assert_eq!(calls.get(), 1);
        let answer = processor_info_from_probe(&probe, t0 + Duration::from_secs(60), || {
            calls.set(calls.get() + 1);
            Some((3000, 32, 2))
        });
        assert_eq!(answer, (Some(3000), Some(32), 2));
        assert_eq!(calls.get(), 2);
        let later = processor_info_from_probe(&probe, t0 + Duration::from_secs(86_400), failing);
        assert_eq!(later, (Some(3000), Some(32), 2), "success kept for good");
        assert_eq!(calls.get(), 2);
    }
}
