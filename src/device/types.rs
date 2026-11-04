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
    /// Current fan speed in revolutions per minute. `None` when the device
    /// or driver does not report a fan tachometer: passively cooled
    /// datacenter cards have no fan at all, and some drivers expose only a
    /// duty-cycle percentage. Callers must render nothing in that case
    /// rather than substituting `0`, which is indistinguishable from a
    /// stalled fan on a card that does have one.
    ///
    /// This is the per-device GPU tachometer and is deliberately separate
    /// from `ChassisInfo::fan_speeds`, which models host and enclosure fans
    /// (for example the macOS SMC fans read in
    /// `src/device/macos_native/smc.rs`) and is exported as
    /// `all_smi_chassis_fan_speed_rpm`. A machine can report both, so the
    /// two never share a field or a metric name.
    #[serde(default)]
    pub fan_speed_rpm: Option<u32>,
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
    /// Optional per-link bandwidth hint in MB/s as reported by NVML
    /// `nvmlDeviceGetNvLinkUtilizationCounter` / Gen-specific ceilings.
    /// `None` when the driver does not expose a per-link speed — the
    /// topology tab's NVn classifier falls back to a generic `"NV"`
    /// label in that case rather than misreporting a generation.
    ///
    /// Serialised with `#[serde(default)]` so snapshots / exporters
    /// produced before this field existed continue to deserialise
    /// cleanly (backward-compatibility requirement from issue #190).
    #[serde(default)]
    pub bandwidth_mb_s: Option<u32>,
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

/// Sentinel written into [`GpuInfo`]'s non-optional `f64` metric fields
/// (`utilization`, `ane_utilization`, `power_consumption`) when the reader
/// could not obtain a reading at all.
///
/// These three fields are plain `f64` rather than `Option<f64>` because they
/// are consumed by roughly sixty call sites (gauges, sparklines, sort
/// comparators, energy accumulation, three CLI shims) that all treat them as
/// numbers. A negative value is outside the valid range of every one of them
/// (a percentage is `0..=100`, a power rail draws `>= 0` watts), so it cannot
/// collide with a real reading. Read them through
/// [`GpuInfo::utilization_reading`], [`GpuInfo::ane_utilization_reading`] and
/// [`GpuInfo::power_consumption_reading`] rather than comparing against this
/// constant directly.
///
/// This value must never reach the Prometheus exposition: the exporter omits
/// the series instead. See the module docs on
/// `crate::device::macos_native::manager` for the full policy.
pub const GPU_METRIC_UNAVAILABLE: f64 = -1.0;

/// Upper bound on a plausible [`GpuInfo::fan_speed_rpm`] reading, shared by
/// every producer and consumer of the value so none of them can disagree
/// about what counts as a garbled tachometer read: the reader clamps
/// (`intel_gpu_linux`, `amd`, `amd_adl`, `intel_gpu_level_zero::apply`) cap a
/// raw sensor value before it ever reaches [`GpuInfo`], the exporter
/// (`api::metrics::gpu`) applies the same bound to its legacy detail-string
/// fallback, and the remote scrape parser (`network::metrics_parser`)
/// applies it again to a value arriving over the wire. Blower fans on
/// workstation cards top out around 6 000 RPM and the small high-static fans
/// on some accelerators reach roughly 20 000; 100 000 is far beyond any real
/// tachometer while still rejecting values like `u32::MAX` that would blow
/// out the TUI column width.
pub const MAX_GPU_FAN_RPM: u32 = 100_000;

impl GpuInfo {
    /// GPU utilization percentage, or `None` when the reader had no source
    /// for it. A genuinely idle GPU returns `Some(0.0)`.
    pub fn utilization_reading(&self) -> Option<f64> {
        (self.utilization >= 0.0).then_some(self.utilization)
    }

    /// ANE power draw in milliwatts (Apple Silicon), or `None` when the
    /// reader had no source for it. An idle ANE returns `Some(0.0)`.
    ///
    /// Non-Apple readers set this to `0.0` to mean "not applicable", which is
    /// indistinguishable from an idle ANE and is preserved as `Some(0.0)` for
    /// backward compatibility with existing scrapes.
    pub fn ane_utilization_reading(&self) -> Option<f64> {
        (self.ane_utilization >= 0.0).then_some(self.ane_utilization)
    }

