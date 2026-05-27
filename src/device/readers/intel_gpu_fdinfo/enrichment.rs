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

//! Sysinfo enrichment for the Intel per-process GPU memory pipeline.
//!
//! Split out of the parent module so the per-driver fdinfo parser
//! stays small and the sysinfo-coupled enrichment logic can grow
//! independently. The pure-Rust fdinfo work is fully tested via
//! synthetic procfs fixtures; this enrichment layer simply merges
//! the GPU-only `(pid, card_index, used_memory_bytes)` aggregates
//! with the cpu / user / state / rss metadata that sysinfo provides.
//!
//! Mirrors the AMD reader's `get_process_info` enrichment pattern
//! at `crate::device::readers::amd` so cross-vendor consumers see
//! a consistent `ProcessInfo` shape.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::collect_intel_gpu_processes;

/// Walk `/proc` for Intel-GPU-using processes, enrich them with
/// sysinfo metadata, and assemble the full `Vec<ProcessInfo>` the
/// [`crate::device::traits::GpuReader`] contract expects.
///
/// `card_uuids` maps each Intel card index to the same UUID the
/// reader emits from `get_gpu_info` so consumers correlating
/// `ProcessInfo.device_uuid` against `GpuInfo.uuid` see consistent
/// identifiers.
///
/// `proc_root` is normally `/proc`; the parameter exists so callers
/// can drive a synthetic procfs in integration tests.
pub fn build_intel_process_infos(
    intel_drm_basenames: &HashMap<String, usize>,
    card_uuids: &HashMap<usize, String>,
    proc_root: &Path,
) -> Vec<crate::device::types::ProcessInfo> {
    use crate::utils::with_global_system;
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, UpdateKind};

    let usages = collect_intel_gpu_processes(intel_drm_basenames, proc_root);
    if usages.is_empty() {
        return Vec::new();
    }

    // Single sysinfo refresh — same shape as AMD's reader: minimal
    // ProcessRefreshKind so we do not pay for full-system refresh,
    // and `get_all_processes` returns rich ProcessInfo rows for every
    // PID in `gpu_pids`.
    let gpu_pids: HashSet<u32> = usages.iter().map(|u| u.pid).collect();
    let system_processes = with_global_system(|system| {
        let refresh_kind = ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_user(UpdateKind::OnlyIfNotSet);
        system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind);
        crate::device::process_list::get_all_processes(system, &gpu_pids)
    });
    let process_map: HashMap<u32, _> = system_processes.iter().map(|p| (p.pid, p)).collect();

    let mut out = Vec::with_capacity(usages.len());
    for usage in usages {
        // device_uuid for a card we no longer know is a contract bug —
        // emit an empty string rather than fabricating one.
        let uuid = card_uuids
            .get(&usage.card_index)
            .cloned()
            .unwrap_or_default();
        let sys_proc = process_map.get(&usage.pid);
        out.push(crate::device::types::ProcessInfo {
            device_id: usage.card_index,
            device_uuid: uuid,
            pid: usage.pid,
            process_name: sys_proc.map(|p| p.process_name.clone()).unwrap_or_default(),
            used_memory: usage.used_memory_bytes,
            cpu_percent: sys_proc.map(|p| p.cpu_percent).unwrap_or(0.0),
            memory_percent: sys_proc.map(|p| p.memory_percent).unwrap_or(0.0),
            memory_rss: sys_proc.map(|p| p.memory_rss).unwrap_or(0),
            memory_vms: sys_proc.map(|p| p.memory_vms).unwrap_or(0),
            user: sys_proc.map(|p| p.user.clone()).unwrap_or_default(),
            state: sys_proc.map(|p| p.state.clone()).unwrap_or_default(),
            start_time: sys_proc.map(|p| p.start_time.clone()).unwrap_or_default(),
            cpu_time: sys_proc.map(|p| p.cpu_time).unwrap_or(0),
            command: sys_proc.map(|p| p.command.clone()).unwrap_or_default(),
            ppid: sys_proc.map(|p| p.ppid).unwrap_or(0),
            threads: sys_proc.map(|p| p.threads).unwrap_or(0),
            uses_gpu: true,
            priority: sys_proc.map(|p| p.priority).unwrap_or(0),
            nice_value: sys_proc.map(|p| p.nice_value).unwrap_or(0),
            // Per-process engine-time utilization is the deferred
            // stretch goal of issue #247; v1 reports zero.
            gpu_utilization: 0.0,
        });
    }
    out
}
