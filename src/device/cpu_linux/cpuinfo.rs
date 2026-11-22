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

//! `/proc/cpuinfo` split into what is read once and what is sampled per tick
//! (issue #504).
//!
//! Model, architecture, socket and thread counts, cache size, the maximum
//! frequency and the base-frequency fallback chain never change while the
//! process runs, so [`StaticCpuInfo::load`] computes them once when the reader
//! is built. The only value that moves between ticks is the current
//! frequency, which [`FrequencySource`] reads from `scaling_cur_freq` where
//! cpufreq exists and from the `cpu MHz` lines of `/proc/cpuinfo` otherwise.

use std::collections::HashMap;
use std::ffi::CStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::device::CpuPlatformType;
use crate::device::container_info::ContainerInfo;
use crate::device::cpu_topology::parse_cpumask_list;
use crate::utils::command::new_command;
use crate::utils::{hz_to_mhz, khz_to_mhz};

use super::{MAX_TRACKED_CPU_ID, enumerate_cpu_ids};

pub(super) type CpuInfoParseResult = Result<
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

/// The kernel's machine string, exactly what `uname -m` prints (`x86_64`,
/// `aarch64`, `armv7l`, `ppc64le`, ...).
///
/// Read through `uname(2)` so no process is spawned. `std::env::consts::ARCH`
/// is only the fallback: it names the compile target (`arm`, `powerpc64`),
/// not the running kernel, so it would change `CpuInfo.architecture`.
pub(super) fn machine_string() -> String {
    // SAFETY: `utsname` is a plain C struct of byte arrays, for which the
    // all-zero bit pattern is valid.
    let mut uts: libc::utsname = unsafe { std::mem::zeroed() };
    // SAFETY: `uts` is a valid, writable `utsname`; `uname` fills it and
    // returns 0 on success.
    if unsafe { libc::uname(&mut uts) } == 0 {
        // `c_char` is `i8` or `u8` depending on the target; reinterpret as bytes.
        let bytes: Vec<u8> = uts
            .machine
            .iter()
            .map(|&c| u8::from_ne_bytes(c.to_ne_bytes()))
            .collect();
        if let Ok(machine) = CStr::from_bytes_until_nul(&bytes) {
            let machine = machine.to_string_lossy().trim().to_string();
            if !machine.is_empty() {
                return machine;
            }
        }
    }
    std::env::consts::ARCH.to_string()
}

/// `lscpu` output, run at most once per process. `lscpu` describes static
/// hardware, so neither a success nor a failure is worth repeating.
pub(super) fn lscpu_output() -> Option<&'static str> {
    static LSCPU: OnceLock<Option<String>> = OnceLock::new();
    LSCPU
        .get_or_init(|| {
            new_command("lscpu")
                .output()
                .ok()
                .and_then(|output| String::from_utf8(output.stdout).ok())
        })
        .as_deref()
}

/// Static CPU description, computed once per reader.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct StaticCpuInfo {
    pub cpu_model: String,
    pub architecture: String,
    pub platform_type: CpuPlatformType,
    pub socket_count: u32,
    pub total_cores: u32,
    pub total_threads: u32,
    /// The base frequency at construction. Used as the per-tick value when
    /// neither `scaling_cur_freq` nor `cpu MHz` yields one, which is where
    /// the static fallback chain (min frequency, lscpu, ARM clock rate,
    /// BogoMIPS, platform default) ends up.
    pub base_frequency_mhz: u32,
    pub max_frequency_mhz: u32,
    pub cache_size_mb: u32,
}

impl StaticCpuInfo {
    /// Parse `cpuinfo` and apply the container adjustments and the lscpu
    /// cache-size fallback that used to run on every tick.
    pub(super) fn load(
        cpuinfo: &str,
        machine: &str,
        container_info: &ContainerInfo,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (
            cpu_model,
            architecture,
            platform_type,
            mut socket_count,
            mut total_cores,
            mut total_threads,
            base_frequency_mhz,
            max_frequency_mhz,
            mut cache_size_mb,
        ) = parse_cpuinfo(cpuinfo, machine, container_info)?;

        if container_info.is_container {
            // If in a container, adjust the reported cores based on CPU quota
            let effective_cores = container_info.effective_cpu_count.ceil() as u32;
            if let Some(cpuset) = &container_info.cpuset_cpus {
                total_cores = cpuset.len() as u32;
                total_threads = total_cores; // Assume no hyperthreading for simplicity
            } else if effective_cores < total_cores {
                total_cores = effective_cores;
                total_threads = effective_cores;
            }
            // Container typically appears as single socket
            socket_count = 1;
        }

        if cache_size_mb == 0
            && let Some(lscpu_cache) = lscpu_output().and_then(cache_size_from_lscpu)
        {
            cache_size_mb = lscpu_cache;
        }

        Ok(Self {
            cpu_model,
            architecture,
            platform_type,
            socket_count,
            total_cores,
            total_threads,
            base_frequency_mhz,
            max_frequency_mhz,
            cache_size_mb,
        })
    }
}

