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

//! AMD GPU reader for Windows.
//!
//! `Win32_VideoController` supplies the card's identity (name, PCI ids,
//! driver version) and nothing else of substance: no utilization, no
//! temperature, and a `uint32` `AdapterRAM` field that saturates at
//! 4 GB. The vendor-neutral [`windows_gpu_perf`] layer is applied on top
//! to fill in the true VRAM size (DXGI), device utilization and
//! system-wide used VRAM (PDH), and per-process GPU memory.
//!
//! Temperature, power, fan speed, and clocks are still absent here:
//! WDDM does not publish them, and they need AMD's own ADL library.

use crate::device::GpuReader;
use crate::device::readers::windows_gpu_perf::{self, ids::AdapterLuid};
use crate::device::types::{GpuInfo, ProcessInfo};
use crate::utils::get_hostname;
use chrono::Local;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Mutex;
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

pub struct AmdWindowsGpuReader {
    /// Adapter LUID to `(device index, GPU uuid)`, recorded by
    /// `get_gpu_info` so `get_process_info` can attribute a PDH
    /// per-process row to the right card without re-querying WMI or
    /// consuming a second PDH collection.
    ///
    /// Empty until the first `get_gpu_info` call, so a
    /// `get_process_info` that runs first simply reports nothing and the
    /// next poll fills it in.
    adapter_index: Mutex<HashMap<AdapterLuid, (usize, String)>>,
}

impl Default for AmdWindowsGpuReader {
    fn default() -> Self {
        Self::new()
    }
}

impl AmdWindowsGpuReader {
    pub fn new() -> Self {
        Self {
            adapter_index: Mutex::new(HashMap::new()),
        }
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

                // Baseline VRAM size. `Win32_VideoController.AdapterRAM`
                // is a `uint32` in the WMI schema, so anything above 4 GB
                // arrives saturated or wrapped. This value is only a
                // fallback: `windows_gpu_perf` overwrites it with DXGI's
                // 64-bit `DedicatedVideoMemory` whenever DXGI answers,
                // which is every machine with a working display driver.
                let total_memory = controller.adapter_r_a_m.unwrap_or(0);

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

                // `Metrics Source` advertises which backends produced
                // this card's numbers; `windows_gpu_perf` appends to it
                // as DXGI and PDH contribute. The per-field `Source: *`
                // keys mirror the Intel Windows reader so both vendors
                // expose provenance the same way.
                detail.insert("Metrics Source".to_string(), "WMI".to_string());
                detail.insert(
                    "Note".to_string(),
                    "Temperature, power, and fan need the AMD ADL library".to_string(),
                );
                detail.insert("Source: Utilization".to_string(), "unavailable".to_string());
                detail.insert("Source: Temperature".to_string(), "unavailable".to_string());
                detail.insert("Source: Power".to_string(), "unavailable".to_string());
                detail.insert("Source: Frequency".to_string(), "unavailable".to_string());
                detail.insert("Source: Fan".to_string(), "unavailable".to_string());
                detail.insert(
                    "Source: Memory".to_string(),
                    if total_memory > 0 { "WMI" } else { "unavailable" }.to_string(),
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
                    // AMD-on-Windows surfaces nothing beyond the basic WMI
                    // query — NVML thermal thresholds / P-states and NVIDIA
                    // hardware details (NUMA, GSP firmware, NvLink, GPM) do
                    // not apply.
                    temperature_threshold_slowdown: None,
                    temperature_threshold_shutdown: None,
                    temperature_threshold_max_operating: None,
                    temperature_threshold_acoustic: None,
                    performance_state: None,
                    fan_speed_rpm: None,
                    numa_node_id: None,
                    gsp_firmware_mode: None,
                    gsp_firmware_version: None,
                    nvlink_remote_devices: Vec::new(),
                    gpm_metrics: None,
                    detail,
                });
            }
            }

            gpu_list
        })
        .unwrap_or_default()
    }

    /// Layer the vendor-neutral DXGI and PDH metrics onto the WMI
    /// baseline, and record the LUID mapping `get_process_info` needs.
    fn augment_with_windows_perf(&self, gpus: &mut [GpuInfo]) {
        let adapter_index = windows_gpu_perf::augment_gpus(gpus);
        if let Ok(mut guard) = self.adapter_index.lock() {
            *guard = adapter_index;
        }
    }
}

impl GpuReader for AmdWindowsGpuReader {
    fn get_gpu_info(&self) -> Vec<GpuInfo> {
        // Query fresh data each time (timestamp updates)
        // But we could cache the static parts if needed
        let mut gpus = self.query_amd_gpus();
        self.augment_with_windows_perf(&mut gpus);
        // ADL last: it reads the hardware's own telemetry rather than
        // the OS's accounting of it, so where both produce a figure the
        // vendor number wins. It is also the only source for
        // temperature, power, fan, and clocks.
        crate::device::readers::amd_adl::augment(&mut gpus);
        gpus
    }

    fn get_process_info(&self) -> Vec<ProcessInfo> {
        // Per-process GPU memory comes from the PDH `GPU Process
        // Memory` counter, reusing the sample `get_gpu_info` already
        // took. The closure covers one-shot callers that never call
        // `get_gpu_info` at all, such as
        // `all-smi snapshot --include process`.
        let Ok(adapter_index) = self.adapter_index.lock() else {
            return Vec::new();
        };
        windows_gpu_perf::process_rows_with(&adapter_index, || {
            let mut gpus = self.query_amd_gpus();
            windows_gpu_perf::augment_gpus(&mut gpus)
        })
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
