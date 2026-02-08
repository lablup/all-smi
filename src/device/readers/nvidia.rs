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

use crate::device::common::constants::BYTES_PER_MB;
use crate::device::common::{execute_command_default, parse_csv_line};
use crate::device::process_list::{get_all_processes, merge_gpu_processes};
use crate::device::readers::common_cache::{DetailBuilder, DeviceStaticInfo, MAX_DEVICES};
use crate::device::types::{GpuInfo, ProcessInfo};
use crate::device::GpuReader;
use crate::utils::{get_hostname, with_global_system};
use chrono::Local;
use nvml_wrapper::enums::device::UsedGpuMemory;
use nvml_wrapper::error::NvmlError;
use nvml_wrapper::{cuda_driver_version_major, cuda_driver_version_minor, Nvml};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

// Global status for NVML error messages
static NVML_STATUS: Mutex<Option<String>> = Mutex::new(None);

pub struct NvidiaGpuReader {
    /// Cached driver version (fetched only once)
    driver_version: OnceLock<String>,
    /// Cached CUDA version (fetched only once)
    cuda_version: OnceLock<String>,
    /// Cached static device information per device index
    device_static_info: OnceLock<HashMap<u32, DeviceStaticInfo>>,
    /// Cached NVML handle (initialized once, reused across calls)
    nvml: Mutex<Option<Nvml>>,
}

impl Default for NvidiaGpuReader {
    fn default() -> Self {
        Self::new()
    }
}

impl NvidiaGpuReader {
    pub fn new() -> Self {
        Self {
            driver_version: OnceLock::new(),
            cuda_version: OnceLock::new(),
            device_static_info: OnceLock::new(),
            nvml: Mutex::new(Nvml::init().ok()),
        }
    }

    /// Get cached driver version, initializing if needed
    fn get_driver_version(&self, nvml: &Nvml) -> String {
        self.driver_version
            .get_or_init(|| {
                nvml.sys_driver_version()
                    .unwrap_or_else(|_| "Unknown".to_string())
            })
            .clone()
    }

    /// Get cached CUDA version, initializing if needed
    fn get_cuda_version(&self, nvml: &Nvml) -> String {
        self.cuda_version
            .get_or_init(|| {
                let version = nvml.sys_cuda_driver_version().unwrap_or(0);
                format!(
                    "{}.{}",
                    cuda_driver_version_major(version),
                    cuda_driver_version_minor(version)
                )
            })
            .clone()
    }

    /// Execute a closure with a reference to the cached NVML handle.
    /// Reinitializes the handle if it was previously unavailable or became invalid.
    fn with_nvml<F, T>(&self, f: F) -> Result<T, NvmlError>
    where
        F: FnOnce(&Nvml) -> T,
    {
        let mut guard = self.nvml.lock().map_err(|_| NvmlError::Unknown)?;
        // Try to use existing handle first
        if let Some(ref nvml) = *guard {
            // Validate the handle is still usable by querying device count
            if nvml.device_count().is_ok() {
                return Ok(f(nvml));
            }
            // Handle is stale, drop and reinitialize below
        }
        // Initialize or reinitialize
        match Nvml::init() {
            Ok(nvml) => {
                let result = f(&nvml);
                *guard = Some(nvml);
                Ok(result)
            }
            Err(e) => {
                *guard = None;
                Err(e)
            }
        }
    }

    /// Get cached static device info for all devices, initializing if needed
    fn get_device_static_info(&self, nvml: &Nvml) -> &HashMap<u32, DeviceStaticInfo> {
        self.device_static_info.get_or_init(|| {
            let mut device_info_map = HashMap::new();
            let driver_version = self.get_driver_version(nvml);
            let cuda_version = self.get_cuda_version(nvml);

            if let Ok(device_count) = nvml.device_count() {
                // Add device count validation to prevent unbounded growth
                let device_count = device_count.min(MAX_DEVICES as u32);

                for i in 0..device_count {
                    if let Ok(device) = nvml.device_by_index(i) {
                        let detail = create_device_detail(&device, &driver_version, &cuda_version);
                        let name = device.name().unwrap_or_else(|_| "Unknown GPU".to_string());
                        let uuid = device.uuid().ok();
                        device_info_map
                            .insert(i, DeviceStaticInfo::with_details(name, uuid, detail));
                    }
                }
            }
            device_info_map
        })
    }

    /// Get GPU processes using cached NVML handle, falling back to nvidia-smi
    fn get_gpu_processes_cached(&self) -> (Vec<ProcessInfo>, HashSet<u32>) {
        match self.with_nvml(get_gpu_processes_nvml) {
            Ok(result) => result,
            Err(e) => {
                set_nvml_status(e);
                get_gpu_processes_nvidia_smi()
            }
        }
    }

