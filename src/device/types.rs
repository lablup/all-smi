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
    /// Slowdown temperature threshold in Celsius. HW throttling engages at or
    /// above this value. `None` when the driver / device does not report it
    /// (older drivers, non-NVIDIA) — callers must treat this as "unknown" and
    /// render nothing rather than substituting a default.
    #[serde(default)]
    pub temperature_threshold_slowdown: Option<u32>,
    /// Shutdown temperature threshold in Celsius. The GPU powers off at or
    /// above this value to protect the silicon. `None` when unavailable.
    #[serde(default)]
    pub temperature_threshold_shutdown: Option<u32>,
    /// Maximum operating temperature threshold (`GpuMax`) in Celsius. The
    /// GPU may throttle below base clock at or above this point. `None`
    /// when unavailable.
    #[serde(default)]
    pub temperature_threshold_max_operating: Option<u32>,
    /// Current acoustic temperature threshold in Celsius, if the driver
    /// exposes one (typically consumer cards). `None` on datacenter GPUs
    /// and older drivers.
    #[serde(default)]
    pub temperature_threshold_acoustic: Option<u32>,
    /// Current performance state (P-state). `0` is the highest-performance
    /// state and `15` is the lowest. `None` when the GPU does not expose a
    /// P-state (e.g. non-NVIDIA, MIG child devices, driver too old).
    #[serde(default)]
    pub performance_state: Option<u32>,
    /// NUMA node the GPU is attached to. `None` when the host has no NUMA
    /// topology (non-Linux platforms, driver too old, or an NVML
    /// `NotSupported` response). Valid values are non-negative; a value of
    /// -1 in NVML's raw encoding is canonicalised to `None` rather than
    /// rendered as a negative number.
    #[serde(default)]
    pub numa_node_id: Option<i32>,
    /// GSP firmware mode, encoded as `0=disabled`, `1=enabled`, `2=default`.
    /// `None` when the driver does not expose the GSP firmware family
    /// (pre-R525 drivers, non-datacenter SKUs).
    #[serde(default)]
    pub gsp_firmware_mode: Option<u8>,
    /// GSP firmware version string (e.g. `"550.54.15"`). `None` when
    /// unavailable. Cached once per device since the version is static for
    /// the lifetime of the process.
    #[serde(default)]
    pub gsp_firmware_version: Option<String>,
    /// Active NvLinks and their remote endpoint classification. Empty when
    /// the GPU has no active links, when NvLink APIs are not supported by
    /// the driver, or on non-NVIDIA paths.
    #[serde(default)]
    pub nvlink_remote_devices: Vec<NvLinkRemoteDevice>,
    /// GPU Performance Monitoring metrics snapshot, when supported
    /// (Hopper+). `None` everywhere else — non-NVIDIA paths and older
    /// architectures must never populate this field.
    #[serde(default)]
    pub gpm_metrics: Option<GpmMetrics>,
    pub detail: HashMap<String, String>,
}

/// Remote endpoint classification for a single NvLink reported as active on
/// a parent GPU. Populated by the NVIDIA reader via NVML and round-tripped
/// through the Prometheus exporter so remote scrapers see the same topology.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct NvLinkRemoteDevice {
    /// Zero-based link index (0..18 on current NVIDIA hardware). Matches
    /// the `link` argument of `nvmlDeviceGetNvLinkRemoteDeviceType`.
    pub link_index: u32,
    /// Classification of the node sitting on the other side of the link.
    pub remote_type: NvLinkRemoteType,
}

/// NvLink remote device classification as returned by
/// `nvmlDeviceGetNvLinkRemoteDeviceType`. Serialised as lowercase strings so
/// the Prometheus exporter can surface them as label values without further
/// encoding.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "lowercase")]
pub enum NvLinkRemoteType {
    /// Remote endpoint is another GPU.
    Gpu,
    /// Remote endpoint is an IBM NPU (POWER9 systems).
    IbmNpu,
    /// Remote endpoint is an NvSwitch.
    Switch,
    /// Remote endpoint is unknown or not classified by the driver.
    #[default]
    Unknown,
}

impl NvLinkRemoteType {
    /// Stable lowercase label used for Prometheus values and the network
    /// parser. MUST stay in sync with the serde `rename_all` annotation so
    /// the serialised JSON and the exported label encoding agree.
    pub fn as_label(self) -> &'static str {
        match self {
            Self::Gpu => "gpu",
            Self::IbmNpu => "ibmnpu",
            Self::Switch => "switch",
            Self::Unknown => "unknown",
        }
    }

    /// Inverse of [`NvLinkRemoteType::as_label`]. Unknown inputs map to
    /// [`NvLinkRemoteType::Unknown`] rather than returning an error — the
    /// parser must never reject metric lines, only classify them.
    pub fn from_label(value: &str) -> Self {
        match value {
            "gpu" => Self::Gpu,
            "ibmnpu" => Self::IbmNpu,
            "switch" => Self::Switch,
            _ => Self::Unknown,
        }
    }
}

