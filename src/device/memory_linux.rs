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

use chrono::Local;
use once_cell::sync::Lazy;
use std::fs;

use crate::device::container_info::ContainerInfo;
use crate::device::{MemoryInfo, MemoryReader};
use crate::utils::get_hostname;

// Cache container detection result globally to avoid repeated filesystem operations
static CONTAINER_INFO: Lazy<Option<ContainerInfo>> = Lazy::new(|| {
    let info = ContainerInfo::detect();
    if info.is_container { Some(info) } else { None }
});

pub struct LinuxMemoryReader {
    // Reference to the global container info
    container_info: &'static Option<ContainerInfo>,
}

impl Default for LinuxMemoryReader {
    fn default() -> Self {
        Self::new()
    }
}

impl LinuxMemoryReader {
    pub fn new() -> Self {
        LinuxMemoryReader {
            container_info: &CONTAINER_INFO,
        }
    }
}

impl MemoryReader for LinuxMemoryReader {
    fn get_memory_info(&self) -> Vec<MemoryInfo> {
        let mut memory_info = Vec::new();

        // Check if we're in a container and have memory limits
        if let Some(container_info) = self.container_info
            && let Some((total, used, utilization)) = container_info.get_memory_stats()
        {
            // Use container memory limits
            let hostname = get_hostname();
            let now = Local::now();

            // For containers, we primarily care about the limit and usage
            // Other values like buffers/cached are from /proc/meminfo but less relevant
            memory_info.push(MemoryInfo {
                host_id: hostname.clone(),
                hostname: hostname.clone(),
                instance: hostname,
                total_bytes: total,
                used_bytes: used,
                available_bytes: total.saturating_sub(used),
                free_bytes: total.saturating_sub(used),
                buffers_bytes: 0,    // Not easily available in container context
                cached_bytes: 0,     // Not easily available in container context
                swap_total_bytes: 0, // Container swap is complex
                swap_used_bytes: 0,
                swap_free_bytes: 0,
                utilization,
                time: now.format("%Y-%m-%d %H:%M:%S").to_string(),
            });

            return memory_info;
        }

        // Fall back to reading /proc/meminfo for non-container or when container info is not available
        if let Ok(meminfo_content) = fs::read_to_string("/proc/meminfo") {
            let mut total_bytes = 0;
            let mut available_bytes = 0;
            let mut free_bytes = 0;
            let mut buffers_bytes = 0;
            let mut cached_bytes = 0;
            let mut swap_total_bytes = 0;
            let mut swap_free_bytes = 0;

            for line in meminfo_content.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    let key = parts[0].trim_end_matches(':');
                    if let Ok(value) = parts[1].parse::<u64>() {
                        let value_bytes = value * 1024; // Convert from kB to bytes

                        match key {
                            "MemTotal" => total_bytes = value_bytes,
                            "MemAvailable" => available_bytes = value_bytes,
                            "MemFree" => free_bytes = value_bytes,
                            "Buffers" => buffers_bytes = value_bytes,
                            "Cached" => cached_bytes = value_bytes,
                            "SwapTotal" => swap_total_bytes = value_bytes,
                            "SwapFree" => swap_free_bytes = value_bytes,
                            _ => {}
                        }
                    }
                }
            }

            // Calculate used memory
            let used_bytes = total_bytes - available_bytes;
            let swap_used_bytes = swap_total_bytes - swap_free_bytes;

            // Calculate utilization percentage
            let utilization = if total_bytes > 0 {
                (used_bytes as f64 / total_bytes as f64) * 100.0
            } else {
                0.0
            };

            let hostname = get_hostname();
            let now = Local::now();

            memory_info.push(MemoryInfo {
                host_id: hostname.clone(), // For local mode, host_id is just the hostname
                hostname: hostname.clone(),
                instance: hostname,
                total_bytes,
                used_bytes,
                available_bytes,
                free_bytes,
                buffers_bytes,
                cached_bytes,
                swap_total_bytes,
                swap_used_bytes,
                swap_free_bytes,
                utilization,
                time: now.format("%Y-%m-%d %H:%M:%S").to_string(),
            });
        }

        memory_info
    }
}

#[cfg(test)]
#[path = "memory_linux/tests.rs"]
mod tests;