/// Where the per-tick current frequency comes from.
///
/// Resolved once: the `scaling_cur_freq` files of the CPUs that count (the
/// container cpuset, or every online CPU) that exist. An empty list means the
/// host has no cpufreq (common in VMs) and the tick falls back to the `cpu MHz`
/// lines of `/proc/cpuinfo`.
#[derive(Debug, Clone, Default)]
pub(super) struct FrequencySource {
    paths: Vec<PathBuf>,
}

impl FrequencySource {
    /// `root` is `/sys` in production and a fixture directory in tests.
    pub(super) fn resolve(root: &Path, cpuset: Option<&[u32]>) -> Self {
        let cpu_dir = root.join("devices/system/cpu");
        let mut cpu_ids = match cpuset {
            Some(cpuset) => cpuset.to_vec(),
            None => {
                let online = fs::read_to_string(cpu_dir.join("online"))
                    .map(|mask| parse_cpumask_list(&mask))
                    .unwrap_or_default();
                if online.is_empty() {
                    enumerate_cpu_ids(root)
                } else {
                    online
                }
            }
        };
        cpu_ids.retain(|id| *id <= MAX_TRACKED_CPU_ID);
        cpu_ids.sort_unstable();
        cpu_ids.dedup();

        let paths = cpu_ids
            .into_iter()
            .map(|id| cpu_dir.join(format!("cpu{id}/cpufreq/scaling_cur_freq")))
            .filter(|path| path.exists())
            .collect();
        Self { paths }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Average of the readable `scaling_cur_freq` values, in MHz.
    pub(super) fn sample(&self) -> Option<u32> {
        let mut sum_khz = 0u64;
        let mut count = 0u64;
        for path in &self.paths {
            if let Ok(content) = fs::read_to_string(path)
                && let Ok(khz) = content.trim().parse::<u64>()
                && khz > 0
            {
                sum_khz += khz;
                count += 1;
            }
        }
        if count == 0 {
            return None;
        }
        let avg_khz = u32::try_from(sum_khz / count).unwrap_or(u32::MAX);
        Some(khz_to_mhz(avg_khz)).filter(|mhz| *mhz > 0)
    }
}

/// Average of the `cpu MHz` lines, cpuset-filtered when `cpuset` is given.
///
/// This is the dynamic part of [`parse_cpuinfo`], factored out so the tick
/// can parse only these lines and produce the same value the full parser
/// does.
pub(super) fn parse_cpu_mhz(content: &str, cpuset: Option<&[u32]>) -> Option<u32> {
    let mut cpu_mhz_values = Vec::new();
    let mut cpu_mhz_by_processor: HashMap<u32, f64> = HashMap::new();
    let mut current_processor_id = None;

    for line in content.lines() {
        if line.starts_with("processor") {
            if let Some(value) = line.split(':').nth(1)
                && let Ok(id) = value.trim().parse::<u32>()
            {
                current_processor_id = Some(id);
            }
        } else if line.starts_with("cpu MHz")
            && let Some(value) = line.split(':').nth(1)
            && let Ok(freq) = value.trim().parse::<f64>()
            && freq > 0.0
        {
            cpu_mhz_values.push(freq);
            if let Some(proc_id) = current_processor_id {
                cpu_mhz_by_processor.insert(proc_id, freq);
            }
        }
    }

    let selected: Vec<f64> = match cpuset {
        // Container mode: only average frequencies from assigned CPUs
        Some(cpuset) => cpuset
            .iter()
            .filter_map(|cpu_id| cpu_mhz_by_processor.get(cpu_id).copied())
            .collect(),
        None => cpu_mhz_values,
    };
    if selected.is_empty() {
        return None;
    }
    let avg = selected.iter().sum::<f64>() / selected.len() as f64;
    Some(avg as u32)
}

/// Build an ARM model name from `CPU implementer` / `CPU part` when
/// `model name` is absent.
fn synthesize_arm_model(cpu_implementer: &str, cpu_part: &str) -> String {
    if cpu_implementer.is_empty() && cpu_part.is_empty() {
        return "ARM Processor".to_string();
    }
    let implementer_name = match cpu_implementer {
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
    let mut model = format!("{implementer_name} ARM Processor");
    if !cpu_part.is_empty() {
        model.push_str(&format!(" (Part: {cpu_part})"));
    }
    model
}

/// First readable kHz value among `paths`, in MHz.
fn first_khz_as_mhz(paths: &[String]) -> Option<u32> {
    paths.iter().find_map(|path| {
        fs::read_to_string(path)
            .ok()
            .and_then(|content| content.trim().parse::<u32>().ok())
            .map(khz_to_mhz)
    })
}

/// Parse a full `/proc/cpuinfo`.
///
/// `machine` is the cached [`machine_string`]; it fills `architecture` and
/// decides whether an ARM model name is synthesized.
pub(super) fn parse_cpuinfo(
    content: &str,
    machine: &str,
    container_info: &ContainerInfo,
) -> CpuInfoParseResult {
    let mut cpu_model = String::new();
    let mut platform_type = CpuPlatformType::Other("Unknown".to_string());
    let mut cache_size = 0u32;
    let mut bogomips = 0f64;
    let mut physical_ids = std::collections::HashSet::new();
    let mut processor_count = 0u32;
    let mut cpu_implementer = String::new();
    let mut cpu_part = String::new();

    for line in content.lines() {
        if line.starts_with("model name") {
            if let Some(value) = line.split(':').nth(1) {
                cpu_model = value.trim().to_string();
                let lower = cpu_model.to_lowercase();
                if lower.contains("intel") {
                    platform_type = CpuPlatformType::Intel;
                } else if lower.contains("amd") {
                    platform_type = CpuPlatformType::Amd;
                } else if lower.contains("arm") {
                    platform_type = CpuPlatformType::Arm;
                }
            }
        } else if line.starts_with("processor") {
            processor_count += 1;
        } else if line.starts_with("physical id") {
            if let Some(value) = line.split(':').nth(1)
                && let Ok(id) = value.trim().parse::<u32>()
            {
                physical_ids.insert(id);
            }
        } else if line.starts_with("cache size") && cache_size == 0 {
            if let Some(size_str) = line
                .split(':')
                .nth(1)
                .and_then(|value| value.split_whitespace().next())
                && let Ok(size) = size_str.parse::<u32>()
            {
                cache_size = size / 1024; // Convert KB to MB
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
    // Default assumption, may be incorrect with hyperthreading
    let total_cores = total_threads;

    let architecture = machine.to_string();
    let is_arm =
        architecture == "aarch64" || architecture == "arm64" || architecture.starts_with("arm");
    if is_arm && cpu_model.is_empty() {
        platform_type = CpuPlatformType::Arm;
        cpu_model = synthesize_arm_model(&cpu_implementer, &cpu_part);
    }

    // Average of the `cpu MHz` lines; in a container only the cpuset's CPUs.
    let mut base_frequency =
        parse_cpu_mhz(content, container_info.cpuset_cpus.as_deref()).unwrap_or(0);

    // cpufreq files of the first CPU that counts.
    let cpu = container_info
        .cpuset_cpus
        .as_ref()
        .and_then(|cpuset| cpuset.first().copied())
        .unwrap_or(0);
    let cpufreq = |name: &str| -> Vec<String> {
        vec![
            format!("/sys/devices/system/cpu/cpu{cpu}/cpufreq/cpuinfo_{name}"),
            format!("/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_{name}"),
            format!("/sys/devices/system/cpu/cpufreq/policy0/cpuinfo_{name}"),
            format!("/sys/devices/system/cpu/cpufreq/policy0/scaling_{name}"),
        ]
    };

    let mut max_frequency = first_khz_as_mhz(&cpufreq("max_freq")).unwrap_or(0);

    if base_frequency == 0 {
        // `scaling_cur_freq` is listed before `cpuinfo_cur_freq` here, as it
        // always was.
        let cur = cpufreq("cur_freq");
        let ordered = [
            cur[1].clone(),
            cur[0].clone(),
            cur[3].clone(),
            cur[2].clone(),
        ];
        base_frequency = first_khz_as_mhz(&ordered).unwrap_or(0);
    }
    if base_frequency == 0 {
        base_frequency = first_khz_as_mhz(&cpufreq("min_freq")).unwrap_or(0);
    }
    if max_frequency == 0 {
        max_frequency = base_frequency;
    }

    // If we still don't have frequencies, try lscpu as fallback
    if base_frequency == 0
        && max_frequency == 0
        && let Some(lscpu) = lscpu_output()
    {
        for line in lscpu.lines() {
            let value = || {
                line.split(':')
                    .nth(1)
                    .and_then(|value| value.trim().parse::<f64>().ok())
            };
            if line.starts_with("CPU MHz:") {
                if let Some(freq) = value() {
                    base_frequency = freq as u32;
                    break;
                }
            } else if line.starts_with("CPU max MHz:") {
                if let Some(freq) = value() {
                    max_frequency = freq as u32;
                }
            } else if line.starts_with("CPU min MHz:")
                && base_frequency == 0
                && let Some(freq) = value()
            {
                base_frequency = freq as u32;
            }
        }
    }

    // ARM clock rate from sysfs, then the device tree
    if base_frequency == 0 && platform_type == CpuPlatformType::Arm {
        if let Ok(content) = fs::read_to_string("/sys/devices/system/cpu/cpu0/clock_rate")
            && let Ok(freq_hz) = content.trim().parse::<u64>()
        {
            base_frequency = hz_to_mhz(freq_hz);
        }
        if base_frequency == 0
            && let Ok(content) = fs::read_to_string("/proc/device-tree/cpus/cpu@0/clock-frequency")
            && content.len() >= 4
        {
            // Device tree values are big-endian
            let bytes = content.as_bytes();
            let freq_hz = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as u64;
            if freq_hz > 0 {
                base_frequency = hz_to_mhz(freq_hz);
            }
        }
    }

    // BogoMIPS as last resort: close to the clock on ARM, about 2x on x86
    if base_frequency == 0 && bogomips > 0.0 {
        let estimated = match platform_type {
            CpuPlatformType::Arm => bogomips as u32,
            _ => (bogomips / 2.0) as u32,
        };
        if estimated > 0 {
            base_frequency = estimated;
        }
    }

    // Final fallback - architecture-specific defaults
    if base_frequency == 0 && max_frequency == 0 {
        base_frequency = match platform_type {
            CpuPlatformType::Arm => 2000,
            _ => 1000,
        };
        max_frequency = base_frequency;
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

/// Cache size from `lscpu` output: L3 preferred, L2 as fallback, in MB.
pub(super) fn cache_size_from_lscpu(output: &str) -> Option<u32> {
    let mut found_l3 = None;
    let mut found_l2 = None;
    for line in output.lines() {
        // On some systems like Jetson the lines are indented.
        let line = line.trim();
        if line.starts_with("L3:") || line.starts_with("L3 cache:") {
            if let Some(size) = line.split(':').nth(1).and_then(parse_lscpu_size_mb) {
                found_l3 = Some(size);
            }
        } else if (line.starts_with("L2:") || line.starts_with("L2 cache:"))
            && found_l3.is_none()
            && let Some(size) = line.split(':').nth(1).and_then(parse_lscpu_size_mb)
        {
            found_l2 = Some(size);
        }
    }
    found_l3.or(found_l2)
}

/// Parse `"4 MiB"`, `"4MiB"`, `"4096 KiB"`, `"4 MiB (2 instances)"` into MB.
fn parse_lscpu_size_mb(size_part: &str) -> Option<u32> {
    let parts: Vec<&str> = size_part.split_whitespace().collect();
    let first = parts.first()?;
    let num_end = first
        .find(|c: char| !c.is_numeric() && c != '.')
        .unwrap_or(first.len());
    let size = first[..num_end].parse::<f64>().ok()?;
    let unit = if num_end == first.len() {
        parts
            .get(1)
            .map(|unit| unit.to_lowercase())
            .unwrap_or_default()
    } else {
        first[num_end..].to_lowercase()
    };
    let size_mb = match unit.as_str() {
        "mib" | "mb" => size as u32,
        "kib" | "kb" => (size / 1024.0) as u32,
        "gib" | "gb" => (size * 1024.0) as u32,
        _ => 0,
    };
    Some(size_mb).filter(|size| *size > 0)
}

#[cfg(test)]
#[path = "cpuinfo_tests.rs"]
mod tests;
