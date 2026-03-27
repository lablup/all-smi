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

use crate::device::common::constants::FURIOSA_HBM3_MEMORY_BYTES;
use crate::device::common::execute_command_default;
use crate::device::common::parsers::{
    parse_device_id, parse_frequency_mhz, parse_power, parse_temperature,
};
use crate::device::readers::common_cache::{DetailBuilder, DeviceStaticInfo};
use crate::device::types::{GpuInfo, ProcessInfo};
use crate::device::GpuReader;
use crate::utils::get_hostname;
use chrono::Local;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::OnceLock;

// Import furiosa-smi-rs if available on Linux
#[cfg(all(target_os = "linux", feature = "furiosa-smi-rs"))]
use furiosa_smi_rs::list_devices;

/// Collection method for Furiosa NPU metrics
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub enum CollectionMethod {
    /// Use furiosa-smi command-line tool
    FuriosaSmi,
    /// Use furiosa-smi-rs crate
    FuriosaSmiRs,
}

/// JSON structures for furiosa-smi outputs
/// Supports both legacy (Warboy) format with --output flag
/// and RNGD format with --format flag
#[derive(Debug, Deserialize)]
struct FuriosaSmiInfoJson {
    /// Warboy uses numeric "index" (e.g., "0"), RNGD omits this field
    #[serde(default)]
    #[allow(dead_code)]
    index: String,
    arch: String,
    dev_name: String,
    device_uuid: String,
    device_sn: String,
    firmware: String,
    /// Warboy has pert version, RNGD omits this field
    #[serde(default)]
    pert: String,
    temperature: String,
    power: String,
    core_clock: String,
    governor: String,
    pci_bdf: String,
    pci_dev: String,
}

#[derive(Debug, Deserialize)]
struct FuriosaSmiStatusJson {
    /// Warboy uses "index", RNGD omits this field
    #[serde(default)]
    #[allow(dead_code)]
    index: String,
    #[allow(dead_code)]
    arch: String,
    device: String,
    #[allow(dead_code)]
    liveness: String,
    /// Warboy only
    #[serde(default)]
    #[allow(dead_code)]
    cores: Vec<FuriosaCoreInfo>,
    /// RNGD provides per-device memory info
    #[serde(default)]
    memory: Option<FuriosaStatusMemory>,
    pe_utilizations: Vec<FuriosaPeUtilization>,
}

/// Memory info from RNGD status output
#[derive(Debug, Deserialize)]
struct FuriosaStatusMemory {
    #[serde(alias = "DRAM")]
    dram: Option<FuriosaDramInfo>,
}

#[derive(Debug, Deserialize)]
struct FuriosaDramInfo {
    used_size: u64,
    total_size: u64,
    #[allow(dead_code)]
    used_ratio: f64,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct FuriosaCoreInfo {
    idx: u32,
    status: String,
}

#[derive(Debug, Deserialize)]
struct FuriosaPeUtilization {
    #[allow(dead_code)]
    pe_core: u32,
    /// RNGD has pe_occupancy field
    #[serde(default)]
    #[allow(dead_code)]
    pe_occupancy: bool,
    /// Warboy uses "utilization", RNGD uses "pe_utilization"
    #[serde(alias = "pe_utilization")]
    utilization: f64,
}

#[derive(Debug, Deserialize)]
struct FuriosaPsOutputJson {
    /// Warboy uses "npu", RNGD uses "dev_name"
    #[serde(alias = "dev_name")]
    npu: String,
    pid: u32,
    /// Warboy uses "cmd", RNGD uses "cmdline"
    #[serde(alias = "cmdline")]
    cmd: String,
    /// Warboy has memory field, RNGD omits it
    #[serde(default)]
    #[allow(dead_code)]
    memory: String,
}

pub struct FuriosaNpuReader {
    collection_method: CollectionMethod,
    /// Cached static device information per device index (CLI method)
    device_static_info_cli: OnceLock<HashMap<String, DeviceStaticInfo>>,
    /// Cached static device information per device UUID (RS method)
    #[cfg(all(target_os = "linux", feature = "furiosa-smi-rs"))]
    device_static_info_rs: OnceLock<HashMap<String, DeviceStaticInfo>>,
}

impl Default for FuriosaNpuReader {
    fn default() -> Self {
        Self::new()
    }
}

impl FuriosaNpuReader {
    pub fn new() -> Self {
        // Determine which collection method to use
        #[cfg(all(target_os = "linux", feature = "furiosa-smi-rs"))]
        let collection_method = CollectionMethod::FuriosaSmiRs;

        #[cfg(not(all(target_os = "linux", feature = "furiosa-smi-rs")))]
        let collection_method = CollectionMethod::FuriosaSmi;

        Self {
            collection_method,
            device_static_info_cli: OnceLock::new(),
            #[cfg(all(target_os = "linux", feature = "furiosa-smi-rs"))]
            device_static_info_rs: OnceLock::new(),
        }
    }

