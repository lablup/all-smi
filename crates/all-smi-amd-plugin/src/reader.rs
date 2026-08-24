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

use all_smi::device::GpuReader;
use all_smi::device::readers::common_cache::{DetailBuilder, DeviceStaticInfo};
use all_smi::device::types::{GpuInfo, MAX_GPU_FAN_RPM, ProcessInfo};
use all_smi::utils::get_hostname;
use chrono::Local;
use libamdgpu_top::AMDGPU::{DeviceHandle, GPU_INFO, GpuMetrics, MetricsInfo};
use libamdgpu_top::stat::{self, FdInfoStat, ProcInfo};
use libamdgpu_top::{AppDeviceInfo, DevicePath, VramUsage};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

// GPU metric validation constants
const MAX_GPU_UTILIZATION: f64 = 100.0; // Maximum utilization percentage
const MAX_GPU_POWER_WATTS: f64 = 1000.0; // Maximum power consumption in watts
const MAX_GPU_TEMP_CELSIUS: u32 = 125; // Maximum temperature in Celsius
const MAX_GPU_FREQ_MHZ: u32 = 5000; // Maximum frequency in MHz
const MAX_GPU_MEMORY_BYTES: u64 = 512 * 1024 * 1024 * 1024; // 512GB max memory

// Driver version validation constant
// Linux kernel versions typically don't exceed 999 for any component
const MAX_VERSION_COMPONENT: i32 = 999;

/// Cap a raw `sensors.fan_rpm` reading so a garbled `libamdgpu_top` sample
/// can never propagate `u32::MAX` into `GpuInfo::fan_speed_rpm` or the `Fan
/// Speed` detail string. Mirrors the same defence applied to temperature,
/// frequency, power, and memory a few lines below, and shares its bound
/// with the Windows ADL, Intel sysfs, and Intel Level Zero fan readings via
/// [`all_smi::device::types::MAX_GPU_FAN_RPM`].
fn clamp_fan_rpm(rpm: Option<u32>) -> Option<u32> {
    rpm.map(|value| value.min(MAX_GPU_FAN_RPM))
}

/// Per-device state that needs to be cached
///
/// # Thread Safety
///
/// The `vram_usage` field is protected by a `Mutex` to ensure thread-safe access
/// across multiple concurrent readers. The VramUsage struct from libamdgpu_top
/// maintains internal state that must be updated atomically.
///
/// ## Synchronization Guarantees
/// - All reads and writes to `vram_usage` are serialized through the mutex
/// - The mutex ensures memory ordering: all writes before unlock are visible after lock
/// - No data races can occur as long as all access goes through the mutex
///
/// ## Mutex Poisoning Recovery
/// If a thread panics while holding the mutex lock, the mutex becomes "poisoned"
/// to prevent other threads from observing potentially inconsistent state.
/// We handle this by:
/// 1. Detecting the poisoned state
/// 2. Attempting to recover with fresh data from the driver
/// 3. Using `catch_unwind` to handle potential panics during recovery
/// 4. Skipping the device if recovery fails to maintain system stability
///
/// ## Performance Considerations
/// - Mutex contention is minimal as updates are quick (microseconds)
/// - Each device has its own mutex, preventing global bottlenecks
/// - The lock is held only during the VramUsage update operations
struct AmdGpuDevice {
    device_path: DevicePath,
    device_handle: DeviceHandle,
    vram_usage: Mutex<VramUsage>, // Protected by mutex for thread-safe updates
    static_info: OnceLock<DeviceStaticInfo>, // Cached static device information
}

pub struct AmdGpuReader {
    devices: Vec<AmdGpuDevice>,
    /// Cached ROCm version (fetched only once, shared across all devices)
    rocm_version: OnceLock<Option<String>>,
}

impl Default for AmdGpuReader {
    fn default() -> Self {
        Self::new()
    }
}

impl AmdGpuReader {
    pub fn new() -> Self {
        // Check if we have permission to access AMD GPU devices
        // This prevents panic from libamdgpu_top when running without sudo
        // If no permission, silently return empty device list
        // The main program will handle showing sudo message before TUI starts
        if !Self::check_amd_gpu_permissions() {
            return Self {
                devices: Vec::new(),
                rocm_version: OnceLock::new(),
            };
        }

        let device_path_list = DevicePath::get_device_path_list();
        let mut devices = Vec::new();

        // Add device count validation to prevent unbounded growth
        const MAX_DEVICES: usize = 256;
        let device_paths_to_process: Vec<_> =
            device_path_list.into_iter().take(MAX_DEVICES).collect();

        for device_path in device_paths_to_process {
            match device_path.init() {
                Ok(amdgpu_dev) => {
                    // Get initial memory_info to create VramUsage
                    match amdgpu_dev.memory_info() {
                        Ok(memory_info) => {
                            let vram_usage = VramUsage::new(&memory_info);
                            devices.push(AmdGpuDevice {
                                device_path: device_path.clone(),
                                device_handle: amdgpu_dev,
                                vram_usage: Mutex::new(vram_usage),
                                static_info: OnceLock::new(),
                            });
                        }
                        Err(e) => {
                            eprintln!(
                                "Warning: Failed to get memory info for AMD GPU {}: {e}",
                                device_path.pci
                            );
                        }
                    }
                }
                Err(e) => {
                    eprintln!(
                        "Warning: Failed to initialize AMD GPU {}: {e}",
                        device_path.pci
                    );
                }
            }
        }

        Self {
            devices,
            rocm_version: OnceLock::new(),
        }
    }

    /// Get cached ROCm version, initializing if needed
    fn get_rocm_version(&self) -> Option<String> {
        self.rocm_version
            .get_or_init(libamdgpu_top::get_rocm_version)
            .clone()
    }

