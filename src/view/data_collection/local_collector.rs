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

use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use sysinfo::Disks;
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio::time::timeout;

/// Type alias for the process cache using std::sync::RwLock for synchronous access
type ProcessCache = std::sync::RwLock<HashMap<u32, ProcessInfo>>;

use crate::app_state::AppState;
#[cfg(target_os = "linux")]
use crate::device::platform_detection::has_tenstorrent;
use crate::device::{
    ChassisInfo, ChassisReader, CpuInfo, CpuReader, GpuInfo, GpuReader, MemoryInfo, MemoryReader,
    MigGpuInfo, ProcessInfo, VgpuHostInfo, create_chassis_reader, get_cpu_readers, get_gpu_readers,
    get_memory_readers, get_nvml_status_message,
    platform_detection::has_nvidia,
    process_list::{merge_gpu_processes, update_process_cache},
};

#[cfg(target_os = "linux")]
use crate::device::get_tenstorrent_status_message;
#[cfg(target_os = "linux")]
use crate::device::get_tpu_status_message;
#[cfg(target_os = "linux")]
use crate::device::platform_detection::has_google_tpu;
use crate::storage::info::StorageInfo;
use crate::utils::{filter_docker_aware_disks, get_hostname, with_global_system};

use super::aggregator::DataAggregator;
use super::strategy::{
    CollectionConfig, CollectionData, CollectionError, CollectionResult, DataCollectionStrategy,
};

/// Maximum number of processes to keep after collection.
/// Processes are sorted by CPU usage (descending) and truncated to this limit
/// to reduce CPU overhead from tracking thousands of processes.
const MAX_DISPLAY_PROCESSES: usize = 500;

/// How often to do a full process refresh to discover new high-CPU processes.
/// Every N cycles, we refresh all processes; otherwise, we only refresh tracked PIDs.
const FULL_REFRESH_INTERVAL: u32 = 5;

/// Inject aggregated GPU power into chassis info when not already set.
fn inject_gpu_power(chassis_info: Vec<ChassisInfo>, gpu_info: &[GpuInfo]) -> Vec<ChassisInfo> {
    chassis_info
        .into_iter()
        .map(|mut ci| {
            if ci.total_power_watts.is_none() {
                let total: f64 = gpu_info.iter().map(|g| g.power_consumption).sum();
                if total > 0.0 {
                    ci.total_power_watts = Some(total);
                }
            }
            ci
        })
        .collect()
}

/// Everything the GPU reader set produces in one pass.
///
/// The four GPU queries stay grouped in a single unit of work on purpose.
/// They all run against the same `Box<dyn GpuReader>` instances, which carry
/// internal sampling state (IOReport deltas on Apple Silicon, cached NVML
/// handles on NVIDIA), so issuing them concurrently against one reader would
/// change the order in which that state is touched. Keeping them back to back
/// preserves the exact call sequence the collector used before, while still
/// letting the whole group overlap with the CPU, memory, chassis, storage and
/// process groups.
#[derive(Default)]
struct GpuCollection {
    gpu_info: Vec<GpuInfo>,
    gpu_processes: Vec<ProcessInfo>,
    gpu_pids: HashSet<u32>,
    vgpu_info: Vec<VgpuHostInfo>,
    mig_info: Vec<MigGpuInfo>,
}

/// Run the full GPU reader pass in the same order the collector always used:
/// device info, GPU processes, vGPU, then MIG.
fn collect_from_gpu_readers(readers: &[Box<dyn GpuReader>]) -> GpuCollection {
    let gpu_info: Vec<GpuInfo> = readers
        .iter()
        .flat_map(|reader| reader.get_gpu_info())
        .collect();

    let mut gpu_processes = Vec::new();
    let mut gpu_pids = HashSet::new();
    for reader in readers.iter() {
        let (procs, pids) = reader.get_gpu_processes();
        gpu_processes.extend(procs);
        gpu_pids.extend(pids);
    }

    // vGPU and MIG run on the same readers set so they share NVML handle
    // caching with the main GPU collection.
    let vgpu_info: Vec<VgpuHostInfo> = readers
        .iter()
        .flat_map(|reader| reader.get_vgpu_info())
        .collect();
    let mig_info: Vec<MigGpuInfo> = readers
        .iter()
        .flat_map(|reader| reader.get_mig_info())
        .collect();

    GpuCollection {
        gpu_info,
        gpu_processes,
        gpu_pids,
        vgpu_info,
        mig_info,
    }
}

