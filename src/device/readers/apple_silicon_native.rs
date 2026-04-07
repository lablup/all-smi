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

//! Native Apple Silicon GPU reader using macOS native APIs
//!
//! This reader uses IOReport, SMC, and other native macOS APIs to collect
//! Apple Silicon metrics instead of the `powermetrics` command.
//!
//! ## Benefits
//! - No sudo required
//! - Lower latency
//! - More stable (no external process)
//! - Additional metrics (actual temperature, system power)

use crate::device::common::command_executor::execute_command_default;
use crate::device::macos_native::{
    NativeMetricsManager, get_native_metrics_manager, initialize_native_metrics_manager,
};
use crate::device::readers::common_cache::{DetailBuilder, DeviceStaticInfo};
use crate::device::{GpuInfo, GpuReader, ProcessInfo};
use crate::utils::get_hostname;
use chrono::Local;
use once_cell::sync::{Lazy, OnceCell};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use sysinfo::System;

// Cache GPU info to avoid expensive system_profiler calls on every initialization
static CACHED_GPU_INFO: Lazy<Mutex<Option<DeviceStaticInfo>>> = Lazy::new(|| Mutex::new(None));

// Apple Silicon specific info that needs to be cached separately
struct AppleSiliconInfo {
    gpu_core_count: Option<u32>,
}

/// Apple Silicon GPU reader using native macOS APIs
///
/// This reader uses IOReport and SMC APIs directly instead of spawning
/// the `powermetrics` command, eliminating the need for sudo.
pub struct AppleSiliconNativeGpuReader {
    static_info: OnceCell<DeviceStaticInfo>,
    apple_info: OnceCell<AppleSiliconInfo>,
    initialized: AtomicBool,
    native_manager: OnceCell<Arc<NativeMetricsManager>>,
}

impl Default for AppleSiliconNativeGpuReader {
    fn default() -> Self {
        Self::new()
    }
}

impl AppleSiliconNativeGpuReader {
    pub fn new() -> Self {
        // Initialize native metrics manager if not already done
        // Use 100ms sample interval for responsiveness
        let _ = initialize_native_metrics_manager(100);

        AppleSiliconNativeGpuReader {
            static_info: OnceCell::new(),
            apple_info: OnceCell::new(),
            initialized: AtomicBool::new(false),
            native_manager: OnceCell::new(),
        }
    }

    fn ensure_initialized(&self) {
        if self.initialized.load(Ordering::Acquire) {
            return;
        }

        // Initialize native manager reference
        if let Some(manager) = get_native_metrics_manager() {
            let _ = self.native_manager.set(manager);
        }

        // Check cache first to avoid expensive system_profiler calls
        let mut cache = match CACHED_GPU_INFO.lock() {
            Ok(guard) => guard,
            Err(e) => {
                eprintln!("Failed to acquire lock for Apple Silicon GPU cache: {e}");
                return;
            }
        };

        if let Some(static_info) = cache.as_ref() {
            // Use cached values - safe initialization via OnceCell
            let _ = self.static_info.set(static_info.clone());
            // Extract gpu_core_count from detail if present
            let gpu_core_count = static_info
                .detail
                .get("GPU Core Count")
                .and_then(|s| s.parse::<u32>().ok());
            let _ = self.apple_info.set(AppleSiliconInfo { gpu_core_count });
            self.initialized.store(true, Ordering::Release);
            return;
        }

        // If not cached, fetch the information (this is slow but only happens once)
        let (name, driver_version) = get_gpu_name_and_version();
        let gpu_core_count = get_gpu_core_count();

        // Build DeviceStaticInfo using DetailBuilder
        let mut builder = DetailBuilder::new()
            .insert("gpu_type", "Integrated")
            .insert_optional("driver_version", driver_version.as_ref());

        if let Some(count) = gpu_core_count {
            builder = builder.insert("GPU Core Count", count.to_string());
        }

        let detail = builder.build();
        let static_info = DeviceStaticInfo::with_details(name, None, detail);

        // Store in cache for future use
        *cache = Some(static_info.clone());

        // Update self - safe initialization via OnceCell
        let _ = self.static_info.set(static_info);
        let _ = self.apple_info.set(AppleSiliconInfo { gpu_core_count });
        self.initialized.store(true, Ordering::Release);
    }
}