    /// GPU power draw in watts, or `None` when the reader had no source for
    /// it. A GPU parked at zero watts returns `Some(0.0)`.
    pub fn power_consumption_reading(&self) -> Option<f64> {
        (self.power_consumption >= 0.0).then_some(self.power_consumption)
    }

    /// GPU temperature in Celsius, or `None` when no sensor produced a value.
    ///
    /// `temperature` is a `u32` and therefore cannot carry a negative
    /// sentinel, so `0` is the unavailable marker. That is the convention the
    /// TUI renderer, the alert engine and the filter DSL already applied
    /// before this accessor existed: a powered SoC or board never reads
    /// 0 °C, and 0 is what a missing SMC/NVML key decodes to.
    pub fn temperature_reading(&self) -> Option<u32> {
        (self.temperature > 0).then_some(self.temperature)
    }

    /// GPU core clock in MHz, or `None` when the platform exposes no
    /// frequency probe.
    ///
    /// As with [`GpuInfo::temperature_reading`], `0` is the unavailable
    /// marker: Rebellions, Intel Gaudi and AMD-via-WMI already report a
    /// static `0` to mean "no probe", and a clock-gated GPU still reports its
    /// nominal clock rather than 0 MHz.
    pub fn frequency_reading(&self) -> Option<u32> {
        (self.frequency > 0).then_some(self.frequency)
    }

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
    /// Stable 0-based correlation identifier assigned by
    /// [`crate::client::AllSmi::get_cpu_info`] as readers are flattened.
    /// CPU topology is static, so callers can use this as a stable key
    /// across refreshes to match an old entry to its refreshed counterpart.
    /// `#[serde(default)]` keeps older snapshots and wire payloads
    /// deserializing cleanly.
    #[serde(default)]
    pub index: u32,
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
    /// Ordered CPU tiers, fastest first (issue #473).
    ///
    /// `[S, P]` on M5 Pro/Max, `[S, E]` on base M5, `[P, M, E]` on a
    /// three-cluster ARM part, `[P, E]` on an Intel hybrid part. An empty list
    /// means the CPU is homogeneous or the topology could not be detected, and
    /// consumers treat every core as `Standard`.
    ///
    /// The platform readers populate this once per reader; only the
    /// `utilization` field is refreshed per tick. `#[serde(default)]` keeps
    /// snapshots and remote payloads written before the field existed
    /// deserializing cleanly, and the skip keeps them byte-identical.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cpu_tiers: Vec<CpuTier>,
    pub per_core_utilization: Vec<CoreUtilization>, // Per-core utilization data
    pub time: String,                               // Timestamp
}

/// One CPU tier in a heterogeneous topology (issue #473).
///
/// `label` is the display token and the Prometheus `cpu_tier` value. The
/// canonical fastest-first vocabulary is `S` (Apple Super), `P` (fastest on
/// every platform), `M` (ARM mid cluster), `E` (efficiency / little), then
/// `E4`, `E5`, ... for capacity groups beyond three.
///
/// `core_type` is the coarse legacy class (`Performance` for the fastest tier,
/// `Efficiency` below it, `Standard` for a one-tier list). The exact tier is
/// `label` plus the per-core [`CoreUtilization::tier_index`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CpuTier {
    pub label: String,
    pub core_type: CoreType,
    pub core_count: u32,
    pub utilization: f64,
    /// Per-tier frequency in MHz. Populated only where the platform reports
    /// it (Apple Silicon); `None` on Linux and Windows.
    #[serde(default)]
    pub frequency_mhz: Option<u32>,
    /// Per-tier L2 cache in MB. Populated only where the platform reports it
    /// (Apple Silicon); `None` on Linux and Windows.
    #[serde(default)]
    pub l2_cache_mb: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CoreUtilization {
    pub core_id: u32,        // Core identifier (0-based)
    pub core_type: CoreType, // Type of core (Performance, Efficiency, Standard)
    pub utilization: f64,    // Core utilization percentage (0-100)
    /// Index into [`CpuInfo::cpu_tiers`], when the core belongs to a detected
    /// tier. `None` on a homogeneous CPU and in snapshots written before
    /// tiers existed. `#[serde(default)]` keeps those payloads
    /// deserializing cleanly.
    #[serde(default)]
    pub tier_index: Option<u8>,
}

