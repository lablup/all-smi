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
use crate::device::common::command_executor::CommandOutput;
use crate::device::common::{execute_command_default, parse_csv_line};
use crate::device::process_list::{get_all_processes, merge_gpu_processes};
use crate::device::readers::common_cache::{DetailBuilder, DeviceStaticInfo, RetryGate};
use crate::device::readers::nvidia::read_meminfo_memory;
use crate::device::types::{GpuInfo, ProcessInfo};
use crate::utils::{get_hostname, hz_to_mhz, millicelsius_to_celsius, with_global_system};
use chrono::Local;
use std::collections::HashSet;
use std::fs;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// System memory source. On Jetson the GPU shares system RAM, so the
/// device's total and used memory are `MemTotal` and
/// `MemTotal - MemAvailable`.
const MEMINFO_PATH: &str = "/proc/meminfo";

/// How long `nvidia-smi --query-compute-apps` is skipped after it failed.
/// JetPack releases without `nvidia-smi`, or with one that rejects the
/// query, fall back to name matching for this long before trying again.
const COMPUTE_APPS_RETRY_INTERVAL: Duration = Duration::from_secs(60);

/// Runs the `nvidia-smi` compute-apps query. `None` is a spawn failure or
/// a timeout.
type ComputeAppsRunner = Box<dyn Fn() -> Option<CommandOutput> + Send + Sync>;

pub struct NvidiaJetsonGpuReader {
    /// Cached static device information (fetched only once)
    static_info: OnceLock<DeviceStaticInfo>,
    /// Negative cache for the compute-apps query.
    compute_apps_gate: RetryGate,
    /// The compute-apps query; replaced by a counting fake in tests.
    compute_apps_runner: ComputeAppsRunner,
}

impl Default for NvidiaJetsonGpuReader {
    fn default() -> Self {
        Self::new()
    }
}

impl NvidiaJetsonGpuReader {
    pub fn new() -> Self {
        Self::with_compute_apps_runner(Box::new(run_compute_apps_query))
    }

    fn with_compute_apps_runner(compute_apps_runner: ComputeAppsRunner) -> Self {
        Self {
            static_info: OnceLock::new(),
            compute_apps_gate: RetryGate::new(),
            compute_apps_runner,
        }
    }

    /// Stdout of the compute-apps query, or `None` when it is backed off or
    /// failed. A spawn error, a timeout or a non-zero status starts the
    /// back-off; the caller then uses the name-matching fallback.
    fn compute_apps_stdout(&self, now: Instant) -> Option<String> {
        if self.compute_apps_gate.is_blocked(now) {
            return None;
        }
        match (self.compute_apps_runner)() {
            Some(output) if output.status == 0 => {
                self.compute_apps_gate.clear();
                Some(output.stdout)
            }
            _ => {
                self.compute_apps_gate
                    .fail(now, COMPUTE_APPS_RETRY_INTERVAL);
                None
            }
        }
    }

    /// Get cached static device info, initializing if needed
    fn get_static_info(&self) -> &DeviceStaticInfo {
        self.static_info.get_or_init(|| {
            // Get device name
            let name = fs::read_to_string("/proc/device-tree/model")
                .unwrap_or_else(|_| "NVIDIA Jetson".to_string())
                .trim_end_matches('\0')
                .to_string();

            let mut builder = DetailBuilder::new();

            // Try to get CUDA version from nvidia-smi if available
            if let Ok(output) = execute_command_default("nvidia-smi", &[])
                && output.status == 0
            {
                // Parse CUDA version from header
                for line in output.stdout.lines() {
                    if line.contains("CUDA Version:") {
                        if let Some(version_part) = line.split("CUDA Version:").nth(1) {
                            let version = version_part
                                .split_whitespace()
                                .next()
                                .unwrap_or("Unknown")
                                .to_string();
                            builder = builder
                                .insert("CUDA Version", &version)
                                // Add unified AI acceleration library labels
                                .insert("lib_name", "CUDA")
                                .insert("lib_version", version);
                        }
                        break;
                    }
                }
            }

            // Get JetPack version if available
            let jetpack_version =
                fs::read_to_string("/etc/nv_jetpack_release")
                    .ok()
                    .and_then(|jetpack| {
                        jetpack
                            .lines()
                            .find(|line| line.starts_with("JETPACK_VERSION"))
                            .and_then(|line| line.split('=').nth(1))
                            .map(|v| v.trim().to_string())
                    });
            builder = builder.insert_optional("JetPack Version", jetpack_version);

            // Get L4T version
            let mut detail = builder.build();
            if let Ok(l4t) = fs::read_to_string("/etc/nv_tegra_release")
                && let Some(version) = l4t.split_whitespace().nth(1)
            {
                detail.insert("L4T Version".to_string(), version.to_string());
            }

            // Static hardware info
            detail.insert("GPU Type".to_string(), "Integrated".to_string());
            detail.insert("architecture".to_string(), "Tegra".to_string());

            DeviceStaticInfo::with_details(name, None, detail)
        })
    }
}