/// Move one synchronous reader pass onto the tokio blocking pool.
///
/// The owned read guard is acquired here, in async context, and then moved
/// into the closure. That ordering is what makes the whole refactor possible:
/// the reader collections live behind tokio's async `RwLock`, whose `read()`
/// is a future and therefore unusable inside a blocking closure, while
/// `blocking_read()` from the blocking pool would risk deadlocking against a
/// queued async writer. `OwnedRwLockReadGuard<T>` is `'static + Send` whenever
/// `T: Send + Sync`, which every reader trait already guarantees, so it
/// satisfies the `spawn_blocking` bounds without cloning or re-owning a single
/// reader.
///
/// The guard is held for the lifetime of the blocking task. That is safe
/// because the reader collections are written exactly once, by
/// `initialize_readers`, before the first collection runs, and are read-only
/// from then on. There is no hotplug/refresh path that takes a write lock
/// while a collection is in flight.
async fn spawn_reader_pass<T, R, F>(lock: &Arc<RwLock<T>>, f: F) -> JoinHandle<R>
where
    T: Send + Sync + 'static,
    R: Send + 'static,
    F: FnOnce(&T) -> R + Send + 'static,
{
    let guard = Arc::clone(lock).read_owned().await;
    tokio::task::spawn_blocking(move || f(&guard))
}

pub struct LocalCollector {
    gpu_readers: Arc<RwLock<Vec<Box<dyn GpuReader>>>>,
    cpu_readers: Arc<RwLock<Vec<Box<dyn CpuReader>>>>,
    memory_readers: Arc<RwLock<Vec<Box<dyn MemoryReader>>>>,
    chassis_reader: Arc<RwLock<Option<Box<dyn ChassisReader>>>>,
    aggregator: DataAggregator,
    initialized: Arc<Mutex<bool>>,
    /// PIDs of processes from the previous collection cycle (top N by CPU usage).
    /// Used for selective process refresh to reduce CPU overhead.
    tracked_pids: Arc<RwLock<Vec<sysinfo::Pid>>>,
    /// Counter for refresh cycles; every FULL_REFRESH_INTERVAL cycles we do a full refresh.
    refresh_cycle: Arc<AtomicU32>,
    /// Cache of ProcessInfo objects by PID to reduce memory allocation overhead.
    /// On each collection, existing objects are updated in place rather than reallocated.
    /// Uses std::sync::RwLock for synchronous access within with_global_system closure.
    process_cache: Arc<ProcessCache>,
}

impl LocalCollector {
    pub fn new() -> Self {
        Self {
            gpu_readers: Arc::new(RwLock::new(Vec::new())),
            cpu_readers: Arc::new(RwLock::new(Vec::new())),
            memory_readers: Arc::new(RwLock::new(Vec::new())),
            chassis_reader: Arc::new(RwLock::new(None)),
            aggregator: DataAggregator::new(),
            initialized: Arc::new(Mutex::new(false)),
            tracked_pids: Arc::new(RwLock::new(Vec::new())),
            refresh_cycle: Arc::new(AtomicU32::new(0)),
            process_cache: Arc::new(std::sync::RwLock::new(HashMap::with_capacity(
                MAX_DISPLAY_PROCESSES,
            ))),
        }
    }

