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

use crate::device::common::{execute_command_default, parse_csv_line};
use crate::device::process_list::{get_all_processes, merge_gpu_processes};
use crate::device::readers::common_cache::{DetailBuilder, DeviceStaticInfo};
use crate::device::types::{GpuInfo, ProcessInfo};
use crate::device::GpuReader;
use crate::utils::{get_hostname, hz_to_mhz, millicelsius_to_celsius, with_global_system};
use chrono::Local;
use std::collections::HashSet;
use std::fs;
use std::sync::OnceLock;

pub struct NvidiaJetsonGpuReader {
    /// Cached static device information (fetched only once)
    static_info: OnceLock<DeviceStaticInfo>,
}

impl Default for NvidiaJetsonGpuReader {
    fn default() -> Self {
        Self::new()
    }
}

impl NvidiaJetsonGpuReader {
    pub fn new() -> Self {
        Self {
            static_info: OnceLock::new(),
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
            if let Ok(output) = execute_command_default("nvidia-smi", &[]) {
                if output.status == 0 {
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
            if let Ok(l4t) = fs::read_to_string("/etc/nv_tegra_release") {
                if let Some(version) = l4t.split_whitespace().nth(1) {
                    detail.insert("L4T Version".to_string(), version.to_string());
                }
            }

            // Static hardware info
            detail.insert("GPU Type".to_string(), "Integrated".to_string());
            detail.insert("Architecture".to_string(), "Tegra".to_string());

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

        let (total_memory, used_memory) = get_memory_info();

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
            detail: static_info.detail.clone(),
        };

        gpu_info.push(info);
        gpu_info
    }

    fn get_process_info(&self) -> Vec<ProcessInfo> {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, UpdateKind};

        // Get GPU processes and PIDs
        let (gpu_processes, gpu_pids) = get_gpu_processes();

        // Use global system instance to avoid file descriptor leak
        let mut all_processes = with_global_system(|system| {
            system.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::everything().with_user(UpdateKind::Always),
            );
            system.refresh_memory();

            // Get all system processes
            get_all_processes(system, &gpu_pids)
        });

        // Merge GPU information into the process list
        merge_gpu_processes(&mut all_processes, gpu_processes);

        all_processes
    }
}

/// Get GPU processes for Jetson
fn get_gpu_processes() -> (Vec<ProcessInfo>, HashSet<u32>) {
    let mut gpu_processes = Vec::new();
    let mut gpu_pids = HashSet::new();

    // Jetson doesn't have a direct way to query GPU processes
    // We can try nvidia-smi if available (on newer Jetson models)
    if let Ok(output) = execute_command_default(
        "nvidia-smi",
        &[
            "--query-compute-apps=pid,used_memory",
            "--format=csv,noheader,nounits",
        ],
    ) {
        if output.status == 0 {
            for line in output.stdout.lines() {
                let parts = parse_csv_line(line);
                if parts.len() >= 2 {
                    if let Ok(pid) = parts[0].parse::<u32>() {
                        if let Ok(used_memory_mb) = parts[1].parse::<u64>() {
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

fn get_memory_info() -> (u64, u64) {
    // Try to get GPU memory from tegrastats
    if let Ok(output) = execute_command_default("tegrastats", &["--once"]) {
        if output.status == 0 {
            let output_str = &output.stdout;
            // Parse memory info from tegrastats output
            // Format: RAM 2298/3964MB (lfb 25x4MB) SWAP 0/1982MB (cached 0MB)
            if let Some(ram_part) = output_str.split("RAM ").nth(1) {
                if let Some(ram_info) = ram_part.split("MB").next() {
                    let parts: Vec<&str> = ram_info.split('/').collect();
                    if parts.len() == 2 {
                        let used = parts[0].parse::<u64>().unwrap_or(0) * 1024 * 1024;
                        let total = parts[1].parse::<u64>().unwrap_or(0) * 1024 * 1024;
                        return (total, used);
                    }
                }
            }
        }
    }

    // Fallback to system memory
    if let Ok(meminfo) = fs::read_to_string("/proc/meminfo") {
        let mut total = 0;
        let mut available = 0;

        for line in meminfo.lines() {
            if line.starts_with("MemTotal:") {
                if let Some(value) = line.split_whitespace().nth(1) {
                    total = value.parse::<u64>().unwrap_or(0) * 1024; // Convert KB to bytes
                }
            } else if line.starts_with("MemAvailable:") {
                if let Some(value) = line.split_whitespace().nth(1) {
                    available = value.parse::<u64>().unwrap_or(0) * 1024; // Convert KB to bytes
                }
            }
        }

        let used = total.saturating_sub(available);
        (total, used)
    } else {
        (0, 0)
    }
}
