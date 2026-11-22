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
use crate::device::common::constants::FURIOSA_HBM3_MEMORY_BYTES;
use crate::device::common::execute_command_default;
use crate::device::common::parsers::{
    parse_device_id, parse_frequency_mhz, parse_memory_mb_to_bytes, parse_power, parse_temperature,
};
use crate::device::readers::common_cache::{
    COALESCE_WINDOW, CoalescedSlot, DetailBuilder, DeviceStaticInfo,
};
use crate::device::types::{GpuInfo, ProcessInfo};
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

/// Runs one `furiosa-smi <subcommand>` with JSON output and returns its
/// stdout, or `None` when the run failed.
type SmiRunner = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// How long a failed re-enumeration of the devices is remembered (`furiosa`
/// feature). Enumerating creates a new device observer, so a device that
/// keeps erroring triggers it at most once per interval.
#[cfg(all(target_os = "linux", feature = "furiosa-smi-rs"))]
const DEVICE_REENUMERATE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// One enumerated device with the identity read from it at enumeration time
/// (`furiosa` feature).
#[cfg(all(target_os = "linux", feature = "furiosa-smi-rs"))]
struct RsDevice {
    device: furiosa_smi_rs::Device,
    static_info: DeviceStaticInfo,
}

/// The cached device list of the `furiosa` feature path.
#[cfg(all(target_os = "linux", feature = "furiosa-smi-rs"))]
#[derive(Default)]
struct RsDeviceCache {
    devices: Option<std::sync::Arc<Vec<RsDevice>>>,
    /// Set when a per-device call failed; the list is enumerated again once
    /// the gate allows it.
    stale: bool,
}

pub struct FuriosaNpuReader {
    collection_method: CollectionMethod,
    /// Cached static device information per device index (CLI method)
    device_static_info_cli: OnceLock<HashMap<String, DeviceStaticInfo>>,
    /// This tick's parsed `furiosa-smi info`, so a repeated `get_gpu_info`
    /// within the window does not spawn again (CLI method).
    info_cli: CoalescedSlot<Vec<FuriosaSmiInfoJson>>,
    /// This tick's parsed `furiosa-smi status` (CLI method).
    status_cli: CoalescedSlot<Vec<FuriosaSmiStatusJson>>,
    /// Runs `furiosa-smi`; replaced by a counting fake in tests.
    smi_runner: SmiRunner,
    /// Devices and their static identity, enumerated once (RS method).
    /// `list_devices()` builds a new device observer on every call, so the
    /// handles are kept instead of being fetched each tick.
    #[cfg(all(target_os = "linux", feature = "furiosa-smi-rs"))]
    devices_rs: std::sync::Mutex<RsDeviceCache>,
    /// Limits re-enumeration to once per [`DEVICE_REENUMERATE_INTERVAL`].
    #[cfg(all(target_os = "linux", feature = "furiosa-smi-rs"))]
    reenumerate_gate: crate::device::readers::common_cache::RetryGate,
}

impl Default for FuriosaNpuReader {
    fn default() -> Self {
        Self::new()
    }
}

impl FuriosaNpuReader {
    pub fn new() -> Self {
        Self::with_smi_runner(Box::new(furiosa_smi_json))
    }

    fn with_smi_runner(smi_runner: SmiRunner) -> Self {
        // Determine which collection method to use
        #[cfg(all(target_os = "linux", feature = "furiosa-smi-rs"))]
        let collection_method = CollectionMethod::FuriosaSmiRs;

        #[cfg(not(all(target_os = "linux", feature = "furiosa-smi-rs")))]
        let collection_method = CollectionMethod::FuriosaSmi;

        Self {
            collection_method,
            device_static_info_cli: OnceLock::new(),
            info_cli: CoalescedSlot::new(),
            status_cli: CoalescedSlot::new(),
            smi_runner,
            #[cfg(all(target_os = "linux", feature = "furiosa-smi-rs"))]
            devices_rs: std::sync::Mutex::new(RsDeviceCache::default()),
            #[cfg(all(target_os = "linux", feature = "furiosa-smi-rs"))]
            reenumerate_gate: crate::device::readers::common_cache::RetryGate::new(),
        }
    }

