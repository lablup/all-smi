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

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::RwLock;
use sysinfo::System;

use chrono::Local;
use once_cell::sync::Lazy;

use crate::device::container_info::{ContainerInfo, parse_cpu_stat_with_container_limits};
use crate::device::cpu_topology::{
    self, CoreTier, decode_midr, group_cpus_by_capacity, parse_cpumask_list, tier_label_for_rank,
    tier_of, tier_utilization,
};
use crate::device::{CoreType, CoreUtilization, CpuInfo, CpuReader, CpuSocketInfo, CpuTier};
use crate::utils::system::get_hostname;

mod cpuinfo;
mod temperature;
use cpuinfo::{FrequencySource, StaticCpuInfo};
#[cfg_attr(not(feature = "cli"), allow(unused_imports))]
pub(crate) use temperature::{ASUS_WMI_SENSORS_DRIVER, CpuTempResolution, TempRule};
use temperature::{CPU_TEMP_SOURCE, CpuTempSource};

/// The process-wide CPU temperature resolution plus a current reading, for
/// `all-smi doctor`. Resolves through the same cached source the readers use,
/// so calling it never adds a second sysfs scan.
#[cfg_attr(not(feature = "cli"), allow(dead_code))]
pub(crate) fn cpu_temp_status() -> (&'static CpuTempResolution, Option<u32>) {
    (CPU_TEMP_SOURCE.resolution(), CPU_TEMP_SOURCE.read_celsius())
}

type CpuStatParseResult =
    Result<(f64, Vec<CpuSocketInfo>, Vec<CoreUtilization>), Box<dyn std::error::Error>>;

// Cache container detection result globally to avoid repeated filesystem operations
static CONTAINER_INFO: Lazy<ContainerInfo> = Lazy::new(ContainerInfo::detect);

/// Static CPU tier topology detected once per reader (issue #473).
///
/// `tiers` carries labels and core counts; only the per-tick utilization is
/// refreshed. `tier_by_cpu` maps a **host** CPU id to its tier position, which
/// is what the container path needs before it renumbers `core_id`
/// sequentially.
#[derive(Debug, Clone, Default)]
struct CpuTopology {
    tiers: Vec<CpuTier>,
    tier_by_cpu: Vec<Option<u8>>,
}

/// Upper bound on detected tiers. Positions are stored as `u8` and the remote
/// parser caps at the same value, so a machine with more distinct firmware
/// capacities than this is truncated rather than risk an index overflow.
const MAX_DETECTED_TIERS: usize = 64;

/// Highest CPU id the tier map tracks.
///
/// `tier_by_cpu` is indexed by CPU id, so a single implausible id would size the
/// vector to that id. A real machine numbers its logical CPUs in the low
/// thousands, and this matches the cpumask expansion bound, so anything above
/// it is treated as untiered instead of sizing an allocation from a file that a
/// corrupted or attacker-controlled sysfs could supply.
const MAX_TRACKED_CPU_ID: u32 = 8191;

impl CpuTopology {
    fn tier_index_for(&self, cpu_id: u32) -> Option<u8> {
        self.tier_by_cpu.get(cpu_id as usize).copied().flatten()
    }
}

/// Detect the CPU tier topology from sysfs.
///
/// Precedence, each step falling through when it cannot confirm at least two
/// tiers:
/// 1. Intel hybrid PMU cpumasks (`cpu_core/cpus` -> `P`, `cpu_atom/cpus` -> `E`).
/// 2. arm64 `MIDR_EL1` part numbers mapped through [`tier_of`].
/// 3. Firmware `cpu_capacity`, grouped descending and labelled through
///    [`tier_label_for_rank`].
/// 4. Empty (homogeneous or undetected).
///
/// `root` is `/sys` in production and a fixture directory in tests. `allowed`
/// is the container cpuset; `None` means every CPU on the host participates.
/// The function never fails: an unreadable source is skipped and the affected
/// cores stay untiered.
fn detect_cpu_tiers(root: &Path, allowed: Option<&[u32]>) -> CpuTopology {
    let mut cpu_ids = enumerate_cpu_ids(root);
    if let Some(allowed) = allowed {
        let allowed: HashSet<u32> = allowed.iter().copied().collect();
        cpu_ids.retain(|id| allowed.contains(id));
    }
    if cpu_ids.is_empty() {
        return CpuTopology::default();
    }

    detect_intel_hybrid(root, &cpu_ids)
        .or_else(|| detect_arm_midr(root, &cpu_ids))
        .or_else(|| detect_capacity(root, &cpu_ids))
        .unwrap_or_default()
}

