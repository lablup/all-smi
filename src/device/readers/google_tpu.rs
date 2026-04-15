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

//! Google TPU (Tensor Processing Unit) reader module.
//!
//! This module provides support for monitoring Google Cloud TPU accelerators.
//! TPUs are custom-designed ASICs developed by Google for accelerating machine
//! learning workloads.
//!
//! # Platform Detection
//!
//! TPU devices are detected by:
//! - Checking for `/dev/accel*` device files
//! - Checking for libtpu library availability
//! - Verifying TPU device presence via sysfs
//!
//! # Supported TPU Generations
//!
//! | Generation | Codename | HBM | Notes |
//! |------------|----------|-----|-------|
//! | TPU v2 | - | 8 GB | Legacy |
//! | TPU v3 | - | 16 GB | Legacy |
//! | TPU v4 | - | 32 GB | Production |
//! | TPU v5e | - | 16 GB | Cost-optimized |
//! | TPU v5p | - | 95 GB | High performance |
//! | TPU v6 | Trillium | 32 GB | 4.7x v5e performance |
//! | TPU v7 | Ironwood | 192 GB | Latest generation, HBM3e |

use crate::device::GpuReader;
#[cfg(target_os = "linux")]
use crate::device::common::constants::google_tpu::is_libtpu_available;
#[cfg(target_os = "linux")]
use crate::device::readers::common_cache::{DetailBuilder, DeviceStaticInfo};
#[cfg(target_os = "linux")]
use crate::device::readers::tpu_grpc;
#[cfg(target_os = "linux")]
use crate::device::readers::tpu_info_runner;
#[cfg(target_os = "linux")]
use crate::device::readers::tpu_sysfs;
use crate::device::types::{GpuInfo, ProcessInfo};
#[cfg(target_os = "linux")]
use crate::utils::get_hostname;
#[cfg(target_os = "linux")]
use chrono::Local;
#[cfg(target_os = "linux")]
use once_cell::sync::Lazy;
#[cfg(target_os = "linux")]
use serde::Deserialize;
#[cfg(target_os = "linux")]
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::sync::{Arc, Mutex, OnceLock};

/// TPU generation enumeration with specifications
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg(target_os = "linux")]
pub enum TpuGeneration {
    V2,
    V3,
    V4,
    V5e,
    V5p,
    V6e,        // Cost-optimized v6 (16 GB HBM)
    V6Trillium, // Full v6 Trillium (32 GB HBM)
    V7Ironwood,
    V7x,
    Unknown,
}

#[cfg(target_os = "linux")]
impl TpuGeneration {
    /// Get HBM memory size in bytes for each TPU generation
    pub fn hbm_size_bytes(&self) -> u64 {
        match self {
            TpuGeneration::V2 => 8 * 1024 * 1024 * 1024,   // 8 GB
            TpuGeneration::V3 => 16 * 1024 * 1024 * 1024,  // 16 GB
            TpuGeneration::V4 => 32 * 1024 * 1024 * 1024,  // 32 GB
            TpuGeneration::V5e => 16 * 1024 * 1024 * 1024, // 16 GB
            TpuGeneration::V5p => 95 * 1024 * 1024 * 1024, // 95 GB
            TpuGeneration::V6e => 16 * 1024 * 1024 * 1024, // 16 GB (cost-optimized)
            TpuGeneration::V6Trillium => 32 * 1024 * 1024 * 1024, // 32 GB
            TpuGeneration::V7Ironwood => 192 * 1024 * 1024 * 1024, // 192 GB HBM3e
            TpuGeneration::V7x => 192 * 1024 * 1024 * 1024, // 192 GB
            TpuGeneration::Unknown => 16 * 1024 * 1024 * 1024, // Default 16 GB
        }
    }

    /// Get TensorCore count for each TPU generation
    pub fn tensor_cores(&self) -> u32 {
        match self {
            TpuGeneration::V2 => 2,
            TpuGeneration::V3 => 2,
            TpuGeneration::V4 => 2,
            TpuGeneration::V5e => 1,
            TpuGeneration::V5p => 2,
            TpuGeneration::V6e => 1, // Cost-optimized, single core
            TpuGeneration::V6Trillium => 2,
            TpuGeneration::V7Ironwood => 2, // Estimated based on architecture
            TpuGeneration::V7x => 2,
            TpuGeneration::Unknown => 1,
        }
    }

