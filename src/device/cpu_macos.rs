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

// Use native metrics manager for Apple Silicon
use crate::device::macos_native::get_native_metrics_manager;
// Intel Macs read temperature and package power straight from the SMC: the
// native metrics manager is IOReport-backed and therefore Apple Silicon only.
use crate::device::macos_native::smc::SmcConnection;

use crate::device::apple_core_layout::{CoreLayout, gpu_core_count_from_ioreg};
use crate::device::cpu_macos_intel::IntelCpuHardware;
use crate::device::{
    AppleSiliconCpuInfo, CoreType, CoreUtilization, CpuInfo, CpuPlatformType, CpuReader,
    CpuSocketInfo,
};
use crate::utils::cached_probe::CachedProbe;
use crate::utils::command::new_command;
use crate::utils::system::get_hostname;
use chrono::Local;
use std::ffi::CString;
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};
use sysinfo::System;

/// (cpu_model, core_layout, gpu_core_count)
type AppleSiliconHwInfo = (String, CoreLayout, u32);

/// How long a failed Apple Silicon hardware probe waits before it spawns
/// `sysctl`/`ioreg` again (issue #501).
const HARDWARE_PROBE_RETRY: Duration = Duration::from_secs(60);

/// Read an integer sysctl by name without spawning `sysctl`.
///
/// Accepts both 32-bit and 64-bit values: the kernel writes the value's own
/// width and reports it in `size`. Returns `None` for a missing key.
fn sysctl_u64(name: &str) -> Option<u64> {
    let name = CString::new(name).ok()?;
    let mut value: u64 = 0;
    let mut size = std::mem::size_of::<u64>();
    // SAFETY: `name` is NUL-terminated; `value` is a writable 8-byte buffer
    // whose length is passed in `size`, which the kernel updates to the
    // bytes written; no new value is set (null pointer, zero length).
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&raw mut value).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    match size {
        8 => Some(value),
        // A 32-bit value lands in the low four bytes; Apple platforms are
        // little-endian, and `value` started at zero.
        4 => Some(value & u64::from(u32::MAX)),
        _ => None,
    }
}

pub struct MacOsCpuReader {
    is_apple_silicon: bool,
    // System info for CPU metrics (using RwLock for better concurrent read access)
    system: RwLock<System>,
    // Track if we've done the first refresh
    first_refresh_done: RwLock<bool>,
    /// When the first `refresh_cpu_usage` sample was taken, in `new`. The
    /// first `ensure_cpu_refreshed` sleeps only what is left of the warm-up
    /// interval after this instant, so the wait overlaps whatever else runs
    /// between construction and the first tick (issue #414).
    first_sample_at: Instant,
    /// Apple Silicon CPU model, perflevel topology and GPU core count.
    /// Kept for the process lifetime once read; a failed probe is retried
    /// at most once per `HARDWARE_PROBE_RETRY`.
    hardware_probe: Mutex<CachedProbe<AppleSiliconHwInfo>>,
    /// Per-tier L2 cache size in MB. Fixed hardware, so the outcome is
    /// cached whether or not the sysctl answered.
    s_core_l2_cache_mb: OnceLock<Option<u32>>,
    p_core_l2_cache_mb: OnceLock<Option<u32>>,
    e_core_l2_cache_mb: OnceLock<Option<u32>>,
    // Cached hardware info for Intel
    cached_intel_info: Mutex<Option<IntelCpuHardware>>,
    // Cached SMC connection, used by the Intel path only
    smc: Mutex<SmcConnection>,
}

impl Default for MacOsCpuReader {
    fn default() -> Self {
        Self::new()
    }
}

impl MacOsCpuReader {
    pub fn new() -> Self {
        let is_apple_silicon = Self::detect_apple_silicon();
        // Take the first CPU usage sample now: utilization is a delta between
        // two samples, and starting the clock at construction lets the first
        // tick pay only the remainder of `CPU_WARM_UP` instead of all of it.
        let mut system = crate::utils::system::new_system();
        system.refresh_cpu_usage();
        let first_sample_at = Instant::now();

        Self {
            is_apple_silicon,
            system: RwLock::new(system),
            first_refresh_done: RwLock::new(false),
            first_sample_at,
            hardware_probe: Mutex::new(CachedProbe::new()),
            s_core_l2_cache_mb: OnceLock::new(),
            p_core_l2_cache_mb: OnceLock::new(),
            e_core_l2_cache_mb: OnceLock::new(),
            cached_intel_info: Mutex::new(None),
            smc: Mutex::new(SmcConnection::default()),
        }
    }