impl GpuReader for NvidiaJetsonGpuReader {
    fn get_gpu_info(&self) -> Vec<GpuInfo> {
        let mut gpu_info = Vec::new();

        // Get cached static info
        let static_info = self.get_static_info();

        // Read dynamic metrics only
        let utilization = fs::read_to_string("/sys/devices/platform/tegra-soc/gpu.0/load")
            .map_or(0.0, |s| s.trim().parse::<f64>().unwrap_or(0.0) / 10.0);

        let frequency = fs::read_to_string("/sys/devices/platform/tegra-soc/gpu.0/cur_freq")
            .map_or(0, |s| s.trim().parse::<u64>().map(hz_to_mhz).unwrap_or(0));

        let temperature = fs::read_to_string("/sys/devices/virtual/thermal/thermal_zone0/temp")
            .map_or(0, |s| {
                s.trim()
                    .parse::<u32>()
                    .map(millicelsius_to_celsius)
                    .unwrap_or(0)
            });

        let power_consumption =
            fs::read_to_string("/sys/bus/i2c/drivers/ina3221x/0-0040/iio:device0/in_power0_input")
                .map_or(0.0, |s| s.trim().parse::<f64>().unwrap_or(0.0) / 1000.0);

        let dla0_util = fs::read_to_string("/sys/kernel/debug/dla_0/load")
            .map_or(0.0, |s| s.trim().parse::<f64>().unwrap_or(0.0));
        let dla1_util = fs::read_to_string("/sys/kernel/debug/dla_1/load")
            .map_or(0.0, |s| s.trim().parse::<f64>().unwrap_or(0.0));
        let dla_utilization = if dla0_util > 0.0 || dla1_util > 0.0 {
            Some(dla0_util + dla1_util)
        } else {
            None
        };

        let (total_memory, used_memory) = get_memory_info(MEMINFO_PATH);

        let info = GpuInfo {
            uuid: "JetsonGPU".to_string(),
            time: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            name: static_info.name.clone(),
            device_type: "GPU".to_string(),
            host_id: get_hostname(), // For local mode, host_id is just the hostname
            hostname: get_hostname(), // DNS hostname
            instance: get_hostname(),
            utilization,
            ane_utilization: 0.0,
            dla_utilization,
            tensorcore_utilization: None,
            temperature,
            used_memory,
            total_memory,
            frequency,
            power_consumption,
            gpu_core_count: None,
            // Jetson uses `/sys/class/thermal` readings rather than NVML
            // thermal-threshold APIs, so the extended thresholds, P-state,
            // and NVIDIA hardware details (NUMA/GSP/NvLink/GPM) are not
            // available on this platform.
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
            detail: static_info.detail.clone(),
        };

        gpu_info.push(info);
        gpu_info
    }

    fn get_process_info(&self) -> Vec<ProcessInfo> {
        use sysinfo::ProcessesToUpdate;

        // Get GPU processes and PIDs
        let (gpu_processes, gpu_pids) = get_gpu_processes(self.compute_apps_stdout(Instant::now()));

        // Use global system instance to avoid file descriptor leak
        let all_processes = with_global_system(|system| {
            system.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                crate::device::process_list::process_row_refresh_kind(),
            );
            system.refresh_memory();

            // Get all system processes
            get_all_processes(system, &gpu_pids)
        });

