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

#[cfg(target_os = "linux")]
use cgroups_rs::fs::Cgroup;
#[cfg(target_os = "linux")]
use cgroups_rs::fs::cpu::CpuController;
#[cfg(target_os = "linux")]
use cgroups_rs::fs::cpuset::CpuSetController;
#[cfg(target_os = "linux")]
use cgroups_rs::fs::hierarchies;
#[cfg(target_os = "linux")]
use cgroups_rs::fs::memory::MemController;

use once_cell::sync::Lazy;
use std::fs;
use std::path::Path;

#[derive(Default, Debug, Clone)]
#[allow(dead_code)]
pub struct MemoryStats {
    pub anon_bytes: u64,
    pub file_bytes: u64,
    pub kernel_stack_bytes: u64,
    pub slab_bytes: u64,
    pub sock_bytes: u64,
    pub file_mapped_bytes: u64,
    pub file_dirty_bytes: u64,
    pub file_writeback_bytes: u64,
}

pub struct ContainerInfo {
    pub is_container: bool,
    // CPU limits
    #[allow(dead_code)]
    pub cpu_quota: Option<i64>,
    #[allow(dead_code)]
    pub cpu_period: Option<u64>,
    #[allow(dead_code)]
    pub cpu_shares: Option<u64>,
    pub cpuset_cpus: Option<Vec<u32>>,
    pub effective_cpu_count: f64,
    // Memory limits
    pub memory_limit_bytes: Option<u64>,
    #[allow(dead_code)]
    pub memory_soft_limit_bytes: Option<u64>,
    #[allow(dead_code)]
    pub memory_swap_limit_bytes: Option<u64>,
    #[allow(dead_code)]
    pub memory_usage_bytes: Option<u64>,
    // Cgroup handle
    #[cfg(target_os = "linux")]
    cgroup: Option<Cgroup>,
}

// Cache container detection data (excluding Cgroup handle)
#[derive(Clone)]
struct CachedContainerData {
    is_container: bool,
    cpu_quota: Option<i64>,
    cpu_period: Option<u64>,
    cpu_shares: Option<u64>,
    cpuset_cpus: Option<Vec<u32>>,
    effective_cpu_count: f64,
    memory_limit_bytes: Option<u64>,
    memory_soft_limit_bytes: Option<u64>,
    memory_swap_limit_bytes: Option<u64>,
}

static CACHED_CONTAINER_DATA: Lazy<CachedContainerData> = Lazy::new(|| {
    let info = ContainerInfo::detect_impl();
    CachedContainerData {
        is_container: info.is_container,
        cpu_quota: info.cpu_quota,
        cpu_period: info.cpu_period,
        cpu_shares: info.cpu_shares,
        cpuset_cpus: info.cpuset_cpus,
        effective_cpu_count: info.effective_cpu_count,
        memory_limit_bytes: info.memory_limit_bytes,
        memory_soft_limit_bytes: info.memory_soft_limit_bytes,
        memory_swap_limit_bytes: info.memory_swap_limit_bytes,
    }
});

impl ContainerInfo {
    pub fn detect() -> Self {
        let cached = &*CACHED_CONTAINER_DATA;

        // Reconstruct ContainerInfo from cached data
        if !cached.is_container {
            return ContainerInfo {
                is_container: false,
                cpu_quota: None,
                cpu_period: None,
                cpu_shares: None,
                cpuset_cpus: None,
                effective_cpu_count: cached.effective_cpu_count,
                memory_limit_bytes: None,
                memory_soft_limit_bytes: None,
                memory_swap_limit_bytes: None,
                memory_usage_bytes: None,
                #[cfg(target_os = "linux")]
                cgroup: None,
            };
        }

        // For containers, reinitialize cgroup handle for fresh memory usage
        #[cfg(target_os = "linux")]
        let cgroup = Self::init_cgroup();

        ContainerInfo {
            is_container: cached.is_container,
            cpu_quota: cached.cpu_quota,
            cpu_period: cached.cpu_period,
            cpu_shares: cached.cpu_shares,
            cpuset_cpus: cached.cpuset_cpus.clone(),
            effective_cpu_count: cached.effective_cpu_count,
            memory_limit_bytes: cached.memory_limit_bytes,
            memory_soft_limit_bytes: cached.memory_soft_limit_bytes,
            memory_swap_limit_bytes: cached.memory_swap_limit_bytes,
            memory_usage_bytes: None, // Will be fetched fresh
            #[cfg(target_os = "linux")]
            cgroup,
        }
    }