/// Optional GPU Performance Monitoring (GPM) metrics snapshot. Populated
/// only when the device reports `gpm_support() == true` (Hopper+ on a
/// driver that exposes the GPM family). All fields are `Option<f32>` so
/// individual metric failures do not invalidate the rest of the snapshot.
///
/// Values are expressed as a fraction in `[0.0, 1.0]` to match Prometheus'
/// convention for utilization gauges — the NVML `*_UTIL` family reports
/// percentages which the reader divides by 100 on the way in.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Default)]
pub struct GpmMetrics {
    /// Fraction of warps active vs theoretical maximum, averaged across
    /// all SMs. Maps to `NVML_GPM_METRIC_SM_OCCUPANCY` / 100.
    pub sm_occupancy: Option<f32>,
    /// Fraction of memory bandwidth in use, averaged across the polling
    /// window. Maps to `NVML_GPM_METRIC_DRAM_BW_UTIL` / 100.
    pub memory_bandwidth_utilization: Option<f32>,
}

/// Proximity classification for the current GPU temperature relative to the
/// slowdown / shutdown thresholds. Used by the TUI to highlight dangerous
/// thermal conditions and by tests to lock in the classification contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThermalProximity {
    /// No threshold data, or the current temperature is comfortably below
    /// every reported threshold (default rendering).
    Normal,
    /// Current temperature is within [`ThermalProximityConfig::slowdown_margin`]
    /// Celsius of the slowdown threshold, or already at/above it. Render
    /// the current temperature in yellow.
    Slowdown,
    /// Current temperature is within [`ThermalProximityConfig::shutdown_margin`]
    /// Celsius of the shutdown threshold, or already at/above it. Render
    /// the current temperature in red.
    Shutdown,
}

/// Thresholds that decide when the TUI highlights the current GPU temperature.
/// Centralised here so the renderer, exporter documentation, and tests agree
/// on the numbers without scattering magic values across the codebase.
#[derive(Debug, Clone, Copy)]
pub struct ThermalProximityConfig {
    /// Margin in Celsius from the slowdown threshold at which the TUI
    /// switches to a `Slowdown` warning state.
    pub slowdown_margin: u32,
    /// Margin in Celsius from the shutdown threshold at which the TUI
    /// switches to a `Shutdown` warning state. `Shutdown` wins over
    /// `Slowdown` when both apply.
    pub shutdown_margin: u32,
}

impl Default for ThermalProximityConfig {
    fn default() -> Self {
        Self {
            slowdown_margin: 5,
            shutdown_margin: 2,
        }
    }
}

impl GpuInfo {
    /// Classify the current temperature against the reported thresholds.
    ///
    /// Returns [`ThermalProximity::Normal`] when no threshold data is
    /// available — the feature MUST gracefully degrade on older drivers or
    /// non-NVIDIA GPUs.
    pub fn thermal_proximity(&self, cfg: ThermalProximityConfig) -> ThermalProximity {
        // Shutdown wins unconditionally over slowdown.
        //
        // `saturating_add` defends against malformed remote inputs: the
        // network parser's `saturating_u32` helper can produce `u32::MAX`
        // when a scrape contains nonsense values, and an unchecked
        // `temperature + margin` would panic in debug builds. Saturating
        // simply yields `u32::MAX` in that pathological case and the
        // comparison still produces a sane (and harmless) result.
        if let Some(shutdown) = self.temperature_threshold_shutdown
            && shutdown > 0
            && self.temperature.saturating_add(cfg.shutdown_margin) >= shutdown
        {
            return ThermalProximity::Shutdown;
        }
        if let Some(slowdown) = self.temperature_threshold_slowdown
            && slowdown > 0
            && self.temperature.saturating_add(cfg.slowdown_margin) >= slowdown
        {
            return ThermalProximity::Slowdown;
        }
        ThermalProximity::Normal
    }
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

/// Per-vGPU instance metrics collected from NVML.
///
/// A vGPU is a virtualized slice of a physical NVIDIA GPU. Each instance has its
/// own UUID, framebuffer budget, utilization, and memory statistics.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct VgpuInfo {
    /// NVML-assigned instance id (opaque u32).
    pub instance_id: u32,
    /// vGPU instance UUID (e.g. `GRID-…`).
    pub uuid: String,
    /// Owning VM id if reported by NVML (typically a UUID or label). Empty when
    /// the driver does not expose one (e.g. early-boot or SR-IOV VFs).
    pub vm_id: String,
    /// Human-readable vGPU profile name (e.g. `GRID A100-8C`).
    pub vgpu_type_name: String,
    /// Framebuffer used (bytes).
    pub fb_used_bytes: u64,
    /// Framebuffer total (bytes).
    pub fb_total_bytes: u64,
    /// GPU utilization percentage over the instance's lifetime (0-100).
    /// `None` when NVML reports `NVML_VALUE_NOT_AVAILABLE`.
    pub gpu_utilization: Option<u32>,
    /// Memory bandwidth utilization percentage (0-100).
    pub memory_utilization: Option<u32>,
    /// Whether at least one accounting PID is currently active on the vGPU.
    pub is_active: bool,
}