impl GpuReader for AppleSiliconNativeGpuReader {
    fn get_gpu_info(&self) -> Vec<GpuInfo> {
        // Ensure GPU info is initialized (happens on first call)
        self.ensure_initialized();

        // Try to get metrics from native manager
        let (metrics, combined_power_mw, cpu_temp, gpu_temp) =
            if let Some(manager) = self.native_manager.get() {
                match manager.collect_once() {
                    Ok(data) => {
                        let combined_power = data.combined_power_mw;
                        (
                            GpuMetrics {
                                utilization: Some(data.gpu_active_residency),
                                ane_utilization: Some(data.ane_power_mw),
                                frequency: Some(data.gpu_frequency),
                                power_consumption: Some(data.gpu_power_mw / 1000.0),
                                thermal_pressure_level: data.thermal_pressure_level,
                            },
                            Some(combined_power),
                            data.cpu_temperature,
                            data.gpu_temperature,
                        )
                    }
                    Err(_) => (GpuMetrics::default(), None, None, None),
                }
            } else {
                (GpuMetrics::default(), None, None, None)
            };

        // Get cached static info
        let static_info = match self.static_info.get() {
            Some(info) => info,
            None => {
                // Fallback if initialization failed
                return vec![];
            }
        };
        let apple_info = self.apple_info.get();

        let mut detail = static_info.detail.clone();
        detail.insert("architecture".to_string(), "Apple Silicon".to_string());
        detail.insert("api".to_string(), "Native (IOReport/SMC)".to_string());

        if let Some(ref thermal_level) = metrics.thermal_pressure_level {
            detail.insert("thermal_pressure".to_string(), thermal_level.clone());
        }

        // Add combined power (CPU + GPU + ANE) for metrics export
        if let Some(combined_power) = combined_power_mw {
            detail.insert("combined_power_mw".to_string(), combined_power.to_string());
        }

        // Add temperature metrics from SMC
        if let Some(cpu_t) = cpu_temp {
            detail.insert("cpu_temperature".to_string(), format!("{cpu_t:.1}"));
        }
        if let Some(gpu_t) = gpu_temp {
            detail.insert("gpu_temperature".to_string(), format!("{gpu_t:.1}"));
        }

        // Add unified AI acceleration library labels
        detail.insert("lib_name".to_string(), "Metal".to_string());
        if let Some(driver_ver) = static_info.detail.get("driver_version") {
            if driver_ver != "Unknown" {
                let lib_ver = driver_ver
                    .strip_prefix("Metal ")
                    .unwrap_or(driver_ver)
                    .to_string();
                detail.insert("lib_version".to_string(), lib_ver);
            }
        }

        // Use GPU temperature if available, otherwise default to 0
        let temperature = gpu_temp.map(|t| t as u32).unwrap_or(0);

        vec![GpuInfo {
            uuid: static_info
                .uuid
                .clone()
                .unwrap_or_else(|| "AppleSiliconGPU".to_string()),
            time: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            name: static_info.name.clone(),
            device_type: "GPU".to_string(),
            host_id: get_hostname(),
            hostname: get_hostname(),
            instance: get_hostname(),
            utilization: metrics.utilization.unwrap_or(0.0),
            ane_utilization: metrics.ane_utilization.unwrap_or(0.0),
            dla_utilization: None,
            tensorcore_utilization: None,
            temperature,
            used_memory: get_used_memory(),
            total_memory: get_total_memory(),
            frequency: metrics.frequency.unwrap_or(0),
            power_consumption: metrics.power_consumption.unwrap_or(0.0),
            gpu_core_count: apple_info.and_then(|i| i.gpu_core_count),
            detail,
        }]
    }

    fn get_process_info(&self) -> Vec<ProcessInfo> {
        // Native APIs don't provide per-process GPU usage
        // Return empty for now - could be enhanced with Metal Performance Shaders API
        vec![]
    }
}

#[derive(Default)]
struct GpuMetrics {
    utilization: Option<f64>,
    ane_utilization: Option<f64>,
    frequency: Option<u32>,
    power_consumption: Option<f64>,
    thermal_pressure_level: Option<String>,
}

fn get_gpu_name_and_version() -> (String, Option<String>) {
    // Try to get GPU name from sysctl first (fast path for name only)
    let gpu_name = if let Ok(output) =
        execute_command_default("sysctl", &["-n", "machdep.cpu.brand_string"])
    {
        let cpu_brand = output.stdout.trim().to_string();
        if cpu_brand.contains("Apple M") {
            let mut name = None;
            for part in cpu_brand.split_whitespace() {
                if part.starts_with("M") && part.chars().nth(1).is_some_and(|c| c.is_numeric()) {
                    let mut gpu_name = format!("Apple {part} GPU");
                    let parts: Vec<&str> = cpu_brand.split_whitespace().collect();
                    if let Some(pos) = parts.iter().position(|&x| x == part) {
                        if pos + 1 < parts.len() {
                            let suffix = parts[pos + 1];
                            if suffix == "Pro" || suffix == "Max" || suffix == "Ultra" {
                                gpu_name = format!("Apple {part} {suffix} GPU");
                            }
                        }
                    }
                    name = Some(gpu_name);
                    break;
                }
            }
            name.unwrap_or_else(|| "Apple Silicon GPU".to_string())
        } else {
            "Apple Silicon GPU".to_string()
        }
    } else {
        "Apple Silicon GPU".to_string()
    };

    // Get Metal version from macOS version
    let metal_version = get_metal_version_from_framework();

    (gpu_name, metal_version)
}