    fn detect_impl() -> Self {
        let is_container = Self::is_in_container();

        if !is_container {
            return ContainerInfo {
                is_container: false,
                cpu_quota: None,
                cpu_period: None,
                cpu_shares: None,
                cpuset_cpus: None,
                effective_cpu_count: num_cpus::get() as f64,
                memory_limit_bytes: None,
                memory_soft_limit_bytes: None,
                memory_swap_limit_bytes: None,
                memory_usage_bytes: None,
                #[cfg(target_os = "linux")]
                cgroup: None,
            };
        }

        // Initialize cgroup handle
        #[cfg(target_os = "linux")]
        let cgroup = Self::init_cgroup();

        // Get limits from cgroup
        #[cfg(target_os = "linux")]
        let (cpu_quota, cpu_period, cpu_shares) = Self::get_cpu_limits(&cgroup);
        #[cfg(not(target_os = "linux"))]
        let (cpu_quota, cpu_period, cpu_shares) = (None, None, None);

        #[cfg(target_os = "linux")]
        let cpuset_cpus = Self::get_cpuset_cpus(&cgroup);
        #[cfg(not(target_os = "linux"))]
        let cpuset_cpus = None;

        #[cfg(target_os = "linux")]
        let (
            memory_limit_bytes,
            memory_soft_limit_bytes,
            memory_swap_limit_bytes,
            memory_usage_bytes,
        ) = Self::get_memory_limits(&cgroup);
        #[cfg(not(target_os = "linux"))]
        let (
            memory_limit_bytes,
            memory_soft_limit_bytes,
            memory_swap_limit_bytes,
            memory_usage_bytes,
        ) = (None, None, None, None);

        // Calculate effective CPU count based on quota and period
        let effective_cpu_count =
            Self::calculate_effective_cpus(cpu_quota, cpu_period, cpu_shares, &cpuset_cpus);

        ContainerInfo {
            is_container,
            cpu_quota,
            cpu_period,
            cpu_shares,
            cpuset_cpus,
            effective_cpu_count,
            memory_limit_bytes,
            memory_soft_limit_bytes,
            memory_swap_limit_bytes,
            memory_usage_bytes,
            #[cfg(target_os = "linux")]
            cgroup,
        }
    }

    fn is_in_container() -> bool {
        // Check common container indicators
        if Path::new("/.dockerenv").exists() {
            return true;
        }

        // Check for Kubernetes service account
        if Path::new("/var/run/secrets/kubernetes.io").exists() {
            return true;
        }

        // Check /proc/self/cgroup for container indicators
        if let Ok(cgroup_content) = fs::read_to_string("/proc/self/cgroup") {
            for line in cgroup_content.lines() {
                if line.contains("/docker/")
                    || line.contains("/lxc/")
                    || line.contains("/kubepods/")
                    || line.contains("/containerd/")
                    || line.contains("/podman/")
                    || line.contains("/machine.slice/")
                    || line.contains("/system.slice/docker-")
                {
                    return true;
                }
            }
        }

        // Check for cgroups v2 unified hierarchy
        if let Ok(cgroup_content) = fs::read_to_string("/proc/self/mountinfo") {
            for line in cgroup_content.lines() {
                if line.contains("/sys/fs/cgroup") && line.contains("cgroup2") {
                    // In cgroups v2, check if we're in a non-root cgroup
                    if let Ok(cgroup_path) = fs::read_to_string("/proc/self/cgroup") {
                        // In cgroups v2, format is "0::/path"
                        if let Some(line) = cgroup_path.lines().find(|l| l.starts_with("0::"))
                            && let Some(path) = line.strip_prefix("0::")
                        {
                            // If path is not "/" we're in a container
                            if path != "/" && !path.is_empty() {
                                return true;
                            }
                        }
                    }
                }
            }
        }

        false
    }