    /// Get GPU info using NVML with cached static values
    fn get_gpu_info_nvml(&self, nvml: &Nvml) -> Vec<GpuInfo> {
        let mut gpu_info = Vec::new();

        // Get cached static device information (fetched only once)
        let device_static_info = self.get_device_static_info(nvml);

        if let Ok(device_count) = nvml.device_count() {
            for i in 0..device_count {
                if let Ok(device) = nvml.device_by_index(i) {
                    // Get cached static detail for this device
                    let detail = device_static_info
                        .get(&i)
                        .map(|info| info.detail.clone())
                        .unwrap_or_default();

                    let info = GpuInfo {
                        uuid: device.uuid().unwrap_or_else(|_| format!("GPU-{i}")),
                        time: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                        name: device.name().unwrap_or_else(|_| "Unknown GPU".to_string()),
                        device_type: "GPU".to_string(),
                        host_id: get_hostname(),
                        hostname: get_hostname(),
                        instance: get_hostname(),
                        utilization: device
                            .utilization_rates()
                            .map(|u| u.gpu as f64)
                            .unwrap_or(0.0),
                        ane_utilization: 0.0,
                        dla_utilization: None,
                        tensorcore_utilization: None,
                        temperature: device
                            .temperature(
                                nvml_wrapper::enum_wrappers::device::TemperatureSensor::Gpu,
                            )
                            .unwrap_or(0),
                        used_memory: device.memory_info().map(|m| m.used).unwrap_or(0),
                        total_memory: device.memory_info().map(|m| m.total).unwrap_or(0),
                        frequency: device
                            .clock(
                                nvml_wrapper::enum_wrappers::device::Clock::Graphics,
                                nvml_wrapper::enum_wrappers::device::ClockId::Current,
                            )
                            .unwrap_or(0),
                        power_consumption: device
                            .power_usage()
                            .map(|p| p as f64 / 1000.0)
                            .unwrap_or(0.0),
                        gpu_core_count: None,
                        detail,
                    };
                    gpu_info.push(info);
                }
            }
        }

        gpu_info
    }
}

impl GpuReader for NvidiaGpuReader {
    fn get_gpu_info(&self) -> Vec<GpuInfo> {
        // Try cached NVML handle first
        match self.with_nvml(|nvml| self.get_gpu_info_nvml(nvml)) {
            Ok(info) => {
                // Clear any previous error status on success
                if let Ok(mut status) = NVML_STATUS.lock() {
                    *status = None;
                }
                info
            }
            Err(e) => {
                // Store the error status for notification
                set_nvml_status(e);
                get_gpu_info_nvidia_smi()
            }
        }
    }

    fn get_process_info(&self) -> Vec<ProcessInfo> {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, UpdateKind};

        // Get GPU processes and PIDs using cached NVML handle
        let (gpu_processes, gpu_pids) = self.get_gpu_processes_cached();

        // Use global system instance to avoid file descriptor leak
        let mut all_processes = with_global_system(|system| {
            system.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::everything().with_user(UpdateKind::Always),
            );
            system.refresh_memory();

            // Get all system processes
            get_all_processes(system, &gpu_pids)
        });

        // Merge GPU information into the process list
        merge_gpu_processes(&mut all_processes, gpu_processes);

        all_processes
    }
}

// Helper function to set NVML status
fn set_nvml_status(error: NvmlError) {
    if let Ok(mut status) = NVML_STATUS.lock() {
        *status = Some(format!("NVML Error: {error}"));
    }
}

// Get global NVML status
#[allow(dead_code)]
pub fn get_nvml_status() -> Option<String> {
    NVML_STATUS.lock().ok()?.clone()
}

/// Get a user-friendly message about NVML status
#[allow(dead_code)]
pub fn get_nvml_status_message() -> Option<String> {
    // Only return the stored status, don't try to initialize NVML here
    if let Ok(status) = NVML_STATUS.lock() {
        status.clone()
    } else {
        None
    }
}

