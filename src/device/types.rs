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

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// Type aliases for complex return types
#[allow(dead_code)]
pub type ProcessInfoResult = Option<(
    f64,
    f64,
    u64,
    u64,
    String,
    String,
    String,
    u64,
    String,
    u32,
    u32,
)>;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GpuInfo {
    pub uuid: String,
    pub time: String,
    pub name: String,
    pub device_type: String, // "GPU", "NPU", etc.
    pub host_id: String,     // Host identifier (e.g., "10.82.128.41:9090")
    pub hostname: String,    // DNS hostname of the server
    pub instance: String,    // Instance name from metrics
    pub utilization: f64,
    pub ane_utilization: f64,
    pub dla_utilization: Option<f64>,
    pub tensorcore_utilization: Option<f64>, // TPU TensorCore utilization
    pub temperature: u32,
    pub used_memory: u64,
    pub total_memory: u64,
    pub frequency: u32,
    pub power_consumption: f64,
    pub gpu_core_count: Option<u32>, // Number of GPU cores (e.g., Apple Silicon)
    pub detail: HashMap<String, String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub device_id: usize,     // GPU index (internal)
    pub device_uuid: String,  // GPU UUID
    pub pid: u32,             // Process ID
    pub process_name: String, // Process name
    pub used_memory: u64,     // GPU memory usage in bytes
    pub cpu_percent: f64,     // CPU usage percentage
    pub memory_percent: f64,  // System memory usage percentage
    pub memory_rss: u64,      // Resident Set Size in bytes
    pub memory_vms: u64,      // Virtual Memory Size in bytes
    pub user: String,         // User name
    pub state: String,        // Process state (R, S, D, etc.)
    pub start_time: String,   // Process start time
    pub cpu_time: u64,        // Total CPU time in seconds
    pub command: String,      // Full command line
    pub ppid: u32,            // Parent process ID
    pub threads: u32,         // Number of threads
    pub uses_gpu: bool,       // Whether the process uses GPU
    pub priority: i32,        // Process priority (PRI)
    pub nice_value: i32,      // Nice value (NI)
    pub gpu_utilization: f64, // GPU utilization percentage
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CpuInfo {
    pub host_id: String,  // Host identifier (e.g., "10.82.128.41:9090")
    pub hostname: String, // DNS hostname of the server
    pub instance: String, // Instance name from metrics
    pub cpu_model: String,
    pub architecture: String, // "x86_64", "arm64", etc.
    pub platform_type: CpuPlatformType,
    pub socket_count: u32,                   // Number of CPU sockets
    pub total_cores: u32,                    // Total logical cores
    pub total_threads: u32,                  // Total threads (with hyperthreading)
    pub base_frequency_mhz: u32,             // Base CPU frequency
    pub max_frequency_mhz: u32,              // Maximum CPU frequency
    pub cache_size_mb: u32,                  // Total cache size in MB
    pub utilization: f64,                    // Overall CPU utilization percentage
    pub temperature: Option<u32>,            // CPU temperature (if available)
    pub power_consumption: Option<f64>,      // Power consumption in watts (if available)
    pub per_socket_info: Vec<CpuSocketInfo>, // Per-socket information
    pub apple_silicon_info: Option<AppleSiliconCpuInfo>, // Apple Silicon specific info
    pub per_core_utilization: Vec<CoreUtilization>, // Per-core utilization data
    pub time: String,                        // Timestamp
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CoreUtilization {
    pub core_id: u32,        // Core identifier (0-based)
    pub core_type: CoreType, // Type of core (Performance, Efficiency, Standard)
    pub utilization: f64,    // Core utilization percentage (0-100)
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum CoreType {
    Super,       // S-cores (Apple Silicon M5 Pro/Max Super cores)
    Performance, // P-cores (Apple Silicon) or Performance cores (Intel/AMD)
    Efficiency,  // E-cores (Apple Silicon) or Efficiency cores (Intel/AMD)
    Standard,    // Regular cores (no P/E distinction)
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum CpuPlatformType {
    Intel,
    Amd,
    AppleSilicon,
    Arm,
    Other(String), // For unknown or other CPU types
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CpuSocketInfo {
    pub socket_id: u32,           // Socket identifier
    pub utilization: f64,         // Per-socket utilization
    pub cores: u32,               // Number of cores in this socket
    pub threads: u32,             // Number of threads in this socket
    pub temperature: Option<u32>, // Socket temperature (if available)
    pub frequency_mhz: u32,       // Current frequency
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AppleSiliconCpuInfo {
    pub s_core_count: u32,                    // Super core count (M5 Pro/Max)
    pub p_core_count: u32,                    // Performance core count
    pub e_core_count: u32,                    // Efficiency core count
    pub gpu_core_count: u32,                  // GPU core count
    pub s_core_utilization: f64,              // Super core utilization (M5 Pro/Max)
    pub p_core_utilization: f64,              // Performance core utilization
    pub e_core_utilization: f64,              // Efficiency core utilization
    pub ane_ops_per_second: Option<f64>,      // ANE operations per second
    pub s_cluster_frequency_mhz: Option<u32>, // S-cluster frequency in MHz (M5 Pro/Max)
    pub p_cluster_frequency_mhz: Option<u32>, // P-cluster frequency in MHz
    pub e_cluster_frequency_mhz: Option<u32>, // E-cluster frequency in MHz
    pub s_core_l2_cache_mb: Option<u32>,      // S-core L2 cache size in MB (M5 Pro/Max)
    pub p_core_l2_cache_mb: Option<u32>,      // P-core L2 cache size in MB
    pub e_core_l2_cache_mb: Option<u32>,      // E-core L2 cache size in MB
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MemoryInfo {
    pub host_id: String,       // Host identifier (e.g., "10.82.128.41:9090")
    pub hostname: String,      // DNS hostname of the server
    pub instance: String,      // Instance name from metrics
    pub total_bytes: u64,      // Total system memory in bytes
    pub used_bytes: u64,       // Used memory in bytes
    pub available_bytes: u64,  // Available memory in bytes
    pub free_bytes: u64,       // Free memory in bytes
    pub buffers_bytes: u64,    // Buffer memory in bytes (Linux specific)
    pub cached_bytes: u64,     // Cached memory in bytes (Linux specific)
    pub swap_total_bytes: u64, // Total swap space in bytes
    pub swap_used_bytes: u64,  // Used swap space in bytes
    pub swap_free_bytes: u64,  // Free swap space in bytes
    pub utilization: f64,      // Memory utilization percentage
    pub time: String,          // Timestamp
}

/// Chassis/Node-level information for system-wide metrics
/// This provides visibility into total power consumption, thermal data, and BMC information
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ChassisInfo {
    pub host_id: String,  // Host identifier (e.g., "10.82.128.41:9090")
    pub hostname: String, // DNS hostname of the server
    pub instance: String, // Instance name from metrics

    // Power
    pub total_power_watts: Option<f64>, // Combined power consumption (CPU+GPU+ANE)

    // Thermal (BMC)
    pub inlet_temperature: Option<f64>, // Inlet temperature (if available)
    pub outlet_temperature: Option<f64>, // Outlet temperature (if available)
    pub thermal_pressure: Option<String>, // Thermal pressure level (Apple Silicon)

    // Cooling
    pub fan_speeds: Vec<FanInfo>, // Fan speed information

    // PSU
    pub psu_status: Vec<PsuInfo>, // PSU status information

    // Platform-specific details
    pub detail: HashMap<String, String>,

    pub time: String, // Timestamp
}

/// Fan information for cooling monitoring
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FanInfo {
    pub id: u32,        // Fan identifier
    pub name: String,   // Fan name
    pub speed_rpm: u32, // Current speed in RPM
    pub max_rpm: u32,   // Maximum speed in RPM
}

/// Power Supply Unit (PSU) status information
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PsuInfo {
    pub id: u32,                  // PSU identifier
    pub name: String,             // PSU name
    pub status: PsuStatus,        // PSU status
    pub power_watts: Option<f64>, // Current power output
}

/// PSU operational status
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
pub enum PsuStatus {
    #[default]
    Unknown, // Status unknown
    Ok,         // Normal operation
    Degraded,   // Operating but with issues
    Failed,     // PSU has failed
    NotPresent, // PSU not installed
}