    #[cfg(target_os = "linux")]
    fn init_cgroup() -> Option<Cgroup> {
        // Try to get the cgroup of the current process
        let h = hierarchies::auto();
        let cgroup = Cgroup::load(h, "self");

        // Check if the cgroup actually exists before returning
        if cgroup.exists() { Some(cgroup) } else { None }
    }

    #[cfg(target_os = "linux")]
    fn get_cpu_limits(cgroup: &Option<Cgroup>) -> (Option<i64>, Option<u64>, Option<u64>) {
        let mut cpu_quota = None;
        let mut cpu_period = None;
        let mut cpu_shares = None;

        if let Some(cg) = cgroup {
            // Try to get CPU controller
            if let Some(cpu_controller) = cg.controller_of::<CpuController>() {
                if let Ok(quota) = cpu_controller.cfs_quota() {
                    cpu_quota = Some(quota);
                }
                if let Ok(period) = cpu_controller.cfs_period() {
                    cpu_period = Some(period);
                }
                if let Ok(shares) = cpu_controller.shares() {
                    cpu_shares = Some(shares);
                }
            }
        }

        // Fallback to filesystem if cgroups-rs doesn't work
        if cpu_quota.is_none() || cpu_period.is_none() {
            let (fs_quota, fs_period, fs_shares) = Self::get_cpu_limits_from_fs();
            cpu_quota = cpu_quota.or(fs_quota);
            cpu_period = cpu_period.or(fs_period);
            cpu_shares = cpu_shares.or(fs_shares);
        }

        (cpu_quota, cpu_period, cpu_shares)
    }

    fn get_cpu_limits_from_fs() -> (Option<i64>, Option<u64>, Option<u64>) {
        let mut cpu_quota = None;
        let mut cpu_period = None;
        let mut cpu_shares = None;

        // Try cgroups v2 first
        if let Ok(content) = fs::read_to_string("/sys/fs/cgroup/cpu.max") {
            let parts: Vec<&str> = content.split_whitespace().collect();
            if parts.len() == 2 {
                if parts[0] != "max" {
                    cpu_quota = parts[0].parse::<i64>().ok();
                }
                cpu_period = parts[1].parse::<u64>().ok();
            }
        }

        // Try cgroups v1
        if cpu_quota.is_none()
            && let Ok(content) = fs::read_to_string("/sys/fs/cgroup/cpu/cpu.cfs_quota_us")
        {
            cpu_quota = content.trim().parse::<i64>().ok();
        }

        if cpu_period.is_none()
            && let Ok(content) = fs::read_to_string("/sys/fs/cgroup/cpu/cpu.cfs_period_us")
        {
            cpu_period = content.trim().parse::<u64>().ok();
        }

        // Get CPU shares
        if let Ok(content) = fs::read_to_string("/sys/fs/cgroup/cpu.weight") {
            // cgroups v2 uses weight (1-10000), convert to shares
            if let Ok(weight) = content.trim().parse::<u64>() {
                cpu_shares = Some((weight * 1024) / 100);
            }
        } else if let Ok(content) = fs::read_to_string("/sys/fs/cgroup/cpu/cpu.shares") {
            // cgroups v1 uses shares directly
            cpu_shares = content.trim().parse::<u64>().ok();
        }

        (cpu_quota, cpu_period, cpu_shares)
    }

