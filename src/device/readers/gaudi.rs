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

use crate::device::GpuReader;
#[cfg(target_os = "linux")]
use crate::device::hlsmi::parser::{GaudiDeviceMetrics, map_device_name};
#[cfg(target_os = "linux")]
use crate::device::readers::common_cache::{DetailBuilder, DeviceStaticInfo};
use crate::device::types::{GpuInfo, ProcessInfo};
#[cfg(target_os = "linux")]
use crate::utils::get_hostname;
#[cfg(target_os = "linux")]
use chrono::Local;
#[cfg(target_os = "linux")]
use once_cell::sync::Lazy;
#[cfg(target_os = "linux")]
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::path::Path;
#[cfg(target_os = "linux")]
use std::sync::{Arc, Mutex, OnceLock};

#[cfg(target_os = "linux")]
use crate::device::hlsmi;

/// Cache for hl-smi command path
#[cfg(target_os = "linux")]
static HLSMI_COMMAND_AVAILABLE: Lazy<Arc<Mutex<Option<bool>>>> =
    Lazy::new(|| Arc::new(Mutex::new(None)));

pub struct GaudiNpuReader {
    /// Cached static device information per UUID
    #[cfg(target_os = "linux")]
    device_static_info: OnceLock<HashMap<String, DeviceStaticInfo>>,
}

impl Default for GaudiNpuReader {
    fn default() -> Self {
        Self::new()
    }
}

impl GaudiNpuReader {
    pub fn new() -> Self {
        Self {
            #[cfg(target_os = "linux")]
            device_static_info: OnceLock::new(),
        }
    }

    /// Initialize static device cache on first access
    #[cfg(target_os = "linux")]
    fn ensure_static_cache_initialized(&self, devices: &[GaudiDeviceMetrics]) {
        self.device_static_info.get_or_init(|| {
            let mut device_map = HashMap::new();
            // Use common MAX_DEVICES constant from common_cache module
            const MAX_DEVICES: usize = crate::device::readers::common_cache::MAX_DEVICES;
            let devices_to_process: Vec<_> = devices.iter().take(MAX_DEVICES).collect();

            for device in devices_to_process {
                // Map device name to human-friendly name
                let friendly_name = map_device_name(&device.name);

                // Build detail HashMap using DetailBuilder
                let detail = DetailBuilder::new()
                    .insert("Device Index", device.index.to_string())
                    .insert("Internal Name", &device.name) // Keep original name
                    .insert("Max Power", format!("{} W", device.power_max))
                    .insert("Total Memory", format!("{} MiB", device.memory_total))
                    .build();

                let static_info = DeviceStaticInfo::with_details(
                    friendly_name,
                    Some(device.uuid.clone()),
                    detail,
                );

                device_map.insert(device.uuid.clone(), static_info);
            }
            device_map
        });
    }

    /// Get cached static device info
    #[cfg(target_os = "linux")]
    fn get_device_static_info(&self, uuid: &str) -> Option<&DeviceStaticInfo> {
        self.device_static_info.get().and_then(|map| map.get(uuid))
    }

    /// Check if hl-smi command is available
    #[cfg(target_os = "linux")]
    fn is_hlsmi_available() -> bool {
        // Check cache first
        if let Ok(cache) = HLSMI_COMMAND_AVAILABLE.lock()
            && let Some(available) = *cache
        {
            return available;
        }

        // Check specific paths first
        const PATHS: &[&str] = &[
            "/usr/bin/hl-smi",
            "/usr/local/bin/hl-smi",
            "/opt/habanalabs/bin/hl-smi",
        ];

        for path in PATHS {
            if Path::new(path).exists() {
                // Cache the result
                if let Ok(mut cache) = HLSMI_COMMAND_AVAILABLE.lock() {
                    *cache = Some(true);
                }
                return true;
            }
        }

        // Check if command is available in PATH
        if let Ok(output) = std::process::Command::new("which").arg("hl-smi").output()
            && output.status.success()
        {
            // Cache the result
            if let Ok(mut cache) = HLSMI_COMMAND_AVAILABLE.lock() {
                *cache = Some(true);
            }
            return true;
        }

        // Cache negative result
        if let Ok(mut cache) = HLSMI_COMMAND_AVAILABLE.lock() {
            *cache = Some(false);
        }
        false
    }

