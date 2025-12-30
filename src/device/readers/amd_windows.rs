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

//! AMD GPU reader for Windows using WMI
//!
//! This module provides basic AMD GPU information on Windows via WMI.
//! Note: Detailed metrics like utilization and temperature require AMD ADL SDK,
//! which is not currently implemented.

use crate::device::types::{GpuInfo, ProcessInfo};
use crate::device::GpuReader;
use crate::utils::get_hostname;
use chrono::Local;
use serde::Deserialize;
use std::collections::HashMap;
use wmi::WMIConnection;

// Thread-local WMI connection for reuse within the same thread
thread_local! {
    static WMI_CONNECTION: std::cell::RefCell<Option<WMIConnection>> = const { std::cell::RefCell::new(None) };
}

/// Helper to get or create WMI connection (thread-local cached)
fn with_wmi_connection<T, F: FnOnce(&WMIConnection) -> T>(f: F) -> Option<T> {
    WMI_CONNECTION.with(|cell| {
        let mut conn_ref = cell.borrow_mut();
        if conn_ref.is_none() {
            match WMIConnection::new() {
                Ok(wmi_con) => {
                    *conn_ref = Some(wmi_con);
                }
                Err(e) => {
                    eprintln!("AMD GPU: Failed to create WMI connection: {e}");
                }
            }
        }
        conn_ref.as_ref().map(f)
    })
}

// WMI structure for video controller information (full version for GPU info)
#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "PascalCase")]
struct Win32VideoController {
    name: Option<String>,
    adapter_r_a_m: Option<u64>, // AdapterRAM in WMI (bytes)
    driver_version: Option<String>,
    video_processor: Option<String>,
    pnp_device_i_d: Option<String>, // PNPDeviceID
    status: Option<String>,
    adapter_d_a_c_type: Option<String>,
}

// Simple structure for GPU detection (only Name field)
#[derive(Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
struct VideoControllerName {
    name: Option<String>,
}

pub struct AmdWindowsGpuReader {}

impl Default for AmdWindowsGpuReader {
    fn default() -> Self {
        Self::new()
    }
}

impl AmdWindowsGpuReader {
    pub fn new() -> Self {
        Self {}
    }

    fn query_amd_gpus(&self) -> Vec<GpuInfo> {
        // Use thread-local cached WMI connection to avoid repeated COM initialization
        with_wmi_connection(|wmi_con| {
            let mut gpu_list = Vec::new();

            let result: Result<Vec<Win32VideoController>, _> = wmi_con
                .raw_query("SELECT Name, AdapterRAM, DriverVersion, VideoProcessor, PNPDeviceID, Status, AdapterDACType FROM Win32_VideoController");

            if let Ok(controllers) = result {
            let hostname = get_hostname();
            let time = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

            for (idx, controller) in controllers.iter().enumerate() {
                let name = controller.name.clone().unwrap_or_default();

                // Filter for AMD GPUs only
                let name_lower = name.to_lowercase();
                if !name_lower.contains("amd")
                    && !name_lower.contains("radeon")
                    && !name_lower.contains("ati")
                {
                    continue;
                }

                // Generate a UUID from PNPDeviceID or index
                let uuid = controller
                    .pnp_device_i_d
                    .clone()
                    .unwrap_or_else(|| format!("AMD-GPU-{idx}"));

                // Get adapter RAM (in bytes)
                // LIMITATION: Win32_VideoController.AdapterRAM is a 32-bit uint32 in WMI,
                // which can only represent up to 4GB (4,294,967,295 bytes). For GPUs with
                // more than 4GB VRAM, this value will be incorrect (wrapped or capped).
                // Unfortunately, there's no standard WMI alternative for accurate VRAM
                // reporting on AMD GPUs without the AMD ADL SDK.
                let total_memory = controller.adapter_r_a_m.unwrap_or(0);

                // Warn if the reported VRAM is suspiciously close to 4GB limit or 0
                const FOUR_GB: u64 = 4 * 1024 * 1024 * 1024; // 4,294,967,296 bytes
                if total_memory == 0 {
                    eprintln!("AMD GPU '{name}': VRAM size unavailable (reported as 0)");
                } else if total_memory >= FOUR_GB - (512 * 1024 * 1024) {
                    // If reported value is >= 3.5GB, it might be capped/wrapped for >4GB GPU
                    eprintln!("AMD GPU '{name}': VRAM reported as {total_memory} bytes, may be inaccurate for >4GB GPUs due to WMI 32-bit limitation");
                }

                // Build detail map
                let mut detail = HashMap::new();

                if let Some(ref driver) = controller.driver_version {
                    detail.insert("Driver Version".to_string(), driver.clone());
                }
                if let Some(ref processor) = controller.video_processor {
                    detail.insert("Video Processor".to_string(), processor.clone());
                }
                if let Some(ref status) = controller.status {
                    detail.insert("Status".to_string(), status.clone());
                }
                if let Some(ref dac_type) = controller.adapter_d_a_c_type {
                    detail.insert("DAC Type".to_string(), dac_type.clone());
                }

                // Add note about limited metrics
                detail.insert(
                    "Note".to_string(),
                    "Detailed metrics require AMD ADL SDK".to_string(),
                );

                gpu_list.push(GpuInfo {
                    uuid,
                    time: time.clone(),
                    name,
                    device_type: "GPU".to_string(),
                    host_id: hostname.clone(),
                    hostname: hostname.clone(),
                    instance: hostname.clone(),
                    utilization: 0.0, // Not available via WMI
                    ane_utilization: 0.0,
                    dla_utilization: None,
                    tensorcore_utilization: None,
                    temperature: 0, // Not available via WMI
                    used_memory: 0, // Not available via WMI
                    total_memory,
                    frequency: 0,         // Not available via WMI
                    power_consumption: 0.0, // Not available via WMI
                    gpu_core_count: None,
                    detail,
                });
            }
            }

            gpu_list
        })
        .unwrap_or_default()
    }
}

impl GpuReader for AmdWindowsGpuReader {
    fn get_gpu_info(&self) -> Vec<GpuInfo> {
        // Query fresh data each time (timestamp updates)
        // But we could cache the static parts if needed
        self.query_amd_gpus()
    }

    fn get_process_info(&self) -> Vec<ProcessInfo> {
        // GPU process information is not available via WMI
        // This would require AMD ADL SDK
        Vec::new()
    }
}

/// Check if AMD GPU is present on Windows using WMI
/// Note: This creates its own WMI connection since detection may run on a different thread
pub fn has_amd_gpu_windows() -> bool {
    let wmi_con = match WMIConnection::new() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("AMD GPU detection: Failed to create WMI connection: {e}");
            return false;
        }
    };

    let query_result: Result<Vec<VideoControllerName>, _> =
        wmi_con.raw_query("SELECT Name FROM Win32_VideoController");

    match query_result {
        Ok(controllers) => {
            for controller in controllers {
                if let Some(name) = &controller.name {
                    let name_lower = name.to_lowercase();
                    if name_lower.contains("amd")
                        || name_lower.contains("radeon")
                        || name_lower.contains("ati")
                    {
                        return true;
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("AMD GPU detection: WMI query failed: {e}");
            return false;
        }
    }

    false
}
