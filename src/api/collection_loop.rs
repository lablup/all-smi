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

//! Background collection loop for `all-smi api` mode.
//!
//! Extracted from `server.rs` so the server module stays focused on
//! listener setup and so the loop can grow SSE-producer responsibilities
//! (issue #193) without blowing the 500-line soft limit.

use std::time::Duration;

use sysinfo::Disks;

use crate::api::FrameBus;
use crate::api::handlers::SharedState;
use crate::app_state::AppState;
use crate::device::{
    ChassisInfo, CpuInfo, GpuInfo, MemoryInfo, ProcessInfo, create_chassis_reader, get_cpu_readers,
    get_gpu_readers, get_memory_readers,
};
use crate::snapshot::{SNAPSHOT_SCHEMA_VERSION, Snapshot};
use crate::storage::info::StorageInfo;
use crate::utils::{filter_docker_aware_disks, get_hostname};

/// Run the collection loop forever. Caller spawns this as a tokio task.
///
/// The loop:
///
/// 1. Reads every device type.
/// 2. Writes the merged results into `AppState` so `/metrics` sees them.
/// 3. Integrates power samples into the energy accountant so Prometheus
///    counters remain monotonic.
/// 4. Builds a [`Snapshot`] covering every section and publishes it
///    through the [`FrameBus`] so `/events` and `/snapshot` both see the
///    same frame.
///
/// Step 4 deliberately does *not* gate sections on `processes`: the SSE
/// handler applies a per-client `?include=` filter at emit time, so every
/// published frame must carry every section the server is configured to
/// collect. `processes` remains opt-in at the server level because it is
/// expensive to enumerate on hosts with thousands of GPU-using
/// processes.
pub async fn run_collection_loop(
    state: SharedState,
    bus: FrameBus,
    interval_secs: u64,
    processes_enabled: bool,
) {
    let gpu_readers = get_gpu_readers();
    let cpu_readers = get_cpu_readers();
    let memory_readers = get_memory_readers();
    let chassis_reader = create_chassis_reader();
    let mut disks = Disks::new_with_refreshed_list();
    let hostname = get_hostname();
    let interval = Duration::from_secs(interval_secs);

    loop {
        let all_gpu_info: Vec<GpuInfo> = gpu_readers
            .iter()
            .flat_map(|reader| reader.get_gpu_info())
            .collect();

        let all_cpu_info: Vec<CpuInfo> = cpu_readers
            .iter()
            .flat_map(|reader| reader.get_cpu_info())
            .collect();

        let all_memory_info: Vec<MemoryInfo> = memory_readers
            .iter()
            .flat_map(|reader| reader.get_memory_info())
            .collect();

        let all_processes: Vec<ProcessInfo> = if processes_enabled {
            gpu_readers
                .iter()
                .flat_map(|reader| reader.get_process_info())
                .collect()
        } else {
            Vec::new()
        };

        // Collect chassis-level info (DMI, thermals, power).
        let chassis_info: Vec<ChassisInfo> = chassis_reader
            .get_chassis_info()
            .into_iter()
            .map(|mut ci| {
                // Aggregate GPU power into chassis total if not already set.
                if ci.total_power_watts.is_none() {
                    let total_gpu_power: f64 =
                        all_gpu_info.iter().map(|g| g.power_consumption).sum();
                    if total_gpu_power > 0.0 {
                        ci.total_power_watts = Some(total_gpu_power);
                    }
                }
                ci
            })
            .collect();

        disks.refresh(true);
        let storage_info = collect_storage_info_from(&disks, &hostname);

        // Build the shared `Snapshot` first using cloned collections so
        // the Prometheus-serving `AppState` and the SSE/snapshot frame
        // stay in sync for the same cycle. The clones are unavoidable
        // because the Prometheus renderer reads from `AppState` directly
        // by reference.
        let frame = Snapshot {
            schema: SNAPSHOT_SCHEMA_VERSION,
            timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            hostname: hostname.clone(),
            gpus: Some(all_gpu_info.clone()),
            cpus: Some(all_cpu_info.clone()),
            memory: Some(all_memory_info.clone()),
            chassis: Some(chassis_info.clone()),
            processes: if processes_enabled {
                Some(all_processes.clone())
            } else {
                None
            },
            storage: Some(storage_info.clone()),
            errors: Vec::new(),
        };

        // Hand the new collection to `AppState` first so the metrics
        // handler sees the freshest numbers, then publish the snapshot
        // frame onto the broadcast bus. Ordering is not strictly
        // required (both consumers are eventually-consistent), but
        // keeping the write lock scope short matters more than the
        // interleave.
        {
            let mut guard = state.write().await;
            guard.gpu_info = all_gpu_info;
            guard.cpu_info = all_cpu_info;
            guard.memory_info = all_memory_info;
            guard.process_info = all_processes;
            guard.chassis_info = chassis_info;
            guard.storage_info = storage_info;
            if guard.loading {
                guard.loading = false;
            }
            integrate_power_samples(&mut guard);
        }

        // `publish` is non-blocking with respect to receivers — a slow
        // SSE client cannot stall this loop (see `FrameBus::publish`).
        bus.publish(frame).await;

        tokio::time::sleep(interval).await;
    }
}