    /// Detect Apple Silicon through the shared, cached platform probe.
    ///
    /// This used to be a second private copy of the `uname -m` check. Routing
    /// it through `platform_detection` keeps a single answer per process, and
    /// picks up the Rosetta 2 handling that copy did not have.
    fn detect_apple_silicon() -> bool {
        crate::device::platform_detection::is_apple_silicon()
    }

    fn get_cpu_info_from_system(&self) -> Result<CpuInfo, Box<dyn std::error::Error>> {
        let hostname = get_hostname();
        let instance = hostname.clone();
        let time = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

        if self.is_apple_silicon {
            self.get_apple_silicon_cpu_info(hostname, instance, time)
        } else {
            self.get_intel_mac_cpu_info(hostname, instance, time)
        }
    }

    fn get_apple_silicon_cpu_info(
        &self,
        hostname: String,
        instance: String,
        time: String,
    ) -> Result<CpuInfo, Box<dyn std::error::Error>> {
        // sysctl/ioreg run once on success, and at most once per
        // HARDWARE_PROBE_RETRY while the probe keeps failing (issue #501).
        let (cpu_model, core_layout, gpu_core_count) = self
            .hardware_probe
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_try(Instant::now(), None, HARDWARE_PROBE_RETRY, || {
                self.probe_apple_silicon_hardware()
                    .map_err(|e| eprintln!("Apple Silicon hardware probe failed: {e}"))
                    .ok()
            })
            .ok_or("Apple Silicon hardware info is unavailable")?;

        let s_core_count = core_layout.super_cores();
        let p_core_count = core_layout.performance_cores();
        let e_core_count = core_layout.efficiency_cores();

        // OPTIMIZATION: Refresh CPU usage ONCE and reuse for all subsequent reads
        // This avoids multiple refresh_cpu_usage() calls which was causing high CPU usage
        self.ensure_cpu_refreshed();

        // Get actual CPU utilization (no refresh needed - already done above)
        let cpu_utilization = self.system.read().unwrap().global_cpu_usage() as f64;

        // OPTIMIZATION: Get native metrics ONCE and reuse for frequency/power/residency
        // This avoids multiple collect_once() calls which was expensive
        let native_data = get_native_metrics_manager().and_then(|m| m.collect_once().ok());

        // Get CPU frequency information from cached native metrics
        let (base_frequency, max_frequency, s_cluster_freq, p_cluster_freq, e_cluster_freq): (
            u32,
            u32,
            Option<u32>,
            Option<u32>,
            Option<u32>,
        ) = {
            if let Some(ref data) = native_data {
                // Only clusters the layout actually has contribute. The array
                // is ordered fastest first: Super, then Performance, then
                // Efficiency.
                let present: Vec<(CoreType, u32)> = [
                    (CoreType::Super, data.s_cluster_frequency),
                    (CoreType::Performance, data.p_cluster_frequency),
                    (CoreType::Efficiency, data.e_cluster_frequency),
                ]
                .into_iter()
                .filter(|(tier, _)| core_layout.has(*tier))
                .collect();

                let avg_freq = if present.is_empty() {
                    0
                } else {
                    present.iter().map(|(_, freq)| *freq).sum::<u32>() / present.len() as u32
                };
                let max_freq = present.first().map_or(0, |(_, freq)| *freq);

                (
                    avg_freq,
                    max_freq,
                    core_layout
                        .has(CoreType::Super)
                        .then_some(data.s_cluster_frequency),
                    core_layout
                        .has(CoreType::Performance)
                        .then_some(data.p_cluster_frequency),
                    core_layout
                        .has(CoreType::Efficiency)
                        .then_some(data.e_cluster_frequency),
                )
            } else {
                (
                    self.get_cpu_base_frequency()?,
                    self.get_cpu_max_frequency()?,
                    None,
                    None,
                    None,
                )
            }
        };

        // Get per-core utilization (no refresh needed - already done above)
        let mut per_core_utilization = self.get_per_core_utilization_no_refresh(&core_layout);

        // Get CPU temperature from cached native metrics (SMC sensor reading).
        // Falls back to None if SMC didn't return a usable value.
        let temperature = native_data
            .as_ref()
            .and_then(|d| d.cpu_temperature)
            .map(|t| t.round() as u32);

        // Power consumption from cached native metrics
        let power_consumption = native_data.as_ref().map(|d| d.cpu_power_mw / 1000.0);

        let total_cores = s_core_count + p_core_count + e_core_count;
        let total_threads = total_cores; // Apple Silicon doesn't use hyperthreading

        // Get cache sizes for S, P and E cores
        let s_core_l2_cache_mb =
            Self::core_l2_cache_mb(CoreType::Super, &core_layout, &self.s_core_l2_cache_mb);
        let p_core_l2_cache_mb = Self::core_l2_cache_mb(
            CoreType::Performance,
            &core_layout,
            &self.p_core_l2_cache_mb,
        );
        let e_core_l2_cache_mb =
            Self::core_l2_cache_mb(CoreType::Efficiency, &core_layout, &self.e_core_l2_cache_mb);

        // Calculate S-core, P-core and E-core utilization from per-core data if available
        let (s_core_utilization, p_core_utilization, e_core_utilization) =
            if !per_core_utilization.is_empty() {
                let mut s_sum = 0.0;
                let mut s_count = 0;
                let mut p_sum = 0.0;
                let mut p_count = 0;
                let mut e_sum = 0.0;
                let mut e_count = 0;

                for core in &per_core_utilization {
                    match core.core_type {
                        CoreType::Super => {
                            s_sum += core.utilization;
                            s_count += 1;
                        }
                        CoreType::Performance => {
                            p_sum += core.utilization;
                            p_count += 1;
                        }
                        CoreType::Efficiency => {
                            e_sum += core.utilization;
                            e_count += 1;
                        }
                        _ => {}
                    }
                }

                let s_util = if s_count > 0 {
                    s_sum / s_count as f64
                } else {
                    0.0
                };

                let p_util = if p_count > 0 {
                    p_sum / p_count as f64
                } else if core_layout.has(CoreType::Performance) {
                    // No per-core reading even though the cluster exists.
                    cpu_utilization * 0.6
                } else {
                    // Base M5 has no Performance cluster at all.
                    0.0
                };

                let e_util = if e_count > 0 {
                    e_sum / e_count as f64
                } else if core_layout.has(CoreType::Efficiency) {
                    cpu_utilization * 0.4
                } else {
                    // M5 Pro/Max has no Efficiency cores.
                    0.0
                };

                (s_util, p_util, e_util)
            } else {
                // Fallback to estimated values
                let (s_util, p_util, e_util) = self
                    .get_apple_silicon_core_utilization(&core_layout)
                    .unwrap_or((0.0, cpu_utilization * 0.6, cpu_utilization * 0.4));
                (s_util, p_util, e_util)
            };

        let apple_silicon_info = Some(AppleSiliconCpuInfo {
            s_core_count,
            p_core_count,
            e_core_count,
            gpu_core_count,
            s_core_utilization,
            p_core_utilization,
            e_core_utilization,
            ane_ops_per_second: None, // ANE metrics are complex to get
            s_cluster_frequency_mhz: s_cluster_freq,
            p_cluster_frequency_mhz: p_cluster_freq,
            e_cluster_frequency_mhz: e_cluster_freq,
            s_core_l2_cache_mb,
            p_core_l2_cache_mb,
            e_core_l2_cache_mb,
        });

        // Mirror the perflevel topology into the platform-neutral tier list
        // (issue #473) and index each core into it. The exact Apple core type
        // is already carried on every core, so the index follows that label.
        let cpu_tiers = apple_silicon_info
            .as_ref()
            .map(AppleSiliconCpuInfo::to_cpu_tiers)
            .unwrap_or_default();
        for core in &mut per_core_utilization {
            core.tier_index = cpu_tiers
                .iter()
                .position(|tier| tier.label == core.core_type.short_label())
                .map(|position| position as u8);
        }

        // Create per-socket info (Apple Silicon typically has 1 socket)
        let per_socket_info = vec![CpuSocketInfo {
            socket_id: 0,
            utilization: cpu_utilization,
            cores: total_cores,
            threads: total_threads,
            temperature,
            frequency_mhz: base_frequency,
        }];

        Ok(CpuInfo {
            index: 0,                  // Overwritten by AllSmi::get_cpu_info when flattening readers
            host_id: hostname.clone(), // For local mode, host_id is just the hostname
            hostname,
            instance,
            cpu_model,
            architecture: "arm64".to_string(),
            platform_type: CpuPlatformType::AppleSilicon,
            socket_count: 1,
            total_cores,
            total_threads,
            base_frequency_mhz: base_frequency,
            max_frequency_mhz: max_frequency,
            cache_size_mb: s_core_l2_cache_mb.unwrap_or(0)
                + p_core_l2_cache_mb.unwrap_or(0)
                + e_core_l2_cache_mb.unwrap_or(0), // Total L2 cache
            utilization: cpu_utilization,
            temperature,
            power_consumption,
            per_socket_info,
            apple_silicon_info,
            cpu_tiers,
            per_core_utilization,
            time,
        })
    }