    /// Get cached static device info for CLI method, initializing if needed
    fn get_device_static_info_cli(&self) -> &HashMap<String, DeviceStaticInfo> {
        self.device_static_info_cli.get_or_init(|| {
            let mut device_info_map = HashMap::new();

            // Get device info to extract static fields
            if let Some(stdout) = furiosa_smi_json("info") {
                if let Ok(devices) = serde_json::from_str::<Vec<FuriosaSmiInfoJson>>(&stdout) {
                    // Use common MAX_DEVICES constant
                    const MAX_DEVICES: usize = crate::device::readers::common_cache::MAX_DEVICES;
                    let devices_to_process: Vec<_> =
                        devices.into_iter().take(MAX_DEVICES).collect();

                    for device in devices_to_process {
                        // Build detail HashMap using DetailBuilder
                        let mut builder = DetailBuilder::new()
                            .insert("serial_number", &device.device_sn)
                            .insert("firmware_version", &device.firmware)
                            .insert("pci_bdf", &device.pci_bdf)
                            .insert("pci_dev", &device.pci_dev)
                            .insert("architecture", device.arch.to_uppercase())
                            .insert("core_count", "8")
                            .insert("pe_count", "64K")
                            .insert("memory_bandwidth", "1.63TB/s")
                            .insert("on_chip_sram", "256MB");

                        // Only add pert_version if available (Warboy has it, RNGD may not)
                        if !device.pert.is_empty() {
                            builder = builder
                                .insert("pert_version", &device.pert)
                                .insert_lib_info("PERT", Some(&device.pert));
                        }

                        let detail = builder.build();

                        let static_info = DeviceStaticInfo::with_details(
                            format!("Furiosa {}", device.arch.to_uppercase()),
                            Some(device.device_uuid.clone()),
                            detail,
                        );

                        // Use dev_name as key (e.g., "npu0") for both Warboy and RNGD
                        device_info_map.insert(device.dev_name.clone(), static_info);
                    }
                }
            }

            device_info_map
        })
    }

    /// Get cached static device info for RS method, initializing if needed
    #[cfg(all(target_os = "linux", feature = "furiosa-smi-rs"))]
    fn get_device_static_info_rs(&self) -> &HashMap<String, DeviceStaticInfo> {
        self.device_static_info_rs.get_or_init(|| {
            let mut device_info_map = HashMap::new();

            if let Ok(devices) = list_devices() {
                // Use common MAX_DEVICES constant
                const MAX_DEVICES: usize = crate::device::readers::common_cache::MAX_DEVICES;
                let devices_to_process: Vec<_> = devices.iter().take(MAX_DEVICES).collect();

                for device in devices_to_process {
                    if let Ok(info) = device.device_info() {
                        // Build detail HashMap using DetailBuilder
                        let detail = DetailBuilder::new()
                            .insert("serial_number", info.serial())
                            .insert("firmware_version", &info.firmware_version().to_string())
                            .insert("architecture", format!("{:?}", info.arch()))
                            .insert("core_count", &info.core_num().to_string())
                            .insert("bdf", info.bdf())
                            .insert("numa_node", &info.numa_node().to_string())
                            // Add unified AI acceleration library labels
                            .insert_lib_info("PERT", Some(&info.pert_version().to_string()))
                            .build();

                        let static_info = DeviceStaticInfo::with_details(
                            format!("Furiosa {:?}", info.arch()),
                            Some(info.uuid()),
                            detail,
                        );

                        device_info_map.insert(info.uuid(), static_info);
                    }
                }
            }

            device_info_map
        })
    }

    /// Get NPU info based on collection method
    fn get_npu_info_internal(&self) -> Vec<GpuInfo> {
        match self.collection_method {
            #[cfg(all(target_os = "linux", feature = "furiosa-smi-rs"))]
            CollectionMethod::FuriosaSmiRs => self.get_npu_info_rs(),
            _ => self.get_npu_info_cli(),
        }
    }