// Get GPU processes using NVML
fn get_gpu_processes_nvml(nvml: &Nvml) -> (Vec<ProcessInfo>, HashSet<u32>) {
    let mut gpu_processes = Vec::new();
    let mut gpu_pids = HashSet::new();

    if let Ok(device_count) = nvml.device_count() {
        for device_index in 0..device_count {
            if let Ok(device) = nvml.device_by_index(device_index) {
                let device_uuid = device
                    .uuid()
                    .unwrap_or_else(|_| format!("GPU-{device_index}"));

                // Get compute processes
                if let Ok(processes) = device.running_compute_processes() {
                    for proc in processes {
                        if proc.pid > 0 {
                            gpu_pids.insert(proc.pid);
                            let process_info = create_base_process_info(
                                device_index as usize,
                                device_uuid.clone(),
                                proc.pid,
                                proc.used_gpu_memory,
                            );
                            gpu_processes.push(process_info);
                        }
                    }
                }

                // Also check graphics processes
                if let Ok(processes) = device.running_graphics_processes() {
                    for proc in processes {
                        if proc.pid > 0 && !gpu_pids.contains(&proc.pid) {
                            gpu_pids.insert(proc.pid);
                            let process_info = create_base_process_info(
                                device_index as usize,
                                device_uuid.clone(),
                                proc.pid,
                                proc.used_gpu_memory,
                            );
                            gpu_processes.push(process_info);
                        }
                    }
                }
            }
        }
    }

    (gpu_processes, gpu_pids)
}

// Helper to create base ProcessInfo
fn create_base_process_info(
    device_id: usize,
    device_uuid: String,
    pid: u32,
    memory: UsedGpuMemory,
) -> ProcessInfo {
    let used_memory_mb = match memory {
        UsedGpuMemory::Used(bytes) => bytes / BYTES_PER_MB,
        UsedGpuMemory::Unavailable => 0,
    };

    ProcessInfo {
        device_id,
        device_uuid,
        pid,
        process_name: String::new(), // Will be filled by sysinfo
        used_memory: used_memory_mb * BYTES_PER_MB, // Convert MB to bytes
        cpu_percent: 0.0,            // Will be filled by sysinfo
        memory_percent: 0.0,         // Will be filled by sysinfo
        memory_rss: 0,               // Will be filled by sysinfo
        memory_vms: 0,               // Will be filled by sysinfo
        user: String::new(),         // Will be filled by sysinfo
        state: String::new(),        // Will be filled by sysinfo
        start_time: String::new(),   // Will be filled by sysinfo
        cpu_time: 0,                 // Will be filled by sysinfo
        command: String::new(),      // Will be filled by sysinfo
        ppid: 0,                     // Will be filled by sysinfo
        threads: 0,                  // Will be filled by sysinfo
        uses_gpu: true,
        priority: 0,          // Will be filled by sysinfo
        nice_value: 0,        // Will be filled by sysinfo
        gpu_utilization: 0.0, // NVIDIA doesn't provide per-process GPU utilization
    }
}

// Macros to reduce boilerplate
macro_rules! add_detail {
    ($detail:expr, $result:expr, $key:expr) => {
        if let Ok(value) = $result {
            $detail.insert($key.to_string(), format!("{value:?}"));
        }
    };
}

macro_rules! add_detail_fmt {
    ($detail:expr, $result:expr, $key:expr, $fmt:expr) => {
        if let Ok(value) = $result {
            $detail.insert($key.to_string(), format!($fmt, value));
        }
    };
}

// Helper to create device detail HashMap
fn create_device_detail(
    device: &nvml_wrapper::Device,
    driver_version: &str,
    cuda_version: &str,
) -> HashMap<String, String> {
    let builder = DetailBuilder::new()
        .insert("Driver Version", driver_version)
        .insert("CUDA Version", cuda_version)
        // Add unified AI acceleration library labels
        .insert("lib_name", "CUDA")
        .insert("lib_version", cuda_version);

    // Add all device details using helper macros
    let mut detail = builder.build();
    add_detail!(detail, device.brand(), "Brand");
    add_detail!(detail, device.architecture(), "Architecture");
    add_detail!(detail, device.current_pcie_link_gen(), "PCIe Generation");
    add_detail_fmt!(
        detail,
        device.current_pcie_link_width(),
        "PCIe Width",
        "x{}"
    );
    add_detail!(detail, device.compute_mode(), "compute_mode");
    add_detail!(detail, device.max_pcie_link_gen(), "pcie_gen_max");
    add_detail!(detail, device.max_pcie_link_width(), "pcie_width_max");
    add_detail!(detail, device.performance_state(), "performance_state");

    // Power limits
    if let Ok(power_limit) = device.power_management_limit() {
        detail.insert(
            "power_limit_current".to_string(),
            format!("{:.2}", power_limit as f64 / 1000.0),
        );
    }
    if let Ok(power_limit_default) = device.power_management_limit_default() {
        detail.insert(
            "power_limit_default".to_string(),
            format!("{:.2}", power_limit_default as f64 / 1000.0),
        );
    }
    if let Ok(constraints) = device.power_management_limit_constraints() {
        detail.insert(
            "power_limit_min".to_string(),
            format!("{:.2}", constraints.min_limit as f64 / 1000.0),
        );
        detail.insert(
            "power_limit_max".to_string(),
            format!("{:.2}", constraints.max_limit as f64 / 1000.0),
        );
    }

    // Max clocks
    use nvml_wrapper::enum_wrappers::device::Clock;
    add_detail!(
        detail,
        device.max_customer_boost_clock(Clock::Graphics),
        "clock_graphics_max"
    );
    add_detail!(
        detail,
        device.max_customer_boost_clock(Clock::Memory),
        "clock_memory_max"
    );

    // ECC mode
    if let Ok(ecc_enabled) = device.is_ecc_enabled() {
        detail.insert(
            "ecc_mode_current".to_string(),
            if ecc_enabled.currently_enabled {
                "Enabled"
            } else {
                "Disabled"
            }
            .to_string(),
        );
        if ecc_enabled.currently_enabled != ecc_enabled.pending_enabled {
            detail.insert(
                "ecc_mode_pending".to_string(),
                if ecc_enabled.pending_enabled {
                    "Enabled"
                } else {
                    "Disabled"
                }
                .to_string(),
            );
        }
    }

    // MIG mode
    if let Ok(mig_mode) = device.mig_mode() {
        detail.insert(
            "mig_mode_current".to_string(),
            format!("{:?}", mig_mode.current),
        );
        if mig_mode.current != mig_mode.pending {
            detail.insert(
                "mig_mode_pending".to_string(),
                format!("{:?}", mig_mode.pending),
            );
        }
    }

    // VBIOS version
    add_detail!(detail, device.vbios_version(), "vbios_version");

    detail
}

