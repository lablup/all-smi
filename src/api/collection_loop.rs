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
//!
//! Division of labour (issue #503): every reader call is blocking (driver
//! ioctls, sysfs, CLI fallbacks, the disk capacity refresh), so the whole
//! read phase of a tick runs in one `spawn_blocking` task and never on an
//! async worker. The async side only takes the `AppState` write lock,
//! integrates energy, renders the `/metrics` body once, and publishes the
//! frame.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use tokio::time::MissedTickBehavior;

use crate::api::FrameBus;
use crate::api::handlers::SharedState;
use crate::api::handlers::ready::is_ready;
use crate::api::metrics::render::{MetricsRenderInputs, render_prometheus_exposition};
use crate::app_state::AppState;
use crate::device::{
    ChassisInfo, ChassisReader, CpuInfo, CpuReader, GpuInfo, GpuReader, MemoryInfo, MemoryReader,
    ProcessInfo, create_chassis_reader, get_cpu_readers, get_gpu_readers, get_memory_readers,
};
use crate::snapshot::{SNAPSHOT_SCHEMA_VERSION, Snapshot};
use crate::storage::disk_cache::DiskCache;
use crate::storage::info::StorageInfo;
use crate::utils::get_hostname;

/// Everything one tick reads from the host.
pub(crate) struct Collected {
    pub gpus: Vec<GpuInfo>,
    pub cpus: Vec<CpuInfo>,
    pub memory: Vec<MemoryInfo>,
    pub processes: Vec<ProcessInfo>,
    pub chassis: Vec<ChassisInfo>,
    pub storage: Vec<StorageInfo>,
}

/// The long-lived reader set the loop collects from.
///
/// Shared with the blocking pool through an `Arc`, so construction and
/// every per-tick read happen off the async workers. `DiskCache` needs
/// `&mut self`, hence the mutex; only one tick runs at a time, so it is
/// never contended.
pub(crate) struct CollectionReaders {
    gpu_readers: Vec<Box<dyn GpuReader>>,
    cpu_readers: Vec<Box<dyn CpuReader>>,
    memory_readers: Vec<Box<dyn MemoryReader>>,
    chassis_reader: Box<dyn ChassisReader>,
    disks: Mutex<DiskCache>,
    hostname: String,
    processes_enabled: bool,
}

impl CollectionReaders {
    /// Build every reader. Blocking: NVML init, CLI probes, and the first
    /// mount-table listing all happen here.
    pub(crate) fn new(processes_enabled: bool) -> Self {
        // CPU readers first: the macOS reader takes its first utilization
        // sample at construction and its first tick waits only for what is
        // left of the warm-up interval, so building the other readers after
        // it overlaps that wait (issue #414).
        let cpu_readers = get_cpu_readers();
        let gpu_readers = get_gpu_readers();
        let memory_readers = get_memory_readers();
        let chassis_reader = create_chassis_reader();
        Self {
            gpu_readers,
            cpu_readers,
            memory_readers,
            chassis_reader,
            // Shared with the view collector and `LocalStorageReader`: the
            // mount table is enumerated off the loop every 30 s, and each
            // cycle only refreshes capacities.
            disks: Mutex::new(DiskCache::new()),
            hostname: get_hostname(),
            processes_enabled,
        }
    }

    /// The process list for this tick.
    ///
    /// Deliberately the single place the loop assembles processes, so a
    /// different process source can replace this one call.
    fn collect_processes(&self) -> Vec<ProcessInfo> {
        crate::device::process_list::collect_process_rows(&self.gpu_readers)
    }

    /// Run the whole read phase of one tick. Blocking.
    pub(crate) fn collect(&self) -> Collected {
        let gpus: Vec<GpuInfo> = self
            .gpu_readers
            .iter()
            .flat_map(|reader| reader.get_gpu_info())
            .collect();

        let cpus: Vec<CpuInfo> = self
            .cpu_readers
            .iter()
            .flat_map(|reader| reader.get_cpu_info())
            .collect();

        let memory: Vec<MemoryInfo> = self
            .memory_readers
            .iter()
            .flat_map(|reader| reader.get_memory_info())
            .collect();

        let processes = if self.processes_enabled {
            self.collect_processes()
        } else {
            Vec::new()
        };

        // Collect chassis-level info (DMI, thermals, power).
        let chassis: Vec<ChassisInfo> = self
            .chassis_reader
            .get_chassis_info()
            .into_iter()
            .map(|mut ci| {
                // Aggregate GPU power into chassis total if not already set.
                if ci.total_power_watts.is_none() {
                    let total_gpu_power = crate::metrics::gpu_readings::total_power_watts(&gpus);
                    if total_gpu_power > 0.0 {
                        ci.total_power_watts = Some(total_gpu_power);
                    }
                }
                ci
            })
            .collect();

        let storage = match self.disks.lock() {
            Ok(mut disks) => disks.storage_info(&self.hostname),
            // A reader panic on an earlier tick poisoned the cache. Its
            // state is a plain mount list, so keep using it.
            Err(poisoned) => poisoned.into_inner().storage_info(&self.hostname),
        };

        Collected {
            gpus,
            cpus,
            memory,
            processes,
            chassis,
            storage,
        }
    }
}