    /// Get cached static device info for a device, initializing if needed
    fn get_device_static_info<'a>(&self, device: &'a AmdGpuDevice) -> &'a DeviceStaticInfo {
        device
            .static_info
            .get_or_init(|| {
                // Fetch static device information once
                let ext_info = device.device_handle.device_info().ok();
                let memory_info = device.device_handle.memory_info().ok();

                let (device_name, mut detail) = if let (Some(ext), Some(mem)) =
                    (ext_info.as_ref(), memory_info.as_ref())
                {
                    let sensors = libamdgpu_top::stat::Sensors::new(
                        &device.device_handle,
                        &device.device_path.pci,
                        ext,
                    );

                    let app_device_info = AppDeviceInfo::new(
                        &device.device_handle,
                        ext,
                        mem,
                        &sensors,
                        &device.device_path,
                    );

                    let mut builder = DetailBuilder::new()
                        .insert("Device Name", &app_device_info.marketing_name)
                        .insert("PCI Bus", app_device_info.pci_bus.to_string());

                    // Add ROCm version
                    if let Some(ref ver) = self.get_rocm_version() {
                        builder = builder
                            .insert("ROCm Version", ver)
                            .insert("lib_name", "ROCm")
                            .insert("lib_version", ver);
                    }

                    let mut detail = builder.build();

                    // Add device details
                    detail.insert(
                        "Device ID".to_string(),
                        format!("{:#06x}", ext.device_id()),
                    );
                    detail.insert(
                        "Revision ID".to_string(),
                        format!("{:#04x}", ext.pci_rev_id()),
                    );
                    detail.insert(
                        "ASIC Name".to_string(),
                        app_device_info.asic_name.to_string(),
                    );

                    if let Some(ref vbios) = app_device_info.vbios {
                        detail.insert("VBIOS Version".to_string(), vbios.ver.clone());
                        detail.insert("VBIOS Date".to_string(), vbios.date.clone());
                    }

                    if let Some(ref cap) = app_device_info.power_cap {
                        detail.insert("Power Cap".to_string(), format!("{} W", cap.current));
                        detail.insert("Power Cap (Min)".to_string(), format!("{} W", cap.min));
                        detail.insert("Power Cap (Max)".to_string(), format!("{} W", cap.max));
                    }

                    if let Some(link) = app_device_info.max_gpu_link {
                        detail.insert(
                            "Max GPU Link".to_string(),
                            format!("Gen{} x{}", link.r#gen, link.width),
                        );
                    }

                    if let Some(link) = app_device_info.max_system_link {
                        detail.insert(
                            "Max System Link".to_string(),
                            format!("Gen{} x{}", link.r#gen, link.width),
                        );
                    }

                    if let Some(min_dpm_link) = app_device_info.min_dpm_link {
                        detail.insert(
                            "Min DPM Link".to_string(),
                            format!("Gen{} x{}", min_dpm_link.r#gen, min_dpm_link.width),
                        );
                    }

                    if let Some(max_dpm_link) = app_device_info.max_dpm_link {
                        detail.insert(
                            "Max DPM Link".to_string(),
                            format!("Gen{} x{}", max_dpm_link.r#gen, max_dpm_link.width),
                        );
                    }

                    (app_device_info.marketing_name, detail)
                } else {
                    (String::from("Unknown GPU"), HashMap::new())
                };

                // Get driver version
                match device.device_handle.get_drm_version_struct() {
                    Ok(drm) => {
                        if drm.version_major >= 0
                            && drm.version_major <= MAX_VERSION_COMPONENT
                            && drm.version_minor >= 0
                            && drm.version_minor <= MAX_VERSION_COMPONENT
                            && drm.version_patchlevel >= 0
                            && drm.version_patchlevel <= MAX_VERSION_COMPONENT
                        {
                            let ver = format!(
                                "{}.{}.{}",
                                drm.version_major, drm.version_minor, drm.version_patchlevel
                            );
                            detail.insert("Driver Version".to_string(), ver);
                        } else {
                            eprintln!(
                                "Warning: Invalid driver version components detected: {}.{}.{} for device {}",
                                drm.version_major, drm.version_minor, drm.version_patchlevel,
                                device.device_path.pci
                            );
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "Warning: Failed to get driver version for device {}: {e}",
                            device.device_path.pci
                        );
                    }
                };

                DeviceStaticInfo::with_details(device_name, None, detail)
            })
    }

    /// Check if we have permission to access AMD GPU devices
    /// Returns false if /dev/dri devices are not accessible
    fn check_amd_gpu_permissions() -> bool {
        use std::fs;

        // Check if /dev/dri directory exists and is accessible
        let dri_path = std::path::Path::new("/dev/dri");
        if !dri_path.exists() {
            return false;
        }

        // Try to read the directory to check permissions
        match fs::read_dir(dri_path) {
            Ok(entries) => {
                // Check if we can read at least one render or card device
                for entry in entries.flatten() {
                    let path = entry.path();
                    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

                    // Check card or render devices
                    if file_name.starts_with("card") || file_name.starts_with("render") {
                        // Check if we have read/write permissions
                        // For root: always has access
                        // SAFETY: libc::geteuid() is always safe to call - it's a simple
                        // system call that reads the effective user ID from the kernel.
                        // It cannot fail and doesn't access any memory we provide.
                        if unsafe { libc::geteuid() } == 0 {
                            return true; // Root always has access
                        }

                        // For non-root, check if we can actually open the device
                        if let Ok(_file) = fs::OpenOptions::new().read(true).write(true).open(&path)
                        {
                            return true; // We have access
                        }
                    }
                }
                false // No accessible devices found
            }
            Err(_) => false, // Cannot read /dev/dri directory
        }
    }
}

include!("reader/gpu_reader.rs");
include!("reader/tests.rs");
