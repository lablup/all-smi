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

//! Caches derived TUI view data to avoid per-frame sorting, filtering, and cloning.
//!
//! The cache is keyed by the inputs that affect the derived result (data version,
//! current tab, sort criteria, GPU filter state). When any of those inputs change
//! the relevant cache entry is invalidated and recomputed on the next render.
//!
//! The cache is owned by the UI loop and lives alongside the `RenderSnapshot`.

use crate::app_state::SortCriteria;
use crate::device::ProcessInfo;
use crate::view::render_snapshot::RenderSnapshot;

/// Key that determines whether the GPU display cache is still valid.
///
/// When any field in this key differs from the previous render, the
/// sorted/filtered GPU list is recomputed.
#[derive(Clone, Debug, PartialEq, Eq)]
struct GpuCacheKey {
    data_version: u64,
    current_tab: usize,
    sort_criteria_ordinal: u8,
}

/// Key for host-specific device subsets (CPU, memory, storage, chassis).
#[derive(Clone, Debug, PartialEq, Eq)]
struct HostDeviceCacheKey {
    data_version: u64,
    current_tab: usize,
    is_local_mode: bool,
}

/// Key for GPU-filtered process list.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ProcessFilterCacheKey {
    data_version: u64,
    gpu_filter_enabled: bool,
}

/// Map `SortCriteria` to a stable ordinal for use in cache keys.
///
/// Using an ordinal avoids requiring `Eq`/`Hash` on `SortCriteria` variants
/// that only affect GPU sorting (Default, Utilization, GpuMemory, Power,
/// Temperature) while keeping all process-sort variants collapsed to a
/// single sentinel, since GPU sorting ignores them.
fn sort_criteria_ordinal(criteria: SortCriteria) -> u8 {
    match criteria {
        SortCriteria::Default => 0,
        SortCriteria::Utilization => 1,
        SortCriteria::GpuMemory => 2,
        SortCriteria::Power => 3,
        SortCriteria::Temperature => 4,
        // All process-only criteria map to the same Default GPU sort
        _ => 0,
    }
}

/// Pre-computed, sorted GPU display list for the current tab + sort criteria.
#[derive(Clone)]
pub struct CachedGpuList {
    /// Sorted indices into `RenderSnapshot::gpu_info`.
    ///
    /// Storing indices instead of cloned `GpuInfo` avoids duplicating the
    /// (potentially large) GPU data. The consumer reads the original snapshot
    /// through these indices.
    pub indices: Vec<usize>,
}

/// Pre-computed host-filtered device subsets for the current tab.
#[derive(Clone)]
pub struct CachedHostDevices {
    pub chassis_indices: Vec<usize>,
    pub cpu_indices: Vec<usize>,
    pub memory_indices: Vec<usize>,
    pub storage_indices: Vec<usize>,
}

/// Pre-computed GPU-filtered process list.
#[derive(Clone)]
pub struct CachedProcessList {
    /// When GPU filter is disabled, this is `None` and callers should use
    /// `snapshot.process_info` directly to avoid any clone at all.
    /// When enabled, contains only processes with `used_memory > 0`.
    pub filtered: Option<Vec<ProcessInfo>>,
}

/// Holds all cached derived view data with their invalidation keys.
///
/// The cache is designed to be created once and reused across frames.
/// Each `update()` call checks whether the keys have changed and only
/// recomputes the entries that are stale.
pub struct ViewCache {
    gpu_key: Option<GpuCacheKey>,
    pub gpu_list: Option<CachedGpuList>,

    host_key: Option<HostDeviceCacheKey>,
    pub host_devices: Option<CachedHostDevices>,

    process_key: Option<ProcessFilterCacheKey>,
    pub process_list: Option<CachedProcessList>,
}

impl Default for ViewCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ViewCache {
    /// Create an empty cache. All entries will be computed on the first call
    /// to `update()`.
    pub fn new() -> Self {
        Self {
            gpu_key: None,
            gpu_list: None,
            host_key: None,
            host_devices: None,
            process_key: None,
            process_list: None,
        }
    }