        // Merge GPU information into the process list while preserving per-device rows.
        merge_gpu_processes(all_processes, gpu_processes)
    }

    fn get_gpu_processes(&self) -> (Vec<ProcessInfo>, HashSet<u32>) {
        get_gpu_processes(self.compute_apps_stdout(Instant::now()))
    }

    fn includes_system_processes(&self) -> bool {
        true
    }
}

/// `nvidia-smi --query-compute-apps`, available on newer Jetson models only.
fn run_compute_apps_query() -> Option<CommandOutput> {
    execute_command_default(
        "nvidia-smi",
        &[
            "--query-compute-apps=pid,used_memory",
            "--format=csv,noheader,nounits",
        ],
    )
    .ok()
}

/// Get GPU processes for Jetson.
///
/// `compute_apps_stdout` is the output of the `nvidia-smi` compute-apps
/// query when it ran and succeeded this tick (see
/// `NvidiaJetsonGpuReader::compute_apps_stdout`).
fn get_gpu_processes(compute_apps_stdout: Option<String>) -> (Vec<ProcessInfo>, HashSet<u32>) {
    let mut gpu_processes = Vec::new();
    let mut gpu_pids = HashSet::new();

    // Jetson doesn't have a direct way to query GPU processes
    // We can try nvidia-smi if available (on newer Jetson models)
    if let Some(stdout) = compute_apps_stdout {
        for line in stdout.lines() {
            let parts = parse_csv_line(line);
            if parts.len() >= 2
                && let Ok(pid) = parts[0].parse::<u32>()
                && let Ok(used_memory_mb) = parts[1].parse::<u64>()
            {
                gpu_pids.insert(pid);

                gpu_processes.push(ProcessInfo {
                    device_id: 0,
                    device_uuid: "JetsonGPU".to_string(),
                    pid,
                    process_name: String::new(), // Will be filled by sysinfo
                    used_memory: used_memory_mb * 1024 * 1024, // Convert MB to bytes
                    cpu_percent: 0.0,            // Will be filled by sysinfo
                    memory_percent: 0.0,         // Will be filled by sysinfo
                    memory_rss: 0,               // Will be filled by sysinfo
                    memory_vms: 0,               // Will be filled by sysinfo
                    user: String::new(),         // Will be filled by sysinfo
                    state: String::new(),        // Will be filled by sysinfo
                    start_time: String::new(),   // Will be filled by sysinfo
                    cpu_time: 0,                 // Will be filled by sysinfo
                    command: String::new(),      // Will be filled by sysinfo
                    ppid: 0,                     // Will be filled by sysinfo
                    threads: 0,                  // Will be filled by sysinfo
                    uses_gpu: true,
                    priority: 0,          // Will be filled by sysinfo
                    nice_value: 0,        // Will be filled by sysinfo
                    gpu_utilization: 0.0, // nvidia-smi on Jetson doesn't provide per-process GPU utilization
                });
            }
        }
    }

    // If nvidia-smi is not available or doesn't return processes,
    // we can look for known GPU-using processes by name
    if gpu_processes.is_empty() {
        // Look for common GPU applications on Jetson
        let gpu_process_names = vec![
            "nvargus-daemon",
            "nvgstcapture",
            "deepstream",
            "tensorrt",
            "cuda",
        ];

        with_global_system(|system| {
            system.refresh_memory();
            for (pid, process) in system.processes() {
                let process_name = process.name().to_string_lossy().to_lowercase();
                for gpu_name in &gpu_process_names {
                    if process_name.contains(gpu_name) {
                        let pid_u32 = pid.as_u32();
                        gpu_pids.insert(pid_u32);

                        gpu_processes.push(ProcessInfo {
                            device_id: 0,
                            device_uuid: "JetsonGPU".to_string(),
                            pid: pid_u32,
                            process_name: String::new(), // Will be filled by sysinfo
                            used_memory: 0, // Can't determine GPU memory usage without nvidia-smi
                            cpu_percent: 0.0, // Will be filled by sysinfo
                            memory_percent: 0.0, // Will be filled by sysinfo
                            memory_rss: 0,  // Will be filled by sysinfo
                            memory_vms: 0,  // Will be filled by sysinfo
                            user: String::new(), // Will be filled by sysinfo
                            state: String::new(), // Will be filled by sysinfo
                            start_time: String::new(), // Will be filled by sysinfo
                            cpu_time: 0,    // Will be filled by sysinfo
                            command: String::new(), // Will be filled by sysinfo
                            ppid: 0,        // Will be filled by sysinfo
                            threads: 0,     // Will be filled by sysinfo
                            uses_gpu: true,
                            priority: 0,          // Will be filled by sysinfo
                            nice_value: 0,        // Will be filled by sysinfo
                            gpu_utilization: 0.0, // Can't determine per-process GPU utilization
                        });
                        break;
                    }
                }
            }
        });
    }

    (gpu_processes, gpu_pids)
}