    /// Get human-readable name for the TPU generation
    pub fn display_name(&self) -> &'static str {
        match self {
            TpuGeneration::V2 => "Google TPU v2",
            TpuGeneration::V3 => "Google TPU v3",
            TpuGeneration::V4 => "Google TPU v4",
            TpuGeneration::V5e => "Google TPU v5e",
            TpuGeneration::V5p => "Google TPU v5p",
            TpuGeneration::V6e => "Google TPU v6e",
            TpuGeneration::V6Trillium => "Google TPU v6 Trillium",
            TpuGeneration::V7Ironwood => "Google TPU v7 Ironwood 192GB HBM3e",
            TpuGeneration::V7x => "Google TPU v7x",
            TpuGeneration::Unknown => "Google TPU",
        }
    }

    /// Get memory type string for the TPU generation
    pub fn memory_type(&self) -> &'static str {
        match self {
            TpuGeneration::V7Ironwood | TpuGeneration::V7x => "HBM3e",
            TpuGeneration::V5p | TpuGeneration::V6e | TpuGeneration::V6Trillium => "HBM2e",
            _ => "HBM2",
        }
    }

    /// Parse TPU generation from chip version string
    pub fn from_chip_version(version: &str) -> Self {
        let version_lower = version.to_lowercase();
        if version_lower.contains("v7x") || version_lower.contains("7x") {
            TpuGeneration::V7x
        } else if version_lower.contains("v7") || version_lower.contains("ironwood") {
            TpuGeneration::V7Ironwood
        } else if version_lower.contains("v6e") {
            // Must check v6e before v6 to avoid false positive
            TpuGeneration::V6e
        } else if version_lower.contains("v6") || version_lower.contains("trillium") {
            TpuGeneration::V6Trillium
        } else if version_lower.contains("v5p") {
            TpuGeneration::V5p
        } else if version_lower.contains("v5e") || version_lower.contains("v5lite") {
            TpuGeneration::V5e
        } else if version_lower.contains("v4") {
            TpuGeneration::V4
        } else if version_lower.contains("v3") || version_lower.contains("v2/v3") {
            TpuGeneration::V3
        } else if version_lower.contains("v2") {
            TpuGeneration::V2
        } else {
            TpuGeneration::Unknown
        }
    }
}

/// JSON structure for TPU device information from Python/CLI output
#[derive(Debug, Deserialize)]
#[cfg(target_os = "linux")]
struct TpuDeviceInfo {
    /// Device index (0, 1, 2, ...)
    index: u32,
    /// Chip version (e.g., "v4", "v5e", "v5p", "v6", "v7")
    #[serde(default)]
    chip_version: String,
    /// Device UUID
    #[serde(default)]
    uuid: String,
    /// Core count per chip
    #[serde(default)]
    core_count: u32,
    /// Current utilization percentage (0-100) - duty cycle
    #[serde(default)]
    utilization: f64,
    /// TensorCore utilization percentage (0-100)
    #[serde(default)]
    tensorcore_utilization: f64,
    /// HBM memory used in bytes
    #[serde(default)]
    memory_used: u64,
    /// HBM memory total in bytes
    #[serde(default)]
    memory_total: u64,
    /// Current temperature in Celsius
    #[serde(default)]
    temperature: u32,
    /// Power consumption in Watts
    #[serde(default)]
    power_draw: f64,
    /// Maximum power limit in Watts
    #[serde(default)]
    power_max: f64,
    /// TPU runtime version
    #[serde(default)]
    tpu_runtime_version: String,
    /// Accelerator type string
    #[serde(default)]
    accelerator_type: String,
    /// HLO Queue Size (number of pending programs)
    #[serde(default)]
    hlo_queue_size: Option<i64>,
    /// HLO Execution Timing - mean (microseconds)
    #[serde(default)]
    hlo_exec_mean_us: Option<f64>,
    /// HLO Execution Timing - p50 (microseconds)
    #[serde(default)]
    hlo_exec_p50_us: Option<f64>,
    /// HLO Execution Timing - p90 (microseconds)
    #[serde(default)]
    hlo_exec_p90_us: Option<f64>,
    /// HLO Execution Timing - p95 (microseconds)
    #[serde(default)]
    hlo_exec_p95_us: Option<f64>,
    /// HLO Execution Timing - p99.9 (microseconds)
    #[serde(default)]
    hlo_exec_p999_us: Option<f64>,
}

/// JSON structure for TPU process information
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[cfg(target_os = "linux")]
struct TpuProcessInfo {
    /// Device index
    device_index: u32,
    /// Process ID
    pid: u32,
    /// Command line
    #[serde(default)]
    command: String,
    /// Memory used by the process
    #[serde(default)]
    memory_used: u64,
}

/// Static metadata for a TPU device (does not change during runtime)
#[derive(Debug, Clone)]
#[cfg(target_os = "linux")]
struct TpuDeviceMetadata {
    index: u32,
    chip_version: String,
    uuid: String,
    core_count: u32,
    memory_total: u64,
    accelerator_type: String,
    #[allow(dead_code)]
    source: String,
}

/// Cache for TPU Python script availability
#[cfg(target_os = "linux")]
#[allow(dead_code)]
static TPU_SCRIPT_AVAILABLE: Lazy<Arc<Mutex<Option<bool>>>> =
    Lazy::new(|| Arc::new(Mutex::new(None)));

/// Google TPU Reader implementation
pub struct GoogleTpuReader {
    /// Cached static device information per UUID
    #[cfg(target_os = "linux")]
    device_static_info: OnceLock<HashMap<String, DeviceStaticInfo>>,
    /// Cached detected device metadata (to avoid re-scanning filesystem)
    #[cfg(target_os = "linux")]
    device_metadata: OnceLock<Vec<TpuDeviceMetadata>>,
}

impl Default for GoogleTpuReader {
    fn default() -> Self {
        Self::new()
    }
}

impl GoogleTpuReader {
    pub fn new() -> Self {
        #[cfg(target_os = "linux")]
        {
            // Initialize background runner for tpu-info streaming
            tpu_info_runner::get_runner();
        }

        Self {
            #[cfg(target_os = "linux")]
            device_static_info: OnceLock::new(),
            #[cfg(target_os = "linux")]
            device_metadata: OnceLock::new(),
        }
    }