    async fn initialize_readers(&self, app_state: Arc<Mutex<AppState>>) {
        // Use timeout to prevent deadlock
        let initialized_result = timeout(Duration::from_secs(5), self.initialized.lock()).await;

        let mut initialized = match initialized_result {
            Ok(lock) => lock,
            Err(_) => {
                eprintln!("Warning: Timeout acquiring initialized lock");
                return;
            }
        };

        if *initialized {
            return;
        }

        // Add startup status with timeout
        {
            let state_result = timeout(Duration::from_secs(2), app_state.lock()).await;

            if let Ok(mut state) = state_result {
                state
                    .startup_status_lines
                    .push("✓ Initializing GPU readers...".to_string());
            }
        }

        let gpu_readers = get_gpu_readers();

        // Add startup status
        {
            let mut state = app_state.lock().await;
            state
                .startup_status_lines
                .push("✓ Initializing CPU readers...".to_string());
        }

        let cpu_readers = get_cpu_readers();

        // Add startup status
        {
            let mut state = app_state.lock().await;
            state
                .startup_status_lines
                .push("✓ Initializing memory readers...".to_string());
        }

        let memory_readers = get_memory_readers();

        // Create chassis reader
        let chassis_reader = create_chassis_reader();

        // Store the readers in self using RwLock with timeout
        {
            match timeout(Duration::from_secs(2), self.gpu_readers.write()).await {
                Ok(mut gpu_lock) => {
                    *gpu_lock = gpu_readers;
                }
                _ => {
                    eprintln!("Warning: Timeout acquiring GPU readers lock");
                }
            }
        }
        {
            match timeout(Duration::from_secs(2), self.cpu_readers.write()).await {
                Ok(mut cpu_lock) => {
                    *cpu_lock = cpu_readers;
                }
                _ => {
                    eprintln!("Warning: Timeout acquiring CPU readers lock");
                }
            }
        }
        {
            match timeout(Duration::from_secs(2), self.memory_readers.write()).await {
                Ok(mut mem_lock) => {
                    *mem_lock = memory_readers;
                }
                _ => {
                    eprintln!("Warning: Timeout acquiring memory readers lock");
                }
            }
        }
        {
            match timeout(Duration::from_secs(2), self.chassis_reader.write()).await {
                Ok(mut chassis_lock) => {
                    *chassis_lock = Some(chassis_reader);
                }
                _ => {
                    eprintln!("Warning: Timeout acquiring chassis reader lock");
                }
            }
        }

        *initialized = true;
    }