    fn get_intel_mac_cpu_info(
        &self,
        hostname: String,
        instance: String,
        time: String,
    ) -> Result<CpuInfo, Box<dyn std::error::Error>> {
        let hardware = self.get_intel_hardware_info();

        // Refresh once per collection cycle, then reuse the snapshot for both
        // the aggregate and the per-core numbers.
        self.ensure_cpu_refreshed();
        let cpu_utilization = self.system.read().unwrap().global_cpu_usage() as f64;

        // Intel cores are homogeneous, so no P/E/S split applies: an empty
        // layout makes every core Standard.
        let per_core_utilization = self.get_per_core_utilization_no_refresh(&CoreLayout::default());

        // CPU temperature and package power come from the SMC over the cached
        // connection. Both are absent on some models, which is not an error.
        let (temperature, power_consumption) = self.read_intel_smc_metrics();

        let socket_count = hardware.socket_count.max(1);

        // Create per-socket info
        let mut per_socket_info = Vec::new();
        for socket_id in 0..socket_count {
            per_socket_info.push(CpuSocketInfo {
                socket_id,
                utilization: cpu_utilization,
                cores: hardware.physical_cores / socket_count,
                threads: hardware.logical_cores / socket_count,
                temperature,
                frequency_mhz: hardware.base_frequency_mhz,
            });
        }

        Ok(CpuInfo {
            index: 0,                  // Overwritten by AllSmi::get_cpu_info when flattening readers
            host_id: hostname.clone(), // For local mode, host_id is just the hostname
            hostname,
            instance,
            cpu_model: hardware.model.clone(),
            architecture: "x86_64".to_string(),
            platform_type: CpuPlatformType::Intel,
            socket_count,
            total_cores: hardware.physical_cores,
            total_threads: hardware.logical_cores,
            base_frequency_mhz: hardware.base_frequency_mhz,
            max_frequency_mhz: hardware.max_frequency_mhz,
            cache_size_mb: hardware.l3_cache_mb,
            utilization: cpu_utilization,
            temperature,
            power_consumption,
            per_socket_info,
            apple_silicon_info: None,
            cpu_tiers: Vec::new(), // Intel Macs are homogeneous
            per_core_utilization,
            time,
        })
    }

