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

use crate::device::{
    AppleSiliconCpuInfo, CoreType, CoreUtilization, CpuInfo, CpuPlatformType, CpuReader,
    CpuSocketInfo,
};
use crate::utils::system::get_hostname;
use chrono::Local;
use std::process::Command;
use std::sync::{Mutex, RwLock};
use sysinfo::System;

type CpuHardwareParseResult = Result<(String, u32, u32, u32, u32, u32), Box<dyn std::error::Error>>;
type IntelCpuInfo = (String, u32, u32, u32, u32, u32);

pub struct MacOsCpuReader {
    is_apple_silicon: bool,
    // System info for CPU metrics (using RwLock for better concurrent read access)
    system: RwLock<System>,
    // Track if we've done the first refresh
    first_refresh_done: RwLock<bool>,
    // Cached hardware info for Apple Silicon
    cached_cpu_model: Mutex<Option<String>>,
    cached_p_core_count: Mutex<Option<u32>>,
    cached_e_core_count: Mutex<Option<u32>>,
    cached_gpu_core_count: Mutex<Option<u32>>,
    cached_p_core_l2_cache_mb: Mutex<Option<u32>>,
    cached_e_core_l2_cache_mb: Mutex<Option<u32>>,
    // Cached hardware info for Intel
    cached_intel_info: Mutex<Option<IntelCpuInfo>>,
}

impl Default for MacOsCpuReader {
    fn default() -> Self {
        Self::new()
    }
}

impl MacOsCpuReader {
    pub fn new() -> Self {
        let is_apple_silicon = Self::detect_apple_silicon();
        let system = System::new();

        Self {
            is_apple_silicon,
            system: RwLock::new(system),
            first_refresh_done: RwLock::new(false),
            cached_cpu_model: Mutex::new(None),
            cached_p_core_count: Mutex::new(None),
            cached_e_core_count: Mutex::new(None),
            cached_gpu_core_count: Mutex::new(None),
            cached_p_core_l2_cache_mb: Mutex::new(None),
            cached_e_core_l2_cache_mb: Mutex::new(None),
            cached_intel_info: Mutex::new(None),
        }
    }