    /// Recompute any stale cache entries based on the current snapshot.
    ///
    /// Returns `true` if any cache entry was recomputed (useful for debugging
    /// or metrics). Each section is checked independently so that, for
    /// example, a tab change invalidates the GPU and host-device caches but
    /// not the process-filter cache.
    pub fn update(&mut self, snapshot: &RenderSnapshot) -> bool {
        let mut recomputed = false;

        recomputed |= self.update_gpu_list(snapshot);
        recomputed |= self.update_host_devices(snapshot);
        recomputed |= self.update_process_list(snapshot);

        recomputed
    }

    /// Invalidate all cache entries, forcing recomputation on the next
    /// `update()` call.
    pub fn invalidate_all(&mut self) {
        self.gpu_key = None;
        self.gpu_list = None;
        self.host_key = None;
        self.host_devices = None;
        self.process_key = None;
        self.process_list = None;
    }

    // ------------------------------------------------------------------
    // GPU display list
    // ------------------------------------------------------------------

    fn update_gpu_list(&mut self, snapshot: &RenderSnapshot) -> bool {
        let new_key = GpuCacheKey {
            data_version: snapshot.data_version,
            current_tab: snapshot.current_tab,
            sort_criteria_ordinal: sort_criteria_ordinal(snapshot.sort_criteria),
        };

        if self.gpu_key.as_ref() == Some(&new_key) {
            return false;
        }

        // Build filtered + sorted index list.
        // Guard against current_tab being out of bounds (defensive) --
        // show all GPUs in that case rather than panicking.
        let mut indices: Vec<usize> =
            if let Some(tab_name) = snapshot.tabs.get(snapshot.current_tab) {
                if tab_name == "All" {
                    (0..snapshot.gpu_info.len()).collect()
                } else {
                    snapshot
                        .gpu_info
                        .iter()
                        .enumerate()
                        .filter(|(_, info)| info.host_id == *tab_name)
                        .map(|(i, _)| i)
                        .collect()
                }
            } else {
                // Out-of-bounds tab index: show all (defensive)
                (0..snapshot.gpu_info.len()).collect()
            };

        // Sort by the current criteria
        let criteria = snapshot.sort_criteria;
        indices.sort_by(|&a, &b| criteria.sort_gpus(&snapshot.gpu_info[a], &snapshot.gpu_info[b]));

        self.gpu_key = Some(new_key);
        self.gpu_list = Some(CachedGpuList { indices });
        true
    }

    // ------------------------------------------------------------------
    // Host device subsets
    // ------------------------------------------------------------------