    /// Get NPU info using furiosa-smi-rs crate
    #[cfg(all(target_os = "linux", feature = "furiosa-smi-rs"))]
    fn get_npu_info_rs(&self) -> Vec<GpuInfo> {
        // Initialize library and list devices
        let devices = match list_devices() {
            Ok(devices) => devices,
            Err(_) => return Vec::new(),
        };

        let time = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let hostname = get_hostname();

        // Get cached static info
        let static_info_map = self.get_device_static_info_rs();

        devices
            .iter()
            .filter_map(|device| {
                // Get device information using 2025.3.0 API
                let info = device.device_info().ok()?;
                let uuid = info.uuid();

                // Get cached static info for this device
                let static_info = static_info_map.get(&uuid)?;

                // Get dynamic performance metrics only
                let utilization = device.core_utilization().ok()?;
                let temperature = device.device_temperature().ok()?;
                let power = device.power_consumption().ok()?;
                let governor = device.governor_profile().ok()?;
                let core_freq = device.core_frequency().ok()?;

                create_gpu_info_from_device_2025_cached(
                    static_info,
                    &utilization,
                    &temperature,
                    &power,
                    &governor,
                    &core_freq,
                    &time,
                    &hostname,
                )
            })
            .collect()
    }

    /// Get NPU info using furiosa-smi command
    fn get_npu_info_cli(&self) -> Vec<GpuInfo> {
        // Get cached static info first (this will call furiosa-smi info once)
        let static_info_map = self.get_device_static_info_cli();

        // Get status for utilization and memory (dynamic data)
        let status_stdout = match furiosa_smi_json("status") {
            Some(stdout) => stdout,
            None => return Vec::new(),
        };

        let status_list: Vec<FuriosaSmiStatusJson> =
            serde_json::from_str(&status_stdout).unwrap_or_default();

        // Also need to get info for dynamic fields (temperature, power, frequency, governor)
        let info_stdout = match furiosa_smi_json("info") {
            Some(stdout) => stdout,
            None => return Vec::new(),
        };

        let devices: Vec<FuriosaSmiInfoJson> = match serde_json::from_str(&info_stdout) {
            Ok(devices) => devices,
            Err(_) => return Vec::new(),
        };

        let time = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let hostname = get_hostname();

        devices
            .into_iter()
            .filter_map(|device| {
                // Use dev_name (e.g., "npu0") as the lookup key
                let static_info = static_info_map.get(&device.dev_name)?;
                // Match status by device field (RNGD) which also uses dev_name format
                let status = status_list.iter().find(|s| s.device == device.dev_name);
                create_gpu_info_from_cli_cached(
                    static_info,
                    &device,
                    status,
                    &time,
                    &hostname,
                )
            })
            .collect()
    }

    /// Get process info using furiosa-smi ps
    fn get_process_info_internal(&self) -> Vec<ProcessInfo> {
        let stdout = match furiosa_smi_json("ps") {
            Some(stdout) => stdout,
            None => return Vec::new(),
        };

        let processes: Vec<FuriosaPsOutputJson> = match serde_json::from_str(&stdout) {
            Ok(procs) => procs,
            Err(_) => return Vec::new(),
        };

        processes
            .into_iter()
            .map(|proc| create_process_info_from_ps(&proc))
            .collect()
    }
}

impl GpuReader for FuriosaNpuReader {
    fn get_gpu_info(&self) -> Vec<GpuInfo> {
        self.get_npu_info_internal()
    }

    fn get_process_info(&self) -> Vec<ProcessInfo> {
        self.get_process_info_internal()
    }
}

// Helper functions

/// Run furiosa-smi subcommand with JSON output.
/// Tries RNGD format (--format json) first, falls back to legacy (--output json).
fn furiosa_smi_json(subcommand: &str) -> Option<String> {
    // Try RNGD format first (--format json)
    if let Ok(output) = execute_command_default("furiosa-smi", &[subcommand, "--format", "json"]) {
        if output.status == 0 && !output.stdout.is_empty() {
            return Some(output.stdout);
        }
    }
    // Fall back to legacy format (--output json)
    if let Ok(output) = execute_command_default("furiosa-smi", &[subcommand, "--output", "json"]) {
        if output.status == 0 && !output.stdout.is_empty() {
            return Some(output.stdout);
        }
    }
    None
}

/// Create GpuInfo from CLI data using cached static info
fn create_gpu_info_from_cli_cached(
    static_info: &DeviceStaticInfo,
    device: &FuriosaSmiInfoJson,
    status: Option<&FuriosaSmiStatusJson>,
    time: &str,
    hostname: &str,
) -> Option<GpuInfo> {
    // Clone static detail and add dynamic governor field
    let mut detail = static_info.detail.clone();
    detail.insert("governor".to_string(), device.governor.clone());

    // Parse dynamic metrics only
    let temperature = parse_temperature(&device.temperature).unwrap_or_else(|| {
        eprintln!("Failed to parse temperature: {}", device.temperature);
        0
    });
    let power = parse_power(&device.power).unwrap_or_else(|| {
        eprintln!("Failed to parse power: {}", device.power);
        0.0
    });
    let frequency = parse_frequency_mhz(&device.core_clock).unwrap_or_else(|| {
        eprintln!("Failed to parse frequency: {}", device.core_clock);
        0
    });

    let utilization = status
        .and_then(|s| {
            s.pe_utilizations
                .iter()
                .map(|pe| pe.utilization)
                .max_by(|a, b| {
                    // Safe comparison handling NaN values
                    match (a.is_nan(), b.is_nan()) {
                        (true, true) => std::cmp::Ordering::Equal,
                        (true, false) => std::cmp::Ordering::Less,
                        (false, true) => std::cmp::Ordering::Greater,
                        (false, false) => a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal),
                    }
                })
        })
        .unwrap_or(0.0);