    fn detect_apple_silicon() -> bool {
        if let Ok(output) = Command::new("uname").arg("-m").output() {
            let architecture = String::from_utf8_lossy(&output.stdout);
            return architecture.trim() == "arm64";
        }
        false
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
        // Check cache BEFORE calling sysctl commands
        // IMPORTANT: Read cache values first and drop the locks before any else branch
        let cached_values = {
            let cpu_model = self.cached_cpu_model.lock().unwrap().clone();
            let p_core_count = *self.cached_p_core_count.lock().unwrap();
            let e_core_count = *self.cached_e_core_count.lock().unwrap();
            let gpu_core_count = *self.cached_gpu_core_count.lock().unwrap();
            (cpu_model, p_core_count, e_core_count, gpu_core_count)
        };
        // Now all locks are released

        let (cpu_model, p_core_count, e_core_count, gpu_core_count) = if let (
            Some(cpu_model),
            Some(p_core_count),
            Some(e_core_count),
            Some(gpu_core_count),
        ) = cached_values
        {
            // Use cached values - avoids sysctl/ioreg calls
            (cpu_model, p_core_count, e_core_count, gpu_core_count)
        } else {
            // Get CPU model and core counts using fast sysctl commands (first time only)
            self.parse_apple_silicon_hardware_info_fast()?
        };

        // OPTIMIZATION: Refresh CPU usage ONCE and reuse for all subsequent reads
        // This avoids multiple refresh_cpu_usage() calls which was causing high CPU usage
        self.ensure_cpu_refreshed();

        // Get actual CPU utilization (no refresh needed - already done above)
        let cpu_utilization = self.system.read().unwrap().global_cpu_usage() as f64;

        // OPTIMIZATION: Get native metrics ONCE and reuse for frequency/power/residency
        // This avoids multiple collect_once() calls which was expensive
        let native_data = get_native_metrics_manager().and_then(|m| m.collect_once().ok());

        // Get CPU frequency information from cached native metrics
        let (base_frequency, max_frequency, p_cluster_freq, e_cluster_freq): (
            u32,
            u32,
            Option<u32>,
            Option<u32>,
        ) = {
            if let Some(ref data) = native_data {
                // Use actual frequencies from native metrics
                let avg_freq = (data.p_cluster_frequency + data.e_cluster_frequency) / 2;
                (
                    avg_freq,
                    data.p_cluster_frequency, // P-cluster frequency as max
                    Some(data.p_cluster_frequency),
                    Some(data.e_cluster_frequency),
                )
            } else {
                (
                    self.get_cpu_base_frequency()?,
                    self.get_cpu_max_frequency()?,
                    None,
                    None,
                )
            }
        };

        // Get per-core utilization (no refresh needed - already done above)
        let per_core_utilization =
            self.get_per_core_utilization_no_refresh(e_core_count as usize, p_core_count as usize);

        // Get CPU temperature from cached native metrics (SMC sensor reading).
        // Falls back to None if SMC didn't return a usable value.
        let temperature = native_data
            .as_ref()
            .and_then(|d| d.cpu_temperature)
            .map(|t| t.round() as u32);

        // Power consumption from cached native metrics
        let power_consumption = native_data.as_ref().map(|d| d.cpu_power_mw / 1000.0);

        let total_cores = p_core_count + e_core_count;
        let total_threads = total_cores; // Apple Silicon doesn't use hyperthreading

        // Get cache sizes for P and E cores
        let p_core_l2_cache_mb = self.get_p_core_l2_cache_size().ok();
        let e_core_l2_cache_mb = self.get_e_core_l2_cache_size().ok();

        // Calculate P-core and E-core utilization from per-core data if available
        let (p_core_utilization, e_core_utilization) = if !per_core_utilization.is_empty() {
            let mut p_sum = 0.0;
            let mut p_count = 0;
            let mut e_sum = 0.0;
            let mut e_count = 0;

            for core in &per_core_utilization {
                match core.core_type {
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

            let p_util = if p_count > 0 {
                p_sum / p_count as f64
            } else {
                cpu_utilization * 0.6
            };

            let e_util = if e_count > 0 {
                e_sum / e_count as f64
            } else {
                cpu_utilization * 0.4
            };

            (p_util, e_util)
        } else {
            // Fallback to estimated values
            self.get_apple_silicon_core_utilization()
                .unwrap_or((cpu_utilization * 0.6, cpu_utilization * 0.4))
        };

        let apple_silicon_info = Some(AppleSiliconCpuInfo {
            p_core_count,
            e_core_count,
            gpu_core_count,
            p_core_utilization,
            e_core_utilization,
            ane_ops_per_second: None, // ANE metrics are complex to get
            p_cluster_frequency_mhz: p_cluster_freq,
            e_cluster_frequency_mhz: e_cluster_freq,
            p_core_l2_cache_mb,
            e_core_l2_cache_mb,
        });

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
            cache_size_mb: p_core_l2_cache_mb.unwrap_or(0) + e_core_l2_cache_mb.unwrap_or(0), // Total L2 cache
            utilization: cpu_utilization,
            temperature,
            power_consumption,
            per_socket_info,
            apple_silicon_info,
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
        // Check cache BEFORE calling expensive system_profiler command
        // IMPORTANT: Read cache value first and drop the lock before any else branch
        let cached_info = self.cached_intel_info.lock().unwrap().clone();
        // Lock is now released

        let (cpu_model, socket_count, total_cores, total_threads, base_frequency, cache_size) =
            if let Some(info) = cached_info {
                // Use cached values - avoids expensive system_profiler call
                info
            } else {
                // Get CPU information using system_profiler (first time only)
                let output = Command::new("system_profiler")
                    .arg("SPHardwareDataType")
                    .output()?;

                let hardware_info = String::from_utf8_lossy(&output.stdout);
                self.parse_intel_mac_hardware_info(&hardware_info)?
            };

        // Get CPU utilization using sysinfo
        let cpu_utilization = self.get_cpu_utilization_sysinfo()?;

        // Get CPU temperature (may not be available)
        let temperature = self.get_cpu_temperature();

        // Power consumption is not easily available on Intel Macs
        let power_consumption = None;

        // Create per-socket info
        let mut per_socket_info = Vec::new();
        for socket_id in 0..socket_count {
            per_socket_info.push(CpuSocketInfo {
                socket_id,
                utilization: cpu_utilization,
                cores: total_cores / socket_count,
                threads: total_threads / socket_count,
                temperature,
                frequency_mhz: base_frequency,
            });
        }

        Ok(CpuInfo {
            host_id: hostname.clone(), // For local mode, host_id is just the hostname
            hostname,
            instance,
            cpu_model,
            architecture: "x86_64".to_string(),
            platform_type: CpuPlatformType::Intel,
            socket_count,
            total_cores,
            total_threads,
            base_frequency_mhz: base_frequency,
            max_frequency_mhz: base_frequency, // Max frequency not easily available
            cache_size_mb: cache_size,
            utilization: cpu_utilization,
            temperature,
            power_consumption,
            per_socket_info,
            apple_silicon_info: None,
            per_core_utilization: Vec::new(), // Intel Macs don't have easy per-core data
            time,
        })
    }

    /// Fast method to get Apple Silicon hardware info using sysctl and ioreg
    /// This is ~50x faster than system_profiler (~10ms vs ~500ms)
    fn parse_apple_silicon_hardware_info_fast(
        &self,
    ) -> Result<(String, u32, u32, u32), Box<dyn std::error::Error>> {
        // Check if we have cached values
        if let (Some(cpu_model), Some(p_core_count), Some(e_core_count), Some(gpu_core_count)) = (
            self.cached_cpu_model.lock().unwrap().clone(),
            *self.cached_p_core_count.lock().unwrap(),
            *self.cached_e_core_count.lock().unwrap(),
            *self.cached_gpu_core_count.lock().unwrap(),
        ) {
            return Ok((cpu_model, p_core_count, e_core_count, gpu_core_count));
        }

        // Get CPU model using sysctl (fast: ~10ms vs system_profiler ~500ms)
        let cpu_model = self.get_cpu_model_sysctl()?;

        // Get P and E core counts in a single sysctl call
        let output = Command::new("sysctl")
            .args(["hw.perflevel0.physicalcpu", "hw.perflevel1.physicalcpu"])
            .output()?;

        let output_str = String::from_utf8_lossy(&output.stdout);
        let mut p_core_count = 0u32;
        let mut e_core_count = 0u32;

        for line in output_str.lines() {
            if line.starts_with("hw.perflevel0.physicalcpu:") {
                if let Some(value) = line.split(':').nth(1) {
                    p_core_count = value.trim().parse().unwrap_or(0);
                }
            } else if line.starts_with("hw.perflevel1.physicalcpu:")
                && let Some(value) = line.split(':').nth(1)
            {
                e_core_count = value.trim().parse().unwrap_or(0);
            }
        }

        // Get GPU core count using ioreg (fast: ~50ms vs system_profiler ~500ms)
        let gpu_core_count = self.get_gpu_core_count_ioreg().unwrap_or(0);

        // Validate we got valid counts
        if p_core_count == 0 || e_core_count == 0 {
            return Err("Failed to get core counts".into());
        }

        // Cache the values
        *self.cached_cpu_model.lock().unwrap() = Some(cpu_model.clone());
        *self.cached_p_core_count.lock().unwrap() = Some(p_core_count);
        *self.cached_e_core_count.lock().unwrap() = Some(e_core_count);
        *self.cached_gpu_core_count.lock().unwrap() = Some(gpu_core_count);

        Ok((cpu_model, p_core_count, e_core_count, gpu_core_count))
    }

    /// Get CPU model from sysctl machdep.cpu.brand_string
    /// This is significantly faster than system_profiler (~10ms vs ~500ms)
    fn get_cpu_model_sysctl(&self) -> Result<String, Box<dyn std::error::Error>> {
        let output = Command::new("sysctl")
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

    /// Get GPU core count using ioreg (faster than system_profiler)
    fn get_gpu_core_count_ioreg(&self) -> Result<u32, Box<dyn std::error::Error>> {
        let output = Command::new("ioreg")
            .args(["-rc", "AGXAccelerator", "-d1"])
            .output()?;

        let output_str = String::from_utf8_lossy(&output.stdout);

        // Look for "gpu-core-count" in ioreg output
        for line in output_str.lines() {
            if line.contains("\"gpu-core-count\"") {
                // Format: "gpu-core-count" = 10
                let parts: Vec<&str> = line.split('=').collect();
                if parts.len() >= 2
                    && let Ok(count) = parts[1].trim().parse::<u32>()
                {
                    return Ok(count);
                }
            }
        }

        // Fallback: estimate from CPU model
        self.estimate_gpu_cores_from_model()
    }

    /// Estimate GPU core count from CPU model name
    fn estimate_gpu_cores_from_model(&self) -> Result<u32, Box<dyn std::error::Error>> {
        if let Some(cpu_model) = self.cached_cpu_model.lock().unwrap().clone() {
            let model = cpu_model.as_str();
            let core_count = match model {
                s if s.contains("M1 ")
                    && !s.contains("Pro")
                    && !s.contains("Max")
                    && !s.contains("Ultra") =>
                {
                    8
                }
                s if s.contains("M1 Pro") => 16,
                s if s.contains("M1 Max") => 32,
                s if s.contains("M1 Ultra") => 64,
                s if s.contains("M2 ")
                    && !s.contains("Pro")
                    && !s.contains("Max")
                    && !s.contains("Ultra") =>
                {
                    10
                }
                s if s.contains("M2 Pro") => 19,
                s if s.contains("M2 Max") => 38,
                s if s.contains("M2 Ultra") => 76,
                s if s.contains("M3 ") && !s.contains("Pro") && !s.contains("Max") => 10,
                s if s.contains("M3 Pro") => 18,
                s if s.contains("M3 Max") => 40,
                s if s.contains("M4 ") && !s.contains("Pro") && !s.contains("Max") => 10,
                s if s.contains("M4 Pro") => 20,
                s if s.contains("M4 Max") => 40,
                _ => 8, // Default fallback
            };
            return Ok(core_count);
        }
        Err("Cannot estimate GPU cores without CPU model".into())
    }

    // These methods are no longer used since we fetch both values in a single sysctl call
    #[allow(dead_code)]
    fn get_p_core_count(&self) -> Result<u32, Box<dyn std::error::Error>> {
        let output = Command::new("sysctl")
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
        let output = Command::new("sysctl")
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
        let output = Command::new("system_profiler")
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

    fn get_p_core_l2_cache_size(&self) -> Result<u32, Box<dyn std::error::Error>> {
        // Check if we have cached value
        if let Some(cached) = *self.cached_p_core_l2_cache_mb.lock().unwrap() {
            return Ok(cached);
        }

        let output = Command::new("sysctl")
            .arg("hw.perflevel0.l2cachesize")
            .output()?;

        let output_str = String::from_utf8_lossy(&output.stdout);
        if let Some(value_str) = output_str.split(':').nth(1) {
            let cache_bytes = value_str.trim().parse::<u64>()?;
            let cache_mb = (cache_bytes / 1024 / 1024) as u32; // Convert bytes to MB

            // Cache the value
            *self.cached_p_core_l2_cache_mb.lock().unwrap() = Some(cache_mb);
            Ok(cache_mb)
        } else {
            Err("Failed to parse P-core L2 cache size".into())
        }
    }

    fn get_e_core_l2_cache_size(&self) -> Result<u32, Box<dyn std::error::Error>> {
        // Check if we have cached value
        if let Some(cached) = *self.cached_e_core_l2_cache_mb.lock().unwrap() {
            return Ok(cached);
        }

        let output = Command::new("sysctl")
            .arg("hw.perflevel1.l2cachesize")
            .output()?;

        let output_str = String::from_utf8_lossy(&output.stdout);
        if let Some(value_str) = output_str.split(':').nth(1) {
            let cache_bytes = value_str.trim().parse::<u64>()?;
            let cache_mb = (cache_bytes / 1024 / 1024) as u32; // Convert bytes to MB

            // Cache the value
            *self.cached_e_core_l2_cache_mb.lock().unwrap() = Some(cache_mb);
            Ok(cache_mb)
        } else {
            Err("Failed to parse E-core L2 cache size".into())
        }
    }

    fn parse_intel_mac_hardware_info(&self, hardware_info: &str) -> CpuHardwareParseResult {
        // Check if we have cached values
        if let Some(cached_info) = self.cached_intel_info.lock().unwrap().clone() {
            return Ok(cached_info);
        }

        let mut cpu_model = String::new();
        let mut socket_count = 1u32;
        let mut total_cores = 0u32;
        let mut total_threads = 0u32;
        let mut base_frequency = 0u32;
        let mut cache_size = 0u32;

        for line in hardware_info.lines() {
            let line = line.trim();
            if line.starts_with("Processor Name:") {
                cpu_model = line.split(':').nth(1).unwrap_or("").trim().to_string();
            } else if line.starts_with("Processor Speed:") {
                if let Some(ghz) = crate::parse_colon_value!(line, f64) {
                    base_frequency = (ghz * 1000.0) as u32;
                }
            } else if line.starts_with("Number of Processors:") {
                if let Some(procs) = crate::parse_colon_value!(line, u32) {
                    socket_count = procs;
                }
            } else if line.starts_with("Total Number of Cores:") {
                if let Some(cores) = crate::parse_colon_value!(line, u32) {
                    total_cores = cores;
                    total_threads = cores * 2; // Assume hyperthreading
                }
            } else if line.starts_with("L3 Cache:")
                && let Some(size) = crate::parse_colon_value!(line, u32)
            {
                cache_size = size;
            }
        }

        let result = (
            cpu_model,
            socket_count,
            total_cores,
            total_threads,
            base_frequency,
            cache_size,
        );

        // Cache the values
        *self.cached_intel_info.lock().unwrap() = Some(result.clone());

        Ok(result)
    }

    fn get_cpu_utilization_sysinfo(&self) -> Result<f64, Box<dyn std::error::Error>> {
        // Check if we need to do first refresh
        if !*self.first_refresh_done.read().unwrap() {
            self.system.write().unwrap().refresh_cpu_usage();
            std::thread::sleep(std::time::Duration::from_millis(100));
            *self.first_refresh_done.write().unwrap() = true;
        }

        // Refresh CPU information to get latest data
        self.system.write().unwrap().refresh_cpu_usage();

        // Get global CPU usage
        let cpu_usage = self.system.read().unwrap().global_cpu_usage() as f64;

        Ok(cpu_usage)
    }

    /// OPTIMIZATION: Refresh CPU usage once per collection cycle
    /// This avoids multiple refresh_cpu_usage() calls which was causing high CPU usage
    fn ensure_cpu_refreshed(&self) {
        // Check if we need to do first refresh with initialization delay
        if !*self.first_refresh_done.read().unwrap() {
            self.system.write().unwrap().refresh_cpu_usage();
            std::thread::sleep(std::time::Duration::from_millis(100));
            *self.first_refresh_done.write().unwrap() = true;
        }

        // Single refresh per collection cycle
        self.system.write().unwrap().refresh_cpu_usage();
    }

    #[allow(dead_code)] // Kept as fallback method when sysinfo is unavailable
    fn get_cpu_utilization_iostat(&self) -> Result<f64, Box<dyn std::error::Error>> {
        // Fallback method using iostat (kept for compatibility)
        let output = Command::new("iostat").args(["-c", "1"]).output()?;

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

    fn get_apple_silicon_core_utilization(&self) -> Result<(f64, f64), Box<dyn std::error::Error>> {
        // OPTIMIZATION: This is a fallback method only called when per_core_utilization is empty
        // Don't refresh CPU here since caller (get_apple_silicon_cpu_info) already did via ensure_cpu_refreshed()
        let total_cpu_util = self.system.read().unwrap().global_cpu_usage() as f64;

        // Use native metrics manager for cluster residency (cached, no extra collection)
        if let Some(manager) = get_native_metrics_manager()
            && let Ok(data) = manager.collect_once()
        {
            // Use cluster residency from native metrics
            let p_residency = data.p_cluster_active_residency.clamp(0.0, 100.0);
            let e_residency = data.e_cluster_active_residency.clamp(0.0, 100.0);
            let total_residency = p_residency + e_residency;

            if total_residency > 0.0 {
                let p_ratio = p_residency / total_residency;
                let e_ratio = e_residency / total_residency;
                return Ok((
                    (total_cpu_util * p_ratio * 1.2).clamp(0.0, 100.0),
                    (total_cpu_util * e_ratio * 0.8).clamp(0.0, 100.0),
                ));
            }
        }

        // Return default values if manager is not available
        // Split total utilization evenly
        Ok((total_cpu_util * 0.6, total_cpu_util * 0.4))
    }

    fn get_cpu_base_frequency(&self) -> Result<u32, Box<dyn std::error::Error>> {
        if self.is_apple_silicon {
            // Apple Silicon base frequencies are not easily available
            // Return typical values based on chip
            Ok(3000) // 3 GHz as default
        } else {
            // Try to get from system_profiler (already parsed in get_intel_mac_cpu_info)
            Ok(2400) // Default fallback
        }
    }

    fn get_cpu_max_frequency(&self) -> Result<u32, Box<dyn std::error::Error>> {
        if self.is_apple_silicon {
            // Apple Silicon max frequencies vary by core type
            Ok(3500) // Typical P-core max frequency
        } else {
            Ok(3000) // Default for Intel Macs
        }
    }

    fn get_cpu_temperature(&self) -> Option<u32> {
        // Temperature monitoring on macOS requires specialized tools
        // This is a placeholder - actual implementation might use external tools
        None
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
    /// For Apple Silicon: E-cores are indexed first (0 to e_core_count-1),
    /// then P-cores (e_core_count to total-1)
    #[allow(dead_code)] // Kept for potential future use when refresh is needed
    fn get_per_core_utilization(
        &self,
        e_core_count: usize,
        p_core_count: usize,
    ) -> Vec<CoreUtilization> {
        // Refresh CPU usage to get latest data
        self.system.write().unwrap().refresh_cpu_usage();
        self.get_per_core_utilization_no_refresh(e_core_count, p_core_count)
    }

    /// Get per-core CPU utilization WITHOUT refreshing CPU data
    /// OPTIMIZATION: Used when CPU has already been refreshed via ensure_cpu_refreshed()
    /// For Apple Silicon: E-cores are indexed first (0 to e_core_count-1),
    /// then P-cores (e_core_count to total-1)
    fn get_per_core_utilization_no_refresh(
        &self,
        e_core_count: usize,
        p_core_count: usize,
    ) -> Vec<CoreUtilization> {
        let mut per_core_utilization = Vec::new();

        let system = self.system.read().unwrap();
        let cpus = system.cpus();

        for (core_id, cpu) in cpus.iter().enumerate() {
            let utilization = cpu.cpu_usage() as f64;

            // Determine core type based on index
            // Apple Silicon layout: E-cores first (indices 0 to e_core_count-1),
            // then P-cores (indices e_core_count to total-1)
            let core_type = if self.is_apple_silicon {
                if core_id < e_core_count {
                    CoreType::Efficiency
                } else if core_id < e_core_count + p_core_count {
                    CoreType::Performance
                } else {
                    CoreType::Standard // Fallback for any extra cores
                }
            } else {
                CoreType::Standard // Intel Macs use standard cores
            };

            per_core_utilization.push(CoreUtilization {
                core_id: core_id as u32,
                core_type,
                utilization,
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