    /// Cached static device info for the CLI method, built from an already
    /// parsed `furiosa-smi info` so the first tick does not run it twice.
    ///
    /// Returns `None` until an `info` result with at least one device has
    /// been seen. An empty map is never committed to the cache: caching it
    /// after one failed or empty first `info` would hide every device for
    /// the rest of the process.
    fn get_device_static_info_cli(
        &self,
        devices: &[FuriosaSmiInfoJson],
    ) -> Option<&HashMap<String, DeviceStaticInfo>> {
        if let Some(cached) = self.device_static_info_cli.get() {
            return Some(cached);
        }
        let device_info_map = build_static_info_cli(devices);
        if device_info_map.is_empty() {
            return None;
        }
        Some(self.device_static_info_cli.get_or_init(|| device_info_map))
    }

    /// The enumerated devices of the RS method.
    ///
    /// Enumerates on the first call, and again when a per-device call has
    /// failed since (see [`Self::mark_devices_rs_stale`]), at most once per
    /// [`DEVICE_REENUMERATE_INTERVAL`]. A failed enumeration keeps the list
    /// already held.
    #[cfg(all(target_os = "linux", feature = "furiosa-smi-rs"))]
    fn get_devices_rs(&self, now: std::time::Instant) -> Option<std::sync::Arc<Vec<RsDevice>>> {
        let mut cache = self.devices_rs.lock().unwrap_or_else(|e| e.into_inner());
        let wanted = cache.devices.is_none() || cache.stale;
        if wanted && !self.reenumerate_gate.is_blocked(now) {
            // Whatever the outcome, do not enumerate again within the interval.
            self.reenumerate_gate.fail(now, DEVICE_REENUMERATE_INTERVAL);
            if let Some(devices) = enumerate_devices_rs() {
                cache.devices = Some(std::sync::Arc::new(devices));
                cache.stale = false;
            }
        }
        cache.devices.clone()
    }

    /// Note that a per-device call failed, so the handles may be outdated.
    #[cfg(all(target_os = "linux", feature = "furiosa-smi-rs"))]
    fn mark_devices_rs_stale(&self) {
        self.devices_rs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stale = true;
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
        let Some(devices) = self.get_devices_rs(std::time::Instant::now()) else {
            return Vec::new();
        };

        let time = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let hostname = get_hostname();

        let mut any_failed = false;
        let rows = devices
            .iter()
            .filter_map(|entry| {
                // Dynamic performance metrics only; identity is cached.
                let row = (|| {
                    let device = &entry.device;
                    let utilization = device.core_utilization().ok()?;
                    let temperature = device.device_temperature().ok()?;
                    let power = device.power_consumption().ok()?;
                    let governor = device.governor_profile().ok()?;
                    let core_freq = device.core_frequency().ok()?;

                    create_gpu_info_from_device_2025_cached(
                        &entry.static_info,
                        &utilization,
                        &temperature,
                        &power,
                        &governor,
                        &core_freq,
                        &time,
                        &hostname,
                    )
                })();
                any_failed |= row.is_none();
                row
            })
            .collect();
        if any_failed {
            self.mark_devices_rs_stale();
        }
        rows
    }

