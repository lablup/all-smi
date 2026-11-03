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

use std::collections::{BTreeMap, HashMap, HashSet};
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
use crate::device::{
    CoreType, CoreUtilization, CpuInfo, CpuPlatformType, CpuReader, CpuSocketInfo, CpuTier,
};
use crate::utils::command::new_command;
use crate::utils::system::get_hostname;
use crate::utils::{hz_to_mhz, khz_to_mhz, millicelsius_to_celsius};

type CpuInfoParseResult = Result<
    (
        String,
        String,
        CpuPlatformType,
        u32,
        u32,
        u32,
        u32,
        u32,
        u32,
    ),
    Box<dyn std::error::Error>,
>;

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
    // Use Option<Option<u32>> to distinguish:
    // - None: not cached yet
    // - Some(None): lscpu was called but failed
    // - Some(Some(value)): lscpu succeeded with value
    cached_lscpu_cache_size: RwLock<Option<Option<u32>>>,
    // Cache entire lscpu output to avoid multiple calls
    cached_lscpu_output: RwLock<Option<String>>,
    container_info: &'static ContainerInfo,
    // System handle for CPU monitoring
    system: RwLock<System>,
    // Track if we've done the first refresh
    first_refresh_done: RwLock<bool>,
    /// Static tier topology, read from sysfs once at construction and never
    /// per tick (issue #473).
    topology: CpuTopology,
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

        // Topology is static for the process lifetime, so it is read once here
        // and never from the per-tick paths (issue #473).
        let topology = detect_cpu_tiers(Path::new("/sys"), CONTAINER_INFO.cpuset_cpus.as_deref());

        Self {
            cached_lscpu_cache_size: RwLock::new(None),
            cached_lscpu_output: RwLock::new(None),
            container_info: &*CONTAINER_INFO,
            system: RwLock::new(system),
            first_refresh_done: RwLock::new(false),
            topology,
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

    fn get_lscpu_output(&self) -> Option<String> {
        // Check cache first
        if let Some(ref cached) = *self.cached_lscpu_output.read().unwrap() {
            return Some(cached.clone());
        }

        // Run lscpu once and cache the result
        if let Ok(output) = new_command("lscpu").output()
            && let Ok(lscpu_output) = String::from_utf8(output.stdout)
        {
            *self.cached_lscpu_output.write().unwrap() = Some(lscpu_output.clone());
            return Some(lscpu_output);
        }

        None
    }

    fn get_cpu_info_from_proc(&self) -> Result<CpuInfo, Box<dyn std::error::Error>> {
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

        // Read /proc/cpuinfo for CPU details
        let cpuinfo_content = fs::read_to_string("/proc/cpuinfo")?;
        let (
            cpu_model,
            architecture,
            platform_type,
            mut socket_count,
            mut total_cores,
            mut total_threads,
            base_frequency,
            max_frequency,
            mut cache_size,
        ) = self.parse_cpuinfo(&cpuinfo_content)?;

        // Adjust core/thread counts based on container limits
        if self.container_info.is_container {
            // If in a container, adjust the reported cores based on CPU quota
            let effective_cores = self.container_info.effective_cpu_count.ceil() as u32;

            // If cpuset is specified, use its count
            if let Some(cpuset) = &self.container_info.cpuset_cpus {
                total_cores = cpuset.len() as u32;
                total_threads = total_cores; // Assume no hyperthreading for simplicity
            } else if effective_cores < total_cores {
                // Use quota-based limit if it's more restrictive
                total_cores = effective_cores;
                total_threads = effective_cores;
            }

            // Container typically appears as single socket
            socket_count = 1;
        }

        // If cache_size is 0, try to get it from lscpu
        if cache_size == 0
            && let Some(lscpu_cache) = self.get_cache_size_from_lscpu()
        {
            cache_size = lscpu_cache;
        }

        // Get overall CPU utilization from sysinfo
        let overall_utilization = self.system.read().unwrap().global_cpu_usage() as f64;

        // Read /proc/stat only to determine which cores are active
        let stat_content = fs::read_to_string("/proc/stat")?;
        let (per_socket_info, per_core_utilization) = if self.container_info.is_container {
            // Use container-aware parsing to determine active cores
            let (_stat_utilization, active_cores) =
                parse_cpu_stat_with_container_limits(&stat_content, self.container_info);

            // Convert active cores to per-core utilization
            let mut core_utils = Vec::new();
            // Limit the number of cores displayed based on container limits
            let max_cores_to_display = if self.container_info.cpuset_cpus.is_some() {
                active_cores.len()
            } else {
                // If no cpuset, limit to effective CPU count
                self.container_info.effective_cpu_count.ceil() as usize
            };

            // Use sysinfo to get accurate CPU utilization with delta calculation
            let system = self.system.read().unwrap();
            let cpus = system.cpus();

            for (idx, &core_id) in active_cores.iter().take(max_cores_to_display).enumerate() {
                // Get utilization from sysinfo which handles delta calculation properly
                let core_util = if (core_id as usize) < cpus.len() {
                    cpus[core_id as usize].usage() as f64
                } else {
                    0.0
                };

                core_utils.push(CoreUtilization {
                    core_id: idx as u32, // Use sequential IDs for display, but read from actual core_id
                    core_type: CoreType::Standard,
                    utilization: core_util,
                    // Look up the real host CPU id before the display id is
                    // renumbered (issue #473).
                    tier_index: self.topology.tier_index_for(core_id),
                });
            }

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
            let (_util, socket_info, core_utils) =
                self.parse_cpu_stat(&stat_content, socket_count)?;
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
            cpu_model,
            architecture,
            platform_type,
            socket_count,
            total_cores,
            total_threads,
            base_frequency_mhz: base_frequency,
            max_frequency_mhz: max_frequency,
            cache_size_mb: cache_size,
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

    fn parse_cpuinfo(&self, content: &str) -> CpuInfoParseResult {
        // Get container info to check CPU allocation
        let container_info = self.container_info;
        let mut cpu_model = String::new();
        let mut architecture = String::new();
        let mut platform_type = CpuPlatformType::Other("Unknown".to_string());

        let mut base_frequency = 0u32;
        let mut max_frequency = 0u32;
        let mut cache_size = 0u32;
        let mut bogomips = 0f64;
        let mut cpu_mhz_values = Vec::new();
        let mut cpu_mhz_by_processor: HashMap<u32, f64> = HashMap::new();
        let mut current_processor_id = None;

        let mut physical_ids = std::collections::HashSet::new();
        let mut processor_count = 0u32;
        let mut cpu_implementer = String::new();
        let mut cpu_part = String::new();

        for line in content.lines() {
            if line.starts_with("model name") {
                if let Some(value) = line.split(':').nth(1) {
                    cpu_model = value.trim().to_string();

                    // Determine platform type from model name
                    if cpu_model.to_lowercase().contains("intel") {
                        platform_type = CpuPlatformType::Intel;
                    } else if cpu_model.to_lowercase().contains("amd") {
                        platform_type = CpuPlatformType::Amd;
                    } else if cpu_model.to_lowercase().contains("arm") {
                        platform_type = CpuPlatformType::Arm;
                    }
                }
            } else if line.starts_with("processor") {
                if let Some(value) = line.split(':').nth(1)
                    && let Ok(id) = value.trim().parse::<u32>()
                {
                    current_processor_id = Some(id);
                }
                processor_count += 1;
            } else if line.starts_with("physical id") {
                if let Some(value) = line.split(':').nth(1)
                    && let Ok(id) = value.trim().parse::<u32>()
                {
                    physical_ids.insert(id);
                }
            } else if line.starts_with("cpu MHz") {
                if let Some(value) = line.split(':').nth(1)
                    && let Ok(freq) = value.trim().parse::<f64>()
                    && freq > 0.0
                {
                    cpu_mhz_values.push(freq);
                    // Map frequency to processor ID if available
                    if let Some(proc_id) = current_processor_id {
                        cpu_mhz_by_processor.insert(proc_id, freq);
                    }
                }
            } else if line.starts_with("cache size") && cache_size == 0 {
                if let Some(value) = line.split(':').nth(1) {
                    let value = value.trim();
                    if let Some(size_str) = value.split_whitespace().next()
                        && let Ok(size) = size_str.parse::<u32>()
                    {
                        cache_size = size / 1024; // Convert KB to MB
                    }
                }
            } else if line.starts_with("CPU implementer") {
                if let Some(value) = line.split(':').nth(1) {
                    cpu_implementer = value.trim().to_string();
                }
            } else if line.starts_with("CPU part") {
                if let Some(value) = line.split(':').nth(1) {
                    cpu_part = value.trim().to_string();
                }
            } else if line.starts_with("bogomips")
                && bogomips == 0.0
                && let Some(value) = line.split(':').nth(1)
                && let Ok(bogo) = value.trim().parse::<f64>()
            {
                bogomips = bogo;
            }
        }

        let socket_count = if physical_ids.is_empty() {
            1
        } else {
            physical_ids.len() as u32
        };
        let total_threads = processor_count;

        // Try to get core count from /proc/cpuinfo siblings field or estimate
        let total_cores = total_threads; // Default assumption, may be incorrect with hyperthreading

        // Try to get architecture from uname
        if let Ok(output) = new_command("uname").arg("-m").output() {
            architecture = String::from_utf8_lossy(&output.stdout).trim().to_string();

            // If architecture is ARM and we don't have a CPU model, construct one
            if (architecture == "aarch64"
                || architecture == "arm64"
                || architecture.starts_with("arm"))
                && cpu_model.is_empty()
            {
                platform_type = CpuPlatformType::Arm;

                // Try to construct a model name from implementer and part
                if !cpu_implementer.is_empty() || !cpu_part.is_empty() {
                    let implementer_name = match cpu_implementer.as_str() {
                        "0x41" => "ARM",
                        "0x42" => "Broadcom",
                        "0x43" => "Cavium",
                        "0x44" => "DEC",
                        "0x4e" => "NVIDIA",
                        "0x50" => "APM",
                        "0x51" => "Qualcomm",
                        "0x53" => "Samsung",
                        "0x54" => "HiSilicon",
                        "0x56" => "Marvell",
                        "0x61" => "Apple",
                        "0x66" => "Faraday",
                        "0x69" => "Intel",
                        _ => "Unknown",
                    };

                    cpu_model = format!("{implementer_name} ARM Processor");
                    if !cpu_part.is_empty() {
                        cpu_model.push_str(&format!(" (Part: {cpu_part})"));
                    }
                } else {
                    cpu_model = "ARM Processor".to_string();
                }
            }
        }

        // Calculate average frequency from cpu MHz values
        // If in container, only use CPUs assigned to the container
        if let Some(ref cpuset) = container_info.cpuset_cpus {
            // Container mode: only average frequencies from assigned CPUs
            let mut container_cpu_freqs = Vec::new();
            for &cpu_id in cpuset {
                if let Some(&freq) = cpu_mhz_by_processor.get(&cpu_id) {
                    container_cpu_freqs.push(freq);
                }
            }
            if !container_cpu_freqs.is_empty() {
                let avg_freq =
                    container_cpu_freqs.iter().sum::<f64>() / container_cpu_freqs.len() as f64;
                base_frequency = avg_freq as u32;
                // Container CPU frequency: Using container CPUs from cpuset
            }
        } else if !cpu_mhz_values.is_empty() {
            // Host mode: use all CPU frequencies
            let avg_freq = cpu_mhz_values.iter().sum::<f64>() / cpu_mhz_values.len() as f64;
            base_frequency = avg_freq as u32;
        }

        // Try to get frequency from cpufreq
        // If in container, try to read from one of the assigned CPUs
        let cpu_to_check = if let Some(ref cpuset) = container_info.cpuset_cpus {
            cpuset.first().copied().unwrap_or(0u32)
        } else {
            0u32
        };

        // Try multiple cpufreq paths (some ARM systems use different paths)
        let cpufreq_paths = [
            format!("/sys/devices/system/cpu/cpu{cpu_to_check}/cpufreq/cpuinfo_max_freq"),
            format!("/sys/devices/system/cpu/cpu{cpu_to_check}/cpufreq/scaling_max_freq"),
            "/sys/devices/system/cpu/cpufreq/policy0/cpuinfo_max_freq".to_string(),
            "/sys/devices/system/cpu/cpufreq/policy0/scaling_max_freq".to_string(),
        ];

        for path in &cpufreq_paths {
            if let Ok(content) = fs::read_to_string(path)
                && let Ok(freq_khz) = content.trim().parse::<u32>()
            {
                max_frequency = khz_to_mhz(freq_khz);
                // Found max frequency from scaling_max_freq
                break;
            }
        }

        // Try to get current frequency for base frequency if we don't have it
        if base_frequency == 0 {
            let scaling_paths = [
                format!("/sys/devices/system/cpu/cpu{cpu_to_check}/cpufreq/scaling_cur_freq"),
                format!("/sys/devices/system/cpu/cpu{cpu_to_check}/cpufreq/cpuinfo_cur_freq"),
                "/sys/devices/system/cpu/cpufreq/policy0/scaling_cur_freq".to_string(),
                "/sys/devices/system/cpu/cpufreq/policy0/cpuinfo_cur_freq".to_string(),
            ];

            for path in &scaling_paths {
                if let Ok(content) = fs::read_to_string(path)
                    && let Ok(freq_khz) = content.trim().parse::<u32>()
                {
                    base_frequency = khz_to_mhz(freq_khz);
                    // Found current frequency from scaling_cur_freq
                    break;
                }
            }
        }

        // If still no base frequency, try cpuinfo_min_freq
        if base_frequency == 0 {
            let min_freq_paths = [
                format!("/sys/devices/system/cpu/cpu{cpu_to_check}/cpufreq/cpuinfo_min_freq"),
                format!("/sys/devices/system/cpu/cpu{cpu_to_check}/cpufreq/scaling_min_freq"),
                "/sys/devices/system/cpu/cpufreq/policy0/cpuinfo_min_freq".to_string(),
                "/sys/devices/system/cpu/cpufreq/policy0/scaling_min_freq".to_string(),
            ];

            for path in &min_freq_paths {
                if let Ok(content) = fs::read_to_string(path)
                    && let Ok(freq_khz) = content.trim().parse::<u32>()
                {
                    base_frequency = khz_to_mhz(freq_khz);
                    // Using min frequency from scaling_min_freq
                    break;
                }
            }
        }

        if max_frequency == 0 {
            max_frequency = base_frequency;
        }

        // If we still don't have frequencies, try lscpu command as fallback
        if base_frequency == 0
            && max_frequency == 0
            && let Some(lscpu_output) = self.get_lscpu_output()
        {
            for line in lscpu_output.lines() {
                if line.starts_with("CPU MHz:") {
                    if let Some(value) = line.split(':').nth(1)
                        && let Ok(freq) = value.trim().parse::<f64>()
                    {
                        base_frequency = freq as u32;
                        break;
                    }
                } else if line.starts_with("CPU max MHz:") {
                    if let Some(value) = line.split(':').nth(1)
                        && let Ok(freq) = value.trim().parse::<f64>()
                    {
                        max_frequency = freq as u32;
                    }
                } else if line.starts_with("CPU min MHz:")
                    && base_frequency == 0
                    && let Some(value) = line.split(':').nth(1)
                    && let Ok(freq) = value.trim().parse::<f64>()
                {
                    base_frequency = freq as u32;
                }
            }
        }

        // Try DMI/sysfs for ARM systems
        if base_frequency == 0 && platform_type == CpuPlatformType::Arm {
            // Check for ARM-specific frequency files
            if let Ok(content) = fs::read_to_string("/sys/devices/system/cpu/cpu0/clock_rate")
                && let Ok(freq_hz) = content.trim().parse::<u64>()
            {
                base_frequency = hz_to_mhz(freq_hz);
                // Found clock_rate from device-tree
            }

            // Try to read from device tree
            if base_frequency == 0
                && let Ok(content) =
                    fs::read_to_string("/proc/device-tree/cpus/cpu@0/clock-frequency")
            {
                // Device tree values are often in big-endian format
                if content.len() >= 4 {
                    let bytes = content.as_bytes();
                    let freq_hz =
                        u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as u64;
                    if freq_hz > 0 {
                        base_frequency = hz_to_mhz(freq_hz);
                        // Found device-tree frequency
                    }
                }
            }
        }

        // Use BogoMIPS as last resort to estimate frequency
        if base_frequency == 0 && bogomips > 0.0 {
            // BogoMIPS calculation varies by architecture
            let estimated_freq = match platform_type {
                CpuPlatformType::Arm => {
                    // On ARM, BogoMIPS is often close to actual frequency
                    bogomips as u32
                }
                _ => {
                    // On x86, BogoMIPS is roughly 2x the frequency
                    (bogomips / 2.0) as u32
                }
            };
            if estimated_freq > 0 {
                base_frequency = estimated_freq;
                // Estimated frequency from BogoMIPS
            }
        }

        // Final fallback - use architecture-specific defaults
        if base_frequency == 0 && max_frequency == 0 {
            base_frequency = match platform_type {
                CpuPlatformType::Arm => 2000, // Common ARM frequency
                _ => 1000,                    // Generic default
            };
            max_frequency = base_frequency;
            // Using default frequency for platform
        }

        Ok((
            cpu_model,
            architecture,
            platform_type,
            socket_count,
            total_cores,
            total_threads,
            base_frequency,
            max_frequency,
            cache_size,
        ))
    }

    fn parse_cpu_stat(&self, _content: &str, socket_count: u32) -> CpuStatParseResult {
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
        // Try to read from various thermal zone files
        let thermal_paths = [
            "/sys/class/thermal/thermal_zone0/temp",
            "/sys/class/thermal/thermal_zone1/temp",
            "/sys/class/hwmon/hwmon0/temp1_input",
            "/sys/class/hwmon/hwmon1/temp1_input",
        ];

        for path in &thermal_paths {
            if let Ok(content) = fs::read_to_string(path)
                && let Ok(temp_millicelsius) = content.trim().parse::<u32>()
            {
                return Some(millicelsius_to_celsius(temp_millicelsius));
            }
        }

        None
    }

    fn get_cache_size_from_lscpu(&self) -> Option<u32> {
        // Check if we have cached value
        if let Some(cached_result) = &*self.cached_lscpu_cache_size.read().unwrap() {
            // We've already tried lscpu, return the cached result
            return *cached_result;
        }

        // Try to get cache size from cached lscpu output
        let result = if let Some(output_str) = self.get_lscpu_output() {
            // Look for cache lines (L3 preferred, then L2 as fallback)
            // Note: On some systems like Jetson, the lines might be indented
            let mut found_l3_cache = None;
            let mut found_l2_cache = None;

            for line in output_str.lines() {
                let line = line.trim();

                // Check for L3 cache (handle both "L3:" and "L3 cache:" formats)
                if (line.starts_with("L3:") || line.starts_with("L3 cache:"))
                    && let Some(size_part) = line.split(':').nth(1)
                {
                    let size_part = size_part.trim();

                    // Parse different formats: "4 MiB", "4MiB", "4096 KiB", etc.
                    // Also handle format with instances: "4 MiB (2 instances)"
                    let parts: Vec<&str> = size_part.split_whitespace().collect();
                    if !parts.is_empty()
                        && let Ok(size) = parts[0].parse::<f64>()
                    {
                        let unit = if parts.len() > 1 {
                            parts[1].to_lowercase()
                        } else {
                            // Try to extract unit from the first part if it's like "4MiB"
                            let num_end = parts[0]
                                .find(|c: char| !c.is_numeric() && c != '.')
                                .unwrap_or(parts[0].len());
                            parts[0][num_end..].to_lowercase()
                        };

                        let size_mb = match unit.as_str() {
                            "mib" | "mb" => size as u32,
                            "kib" | "kb" => (size / 1024.0) as u32,
                            "gib" | "gb" => (size * 1024.0) as u32,
                            _ => 0,
                        };

                        if size_mb > 0 {
                            found_l3_cache = Some(size_mb);
                        }
                    }
                }

                // Check for L2 cache as fallback (handle both "L2:" and "L2 cache:" formats)
                if (line.starts_with("L2:") || line.starts_with("L2 cache:"))
                    && found_l3_cache.is_none()
                    && let Some(size_part) = line.split(':').nth(1)
                {
                    let size_part = size_part.trim();

                    let parts: Vec<&str> = size_part.split_whitespace().collect();
                    if !parts.is_empty()
                        && let Ok(size) = parts[0].parse::<f64>()
                    {
                        let unit = if parts.len() > 1 {
                            parts[1].to_lowercase()
                        } else {
                            let num_end = parts[0]
                                .find(|c: char| !c.is_numeric() && c != '.')
                                .unwrap_or(parts[0].len());
                            parts[0][num_end..].to_lowercase()
                        };

                        let size_mb = match unit.as_str() {
                            "mib" | "mb" => size as u32,
                            "kib" | "kb" => (size / 1024.0) as u32,
                            "gib" | "gb" => (size * 1024.0) as u32,
                            _ => 0,
                        };

                        if size_mb > 0 {
                            found_l2_cache = Some(size_mb);
                        }
                    }
                }
            }

            // Return L3 if found, otherwise L2
            found_l3_cache.or(found_l2_cache)
        } else {
            None
        };

        // Cache the result (whether success or failure)
        *self.cached_lscpu_cache_size.write().unwrap() = Some(result);

        result
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