    async fn collect_parallel_first_iteration(
        &self,
        app_state: Arc<Mutex<AppState>>,
    ) -> CollectionData {
        use tokio::sync::mpsc;
        use tokio::task;

        // Add initial startup status
        {
            let mut state = app_state.lock().await;
            state
                .startup_status_lines
                .push("○ Collecting GPU information...".to_string());
            state
                .startup_status_lines
                .push("○ Collecting CPU information...".to_string());
            state
                .startup_status_lines
                .push("○ Collecting memory information...".to_string());
            state
                .startup_status_lines
                .push("○ Collecting process information...".to_string());
            state
                .startup_status_lines
                .push("○ Collecting storage information...".to_string());
        }

        // Create channel for status updates
        let (status_tx, mut status_rx) = mpsc::channel::<(usize, String)>(10);
        let app_state_clone = Arc::clone(&app_state);

        // Spawn task to handle status updates
        let status_handler = task::spawn(async move {
            while let Some((index, message)) = status_rx.recv().await {
                let mut state = app_state_clone.lock().await;
                // The five "Collecting ..." lines pushed above sit after the
                // three reader-initialization lines, hence the +3 offset. Bound
                // the shifted index, not the raw one, so a caller that pushed
                // fewer preamble lines cannot panic this task.
                let slot = 3 + index;
                if slot < state.startup_status_lines.len() {
                    state.startup_status_lines[slot] = message;
                }
            }
        });

        // Fan every synchronous reader pass out onto the blocking pool. Each
        // group owns its read guard for the duration of its task, so nothing
        // below runs on the async worker, and the six groups genuinely overlap
        // instead of taking turns on one task the way `tokio::join!` did.
        let process_cache = Arc::clone(&self.process_cache);
        let status_tx_gpu = status_tx.clone();
        let status_tx_cpu = status_tx.clone();
        let status_tx_mem = status_tx.clone();
        let status_tx_proc = status_tx.clone();
        let status_tx_storage = status_tx.clone();

        // GPU device info, GPU processes, vGPU and MIG in one pass.
        let gpu_task = spawn_reader_pass(&self.gpu_readers, move |readers| {
            let collected = collect_from_gpu_readers(readers);
            let _ = status_tx_gpu.blocking_send((0, "✓ GPU information collected".to_string()));
            collected
        })
        .await;

        let cpu_task = spawn_reader_pass(
            &self.cpu_readers,
            move |readers: &Vec<Box<dyn CpuReader>>| {
                let info: Vec<CpuInfo> = readers
                    .iter()
                    .flat_map(|reader| reader.get_cpu_info())
                    .collect();
                let _ = status_tx_cpu.blocking_send((1, "✓ CPU information collected".to_string()));
                info
            },
        )
        .await;

        let memory_task = spawn_reader_pass(
            &self.memory_readers,
            move |readers: &Vec<Box<dyn MemoryReader>>| {
                let info: Vec<MemoryInfo> = readers
                    .iter()
                    .flat_map(|reader| reader.get_memory_info())
                    .collect();
                let _ =
                    status_tx_mem.blocking_send((2, "✓ Memory information collected".to_string()));
                info
            },
        )
        .await;

        let chassis_task = spawn_reader_pass(
            &self.chassis_reader,
            |reader: &Option<Box<dyn ChassisReader>>| {
                let info: Vec<ChassisInfo> = reader
                    .as_ref()
                    .and_then(|r| r.get_chassis_info())
                    .into_iter()
                    .collect();
                info
            },
        )
        .await;

        let storage_task = task::spawn_blocking(move || {
            let storage_info = Self::collect_storage_info();
            let _ =
                status_tx_storage.blocking_send((4, "✓ Storage information collected".to_string()));
            storage_info
        });

        let processes_task = task::spawn_blocking(move || {
            let all_processes = with_global_system(|system| {
                use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, UpdateKind};
                // OPTIMIZATION: Only refresh fields we actually need
                // - CPU usage for cpu_percent
                // - Memory for memory_percent/memory_rss/memory_vms
                // - User only if not already set (avoid repeated lookups)
                // This is much cheaper than everything() which includes disk I/O, etc.
                let refresh_kind = ProcessRefreshKind::nothing()
                    .with_cpu()
                    .with_memory()
                    .with_user(UpdateKind::OnlyIfNotSet);
                system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind);
                system.refresh_memory();

                // OPTIMIZATION: Initialize process cache on first iteration
                // This populates the cache with all current processes. The GPU
                // pid set is deliberately empty here: `merge_gpu_processes`
                // below applies the GPU attribution for the first cycle.
                let gpu_pids: HashSet<u32> = HashSet::new();
                let mut cache = process_cache.write().unwrap();
                update_process_cache(system, &gpu_pids, &mut cache)
            });
            let _ =
                status_tx_proc.blocking_send((3, "✓ Process information collected".to_string()));
            all_processes
        });

        // A panicking reader degrades that group to empty rather than taking the
        // whole collection loop down, matching how the process arm already
        // handled a failed `spawn_blocking`.
        let GpuCollection {
            gpu_info: all_gpu_info,
            gpu_processes,
            // The first cycle deliberately seeds the process cache with an empty
            // GPU pid set; `merge_gpu_processes` below applies the attribution.
            gpu_pids: _,
            vgpu_info: all_vgpu_info,
            mig_info: all_mig_info,
        } = gpu_task.await.unwrap_or_default();
        let all_cpu_info = cpu_task.await.unwrap_or_default();
        let all_memory_info = memory_task.await.unwrap_or_default();
        let all_chassis_info = chassis_task.await.unwrap_or_default();
        let all_storage_info = storage_task.await.unwrap_or_default();
        let all_processes = processes_task.await.unwrap_or_default();

        // Close the channel and wait for status handler to finish
        drop(status_tx);
        let _ = status_handler.await;

        // Merge raw GPU processes into main process list
        let mut all_processes_merged = merge_gpu_processes(all_processes, gpu_processes);

        // Sort by CPU usage descending and limit to top MAX_DISPLAY_PROCESSES
        all_processes_merged.sort_by(|a, b| {
            b.cpu_percent
                .partial_cmp(&a.cpu_percent)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if all_processes_merged.len() > MAX_DISPLAY_PROCESSES {
            all_processes_merged.truncate(MAX_DISPLAY_PROCESSES);
        }

        // Initialize tracked PIDs for selective refresh in subsequent cycles
        let new_tracked_pids: Vec<sysinfo::Pid> = all_processes_merged
            .iter()
            .map(|p| sysinfo::Pid::from_u32(p.pid))
            .collect();
        *self.tracked_pids.write().await = new_tracked_pids;

        // Reset refresh cycle counter (first iteration counts as cycle 0)
        self.refresh_cycle.store(1, Ordering::Relaxed);

        // Inject aggregated GPU power into chassis info
        let all_chassis_info = inject_gpu_power(all_chassis_info, &all_gpu_info);

        CollectionData {
            gpu_info: all_gpu_info,
            cpu_info: all_cpu_info,
            memory_info: all_memory_info,
            process_info: all_processes_merged,
            storage_info: all_storage_info,
            chassis_info: all_chassis_info,
            vgpu_info: all_vgpu_info,
            mig_info: all_mig_info,
            connection_statuses: Vec::new(),
            // Local mode has no cluster-wide Users tab (issue #189).
            remote_process_info: Vec::new(),
        }
    }

    /// Steady-state collection, used for every cycle after the first.
    ///
    /// Formerly `collect_sequential`, and renamed because nothing here runs
    /// serially on the async task any more: every reader pass is dispatched to
    /// the blocking pool up front and only joined at the end.
    async fn collect_steady_state(&self) -> CollectionData {
        // Fan every synchronous reader pass out onto the blocking pool before
        // touching anything else, so the CPU, memory, chassis and storage
        // groups are already in flight while the GPU group runs.
        let gpu_task = spawn_reader_pass(&self.gpu_readers, |readers: &Vec<Box<dyn GpuReader>>| {
            collect_from_gpu_readers(readers)
        })
        .await;

        let cpu_task = spawn_reader_pass(&self.cpu_readers, |readers: &Vec<Box<dyn CpuReader>>| {
            readers
                .iter()
                .flat_map(|reader| reader.get_cpu_info())
                .collect::<Vec<CpuInfo>>()
        })
        .await;

        let memory_task = spawn_reader_pass(
            &self.memory_readers,
            |readers: &Vec<Box<dyn MemoryReader>>| {
                readers
                    .iter()
                    .flat_map(|reader| reader.get_memory_info())
                    .collect::<Vec<MemoryInfo>>()
            },
        )
        .await;

        let chassis_task = spawn_reader_pass(
            &self.chassis_reader,
            |reader: &Option<Box<dyn ChassisReader>>| {
                reader
                    .as_ref()
                    .and_then(|r| r.get_chassis_info())
                    .into_iter()
                    .collect::<Vec<ChassisInfo>>()
            },
        )
        .await;

        let storage_task = tokio::task::spawn_blocking(Self::collect_storage_info);

        // Determine if we should do a full refresh or selective refresh
        let cycle = self.refresh_cycle.fetch_add(1, Ordering::Relaxed);
        let do_full_refresh = cycle.is_multiple_of(FULL_REFRESH_INTERVAL);

        // Read tracked PIDs for selective refresh (outside the closure)
        let tracked_pids_for_refresh: Vec<sysinfo::Pid> = if do_full_refresh {
            Vec::new() // Not needed for full refresh
        } else {
            self.tracked_pids.read().await.clone()
        };

        // The process pass is the one group that cannot start independently:
        // `update_process_cache` needs this cycle's GPU pid set to decide which
        // cached entries are GPU-attributed. Joining the GPU group first keeps
        // that value identical to the pre-change behavior. The remaining groups
        // are already running, so this join does not extend the critical path
        // unless GPU plus process collection outlasts all of them.
        let GpuCollection {
            gpu_info: all_gpu_info,
            gpu_processes,
            gpu_pids,
            vgpu_info: all_vgpu_info,
            mig_info: all_mig_info,
        } = gpu_task.await.unwrap_or_default();

        let process_cache = Arc::clone(&self.process_cache);
        let processes_task = tokio::task::spawn_blocking(move || {
            with_global_system(|system| {
                use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, UpdateKind};
                // OPTIMIZATION: Only refresh fields we actually need
                // - CPU usage for cpu_percent
                // - Memory for memory_percent/memory_rss/memory_vms
                // - User only if not already set (avoid repeated lookups)
                let refresh_kind = ProcessRefreshKind::nothing()
                    .with_cpu()
                    .with_memory()
                    .with_user(UpdateKind::OnlyIfNotSet);

                // OPTIMIZATION: Selective process refresh
                // Full refresh every N cycles to discover new high-CPU processes;
                // otherwise only refresh tracked PIDs to significantly reduce CPU usage.
                if do_full_refresh || tracked_pids_for_refresh.is_empty() {
                    system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind);
                } else {
                    system.refresh_processes_specifics(
                        ProcessesToUpdate::Some(&tracked_pids_for_refresh),
                        true,
                        refresh_kind,
                    );
                }
                system.refresh_memory();

                // OPTIMIZATION: Use process cache to reduce memory allocation overhead
                // Instead of creating new ProcessInfo objects every cycle, we update
                // existing cached objects and only allocate for new processes.
                let mut cache = process_cache.write().unwrap();
                update_process_cache(system, &gpu_pids, &mut cache)
            })
        });

        let all_cpu_info = cpu_task.await.unwrap_or_default();
        let all_memory_info = memory_task.await.unwrap_or_default();
        let all_chassis_info = chassis_task.await.unwrap_or_default();
        let all_storage_info = storage_task.await.unwrap_or_default();
        let all_processes = processes_task.await.unwrap_or_default();

        let mut all_processes = merge_gpu_processes(all_processes, gpu_processes);

        // Sort by CPU usage descending and limit to top MAX_DISPLAY_PROCESSES
        all_processes.sort_by(|a, b| {
            b.cpu_percent
                .partial_cmp(&a.cpu_percent)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if all_processes.len() > MAX_DISPLAY_PROCESSES {
            all_processes.truncate(MAX_DISPLAY_PROCESSES);
        }

        // Update tracked PIDs for next cycle (after truncation to top N)
        let new_tracked_pids: Vec<sysinfo::Pid> = all_processes
            .iter()
            .map(|p| sysinfo::Pid::from_u32(p.pid))
            .collect();
        *self.tracked_pids.write().await = new_tracked_pids;

        // Inject aggregated GPU power into chassis info
        let all_chassis_info = inject_gpu_power(all_chassis_info, &all_gpu_info);

        CollectionData {
            gpu_info: all_gpu_info,
            cpu_info: all_cpu_info,
            memory_info: all_memory_info,
            process_info: all_processes,
            storage_info: all_storage_info,
            chassis_info: all_chassis_info,
            vgpu_info: all_vgpu_info,
            mig_info: all_mig_info,
            connection_statuses: Vec::new(),
            // Local mode has no cluster-wide Users tab (issue #189).
            remote_process_info: Vec::new(),
        }
    }

    fn collect_storage_info() -> Vec<StorageInfo> {
        let mut all_storage_info = Vec::new();
        let disks = Disks::new_with_refreshed_list();
        let hostname = get_hostname();

        let mut filtered_disks = filter_docker_aware_disks(&disks);
        filtered_disks.sort_by(|a, b| {
            a.mount_point()
                .to_string_lossy()
                .cmp(&b.mount_point().to_string_lossy())
        });

        for (index, disk) in filtered_disks.iter().enumerate() {
            let mount_point_str = disk.mount_point().to_string_lossy();
            all_storage_info.push(StorageInfo {
                mount_point: mount_point_str.to_string(),
                total_bytes: disk.total_space(),
                available_bytes: disk.available_space(),
                host_id: hostname.clone(),
                hostname: hostname.clone(),
                index: index as u32,
            });
        }

        all_storage_info
    }

    fn update_notifications(state: &mut AppState) {
        // Update notifications (remove expired ones)
        state.notifications.update();

        // Only check NVML status if we're trying to monitor NVIDIA devices
        if has_nvidia()
            && let Some(nvml_message) = get_nvml_status_message()
            && !state.nvml_notification_shown
        {
            if let Err(e) = state.notifications.warning(nvml_message) {
                eprintln!("Failed to show NVML notification: {e}");
            }
            state.nvml_notification_shown = true;
        }

        // Only check Tenstorrent status if we're trying to monitor Tenstorrent devices
        #[cfg(target_os = "linux")]
        if has_tenstorrent()
            && let Some(tt_message) = get_tenstorrent_status_message()
            && !state.tenstorrent_notification_shown
        {
            if let Err(e) = state.notifications.warning(tt_message) {
                eprintln!("Failed to show Tenstorrent notification: {e}");
            }
            state.tenstorrent_notification_shown = true;
        }

        // Google TPU status (Initializing / Failed)
        #[cfg(target_os = "linux")]
        if has_google_tpu()
            && let Some(msg) = get_tpu_status_message()
        {
            // If initializing, allow repeated updates (it will be "Initializing...")
            // If failed, show error once.
            if msg.contains("Initializing") {
                let _ = state.notifications.status(msg);
            } else if (msg.contains("failed") || msg.contains("error"))
                && !state.tpu_notification_shown
            {
                let _ = state.notifications.error(msg);
                state.tpu_notification_shown = true;
            }
        }
    }

    fn update_tabs(state: &mut AppState) {
        let mut host_ids: Vec<String> = state
            .gpu_info
            .iter()
            .map(|info| info.host_id.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        // If no GPU info available, use the local hostname
        if host_ids.is_empty() {
            host_ids.push(get_hostname());
        }

        host_ids.sort();

        // Always create "All" tab for consistent UI behavior
        let mut tabs = vec!["All".to_string()];
        tabs.extend(host_ids);

        state.tabs = tabs;
    }
}

#[async_trait]
impl DataCollectionStrategy for LocalCollector {
    async fn collect(&self, config: &CollectionConfig) -> CollectionResult {
        if config.first_iteration {
            // For first iteration, we need app_state for status updates
            // This is a limitation that needs to be addressed in the refactor
            // For now, return an error indicating initialization is needed
            return Err(CollectionError::Other(
                "First iteration requires app_state initialization".to_string(),
            ));
        }

        Ok(self.collect_steady_state().await)
    }

    async fn update_state(
        &self,
        app_state: Arc<Mutex<AppState>>,
        data: CollectionData,
        _config: &CollectionConfig,
    ) {
        // Check if we need to initialize readers
        if !*self.initialized.lock().await {
            self.initialize_readers(app_state.clone()).await;
        }

        let mut state = app_state.lock().await;

        // Update GPU info with UUID matching
        if state.gpu_info.is_empty() {
            state.gpu_info = data.gpu_info;
        } else {
            for new_info in data.gpu_info {
                if let Some(old_info) = state
                    .gpu_info
                    .iter_mut()
                    .find(|info| info.uuid == new_info.uuid)
                {
                    *old_info = new_info;
                }
            }
        }

        state.cpu_info = data.cpu_info;
        state.memory_info = data.memory_info;

        // Sort processes based on current criteria
        let mut sorted_processes = data.process_info;
        sorted_processes.sort_by(|a, b| {
            state
                .sort_criteria
                .sort_processes(a, b, state.sort_direction)
        });
        state.process_info = sorted_processes;

        state.storage_info = data.storage_info;
        state.chassis_info = data.chassis_info;
        state.vgpu_info = data.vgpu_info;
        state.mig_info = data.mig_info;

        // Mark data as changed to trigger UI update AND invalidate
        // collector-keyed caches (e.g. Users-tab aggregation).
        state.mark_collector_data_changed();

        // Update notifications
        Self::update_notifications(&mut state);

        // Update utilization history
        self.aggregator.update_utilization_history(&mut state);

        // Feed power samples into the energy integrator (issue #191).
        // Must run AFTER the new GPU / CPU / chassis info has been
        // written to state.
        self.aggregator.update_energy_counters(&mut state);

        // Update tabs
        Self::update_tabs(&mut state);

        // Always clear loading state in local mode after first iteration
        state.loading = false;
    }

    fn strategy_type(&self) -> &str {
        "local"
    }

    async fn is_ready(&self) -> bool {
        *self.initialized.lock().await
    }
}

impl LocalCollector {
    pub async fn collect_with_app_state(
        &self,
        app_state: Arc<Mutex<AppState>>,
        config: &CollectionConfig,
    ) -> CollectionResult {
        if !*self.initialized.lock().await {
            self.initialize_readers(app_state.clone()).await;
        }

        if config.first_iteration {
            Ok(self.collect_parallel_first_iteration(app_state).await)
        } else {
            Ok(self.collect_steady_state().await)
        }
    }
}

#[cfg(test)]
#[path = "local_collector/tests.rs"]
mod tests;