/// Per-GPU vGPU host metadata and the instances running on that GPU.
///
/// Populated only when NVML reports the host GPU as vGPU-capable (i.e.
/// `vgpu_host_mode()` succeeds). On bare-metal hosts this data is not
/// collected and the vector is empty.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct VgpuHostInfo {
    /// Host identifier (e.g. `10.82.128.41:9090` or the local hostname).
    pub host_id: String,
    /// DNS hostname of the host.
    pub hostname: String,
    /// Prometheus `instance` label.
    pub instance: String,
    /// NVML device index of the physical GPU this record refers to.
    pub gpu_index: u32,
    /// UUID of the physical GPU.
    pub gpu_uuid: String,
    /// Device display name (e.g. `NVIDIA A100-SXM4-80GB`).
    pub gpu_name: String,
    /// NVML host vGPU mode. `"Sriov"` or `"NonSriov"` on vGPU-enabled hosts,
    /// `"Disabled"` when the query is supported but the host is not vGPU.
    pub host_mode: String,
    /// Numeric vGPU scheduler policy id from NVML (0 = best-effort, 1 = equal share, etc.).
    pub scheduler_policy: u32,
    /// Adaptive Round Robin mode (0 = unsupported, 1 = off, 2 = on).
    pub scheduler_arr_mode: u32,
    /// Whether Adaptive Round Robin is reported as supported by the driver.
    pub is_arr_supported: bool,
    /// Active vGPU instances running on this GPU.
    pub vgpus: Vec<VgpuInfo>,
    /// Free-form diagnostic text surfaced to the UI (e.g. scheduler log line).
    pub detail: HashMap<String, String>,
}

impl VgpuHostInfo {
    /// Returns `true` when the GPU is reporting any vGPU-related data that the
    /// UI should surface (host mode, scheduler info, or live instances).
    pub fn is_vgpu_active(&self) -> bool {
        !self.vgpus.is_empty() || self.host_mode != "Disabled"
    }
}

/// Per-MIG-instance metrics collected from NVML.
///
/// A MIG (Multi-Instance GPU) instance is an isolated partition of an NVIDIA
/// datacenter GPU (A100/A30/H100/H200). Each instance has its own SM slice,
/// memory carve-out, and L2 cache, exposed through NVML as a child `Device`
/// handle of the parent physical GPU.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct MigInstanceInfo {
    /// MIG instance index returned by `mig_device_by_index` enumeration (0-based).
    pub instance_id: u32,
    /// GPU instance id (the SM/memory slice id, distinct from `instance_id`),
    /// as reported by `nvmlDeviceGetGpuInstanceId`. `None` when the driver does
    /// not expose it (older drivers).
    pub gpu_instance_id: Option<u32>,
    /// Compute instance id, as reported by `nvmlDeviceGetComputeInstanceId`.
    /// `None` when the driver does not expose it.
    pub compute_instance_id: Option<u32>,
    /// MIG instance UUID (e.g. `MIG-…`). Empty when the driver does not expose one.
    pub uuid: String,
    /// MIG profile/slice name (e.g. `1g.5gb`, `2g.10gb`, `7g.40gb`). Best-effort
    /// — empty when not derivable from the available NVML data.
    pub profile_name: String,
    /// GPU SM utilization percentage over the most recent NVML sample (0-100).
    /// `None` when NVML reports the metric as unavailable for the instance.
    pub utilization_gpu: Option<u32>,
    /// Memory bandwidth utilization percentage (0-100). `None` when unavailable.
    pub utilization_memory: Option<u32>,
    /// Framebuffer used (bytes). `0` when NVML cannot report it.
    pub memory_used_bytes: u64,
    /// Framebuffer total carve-out for this instance (bytes). `0` when unavailable.
    pub memory_total_bytes: u64,
}

/// Per-physical-GPU MIG host record.
///
/// Populated only when NVML reports MIG mode enabled for the GPU and at least
/// one instance enumeration succeeded. On non-MIG GPUs and older architectures
/// the parent vector stays empty — the feature MUST be a silent no-op.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct MigGpuInfo {
    /// Host identifier (e.g. `10.82.128.41:9090` or local hostname).
    pub host_id: String,
    /// DNS hostname of the host.
    pub hostname: String,
    /// Prometheus `instance` label.
    pub instance: String,
    /// NVML device index of the parent physical GPU.
    pub gpu_index: u32,
    /// UUID of the parent physical GPU.
    pub gpu_uuid: String,
    /// Device display name (e.g. `NVIDIA A100-SXM4-80GB`).
    pub gpu_name: String,
    /// Whether MIG mode is currently enabled (`true`) or disabled (`false`)
    /// on the parent GPU.
    pub mig_mode: bool,
    /// Live MIG instances enumerated from NVML.
    pub instances: Vec<MigInstanceInfo>,
}

impl MigGpuInfo {
    /// Returns `true` when the parent GPU should surface a MIG section in the
    /// TUI — either MIG mode is enabled or live instances were enumerated.
    pub fn is_mig_active(&self) -> bool {
        self.mig_mode || !self.instances.is_empty()
    }
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
