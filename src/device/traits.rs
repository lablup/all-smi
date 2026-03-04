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

use crate::device::{ChassisInfo, CpuInfo, GpuInfo, MemoryInfo, ProcessInfo};
use std::collections::HashSet;

pub trait GpuReader: Send + Sync {
    fn get_gpu_info(&self) -> Vec<GpuInfo>;
    fn get_process_info(&self) -> Vec<ProcessInfo>;

    /// Return only raw GPU/NPU process entries and their PIDs, without
    /// system-wide process enumeration.  The collector uses this to avoid
    /// a redundant second call to `merge_gpu_processes`.
    fn get_gpu_processes(&self) -> (Vec<ProcessInfo>, HashSet<u32>) {
        let processes = self.get_process_info();
        let pids = processes
            .iter()
            .filter(|p| p.uses_gpu)
            .map(|p| p.pid)
            .collect();
        let gpu_only = processes.into_iter().filter(|p| p.uses_gpu).collect();
        (gpu_only, pids)
    }
}

pub trait CpuReader: Send + Sync {
    fn get_cpu_info(&self) -> Vec<CpuInfo>;
}

pub trait MemoryReader: Send + Sync {
    fn get_memory_info(&self) -> Vec<MemoryInfo>;
}

/// Chassis/Node-level reader for system-wide metrics
/// Provides access to total power, thermal data, and BMC information
pub trait ChassisReader: Send + Sync {
    /// Get chassis information for the current node
    fn get_chassis_info(&self) -> Option<ChassisInfo>;
}