    #[cfg(target_os = "linux")]
    fn get_cpuset_cpus(cgroup: &Option<Cgroup>) -> Option<Vec<u32>> {
        if let Some(cg) = cgroup
            && let Some(cpuset_controller) = cg.controller_of::<CpuSetController>()
        {
            let cpus_info = cpuset_controller.cpuset();
            let cpus = cpus_info.cpus;
            // Convert Vec<(u64, u64)> to a string representation
            let mut ranges = Vec::new();
            for (start, end) in cpus {
                if start == end {
                    ranges.push(start.to_string());
                } else {
                    ranges.push(format!("{start}-{end}"));
                }
            }
            let cpus_str = ranges.join(",");
            return Self::parse_cpuset_range(&cpus_str);
        }

        // Fallback to filesystem
        Self::get_cpuset_cpus_from_fs()
    }

    fn get_cpuset_cpus_from_fs() -> Option<Vec<u32>> {
        // Try cgroups v2 first
        let cpuset_path = if Path::new("/sys/fs/cgroup/cpuset.cpus.effective").exists() {
            "/sys/fs/cgroup/cpuset.cpus.effective"
        } else if Path::new("/sys/fs/cgroup/cpuset.cpus").exists() {
            "/sys/fs/cgroup/cpuset.cpus"
        } else if Path::new("/sys/fs/cgroup/cpuset/cpuset.cpus").exists() {
            // cgroups v1
            "/sys/fs/cgroup/cpuset/cpuset.cpus"
        } else {
            return None;
        };

        if let Ok(content) = fs::read_to_string(cpuset_path) {
            Self::parse_cpuset_range(content.trim())
        } else {
            None
        }
    }

    fn parse_cpuset_range(cpuset_str: &str) -> Option<Vec<u32>> {
        let mut cpus = Vec::new();

        for part in cpuset_str.split(',') {
            let part = part.trim();
            if part.contains('-') {
                // Range like "0-3"
                let range_parts: Vec<&str> = part.split('-').collect();
                if range_parts.len() == 2
                    && let (Ok(start), Ok(end)) =
                        (range_parts[0].parse::<u32>(), range_parts[1].parse::<u32>())
                {
                    for cpu in start..=end {
                        cpus.push(cpu);
                    }
                }
            } else {
                // Single CPU like "0"
                if let Ok(cpu) = part.parse::<u32>() {
                    cpus.push(cpu);
                }
            }
        }

        if cpus.is_empty() { None } else { Some(cpus) }
    }

    fn calculate_effective_cpus(
        cpu_quota: Option<i64>,
        cpu_period: Option<u64>,
        cpu_shares: Option<u64>,
        cpuset_cpus: &Option<Vec<u32>>,
    ) -> f64 {
        let total_cpus = num_cpus::get() as f64;

        // Start with cpuset limit
        let cpuset_limit = if let Some(cpus) = cpuset_cpus {
            cpus.len() as f64
        } else {
            total_cpus
        };

        // Calculate quota-based limit
        let quota_limit = if let (Some(quota), Some(period)) = (cpu_quota, cpu_period) {
            if quota > 0 && period > 0 {
                (quota as f64) / (period as f64)
            } else {
                cpuset_limit
            }
        } else {
            cpuset_limit
        };

        // Calculate shares-based limit (rough approximation)
        let shares_limit = if let Some(shares) = cpu_shares {
            // Default shares is 1024, so we scale based on that
            let share_ratio = (shares as f64) / 1024.0;
            (share_ratio * total_cpus).min(cpuset_limit)
        } else {
            cpuset_limit
        };

        // Return the most restrictive limit
        quota_limit.min(shares_limit).min(cpuset_limit)
    }

    #[allow(dead_code)]
    pub fn get_cpu_usage_from_cgroup(&self) -> Option<f64> {
        #[cfg(target_os = "linux")]
        if let Some(ref _cg) = self.cgroup {
            // CPU usage tracking is done via /proc/stat, not directly from cgroups
            // cgroups provides limits, not usage metrics
        }

        // Try filesystem approach
        // cgroups v2
        if let Ok(content) = fs::read_to_string("/sys/fs/cgroup/cpu.stat") {
            let mut _usage_usec = 0u64;
            for line in content.lines() {
                if line.starts_with("usage_usec")
                    && let Some(value) = line.split_whitespace().nth(1)
                {
                    _usage_usec = value.parse().unwrap_or(0);
                    break;
                }
            }

            // This would need to be calculated as a delta over time
            // For now, return None to fall back to /proc/stat
            return None;
        }

        // cgroups v1
        if let Ok(_content) = fs::read_to_string("/sys/fs/cgroup/cpuacct/cpuacct.usage") {
            // This is cumulative nanoseconds, would need delta calculation
            return None;
        }

        None
    }