    /// Initialize static device cache on first access
    #[cfg(target_os = "linux")]
    fn ensure_static_cache_initialized(&self, devices: &[TpuDeviceInfo]) {
        self.device_static_info.get_or_init(|| {
            let mut device_map = HashMap::new();
            const MAX_DEVICES: usize = crate::device::readers::common_cache::MAX_DEVICES;
            let devices_to_process: Vec<_> = devices.iter().take(MAX_DEVICES).collect();

            for device in devices_to_process {
                let generation = TpuGeneration::from_chip_version(&device.chip_version);

                // Build detail HashMap using DetailBuilder
                let detail = DetailBuilder::new()
                    .insert("Device Index", device.index.to_string())
                    .insert("Chip Version", &device.chip_version)
                    .insert("Accelerator Type", &device.accelerator_type)
                    .insert("Core Count", device.core_count.to_string())
                    .insert("TensorCore Count", generation.tensor_cores().to_string())
                    .insert("Memory Type", generation.memory_type())
                    .insert(
                        "Total Memory",
                        format_memory_size(generation.hbm_size_bytes()),
                    )
                    .insert("Max Power", format!("{:.0} W", device.power_max))
                    .insert_optional(
                        "TPU Runtime Version",
                        if device.tpu_runtime_version.is_empty() {
                            None
                        } else {
                            Some(&device.tpu_runtime_version)
                        },
                    )
                    // Add unified AI acceleration library labels
                    .insert("lib_name", "libtpu")
                    .insert_optional(
                        "lib_version",
                        if device.tpu_runtime_version.is_empty() {
                            None
                        } else {
                            Some(&device.tpu_runtime_version)
                        },
                    )
                    .build();

                let uuid = if device.uuid.is_empty() {
                    format!("TPU-{}", device.index)
                } else {
                    device.uuid.clone()
                };

                let static_info = DeviceStaticInfo::with_details(
                    generation.display_name().to_string(),
                    Some(uuid.clone()),
                    detail,
                );

                device_map.insert(uuid, static_info);
            }
            device_map
        });
    }

    /// Get cached static device info
    #[cfg(target_os = "linux")]
    fn get_device_static_info(&self, uuid: &str) -> Option<&DeviceStaticInfo> {
        self.device_static_info.get().and_then(|map| map.get(uuid))
    }

    /// Get TPU info using gRPC (primary) or CLI fallback
    #[cfg(target_os = "linux")]
    fn get_tpu_info_internal(&self) -> Vec<GpuInfo> {
        // Perform device discovery once and cache the result
        let metadata_list = self.device_metadata.get_or_init(Self::discover_tpu_devices);

        if metadata_list.is_empty() {
            return Vec::new();
        }

        // Try gRPC first (native, faster when TPU workload is running)
        let grpc_metrics = tpu_grpc::get_tpu_metrics_grpc_sync();

        // Fetch HLO metrics via gRPC
        let hlo_queue_sizes = tpu_grpc::get_hlo_queue_size_sync();
        let hlo_exec_timings = tpu_grpc::get_hlo_execution_timing_sync();

        // Construct current device info by combining static metadata with dynamic metrics
        let runner = tpu_info_runner::get_runner();
        let devices: Vec<TpuDeviceInfo> = metadata_list
            .iter()
            .map(|meta| {
                let mut utilization = 0.0;
                let mut tensorcore_utilization = 0.0;
                let mut memory_used = 0;
                let mut total_memory = meta.memory_total;
                let mut power_draw = 0.0;

                // Try gRPC metrics first for memory and duty cycle
                if let Some(ref metrics) = grpc_metrics {
                    if let Some(grpc_data) =
                        metrics.iter().find(|m| m.device_id == meta.index as i64)
                    {
                        // gRPC provides duty_cycle which we use as main utilization
                        utilization = grpc_data.duty_cycle_pct;
                        memory_used = grpc_data.memory_usage;
                        if grpc_data.total_memory > 0 {
                            total_memory = grpc_data.total_memory;
                        }
                    }
                } else {
                    // Fallback to CLI-based metrics for memory when gRPC not available
                    if let Some(val) = runner.get_metric(meta.index, "duty_cycle_percent") {
                        utilization = val;
                    }

                    if let Some(val) = runner.get_metric(meta.index, "hbm_usage") {
                        memory_used = val as u64;
                    }

                    if let Some(val) = runner.get_metric(meta.index, "memory_total")
                        && val > 0.0
                    {
                        total_memory = val as u64;
                    }
                }

                // TensorCore utilization comes from CLI (libtpu SDK monitoring module)
                // This is a separate metric from duty_cycle, retrieved via tpu-info CLI
                if let Some(val) = runner.get_metric(meta.index, "tensorcore_utilization") {
                    tensorcore_utilization = val;
                }

                // If no duty cycle but we have tensorcore, use it as main utilization
                if utilization == 0.0 && tensorcore_utilization > 0.0 {
                    utilization = tensorcore_utilization;
                }

                // Power metrics (only available via CLI for now)
                if let Some(val) = runner.get_metric(meta.index, "power_usage") {
                    power_draw = val;
                }

                // HLO Queue Size
                let hlo_queue_size = hlo_queue_sizes
                    .as_ref()
                    .and_then(|sizes| sizes.iter().find(|s| s.device_id == meta.index as i64))
                    .map(|s| s.queue_size);

                // HLO Execution Timing
                let (
                    hlo_exec_mean_us,
                    hlo_exec_p50_us,
                    hlo_exec_p90_us,
                    hlo_exec_p95_us,
                    hlo_exec_p999_us,
                ) = hlo_exec_timings
                    .as_ref()
                    .and_then(|timings| timings.iter().find(|t| t.device_id == meta.index as i64))
                    .map(|t| {
                        (
                            Some(t.mean_us),
                            Some(t.p50_us),
                            Some(t.p90_us),
                            Some(t.p95_us),
                            Some(t.p999_us),
                        )
                    })
                    .unwrap_or((None, None, None, None, None));

                TpuDeviceInfo {
                    index: meta.index,
                    chip_version: meta.chip_version.clone(),
                    uuid: meta.uuid.clone(),
                    core_count: meta.core_count,
                    utilization,
                    tensorcore_utilization,
                    memory_used,
                    memory_total: total_memory,
                    temperature: 0,
                    power_draw,
                    power_max: 0.0,
                    tpu_runtime_version: String::new(),
                    accelerator_type: meta.accelerator_type.clone(),
                    hlo_queue_size,
                    hlo_exec_mean_us,
                    hlo_exec_p50_us,
                    hlo_exec_p90_us,
                    hlo_exec_p95_us,
                    hlo_exec_p999_us,
                }
            })
            .collect();

        // Initialize static cache on first call
        self.ensure_static_cache_initialized(&devices);

        let time = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let hostname = get_hostname();

        devices
            .into_iter()
            .filter_map(|device| {
                let uuid = if device.uuid.is_empty() {
                    format!("TPU-{}", device.index)
                } else {
                    device.uuid.clone()
                };
                let static_info = self.get_device_static_info(&uuid);
                create_gpu_info_from_device(device, static_info, &time, &hostname)
            })
            .collect()
    }