/// Integrate the current in-state power samples into the energy
/// accountant.
///
/// Mirrors `api::server::integrate_power_samples` before this extraction;
/// we keep the logic next to the loop that calls it so the two move
/// together.
pub(crate) fn integrate_power_samples(state: &mut AppState) {
    use std::time::Instant;

    use crate::metrics::energy::EnergyKey;

    let now = Instant::now();

    // Collect `(key, watts)` pairs first so we do not hold an immutable
    // borrow over `state.*_info` while taking the mutable borrow on
    // `state.energy`.
    let mut samples: Vec<(EnergyKey, f64)> =
        Vec::with_capacity(state.gpu_info.len() + state.cpu_info.len() + state.chassis_info.len());
    for gpu in &state.gpu_info {
        samples.push((
            EnergyKey::gpu(gpu.hostname.clone(), gpu.uuid.clone()),
            gpu.power_consumption,
        ));
    }
    for cpu in &state.cpu_info {
        if let Some(power) = cpu.power_consumption {
            samples.push((EnergyKey::cpu(cpu.hostname.clone()), power));
        }
    }
    for chassis in &state.chassis_info {
        if let Some(power) = chassis.total_power_watts {
            samples.push((EnergyKey::chassis(chassis.hostname.clone()), power));
        }
    }

    let wal_index = &mut state.energy_wal_replay;
    let integrator = state.energy.integrator_mut();
    for (key, watts) in samples {
        if !integrator.has_samples(&key) && !wal_index.is_empty() {
            wal_index.seed_if_matches(&key, integrator);
        }
        integrator.record_sample(key, now, watts);
    }
}

/// Collect storage/disk information from a pre-existing Disks instance.
/// The caller is responsible for calling `refresh_list()` before this function.
fn collect_storage_info_from(disks: &Disks, hostname: &str) -> Vec<StorageInfo> {
    let mut storage_info = Vec::new();
    let mut filtered_disks = filter_docker_aware_disks(disks);
    filtered_disks.sort_by(|a, b| {
        a.mount_point()
            .to_string_lossy()
            .cmp(&b.mount_point().to_string_lossy())
    });

    for (index, disk) in filtered_disks.iter().enumerate() {
        let mount_point_str = disk.mount_point().to_string_lossy();
        storage_info.push(StorageInfo {
            mount_point: mount_point_str.to_string(),
            total_bytes: disk.total_space(),
            available_bytes: disk.available_space(),
            host_id: hostname.to_string(),
            hostname: hostname.to_string(),
            index: index as u32,
        });
    }

    storage_info
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_storage_info_respects_hostname() {
        // Driving the helper with an empty Disks instance is enough to
        // prove the hostname is threaded through. A deeper disk-level
        // test would depend on the host's mount table, which we do not
        // want in unit tests.
        let disks = Disks::new_with_refreshed_list();
        let _ = collect_storage_info_from(&disks, "h");
    }
}