    #[cfg(target_os = "linux")]
    fn get_memory_limits(
        cgroup: &Option<Cgroup>,
    ) -> (Option<u64>, Option<u64>, Option<u64>, Option<u64>) {
        let mut memory_limit = None;
        let mut memory_soft_limit = None;
        let mut memory_swap_limit = None;
        let mut memory_usage = None;

        if let Some(cg) = cgroup
            && let Some(mem_controller) = cg.controller_of::<MemController>()
        {
            let mem_stat = mem_controller.memory_stat();
            // cgroups-rs returns i64, but we need u64
            if mem_stat.limit_in_bytes > 0 && mem_stat.limit_in_bytes < i64::MAX {
                memory_limit = Some(mem_stat.limit_in_bytes as u64);
            }

            memory_usage = Some(mem_stat.usage_in_bytes);

            if mem_stat.soft_limit_in_bytes > 0 && mem_stat.soft_limit_in_bytes < i64::MAX {
                memory_soft_limit = Some(mem_stat.soft_limit_in_bytes as u64);
            }

            let swap_stat = mem_controller.memswap();
            if swap_stat.limit_in_bytes > 0 && swap_stat.limit_in_bytes < i64::MAX {
                memory_swap_limit = Some(swap_stat.limit_in_bytes as u64);
            }
        }

        // Fallback to filesystem if cgroups-rs doesn't work
        if memory_limit.is_none() || memory_usage.is_none() {
            let (fs_limit, fs_soft_limit, fs_swap_limit, fs_usage) =
                Self::get_memory_limits_from_fs();
            memory_limit = memory_limit.or(fs_limit);
            memory_soft_limit = memory_soft_limit.or(fs_soft_limit);
            memory_swap_limit = memory_swap_limit.or(fs_swap_limit);
            memory_usage = memory_usage.or(fs_usage);
        }

        (
            memory_limit,
            memory_soft_limit,
            memory_swap_limit,
            memory_usage,
        )
    }

    fn get_memory_limits_from_fs() -> (Option<u64>, Option<u64>, Option<u64>, Option<u64>) {
        let mut memory_limit = None;
        let mut memory_soft_limit = None;
        let mut memory_swap_limit = None;
        let mut memory_usage = None;

        // Try cgroups v2 first
        if let Ok(content) = fs::read_to_string("/sys/fs/cgroup/memory.max") {
            let trimmed = content.trim();
            if trimmed != "max" {
                memory_limit = trimmed.parse::<u64>().ok();
            }
        }

        if let Ok(content) = fs::read_to_string("/sys/fs/cgroup/memory.high") {
            let trimmed = content.trim();
            if trimmed != "max" {
                memory_soft_limit = trimmed.parse::<u64>().ok();
            }
        }

        if let Ok(content) = fs::read_to_string("/sys/fs/cgroup/memory.swap.max") {
            let trimmed = content.trim();
            if trimmed != "max" {
                memory_swap_limit = trimmed.parse::<u64>().ok();
            }
        }

        if let Ok(content) = fs::read_to_string("/sys/fs/cgroup/memory.current") {
            memory_usage = content.trim().parse::<u64>().ok();
        }

        // Try cgroups v1 if v2 didn't work
        if memory_limit.is_none()
            && let Ok(content) = fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes")
        {
            let limit = content.trim().parse::<u64>().unwrap_or(0);
            // cgroups v1 uses a very large number to indicate no limit
            if limit < 9223372036854771712 {
                // Less than 8 EiB
                memory_limit = Some(limit);
            }
        }

        if memory_soft_limit.is_none()
            && let Ok(content) =
                fs::read_to_string("/sys/fs/cgroup/memory/memory.soft_limit_in_bytes")
        {
            let limit = content.trim().parse::<u64>().unwrap_or(0);
            if limit < 9223372036854771712 {
                memory_soft_limit = Some(limit);
            }
        }

        if memory_swap_limit.is_none()
            && let Ok(content) =
                fs::read_to_string("/sys/fs/cgroup/memory/memory.memsw.limit_in_bytes")
        {
            let limit = content.trim().parse::<u64>().unwrap_or(0);
            if limit < 9223372036854771712 {
                memory_swap_limit = Some(limit);
            }
        }

        if memory_usage.is_none()
            && let Ok(content) = fs::read_to_string("/sys/fs/cgroup/memory/memory.usage_in_bytes")
        {
            memory_usage = content.trim().parse::<u64>().ok();
        }

        (
            memory_limit,
            memory_soft_limit,
            memory_swap_limit,
            memory_usage,
        )
    }