    /// Discover TPU devices (sysfs, vfio, env) - Run only once!
    #[cfg(target_os = "linux")]
    fn discover_tpu_devices() -> Vec<TpuDeviceMetadata> {
        let mut metadata = Vec::new();

        // 1. Sysfs scan (Most reliable for hardware presence)
        let sysfs_devices = tpu_sysfs::scan_sysfs_tpus();
        if !sysfs_devices.is_empty() {
            for sys_dev in sysfs_devices {
                let chip_version = Self::detect_tpu_version_from_device_id(&sys_dev.device_id);
                let accel_type = format!("TPU {}", &chip_version);
                let generation = TpuGeneration::from_chip_version(&chip_version);

                metadata.push(TpuDeviceMetadata {
                    index: sys_dev.index,
                    chip_version,
                    uuid: format!("TPU-{}", sys_dev.index),
                    core_count: 1,
                    memory_total: generation.hbm_size_bytes(),
                    accelerator_type: accel_type,
                    source: "sysfs".to_string(),
                });
            }
        }

        // Method 3: Check /dev/vfio/* for v6e and newer TPUs (if not found via sysfs)
        if metadata.is_empty()
            && let Some(vfio_metadata) = Self::detect_tpu_metadata_from_vfio()
        {
            metadata = vfio_metadata;
        }

        // Method 4: For TPU VMs, use environment variables
        if metadata.is_empty()
            && let Some(env_metadata) = Self::detect_tpu_metadata_from_environment()
        {
            metadata.push(env_metadata);
        }

        metadata
    }