// Fallback implementation using nvidia-smi
fn get_gpu_info_nvidia_smi() -> Vec<GpuInfo> {
    let output = match execute_command_default("nvidia-smi", &[
        "--query-gpu=index,uuid,name,utilization.gpu,temperature.gpu,memory.used,memory.total,clocks.gr,power.draw",
        "--format=csv,noheader,nounits"
    ]) {
        Ok(output) => output.stdout,
        Err(_) => return Vec::new(),
    };

    let time = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let hostname = get_hostname();

    output
        .lines()
        .filter_map(|line| {
            let parts = parse_csv_line(line);
            if parts.len() >= 9 {
                Some(GpuInfo {
                    uuid: parts[1].to_string(),
                    time: time.clone(),
                    name: parts[2].to_string(),
                    device_type: "GPU".to_string(),
                    host_id: hostname.clone(),
                    hostname: hostname.clone(),
                    instance: hostname.clone(),
                    utilization: parts[3].parse().unwrap_or(0.0),
                    ane_utilization: 0.0,
                    dla_utilization: None,
                    tensorcore_utilization: None,
                    temperature: parts[4].parse().unwrap_or(0),
                    used_memory: parse_memory_value(&parts[5]),
                    total_memory: parse_memory_value(&parts[6]),
                    frequency: parts[7].parse().unwrap_or(0),
                    power_consumption: parts[8].replace("[N/A]", "0").parse::<f64>().unwrap_or(0.0)
                        / 1000.0,
                    gpu_core_count: None,
                    detail: HashMap::new(),
                })
            } else {
                None
            }
        })
        .collect()
}

// Get GPU processes using nvidia-smi
fn get_gpu_processes_nvidia_smi() -> (Vec<ProcessInfo>, HashSet<u32>) {
    let mut gpu_processes = Vec::new();
    let mut gpu_pids = HashSet::new();

    let output = match execute_command_default(
        "nvidia-smi",
        &[
            "--query-compute-apps=gpu_uuid,pid,used_memory",
            "--format=csv,noheader,nounits",
        ],
    ) {
        Ok(output) => output.stdout,
        Err(_) => return (gpu_processes, gpu_pids),
    };

    for line in output.lines() {
        let parts = parse_csv_line(line);
        if parts.len() >= 3 {
            if let Ok(pid) = parts[1].parse::<u32>() {
                gpu_pids.insert(pid);
                gpu_processes.push(ProcessInfo {
                    device_id: 0, // We don't have device index from this query
                    device_uuid: parts[0].to_string(),
                    pid,
                    process_name: String::new(),
                    used_memory: parse_memory_value(&parts[2]),
                    cpu_percent: 0.0,
                    memory_percent: 0.0,
                    memory_rss: 0,
                    memory_vms: 0,
                    user: String::new(),
                    state: String::new(),
                    start_time: String::new(),
                    cpu_time: 0,
                    command: String::new(),
                    ppid: 0,
                    threads: 0,
                    uses_gpu: true,
                    priority: 0,
                    nice_value: 0,
                    gpu_utilization: 0.0,
                });
            }
        }
    }

    (gpu_processes, gpu_pids)
}

// Helper to parse memory values
fn parse_memory_value(value: &str) -> u64 {
    value.parse::<u64>().unwrap_or(0) * BYTES_PER_MB // Convert MB to bytes
}