/// Enumerate logical CPU ids from `devices/system/cpu/cpuN` directories,
/// falling back to the kernel `possible` cpumask when the directory listing is
/// unavailable.
fn enumerate_cpu_ids(root: &Path) -> Vec<u32> {
    let cpu_dir = root.join("devices/system/cpu");
    let mut ids = Vec::new();

    if let Ok(entries) = fs::read_dir(&cpu_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(suffix) = name.strip_prefix("cpu")
                && let Ok(id) = suffix.parse::<u32>()
            {
                ids.push(id);
            }
        }
    }

    if ids.is_empty()
        && let Ok(mask) = fs::read_to_string(cpu_dir.join("possible"))
    {
        ids = parse_cpumask_list(&mask);
    }

    // A directory entry or cpumask may name `cpuN` with an arbitrary `N`; the
    // per-CPU tier map is indexed by id, so ignore ids beyond the tracked range
    // rather than let one entry size an allocation proportional to it.
    ids.retain(|id| *id <= MAX_TRACKED_CPU_ID);
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Intel hybrid: map the `cpu_core` PMU cpumask to `P` and `cpu_atom` to `E`.
///
/// The kernel registers exactly two hybrid classes, so there is no third tier
/// to invent. Requires at least one core in each class.
fn detect_intel_hybrid(root: &Path, cpu_ids: &[u32]) -> Option<CpuTopology> {
    let base = root.join("bus/event_source/devices");
    let p_mask = fs::read_to_string(base.join("cpu_core/cpus")).ok()?;
    let e_mask = fs::read_to_string(base.join("cpu_atom/cpus")).ok()?;

    let allowed: HashSet<u32> = cpu_ids.iter().copied().collect();
    let mut performance: Vec<u32> = parse_cpumask_list(&p_mask)
        .into_iter()
        .filter(|id| allowed.contains(id))
        .collect();
    let mut efficiency: Vec<u32> = parse_cpumask_list(&e_mask)
        .into_iter()
        .filter(|id| allowed.contains(id))
        .collect();

    if performance.is_empty() || efficiency.is_empty() {
        return None;
    }

    performance.sort_unstable();
    efficiency.sort_unstable();
    Some(topology_from_labeled_groups(vec![
        ("P".to_string(), performance),
        ("E".to_string(), efficiency),
    ]))
}

/// arm64: classify each CPU by its `MIDR_EL1` part number.
///
/// `Big` becomes `P`, `Mid` becomes `M` and `Little` becomes `E`. Unknown or
/// unreadable parts stay untiered; fewer than two confirmed tiers falls
/// through to the capacity fallback.
fn detect_arm_midr(root: &Path, cpu_ids: &[u32]) -> Option<CpuTopology> {
    let mut by_tier: BTreeMap<CoreTier, Vec<u32>> = BTreeMap::new();

    for &cpu_id in cpu_ids {
        let path = root.join(format!(
            "devices/system/cpu/cpu{cpu_id}/regs/identification/midr_el1"
        ));
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Some(part) = decode_midr(&text) else {
            continue;
        };
        let Some(tier) = tier_of(part) else {
            continue;
        };
        by_tier.entry(tier).or_default().push(cpu_id);
    }

    if by_tier.len() < 2 {
        return None;
    }

    // `BTreeMap` orders `Little < Mid < Big`; the tier list must be fastest
    // first, so pull them explicitly.
    let groups = [CoreTier::Big, CoreTier::Mid, CoreTier::Little]
        .into_iter()
        .filter_map(|tier| {
            by_tier
                .remove(&tier)
                .map(|cores| (tier.label().to_string(), cores))
        })
        .collect();
    Some(topology_from_labeled_groups(groups))
}