    /// Get NPU info using furiosa-smi command
    fn get_npu_info_cli(&self) -> Vec<GpuInfo> {
        // Get status for utilization and memory (dynamic data)
        let Some(status_list) = self.status_cli.get_or_fetch(COALESCE_WINDOW, || {
            let stdout = (self.smi_runner)("status")?;
            Some(serde_json::from_str::<Vec<FuriosaSmiStatusJson>>(&stdout).unwrap_or_default())
        }) else {
            return Vec::new();
        };

        // `info` carries the dynamic fields (temperature, power, frequency,
        // governor) and, on the first tick, the static identity as well.
        let Some(devices) = self.info_cli.get_or_fetch(COALESCE_WINDOW, || {
            let stdout = (self.smi_runner)("info")?;
            serde_json::from_str::<Vec<FuriosaSmiInfoJson>>(&stdout).ok()
        }) else {
            return Vec::new();
        };

        let Some(static_info_map) = self.get_device_static_info_cli(&devices) else {
            return Vec::new();
        };

        let time = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let hostname = get_hostname();

        devices
            .iter()
            .filter_map(|device| {
                // Use dev_name (e.g., "npu0") as the lookup key
                let static_info = static_info_map.get(&device.dev_name)?;
                // Match status by device field (RNGD) which also uses dev_name format
                let status = status_list.iter().find(|s| s.device == device.dev_name);
                create_gpu_info_from_cli_cached(static_info, device, status, &time, &hostname)
            })
            .collect()
    }

    /// Get process info using furiosa-smi ps
    fn get_process_info_internal(&self) -> Vec<ProcessInfo> {
        let stdout = match (self.smi_runner)("ps") {
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

/// Static identity per `dev_name` from one parsed `furiosa-smi info`.
fn build_static_info_cli(devices: &[FuriosaSmiInfoJson]) -> HashMap<String, DeviceStaticInfo> {
    // Use common MAX_DEVICES constant
    const MAX_DEVICES: usize = crate::device::readers::common_cache::MAX_DEVICES;
    let mut device_info_map = HashMap::new();

    for device in devices.iter().take(MAX_DEVICES) {
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

        let static_info = DeviceStaticInfo::with_details(
            format!("Furiosa {}", device.arch.to_uppercase()),
            Some(device.device_uuid.clone()),
            builder.build(),
        );

        // Use dev_name as key (e.g., "npu0") for both Warboy and RNGD
        device_info_map.insert(device.dev_name.clone(), static_info);
    }

    device_info_map
}

/// Enumerate the devices and read each one's static identity (RS method).
///
/// `None` when enumeration fails or yields no usable device, so the caller
/// keeps whatever list it already holds. A device whose `device_info()`
/// fails is left out, as it was when identity was read on every tick.
#[cfg(all(target_os = "linux", feature = "furiosa-smi-rs"))]
fn enumerate_devices_rs() -> Option<Vec<RsDevice>> {
    // Use common MAX_DEVICES constant
    const MAX_DEVICES: usize = crate::device::readers::common_cache::MAX_DEVICES;

    let devices: Vec<RsDevice> = list_devices()
        .ok()?
        .into_iter()
        .take(MAX_DEVICES)
        .filter_map(|device| {
            let info = device.device_info().ok()?;
            // Build detail HashMap using DetailBuilder
            let detail = DetailBuilder::new()
                .insert("serial_number", info.serial())
                .insert("firmware_version", info.firmware_version().to_string())
                .insert("architecture", format!("{:?}", info.arch()))
                .insert("core_count", info.core_num().to_string())
                .insert("bdf", info.bdf())
                .insert("numa_node", info.numa_node().to_string())
                // Add unified AI acceleration library labels
                .insert_lib_info("PERT", Some(&info.pert_version().to_string()))
                .build();
            let static_info = DeviceStaticInfo::with_details(
                format!("Furiosa {:?}", info.arch()),
                Some(info.uuid()),
                detail,
            );
            Some(RsDevice {
                device,
                static_info,
            })
        })
        .collect();
    (!devices.is_empty()).then_some(devices)
}

/// Cached JSON flag: "--format" for RNGD, "--output" for Warboy.
/// Detected once on first successful call.
static FURIOSA_JSON_FLAG: OnceLock<&str> = OnceLock::new();

/// Run furiosa-smi subcommand with JSON output.
/// Auto-detects and caches the correct flag (--format for RNGD, --output for Warboy).
/// On the first call the probe result is reused to avoid a redundant command execution.
fn furiosa_smi_json(subcommand: &str) -> Option<String> {
    // Fast path: flag already detected, just run the command.
    if let Some(flag) = FURIOSA_JSON_FLAG.get() {
        let output = execute_command_default("furiosa-smi", &[subcommand, flag, "json"]).ok()?;
        return if output.status == 0 && !output.stdout.is_empty() {
            Some(output.stdout)
        } else {
            None
        };
    }

    // Slow path (first call only): probe with --format first (RNGD), then --output (Warboy).
    // Reuse the successful probe result directly instead of discarding it.
    if let Ok(output) = execute_command_default("furiosa-smi", &[subcommand, "--format", "json"])
        && output.status == 0
        && !output.stdout.is_empty()
    {
        let _ = FURIOSA_JSON_FLAG.set("--format");
        return Some(output.stdout);
    }

    // Fall back to --output (Warboy)
    let _ = FURIOSA_JSON_FLAG.set("--output");
    let output = execute_command_default("furiosa-smi", &[subcommand, "--output", "json"]).ok()?;
    if output.status == 0 && !output.stdout.is_empty() {
        Some(output.stdout)
    } else {
        None
    }
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
        // Furiosa RNGD exposes temperature only; no NVML thermal threshold,
        // P-state, or NVIDIA hardware detail (NUMA/GSP/NvLink/GPM)
        // equivalents exist for this accelerator.
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
///
/// The eight parameters are the cached static block plus the five dynamic
/// samples one poll produces. Bundling them into a struct would change the
/// polling shape, which is a separate refactor (issue #479).
#[cfg(all(target_os = "linux", feature = "furiosa-smi-rs"))]
#[allow(clippy::too_many_arguments)]
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
        // Furiosa RNGD exposes temperature only; no NVML thermal threshold,
        // P-state, or NVIDIA hardware detail (NUMA/GSP/NvLink/GPM)
        // equivalents exist for this accelerator.
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
    })
}

fn create_process_info_from_ps(proc: &FuriosaPsOutputJson) -> ProcessInfo {
    // Extract the first device name from potentially complex RNGD format
    let first_device = extract_first_device_name(&proc.npu);
    let device_id = parse_device_id(first_device).unwrap_or_else(|| {
        eprintln!("Failed to parse device ID: {}", proc.npu);
        0
    });

    // Parse memory when available (Warboy provides it, RNGD does not)
    let used_memory = if proc.memory.is_empty() {
        0
    } else {
        parse_memory_mb_to_bytes(&proc.memory).unwrap_or(0)
    };

    ProcessInfo {
        device_id,
        device_uuid: proc.npu.clone(),
        pid: proc.pid,
        process_name: extract_process_name(&proc.cmd),
        used_memory,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    /// `furiosa-smi info --format json` for one RNGD, trimmed to the fields
    /// the reader deserializes.
    const INFO_JSON: &str = r#"[{"arch":"rngd","dev_name":"npu0",
        "device_uuid":"09512C86-0702-4303-8F43-434445434443",
        "device_sn":"RNGDXXXXXXXX","firmware":"2025.3.0","temperature":"39.50C",
        "power":"38.00 W","core_clock":"500 MHz","governor":"OnDemand",
        "pci_bdf":"0000:17:00.0","pci_dev":"507:0"}]"#;