    // Get memory from status (RNGD provides per-device DRAM info), fall back to constant
    let (used_memory, total_memory) = match status.and_then(|s| s.memory.as_ref()) {
        Some(mem) => {
            let dram = mem.dram.as_ref();
            let used = dram.map_or(0, |d| d.used_size);
            let total = dram.map_or(FURIOSA_HBM3_MEMORY_BYTES, |d| d.total_size);
            (used, total)
        }
        None => (0, FURIOSA_HBM3_MEMORY_BYTES),
    };

    Some(GpuInfo {
        uuid: static_info
            .uuid
            .clone()
            .unwrap_or_else(|| device.device_uuid.clone()),
        time: time.to_string(),
        name: static_info.name.clone(),
        device_type: "NPU".to_string(),
        host_id: hostname.to_string(),
        hostname: hostname.to_string(),
        instance: hostname.to_string(),
        utilization,
        ane_utilization: 0.0,
        dla_utilization: None,
        tensorcore_utilization: None,
        temperature,
        used_memory,
        total_memory,
        frequency,
        power_consumption: power,
        gpu_core_count: None,
        detail,
    })
}

/// Extract the first device name from an RNGD ps dev_name string.
/// RNGD format: "npu4:[0, 7], npu5:[0, 7], npu6:[0, 7], npu7:[0, 7]"
/// Legacy format: "npu0"
fn extract_first_device_name(dev_name: &str) -> &str {
    // Split on ':' to get the first device name prefix (e.g., "npu4" from "npu4:[0, 7], ...")
    // If no ':', the whole string is the device name (legacy format)
    dev_name.split(':').next().unwrap_or(dev_name).trim()
}

/// Helper to compute average PE utilization from CoreUtilization (RS API)
#[cfg(all(target_os = "linux", feature = "furiosa-smi-rs"))]
fn compute_avg_pe_utilization(utilization: &furiosa_smi_rs::CoreUtilization) -> f64 {
    let pe_utils = utilization.pe_utilization();
    if pe_utils.is_empty() {
        return 0.0;
    }
    let sum: f64 = pe_utils.iter().map(|pe| pe.pe_usage_percentage()).sum();
    sum / pe_utils.len() as f64
}

/// Helper to get the first PE frequency in MHz from CoreFrequency (RS API)
#[cfg(all(target_os = "linux", feature = "furiosa-smi-rs"))]
fn first_pe_frequency_mhz(core_freq: &furiosa_smi_rs::CoreFrequency) -> u32 {
    core_freq
        .pe_frequency()
        .first()
        .map(|pf| pf.frequency())
        .unwrap_or(0)
}

/// Create GpuInfo from RS API data using cached static info
#[cfg(all(target_os = "linux", feature = "furiosa-smi-rs"))]
fn create_gpu_info_from_device_2025_cached(
    static_info: &DeviceStaticInfo,
    utilization: &furiosa_smi_rs::CoreUtilization,
    temperature: &furiosa_smi_rs::DeviceTemperature,
    power: &f64,
    governor: &furiosa_smi_rs::GovernorProfile,
    core_freq: &furiosa_smi_rs::CoreFrequency,
    time: &str,
    hostname: &str,
) -> Option<GpuInfo> {
    let freq_mhz = first_pe_frequency_mhz(core_freq);

    // Clone static detail and add dynamic fields
    let mut detail = static_info.detail.clone();
    detail.insert("governor".to_string(), format!("{governor}"));
    detail.insert("frequency".to_string(), format!("{freq_mhz}MHz"));

    let avg_util = compute_avg_pe_utilization(utilization);

    // TODO: Get memory info - not directly available in 2025.3.0 API
    let (used_memory, total_memory) = (0u64, FURIOSA_HBM3_MEMORY_BYTES);

    // Extract core_num from static detail for gpu_core_count
    let gpu_core_count = detail.get("core_count").and_then(|s| s.parse::<u32>().ok());

    Some(GpuInfo {
        uuid: static_info.uuid.clone().unwrap_or_default(),
        time: time.to_string(),
        name: static_info.name.clone(),
        device_type: "NPU".to_string(),
        host_id: hostname.to_string(),
        hostname: hostname.to_string(),
        instance: hostname.to_string(),
        utilization: avg_util,
        ane_utilization: 0.0,
        dla_utilization: None,
        tensorcore_utilization: None,
        temperature: temperature.soc_peak() as u32,
        used_memory,
        total_memory,
        frequency: freq_mhz,
        power_consumption: *power,
        gpu_core_count,
        detail,
    })
}