/// Firmware capacity fallback: group CPUs by `cpu_capacity`, order the groups
/// by capacity descending, and emit them only when at least two distinct
/// values exist.
fn detect_capacity(root: &Path, cpu_ids: &[u32]) -> Option<CpuTopology> {
    let mut capacities = Vec::new();
    for &cpu_id in cpu_ids {
        let path = root.join(format!("devices/system/cpu/cpu{cpu_id}/cpu_capacity"));
        if let Ok(text) = fs::read_to_string(&path)
            && let Ok(capacity) = text.trim().parse::<u32>()
            && capacity > 0
        {
            capacities.push((cpu_id, capacity));
        }
    }

    let mut groups = group_cpus_by_capacity(&capacities);
    if groups.len() < 2 {
        return None;
    }
    // Keep the fastest groups; cores in a truncated group stay untiered.
    groups.truncate(MAX_DETECTED_TIERS);

    Some(topology_from_labeled_groups(
        groups
            .into_iter()
            .enumerate()
            .map(|(position, (_, cores))| (tier_label_for_rank(position), cores))
            .collect(),
    ))
}

/// Build a topology from ordered `(label, cpu ids)` groups, fastest first.
///
/// The list is capped at [`MAX_DETECTED_TIERS`] so tier positions always fit
/// a `u8`; callers pass groups already ordered fastest first, so the cap keeps
/// the fastest ones.
fn topology_from_labeled_groups(mut groups: Vec<(String, Vec<u32>)>) -> CpuTopology {
    groups.truncate(MAX_DETECTED_TIERS);
    // Defense in depth: an id beyond the tracked range would size `tier_by_cpu`
    // to that id, so it is dropped here as well as in `enumerate_cpu_ids`.
    for (_, cores) in &mut groups {
        cores.retain(|id| *id <= MAX_TRACKED_CPU_ID);
    }
    let total = groups.len();
    let max_cpu_id = groups
        .iter()
        .flat_map(|(_, cores)| cores.iter().copied())
        .max()
        .unwrap_or(0);
    let mut tier_by_cpu = vec![None; max_cpu_id as usize + 1];
    let mut tiers = Vec::with_capacity(total);

    for (position, (label, cores)) in groups.into_iter().enumerate() {
        for cpu_id in &cores {
            if let Some(slot) = tier_by_cpu.get_mut(*cpu_id as usize) {
                *slot = Some(position as u8);
            }
        }
        tiers.push(CpuTier {
            label,
            core_type: cpu_topology::coarse_core_type(position, total),
            core_count: cores.len() as u32,
            utilization: 0.0,
            frequency_mhz: None,
            l2_cache_mb: None,
        });
    }

    CpuTopology { tiers, tier_by_cpu }
}

pub struct LinuxCpuReader {
    container_info: &'static ContainerInfo,
    // System handle for CPU monitoring
    system: RwLock<System>,
    // Track if we've done the first refresh
    first_refresh_done: RwLock<bool>,
    /// Static tier topology, read from sysfs once at construction and never
    /// per tick (issue #473).
    topology: CpuTopology,
    /// Process-wide CPU temperature source, shared by every reader instance.
    temp_source: &'static CpuTempSource,
    /// The kernel's machine string (`uname -m`), read once through
    /// `uname(2)` instead of spawning `uname` every tick (issue #504). The
    /// tick reads `architecture` from `static_info`; this copy is what the
    /// test-only full parser runs with.
    #[cfg_attr(not(test), allow(dead_code))]
    machine: String,
    /// Everything `/proc/cpuinfo` says that does not change per tick, read
    /// once at construction (issue #504). `None` only when `/proc/cpuinfo`
    /// could not be read, in which case every sample reports the error.
    static_info: Option<StaticCpuInfo>,
    /// Per-tick current-frequency source (issue #504).
    frequency: FrequencySource,
    /// Host CPU ids a container may use, intersected with the `cpuN` lines of
    /// `/proc/stat`. Read once at construction: neither the cpuset nor the
    /// CPU list changes per tick (issue #504). Empty outside a container.
    container_active_cores: Vec<u32>,
}