    /// Read CPU temperature (Celsius) and package power (watts) from the SMC.
    ///
    /// Both readings share one cached connection so a poll costs two key reads
    /// rather than a fresh IOKit handshake.
    fn read_intel_smc_metrics(&self) -> (Option<u32>, Option<f64>) {
        let Ok(mut guard) = self.smc.lock() else {
            return (None, None);
        };
        let Some(smc) = guard.get() else {
            return (None, None);
        };

        let temperature = smc.get_cpu_temperature().map(|t| t.round() as u32);
        let power = smc.get_cpu_package_power();

        (temperature, power)
    }

    /// Collect the immutable Intel CPU hardware description, caching it.
    ///
    /// Discovery itself lives in `cpu_macos_intel`; this only owns the cache.
    fn get_intel_hardware_info(&self) -> IntelCpuHardware {
        if let Some(cached) = self.cached_intel_info.lock().unwrap().clone() {
            return cached;
        }

        // Last-resort thread count, used only when `hw.logicalcpu` is missing.
        // Deliberately not `self.system.cpus().len()`: this runs before the
        // first `ensure_cpu_refreshed`, and a freshly constructed `System` has
        // an empty CPU list, so that source reports 0 and the result would be
        // cached for the process lifetime.
        let logical_cpu_fallback = std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(0);
        let hardware = IntelCpuHardware::collect(logical_cpu_fallback);

        *self.cached_intel_info.lock().unwrap() = Some(hardware.clone());
        hardware
    }