/// Total and used memory of the Jetson GPU, as `(total, used)` in bytes.
///
/// The GPU shares system RAM, so this is `MemTotal` and
/// `MemTotal - MemAvailable` from `meminfo_path`: the same figure
/// `tegrastats` prints as `RAM used/totalMB`, without spawning it.
fn get_memory_info(meminfo_path: &str) -> (u64, u64) {
    read_meminfo_memory(meminfo_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn memory_is_mem_total_and_total_minus_available() {
        let mut meminfo = tempfile::NamedTempFile::new().unwrap();
        writeln!(meminfo, "MemTotal:        7990124 kB").unwrap();
        writeln!(meminfo, "MemFree:          812340 kB").unwrap();
        writeln!(meminfo, "MemAvailable:    5120000 kB").unwrap();
        meminfo.flush().unwrap();

        let (total, used) = get_memory_info(meminfo.path().to_str().unwrap());
        assert_eq!(total, 7_990_124 * 1024);
        assert_eq!(used, (7_990_124 - 5_120_000) * 1024);
    }

    #[test]
    fn missing_meminfo_reads_as_zero() {
        assert_eq!(get_memory_info("/nonexistent/meminfo"), (0, 0));
    }

    fn counting_reader(
        result: fn() -> Option<CommandOutput>,
    ) -> (NvidiaJetsonGpuReader, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let reader = NvidiaJetsonGpuReader::with_compute_apps_runner(Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            result()
        }));
        (reader, calls)
    }

    #[test]
    fn a_missing_nvidia_smi_is_attempted_once_per_retry_interval() {
        let (reader, calls) = counting_reader(|| None);
        let t0 = Instant::now();

        assert!(reader.compute_apps_stdout(t0).is_none());
        assert!(
            reader
                .compute_apps_stdout(t0 + Duration::from_secs(59))
                .is_none()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        assert!(
            reader
                .compute_apps_stdout(t0 + COMPUTE_APPS_RETRY_INTERVAL)
                .is_none()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_non_zero_status_also_starts_the_back_off() {
        let (reader, calls) = counting_reader(|| {
            Some(CommandOutput {
                status: 1,
                stdout: "unsupported query".to_string(),
                stderr: String::new(),
            })
        });
        let t0 = Instant::now();

        assert!(reader.compute_apps_stdout(t0).is_none());
        assert!(reader.compute_apps_stdout(t0).is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_working_query_runs_every_call_and_yields_gpu_rows() {
        let (reader, calls) = counting_reader(|| {
            Some(CommandOutput {
                status: 0,
                stdout: "4242, 512\n".to_string(),
                stderr: String::new(),
            })
        });
        let t0 = Instant::now();

        let stdout = reader.compute_apps_stdout(t0);
        assert!(reader.compute_apps_stdout(t0).is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        let (rows, pids) = get_gpu_processes(stdout);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pid, 4242);
        assert_eq!(rows[0].used_memory, 512 * 1024 * 1024);
        assert!(pids.contains(&4242));
    }
}
