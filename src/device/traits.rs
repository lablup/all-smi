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

use crate::device::{
    ChassisInfo, CpuInfo, GpuInfo, MemoryInfo, MigGpuInfo, ProcessInfo, VgpuHostInfo,
};
use std::collections::HashSet;

pub trait GpuReader: Send + Sync {
    fn get_gpu_info(&self) -> Vec<GpuInfo>;
    fn get_process_info(&self) -> Vec<ProcessInfo>;

    /// Fetch fresh information for a single GPU/NPU identified by its
    /// stable [`GpuInfo::uuid`].
    ///
    /// The default implementation filters the full enumeration produced by
    /// [`GpuReader::get_gpu_info`] and is therefore no faster than the
    /// existing path; it exists so callers always have a uniform single-device
    /// API and so existing readers compile without change.
    ///
    /// Readers that can address a device directly (for example via
    /// `nvmlDeviceGetHandleByUUID`) SHOULD override this with a path that
    /// avoids enumerating every device.
    ///
    /// Returns `None` when no device with `uuid` is currently visible to this
    /// reader (e.g., the device was removed, the driver lost it, or the UUID
    /// belongs to a different reader's domain).
    // `#[allow(dead_code)]`: the default body is dispatched into via
    // `AllSmi::get_gpu_by_uuid`. The binary target does not exercise that
    // call path directly, which triggers a spurious unused-warning on the
    // trait method in `--bin all-smi --tests` builds.
    #[allow(dead_code)]
    fn get_gpu_info_by_uuid(&self, uuid: &str) -> Option<GpuInfo> {
        self.get_gpu_info().into_iter().find(|g| g.uuid == uuid)
    }

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

    /// Collect per-GPU vGPU host and instance information.
    ///
    /// Returns an empty vector on non-vGPU hardware or when the reader does
    /// not support vGPU at all (the default). Implementations that do support
    /// vGPU MUST ensure that a missing-host case returns an empty vector
    /// rather than panicking or producing synthetic rows.
    fn get_vgpu_info(&self) -> Vec<VgpuHostInfo> {
        Vec::new()
    }

    /// Collect per-GPU MIG (Multi-Instance GPU) host and instance information.
    ///
    /// Returns an empty vector on non-MIG hardware (consumer cards, older
    /// architectures than Ampere) or when the reader does not support MIG at
    /// all (the default). Implementations that do support MIG MUST ensure
    /// that any NVML failure (driver too old, `NotSupported`, missing
    /// permissions to enumerate instances) degrades gracefully to an empty
    /// vector rather than panicking or producing synthetic rows.
    fn get_mig_info(&self) -> Vec<MigGpuInfo> {
        Vec::new()
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