    fn update_host_devices(&mut self, snapshot: &RenderSnapshot) -> bool {
        let new_key = HostDeviceCacheKey {
            data_version: snapshot.data_version,
            current_tab: snapshot.current_tab,
            is_local_mode: snapshot.is_local_mode,
        };

        if self.host_key.as_ref() == Some(&new_key) {
            return false;
        }

        let cached = if snapshot.is_local_mode {
            // Local mode: all devices are relevant
            CachedHostDevices {
                chassis_indices: (0..snapshot.chassis_info.len()).collect(),
                cpu_indices: (0..snapshot.cpu_info.len()).collect(),
                memory_indices: (0..snapshot.memory_info.len()).collect(),
                storage_indices: (0..snapshot.storage_info.len()).collect(),
            }
        } else if snapshot.current_tab == 0 {
            // Remote "All" tab: chassis are hidden; other devices shown on
            // per-host tabs only, so return empty.
            CachedHostDevices {
                chassis_indices: Vec::new(),
                cpu_indices: Vec::new(),
                memory_indices: Vec::new(),
                storage_indices: Vec::new(),
            }
        } else if snapshot.current_tab < snapshot.tabs.len() {
            let hostname = &snapshot.tabs[snapshot.current_tab];
            CachedHostDevices {
                chassis_indices: snapshot
                    .chassis_info
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.host_id == *hostname || c.hostname == *hostname)
                    .map(|(i, _)| i)
                    .collect(),
                cpu_indices: snapshot
                    .cpu_info
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.host_id == *hostname)
                    .map(|(i, _)| i)
                    .collect(),
                memory_indices: snapshot
                    .memory_info
                    .iter()
                    .enumerate()
                    .filter(|(_, m)| m.host_id == *hostname)
                    .map(|(i, _)| i)
                    .collect(),
                storage_indices: snapshot
                    .storage_info
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.host_id == *hostname)
                    .map(|(i, _)| i)
                    .collect(),
            }
        } else {
            // Out-of-bounds tab: show all (defensive)
            CachedHostDevices {
                chassis_indices: (0..snapshot.chassis_info.len()).collect(),
                cpu_indices: (0..snapshot.cpu_info.len()).collect(),
                memory_indices: (0..snapshot.memory_info.len()).collect(),
                storage_indices: (0..snapshot.storage_info.len()).collect(),
            }
        };

        self.host_key = Some(new_key);
        self.host_devices = Some(cached);
        true
    }

    // ------------------------------------------------------------------
    // GPU-filtered process list
    // ------------------------------------------------------------------

    fn update_process_list(&mut self, snapshot: &RenderSnapshot) -> bool {
        let new_key = ProcessFilterCacheKey {
            data_version: snapshot.data_version,
            gpu_filter_enabled: snapshot.gpu_filter_enabled,
        };

        if self.process_key.as_ref() == Some(&new_key) {
            return false;
        }

        let filtered = if snapshot.gpu_filter_enabled {
            Some(
                snapshot
                    .process_info
                    .iter()
                    .filter(|p| p.used_memory > 0)
                    .cloned()
                    .collect(),
            )
        } else {
            None
        };

        self.process_key = Some(new_key);
        self.process_list = Some(CachedProcessList { filtered });
        true
    }

    // ------------------------------------------------------------------
    // Accessor helpers (convenience for the frame renderer)
    // ------------------------------------------------------------------

    /// Return the cached sorted GPU indices, or `None` if the cache has not
    /// been populated yet.
    pub fn gpu_indices(&self) -> Option<&[usize]> {
        self.gpu_list.as_ref().map(|c| c.indices.as_slice())
    }

    /// Return the cached host-device indices, or `None` if unpopulated.
    pub fn host_device_indices(&self) -> Option<&CachedHostDevices> {
        self.host_devices.as_ref()
    }

    /// Return the cached process list. When the GPU filter is off the inner
    /// `filtered` field is `None` and the caller should read directly from
    /// the snapshot.
    pub fn process_display_list(&self) -> Option<&CachedProcessList> {
        self.process_list.as_ref()
    }
}