    /// Get NPU info using the hl-smi manager
    #[cfg(target_os = "linux")]
    fn get_npu_info_internal(&self) -> Vec<GpuInfo> {
        // Check if hl-smi is available
        if !Self::is_hlsmi_available() {
            return Vec::new();
        }

        // Get data from the manager
        let manager = match hlsmi::get_hlsmi_manager() {
            Some(m) => m,
            None => return Vec::new(),
        };

        let metrics_data = match manager.get_latest_data_result() {
            Ok(data) => data,
            Err(_) => return Vec::new(),
        };

        // Initialize static cache on first call
        self.ensure_static_cache_initialized(&metrics_data.devices);

        let time = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let hostname = get_hostname();

        metrics_data
            .devices
            .into_iter()
            .filter_map(|device| {
                let static_info = self.get_device_static_info(&device.uuid);
                create_gpu_info_from_device(device, static_info, &time, &hostname)
            })
            .collect()
    }
}

impl GpuReader for GaudiNpuReader {
    fn get_gpu_info(&self) -> Vec<GpuInfo> {
        #[cfg(target_os = "linux")]
        {
            self.get_npu_info_internal()
        }
        #[cfg(not(target_os = "linux"))]
        {
            Vec::new()
        }
    }

    fn get_process_info(&self) -> Vec<ProcessInfo> {
        // Intel Gaudi hl-smi doesn't provide process information in the same way
        // This would require additional integration with Habana tools
        Vec::new()
    }
}

// Helper functions

#[cfg(target_os = "linux")]
fn create_gpu_info_from_device(
    device: GaudiDeviceMetrics,
    static_info: Option<&DeviceStaticInfo>,
    time: &str,
    hostname: &str,
) -> Option<GpuInfo> {
    // Map device name to human-friendly name (e.g., HL-325L -> Intel Gaudi 3)
    let friendly_name = map_device_name(&device.name);

    // Use cached static info if available, otherwise build from current device data
    let (uuid, _name, mut detail) = if let Some(info) = static_info {
        (
            info.uuid.clone().unwrap_or_else(|| device.uuid.clone()),
            info.name.clone(),
            info.detail.clone(),
        )
    } else {
        // Build detail HashMap if no cache available (first call)
        let detail = DetailBuilder::new()
            .insert("Device Index", device.index.to_string())
            .insert("Internal Name", &device.name) // Keep original name in details
            .insert("Max Power", format!("{} W", device.power_max))
            .insert("Total Memory", format!("{} MiB", device.memory_total))
            .build();

        (device.uuid.clone(), friendly_name.clone(), detail)
    };

    // Add unified AI acceleration library labels
    detail.insert("lib_name".to_string(), "Habana".to_string());
    detail.insert("lib_version".to_string(), device.driver_version.clone());

    // Dynamic values
    detail.insert(
        "Current Power".to_string(),
        format!("{} W", device.power_draw),
    );
    detail.insert(
        "Used Memory".to_string(),
        format!("{} MiB", device.memory_used),
    );
    detail.insert(
        "Free Memory".to_string(),
        format!("{} MiB", device.memory_free),
    );

    // Add power limit max for display
    detail.insert("power_limit_max".to_string(), device.power_max.to_string());

    // Convert memory from MiB to bytes
    let total_memory = device.memory_total * 1024 * 1024;
    let used_memory = device.memory_used * 1024 * 1024;

    Some(GpuInfo {
        uuid,
        time: time.to_string(),
        name: friendly_name,
        device_type: "NPU".to_string(),
        host_id: hostname.to_string(),
        hostname: hostname.to_string(),
        instance: hostname.to_string(),
        utilization: device.utilization,
        ane_utilization: 0.0,
        dla_utilization: None,
        tensorcore_utilization: None,
        temperature: device.temperature,
        used_memory,
        total_memory,
        frequency: 0, // Intel Gaudi doesn't report frequency via hl-smi CSV
        power_consumption: device.power_draw,
        gpu_core_count: None,
        detail,
    })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn test_reader_creation() {
        let reader = GaudiNpuReader::new();
        // Just verify we can create the reader
        let _ = reader.get_gpu_info();
    }

    #[test]
    fn test_is_hlsmi_available() {
        // This will check actual system availability
        let _ = GaudiNpuReader::is_hlsmi_available();
        // Test passes if no panic occurs
    }
}