impl CpuInfo {
    /// Tiers to display: [`CpuInfo::cpu_tiers`] when populated, otherwise
    /// synthesized from [`CpuInfo::apple_silicon_info`].
    ///
    /// The fallback covers snapshots and older remote servers that predate
    /// tier export. Display consumers (the Activity panel, the CPU renderer
    /// and local details) use this; the Prometheus exporter and the remote
    /// parser read `cpu_tiers` directly because both run against the data
    /// they just produced.
    pub fn display_cpu_tiers(&self) -> Vec<CpuTier> {
        if !self.cpu_tiers.is_empty() {
            return self.cpu_tiers.clone();
        }
        self.apple_silicon_info
            .as_ref()
            .map(AppleSiliconCpuInfo::to_cpu_tiers)
            .unwrap_or_default()
    }

    /// Cores that belong to the tier at `position` in
    /// [`CpuInfo::display_cpu_tiers`].
    ///
    /// `tier_index` is authoritative when present. Legacy Apple payloads carry
    /// no index, so their cores are matched by the exact `CoreType` label
    /// (`S`, `P`, `E`) instead.
    pub fn cores_of_display_tier(&self, position: usize, tier: &CpuTier) -> Vec<&CoreUtilization> {
        self.per_core_utilization
            .iter()
            .filter(|core| match core.tier_index {
                Some(index) => usize::from(index) == position,
                None => core.core_type.short_label() == tier.label,
            })
            .collect()
    }

    /// Restore the invariant that the tier core counts sum to the number of
    /// cores carrying a [`CoreUtilization::tier_index`].
    ///
    /// A reader can detect host-wide topology while displaying only a subset
    /// of cores: a quota-only container (`docker run --cpus=4` with no cpuset)
    /// sees every host tier but emits only `effective_cpu_count` cores, so the
    /// header would otherwise read `4P+8E` for four displayed cores.
    ///
    /// Drop tiers that have no core in `per_core_utilization`, recount every
    /// remaining tier from the cores actually present, and remap `tier_index`
    /// so positions stay contiguous. Tiers keep their relative order. Fewer
    /// than two remaining tiers is homogeneous, so the whole list and every
    /// index are cleared.
    pub fn reconcile_cpu_tiers(&mut self) {
        if self.cpu_tiers.is_empty() {
            for core in &mut self.per_core_utilization {
                core.tier_index = None;
            }
            return;
        }

        let tier_count = self.cpu_tiers.len();
        let mut counts = vec![0u32; tier_count];
        for core in &mut self.per_core_utilization {
            match core.tier_index {
                Some(index) if usize::from(index) < tier_count => {
                    counts[usize::from(index)] += 1;
                }
                Some(_) => core.tier_index = None,
                None => {}
            }
        }

        let kept: Vec<usize> = (0..tier_count)
            .filter(|&position| counts[position] > 0)
            .collect();
        if kept.len() < 2 {
            self.cpu_tiers.clear();
            for core in &mut self.per_core_utilization {
                core.tier_index = None;
            }
            return;
        }

        let total = kept.len();
        let mut remap = vec![None; tier_count];
        let mut tiers = Vec::with_capacity(total);
        for (new_position, &old_position) in kept.iter().enumerate() {
            remap[old_position] = Some(new_position as u8);
            let mut tier = self.cpu_tiers[old_position].clone();
            tier.core_count = counts[old_position];
            tier.core_type = crate::device::cpu_topology::coarse_core_type(new_position, total);
            tiers.push(tier);
        }

        for core in &mut self.per_core_utilization {
            if let Some(index) = core.tier_index {
                core.tier_index = remap[usize::from(index)];
            }
        }

        self.cpu_tiers = tiers;
    }