// ======================================================================
// Tests
// ======================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::{AppState, SortCriteria};
    use crate::device::{CpuInfo, GpuInfo, MemoryInfo};
    use crate::storage::info::StorageInfo;
    use crate::view::render_snapshot::RenderSnapshot;

    fn make_snapshot_with_gpus(count: usize) -> RenderSnapshot {
        let mut state = AppState::new();
        for i in 0..count {
            state.gpu_info.push(GpuInfo {
                uuid: format!("gpu-{i}"),
                time: String::new(),
                name: format!("GPU {i}"),
                device_type: "GPU".to_string(),
                host_id: if i % 2 == 0 {
                    "host-a".to_string()
                } else {
                    "host-b".to_string()
                },
                hostname: if i % 2 == 0 {
                    "host-a".to_string()
                } else {
                    "host-b".to_string()
                },
                instance: String::new(),
                utilization: (count - i) as f64 * 10.0,
                ane_utilization: 0.0,
                dla_utilization: None,
                tensorcore_utilization: None,
                temperature: 50 + i as u32,
                used_memory: (i as u64 + 1) * 1024,
                total_memory: 16384,
                frequency: 1500,
                power_consumption: 100.0,
                gpu_core_count: None,
                temperature_threshold_slowdown: None,
                temperature_threshold_shutdown: None,
                temperature_threshold_max_operating: None,
                temperature_threshold_acoustic: None,
                performance_state: None,
                numa_node_id: None,
                gsp_firmware_mode: None,
                gsp_firmware_version: None,
                nvlink_remote_devices: Vec::new(),
                gpm_metrics: None,
                detail: {
                    let mut m = std::collections::HashMap::new();
                    m.insert("index".to_string(), i.to_string());
                    m
                },
            });
        }
        state.tabs = vec![
            "All".to_string(),
            "host-a".to_string(),
            "host-b".to_string(),
        ];
        state.is_local_mode = false;
        state.mark_data_changed();
        RenderSnapshot::capture(&mut state)
    }

    // ------------------------------------------------------------------
    // Basic construction
    // ------------------------------------------------------------------

    #[test]
    fn test_new_cache_is_empty() {
        let cache = ViewCache::new();
        assert!(cache.gpu_indices().is_none());
        assert!(cache.host_device_indices().is_none());
        assert!(cache.process_display_list().is_none());
    }

    // ------------------------------------------------------------------
    // GPU list caching
    // ------------------------------------------------------------------

    #[test]
    fn test_gpu_cache_populated_on_first_update() {
        let snapshot = make_snapshot_with_gpus(4);
        let mut cache = ViewCache::new();

        let recomputed = cache.update(&snapshot);
        assert!(recomputed);
        assert!(cache.gpu_indices().is_some());
    }

    #[test]
    fn test_gpu_cache_reused_on_identical_snapshot() {
        let snapshot = make_snapshot_with_gpus(4);
        let mut cache = ViewCache::new();

        cache.update(&snapshot);
        let recomputed = cache.update(&snapshot);
        assert!(
            !recomputed,
            "cache should be reused when inputs are unchanged"
        );
    }

    #[test]
    fn test_gpu_cache_invalidated_on_data_version_change() {
        let mut state = AppState::new();
        for i in 0..2 {
            state.gpu_info.push(GpuInfo {
                uuid: format!("gpu-{i}"),
                time: String::new(),
                name: format!("GPU {i}"),
                device_type: "GPU".to_string(),
                host_id: "host-a".to_string(),
                hostname: "host-a".to_string(),
                instance: String::new(),
                utilization: 50.0,
                ane_utilization: 0.0,
                dla_utilization: None,
                tensorcore_utilization: None,
                temperature: 60,
                used_memory: 4096,
                total_memory: 16384,
                frequency: 1500,
                power_consumption: 100.0,
                gpu_core_count: None,
                temperature_threshold_slowdown: None,
                temperature_threshold_shutdown: None,
                temperature_threshold_max_operating: None,
                temperature_threshold_acoustic: None,
                performance_state: None,
                numa_node_id: None,
                gsp_firmware_mode: None,
                gsp_firmware_version: None,
                nvlink_remote_devices: Vec::new(),
                gpm_metrics: None,
                detail: std::collections::HashMap::new(),
            });
        }
        state.mark_data_changed();
        let snap1 = RenderSnapshot::capture(&mut state);

        let mut cache = ViewCache::new();
        cache.update(&snap1);
        assert!(!cache.update(&snap1));

        // Bump version
        state.mark_data_changed();
        let snap2 = RenderSnapshot::capture(&mut state);
        assert!(
            cache.update(&snap2),
            "cache should invalidate on data_version change"
        );
    }

    #[test]
    fn test_gpu_cache_invalidated_on_tab_change() {
        let snapshot = make_snapshot_with_gpus(4);
        let mut cache = ViewCache::new();
        cache.update(&snapshot);

        // Switch to host-a tab
        let mut state = AppState::new();
        state.gpu_info = snapshot.gpu_info.clone();
        state.tabs = snapshot.tabs.clone();
        state.current_tab = 1; // host-a
        state.data_version = snapshot.data_version;
        let snap2 = RenderSnapshot::capture(&mut state);

        assert!(
            cache.update(&snap2),
            "tab change should invalidate GPU cache"
        );

        // Only host-a GPUs (even indices) should appear
        let indices = cache.gpu_indices().unwrap();
        for &idx in indices {
            assert_eq!(
                snap2.gpu_info[idx].host_id, "host-a",
                "filtered GPU should belong to host-a"
            );
        }
    }

    #[test]
    fn test_gpu_cache_invalidated_on_sort_change() {
        let snapshot = make_snapshot_with_gpus(4);
        let mut cache = ViewCache::new();
        cache.update(&snapshot);

        let mut state = AppState::new();
        state.gpu_info = snapshot.gpu_info.clone();
        state.tabs = snapshot.tabs.clone();
        state.sort_criteria = SortCriteria::Utilization;
        state.data_version = snapshot.data_version;
        let snap2 = RenderSnapshot::capture(&mut state);

        assert!(
            cache.update(&snap2),
            "sort change should invalidate GPU cache"
        );
    }

    // ------------------------------------------------------------------
    // Process list caching
    // ------------------------------------------------------------------

    #[test]
    fn test_process_cache_no_filter_returns_none_filtered() {
        let mut state = AppState::new();
        state.gpu_filter_enabled = false;
        state.process_info.push(ProcessInfo {
            device_id: 0,
            device_uuid: "u".into(),
            pid: 1,
            used_memory: 0,
            process_name: "test".into(),
            user: "u".into(),
            state: "S".into(),
            command: "c".into(),
            cpu_percent: 1.0,
            memory_percent: 1.0,
            gpu_utilization: 0.0,
            priority: 20,
            nice_value: 0,
            memory_vms: 0,
            memory_rss: 0,
            cpu_time: 0,
            start_time: "".into(),
            ppid: 0,
            threads: 1,
            uses_gpu: false,
        });
        let snapshot = RenderSnapshot::capture(&mut state);
        let mut cache = ViewCache::new();
        cache.update(&snapshot);

        let pl = cache.process_display_list().unwrap();
        assert!(
            pl.filtered.is_none(),
            "when GPU filter is off, filtered should be None (use snapshot directly)"
        );
    }

    #[test]
    fn test_process_cache_gpu_filter_clones_only_gpu_processes() {
        let mut state = AppState::new();
        state.gpu_filter_enabled = true;
        // Process with GPU memory
        state.process_info.push(ProcessInfo {
            device_id: 0,
            device_uuid: "u".into(),
            pid: 1,
            used_memory: 4096,
            process_name: "gpu_proc".into(),
            user: "u".into(),
            state: "S".into(),
            command: "c".into(),
            cpu_percent: 1.0,
            memory_percent: 1.0,
            gpu_utilization: 50.0,
            priority: 20,
            nice_value: 0,
            memory_vms: 0,
            memory_rss: 0,
            cpu_time: 0,
            start_time: "".into(),
            ppid: 0,
            threads: 1,
            uses_gpu: true,
        });
        // Process without GPU memory
        state.process_info.push(ProcessInfo {
            device_id: 0,
            device_uuid: "u".into(),
            pid: 2,
            used_memory: 0,
            process_name: "cpu_proc".into(),
            user: "u".into(),
            state: "S".into(),
            command: "c".into(),
            cpu_percent: 50.0,
            memory_percent: 10.0,
            gpu_utilization: 0.0,
            priority: 20,
            nice_value: 0,
            memory_vms: 0,
            memory_rss: 0,
            cpu_time: 0,
            start_time: "".into(),
            ppid: 0,
            threads: 1,
            uses_gpu: false,
        });
        let snapshot = RenderSnapshot::capture(&mut state);
        let mut cache = ViewCache::new();
        cache.update(&snapshot);

        let pl = cache.process_display_list().unwrap();
        let filtered = pl.filtered.as_ref().unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].pid, 1);
    }

    // ------------------------------------------------------------------
    // Host device caching
    // ------------------------------------------------------------------

    #[test]
    fn test_host_devices_local_mode_all_indices() {
        let mut state = AppState::new();
        state.is_local_mode = true;
        state.cpu_info.push(CpuInfo {
            host_id: "local".into(),
            hostname: "local".into(),
            instance: "".into(),
            cpu_model: "Test".into(),
            architecture: "x86_64".into(),
            platform_type: crate::device::CpuPlatformType::Intel,
            socket_count: 1,
            total_cores: 4,
            total_threads: 8,
            base_frequency_mhz: 3000,
            max_frequency_mhz: 4000,
            cache_size_mb: 8,
            utilization: 10.0,
            temperature: None,
            power_consumption: None,
            per_socket_info: Vec::new(),
            apple_silicon_info: None,
            per_core_utilization: Vec::new(),
            time: "".into(),
        });
        state.memory_info.push(MemoryInfo {
            host_id: "local".into(),
            hostname: "local".into(),
            instance: "".into(),
            total_bytes: 1024,
            used_bytes: 512,
            available_bytes: 512,
            free_bytes: 256,
            buffers_bytes: 0,
            cached_bytes: 0,
            swap_total_bytes: 0,
            swap_used_bytes: 0,
            swap_free_bytes: 0,
            utilization: 50.0,
            time: "".into(),
        });
        let snapshot = RenderSnapshot::capture(&mut state);
        let mut cache = ViewCache::new();
        cache.update(&snapshot);

        let hd = cache.host_device_indices().unwrap();
        assert_eq!(hd.cpu_indices.len(), 1);
        assert_eq!(hd.memory_indices.len(), 1);
    }

    #[test]
    fn test_host_devices_remote_all_tab_empty() {
        let mut state = AppState::new();
        state.is_local_mode = false;
        state.current_tab = 0; // "All" tab
        state.cpu_info.push(CpuInfo {
            host_id: "host-a".into(),
            hostname: "host-a".into(),
            instance: "".into(),
            cpu_model: "Test".into(),
            architecture: "x86_64".into(),
            platform_type: crate::device::CpuPlatformType::Intel,
            socket_count: 1,
            total_cores: 4,
            total_threads: 8,
            base_frequency_mhz: 3000,
            max_frequency_mhz: 4000,
            cache_size_mb: 8,
            utilization: 10.0,
            temperature: None,
            power_consumption: None,
            per_socket_info: Vec::new(),
            apple_silicon_info: None,
            per_core_utilization: Vec::new(),
            time: "".into(),
        });
        let snapshot = RenderSnapshot::capture(&mut state);
        let mut cache = ViewCache::new();
        cache.update(&snapshot);

        let hd = cache.host_device_indices().unwrap();
        // "All" tab in remote mode returns empty host-device indices
        assert!(hd.cpu_indices.is_empty());
    }

    #[test]
    fn test_host_devices_remote_host_tab_filters() {
        let mut state = AppState::new();
        state.is_local_mode = false;
        state.tabs = vec![
            "All".to_string(),
            "host-a".to_string(),
            "host-b".to_string(),
        ];
        state.current_tab = 1; // host-a
        state.storage_info.push(StorageInfo {
            host_id: "host-a".into(),
            hostname: "host-a".into(),
            mount_point: "/".into(),
            total_bytes: 1024,
            available_bytes: 512,
            index: 0,
        });
        state.storage_info.push(StorageInfo {
            host_id: "host-b".into(),
            hostname: "host-b".into(),
            mount_point: "/".into(),
            total_bytes: 1024,
            available_bytes: 512,
            index: 0,
        });
        let snapshot = RenderSnapshot::capture(&mut state);
        let mut cache = ViewCache::new();
        cache.update(&snapshot);

        let hd = cache.host_device_indices().unwrap();
        assert_eq!(
            hd.storage_indices.len(),
            1,
            "only host-a storage should be cached"
        );
        assert_eq!(
            snapshot.storage_info[hd.storage_indices[0]].host_id,
            "host-a"
        );
    }

    // ------------------------------------------------------------------
    // invalidate_all
    // ------------------------------------------------------------------

    #[test]
    fn test_invalidate_all_clears_caches() {
        let snapshot = make_snapshot_with_gpus(2);
        let mut cache = ViewCache::new();
        cache.update(&snapshot);

        assert!(cache.gpu_indices().is_some());
        cache.invalidate_all();
        assert!(cache.gpu_indices().is_none());
        assert!(cache.host_device_indices().is_none());
        assert!(cache.process_display_list().is_none());
    }

    // ------------------------------------------------------------------
    // sort_criteria_ordinal
    // ------------------------------------------------------------------

    #[test]
    fn test_sort_criteria_ordinal_distinct_for_gpu_sorts() {
        assert_ne!(
            sort_criteria_ordinal(SortCriteria::Default),
            sort_criteria_ordinal(SortCriteria::Utilization)
        );
        assert_ne!(
            sort_criteria_ordinal(SortCriteria::Utilization),
            sort_criteria_ordinal(SortCriteria::GpuMemory)
        );
    }

    #[test]
    fn test_sort_criteria_ordinal_process_sorts_collapse() {
        // All process-only criteria should return the same ordinal (they
        // don't affect GPU sort order).
        let base = sort_criteria_ordinal(SortCriteria::Default);
        assert_eq!(sort_criteria_ordinal(SortCriteria::Pid), base);
        assert_eq!(sort_criteria_ordinal(SortCriteria::User), base);
        assert_eq!(sort_criteria_ordinal(SortCriteria::CpuPercent), base);
        assert_eq!(sort_criteria_ordinal(SortCriteria::MemoryPercent), base);
    }
}