    /// Detect TPU devices via /dev/vfio/* (used by v6e and newer)
    #[cfg(target_os = "linux")]
    fn detect_tpu_metadata_from_vfio() -> Option<Vec<TpuDeviceMetadata>> {
        let vfio_path = std::path::Path::new("/dev/vfio");
        if !vfio_path.exists() {
            return None;
        }

        // Count vfio devices (excluding "vfio" control device and "devices" directory)
        let mut vfio_count = 0;
        if let Ok(entries) = std::fs::read_dir(vfio_path) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                // /dev/vfio/0, /dev/vfio/1, etc. are TPU devices
                // /dev/vfio/vfio is the control device
                // /dev/vfio/devices is a directory
                if name.parse::<u32>().is_ok() {
                    vfio_count += 1;
                }
            }
        }

        if vfio_count == 0 {
            return None;
        }

        // Check if this looks like a TPU VM:
        // 1. tpu-info CLI can detect it (most reliable)
        // 2. TPU environment variables are set
        // 3. libtpu.so exists in known locations
        let chip_version_from_cli = Self::get_accelerator_type_from_tpu_info();
        let chip_version_from_env = std::env::var("TPU_ACCELERATOR_TYPE").ok();
        let has_tpu_env = std::env::var("TPU_CHIPS_PER_HOST_BOUNDS").is_ok()
            || std::env::var("TPU_HOST_BOUNDS").is_ok()
            || std::env::var("TPU_NAME").is_ok();
        let has_libtpu = is_libtpu_available();

        // Determine if this is actually a TPU VM
        let is_tpu_vm = chip_version_from_cli.is_some()
            || chip_version_from_env.is_some()
            || has_tpu_env
            || has_libtpu;

        if !is_tpu_vm {
            return None;
        }

        // Get the best available version info
        let chip_version = chip_version_from_cli
            .or(chip_version_from_env)
            .unwrap_or_else(|| "unknown".to_string());

        let generation = TpuGeneration::from_chip_version(&chip_version);

        // Create device entries for each vfio device
        let devices: Vec<TpuDeviceMetadata> = (0..vfio_count)
            .map(|i| {
                let accel_type = format!("TPU {}", &chip_version);
                TpuDeviceMetadata {
                    index: i,
                    chip_version: chip_version.clone(),
                    uuid: format!("TPU-{i}"),
                    core_count: 1,
                    memory_total: generation.hbm_size_bytes(),
                    accelerator_type: accel_type,
                    source: "vfio".to_string(),
                }
            })
            .collect();

        Some(devices)
    }

    /// Detect TPU version from PCI device ID
    #[cfg(target_os = "linux")]
    fn detect_tpu_version_from_device_id(device_id: &str) -> String {
        // Google TPU PCI device IDs (mappings from tpu-info/device.py)
        match device_id.to_lowercase().as_str() {
            "0x0027" => "v2/v3".to_string(),
            "0x005e" => "v4".to_string(),
            "0x0063" => "v5e".to_string(),
            "0x0062" => "v5p".to_string(),
            "0x006f" => "v6e".to_string(),
            "0x0076" => "v7x".to_string(),
            // Fallbacks for IDs that might appear in sysfs without 0x or with variations
            "005e" => "v4".to_string(),
            "0063" => "v5e".to_string(),
            "0062" => "v5p".to_string(),
            "006f" => "v6e".to_string(),
            "0076" => "v7x".to_string(),
            _ => "unknown".to_string(),
        }
    }

    /// Try to get accelerator type from tpu-info CLI (fast, no heavy imports)
    #[cfg(target_os = "linux")]
    fn get_accelerator_type_from_tpu_info() -> Option<String> {
        use std::process::Command;
        static ACCELERATOR_TYPE_CACHE: OnceLock<Option<String>> = OnceLock::new();

        ACCELERATOR_TYPE_CACHE
            .get_or_init(|| {
                // tpu-info -v is fast and outputs:
                // - tpu-info version: 0.8.0
                // - libtpu version: 0.0.17
                // - accelerator type: v6e
                let output = Command::new("tpu-info").arg("-v").output().ok()?;

                if !output.status.success() {
                    return None;
                }

                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines() {
                    if line.contains("accelerator type:") {
                        // Extract the type after the colon
                        if let Some(type_str) = line.split(':').nth(1) {
                            return Some(type_str.trim().to_string());
                        }
                    }
                }
                None
            })
            .clone()
    }

    /// Detect TPU info from TPU VM environment variables
    #[cfg(target_os = "linux")]
    fn detect_tpu_metadata_from_environment() -> Option<TpuDeviceMetadata> {
        // Check if we're on a TPU VM
        let tpu_name = std::env::var("TPU_NAME").ok();
        let accelerator_type = std::env::var("TPU_ACCELERATOR_TYPE").ok();
        let chips_per_host = std::env::var("TPU_CHIPS_PER_HOST_BOUNDS").ok();

        // At least one TPU environment variable must be set
        if tpu_name.is_none() && accelerator_type.is_none() && chips_per_host.is_none() {
            // Also check for worker-related variables
            if std::env::var("TPU_WORKER_ID").is_err()
                && std::env::var("TPU_WORKER_HOSTNAMES").is_err()
            {
                return None;
            }
        }

        // Parse accelerator type to determine TPU version
        // Priority: 1) tpu-info CLI, 2) TPU_ACCELERATOR_TYPE env var
        let chip_version = Self::get_accelerator_type_from_tpu_info()
            .or_else(|| {
                accelerator_type
                    .as_ref()
                    .map(|t| Self::parse_accelerator_type(t))
            })
            .unwrap_or_else(|| "unknown".to_string());

        let generation = TpuGeneration::from_chip_version(&chip_version);

        // Parse number of chips from TPU_CHIPS_PER_HOST_BOUNDS (format: "x,y,z")
        let chip_count = chips_per_host
            .as_ref()
            .map(|s| {
                let parts: Vec<u32> = s.split(',').filter_map(|p| p.trim().parse().ok()).collect();
                if parts.len() == 3 {
                    parts[0] * parts[1] * parts[2]
                } else {
                    1
                }
            })
            .unwrap_or(1);

        // Create device info for each chip
        // For simplicity, we report the total as one "device" since we can't get per-chip metrics
        let accel_type = format!("TPU {}", &chip_version);
        let device = TpuDeviceMetadata {
            index: 0,
            chip_version,
            uuid: tpu_name.unwrap_or_else(|| "TPU-VM".to_string()),
            core_count: chip_count,
            memory_total: generation.hbm_size_bytes() * chip_count as u64,
            accelerator_type: accel_type,
            source: "env".to_string(),
        };

        Some(device)
    }

    /// Parse TPU accelerator type string (e.g., "v6e-16", "v5litepod-4")
    #[cfg(target_os = "linux")]
    fn parse_accelerator_type(accel_type: &str) -> String {
        let lower = accel_type.to_lowercase();

        if lower.contains("v7") || lower.contains("ironwood") {
            "v7".to_string()
        } else if lower.contains("v6e") {
            // Must check v6e before v6 to avoid false positive
            "v6e".to_string()
        } else if lower.contains("v6") || lower.contains("trillium") {
            "v6".to_string()
        } else if lower.contains("v5p") {
            "v5p".to_string()
        } else if lower.contains("v5e") || lower.contains("v5lite") {
            "v5e".to_string()
        } else if lower.contains("v4") {
            "v4".to_string()
        } else if lower.contains("v3") {
            "v3".to_string()
        } else if lower.contains("v2") {
            "v2".to_string()
        } else {
            // Return the original type if we can't parse it
            accel_type.to_string()
        }
    }

    /// Validate TPU device data schema
    #[cfg(target_os = "linux")]
    #[allow(dead_code)]
    fn validate_tpu_device_schema(devices: &[TpuDeviceInfo]) -> Option<()> {
        if devices.is_empty() {
            return None;
        }

        // Perform basic validation on the device data
        for device in devices {
            // Ensure utilization is within valid range
            if !(0.0..=100.0).contains(&device.utilization) {
                return None;
            }

            // Ensure memory values are reasonable
            if device.memory_used > device.memory_total && device.memory_total > 0 {
                return None;
            }

            // Ensure power values are non-negative
            if device.power_draw < 0.0 || device.power_max < 0.0 {
                return None;
            }

            // Ensure temperature is in a reasonable range (0-200 C)
            if device.temperature > 200 {
                return None;
            }
        }

        Some(())
    }

    /// Get TPU process information
    #[cfg(target_os = "linux")]
    fn get_process_info_internal(&self) -> Vec<ProcessInfo> {
        // TPU process tracking would require integration with cloud-tpu-diagnostics
        // or parsing /proc for processes using /dev/accel* devices
        // For now, return empty as TPU process info is not readily available
        Vec::new()
    }
}