    /// Neutral value for test literals.
    ///
    /// Test helpers should build `CpuInfo { field: value, ..CpuInfo::test_default() }`
    /// so a new field no longer breaks every unrelated test module with
    /// `error[E0063]` (issues #474 and #479). Production readers keep
    /// exhaustive literals: a new field must force a decision there.
    #[cfg(test)]
    pub(crate) fn test_default() -> Self {
        Self {
            index: 0,
            host_id: String::new(),
            hostname: String::new(),
            instance: String::new(),
            cpu_model: String::new(),
            architecture: String::new(),
            platform_type: CpuPlatformType::Intel,
            socket_count: 0,
            total_cores: 0,
            total_threads: 0,
            base_frequency_mhz: 0,
            max_frequency_mhz: 0,
            cache_size_mb: 0,
            utilization: 0.0,
            temperature: None,
            power_consumption: None,
            per_socket_info: Vec::new(),
            apple_silicon_info: None,
            cpu_tiers: Vec::new(),
            per_core_utilization: Vec::new(),
            time: String::new(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq)]
pub enum CoreType {
    Super,       // S-cores (Apple Silicon M5 Pro/Max Super cores)
    Performance, // P-cores (Apple Silicon) or Performance cores (Intel/AMD)
    Efficiency,  // E-cores (Apple Silicon) or Efficiency cores (Intel/AMD)
    Standard,    // Regular cores (no P/E distinction)
}

impl CoreType {
    /// Single-letter label used by the TUI core topology rows and gauges.
    pub fn short_label(self) -> &'static str {
        match self {
            CoreType::Super => "S",
            CoreType::Performance => "P",
            CoreType::Efficiency => "E",
            CoreType::Standard => "C",
        }
    }
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

impl AppleSiliconCpuInfo {
    /// Core tiers present on this chip, fastest first.
    ///
    /// M1-M4 yield `[Performance, Efficiency]`, M5 Pro/Max yield
    /// `[Super, Performance]`, and base M5 yields `[Super, Efficiency]`.
    /// Display code must iterate this instead of testing `s_core_count > 0`,
    /// which cannot distinguish a Super + Performance chip from a Super +
    /// Efficiency one.
    pub fn core_tiers(&self) -> Vec<CoreType> {
        [CoreType::Super, CoreType::Performance, CoreType::Efficiency]
            .into_iter()
            .filter(|tier| self.core_count_of(*tier) > 0)
            .collect()
    }

    /// Number of cores across every tier the chip has.
    pub fn total_cores(&self) -> u32 {
        self.s_core_count + self.p_core_count + self.e_core_count
    }

    /// Number of cores in `tier`. `Standard` has no Apple Silicon field and
    /// reports zero.
    pub fn core_count_of(&self, tier: CoreType) -> u32 {
        match tier {
            CoreType::Super => self.s_core_count,
            CoreType::Performance => self.p_core_count,
            CoreType::Efficiency => self.e_core_count,
            CoreType::Standard => 0,
        }
    }

    pub fn core_utilization_of(&self, tier: CoreType) -> f64 {
        match tier {
            CoreType::Super => self.s_core_utilization,
            CoreType::Performance => self.p_core_utilization,
            CoreType::Efficiency => self.e_core_utilization,
            CoreType::Standard => 0.0,
        }
    }

    pub fn cluster_frequency_of(&self, tier: CoreType) -> Option<u32> {
        match tier {
            CoreType::Super => self.s_cluster_frequency_mhz,
            CoreType::Performance => self.p_cluster_frequency_mhz,
            CoreType::Efficiency => self.e_cluster_frequency_mhz,
            CoreType::Standard => None,
        }
    }

    pub fn core_l2_cache_of(&self, tier: CoreType) -> Option<u32> {
        match tier {
            CoreType::Super => self.s_core_l2_cache_mb,
            CoreType::Performance => self.p_core_l2_cache_mb,
            CoreType::Efficiency => self.e_core_l2_cache_mb,
            CoreType::Standard => None,
        }
    }

    /// Build the ordered [`CpuTier`] list for this chip, fastest first.
    ///
    /// Used by `cpu_macos` to populate [`CpuInfo::cpu_tiers`] and by
    /// [`CpuInfo::display_cpu_tiers`] to synthesize tiers for legacy
    /// snapshots. `core_type` follows the shared coarse rule; `label` carries
    /// the exact tier.
    pub fn to_cpu_tiers(&self) -> Vec<CpuTier> {
        let tiers = self.core_tiers();
        let total = tiers.len();
        tiers
            .iter()
            .enumerate()
            .map(|(position, tier)| CpuTier {
                label: tier.short_label().to_string(),
                core_type: crate::device::cpu_topology::coarse_core_type(position, total),
                core_count: self.core_count_of(*tier),
                utilization: self.core_utilization_of(*tier),
                frequency_mhz: self.cluster_frequency_of(*tier),
                l2_cache_mb: self.core_l2_cache_of(*tier),
            })
            .collect()
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MemoryInfo {
    /// Stable 0-based correlation identifier assigned by
    /// [`crate::client::AllSmi::get_memory_info`] as readers are flattened.
    /// Memory is effectively a per-host singleton, so this field exists
    /// primarily for API symmetry with [`CpuInfo::index`] and
    /// [`crate::storage::StorageInfo::index`]. `#[serde(default)]` keeps
    /// older snapshots and wire payloads deserializing cleanly.
    #[serde(default)]
    pub index: u32,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn apple(s: u32, p: u32, e: u32) -> AppleSiliconCpuInfo {
        AppleSiliconCpuInfo {
            s_core_count: s,
            p_core_count: p,
            e_core_count: e,
            gpu_core_count: 10,
            s_core_utilization: 10.0,
            p_core_utilization: 20.0,
            e_core_utilization: 30.0,
            ane_ops_per_second: None,
            s_cluster_frequency_mhz: Some(4200),
            p_cluster_frequency_mhz: Some(3800),
            e_cluster_frequency_mhz: Some(2800),
            s_core_l2_cache_mb: Some(16),
            p_core_l2_cache_mb: Some(8),
            e_core_l2_cache_mb: Some(4),
        }
    }

    fn cpu_with(tiers: Vec<CpuTier>, cores: Vec<CoreUtilization>) -> CpuInfo {
        CpuInfo {
            host_id: "localhost".to_string(),
            hostname: "localhost".to_string(),
            instance: "localhost".to_string(),
            cpu_model: "CPU".to_string(),
            architecture: "arm64".to_string(),
            platform_type: CpuPlatformType::AppleSilicon,
            socket_count: 1,
            total_cores: cores.len() as u32,
            total_threads: cores.len() as u32,
            cpu_tiers: tiers,
            per_core_utilization: cores,
            ..CpuInfo::test_default()
        }
    }

    #[test]
    fn apple_tiers_are_fastest_first() {
        let m5_max = apple(6, 12, 0).to_cpu_tiers();
        assert_eq!(
            m5_max
                .iter()
                .map(|tier| (tier.label.as_str(), tier.core_count))
                .collect::<Vec<_>>(),
            vec![("S", 6), ("P", 12)]
        );
        assert_eq!(m5_max[0].core_type, CoreType::Performance);
        assert_eq!(m5_max[1].core_type, CoreType::Efficiency);

        let base_m5 = apple(4, 0, 6).to_cpu_tiers();
        assert_eq!(
            base_m5
                .iter()
                .map(|tier| (tier.label.as_str(), tier.core_count))
                .collect::<Vec<_>>(),
            vec![("S", 4), ("E", 6)]
        );

        let m2 = apple(0, 8, 4).to_cpu_tiers();
        assert_eq!(
            m2.iter()
                .map(|tier| tier.label.as_str())
                .collect::<Vec<_>>(),
            vec!["P", "E"]
        );
        assert_eq!(m2[0].frequency_mhz, Some(3800));
        assert_eq!(m2[1].l2_cache_mb, Some(4));
    }

    #[test]
    fn a_single_apple_tier_uses_the_standard_coarse_class() {
        let tiers = apple(4, 0, 0).to_cpu_tiers();
        assert_eq!(tiers.len(), 1);
        assert_eq!(tiers[0].label, "S");
        assert_eq!(tiers[0].core_type, CoreType::Standard);
    }

    #[test]
    fn display_tiers_fall_back_to_apple_info_for_legacy_payloads() {
        let mut cpu = cpu_with(Vec::new(), Vec::new());
        cpu.apple_silicon_info = Some(apple(0, 8, 4));
        assert_eq!(
            cpu.display_cpu_tiers()
                .iter()
                .map(|tier| tier.label.as_str())
                .collect::<Vec<_>>(),
            vec!["P", "E"]
        );
    }

    #[test]
    fn display_tiers_prefer_the_explicit_list() {
        let mut cpu = cpu_with(
            vec![CpuTier {
                label: "P".to_string(),
                core_type: CoreType::Performance,
                core_count: 2,
                utilization: 50.0,
                frequency_mhz: None,
                l2_cache_mb: None,
            }],
            Vec::new(),
        );
        cpu.apple_silicon_info = Some(apple(0, 8, 4));
        assert_eq!(cpu.display_cpu_tiers().len(), 1);
    }

    #[test]
    fn cores_of_display_tier_prefers_tier_index_over_core_type() {
        let tier = CpuTier {
            label: "P".to_string(),
            core_type: CoreType::Performance,
            core_count: 2,
            utilization: 0.0,
            frequency_mhz: None,
            l2_cache_mb: None,
        };
        let cpu = cpu_with(
            vec![tier.clone()],
            vec![
                CoreUtilization {
                    core_id: 0,
                    core_type: CoreType::Standard,
                    utilization: 1.0,
                    tier_index: Some(0),
                },
                CoreUtilization {
                    core_id: 1,
                    core_type: CoreType::Performance,
                    utilization: 2.0,
                    tier_index: None,
                },
                CoreUtilization {
                    core_id: 2,
                    core_type: CoreType::Efficiency,
                    utilization: 3.0,
                    tier_index: None,
                },
            ],
        );

        let cores = cpu.cores_of_display_tier(0, &tier);
        assert_eq!(
            cores.iter().map(|core| core.core_id).collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    #[test]
    fn legacy_core_utilization_deserializes_without_a_tier_index() {
        let json = r#"{"core_id":0,"core_type":"Performance","utilization":12.5}"#;
        let core: CoreUtilization = serde_json::from_str(json).unwrap();
        assert_eq!(core.tier_index, None);
        assert_eq!(core.core_type, CoreType::Performance);
    }

    fn tier(label: &str, core_count: u32, core_type: CoreType) -> CpuTier {
        CpuTier {
            label: label.to_string(),
            core_type,
            core_count,
            utilization: 0.0,
            frequency_mhz: None,
            l2_cache_mb: None,
        }
    }

    fn core_with_tier(core_id: u32, tier_index: Option<u8>) -> CoreUtilization {
        CoreUtilization {
            core_id,
            core_type: CoreType::Standard,
            utilization: f64::from(core_id),
            tier_index,
        }
    }

    #[test]
    fn reconcile_drops_detected_tiers_absent_from_the_displayed_cores() {
        // Quota-only container: host topology is 2P+8E, but only two P and two
        // E cores are displayed.
        let mut cpu = cpu_with(
            vec![
                tier("P", 2, CoreType::Performance),
                tier("E", 8, CoreType::Efficiency),
            ],
            vec![
                core_with_tier(0, Some(0)),
                core_with_tier(1, Some(0)),
                core_with_tier(2, Some(1)),
                core_with_tier(3, Some(1)),
            ],
        );

        cpu.reconcile_cpu_tiers();

        assert_eq!(
            cpu.cpu_tiers
                .iter()
                .map(|tier| (tier.label.as_str(), tier.core_count))
                .collect::<Vec<_>>(),
            vec![("P", 2), ("E", 2)]
        );
        assert_eq!(cpu.cpu_tiers[0].core_type, CoreType::Performance);
        assert_eq!(cpu.cpu_tiers[1].core_type, CoreType::Efficiency);
        assert_eq!(
            cpu.per_core_utilization
                .iter()
                .map(|core| core.tier_index)
                .collect::<Vec<_>>(),
            vec![Some(0), Some(0), Some(1), Some(1)]
        );
    }

    #[test]
    fn reconcile_clears_a_displayed_subset_with_only_one_tier() {
        let mut cpu = cpu_with(
            vec![
                tier("P", 2, CoreType::Performance),
                tier("E", 8, CoreType::Efficiency),
            ],
            vec![
                core_with_tier(0, Some(0)),
                core_with_tier(1, Some(0)),
                core_with_tier(2, Some(0)),
            ],
        );

        cpu.reconcile_cpu_tiers();

        assert!(cpu.cpu_tiers.is_empty());
        assert!(
            cpu.per_core_utilization
                .iter()
                .all(|core| core.tier_index.is_none())
        );
    }

    #[test]
    fn reconcile_remaps_indices_when_a_middle_tier_is_dropped() {
        let mut cpu = cpu_with(
            vec![
                tier("P", 2, CoreType::Performance),
                tier("M", 4, CoreType::Efficiency),
                tier("E", 2, CoreType::Efficiency),
            ],
            vec![
                core_with_tier(0, Some(0)),
                core_with_tier(1, Some(0)),
                core_with_tier(2, Some(2)),
                core_with_tier(3, Some(2)),
            ],
        );

        cpu.reconcile_cpu_tiers();

        assert_eq!(
            cpu.cpu_tiers
                .iter()
                .map(|tier| (tier.label.as_str(), tier.core_count))
                .collect::<Vec<_>>(),
            vec![("P", 2), ("E", 2)]
        );
        assert_eq!(
            cpu.per_core_utilization
                .iter()
                .map(|core| core.tier_index)
                .collect::<Vec<_>>(),
            vec![Some(0), Some(0), Some(1), Some(1)]
        );
    }

    #[test]
    fn reconcile_keeps_the_core_count_sum_invariant() {
        let mut cpu = cpu_with(
            vec![
                tier("S", 4, CoreType::Performance),
                tier("P", 8, CoreType::Efficiency),
                tier("E", 6, CoreType::Efficiency),
            ],
            (0..18)
                .map(|index| core_with_tier(index, Some((index / 6) as u8)))
                .collect(),
        );

        cpu.reconcile_cpu_tiers();

        let counted: u32 = cpu.cpu_tiers.iter().map(|tier| tier.core_count).sum();
        let assigned = cpu
            .per_core_utilization
            .iter()
            .filter(|core| core.tier_index.is_some())
            .count() as u32;
        assert_eq!(counted, assigned);
        assert_eq!(counted, 18);
    }

    #[test]
    fn reconcile_leaves_a_complete_apple_topology_unchanged() {
        // macOS assigns every core to a tier, so reconciliation must not
        // change the list (the two-tier UI output depends on it).
        let tiers = apple(0, 8, 4).to_cpu_tiers();
        let mut cpu = cpu_with(
            tiers.clone(),
            (0..12)
                .map(|core_id| CoreUtilization {
                    core_id,
                    core_type: if core_id < 8 {
                        CoreType::Performance
                    } else {
                        CoreType::Efficiency
                    },
                    utilization: 10.0,
                    tier_index: Some(if core_id < 8 { 0 } else { 1 }),
                })
                .collect(),
        );

        cpu.reconcile_cpu_tiers();

        assert_eq!(cpu.cpu_tiers, tiers);
        assert_eq!(cpu.per_core_utilization[0].tier_index, Some(0));
        assert_eq!(cpu.per_core_utilization[11].tier_index, Some(1));
    }

    #[test]
    fn reconcile_clears_stray_indices_with_an_empty_tier_list() {
        let mut cpu = cpu_with(
            Vec::new(),
            vec![core_with_tier(0, Some(3)), core_with_tier(1, None)],
        );

        cpu.reconcile_cpu_tiers();

        assert!(cpu.cpu_tiers.is_empty());
        assert!(
            cpu.per_core_utilization
                .iter()
                .all(|core| core.tier_index.is_none())
        );
    }
}