/// Run the collection loop forever. Caller spawns this as a tokio task.
///
/// Each tick:
///
/// 1. Reads every device type on the blocking pool.
/// 2. Integrates power samples into the energy accountant so Prometheus
///    counters remain monotonic.
/// 3. Renders the `/metrics` exposition once and stores it in `AppState`,
///    so a scrape is a pointer clone (issue #503).
/// 4. Moves the collected vectors into a [`Snapshot`] and publishes it
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
    let readers = match tokio::task::spawn_blocking(move || {
        CollectionReaders::new(processes_enabled)
    })
    .await
    {
        Ok(readers) => Arc::new(readers),
        Err(e) => {
            tracing::error!("collection loop: building the device readers failed: {e}");
            return;
        }
    };
    let hostname = readers.hostname.clone();
    run_loop_with(
        state,
        bus,
        Duration::from_secs(interval_secs),
        hostname,
        processes_enabled,
        move || readers.collect(),
    )
    .await;
}

/// The loop itself, with the read phase injected so tests can drive it
/// with a fake reader set.
///
/// Ticks come from a [`tokio::time::interval`], so tick start times are
/// `interval` apart regardless of how long a collection takes (the old
/// trailing sleep made the period `interval + collection time`). With
/// [`MissedTickBehavior::Delay`], a collection that overruns the interval
/// makes the ticks run back to back instead of bursting to catch up. The
/// first tick fires immediately.
pub(crate) async fn run_loop_with<F>(
    state: SharedState,
    bus: FrameBus,
    interval: Duration,
    hostname: String,
    processes_enabled: bool,
    collect: F,
) where
    F: Fn() -> Collected + Send + Sync + 'static,
{
    let collect = Arc::new(collect);
    // `tokio::time::interval` panics on a zero period.
    let mut ticker = tokio::time::interval(interval.max(Duration::from_millis(1)));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        ticker.tick().await;

        let collect = collect.clone();
        let collected = match tokio::task::spawn_blocking(move || collect()).await {
            Ok(collected) => collected,
            Err(e) => {
                // A reader panicked (or the runtime is shutting down).
                // Skip this tick; the previous `/metrics` body and frame
                // stay in place and the next tick tries again.
                tracing::warn!("collection loop: skipping a tick, the read phase failed: {e}");
                continue;
            }
        };

        publish_tick(&state, &bus, &hostname, processes_enabled, collected).await;
    }
}

/// The async half of a tick: account energy, render `/metrics`, publish.
pub(crate) async fn publish_tick(
    state: &SharedState,
    bus: &FrameBus,
    hostname: &str,
    processes_enabled: bool,
    collected: Collected,
) {
    let Collected {
        gpus,
        cpus,
        memory,
        processes,
        chassis,
        storage,
    } = collected;

    // Render while holding the write lock, after the energy integration,
    // so the stored body is consistent with the counters it prints. The
    // device vectors are read from the locals: `/metrics` no longer reads
    // them out of `AppState`, so nothing here needs a clone.
    {
        let mut guard = state.write().await;
        if guard.loading {
            guard.loading = false;
        }
        integrate_power_samples(&mut guard, &gpus, &cpus, &chassis);
        let body = render_prometheus_exposition(&MetricsRenderInputs {
            gpu_info: &gpus,
            process_info: &processes,
            cpu_info: &cpus,
            memory_info: &memory,
            storage_info: &storage,
            runtime_environment: &guard.runtime_environment,
            chassis_info: &chassis,
            vgpu_info: &guard.vgpu_info,
            mig_info: &guard.mig_info,
            energy_integrator: Some(guard.energy.integrator()),
            // Same predicate `/-/ready` answers with, read under the same
            // lock acquisition as the data itself so the gauge cannot
            // disagree with the samples it is rendered alongside (issue
            // #324).
            ready: is_ready(&guard),
        });
        guard.metrics_body = Bytes::from(body);
    }

    let frame = Snapshot {
        schema: SNAPSHOT_SCHEMA_VERSION,
        timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        hostname: hostname.to_string(),
        gpus: Some(gpus),
        cpus: Some(cpus),
        memory: Some(memory),
        chassis: Some(chassis),
        processes: processes_enabled.then_some(processes),
        storage: Some(storage),
        errors: Vec::new(),
    };

    // `publish` is non-blocking with respect to receivers — a slow
    // SSE client cannot stall this loop (see `FrameBus::publish`).
    bus.publish(frame).await;
}

/// Integrate this tick's power samples into the energy accountant.
///
/// Mirrors `api::server::integrate_power_samples` before this extraction;
/// we keep the logic next to the loop that calls it so the two move
/// together.
pub(crate) fn integrate_power_samples(
    state: &mut AppState,
    gpus: &[GpuInfo],
    cpus: &[CpuInfo],
    chassis: &[ChassisInfo],
) {
    use std::time::Instant;

    use crate::metrics::energy::EnergyKey;

    let now = Instant::now();

    let mut samples: Vec<(EnergyKey, f64)> =
        Vec::with_capacity(gpus.len() + cpus.len() + chassis.len());
    // Devices with no power reading contribute no sample: the integrator's
    // "has this device ever been seen?" check is what drives the exporter's
    // omit-on-no-data behavior, and feeding it the unavailable sentinel would
    // accumulate negative joules (issue #325).
    for gpu in gpus {
        if let Some(watts) = gpu.power_consumption_reading() {
            samples.push((
                EnergyKey::gpu(gpu.hostname.clone(), gpu.uuid.clone()),
                watts,
            ));
        }
    }
    for cpu in cpus {
        if let Some(power) = cpu.power_consumption {
            samples.push((EnergyKey::cpu(cpu.hostname.clone()), power));
        }
    }
    for chassis in chassis {
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

#[cfg(test)]
#[path = "collection_loop_tests.rs"]
mod tests;