    pub fn get_memory_stats(&self) -> Option<(u64, u64, f64)> {
        // Returns (total, used, utilization_percentage)
        if !self.is_container {
            return None;
        }

        let total = self.memory_limit_bytes?;

        // Always get fresh memory usage, don't rely on cached value
        let used = self.get_current_memory_usage().unwrap_or(0);

        let utilization = if total > 0 {
            (used as f64 / total as f64) * 100.0
        } else {
            0.0
        };

        Some((total, used, utilization))
    }

    pub fn get_current_memory_usage(&self) -> Option<u64> {
        #[cfg(target_os = "linux")]
        {
            // Direct filesystem read is most efficient
            // Try cgroups v2 first
            if let Ok(content) = fs::read_to_string("/sys/fs/cgroup/memory.current")
                && let Ok(usage) = content.trim().parse::<u64>()
            {
                return Some(usage);
            }

            // Try cgroups v1
            if let Ok(content) = fs::read_to_string("/sys/fs/cgroup/memory/memory.usage_in_bytes")
                && let Ok(usage) = content.trim().parse::<u64>()
            {
                return Some(usage);
            }
        }

        None
    }

    #[allow(dead_code)]
    pub fn get_detailed_memory_stats(&self) -> Option<MemoryStats> {
        if !self.is_container {
            return None;
        }

        let mut stats = MemoryStats::default();

        #[cfg(target_os = "linux")]
        if let Some(ref cg) = self.cgroup
            && let Some(mem_controller) = cg.controller_of::<MemController>()
        {
            let mem_stat = mem_controller.memory_stat();
            // Map the MemoryStat fields to our MemoryStats
            stats.anon_bytes = mem_stat.stat.rss; // RSS is roughly anonymous memory
            stats.file_bytes = mem_stat.stat.cache; // Cache is file-backed memory
            stats.file_mapped_bytes = mem_stat.stat.mapped_file;
            stats.file_dirty_bytes = mem_stat.stat.dirty;
            stats.file_writeback_bytes = mem_stat.stat.writeback;
            // Note: kernel_stack, slab, and sock are not directly available in MemoryStat
            return Some(stats);
        }

        // Fallback to filesystem
        // Try cgroups v2 memory.stat
        if let Ok(content) = fs::read_to_string("/sys/fs/cgroup/memory.stat") {
            for line in content.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() == 2
                    && let Ok(value) = parts[1].parse::<u64>()
                {
                    match parts[0] {
                        "anon" => stats.anon_bytes = value,
                        "file" => stats.file_bytes = value,
                        "kernel_stack" => stats.kernel_stack_bytes = value,
                        "slab" => stats.slab_bytes = value,
                        "sock" => stats.sock_bytes = value,
                        "file_mapped" => stats.file_mapped_bytes = value,
                        "file_dirty" => stats.file_dirty_bytes = value,
                        "file_writeback" => stats.file_writeback_bytes = value,
                        _ => {}
                    }
                }
            }
            return Some(stats);
        }