impl Default for LinuxCpuReader {
    fn default() -> Self {
        Self::new()
    }
}

impl LinuxCpuReader {
    pub fn new() -> Self {
        // Create system with minimal initialization - delay CPU refresh until needed
        let system = crate::utils::system::new_system();
        let container_info: &'static ContainerInfo = &CONTAINER_INFO;
        let cpuset = container_info.cpuset_cpus.as_deref();

        // Topology is static for the process lifetime, so it is read once here
        // and never from the per-tick paths (issue #473).
        let topology = detect_cpu_tiers(Path::new("/sys"), cpuset);

        // Static CPU description, frequency source and the container's core
        // list are likewise read once here (issue #504).
        let machine = cpuinfo::machine_string();
        let static_info = match fs::read_to_string("/proc/cpuinfo") {
            Ok(content) => match StaticCpuInfo::load(&content, &machine, container_info) {
                Ok(info) => Some(info),
                Err(e) => {
                    eprintln!("Error parsing /proc/cpuinfo: {e}");
                    None
                }
            },
            Err(e) => {
                eprintln!("Error reading /proc/cpuinfo: {e}");
                None
            }
        };
        let frequency = FrequencySource::resolve(Path::new("/sys"), cpuset);
        let container_active_cores = if container_info.is_container {
            fs::read_to_string("/proc/stat")
                .map(|stat| parse_cpu_stat_with_container_limits(&stat, container_info).1)
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        Self {
            container_info,
            system: RwLock::new(system),
            first_refresh_done: RwLock::new(false),
            topology,
            temp_source: &CPU_TEMP_SOURCE,
            machine,
            static_info,
            frequency,
            container_active_cores,
        }
    }

    /// Build a reader that resolves CPU temperature through `source` instead of
    /// the process-wide `/sys` instance, so tests can count scans across readers.
    #[cfg(test)]
    fn with_temp_source(source: &'static CpuTempSource) -> Self {
        Self {
            temp_source: source,
            ..Self::new()
        }
    }

    /// Clone the static topology and refresh only the per-tick utilization.
    fn resolved_tiers(&self, per_core: &[CoreUtilization]) -> Vec<CpuTier> {
        let mut tiers = self.topology.tiers.clone();
        for (position, tier) in tiers.iter_mut().enumerate() {
            tier.utilization = tier_utilization(per_core, position);
        }
        tiers
    }

    /// Parse a full `/proc/cpuinfo` with this reader's machine string and
    /// container view. Only construction and tests call it; the tick reads
    /// the static result instead.
    #[cfg(test)]
    fn parse_cpuinfo(&self, content: &str) -> cpuinfo::CpuInfoParseResult {
        cpuinfo::parse_cpuinfo(content, &self.machine, self.container_info)
    }

    /// The current base frequency for this tick (issue #504).
    ///
    /// `scaling_cur_freq` averaged over the CPUs that count when cpufreq
    /// exists, so `/proc/cpuinfo` is not regenerated by the kernel every
    /// tick; otherwise only the `cpu MHz` lines of `/proc/cpuinfo`; otherwise
    /// the value computed at construction.
    fn current_base_frequency(&self, static_base: u32) -> u32 {
        let sampled = if self.frequency.is_empty() {
            fs::read_to_string("/proc/cpuinfo")
                .ok()
                .and_then(|content| {
                    cpuinfo::parse_cpu_mhz(&content, self.container_info.cpuset_cpus.as_deref())
                })
        } else {
            self.frequency.sample()
        };
        sampled.unwrap_or(static_base)
    }

    fn get_cpu_info_from_proc(&self) -> Result<CpuInfo, Box<dyn std::error::Error>> {
        let static_info = self
            .static_info
            .as_ref()
            .ok_or("/proc/cpuinfo was unreadable when the CPU reader was built")?;

        // OPTIMIZATION: Refresh CPU usage ONCE per collection cycle
        // On first call, do initial refresh with delay to establish baseline
        if !*self.first_refresh_done.read().unwrap() {
            self.system.write().unwrap().refresh_cpu_usage();
            // Minimal delay for initial measurement (only on first call)
            std::thread::sleep(std::time::Duration::from_millis(10));
            *self.first_refresh_done.write().unwrap() = true;
        }
        // Single refresh for current data
        self.system.write().unwrap().refresh_cpu_usage();
        let hostname = get_hostname();
        let instance = hostname.clone();
        let time = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

        let base_frequency = self.current_base_frequency(static_info.base_frequency_mhz);
        let total_cores = static_info.total_cores;
        let total_threads = static_info.total_threads;

        // Get overall CPU utilization from sysinfo
        let overall_utilization = self.system.read().unwrap().global_cpu_usage() as f64;

        // sysinfo's `refresh_cpu_usage` above already read `/proc/stat`; the
        // container's core list was resolved at construction (issue #504).
        let (per_socket_info, per_core_utilization) = if self.container_info.is_container {
            let core_utils = self.container_core_utilization();
            // Create socket info for container
            let socket_info = vec![CpuSocketInfo {
                socket_id: 0,
                utilization: overall_utilization,
                cores: total_cores,
                threads: total_threads,
                temperature: None,
                frequency_mhz: base_frequency,
            }];
            (socket_info, core_utils)
        } else {
            let (_util, socket_info, core_utils) = self.parse_cpu_stat(static_info.socket_count)?;
            (socket_info, core_utils)
        };

        // Try to get CPU temperature (may not be available on all systems)
        let temperature = self.get_cpu_temperature();

        // Power consumption is not readily available on most Linux systems
        let power_consumption = None;

        let cpu_tiers = self.resolved_tiers(&per_core_utilization);

        Ok(CpuInfo {
            index: 0,                  // Overwritten by AllSmi::get_cpu_info when flattening readers
            host_id: hostname.clone(), // For local mode, host_id is just the hostname
            hostname,
            instance,
            cpu_model: static_info.cpu_model.clone(),
            architecture: static_info.architecture.clone(),
            platform_type: static_info.platform_type.clone(),
            socket_count: static_info.socket_count,
            total_cores,
            total_threads,
            base_frequency_mhz: base_frequency,
            max_frequency_mhz: static_info.max_frequency_mhz,
            cache_size_mb: static_info.cache_size_mb,
            utilization: overall_utilization,
            temperature,
            power_consumption,
            per_socket_info,
            apple_silicon_info: None, // Not applicable for Linux
            cpu_tiers,
            per_core_utilization,
            time,
        })
    }

    /// Per-core rows for a container: one per active host core, renumbered
    /// sequentially for display, with utilization and tier read from the real
    /// host CPU id.
    fn container_core_utilization(&self) -> Vec<CoreUtilization> {
        // Limit the number of cores displayed based on container limits
        let max_cores_to_display = if self.container_info.cpuset_cpus.is_some() {
            self.container_active_cores.len()
        } else {
            // If no cpuset, limit to effective CPU count
            self.container_info.effective_cpu_count.ceil() as usize
        };

        // Use sysinfo to get accurate CPU utilization with delta calculation
        let system = self.system.read().unwrap();
        let cpus = system.cpus();

        self.container_active_cores
            .iter()
            .take(max_cores_to_display)
            .enumerate()
            .map(|(idx, &core_id)| CoreUtilization {
                core_id: idx as u32, // Sequential display id, read from the real core_id
                core_type: CoreType::Standard,
                utilization: cpus
                    .get(core_id as usize)
                    .map_or(0.0, |cpu| cpu.usage() as f64),
                // Look up the real host CPU id before the display id is
                // renumbered (issue #473).
                tier_index: self.topology.tier_index_for(core_id),
            })
            .collect()
    }

    fn parse_cpu_stat(&self, socket_count: u32) -> CpuStatParseResult {
        // OPTIMIZATION: Don't refresh here - caller (get_cpu_info_from_proc) already refreshed
        // This avoids duplicate refresh_cpu_usage() calls which was causing high CPU usage
        // However, for direct calls (e.g., tests), we need to ensure cpus() is populated
        {
            let system = self.system.read().unwrap();
            if system.cpus().is_empty() {
                drop(system);
                // Fallback: refresh if not yet initialized (for direct test calls)
                self.system.write().unwrap().refresh_cpu_usage();
                std::thread::sleep(std::time::Duration::from_millis(10));
                self.system.write().unwrap().refresh_cpu_usage();
            }
        }

        let overall_utilization = self.system.read().unwrap().global_cpu_usage() as f64;
        let mut per_socket_info = Vec::new();
        let mut per_core_utilization = Vec::new();

        // Use sysinfo to get per-core utilization
        let system = self.system.read().unwrap();
        let cpus = system.cpus();

        for (core_id, cpu) in cpus.iter().enumerate() {
            let utilization = cpu.usage() as f64;

            // Check if this is a P-core or E-core based on CPU topology
            // For now, we'll use Standard type for all Linux cores
            let core_type = CoreType::Standard;

            per_core_utilization.push(CoreUtilization {
                core_id: core_id as u32,
                core_type,
                utilization,
                // The host path's `core_id` is the host CPU id.
                tier_index: self.topology.tier_index_for(core_id as u32),
            });
        }

        // Sort cores by ID for consistent display
        per_core_utilization.sort_by_key(|c| c.core_id);

        // Create per-socket info (simplified - assumes even distribution across sockets)
        for socket_id in 0..socket_count {
            per_socket_info.push(CpuSocketInfo {
                socket_id,
                utilization: overall_utilization, // Simplified - same as overall
                cores: 0,          // Will be calculated based on total_cores / socket_count
                threads: 0,        // Will be calculated based on total_threads / socket_count
                temperature: None, // Not easily available per socket
                frequency_mhz: 0,  // Will be set from base frequency
            });
        }

        Ok((overall_utilization, per_socket_info, per_core_utilization))
    }

    fn get_cpu_temperature(&self) -> Option<u32> {
        // The sensor is resolved once per process on the first read; each sample
        // then reads only the cached file (issue #492).
        self.temp_source.read_celsius()
    }

    /// L3 (else L2) cache size from the process-wide cached `lscpu` output.
    #[cfg(test)]
    fn get_cache_size_from_lscpu(&self) -> Option<u32> {
        cpuinfo::lscpu_output().and_then(cpuinfo::cache_size_from_lscpu)
    }
}

impl CpuReader for LinuxCpuReader {
    fn get_cpu_info(&self) -> Vec<CpuInfo> {
        match self.get_cpu_info_from_proc() {
            Ok(mut cpu_info) => {
                // Fill in cores and threads per socket
                let cores_per_socket = cpu_info.total_cores / cpu_info.socket_count;
                let threads_per_socket = cpu_info.total_threads / cpu_info.socket_count;

                for socket_info in &mut cpu_info.per_socket_info {
                    socket_info.cores = cores_per_socket;
                    socket_info.threads = threads_per_socket;
                    socket_info.frequency_mhz = cpu_info.base_frequency_mhz;
                }

                // A quota-only container detects tiers across the whole host
                // but emits only the cores it may use, so recount from the
                // cores this payload actually carries (issue #473). Done here,
                // in the reader, so every consumer sees the reconciled list:
                // the local TUI and API loop call this trait method directly
                // rather than going through `AllSmi::get_cpu_info`.
                cpu_info.reconcile_cpu_tiers();

                vec![cpu_info]
            }
            Err(e) => {
                eprintln!("Error reading CPU info: {e}");
                vec![]
            }
        }
    }
}

#[cfg(test)]
#[path = "cpu_linux/tests.rs"]
mod tests;
