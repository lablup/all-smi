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
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use sysinfo::Disks;
use tokio::sync::{Mutex, RwLock};
use tokio::time::timeout;

/// Type alias for the process cache using std::sync::RwLock for synchronous access
type ProcessCache = std::sync::RwLock<HashMap<u32, ProcessInfo>>;

use crate::app_state::AppState;
#[cfg(target_os = "linux")]
use crate::device::platform_detection::has_tenstorrent;
use crate::device::{
    create_chassis_reader, get_cpu_readers, get_gpu_readers, get_memory_readers,
    get_nvml_status_message,
    platform_detection::has_nvidia,
    process_list::{merge_gpu_processes, update_process_cache},
    ChassisInfo, ChassisReader, CpuInfo, CpuReader, GpuInfo, GpuReader, MemoryInfo, MemoryReader,
    ProcessInfo,
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
            if let Ok(mut gpu_lock) =
                timeout(Duration::from_secs(2), self.gpu_readers.write()).await
            {
                *gpu_lock = gpu_readers;
            } else {
                eprintln!("Warning: Timeout acquiring GPU readers lock");
            }
        }
        {
            if let Ok(mut cpu_lock) =
                timeout(Duration::from_secs(2), self.cpu_readers.write()).await
            {
                *cpu_lock = cpu_readers;
            } else {
                eprintln!("Warning: Timeout acquiring CPU readers lock");
            }
        }
        {
            if let Ok(mut mem_lock) =
                timeout(Duration::from_secs(2), self.memory_readers.write()).await
            {
                *mem_lock = memory_readers;
            } else {
                eprintln!("Warning: Timeout acquiring memory readers lock");
            }
        }
        {
            if let Ok(mut chassis_lock) =
                timeout(Duration::from_secs(2), self.chassis_reader.write()).await
            {
                *chassis_lock = Some(chassis_reader);
            } else {
                eprintln!("Warning: Timeout acquiring chassis reader lock");
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
        let (status_tx, mut status_rx) = mpsc::channel(10);
        let app_state_clone = Arc::clone(&app_state);

        // Spawn task to handle status updates
        let status_handler = task::spawn(async move {
            while let Some((index, message)) = status_rx.recv().await {
                let mut state = app_state_clone.lock().await;
                if index < state.startup_status_lines.len() {
                    state.startup_status_lines[3 + index] = message;
                }
            }
        });

        // Run all collections in parallel with status updates
        // Use Arc references instead of cloning the entire Arc<RwLock<_>>
        let gpu_readers_1 = Arc::clone(&self.gpu_readers);
        let gpu_readers_2 = Arc::clone(&self.gpu_readers);
        let cpu_readers = Arc::clone(&self.cpu_readers);
        let memory_readers = Arc::clone(&self.memory_readers);
        let chassis_reader = Arc::clone(&self.chassis_reader);
        let process_cache = Arc::clone(&self.process_cache);

        let (
            all_gpu_info,
            all_cpu_info,
            all_memory_info,
            gpu_processes,
            all_processes,
            all_storage_info,
            all_chassis_info,
        ) = {
            let status_tx_gpu = status_tx.clone();
            let status_tx_cpu = status_tx.clone();
            let status_tx_mem = status_tx.clone();
            let status_tx_proc = status_tx.clone();
            let status_tx_storage = status_tx.clone();

            tokio::join!(
                // GPU info collection
                async move {
                    let readers = gpu_readers_1.read().await;
                    let info: Vec<GpuInfo> = readers
                        .iter()
                        .flat_map(|reader| reader.get_gpu_info())
                        .collect();
                    let _ = status_tx_gpu
                        .send((0, "✓ GPU information collected".to_string()))
                        .await;
                    info
                },
                // CPU info collection
                async move {
                    let readers = cpu_readers.read().await;
                    let info: Vec<CpuInfo> = readers
                        .iter()
                        .flat_map(|reader| reader.get_cpu_info())
                        .collect();
                    let _ = status_tx_cpu
                        .send((1, "✓ CPU information collected".to_string()))
                        .await;
                    info
                },
                // Memory info collection
                async move {
                    let readers = memory_readers.read().await;
                    let info: Vec<MemoryInfo> = readers
                        .iter()
                        .flat_map(|reader| reader.get_memory_info())
                        .collect();
                    let _ = status_tx_mem
                        .send((2, "✓ Memory information collected".to_string()))
                        .await;
                    info
                },
                // GPU process collection (lightweight)
                async move {
                    let readers = gpu_readers_2.read().await;
                    let processes = readers
                        .iter()
                        .flat_map(|reader| reader.get_process_info())
                        .collect::<Vec<ProcessInfo>>();
                    processes
                },
                // Full process collection - use spawn_blocking to avoid blocking tokio runtime
                async move {
                    let all_processes = tokio::task::spawn_blocking(move || {
                        with_global_system(|system| {
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
                            system.refresh_processes_specifics(
                                ProcessesToUpdate::All,
                                true,
                                refresh_kind,
                            );
                            system.refresh_memory();

                            // OPTIMIZATION: Initialize process cache on first iteration
                            // This populates the cache with all current processes
                            let gpu_pids: HashSet<u32> = HashSet::new();
                            let mut cache = process_cache.write().unwrap();
                            update_process_cache(system, &gpu_pids, &mut cache)
                        })
                    })
                    .await
                    .unwrap_or_default();
                    let _ = status_tx_proc
                        .send((3, "✓ Process information collected".to_string()))
                        .await;
                    all_processes
                },
                // Storage collection
                async move {
                    let storage_info = Self::collect_storage_info();
                    let _ = status_tx_storage
                        .send((4, "✓ Storage information collected".to_string()))
                        .await;
                    storage_info
                },
                // Chassis info collection
                async move {
                    let reader = chassis_reader.read().await;
                    let info: Vec<ChassisInfo> = reader
                        .as_ref()
                        .and_then(|r| r.get_chassis_info())
                        .into_iter()
                        .collect();
                    info
                }
            )
        };

        // Close the channel and wait for status handler to finish
        drop(status_tx);
        let _ = status_handler.await;

        // Merge GPU processes into main process list
        let mut all_processes_merged = all_processes;
        merge_gpu_processes(&mut all_processes_merged, gpu_processes);

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

        CollectionData {
            gpu_info: all_gpu_info,
            cpu_info: all_cpu_info,
            memory_info: all_memory_info,
            process_info: all_processes_merged,
            storage_info: all_storage_info,
            chassis_info: all_chassis_info,
            connection_statuses: Vec::new(),
        }
    }

    async fn collect_sequential(&self) -> CollectionData {
        let gpu_readers = self.gpu_readers.read().await;
        let all_gpu_info: Vec<GpuInfo> = gpu_readers
            .iter()
            .flat_map(|reader| reader.get_gpu_info())
            .collect();

        let cpu_readers = self.cpu_readers.read().await;
        let all_cpu_info: Vec<CpuInfo> = cpu_readers
            .iter()
            .flat_map(|reader| reader.get_cpu_info())
            .collect();

        let memory_readers = self.memory_readers.read().await;
        let all_memory_info: Vec<MemoryInfo> = memory_readers
            .iter()
            .flat_map(|reader| reader.get_memory_info())
            .collect();

        let gpu_processes: Vec<ProcessInfo> = gpu_readers
            .iter()
            .flat_map(|reader| reader.get_process_info())
            .collect();

        // Determine if we should do a full refresh or selective refresh
        let cycle = self.refresh_cycle.fetch_add(1, Ordering::Relaxed);
        let do_full_refresh = cycle % FULL_REFRESH_INTERVAL == 0;

        // Read tracked PIDs for selective refresh (outside the closure)
        let tracked_pids_for_refresh: Vec<sysinfo::Pid> = if do_full_refresh {
            Vec::new() // Not needed for full refresh
        } else {
            self.tracked_pids.read().await.clone()
        };

        let gpu_pids: HashSet<u32> = gpu_processes.iter().map(|p| p.pid).collect();
        let process_cache = Arc::clone(&self.process_cache);
        let mut all_processes = with_global_system(|system| {
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
        });
        merge_gpu_processes(&mut all_processes, gpu_processes);

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

        let all_storage_info = Self::collect_storage_info();

        // Collect chassis info
        let chassis_reader = self.chassis_reader.read().await;
        let all_chassis_info: Vec<ChassisInfo> = chassis_reader
            .as_ref()
            .and_then(|r| r.get_chassis_info())
            .into_iter()
            .collect();

        CollectionData {
            gpu_info: all_gpu_info,
            cpu_info: all_cpu_info,
            memory_info: all_memory_info,
            process_info: all_processes,
            storage_info: all_storage_info,
            chassis_info: all_chassis_info,
            connection_statuses: Vec::new(),
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
        if has_nvidia() {
            if let Some(nvml_message) = get_nvml_status_message() {
                if !state.nvml_notification_shown {
                    if let Err(e) = state.notifications.warning(nvml_message) {
                        eprintln!("Failed to show NVML notification: {e}");
                    }
                    state.nvml_notification_shown = true;
                }
            }
        }

        // Only check Tenstorrent status if we're trying to monitor Tenstorrent devices
        #[cfg(target_os = "linux")]
        if has_tenstorrent() {
            if let Some(tt_message) = get_tenstorrent_status_message() {
                if !state.tenstorrent_notification_shown {
                    if let Err(e) = state.notifications.warning(tt_message) {
                        eprintln!("Failed to show Tenstorrent notification: {e}");
                    }
                    state.tenstorrent_notification_shown = true;
                }
            }
        }

        // Google TPU status (Initializing / Failed)
        #[cfg(target_os = "linux")]
        if has_google_tpu() {
            if let Some(msg) = get_tpu_status_message() {
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

        Ok(self.collect_sequential().await)
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

        // Mark data as changed to trigger UI update
        state.mark_data_changed();

        // Update notifications
        Self::update_notifications(&mut state);

        // Update utilization history
        self.aggregator.update_utilization_history(&mut state);

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
            Ok(self.collect_sequential().await)
        }
    }
}