        // Try cgroups v1
        if let Ok(content) = fs::read_to_string("/sys/fs/cgroup/memory/memory.stat") {
            for line in content.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() == 2
                    && let Ok(value) = parts[1].parse::<u64>()
                {
                    match parts[0] {
                        "rss" => stats.anon_bytes = value,
                        "cache" => stats.file_bytes = value,
                        "mapped_file" => stats.file_mapped_bytes = value,
                        "dirty" => stats.file_dirty_bytes = value,
                        "writeback" => stats.file_writeback_bytes = value,
                        _ => {}
                    }
                }
            }
            return Some(stats);
        }

        None
    }
}

// Helper to parse CPU stats considering container limits
pub fn parse_cpu_stat_with_container_limits(
    stat_content: &str,
    container_info: &ContainerInfo,
) -> (f64, Vec<u32>) {
    let mut overall_utilization = 0.0;
    let mut active_cores = Vec::new();

    // If we have a cpuset, only consider those CPUs
    let allowed_cpus = if let Some(cpuset) = &container_info.cpuset_cpus {
        cpuset.clone()
    } else if container_info.is_container {
        // In a container without cpuset, limit to effective CPU count
        let effective_cores = container_info.effective_cpu_count.ceil() as u32;
        (0..effective_cores.min(num_cpus::get() as u32)).collect()
    } else {
        // Not in container, consider all CPUs
        (0..num_cpus::get() as u32).collect()
    };

    let lines: Vec<&str> = stat_content.lines().collect();

    // Parse overall CPU stats
    if let Some(cpu_line) = lines.iter().find(|line| line.starts_with("cpu ")) {
        let fields: Vec<&str> = cpu_line.split_whitespace().collect();
        if fields.len() >= 8 {
            let user: u64 = fields[1].parse().unwrap_or(0);
            let nice: u64 = fields[2].parse().unwrap_or(0);
            let system: u64 = fields[3].parse().unwrap_or(0);
            let idle: u64 = fields[4].parse().unwrap_or(0);
            let iowait: u64 = fields[5].parse().unwrap_or(0);
            let irq: u64 = fields[6].parse().unwrap_or(0);
            let softirq: u64 = fields[7].parse().unwrap_or(0);

            let total_time = user + nice + system + idle + iowait + irq + softirq;
            let active_time = total_time - idle - iowait;

            if total_time > 0 {
                let raw_utilization = (active_time as f64 / total_time as f64) * 100.0;

                // Scale utilization based on container's effective CPU count
                if container_info.is_container {
                    let scale_factor =
                        container_info.effective_cpu_count / allowed_cpus.len() as f64;
                    overall_utilization = (raw_utilization * scale_factor).min(100.0);
                } else {
                    overall_utilization = raw_utilization;
                }
            }
        }
    }

    // Track which cores are active
    for line in lines.iter() {
        if line.starts_with("cpu")
            && !line.starts_with("cpu ")
            && let Some(cpu_num_str) = line.split_whitespace().next()
            && let Some(cpu_num_str) = cpu_num_str.strip_prefix("cpu")
            && let Ok(core_id) = cpu_num_str.parse::<u32>()
            && allowed_cpus.contains(&core_id)
        {
            active_cores.push(core_id);
        }
    }

    (overall_utilization, active_cores)
}

// Add dependency in the module
#[cfg(not(target_os = "linux"))]
pub struct ContainerInfo {
    pub is_container: bool,
    pub effective_cpu_count: f64,
    pub memory_limit_bytes: Option<u64>,
    pub memory_usage_bytes: Option<u64>,
}

#[cfg(not(target_os = "linux"))]
impl ContainerInfo {
    pub fn detect() -> Self {
        ContainerInfo {
            is_container: false,
            effective_cpu_count: 1.0,
            memory_limit_bytes: None,
            memory_usage_bytes: None,
        }
    }

    pub fn get_memory_stats(&self) -> Option<(u64, u64, f64)> {
        None
    }
}

#[cfg(test)]
#[path = "container_info/tests.rs"]
mod tests;
