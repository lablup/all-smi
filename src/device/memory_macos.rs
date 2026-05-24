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
use std::sync::RwLock;
use sysinfo::System;

use crate::device::{MemoryInfo, MemoryReader};
use crate::utils::get_hostname;

pub struct MacOsMemoryReader {
    system: RwLock<System>,
}

impl Default for MacOsMemoryReader {
    fn default() -> Self {
        Self::new()
    }
}

impl MacOsMemoryReader {
    pub fn new() -> Self {
        let mut system = System::new();
        // Initial refresh to populate memory information
        system.refresh_memory();

        Self {
            system: RwLock::new(system),
        }
    }
}

impl MemoryReader for MacOsMemoryReader {
    fn get_memory_info(&self) -> Vec<MemoryInfo> {
        let mut memory_info = Vec::new();

        // Refresh memory information using the cached System instance
        self.system.write().unwrap().refresh_memory();

        // Now read the memory information
        let system = self.system.read().unwrap();

        let total_bytes = system.total_memory();
        let used_bytes = system.used_memory();
        let free_bytes = system.free_memory();
        let available_bytes = system.available_memory();

        // Calculate utilization percentage
        let utilization = if total_bytes > 0 {
            (used_bytes as f64 / total_bytes as f64) * 100.0
        } else {
            0.0
        };

        // Get swap information
        let swap_total_bytes = system.total_swap();
        let swap_used_bytes = system.used_swap();
        let swap_free_bytes = system.free_swap();

        let hostname = get_hostname();
        let now = Local::now();

        memory_info.push(MemoryInfo {
            index: 0,                  // Overwritten by AllSmi::get_memory_info when flattening readers
            host_id: hostname.clone(), // For local mode, host_id is just the hostname
            hostname: hostname.clone(),
            instance: hostname,
            total_bytes,
            used_bytes,
            available_bytes,
            free_bytes,
            buffers_bytes: 0, // Not applicable on macOS
            cached_bytes: 0,  // sysinfo doesn't provide cached memory on macOS
            swap_total_bytes,
            swap_used_bytes,
            swap_free_bytes,
            utilization,
            time: now.format("%Y-%m-%d %H:%M:%S").to_string(),
        });

        memory_info
    }
}