    /// Fast method to get Apple Silicon hardware info using sysctl and ioreg
    /// This is ~50x faster than system_profiler (~10ms vs ~500ms)
    /// Returns (cpu_model, core_layout, gpu_core_count). Caching is the
    /// caller's job (`hardware_probe`).
    fn probe_apple_silicon_hardware(
        &self,
    ) -> Result<AppleSiliconHwInfo, Box<dyn std::error::Error>> {
        // Get CPU model using sysctl (fast: ~10ms vs system_profiler ~500ms)
        let cpu_model = self.get_cpu_model_sysctl()?;

        // Read both perflevel names and counts in one call. `sysctl` exits
        // non-zero when a name key is absent (M1-M3 on older macOS), so only
        // stdout is parsed and the exit status is deliberately ignored.
        let output = new_command("sysctl")
            .args([
                "hw.perflevel0.name",
                "hw.perflevel0.physicalcpu",
                "hw.perflevel1.name",
                "hw.perflevel1.physicalcpu",
            ])
            .output()?;

        let core_layout = CoreLayout::from_sysctl_output(&String::from_utf8_lossy(&output.stdout));

        // Any layout that names no cores at all is unusable.
        if core_layout.total_cores() == 0 {
            return Err("Failed to get core counts".into());
        }

        // Get GPU core count using ioreg (fast: ~50ms vs system_profiler
        // ~500ms). The model is passed in: the estimate fallback used to read
        // a cache that was only written after this call (issue #501).
        let gpu_core_count = self.get_gpu_core_count_ioreg(&cpu_model);

        Ok((cpu_model, core_layout, gpu_core_count))
    }