impl GpuReader for GoogleTpuReader {
    fn get_gpu_info(&self) -> Vec<GpuInfo> {
        #[cfg(target_os = "linux")]
        {
            self.get_tpu_info_internal()
        }
        #[cfg(not(target_os = "linux"))]
        {
            Vec::new()
        }
    }

    fn get_process_info(&self) -> Vec<ProcessInfo> {
        #[cfg(target_os = "linux")]
        {
            self.get_process_info_internal()
        }
        #[cfg(not(target_os = "linux"))]
        {
            Vec::new()
        }
    }
}

// Helper functions

#[cfg(target_os = "linux")]
pub fn get_tpu_status_message() -> Option<String> {
    tpu_info_runner::get_runner().get_status()
}

#[cfg(not(target_os = "linux"))]
pub fn get_tpu_status_message() -> Option<String> {
    None
}

#[cfg(target_os = "linux")]
fn format_memory_size(bytes: u64) -> String {
    const GB: u64 = 1024 * 1024 * 1024;
    const MB: u64 = 1024 * 1024;

    if bytes >= GB {
        let gb = bytes / GB;
        format!("{gb} GB")
    } else if bytes >= MB {
        let mb = bytes / MB;
        format!("{mb} MB")
    } else {
        format!("{bytes} B")
    }
}