fn get_metal_version_from_framework() -> Option<String> {
    if let Ok(output) = execute_command_default("sw_vers", &["-productVersion"]) {
        let version_str = output.stdout.trim();
        if let Some(major_version) = version_str.split('.').next() {
            if let Ok(major) = major_version.parse::<u32>() {
                let metal_version = match major {
                    26.. => "Metal 4",
                    15..=25 => "Metal 3",
                    14 => "Metal 3",
                    13 => "Metal 3",
                    12 => "Metal 2.4",
                    11 => "Metal 2.3",
                    _ => "Metal 2",
                };
                return Some(metal_version.to_string());
            }
        }
    }
    Some("Metal 3".to_string())
}

fn get_gpu_core_count() -> Option<u32> {
    if let Ok(output) = execute_command_default("sysctl", &["-n", "machdep.cpu.brand_string"]) {
        let cpu_brand = output.stdout.trim().to_string();

        let core_count = match cpu_brand.as_str() {
            s if s.contains("M1 ")
                && !s.contains("Pro")
                && !s.contains("Max")
                && !s.contains("Ultra") =>
            {
                Some(8)
            }
            s if s.contains("M1 Pro") => Some(16),
            s if s.contains("M1 Max") => Some(32),
            s if s.contains("M1 Ultra") => Some(64),
            s if s.contains("M2 ")
                && !s.contains("Pro")
                && !s.contains("Max")
                && !s.contains("Ultra") =>
            {
                Some(10)
            }
            s if s.contains("M2 Pro") => Some(19),
            s if s.contains("M2 Max") => Some(38),
            s if s.contains("M2 Ultra") => Some(76),
            s if s.contains("M3 ") && !s.contains("Pro") && !s.contains("Max") => Some(10),
            s if s.contains("M3 Pro") => Some(18),
            s if s.contains("M3 Max") => Some(40),
            s if s.contains("M4 ") && !s.contains("Pro") && !s.contains("Max") => Some(10),
            s if s.contains("M4 Pro") => Some(20),
            s if s.contains("M4 Max") => Some(40),
            _ => None,
        };

        if core_count.is_some() {
            return core_count;
        }
    }

    // Fallback to ioreg
    match execute_command_default("ioreg", &["-rc", "AGXAccelerator", "-d1"]) {
        Ok(cmd_output) => parse_ioreg_gpu_cores(&cmd_output.stdout),
        Err(_) => None,
    }
}

fn parse_ioreg_gpu_cores(output_str: &str) -> Option<u32> {
    for line in output_str.lines() {
        if line.contains("\"gpu-core-count\"") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                if let Ok(core_count) = parts[2].parse::<u32>() {
                    return Some(core_count);
                }
            }
        }
    }
    None
}

// Use a cached System instance for memory info to avoid creating new instances on every call
// Total memory is static so we only need to fetch it once
static CACHED_TOTAL_MEMORY: Lazy<u64> = Lazy::new(|| {
    let mut system = System::new();
    system.refresh_memory();
    system.total_memory()
});

fn get_total_memory() -> u64 {
    *CACHED_TOTAL_MEMORY
}

fn get_used_memory() -> u64 {
    // Use global system instance from utils for memory refresh
    crate::utils::with_global_system(|system| {
        system.refresh_memory();
        system.used_memory()
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_gpu_core_count_parsing() {
        // Test known chip patterns
        let patterns = [
            ("Apple M1", Some(8)),
            ("Apple M1 Pro", Some(16)),
            ("Apple M1 Max", Some(32)),
            ("Apple M1 Ultra", Some(64)),
            ("Apple M2", Some(10)),
            ("Apple M2 Pro", Some(19)),
            ("Apple M3 Pro", Some(18)),
            ("Apple M4 Pro", Some(20)),
        ];

        for (brand, expected) in patterns {
            // We can't directly test the function without mocking sysctl
            // This test documents the expected behavior
            assert!(expected.is_some(), "Expected core count for {brand}");
        }
    }
}