    /// Get CPU model from sysctl machdep.cpu.brand_string
    /// This is significantly faster than system_profiler (~10ms vs ~500ms)
    fn get_cpu_model_sysctl(&self) -> Result<String, Box<dyn std::error::Error>> {
        let output = new_command("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()?;

        let cpu_brand = String::from_utf8_lossy(&output.stdout).trim().to_string();

        // Extract chip name from brand string
        // e.g., "Apple M2 Pro" -> "Apple M2 Pro"
        if cpu_brand.starts_with("Apple M") {
            Ok(cpu_brand)
        } else if cpu_brand.is_empty() {
            Err("Failed to get CPU model from sysctl".into())
        } else {
            // For other cases, return as-is
            Ok(cpu_brand)
        }
    }

    /// GPU core count from ioreg (faster than system_profiler), estimated
    /// from `cpu_model` when ioreg publishes no `gpu-core-count`; 0 when
    /// neither knows.
    fn get_gpu_core_count_ioreg(&self, cpu_model: &str) -> u32 {
        let output = new_command("ioreg")
            .args(["-rc", "AGXAccelerator", "-d1"])
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
            .unwrap_or_default();
        gpu_core_count_from_ioreg(&output, cpu_model).unwrap_or(0)
    }

    // These methods are no longer used since we fetch both values in a single sysctl call
    #[allow(dead_code)]
    fn get_p_core_count(&self) -> Result<u32, Box<dyn std::error::Error>> {
        let output = new_command("sysctl")
            .arg("hw.perflevel0.physicalcpu")
            .output()?;

        let output_str = String::from_utf8_lossy(&output.stdout);
        if let Some(value_str) = output_str.split(':').nth(1) {
            let count = value_str.trim().parse::<u32>()?;
            Ok(count)
        } else {
            Err("Failed to parse P-core count".into())
        }
    }

    #[allow(dead_code)]
    fn get_e_core_count(&self) -> Result<u32, Box<dyn std::error::Error>> {
        let output = new_command("sysctl")
            .arg("hw.perflevel1.physicalcpu")
            .output()?;

        let output_str = String::from_utf8_lossy(&output.stdout);
        if let Some(value_str) = output_str.split(':').nth(1) {
            let count = value_str.trim().parse::<u32>()?;
            Ok(count)
        } else {
            Err("Failed to parse E-core count".into())
        }
    }

    #[allow(dead_code)] // Kept as fallback, but get_gpu_core_count_ioreg() is preferred
    fn get_gpu_core_count(&self) -> Result<u32, Box<dyn std::error::Error>> {
        let output = new_command("system_profiler")
            .arg("SPDisplaysDataType")
            .arg("-json")
            .output()?;

        let output_str = String::from_utf8_lossy(&output.stdout);

        // Parse JSON to find GPU core count
        // Look for "sppci_cores" field in the JSON output
        for line in output_str.lines() {
            if line.contains("sppci_cores") {
                // Extract the value between quotes after the colon
                if let Some(value_part) = line.split(':').nth(1)
                    && let Some(start_quote) = value_part.find('"')
                    && let Some(end_quote) = value_part[start_quote + 1..].find('"')
                {
                    let core_str = &value_part[start_quote + 1..start_quote + 1 + end_quote];
                    if let Ok(count) = core_str.parse::<u32>() {
                        return Ok(count);
                    }
                }
            }
        }

        Err("Failed to parse GPU core count".into())
    }

    /// L2 cache size in MB for the perflevel behind `tier`, read once.
    ///
    /// The key comes from the layout, so base M5 reads its E-cluster cache
    /// from perflevel1 even though its Super cluster occupies perflevel0. A
    /// tier the chip lacks, or a key the kernel does not publish, caches
    /// `None`.
    fn core_l2_cache_mb(
        tier: CoreType,
        layout: &CoreLayout,
        cache: &OnceLock<Option<u32>>,
    ) -> Option<u32> {
        *cache.get_or_init(|| {
            let key = layout.l2_cache_key(tier)?;
            sysctl_u64(&key).map(|bytes| (bytes / 1024 / 1024) as u32)
        })
    }

    /// The least time between the first two `refresh_cpu_usage` samples for
    /// the first utilization reading to be meaningful.
    const CPU_WARM_UP: Duration = Duration::from_millis(100);

    /// OPTIMIZATION: Refresh CPU usage once per collection cycle
    /// This avoids multiple refresh_cpu_usage() calls which was causing high CPU usage
    fn ensure_cpu_refreshed(&self) {
        // The first sample was taken in `new`; on the first call, wait out
        // whatever is left of the warm-up interval since then. When more than
        // that has passed already, as it has whenever other readers were
        // built in between, there is nothing to wait for.
        if !*self.first_refresh_done.read().unwrap() {
            let remaining = Self::CPU_WARM_UP.saturating_sub(self.first_sample_at.elapsed());
            if !remaining.is_zero() {
                std::thread::sleep(remaining);
            }
            *self.first_refresh_done.write().unwrap() = true;
        }

        // Single refresh per collection cycle
        self.system.write().unwrap().refresh_cpu_usage();
    }

    #[allow(dead_code)] // Kept as fallback method when sysinfo is unavailable
    fn get_cpu_utilization_iostat(&self) -> Result<f64, Box<dyn std::error::Error>> {
        // Fallback method using iostat (kept for compatibility)
        let output = new_command("iostat").args(["-c", "1"]).output()?;

        let iostat_output = String::from_utf8_lossy(&output.stdout);

        // Parse CPU utilization from iostat output
        for line in iostat_output.lines() {
            if line.contains("avg-cpu") {
                continue;
            }
            if line
                .trim()
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
            {
                let fields: Vec<&str> = line.split_whitespace().collect();
                if fields.len() >= 6 {
                    // iostat format: %user %nice %system %iowait %steal %idle
                    let idle = fields[5].parse::<f64>().unwrap_or(0.0);
                    return Ok(100.0 - idle);
                }
            }
        }

        Ok(0.0)
    }

    /// Estimate per-cluster utilization from cluster residency when sysinfo
    /// returned no per-core data. Only tiers the layout actually has receive a
    /// value, and the two fastest present tiers keep the historical 1.2 / 0.8
    /// weighting.
    fn get_apple_silicon_core_utilization(
        &self,
        layout: &CoreLayout,
    ) -> Result<(f64, f64, f64), Box<dyn std::error::Error>> {
        // OPTIMIZATION: This is a fallback method only called when per_core_utilization is empty
        // Don't refresh CPU here since caller (get_apple_silicon_cpu_info) already did via ensure_cpu_refreshed()
        let total_cpu_util = self.system.read().unwrap().global_cpu_usage() as f64;

        let present = |tier: CoreType| -> bool { layout.has(tier) };

        // Use native metrics manager for cluster residency (cached, no extra collection)
        if let Some(manager) = get_native_metrics_manager()
            && let Ok(data) = manager.collect_once()
        {
            let clusters: Vec<(CoreType, f64)> = [
                (CoreType::Super, data.s_cluster_active_residency),
                (CoreType::Performance, data.p_cluster_active_residency),
                (CoreType::Efficiency, data.e_cluster_active_residency),
            ]
            .into_iter()
            .filter(|(tier, _)| present(*tier))
            .collect();

            let total_residency: f64 = clusters
                .iter()
                .map(|(_, residency)| residency.clamp(0.0, 100.0))
                .sum();

            if total_residency > 0.0 {
                let mut result = (0.0, 0.0, 0.0);
                for (rank, (tier, residency)) in clusters.iter().enumerate() {
                    let weight = if rank == 0 { 1.2 } else { 0.8 };
                    let value =
                        (total_cpu_util * (residency.clamp(0.0, 100.0) / total_residency) * weight)
                            .clamp(0.0, 100.0);
                    match tier {
                        CoreType::Super => result.0 = value,
                        CoreType::Performance => result.1 = value,
                        _ => result.2 = value,
                    }
                }
                return Ok(result);
            }
        }

        // Return default values if the manager is not available: split the
        // total across the two fastest present tiers (60/40 on M1-M4).
        let clusters: Vec<CoreType> =
            [CoreType::Super, CoreType::Performance, CoreType::Efficiency]
                .into_iter()
                .filter(|tier| present(*tier))
                .take(2)
                .collect();

        let mut result = (0.0, 0.0, 0.0);
        for (rank, tier) in clusters.iter().enumerate() {
            let share = if clusters.len() == 1 {
                1.0
            } else if rank == 0 {
                0.6
            } else {
                0.4
            };
            let value = total_cpu_util * share;
            match tier {
                CoreType::Super => result.0 = value,
                CoreType::Performance => result.1 = value,
                _ => result.2 = value,
            }
        }
        Ok(result)
    }

    fn get_cpu_base_frequency(&self) -> Result<u32, Box<dyn std::error::Error>> {
        if self.is_apple_silicon {
            // Apple Silicon base frequencies are not easily available
            // Return typical values based on chip
            Ok(3000) // 3 GHz as default
        } else {
            // Intel: the real nominal clock, from hw.cpufrequency or the brand
            // string, instead of a hardcoded constant.
            Ok(self.get_intel_hardware_info().base_frequency_mhz)
        }
    }

    fn get_cpu_max_frequency(&self) -> Result<u32, Box<dyn std::error::Error>> {
        if self.is_apple_silicon {
            // Apple Silicon max frequencies vary by core type
            Ok(3500) // Typical P-core max frequency
        } else {
            // Intel: hw.cpufrequency_max, falling back to the base clock when
            // the key is absent (see IntelCpuHardware::apply_defaults).
            Ok(self.get_intel_hardware_info().max_frequency_mhz)
        }
    }

    #[allow(dead_code)] // OPTIMIZATION: Now using cached native_data instead
    fn get_cpu_power_consumption(&self) -> Option<f64> {
        // Use native metrics manager
        if let Some(manager) = get_native_metrics_manager()
            && let Ok(data) = manager.collect_once()
        {
            return Some(data.cpu_power_mw / 1000.0); // Convert mW to W
        }

        None
    }

    /// Get per-core CPU utilization using sysinfo (with refresh)
    #[allow(dead_code)] // Kept for potential future use when refresh is needed
    fn get_per_core_utilization(&self, layout: &CoreLayout) -> Vec<CoreUtilization> {
        // Refresh CPU usage to get latest data
        self.system.write().unwrap().refresh_cpu_usage();
        self.get_per_core_utilization_no_refresh(layout)
    }

    /// Get per-core CPU utilization WITHOUT refreshing CPU data
    /// OPTIMIZATION: Used when CPU has already been refreshed via ensure_cpu_refreshed()
    ///
    /// Super-core chips list their logical CPUs Super first, then Performance,
    /// then Efficiency. Legacy chips keep the Efficiency-first order.
    fn get_per_core_utilization_no_refresh(&self, layout: &CoreLayout) -> Vec<CoreUtilization> {
        let mut per_core_utilization = Vec::new();

        let system = self.system.read().unwrap();
        let cpus = system.cpus();

        let s_core_count = layout.super_cores() as usize;
        let p_core_count = layout.performance_cores() as usize;
        let e_core_count = layout.efficiency_cores() as usize;
        let has_super = s_core_count > 0;

        for (core_id, cpu) in cpus.iter().enumerate() {
            let utilization = cpu.usage() as f64;

            // Determine core type based on index and chip architecture
            let core_type = if self.is_apple_silicon {
                if has_super {
                    // Super chips: perflevel0 (Super) first, then
                    // Performance, then the E-cluster.
                    if core_id < s_core_count {
                        CoreType::Super
                    } else if core_id < s_core_count + p_core_count {
                        CoreType::Performance
                    } else if core_id < s_core_count + p_core_count + e_core_count {
                        CoreType::Efficiency
                    } else {
                        CoreType::Standard
                    }
                } else {
                    // M1-M4 layout: E-cores first, then P-cores
                    if core_id < e_core_count {
                        CoreType::Efficiency
                    } else if core_id < e_core_count + p_core_count {
                        CoreType::Performance
                    } else {
                        CoreType::Standard
                    }
                }
            } else {
                CoreType::Standard // Intel Macs use standard cores
            };

            per_core_utilization.push(CoreUtilization {
                core_id: core_id as u32,
                core_type,
                utilization,
                tier_index: None,
            });
        }

        per_core_utilization
    }
}

impl CpuReader for MacOsCpuReader {
    fn get_cpu_info(&self) -> Vec<CpuInfo> {
        match self.get_cpu_info_from_system() {
            Ok(cpu_info) => {
                vec![cpu_info]
            }
            Err(e) => {
                eprintln!("Error reading CPU info: {e}");
                vec![]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sysctl_u64_reads_64_and_32_bit_values() {
        assert!(sysctl_u64("hw.memsize").is_some_and(|bytes| bytes > 0));
        assert!(sysctl_u64("hw.ncpu").is_some_and(|n| n > 0));
    }

    #[test]
    fn sysctl_u64_reads_perflevel_l2_cache_when_published() {
        // Apple Silicon publishes hw.perflevel0; a virtualised runner may
        // not, and then the key must read as missing rather than as zero.
        if let Some(bytes) = sysctl_u64("hw.perflevel0.l2cachesize") {
            assert!(bytes > 0);
        }
    }

    #[test]
    fn sysctl_u64_missing_key_is_none() {
        assert_eq!(sysctl_u64("hw.perflevel9.no_such_key"), None);
        assert_eq!(sysctl_u64("bad\0name"), None);
    }
}