#[cfg(target_os = "linux")]
fn create_gpu_info_from_device(
    device: TpuDeviceInfo,
    static_info: Option<&DeviceStaticInfo>,
    time: &str,
    hostname: &str,
) -> Option<GpuInfo> {
    let generation = TpuGeneration::from_chip_version(&device.chip_version);

    // Use cached static info if available, otherwise build from current device data
    let (uuid, name, mut detail) = if let Some(info) = static_info {
        let uuid = info
            .uuid
            .clone()
            .unwrap_or_else(|| format!("TPU-{}", device.index));
        (uuid, info.name.clone(), info.detail.clone())
    } else {
        // Build detail HashMap if no cache available (first call)
        let detail = DetailBuilder::new()
            .insert("Device Index", device.index.to_string())
            .insert("Chip Version", &device.chip_version)
            .insert("Accelerator Type", &device.accelerator_type)
            .insert("Core Count", device.core_count.to_string())
            .insert("TensorCore Count", generation.tensor_cores().to_string())
            .insert("Memory Type", generation.memory_type())
            .insert(
                "Total Memory",
                format_memory_size(generation.hbm_size_bytes()),
            )
            .insert("Max Power", format!("{:.0} W", device.power_max))
            // Add unified AI acceleration library labels
            .insert("lib_name", "libtpu")
            .insert_optional(
                "lib_version",
                if device.tpu_runtime_version.is_empty() {
                    None
                } else {
                    Some(&device.tpu_runtime_version)
                },
            )
            .build();

        let uuid = if device.uuid.is_empty() {
            format!("TPU-{}", device.index)
        } else {
            device.uuid.clone()
        };

        (uuid, generation.display_name().to_string(), detail)
    };

    // Dynamic values - update with current readings
    detail.insert(
        "Current Power".to_string(),
        format!("{:.1} W", device.power_draw),
    );
    detail.insert(
        "Used Memory".to_string(),
        format_memory_size(device.memory_used),
    );

    // HLO metrics (from gRPC)
    if let Some(queue_size) = device.hlo_queue_size {
        detail.insert("HLO Queue Size".to_string(), queue_size.to_string());
    }
    if let Some(mean_us) = device.hlo_exec_mean_us {
        detail.insert("HLO Exec Mean".to_string(), format!("{mean_us:.1} µs"));
    }
    if let Some(p50_us) = device.hlo_exec_p50_us {
        detail.insert("HLO Exec P50".to_string(), format!("{p50_us:.1} µs"));
    }
    if let Some(p90_us) = device.hlo_exec_p90_us {
        detail.insert("HLO Exec P90".to_string(), format!("{p90_us:.1} µs"));
    }
    if let Some(p95_us) = device.hlo_exec_p95_us {
        detail.insert("HLO Exec P95".to_string(), format!("{p95_us:.1} µs"));
    }
    if let Some(p999_us) = device.hlo_exec_p999_us {
        detail.insert("HLO Exec P99.9".to_string(), format!("{p999_us:.1} µs"));
    }

    // Get memory total - use device reported if available, otherwise use generation default
    let total_memory = if device.memory_total > 0 {
        device.memory_total
    } else {
        generation.hbm_size_bytes()
    };

    // Include TensorCore utilization if non-zero
    let tensorcore_util = if device.tensorcore_utilization > 0.0 {
        Some(device.tensorcore_utilization)
    } else {
        None
    };

    Some(GpuInfo {
        uuid,
        time: time.to_string(),
        name,
        device_type: "TPU".to_string(),
        host_id: hostname.to_string(),
        hostname: hostname.to_string(),
        instance: hostname.to_string(),
        utilization: device.utilization,
        ane_utilization: 0.0,
        dla_utilization: None,
        tensorcore_utilization: tensorcore_util,
        temperature: device.temperature,
        used_memory: device.memory_used,
        total_memory,
        frequency: 0, // TPU doesn't report frequency in the same way
        power_consumption: device.power_draw,
        gpu_core_count: Some(device.core_count),
        // Google TPU exposes only a single temperature reading; no NVML
        // thermal-threshold / P-state or NVIDIA hardware-detail
        // (NUMA/GSP/NvLink/GPM) equivalents.
        temperature_threshold_slowdown: None,
        temperature_threshold_shutdown: None,
        temperature_threshold_max_operating: None,
        temperature_threshold_acoustic: None,
        performance_state: None,
        numa_node_id: None,
        gsp_firmware_mode: None,
        gsp_firmware_version: None,
        nvlink_remote_devices: Vec::new(),
        gpm_metrics: None,
        detail,
    })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn test_reader_creation() {
        let reader = GoogleTpuReader::new();
        // Just verify we can create the reader
        let _ = reader.get_gpu_info();
    }

    #[test]
    fn test_tpu_generation_from_chip_version() {
        assert_eq!(TpuGeneration::from_chip_version("v2"), TpuGeneration::V2);
        assert_eq!(TpuGeneration::from_chip_version("v3"), TpuGeneration::V3);
        assert_eq!(TpuGeneration::from_chip_version("v4"), TpuGeneration::V4);
        assert_eq!(TpuGeneration::from_chip_version("v5e"), TpuGeneration::V5e);
        assert_eq!(
            TpuGeneration::from_chip_version("v5lite"),
            TpuGeneration::V5e
        );
        assert_eq!(TpuGeneration::from_chip_version("v5p"), TpuGeneration::V5p);
        assert_eq!(TpuGeneration::from_chip_version("v6e"), TpuGeneration::V6e);
        assert_eq!(
            TpuGeneration::from_chip_version("v6e-16"),
            TpuGeneration::V6e
        );
        assert_eq!(
            TpuGeneration::from_chip_version("trillium"),
            TpuGeneration::V6Trillium
        );
        assert_eq!(
            TpuGeneration::from_chip_version("v6"),
            TpuGeneration::V6Trillium
        );
        assert_eq!(
            TpuGeneration::from_chip_version("ironwood"),
            TpuGeneration::V7Ironwood
        );
        assert_eq!(
            TpuGeneration::from_chip_version("v7"),
            TpuGeneration::V7Ironwood
        );
        assert_eq!(
            TpuGeneration::from_chip_version("unknown"),
            TpuGeneration::Unknown
        );
    }

    #[test]
    fn test_tpu_generation_hbm_size() {
        assert_eq!(TpuGeneration::V2.hbm_size_bytes(), 8 * 1024 * 1024 * 1024);
        assert_eq!(TpuGeneration::V3.hbm_size_bytes(), 16 * 1024 * 1024 * 1024);
        assert_eq!(TpuGeneration::V4.hbm_size_bytes(), 32 * 1024 * 1024 * 1024);
        assert_eq!(TpuGeneration::V5e.hbm_size_bytes(), 16 * 1024 * 1024 * 1024);
        assert_eq!(TpuGeneration::V5p.hbm_size_bytes(), 95 * 1024 * 1024 * 1024);
        assert_eq!(TpuGeneration::V6e.hbm_size_bytes(), 16 * 1024 * 1024 * 1024);
        assert_eq!(
            TpuGeneration::V6Trillium.hbm_size_bytes(),
            32 * 1024 * 1024 * 1024
        );
        assert_eq!(
            TpuGeneration::V7Ironwood.hbm_size_bytes(),
            192 * 1024 * 1024 * 1024
        );
    }

    #[test]
    fn test_tpu_generation_display_name() {
        assert_eq!(TpuGeneration::V2.display_name(), "Google TPU v2");
        assert_eq!(TpuGeneration::V3.display_name(), "Google TPU v3");
        assert_eq!(TpuGeneration::V4.display_name(), "Google TPU v4");
        assert_eq!(TpuGeneration::V5e.display_name(), "Google TPU v5e");
        assert_eq!(TpuGeneration::V5p.display_name(), "Google TPU v5p");
        assert_eq!(TpuGeneration::V6e.display_name(), "Google TPU v6e");
        assert_eq!(
            TpuGeneration::V6Trillium.display_name(),
            "Google TPU v6 Trillium"
        );
        assert_eq!(
            TpuGeneration::V7Ironwood.display_name(),
            "Google TPU v7 Ironwood 192GB HBM3e"
        );
    }

    #[test]
    fn test_tpu_generation_memory_type() {
        assert_eq!(TpuGeneration::V2.memory_type(), "HBM2");
        assert_eq!(TpuGeneration::V5p.memory_type(), "HBM2e");
        assert_eq!(TpuGeneration::V6e.memory_type(), "HBM2e");
        assert_eq!(TpuGeneration::V6Trillium.memory_type(), "HBM2e");
        assert_eq!(TpuGeneration::V7Ironwood.memory_type(), "HBM3e");
    }

    #[test]
    fn test_format_memory_size() {
        assert_eq!(format_memory_size(1024), "1024 B");
        assert_eq!(format_memory_size(1024 * 1024), "1 MB");
        assert_eq!(format_memory_size(1024 * 1024 * 1024), "1 GB");
        assert_eq!(format_memory_size(16 * 1024 * 1024 * 1024), "16 GB");
        assert_eq!(format_memory_size(192 * 1024 * 1024 * 1024), "192 GB");
    }

    #[test]
    fn test_create_gpu_info_from_mock_device() {
        let device = TpuDeviceInfo {
            index: 0,
            chip_version: "v4".to_string(),
            uuid: "TPU-0-test".to_string(),
            core_count: 2,
            utilization: 75.5,
            tensorcore_utilization: 50.0,
            memory_used: 16 * 1024 * 1024 * 1024,  // 16 GB
            memory_total: 32 * 1024 * 1024 * 1024, // 32 GB
            temperature: 65,
            power_draw: 150.0,
            power_max: 200.0,
            tpu_runtime_version: "2.13.0".to_string(),
            accelerator_type: "TPU v4".to_string(),
            hlo_queue_size: Some(3),
            hlo_exec_mean_us: Some(125.5),
            hlo_exec_p50_us: Some(100.0),
            hlo_exec_p90_us: Some(200.0),
            hlo_exec_p95_us: Some(250.0),
            hlo_exec_p999_us: Some(500.0),
        };

        let time = "2025-01-01 00:00:00";
        let hostname = "test-host";

        let gpu_info = create_gpu_info_from_device(device, None, time, hostname);

        assert!(gpu_info.is_some());
        let info = gpu_info.unwrap();

        assert_eq!(info.uuid, "TPU-0-test");
        assert_eq!(info.name, "Google TPU v4");
        assert_eq!(info.device_type, "TPU");
        assert_eq!(info.utilization, 75.5);
        assert_eq!(info.temperature, 65);
        assert_eq!(info.used_memory, 16 * 1024 * 1024 * 1024);
        assert_eq!(info.total_memory, 32 * 1024 * 1024 * 1024);
        assert_eq!(info.power_consumption, 150.0);
        assert_eq!(info.gpu_core_count, Some(2));
        assert_eq!(info.hostname, "test-host");

        // Check detail fields
        assert_eq!(info.detail.get("lib_name"), Some(&"libtpu".to_string()));
        assert_eq!(info.detail.get("lib_version"), Some(&"2.13.0".to_string()));

        // Check HLO metrics in detail
        assert_eq!(info.detail.get("HLO Queue Size"), Some(&"3".to_string()));
        assert_eq!(
            info.detail.get("HLO Exec Mean"),
            Some(&"125.5 µs".to_string())
        );
        assert_eq!(
            info.detail.get("HLO Exec P50"),
            Some(&"100.0 µs".to_string())
        );
    }

    #[test]
    fn test_create_gpu_info_with_empty_uuid() {
        let device = TpuDeviceInfo {
            index: 5,
            chip_version: "v7".to_string(),
            uuid: "".to_string(), // Empty UUID should be auto-generated
            core_count: 2,
            utilization: 0.0,
            tensorcore_utilization: 0.0,
            memory_used: 0,
            memory_total: 0, // Should default to generation size
            temperature: 45,
            power_draw: 0.0,
            power_max: 400.0,
            tpu_runtime_version: "".to_string(),
            accelerator_type: "TPU v7 Ironwood".to_string(),
            hlo_queue_size: None,
            hlo_exec_mean_us: None,
            hlo_exec_p50_us: None,
            hlo_exec_p90_us: None,
            hlo_exec_p95_us: None,
            hlo_exec_p999_us: None,
        };

        let gpu_info = create_gpu_info_from_device(device, None, "2025-01-01 00:00:00", "host");

        assert!(gpu_info.is_some());
        let info = gpu_info.unwrap();

        // UUID should be auto-generated from index
        assert_eq!(info.uuid, "TPU-5");
        // Name should reflect v7 Ironwood
        assert_eq!(info.name, "Google TPU v7 Ironwood 192GB HBM3e");
        // Memory should default to v7 Ironwood size (192 GB)
        assert_eq!(info.total_memory, 192 * 1024 * 1024 * 1024);
    }
}