#[cfg(all(target_os = "linux", feature = "furiosa-smi-rs"))]
#[allow(dead_code)]
fn create_gpu_info_from_device_2025(
    info: &furiosa_smi_rs::DeviceInfo,
    utilization: &furiosa_smi_rs::CoreUtilization,
    temperature: &furiosa_smi_rs::DeviceTemperature,
    power: &f64,
    governor: &furiosa_smi_rs::GovernorProfile,
    core_freq: &furiosa_smi_rs::CoreFrequency,
    time: &str,
    hostname: &str,
) -> Option<GpuInfo> {
    let freq_mhz = first_pe_frequency_mhz(core_freq);
    let mut detail = HashMap::new();

    // Add device details from DeviceInfo using 2025.3.0 API methods
    detail.insert("serial_number".to_string(), info.serial());
    detail.insert(
        "firmware_version".to_string(),
        info.firmware_version().to_string(),
    );
    detail.insert("architecture".to_string(), format!("{:?}", info.arch()));
    detail.insert("core_count".to_string(), info.core_num().to_string());
    detail.insert("bdf".to_string(), info.bdf());
    detail.insert("numa_node".to_string(), info.numa_node().to_string());

    // Add performance details
    detail.insert("governor".to_string(), format!("{governor}"));
    detail.insert("frequency".to_string(), format!("{freq_mhz}MHz"));

    // Add unified AI acceleration library labels using PERT version
    detail.insert("lib_name".to_string(), "PERT".to_string());
    detail.insert("lib_version".to_string(), info.pert_version().to_string());

    let avg_util = compute_avg_pe_utilization(utilization);

    // TODO: Get memory info - not directly available in 2025.3.0 API
    let (used_memory, total_memory) = (0u64, FURIOSA_HBM3_MEMORY_BYTES);

    Some(GpuInfo {
        uuid: info.uuid(),
        time: time.to_string(),
        name: format!("Furiosa {:?}", info.arch()),
        device_type: "NPU".to_string(),
        host_id: hostname.to_string(),
        hostname: hostname.to_string(),
        instance: hostname.to_string(),
        utilization: avg_util,
        ane_utilization: 0.0,
        dla_utilization: None,
        tensorcore_utilization: None,
        temperature: temperature.soc_peak() as u32,
        used_memory,
        total_memory,
        frequency: freq_mhz,
        power_consumption: *power,
        gpu_core_count: Some(info.core_num()),
        detail,
    })
}

fn create_process_info_from_ps(proc: &FuriosaPsOutputJson) -> ProcessInfo {
    // Extract the first device name from potentially complex RNGD format
    let first_device = extract_first_device_name(&proc.npu);
    let device_id = parse_device_id(first_device).unwrap_or_else(|| {
        eprintln!("Failed to parse device ID: {}", proc.npu);
        0
    });

    ProcessInfo {
        device_id,
        device_uuid: proc.npu.clone(),
        pid: proc.pid,
        process_name: extract_process_name(&proc.cmd),
        used_memory: 0,
        cpu_percent: 0.0,
        memory_percent: 0.0,
        memory_rss: 0,
        memory_vms: 0,
        user: String::new(),
        state: String::new(),
        start_time: String::new(),
        cpu_time: 0,
        command: proc.cmd.clone(),
        ppid: 0,
        threads: 0,
        uses_gpu: true,
        priority: 0,
        nice_value: 0,
        gpu_utilization: 0.0,
    }
}

fn extract_process_name(cmd: &str) -> String {
    cmd.split_whitespace()
        .next()
        .and_then(|path| path.split('/').next_back())
        .unwrap_or("unknown")
        .to_string()
}