    const STATUS_JSON: &str = r#"[{"arch":"rngd","device":"npu0","liveness":"alive",
        "pe_utilizations":[{"pe_core":0,"pe_utilization":12.5}]}]"#;

    type CallLog = Arc<Mutex<Vec<String>>>;

    /// A reader whose `furiosa-smi` is a fake that logs each subcommand and
    /// answers `info` from `info_answers` in order (the last one repeats).
    fn fake_reader(info_answers: Vec<Option<&'static str>>) -> (FuriosaNpuReader, CallLog) {
        let calls: CallLog = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&calls);
        let answers = Mutex::new(VecDeque::from(info_answers));
        let mut reader = FuriosaNpuReader::with_smi_runner(Box::new(move |subcommand: &str| {
            log.lock().unwrap().push(subcommand.to_string());
            match subcommand {
                "status" => Some(STATUS_JSON.to_string()),
                "info" => {
                    let mut answers = answers.lock().unwrap();
                    let answer = if answers.len() > 1 {
                        answers.pop_front().flatten()
                    } else {
                        answers.front().copied().flatten()
                    };
                    answer.map(str::to_string)
                }
                _ => None,
            }
        }));
        // These tests drive the CLI path whatever the build's default is.
        reader.collection_method = CollectionMethod::FuriosaSmi;
        (reader, calls)
    }

    /// Freeze both coalescing slots of `reader` at `now`.
    fn set_now(reader: &FuriosaNpuReader, now: std::time::Instant) {
        reader.info_cli.set_test_now(now);
        reader.status_cli.set_test_now(now);
    }

    fn count(calls: &CallLog, subcommand: &str) -> usize {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.as_str() == subcommand)
            .count()
    }

    #[test]
    fn first_tick_runs_info_once_and_status_once() {
        let (reader, calls) = fake_reader(vec![Some(INFO_JSON)]);

        let rows = reader.get_gpu_info();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Furiosa RNGD");
        assert_eq!(rows[0].uuid, "09512C86-0702-4303-8F43-434445434443");
        assert_eq!(rows[0].utilization, 12.5);
        assert_eq!(count(&calls, "info"), 1);
        assert_eq!(count(&calls, "status"), 1);
    }

    #[test]
    fn a_repeat_within_the_window_does_not_spawn_again() {
        let (reader, calls) = fake_reader(vec![Some(INFO_JSON)]);
        let t0 = std::time::Instant::now();

        set_now(&reader, t0);
        assert_eq!(reader.get_gpu_info().len(), 1);
        set_now(
            &reader,
            t0 + COALESCE_WINDOW - std::time::Duration::from_millis(1),
        );
        assert_eq!(reader.get_gpu_info().len(), 1);

        assert_eq!(count(&calls, "info"), 1);
        assert_eq!(count(&calls, "status"), 1);

        // The next tick spawns both again.
        set_now(&reader, t0 + COALESCE_WINDOW);
        assert_eq!(reader.get_gpu_info().len(), 1);
        assert_eq!(count(&calls, "info"), 2);
        assert_eq!(count(&calls, "status"), 2);
    }

    #[test]
    fn a_failed_first_info_does_not_hide_devices_for_good() {
        let (reader, calls) = fake_reader(vec![None, Some(INFO_JSON)]);
        // Both calls in one window: only the failed `info` is retried.
        set_now(&reader, std::time::Instant::now());

        assert!(reader.get_gpu_info().is_empty());
        // The failed `info` cached neither a result nor an empty static map.
        assert!(reader.device_static_info_cli.get().is_none());

        let rows = reader.get_gpu_info();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Furiosa RNGD");
        assert_eq!(count(&calls, "info"), 2);
        assert_eq!(count(&calls, "status"), 1);
    }

    #[test]
    fn an_empty_first_info_is_not_cached_as_the_static_map() {
        let (reader, _calls) = fake_reader(vec![Some("[]"), Some(INFO_JSON)]);

        assert!(reader.get_gpu_info().is_empty());
        assert!(reader.device_static_info_cli.get().is_none());
    }

    #[test]
    fn test_extract_first_device_name_rngd_format() {
        assert_eq!(
            extract_first_device_name("npu4:[0, 7], npu5:[0, 7]"),
            "npu4"
        );
    }

    #[test]
    fn test_extract_first_device_name_legacy_format() {
        assert_eq!(extract_first_device_name("npu0"), "npu0");
    }

    #[test]
    fn test_extract_first_device_name_single_rngd() {
        assert_eq!(extract_first_device_name("npu2:[0, 3]"), "npu2");
    }

    #[test]
    fn test_extract_process_name_full_path() {
        assert_eq!(extract_process_name("/usr/bin/python3 train.py"), "python3");
    }

    #[test]
    fn test_extract_process_name_simple() {
        assert_eq!(extract_process_name("train"), "train");
    }

    #[test]
    fn test_extract_process_name_empty() {
        assert_eq!(extract_process_name(""), "unknown");
    }
}
